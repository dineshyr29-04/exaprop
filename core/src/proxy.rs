//! A small, bounded HTTP/1.1 reverse proxy.
//!
//! Hyper owns HTTP framing and parsing; this module only changes the request
//! destination, removes hop-by-hop headers, and relays bounded bodies. Bodies
//! are buffered up to [`MAX_BODY_BYTES`] so every in-flight request has a
//! predictable memory ceiling.

use std::{convert::Infallible, future::Future, net::SocketAddr, time::Duration};

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::{
    body::{Body, Incoming},
    header::{HeaderMap, HeaderName, HeaderValue, CONNECTION, CONTENT_LENGTH, EXPECT, UPGRADE},
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode, Uri,
};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo, TokioTimer},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};

/// Largest request or response body accepted by the proxy.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

const MAX_CONNECTIONS: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

type ProxyClient = Client<HttpConnector, Full<Bytes>>;

/// Start a proxy on `listen_addr`, forwarding traffic to `backend_addr`.
///
/// Pressing Ctrl-C stops accepting new connections and gives active
/// connections up to two seconds to finish before they are cancelled.
pub async fn run(listen_addr: SocketAddr, backend_addr: SocketAddr) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    let actual_addr = listener.local_addr()?;
    let client = build_client();

    eprintln!(
        "ExaProp proxy listening on http://{actual_addr}, forwarding to http://{backend_addr}"
    );

    run_listener(listener, backend_addr, client, async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Could not listen for Ctrl-C: {error}");
        }
    })
    .await;

    eprintln!("Proxy shutting down");
    Ok(())
}

fn build_client() -> ProxyClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(IO_TIMEOUT));

    Client::builder(TokioExecutor::new()).build(connector)
}

async fn run_listener<F>(
    listener: TcpListener,
    backend_addr: SocketAddr,
    client: ProxyClient,
    shutdown: F,
) where
    F: Future<Output = ()> + Send,
{
    tokio::pin!(shutdown);
    let permits = std::sync::Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();

    loop {
        while connections.try_join_next().is_some() {}

        // Acquire capacity before accepting so active connections, including
        // keep-alive connections, cannot grow without bound.
        let permit = tokio::select! {
            _ = &mut shutdown => break,
            result = permits.clone().acquire_owned() => match result {
                Ok(permit) => permit,
                Err(_) => break,
            },
        };

        let accepted = tokio::select! {
            _ = &mut shutdown => {
                drop(permit);
                break;
            }
            result = listener.accept() => result,
        };

        match accepted {
            Ok((stream, peer)) => {
                eprintln!("Accepted client {peer}");
                let client = client.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, backend_addr, client).await {
                        eprintln!("Connection from {peer} ended with an error: {error}");
                    }
                });
            }
            Err(error) => {
                drop(permit);
                eprintln!("Could not accept client connection: {error}");
            }
        }
    }

    if timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

async fn serve_connection(
    stream: TcpStream,
    backend_addr: SocketAddr,
    client: ProxyClient,
) -> Result<(), hyper::Error> {
    let service = service_fn(move |request| {
        let client = client.clone();
        async move { Ok::<_, Infallible>(handle_request(request, backend_addr, client).await) }
    });

    http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(IO_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .await
}

async fn handle_request(
    request: Request<Incoming>,
    backend_addr: SocketAddr,
    client: ProxyClient,
) -> Response<Full<Bytes>> {
    if request.method() == Method::CONNECT
        || request.headers().contains_key(UPGRADE)
        || request
            .headers()
            .get(CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
    {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "HTTP upgrades are not supported\n",
        );
    }

    if request.headers().contains_key(EXPECT) {
        return error_response(
            StatusCode::EXPECTATION_FAILED,
            "Expect headers are not supported\n",
        );
    }

    if request.headers().contains_key("trailer") {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "HTTP trailers are not supported\n",
        );
    }

    let method = request.method().clone();
    let version = request.version();
    let path_and_query = request
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let uri = match format!("http://{backend_addr}{path_and_query}").parse::<Uri>() {
        Ok(uri) => uri,
        Err(error) => {
            eprintln!("Could not construct backend URI: {error}");
            return error_response(StatusCode::BAD_REQUEST, "Invalid request target\n");
        }
    };

    let mut headers = request.headers().clone();
    strip_hop_by_hop_headers(&mut headers);
    headers.remove(CONTENT_LENGTH);

    let body = match read_limited_body(request.into_body()).await {
        Ok(body) => body,
        Err(BodyReadError::TooLarge) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Request body exceeds 4 MiB\n",
            );
        }
        Err(BodyReadError::Timeout) => {
            return error_response(
                StatusCode::REQUEST_TIMEOUT,
                "Timed out reading request body\n",
            );
        }
        Err(BodyReadError::Trailers) => {
            return error_response(
                StatusCode::NOT_IMPLEMENTED,
                "HTTP trailers are not supported\n",
            );
        }
        Err(BodyReadError::Invalid) => {
            return error_response(StatusCode::BAD_REQUEST, "Could not read request body\n");
        }
    };
    if let Ok(length) = HeaderValue::from_str(&body.len().to_string()) {
        headers.insert(CONTENT_LENGTH, length);
    }

    // Keep the client's Host header when present. It is often used by the
    // backend for virtual-host routing, while the URI authority selects where
    // ExaProp opens the upstream connection.
    let outbound = match Request::builder()
        .method(method.clone())
        .version(version)
        .uri(uri)
        .body(Full::new(body))
    {
        Ok(mut request) => {
            *request.headers_mut() = headers;
            request
        }
        Err(error) => {
            eprintln!("Could not build backend request: {error}");
            return error_response(StatusCode::BAD_REQUEST, "Could not build backend request\n");
        }
    };

    let upstream = match timeout(IO_TIMEOUT, client.request(outbound)).await {
        Err(_) => {
            return error_response(StatusCode::GATEWAY_TIMEOUT, "Backend request timed out\n")
        }
        Ok(Err(error)) => {
            eprintln!("Backend request failed: {error}");
            return error_response(StatusCode::BAD_GATEWAY, "Backend request failed\n");
        }
        Ok(Ok(response)) => response,
    };

    let status = upstream.status();
    let version = upstream.version();
    let mut headers = upstream.headers().clone();
    strip_hop_by_hop_headers(&mut headers);

    let body = match read_limited_body(upstream.into_body()).await {
        Ok(body) => body,
        Err(BodyReadError::TooLarge) => {
            return error_response(StatusCode::BAD_GATEWAY, "Backend response exceeds 4 MiB\n");
        }
        Err(BodyReadError::Timeout) => {
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "Timed out reading backend response\n",
            );
        }
        Err(BodyReadError::Trailers) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "Backend response trailers are unsupported\n",
            );
        }
        Err(BodyReadError::Invalid) => {
            return error_response(StatusCode::BAD_GATEWAY, "Could not read backend response\n");
        }
    };

    // HEAD, 204, and 304 responses must not acquire a body length based on the
    // bytes collected here. Preserve any legal upstream Content-Length value.
    let body_is_forbidden = method == Method::HEAD
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::RESET_CONTENT
        || status == StatusCode::NOT_MODIFIED;
    if !body_is_forbidden {
        headers.remove(CONTENT_LENGTH);
        if let Ok(length) = HeaderValue::from_str(&body.len().to_string()) {
            headers.insert(CONTENT_LENGTH, length);
        }
    }

    match Response::builder()
        .status(status)
        .version(version)
        .body(Full::new(body))
    {
        Ok(mut response) => {
            *response.headers_mut() = headers;
            response
        }
        Err(error) => {
            eprintln!("Could not build client response: {error}");
            error_response(StatusCode::BAD_GATEWAY, "Invalid backend response\n")
        }
    }
}

#[derive(Debug)]
enum BodyReadError {
    TooLarge,
    Timeout,
    Trailers,
    Invalid,
}

async fn read_limited_body<B>(mut body: B) -> Result<Bytes, BodyReadError>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    read_limited_body_with_timeout(&mut body, IO_TIMEOUT).await
}

async fn read_limited_body_with_timeout<B>(
    body: &mut B,
    io_timeout: Duration,
) -> Result<Bytes, BodyReadError>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    let mut collected = BytesMut::new();

    loop {
        let next = timeout(io_timeout, body.frame())
            .await
            .map_err(|_| BodyReadError::Timeout)?;
        let Some(frame) = next else {
            break;
        };
        let frame = frame.map_err(|error| {
            eprintln!("HTTP body read failed: {error}");
            BodyReadError::Invalid
        })?;

        match frame.into_data() {
            Ok(data) => {
                if collected.len().saturating_add(data.len()) > MAX_BODY_BYTES {
                    return Err(BodyReadError::TooLarge);
                }
                collected.extend_from_slice(&data);
            }
            Err(frame) if frame.is_trailers() => return Err(BodyReadError::Trailers),
            Err(_) => return Err(BodyReadError::Invalid),
        }
    }

    Ok(collected.freeze())
}

/// Remove hop-by-hop headers, including headers nominated by `Connection`.
/// These headers describe one transport connection and must not be forwarded
/// unchanged through a proxy.
fn strip_hop_by_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();

    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    for name in nominated {
        headers.remove(name);
    }
}

fn error_response(status: StatusCode, message: &'static str) -> Response<Full<Bytes>> {
    let body = Bytes::from_static(message.as_bytes());
    let mut response = Response::new(Full::new(body.clone()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&body.len().to_string()).expect("a byte length is a valid header"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::{body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::{TokioIo, TokioTimer};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

    async fn start_proxy(backend: SocketAddr) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            run_listener(listener, backend, build_client(), async {
                let _ = shutdown_rx.await;
            })
            .await;
        });
        (address, shutdown_tx, task)
    }

    async fn start_backend<F, Fut>(handler: F) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>)
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<Full<Bytes>>> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    result = listener.accept() => {
                        let Ok((stream, _)) = result else { continue };
                        let handler = Arc::clone(&handler);
                        tokio::spawn(async move {
                            let service = service_fn(move |request| {
                                let handler = Arc::clone(&handler);
                                async move { Ok::<_, Infallible>(handler(request).await) }
                            });
                            let _ = http1::Builder::new()
                                .timer(TokioTimer::new())
                                .serve_connection(TokioIo::new(stream), service)
                                .await;
                        });
                    }
                }
            }
        });
        (address, shutdown_tx, task)
    }

    fn test_client() -> Client<HttpConnector, Full<Bytes>> {
        Client::builder(TokioExecutor::new()).build(HttpConnector::new())
    }

    fn request(uri: String, method: Method, body: &'static [u8]) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Full::new(Bytes::from_static(body)))
            .unwrap()
    }

    async fn stop_server(tx: oneshot::Sender<()>, task: JoinHandle<()>) {
        let _ = tx.send(());
        let _ = timeout(Duration::from_secs(4), task).await;
    }

    #[tokio::test]
    async fn forwards_get_path_query_status_and_headers() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            let mut response = Response::new(Full::new(Bytes::from(request.uri().to_string())));
            *response.status_mut() = StatusCode::ACCEPTED;
            response
                .headers_mut()
                .insert("x-backend", HeaderValue::from_static("yes"));
            response
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(
                format!("http://{proxy}/search?q=rust"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(response.headers()["x-backend"], "yes");
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            b"/search?q=rust"
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn forwards_post_body() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            Response::new(Full::new(body))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(
                format!("http://{proxy}/submit"),
                Method::POST,
                b"name=exaprop",
            ))
            .await
            .unwrap();
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            b"name=exaprop"
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn forwards_chunked_request_body() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            Response::new(Full::new(body))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let mut stream = TcpStream::connect(proxy).await.unwrap();
        stream
            .write_all(
                b"POST /chunked HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n",
            )
            .await
            .unwrap();

        let mut response = Vec::new();
        timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response
            .windows(b"Wikipedia".len())
            .any(|window| window == b"Wikipedia"));

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn handles_head_without_forwarding_a_response_body() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            let mut response = Response::new(Full::new(Bytes::from_static(b"hello")));
            response
                .headers_mut()
                .insert(CONTENT_LENGTH, HeaderValue::from_static("5"));
            response
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(format!("http://{proxy}/head"), Method::HEAD, b""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_LENGTH], "5");
        assert!(response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn forwards_binary_response_without_corruption() {
        let binary = Bytes::from_static(&[0, 159, 146, 150, 255, 0]);
        let expected = binary.clone();
        let (backend, backend_stop, backend_task) = start_backend(move |_| {
            let binary = binary.clone();
            async move { Response::new(Full::new(binary)) }
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(format!("http://{proxy}/binary"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn handles_sequential_requests_and_concurrent_clients() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            Response::new(Full::new(Bytes::from(request.uri().path().to_owned())))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let client = test_client();

        for path in ["/one", "/two"] {
            let response = client
                .request(request(format!("http://{proxy}{path}"), Method::GET, b""))
                .await
                .unwrap();
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                path.as_bytes()
            );
        }

        let client = test_client();
        let (first, second) = tokio::join!(
            client.request(request(format!("http://{proxy}/a"), Method::GET, b"")),
            client.request(request(format!("http://{proxy}/b"), Method::GET, b"")),
        );
        assert!(first.is_ok());
        assert!(second.is_ok());

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn reports_backend_unavailable_without_panicking() {
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = reservation.local_addr().unwrap();
        drop(reservation);
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(format!("http://{proxy}/failure"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        stop_server(proxy_stop, proxy_task).await;
    }

    #[tokio::test]
    async fn rejects_upgrade_requests() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let mut upgrade = request(format!("http://{proxy}/upgrade"), Method::GET, b"");
        upgrade
            .headers_mut()
            .insert(UPGRADE, HeaderValue::from_static("websocket"));

        let response = test_client().request(upgrade).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn malformed_http_request_gets_a_bad_request_response() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let mut stream = TcpStream::connect(proxy).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\ninvalid header\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400"));

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn client_disconnect_does_not_prevent_another_client_from_working() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::from_static(b"healthy"))) })
                .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let mut disconnected = TcpStream::connect(proxy).await.unwrap();
        disconnected
            .write_all(
                b"POST /partial HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\nbody",
            )
            .await
            .unwrap();
        drop(disconnected);

        let response = test_client()
            .request(request(format!("http://{proxy}/healthy"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            b"healthy"
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn rejects_oversized_request_body() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;
        let body = vec![b'x'; MAX_BODY_BYTES + 1];
        let response = test_client()
            .request(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("http://{proxy}/large"))
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn rejects_oversized_backend_response() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            Response::new(Full::new(Bytes::from(vec![b'x'; MAX_BODY_BYTES + 1])))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(format!("http://{proxy}/large"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn times_out_when_a_body_stops_producing_frames() {
        use std::{convert::Infallible, task::Poll};

        struct PendingBody;

        impl Body for PendingBody {
            type Data = Bytes;
            type Error = Infallible;

            fn poll_frame(
                self: std::pin::Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
                Poll::Pending
            }
        }

        let result =
            read_limited_body_with_timeout(&mut PendingBody, Duration::from_millis(10)).await;
        assert!(matches!(result, Err(BodyReadError::Timeout)));
    }

    #[test]
    fn strips_standard_and_connection_nominated_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONNECTION,
            HeaderValue::from_static("x-private, keep-alive"),
        );
        headers.insert("x-private", HeaderValue::from_static("remove"));
        headers.insert("x-end-to-end", HeaderValue::from_static("keep"));

        strip_hop_by_hop_headers(&mut headers);

        assert!(!headers.contains_key(CONNECTION));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key("x-private"));
        assert_eq!(headers["x-end-to-end"], "keep");
    }
}

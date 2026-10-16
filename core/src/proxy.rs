//! A small, bounded HTTP/1.1 reverse proxy.
//!
//! Hyper owns HTTP framing and parsing; this module only changes the request
//! destination, removes hop-by-hop headers, and relays bounded bodies. Bodies
//! are buffered up to [`MAX_BODY_BYTES`] so every in-flight request has a
//! predictable memory ceiling.

use std::{
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

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
    sync::{mpsc::Sender, Semaphore},
    task::JoinSet,
    time::timeout,
};

use crate::observation::{Observation, RequestOutcome};

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
    run_with_observer(
        listen_addr,
        backend_addr,
        "default-service".to_string(),
        None,
    )
    .await
}

/// Start a proxy on `listen_addr`, tagging emitted observations with `service_name`
/// and forwarding them through `observer` channel if provided.
pub async fn run_with_observer(
    listen_addr: SocketAddr,
    backend_addr: SocketAddr,
    service_name: String,
    observer: Option<Sender<Observation>>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    let actual_addr = listener.local_addr()?;
    let client = build_client();

    eprintln!(
        "ExaProp proxy listening on http://{actual_addr}, forwarding to http://{backend_addr}"
    );

    let service_name: Arc<str> = Arc::from(service_name);
    run_listener(
        listener,
        backend_addr,
        client,
        service_name,
        observer,
        async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("Could not listen for Ctrl-C: {error}");
            }
        },
    )
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
    service_name: Arc<str>,
    observer: Option<Sender<Observation>>,
    shutdown: F,
) where
    F: Future<Output = ()> + Send,
{
    run_listener_with_timeout(
        listener,
        backend_addr,
        client,
        service_name,
        observer,
        IO_TIMEOUT,
        shutdown,
    )
    .await;
}

async fn run_listener_with_timeout<F>(
    listener: TcpListener,
    backend_addr: SocketAddr,
    client: ProxyClient,
    service_name: Arc<str>,
    observer: Option<Sender<Observation>>,
    io_timeout: Duration,
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
                let service_name = service_name.clone();
                let observer = observer.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(
                        stream,
                        backend_addr,
                        client,
                        service_name,
                        observer,
                        io_timeout,
                    )
                    .await
                    {
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
    service_name: Arc<str>,
    observer: Option<Sender<Observation>>,
    io_timeout: Duration,
) -> Result<(), hyper::Error> {
    let service = service_fn(move |request| {
        let client = client.clone();
        let service_name = service_name.clone();
        let observer = observer.clone();
        async move {
            Ok::<_, Infallible>(
                handle_request(
                    request,
                    backend_addr,
                    client,
                    service_name,
                    observer,
                    io_timeout,
                )
                .await,
            )
        }
    });

    http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(io_timeout)
        .serve_connection(TokioIo::new(stream), service)
        .await
}

async fn handle_request(
    request: Request<Incoming>,
    backend_addr: SocketAddr,
    client: ProxyClient,
    service_name: Arc<str>,
    observer: Option<Sender<Observation>>,
    io_timeout: Duration,
) -> Response<Full<Bytes>> {
    let start_instant = Instant::now();
    let start_timestamp = SystemTime::now();

    let method = request.method().clone();
    let version = request.version();
    let raw_path_and_query = request
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str())
        .to_string();

    let emit = |status: StatusCode, outcome: RequestOutcome, response: Response<Full<Bytes>>| {
        if let Some(ref tx) = observer {
            let obs = Observation::new(
                start_timestamp,
                service_name.to_string(),
                method.clone(),
                &raw_path_and_query,
                status,
                start_instant.elapsed(),
                outcome,
            );
            let _ = tx.try_send(obs);
        }
        response
    };

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
        return emit(
            StatusCode::NOT_IMPLEMENTED,
            RequestOutcome::Rejected,
            error_response(
                StatusCode::NOT_IMPLEMENTED,
                "HTTP upgrades are not supported\n",
            ),
        );
    }

    if request.headers().contains_key(EXPECT) {
        return emit(
            StatusCode::EXPECTATION_FAILED,
            RequestOutcome::Rejected,
            error_response(
                StatusCode::EXPECTATION_FAILED,
                "Expect headers are not supported\n",
            ),
        );
    }

    if request.headers().contains_key("trailer") {
        return emit(
            StatusCode::NOT_IMPLEMENTED,
            RequestOutcome::Rejected,
            error_response(
                StatusCode::NOT_IMPLEMENTED,
                "HTTP trailers are not supported\n",
            ),
        );
    }

    let uri = match format!("http://{backend_addr}{raw_path_and_query}").parse::<Uri>() {
        Ok(uri) => uri,
        Err(error) => {
            eprintln!("Could not construct backend URI: {error}");
            return emit(
                StatusCode::BAD_REQUEST,
                RequestOutcome::ClientFailure,
                error_response(StatusCode::BAD_REQUEST, "Invalid request target\n"),
            );
        }
    };

    let mut headers = request.headers().clone();
    strip_hop_by_hop_headers(&mut headers);
    headers.remove(CONTENT_LENGTH);

    let body = match read_limited_body_with_timeout(&mut request.into_body(), io_timeout).await {
        Ok(body) => body,
        Err(BodyReadError::TooLarge) => {
            return emit(
                StatusCode::PAYLOAD_TOO_LARGE,
                RequestOutcome::Rejected,
                error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Request body exceeds 4 MiB\n",
                ),
            );
        }
        Err(BodyReadError::Timeout) => {
            return emit(
                StatusCode::REQUEST_TIMEOUT,
                RequestOutcome::ClientTimeout,
                error_response(
                    StatusCode::REQUEST_TIMEOUT,
                    "Timed out reading request body\n",
                ),
            );
        }
        Err(BodyReadError::Trailers) => {
            return emit(
                StatusCode::NOT_IMPLEMENTED,
                RequestOutcome::Rejected,
                error_response(
                    StatusCode::NOT_IMPLEMENTED,
                    "HTTP trailers are not supported\n",
                ),
            );
        }
        Err(BodyReadError::Invalid) => {
            return emit(
                StatusCode::BAD_REQUEST,
                RequestOutcome::ClientFailure,
                error_response(StatusCode::BAD_REQUEST, "Could not read request body\n"),
            );
        }
    };
    // RFC 9112 §6.2: A client SHOULD NOT send Content-Length when the request
    // has no body and the method semantics do not anticipate one (e.g. GET, HEAD).
    if !body.is_empty() || (method != Method::GET && method != Method::HEAD) {
        if let Ok(length) = HeaderValue::from_str(&body.len().to_string()) {
            headers.insert(CONTENT_LENGTH, length);
        }
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
            return emit(
                StatusCode::BAD_REQUEST,
                RequestOutcome::ClientFailure,
                error_response(StatusCode::BAD_REQUEST, "Could not build backend request\n"),
            );
        }
    };

    let upstream = match timeout(io_timeout, client.request(outbound)).await {
        Err(_) => {
            return emit(
                StatusCode::GATEWAY_TIMEOUT,
                RequestOutcome::BackendTimeout,
                error_response(StatusCode::GATEWAY_TIMEOUT, "Backend request timed out\n"),
            );
        }
        Ok(Err(error)) => {
            eprintln!("Backend request failed: {error}");
            return emit(
                StatusCode::BAD_GATEWAY,
                RequestOutcome::BackendFailure,
                error_response(StatusCode::BAD_GATEWAY, "Backend request failed\n"),
            );
        }
        Ok(Ok(response)) => response,
    };

    let status = upstream.status();
    let version = upstream.version();
    let mut headers = upstream.headers().clone();
    strip_hop_by_hop_headers(&mut headers);

    let body = match read_limited_body_with_timeout(&mut upstream.into_body(), io_timeout).await {
        Ok(body) => body,
        Err(BodyReadError::TooLarge) => {
            return emit(
                StatusCode::BAD_GATEWAY,
                RequestOutcome::BackendFailure,
                error_response(StatusCode::BAD_GATEWAY, "Backend response exceeds 4 MiB\n"),
            );
        }
        Err(BodyReadError::Timeout) => {
            return emit(
                StatusCode::GATEWAY_TIMEOUT,
                RequestOutcome::BackendTimeout,
                error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Timed out reading backend response\n",
                ),
            );
        }
        Err(BodyReadError::Trailers) => {
            return emit(
                StatusCode::BAD_GATEWAY,
                RequestOutcome::BackendFailure,
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "Backend response trailers are unsupported\n",
                ),
            );
        }
        Err(BodyReadError::Invalid) => {
            return emit(
                StatusCode::BAD_GATEWAY,
                RequestOutcome::BackendFailure,
                error_response(StatusCode::BAD_GATEWAY, "Could not read backend response\n"),
            );
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
    } else if status == StatusCode::NO_CONTENT || status == StatusCode::RESET_CONTENT {
        // RFC 9110 §15.3.5: 204 and 205 responses MUST NOT include Content-Length.
        headers.remove(CONTENT_LENGTH);
    }

    let response_body = if body_is_forbidden {
        Bytes::new()
    } else {
        body
    };

    match Response::builder()
        .status(status)
        .version(version)
        .body(Full::new(response_body))
    {
        Ok(mut response) => {
            *response.headers_mut() = headers;
            emit(status, RequestOutcome::Success, response)
        }
        Err(error) => {
            eprintln!("Could not build client response: {error}");
            emit(
                StatusCode::BAD_GATEWAY,
                RequestOutcome::BackendFailure,
                error_response(StatusCode::BAD_GATEWAY, "Invalid backend response\n"),
            )
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

#[allow(dead_code)]
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
        start_proxy_with_timeout_and_observer(backend, "test-service", None, IO_TIMEOUT).await
    }

    async fn start_proxy_with_observer(
        backend: SocketAddr,
        service_name: &str,
        observer: Option<Sender<Observation>>,
    ) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
        start_proxy_with_timeout_and_observer(backend, service_name, observer, IO_TIMEOUT).await
    }

    async fn start_proxy_with_timeout_and_observer(
        backend: SocketAddr,
        service_name: &str,
        observer: Option<Sender<Observation>>,
        io_timeout: Duration,
    ) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let service_name: Arc<str> = Arc::from(service_name);
        let task = tokio::spawn(async move {
            run_listener_with_timeout(
                listener,
                backend,
                build_client(),
                service_name,
                observer,
                io_timeout,
                async {
                    let _ = shutdown_rx.await;
                },
            )
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

    #[tokio::test]
    async fn forwards_no_content_and_not_modified_without_body() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            if request.uri().path() == "/no-content" {
                let mut res = Response::new(Full::new(Bytes::new()));
                *res.status_mut() = StatusCode::NO_CONTENT;
                res.headers_mut()
                    .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
                res
            } else {
                let mut res = Response::new(Full::new(Bytes::new()));
                *res.status_mut() = StatusCode::NOT_MODIFIED;
                res.headers_mut()
                    .insert("etag", HeaderValue::from_static("\"abc-123\""));
                res.headers_mut()
                    .insert(CONTENT_LENGTH, HeaderValue::from_static("42"));
                res
            }
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let client = test_client();

        let res204 = client
            .request(request(
                format!("http://{proxy}/no-content"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res204.status(), StatusCode::NO_CONTENT);
        assert!(!res204.headers().contains_key(CONTENT_LENGTH));
        let body204 = res204.into_body().collect().await.unwrap().to_bytes();
        assert!(body204.is_empty());

        let res304 = client
            .request(request(
                format!("http://{proxy}/not-modified"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res304.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(res304.headers()["etag"], "\"abc-123\"");
        assert!(!res304.headers().contains_key(CONTENT_LENGTH));
        let body304 = res304.into_body().collect().await.unwrap().to_bytes();
        assert!(body304.is_empty());

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn accepts_body_at_exact_limit() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body.len(), MAX_BODY_BYTES);
            Response::new(Full::new(Bytes::from("exact_limit_ok")))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let payload = vec![b'z'; MAX_BODY_BYTES];
        let response = test_client()
            .request(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("http://{proxy}/exact"))
                    .body(Full::new(Bytes::from(payload)))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(resp_body.as_ref(), b"exact_limit_ok");

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn rejects_expect_header() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let mut req = request(format!("http://{proxy}/expect"), Method::POST, b"data");
        req.headers_mut()
            .insert(EXPECT, HeaderValue::from_static("100-continue"));
        let response = test_client().request(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::EXPECTATION_FAILED);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn rejects_trailer_header() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let mut req = request(format!("http://{proxy}/trailer"), Method::POST, b"data");
        req.headers_mut()
            .insert("trailer", HeaderValue::from_static("x-checksum"));
        let response = test_client().request(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn rejects_connect_method() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let req = Request::builder()
            .method(Method::CONNECT)
            .uri(format!("http://{proxy}/tunnel"))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = test_client().request(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn preserves_duplicate_headers() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            let mut res = Response::new(Full::new(Bytes::from("cookie_ok")));
            res.headers_mut()
                .append("set-cookie", HeaderValue::from_static("session=123"));
            res.headers_mut()
                .append("set-cookie", HeaderValue::from_static("theme=dark"));
            res
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(format!("http://{proxy}/cookies"), Method::GET, b""))
            .await
            .unwrap();

        let cookies: Vec<&str> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(cookies, vec!["session=123", "theme=dark"]);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn omits_content_length_on_empty_get_request() {
        let (backend, backend_stop, backend_task) = start_backend(|request| async move {
            assert!(!request.headers().contains_key(CONTENT_LENGTH));
            Response::new(Full::new(Bytes::from("no_cl_ok")))
        })
        .await;
        let (proxy, proxy_stop, proxy_task) = start_proxy(backend).await;

        let response = test_client()
            .request(request(
                format!("http://{proxy}/empty-get"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn records_successful_get_observation() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::from("hello_obs"))) }).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "api-gateway", Some(tx)).await;

        let res = test_client()
            .request(request(
                format!("http://{proxy}/items?page=1"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.service_name, "api-gateway");
        assert_eq!(obs.method, Method::GET);
        assert_eq!(obs.path, "/items");
        assert_eq!(obs.query.as_deref(), Some("page=1"));
        assert_eq!(obs.status, StatusCode::OK);
        assert_eq!(obs.outcome, RequestOutcome::Success);
        assert!(obs.duration >= Duration::ZERO);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn records_successful_post_observation() {
        let (backend, backend_stop, backend_task) = start_backend(|req| async move {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            Response::new(Full::new(body))
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "orders-service", Some(tx)).await;

        let res = test_client()
            .request(request(
                format!("http://{proxy}/orders"),
                Method::POST,
                b"order_id=42",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.service_name, "orders-service");
        assert_eq!(obs.method, Method::POST);
        assert_eq!(obs.path, "/orders");
        assert_eq!(obs.status, StatusCode::OK);
        assert_eq!(obs.outcome, RequestOutcome::Success);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn records_backend_500_as_completed_exchange() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            let mut res = Response::new(Full::new(Bytes::from("server_error")));
            *res.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            res
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "payment-svc", Some(tx)).await;

        let res = test_client()
            .request(request(format!("http://{proxy}/charge"), Method::POST, b""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(obs.outcome, RequestOutcome::Success);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn distinguishes_backend_failure_from_backend_502() {
        let dead_backend: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(dead_backend, "dead-svc", Some(tx.clone())).await;

        let res = test_client()
            .request(request(format!("http://{proxy}/fail"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);

        let obs_fail = rx.recv().await.expect("observation for failure");
        assert_eq!(obs_fail.status, StatusCode::BAD_GATEWAY);
        assert_eq!(obs_fail.outcome, RequestOutcome::BackendFailure);

        stop_server(proxy_stop, proxy_task).await;

        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            let mut res = Response::new(Full::new(Bytes::from("deliberate_502")));
            *res.status_mut() = StatusCode::BAD_GATEWAY;
            res
        })
        .await;
        let (proxy2, proxy_stop2, proxy_task2) =
            start_proxy_with_observer(backend, "live-svc", Some(tx)).await;

        let res2 = test_client()
            .request(request(
                format!("http://{proxy2}/upstream-502"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::BAD_GATEWAY);

        let obs_live = rx.recv().await.expect("observation for completed 502");
        assert_eq!(obs_live.status, StatusCode::BAD_GATEWAY);
        assert_eq!(obs_live.outcome, RequestOutcome::Success);

        stop_server(proxy_stop2, proxy_task2).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn records_backend_timeout_observation() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Response::new(Full::new(Bytes::from("delayed_ok")))
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) = start_proxy_with_timeout_and_observer(
            backend,
            "timeout-svc",
            Some(tx),
            Duration::from_millis(20),
        )
        .await;

        let res = test_client()
            .request(request(format!("http://{proxy}/timeout"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(obs.outcome, RequestOutcome::BackendTimeout);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn records_request_rejection_observation() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::new())) }).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "reject-svc", Some(tx)).await;

        let mut req = request(format!("http://{proxy}/expect"), Method::POST, b"data");
        req.headers_mut()
            .insert(EXPECT, HeaderValue::from_static("100-continue"));
        let res = test_client().request(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::EXPECTATION_FAILED);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.status, StatusCode::EXPECTATION_FAILED);
        assert_eq!(obs.outcome, RequestOutcome::Rejected);

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn captures_accurate_method_and_path() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::from("path_ok"))) }).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "catalog-svc", Some(tx)).await;

        let res = test_client()
            .request(request(
                format!("http://{proxy}/catalog/v2/items/99"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.method, Method::GET);
        assert_eq!(obs.path, "/catalog/v2/items/99");

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn monotonic_duration_is_non_negative() {
        let (backend, backend_stop, backend_task) = start_backend(|_| async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Response::new(Full::new(Bytes::from("sleep_ok")))
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "latency-svc", Some(tx)).await;

        let res = test_client()
            .request(request(format!("http://{proxy}/latency"), Method::GET, b""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let obs = rx.recv().await.expect("observation emitted");
        assert!(obs.duration >= Duration::from_millis(4));

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn emits_exactly_one_observation_per_request() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::from("ok"))) }).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "single-svc", Some(tx)).await;

        let client = test_client();
        for i in 0..3 {
            let res = client
                .request(request(format!("http://{proxy}/req/{i}"), Method::GET, b""))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let obs = rx.recv().await.expect("one observation per request");
            assert_eq!(obs.path, format!("/req/{i}"));
        }

        let extra = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(extra.is_err());

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[tokio::test]
    async fn concurrent_requests_produce_independent_observations() {
        let (backend, backend_stop, backend_task) = start_backend(|req| async move {
            let path = req.uri().path().to_string();
            Response::new(Full::new(Bytes::from(path)))
        })
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(20);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "concurrent-svc", Some(tx)).await;

        let mut handles = Vec::new();
        for i in 0..5 {
            let client = test_client();
            let url = format!("http://{proxy}/conc/{i}");
            handles.push(tokio::spawn(async move {
                client
                    .request(request(url, Method::GET, b""))
                    .await
                    .unwrap()
            }));
        }

        for handle in handles {
            let res = handle.await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        let mut received_paths = Vec::new();
        for _ in 0..5 {
            let obs = rx.recv().await.expect("observation emitted");
            received_paths.push(obs.path);
        }
        received_paths.sort();
        assert_eq!(
            received_paths,
            vec!["/conc/0", "/conc/1", "/conc/2", "/conc/3", "/conc/4"]
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }

    #[test]
    fn no_request_or_response_bodies_recorded() {
        let obs = Observation::new(
            SystemTime::now(),
            "svc".to_string(),
            Method::POST,
            "/data",
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        );

        assert_eq!(obs.method, Method::POST);
        assert_eq!(obs.path, "/data");
        assert_eq!(obs.status, StatusCode::OK);
        assert_eq!(obs.outcome, RequestOutcome::Success);
    }

    #[tokio::test]
    async fn redacts_sensitive_query_parameters() {
        let (backend, backend_stop, backend_task) =
            start_backend(|_| async { Response::new(Full::new(Bytes::from("redacted_ok"))) }).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let (proxy, proxy_stop, proxy_task) =
            start_proxy_with_observer(backend, "secure-svc", Some(tx)).await;

        let res = test_client()
            .request(request(
                format!("http://{proxy}/account?token=supersecret123&page=5&api_key=privkey"),
                Method::GET,
                b"",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let obs = rx.recv().await.expect("observation emitted");
        assert_eq!(obs.path, "/account");
        assert_eq!(
            obs.query.as_deref(),
            Some("token=[REDACTED]&page=5&api_key=[REDACTED]")
        );

        stop_server(proxy_stop, proxy_task).await;
        stop_server(backend_stop, backend_task).await;
    }
}

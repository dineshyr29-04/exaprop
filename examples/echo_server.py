"""A tiny local backend for manually trying ExaProp's HTTP proxy."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class EchoHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        self._send(200, f"GET {self.path}\n".encode())

    def do_HEAD(self):
        body = f"HEAD {self.path}\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        self._send(200, body)

    def _send(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    address = ("127.0.0.1", 8081)
    print(f"Example backend listening on http://{address[0]}:{address[1]}")
    ThreadingHTTPServer(address, EchoHandler).serve_forever()

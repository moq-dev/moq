"""Serve the harness and store POSTed results. Usage: serve.py <www> <results> <port>"""
import http.server, os, sys, urllib.parse

www, results, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
os.makedirs(results, exist_ok=True)

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=www, **kw)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        name = "".join(c for c in q["browser"][0] if c.isalnum() or c in "-_")
        body = self.rfile.read(int(self.headers["Content-Length"]))
        if self.path.startswith("/log"):
            with open(f"{results}/{name}.progress", "ab") as f:
                f.write(body + b"\n")
            self.send_response(204)
            self.end_headers()
            return
        tmp = f"{results}/{name}.json.part"
        open(tmp, "wb").write(body)
        os.replace(tmp, f"{results}/{name}.json")
        self.send_response(204)
        self.end_headers()

    def log_message(self, *a):
        pass

http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()

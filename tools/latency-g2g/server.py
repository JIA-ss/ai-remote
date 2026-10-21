"""Local helper for probe.html: serves the page, relays ai-remote's live view from the controller
agent (/ours?...) as a streamed HTTP body, and appends results (POST /log) to results.jsonl next
to this file. G2G_SOCK: the controller agent's socket (default: the default profile's). macOS.
"""
import http.server, json, socket, sys, urllib.parse, os
HERE = os.path.dirname(os.path.abspath(__file__))
SOCK = os.environ.get("G2G_SOCK") or os.path.expanduser("~/Library/Application Support/ai-remote/controller/control.sock")
LOG = open(os.path.join(HERE, "results.jsonl"), "a")
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def log_message(self, *a): pass
    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        if u.path in ("/", "/probe.html"):
            body = open(os.path.join(HERE, "probe.html"), "rb").read()
            self.send_response(200); self.send_header("Content-Type", "text/html"); self.end_headers(); self.wfile.write(body); return
        if u.path == "/ours":
            q = {k: v[0] for k, v in urllib.parse.parse_qs(u.query).items()}
            req = {"max_fps": int(q["fps"]), "max_long_edge": int(q["edge"]), "bitrate_kbps": int(q["kbps"]), "codecs": q["codecs"].split(","), "cursor": "client"}
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(SOCK)
            s.sendall((json.dumps({"op": "screen", "host": q["host"], "request": req}) + "\n").encode())
            buf = b""
            while b"\n" not in buf:
                buf += s.recv(65536)
            line, rest = buf.split(b"\n", 1)
            sys.stderr.write("reply " + line.decode()[:300] + "\n")
            self.send_response(200); self.send_header("Content-Type", "application/octet-stream"); self.send_header("Cache-Control", "no-store"); self.end_headers()
            try:
                if rest:
                    self.wfile.write(rest); self.wfile.flush()
                while True:
                    d = s.recv(1 << 20)
                    if not d:
                        break
                    self.wfile.write(d); self.wfile.flush()
            except Exception:
                pass
            s.close(); return
        self.send_response(404); self.end_headers()
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0)); data = self.rfile.read(n)
        LOG.write(data.decode() + "\n"); LOG.flush()
        self.send_response(204); self.end_headers()
http.server.ThreadingHTTPServer(("127.0.0.1", 18780), H).serve_forever()

#!/usr/bin/env python3
"""Fake Immich server for development and end-to-end tests.

Usage: python3 -I tools/fake_immich.py PORT OUT_DIR
Answers GET /api/users/me, POST /api/assets (saves the JPEG to OUT_DIR),
PUT /api/albums/<id>/assets, and logs every request to OUT_DIR/requests.log.
Point the booth at it with [immich] base_url = "http://127.0.0.1:PORT/api".
"""
import http.server, json, sys, os, uuid, re
OUT = sys.argv[2]
os.makedirs(OUT, exist_ok=True)
log = open(os.path.join(OUT, "requests.log"), "a", buffering=1)
class H(http.server.BaseHTTPRequestHandler):
    def _reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        log.write(f"GET {self.path} key={self.headers.get('x-api-key')}\n")
        if self.path == "/api/users/me": return self._reply(200, {"name": "Booth", "email": "booth@example.com"})
        self._reply(404, {})
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); data = self.rfile.read(n)
        fields = re.findall(rb'name="([^"]+)"(?:; filename="([^"]+)")?\r\n(?:Content-Type: [^\r]+\r\n)?\r\n', data)
        log.write(f"POST {self.path} key={self.headers.get('x-api-key')} bytes={n} fields={[f[0].decode() for f in fields]}\n")
        m = re.search(rb'name="fileCreatedAt"\r\n\r\n([^\r]+)', data); log.write(f"  fileCreatedAt={m.group(1).decode() if m else None}\n")
        i = data.find(b"\xff\xd8"); j = data.rfind(b"\xff\xd9")
        if i >= 0 and j > i: open(os.path.join(OUT, f"upload-{uuid.uuid4().hex[:6]}.jpg"), "wb").write(data[i:j+2])
        self._reply(201, {"id": str(uuid.uuid4()), "status": "created"})
    def do_PUT(self):
        n = int(self.headers.get("content-length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        log.write(f"PUT {self.path} body={body}\n")
        self._reply(200, [{"id": i, "success": True} for i in body.get("ids", [])])
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()

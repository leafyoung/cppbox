#!/usr/bin/env python3
"""Faithful local mock of the Cloudflare Worker (same routes), for testing
push -> submit -> pull -> organize without a Cloudflare account.
Run: python3 worker/mock_server.py [port]"""
import json, os, sys, zipfile, io, time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, unquote, parse_qs

ROOT = os.path.dirname(os.path.abspath(__file__))
R2_DIR = os.path.join(ROOT, "_mock_r2")
KV_FILE = os.path.join(ROOT, "_mock_kv.json")
os.makedirs(R2_DIR, exist_ok=True)
SECRET = os.environ.get("MOCK_SECRET", "dev-secret")

FORM = """<!doctype html><html><body style="font-family:sans-serif;max-width:480px;margin:40px auto">
<h2>CPPBox mock submit</h2><form method=post action=/submit enctype=multipart/form-data>
<input name=key placeholder="key" style="width:100%;padding:8px"><p>
<input type=file name=files multiple><p>
<button>Submit</button></form><div id=r></div></body></html>"""


def load_keys():
    if os.path.exists(KV_FILE):
        return json.load(open(KV_FILE))
    return []


def save_keys(keys):
    json.dump(sorted(set(keys)), open(KV_FILE, "w"))


class H(BaseHTTPRequestHandler):
    def _send(self, code, body=b"", ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, code, obj):
        self._send(code, json.dumps(obj).encode())

    def _authed(self):
        return self.headers.get("X-Admin-Secret") == SECRET

    def do_GET(self):
        p = urlparse(self.path).path
        if p in ("/", "/index.html"):
            return self._send(200, FORM.encode(), "text/html")
        if p == "/health":
            return self._json(200, {"ok": True, "time": int(time.time() * 1000)})
        if p == "/admin/list":
            if not self._authed():
                return self._json(401, {"error": "unauthorized"})
            objs = [{"name": f, "size": os.path.getsize(os.path.join(R2_DIR, f))}
                    for f in sorted(os.listdir(R2_DIR)) if f.endswith(".zip")]
            return self._json(200, {"objects": objs})
        if p.startswith("/admin/object/"):
            if not self._authed():
                return self._json(401, {"error": "unauthorized"})
            name = unquote(p[len("/admin/object/"):])
            fp = os.path.join(R2_DIR, name)
            if not os.path.exists(fp):
                return self._json(404, {"error": "not found"})
            data = open(fp, "rb").read()
            return self._send(200, data, "application/zip")
        return self._json(404, {"error": "not found"})

    def do_DELETE(self):
        p = urlparse(self.path).path
        if not self._authed():
            return self._json(401, {"error": "unauthorized"})
        if p.startswith("/admin/object/"):
            name = unquote(p[len("/admin/object/"):])
            fp = os.path.join(R2_DIR, name)
            if os.path.exists(fp):
                os.remove(fp)
            return self._json(200, {"ok": True})
        self._json(404, {"error": "not found"})

    def do_POST(self):
        p = urlparse(self.path).path
        if p == "/admin/keys":
            if not self._authed():
                return self._json(401, {"error": "unauthorized"})
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or "{}")
            keys = [k for k in body.get("keys", []) if isinstance(k, str) and k.strip()]
            cur = load_keys()
            merged = sorted(set(cur) | set(keys))
            save_keys(merged)
            return self._json(200, {"ok": True, "total": len(merged), "added": len(merged) - len(cur)})
        if p == "/submit":
            return self._handle_submit()
        self._json(404, {"error": "not found"})

    def _handle_submit(self):
        ctype = self.headers.get("Content-Type", "")
        if "multipart/form-data" not in ctype:
            return self._json(400, {"error": "multipart required"})
        # crude multipart parse
        boundary = ctype.split("boundary=")[1].encode()
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        parts = raw.split(b"--" + boundary)
        key = None
        files = {}
        for part in parts:
            if b"Content-Disposition" not in part:
                continue
            head, _, body = part.partition(b"\r\n\r\n")
            head = head.decode("latin1")
            name = (parse_qs(head.split("\r\n")[0]) or {})
            disp = head.split("Content-Disposition:")[1] if "Content-Disposition:" in head else ""
            # extract name=
            import re
            nm = re.search(r'name="([^"]+)"', disp)
            fn = re.search(r'filename="([^"]*)"', disp)
            field = nm.group(1) if nm else ""
            if field == "key":
                key = body.rstrip(b"\r\n").decode()
            elif fn:
                fname = fn.group(1)
                if fname:
                    files[fname] = body.rstrip(b"\r\n")
        if not key:
            return self._json(400, {"error": "missing key"})
        if key not in load_keys():
            return self._json(403, {"error": "invalid or unissued key"})
        if not files:
            return self._json(400, {"error": "no files attached"})
        import time
        counter = int(time.time() * 1000)
        meta = {"key": key, "counter": counter, "submitted_at": datetime.now(timezone.utc).isoformat(), "files": list(files)}
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
            zf.writestr("meta.json", json.dumps(meta, indent=2))
            for n, c in files.items():
                zf.writestr(n, c)
        name = f"{key}+{counter}.zip"
        open(os.path.join(R2_DIR, name), "wb").write(buf.getvalue())
        self._json(200, {"ok": True, "name": name, "size": buf.tell(), "files": len(files)})

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8001
    print(f"mock worker on http://localhost:{port}  (secret={SECRET}, R2={R2_DIR})")
    ThreadingHTTPServer(("0.0.0.0", port), H).serve_forever()

#!/usr/bin/env python3
"""Alex share server: temporary, end-to-end encrypted file drop.

Stdlib only (Python 3.8+). Meant to sit behind Caddy (HTTPS) on 127.0.0.1.

Files arrive already encrypted by Alex (AES-256-GCM, key never needed by the
server for links). Each upload gets:
  * an unguessable id   -> link  https://host/s/<id>#<key>  (key stays in the
    URL fragment, which browsers never send to the server)
  * optionally a 6-digit code -> only then the sender also deposits the key
    here, so typing the code is enough (convenient, but this server can then
    decrypt that file). Code lookups are rate limited.
Everything expires (default 24 h) and is deleted.

Config via environment:
  ALEX_TOKEN      upload token (required for uploads)
  ALEX_DATA       storage dir           (default /var/lib/alex-share)
  ALEX_BIND       host:port             (default 127.0.0.1:8080)
  ALEX_MAX_MB     max size per upload   (default 1024)
  ALEX_QUOTA_GB   max total storage     (default 20)
  ALEX_TTL_HOURS  lifetime              (default 24)
"""
import hmac
import json
import os
import re
import secrets
import shutil
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = os.environ.get("ALEX_TOKEN", "")
DATA = os.environ.get("ALEX_DATA", "/var/lib/alex-share")
BIND = os.environ.get("ALEX_BIND", "127.0.0.1:8080")
MAX_BYTES = int(os.environ.get("ALEX_MAX_MB", "1024")) * 1024 * 1024
QUOTA = int(float(os.environ.get("ALEX_QUOTA_GB", "20")) * 1024 ** 3)
TTL = int(float(os.environ.get("ALEX_TTL_HOURS", "24")) * 3600)

ID_RE = re.compile(r"^[A-Za-z0-9_-]{22}$")
CODE_RE = re.compile(r"^\d{6}$")
KEY_RE = re.compile(r"^[A-Za-z0-9_-]{43}$")
# Browser pages on this host may call the API. Origin is scheme + host, no path.
CORS_ORIGINS = {"https://alex.tailieuptit.com", "http://alex.tailieuptit.com"}

lock = threading.Lock()
items = {}  # id -> meta dict
codes = {}  # code -> id
hits = {}   # ip -> [timestamps] for code lookups


def meta_path(i):
    return os.path.join(DATA, i + ".json")


def blob_path(i):
    return os.path.join(DATA, i + ".bin")


def load():
    os.makedirs(DATA, exist_ok=True)
    for name in os.listdir(DATA):
        if name.endswith(".part"):
            os.remove(os.path.join(DATA, name))
        if not name.endswith(".json"):
            continue
        try:
            with open(os.path.join(DATA, name)) as f:
                m = json.load(f)
            items[m["id"]] = m
            if m.get("code"):
                codes[m["code"]] = m["id"]
        except Exception:
            pass


def remove(i):
    m = items.pop(i, None)
    if m and m.get("code"):
        codes.pop(m["code"], None)
    for p in (meta_path(i), blob_path(i)):
        try:
            os.remove(p)
        except FileNotFoundError:
            pass


def used_bytes():
    return sum(m["size"] for m in items.values())


def cleaner():
    while True:
        now = time.time()
        with lock:
            for i in [i for i, m in items.items() if m["expires"] <= now]:
                remove(i)
            for ip in list(hits):
                hits[ip] = [t for t in hits[ip] if now - t < 600]
                if not hits[ip]:
                    del hits[ip]
        time.sleep(60)


PAGE = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "page.html"), encoding="utf-8").read()


class Handler(BaseHTTPRequestHandler):
    server_version = "AlexShare/1"
    sys_version = ""

    def log_message(self, fmt, *args):  # no file names / keys in logs
        pass

    def end_headers(self):
        origin = self.headers.get("Origin", "")
        if origin in CORS_ORIGINS:
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Vary", "Origin")
            self.send_header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Content-Type, X-Alex-Token, X-Alex-Key, X-Delete-Token")
            self.send_header("Access-Control-Max-Age", "600")
        super().end_headers()

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def client_ip(self):
        ip = self.client_address[0]
        if ip in ("127.0.0.1", "::1"):
            ip = self.headers.get("X-Forwarded-For", ip).split(",")[0].strip()
        return ip

    def send_json(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def authed(self):
        tok = self.headers.get("X-Alex-Token", "")
        return bool(TOKEN) and hmac.compare_digest(tok, TOKEN)

    # ------------------------------------------------------------- GET
    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html") or path.startswith("/s/"):
            body = PAGE.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header(
                "Content-Security-Policy",
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:",
            )
            self.end_headers()
            self.wfile.write(body)
            return
        if path == "/api/ping":
            return self.send_json(200, {"ok": True, "auth": self.authed(), "max_bytes": MAX_BYTES, "ttl": TTL})
        m = re.match(r"^/api/code/(\d{6})$", path)
        if m:
            ip = self.client_ip()
            with lock:
                h = hits.setdefault(ip, [])
                h[:] = [t for t in h if time.time() - t < 600]
                if len(h) >= 30:
                    return self.send_json(429, {"error": "Thử quá nhiều lần, đợi 10 phút"})
                h.append(time.time())
                i = codes.get(m.group(1))
                meta = items.get(i) if i else None
            if not meta or not meta.get("key"):
                return self.send_json(404, {"error": "Mã không đúng hoặc đã hết hạn"})
            return self.send_json(200, {"id": i, "key": meta["key"], "size": meta["size"], "expires": meta["expires"]})
        m = re.match(r"^/api/blob/([A-Za-z0-9_-]{22})$", path)
        if m:
            i = m.group(1)
            with lock:
                meta = items.get(i)
            if not meta or meta["expires"] <= time.time():
                return self.send_json(404, {"error": "File không tồn tại hoặc đã hết hạn"})
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(meta["size"]))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            with open(blob_path(i), "rb") as f:
                shutil.copyfileobj(f, self.wfile, 1024 * 1024)
            return
        self.send_json(404, {"error": "not found"})

    # ------------------------------------------------------------- POST
    def do_POST(self):
        if self.path.split("?")[0] != "/api/upload":
            return self.send_json(404, {"error": "not found"})
        if not self.authed():
            return self.send_json(401, {"error": "Sai mã truy cập máy chủ"})
        try:
            size = int(self.headers.get("Content-Length", "-1"))
        except ValueError:
            size = -1
        if size <= 0:
            return self.send_json(411, {"error": "Thiếu Content-Length"})
        if size > MAX_BYTES:
            return self.send_json(413, {"error": "File quá lớn (tối đa %d MB)" % (MAX_BYTES // 1024 // 1024)})
        key = self.headers.get("X-Alex-Key", "")
        if key and not KEY_RE.match(key):
            return self.send_json(400, {"error": "khoá không hợp lệ"})
        with lock:
            if used_bytes() + size > QUOTA:
                return self.send_json(507, {"error": "Máy chủ đã đầy, thử lại sau"})
            i = secrets.token_urlsafe(16)[:22]
            while i in items:
                i = secrets.token_urlsafe(16)[:22]
        tmp = blob_path(i) + ".part"
        left = size
        try:
            with open(tmp, "wb") as f:
                while left > 0:
                    chunk = self.rfile.read(min(left, 1024 * 1024))
                    if not chunk:
                        raise IOError("upload cut short")
                    f.write(chunk)
                    left -= len(chunk)
            with open(tmp, "rb") as f:
                if f.read(5) != b"ALXS1":
                    raise ValueError("not an Alex blob")
        except Exception:
            try:
                os.remove(tmp)
            except FileNotFoundError:
                pass
            return self.send_json(400, {"error": "Tải lên bị lỗi"})
        now = time.time()
        meta = {"id": i, "size": size, "created": now, "expires": now + TTL, "delete": secrets.token_urlsafe(16), "code": None}
        with lock:
            if key:
                code = "%06d" % secrets.randbelow(1000000)
                while code in codes:
                    code = "%06d" % secrets.randbelow(1000000)
                meta["code"], meta["key"] = code, key
                codes[code] = i
            os.replace(tmp, blob_path(i))
            with open(meta_path(i), "w") as f:
                json.dump(meta, f)
            items[i] = meta
        self.send_json(200, {"id": i, "code": meta["code"], "expires": meta["expires"], "delete_token": meta["delete"]})

    # ------------------------------------------------------------- DELETE
    def do_DELETE(self):
        m = re.match(r"^/api/blob/([A-Za-z0-9_-]{22})$", self.path)
        if not m:
            return self.send_json(404, {"error": "not found"})
        with lock:
            meta = items.get(m.group(1))
            if not meta or not hmac.compare_digest(self.headers.get("X-Delete-Token", ""), meta["delete"]):
                return self.send_json(404, {"error": "not found"})
            remove(m.group(1))
        self.send_json(200, {"ok": True})


def main():
    if not TOKEN:
        print("ALEX_TOKEN is not set: uploads are disabled")
    load()
    threading.Thread(target=cleaner, daemon=True).start()
    host, port = BIND.rsplit(":", 1)
    srv = ThreadingHTTPServer((host, int(port)), Handler)
    srv.daemon_threads = True
    print("Alex share server on http://%s (data: %s, ttl %dh)" % (BIND, DATA, TTL // 3600))
    srv.serve_forever()


if __name__ == "__main__":
    main()

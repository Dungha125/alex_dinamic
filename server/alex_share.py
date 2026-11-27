#!/usr/bin/env python3
"""Alex share server: device accounts + temporary, encrypted file drop.

Stdlib only (Python 3.8+). Runs behind Caddy (HTTPS) on 127.0.0.1.

Accounts (no shared access token any more):
  * Every Alex desktop registers itself once and gets a 9-digit ID, a password
    (shown in the app, for phones) and a private device secret (kept by the app).
  * A phone / web browser signs in with ID + password and gets a session token.
    One browser can be signed in to several PCs.
Transfers (all blobs are AES-256-GCM encrypted by the client, format "ALXS1"):
  * link   : key only in the URL #fragment (server cannot decrypt)
  * code   : 6-digit code; the key is deposited here
  * inbox  : phone -> its PC   (Alex downloads automatically)
  * outbox : PC -> its phones  (listed in the web app)
  For code / inbox / outbox the key is stored on this server, so whoever runs
  the server could decrypt those files. Everything expires (default 24 h).

Environment:
  ALEX_DATA         storage dir                 (default /var/lib/alex-share)
  ALEX_BIND         host:port                   (default 127.0.0.1:8080)
  ALEX_MAX_MB       max size per upload         (default 1024)
  ALEX_QUOTA_GB     max total storage           (default 20)
  ALEX_DEVICE_GB    max storage per device      (default 3)
  ALEX_TTL_HOURS    lifetime of files           (default 24)
  ALEX_REG_PER_DAY  registrations per IP / day  (default 10)
  ALEX_CORS         extra allowed web origins, comma separated
"""
import hashlib
import hmac
import json
import os
import re
import secrets
import shutil
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

DATA = os.environ.get("ALEX_DATA", "/var/lib/alex-share")
BIND = os.environ.get("ALEX_BIND", "127.0.0.1:8080")
MAX_BYTES = int(os.environ.get("ALEX_MAX_MB", "1024")) * 1024 * 1024
QUOTA = int(float(os.environ.get("ALEX_QUOTA_GB", "20")) * 1024 ** 3)
DEVICE_QUOTA = int(float(os.environ.get("ALEX_DEVICE_GB", "3")) * 1024 ** 3)
TTL = int(float(os.environ.get("ALEX_TTL_HOURS", "24")) * 3600)
REG_PER_DAY = int(os.environ.get("ALEX_REG_PER_DAY", "10"))
SESSION_DAYS = 180
CORS_ORIGINS = {"https://alex.tailieuptit.com", "http://alex.tailieuptit.com"} | {
    o.strip() for o in os.environ.get("ALEX_CORS", "").split(",") if o.strip()
}

KEY_RE = re.compile(r"^[A-Za-z0-9_-]{43}$")
PW_ALPHABET = "abcdefghjkmnpqrstuvwxyz23456789"  # no i/l/o/0/1

cond = threading.Condition()  # guards everything below; notified on new inbox items
items = {}     # blob id -> meta
codes = {}     # 6-digit code -> blob id
devices = {}   # device id -> {id, name, secret, salt, pw, created, seen}
sessions = {}  # sha256(token) -> {sid, device, name, created, last}
hits = {}      # (bucket, key) -> [timestamps]   rate limiting


# ------------------------------------------------------------------ storage
def path(*p):
    return os.path.join(DATA, *p)


def save_json(name, obj):
    tmp = path(name + ".tmp")
    with open(tmp, "w") as f:
        json.dump(obj, f)
    os.replace(tmp, path(name))


def save_accounts():
    save_json("devices.json", devices)
    save_json("sessions.json", sessions)


def load():
    os.makedirs(path("blobs"), exist_ok=True)
    for name, target in (("devices.json", devices), ("sessions.json", sessions)):
        try:
            with open(path(name)) as f:
                target.update(json.load(f))
        except FileNotFoundError:
            pass
    for name in os.listdir(path("blobs")):
        full = path("blobs", name)
        if name.endswith(".part"):
            os.remove(full)
        elif name.endswith(".json"):
            try:
                with open(full) as f:
                    m = json.load(f)
                items[m["id"]] = m
                if m.get("code"):
                    codes[m["code"]] = m["id"]
            except Exception:
                pass


def blob_path(i):
    return path("blobs", i + ".bin")


def remove_item(i):
    m = items.pop(i, None)
    if m and m.get("code"):
        codes.pop(m["code"], None)
    for p in (path("blobs", i + ".json"), blob_path(i)):
        try:
            os.remove(p)
        except FileNotFoundError:
            pass


def cleaner():
    while True:
        now = time.time()
        with cond:
            for i in [i for i, m in items.items() if m["expires"] <= now]:
                remove_item(i)
            stale = [k for k, s in sessions.items() if now - s["last"] > SESSION_DAYS * 86400]
            for k in stale:
                del sessions[k]
            if stale:
                save_accounts()
            for k in list(hits):
                hits[k] = [t for t in hits[k] if now - t < 86400]
                if not hits[k]:
                    del hits[k]
        time.sleep(60)


# ------------------------------------------------------------------ helpers
def sha(s):
    return hashlib.sha256(s.encode()).hexdigest()


def pw_hash(password, salt):
    return hashlib.pbkdf2_hmac("sha256", password.encode(), bytes.fromhex(salt), 200_000).hex()


def new_password():
    return "".join(secrets.choice(PW_ALPHABET) for _ in range(4)) + "-" + "".join(secrets.choice(PW_ALPHABET) for _ in range(4))


def limited(bucket, key, limit, window):
    """True if `key` already used `limit` attempts in `window` seconds (call under cond)."""
    now = time.time()
    h = hits.setdefault((bucket, key), [])
    h[:] = [t for t in h if now - t < window]
    return len(h) >= limit


def note(bucket, key):
    hits.setdefault((bucket, key), []).append(time.time())


def clean_name(s, default):
    s = re.sub(r"[\x00-\x1f]", "", str(s or "")).strip()[:40]
    return s or default


PAGE = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "page.html"), encoding="utf-8").read()


class Handler(BaseHTTPRequestHandler):
    server_version = "AlexShare/2"
    sys_version = ""

    def log_message(self, fmt, *args):  # never log names, keys or passwords
        pass

    # -------------------------------------------------------------- plumbing
    def end_headers(self):
        origin = self.headers.get("Origin", "")
        if origin in CORS_ORIGINS:
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Vary", "Origin")
            self.send_header("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Content-Type, Authorization, X-Alex-Key, X-Alex-To, X-Alex-Label, X-Delete-Token")
            self.send_header("Access-Control-Max-Age", "600")
        super().end_headers()

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def ip(self):
        ip = self.client_address[0]
        if ip in ("127.0.0.1", "::1"):
            ip = self.headers.get("X-Forwarded-For", ip).split(",")[0].strip()
        return ip

    def reply(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def body_json(self, limit=4096):
        n = int(self.headers.get("Content-Length", "0") or 0)
        if n <= 0:
            return {}
        if n > limit:
            raise ValueError("body too large")
        return json.loads(self.rfile.read(n) or b"{}")

    def auth(self):
        """-> ("device", device_id, None) | ("session", device_id, session_key) | (None, None, None)"""
        h = self.headers.get("Authorization", "")
        with cond:
            if h.startswith("Device "):
                did, _, secret = h[7:].partition(":")
                d = devices.get(did)
                if d and secret and hmac.compare_digest(d["secret"], sha(secret)):
                    d["seen"] = time.time()
                    return "device", did, None
            elif h.startswith("Session "):
                k = sha(h[8:].strip())
                s = sessions.get(k)
                if s and s["device"] in devices:
                    s["last"] = time.time()
                    return "session", s["device"], k
        return None, None, None

    def online(self, did):
        d = devices.get(did)
        return bool(d and time.time() - d.get("seen", 0) < 90)

    def item_view(self, m):
        return {"id": m["id"], "key": m.get("key"), "size": m["size"], "label": m.get("label", ""), "created": m["created"], "expires": m["expires"]}

    # -------------------------------------------------------------- GET
    def do_GET(self):
        u = urlparse(self.path)
        p = u.path
        if p in ("/", "/index.html") or p.startswith("/s/"):
            body = PAGE.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("Content-Security-Policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:")
            self.end_headers()
            self.wfile.write(body)
            return
        if p == "/api/ping":
            kind, did, _ = self.auth()
            return self.reply(200, {"ok": True, "auth": kind, "device": did, "max_bytes": MAX_BYTES, "ttl": TTL, "v": 2})

        m = re.match(r"^/api/code/(\d{6})$", p)
        if m:
            with cond:
                if limited("code", self.ip(), 30, 600):
                    return self.reply(429, {"error": "Thử quá nhiều lần, đợi 10 phút"})
                note("code", self.ip())
                i = codes.get(m.group(1))
                meta = items.get(i) if i else None
            if not meta or not meta.get("key"):
                return self.reply(404, {"error": "Mã không đúng hoặc đã hết hạn"})
            return self.reply(200, {"id": i, "key": meta["key"], "size": meta["size"], "expires": meta["expires"]})

        m = re.match(r"^/api/blob/([A-Za-z0-9_-]{22})$", p)
        if m:
            with cond:
                meta = items.get(m.group(1))
            if not meta or meta["expires"] <= time.time():
                return self.reply(404, {"error": "File không tồn tại hoặc đã hết hạn"})
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(meta["size"]))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            with open(blob_path(meta["id"]), "rb") as f:
                shutil.copyfileobj(f, self.wfile, 1024 * 1024)
            return

        kind, did, skey = self.auth()
        if p == "/api/device/sessions":
            if kind != "device":
                return self.reply(401, {"error": "Chưa đăng nhập"})
            with cond:
                out = [{"sid": s["sid"], "name": s["name"], "created": s["created"], "last": s["last"]} for s in sessions.values() if s["device"] == did]
            return self.reply(200, {"sessions": sorted(out, key=lambda s: -s["last"])})
        if p == "/api/inbox":
            if kind != "device":
                return self.reply(401, {"error": "Chưa đăng nhập"})
            wait = min(30.0, float(parse_qs(u.query).get("wait", ["0"])[0] or 0))
            deadline = time.time() + wait
            with cond:
                while True:
                    mine = [self.item_view(x) for x in items.values() if x.get("to") == did and x.get("dir") == "pc"]
                    left = deadline - time.time()
                    if mine or left <= 0:
                        break
                    cond.wait(min(left, 5))
                    devices[did]["seen"] = time.time()
            return self.reply(200, {"items": mine})
        if p == "/api/outbox":
            if kind != "session":
                return self.reply(401, {"error": "Chưa kết nối máy này"})
            with cond:
                mine = [self.item_view(x) for x in items.values() if x.get("owner") == did and x.get("dir") == "phones"]
            return self.reply(200, {"items": sorted(mine, key=lambda x: -x["created"])})
        if p == "/api/linked":
            if kind != "session":
                return self.reply(401, {"error": "Phiên đăng nhập đã hết, hãy kết nối lại"})
            with cond:
                d = devices[did]
                return self.reply(200, {"id": did, "name": d["name"], "online": self.online(did)})
        self.reply(404, {"error": "not found"})

    # -------------------------------------------------------------- POST
    def do_POST(self):
        p = urlparse(self.path).path
        if p == "/api/upload":
            return self.upload()
        try:
            data = self.body_json()
        except Exception:
            return self.reply(400, {"error": "Dữ liệu không hợp lệ"})

        if p == "/api/device/register":
            ip = self.ip()
            with cond:
                if limited("reg", ip, REG_PER_DAY, 86400):
                    return self.reply(429, {"error": "Đăng ký quá nhiều thiết bị từ mạng này, thử lại ngày mai"})
                note("reg", ip)
                did = "%09d" % secrets.randbelow(10 ** 9)
                while did in devices or did.startswith("0"):
                    did = "%09d" % secrets.randbelow(10 ** 9)
                secret, password, salt = secrets.token_urlsafe(32), new_password(), secrets.token_hex(16)
                devices[did] = {"id": did, "name": clean_name(data.get("name"), "Máy tính"), "secret": sha(secret),
                                "salt": salt, "pw": pw_hash(password, salt), "created": time.time(), "seen": time.time()}
                save_accounts()
            return self.reply(200, {"id": did, "secret": secret, "password": password})

        if p == "/api/login":
            did = re.sub(r"\D", "", str(data.get("id", "")))
            password = str(data.get("password", "")).strip().lower()
            ip = self.ip()
            with cond:
                if limited("login-ip", ip, 20, 900) or limited("login-dev", did, 10, 900):
                    return self.reply(429, {"error": "Nhập sai quá nhiều lần, đợi 15 phút"})
                d = devices.get(did)
                if not d or not hmac.compare_digest(d["pw"], pw_hash(password, d["salt"])):
                    note("login-ip", ip)
                    note("login-dev", did)
                    return self.reply(401, {"error": "Sai ID hoặc mật khẩu"})
                token = secrets.token_urlsafe(32)
                sessions[sha(token)] = {"sid": secrets.token_hex(6), "device": did, "name": clean_name(data.get("name"), "Điện thoại"),
                                        "created": time.time(), "last": time.time()}
                save_accounts()
                return self.reply(200, {"token": token, "device": {"id": did, "name": d["name"], "online": self.online(did)}})

        kind, did, skey = self.auth()
        if p == "/api/logout":
            if kind == "session":
                with cond:
                    sessions.pop(skey, None)
                    save_accounts()
            return self.reply(200, {"ok": True})
        if kind != "device":
            return self.reply(401, {"error": "Chưa đăng nhập"})
        if p == "/api/device/password":
            pw = str(data.get("password") or "").strip().lower() or new_password()
            if len(pw) < 6:
                return self.reply(400, {"error": "Mật khẩu cần ít nhất 6 ký tự"})
            with cond:
                d = devices[did]
                d["salt"] = secrets.token_hex(16)
                d["pw"] = pw_hash(pw, d["salt"])
                for k in [k for k, s in sessions.items() if s["device"] == did]:  # sign every phone out
                    del sessions[k]
                save_accounts()
            return self.reply(200, {"password": pw})
        if p == "/api/device/name":
            with cond:
                devices[did]["name"] = clean_name(data.get("name"), devices[did]["name"])
                save_accounts()
            return self.reply(200, {"ok": True})
        m = re.match(r"^/api/inbox/([A-Za-z0-9_-]{22})/ack$", p)
        if m:
            with cond:
                x = items.get(m.group(1))
                if x and x.get("to") == did and x.get("dir") == "pc":
                    remove_item(x["id"])
            return self.reply(200, {"ok": True})
        self.reply(404, {"error": "not found"})

    def upload(self):
        kind, did, _ = self.auth()
        if not kind:
            return self.reply(401, {"error": "Chưa đăng nhập máy chủ Alex"})
        try:
            size = int(self.headers.get("Content-Length", "-1"))
        except ValueError:
            size = -1
        if size <= 0:
            return self.reply(411, {"error": "Thiếu Content-Length"})
        if size > MAX_BYTES:
            return self.reply(413, {"error": "File quá lớn (tối đa %d MB)" % (MAX_BYTES // 1024 // 1024)})
        key = self.headers.get("X-Alex-Key", "")
        if key and not KEY_RE.match(key):
            return self.reply(400, {"error": "khoá không hợp lệ"})
        to = self.headers.get("X-Alex-To", "")
        if to not in ("", "pc", "phones") or (to == "pc" and kind != "session") or (to == "phones" and kind != "device"):
            return self.reply(400, {"error": "Đích gửi không hợp lệ"})
        if to and not key:
            return self.reply(400, {"error": "Thiếu khoá"})
        label = clean_name(unquote(self.headers.get("X-Alex-Label", "")), "file")[:120]
        with cond:
            if sum(m["size"] for m in items.values()) + size > QUOTA:
                return self.reply(507, {"error": "Máy chủ đã đầy, thử lại sau"})
            if sum(m["size"] for m in items.values() if m.get("owner") == did) + size > DEVICE_QUOTA:
                return self.reply(507, {"error": "Máy này đã dùng hết dung lượng tạm, đợi file cũ hết hạn"})
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
            return self.reply(400, {"error": "Tải lên bị lỗi"})
        now = time.time()
        meta = {"id": i, "size": size, "created": now, "expires": now + TTL, "delete": secrets.token_urlsafe(16),
                "code": None, "owner": did, "label": label}
        with cond:
            if to == "pc":
                meta.update(dir="pc", to=did, key=key)
            elif to == "phones":
                meta.update(dir="phones", key=key)
            elif key:
                code = "%06d" % secrets.randbelow(1000000)
                while code in codes:
                    code = "%06d" % secrets.randbelow(1000000)
                meta["code"], meta["key"] = code, key
                codes[code] = i
            os.replace(tmp, blob_path(i))
            save_json(os.path.join("blobs", i + ".json"), meta)
            items[i] = meta
            cond.notify_all()
        self.reply(200, {"id": i, "code": meta["code"], "expires": meta["expires"], "delete_token": meta["delete"]})

    # -------------------------------------------------------------- DELETE
    def do_DELETE(self):
        p = urlparse(self.path).path
        m = re.match(r"^/api/blob/([A-Za-z0-9_-]{22})$", p)
        if m:
            kind, did, _ = self.auth()
            with cond:
                meta = items.get(m.group(1))
                ok = meta and (hmac.compare_digest(self.headers.get("X-Delete-Token", ""), meta["delete"])
                               or (kind and meta.get("owner") == did))
                if not ok:
                    return self.reply(404, {"error": "not found"})
                remove_item(meta["id"])
            return self.reply(200, {"ok": True})
        m = re.match(r"^/api/device/sessions/([0-9a-f]{12})$", p)
        if m:
            kind, did, _ = self.auth()
            if kind != "device":
                return self.reply(401, {"error": "Chưa đăng nhập"})
            with cond:
                for k in [k for k, s in sessions.items() if s["device"] == did and s["sid"] == m.group(1)]:
                    del sessions[k]
                save_accounts()
            return self.reply(200, {"ok": True})
        self.reply(404, {"error": "not found"})


def main():
    load()
    threading.Thread(target=cleaner, daemon=True).start()
    host, port = BIND.rsplit(":", 1)
    srv = ThreadingHTTPServer((host, int(port)), Handler)
    srv.daemon_threads = True
    print("Alex share server v2 on http://%s (data: %s, ttl %dh)" % (BIND, DATA, TTL // 3600))
    srv.serve_forever()


if __name__ == "__main__":
    main()

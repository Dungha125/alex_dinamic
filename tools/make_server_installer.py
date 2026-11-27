"""Builds server/install.sh: one self-contained script that installs the Alex share
server on Ubuntu (Python + Caddy for HTTPS + systemd). Re-run after editing
server/alex_share.py or server/page.html."""
import base64
import pathlib

root = pathlib.Path(__file__).resolve().parent.parent
srv = root / "server"
py = base64.b64encode((srv / "alex_share.py").read_bytes()).decode()
page = base64.b64encode((srv / "page.html").read_bytes()).decode()

script = r'''#!/usr/bin/env bash
# Alex share server installer for Ubuntu 20.04+.
#   sudo bash install.sh                 -> HTTPS on <your-ip>.sslip.io
#   sudo DOMAIN=share.example.com bash install.sh   -> your own domain (DNS A record first)
# Safe to re-run (upgrades the server, keeps the access token and stored files).
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "Chạy bằng root: sudo bash install.sh"; exit 1; }
command -v apt-get >/dev/null || { echo "Script này dành cho Ubuntu/Debian"; exit 1; }

echo "==> Kiểm tra cổng 80/443"
busy=$(ss -ltnpH '( sport = :80 or sport = :443 )' 2>/dev/null | grep -v caddy || true)
if [ -n "$busy" ]; then
  echo "Cổng 80/443 đang được chương trình khác dùng:"; echo "$busy"
  echo "Dừng lại để không ảnh hưởng dịch vụ đang chạy. Hãy giải phóng cổng hoặc cấu hình reverse proxy tới 127.0.0.1:8080 thủ công."
  exit 1
fi

echo "==> Cài gói cần thiết"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq python3 curl ca-certificates gnupg debian-keyring debian-archive-keyring apt-transport-https >/dev/null
if ! command -v caddy >/dev/null; then
  echo "==> Cài Caddy (HTTPS tự động)"
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -qq
  apt-get install -y -qq caddy >/dev/null
fi

echo "==> Cài Alex share server"
id alexshare >/dev/null 2>&1 || useradd --system --home /var/lib/alex-share --shell /usr/sbin/nologin alexshare
install -d -o root -g root -m 755 /opt/alex-share
install -d -o alexshare -g alexshare -m 700 /var/lib/alex-share
echo '__PY__' | base64 -d > /opt/alex-share/alex_share.py
echo '__PAGE__' | base64 -d > /opt/alex-share/page.html
chmod 644 /opt/alex-share/*

if [ ! -f /etc/alex-share.env ]; then
  TOKEN=$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')
  cat > /etc/alex-share.env <<EOF
ALEX_TOKEN=$TOKEN
ALEX_DATA=/var/lib/alex-share
ALEX_BIND=127.0.0.1:8080
ALEX_MAX_MB=1024
ALEX_QUOTA_GB=20
ALEX_TTL_HOURS=24
EOF
  chmod 600 /etc/alex-share.env
fi
TOKEN=$(grep '^ALEX_TOKEN=' /etc/alex-share.env | cut -d= -f2-)

cat > /etc/systemd/system/alex-share.service <<'EOF'
[Unit]
Description=Alex share server
After=network-online.target

[Service]
User=alexshare
Group=alexshare
EnvironmentFile=/etc/alex-share.env
ExecStart=/usr/bin/python3 /opt/alex-share/alex_share.py
Restart=always
RestartSec=2
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ReadWritePaths=/var/lib/alex-share
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now alex-share >/dev/null
systemctl restart alex-share

if [ -z "${DOMAIN:-}" ]; then
  IP=$(curl -4 -fsS https://api.ipify.org || hostname -I | awk '{print $1}')
  DOMAIN="${IP//./-}.sslip.io"
fi
echo "==> Cấu hình HTTPS cho $DOMAIN"
cat > /etc/caddy/alex-share.caddy <<EOF
$DOMAIN {
	request_body {
		max_size 1100MB
	}
	reverse_proxy 127.0.0.1:8080 {
		flush_interval -1
	}
}
EOF
if [ ! -f /etc/caddy/Caddyfile ] || grep -q '/usr/share/caddy' /etc/caddy/Caddyfile; then
  # stock Caddyfile (or none): Alex is the only site
  echo 'import /etc/caddy/alex-share.caddy' > /etc/caddy/Caddyfile
elif ! grep -q 'alex-share.caddy' /etc/caddy/Caddyfile; then
  # keep the existing sites, just add ours
  printf '\nimport /etc/caddy/alex-share.caddy\n' >> /etc/caddy/Caddyfile
fi
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null
systemctl enable caddy >/dev/null 2>&1 || true
systemctl reload caddy 2>/dev/null || systemctl restart caddy

if command -v ufw >/dev/null && ufw status | grep -q 'Status: active'; then
  ufw allow 80/tcp >/dev/null; ufw allow 443/tcp >/dev/null
fi

echo "==> Chờ chứng chỉ HTTPS"
for i in $(seq 1 30); do
  if curl -fsS "https://$DOMAIN/api/ping" >/dev/null 2>&1; then ok=1; break; fi
  sleep 2
done
echo
if [ "${ok:-}" = 1 ]; then echo "Máy chủ đã chạy."; else echo "Máy chủ đã cài nhưng HTTPS chưa sẵn sàng (kiểm tra cổng 80/443 có mở ở tường lửa nhà cung cấp không)."; fi
echo "----------------------------------------------"
echo " Địa chỉ máy chủ : https://$DOMAIN"
echo " Mã truy cập     : $TOKEN"
echo "----------------------------------------------"
echo "Nhập hai dòng trên vào Alex: tab Gửi > Qua Internet > Cài đặt máy chủ."
'''
script = script.replace("__PY__", py).replace("__PAGE__", page)
out = srv / "install.sh"
out.write_bytes(script.encode("utf-8").replace(b"\r\n", b"\n"))
print("wrote", out, len(script), "bytes")

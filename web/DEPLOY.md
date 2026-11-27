# Deploy web Alex (alex.tailieuptit.com)

Thư mục `web/` là trang tĩnh, không cần build. Chép **toàn bộ** thư mục lên hosting:

```
index.html  app.js  qr.js  style.css  config.js  sw.js  manifest.webmanifest
icons/      download/      latest.json
```

## 1. Kiểm tra `config.js`

```js
API_BASE: "https://api.alex.tailieuptit.com",   // máy chủ API
SITE_URL: "https://alex.tailieuptit.com",       // chính trang này (dùng tạo link chia sẻ)
```

Máy chủ API phải có `https://alex.tailieuptit.com` trong `CORS_ORIGINS` (file `alex_share.py`, đã có sẵn).

Web này cần **máy chủ API bản 2** (tài khoản ID + mật khẩu). Cập nhật server bằng file cài mới:

```powershell
scp E:\DEV\Alex\server\install.sh root@180.93.32.135:/root/
```

```powershell
ssh root@180.93.32.135 "DOMAIN=api.alex.tailieuptit.com bash /root/install.sh"
```

Bản 2 lưu file trong thư mục con mới, nên các link/mã tạo bằng bản cũ sẽ không mở được nữa (vốn chỉ sống 24 giờ).

## 2. Bắt buộc HTTPS

Mã hoá / giải mã dùng WebCrypto, trình duyệt chỉ cho chạy trên HTTPS.

## 3. Ví dụ với Caddy (trên VPS)

```caddy
alex.tailieuptit.com {
	root * /var/www/alex-web
	file_server
	header /latest.json Cache-Control "no-store"
	header /sw.js Cache-Control "no-cache"
	header /download/* Cache-Control "no-cache"
	header {
		Content-Security-Policy "default-src 'self'; connect-src 'self' https://api.alex.tailieuptit.com; img-src 'self' data: blob:; media-src blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"
		Referrer-Policy "no-referrer"
		X-Content-Type-Options "nosniff"
	}
}
```

Chép lên VPS (chạy trên máy Windows, tự nhập mật khẩu/khoá SSH khi được hỏi):

```powershell
scp -r E:\DEV\Alex\web\* root@180.93.32.135:/var/www/alex-web/
```

Hosting tĩnh khác (Cloudflare Pages, Netlify, Vercel, GitHub Pages…) cũng được: chọn thư mục `web/`, không có lệnh build.
Nhớ đặt `latest.json` không bị cache lâu, nếu không app sẽ thấy bản mới chậm.

## 4. Phát hành bản Alex mới

```powershell
.\tools\release.ps1 -Version 0.2.0 -Notes "Có gì mới"
```

Script build, đóng gói, ký, rồi cập nhật `web/download/` và `web/latest.json`.
Deploy lại thư mục `web/` → mọi máy đã cài Alex tự nhận bản mới (kiểm tra 6 giờ/lần, hoặc bấm "Kiểm tra ngay" ở tab Công cụ).
Thêm `-Publish` để commit, gắn tag, push và tạo GitHub Release kèm bộ cài (cần repo GitHub riêng cho Alex).

Khoá ký nằm ở `%USERPROFILE%\.alex-release\update-key.pk8` — **sao lưu cẩn thận, không đưa lên Git**.
Mất khoá thì các bản Alex đã cài sẽ không nhận bản cập nhật mới nữa (phải cài tay lại).

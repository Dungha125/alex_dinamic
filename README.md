<p align="center"><img src="assets/logo.png" width="420" alt="Alex"></p>

**Alex** – hòn đảo nổi ở mép trên màn hình Windows. Nhẹ: một file exe ~5,5 MB, ~45 MB RAM riêng, gần như 0% CPU khi rảnh (Rust + egui/OpenGL, chỉ vẽ lại khi có thay đổi).

## Tính năng
- **Đảo nổi**: thu gọn hiện logo + giờ (và bài đang phát), rê chuột lên là mở ra kèm hiệu ứng lò xo. Nút 📌 giữ đảo mở.
- **Chấm mờ khi rảnh**: sau 8 giây không dùng (chỉnh được ở tab Công cụ, hoặc tắt), đảo thu thành chấm tròn mờ và chuột bấm xuyên qua tới cửa sổ phía sau. **Nhấn Ctrl hai lần** để hiện lại (nhấn Ctrl hai lần khi đảo đang mở để thu lại). Chỉ tính hai lần nhấn Ctrl "trống" trong 0,4 giây; Ctrl+C, Ctrl+V… không kích hoạt. Thông báo sao chép vẫn hiện thoáng qua (mờ, vẫn bấm xuyên được); yêu cầu nhận file qua LAN tự đánh thức đảo.
- **Hiện tại**: đồng hồ, ngày, nhạc đang phát (Spotify, trình duyệt, Media Player…) với nút lùi / phát-dừng / tới.
- **Clipboard**: tự lưu văn bản, link, ảnh, tệp đã Ctrl+C; bấm để chép lại, ghim ★ mục hay dùng. Lịch sử chỉ nằm trong RAM, bỏ qua nội dung từ trình quản lý mật khẩu. Chỉ các mục văn bản đã ghim được lưu ra đĩa.
- **Gửi (LAN)**: các máy có Alex trong cùng Wi-Fi/LAN tự thấy nhau (UDP 47820). Ở tab Gửi, thả file/thư mục lên đảo rồi bấm tên máy; hoặc gõ chữ để gửi (chữ nhận được vào lịch sử Clipboard). Truyền thẳng qua TCP 47821, không qua cloud, không giới hạn dung lượng. **Mã hoá đầu cuối**: mỗi kết nối trao đổi khoá tạm X25519, dữ liệu đi trong các khung ChaCha20-Poly1305 (HKDF-SHA256, khoá riêng mỗi chiều; sửa/chèn/đổi thứ tự dữ liệu là bị phát hiện và phần đã ghi dở bị xoá). Cả hai máy hiện cùng một **mã xác minh 6 số**; máy nhận chỉ nên bấm **Nhận** khi mã trùng với máy gửi (chống kẻ chen giữa). Không có gì được ghi trước khi bấm Nhận; file lưu vào `Downloads\Alex`. Lần đầu Windows hỏi tường lửa: chọn Mạng riêng.
- **Ghi chú**: checklist việc cần làm.
- **Bookmark**: kéo file/thư mục thả lên đảo để thêm, bấm để mở.
- **Tài liệu** (tab Công cụ): thả file lên đảo, đảo hiện đúng các nút dùng được. Tất cả miễn phí, xử lý ngay trên máy, kết quả nằm cạnh file gốc:
  - *Chuyển đổi*: Word/Excel/PowerPoint sang PDF, ảnh sang PDF (A4, giữ nền trong suốt), đổi ảnh sang PNG/JPG, **HEIC sang JPG** chất lượng 95% (dùng bộ giải mã HEIF của Windows, tự xoay theo EXIF, chuyển màu về sRGB, giữ ngày sửa file; cần “HEIF Image Extensions” miễn phí từ Microsoft Store), PDF sang ảnh PNG, PDF sang Word (Alex tự tạo file .docx từ chữ trong PDF, không cần Word, không treo; giữ chữ và ngắt trang, không giữ bố cục phức tạp như bảng/cột/ảnh; PDF ảnh quét chưa hỗ trợ).
  - *PDF*: cửa sổ riêng để **xem & tìm chữ**, **tô vẽ / tô sáng / gõ chữ tiếng Việt** (chữ nhúng font Segoe UI, vẫn chọn/copy được), **sắp xếp / xoay / bỏ trang**; lưu thành bản mới, không ghi đè. Gộp, tách, nén PDF. **Kiểm tra chữ ký số** (CryptoAPI của Windows: nội dung có bị sửa sau khi ký không, người ký, chứng thư có được Windows tin cậy không).
  - *Excel, Word & ảnh*: gộp nhiều file Excel, gộp nhiều file Word, nén ảnh JPG/PNG.
  - Office tự đóng nếu treo quá 4 phút (chỉ đóng phiên Office mà Alex mở). PDF có mật khẩu chưa hỗ trợ.
- **Công cụ**: chụp vùng, quay màn hình, Downloads, Máy tính, Task Manager, Âm thanh, Thùng rác, Khoá máy.
- **Vườn uống nước**: uống đủ cốc thì cây lớn rồi nở hoa, có chuỗi ngày.
- Tự ẩn khi xem toàn màn hình / trình chiếu; không lọt vào ảnh chụp và chia sẻ màn hình; icon ở khay hệ thống (bấm trái để ẩn/hiện, bấm phải có menu: khởi động cùng Windows, thoát).

- **Web cho điện thoại** (`web/`, deploy tại alex.tailieuptit.com): gửi ảnh/video/file/chữ từ điện thoại (mã hoá ngay trên máy), nhận bằng mã 6 số, link hoặc QR, lịch sử đã gửi, cài lên màn hình chính (PWA), tải Alex cho Windows. Hướng dẫn: `web/DEPLOY.md`.
- **Tự cập nhật**: Alex kiểm tra `https://alex.tailieuptit.com/latest.json` (15 giây sau khi mở, rồi 6 giờ/lần). Chỉ nhận bản được ký bằng khoá phát hành (Ed25519) và đúng SHA-256; tự cài im lặng khi không dùng đảo hoặc bấm "Cập nhật ngay" ở tab Công cụ, rồi tự mở lại. Phát hành bản mới: `.\tools\release.ps1 -Version x.y.z -Notes "…" [-Publish]`.

Dữ liệu: `%APPDATA%\Alex\data.json`.

## Build
```powershell
cargo build --release
```
File chạy: `target\release\alex.exe`.

Đóng gói bộ cài (cần Inno Setup 6):
```powershell
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" installer\alex.iss
```
Kết quả: `dist\Alex-Setup-<phiên bản>.exe` — cài cho riêng tài khoản hiện tại, không cần quyền admin, vào `%LOCALAPPDATA%\Programs\Alex`; có shortcut Start Menu, tuỳ chọn Desktop và khởi động cùng Windows; gỡ trong Settings → Apps (hỏi trước khi xoá dữ liệu trong `%APPDATA%\Alex`). Khi ra bản mới, đổi `AppVersion` trong `installer\alex.iss` cùng `version` trong `Cargo.toml`.

Logo: `tools/make_logo.py` sinh ra `assets/` (SVG, PNG, ICO).

Test: `cargo test --release` (thêm `-- --include-ignored` để thử cả phần cần Office).

Debug: `ALEX_PDF=<file.pdf>` (+ `ALEX_PDF_MODE=view|draw|pages`) mở thẳng cửa sổ PDF; `ALEX_TAB=0..6` mở sẵn và ghim đảo ở một tab; `ALEX_ALLOW_CAPTURE=1` cho phép chụp màn hình thấy đảo.


<#
  Alex release script.

    .\tools\release.ps1                      # build + package + sign the version in Cargo.toml (local only)
    .\tools\release.ps1 -Version 0.2.0 -Notes "Sửa lỗi ..."    # bump version first
    .\tools\release.ps1 -Version 0.2.0 -Notes "..." -Publish   # also commit, tag, push and create a GitHub Release

  Output:
    dist\Alex-Setup-<v>.exe
    web\download\Alex-Setup-<v>.exe and web\download\Alex-Setup-latest.exe
    web\latest.json  (version, url, sha256, size, Ed25519 signature, notes)
  Deploy the web\ folder as usual; installed Alex apps pick the new version up from latest.json.

  The signing key lives outside the repo: $env:USERPROFILE\.alex-release\update-key.pk8
  Back it up. Without it no update can be published (apps only accept releases signed with it).
#>
param(
  [string]$Version,
  [string]$Notes = "",
  [switch]$Publish,
  [string]$SiteUrl = "https://alex.tailieuptit.com"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$key = Join-Path $env:USERPROFILE ".alex-release\update-key.pk8"
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "C:\Program Files (x86)\Inno Setup 6\ISCC.exe", "C:\Program Files\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Không tìm thấy Inno Setup 6 (ISCC.exe)" }
if (-not (Test-Path $key)) { throw "Thiếu khoá ký $key. Tạo lần đầu: cargo build --release --bin alex-sign; .\target\release\alex-sign.exe keygen `"$key`" > assets\update_pubkey.txt" }

# ---------------------------------------------------------------- version
$cargo = Get-Content Cargo.toml -Raw -Encoding utf8
$current = [regex]::Match($cargo, '(?m)^version = "([^"]+)"').Groups[1].Value
if (-not $Version) { $Version = $current }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Phiên bản phải dạng x.y.z" }
if ($Version -ne $current) {
  $cargo = [regex]::Replace($cargo, '(?m)^version = "[^"]+"', "version = `"$Version`"", 1)
  [IO.File]::WriteAllText((Join-Path $root "Cargo.toml"), $cargo, (New-Object Text.UTF8Encoding $false))
  Write-Host "Cargo.toml: $current -> $Version"
}

# ---------------------------------------------------------------- build
Write-Host "==> cargo build --release"
cargo build --release
if ($LASTEXITCODE) { throw "cargo build failed" }
$pub = (& .\target\release\alex-sign.exe pubkey $key).Trim()
if ($pub -ne (Get-Content assets\update_pubkey.txt -Raw).Trim()) { throw "Khoá công khai trong assets\update_pubkey.txt không khớp khoá ký" }

Write-Host "==> Inno Setup"
& $iscc /Q "/DAppVersion=$Version" installer\alex.iss
if ($LASTEXITCODE) { throw "ISCC failed" }
$setup = Join-Path $root "dist\Alex-Setup-$Version.exe"
if (-not (Test-Path $setup)) { throw "Không thấy $setup" }

# ---------------------------------------------------------------- sign + publish to web\
$sha = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
$size = (Get-Item $setup).Length
$sig = (& .\target\release\alex-sign.exe sign $key "alex-update|$Version|$sha").Trim()
if ($LASTEXITCODE -or -not $sig) { throw "Ký thất bại" }

$dl = Join-Path $root "web\download"
New-Item -ItemType Directory -Force $dl | Out-Null
Get-ChildItem $dl -Filter "Alex-Setup-*.exe" | Where-Object { $_.Name -ne "Alex-Setup-$Version.exe" } | Remove-Item -Force
Copy-Item $setup (Join-Path $dl "Alex-Setup-$Version.exe") -Force
Copy-Item $setup (Join-Path $dl "Alex-Setup-latest.exe") -Force

$repo = ""
if (Get-Command gh -ErrorAction SilentlyContinue) {
  $ErrorActionPreference = "Continue"
  try { $repo = (gh repo view --json nameWithOwner -q .nameWithOwner 2>$null | Out-String).Trim() } catch { $repo = "" }
  if ($LASTEXITCODE) { $repo = "" }
  $ErrorActionPreference = "Stop"
}
$latest = [ordered]@{
  version = $Version
  url     = "$SiteUrl/download/Alex-Setup-$Version.exe"
  sha256  = $sha
  size    = $size
  sig     = $sig
  notes   = $Notes
  date    = (Get-Date).ToString("yyyy-MM-dd")
  github  = $(if ($repo) { "https://github.com/$repo/releases/tag/v$Version" } else { "" })
}
[IO.File]::WriteAllText((Join-Path $root "web\latest.json"), ($latest | ConvertTo-Json), (New-Object Text.UTF8Encoding $false))
Write-Host ""
Write-Host "Alex $Version"
Write-Host "  bộ cài : $setup ($([math]::Round($size / 1MB, 2)) MB)"
Write-Host "  sha256 : $sha"
Write-Host "  web    : web\latest.json + web\download\ (deploy thư mục web\)"

if (-not $Publish) { Write-Host "Chưa đẩy lên Git/GitHub (thêm -Publish để commit, tag, push và tạo GitHub Release)."; return }

# ---------------------------------------------------------------- git + GitHub Release
$top = (git rev-parse --show-toplevel).Trim()
if ((Resolve-Path $top).Path -ne (Resolve-Path $root).Path) { throw "Thư mục Alex chưa là repo Git riêng (đang nằm trong $top). Tạo repo riêng trước." }
if (-not $repo) { throw "Repo chưa nối GitHub (gh repo view thất bại)" }
git add -A
git commit -m "release: v$Version" -m $Notes
git tag "v$Version"
git push origin HEAD --tags
gh release create "v$Version" $setup --title "Alex $Version" --notes $(if ($Notes) { $Notes } else { "Alex $Version" })
Write-Host "Đã tạo GitHub Release v$Version"

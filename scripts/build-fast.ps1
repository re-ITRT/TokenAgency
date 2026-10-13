# [docs/46 §293] **迭代用快速构建**：debug profile（比 release 快数倍）+ 自动铺好内核与 GeoIP。
#
# 用法：
#   pwsh -File scripts/build-fast.ps1            # debug（默认，快）
#   pwsh -File scripts/build-fast.ps1 -Release   # release（慢，但就是交付那个二进制）
# 然后直接喂给真机 E2E：
#   node scripts/e2e/smoke.mjs --exe target\debug\tokenagency.exe
param([switch]$Release)
$ErrorActionPreference = 'Stop'
$profile = if ($Release) { 'release' } else { 'verify' }
Write-Host "cargo build -p tokenagency (profile=$profile)…" -ForegroundColor Cyan
cargo build -p tokenagency --profile $profile
if ($LASTEXITCODE -ne 0) { throw 'cargo build 未通过' }
$dst = "target\$profile"
Copy-Item 'src-tauri\resources\mihomo\mihomo.exe' "$dst\mihomo.exe" -Force
Copy-Item 'src-tauri\resources\mihomo\Country.mmdb' "$dst\Country.mmdb" -Force
Write-Host "产物：$dst\tokenagency.exe（+ mihomo.exe + Country.mmdb 已就位）" -ForegroundColor Green
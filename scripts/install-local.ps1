# **安装器的兜底脚本**：把已经构建好的产物直接铺进当前用户的安装目录。
#
# 什么时候用它：NSIS 安装器在你这台机器上被别的东西挡住（SmartScreen / 杀软 / 文件被占）时，
# 这个脚本做的是**同一件事**（覆盖 exe + 铺 `resources\mihomo\`），只是不经过安装器：
#   · 不弹 UAC（装的是当前用户目录，安装器也是 per-user）；
#   · 不碰注册表/开始菜单（安装器已经建过的那一份继续有效）；
#   · 不碰你的数据目录（`%APPDATA%\TokenAgency` 一个字节都不动）。
#
# 用法：
#   pwsh -File scripts/install-local.ps1                 # 用 target\release 里刚构建的产物
#   pwsh -File scripts/install-local.ps1 -ExeDir target\verify   # 换一个构建目录
#   pwsh -File scripts/install-local.ps1 -Uninstall      # 反向：删掉安装目录（保留数据）
param(
  [string]$ExeDir = 'target\release',
  [string]$InstallDir = "$env:LOCALAPPDATA\TokenAgency",
  [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($Uninstall) {
  Get-Process tokenagency -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Seconds 1
  if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force; "已删除 $InstallDir（数据目录 %APPDATA%\TokenAgency 未动）" }
  else { "没有 $InstallDir" }
  return
}

$exe = Join-Path $ExeDir 'tokenagency.exe'
$res = Join-Path $ExeDir 'resources\mihomo'
if (-not (Test-Path $exe)) { throw "没有 $exe —— 先跑 pwsh -File scripts/build-fast.ps1（或 scripts/build.ps1）" }
if (-not (Test-Path (Join-Path $res 'mihomo.exe'))) { throw "没有 $res\mihomo.exe（内核没铺到构建目录）" }

# 旧实例占着 exe 就覆盖不了：先关掉（与安装器钩子做的是同一件事）
$running = Get-Process tokenagency -ErrorAction SilentlyContinue
if ($running) {
  Write-Host "先关掉正在运行的 TokenAgency（$($running.Count) 个进程）…" -ForegroundColor Yellow
  $running | Stop-Process -Force
  Start-Sleep -Seconds 2
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $InstallDir 'resources\mihomo') | Out-Null
Copy-Item $exe (Join-Path $InstallDir 'tokenagency.exe') -Force
Copy-Item (Join-Path $res 'mihomo.exe') (Join-Path $InstallDir 'resources\mihomo\mihomo.exe') -Force
if (Test-Path (Join-Path $res 'Country.mmdb')) {
  Copy-Item (Join-Path $res 'Country.mmdb') (Join-Path $InstallDir 'resources\mihomo\Country.mmdb') -Force
}

$h = (Get-FileHash (Join-Path $InstallDir 'tokenagency.exe') -Algorithm SHA256).Hash
Write-Host "已铺到 $InstallDir" -ForegroundColor Green
Write-Host ("  tokenagency.exe    {0} MB  SHA256 {1}…" -f [math]::Round((Get-Item (Join-Path $InstallDir 'tokenagency.exe')).Length/1MB,2), $h.Substring(0,16))
Write-Host ("  resources\mihomo    已就位（内核 + GeoIP 库）")
Write-Host "现在直接开开始菜单/桌面上的 TokenAgency 就是这一版（数据目录 %APPDATA%\TokenAgency 没动）"

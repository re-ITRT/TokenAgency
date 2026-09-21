# TokenAgency 开发模式：先起 Vite，再起 Tauri（Tauri 会等在 devUrl 上）
# 用法: pwsh -File scripts/dev.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "[1/3] 确保前端依赖已安装 ..." -ForegroundColor Cyan
if (-not (Test-Path "node_modules")) {
    npm install --no-audit --no-fund
}

Write-Host "[2/3] 生成 IPC 类型绑定（specta -> src/shared/bindings.ts）..." -ForegroundColor Cyan
pwsh -File "$PSScriptRoot/gen-bindings.ps1"

Write-Host "[3/3] 启动 cargo tauri dev ..." -ForegroundColor Cyan
cargo tauri dev

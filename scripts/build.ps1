# TokenAgency 生产构建：前端 -> IPC 绑定 -> Tauri 打包（NSIS 安装包）
# 用法: pwsh -File scripts/build.ps1 [-SkipFrontend] [-SkipBindings]

param(
    [switch]$SkipFrontend,
    [switch]$SkipBindings
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $SkipBindings) {
    Write-Host "[1/4] 生成 IPC 类型绑定 ..." -ForegroundColor Cyan
    pwsh -File "$PSScriptRoot/gen-bindings.ps1"
}

if (-not $SkipFrontend) {
    Write-Host "[2/4] 构建前端 ..." -ForegroundColor Cyan
    npm run build
}

Write-Host "[3/4] Rust workspace 检查 + 测试 ..." -ForegroundColor Cyan
cargo check --workspace
cargo test --workspace --no-fail-fast

Write-Host "[4/4] Tauri 打包（NSIS）..." -ForegroundColor Cyan
cargo tauri build --bundles nsis

Write-Host ""
Write-Host "产物：" -ForegroundColor Green
Get-ChildItem "target/release/bundle/nsis" -ErrorAction SilentlyContinue |
    Select-Object Name, @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } } |
    Format-Table -AutoSize

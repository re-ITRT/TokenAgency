# 由 Rust 侧导出 TypeScript 类型绑定。
#
# 机制：src-tauri 编译时若设置了环境变量 TA_EXPORT_BINDINGS=1，
#       应用启动会把 tauri-specta 生成的 bindings 写到 src/shared/bindings.ts。
#       （工作包 T1 实现该导出；本脚本只是触发器）
#
# 用法: pwsh -File scripts/gen-bindings.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$out = Join-Path $root "src/shared/bindings.ts"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null

Write-Host "编译 src-tauri 以导出 bindings ..." -ForegroundColor Cyan
$env:TA_EXPORT_BINDINGS = "1"
$env:TA_EXPORT_BINDINGS_PATH = $out
try {
    cargo build -p tokenagency --quiet
}
finally {
    Remove-Item Env:\TA_EXPORT_BINDINGS -ErrorAction SilentlyContinue
    Remove-Item Env:\TA_EXPORT_BINDINGS_PATH -ErrorAction SilentlyContinue
}

if (Test-Path $out) {
    $len = (Get-Item $out).Length
    Write-Host "已生成 $out ($len bytes)" -ForegroundColor Green
}
else {
    Write-Warning "未生成 $out —— 若工作包 T1 尚未实现导出，请先完成 T1。"
    Write-Warning "在此之前，前端可先依赖 src/shared/api/ 的手写封装（mock 模式）。"
}

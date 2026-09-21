# 安装 git 钩子（把 core.hooksPath 指向仓库内的 .githooks，钩子随仓库走，不依赖 lefthook/husky）
# 用法: pwsh -File scripts/install-hooks.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Test-Path ".git")) {
    Write-Host "初始化 git 仓库 ..." -ForegroundColor Cyan
    git init | Out-Null
}

git config core.hooksPath .githooks
git config core.autocrlf false
git config pull.rebase true
git config fetch.prune true
git config rerere.enabled true
git config diff.algorithm histogram

Write-Host "已设置 core.hooksPath = .githooks" -ForegroundColor Green
Write-Host ""
Write-Host "已安装的钩子：" -ForegroundColor Cyan
Get-ChildItem .githooks -File | ForEach-Object {
    $first = (Get-Content $_.FullName -TotalCount 1)
    "  {0,-14} {1}" -f $_.Name, $first
}
Write-Host ""
Write-Host "pre-commit : cargo fmt --check + clippy -D warnings + tsc --noEmit + 密钥/大文件守卫" -ForegroundColor DarkGray
Write-Host "pre-push   : cargo test --workspace + npm run build" -ForegroundColor DarkGray
Write-Host "commit-msg : 约定式提交格式" -ForegroundColor DarkGray

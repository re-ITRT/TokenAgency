# TokenAgency 开发模式：先起 Vite，再起 Tauri（Tauri 会等在 devUrl 上）
# 用法: pwsh -File scripts/dev.ps1 [-WithBindings]
#
# ⚠️ 默认**不**重新生成 `src/shared/bindings.ts`（第 11 轮修）。
#
# 原因：那个文件现在装的是**手写的 105 条命令契约**，而 `gen-bindings.ps1`
# 生成的是另一套形态（camelCase 调用面）—— 覆盖后前端立刻崩，而且因为它被
# .gitignore 排除，`git status` 看不见，**排查起来会一头雾水**（"明明没改代码"）。
#
# T1b（E1 把 `src/shared/api/**` 迁到生成物上）完成后再用 -WithBindings 打开。
# 需要看"还差哪些命令"时，直接跑 `pwsh -File scripts/gen-bindings.ps1`（它会
# 打印名字 diff），或者用 `-DryRun` 干跑，不要让它悄悄覆盖。

param(
    [switch]$WithBindings
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host '[1/3] 确保前端依赖已安装 ...' -ForegroundColor Cyan
if (-not (Test-Path 'node_modules')) {
    npm install --no-audit --no-fund
}

Write-Host '[2/3] IPC 类型绑定 ...' -ForegroundColor Cyan
if ($WithBindings) {
    pwsh -File "$PSScriptRoot/gen-bindings.ps1"
} else {
    Write-Host '  未重新生成（默认）。要生成请显式加 -WithBindings。' -ForegroundColor DarkGray
    Write-Host '  理由：src/shared/bindings.ts 目前是手写契约，生成物会破坏前端。' -ForegroundColor DarkGray
}

Write-Host '[3/3] 启动 cargo tauri dev ...' -ForegroundColor Cyan
cargo tauri dev

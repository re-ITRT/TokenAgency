# TokenAgency 生产构建：Rust 门禁 -> 前端 -> IPC 绑定（可选）-> Tauri 打包（NSIS）
#
# 用法: pwsh -File scripts/build.ps1 [-SkipFrontend] [-WithBindings]
#
# 这个脚本是 W5 的入口，所以它自己必须先是"可信的"：
#   * `cargo check` 必须带 `--all-targets` —— 不带就是**非 test 编译**，
#     `#[cfg(test)]` 里的错误（include_str! 路径、类型错）完全看不见。
#     员工 P5 就是被这个洞藏住了一个坏掉的测试目标（docs/13 L18）。
#   * 门禁要跟仓库钩子一致（fmt / clippy -D warnings），不能只在 CI 里严格。

param(
    [switch]$SkipFrontend,

    # ⚠️ 默认**不**重新生成 `src/shared/bindings.ts`。
    #
    # 原因（第 11 轮实测）：那个文件现在装的是**手写的 105 条命令契约**，而
    # `gen-bindings.ps1` 生成的是另一套形态（camelCase 调用面）—— 直接覆盖会让
    # 紧接着的 `tsc --noEmit` 立刻失败（前端还在用 `CommandMap`）。
    # 也就是说：**原来的 build.ps1 在打包第一步就会毁掉前端构建。**
    #
    # T1b（E1 把 `src/shared/api/**` 迁到生成物上）完成后再打开这个开关。
    [switch]$WithBindings
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Step($n, $total, $msg) { Write-Host "[$n/$total] $msg" -ForegroundColor Cyan }

Step 1 5 '前端构建（tsc --noEmit + vite build）'
#
# ⚠️ **必须在 Rust 门禁之前**：`src-tauri` 里 `tauri::generate_context!` 是 proc macro，
# 它在编译期读 `dist/`；干净 clone 里 `dist/` 不存在（它是构建产物，已 gitignore），
# 先跑 `cargo check` 会看到 `error: proc macro panicked` —— 一个和代码无关的假红。
if ($SkipFrontend) {
    if (-not (Test-Path 'dist')) { throw '跳过前端但没有 dist/：cargo 的 generate_context! 会 panic。先去跑 npm run build。' }
    Write-Host '  已跳过（-SkipFrontend，dist/ 已存在）' -ForegroundColor DarkGray
} else {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw 'npm run build 未通过' }
    npm test
    if ($LASTEXITCODE -ne 0) { throw 'npm test 未通过' }
}

Step 2 5 'IPC 类型绑定'
if ($WithBindings) {
    pwsh -File "$PSScriptRoot/gen-bindings.ps1"
    if ($LASTEXITCODE -ne 0) { throw 'gen-bindings.ps1 未通过（看上面的名字 diff）' }
    Write-Host '  已重新生成 —— 完成后请确认 `npm run build` 仍然通过（T1b 之后才成立）' -ForegroundColor Yellow
} else {
    Write-Host '  未重新生成 src/shared/bindings.ts（默认）' -ForegroundColor DarkGray
    Write-Host '  理由：它现在装的是手写 105 条契约（**已入库**），生成物会破坏前端 tsc。' -ForegroundColor DarkGray
    Write-Host '  等 T1b 完成后用 -WithBindings 打开。' -ForegroundColor DarkGray
}

Step 3 5 'Rust 门禁：fmt + check --all-targets + clippy -D warnings'
cargo fmt --all --check
if ($LASTEXITCODE -ne 0) { throw 'cargo fmt --all --check 未通过' }
# ⚠️ 必须 --all-targets：`cargo check --workspace` 看不见 `#[cfg(test)]` 里的错误（L18）
cargo check --workspace --all-targets
if ($LASTEXITCODE -ne 0) { throw 'cargo check --workspace --all-targets 未通过' }
cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'cargo clippy 未通过' }

Step 4 5 'Rust 测试（workspace，不带任何 --exclude）'
cargo test --workspace --no-fail-fast
if ($LASTEXITCODE -ne 0) { throw 'cargo test --workspace 未通过' }

Step 5 5 'Tauri 打包（NSIS）'
cargo tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) { throw 'cargo tauri build 未通过' }

Write-Host ''
Write-Host '产物：' -ForegroundColor Green
Get-ChildItem 'target/release/bundle/nsis' -ErrorAction SilentlyContinue |
    Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } },
                  @{n = 'SHA256'; e = { (Get-FileHash $_.FullName -Algorithm SHA256).Hash.Substring(0, 16) } } |
    Format-Table -AutoSize

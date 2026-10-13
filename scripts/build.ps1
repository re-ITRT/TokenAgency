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

    # ⚠️ GPL-3.0（[docs/46] 与 2026-10-13 的许可决策）。
    #
    # 这里原来写的是"mihomo 是 GPL-3.0，随安装包分发有**传染性**义务"——
    # 那个判断是**偏严的**：mihomo 在本项目里是**独立进程**（`mihomo.exe` + 本地 HTTP 控制口），
    # 属于 GPLv3 §5 末段的 aggregate（聚合），不把 GPL 传染到本仓代码。
    #
    # 但本仓**已经整体选了 GPL-3.0**（LICENSE），所以"要不要为了避 GPL 而让用户自备内核"
    # 这个问题已经不存在了。`-NoKernel` 保留下来只为另一个用途：
    # **给不想让安装包带上 58MB 内核的人**一个更小的包。
    #
    # 两个模式都必须满足 §6 的源码可得性：随包分发内核时，`scripts/fetch-mihomo.ps1 -Mode Source`
    # 拉到的对应源码要**作为附件挂到同一个 Release**（不要只贴上游链接，那个 URL 不在我们手上）。
    [switch]$NoKernel,

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

# ---------------------------------------------------------------- 步骤 0：随包内核
#
# ⚠️ **顺序很要紧**：必须排在 `cargo build` 之前。`src-tauri/resources/**` 是 Tauri 的打包输入，
# 而内核二进制**不入库**（65MB，理由见 .gitignore）—— 干净 clone 里它不存在。
# 缺了它，`tauri build` 会**照常成功**，打出一个没有内核的安装包；那个包
# 能装、能开、界面一切正常，只是**永远起不了内核**。这正是本仓最忌讳的静默失效。
#
# 所以这里按**钉死的 tag + sha256** 拉取（`scripts/fetch-mihomo.ps1`）：
#   * 校验不符就抛错，绝不写出半成品；
#   * 顺带让"**改没改过内核**"永远可查 —— 二进制不在仓库里，谁动过都对不上 sha256。
#     这直接决定 GPL 下"要不要公开我们自己的改动"（现在是**没改**，已实测逐字节相同）。
Write-Host '[0/5] 随包内核（按 tag + sha256 拉取）' -ForegroundColor Cyan
if ($NoKernel) {
    Write-Host '  跳过（-NoKernel：产出用户自备内核的小包）' -ForegroundColor DarkGray
} else {
    pwsh -File "$PSScriptRoot/fetch-mihomo.ps1" -Mode Runtime
    if ($LASTEXITCODE -ne 0) { throw 'fetch-mihomo.ps1 未通过：内核二进制缺失或 sha256 不符' }
}

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
if ($NoKernel) {
    # [docs/46] **GPL-3.0 合规备选**：用一个覆盖配置把 `bundle.resources` 清空，
    # **不修改** `tauri.conf.json`（默认那版仍然内置内核）。用户自备内核时，
    # 把 `mihomo.exe` 放在安装目录旁边即可被 `bundled_kernel_path()` 认出来。
    $overlay = 'tauri.nokernel.conf.json'
    '{ "bundle": { "resources": [] } }' | Set-Content $overlay -Encoding utf8
    Write-Host '  -NoKernel：本次**不**打包内置内核（用户自备 mihomo.exe）' -ForegroundColor Yellow
    cargo tauri build --bundles nsis --config $overlay
    Remove-Item $overlay -Force -ErrorAction SilentlyContinue
} else {
    cargo tauri build --bundles nsis
}
if ($LASTEXITCODE -ne 0) { throw 'cargo tauri build 未通过' }

Write-Host ''
Write-Host '产物：' -ForegroundColor Green
Get-ChildItem 'target/release/bundle/nsis' -ErrorAction SilentlyContinue |
    Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } },
                  @{n = 'SHA256'; e = { (Get-FileHash $_.FullName -Algorithm SHA256).Hash.Substring(0, 16) } } |
    Format-Table -AutoSize

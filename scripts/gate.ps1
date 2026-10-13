# TokenAgency 提交门禁 —— **唯一真相源**。
#
# 两个入口共用本文件，不允许各写一套：
#   .githooks/pre-commit —— 直接提交（git commit）走这里
#   scripts/pr.ps1       —— 员工提 PR（临时 index + git commit-tree）也走这里，
#                            `git commit-tree` 不触发钩子，所以必须显式跑同一份检查
#
# 设计原则：
#   * **只检查本次提交涉及的东西**，不做全仓库检查 —— 全量检查会被别人未完成的
#     在途代码挡住，最终逼所有人用 `--no-verify`，那等于门禁失效。
#   * clippy 必须带 `--no-deps`：它仍然编译依赖（"我错误使用了上游 API"照样抓住），
#     但不去 lint 别人的代码。**门禁的作用域必须与责任的作用域一致。**
#   * 明文密钥守卫分级：生产代码命中 → 拦；测试/夹具/文档命中 → 只警告。
#     误伤合法夹具的后果同样是"大家学会绕过门禁"。
#
# 用法：
#   pwsh -File scripts/gate.ps1                     # 检查真实索引（钩子用）
#   pwsh -File scripts/gate.ps1 -IndexFile <path>   # 检查指定索引（PR 用）
#
# 退出码：0 = 通过，1 = 未通过

[CmdletBinding()]
param(
    # 要检查的索引文件；不传则用仓库真实索引。
    [string]$IndexFile,

    # 拿索引与哪个提交比 —— 决定"本次改动"的文件集。
    #
    # ⚠️ 不传 = 与 HEAD 比。**PR 的追加模式必须传父提交**，否则会算错文件集：
    # E1 实测 —— 它的分支基点是旧 master，而 master 已经前进过，于是
    # `git diff --cached`（vs HEAD）把"master 上新增、而分支里还是旧版"的文件
    # 也算成"本次改动"，门禁去给**别人的文件**跑 rustfmt 并红掉（`✘ 格式不合格`），
    # 与它的改动无关却让它的 PR 提不出去。**门禁的作用域必须等于责任的作用域**
    # 。
    [string]$Against
)

$ErrorActionPreference = 'Stop'

# git 的文本输出收成字符串（空输出在 PowerShell 里是 $null，`.Trim()` 会抛）
function GitText {
    param([string[]]$GitArgs)
    $out = @(& git @GitArgs 2>$null) -join "`n"
    return "$out".Trim()
}

$diffBase = @()
if ($Against) { $diffBase = @($Against) }

function Fail($msg) { Write-Host "`n✘ $msg`n" -ForegroundColor Red; exit 1 }
function Info($msg) { Write-Host "› $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "! $msg" -ForegroundColor Yellow }
function Ok($msg)   { Write-Host "✔ $msg" -ForegroundColor Green }

# ⚠️ 门禁的"改动集"只能有一个来源，而且**git 失败必须与"没有改动"分开**。
#
# 我在这里犯过一次、而且比原来更糟（E2 与 E5 各自实测撞到，E2 定位到行）：
#     $diffBase = if ($Against) { @($Against) } else { @() }
#   PowerShell 的 `if` 会把单元素数组**展平成 String**，于是 `@diffBase` 对字符串
#   splat 时**逐字符**传参（`git diff --cached f a b ...`）→ git 报错；而
#   `2>$null` 把报错吞了 → `$staged` 为空 → 打印 `✔ 没有暂存文件，跳过` 并 **exit 0**。
#   → **门禁静默退化成一个空操作，而且显示通过。**
#
# 所以这里两条一起改（E2 的建议）：
#   ① `[string[]]` 显式保型，不让 PowerShell 展平；
#   ② 检查 `$LASTEXITCODE` —— **"git 失败"永远不许伪装成"没有改动"**。
function Get-ChangedSet {
    param([string]$IndexHint)
    $gitArgs = @('diff', '--cached', '--name-only', '--diff-filter=ACMR')
    if ($IndexHint) { $gitArgs += $IndexHint }
    $out = @(& git @gitArgs 2>&1)
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        Fail "git diff 失败（exit $code）—— 门禁无法确定改动集。**这不是「没有改动」**。`n  $($out -join "`n  ")"
    }
    return @($out | Where-Object { $_ })
}

$root = (git rev-parse --show-toplevel).Trim()
Set-Location $root

# 只在本进程内改 GIT_INDEX_FILE；异常退出也要还原，避免污染调用方
$savedIndex = $env:GIT_INDEX_FILE
if ($IndexFile) {
    $env:GIT_INDEX_FILE = (Resolve-Path $IndexFile).Path
} elseif ($savedIndex) {
    Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue
}

try {
    $staged = @(Get-ChangedSet -IndexHint ($diffBase -join ''))
    if ($staged.Count -eq 0) {
        # 钩子模式下"没有暂存文件"是正常的（用户只改了一个非代码文件）；但**在 PR 模式下
        # 空改动集意味着工具链出了问题** —— 由调用方（pr.ps1）在更早处就会拒绝空 PR，
        # 所以这里只需把两种情形区分清楚，不再无条件报"通过"。
        if ($diffBase.Count -gt 0) {
            Ok "相对 $Against 没有改动（不是错误；空 PR 由 pr.ps1 更早处拦截）"
        } else {
            Ok '没有暂存文件，跳过'
        }
        exit 0
    }

    # ------------------------------------------------------------ 1) 明文密钥守卫
    $leakFail = @()
    $leakWarn = @()
    $secretPattern = '(sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----)'
    foreach ($f in $staged) {
        if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { continue }
        $added = @(& git diff --cached @diffBase -U0 -- $f 2>$null |
            Where-Object { $_ -like '+*' -and $_ -notlike '+++*' } |
            Where-Object { $_ -notmatch 'ta-allow-fake-secret' } |
            Select-String -Pattern $secretPattern -ErrorAction SilentlyContinue)
        if ($added.Count -eq 0) { continue }
        $isLoose = ($f -match '(^|/)(tests?)/') -or ($f -match 'fixture') -or
                   ($f -match 'mock') -or ($f -like '*.md') -or
                   ($f -match '(^|/)examples/') -or ($f -like '.githooks/*')
        if ($isLoose) { $leakWarn += $f } else { $leakFail += $f }
        if (-not $isLoose) { $added | ForEach-Object { Write-Host "      $($_.Line)" } }
    }
    if ($leakWarn.Count -gt 0) {
        Warn "以下文件（测试/夹具/文档）含疑似密钥形态的字符串，已放行 —— 请确认是假值："
        $leakWarn | ForEach-Object { Write-Host "  $_" }
    }
    if ($leakFail.Count -gt 0) {
        Fail "以下生产代码文件含疑似明文密钥。请改用 `${secret:name} 或 `${env:VAR}。`n  $($leakFail -join "`n  ")"
    }

    # ------------------------------------------------------------ 2) 大文件守卫
    $big = @()
    foreach ($f in $staged) {
        if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { continue }
        $sz = (Get-Item -LiteralPath $f).Length
        if ($sz -gt 1048576) { $big += "$f ($([math]::Floor($sz / 1024)) KB)" }
    }
    if ($big.Count -gt 0) {
        Fail "以下文件超过 1MB。确认是否该入库；运行时数据已在 .gitignore 中。`n  $($big -join "`n  ")"
    }

    # ------------------------------------------------------------ 3) 分类暂存文件
    $rsFiles = @()
    $tsFiles = @()
    $crates = @()
    $hasToml = $false
    foreach ($f in $staged) {
        switch -Regex ($f) {
            '\.rs$' {
                $rsFiles += $f
                if ($f -match '^crates/([^/]+)/') { $crates += $Matches[1] }
                elseif ($f -like 'src-tauri/*') { $crates += 'tokenagency' }
            }
            '\.tsx?$' { $tsFiles += $f }
            '^Cargo\.toml$' { $hasToml = $true }
        }
    }
    $crates = @($crates | Sort-Object -Unique)

    # ------------------------------------------------------------ 4) Rust 格式
    if ($rsFiles.Count -gt 0) {
        Info 'rustfmt --check（仅本次提交的 .rs）'
        foreach ($f in $rsFiles) {
            if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { continue }
            & rustfmt --edition 2021 --check $f > $null 2>&1
            if ($LASTEXITCODE -ne 0) {
                Fail "格式不合格：$f`n  修复：rustfmt --edition 2021 $f   或   cargo fmt --all"
            }
        }
    }

    # ------------------------------------------------------------ 5) Clippy
    if ($crates.Count -gt 0) {
        foreach ($c in $crates) {
            Info "cargo clippy -p $c --all-targets --no-deps -- -D warnings"
            & cargo clippy -p $c --all-targets --no-deps --quiet -- -D warnings 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) {
                Fail "clippy 未通过（$c）。
  若报错文件**在 $c 里** —— 是你自己的问题，修掉再提交。
  若报错文件**不在 $c 里** —— 说明某个依赖当前编不过（别人在途）。
  那不是你的错，但你也没法在树不完整时验证自己：请等对方修好，或在隔离副本里验证。"
            }
        }
    } elseif ($hasToml) {
        # ⚠️ 必须带 --all-targets：`cargo check --workspace` 是**非 test** 编译，
        # `#[cfg(test)]` 里的错误它完全看不见。
        Info '仅 Cargo.toml 变更，跑 cargo check --workspace --all-targets'
        & cargo check --workspace --all-targets --quiet 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { Fail 'workspace 检查未通过（含 test target）' }
    }

    # ------------------------------------------------------------ 6) 前端类型
    if ($tsFiles.Count -gt 0) {
        if (Test-Path 'node_modules') {
            Info 'tsc --noEmit'
            & npx --no-install tsc --noEmit 2>&1 | Out-Host
            if ($LASTEXITCODE -ne 0) { Fail 'TypeScript 类型检查失败。' }
        } else {
            Warn '跳过 tsc：node_modules 不存在（先 npm install）'
        }
    }

    Ok 'gate 通过'
} finally {
    if ($savedIndex) { $env:GIT_INDEX_FILE = $savedIndex }
    else { Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue }
}

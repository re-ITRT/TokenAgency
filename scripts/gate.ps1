# TokenAgency 提交门禁 —— **唯一真相源**。
#
# 两个入口共用本文件，不允许各写一套：
#   .githooks/pre-commit —— 直接提交（git commit）走这里
#   scripts/pr.ps1       —— 员工提 PR（临时 index + git commit-tree）也走这里，
#                            `git commit-tree` 不触发钩子，所以必须显式跑同一份检查
#
# 设计原则（照抄自原钩子，理由见 docs/13 L16/L18/L20）：
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
    # （docs/13 L20 的同一条教训，换了个位置又咬了一次）。
    [string]$Against
)

$ErrorActionPreference = 'Stop'

# git 的文本输出收成字符串（空输出在 PowerShell 里是 $null，`.Trim()` 会抛）
function GitText {
    param([string[]]$GitArgs)
    $out = @(& git @GitArgs 2>$null) -join "`n"
    return "$out".Trim()
}

$diffBase = if ($Against) { @($Against) } else { @() }

function Fail($msg) { Write-Host "`n✘ $msg`n" -ForegroundColor Red; exit 1 }
function Info($msg) { Write-Host "› $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "! $msg" -ForegroundColor Yellow }
function Ok($msg)   { Write-Host "✔ $msg" -ForegroundColor Green }

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
    $staged = @(& git diff --cached @diffBase --name-only --diff-filter=ACMR 2>$null)
    if ($staged.Count -eq 0) { Ok '没有暂存文件，跳过'; exit 0 }

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
        Fail "以下生产代码文件含疑似明文密钥。请改用 `${secret:name} 或 `${env:VAR}（见 docs/02 §5）。`n  $($leakFail -join "`n  ")"
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
        # `#[cfg(test)]` 里的错误它完全看不见（docs/13 L18）。
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

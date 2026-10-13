# TokenAgency 提交入口 —— 每个员工用**自己的身份**提交，且**只能提交自己范围内的文件**
#
# 为什么要有这个脚本（两个真实痛点）：
#   1. 主管排查"谁动了 X"时，之前只能靠文件时间戳**推断**，结果推错了人。
#      不同提交身份能从根上解决：`git log --format='%an'` 直接出答案。
#   2. 越界修改（改了不属于自己的文件）以前只能靠人工看 diff。
#      这里把  的编制表变成**机械检查** —— 路径不在你的范围内直接拒绝。
#
# 用法：
#   pwsh -File scripts/commit.ps1 -As E4 -Paths crates/ta-subscription -MessageFile msg.txt -AgentId <id>
#   pwsh -File scripts/commit.ps1 -As E1 -Paths crates/ta-core/src/domain,config.example.yaml -Message "feat(core): ..."
#   pwsh -File scripts/commit.ps1 -As E1 -Paths @('a','b','c') -Message "..."    # 数组写法也可
#   pwsh -File scripts/commit.ps1 -As lead -Paths docs -Message "docs: ..."
#   加 -DryRun 先干跑（**不写任何文件、不提交**，只打印最终提交信息）
#
# 约定：
#   * `-Paths` 支持逗号串联，也支持数组（`pwsh -File` 会把 `a,b,c` 合成一个字符串，脚本内部会拆）
#   * `-MessageFile` 是**输入**，脚本不会改写它（trailer 只写进临时文件）
#   * 多行中文**必须**用 `-MessageFile`（PowerShell 传多行参数会被切碎，提交信息会被截断成 pathspec）
#   * 只提交自己范围外的东西时会被拒绝；确实需要时用 `-Override "原因"` 显式承担

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('lead', 'E1', 'E2', 'E3', 'E4', 'E5', 'mech')]
    [string]$As,

    # 只提交这些路径（相对仓库根）。**不会影响别人已暂存的文件。**
    [Parameter(Mandatory = $true)]
    [string[]]$Paths,

    [string]$Message,
    [string]$MessageFile,

    # 精确到 agent 实例，写进 trailer 便于交叉核对
    [string]$AgentId,

    # 明确承担"提交范围外文件"的责任（会写进 trailer，可审计）
    [string]$Override,

    [switch]$NoVerify,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# 兼容两种传法：
#   pwsh -File scripts/commit.ps1 -Paths a,b,c        ← -File 会把它们合成一个字符串
#   pwsh -File scripts/commit.ps1 -Paths a -Paths b   ← 真正的数组
$Paths = @($Paths |
        ForEach-Object { $_ -split ',' } |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ })

if ($Paths.Count -eq 0) { throw "-Paths 不能为空" }

if (-not $Message -and -not $MessageFile) {
    throw "必须给 -Message 或 -MessageFile"
}
if ($Message -and $MessageFile) {
    throw "-Message 与 -MessageFile 只能给一个"
}

# ---------------------------------------------------------------- 身份与文件范围
#
# **唯一真相源在 scripts/_team.ps1**（commit.ps1 与 pr.ps1 共用同一份）。
# 原来这里自带一份、 又抄一份，两处不一致让 E1 每提交一次前端
# 都要 `-Override`。同一事实有两个真相源，就会漂移。
. "$PSScriptRoot\_team.ps1"

# ---------------------------------------------------------------- 谁可以直提 master
#
# 第 11 轮改的流程：**员工不再直提 master**，改为在自己分支上提 PR
# （scripts/pr.ps1），由主管评审后合并。
#
# 起因是真实的痛：员工直提让"在途的半成品"落在 main 上 —— E2 写 diag 的那段时间
# ta-core 编不过，E4/E5/E3 全被挡住一轮；而且 main 因此没有评审点，
# 变成"谁的活干到哪算哪"，而不是"过审的东西"。
if ($As -ne 'lead' -and $As -ne 'mech') {
    Write-Host ""
    Write-Host "✘ 员工不再直接提交 master（$As）。" -ForegroundColor Red
    Write-Host ""
    Write-Host "  改成提 PR —— 你自己的分支，主管评审后合并：" -ForegroundColor Yellow
    Write-Host "    pwsh -File scripts/pr.ps1 -As $As -Paths <你的范围> -Title ""..."" -MessageFile <临时文件>"
    Write-Host "    pwsh -File scripts/pr.ps1 -List                 # 看所有在提的 PR"
    Write-Host ""
    Write-Host "  为什么：main 只接收过审的东西。你在途的半成品不该挡住别人的编译，" -ForegroundColor Cyan
    Write-Host "  也不该在没过评审时就成为「主干」。你的工作一点都不会丢 ——" -ForegroundColor Cyan
    Write-Host "  pr.ps1 用临时索引造提交，**不切分支、不动工作区**，可反复调用追加提交。" -ForegroundColor Cyan
    Write-Host ""
    exit 1
}

# ---------------------------------------------------------------- 文件范围
#
# `$Scopes` / `$Identities` / `Test-InScope` 全部来自上面 dot-source 的
# `scripts/_team.ps1` —— 那是唯一真相源，本文件**不再自带一份**。

$id = $Identities[$As]
$scope = $Scopes[$As]

# ---------------------------------------------------------------- 范围检查
$outside = @()
foreach ($p in $Paths) {
    if (-not (Test-InScope $p $scope)) { $outside += $p }
}
if ($outside.Count -gt 0) {
    if (-not $Override) {
        Write-Host ""
        Write-Host "✘ 越界：以下路径不在 $As（$($id.Desc)）的范围内" -ForegroundColor Red
        $outside | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
        Write-Host ""
        Write-Host "  你的范围：" -ForegroundColor DarkGray
        $scope | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
        Write-Host ""
        Write-Host "  这是编制表。确实需要提交范围外的文件时：" -ForegroundColor Yellow
        Write-Host "    -Override ""一句话说明原因""" -ForegroundColor Yellow
        Write-Host "  （会写进提交 trailer，可审计）" -ForegroundColor DarkGray
        exit 2
    }
    Write-Host "⚠ 已用 -Override 承担越界提交：$Override" -ForegroundColor Yellow
    $outside | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
}

# ---------------------------------------------------------------- 跨范围保护（主管也适用）
#
# 真实事故（第 11 轮，我犯的）：用 `-Paths docs,crates/ta-core` 提交契约改动时，
# `crates/ta-core` 是个**目录 pathspec**，于是把 E2 在途的 quota/metering 代码
# （201 行）一起卷了进来 —— 提交信息写的是 "docs(observability)"，署名是主管，
# 而 E2 的 PR 当场变成"无可提内容"。三样都错：归属、信息、评审点。
#
# 根因：**`-Paths` 是 pathspec，不是"我要提交的文件清单"。** 传目录 = 把那一刻
# 该目录下所有脏文件都收了，包括别人的在途工作。
#
# 所以这里把"每个人的独占范围"变成机械检查：只要待提交的文件落在**别人**的
# 独占范围里，就必须显式 `-Override "原因"`。契约文件（如  +
# `diag/fields.rs` 必须同改）会命中这条，那就堂堂正正写理由 —— 这正是它存在的意义。
if ($As -eq 'lead') {
    $willStage = @(git status --porcelain --untracked-files=all -- $Paths 2>$null | ForEach-Object {
            if (-not $_) { return }
            $p = $_.Substring(3)
            if ($p -match ' -> ') { $p = ($p -split ' -> ')[1] }
            $p.Trim('"')
        } | Where-Object { $_ })
    $foreign = @()
    foreach ($f in $willStage) {
        foreach ($k in $Identities.Keys) {
            if ($k -eq 'lead' -or $k -eq 'mech') { continue }
            if ($Scopes.ContainsKey($k) -and (Test-InScope $f $Scopes[$k])) {
                $foreign += "$f  (属于 $k)"
                break
            }
        }
    }
    if ($foreign.Count -gt 0 -and -not $Override) {
        Write-Host ""
        Write-Host "✘ 待提交文件里有**别人独占范围**的东西 —— 停一下，这多半不是你的本意" -ForegroundColor Red
        $foreign | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
        Write-Host ""
        Write-Host "  常见成因：`-Paths` 传的是**目录**（如 crates/ta-core），它会把该目录下" -ForegroundColor Yellow
        Write-Host "  那一刻所有脏文件都收进来，**包括同事的在途工作**。同事的活应该走他的 PR。" -ForegroundColor Yellow
        Write-Host ""
        Write-Host "  两条出路：" -ForegroundColor Cyan
        Write-Host "    ① 换成**具体文件**：-Paths config.example.yaml,crates/ta-core/src/diag/fields.rs"
        Write-Host "    ② 确实要主管落笔（例如契约同改）：-Override ""一句话说明原因"""
        Write-Host ""
        exit 3
    }
    if ($foreign.Count -gt 0 -and $Override) {
        Write-Host "⚠ 已用 -Override 承担跨范围提交：$Override" -ForegroundColor Yellow
        $foreign | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
    }
}

# ---------------------------------------------------------------- 确认路径存在
$missing = @()
foreach ($p in $Paths) {
    if (-not (Test-Path $p)) { $missing += $p }
}
if ($missing.Count -gt 0) {
    Write-Host "✘ 路径不存在（或拼错了）：" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    exit 2
}

# ---------------------------------------------------------------- trailer 与消息
#
# 设计约定：**`-MessageFile` 是输入，不是输出。** 我们不改调用方的文件。
# （早期版本把 trailer 原地追加回 `-MessageFile`，导致"先干跑再正式提交"用同一个文件时
#   trailer 会重复 —— 员工 P1 反馈的 bug，已修。）
$trailers = @()
$trailers += "Work-Package: $As"
if ($AgentId) { $trailers += "Agent-Id: $AgentId" }
if ($Override) { $trailers += "Scope-Override: $Override" }

$base = if ($MessageFile) { (Get-Content $MessageFile -Raw).TrimEnd() } else { $Message.TrimEnd() }
$body = $base + "`n`n" + ($trailers -join "`n") + "`n"

if ($DryRun) {
    Write-Host "`n(干跑，未写任何文件、未提交) 将使用的提交信息：" -ForegroundColor Yellow
    $body -split "`n" | ForEach-Object { Write-Host "  | $_" -ForegroundColor DarkGray }
    exit 0
}

$msgFile = Join-Path $env:TEMP "ta-commit-msg-$([guid]::NewGuid().ToString('N').Substring(0,8)).txt"
[System.IO.File]::WriteAllText($msgFile, $body, (New-Object System.Text.UTF8Encoding($false)))

# ---------------------------------------------------------------- 暂存 + 提交（带 index.lock 重试）
Write-Host "以 $($id.Name) <$($id.Email)> 提交：" -ForegroundColor Cyan
$Paths | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }

if ($DryRun) {
    Write-Host "`n(干跑，未实际提交) 消息如下：" -ForegroundColor Yellow
    Get-Content $msgFile | ForEach-Object { Write-Host "  | $_" -ForegroundColor DarkGray }
    if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
    exit 0
}

$gitArgs = @(
    '-c', "user.name=$($id.Name)",
    '-c', "user.email=$($id.Email)",
    'commit'
)
if ($NoVerify) { $gitArgs += '--no-verify' }
$gitArgs += @('-F', $msgFile, '--')
$gitArgs += $Paths

# 并行开发下 `.git/index.lock` 是**高频**竞争点：git 在执行 pre-commit 钩子期间
# 会一直持有它，而钩子要跑 clippy —— 那一跑可能是几分钟。所以：
#   * 重试预算要按"分钟级"给，不是"秒级"（原来 6 次 × 最多 2.4s ≈ 8s，实测不够，连续 6 次全撞）
#   * 后退要加**抖动**，否则五个员工会同步重试、永远撞在一起
#   * `git add` 的退出码**必须检查** —— 见下面的注释，这是"假绿"的入口
$maxTry = 25
for ($i = 1; $i -le $maxTry; $i++) {
    # ⚠️ 只 add 自己的路径；且**必须检查退出码**。
    #
    # 若 `git add` 因锁竞争静默失败，而 `git commit -- <paths>` 仍然成功
    # （它对给定路径走 partial commit，不依赖索引），那么 pre-commit 钩子读到的
    # `git diff --cached --name-only` 会是**别人或空的暂存集** → 钩子检查了错误的东西
    # → **门禁假绿**。这比提交失败危险得多，所以这里一旦 add 失败就重试，绝不继续。
    $addOut = git add -- $Paths 2>&1
    if ($LASTEXITCODE -ne 0) {
        if (($addOut | Out-String) -match 'index\.lock') {
            $wait = [Math]::Min(300 * $i, 4000) + (Get-Random -Minimum 0 -Maximum 500)
            Write-Host "  · 索引被占用（git add），重试 $i/$maxTry，等 $([Math]::Round($wait/1000,1))s ..." -ForegroundColor DarkGray
            Start-Sleep -Milliseconds $wait
            continue
        }
        Write-Host ""
        $addOut | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        Write-Host "✘ git add 失败（非锁竞争）。未提交。" -ForegroundColor Red
        if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
        exit 1
    }

    $out = git @gitArgs 2>&1
    if ($LASTEXITCODE -eq 0) {
        if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
        Write-Host ""
        git --no-pager log -1 --format='  %h  %an  %s'
        Write-Host "✔ 提交成功（Work-Package: $As）" -ForegroundColor Green
        exit 0
    }
    # index.lock 竞争：别的员工也在提交（或某人的钩子正在跑 clippy）
    if (($out | Out-String) -match 'index\.lock') {
        $wait = [Math]::Min(300 * $i, 4000) + (Get-Random -Minimum 0 -Maximum 500)
        Write-Host "  · 索引被占用，重试 $i/$maxTry，等 $([Math]::Round($wait/1000,1))s ..." -ForegroundColor DarkGray
        Start-Sleep -Milliseconds $wait
        continue
    }
    Write-Host ""
    $out | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    Write-Host ""
    Write-Host "✘ 提交失败（多半是门禁未过）。修好后重跑本命令。" -ForegroundColor Red
    if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
    exit 1
}

Write-Host "✘ 索引持续被占用（已重试 $maxTry 次，约 1 分钟）。" -ForegroundColor Red
Write-Host "  5 人并行提交时这是正常的 —— 别的同事的钩子可能正在跑 clippy。" -ForegroundColor DarkGray
Write-Host "  稍等片刻重跑本命令即可，你的改动还在工作区，不会丢。" -ForegroundColor DarkGray
if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
exit 1

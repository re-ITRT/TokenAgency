# TokenAgency PR 入口 —— 员工**不直接提交 master**，而是在自己的分支上组装一个 PR，
# 交主管评审后由主管合并。
#
# 为什么改（第 11 轮）：
#   * 之前员工用 commit.ps1 直提 master，于是**在途的半成品就在 main 上**。
#     E2 写 diag 的那段时间 ta-core 编不过，E4/E5/E3 全被挡住一轮（docs/13 L16）。
#   * 直提还让主管没有评审点：main 变成"谁的活干到哪算哪"，而不是"过审的东西"。
#   * 用户要求"越来越像真实团队"：自己 fork 出去 → 自己 commit → 提 PR → 主管审。
#
# 怎么做到的（关键：**不需要 second worktree，也不动工作区**）：
#   用一个**临时 index**（GIT_INDEX_FILE）从分支基点 read-tree，只把该员工范围内的
#   改动 `git add` 进去，再 `git write-tree` + `git commit-tree` 造出提交，
#   最后 `git update-ref refs/heads/wp/<员工>` 把分支指针挪过去。
#   全程**不切分支、不动 HEAD、不动真实索引、不动工作区文件** —— 所以五个员工
#   可以在同一棵树上并行干活，而 main 只接收过审的东西。
#
# 用法：
#   # 员工提 PR（可反复调用，每次都往分支上追加一个提交）
#   pwsh -File scripts/pr.ps1 -As E4 -Paths crates/ta-kernel -Title "B4 内核 stdout 转发进 tracing" -MessageFile msg.txt
#   # 看自己的 PR 状态 / 全部 PR
#   pwsh -File scripts/pr.ps1 -List
#   # 看某个 PR 的完整 diff
#   pwsh -File scripts/pr.ps1 -Show wp/e4
#
# 约定：
#   * `-Paths` 必须落在你的独占范围内（范围表见 scripts/_team.ps1，权威定义）
#   * 多行中文**必须**用 `-MessageFile`（PowerShell 会把多行参数切碎）
#   * 提交前会跑 `scripts/gate.ps1`（与 git 钩子**同一份**检查）；不过就不产生提交
#   * 主管评审后合并；分支保留，便于 `git log` 溯源到你

[CmdletBinding()]
param(
    [ValidateSet('E1', 'E2', 'E3', 'E4', 'E5')]
    [string]$As,

    [string[]]$Paths,
    [string]$Title,
    [string]$Message,
    [string]$MessageFile,
    [string]$AgentId,

    # 明确承担"提交范围外文件"的责任（会写进 trailer，可审计）
    [string]$Override,

    [switch]$DryRun,
    [switch]$NoGate,

    # 从基点重新开始，丢弃分支上已有的提交。用于改基点、或修掉 PR 里不该有的东西
    # （例如隐式带上来的 Cargo.lock）。不加就是**追加**一个提交。
    [switch]$Reset,

    # 我（主管）之外的**同行评审人**：必须是另一个员工，不能是作者自己。
    # 会写进 trailer 并出现在 -List 里 —— 评审不是"有人会看"，而是**指派人**。
    [ValidateSet('E1', 'E2', 'E3', 'E4', 'E5')]
    [string]$Reviewer,

    [switch]$List,

    # 同步状态：master 走到哪、你的 PR 基点落后多少、你范围内有没有未提交改动、
    # 最近 master 上落在你范围内的文件。**每次动手前先跑这个。**
    [switch]$Status,

    [string]$Show
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\_team.ps1"

function Fail($msg) { Write-Host "`n✘ $msg`n" -ForegroundColor Red; exit 1 }
function Info($msg) { Write-Host "› $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "✔ $msg" -ForegroundColor Green }
function Head($msg) { Write-Host "`n=== $msg ===" -ForegroundColor Magenta }

# git 的**文本**输出收成字符串。空输出在 PowerShell 里会变成 `$null`，
# 直接 `.Trim()` 会抛 "不能对值为 Null 的表达式调用方法"。
#
# 这个坑是 E2 实测抓到的：`pr.ps1 -List` 在没有 `Reviewer:` trailer 的分支上崩，
# 而崩的恰恰是"没有评审人"的那几个 —— **最该被这条新规则暴露的对象反而看不见**。
# 同族风险：`git diff --shortstat` 在无差异时也是空输出。
function GitText {
    param([string[]]$GitArgs)
    $out = @(& git @GitArgs 2>$null) -join "`n"
    return "$out".Trim()
}

$root = (git rev-parse --show-toplevel).Trim()
Set-Location $root
$base = (git rev-parse $BaseBranch).Trim()

if ($Reviewer -and $Reviewer -eq $As) {
    Fail "评审人不能是作者自己（$As）—— 同行评审的全部意义就是「另一个人看」"
}

# ================================================================= -Status
if ($Status) {
    if (-not $As) { Fail '-Status 需要 -As E1..E5' }
    $branch = Get-PrBranch $As
    $scopes = Get-ScopeFor $As
    Head "同步状态：$As（分支 $branch）"

    $headShort = (git log -1 --format='%h %s' master).Trim()
    Write-Host "  master 现在：$headShort" -ForegroundColor White

    $tipRaw = (git rev-parse --verify -q "refs/heads/$branch")
    if (-not $tipRaw) {
        Write-Host '  你还没有 PR 分支 —— 工作区的改动还没提过 PR' -ForegroundColor Yellow
    } else {
        $tip = $tipRaw.Trim()
        $baseSha = (git merge-base master $tip).Trim()
        $behind = (git rev-list --count "$tip..master").Trim()
        $ahead = (git rev-list --count "$baseSha..$tip").Trim()
        Write-Host "  你的 PR 基点：$($baseSha.Substring(0,7))"
        Write-Host "  你的分支：$ahead 个提交"
        if ($behind -ne '0') {
            Write-Host "  ⚠ master 已经往前走了 $behind 个提交 —— 下次 pr.ps1 会自动基于新 master 快照，但你**必须重新读一遍你依赖的文件**" -ForegroundColor Yellow
            Write-Host '    master 上落在你范围内的改动：'
            $touched = @(git diff --name-only "$baseSha..master" -- $scopes)
            if ($touched.Count -eq 0) { Write-Host '      （无）' -ForegroundColor DarkGray }
            else { $touched | ForEach-Object { Write-Host "      $_" } }

            # ── 基点体检（E1 报的真实事故：master 被改写过）──────────────────
            #
            # 症状：`git diff master...branch` 里冒出**不属于你范围**的文件，合并时等于把
            # 别人的改动"复活"进主干。E1 就撞到过：它 `-Reset` 时的 master 是 `35d4d99`
            # （一笔误含 E2 的 201 行代码的提交），随后主干被改写为 `d843946`，
            # 于是它分支里的差异把 E2 的代码当成了自己的。
            #
            # 两条判据，任一命中就提示必须 `-Reset` —— 只说"重新读文件"是不够的，
            # 因为这种情况下**追加提交永远修不好**，必须按新基点重建分支。
            $recordedBase = GitText @('log', '-1', '--format=%(trailers:key=Base,valueonly)', $tip)
            $staleBase = $false
            if ($recordedBase) {
                $full = GitText @('rev-parse', "$recordedBase^{commit}")
                if ($full) {
                    & git merge-base --is-ancestor $full master 2>$null
                    if ($LASTEXITCODE -ne 0) { $staleBase = $true }
                }
            }
            $foreignInDiff = @(git diff --name-only "$baseSha..$tip" |
                Where-Object { $_ -and (-not (Test-InScope $_ $scopes)) })
            if ($staleBase -or $foreignInDiff.Count -gt 0) {
                Write-Host ''
                Write-Host '  ✘ 基点体检不通过 —— **追加提交修不好这个**，必须重建分支' -ForegroundColor Red
                if ($staleBase) {
                    Write-Host "     记录在提交里的基点 $recordedBase 已不是当前 master 的祖先 → master 被改写过了" -ForegroundColor Red
                }
                if ($foreignInDiff.Count -gt 0) {
                    Write-Host "     你的分支差异里有 $($foreignInDiff.Count) 个**不属于你范围**的文件（合并会把这些复活进主干）：" -ForegroundColor Red
                    $foreignInDiff | Select-Object -First 10 | ForEach-Object { Write-Host "        $_" -ForegroundColor Red }
                }
                Write-Host '     处置（照 E1 那次的正确做法，先用只读预演确认）：' -ForegroundColor Yellow
                Write-Host '       git merge-tree --write-tree master <你的分支>   # 只读，不改任何东西'
                Write-Host '       pwsh -File scripts/pr.ps1 -As <你> -Paths <你的范围> -Title "..." -MessageFile <文件> -Reset'
                Write-Host '     `-Reset` 会按**当前 master** 重建分支快照；内容取自工作区，不会丢代码。' -ForegroundColor DarkGray
                Write-Host '     重建前请先确认工作区里你的改动是完整的（`-Status` 下面那段就是查这个）。' -ForegroundColor DarkGray
            }
        } else {
            Write-Host '  ✔ 你的基点就是当前 master' -ForegroundColor Green
        }
    }

    # master 上所有与你相关的改动（跨范围也算：依赖方向上的别人）
    $allTouched = @(git diff --name-only master~20..master 2>$null)
    $relevant = @($allTouched | Where-Object { Test-InScope $_ $scopes })
    if ($relevant.Count -gt 0) {
        Write-Host '  最近 20 个提交里落在你范围内的文件：'
        $relevant | Select-Object -First 12 | ForEach-Object { Write-Host "      $_" }
    }

    $dirty = @(git status --porcelain --untracked-files=all -- $scopes)
    if ($dirty.Count -gt 0) {
        Write-Host "  你范围内有 $($dirty.Count) 个未提交改动：" -ForegroundColor Yellow
        $dirty | Select-Object -First 15 | ForEach-Object { Write-Host "      $($_.Substring(0, [Math]::Min(2, $_.Length))) $($_.Substring(3))" }
        Write-Host '      提示：提交过的里程碑要提 PR；未提交的改动会在"合并"时挡住主管（merge 拒绝覆盖工作区）' -ForegroundColor DarkGray
    } else {
        Write-Host '  ✔ 你范围内没有未提交改动' -ForegroundColor Green
    }
    Write-Host ''
    return
}

# ================================================================= -List
if ($List) {
    Head "在提的 PR（分支 wp/*，基点 $BaseBranch）"
    $branches = @(git for-each-ref --format='%(refname:short)' refs/heads/wp/ 2>$null | Where-Object { $_ })
    if ($branches.Count -eq 0) { Write-Host '  （暂无）'; return }
    foreach ($b in $branches) {
        $ahead = (git rev-list --count "$base..$b").Trim()
        $behind = (git rev-list --count "$b..$base").Trim()
        $stat = GitText @('diff', '--shortstat', "$base...$b")
        $last = GitText @('log', '-1', '--format=%h %s', $b)
        $who = GitText @('log', '-1', '--format=%an', $b)
        $rev = GitText @('log', '-1', '--format=%(trailers:key=Reviewer,valueonly)', $b)
        Write-Host ""
        Write-Host "  $b" -ForegroundColor White
        Write-Host "    提交 $ahead 个，落后 $behind 个 | $stat"
        Write-Host "    最后：$last"
        Write-Host "    作者：$who"
        if ($rev) { Write-Host "    同行评审：$rev" -ForegroundColor Cyan }
        else { Write-Host "    ⚠ 未指派同行评审（提 PR 时加 -Reviewer E?）" -ForegroundColor Yellow }
    }
    Write-Host ''
    Write-Host '  提示：主管合并前会核「同行评审意见 → 作者回应 → 合并门禁」三样。' -ForegroundColor DarkGray
    return
}

# ================================================================= -Show
if ($Show) {
    $ref = if ($Show -like 'refs/*') { $Show } elseif ($Show -like 'wp/*') { "refs/heads/$Show" } else { "refs/heads/wp/$Show" }
    git rev-parse --verify -q $ref > $null
    if ($LASTEXITCODE -ne 0) { Fail "找不到分支 $Show" }
    Head "PR diff：$BaseBranch...$Show"
    git log --oneline "$base..$ref"
    Write-Host ''
    git diff --stat "$base...$ref"
    Write-Host ''
    git diff "$base...$ref"
    return
}

# ================================================================= 提交 PR
if (-not $As) { Fail '要提 PR 就得给 -As E1..E5（或使用 -List / -Show）' }
if (-not $Title) { Fail '-Title 必填：PR 的标题就是将来合并后的提交标题' }
if (-not $Paths -or $Paths.Count -eq 0) { Fail '-Paths 必填：写你的独占范围，例如 crates/ta-kernel' }
if ($Message -and $MessageFile) { Fail '-Message 与 -MessageFile 只能给一个' }

$id = $Identities[$As]
$branch = Get-PrBranch $As
$scopes = Get-ScopeFor $As

# ---------------------------------------------------------------- 1) 范围检查
$requested = @()
foreach ($p in $Paths) { $requested += ($p -split ',' | ForEach-Object { $_.Trim() }) }
$requested = @($requested | Where-Object { $_ })
$outside = @($requested | Where-Object { -not (Test-InScope $_ $scopes) })
if ($outside.Count -gt 0 -and -not $Override) {
    Write-Host "`n✘ 以下路径不在 $As 的独占范围内：" -ForegroundColor Red
    $outside | ForEach-Object { Write-Host "    $_" }
    Write-Host "`n  你的范围：" -ForegroundColor Yellow
    $scopes | ForEach-Object { Write-Host "    $_" }
    Write-Host "`n  确实需要范围外的文件时：-Override `"一句话说明原因`"（会写进 trailer，可审计）" -ForegroundColor Yellow
    Write-Host "  需要别人改他的文件时：发消息给主管，不要越界。`n"
    exit 1
}

# ---------------------------------------------------------------- 2) 收集实际改动
#
# 先只看**你自己范围**内的改动。
$ownChanged = @()
foreach ($line in @(git status --porcelain --untracked-files=all -- $requested)) {
    if (-not $line) { continue }
    $p = $line.Substring(3)
    if ($p -match ' -> ') { $p = ($p -split ' -> ')[1] }
    $ownChanged += $p.Trim('"')
}
$ownChanged = @($ownChanged | Sort-Object -Unique)

# 隐式范围（`Cargo.lock`）**只在确实由本次改动引起时**才带上：
# 只有你动了某个 `Cargo.toml`，锁文件才可能是你造成的。
#
# 无条件带上会把**别人**在途的依赖变更捎进你的 PR —— 真实发生过：E4 改了
# `crates/ta-kernel/Cargo.toml`（在途未提交），E2 提 PR 时就多出一个不属于它的
# `Cargo.lock` 变更。合并它等于让 main 的锁文件先于 manifest 落地。
$touchedManifest = @($ownChanged | Where-Object { $_ -like '*Cargo.toml' }).Count -gt 0
$searchPaths = if ($touchedManifest) {
    @($requested + $ImplicitScopes | Sort-Object -Unique)
} else {
    $requested
}

$changed = @()
foreach ($line in @(git status --porcelain --untracked-files=all -- $searchPaths)) {
    if (-not $line) { continue }
    $p = $line.Substring(3)
    if ($p -match ' -> ') { $p = ($p -split ' -> ')[1] }
    $changed += $p.Trim('"')
}
$changed = @($changed | Sort-Object -Unique)
if ($changed.Count -eq 0) {
    Fail "在 $($requested -join ', ') 下没有发现任何改动 —— 没有东西可以提 PR。`n  先干活，或者确认你的路径写对了。"
}

Info "本次 PR 涉及 $($changed.Count) 个文件："
$changed | ForEach-Object { Write-Host "    $_" }

# ---------------------------------------------------------------- 3) 临时 index 造树
$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("ta-pr-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmpDir -Force > $null
$idx = Join-Path $tmpDir 'index'

$tipRaw = (git rev-parse --verify -q "refs/heads/$branch")
$hasBranch = ($LASTEXITCODE -eq 0 -and $tipRaw)
$tip = if ($hasBranch) { $tipRaw.Trim() } else { $null }
$parent = if ($hasBranch -and -not $Reset) { $tip } else { $base }
$seed = $parent
if ($Reset -and $hasBranch) { Write-Host "› -Reset：分支 $branch 将从基点 $($base.Substring(0,7)) 重新开始（已有提交被丢弃）" -ForegroundColor Yellow }

$savedIndex = $env:GIT_INDEX_FILE
try {
    $env:GIT_INDEX_FILE = $idx
    git read-tree $seed
    if ($LASTEXITCODE -ne 0) { Fail "read-tree $seed 失败" }
    git add -- $changed
    if ($LASTEXITCODE -ne 0) { Fail 'git add 失败（临时索引）' }
    $tree = (git write-tree).Trim()
    if ($LASTEXITCODE -ne 0) { Fail 'write-tree 失败' }
} finally {
    if ($savedIndex) { $env:GIT_INDEX_FILE = $savedIndex }
    else { Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue }
}

$parentTree = (git rev-parse "$seed^{tree}").Trim()
if ($tree -eq $parentTree) {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    Fail "这一批改动与 $branch 的当前内容完全一致 —— 没有新东西可提。"
}

# ---------------------------------------------------------------- 4) 门禁（与钩子同一份）
if (-not $NoGate) {
    Head '门禁 scripts/gate.ps1（与 git 钩子同一份检查）'
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'gate.ps1') -IndexFile $idx
    if ($LASTEXITCODE -ne 0) {
        Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
        Fail "门禁未通过 —— **没有产生任何提交**。修好后重新跑同一条命令即可。"
    }
} else {
    Write-Host '! 跳过门禁（-NoGate）：只允许在隔离复现里用，别用来绕过。' -ForegroundColor Yellow
}

# ---------------------------------------------------------------- 5) 组装提交
$body = ''
if ($MessageFile) {
    if (-not (Test-Path -LiteralPath $MessageFile)) { Fail "找不到 -MessageFile：$MessageFile" }
    $body = (Get-Content -LiteralPath $MessageFile -Raw).TrimEnd()
} elseif ($Message) {
    $body = $Message.TrimEnd()
}

$trailers = @(
    '',
    "PR-Branch: $branch",
    "Base: $($base.Substring(0, 7))",
    "Work-Package: $As",
    "Scoped-Paths: $($requested -join ' ')"
)
if ($AgentId) { $trailers += "Agent: $AgentId" }
if ($Reviewer) { $trailers += "Reviewer: $Reviewer" }
if ($Override) { $trailers += "Override: $Override" }

$fullMsg = if ($body) { "$Title`n`n$body`n$($trailers -join "`n")" } else { "$Title`n$($trailers -join "`n")" }
$msgPath = Join-Path $tmpDir 'msg.txt'
Set-Content -LiteralPath $msgPath -Value $fullMsg -Encoding UTF8

if ($DryRun) {
    Head '干跑：不产生任何提交、不动任何分支'
    Write-Host "  分支：$branch"
    Write-Host "  基点：$($base.Substring(0, 7))（$BaseBranch）"
    Write-Host "  父提交：$($parent.Substring(0, 7))$(if ($hasBranch) { '（追加到已有 PR 分支）' } else { '（新建 PR 分支）' })"
    Write-Host "  树：$tree"
    Write-Host "  提交信息："
    $fullMsg -split "`n" | ForEach-Object { Write-Host "  | $_" }
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    return
}

$savedAuthor = @{ N = $env:GIT_AUTHOR_NAME; E = $env:GIT_AUTHOR_EMAIL; CN = $env:GIT_COMMITTER_NAME; CE = $env:GIT_COMMITTER_EMAIL }
try {
    $env:GIT_AUTHOR_NAME = $id.Name; $env:GIT_AUTHOR_EMAIL = $id.Email
    $env:GIT_COMMITTER_NAME = $id.Name; $env:GIT_COMMITTER_EMAIL = $id.Email
    $commit = (& git commit-tree $tree -p $parent -F $msgPath 2>&1 | Select-Object -Last 1)
    if ($LASTEXITCODE -ne 0) { Fail "commit-tree 失败：$commit" }
    $commit = "$commit".Trim()
} finally {
    $env:GIT_AUTHOR_NAME = $savedAuthor.N; $env:GIT_AUTHOR_EMAIL = $savedAuthor.E
    $env:GIT_COMMITTER_NAME = $savedAuthor.CN; $env:GIT_COMMITTER_EMAIL = $savedAuthor.CE
}

$oldValue = if ($hasBranch) { $tip } else { '0000000000000000000000000000000000000000' }
& git update-ref "refs/heads/$branch" $commit $oldValue
if ($LASTEXITCODE -ne 0) {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    Fail "update-ref 失败（分支 $branch 可能被别人动过，重跑一次）"
}
Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue

# ---------------------------------------------------------------- 6) PR 摘要
$short = $commit.Substring(0, 7)
Head "PR 已提交：$branch @ $short"
Write-Host "  标题：$Title"
Write-Host "  作者：$($id.Name) <$($id.Email)>"
Write-Host "  基点：$($base.Substring(0, 7))（$BaseBranch）"
Write-Host "  相对 $BaseBranch 的全量差异："
git diff --stat "$base...refs/heads/$branch" | ForEach-Object { Write-Host "    $_" }
Write-Host ''
Write-Host "  分支上已有 $((git rev-list --count "$base..refs/heads/$branch").Trim()) 个提交（相对 $BaseBranch）"
Write-Host ''
Write-Host "  主管评审用：pwsh -File scripts/pr.ps1 -Show $branch" -ForegroundColor Cyan
Ok '已通知主管（下一步：主管评审 → 合并；评审不通过会在你的 PR 分支上追加修改）'

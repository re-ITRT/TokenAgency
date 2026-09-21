# 主管合并 PR —— 处理"共享工作树 + PR 分支"模型下的合并阻碍，并机械执行一条纪律。
#
# 为什么需要这个脚本：
#   我们五个员工共享同一棵工作树（为了让文件工具的相对路径一直有效）。员工提 PR 时，
#   内容**同时存在于工作区**（未提交/未跟踪）**和**分支上。于是 `git merge <分支>` 会：
#     * 对"已修改"的文件：`Your local changes would be overwritten` → Aborting
#     * 对"未跟踪"的文件：`untracked working tree files would be overwritten` → Aborting
#   两者都**不会**覆盖员工的工作（git 宁可报错也不覆盖 —— 这是好事），但也不让合并发生。
#
#   安全的前提：**工作区里该文件的内容必须与分支上的完全一致**（逐字节，用 blob hash 比）。
#   一致时，"先让工作区回到 HEAD 版本、再 merge"与"直接 merge"结果相同 —— 所以可以放心做。
#   不一致时**绝不能猜**：那说明员工在提交后又改了同一批文件（违反协议），或别人动了它。
#   那种情况下合并会把他的新工作"复活/覆盖"，一律拒绝并让人先追加提交。
#
#   所以这个脚本一石二鸟：**既解开合并阻碍，又把"提交后不要改同一批文件"变成机械检查。**
#
# 用法：
#   pwsh -File scripts/merge-pr.ps1 -Branch wp/e4 -DryRun    # 先预演，看清楚会动哪些文件
#   pwsh -File scripts/merge-pr.ps1 -Branch wp/e4            # 真合并（--no-ff，保留 PR 边界）

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Branch,
    [switch]$DryRun,
    # 合并提交的信息（默认自动生成）
    [string]$Message
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\_team.ps1"

function Fail($msg) { Write-Host "`n✘ $msg`n" -ForegroundColor Red; exit 1 }
function Info($msg) { Write-Host "› $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "✔ $msg" -ForegroundColor Green }
function Head($msg) { Write-Host "`n=== $msg ===" -ForegroundColor Magenta }

$root = (git rev-parse --show-toplevel).Trim()
Set-Location $root

# 规范化分支名：接受 e4 / wp/e4 / refs/heads/wp/e4
$ref = if ($Branch -like 'refs/*') { $Branch }
elseif ($Branch -like 'wp/*') { "refs/heads/$Branch" }
else { "refs/heads/wp/$Branch" }
$short = $ref -replace '^refs/heads/', ''
& git rev-parse --verify -q $ref > $null
if ($LASTEXITCODE -ne 0) { Fail "找不到分支 $Branch（已试 $ref）" }

$base = (git rev-parse $BaseBranch).Trim()
$tip = (git rev-parse $ref).Trim()
Head "合并 PR：$short → $BaseBranch"

# ---------------------------------------------------------------- 1) 先看差异面
$files = @(git diff --name-only "$base...$ref" 2>$null | Where-Object { $_ })
if ($files.Count -eq 0) { Fail "$short 相对 $BaseBranch 没有差异 —— 没什么可合并的。" }
Write-Host "  PR 涉及 $($files.Count) 个文件；基点 $((git merge-base $base $tip).Trim().Substring(0,7))"

# ---------------------------------------------------------------- 2) 安全性：工作区 vs 分支，逐字节
$mismatch = @()
$cleanupTracked = @()
$cleanupUntracked = @()
foreach ($f in $files) {
    $branchHash = (git rev-parse --verify -q "$ref`:$f" 2>$null)
    $onBranch = ($LASTEXITCODE -eq 0 -and $branchHash)

    if (-not (Test-Path -LiteralPath $f)) {
        # 工作区没有这个文件：合并会按分支内容创建它（或删除，若分支也没有）。无需处理。
        continue
    }
    $wtHash = (git hash-object -- $f).Trim()

    if ($onBranch) {
        if ($wtHash -ne $branchHash.Trim()) {
            $mismatch += "$f  (工作区 $($wtHash.Substring(0,8)) ≠ 分支 $($branchHash.Trim().Substring(0,8)))"
        } else {
            # 内容一致 → 可安全让 merge 接管
            $tracked = (git ls-files --error-unmatch -- $f 2>$null)
            if ($LASTEXITCODE -eq 0) { $cleanupTracked += $f } else { $cleanupUntracked += $f }
        }
    } else {
        # 分支上删除了这个文件：工作区若还有内容且与 HEAD 不同 → 说明有人在改它
        $headHash = (git rev-parse --verify -q "$base`:$f" 2>$null)
        if ($headHash -and $wtHash -ne $headHash.Trim()) {
            $mismatch += "$f  (分支要删除它，但工作区内容与 $BaseBranch 不一致)"
        }
    }
}

if ($mismatch.Count -gt 0) {
    Write-Host ""
    Write-Host "✘ 有 $($mismatch.Count) 个文件**工作区内容与 PR 分支不一致** —— 拒绝合并" -ForegroundColor Red
    $mismatch | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    Write-Host ""
    Write-Host "  这通常意味着：**作者在提交 PR 之后又改了同一批文件。**" -ForegroundColor Yellow
    Write-Host "  这时合并会把他的新工作复活或覆盖，所以停手。正确处置：" -ForegroundColor Yellow
    Write-Host "   ① 让作者把新改动**追加提交到同一条 PR 分支**（再跑一次 pr.ps1，不加 -Reset）；" -ForegroundColor Cyan
    Write-Host "   ② 或确认那批改动确实不该进这次 PR —— 那也要作者自己说明，不由主管替他决定。" -ForegroundColor Cyan
    Write-Host ""
    exit 4
}
Ok "工作区与分支逐字节一致（$($cleanupTracked.Count) 个已跟踪 + $($cleanupUntracked.Count) 个未跟踪），可以安全合并"

if ($DryRun) {
    Head '干跑：下面是合并前会做的清理动作'
    if ($cleanupTracked.Count) { Write-Host "  回到 $BaseBranch 版本（merge 随后会更新它们）："; $cleanupTracked | ForEach-Object { Write-Host "    $_" } }
    if ($cleanupUntracked.Count) { Write-Host "  删除（merge 随后会以分支内容重建）："; $cleanupUntracked | ForEach-Object { Write-Host "    $_" } }
    Write-Host "`n  然后：git merge --no-ff --no-edit $short"
    Write-Host '  （干跑未改动任何文件、未合并）'
    return
}

# ---------------------------------------------------------------- 3) 清理阻碍（内容已验证一致）
foreach ($f in $cleanupTracked) { git checkout -- $f; if ($LASTEXITCODE -ne 0) { Fail "checkout $f 失败" } }
foreach ($f in $cleanupUntracked) { Remove-Item -LiteralPath $f -Force -ErrorAction SilentlyContinue }

# ---------------------------------------------------------------- 4) 合并
$pat = $short
if ($Message) { & git merge --no-ff --no-edit -m $Message $pat }
else {
    $lastMsg = (git log -1 --format='%s' $ref).Trim()
    # 注意：`"$short:"` 会被 PowerShell 当成作用域引用 → 必须用 `${short}` 分隔
    & git merge --no-ff --no-edit -m "Merge ${short}: $lastMsg" $pat
}
if ($LASTEXITCODE -ne 0) {
    Fail "git merge 失败（可能有冲突）。请人工处理：git status 看冲突文件，**不要**用 -X theirs/ours。"
}

# ---------------------------------------------------------------- 5) 复核内容没被改坏
$bad = @()
foreach ($f in $files) {
    $branchHash = (git rev-parse --verify -q "$ref`:$f" 2>$null)
    if (-not $branchHash) { continue }
    if (-not (Test-Path -LiteralPath $f)) { $bad += "$f 已消失"; continue }
    if ((git hash-object -- $f).Trim() -ne $branchHash.Trim()) { $bad += "$f 内容与分支不一致" }
}
if ($bad.Count -gt 0) {
    Write-Host "⚠ 合并后复核发现 $($bad.Count) 处异常：" -ForegroundColor Yellow
    $bad | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
} else {
    Ok "合并后复核：$($files.Count) 个文件的内容与分支逐字节一致"
}

Head "结果"
git log --oneline -1
Write-Host "`n  下一步（合并的验收不能省）：" -ForegroundColor DarkGray
Write-Host "    cargo fmt --all --check" -ForegroundColor DarkGray
Write-Host "    cargo check  --workspace --all-targets" -ForegroundColor DarkGray
Write-Host "    cargo clippy --workspace --all-targets -- -D warnings" -ForegroundColor DarkGray
Write-Host "    cargo test   --workspace" -ForegroundColor DarkGray
Write-Host "    （git merge 不触发 pre-commit，主管的门禁就是唯一门禁 —— docs/13 §7.6.4）" -ForegroundColor DarkGray

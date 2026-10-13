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
        # 内联取值（本脚本没有 gate.ps1 里那个 GitText 助手）
        $headHashRaw = (& git rev-parse --verify -q "HEAD:$f" 2>$null)
        $headHash = if ($headHashRaw) { "$headHashRaw".Trim() } else { '' }
        if ($wtHash -eq $branchHash.Trim()) {
            # 工作区 == 分支内容 → 清掉它，让 merge 接管
            $tracked = (git ls-files --error-unmatch -- $f 2>$null)
            if ($LASTEXITCODE -eq 0) { $cleanupTracked += $f } else { $cleanupUntracked += $f }
        } elseif ($headHash -and $wtHash -eq $headHash) {
            # 工作区 == HEAD 内容 → **本来就干净**，merge 会正常更新它，什么都不用做。
            #
            # ⚠️ 这一格是踩坑后补的：原来只比"工作区 vs 分支"，于是**派生文件**会被误判。
            # 典型是 `Cargo.lock` —— 作者改了自己的 `Cargo.toml`（合法，例如加 dev-dependency），
            # 于是分支上的锁与工作区不同；而工作区那份其实与 master **逐字节一致**（干净）。
            # 旧逻辑会报"作者在提交后又改了文件"并拒绝合并，**把主管卡死在自己工具的误报上**。
            continue
        } else {
            $mismatch += "$f  (工作区 $($wtHash.Substring(0,8)) ≠ 分支 $($branchHash.Trim().Substring(0,8))，也 ≠ HEAD)"
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

# ---------------------------------------------------------------- 3) 先**只读预演**合并
#
# ⚠️ 这一步是踩坑之后加的：清理工作区（回退/删除文件）**发生在前**，如果随后合并因为
# 冲突或别的原因失败，工作区就停在"和 PR 分支不一致"的状态 —— 而本脚本下一次运行
# 会因此判定"作者在提交后又改了文件"并拒绝，**把人卡住**（真实发生过：wp/e5）。
# 所以：先确认能合（只读、不动任何文件），再动工作区。
& git merge-tree --write-tree $base $ref > $null 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "`n✘ 只读预演发现合并会冲突（工作区未被改动）：" -ForegroundColor Red
    & git merge-tree --write-tree $base $ref 2>&1 | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" }
    Write-Host "  请作者按新基点重建（`-Reset`）或人工解冲突 —— **不要**用 -X theirs/ours。" -ForegroundColor Yellow
    exit 5
}
Ok "只读预演：可干净合并（工作区尚未改动）"

# ---------------------------------------------------------------- 4) 清理阻碍（内容已验证一致）
foreach ($f in $cleanupTracked) { git checkout -- $f; if ($LASTEXITCODE -ne 0) { Fail "checkout $f 失败" } }
foreach ($f in $cleanupUntracked) { Remove-Item -LiteralPath $f -Force -ErrorAction SilentlyContinue }

# ---------------------------------------------------------------- 5) 合并
$pat = $short
if ($Message) { & git merge --no-ff --no-edit -m $Message $pat }
else {
    $lastMsg = (git log -1 --format='%s' $ref).Trim()
    # 注意：`"$short:"` 会被 PowerShell 当成作用域引用 → 必须用 `${short}` 分隔
    & git merge --no-ff --no-edit -m "Merge ${short}: $lastMsg" $pat
}
if ($LASTEXITCODE -ne 0) {
    # 合并失败 → **把工作区还原成 PR 分支的内容**，不留一个"和分支不一致"的树
    Write-Host "⚠ 合并失败 —— 正在把工作区恢复为 PR 分支的内容（避免留下不一致的树）" -ForegroundColor Yellow
    & git checkout $ref -- @($files) 2>$null
    & git reset --quiet 2>$null
    Fail "git merge 失败（可能有冲突）。请人工处理：git status 看冲突文件，**不要**用 -X theirs/ours。"
}

# ---------------------------------------------------------------- 6) 复核内容没被改坏
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
Write-Host "    （git merge 不触发 pre-commit，主管的门禁就是唯一门禁）" -ForegroundColor DarkGray

#!/usr/bin/env pwsh
# 在途审计 —— 找出"完成但没入库"的交付（docs/13 L22）
#
# 为什么需要这个脚本：
#   五个人并行开发时，**工作区有在途文件是正常的**，所以单看 `git status` 什么也发现不了。
#   真正会漏掉的是另一种情况：**某个 crate 有大量在途改动，而它已经很久没有被提交过** ——
#   那意味着"一份完成的交付只存在于工作区"。第 9 轮就漏掉了一个 4508 行、零提交历史的
#   ta-kernel 交付（68 个测试全过，但 git 里只有初始骨架）。
#
#   判据不是"有没有在途"，而是 **「在途了多久」×「最后一次提交是什么时候」** 两个一起看。
#
# 用法：
#   pwsh -File scripts/audit-work.ps1
#   pwsh -File scripts/audit-work.ps1 -WarnHours 2      # 自定义告警阈值
#   pwsh -File scripts/audit-work.ps1 -StaleMinutes 45  # 在途文件多久没动算"停滞"
#
# 退出码：0 = 无告警，1 = 有告警（可用于 CI / W5 前置检查）

[CmdletBinding()]
param(
    # 某个 crate 超过这么多小时没提交、且仍有在途改动 → 告警
    [double]$WarnHours = 3,
    # 在途文件超过这么多分钟没被写过 → 视为"停滞的在途"（人已经停手，但东西没提交）
    [int]$StaleMinutes = 30
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

Write-Host ""
Write-Host "=== 在途与提交历史对照（docs/13 L22）===" -ForegroundColor Cyan
Write-Host "判据：有在途是正常的；'在途很多 + 很久没提交' 才是危险信号。" -ForegroundColor DarkGray
Write-Host ""

# 关注的目录：crate 与前端源码
$targets = @()
$targets += Get-ChildItem -Path crates -Directory -ErrorAction SilentlyContinue | ForEach-Object { "crates/$($_.Name)" }
foreach ($d in 'src', 'src-tauri', 'docs', 'scripts', '.githooks') {
    if (Test-Path $d) { $targets += $d }
}

$rows = @()
foreach ($t in $targets) {
    # 在途文件（含未跟踪）
    $pending = @(git status --porcelain -- $t 2>$null)
    $nPending = $pending.Count

    # 最后一次提交
    $lastLine = git log -1 --format='%h|%ct|%an|%s' -- $t 2>$null
    if ([string]::IsNullOrWhiteSpace($lastLine)) {
        $hash = '—'; $ts = $null; $who = '（从未提交）'; $subj = ''
    }
    else {
        $parts = $lastLine -split '\|', 4
        $hash = $parts[0]; $ts = [int]$parts[1]; $who = $parts[2]; $subj = $parts[3]
    }

    # 这个路径一共被提交过几次 —— L22 的关键补充指标
    # 第 9 轮漏掉的 ta-kernel，本质信号不是"停滞时间长"，而是
    # **它历史上只有初始骨架那一次提交，却有 4508 行在途**。
    $commitCount = @(git log --oneline -- $t 2>$null).Count

    # 在途文件里最新的修改时间 → "这条路径的未提交工作最后一次被动是什么时候"
    #
    # ⚠️ 取**最新**而不是最早。
    # 取最早会把「一个 3 小时前的陈旧文件 + 一个刚写的文件」算成"停滞 3 小时"，
    # 而实际上人正在写。取最新才回答得了那个真正的问题：**人是不是已经停手了？**
    $newest = $null
    foreach ($line in $pending) {
        $p = $line.Substring(3).Trim().Trim('"')
        if (Test-Path $p) {
            $w = (Get-Item $p).LastWriteTime
            if (-not $newest -or $w -gt $newest) { $newest = $w }
        }
    }

    $sinceCommitH = if ($ts) { [Math]::Round(((Get-Date) - [DateTimeOffset]::FromUnixTimeSeconds($ts).LocalDateTime).TotalHours, 1) } else { [double]::PositiveInfinity }
    $staleMin = if ($newest) { [Math]::Round(((Get-Date) - $newest).TotalMinutes, 0) } else { $null }

    $rows += [pscustomobject]@{
        Target     = $t
        Pending    = $nPending
        Commits    = $commitCount
        LastCommit = if ($ts) { [DateTimeOffset]::FromUnixTimeSeconds($ts).LocalDateTime.ToString('MM-dd HH:mm') } else { '从未' }
        HoursAgo   = $sinceCommitH
        Who        = $who
        StaleMin   = $staleMin
        Hash       = $hash
        Subject    = if ($subj.Length -gt 34) { $subj.Substring(0, 34) + '…' } else { $subj }
    }
}

# 有在途的排前面，然后按"距上次提交"从久到近
$sorted = $rows | Sort-Object @{E = { $_.Pending -eq 0 } }, @{E = { $_.HoursAgo }; Descending = $true }
$sorted | Format-Table -AutoSize -Property Target, Pending, Commits, LastCommit, HoursAgo, Who, StaleMin, Subject |
    Out-String -Width 200 | Write-Host

# ---------------------------------------------------------------- 告警
$alerts = @()
foreach ($r in $rows) {
    if ($r.Pending -eq 0) { continue }

    # 1) 从未提交过，却有在途 → 最严重
    if ($r.Hash -eq '—') {
        $alerts += "【从未提交】$($r.Target) 有 $($r.Pending) 个在途文件，但**这个路径一条提交历史都没有**"
        continue
    }
    # 2) ★ L22 的核心信号：历史上几乎没提交过，却有在途
    #    （第 9 轮那个 4508 行的 ta-kernel 就是这样 —— 只有初始骨架一次提交）
    if ($r.Commits -le 1) {
        $alerts += "【历史空白】$($r.Target) 只有 $($r.Commits) 次提交，却有 $($r.Pending) 个在途文件 —— 很像'完成的交付从未入库'"
        continue
    }
    # 3) 久未提交且仍有在途
    if ($r.HoursAgo -ge $WarnHours) {
        $alerts += "【久未入库】$($r.Target) 有 $($r.Pending) 个在途文件，距上次提交已 $($r.HoursAgo) 小时（>$WarnHours）"
    }
    # 4) 在途停滞：东西没动很久了，说明人已经停手，但没提交
    if ($r.StaleMin -ne $null -and $r.StaleMin -ge $StaleMinutes) {
        $alerts += "【在途停滞】$($r.Target) 的 $($r.Pending) 个在途文件已 $($r.StaleMin) 分钟没被改动（人可能已停手，但没提交）"
    }
}

Write-Host ""
# 在途总量（信息性，不告警 —— 并行开发时它本来就该不是 0）
$totalPending = (git status --porcelain 2>$null | Measure-Object).Count
Write-Host "工作区在途文件总数：$totalPending （并行开发时非 0 是正常的）" -ForegroundColor DarkGray

if ($alerts.Count -eq 0) {
    Write-Host ""
    Write-Host "✔ 无告警：没有'完成但没入库'的迹象" -ForegroundColor Green
    exit 0
}

Write-Host ""
Write-Host "⚠ 发现 $($alerts.Count) 条需要处理的迹象：" -ForegroundColor Yellow
$alerts | ForEach-Object { Write-Host "  $_" -ForegroundColor Yellow }
Write-Host ""
Write-Host "处置（按 docs/13 §7.5 提交协议）：" -ForegroundColor DarkGray
Write-Host "  · 找到该路径的 Owner，让它用 scripts/commit.ps1 -As <它的身份> 提交" -ForegroundColor DarkGray
Write-Host "  · 若无法确定 Owner，按 L17 的纪律：**先让对方自证范围，不要靠时间戳推断**" -ForegroundColor DarkGray
Write-Host "  · 在途本身不是问题，'在途了很久还没入库'才是" -ForegroundColor DarkGray
exit 1

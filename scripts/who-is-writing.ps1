#!/usr/bin/env pwsh
# 谁在写仓库 / 仓库现在是否"静止"（docs/13 L24）
#
# 为什么需要这个脚本：
#   多人（或多代理）在**同一个工作目录**里并行开发时，"谁在写"是一个**看不见的事实** ——
#   `git status` 只会告诉你"有东西改了"，不会告诉你"此刻还有人正在改"。
#   而这两种情况需要完全不同的处置：
#     · 静止 + 有在途  → 可以安全接手、可以安全提交
#     · 仍在写 + 有在途 → **绝不能落笔**，否则两个写者会互相覆盖（丢更新、半成品被读到）
#
#   第 10 轮真实发生过：我新建的长期成员与上一批没被回收的子代理**同时写同一批文件**，
#   结果是 `pipeline.rs` 的中间态（`mark_lane_down` 少传一个参数）把全队的 `cargo check` 卡红。
#
# 用法：
#   pwsh -File scripts/who-is-writing.ps1                  # 默认看最近 5 分钟
#   pwsh -File scripts/who-is-writing.ps1 -Minutes 15
#   pwsh -File scripts/who-is-writing.ps1 -QuietSeconds 90 # 判定"静止"的阈值
#
# 退出码：0 = 已静止（可以安全接手），1 = 仍在写入（不要落笔）

[CmdletBinding()]
param(
    [int]$Minutes = 5,
    # 最近这么多秒内没有任何文件变化 → 判定为"静止"
    [int]$QuietSeconds = 90
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$now = Get-Date
$cut = $now.AddMinutes(-$Minutes)

Write-Host ""
Write-Host "=== 最近 $Minutes 分钟内被修改的文件 ===" -ForegroundColor Cyan

$roots = @('crates', 'src', 'src-tauri', 'docs', 'scripts', '.githooks') |
    Where-Object { Test-Path $_ }

$files = Get-ChildItem -Path $roots -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch '\\target\\|\\node_modules\\|\\\.git\\' }

$recent = $files |
    Where-Object { $_.LastWriteTime -gt $cut } |
    Sort-Object LastWriteTime -Descending

if ($recent.Count -eq 0) {
    Write-Host "  （无）" -ForegroundColor DarkGray
}
else {
    $recent |
        Select-Object @{n='Time'; e={$_.LastWriteTime.ToString('HH:mm:ss')}},
                      @{n='Ago(s)'; e={[int]($now - $_.LastWriteTime).TotalSeconds}},
                      @{n='Path'; e={$_.FullName.Replace((Get-Location).Path + '\', '')}} |
        Format-Table -AutoSize | Out-String -Width 140 | Write-Host
}

# ---------------------------------------------------------------- 活跃写者检测
Write-Host "=== 活跃写者检测（最近 $QuietSeconds 秒内有改动的文件）===" -ForegroundColor Cyan
$activeCut = $now.AddSeconds(-$QuietSeconds)
$active = $recent | Where-Object { $_.LastWriteTime -gt $activeCut }

# 按顶层领域归并，便于一眼看出"哪个领域有人在写"
$byArea = @{}
foreach ($f in $active) {
    $rel = $f.FullName.Replace((Get-Location).Path + '\', '')
    $parts = $rel -split '\\'
    $area = if ($parts[0] -eq 'crates' -and $parts.Count -gt 1) { "crates/$($parts[1])" }
            elseif ($parts[0] -eq 'src-tauri') { 'src-tauri' }
            elseif ($parts[0] -eq 'src') { 'src (前端)' }
            else { $parts[0] }
    if (-not $byArea.ContainsKey($area)) { $byArea[$area] = @() }
    $byArea[$area] += $f
}

if ($active.Count -eq 0) {
    Write-Host "  （无活跃写入）" -ForegroundColor Green
}
else {
    foreach ($k in ($byArea.Keys | Sort-Object)) {
        $newest = ($byArea[$k] | Sort-Object LastWriteTime -Descending)[0]
        $ago = [int]($now - $newest.LastWriteTime).TotalSeconds
        Write-Host ("  {0,-24} {1} 个文件，最新 {2}s 前" -f $k, $byArea[$k].Count, $ago) -ForegroundColor Yellow
    }
}

# ---------------------------------------------------------------- 编译进程
Write-Host ""
Write-Host "=== 正在跑构建的进程（cargo / rustc / vite）===" -ForegroundColor Cyan
$procs = Get-Process -Name cargo, rustc, vite, node -ErrorAction SilentlyContinue
if ($procs) {
    $procs | Select-Object Id, ProcessName, @{n='StartTime'; e={$_.StartTime.ToString('HH:mm:ss')}} |
        Format-Table -AutoSize | Out-String -Width 100 | Write-Host
}
else {
    Write-Host "  （无）" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------- 在途与提交
Write-Host "=== 在途文件总数 ===" -ForegroundColor Cyan
Write-Host "  $((git status --porcelain 2>$null | Measure-Object).Count)"
Write-Host "=== 最近 3 次提交 ===" -ForegroundColor Cyan
git --no-pager log -3 --format='  %h  %ad  %an  %s' --date=format:'%H:%M:%S'

# ---------------------------------------------------------------- 判定
Write-Host ""
if ($active.Count -eq 0) {
    Write-Host "OK 仓库已静止（最近 $QuietSeconds 秒无写入）—— 可以安全接手 / 提交" -ForegroundColor Green
    exit 0
}

Write-Host "WARN 仍有写者在活动 —— 不要落笔，否则会互相覆盖" -ForegroundColor Yellow
Write-Host "  处置：先确认这些改动属于谁；等它静止（重跑本脚本返回 0）再接手。" -ForegroundColor DarkGray
Write-Host "  若那个写者是你已经无法寻址的旧会话（第 10 轮真实发生过），" -ForegroundColor DarkGray
Write-Host "  则只能等它自然结束 —— 期间把工作切成只读（审计/计划/验收步骤）。" -ForegroundColor DarkGray
exit 1

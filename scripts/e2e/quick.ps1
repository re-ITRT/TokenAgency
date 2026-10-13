# [docs/46 §294/§297] **分情况验证**：只跑本次改动相关的那几步，别再每次都跑满 30 档。
#
#   pwsh -File scripts/e2e/quick.ps1 -Profile kernel     # 自动出口 / 内核 / 测试台（最常用）
#   pwsh -File scripts/e2e/quick.ps1 -Profile ui         # 页面渲染 / 极简 / 问号提示
#   pwsh -File scripts/e2e/quick.ps1 -Profile net        # E 期四步，**复用既有 scratch**（不重跑建状态）
#   pwsh -File scripts/e2e/quick.ps1 -Profile full       # 全量（交付前才跑）
#
# ⚠️ 两个环境坑（都踩过，见 [docs/46 §297]）：
#   1. `TEMP`/`TMP` 必须指到**工作区内**的可写目录 —— 这台机器上子进程在
#      `C:\Users\…\AppData\Local\Temp` 下建目录会拿到 `拒绝访问 (os error 5)`：
#      `msedgedriver` 报 `cannot create temp dir for user data dir`、`cargo test` 31 条一起红。
#   2. 跑之前先把上一次的 `tauri-driver` / `msedgedriver` / `tokenagency` / `mihomo` 清掉，
#      否则 4455 被占、39090 被旧内核占，后面的"内核起不来"全是假红。
param(
  [ValidateSet('kernel','ui','net','full')] [string]$Profile = 'kernel',
  [string]$Exe = 'target\verify\tokenagency.exe'
)
$ErrorActionPreference = 'Stop'
$only = @{
  # 建订阅 → 指向「自动」→ 测试台 → 读内核 → 手动刷新判定 → 冷启动收口 → 粘性
  kernel = 'step3,step10,step11,step25,step26,step27,step28,step29,step30'
  ui     = 'step2,step5,step17,step22'                  # 页面渲染、设置、节点清单、问号提示
  net    = 'step25,step26,step27,step28,step29,step30'  # E 期（+step25 复建"服务商挂自动"那一步）
  full   = ''
}[$Profile]
# `net` 档复用既有 scratch（省掉前 150 秒的建状态）；别的档要干净 scratch。
$keepData = $Profile -eq 'net'
if (-not (Test-Path $Exe)) { throw "没有 $Exe —— 先跑 pwsh -File scripts/build-fast.ps1（或 -Release）" }

# 环境坑①：TEMP 必须可写（工作区内）
$tmp = Join-Path (Resolve-Path .) '.ta-tmp\tmp'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TEMP = $tmp
$env:TMP = $tmp
if (-not $env:TA_E2E_OUT) { $env:TA_E2E_OUT = (Join-Path (Resolve-Path .) '.ta-tmp\ta-e2e') }
# 环境坑②：清掉上一轮的驱动 / 应用 / 内核
Get-Process msedgedriver,tauri-driver,tokenagency,mihomo -ErrorAction SilentlyContinue |
  Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

$args = @('scripts/e2e/smoke.mjs', '--exe', $Exe)
if ($only -ne '') { $args += @('--only', $only) }
if ($keepData) { $args += @('--keep-data') }
Write-Host ("跑 $Profile 档：" + $(if ($only) { $only } else { '全量 30 档' }) + $(if ($keepData) { '（复用 scratch）' } else { '' })) -ForegroundColor Cyan
$sw = [Diagnostics.Stopwatch]::StartNew()
node @args
"耗时 $([math]::Round($sw.Elapsed.TotalSeconds,1)) 秒"

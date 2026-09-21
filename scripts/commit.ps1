# TokenAgency 提交入口 —— 每个员工用**自己的身份**提交，且**只能提交自己范围内的文件**
#
# 为什么要有这个脚本（两个真实痛点）：
#   1. 主管排查"谁动了 X"时，之前只能靠文件时间戳**推断**，结果推错了人（docs/13 L17）。
#      不同提交身份能从根上解决：`git log --format='%an'` 直接出答案。
#   2. 越界修改（改了不属于自己的文件）以前只能靠人工看 diff。
#      这里把 docs/12 §1.2 的编制表变成**机械检查** —— 路径不在你的范围内直接拒绝。
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

# ---------------------------------------------------------------- 身份
$Identities = @{
    'lead' = @{ Name = '技术主管'; Email = 'lead@tokenagency.local'; Desc = '骨架/契约/集成/打包' }
    'E1'   = @{ Name = 'E1 领域官'; Email = 'e1@tokenagency.local'; Desc = '领域与配置' }
    'E2'   = @{ Name = 'E2 决策官'; Email = 'e2@tokenagency.local'; Desc = '决策与账本' }
    'E3'   = @{ Name = 'E3 数据官'; Email = 'e3@tokenagency.local'; Desc = '数据与密钥' }
    'E4'   = @{ Name = 'E4 网络官'; Email = 'e4@tokenagency.local'; Desc = '网络接入' }
    'E5'   = @{ Name = 'E5 网关官'; Email = 'e5@tokenagency.local'; Desc = '网关（关键路径）' }
    'mech' = @{ Name = '机动岗'; Email = 'mech@tokenagency.local'; Desc = 'spike/补强/支援' }
}

# ---------------------------------------------------------------- 文件范围（源自 docs/12 §1.2）
$Scopes = @{
    'lead' = @('*')
    'E1'   = @('crates/ta-core/src/domain', 'crates/ta-core/src/config', 'config.example.yaml')
    'E2'   = @(
        'crates/ta-core/src/routing', 'crates/ta-core/src/sticky',
        'crates/ta-core/src/quota', 'crates/ta-core/src/pricing',
        'crates/ta-core/src/metering'
    )
    'E3'   = @('crates/ta-store', 'crates/ta-secrets')
    'E4'   = @('crates/ta-kernel', 'crates/ta-subscription')
    'E5'   = @('crates/ta-gateway', 'src-tauri', 'src/pages', 'src/app', 'src/stores', 'src/shared')
    'mech' = @('*')
}

function Test-InScope($path, $scopes) {
    if ($scopes -contains '*') { return $true }
    $p = $path -replace '\\', '/'
    foreach ($s in $scopes) {
        if ($p -eq $s -or $p.StartsWith("$s/")) { return $true }
    }
    return $false
}

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
        Write-Host "  这是 docs/12 §1.2 的编制表。确实需要提交范围外的文件时：" -ForegroundColor Yellow
        Write-Host "    -Override ""一句话说明原因""" -ForegroundColor Yellow
        Write-Host "  （会写进提交 trailer，可审计）" -ForegroundColor DarkGray
        exit 2
    }
    Write-Host "⚠ 已用 -Override 承担越界提交：$Override" -ForegroundColor Yellow
    $outside | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
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

$maxTry = 6
for ($i = 1; $i -le $maxTry; $i++) {
    # 只 add 自己的路径 —— 不动别人已暂存的内容
    git add -- $Paths 2>&1 | Out-Null
    $out = git @gitArgs 2>&1
    if ($LASTEXITCODE -eq 0) {
        if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
        Write-Host ""
        git --no-pager log -1 --format='  %h  %an  %s'
        Write-Host "✔ 提交成功（Work-Package: $As）" -ForegroundColor Green
        exit 0
    }
    # index.lock 竞争：别的员工也在提交，等一会儿重试
    if (($out | Out-String) -match 'index\.lock') {
        Write-Host "  · 索引被占用，重试 $i/$maxTry ..." -ForegroundColor DarkGray
        Start-Sleep -Milliseconds (400 * $i)
        continue
    }
    Write-Host ""
    $out | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    Write-Host ""
    Write-Host "✘ 提交失败（多半是门禁未过）。修好后重跑本命令。" -ForegroundColor Red
    if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
    exit 1
}

Write-Host "✘ 索引持续被占用，稍后重试" -ForegroundColor Red
if ($tmpMsg) { Remove-Item $tmpMsg -Force -ErrorAction SilentlyContinue }
exit 1

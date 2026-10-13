# 用**软件自己的端点**访问 DeepSeek V4.1（模型 id `ds`）并输入 hello。
#
# 交付验收用：把客户端（Anthropic Messages 协议，跟 DSH 改好后的姿势一致）的请求打到
# `http://127.0.0.1:8787/v1/messages`，打印状态码 / 响应体 / 以及网关日志里那一行
# 「上游目标」（我们**实际**打给火山的地址）。
#
# 用法：
#   pwsh -File scripts/verify-endpoint-hello.ps1 -ApiKey <TA 端点密钥> [-Prompt hello]
#
# 退出码：0 = 上游真的回了 200；1 = 没通（并打印原因，通常是火山那边
#         `ModelNotOpen`（模型未开通）或 `AuthenticationError`（coding 套餐要用套餐自己的 key））。

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ApiKey,
    [string]$Prompt = "hello",
    [string]$BaseUrl = "http://127.0.0.1:8787",
    [string]$Model = "ds",
    [int]$MaxTokens = 32,
    # anthropic = `/v1/messages`（DSH 现在的姿势）；openai = `/v1/chat/completions`
    [ValidateSet("anthropic", "openai")][string]$Protocol = "anthropic"
)

$ErrorActionPreference = "Stop"

if (-not (Get-NetTCPConnection -LocalPort 8787 -State Listen -ErrorAction SilentlyContinue)) {
    Write-Host "[FAIL] 本机网关没在监听 127.0.0.1:8787 —— 先启动 TokenAgency。" -ForegroundColor Red
    exit 1
}

if ($Protocol -eq "anthropic") {
    $path = "/v1/messages"
    $body = @{
        model      = $Model
        max_tokens = $MaxTokens
        messages   = @(@{ role = "user"; content = $Prompt })
    } | ConvertTo-Json -Depth 6 -Compress
    $authHeaders = @("-H", "x-api-key: $ApiKey", "-H", "anthropic-version: 2023-06-01")
} else {
    $path = "/v1/chat/completions"
    $body = @{
        model      = $Model
        max_tokens = $MaxTokens
        messages   = @(@{ role = "user"; content = $Prompt })
    } | ConvertTo-Json -Depth 6 -Compress
    $authHeaders = @("-H", "Authorization: Bearer $ApiKey")
}

Write-Host "→ POST $BaseUrl$path  model=$Model  prompt=$Prompt  ($Protocol)" -ForegroundColor Cyan
$raw = curl.exe -s -i -X POST "$BaseUrl$path" @authHeaders `
    -H "Content-Type: application/json" `
    -d $body --max-time 90

$statusLine = ($raw | Select-String -Pattern '^HTTP/').Line
$status = [int](($statusLine -split ' ')[1])
$bodyText = ($raw | Select-Object -Last 1)
Write-Host "← $statusLine" -ForegroundColor Cyan
Write-Host "  响应体：$bodyText"

# 网关日志里的「上游目标」——这一行就是"我们到底打给了谁"
$log = Get-ChildItem "$env:APPDATA\TokenAgency\logs" -Filter 'tokenagency.log*' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($log) {
    $target = Select-String -Path $log.FullName -Pattern '上游目标' | Select-Object -Last 1
    if ($target) {
        $url = (($target.Line -split 'ingress: ')[1] -split ' channel_id=')[0]
        Write-Host "  上游目标：$url" -ForegroundColor DarkGray
    }
}

if ($status -eq 200) {
    Write-Host "[PASS] 软件端点已成功访问 $Model（输入 '$Prompt'）。" -ForegroundColor Green
    exit 0
}
Write-Host "[FAIL] 端点没通（HTTP $status）。常见两种原因（都是火山那边的原文）：" -ForegroundColor Red
Write-Host "       * ModelNotOpen            → 火山方舟控制台里这个模型没开通"
Write-Host "       * AuthenticationError     → coding 套餐要用**套餐自己的** API key（控制台 Coding 套餐页取）"
exit 1

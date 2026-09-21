# 把真实机场订阅脱敏成可入库的夹具（**可审计**：脚本入库，任何人可重跑核对）。
#
# 授权来源：第 11 轮，用户明确授权"读本机 clash 的 profiles 并自动脱敏"。
# 背景：[docs/06 §2.1 D2] 要求用**真实机场订阅**验证解析一致性，而 E4 无法从外部
# 获取样本（联网搜索额度用尽），且它正确地拒绝未经授权读用户私人数据。
#
# 边界：
#   * 只读**订阅内容**那一个文件。**绝不读 `profiles.yaml`** —— 那里面有订阅 URL 与 token。
#   * 结构必须保持真实（模式、分组嵌套、filter、规则都要能像吃真订阅一样吃它），
#     否则夹具就退化成"为通过测试而编的东西"（[13 L19] 同族问题）。
#
# ─────────────────────────────────────────────────────────────────────────────
# ⚠️ 第一版脚本在这里翻过车，记下来（这是本文件最重要的注释）：
#
#   第一版用**行首锚点**抓 `uuid:` / `server:`，而真实机场订阅是**flow 风格**：
#       - { name: 日本-优化, type: vmess, server: p4.cna.linuxlh.xin, uuid: 0eb5bc20-… }
#   于是 27 个真 uuid、全部 server 域名**一个都没被替换**，而脚本还自信地打印了
#   "自检：0 泄漏" —— 因为它的自检是"原文里的端点还出现吗"，而端点是**它自己抽出来的**
#   （没抽到 = 不存在 = "没有泄漏"）。**用同一套抽取既做脱敏又做验证，是循环论证。**
#
#   所以本版两条硬要求：
#     ① 抽取必须**格式无关**（不依赖 `key:` 是否在行首、是否 flow 风格）
#     ② 自检必须断言**输出自身的性质**（"每个 server 值都必须是合成的"），
#        而不是断言"原串不在" —— 后者只能证明抽取器工作，不能证明脱敏完整
# ─────────────────────────────────────────────────────────────────────────────
#
# 用法：
#   pwsh -File scripts/desensitize-subscription.ps1 -Source "<订阅文件>" -Out <输出路径>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Source,
    [Parameter(Mandatory = $true)][string]$Out
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Source)) { throw "找不到源文件：$Source" }
$raw = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $Source).Path)
Write-Host "源文件：$Source（$($raw.Length) 字符）" -ForegroundColor Cyan

$FAKE_DOMAIN = 'example-airport.invalid'
$CRED_PREFIX = 'ta-fake-credential-'
$PATH_PREFIX = '/ta-fake-path-'
# 这些字面值不是凭据，替换它们反而会让夹具失真
$NOT_SECRET = @('none', 'auto', 'true', 'false', '0', "''", '""', '', '""')

# ================================================================ 抽取（格式无关）

# 端点：任何位置的 `server|sni|servername|server-name: <值>`（flow 与 block 都覆盖）
function Get-EndpointTokens([string]$t) {
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($m in [regex]::Matches($t, '(?i)(?:^|[\s{,])(?:server|sni|servername|server-name|host)\s*:\s*([^,\s}]+)')) {
        $v = $m.Groups[1].Value.Trim().Trim('"').Trim("'")
        if ($v) { $out.Add($v) }
    }
    # 字面 IPv4（DNS、规则、节点都可能出现）
    foreach ($m in [regex]::Matches($t, '\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b')) { $out.Add($m.Value) }
    return @($out | Sort-Object -Unique)
}

# 凭据：任意位置的 `key: <值>`
$CRED_KEYS = 'uuid|password|psk|auth|token|username|private-key|public-key|pre-shared-key|fingerprint|servername|obfs-password'
function Get-CredentialTokens([string]$t) {
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($m in [regex]::Matches($t, "(?i)(?:^|[\s{,])(?:$CRED_KEYS)\s*:\s*([^,\s}]+)")) {
        $v = $m.Groups[1].Value.Trim().Trim('"').Trim("'")
        if ($v -and ($NOT_SECRET -notcontains $v)) { $out.Add($v) }
    }
    return @($out | Sort-Object -Unique)
}

$endpoints = Get-EndpointTokens $raw
$creds = Get-CredentialTokens $raw
Write-Host "抽取：端点 $($endpoints.Count) 个、凭据 $($creds.Count) 个（不含 `none`/`auto` 这类非密钥字面值）" -ForegroundColor Cyan

# ================================================================ 映射（确定性）
$hostMap = @{}
$ipIdx = 0
$domIdx = 0
foreach ($e in $endpoints) {
    if ($e -match '^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$') {
        # RFC 5737 文档保留网段
        $hostMap[$e] = if ($ipIdx -lt 254) { "198.51.100.$($ipIdx + 1)" } else { "203.0.113.$($ipIdx - 253)" }
        $ipIdx++
    } elseif ($e -match '[A-Za-z]') {
        $domIdx++
        $hostMap[$e] = "node-$domIdx.$FAKE_DOMAIN"
    }
}

$text = $raw
# 长的先替换，避免子串先被吃掉
foreach ($e in ($endpoints | Sort-Object -Property Length -Descending)) {
    if ($hostMap.ContainsKey($e)) { $text = $text.Replace($e, $hostMap[$e]) }
}

# 凭据：按"键: 值"整体替换，保留键与结构
$credMap = @{}
$ci = 0
foreach ($c in $creds) {
    $ci++
    # uuid 要仍是合法 UUID 形态（解析器会校验格式）
    $credMap[$c] = if ($c -match '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$') {
        "00000000-0000-4000-8000-{0:D12}" -f $ci
    } else {
        "$CRED_PREFIX$ci"
    }
}
foreach ($c in ($creds | Sort-Object -Property Length -Descending)) {
    $text = $text.Replace($c, $credMap[$c])
}

# ws/grpc 的 path 常带 token
$pi = 0
$text = [regex]::Replace($text, '(?i)((?:^|[\s{,])(?:path)\s*:\s*)([^,\s}]+)', {
        param($m)
        $script:pi++
        "$($m.Groups[1].Value)$PATH_PREFIX$($script:pi)"
    })

# ================================================================ 自检：断言**输出自身的性质**
$fail = New-Object System.Collections.Generic.List[string]

# ① 每个端点值都必须是合成的（这一条不依赖"原串是否出现"）
foreach ($e in (Get-EndpointTokens $text)) {
    if ($e -match "^node-\d+\.$([regex]::Escape($FAKE_DOMAIN))$") { continue }
    if ($e -match '^198\.51\.100\.\d+$' -or $e -match '^203\.0\.113\.\d+$') { continue }
    $fail.Add("端点仍是真实值：$e")
}

# ② 每个凭据值都必须是合成的或明确的非密钥字面值
foreach ($c in (Get-CredentialTokens $text)) {
    if ($c -like "$CRED_PREFIX*") { continue }
    if ($c -match '^00000000-0000-4000-8000-\d{12}$') { continue }
    if ($NOT_SECRET -contains $c) { continue }
    $fail.Add("凭据仍是真实值：$c")
}

# ③ 结构没被破坏：`uuid` / `server` 的出现次数必须与原文一致
foreach ($k in 'uuid', 'server', 'password') {
    $before = ([regex]::Matches($raw, "(?i)\b$k\b")).Count
    $after = ([regex]::Matches($text, "(?i)\b$k\b")).Count
    if ($before -ne $after) { $fail.Add("结构被改坏：`$k` 出现次数 $before → $after") }
}

# ④ 纵深：原始端点串不应残留
foreach ($e in $endpoints) { if ($text.Contains($e)) { $fail.Add("残留原始端点：$e") } }
foreach ($c in $creds) { if ($text.Contains($c)) { $fail.Add("残留原始凭据：$c") } }

# ⑤ 密钥形态扫描
$susp = [regex]::Matches($text, '(sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----)')
if ($susp.Count -gt 0) { $fail.Add("仍出现 $($susp.Count) 处密钥形态") }

if ($fail.Count -gt 0) {
    Write-Host "`n✘ 自检失败（$($fail.Count) 项），**不产出文件**：" -ForegroundColor Red
    $fail | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" }
    exit 1
}

$outDir = Split-Path -Parent $Out
if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
[System.IO.File]::WriteAllText($Out, $text, (New-Object System.Text.UTF8Encoding($false)))

$nProxies = ([regex]::Matches($text, '(?i)\btype\s*:\s*(vmess|vless|trojan|ss|ssr|hysteria2?|tuic)')).Count
$nGroups = ([regex]::Matches($text, '(?i)\bproxy-groups\b')).Count
Write-Host ""
Write-Host "✔ 已写出 $Out（$($text.Length) 字符）" -ForegroundColor Green
Write-Host "  替换端点 $($endpoints.Count) 个（域名 $domIdx / IP $ipIdx）、凭据 $($creds.Count) 个、path $pi 处"
Write-Host "  节点 type: 出现 $nProxies 次；proxy-groups 段 $nGroups 个"
Write-Host "  自检 5 项全过：端点全合成 / 凭据全合成 / 结构次数一致 / 无残留 / 无密钥形态"

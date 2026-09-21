# 重新生成 config.example.yaml
#
# 为什么要有这个脚本：示例配置的**唯一真源是 Rust 类型**。
# W0 那版是我手写的，结果被员工实测抓出三处硬错误（读不通）+ 一批静默落进 extra 的失效字段。
# 手写必然漂移，所以改成生成。
#
# 语义解释（字段是什么意思）不放在 YAML 里，放 docs/02-config-and-data-model.md
# —— 机器要解析的东西让它保持机器友好，人要读的说明写在文档里。
#
# 用法: pwsh -File scripts/gen-example-config.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$out = Join-Path $root "config.example.yaml"

$header = @'
# ============================================================================
# TokenAgency 配置示例  —— 本文件由 scripts/gen-example-config.ps1 生成，请勿手改
# ============================================================================
#
# 唯一真源是 Rust 类型（crates/ta-core/src/config/defaults.rs）。
# 字段含义与选型理由见 docs/02-config-and-data-model.md，不要在本文件里写长注释
# —— 手写注释必然与类型漂移，这正是本文件改成生成物的原因。
#
# 用法：
#   默认模式（SQLite 为真源）：ta import config.example.yaml
#   文件监听模式：设置里开启后，以复制出来的 YAML 为真源，改动热重载
#
# 密钥一律用 ${secret:name} / ${env:VAR}，不要写明文
#   （git pre-commit 会拦截疑似明文 key，见 .githooks/pre-commit）
#
# 重新生成：
#   pwsh -File scripts/gen-example-config.ps1
#
# 防漂移：crates/ta-core/tests/example_config_guard.rs 会断言本文件能被当前类型读通，
#         且不含任何落进 extra 的失效字段。改结构体忘了重新生成 → CI 直接红。
# ============================================================================

'@

Write-Host "生成中 ..." -ForegroundColor Cyan
$body = & cargo run -q -p ta-core --example dump_default_config
if ($LASTEXITCODE -ne 0) { throw "dump_default_config 执行失败" }

$text = $header + ($body -join "`n") + "`n"
# 统一 LF，避免 Windows 下 CRLF 噪声（.gitattributes 也强制 text=auto eol=lf）
$text = $text -replace "`r`n", "`n"
[System.IO.File]::WriteAllText($out, $text, (New-Object System.Text.UTF8Encoding($false)))

$lines = (Get-Content $out).Count
Write-Host "已生成 $out（$lines 行）" -ForegroundColor Green
Write-Host ""
Write-Host "验证：" -ForegroundColor Cyan
& cargo test -q -p ta-core --test example_config_guard 2>&1 | Select-Object -Last 8

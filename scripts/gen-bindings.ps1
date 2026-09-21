# 由 Rust 侧导出 TypeScript 类型绑定。
#
# 机制：`src-tauri/src/lib.rs` 在 `TA_EXPORT_BINDINGS=1` 时**只导出绑定然后退出**
#       （不建窗口、不连数据库）—— 见 lib.rs 的 `run()` 开头那个分支。
#       本脚本负责设置环境变量并**真正执行**那个分支。
#
# ⚠️ 为什么是 `cargo run` 而**不是** `cargo build`：
#     `cargo build` 只**编译**，不执行任何代码，所以那个导出分支永远跑不到 ——
#     这正是本脚本第 10 轮之前一直"跑完却写不出文件"的原因。
#
# ⚠️ 为什么不是 `cargo test`：
#     `tokenagency` 是 Tauri 的 **cdylib**，它的 lib-test 二进制**无法作为测试 harness 链接**。
#     所以只能走"运行应用自身的导出路径"，而不是找一个 `#[test]` 去跑。
#
# ============================================================================
# 用法
#
#   pwsh -File scripts/gen-bindings.ps1
#       默认**只读**：生成到 `target/bindings/bindings.generated.ts`，并打印与手写契约
#       的命令名差集。**不碰任何被 git 跟踪的文件**，可以随时反复跑。
#
#   pwsh -File scripts/gen-bindings.ps1 -Out <path>
#       除默认产物外，再把生成物复制到你指定的路径（仍受下面的护栏约束）。
#
#   pwsh -File scripts/gen-bindings.ps1 -Overwrite
#       生成物**取代** `src/shared/bindings.ts`（手写 105 条契约的落点）。
#       只有在 T1 命令面补齐之后才该这么做；缺命令时仍然**拒绝覆盖**。
#
#   pwsh -File scripts/gen-bindings.ps1 -Overwrite -Force
#       明知缺口也要覆盖（逃生门；会响亮打印缺了哪些）。
#
# ★ 第 11 轮改（真实事故）：这个脚本以前**无条件覆盖** `src/shared/bindings.ts` ——
#   它在下一段就打印"直接覆盖会让前端 import 失败"，却**先覆盖了再说**。
#   也就是说"我想看看还差哪些命令"这个**纯只读**的问题**会写坏前端唯一的 105 条
#   类型契约**（E3 实际踩到，靠 `bindings.ts` 已在 `ae7911d` 入库才恢复）。
#   根因不是"忘了备份"，而是**读模式与写模式共用一个出口**。现在：生成物永远先落到
#   `target/`（不在 tsconfig 的 include 里，不进 git，不被 `tsc` 检查），护栏跑完才
#   决定要不要复制到被跟踪的位置。
# ============================================================================

[CmdletBinding()]
param(
    # 额外复制一份生成物到这个路径（可选）
    [string]$Out,
    # 用生成物取代 src/shared/bindings.ts
    [switch]$Overwrite,
    # 有缺口也强行覆盖（仅与 -Overwrite 同用）
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$contractPath = Join-Path $root "src/shared/bindings.ts"

# 生成物永远先落这里：不在 tsconfig include 内、不在 git 内，所以"跑一次看看"是纯只读的。
$genPath = Join-Path $root "target/bindings/bindings.generated.ts"
New-Item -ItemType Directory -Force -Path (Split-Path $genPath) | Out-Null

# 护栏通过之后才允许复制到的最终位置（可能为 $null = 不复制）。
$finalPath = $null
if ($Overwrite) { $finalPath = $contractPath }
elseif ($Out)   { $finalPath = $Out }

# ★ 必须先删掉输出文件，否则下面的 `Test-Path` 会把**上一次的旧文件**当成
#   "本次生成成功" —— 导出其实失败了，脚本却报绿。（第 10 轮真的踩到了：导出报
#   `Framework error`，脚本仍然打印"已生成 35561 bytes"，而那正是旧文件。）
Remove-Item $genPath -Force -ErrorAction SilentlyContinue

Write-Host "运行 src-tauri 的绑定导出分支（cargo run，带 TA_EXPORT_BINDINGS=1）..." -ForegroundColor Cyan
$env:TA_EXPORT_BINDINGS = "1"
$env:TA_EXPORT_BINDINGS_PATH = $genPath
$runExit = 0
try {
    cargo run -p tokenagency --quiet
    $runExit = $LASTEXITCODE
}
finally {
    Remove-Item Env:\TA_EXPORT_BINDINGS -ErrorAction SilentlyContinue
    Remove-Item Env:\TA_EXPORT_BINDINGS_PATH -ErrorAction SilentlyContinue
}

if ($runExit -ne 0) {
    Write-Host "cargo run 退出码 $runExit —— 导出分支可能失败，见上面的 stderr。" -ForegroundColor Red
}

if (-not (Test-Path $genPath)) {
    Write-Host "未生成 $genPath。" -ForegroundColor Red
    Write-Host "排查顺序：① lib.rs 的 TA_EXPORT_BINDINGS 分支是否还在；" -ForegroundColor DarkGray
    Write-Host "          ② commands::export_bindings 是否返回 Err（跑 cargo run 看 stderr）；" -ForegroundColor DarkGray
    Write-Host "          ③ 是否所有命令都已注册（specta 只导出已注册的命令）。" -ForegroundColor DarkGray
    exit 1
}
if ((Get-Item $genPath).Length -le 0) {
    Write-Host "生成的 $genPath 是空文件 —— 视为失败。" -ForegroundColor Red
    exit 1
}

$genLen = (Get-Item $genPath).Length
Write-Host "已生成 $genPath ($genLen bytes)" -ForegroundColor Green

# ---------------------------------------------------------------- 契约覆盖护栏
#
# 背景：绑定通道打通之前，`bindings.ts` 里是**手写的 95 条（现 105 条）命令契约**，
# 而 T1 只注册了一部分命令。生成物会**覆盖**它 —— 前端 `src/shared/api/**` 依赖
# 旧契约里的类型名，覆盖后 `npm run build` 会报一堆 TS2305（实测过）。
#
# 这里做一次**名字差集**：拿生成物的命令名去比对手写参考里的命令名，少的那批
# **逐条列出来**（第 11 轮之前只打前 20 条并说"还有 55 条"，等于把清单藏起来）。
# 这不只是护栏 —— 它同时就是 T1b 要补的清单。
#
# 注意别用"文件大小"或"某个函数的出现次数"当指标：两者的 TS 形态完全不同
# （生成物是 `__TAURI_INVOKE("name")`，手写是 `CommandMap` 的键），拿同一个模式去
# 数两边只会得到 0（第 10 轮真踩到过）。
$missing = @()
$refPath = Join-Path $root 'docs/contracts/bindings-handwritten-reference.ts.txt'
if (Test-Path $refPath) {
    # ⚠️ 不要在 `"` 后面要求 `)`：带参数的命令长这样
    #    `__TAURI_INVOKE("app_open_dir", { kind })`
    # 加上 `\)` 只会匹配到**无参数**的命令，把 31 数成 14（第 10 轮真踩到过）。
    $genNames = [regex]::Matches(
        [System.IO.File]::ReadAllText($genPath), '__TAURI_INVOKE\("([a-z][a-z0-9_]*)"'
    ) | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique

    $refText = [System.IO.File]::ReadAllText($refPath)
    $refBlock = [regex]::Match($refText, 'export type CommandMap = \{(.*?)\n\};', 'Singleline')
    $refNames = @()
    if ($refBlock.Success) {
        $refNames = [regex]::Matches(
            $refBlock.Groups[1].Value, '(?m)^\s{2}([a-z][a-z0-9_]*)\s*:'
        ) | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
    }

    if ($refNames.Count -gt 0) {
        $missing = @($refNames | Where-Object { $genNames -notcontains $_ })
        Write-Host ""
        Write-Host "命令数：生成 $($genNames.Count) / 手写契约 $($refNames.Count)" `
            -ForegroundColor $(if ($missing.Count -gt 0) { 'Yellow' } else { 'Green' })
        if ($missing.Count -gt 0) {
            $diffPath = Join-Path $root 'target/bindings/missing-commands.txt'
            New-Item -ItemType Directory -Force -Path (Split-Path $diffPath) | Out-Null
            $missing | Set-Content -LiteralPath $diffPath -Encoding utf8
            Write-Host "⚠ 生成物缺少 $($missing.Count) 条命令 —— 现在覆盖 $contractPath 会让前端 import 失败。" -ForegroundColor Yellow
            Write-Host "  完整清单（$($missing.Count) 条，同时写入 $diffPath）：" -ForegroundColor Yellow
            foreach ($n in $missing) { Write-Host "    - $n" -ForegroundColor DarkGray }
            Write-Host ""
            Write-Host "  处置：T1 补齐命令面**之后**再加 -Overwrite。" -ForegroundColor DarkGray
            Write-Host "  手写契约是**被 git 跟踪**的（ae7911d 起入库）：误覆盖后用" -ForegroundColor DarkGray
            Write-Host "      git restore -- $contractPath" -ForegroundColor DarkGray
            Write-Host "  即可完整恢复 —— 这正是它必须入库的原因。" -ForegroundColor DarkGray
        }
    }
    else {
        Write-Host "! 参考契约里没解析出 CommandMap（$refPath）—— 差集护栏本次未生效。" -ForegroundColor Yellow
    }
}
else {
    Write-Host "! 找不到参考契约 $refPath —— 差集护栏本次未生效。" -ForegroundColor Yellow
}

# ---------------------------------------------------------------- emoji 检查
#
# 为什么要有这一条（E1 的评审员挖出来的）：生成的 TS 会把 Rust 的 doc 注释**原样**
# 带出来（`specta-typescript` 直写 field/variant docs）。所以只要 `crates/**` 的注释里
# 有我们自己的"设计标记"（`⚠️`/`★` 之类），**重新生成就会把它们带回 `src/**`**，
# 而手写的 `// ta-allow-emoji` 标记会随整文件重写一起消失 → emoji 守卫变红 →
# 最省事的"修法"就是加白名单整体绕过。**这条检查把那个陷阱变成一次响亮的失败。**
#
# 分级（避免误伤合法内容）：
#   * **设计标记**（我们用符号当 UI 语义的那几个）→ **失败**：它们本就不该出现在我们的设计里
#   * 其它 emoji（例如 doc 里演示"用户数据可以含 🇯🇵"）→ **只警告并列出**，因为
#     "我们永不自己产生 emoji" 不等于"禁止 emoji"（docs/15 §2.2：用户数据原样呈现）
#
# ⚠️ 第 11 轮修：这里的正则原来写成 `[\u{1F000}-\u{1FAFF}]` —— **.NET 正则不支持
#    `\u{...}` 这种写法**（那是 PCRE/Java 语法），于是每次生成都直接抛
#    "Insufficient or invalid hexadecimal digits"，脚本以退出码 1 死在守卫这一步，
#    而"缺 75 条命令"也就只打出了前 20 条就没了（E3 实测）。星平面字符必须写成
#    **代理对** `[\uD800-\uDBFF][\uDC00-\uDFFF]`。
$designMarkers = '[⚠★✔✘✖▲▼●○]'
$astralOrSymbol = '[\u2190-\u2BFF\uFE0F]|[\uD800-\uDBFF][\uDC00-\uDFFF]'
# 排版符号（箭头 ← →、制表边框 ─│┌）在 Rust doc 里很常见，也不属于"emoji"：
# 单独放行，免得每次生成都刷一屏噪声警告。
$typography = '[\u2190-\u21FF\u2500-\u257F]'

$lines = Get-Content -LiteralPath $genPath
$badMarkers = @()
$otherEmoji = @()
for ($i = 0; $i -lt $lines.Count; $i++) {
    $line = $lines[$i]
    if ($line -notmatch $astralOrSymbol) { continue }
    if ($line -match $designMarkers) {
        $badMarkers += "$($i + 1): $($line.Trim())"
    }
    elseif (($line -replace $typography, '') -match $astralOrSymbol) {
        $otherEmoji += "$($i + 1): $($line.Trim())"
    }
}
if ($otherEmoji.Count -gt 0) {
    Write-Host "! 生成物里有 $($otherEmoji.Count) 处 emoji（多为 doc 里演示的用户数据，按 [15 §2.2] 可保留）：" -ForegroundColor Yellow
    $otherEmoji | Select-Object -First 6 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
}
if ($badMarkers.Count -gt 0) {
    Write-Host ""
    Write-Host "✘ 生成物里有 $($badMarkers.Count) 处**我们自己的设计标记**（`⚠ ★ ✔ ✘ ▲ ▼` 等）" -ForegroundColor Red
    $badMarkers | Select-Object -First 10 | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    Write-Host ""
    Write-Host "  根因几乎总在 `crates/**` 的 doc 注释里（这里只是被原样带出来）。" -ForegroundColor Yellow
    Write-Host "  正确修法是**改源头的注释文字**（[docs/15] 要求我们自己不产生 emoji），" -ForegroundColor Yellow
    Write-Host "  **不要**把 `// ta-allow-emoji` 加回来 —— 那会让 emoji 守卫整体失效。" -ForegroundColor Yellow
    Write-Host "  生成物仍保留在 $genPath（未被复制到任何被跟踪的位置）。" -ForegroundColor DarkGray
    exit 1
}

# ---------------------------------------------------------------- 落地（唯一写路径）
#
# 走到这里 = 护栏都过了。**只有这一步会碰被 git 跟踪的文件**，而且必须先备份。
if (-not $finalPath) {
    Write-Host ""
    Write-Host "只读模式：未改动任何被跟踪的文件。" -ForegroundColor Green
    if ($missing.Count -gt 0) {
        Write-Host "（命令面仍缺 $($missing.Count) 条；清单见 target/bindings/missing-commands.txt）" -ForegroundColor DarkGray
    }
    exit 0
}

# 缺命令却要求覆盖 → 拒绝，除非显式 -Force。
if (($missing.Count -gt 0) -and (-not $Force)) {
    Write-Host ""
    Write-Host "拒绝覆盖 $finalPath：生成物比手写契约少 $($missing.Count) 条命令。" -ForegroundColor Red
    Write-Host "  理由：覆盖后前端 `src/shared/api/**` 的 import 会集体失败（TS2305）。" -ForegroundColor Yellow
    Write-Host "  要强行覆盖请显式加 -Force（并准备好承担前端构建失败）。" -ForegroundColor Yellow
    exit 1
}

if (Test-Path $finalPath) {
    # 备份名带时间戳：以前固定叫 `bindings.ts.bak`，**每次运行都被上上次的结果盖掉**，
    # 而且它在 .gitignore 里 —— 等于"自动备份"这个承诺是假的。现在不覆盖任何已有备份。
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $bakDir = Join-Path $root 'target/bindings/backups'
    New-Item -ItemType Directory -Force -Path $bakDir | Out-Null
    $bak = Join-Path $bakDir "bindings.$stamp.ts"
    Copy-Item $finalPath $bak -Force
    Write-Host "已备份原文件 → $bak" -ForegroundColor DarkGray
    Write-Host "  还原：Copy-Item '$bak' '$finalPath' -Force" -ForegroundColor DarkGray
}

Copy-Item $genPath $finalPath -Force
Write-Host "已落地 $finalPath（$((Get-Item $finalPath).Length) bytes）" -ForegroundColor Green
exit 0

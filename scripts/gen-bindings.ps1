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
# 用法: pwsh -File scripts/gen-bindings.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$out = Join-Path $root "src/shared/bindings.ts"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null

# 覆盖前先备份：绑定文件是 .gitignore 忽略的构建产物，**没有版本控制保护**。
# 生成失败或生成出一份不完整的契约时，至少还能拿回上一版。
$bak = "$out.bak"
if (Test-Path $out) { Copy-Item $out $bak -Force; Write-Host "已备份上一版 → $bak" -ForegroundColor DarkGray }

# ★ 必须先删掉输出文件，否则下面的 `Test-Path $out` 会把**上一次的旧文件**当成
#   "本次生成成功" —— 导出其实失败了，脚本却报绿。（第 10 轮真的踩到了：导出报
#   `Framework error`，脚本仍然打印"已生成 35561 bytes"，而那正是旧文件。）
Remove-Item $out -Force -ErrorAction SilentlyContinue

Write-Host "运行 src-tauri 的绑定导出分支（cargo run，带 TA_EXPORT_BINDINGS=1）..." -ForegroundColor Cyan
$env:TA_EXPORT_BINDINGS = "1"
$env:TA_EXPORT_BINDINGS_PATH = $out
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

if (Test-Path $out) {
    $len = (Get-Item $out).Length
    if ($len -gt 0) {
        Write-Host "已生成 $out ($len bytes)" -ForegroundColor Green

        # ★ 契约覆盖护栏（第 10 轮新增）
        #
        # 背景：绑定通道打通之前，`$out` 里是**手写的 95 条命令契约**（F1 的占位），
        # 而 T1 只注册了一部分命令。生成物会**直接覆盖**它 —— 前端 `src/shared/api/**`
        # 依赖旧契约里的类型名，覆盖后 `npm run build` 会报一堆 TS2305（实测过）。
        #
        # 这里做一次**名字差集**：拿生成物的命令名去比对手写参考里的命令名，
        # 少的那批**逐条列出来**。这不只是护栏 —— 它同时就是 T1b 要补的清单。
        #
        # 注意别用"文件大小"或"某个函数的出现次数"当指标：两者的 TS 形态完全不同
        # （生成物是 `__TAURI_INVOKE("name")`，手写是 `CommandMap` 的键），
        # 拿同一个模式去数两边只会得到 0（第 10 轮真踩到过）。
        $refPath = Join-Path $root 'docs/contracts/bindings-handwritten-reference.ts.txt'
        if (Test-Path $refPath) {
            # ⚠️ 不要在 `"` 后面要求 `)`：带参数的命令长这样
            #    `__TAURI_INVOKE("app_open_dir", { kind })`
            # 加上 `\)` 只会匹配到**无参数**的命令，把 31 数成 14（第 10 轮真踩到过）。
            $genNames = [regex]::Matches(
                [System.IO.File]::ReadAllText($out), '__TAURI_INVOKE\("([a-z][a-z0-9_]*)"'
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
                $missing = $refNames | Where-Object { $genNames -notcontains $_ }
                Write-Host ""
                Write-Host "命令数：生成 $($genNames.Count) / 手写契约 $($refNames.Count)" -ForegroundColor $(if ($missing.Count -gt 0) { 'Yellow' } else { 'Green' })
                if ($missing.Count -gt 0) {
                    Write-Host "⚠ 生成物缺少 $($missing.Count) 条命令 —— 直接覆盖会让前端 import 失败：" -ForegroundColor Yellow
                    $shown = $missing | Select-Object -First 20
                    Write-Host ("    " + ($shown -join '、')) -ForegroundColor Yellow
                    if ($missing.Count -gt $shown.Count) {
                        Write-Host "    …… 还有 $($missing.Count - $shown.Count) 条" -ForegroundColor DarkGray
                    }
                    Write-Host ""
                    Write-Host "  处置：T1 补齐命令面**之后**再让生成物覆盖 $out。" -ForegroundColor DarkGray
                    Write-Host "  上一版已备份在 $bak —— 现在要还原的话：Copy-Item $bak $out -Force" -ForegroundColor DarkGray
                    Write-Host "  完整差集清单见 docs/contracts/bindings-handwritten-reference.ts.txt。" -ForegroundColor DarkGray
                }
            }
        }
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
    if (Test-Path $out) {
        $lines = Get-Content -LiteralPath $out
        $marker = '[⚠★✔✘✖▲▼●○]'
        $anyEmoji = '[\u2190-\u2BFF\uFE0F\u{1F000}-\u{1FAFF}\u{1F1E6}-\u{1F1FF}]'
        $badMarkers = @()
        $otherEmoji = @()
        for ($i = 0; $i -lt $lines.Count; $i++) {
            if ($lines[$i] -notmatch $anyEmoji) { continue }
            if ($lines[$i] -match $marker) { $badMarkers += "$($i + 1): $($lines[$i].Trim())" }
            else { $otherEmoji += "$($i + 1): $($lines[$i].Trim())" }
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
            Write-Host "  （上一版已备份在 $bak）" -ForegroundColor DarkGray
            exit 1
        }
    }

    else {
        Write-Host "生成的 $out 是空文件 —— 视为失败。" -ForegroundColor Red
        exit 1
    }
}
else {
    Write-Host "未生成 $out。" -ForegroundColor Red
    Write-Host "排查顺序：① lib.rs 的 TA_EXPORT_BINDINGS 分支是否还在；" -ForegroundColor DarkGray
    Write-Host "          ② commands::export_bindings 是否返回 Err（跑 cargo run 看 stderr）；" -ForegroundColor DarkGray
    Write-Host "          ③ 是否所有命令都已注册（specta 只导出已注册的命令）。" -ForegroundColor DarkGray
    exit 1
}

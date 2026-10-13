# 贡献指南

先读 [AGENTS.md](AGENTS.md) —— 它是这个仓库的**项目地图**（架构、模块边界、命令表、
领域词汇表、入口表、禁区），而且是**被守卫测试拿代码核对过的**，比一般项目的
AGENTS/README 更可信。本文件只讲"怎么改、怎么验"。

## 1. 环境

* Windows（本项目目前只构建/验证 Windows）
* Rust（stable）、Node.js、**PowerShell 7（`pwsh`，不是 Windows PowerShell 5.1）**
* WebView2 运行时（Tauri 需要）
* Git

首次：

```powershell
pwsh -File scripts/install-hooks.ps1          # 把 core.hooksPath 指向 .githooks
pwsh -File scripts/fetch-mihomo.ps1           # 拉随包内核（按 tag+sha256，不入库）
npm install
```

## 2. ⚠️ 第一号坑：先把 TEMP 指进仓库

**这是新贡献者最容易踩、而且症状最误导的一个**（我自己就因此误报过一次"门禁全红"）：

```powershell
$env:TEMP = "$PWD\.ta-tmp\tmp"; $env:TMP = $env:TEMP
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
```

**为什么必须**：不少测试与工具会在临时目录下建目录。若 `TEMP` 指向系统临时目录，
在受限/沙箱环境里会得到

```
创建测试临时目录失败: Os { code: 5, kind: PermissionDenied, message: "拒绝访问。" }
```

**它不是代码坏了，是环境不对**。表现是 `cargo test --workspace` 里一片 target 全红，
而单独跑某一条却是绿的 —— 见到这个组合，先查 TEMP，别急着改代码。

## 3. 门禁（跑什么、什么时候跑）

钩子已由 `install-hooks.ps1` 装好：

| 钩子 | 跑什么 |
| --- | --- |
| `pre-commit` | 明文密钥分级 / 大文件 / `rustfmt` / `clippy --no-deps` / `tsc --noEmit` |
| `pre-push` | `cargo test --workspace --no-fail-fast` + `npm run build` |

手动跑：

```powershell
pwsh -File scripts/gate.ps1                    # 提交门禁（只查本次改动集）
cargo test --workspace --no-fail-fast          # 全量
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
npx vitest run ; npx tsc --noEmit
```

**不要用 `--no-verify` 绕过。** 门禁红了就是红。如果某条守卫挡错了你，
**改守卫的判据并把理由写进注释**，而不是绕过它 —— 这个仓库的守卫大多带"只许收缩"的棘轮，
判据错了会误伤活代码，所以改判据也要给出证据。

**改哪一块就跑哪一块**：只有动到配置 / 绑定 / 日志字段 / 示例配置时才跑跨切面守卫；
只有要交真机产物时才 `build.ps1` → `install-local.ps1` → e2e。

## 4. 代码约定

1. **注释用中文，写"为什么"**，不是"这行做什么"。
2. **不许假绿**：改不动就说改不动。`SKIP` 必须写清"环境给不了什么条件"，
   而且要**先怀疑自己**（历史上出现过 SKIP 的理由是假的）。
3. **静默失效 = 最严重的缺陷**。这个仓库把"功能看起来在、其实没数据"视为头号问题
   （`meter: None`、少一句 `with_trace_store`、日志 mode 没同步，都真实发生过）。
   写代码时优先保证"**要么工作，要么响亮地失败**"。
4. **前端文案**：版面文案 ≤36 字、一屏一个主操作、表格 ≤6 列、空态 1 行 ——
   有守卫（`uiMinimalism.guard`）；实现细节（函数名 / `后端` / `字段` / 文档编号 `§`）
   **不许出现在界面上**。
5. **测试读 i18n 真源**（`zhCN["key"]`），不要把中文句子抄进断言。
6. **`data-testid` 是 e2e 契约**：删之前先 `rg` 一遍 `scripts/e2e/smoke.mjs`。
7. **`ta-core` 不出现 IO**；**转发路径必须走 `GatewayDeps.dialer`**（见 AGENTS.md §2 的边界铁律）。

## 5. 贡献者授权（**请在提交 PR 前读这一段**）

本项目要求所有提交带 **DCO 签名**（Developer Certificate of Origin 1.1）。提交时：

```
git commit -s -m "fix: ..."
```

`-s` 会加一行 `Signed-off-by: 你的名字 <你的邮箱>`，表示你**有权按本项目的许可证
（GPL-3.0）提交这份代码**。DCO 全文见 <https://developercertificate.org/>。

**为什么是 DCO 而不是 CLA —— 以及你要知道的那条边界：**

DCO 只声明"我有权提交"，**不授予维护者额外的再许可权**。因此：

* 只要 PR 按 DCO 合并，本项目就**只能一直以 GPL-3.0（或其它 GPL 兼容许可）分发**；
* 如果维护者将来想要**闭源 / 商业双授权**，那就必须改用 **CLA**，
  而**这一步只有在"还没有合并过任何外部贡献"时才来得及**。

**当前状态：尚未接受任何外部贡献，所以这个选择仍然是开放的。**
维护者若打算保留商业授权余地，应在处理第一个 PR **之前**把 DCO 换成 CLA。
（DCO → CLA 是能走通的顺序；已经按 DCO 合并过之后，再想回头收 CLA 就晚了。）

## 6. PR 应该长什么样

* **小而聚焦**：一个 PR 一件事。不要把无关文件一起重排 —— 那会让 diff 无法审查。
* **带上验证证据**：跑了什么、结果是什么；红了就写红，不要只说"应该没问题"。
* **新行为要新测试**，而且**测试要能在错误时变红**（写一个只能通过的测试没有意义）。
* **改了地图就更新地图**：新增入口 / 词汇 → 更新 `AGENTS.md`；
  模块结构变了 → 更新该模块的 `README.md`。`repoMap.guard` 会核对路径与计数。
* **不要提交**：`dist/`、`target*/`、`src-tauri/resources/mihomo/*`（由脚本拉取）、
  任何 `*.sqlite`、任何密钥。`pre-commit` 会拦，但别试探它。

## 7. 报告问题

* 普通 bug / 功能建议：开 issue，写清版本与复现步骤。
* **安全问题**：**不要开公开 issue**，见 [SECURITY.md](SECURITY.md)。

## 8. 许可证

贡献即按 **GPL-3.0** 授权（见 [LICENSE](LICENSE)）。
随包第三方组件的许可与署名要求见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

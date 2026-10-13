# scripts — 构建 / 门禁 / 审计 / 协作 / 诊断脚本的唯一索引

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

读者是 AI 编码代理：**按用途找脚本，按"什么时候跑"决定要不要跑**。
真机 e2e 那一套的用法与环境坑在 `e2e/README.md`，本文件只给入口，不重复它的内容。

调用约定：PowerShell 用 `pwsh -File scripts/<脚本>`，Node 用 `node scripts/<脚本>`，Python 用 `python scripts/<脚本>`。
退出码 0 = 通过、非 0 = 有结论：诊断类脚本的"退出码 1"常常就是**它的答案**，不是脚本坏了。

## 入口文件

### 构建 / 出包
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `build.ps1` | 生产构建：前端构建 -> Rust 门禁 -> Tauri NSIS 安装包（`-SkipFrontend` / `-NoKernel` / `-WithBindings`） | 要交**安装包**时（慢） |
| `build-fast.ps1` | 快速构建（debug 档，`-Release` 换 release 档），并把内置内核与 GeoIP 铺到产物旁边 | 迭代，或**要喂 e2e** 时 |
| `install-local.ps1` | 把已构建的产物铺进当前用户安装目录（`-Uninstall` 反向删除，数据目录不动） | NSIS 被 SmartScreen / 杀软 / 文件占用挡住时 |
| `dev.ps1` | 先起 Vite，再起 `cargo tauri dev` | 改前端 / 桌面层、要热重载时 |
| `make-icon.mjs` | 生成 1024x1024 应用图标（Node 内置 zlib 手写 PNG，零依赖） | 换图标素材时 |

### 随包第三方（GPL 合规）
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `fetch-mihomo.ps1` | 按**钉死的 tag + sha256** 拉取随包内核（`-Mode Runtime`）与**对应源码**（`-Mode Source`，发布时挂到同一个 Release）。校验不符就抛错 | `build.ps1` 内部自动调用；内核升级、或发布前准备源码附件时手动跑 `-Mode Source` |
| `gen-third-party.mjs` | 生成 `THIRD-PARTY-DEPS.md`（Rust + npm 全量依赖与许可），并把**非宽松许可**单独标出来等人判断 | 依赖增减后；公开发布前 |

> `fetch-mihomo.ps1` 里那句"**改没改过内核**"是要紧的：二进制不入库，谁动过都对不上 sha256。
> 这直接决定 GPL 下要不要公开我们自己的改动（当前结论：**没改**，与上游逐字节相同）。

### 门禁 / 校验
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `gate.ps1` | 提交门禁**唯一真相源**：只查本次改动集（明文密钥分级 / 大文件 / rustfmt / clippy --no-deps / tsc --noEmit） | 想知道这次提交能不能过；`.githooks/pre-commit` 与 `pr.ps1` 内部也调它 |
| `check-invariants.ps1` | 不变量检查：I1 只监听回环、I2 系统代理与 DNS 与路由启停前后逐字段一致（Snapshot / Compare / Run） | 动到网络或内核启动路径，或要给"不碰系统网络"背书时 |
| `gen-bindings.ps1` | 由 Rust 侧导出 TS 绑定（默认**只读**落到 target，`-Overwrite` 才取代手写契约） | 改命令面后想知道差集时 |
| `gen-example-config.ps1` | 由 Rust 类型重新生成 `config.example.yaml`，并顺手跑防漂移守卫 | 改配置结构后 |
| `install-hooks.ps1` | 把 `core.hooksPath` 指向 `.githooks`，并设好常用 git 配置 | 新 clone 后跑一次 |

### 审计（只读）
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `audit-inline-copy.mjs` | 列出版面上超过 40 字的内联长文案，按页面分组 | 清理文案前先量（它是工具，不判绿） |
| `audit-duplicate-labels.mjs` | 列出同一页面文件里出现 2 次以上的标签 | 极简改造前先量 |
| `e2e/diag-real-config.mjs` | 只读打开真机库，打印订阅 / 线路 / 通道的实际形态 | 要判断真机上配置到底长什么样时 |

### 协作 / 流程
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `pr.ps1` | 在自己的分支上组装 PR（临时索引 + commit-tree，不动工作区与 HEAD）：`-As` / `-Paths` / `-List` / `-Show` | 干完一段活要留一个可评审的提交时 |
| `merge-pr.ps1` | 主管合并 PR（先逐字节校验工作区与分支一致才动手，`--no-ff` 保留 PR 边界） | 评审通过后 |
| `commit.ps1` | 以指定身份提交，且**只能提交自己范围内的文件**（越界要 `-Override` 明确承担） | 需要直接提交时（先用 `-DryRun` 干跑） |
| `audit-work.ps1` | 找"完成但没入库"的交付（在途很多 + 很久没提交），退出码 0 / 1 | 换班、打包、收尾前 |
| `_team.ps1` | 身份 + 独占文件范围的**唯一真相源**（被 `commit.ps1` / `pr.ps1` / `merge-pr.ps1` dot-source） | **不单独跑**；只有改分工或范围时才改它 |

### 诊断 / 排障
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `who.ps1` | 改动溯源"这是谁改的"（`-Path` / `-File` / `-Author` / `-Uncommitted`）；只看**已提交**内容 | 要追责或找引入者时 |
| `who-is-writing.ps1` | 谁在写仓库 / 仓库是否静止；退出码 0 = 静止可接手，1 = 仍在写 | **接手别人的活之前**（退出码 1 就别落笔） |
| `who-holds-tokenagency.ps1` | 谁占着已安装的 exe（独占写句柄 + Restart Manager + 自启项），全程只读 | 安装器报文件被占用时 |
| `e2e/quick.ps1` | 只跑本次改动相关的那几步 e2e（`-Profile kernel` / `ui` / `net` / `full`），自动置 TEMP 并清残留驱动 | 不想跑满全部档位时 |
| `verify-endpoint-hello.ps1` | 用网关自己的端点打一条 hello（`-Protocol anthropic` / `openai`），打印状态码与上游目标 | 要给"端到端真的通"取证时 |
| `desensitize-subscription.ps1` | 把真实机场订阅脱敏成可入库夹具（格式无关抽取 + 断言输出自身的性质） | 需要真实形状的订阅夹具时（会写文件） |
| `check-kernel-available.py` | 只读检查这台机器上内核能不能跑（内置内核 / 配置路径 / PATH 三条路径） | 真机报"内核起不来"时先排除环境 |
| `check-scratch-db.py` | 读 e2e scratch 库的表数量与行数 | 核对 e2e 数据的还原结果时 |
| `probe-due-subscriptions.py` | 把 scratch 订阅改成"从未成功刷新"，观察到期扫描的非空分支（先备份，`--restore` 还原） | 要验订阅到期轮询真的会触发时（会改 scratch 库） |
| `probe-lifecycle-mode.py` | 打印 scratch 配置里的生命周期模式（窗口 / 托盘 / 最小化） | 排查白屏与托盘假设（只读） |

### e2e
| 脚本 | 作用 | 什么时候跑 |
| --- | --- | --- |
| `e2e/smoke.mjs` | 真机冒烟的唯一入口（tauri-driver + msedgedriver 驱动真窗口） | 交付前全量跑；用法与分档见 `e2e/README.md` |

## 公开 API（对外符号）

调用面（其余参数见脚本头部注释）：

| 调用面 | 稳定参数 | 退出码 |
| --- | --- | --- |
| `pwsh -File scripts/gate.ps1` | `-IndexFile`、`-Against` | 0 通过 / 1 未通过 |
| `pwsh -File scripts/build.ps1` | `-SkipFrontend`、`-NoKernel`、`-WithBindings` | 抛错即非 0 |
| `pwsh -File scripts/build-fast.ps1` | `-Release` | 抛错即非 0 |
| `pwsh -File scripts/gen-bindings.ps1` | `-Out`、`-Overwrite`、`-Force` | 缺命令时拒绝覆盖 |
| `pwsh -File scripts/check-invariants.ps1` | `-Mode`（Snapshot / Compare / Run）、`-Baseline`、`-Exe` | 0 通过 / 1 违反 |
| `pwsh -File scripts/pr.ps1` | `-As`（E1..E5）、`-Paths`、`-Title`、`-MessageFile`、`-DryRun` | 门禁不过就不产生提交 |
| `pwsh -File scripts/who-is-writing.ps1` | `-Minutes`、`-QuietSeconds` | 0 静止 / 1 仍在写 |
| `node scripts/e2e/smoke.mjs` | `--exe`、`--only`、`--keep-data`、`--selfcheck` | 见 `e2e/README.md` |

## 依赖

- **运行时**：PowerShell 7（`pwsh`）、Node（4 个 Node 脚本，e2e 要 Node 24）、Python 3（4 个诊断脚本）、`cargo` 与 `cargo tauri`、`git`。
- **包管理器是 npm**：脚本里只出现 `npm install` / `npm run build` 形态的调用（用 pnpm 会向上撞到父目录的假 workspace 根）。
- **外部工具**：`tauri-driver.exe` 与 `msedgedriver.exe` 只有 e2e 需要，版本与路径见 `e2e/README.md`。
- 没有第三方 npm 依赖：所有脚本只用标准库 / Node 内置模块。

## OWNER

未标注（脚本目录没有 F1-F4 归属标注；**分工与文件范围的权威真源是 `_team.ps1`**，不是本文件）。

## 测试位置

| 类型 | 位置 |
| --- | --- |
| 地图守卫 | `src/app/repoMap.guard.test.ts` 逐条核对本文件提到的路径是否真实存在 |
| 工具引用 | `src/app/design.guard.test.ts` 把 `audit-inline-copy.mjs` 写进它的修法提示里（改了文案规则要同步看） |
| 脚本自检 | `node scripts/e2e/smoke.mjs --selfcheck`（挂具纯逻辑，秒级）；`gen-example-config.ps1` 末尾自己跑 `crates/ta-core/tests/example_config_guard.rs` |
| 无单测目录 | 脚本没有单测：正确性靠退出码被消费方使用（`gate.ps1` 被 `.githooks/pre-commit` 与 `pr.ps1` 消费，`audit-work.ps1` 可当交付前置） |

## 关键数据流 / 边界

- **构建链**：`build.ps1` 先建前端（Rust 的 `generate_context!` 在编译期读前端构建产物）-> 再 Rust 门禁 -> 再 NSIS 打包；**顺序不能反**。
- **门禁链**：`.githooks/pre-commit` 与 `pr.ps1` 共用 `gate.ps1`（不允许各写一套）；提交范围表来自 `_team.ps1`。
- **真机链**：`build-fast.ps1` -> `e2e/quick.ps1` 或 `e2e/smoke.mjs`（真窗口 -> Tauri IPC -> 真 SQLite）。
- **只读边界**：`who.ps1`、`who-is-writing.ps1`、`who-holds-tokenagency.ps1`、`audit-inline-copy.mjs`、`audit-duplicate-labels.mjs`、`e2e/diag-real-config.mjs`、`check-kernel-available.py`、`check-scratch-db.py`、`probe-lifecycle-mode.py` 都不改仓库与配置。
- **会写东西的**：`install-local.ps1`（安装目录）、`commit.ps1` / `pr.ps1` / `merge-pr.ps1`（git 引用）、`gen-bindings.ps1` / `gen-example-config.ps1` / `make-icon.mjs`（生成物）、`check-invariants.ps1`（快照 JSON）、`desensitize-subscription.ps1`（夹具文件）、`probe-due-subscriptions.py`（scratch 库）。

## 已知陷阱

1. **TEMP / TMP 必须指到工作区内**：这台机器上在系统临时目录建目录会拿到"拒绝访问"，`msedgedriver` 与 `cargo test` 会一起红；`e2e/quick.ps1` 自己会设。
2. **跑 e2e 前先清残留进程**（驱动 / 应用 / 内核各一个名字），否则端口被占，后面"起不来"全是假红。
3. **`gen-bindings.ps1` 默认只读**：`-Overwrite` 会取代手写的 `src/shared/bindings.ts`，紧接着 `tsc --noEmit` 立刻失败；只想看差集就别加那个开关。
4. **`build.ps1` 的前端那一步不能省**：干净 clone 里先跑 Rust 会看到与代码无关的 proc macro panic。
5. **多行中文提交信息必须用 `-MessageFile`**：PowerShell 会把多行参数切碎，提交信息会被截断成 pathspec。
6. **多人共享同一棵工作树**：动手前先跑 `who-is-writing.ps1`（退出码 1 = 此刻别落笔），提交后别改同一批文件。

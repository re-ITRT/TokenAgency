# 08 · 技术选型（评审与优化）

针对你给的那份选型逐项评审：**✅ 保留 / ⚠️ 调整 / ➕ 新增 / ❌ 建议去掉**。文末有一页速查表。

---

# 0. 先说结论：一个岔路口和一个事实核查

## 0.1 事实核查：`tauri-plugin-mihomo` 不在 crates.io

```
GET https://crates.io/api/v1/crates/tauri-plugin-mihomo  →  404  crate does not exist
```

它是 **clash-verge-rev 组织下的 git 仓库**，Clash Verge Rev 这样引：

```toml
tauri-plugin-mihomo = { git = "https://github.com/clash-verge-rev/tauri-plugin-mihomo", branch = "main" }
```

而 **Clash Verge Rev 本体是 `GPL-3.0-only`**。所以：

| 做法 | 后果 |
|---|---|
| 直接引这个 git 依赖 | 你的项目事实上要和 GPL-3.0 兼容（要么开源 GPL，要么面临许可问题） |
| **自己写 kernel 管理模块（建议）** | 只借鉴"该管哪几件事"的思路，代码自己写，许可证自由 |

好消息是：**需要管的只有 5 件事**，自己写大约 300–500 行，不值得为它背 GPL。

## 0.2 岔路口：内核选型 = 你其实选了"方案 B"

你的选型（外部 mihomo 二进制）等于是把网络层交给内核。我把这个取舍摆清楚，因为它直接推翻了几个之前的成本估算：

| | 自己实现拨号（我上一版） | **外部 mihomo（你的选型）** |
|---|---|---|
| "完全兼容 Clash 订阅" | 最贵的需求：要自己写全协议 parser + 协议栈，2–3 周起，之后长期维护 | **几乎免费**：mihomo 原生支持 L1/L2/L3，连订阅自带的分组、规则、`proxy-providers` 全都有 |
| 节点协议维护 | 持续负担（新协议、新混淆插件） | 内核升级即得 |
| 跳数 | 3 跳 | 4 跳（多一跳 `127.0.0.1` loopback，实测开销 ~0.1ms，**性能上可忽略**） |
| 进程数 | 1 | 2 |
| 启动延迟 | 无 | 内核就绪约 200–800ms |
| 许可 | 自由 | 内核是 GPL-3.0 → 见 [04 §10](04-network-core.md) |
| 崩溃面 | 一个进程全挂 | 内核崩了可单独重启（反而是优点） |

**结论：走内核。** 它把"完全兼容 Clash"这个最贵需求变成免费，还顺带消掉了"自研协议栈维护"这个最大风险项。之前对跳数的顾虑是概念洁癖，不是工程问题。

**但架构上不要丢掉 `OutboundDialer` 抽象**——保留它，默认实现是 `KernelDialer`（经由内核入站端口），把 `BuiltinDialer` 留作以后的"零依赖构建"选项。这样上层路由、UI、配置一个字都不用改。

## 0.3 你的选型里缺了三块地基

| 缺失 | 为什么必须有 |
|---|---|
| **axum / hyper** | 你说"后端负责核心逻辑"，但**本地网关本身就是个 HTTP 服务器**——它是整个产品的心脏，不在你的清单里 |
| **SQLite（sqlx）** | 用量账本、请求日志、成本中心、粘性表全靠它；没有它第 6 章的功能一个都做不了 |
| **secrets（keyring）** | 多 key 管理的前提；key 不能明文进配置文件 |

---

# 1. 前端

## 1.1 ✅ 保留：React 19 + TypeScript + Vite

选型没问题。补充几条**具体怎么用**：

- React 19 里对本项目真正有用的三个能力：`use()` 处理 Promise（配合 Suspense 做数据加载）、`useOptimistic`（托盘/开关这类乐观交互）、ref 作为普通 prop（少写 forwardRef 样板）。React Compiler 可选，别在 v1 引入——数据密集页面容易碰到它不支持的写法。
- TypeScript 配置：`strict: true`、`noUncheckedIndexedAccess: true`（处理请求日志这种稀疏数组时能救命）。
- Vite：`build.target: 'esnext'`（Tauri 的 WebView 版本可控）；`manualChunks` 把 MUI 和图表库拆开；开发时用 `server.strictPort`。

## 1.2 ⚠️ 调整：MUI v7 + Emotion

方向对（和 Clash Verge 同族的成熟方案），但**数据密集型桌面应用**要改三处默认值：

| 问题 | 调整 |
|---|---|
| Emotion 是运行时 CSS-in-JS，高频更新（实时请求流）会触发大量样式重算 | 开启 MUI 的 **CSS 主题变量**（`cssVariables: true`），把主题降为 CSS 变量，减少运行时开销。若仍不够，MUI v7 已支持可选的**零运行时**样式方案（Pigment CSS）——但**别在 v1 上**，迁移成本不小 |
| MUI 默认尺寸是按触屏设计的，桌面端一屏放不下几行 | 全局 `size="small"` + `density="compact"`，自定义 theme 收窄 `spacing`（4px 基准），表格行高锁 32px |
| 表格和图表 MUI 本体不够用 | 加 **MUI X DataGrid**（虚拟滚动 + 列固定 + 服务端分页，请求日志/成本中心/通道矩阵都靠它）；图表见下 |

> MUI v7 的具体 API（`cssVariables` 的写法、`Grid` / `GridLegacy` 的差异、Pigment CSS 的接入方式）请按你锁定的版本核对，我上面的描述是能力层面的。

### 图表：一个决定 + 一个理由

| 方案 | 体积 | 性能 | 省事程度 | 结论 |
|---|---|---|---|---|
| **ECharts** | ~350KB gzip | 好（大数据量要开 `large` 模式） | ★★★ 图例/缩放/提示框全免费 | **建议：主趋势图用它** |
| uPlot | ~40KB | 极好（10 万点无压力） | ★ 提示框、图例要自己写 | 数据量爆炸时再换 |
| MUI X Charts | 中 | 一般 | ★★ 和 MUI 一致性好 | 想少一个依赖可以选 |
| Recharts | 中 | 差（点多了卡） | ★★★ | ❌ 不推荐用于趋势图 |

**决策**：主趋势图用 **ECharts**（个人项目省时间 > 省 300KB），数字卡里的 sparkline **不用库**——手写 SVG path 20 行代码就够，比引任何库都轻。

## 1.3 ⚠️ 调整：Zustand 的职责边界（这是我想改的最重要一处）

Zustand 管全局状态没问题，但**不要把服务端数据放进 Zustand**。

| 状态类型 | 用什么 | 例子 |
|---|---|---|
| **服务端数据**（有缓存、需要失效、分页） | ➕ **TanStack Query** | Provider 列表、模型目录、通道矩阵、请求日志分页、成本中心查询结果 |
| **UI 状态**（瞬时、本地） | **Zustand** | 当前选中的页面、抽屉开关、筛选条件、主题、正在编辑的表单草稿 |
| **实时推送**（高频） | Zustand + 环形缓冲 | 实时请求流（只保留最近 500 条） |

理由：请求日志分页、成本中心任意维度查询、通道矩阵这类数据的"加载中/错误/重新拉取/乐观更新"用手写 Zustand 会写出几百行样板，TanStack Query 直接给。

Zustand 补充两点：开 `subscribeWithSelector` 中间件（Tauri 事件能精准订阅某个 slice）；嵌套配置编辑用 `immer` 中间件（省掉大量展开运算符）。

## 1.4 ➕ 前端要补的依赖

| 用途 | 选择 | 说明 |
|---|---|---|
| 表单 + 校验 | **react-hook-form + zod** | zod schema 由 Rust 侧的 JSON Schema 生成（见 §4），**校验规则只有一份** |
| 列表虚拟化 | DataGrid 自带；非表格的长列表用 `@tanstack/react-virtual` | |
| 国际化 | **react-i18next** | 中英双语；配置文件里也可以存 i18n key |
| 日期时间 | `dayjs`（体积小）或 DataGrid 自带的 | 别引 moment |
| 剪贴板 | `tauri-plugin-clipboard-manager` | "复制接入信息"要用 |
| 图标 | `@mui/icons-material` 按需引入，或 lucide-react | 按需引入否则包会很大 |
| 代码/JSON 查看 | `@uiw/react-codemirror` 或 `react-json-view-lite` | 请求日志详情抽屉里看 body |

## 1.5 前端目录建议

```
src/
  app/            路由、Provider 树、主题、i18n 初始化
  pages/          十个页面（概览/服务商/凭据/模型目录/通道矩阵/策略/网络/成本中心/日志/设置）
  features/       按领域切分的组件与 hooks（如 features/request-log/）
  shared/
    api/          Tauri invoke 封装（★ 全部经这里，页面不直接 invoke）
    bindings.ts   ← tauri-specta 生成，禁止手改
    schema/       ← 由 JSON Schema 生成，禁止手改
    components/   通用组件（数字卡、Sparkline、额度条、状态灯）
    hooks/        useTauriEvent、usePolling、useVirtualList
  stores/         Zustand slices
```

---

# 2. 后端

## 2.1 ✅ 保留：Tauri v2 + Tokio

- Tauri v2 的 `tray-icon` feature 必须开；`async_runtime` 本身就是 Tokio，不用自己建 runtime。
- 网关作为长驻任务 spawn 在 Tauri 的 runtime 里；窗口关闭时 `CloseRequested` → `prevent_close()`，退到托盘。
- macOS 上若要"纯托盘无 Dock 图标"：`ActivationPolicy::Accessory`。

### 要装的 Tauri 插件

```
tauri-plugin-shell          ← sidecar 拉起 mihomo（必装）
tauri-plugin-single-instance
tauri-plugin-notification
tauri-plugin-autostart
tauri-plugin-opener
tauri-plugin-dialog
tauri-plugin-fs
tauri-plugin-process
tauri-plugin-window-state
tauri-plugin-deep-link      （ta:// 协议，从浏览器一键导入订阅）
tauri-plugin-clipboard-manager
tauri-plugin-log            ← 与 tracing 打通
tauri-plugin-updater
tauri-plugin-global-shortcut（可选：全局快捷键暂停/继续）
```

## 2.2 ⚠️ 内核管理：自己写，别引 GPL 插件

自己写的 `ta-kernel` 模块要管的只有 5 件事：

| 职责 | 做法 |
|---|---|
| **进程生命周期** | sidecar 拉起 / 就绪探测（轮询 REST `/version`）/ 崩溃指数退避重启 / 退出时确保不留孤儿（Windows 用 Job Object 绑定） |
| **配置生成** | 从领域模型生成 `kernel.generated.yaml`（带 `GENERATED` 头），强制写死隔离项：`tun.enable: false`、`allow-lan: false`、`bind-address: 127.0.0.1`、`mixed-port: 0` |
| **Lane 映射** | 为每条 lane 生成一个 `listeners` 入站（`type: mixed`、`listen: 127.0.0.1`、`proxy: <组名>`）。这是"每个模型走不同线路"的落地机制 |
| **REST 控制** | 切节点（`PUT /proxies/{name}`）、更新订阅（`PUT /providers/proxies/{name}`）、测速、重载配置（`PUT /configs?force=true`） |
| **端口分配** | lane 端口从 39100–39299 分配，落库，启动时 bind 试探复用 |

> 关于 mihomo 的 REST API 与 `listeners[].proxy` 语义，[04](04-network-core.md) 里已从官方文档核实过。

## 2.3 ⚠️ 重点评审：Boa Engine

核实数据（crates.io）：**最新 0.22.0**（edition 2024，`Unlicense OR MIT`，约 9.7 万行 Rust）。Clash Verge Rev 用的正是 `boa_engine = "0.22.0"`。

**它是好东西，但用错地方会拖垮项目。** 三个必须知道的点：

1. **不是完整 ES 规范**。crates.io 的官方描述原话是 "Currently, it has support for **some** of the language"。用户从网上抄的 JS 脚本有相当概率跑不起来。
2. **二进制体积和启动成本**。9.7 万行解释器进包，对"轻量原生应用"是个不小的负担。
3. **它是攻击面**。任何"执行用户脚本"的功能都是沙箱逃逸的候选目标。

### 我的建议：v1 不做脚本引擎

理由：本项目 95% 的"配置增强"需求都能用**声明式**表达——模型名正则替换、订阅节点重命名、路由条件表达式、header 改写。这些用我们的策略 DSL + `regex` 就够了，而且**可校验、可模拟、可 diff**（脚本做不到）。

**判断标准**：*你的用户会不会把网上找的 JS 脚本粘进来？*
- 会（因为要兼容 Clash Verge 的脚本生态）→ 用 Boa，但**只允许它在配置加载时跑一次**，绝不放在请求热路径上（每请求跑一次 JS = 慢且危险）。
- 不会 → 用 **Rhai**（专为 Rust 嵌入设计，体积小一个数量级，沙箱能力更明确，语法足够表达配置逻辑）。

### 如果最终用 Boa，这四条必须做

```rust
// 沙箱 checklist（具体 API 名以 0.22 文档为准）
1. 不注入任何 host 函数 —— 不给文件、网络、进程、环境变量能力
2. 指令数配额（instruction quota）+ wall-clock 超时（建议 200ms）
3. 内存/栈上限
4. 脚本编译结果缓存，只在配置变更时重新执行
```

## 2.4 ⚠️ 调整：reqwest

选型对，但有三个**必须处理的细节**：

| 细节 | 为什么 |
|---|---|
| **每个 lane 一个 `Client` 实例** | 连接池必须按出口线路隔离，否则 A 线路的连接会被复用到 B 线路 |
| 用 **rustls**（不是 native-tls） | 三个平台行为一致，不依赖系统 OpenSSL，打包省心。`rustls` + `webpki-roots` |
| **订阅下载不能走 lane** | ⚠️ 鸡生蛋问题：你必须先把订阅下下来，才知道有哪些节点。所以订阅下载要用一个**独立的 `Client`**：默认直连，可选"跟随系统代理"或"手动指定一个引导代理" |
| 超时分级 | `connect_timeout` 短（10s）、`read_timeout` 长（流式 600s）、不要设总超时 |
| SSE 流式 | `features = ["stream"]`，响应用 `bytes_stream()` 零缓冲转发 |

## 2.5 ✅ 保留：serde / serde_yaml_ng

`serde_yaml_ng` 是对的（上游 `serde_yaml` 已归档）。补三条：

```toml
serde_json = { version = "1", features = ["preserve_order"] }   # ★ 保字段顺序，配置回写才稳定
serde_path_to_error = "0.1"                                     # ★ 配置报错能指到具体字段路径
schemars = "1"                                                  # ★ 从 Rust 类型导出 JSON Schema
```

### ⚠️ 这里有个必须提前决策的坑：YAML 注释会丢

`serde_yaml_ng` 读写**不保留注释**。如果 YAML 是真源、GUI 又频繁回写，用户手写的注释每次都会被抹掉。

三个方案：

| 方案 | 说明 |
|---|---|
| **A. 真源改成 SQLite，YAML 降为导入/导出**（建议） | GUI 读写 DB（原子、并发安全、无需序列化全文件）；YAML 用于 git 管理、分享、迁移。注释丢失不再是问题——因为 YAML 是"生成物" |
| B. YAML 仍是真源 + 结构化编辑库保留注释 | Rust 生态目前没有可靠的注释保留 YAML 编辑器，风险高 |
| C. YAML 真源 + 接受丢注释 | 简单，但用户会烦（"我写的注释怎么没了"） |

**建议 A**，但要保留 ADR-05 想要的东西：提供 `ta export/import`、一键"导出可 git 的 YAML"、以及**可选的"文件监听模式"**——如果你就是想手写 YAML 并用 git 管，开这个模式，文件变更时热重载（此时 GUI 编辑写回会覆盖，UI 上要明确警告）。

## 2.6 ➕ 后端必须补的核心依赖

| 用途 | 依赖 | 备注 |
|---|---|---|
| **本地网关（心脏）** | `axum` + `hyper` + `http-body-util` + `bytes` | HTTP/1.1 + h2，`Body::from_stream` 做 SSE 零缓冲转发 |
| 中间件 | `tower` + `tower-http`（`limit` / `cors` / `trace`） | 本地密钥校验、RPM 限流 |
| **持久化** | `sqlx`（`sqlite` + `runtime-tokio` + `migrate`） | 用运行时校验的 `query_as` 而不是编译期 `query!`，避免构建期要 DATABASE_URL |
| **密钥** | `keyring` | Windows Credential Manager / macOS Keychain / Linux Secret Service |
| **类型桥接** | `specta` + `tauri-specta` | ★ Rust 命令/结构体自动生成 TS 类型，消灭一整类前后端不一致 bug |
| 日志 | `tracing` + `tracing-subscriber` + `tracing-appender` | 结构化日志；与 `tauri-plugin-log` 打通 |
| 错误 | `thiserror`（库内）+ `anyhow`（边界） | |
| 重试退避 | `backon` | 订阅下载、健康检查 |
| 热重载 | `arc-swap` | 配置/路由表无锁热更新 |
| 压缩 | `zstd`（请求日志 body） | 比 flate2 压缩率高 |
| 哈希 | `sha2` + `base64` | 图片只存哈希不存本体 |
| 时间 | `chrono` | |
| 文件监听 | `notify` | "文件监听模式"用 |
| ID | `nanoid` 或 `uuid` | 请求 ID 要短且可读 |
| 并发原语 | `tokio::sync::Notify`（取消原语；**不用** `tokio-util` 的 `CancellationToken`，避免为它单独开 `sync` feature） | 客户端断连时及时取消上游请求 |

## 2.7 Rust workspace 结构（强烈建议）

把 90% 的逻辑从 `src-tauri` 里拿出来，**不依赖 Tauri 就能 `cargo test`**：

```
Cargo.toml            (workspace)
crates/
  ta-core/            领域模型、配置 schema、校验、策略引擎、token 归一化   ← 无 Tauri 依赖
  ta-gateway/         axum 网关、透传、协议适配、计费、trace                ← 无 Tauri 依赖
  ta-kernel/          mihomo 进程管理、配置生成、REST client、lane 映射
  ta-store/           SQLite、迁移、账本聚合查询
  ta-secrets/         keyring 封装
  ta-subscription/    订阅下载、格式嗅探、缓存与回滚
src-tauri/            ★ 薄：命令层、托盘、窗口、插件注册
src/                  前端
```

**收益**：`cargo test -p ta-gateway` 秒级跑完；`ta serve` 无 GUI 模式直接复用 `ta-gateway`（见 [05 §3](05-ui-tray.md) 的 CLI）；将来想做成 headless 服务端几乎是零成本。

---

# 3. IPC 设计（前后端契约）

三条规则：

1. **前端不直接调管理 API（8788）**，全部走 Tauri IPC。理由：类型安全（specta 生成）、不用管本地 token、不怕端口被占。
   `8788` 只留给 CLI 和外部脚本。
2. **命令（invoke）用于请求-响应**：`get_providers`、`save_channel`、`query_cost`、`test_provider`。
3. **事件（emit）用于推送**：
   ```
   request:new        单条新请求（实时流）
   usage:tick         用量聚合（节流 1s 批量发，不要每条请求都发）
   channel:state      通道状态变化（冷却/耗尽/恢复）
   lane:health        线路健康与延迟
   kernel:status      内核就绪/崩溃/重启
   config:changed     配置变更（托盘与 UI 同时刷新）
   log:append         日志追加（仅在打开日志页时订阅）
   ```

**关键性能点**：实时请求流用 **环形缓冲 + 批量推送**（每 200ms 合并一次），不要一条请求一次 IPC——每秒几十条请求时会打爆 WebView 的消息队列。

---

# 4. 单一真源：Rust 类型 → TS 类型 → 表单校验

这是我最想加进你选型的一条，能省掉大量重复工作和前后端不一致 bug：

```
Rust 结构体 (#[derive(Serialize, Deserialize, JsonSchema, Type)])
        │
        ├── schemars ──→ config.schema.json ──→ json-schema-to-zod ──→ zod schema ──→ react-hook-form
        │                                                                   （前端校验规则只有一份）
        │
        └── tauri-specta ──→ src/shared/bindings.ts（invoke 的类型化封装）
```

配置结构改一次，前端类型、校验、表单提示全部跟着变，**不用手写第二遍**。

---

# 5. 构建、打包、分发

```toml
# Cargo.toml [profile.release]
lto = true
codegen-units = 1
opt-level = "z"        # 体积优先；网关热路径若不够快，单独把 ta-gateway 提到 "3"
strip = true
panic = "abort"
```

- **sidecar 打包**：`tauri.conf.json` → `bundle.externalBin: ["binaries/mihomo"]`，文件名必须带 target triple（如 `mihomo-x86_64-pc-windows-msvc.exe`）。三个平台各一份二进制，CI 里按矩阵拉取。
- **内核版本锁定**：记录并随应用发布；`net.kernel.provider: external` 时允许用户指定路径。
- **代码签名**：Windows 不签会被 SmartScreen 拦；macOS 不公证无法直接运行。这是"个人自用"和"能发给别人"的分界线，v1 可以只做 Windows 未签名版 + 文档说明。
- **更新**：`tauri-plugin-updater`。注意 **sidecar 二进制也要跟着更新**——更新包里同时替换。
- **首次启动体验**：内核就绪约 200–800ms。窗口先渲染骨架屏，不要白屏等内核。

---

# 6. 测试策略

| 层 | 工具 | 测什么 |
|---|---|---|
| Rust 单元 | 内置 + `pretty_assertions` | 策略求值、token 归一化、额度状态机、成本计算 |
| **直通保真（关键）** | `insta` 快照 | 构造含未知字段/新 part 类型/特殊头的请求，快照"网关发出的字节"，改坏了立刻红灯（对应验收 A1/A2） |
| 上游打桩 | `wiremock` 或 `httpmock` | 429/402/5xx/超时/流式中断，验证重试边界（A5） |
| 订阅解析 | 真实订阅样本（脱敏后入库为 fixture） | 节点数/分组数一致性、HTML 错误页不清空缓存（A9） |
| 前端单元 | Vitest + React Testing Library | 组件与 hooks |
| E2E | `tauri-driver` + WebDriver | 托盘、窗口、完整链路冒烟 |
| 性能 | `criterion` | 网关转发吞吐、SSE 首字节延迟 |

---

# 7. 性能预算（给自己定个线，超了就查）

| 指标 | 目标 |
|---|---|
| 网关自身开销（不含上游） | < 5ms P50，< 20ms P99 |
| SSE 首字节额外延迟 | < 10ms |
| 冷启动到窗口可见 | < 1.5s |
| 内核就绪 | < 1s |
| 常驻内存（含内核） | < 150MB |
| 安装包（Windows） | < 40MB（不含内核约 15MB） |
| 前端首屏 JS | < 400KB gzip（MUI + ECharts 是大头，必须分块） |
| 看板在 10 年数据量下的查询 | < 300ms（靠 `usage_daily` 汇总表） |

---

# 8. 一页速查表

| 你的选型 | 评审 | 结论 |
|---|---|---|
| React 19 + TS | ✅ | 保留；`strict` + `noUncheckedIndexedAccess`；React Compiler 别在 v1 上 |
| Vite | ✅ | 保留；`manualChunks` 拆 MUI / 图表 |
| MUI v7 + Emotion | ⚠️ | 保留，但开 **CSS 主题变量**、`size=small` + `density=compact`；**加 MUI X DataGrid** |
| Zustand | ⚠️ | 保留，但**服务端数据交给 TanStack Query**；开 `subscribeWithSelector` + `immer` |
| Tauri v2 | ✅ | 保留；补 §2.1 的 12 个插件 |
| **mihomo 外部二进制** | ⚠️ | **接受，且这是对的**——它让 Clash 兼容免费。但保留 `OutboundDialer` 抽象 |
| **tauri_plugin_mihomo** | ❌ | **crates.io 上不存在**，是 clash-verge-rev 的 GPL git 仓库。**自己写 300–500 行 kernel 模块** |
| Tokio | ✅ | 保留；用 Tauri 自带 runtime，别自建 |
| Serde / serde_yaml_ng | ✅ | 保留；加 `serde_json/preserve_order`、`serde_path_to_error`、`schemars` |
| **Boa Engine** | ⚠️ | **v1 建议不做脚本引擎**。要做就用 Rhai（更小更安全）；坚持 Boa 必须沙箱化且只在配置加载时跑 |
| Reqwest | ⚠️ | 保留；**每 lane 一个 Client**、用 rustls、**订阅下载走独立 Client（鸡生蛋问题）** |
| — | ➕ | **axum/hyper**（网关本体，缺失的地基） |
| — | ➕ | **sqlx/SQLite**（账本、日志、成本中心的地基） |
| — | ➕ | **keyring**（密钥） |
| — | ➕ | **specta + tauri-specta**（前后端类型单一真源） |
| — | ➕ | **tracing + backon + arc-swap + zstd + sha2 + notify + tokio-util** |
| — | ➕ | **Rust workspace 拆分**，让 ta-core / ta-gateway 脱离 Tauri 可测 |
| — | ➕ | 前端补 **react-hook-form + zod + i18next + @tanstack/react-virtual** |

## 最后一条：许可证

你的栈和 Clash Verge Rev 高度重合，**这是好事**（说明可行、有参照），但：

> **可以借鉴思路，不要拷贝代码。** Clash Verge Rev 是 `GPL-3.0-only`。一旦拷了它的代码，你的项目就要 GPL-3.0。自己写的部分 + 把 mihomo 作为独立 sidecar 调用，是可以保持许可自由的最少代价路径（具体法律判断仍需你自己或律师确认，我只说工程事实）。

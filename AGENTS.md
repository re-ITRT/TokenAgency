# AGENTS.md — TokenAgency 的 AI 可读项目地图

> 这份文件是**给 AI 编码代理的入口**，不是给人看的产品介绍（那是 [README.md](README.md)）。
> 目标：让代理**先找模块 → 再找文件 → 再找符号**，而不是一上来就全仓 grep。
>
> **纪律**：本文件的每一条声明都必须**可被验证**。守卫
> `src/app/repoMap.guard.test.ts` 会拿代码去核对它（路由数、命令数、路径是否存在、命令表是否齐全）。
> 地图一旦与代码不一致，守卫就红 —— **不允许让地图烂掉**。

---

## 0. 一句话架构

```
客户端（OpenAI / Anthropic / Responses / Gemini 形状的请求）
        │
        ▼
   ta-gateway  ── 入站分类 → 鉴权 → 路由选通道 → 全透传 → 协议适配 → 计量 → trace
        │                                                      │
        │ 出站拨号（每 lane 一个 HTTP client）                 └→ ta-store（SQLite）
        ▼
   ta-kernel（mihomo sidecar，每 lane 一个 mixed listener 127.0.0.1:3910x）
        │
        ▼
   上游中转站 / 官方 API
```

*Rust workspace 负责全部逻辑；`src-tauri` 只是**薄壳**（命令层 / 托盘 / 生命周期）；
`src` 是 React 前端。*

---

## 1. 命令（复制即用）

| 目的 | 命令 |
| --- | --- |
| 类型检查（最快） | `cargo check --workspace` |
| 全量测试 | `cargo test --workspace` |
| 只测网关适配层 | `cargo test -p ta-gateway --lib` |
| Lint（**零容忍**） | `cargo clippy --workspace --all-targets -- -D warnings` |
| 格式化 | `cargo fmt --all` / `cargo fmt --all -- --check` |
| 前端测试 | `npx vitest run <文件>`（全量 `npx vitest run`） |
| 前端类型 | `npx tsc --noEmit` |
| 前端构建 | `npm run build` |
| 出安装包（**慢**） | `pwsh -File scripts/build.ps1` |
| 装到本机 | `pwsh -File scripts/install-local.ps1` |
| 真机 e2e（**很慢，几分钟**） | `node scripts/e2e/smoke.mjs --exe "<装好的 exe>"` |
| 重新生成命令绑定 | `pwsh -File scripts/gen-bindings.ps1` |

**包管理器是 npm，不是 pnpm**（pnpm 会向上找 workspace 根，撞上父目录那个 0 字节 `package.json`）。

**沙箱与临时目录（Windows）**：跑 cargo/build/e2e 前把 `$env:TEMP`/`$env:TMP` 指到仓库内
（`.ta-tmp\tmp`），否则临时文件散落系统盘。e2e 前先杀掉
`msedgedriver, tauri-driver, tokenagency, mihomo`。

**验证纪律（用户明令）**：改哪一块就跑哪一块的验证，不要每轮全量；
只有动到**配置 / 绑定 / 日志字段 / 示例配置**时才跑跨切面守卫；
只有要交**真机产物**时才 build → install → e2e。

---

## 2. 目录职责与模块边界

| 路径 | 职责 | 允许依赖 | 测试位置 |
| --- | --- | --- | --- |
| `crates/ta-core` | **纯领域层**：领域模型、配置校验、路由引擎、额度、定价、计量。**无 IO** | 无（叶子） | `crates/ta-core/tests/` + 源码内联 `#[cfg(test)]` |
| `crates/ta-store` | SQLite 持久化：配置、账本、trace、粘性、额度状态 | ta-core | `crates/ta-store/tests/` |
| `crates/ta-kernel` | mihomo sidecar：进程看护、YAML 生成、REST 控制、lane 端口分配 | ta-core | 内联 |
| `crates/ta-subscription` | 订阅下载、格式嗅探、版本与回滚 | ta-core | 内联 |
| `crates/ta-secrets` | 系统钥匙串封装 + `${secret:...}` 引用解析 | ta-core | 内联 |
| `crates/ta-gateway` | **本地网关本体**：入站、全透传、路由管线、分发、协议适配、trace | 上面全部 | 源码内联 `#[cfg(test)]` |
| `src-tauri` | Tauri 壳：命令层、托盘、插件、生命周期（**薄**） | 全部 crate | 源码内联 + `src-tauri/tests/`（`src-tauri/tests/boot_real_kernel.rs` 真内核全路径、`src-tauri/tests/runtime_manifest.rs` Windows 清单守卫） |
| `src` | React 19 + MUI v7 前端（页面 / 组件 / 契约绑定） | 无（只经 IPC） | `src/**/*.test.tsx` |
| `docs` | 设计文档**只剩 16 篇**（09 是架构基线、10/11 是接口契约；索引见 `docs/README.md`） | — | — |
| `scripts` | 构建 / 门禁 / 审计 / e2e | — | — |
| `verify` | 厂商错误响应样本（HTML，用于对齐错误形状） | — | — |

**边界铁律**：
* **转发路径**必须走 `GatewayDeps.dialer` —— 另开一条出站路径会**绕过线路策略**。
  注意措辞：`src-tauri` **是**依赖 `reqwest` 的（`src-tauri/Cargo.toml`，额度查询的出网口），
  所以这不是"不许用 reqwest"，而是"**用户流量的转发链路上**不许出现第二个 client"。
  源码级约束有**两条**（别搞混，它们的出处不同）：
  * `crates/ta-gateway/src/probe.rs` 的 `production_slice_has_no_second_egress_path`
    —— `include_str!("probe.rs")`，要求**它自己**的生产段走 `self.dialer.execute(`；
  * `src-tauri/src/commands/upstream.rs` 的 `the_production_slice_has_no_second_egress_path`
    —— `include_str!("upstream.rs")`，禁 `reqwest::` / `TcpStream` / `hyper::Client` / `ureq::`，
      要求走 `UpstreamProbe::new(`。**扫命令层的那条在命令层自己身上**。
* 前端**不直连上游**，一律经 IPC 命令。
* `ta-core` 不出现 IO（文件 / 网络 / 时钟都靠注入的 trait）。

---

## 3. 领域词汇表（业务术语 → 代码符号）

**改代码前先在这里对词**：界面上的中文与代码里的英文是两套词，别按中文去 grep。

| 界面术语 | 代码符号 | 定义在哪 | 含义 |
| --- | --- | --- | --- |
| 服务商 | `Provider` / `ProviderConfig` | `crates/ta-core/src/domain/mod.rs` | 上游中转站/官方的**账号级**配置（baseUrl、协议、密钥引用） |
| 凭据 | credential（`credentials[]`） | 同上 | 一个服务商下的一把密钥 |
| 通道 | `Channel` / `ChannelId` | `crates/ta-core/src/domain/` | 服务商 × 上游模型 的可选目标（路由的最小单位） |
| 线路 | `Lane` / `LaneId` | `crates/ta-core/src/domain/mod.rs` | 出口路径 = 服务商 + 上游模型 + 代理；**每条 lane 一个内核 listener 端口** |
| 节点 | node | `ta-kernel` / `ta-subscription` | 订阅里的代理节点（mihomo 的 proxy） |
| 订阅 | `Subscription` | `crates/ta-core/src/domain/mod.rs` | 机场订阅（含 `interval_s`、过滤、健康检查） |
| 出口 | exit | `crates/ta-gateway/src/dispatch.rs` | 请求实际走的 lane；`直连` = 不经内核 |
| 策略 | `Policy` | `crates/ta-core/src/domain/`（**路由策略**） | 按条件选通道的规则 |
| 访问密钥 | `ApiKey` | `crates/ta-store/src/api_key.rs` | **我们对外**发给客户端的密钥（不是上游 key！） |
| 上游密钥 | secret / `${secret:…}` | `ta-secrets` | 上游 key，只以引用形式存在配置里 |
| 内核 | kernel / mihomo | `ta-kernel` | 随包的代理内核 sidecar |
| 请求日志 | trace | `crates/ta-store/src/trace_store.rs` | 请求级轨迹（mode 决定记多少） |
| 用量 / 花费 | usage / `UsageRecord` | `crates/ta-core/src/types.rs`、`crates/ta-store/src/usage_store.rs` | 账本；花费是**本地估算**，不是上游账单 |
| 计量器 | `ResponseMeter` | `crates/ta-gateway/src/meter.rs` | 把响应 usage 落账的钩子 |
| 协议适配 | adapter / IR | `crates/ta-gateway/src/adapt/` | 边缘 N 种协议 → 内部 1 种 IR |

**类型速查**（都在 `crates/ta-core/src/types.rs`）：
`Protocol`（`Openai` / `Anthropic` / `OpenaiResponses` / `Gemini` / `Custom`）、
`TokenKind`、`TokenUsage`、`Money`、`UsageSource`、`UsageRecord`、
`LogMode`（`Off` / `Meta` / `Shape` / `Full`）、`Retention`、`RequestSummary`。

---

## 4. 接口契约（改一处要同步改的）

| 契约 | 真源 | 生成物 / 镜像 | 同步命令 |
| --- | --- | --- | --- |
| **IPC 命令面**（TS ↔ Rust） | `src-tauri/src/commands/*.rs` 的 `#[tauri::command]` | `src/shared/bindings.ts`、`docs/contracts/bindings-handwritten-reference.ts.txt` | `pwsh -File scripts/gen-bindings.ps1` |
| **用户配置** | `crates/ta-core/src/domain/mod.rs`（`ConfigDoc`） | `config.example.yaml`、`src/shared/bindings.ts` | `pwsh -File scripts/gen-example-config.ps1` |
| **事件**（后端 → 前端） | `crates/ta-gateway/src/events.rs`（`TaEvent::name()`） | 前端订阅处（`src/shared/…`） | 手动；有守卫逐字核对事件名 |
| **日志字段** | 「日志、错误追踪与诊断」一篇里的字段白名单 | — | 手动；`crates/ta-core/tests/log_field_guard.rs` 会拦白名单外的字段名 |
| **trace 表结构** | `crates/ta-store/src/trace_store.rs` | 前端日志页 | 迁移在 `ta-store` |

**跨切面守卫**（改到上面任何一处就该跑它们）：
`commandSurface.guard` · `argsShape.guard` · `serdeDefault.guard` · `registryClaim.guard` ·
`rowReaders.guard` · `protocolList.guard` · `design.guard` · `duplicateCopy.guard` ·
`uiMinimalism.guard` · `noPlaceholder.guard` · `log_field_guard`（Rust）。

---

## 5. 常见入口（"从哪进去"）

| 入口 | 位置 | 数量 |
| --- | --- | --- |
| **前端路由** | `src/app/routes.ts` 的 `ROUTES`（含 `key` / `path` / `titleKey` / `pages`） | 11 条 |
| **页面组件** | `src/pages/**`（约定：页面文件叫 x.tsx 或 x/index.tsx，自动接线见 `src/app/pageRegistry.tsx`） | 每条路由一个入口 |
| **Tauri 命令** | `src-tauri/src/commands/{apikey,app,catalog,config,entity,log,net,settings_ext,upstream,usage}.rs` | 116 个（10 个文件） |
| **事件** | `crates/ta-gateway/src/events.rs` → 前端 `emit` | 10 个事件名 |
| **HTTP 入站** | `crates/ta-gateway/src/ingress.rs`（`/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/models`） | — |
| **协议适配** | `crates/ta-gateway/src/adapt/{openai,anthropic,responses,gemini}.rs` + `crates/ta-gateway/src/adapt/shared.rs`（表驱动） | 4 协议 |
| **周期任务** | `src-tauri/src/lib.rs`：订阅到期轮询（60 秒轮询粒度，周期由每份订阅 `interval_s` 决定） | 1 个 |
| **内核健康检查** | 由 mihomo 按 `crates/ta-kernel/src/config_gen.rs` 生成的间隔执行（应用侧不自建 cron） | — |

---

## 6. 禁区（**别碰 / 要碰先问**）

| 类别 | 路径 | 为什么 |
| --- | --- | --- |
| **生成物** | `src/shared/bindings.ts`、`docs/contracts/bindings-handwritten-reference.ts.txt`、`config.example.yaml` | 手改会被下次 `gen-*.ps1` 覆盖；改真源后重新生成 |
| **构建产物** | `dist/`、`target/`、`src-tauri/icons/`、`app-icon.png` | 由命令生成 |
| **危险配置** | `src-tauri/src/autostart.rs` | **本项目唯一一处会写操作系统持久状态**的代码；`net.kernel.*`、`dns.*` 会改系统网络行为 |
| **删路由的残留** | `PagePlaceholder` 已删除；`/network/assignments`、`/policies` 已删（文档可能还提） | 别照旧文档去找不存在的页面 |
| **旧模块 / 死代码** | `crates/ta-gateway/src/adapt/` 里的 `stream_is_out_of_band`、Gemini **永远不是客户端协议** | 有断言钉住；别为不可达场景加代码 |
| **不许做的事** | 提交（除非用户明确要求）· 打印密钥 · 把"没接线"写成"已接线" | 见本会话纪律 |

**关于"没接线"的仓库文化**：这个仓库把**静默失效**视为最严重的缺陷。
`meter: None`、`with_trace_store` 少一句、日志 mode 没同步 —— 这些都曾真实发生过并造成
"功能看起来在、其实没数据"。**写代码时优先确保"要么工作、要么响亮地失败"**。

---

## 7. 查代码：工具与用法

### 7.1 本机可用性（实测，2026-10）

| 工具 | 状态 | 用途 |
| --- | --- | --- |
| `rg`（ripgrep 15.1） | ✅ | 文本搜索（**首选**，比 `Select-String` 快得多） |
| `git` | ✅ | `git log -S`（找引入）· `git blame`（找最近改动） |
| `node` / `cargo` / `python` | ✅ | 写一次性分析脚本 |
| `fd` | ❌ 未装 | 文件搜索 —— **用 `rg --files` 代替** |
| `ast-grep` / `sg` | ❌ 未装 | AST 模式搜索 —— **用 `rg` + 本仓的 `rg --files -g` 组合代替** |
| `ctags` | ❌ 未装 | 跳定义 —— **用 `rg '^pub (struct\|enum\|fn) Name'`** |

**没有的东西不要假装有**：代理若要 AST 级查询，先用 `rg` 定位文件，再用 `read` 读；
不要声称"用了 ast-grep"。

### 7.2 高频查法（对症下药）

```powershell
rg -n "符号名" crates src src-tauri             # 文本搜索
rg --files -g "*.rs" crates\ta-gateway           # 列文件（fd 的替代）
rg -n "^pub (struct|enum|fn) \w+" crates/ta-core/src   # 符号清单（ctags 的替代）
rg -n "^\s*#\[tauri::command\]" src-tauri/src -A1       # 命令面
rg -n "impl .* for .*" crates/ta-gateway/src     # 找 trait 实现
git log -S "某符号" --oneline -- crates/         # 它是哪次提交引入的
git blame -L 100,140 -- path/file.rs             # 这段最近被谁改过
git log --oneline -20 -- path/file.rs            # 这个文件的近期历史
```

**动态行为的反推**（静态搜不到时）：`request_id` 是贯穿全链路的关联 id ——
从请求日志（前端日志页 / `trace` 表）拿它，再 `rg -n "<request_id>"` 追日志行；
网关的 `tracing` 字段名被白名单约束，所以字段名可当检索锚点。

### 7.3 还没有、但值得接的（诚实清单）

* **MCP**（GitHub / Sentry / DB schema / OpenAPI / 日志 / Jira）：本机**未配置**。
* **跨仓搜索**（Sourcegraph / OpenGrok）：**未部署**，本仓单仓自查用 `rg` 足够。
* **RAG 索引**（按符号/文件/提交切片 + BM25/向量/重排）：**未建**。
  在接上之前，**用第 5 节的入口表 + 第 3 节的词汇表**做"人肉倒排"，
  这比让代理盲搜便宜得多。

---

## 8. 定位工作流与**验收标准**

任何一次"改哪里"的任务，收尾时必须能回答这六问（答不上就是没定位完）：

| # | 问题 | 怎么答 |
| --- | --- | --- |
| 1 | **入口在哪？** | 第 5 节的表：路由 / 命令 / 事件 / 上游 HTTP |
| 2 | **调用链完整吗？** | 从入口到落库/出网，逐跳写出 `文件:符号`；缺环就说明缺环（别糊） |
| 3 | **要改哪些文件？** | 列出清单；**跨 crate 的改动要点名边界铁律**（§2） |
| 4 | **哪些测试？** | 第 4 节的守卫 + 该模块的测试文件；新增行为要新增会红的测试 |
| 5 | **风险和回滚？** | 是否碰 §6 禁区；可否只改前端/只改单个 crate；回滚是不是 `git revert` 就够 |
| 6 | **无关文件比例高吗？** | 定位质量的度量：改动文件里"为了改这个才碰的"应占多数；比例低说明**搜索没收敛**，回 §7.2 换查法 |

**沉淀规则（每次定位后）**：
1. 发现地图缺的入口 / 词汇 → **更新本文件**（§3 或 §5）；
2. 发现模块内部结构变了 → 更新该模块 `README.md`（见 §9）；
3. 踩到"静默失效"型缺陷 → 补一条**会红的守卫**，而不是只写进文档；
4. 守卫红了别绕过 —— 它红了就是地图烂了。

---

## 9. 模块 README（先找模块，再找文件）

每个模块根目录有一份 `README.md`，格式统一：**入口文件 · 公开 API · 依赖 · OWNER · 测试位置**。

| 模块 | README |
| --- | --- |
| 领域层 | [crates/ta-core/README.md](crates/ta-core/README.md) |
| 持久化 | [crates/ta-store/README.md](crates/ta-store/README.md) |
| 内核 | [crates/ta-kernel/README.md](crates/ta-kernel/README.md) |
| 订阅 | [crates/ta-subscription/README.md](crates/ta-subscription/README.md) |
| 密钥 | [crates/ta-secrets/README.md](crates/ta-secrets/README.md) |
| 网关 | [crates/ta-gateway/README.md](crates/ta-gateway/README.md) |
| 桌面壳 | [src-tauri/README.md](src-tauri/README.md) |
| 前端 | [src/README.md](src/README.md) |
| 脚本 | [scripts/README.md](scripts/README.md) |

---

## 10. 本仓特有约定（AI 最容易违反的）

1. **注释用中文，写"为什么"** —— 不是"这行做什么"。
2. **不许假绿**：改不动就说改不动；`SKIP` 要写清"环境给不了什么条件"，
   而且**先怀疑自己**（第 336 轮有个 SKIP 的理由是假的：数据一直在，只是换了页面）。
3. **不许做文档编号引用**：`docs/` 之外出现形如「docs 加两位编号」的引用即红
   （`docsRef.guard`；注意别把它写成反引号里的示例 —— 地图守卫会把反引号内容当路径去核对）；
   约束要写进注释，且**自足到不需要翻文档**。界面上的 `§` 同样不许（`uiMinimalism.guard`）。
4. **一屏一个主操作 / 版面文案 ≤36 字 / 表格 ≤6 列** —— 有守卫（`uiMinimalism.guard`）。
5. **测试读 i18n 真源**（`zhCN["key"]`），不把中文句子抄进断言。
6. **`data-testid` 是 e2e 的契约**：可以加，删之前先 `rg` 一遍 `scripts/e2e/smoke.mjs`。

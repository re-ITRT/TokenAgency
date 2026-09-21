# 01 · 总体架构

> 本文是**专题文档**（技术选型、进程模型、请求管线、ADR 详述与论证）。
> 收敛后的**架构基线**（C4 视图、crate 分层、时序、质量属性、验收场景）见 **[09 软件架构设计](09-architecture-design.md)**。

## 1. 技术选型

| 层 | 选型 | 理由 |
|---|---|---|
| 桌面外壳 / 托盘 | **Tauri 2**（Rust + WebView2/WKWebView） | 托盘 API 完整、安装包小、Rust 侧可直接跑网关，不必再开 Node 进程 |
| UI | **React 19 + TypeScript + Vite + MUI v7 + Emotion** + Zustand + TanStack Query | 与 Clash Verge 同族栈，成熟；详见 [08](08-tech-stack.md) |
| 网关 | **axum + hyper + tokio** | HTTP/1.1 + HTTP/2、SSE 流式 `Body::from_stream` 零缓冲转发 |
| 网络内核 | **mihomo（Clash.Meta）sidecar 子进程** | 订阅解析、节点协议、测速、规则全都现成 → 让"完全兼容 Clash 订阅"从最贵需求变成免费。见 [08 §0.2](08-tech-stack.md) 与 [04](04-network-core.md) |
| 出站拨号 | **`OutboundDialer` 抽象**：默认 `KernelDialer`（经内核入站端口） | 保留抽象是为了留一条"零依赖内建拨号"的后路，上层路由/UI/配置完全不感知实现 |
| 持久化 | **SQLite**（sqlx）+ OS Keychain（keyring） | 账本、日志、成本中心、粘性表 + 密钥不入库 |
| 类型契约 | **specta / tauri-specta** + **schemars** | Rust 类型自动生成 TS 类型与 JSON Schema，配置校验规则只有一份 |
| 打包 | Tauri bundler + `externalBin`（内核二进制随包分发） | Windows 无需管理员权限（不装 TUN、不碰系统代理） |

> 备选：Electron。放弃原因：内存占用翻倍、托盘体验差、需要为内核进程再开一条 IPC 通道。

## 2. 进程与端口模型

```
TokenAgency.exe (Tauri 主进程)
├── tokio runtime
│   ├── 网关 HTTP 服务        127.0.0.1:8787   ← 客户端唯一入口
│   ├── 管理 API (本地)        127.0.0.1:8788   ← 给 CLI/脚本用；GUI 走 Tauri IPC
│   ├── OutboundDialer (KernelDialer)
│   │     ├── lane-jp   → 127.0.0.1:39101  → 内核 → 节点「东京-03」
│   │     ├── lane-us   → 127.0.0.1:39102  → 内核 → 节点「洛杉矶-01」
│   │     └── lane-cn   → 127.0.0.1:39103  → 内核 → DIRECT
│   ├── 后台任务：节点测速 / 订阅更新 / 账本滚动 / 内核看护
│   └── SQLite (WAL)
└── 子进程: mihomo (sidecar)
    ├── external-controller   127.0.0.1:39090  (随机 secret)
    └── listeners: lane-jp / lane-us / lane-cn 各自绑定一个 proxy 组
```

**两个进程，但只有一个"入口"**：客户端只认 `8787`；`39101+` 是进程内自己用的 loopback，不对外、不碰系统网络。

**端口分配器**：lane 端口从 `net.lane_port_range`（默认 39100–39299）分配，落库，启动时 bind 试探复用，对上层透明。

**配置生成**：每次从领域模型生成 `kernel.generated.yaml`（只读、带 `GENERATED` 头）。用户不直接编辑它。

**隔离性硬规则**（生成时强制写入）：

```yaml
mode: rule
mixed-port: 0            # 关闭默认端口
allow-lan: false
bind-address: 127.0.0.1
tun: { enable: false }   # 不接管系统流量
ipv6: false
profile: { store-selected: true, store-fake-ip: false }
```

App 全程**不调用**任何设置系统代理的 API、不写注册表 `ProxyServer`、不执行 `netsh`、不装服务、不开 53 端口、不添加路由。用 netstat 断言没有任何 `0.0.0.0` 监听。

**逃生舱**：内建栈暂未覆盖某个冷门协议时，可在**单条 lane** 上切到兼容模式（指向外部代理端口），其余 lane 不受影响。见 [04 §3](04-network-core.md)。

## 3. 请求管线（12 步）

入口协议：`POST /v1/chat/completions`、`POST /v1/responses`、`POST /v1/messages`、`POST /v1/embeddings`、`GET /v1/models`、`*` 通配（按直通规则处理）。

```
 1. Ingress      解析请求 → request_id；记录原始头快照（用于自检）
 2. Auth         校验本地密钥（可选，默认关闭）；限流（本机 RPM 保护）
 3. Normalize    只读少量字段用于路由：protocol / model / stream；body 不做强类型解析
 4. ModelResolve 逻辑模型名 → ModelEntry（别名表命中）→ 拿到候选通道组
 5. PolicyEval   规则表自上而下求值，首个命中生效 → 得到 候选集 + selector + failover 参数
 6. TagFilter    仅当策略显式要求时才按标签收窄候选（如排除 no-vision 通道）；默认不过滤
 7. Select       sticky 表命中 → 直接用；否则按 selector 选一个 → 写 sticky（含 TTL）
 8. EgressAdapt  只做定点改写：模型名、鉴权头、必需路径；body 其余字段原样
 9. Dispatch     取该 lane 的 client（绑定 127.0.0.1:3910x）→ KernelDialer 经内核出网 → 发送
10. Meter        解析 usage（OpenAI / Anthropic / Gemini 字段差异）→ 更新额度账本
11. Failover     失败且**尚未向下游写出任何字节**时，走下一个候选；否则只记录不可重试
12. Trace        落库：候选、淘汰原因、耗时、lane、出口节点、最终状态
```

### 第 11 步的重试边界（关键工程细节）

流式请求一旦写出第一个 SSE chunk，就**不允许重试**——否则客户端会收到重复内容或半截响应。因此：

- 重试只发生在「连接建立 / 首个响应头到达」窗口内（`send()` 返回前的错误、或返回了 4xx/5xx 状态码且 body 尚未转发）。
- 已开始流式输出后失败 → 记 `stream_broken`，写 trace，通知托盘短暂变色，不做第二次尝试。
- 非流式请求可全量重试（但也受 `max_attempts` 与请求体大小限制）。

## 4. 协议适配层

### 4.1 直通优先（passthrough-first）

这是解决"吞字段"的核心架构决策：

```
if 客户端协议 == 上游协议 (且未配置强制转换):
    RAW 通道：body 以 serde_json::Value 解析 → 只做白名单 patch（如改 model）→ 原样序列化
              未知字段、未知嵌套结构、数组元素内部未识别键，全部保留
else:
    IR 通道：canonical 中间表示 → 目标协议编码器 → 编码后做一次"字段核对"
```

**绝对禁止的写法**（很多中转站翻车的原因）：

```rust
// ❌ 强类型反序列化：未知字段直接蒸发，image_url 里新增的 detail/新格式全丢
#[derive(Deserialize)] struct Message { role: String, content: String }
```

**本项目的写法**：

```rust
// ✅ 只做定点 patch，其余字节级保真
let mut body: serde_json::Value = serde_json::from_slice(&raw)?;
if let Some(m) = body.get_mut("model") { *m = json!(upstream_model); }
// 其余字段一个都不动
```

### 4.2 canonical IR（仅在协议不一致时使用）

```rust
enum Part { Text, ImageUrl { url, detail }, ImageBase64 { media_type, data },
            File { media_type, data, name }, Audio, ToolCall, ToolResult, Reasoning }

struct CanonRequest { protocol, model, system, messages: Vec<Msg>, tools, tool_choice,
                      stream, temperature, max_tokens, extra: Map<String, Value> }
```

支持的协议对：`openai-chat`、`openai-responses`、`anthropic-messages`、`gemini-generateContent`。
每个方向的编码器都要过一遍**字段映射核对**（见 [03 §7.3](03-routing-and-policy.md)），无法映射的字段**默认直接拒绝该请求**，而不是转发一个残缺请求——**永不静默丢弃**。

### 4.3 能力标签（"视觉"问题的正面回答）

先分清两件事——上一版把这两件事混在一起，写成了一大套子系统：

| 问题 | 是"改请求"吗 | 手段 |
|---|---|---|
| 请求里的图片块 / 未知字段被中转层吃掉 | **是** | **全透传**（§4.1）。不需要任何能力机制 |
| 该把带图的请求**发给谁**（A 家识图、B 家不识图） | 否，是选目的地 | 一个可选的通道标签 |

所以本设计：**不做**能力探测、**不做**自动学习、**不做**内置能力库。

- Channel 上一个三态标签：`vision` / `no-vision` / **未标注（默认）**。
- 未标注 = 照发。默认行为永远是"发出去"，而不是"查不到能力就拒绝"。
- 用户哪天发现某家中转的 `gpt-4o` 不识图，在 UI 上点一下标成 `no-vision`；之后的带图请求会跳过它。
- 一条可选规则（默认关闭）：`match: { has_image: true }` + `exclude_tag: no-vision`。

不做探测，是因为探测本身会误判（上游返回 200 但把图片当纯文本忽略）；而"谁识图"这件事用户一句话就能定，比任何启发式都准。

## 5. 错误语义

对外统一错误信封，方便客户端识别：

```json
{ "error": { "type": "tokenagency_no_viable_channel",
             "message": "模型 gpt-4o 无可用通道：3 个候选均被策略排除",
             "request_id": "req_01H...",
             "trace_hint": "ta trace req_01H..." } }
```

| 场景 | HTTP | type |
|---|---|---|
| 模型不存在 | 404 | `model_not_found` |
| 候选被策略/标签全部排除 | 415 | `capability_unsatisfied` |
| 全部通道额度耗尽 | 402 | `all_channels_exhausted` |
| 全部通道失败 | 502 | `all_channels_failed` |
| 上游 429 且不可重试 | 429 | 透传上游语义 + `upstream_status` |

## 6. 架构决策记录（ADR）

| ID | 决策 | 备选 | 结论与理由 |
|---|---|---|---|
| ADR-01 | 网关与网络**同一个决策点**（选通道 = 同时选出口） | 网关 + 独立代理两套配置 | 采纳。这是 TokenHub "挂了梯子痛点照旧" 的根因——两层不知道彼此 |
| ADR-02 | 网络层交给 **mihomo sidecar**，网关经 lane 入站端口出网 | 进程内自己实现协议栈 | 采纳（评审后改）。**它让"完全兼容 Clash 订阅"从最贵需求变成免费**，并消掉协议栈长期维护这个最大风险；代价是多一跳 loopback（~0.1ms，可忽略）。见 [08 §0.2](08-tech-stack.md) |
| ADR-03 | **全透传**为默认且基本是全部答案，IR 仅作跨协议降级路径 | 全量走 IR + 能力矩阵 | 采纳。透传保住"客户端发出的东西"；能力标签只用于**选目的地**，不用于改请求 |
| ADR-04 | 默认**不接管系统网络** | 默认 TUN / 系统代理 | 采纳。"和电脑网络分开"是明确需求；也免去管理员权限与杀软误报 |
| ADR-05 | **SQLite 为配置真源，YAML 为导入导出**（评审后改） | YAML 为真源 | 采纳。`serde_yaml_ng` 不保留注释，GUI 频繁回写会抹掉用户注释；另提供可选的"文件监听模式"给想用 git 管配置的人。见 [08 §2.5](08-tech-stack.md) |
| ADR-06 | 路由状态**有粘性** | 每次无状态择优 | 采纳。prompt cache 命中率与额度连贯性都依赖粘性 |
| ADR-07 | 策略引擎用**有序规则表**而非评分函数 | 加权打分 | 采纳。"可解释 + 可模拟"对个人用户比"最优解"更重要 |
| ADR-08 | 单机单用户 local-first | 多租户 | 采纳。明确非目标，砍掉 80% 复杂度 |
| ADR-09 | 保留 `OutboundDialer` 抽象 | 直接绑死内核 | 采纳。留一条零依赖内建拨号的后路，且上层完全不感知 |
| ADR-10 | 请求体只用 `serde_json::Value` 做定点 patch | 强类型结构体 | 采纳（硬性代码规范）。大量中转"把视觉干成瞎子"就是强类型反序列化丢未知字段导致的 |
| ADR-11 | **v1 不做用户脚本引擎** | 引 Boa 做 JS 脚本增强 | 采纳（评审后定）。声明式 DSL + 正则已覆盖 95% 需求，且可校验/可模拟/可 diff；脚本是持续的安全与兼容负担。见 [08 §2.3](08-tech-stack.md) |
| ADR-12 | 不引 `tauri-plugin-mihomo`（它是 GPL git 仓库），自己写 kernel 管理模块 | 直接引插件 | 采纳。需要管的只有 5 件事，约 300–500 行；引它会把项目拖进 GPL-3.0 |
| ADR-13 | 前端不直连管理 API，全部走 Tauri IPC | 前端 HTTP 调 8788 | 采纳。类型安全（specta 生成）、不用管本地 token、不怕端口被占；8788 只给 CLI |

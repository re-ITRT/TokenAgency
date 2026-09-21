# 09 · 软件架构设计

> 本文是 TokenAgency 的**架构基线（Architecture Baseline）**：把前面 8 份文档的结论收敛成一份可施工、可评审、可验收的整体设计。
> 详细论证在各专题文档里，本文只给结论与结构，并在需要处指过去。

| | |
|---|---|
| 版本 | v1（设计冻结） |
| 读者 | 实现者（主要）、未来的自己、愿意给意见的人 |
| 上游文档 | [01 总体架构](01-architecture.md) · [02 配置与数据模型](02-config-and-data-model.md) · [03 路由与策略](03-routing-and-policy.md) · [04 网络](04-network-core.md) · [05 托盘与界面](05-ui-tray.md) · [06 里程碑与风险](06-roadmap-and-risks.md) · [07 页面与成本中心](07-ui-pages-and-cost.md) · [08 技术选型](08-tech-stack.md) |

---

# 1. 架构目标与约束

## 1.1 设计目标（按优先级）

| # | 目标 | 怎么体现 |
|---|---|---|
| G1 | **一个入口** | 客户端只认 `127.0.0.1:8787`，换套餐/换线路永不改客户端配置 |
| G2 | **不改变请求语义** | 全透传：除 4 处协议必需修改外零改写；跨协议无法映射时拒绝而非静默丢弃 |
| G3 | **不打扰这台电脑** | 不写系统代理、不改路由表、不装 TUN、不开 53、不劫持任何流量 |
| G4 | **自动且可解释** | 多套餐自动择优 + 粘性 + 额度切换；每个决策都能在 trace 里说出理由 |
| G5 | **单机单用户、local-first** | 无账号、无服务端、数据全在本机 |
| G6 | **可测** | 核心逻辑不依赖 Tauri，`cargo test` 秒级；直通保真用快照测试守住 |
| G7 | **可演进** | 网络实现、协议、内核都可替换而不动上层 |

## 1.2 三条硬不变量（写进 CI）

| 不变量 | 断言方式 |
|---|---|
| **I1 入口唯一** | 只有一个对外端口 8787；我们自己的进程所有监听都在 127.0.0.1（无 `0.0.0.0`） |
| **I2 系统网络零副作用** | 启停前后对系统代理 / WinHTTP / 路由表 / DNS / 虚拟网卡做快照 diff，**必须逐字段一致**（[04 §9](04-network-core.md)） |
| **I3 全透传** | golden 快照：含未知字段/未知 part/特殊头的请求，网关进出口字节级 diff（[06 A1](06-roadmap-and-risks.md)） |

工具：`scripts/check-invariants.ps1`（`-Mode Snapshot` / `Compare` / `Run`），退出码非 0 即违反。

### I2 的确切语义（容易搞错，写死）

**I2 是"前后一致"，不是"必须是关的"。**

用户机器上本来就可能开着系统代理（本机实测：`ProxyEnable=1`，`ProxyServer=127.0.0.1:7897`），也可能装着别的软件留下的 TUN/TAP 网卡。对这两种情况：

- ✅ 正确：完全不碰，启停前后一模一样
- ❌ 错误：打开它们（接管整机网络）
- ❌ **同样错误：替用户关掉它们**（擅自改动用户既有配置，也是副作用）

所以 I2 一律是**快照对比**，不做"代理必须为关"这类绝对断言。判据只有一条：**我们有没有改变它**。

> 这条是实测发现的：最初的检查脚本把"代理开着"报成违规，属于误判。已在 `scripts/check-invariants.ps1` 修正为"仅前后对比 + I1 绝对检查"。

### I2 的唯一例外：开机自启动

`system_integration.autostart` 需要**写操作系统状态**（Windows 注册表 `Run` 键 / macOS `LaunchAgent` / Linux XDG autostart）。这是全项目唯一一处，因此边界必须写死：

| # | 规则 | 为什么 |
|---|---|---|
| E1 | **显式 opt-in，默认关闭** | 凡是会写 OS 状态的开关一律默认关：`autostart` / `enable_tun` / `enable_system_proxy`。装完就静默加一个开机启动项，正是用户最反感的那种"副作用" |
| E2 | **完全可逆** | 关闭 = 删除条目；**卸载 = 删除条目**（NSIS 卸载钩子 / macOS 卸载脚本）。绝不留下孤儿项 |
| E3 | **绝不静默重新注册** | 用户若在任务管理器 / 系统设置里禁用了，我们**不偷偷加回来**，只在 UI 提示"配置里是开、系统里没有"。**用户的手动干预永远优先于我们的配置** |
| E4 | **OS 是事实真源，配置是期望状态** | UI 开关的状态读**系统实际状态**，不是配置值。两者漂移时显示出来并提供「重新注册 / 清除」，而不是假装一致 |
| E5 | **不变量检查里列为观察项** | `scripts/check-invariants.ps1` 记录 autostart 条目的存在与内容：允许它存在（那是用户开的），但**任何变化都必须能在日志里找到对应的用户操作**；我们主动新增时必须留下审计记录 |

> 这条规则可以一句话概括：**我们能改系统，但只在用户明确点头的那一件事上，而且随时能撤销、绝不背着他改回去。**

## 1.3 非目标

多用户 / 计费 / 充值 / 对外提供服务 / 公网中转 / 模型训练 / 默认接管整机网络。
（完整清单见 [README §4](../README.md)）

## 1.4 关键约束

| 约束 | 影响 |
|---|---|
| 内核 mihomo 是 **GPL-3.0** | 默认 `net.kernel.provider: external`，不分发内核；不引 `tauri-plugin-mihomo`；不拷 Clash Verge Rev 代码 |
| 内核是**外部进程** | 需要就绪探测、崩溃看护、配置生成、端口分配、孤儿进程防护 |
| 配置要能被人读懂 | SQLite 为真源，但必须有一键导出可 git 的 YAML |
| 用户是开发者、会用深色模式、看数字 | UI 密度优先、深色优先、可下钻、可导出 CSV |

---

# 2. 系统上下文（C4 L1）

```
                    ┌──────────────────────────────────────┐
   人 ────────────▶ │           TokenAgency                 │
 （同一个开发者）     │  托盘 + 主窗口 + 本地网关              │
                    └───┬──────────┬──────────┬────────────┘
                        │          │          │
        ┌───────────────┘          │          └────────────────┐
        ▼                          ▼                           ▼
 ┌─────────────┐          ┌─────────────────┐          ┌──────────────────┐
 │ LLM 客户端   │   HTTP   │ 上游 LLM 服务商   │  HTTPS   │ 机场订阅服务器     │
 │ Cursor /    │─────────▶│ 官方 / 自建中转   │◀─────────│ (Clash 格式)      │
 │ Claude Code │ 8787     │ OpenAI/Anthropic │  经 lane  │                  │
 │ SDK / 脚本   │          │ Gemini / 中转站   │          └──────────────────┘
 └─────────────┘          └─────────────────┘                   ▲
        ▲                          ▲                            │ HTTPS
        │                          │ HTTP CONNECT               │ (独立 Client,
        └──────────────────────────┴────────────────────────────┘  不走 lane)
                            经内核 lane 出网
```

**四个外部关系**：

| 关系 | 协议 | 关键约束 |
|---|---|---|
| 客户端 → TokenAgency | HTTP（OpenAI / Anthropic 兼容） | 只绑 127.0.0.1；可选本地密钥 |
| TokenAgency → 上游 | HTTPS，经内核 lane | 每 lane 一个连接池；全透传 |
| TokenAgency → 机场订阅 | HTTPS，**独立 Client，不走 lane** | ★ 鸡生蛋：必须先拿到订阅才知道有哪些节点 |
| TokenAgency → 内核 | sidecar 进程 + REST(39090) + lane loopback(3910x) | 全部 loopback；内核自身不监听 0.0.0.0 |

---

# 3. 容器视图（C4 L2）

## 3.1 进程与端口

```
TokenAgency.exe  (Tauri 主进程)  ── 唯一"入口"
├── tokio runtime
│   ├── 网关 HTTP        127.0.0.1:8787   ← 客户端唯一入口
│   ├── 管理 API         127.0.0.1:8788   ← 仅供 CLI/脚本；GUI 走 Tauri IPC
│   ├── KernelDialer     每个 lane 一个 reqwest Client，绑定 3910x
│   ├── 后台任务         节点测速 / 订阅更新 / 账本滚动 / 清理 / 内核看护
│   └── SQLite (WAL)
└── 子进程: mihomo
    ├── external-controller 127.0.0.1:39090 (随机 secret)
    └── listeners: lane-jp / lane-us / lane-cn … 各绑定一个 proxy 组
```

| 端口 | 归属 | 对外 | 说明 |
|---|---|---|---|
| 8787 | 网关 | 本机客户端 | 唯一入口 |
| 8788 | 管理 API | 本机 CLI | GUI 不用它 |
| 39090 | 内核 REST | 进程内 | 随机 secret 鉴权 |
| 39100–39299 | lane 入站 | 进程内 | 每条 lane 一个，动态分配并落库 |

**为什么 lane 必须是独立入站**：内核支持 `listeners[].proxy`——把某个入站的所有流量固定交给某个代理组。这正是"每个模型走不同线路"的机制（[04 §7.1](04-network-core.md)）。

## 3.2 数据存储

| 存储 | 内容 | 真源？ |
|---|---|---|
| SQLite `db.sqlite` | 配置实体 + 账本 + trace + 粘性 + 健康样本 | ✅ 配置与运行态真源 |
| OS Keychain | 所有 API key | ✅ 密钥唯一真源 |
| `<AppData>/kernel/kernel.generated.yaml` | 生成的内核配置 | ❌ 生成物，可随时重建 |
| `<AppData>/subscriptions/*.yaml` | 订阅原文缓存（压缩） | ❌ 缓存，可重新下载 |
| `<AppData>/logs/` | 应用日志、内核 stdout | ❌ |

> 配置真源从 YAML 改为 SQLite 的原因见 [08 §2.5](08-tech-stack.md)（`serde_yaml_ng` 不保留注释）。YAML 仍然是一等公民：导入导出、分享、git 快照、以及可选的"文件监听模式"。

## 3.3 组件分层与依赖方向

```
        ┌──────────────────────────────────────────────┐
        │  src-tauri   （薄：命令层 / 托盘 / 窗口 / 插件） │
        └───────┬──────────────┬───────────────┬───────┘
                ▼              ▼               ▼
        ┌────────────┐  ┌────────────┐  ┌──────────────┐
        │ ta-gateway │  │ ta-kernel  │  │ ta-subscription│
        └──┬──┬──┬───┘  └─────┬──────┘  └───────┬──────┘
           │  │  │            │                 │
           │  │  └────────────┴─────────────────┘
           ▼  ▼                 ▼
      ┌──────────┐        ┌──────────┐      ┌───────────┐
      │ ta-store │        │ ta-core  │◀─────│ ta-secrets│
      └────┬─────┘        └────┬─────┘      └───────────┘
           └────────┬──────────┘
                    ▼
              （无 Tauri 依赖）
```

**依赖规则（禁止环）**：

1. `ta-core` 依赖**最少**——纯领域：模型、配置 schema、策略引擎、token 归一化、成本计算。不做 IO（除了 `tracing`）。
2. `ta-store` → `ta-core`：SQLite、迁移、聚合查询。
3. `ta-kernel` → `ta-core`：进程管理、配置生成、REST client、lane 映射。**不碰网关**。
4. `ta-subscription` → `ta-core` + `ta-kernel`：下载、嗅探、缓存、回滚。
5. `ta-gateway` → `ta-core` + `ta-store` + `ta-kernel` + `ta-secrets`：网关本体。
6. `src-tauri` → 全部。**它只做转发与呈现，不写业务逻辑**（硬规定：命令函数体超过 20 行就该下沉到 crate）。

**收益**：`cargo test -p ta-core -p ta-gateway` 秒级；`ta serve` 无 GUI 模式直接复用 `ta-gateway`；将来 headless 化几乎零成本。

## 3.4 前端容器

```
React 19 + TS ──Tauri IPC(invoke/emit)──▶ src-tauri ──▶ crates/*
     │
     ├── TanStack Query   服务端数据（Provider/模型/日志分页/成本查询）
     ├── Zustand          UI 状态（当前页、筛选、抽屉、主题、表单草稿）
     └── Zustand(环形)     实时请求流（保留最近 500 条）
```

**硬规定：前端不直连 8788**。理由：类型安全（specta 生成）、不用管本地 token、不怕端口被占。

---

# 4. 组件职责（C4 L3）

## 4.1 ta-core（纯领域，最重要）

| 模块 | 职责 |
|---|---|
| `model/` | Provider / Credential / Lane / Model / Channel / ChannelGroup / Policy 的领域类型与校验 |
| `config/` | 配置 schema（`schemars` 导出 JSON Schema）、引用完整性校验、版本与迁移 |
| `routing/` | 策略求值、候选集计算、选择器（priority / sticky_then_priority / quota_first / cheapest / lowest_latency / weighted_random / round_robin） |
| `sticky/` | 粘性 key 派生、命中、释放 |
| `quota/` | 四观测源合成、状态机（ok/soft/exhausted）、窗口滚动 |
| `pricing/` | token 类型归一化、费率匹配、成本计算、价格快照 |
| `passthrough/` | 定点 patch 规则、头部例外清单、跨协议字段核对 |
| `lane/` | 五级线路解析（policy → channel → credential → provider → default → direct） |

## 4.2 ta-gateway

| 模块 | 职责 |
|---|---|
| `ingress/` | axum 路由、请求 ID、本地密钥、限流、hop-by-hop 剥离、原始头快照 |
| `pipeline/` | 12 步请求管线编排（见 §5.1） |
| `adapt/` | 协议识别、直通（RAW）路径、跨协议 IR 路径 |
| `dispatch/` | 取 lane 的 client、发送、SSE 零缓冲转发、下游断连取消 |
| `failover/` | 尝试链、重试边界、冷却退避 |
| `meter/` | usage 解析（三家字段差异）、写账本、日汇总 |
| `trace/` | 落 trace、`header_diff` 自检、事件广播 |
| `admin/` | 8788 管理 API（只给 CLI） |

## 4.3 ta-kernel

| 模块 | 职责 |
|---|---|
| `process/` | sidecar 生命周期、就绪探测、崩溃指数退避重启、孤儿防护（Windows Job Object） |
| `config_gen/` | 领域模型 → `kernel.generated.yaml`；强制隔离项；lane → `listeners` |
| `rest/` | 切节点、更新订阅、测速、重载配置 |
| `ports/` | lane 端口分配（bind 试探 + 落库 + 复用） |
| `health/` | 读内核测速结果 → lane 健康状态 |

## 4.4 ta-store / ta-subscription / ta-secrets

| Crate | 职责 |
|---|---|
| `ta-store` | sqlx 连接池、迁移、账本写入与聚合、trace 查询、清理任务 |
| `ta-subscription` | 下载（独立 Client）、格式嗅探、UA 处理、node/group 计数比对、版本保留与回滚、`userinfo` 解析 |
| `ta-secrets` | `keyring` 封装、`${secret:...}` / `${env:...}` 解析、明文迁移提示 |

---

# 5. 运行时视图

## 5.1 请求管线（12 步，定稿）

```
 1. Ingress      解析 → request_id；剥离 hop-by-hop；**鉴权/限流通过后才**记录原始头/体快照
 2. Auth         本地密钥（可选）+ RPM 限流
 3. Hints        只读少量字段用于路由：model / stream / has_image / 估算 tokens
 4. ModelResolve 逻辑模型名 → ModelEntry（别名表）→ 候选通道组
 5. PolicyEval   规则表自上而下，首个命中生效 → 候选集 + selector + failover
 6. TagFilter    仅当策略显式要求时才按标签收窄（如排除 no-vision）；默认不过滤
 7. Select       sticky 命中即用；否则按 selector 选 → 写 sticky
 8. EgressAdapt  定点改写：模型名、鉴权头、必需路径；body 其余字段原样
                 ★ 选 lane 在这一步之内 —— **切换套餐 = 切换 lane**
 9. Dispatch     取该 lane 的 client（127.0.0.1:3910x）→ 发送 → SSE 零缓冲转发
10. Meter        解析 usage → 写账本 + 日汇总 + 额度状态
11. Failover     未向下游写出任何字节时才重试；否则记 stream_broken
12. Trace        落库：候选、淘汰原因、耗时瀑布、lane/节点、header_diff、状态
```

> **修订（第 9 轮，主管裁决）**：步骤 1 原本把"记录原始头/体快照"和"剥离 hop-by-hop"并列在第一句，
> 读起来像是**读 body 快照发生在鉴权之前**。员工 E5 实现 G1 时**刻意偏离**了这个顺序（先鉴权/限流，
> 再读 body），理由是：让**未授权**请求把 `max_body_mb`（默认上限很大）读进内存，是一个真实的
> DoS 面。对合法请求而言两种顺序**不可观测**（读的是同样的字节），所以这是**修正一个写错的文档**，
> 不是偏离契约。**已按实现修订本文。**
>
> **步骤 8 的补充**：lane 解析顺序为
> `policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`
> —— **lane 是在通道选定之后才决定的**，所以"换套餐"必然"换线路"（[03 §0] 的基线场景正是靠这条成立的）。

## 5.2 时序：带图的流式请求（正常路径）

```
Client        Gateway              Kernel            Upstream
  │ POST /v1/chat/completions (image_url ×2)
  ├──────────▶│
  │           │ Hints: model=gpt-4o, has_image=true, stream=true
  │           │ Policy: pol-skip-nonvision 未启用 → 不过滤
  │           │ sticky(key=8f3a) hit → ch-relay-a → lane-jp
  │           │ 定点 patch: model → "gpt-4o"（不变）；auth 换成 relayA 的 key
  │           │ ┌── HTTP CONNECT api.example-relay.com:443
  │           ├─┤──────────────────▶│ ── VLESS/TLS ──▶ 东京-03 ──▶ 上游
  │           │ └───────────────────┤
  │           │      200 + SSE      │◀────────────────────────────────
  │ ◀─────────┤ 逐块转发（零缓冲，不聚合不改行）
  │           │ usage 解析 → 账本（input 1204 / input_cached 51200 / output 386）
  │           │ trace: header_diff={} , 图片块 2 个原样转发
```

## 5.3 时序：套餐额度用尽 → 自动切换（本产品的核心价值）

```
请求 N      ch-relay-a → 上游 429 insufficient_quota
            ├─ 账本：c-relay-a → exhausted（记录 reset 提示）
            ├─ 候选集剔除 ch-relay-a
            ├─ 粘性表 release_on=quota_exhausted → 重选
            └─ 重试 → ch-openai-personal → 200
               粘性重写为 ch-openai-personal
请求 N+1    同 session → 粘性命中 ch-openai-personal（不再回 A）
…
窗口滚动    检测到 reset 或收到 remaining 回升 → c-relay-a 复活 → 下次重选可再参与
```

## 5.4 时序：启动与内核就绪

```
T0  Tauri 启动 → 窗口渲染骨架屏（不等内核）
T1  读 DB 配置 → 分配 lane 端口（bind 试探）→ 生成 kernel.generated.yaml
T2  拉起 mihomo sidecar（绑定 Job Object）
T3  轮询 REST /version（≤5s）→ **且每个我们生成的 provider 的 proxies[] 非空** → 就绪
    （⚠️ 只问 /version 会被骗：provider 加载失败时内核照常启动、REST 照常通，
      而所有 lane 静默降级成直连。见 [06 §1.6] 的「就绪判定会被骗」）
T4  为每条 lane 建 reqwest Client（绑定 3910x）
T5  网关 listen 8787 → emit("kernel:status", ready) → 托盘转绿
    ↑ 若 T3 失败：指数退避重启（1s→30s，最多 5 次）
      失败期间：direct lane 通道仍可用；其余通道标记降级
```

## 5.5 并发模型与背压

| 点 | 设计 |
|---|---|
| 网关 | axum/hyper 每连接一个 task；无全局锁 |
| 配置读取 | `arc-swap` 无锁热更新，请求路径不阻塞 |
| 账本写入 | 批量合并 + 单写线程（SQLite 单写者），避免写竞争 |
| 事件广播 | `tokio::sync::broadcast`，**环形缓冲 + 200ms 批量 emit**，防打爆 WebView |
| 下游断连 | 取消原语 → 及时取消上游请求（省钱、省连接）。**实现用 `tokio::sync::Notify`**（等价于 `CancellationToken`，不引入 `tokio-util` 的 `sync` feature —— 见 [08 §栈表]） |
| 并发保护 | 每凭据令牌桶（RPM）+ 信号量（并发），超限排队或换下一个通道 |
| 背压 | SSE 转发用 `Body::from_stream`，不缓冲全量响应 |

---

# 6. 数据视图

## 6.1 三类数据，三种命运

| 类别 | 存放 | 保留 | 丢失后果 |
|---|---|---|---|
| **配置** | SQLite `config` 表 | 永久 + YAML 快照 | 要重配（严重） |
| **运行态** | SQLite（账本/粘性/额度/健康） | 账本永久；其余滚动 | 粘性丢 → 缓存命中下降；账本丢 → 成本统计断档 |
| **日志/trace** | SQLite（可压缩 body） | 按 `request_log.retention`，默认 30d | 可接受，但排查能力下降 |
| 密钥 | OS Keychain | 永久 | 要重填 |

## 6.2 表清单

```
config              -- 一行一实体（JSON），配置真源
usage_request       -- 每次请求一行：token_kinds(JSON) + usage_raw + price_snapshot + cost
usage_daily         -- 日汇总：token_kind 作为【行】维度（新增类型无需改表）
api_key_usage       -- 按 key 维度汇总
quota_state         -- 每凭据的额度合成状态
sticky              -- 会话粘性
trace               -- 请求尝试链、耗时瀑布、header_diff
health_sample       -- lane/节点健康样本
secret_meta         -- key 元数据（值在钥匙串）
```

设计要点：**token 类型是开放集合**（`token_kinds` JSON + `usage_daily.token_kind` 行维度），上游明天加新计费档不用改 schema。

## 6.3 写入路径（一次请求）

```
响应返回
  → usage 解析（先减掉 cached，避免重复计费统计）
  → pricing：匹配费率 → cost = Σ tokens[k] × rate[k] × multiplier / divisor
  → 事务写：usage_request（含 price_snapshot）+ upsert usage_daily + upsert quota_state
  → trace 写（含 header_diff）
  → broadcast("request:new", 摘要)
```

**价格快照**是硬要求：改价不得追溯改写历史成本。

## 6.4 清理

每小时：trace 按 retention 过期 → 再按 `max_rows` / `max_size_mb` 从最老删 → `PRAGMA incremental_vacuum`（不做全量 VACUUM）。账本永久保留（体积小且是对账依据）。

---

# 7. 关键机制（实现提要）

| 机制 | 一句话 | 细节 |
|---|---|---|
| **路由决策** | 有序规则表，首个命中即生效；产出候选集 + 选择器 + 失败链 | [03 §2](03-routing-and-policy.md) |
| **粘性** | key 派生优先 `x-session-id` → `x-conversation-id` → `metadata.user_id` → 首条 system+user 哈希；释放条件 quota_exhausted / hard_error / cooldown / ttl | [03 §3](03-routing-and-policy.md) |
| **额度账本** | 四观测源取最保守：响应头 / 本地计量 / 手工额度 / 429·402 语义 | [03 §4](03-routing-and-policy.md) |
| **故障转移** | **已向下游写出字节就绝不重试**；重试前须确认未产生上游计费 | [03 §5](03-routing-and-policy.md) |
| **全透传** | 只改 `host`/`content-length`/鉴权头/hop-by-hop，其余一个字节不动；禁止强类型反序列化 | [03 §7](03-routing-and-policy.md) |
| **线路解析** | `policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? default ?? direct` | [04 §8.1](04-network-core.md) |
| **lane → 内核** | 每条 lane 生成一个 `listeners` 入站并绑定 `proxy` 组 | [04 §7.1](04-network-core.md) |
| **成本计算** | `Σ tokens[k] × rate[k] × multiplier / divisor`，结果带价格快照 | [07 §6](07-ui-pages-and-cost.md) |
| **视觉标签** | 三态标签，未标注=照发；能力不参与请求改写，只影响"发给谁" | [03 §6](03-routing-and-policy.md) |

---

# 8. 横切关注点

## 8.1 错误处理

统一信封 + 明确的 `type`，让客户端能识别是网关的问题还是上游的问题：

```json
{ "error": { "type": "tokenagency_no_viable_channel", "message": "...",
             "request_id": "req_...", "trace_hint": "ta trace req_..." } }
```

`ta-core` / `ta-gateway` 内用 `thiserror` 定类型错误；边界（Tauri 命令、CLI）用 `anyhow` 收敛。

## 8.2 日志与追踪

- `tracing` + `tracing-subscriber` + `tracing-appender` 落文件；与 `tauri-plugin-log` 打通
- 日志级别可热改；`request_log.mode` 控制 **请求内容** 的记录（与运行日志是两回事）
- 每个请求一条 `request_id`，贯穿网关 → trace → UI

## 8.3 安全

| 面 | 措施 |
|---|---|
| 本地端口 | 全部 `127.0.0.1`；网关可选本地密钥；管理 API 独立 token |
| 密钥 | 只存钥匙串；日志与 trace **永不记录** `authorization` / `x-api-key` / `cookie` |
| 请求体 | 图片只存 `{type, media_type, bytes, sha256前8位}`，**绝不存 base64** |
| 脚本 | v1 无脚本引擎（ADR-11）；未来若引入，必须无 host 能力 + 指令配额 + 超时 |
| 内核 REST | 随机 secret，仅 loopback |

## 8.4 配置与热更新

写 DB（事务）→ 校验（JSON Schema + 语义）→ 应用 → `emit("config-changed")`。
请求路径读配置走 `arc-swap`，**热更新不阻塞在途请求**。

## 8.5 国际化与可访问性

`react-i18next`（中/英）；深色优先、浅色同构；托盘图标双套（Windows 任务栏主题）；键盘可达。

## 8.6 可观测性（自检优先）

| 手段 | 用途 |
|---|---|
| `trace.header_diff` | **非空即说明网关有 bug**（自检，不是策略） |
| 耗时瀑布 | 区分"网络慢"还是"模型慢" |
| `ta echo <channel>` | 回显"上游实际收到了什么"——回答"谁吞了我的图片/标识" |
| `capability_probe` | ❌ 已删除（不做探测） |

---

# 9. 质量属性与验收场景

## 9.1 性能预算

| 指标 | 目标 |
|---|---|
| 网关自身开销 | < 5ms P50 / < 20ms P99 |
| SSE 首字节额外延迟 | < 10ms |
| 冷启动到窗口可见 | < 1.5s（不等内核） |
| 内核就绪 | < 1s |
| 常驻内存（含内核） | < 150MB |
| 安装包（Windows） | < 40MB |
| 前端首屏 JS | < 400KB gzip |
| 看板查询（10 年数据） | < 300ms（靠 `usage_daily`） |

## 9.2 关键质量场景

| 场景 | 期望 | 对应验收 |
|---|---|---|
| 带图请求遇到不识图的通道 | 跳过它；若全被排除则明确报错，**绝不静默文本化** | A2 |
| 中转返回 429 额度用尽 | 自动切下一个套餐，同会话后续保持新通道 | A4 |
| 流式已输出 3 个 chunk 后上游断连 | **绝不重试**（只记 stream_broken） | A5 |
| 订阅 URL 变成 HTML 错误页 | 旧节点仍在，UI 标红说明原因 | A9 |
| 内核崩溃 | 退避重启，期间直连通道仍可用 | A10 |
| 在用的节点变黑洞 | 10s 内切到下一节点，客户端无感 | A10 |
| 关闭 App | 系统代理/路由/DNS/进程 全部无残留 | A6+A7 |

（完整 10 条见 [06 §6](06-roadmap-and-risks.md)）

## 9.3 可测试性

| 层 | 手段 |
|---|---|
| 领域逻辑 | `ta-core` 纯函数，单测覆盖策略/额度/成本 |
| **保真** | `insta` 快照守 I3 —— 这是防止"越改越丢字段"的唯一可靠手段 |
| 上游打桩 | `wiremock` 模拟 429/402/5xx/超时/流式中断 |
| 订阅 | 真实样本（脱敏）作 fixture，断言节点数与分组数 |
| 前端 | Vitest + RTL |
| E2E | `tauri-driver` 冒烟 |
| 隔离性 | 快照 diff 脚本进 CI（I2） |

## 9.4 可演进性

| 想换的东西 | 需要改哪里 |
|---|---|
| 内核（换 sing-box 等） | 只改 `ta-kernel`；lane 抽象不变 |
| 网络实现（内建拨号） | 只加 `BuiltinDialer`，改一个配置值 |
| 上游协议（新增一家） | 加一个协议适配器 |
| 定价来源 | 加一个 pricing entry 类型 |
| 前端 UI | 只动 `src/`，Rust 契约由 specta 保证 |

---

# 10. ADR 索引

| ID | 决策 | 详见 |
|---|---|---|
| ADR-01 | 网关与网络同一决策点（选通道=同时选出口） | [01](01-architecture.md) |
| ADR-02 | 网络交给 mihomo sidecar，网关经 lane 入站出网 | [08 §0.2](08-tech-stack.md) |
| ADR-03 | 全透传为默认；能力标签只用于选目的地 | [03 §7](03-routing-and-policy.md) |
| ADR-04 | 默认不接管系统网络 | [04 §9](04-network-core.md) |
| ADR-05 | **SQLite 为配置真源，YAML 为导入导出** | [08 §2.5](08-tech-stack.md) |
| ADR-06 | 路由有粘性 | [03 §3](03-routing-and-policy.md) |
| ADR-07 | 有序规则表而非评分函数 | [02 §2.8](02-config-and-data-model.md) |
| ADR-08 | 单机单用户 local-first | [README §4](../README.md) |
| ADR-09 | 保留 `OutboundDialer` 抽象 | [04 §3](04-network-core.md) |
| ADR-10 | 请求体只用 `serde_json::Value` 定点 patch | [03 §7.2](03-routing-and-policy.md) |
| ADR-11 | v1 不做用户脚本引擎 | [08 §2.3](08-tech-stack.md) |
| ADR-12 | 不引 `tauri-plugin-mihomo`，自写 kernel 模块 | [08 §0.1](08-tech-stack.md) |
| ADR-13 | 前端不直连管理 API，全部走 IPC | [08 §3](08-tech-stack.md) |
| ADR-14 | Rust workspace 拆分，核心不依赖 Tauri | [08 §2.7](08-tech-stack.md) |

---

# 11. 演进路线（架构视角）

| 阶段 | 架构新增 | 架构不变的部分 |
|---|---|---|
| **M0** 技术验证 | 内核 sidecar 骨架 + 一个 lane 打通 + 保真快照 | — |
| **M1** MVP | `ta-core`/`ta-gateway`/`ta-store` 立起来；单协议直通；SQLite + specta | 三条不变量 |
| **M2** 网络完整 | `ta-kernel` 完整；`ta-subscription`；五级线路解析 | 请求管线 12 步 |
| **M3** 策略与额度 | 策略引擎、粘性、账本、成本中心 | 管线步骤 5–7、10–11 |
| **M4** 多协议与打磨 | IR 路径 + 跨协议核对；通知；安装包 | 直通优先原则 |
| **M5** 可选 | 内建拨号（`BuiltinDialer`）、headless `ta serve`、TUN/系统代理高级模式 | `OutboundDialer` 抽象 |

**架构的稳定性目标**：M1 定下的三条不变量、12 步管线、crate 边界，在 M2–M5 **不应改变**。会变的只有各 crate 内部的实现密度。

---

# 12. 附录：术语表

| 术语 | 含义 |
|---|---|
| **Provider / 服务商** | 上游是谁：地址、协议、鉴权方式 |
| **套餐 / Credential** | 一份 API 额度：一个 key + 额度 + 重置周期。**用户口中的"某某订阅"通常指它**（如"字节的订阅""command code 的订阅"） |
| **机场订阅 / ProxySubscription** | 一堆**节点**的来源（Clash 格式）。**与"套餐"是两回事** —— 早期文档两者都叫"订阅"，是最容易读错的地方 |
| **Lane / 线路** | 一条出网路径：直连、机场订阅里的某个组、固定节点、自动择优、正则筛选。**出网方式挂在"套餐"上**（`credential.lane`），所以切套餐 = 切线路 |
| **Model / 逻辑模型** | 对外的模型名（如 `gpt-4o`），可带别名 |
| **Channel / 通道** | Provider + Credential + Lane + 模型名映射 = 一条能用的路 |
| **ChannelGroup / 通道组** | 同一逻辑模型的多个候选 + 选择器 + 粘性策略 |
| **Policy / 策略** | 有序规则：match → action |
| **粘性 / Sticky** | 同一会话固定走同一凭据（保 prompt cache 命中、保额度连贯） |
| **额度账本** | 由四个观测源合成的"还剩多少" |
| **lane 端口** | 内核里为某条 lane 开的 loopback 入站端口（3910x） |
| **全透传** | 除 4 处协议必需修改外，客户端请求原样到达上游 |
| **kernel.generated.yaml** | 由领域模型生成的内核配置，只读、可重建 |

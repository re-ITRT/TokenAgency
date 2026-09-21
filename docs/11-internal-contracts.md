# 11 · 接口文档（二）：内部契约

> **自顶向下第 2 层。** 对外契约见 [10](10-interface-spec.md)。本文定义 crate 之间、以及与内核/存储/上游之间的契约。
> 这些 trait 是**分派子 agent 的依据**：每个包只依赖它需要的 trait，不直接依赖别人的具体类型。

---

# 1. L3 · Rust crate 之间的 trait 契约

## 1.1 通用约定

```rust
// 全项目共用
pub type Result<T, E = TaError> = std::result::Result<T, E>;
pub trait Clock: Send + Sync { fn now_ms(&self) -> i64; }   // 注入式时钟，便于测试
```

- 所有跨 crate 的错误统一为 `TaError`（见 §5）。
- 所有 trait 都是 `Send + Sync`；异步方法用 `#[async_trait]`。
- **禁止跨 crate 直接用别人的具体结构体做参数**（除 `ta-core` 的领域类型）。

## 1.2 ta-core → 所有层

```rust
pub mod domain { pub struct Provider; pub struct Credential; pub struct Lane; pub struct Model;
                 pub struct Channel; pub struct ChannelGroup; pub struct Policy; pub struct Subscription; }

pub trait ConfigValidator {
    fn validate(&self, doc: &ConfigDoc) -> ValidationReport;   // JSON Schema + 引用完整性
}

pub trait PolicyEngine: Send + Sync {
    fn decide(&self, input: &RouteInput, ctx: &RouteContext) -> RouteDecision;
}

pub trait ChannelSelector: Send + Sync {
    fn select(&self, decision: &RouteDecision, ctx: &RouteContext) -> Selection;
}

pub trait StickyResolver: Send + Sync {
    fn key_for(&self, input: &RouteInput) -> Option<StickyKey>;
    fn lookup(&self, key: &StickyKey, group: &ChannelGroupId) -> Option<ChannelId>;
    fn store(&self, key: StickyKey, group: ChannelGroupId, channel: ChannelId, ttl_ms: i64);
    fn release(&self, key: &StickyKey, why: ReleaseReason);
}

pub trait QuotaLedger: Send + Sync {
    fn state(&self, cred: &CredentialId) -> QuotaState;
    fn observe(&self, cred: &CredentialId, obs: QuotaObservation);   // 四观测源之一
    fn mark_exhausted(&self, cred: &CredentialId, hint: Option<i64>);
    fn roll_window(&self, now_ms: i64) -> Vec<CredentialId>;         // 返回复活的
}

pub trait PricingEngine: Send + Sync {
    fn resolve(&self, model: &str, provider: &ProviderId, cred: &CredentialId) -> ResolvedPrice;
    fn cost(&self, usage: &TokenUsage, price: &ResolvedPrice) -> Money;
}

pub trait TokenMeter: Send + Sync {
    /// 把任意上游的原始 usage 归一化；**cached 必须先从 input 总数里减掉**
    fn normalize(&self, protocol: Protocol, raw: &serde_json::Value) -> TokenUsage;
    fn estimate(&self, body: &serde_json::Value) -> u64;
}
```

**关键数据契约**：

```rust
/// token 类型是开放集合：新增类型不改 schema
pub enum TokenKind {
    Input, InputCached, InputCacheWrite, InputCacheWrite1h, InputAudio, InputImage,
    Output, OutputReasoning, OutputAudio, OutputPredictionAccepted, OutputPredictionRejected,
    Other(String),
}
pub struct TokenUsage(pub BTreeMap<TokenKind, u64>);   // 未出现的类型 = 0

pub struct ResolvedPrice {
    pub rates: BTreeMap<TokenKind, f64>,   // 每 1M tokens 的价格
    pub multiplier: f64,                    // 中转站倍率，默认 1.0
    pub currency: String,
    pub source: PriceSource,                // Manual | Imported | Builtin | None
    pub plan_fee: Option<PlanFee>,          // 固定套餐费（不摊进单次请求）
}

pub struct RouteDecision {
    pub candidates: Vec<ChannelId>,
    pub selector: Selector,
    pub failover: FailoverPolicy,
    pub on_no_candidate: NoCandidateAction,
    pub matched_policy: Option<PolicyId>,
    pub eliminated: Vec<(ChannelId, EliminateReason)>,
}
```

## 1.3 ta-store

```rust
#[async_trait] pub trait ConfigStore: Send + Sync {
    async fn load(&self) -> Result<ConfigDoc>;
    async fn save(&self, doc: &ConfigDoc) -> Result<()>;          // 事务
    async fn snapshot(&self, doc: &ConfigDoc, label: &str) -> Result<SnapshotId>;
    async fn list_snapshots(&self, limit: u32) -> Result<Vec<SnapshotMeta>>;
    async fn rollback(&self, id: &SnapshotId) -> Result<ConfigDoc>;
}

#[async_trait] pub trait UsageStore: Send + Sync {
    async fn record(&self, rec: UsageRecord) -> Result<()>;        // 含 usage_request + usage_daily + quota_state（一个事务）
    async fn query(&self, q: &UsageQuery) -> Result<UsageTable>;
    async fn dashboard(&self, range: TimeRange) -> Result<DashboardData>;
    async fn token_kinds_seen(&self) -> Result<Vec<TokenKind>>;
    async fn unpriced_models(&self) -> Result<Vec<String>>;
    async fn export_csv(&self, q: &UsageQuery, w: &mut dyn Write) -> Result<()>;
}

#[async_trait] pub trait TraceStore: Send + Sync {
    async fn write(&self, t: TraceRecord) -> Result<()>;
    async fn query(&self, q: &LogQuery) -> Result<LogPage>;
    async fn get(&self, request_id: &str) -> Result<Option<TraceDetail>>;
    async fn set_mode(&self, mode: LogMode) -> Result<()>;
    async fn set_retention(&self, r: Retention) -> Result<()>;
    async fn storage_stats(&self) -> Result<StorageStats>;
    async fn clear(&self, mode: ClearMode) -> Result<u64>;
    async fn gc(&self, now_ms: i64) -> Result<u64>;                // 每小时调用
}

#[async_trait] pub trait StickyStore: Send + Sync { /* get/put/release/gc */ }
#[async_trait] pub trait HealthStore: Send + Sync { /* append/series/latest */ }
#[async_trait] pub trait SecretMetaStore: Send + Sync { /* upsert/list/touch */ }
```

## 1.4 ta-secrets

```rust
#[async_trait] pub trait SecretStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<String>>;
    async fn set(&self, name: &str, value: &str) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
    async fn list_names(&self) -> Result<Vec<String>>;
}

/// 解析 "${secret:xxx}" / "${env:VAR}"；明文则返回 Err(PlaintextSecret)
pub fn resolve_ref(spec: &str, store: &dyn SecretStore) -> Result<SecretRef>;
```

## 1.5 ta-kernel

```rust
#[async_trait] pub trait KernelController: Send + Sync {
    async fn status(&self) -> KernelStatus;
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn restart(&self) -> Result<()>;
    async fn apply(&self, cfg: &KernelConfig) -> Result<ApplyOutcome>;   // 生成+落盘+reload
    async fn set_node(&self, group: &str, node: &str) -> Result<()>;      // PUT /proxies/{g}
    async fn set_select(&self, lane: &LaneId, node: &str) -> Result<()>;
    async fn refresh_provider(&self, name: &str) -> Result<()>;
    async fn delay_of(&self, name: &str, timeout_ms: u64) -> Result<u32>;
    async fn snapshot(&self) -> Result<ProvidersSnapshot>;   // 节点+分组列表，供 UI 与线路映射
    async fn log_tail(&self, lines: u32) -> Result<String>;
}

pub trait KernelConfigGen: Send + Sync {
    /// 领域模型 → 生成的 YAML（强制写死隔离项）
    fn generate(&self, doc: &ConfigDoc, ports: &PortMap) -> Result<String>;
}

pub trait PortAllocator: Send + Sync {
    fn allocate(&self, lane: &LaneId) -> Result<u16>;   // bind 试探 + 落库复用
    fn release(&self, lane: &LaneId);
    fn map(&self) -> PortMap;
}
```

## 1.6 ta-subscription

```rust
#[async_trait] pub trait SubscriptionFetcher: Send + Sync {
    /// ★ 使用**独立 Client**（不走 lane）；默认直连
    async fn fetch(&self, s: &Subscription) -> Result<FetchOutcome>;
}
pub struct FetchOutcome {
    pub raw: Vec<u8>,            // 原文，压缩存
    pub format: DetectedFormat,  // ClashYaml | Base64Links | PlainLinks | Html | Unknown
    pub userinfo: Option<SubUserinfo>,     // upload/download/total/expire
    pub suggested_interval_s: Option<u64>, // Profile-Update-Interval
    pub nodes: Vec<NodeMeta>, pub groups: Vec<GroupMeta>,
}
pub trait SubscriptionValidator: Send + Sync {
    /// 与上一版比对，异常则拒绝采纳（坏更新不清空）
    fn accept(&self, prev: &FetchOutcome, next: &FetchOutcome) -> AcceptDecision;
}
```

## 1.7 ta-gateway

```rust
#[async_trait] pub trait OutboundDialer: Send + Sync {
    async fn execute(&self, lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse>;
    async fn health(&self, lane: &LaneId) -> HealthReport;
}

pub trait ProtocolAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    fn detect(&self, path: &str) -> bool;
    fn to_ir(&self, raw: &serde_json::Value) -> Result<CanonRequest>;
    fn from_ir(&self, req: &CanonRequest) -> Result<serde_json::Value>;
    fn verify_no_loss(&self, src: &CanonRequest, encoded: &serde_json::Value) -> Vec<Loss>;
}

pub trait EventBus: Send + Sync {
    fn publish(&self, e: TaEvent);
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent>;
}

pub trait PassthroughPolicy: Send + Sync {
    /// 返回"要发给上游的头部"，必须是**整体转发 + 定点修改**
    fn build_headers(&self, client: &HeaderMap, channel: &Channel, secret: &str) -> HeaderMap;
    /// 返回"要发给上游的 body"，必须是**Value 定点 patch**
    fn patch_body(&self, raw: &[u8], channel: &Channel) -> Result<Vec<u8>>;
    fn diff_headers(&self, client: &HeaderMap, upstream: &HeaderMap) -> HeaderDiff;  // 自检
}
```

---

# 2. L4 · 内核接口（mihomo sidecar）

## 2.1 生命周期契约

| 阶段 | 约定 |
|---|---|
| 启动 | `mihomo -d <appdata>/kernel -f kernel.generated.yaml`，**绑定 Job Object**（Windows）防孤儿 |
| 就绪 | 轮询 `GET /version`（带 secret），超时 5s |
| 失败 | 指数退避重启 1s→2s→4s→8s→16s（上限 30s），连续 5 次 → `state=failed`，托盘红，通道降级为仅 direct |
| 停止 | 先 `SIGTERM`/`taskkill`，等 2s，再强杀 |
| 状态机 | `stopped → starting → ready → (restarting → ready)* | failed` |

## 2.2 我们生成的配置（唯一真源格式）

```yaml
# GENERATED BY TokenAgency — DO NOT EDIT
mode: rule
log-level: warning
mixed-port: 0                # 关闭默认端口
allow-lan: false
bind-address: 127.0.0.1
ipv6: false
find-process-mode: off
external-controller: 127.0.0.1:39090
secret: "<random>"
profile: { store-selected: true, store-fake-ip: false }
tun: { enable: false }       # ★ 绝不接管系统流量

dns:
  enable: true
  enhanced-mode: redir-host  # 不用 fake-ip（没有 TUN/DNS 劫持）
  nameserver: [...]          # 来自 net.dns.nameserver
  proxy-server-nameserver: [...]   # ★ 节点自身域名解析，必须干净 DoH
  default-nameserver: [...]

proxy-providers:             # 每个订阅一个
  <provider_key>:
    type: http
    url: "<订阅 URL>"
    interval: <interval_s>
    path: ./providers/<key>.yaml
    header:                  # ★ UA 在这里下发
      User-Agent: ["clash-verge/v2.0.0"]
    filter: "<include regex>"
    exclude-filter: "<exclude regex>"
    health-check: { enable: true, url: "<真实上游端点>", interval: 60 }

proxy-groups:                # 我们生成，不是订阅的
  - { name: "LANE-<lane_id>", type: <url-test|select|fallback|load-balance>,
      use: [<provider_key>], url: "<真实上游端点>", interval: 60, tolerance: 100 }

listeners:                   # ★ 每条 lane 一个入站，绑定 proxy
  - { name: "lane-<id>", type: mixed, listen: 127.0.0.1, port: <3910x>, proxy: "LANE-<id>" }

rules:
  - MATCH,DIRECT             # 兜底；listeners 已各自指定 proxy，规则不参与决策
```

**约束**：`listeners[].proxy` 必须是**已存在的组名或 `DIRECT`**，否则内核启动失败 → 生成时必须先校验引用。

## 2.3 我们使用的 REST 端点

| 用途 | 端点 |
|---|---|
| 就绪/版本 | `GET /version` |
| 重载配置 | `PUT /configs?force=true` body `{"path": "<abs>"}` |
| 切换组内节点 | `PUT /proxies/{group}` body `{"name": "<node>"}` |
| 测速 | `GET /proxies/{name}/delay?timeout=5000&url=<probe>` |
| 节点/分组快照 | `GET /proxies` 、`GET /providers/proxies` |
| 刷新订阅 | `PUT /providers/proxies/{provider_key}` |
| 内核日志 | 子进程 stdout/stderr 落 `<appdata>/logs/kernel.log` |

**鉴权**：`Authorization: Bearer <secret>`。**仅 loopback**。

---

# 3. L6 · 订阅服务器契约

| 项 | 约定 |
|---|---|
| Client | **独立 reqwest Client**，不走 lane（鸡生蛋）；默认直连，可选跟随系统代理/引导代理 |
| 请求头 | `User-Agent` 来自 `subscription.user_agent`（默认 `clash-verge/v2.0.0`）；`Accept: */*` |
| 超时 | connect 10s / read 30s（订阅文件小）；重试 2 次（`backon`，指数退避） |
| 响应解析 | 优先 `Content-Disposition` 文件名提示 → 再内容嗅探（`<!DOCTYPE`/`<html` → Html；含 `proxies:` → ClashYaml；base64 可解码且含 `://` → Base64Links；含 `://` → PlainLinks） |
| 响应头 | `Subscription-Userinfo` → 流量/到期；`Profile-Update-Interval` → 建议周期 |
| 失败 | 任何失败**保留上版**，记录原因，`emit("subscription:updated", {ok:false})` |
| 版本 | 保留最近 `keep_versions`（默认 3）份；可回滚 |
| 与内核的关系 | 内核通过 `proxy-providers` **自己拉取**；我们拉取是为了：UA 控制、userinfo、版本与回滚、坏更新拦截、UI 预览 |

> 注意：内核自己也会拉一次订阅。为避免双份请求，默认让**内核负责节点数据**，我们只在需要 userinfo/预览/回滚时才拉。`net.kernel.subscription_source: kernel | app`（默认 `kernel`）。

---

# 4. L7 · 存储契约

## 4.1 迁移

- 用 `sqlx::migrate!`，文件放 `crates/ta-store/migrations/NNNN_<name>.sql`，**只增不改**。
- 每次启动自动 `migrate`；失败则拒绝启动并提示备份路径。
- `user_version` 与迁移版本号必须一致。

## 4.2 表清单（契约，字段见 [02 §3](02-config-and-data-model.md) 与 [07 §6.5](07-ui-pages-and-cost.md)）

```
config / config_snapshot
usage_request / usage_daily / api_key_usage / quota_state
sticky / trace / health_sample / secret_meta
subscription / subscription_version
```

**硬约束**：
- `usage_daily.token_kind` 是**行维度**（不是列）→ 新增 token 类型无需改表。
- `usage_request.price_snapshot` 必须写 → 改价不改历史。
- `trace.body` 只存压缩 Blob，图片只存 `{type, media_type, bytes, sha256_prefix}`。
- **key 绝不入库**。

## 4.3 写入路径（事务边界）

```
一次请求完成
  └─ 单事务: INSERT usage_request
             UPSERT usage_daily (按 token_kind 展开成多行)
             UPSERT quota_state
  └─ 独立写: INSERT trace（可失败，不影响账本）
  └─ 事件:   publish(TaEvent::RequestFinished(摘要))
```

---

# 5. L8 · 错误码总表

`TaError` 的所有变体 → `code` / `http` / `retryable`：

| code | type | HTTP | 含义 | 可重试 |
|---|---|---|---|---|
| TA-1001 | `tokenagency_unauthorized` | 401 | 本地密钥错误/缺失 | 否 |
| TA-1002 | `tokenagency_rate_limited` | 429 | 本机 RPM 超限 | 是 |
| TA-1003 | `tokenagency_body_too_large` | 413 | 超过 `max_body_mb` | 否 |
| TA-1004 | `tokenagency_malformed_request` | 400 | body 不是合法 JSON / 缺 model | 否 |
| TA-2001 | `tokenagency_model_not_found` | 404 | 逻辑模型不存在 | 否 |
| TA-2002 | `tokenagency_no_route` | 400 | 通配路径无法确定模型 | 否 |
| TA-2003 | `tokenagency_no_viable_channel` | 415 | 候选被策略/标签全部排除 | 否 |
| TA-2004 | `tokenagency_all_channels_exhausted` | 402 | 全部候选额度耗尽 | 否 |
| TA-2005 | `tokenagency_all_channels_failed` | 502 | 全部候选失败 | 是（客户端层面） |
| TA-2006 | `tokenagency_capability_loss` | 415 | 跨协议转换会丢内容（拒绝转发） | 否 |
| TA-3001 | `tokenagency_kernel_not_ready` | 503 | 内核未就绪且无 direct 通道 | 是 |
| TA-3002 | `tokenagency_lane_unavailable` | 503 | 该 lane 无可用节点 | 是 |
| TA-3003 | `tokenagency_stream_broken` | — | 已输出后上游断连（仅记 trace） | 否 |
| TA-4001 | `tokenagency_credential_invalid` | — | key 失效/被封（标记并通知） | 否 |
| TA-5001 | `tokenagency_config_invalid` | 400 | 配置校验失败（IPC 用） | 否 |
| TA-5002 | `tokenagency_referenced` | 409 | 被引用，禁止删除 | 否 |
| TA-5003 | `tokenagency_plaintext_secret` | 400 | 检测到明文密钥 | 否 |
| TA-9001 | `tokenagency_internal` | 500 | 其它 | 否 |

**上游错误一律透传**（不改状态码、不改 body），只加 `x-tokenagency-*` 头。

---

# 6. 上游 usage 归一化表（TokenMeter 的规格）

> **裁决记录（第 7 轮修订）**：员工 P3 实现时发现下表不够精确 —— 它只写了"cached 从 input 扣、reasoning/audio 从 output 扣"，
> 但**上游还有别的子类型嵌套在总数里**（OpenAI 的预测 token、输入 audio，Gemini 的 tool token）。
> 不扣就会与上游 `total_tokens` 对不上，也就是重复计数。
>
> **裁决：以"总额守恒"为准，不以本表字面为准。** 即：
> **归一化后的 `Usage.total()` 必须等于上游给出的 total（若上游给了）。**
> 凡是被上游计入父类总数的子类型，一律从父类里扣掉、单独记为自己的 kind。
> 下表已按此修订；P3 的实现是正确的，无需返工。

| TokenKind | OpenAI Chat | OpenAI Responses | Anthropic | Gemini |
|---|---|---|---|---|
| `Input` | `prompt_tokens` − cached − audio | `input_tokens` − cached | `input_tokens` | `promptTokenCount` − cachedContentTokenCount − toolUsePromptTokenCount |
| `InputCached` | `prompt_tokens_details.cached_tokens` | `input_tokens_details.cached_tokens` | `cache_read_input_tokens` | `cachedContentTokenCount` |
| `InputCacheWrite` | — | — | `cache_creation.ephemeral_5m_input_tokens`（无细分时取 `cache_creation_input_tokens`） | — |
| `InputCacheWrite1h` | — | — | `cache_creation.ephemeral_1h_input_tokens` | — |
| `InputAudio` | `prompt_tokens_details.audio_tokens` | — | — | — |
| `Output` | `completion_tokens` − reasoning − audio − accepted_prediction − rejected_prediction | `output_tokens` − reasoning | `output_tokens` | `candidatesTokenCount` |
| `OutputReasoning` | `completion_tokens_details.reasoning_tokens` | `output_tokens_details.reasoning_tokens` | —（已并入 output） | `thoughtsTokenCount` |
| `OutputAudio` | `completion_tokens_details.audio_tokens` | — | — | — |
| `OutputPredictionAccepted` | `completion_tokens_details.accepted_prediction_tokens` | — | — | — |
| `OutputPredictionRejected` | `completion_tokens_details.rejected_prediction_tokens` | — | — | — |
| `Other("tool_use_prompt")` | — | — | — | `toolUsePromptTokenCount` |

**各家的"不扣"也要写清楚（否则会过度扣减）**：

- **Anthropic**：`input_tokens` **本身就不含** `cache_read_input_tokens` / `cache_creation_input_tokens`，所以 Input 不做任何减扣。
- **Gemini**：`candidatesTokenCount` **不含** `thoughtsTokenCount`，所以 Output 不扣 thoughts；而 `totalTokenCount` 含 thoughts —— 这两条并存不矛盾。

**四条铁律**：

1. **总额守恒（最高优先级）**：`Usage.total() == 上游 total`（上游给了 total 时）。本表与该规则冲突时，**以规则为准**。
2. **必须先把子类型从父类总数里减掉**，否则重复计费统计。
3. `usage_raw` 原样保存，供对账与将来补新类型。
4. 上游没给细分 → 只记 `Input`/`Output` 并把 `usage_source` 标为 `UpstreamPartial`，**不估算、不猜**；上游出现的新类型用 `TokenKind::Other(...)` 原样保留，不要丢弃。

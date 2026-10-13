/**
 * 注意：GENERATED REPLACEMENT PENDING — T1 完成后由 `npm run gen:bindings` 覆盖
 * （`tauri-specta` 从 Rust 类型生成；生成后**禁止手改**，前端只允许 import）。
 *
 * 在那之前，这份手写版是 F2/F3/F4 的唯一类型契约。它**逐字段对齐**
 * `crates/ta-core/src/domain/mod.rs` + `src/types.rs`（前者的作者与后者一致），
 * 以及 `docs/10-interface-spec.md` §2.2 的命令签名 / §2.4 的 DTO。
 *
 * 对齐规则（生成后也必须成立）：
 * - 字段名 = Rust 字段名（本项目 config 结构体**没有** `rename_all`，所以是 snake_case）；
 * - `Option<T>` → `T | undefined`（`#[serde(default)]` 的 `Vec`/`bool` 等给默认值，
 *   序列化时可能出现 `null`，故可选项统一写成 `T | null | undefined` 的只在必要处使用）；
 * - 枚举 = 字符串联合，取值与 serde 表示一致（**注意特例**：
 *   `GroupType` 是 kebab-case、`Retention` 是 `7d|30d|90d|forever`、
 *   `Protocol::OpenaiResponses` 序列化为 `openai_responses`）；
 * - 各结构体的 `#[serde(flatten)] extra`（未知字段保留）在 TS 里**不建模**——
 *   类型里没有的键在运行时可能存在，但代码应当只读具名字段。
 */

// ================================================================ ID（Rust 侧是透明 newtype → string）
export type ProviderId = string;
export type CredentialId = string;
export type ModelId = string;
export type ChannelId = string;
export type ChannelGroupId = string;
export type PolicyId = string;
export type LaneId = string;
export type SubscriptionId = string;
export type RequestId = string;
export type StickyKey = string;
export type ClientId = string;

// ================================================================ 基础标量（src/types.rs）

/** 金额：`amount` + ISO 4217 币种。 */
export type Money = { amount: number; currency: string };

/** 归一化后的 token 类型。**开放集合**：上游新增类型会原样出现在这里
 *  （`string & {}` 的作用就是"已知字面量 + 任意字符串"，避免退化成裸 string）。 */
export type TokenKind =
  | "input"
  | "input_cached"
  | "input_cache_write"
  | "input_cache_write_1h"
  | "input_audio"
  | "input_image"
  | "output"
  | "output_reasoning"
  | "output_audio"
  | "output_prediction_accepted"
  | "output_prediction_rejected"
  | (string & {});

/** `BTreeMap<TokenKind, u64>` → JSON object。**未出现的类型就是 0，不写进 map。** */
export type TokenUsage = Partial<Record<TokenKind, number>>;

export type Protocol = "openai" | "openai_responses" | "anthropic" | "gemini" | "custom";

export type UsageSource = "upstream" | "upstream_partial" | "estimated";

/** 请求日志三档 + off（[07 §2.1]）。 */
export type LogMode = "off" | "meta" | "shape" | "full";

export type Retention = "7d" | "30d" | "90d" | "forever";

/** 实时流/事件用的请求摘要。 */
export type RequestSummary = {
  request_id: RequestId;
  ts_ms: number;
  model: string;
  upstream_model: string;
  channel_id: ChannelId;
  client?: ClientId | null;
  lane_id?: LaneId | null;
  node?: string | null;
  status: number;
  outcome: "ok" | "retried" | "failed";
  latency_ms: number;
  ttfb_ms?: number | null;
  total_tokens: number;
  cost: Money;
  attempts: number;
  error_type?: string | null;
};

// ================================================================ 配置实体（domain/mod.rs）

export type HeaderMode = "pass" | "allowlist";
export type BodyMode = "preserve" | "strip";
export type OnLoss = "reject" | "warn";
export type Backoff = "none" | "exponential";
export type ImageHandling = "hash" | "full";
export type DialerKind = "kernel" | "builtin";
export type KernelProvider = "external" | "bundled";
export type SubscriptionSource = "kernel" | "app";
export type SubscriptionFormat =
  | "auto"
  | "clash"
  | "base64_links"
  | "plain_links"
  | "convert_links";
export type LaneKind =
  | "direct"
  | "subscription_auto"
  /**
   * **遗留（第 321 轮取消的概念，[docs/54]）**：用订阅里自带的组。
   *
   * 它**还在这个联合类型里**，因为契约必须逐字对齐 Rust 的 `LaneKind`，而那个 variant
   * 不能删 —— 迁移跑在**已经反序列化完**的文档上，删了会让老配置在 `config.load()`
   * 就解析失败，迁移根本没机会跑（用户升一次版本就丢配置）。
   *
   * 但它**不是可选项**：v2 → v3 迁移会把它转成 `subscription_auto` + 显式 `select`，
   * 迁移之后还出现就是配置损坏，校验器与内核配置生成都会明确报错。
   */
  | "subscription_group"
  | "subscription_node"
  | "filter";
/** 注意：kebab-case（与其它枚举不同）。 */
export type GroupType = "select" | "url-test" | "fallback" | "load-balance";
export type AuthScheme = "bearer" | "header" | "query" | "none";
export type HeaderConflict = "keep_client" | "override";
export type OnExceed = "wait" | "next_candidate";
/**
 * 选路策略。**现在只有一种**（[docs/48]）。
 *
 * 为什么收敛到一种：`docs/47` 查清"各家额度耗尽返回什么"之后确认**分码也分不干净**
 * （同一家 429 既表限流也表额度耗尽、有的家把额度耗尽藏在 400 里），于是
 * "引擎自动挑线路"这条路放弃，改成**用户用开关手动调度 + 引擎只做平均**。
 *
 * **平均粘性**：按会话标识粘住同一条线路；未命中（新会话）挑 token 用量最少的；
 * 没有会话标识时在"最轻的那一档"里随机；这条线路报错就把会话改粘到另一条。
 *
 * 旧取值（`priority` / `cheapest` / …）在 Rust 侧仍能被解析成这一个值
 * （`#[serde(alias)]`），所以老配置不会因为升级而读不出来。
 */
export type Selector = "balanced_sticky";
export type ReleaseReason =
  | "hard_error"
  | "cooldown"
  | "user_pin_change"
  | "ttl";
export type FailoverTrigger =
  | "http429"
  | "http402"
  | "http5xx"
  | "timeout"
  | "connect_error"
  | "channel_tagged_out"
  | "capability_mismatch";
export type NoCandidateAction = "error" | "use_any";
export type PriceSource = "builtin" | "manual" | "imported";
export type PriceUnit = "per_1m" | "per_1k" | "per_token";
export type AutostartMode = "tray" | "window";

export type PassthroughConfig = {
  header_mode: HeaderMode;
  body: BodyMode;
  strip_hop_by_hop: boolean;
  verify_no_diff: boolean;
  on_loss: OnLoss;
};

export type RetryConfig = {
  max_attempts: number;
  stream_max_attempts: number;
  /** **恒为 false**：已向下游写出字节后绝不重试。 */
  retry_after_first_byte: boolean;
  cooldown_s: number;
  backoff: Backoff;
};

export type MeteringConfig = {
  /** 注意：开启会**修改客户端请求**（注入 `stream_options.include_usage`），默认关闭。 */
  inject_stream_usage: boolean;
};

export type GatewayConfig = {
  listen: string;
  admin_listen: string;
  local_key?: string | null;
  max_body_mb: number;
  debug_headers: boolean;
  /** [docs/28 §4] **已删除的死开关**：不再有"要不要对外暴露模型能力标签"这回事（标签本来就是从我们记录的信息派生的）。老配置里带这个键仍然能解析（落进 `extra`）。 */
  fallback_provider?: ProviderId | null;
  passthrough: PassthroughConfig;
  retry: RetryConfig;
  metering: MeteringConfig;
  /** [docs/59] 下游 API 家族开关（勾选式）。 */
  downstream: DownstreamConfig;
};

/**
 * [docs/59] 下游对外提供哪几种 API 家族 —— 每个一批的**勾选式开关**。
 *
 * 用户第 326 轮的需求 2："可以在设置页面选择我们下游是统一成 OPENAI 的两种还是
 * Anthropic 的"，补一句"是打开关闭形式，按需求勾选"。所以这是**三个独立的开关**，
 * 不是"选一个"。关掉的端点会在网关侧**当场明确报错**（错误里点出是哪个开关），
 * 而不是被转发到上游。
 */
export type DownstreamConfig = {
  /** `POST /v1/chat/completions` —— OpenAI 兼容客户端最常用的一种 */
  openai_chat: boolean;
  /** `POST /v1/responses` —— OpenAI 新的 Responses API */
  openai_responses: boolean;
  /** `POST /v1/messages` —— Anthropic / Claude Code */
  anthropic_messages: boolean;
};
export type RequestLogConfig = {
  mode: LogMode;
  retention: Retention;
  max_rows: number;
  max_size_mb: number;
  sample_rate: number;
  image_handling: ImageHandling;
};

export type DnsConfig = {
  enhanced_mode: string;
  nameserver: string[];
  /** 要点：节点自身域名解析，必须配，否则节点都连不上。 */
  proxy_server_nameserver: string[];
  default_nameserver: string[];
};

export type HealthConfig = {
  interval_s: number;
  url_by_provider: boolean;
  probe_url?: string | null;
};

export type KernelConfig = {
  provider: KernelProvider;
  path?: string | null;
  controller: string;
  lane_port_range: number[];
  subscription_source: SubscriptionSource;
};

export type NetConfig = {
  kernel: KernelConfig;
  dialer: DialerKind;
  dns: DnsConfig;
  health: HealthConfig;
  default_lane?: LaneId | null;
  system_proxy: boolean;
  tun: boolean;
};

export type SubscriptionFilter = {
  include?: string | null;
  exclude?: string | null;
};

export type SubscriptionHealthCheck = {
  enabled: boolean;
  url?: string | null;
  interval_s: number;
  tolerance_ms: number;
};

export type Subscription = {
  id: SubscriptionId;
  name: string;
  url: string;
  user_agent: string;
  format: SubscriptionFormat;
  interval_s: number;
  keep_versions: number;
  filter: SubscriptionFilter;
  health_check: SubscriptionHealthCheck;
  /** 省略 = 启用（被列进配置 = 要用它）。 */
  enabled: boolean;
  /** 内核里的 provider key（由 id 派生，稳定；全局唯一）。 */
  provider_key?: string | null;
  /**
   * **用户关掉的节点名**（省略/空 = 全部启用）。
   *
   * 用户第 202 轮：订阅里的节点像服务商的「模型」一样可以单独启用/关闭，带全选、默认全选。
   * 存"关掉的"而不是"开着的"：机场新增的节点默认就是启用的，不需要用户回来补勾。
   */
  disabled_nodes?: string[];
};

export type Lane = {
  id: LaneId;
  name: string;
  kind: LaneKind;
  subscription?: SubscriptionId | null;
  /** **遗留（[docs/54]）**：`kind: subscription_group` 时订阅里自带的组名。理由见 `LaneKind`。 */ // ta-allow-emoji —— 这是用户数据示例（[15 §2.2]），不是我们产生的文本
  group?: string | null;
  /**
   * 组策略。**「自动选择」必须是 `select`** —— `url-test` 会按自己的 interval 周期性重选，
   * 出口 IP 自己就变了，粘性不成立（[docs/54] §1）。
   */
  group_type?: GroupType | null;
  /** `kind: subscription_node` 时的固定节点名。 */
  node?: string | null;
  /** lane 级二次收窄（include 正则字符串）。 */
  filter?: string | null;
  exclude_filter?: string | null;
  enabled: boolean;
};

export type AuthConfig = {
  scheme: AuthScheme;
  /** `scheme: header | query` 时的头名/参数名，如 `x-api-key`。 */
  header?: string | null;
  prefix?: string | null;
};

export type TimeoutConfig = {
  connect_s: number;
  read_s: number;
  total_s?: number | null;
};

export type TlsConfig = {
  insecure: boolean;
  extra_ca_pem?: string | null;
  min_version?: string | null;
};

export type ProviderPassthrough = {
  header_mode?: HeaderMode | null;
  body?: BodyMode | null;
};

export type Provider = {
  id: ProviderId;
  name: string;
  protocol: Protocol;
  base_url: string;
  auth: AuthConfig;
  /** 服务商级默认线路（可被 credential/channel 覆盖）。 */
  lane?: LaneId | null;
  /** 附加头：**默认只补缺，不覆盖客户端同名头**。 */
  headers: Record<string, string>;
  on_header_conflict: HeaderConflict;
  passthrough: ProviderPassthrough;
  timeout: TimeoutConfig;
  tls: TlsConfig;
  model_prefix?: string | null;
  enabled: boolean;
};

/**
 * `provider_upsert` 的入参形态：**只需 4 个必填字段**，其余省略由 Rust 的 `#[serde(default)]` 补默认。
 *
 * 与 [`Provider`] 的唯一区别 = 这些字段在 Rust 侧带 `#[serde(default)]`
 * （`crates/ta-core/src/domain/mod.rs:693` 起）：`auth` / `lane` / `headers` /
 * `on_header_conflict` / `passthrough` / `timeout` / `tls` / `model_prefix` / `enabled`。
 * `auth` 只在真的填了 API Key 时才带；`AuthConfig` 自身所有字段也有默认值
 * （`scheme` 默认 Bearer，见同文件的 `impl Default for AuthConfig`）。
 *
 * **不硬编默认值、也不复制模板**：默认值的唯一真相源是 Rust 的 `Default`。
 * 漏同步会被 `src/app/serdeDefault.guard.test.ts` 机械拦下（契约比线上严/松都红）。
 */
export type ProviderInput = Pick<Provider, "id" | "name" | "protocol" | "base_url"> & {
  auth?: AuthConfig;
  lane?: LaneId | null;
  headers?: Record<string, string>;
  on_header_conflict?: HeaderConflict;
  passthrough?: ProviderPassthrough;
  timeout?: TimeoutConfig;
  tls?: TlsConfig;
  model_prefix?: string | null;
  enabled?: boolean;
};

export type GuardConfig = {
  max_rpm?: number | null;
  max_concurrency?: number | null;
  max_input_tokens?: number | null;
  on_exceed: OnExceed;
};

export type Credential = {
  id: CredentialId;
  provider: ProviderId;
  label: string;
  /** `"${secret:name}"` / `"${env:VAR}"`；**绝不存明文**。 */
  secret: string;
  /** 这份套餐默认走哪条线。 */
  lane?: LaneId | null;
  guard: GuardConfig;
  tags: string[];
  enabled: boolean;
};

export type DisplayConfig = {
  group?: string | null;
  order: number;
};

export type Model = {
  /** 对外暴露的逻辑模型名（Rust 侧字段类型是 `String`，不是 newtype）。 */
  id: string;
  aliases: string[];
  display: DisplayConfig;
  expose: boolean;
  /**
   * **模型级总开关**（[docs/48 §5]）。
   *
   * `false` = **彻底停用**：`GET /v1/models` 不再列出它，调用它直接按"模型不存在"拒绝。
   * 关掉**不删除**任何东西 —— 这正是这个字段的意义：想暂时下线一个模型时不用删了再建。
   *
   * 与 `expose` 的分工：`expose` 管"列不列进 `/v1/models`"，`enabled` 管"能不能用"；
   * 停用需要两者同时成立，所以实际判据是 `enabled && expose`。
   */
  enabled: boolean;
  /**
   * **这个模型要用哪几条线路**（[docs/28 §2]）—— 关系的唯一真源，由模型侧持有。
   *
   * 线路归服务商（在服务商详情页里创建/编辑），模型只负责"用"：一条线路可以服务多个模型，
   * 一个模型由多条线路构成。**至少一条**（[docs/28 §2]：零条线路的模型对外可调用却必然失败）。
   *
   * 删除只往一个方向传：删模型不动线路；删线路会从所有模型的这个列表里摘掉它，
   * 摘完一条都不剩的模型会被自动删除（[docs/27 §1.2]）。
   */
  lines: ChannelId[];
  /**
   * 这个模型**默认走哪条线路**（[docs/19 §1.5]）。
   *
   * 省略 / null = 直连（与 `net.default_lane` 链一致）。
   * 注意：**没有"给模型挑网络代理"这回事**（[docs/28 §1.1]）—— 代理是**线路**的属性，
   * 只在服务商详情页的「编辑线路」里改；这个字段只表达"这个模型的线路默认继承哪条代理"。
   */
  default_lane?: LaneId | null;
};

export type Transform = {
  path?: string | null;
  headers_add: Record<string, string>;
  headers_remove: string[];
  headers_override: Record<string, string>;
  /** 定点 body patch（如强制 `stream_options.include_usage=true`）。 */
  body_patch: Record<string, unknown>;
};

/**
 * 上游能力声明（[docs/17 §2]，对应 Rust `ta_core::domain::ChannelCapabilities`）。
 *
 * **两条线路只有在这些字段完全一致时**，才允许被同一条路由（同一个对外模型）指向。
 * 比较规则：逐字段相等；`modalities` 先排序去重再比；`max_output_tokens` 的 `null`
 * 只等于 `null`（"未声明"不是"无所谓"）。后端会强制校验（TA-2007）。
 *
 * 默认值（后端对老配置的填充）：全 false、`context_tokens` 0、`max_output_tokens` null、
 * `modalities` `["text"]`。
 */
export type ChannelCapabilities = {
  /** 支持图片输入 */
  vision: boolean;
  /** 支持函数调用 / 工具 */
  tools: boolean;
  /** 支持推理 / 思考内容 */
  reasoning: boolean;
  /** 上下文窗口（token） */
  context_tokens: number;
  /** 单次最大输出；`null` = 未声明 */
  max_output_tokens: number | null;
  /** 模态，如 `["text", "image"]`；比较时先排序去重 */
  modalities: string[];
};

export type Channel = {
  id: ChannelId;
  /**
   * **过渡期遗留字段**（[docs/28 §2]）：老配置里"这条线路属于哪个模型"。
   *
   * 关系已经翻转到 `Model.lines`（一条线路可以服务多个模型）。后端目前仍会把它发出来
   * （老配置里就是它），但**新代码不要依赖它**：读"哪些模型在用这条线路"请看下面那个派生值。
   * 标成可选：新写入的线路本来就没有这个值（写命令会把它当空字符串）。
   */
  model?: string;
  /**
   * **哪些模型在用这条线路**（[docs/28 §2]，**只读派生值**，由后端现算）。
   *
   * 关系的真源在 `Model.lines` 上：线路不再写"我是哪个模型的"（一条线路可以同时服务多个模型）。
   * 写命令**不接受**这个字段（要改关系请用 `model_upsert({ model: { lines } })`）。
   *
   * **可选**：S1 阶段后端还没把它加进响应（[docs/28 §4] 的 S2 补上），
   * 所以读它的界面必须按"可能没有"处理（`channel.models ?? []`），不许当成空数组去下结论。
   */
  models?: ModelId[];
  provider: ProviderId;
  credential: CredentialId;
  /** 不填则继承 credential.lane → provider.lane → net.default_lane → direct。 */
  lane?: LaneId | null;
  upstream_model: string;
  transform: Transform;
  priority: number;
  weight: number;
  /** 能力标签：`vision` / `no-vision` / `cheap` / `official` / `cn-direct` / `no-log`。
   *  **未标注 = 照发**（默认行为是发出去，不是拒绝）。 */
  tags: string[];
  target_override?: string | null;
  enabled: boolean;
  /**
   * 上游能力声明（[docs/17 §2]）。Rust 侧是 `#[serde(default)] pub capabilities: ChannelCapabilities`：
   *
   * - **后端发出来的对象永远带全部 6 个字段**（读 `channel.capabilities.vision` 不会 undefined）；
   * - **前端写 `channel_upsert` 时可以整个不填** —— 不填就是后端默认
   *   （全 false / `context_tokens` 0 / `max_output_tokens` null / `modalities` `["text"]`）。
   *
   * 所以这里标成**可选**（与 `lane?` / `target_override?` 同一口径：能省的字段标可选）。
   * 需要比较两条线路的能力时，缺失按后端默认处理，**不要**把它当成"另一种未声明"。
   */
  capabilities?: ChannelCapabilities;
};

export type StickyConfig = {
  enabled: boolean;
  ttl_h: number;
  /** key 派生顺序，取值如 `header:x-session-id` / `metadata.user_id` / `body_hash`。 */
  key: string[];
};

export type ChannelGroup = {
  id: ChannelGroupId;
  model: string;
  /** 顺序即候选顺序（配合 priority 使用）。 */
  members: ChannelId[];
  selector: Selector;
  sticky: StickyConfig;
  release_on: ReleaseReason[];
};

export type PolicyMatch = {
  all: boolean;
  model_matches: string[];
  has_image?: boolean | null;
  header_matches: Record<string, string>;
  ua_matches?: string | null;
  channel_tags_any: string[];
  channel_tags_all: string[];
  input_tokens_lt?: number | null;
  input_tokens_gt?: number | null;
  stream?: boolean | null;
  time_window?: string | null;
  provider_in: ProviderId[];
};

export type FailoverPolicy = {
  on: FailoverTrigger[];
  max_attempts: number;
  cooldown_s: number;
  backoff: Backoff;
};

export type PolicyAction = {
  use_group?: ChannelGroupId | null;
  use_channels: ChannelId[];
  exclude_channels: ChannelId[];
  require_tag?: string | null;
  exclude_tag?: string | null;
  // 这里曾有 `selector?: Selector | null`（策略级选路覆盖）。选路策略收敛为唯一一种之后
  // （[docs/48]）它没有第二种取值，字段删除；老配置里残留的键由 Rust 侧的 `extra` 吸收，
  // 解析不失败（只是不再生效）。
  failover?: FailoverPolicy | null;
  on_no_candidate: NoCandidateAction;
  error_code?: number | null;
  pin_sticky?: ChannelId | null;
  /** 一键全切线路（优先级最高）。 */
  use_lane?: LaneId | null;
};

export type Policy = {
  id: PolicyId;
  name: string;
  enabled: boolean;
  /** 数字小的先求值；**首个命中即生效**。 */
  priority: number;
  match: PolicyMatch;
  action: PolicyAction;
};

export type PriceMatch = {
  model?: string | null;
  provider?: ProviderId | null;
  credential?: CredentialId | null;
};

/**
 * 套餐费的计费周期（Rust 侧 `PlanFeeCycle`）。
 *
 * 注意它**不是**"额度窗口"：这里说的是"这笔套餐费按什么周期摊到每一天"
 * （成本中心把月费摊成日成本）。早先它叫 `QuotaWindow`，与已下线的额度功能同名。
 */
export type PlanFeeCycle = "hour" | "day" | "month" | "none";

export type PlanFee = {
  amount: number;
  currency: string;
  /** 套餐费按什么周期摊到每一天（Rust 侧 `PlanFeeCycle`）。 */
  window: PlanFeeCycle;
};

export type PriceEntry = {
  match: PriceMatch;
  /** `TokenKind` → 每单位价格（单位见 `PricingConfig.unit`）。 */
  rates: Record<string, number>;
  multiplier: number;
  currency?: string | null;
  plan_fee?: PlanFee | null;
  source: PriceSource;
  effective_from?: string | null;
};

export type PricingConfig = {
  currency: string;
  unit: PriceUnit;
  usd_to_cny?: number | null;
  entries: PriceEntry[];
};

export type SystemIntegration = {
  /** **例外：默认关**（会写 OS 状态的开关一律默认关，09 §1.2 E1）。 */
  enable_tun: boolean;
  /** **例外：默认关**。 */
  enable_system_proxy: boolean;
  /** 开机自启动：**本项目唯一会写操作系统状态的功能**；默认 false（E1）。 */
  autostart: boolean;
  autostart_mode: AutostartMode;
  /** 启动延迟秒数（0–300，由我们自己实现）。 */
  autostart_delay_s: number;
  tray_show_latency: boolean;
  file_watch_mode: boolean;
  file_watch_path?: string | null;
};

/** 完整配置树（`config_get` / `config_apply` 的载荷）。 */
export type ConfigDoc = {
  version: number;
  gateway: GatewayConfig;
  request_log: RequestLogConfig;
  pricing: PricingConfig;
  net: NetConfig;
  proxy_subscriptions: Subscription[];
  lanes: Lane[];
  providers: Provider[];
  credentials: Credential[];
  models: Model[];
  channels: Channel[];
  channel_groups: ChannelGroup[];
  policies: Policy[];
  system_integration: SystemIntegration;
};

/** 校验错误/警告：**路径精确到字段**（如 `channels[2].credential`）。 */
export type FieldError = {
  path: string;
  code: string;
  message: string;
};

export type ValidationReport = {
  ok: boolean;
  errors: FieldError[];
  warnings: FieldError[];
};

/**
 * `provider_lines_create_all` 的结果（[docs/28 §1.4]）。
 */
export type LinesCreateAllResult = {
  /** 这次**真新建**了几条线路（0 = 目录里每一条都早就有线路了，一条都没写） */
  created: number;
  /** 上游目录里一共有多少条（列表里会显示多少行；`created <= total`） */
  total: number;
};

export type ApplyResult = {
  applied: boolean;
  restart_required: boolean;
  kernel_reload: boolean;
  /**
   * 本次调用**真新建**的通道数（[docs/20 §1.1]）。
   *
   * 为什么要有这个数：`model_apply_import` 不只是在模型目录里塞一行 —— 它要把模型**接到**这个服务商上
   * （建 `ch-<provider>-<credential>-<model>` 这条线）。界面得能如实说"接上了几条"，而不是让用户
   * 自己去猜（第 71 轮的坑正是"点了添加、上面什么都没有"）。其它返回 `ApplyResult` 的命令填 0。
   */
  channels_created: number;
};

/** `log_export` 的回执（docs/10 §2.2）。**path 是后端解析后的最终路径，不是入参** ——
 *  传目录进来时后端会补 `tokenagency-diagnostic-<时间戳>.zip`。 */
export type LogExportReceipt = {
  path: string;
  bytes: number;
};

export type Snapshot = {
  id: string;
  label: string;
  created_at_ms: number;
  size_bytes?: number;
};

// ================================================================ 运行态与视图（docs/10 §2.2）

export type KernelState = "stopped" | "starting" | "ready" | "restarting" | "failed";

export type KernelStatus = {
  state: KernelState;
  version?: string | null;
  pid?: number | null;
  restarts: number;
  last_error?: string | null;
};

export type AppStatus = {
  gateway: "running" | "paused";
  kernel: KernelStatus;
  version: string;
  uptime_s: number;
  active_policy_id?: PolicyId | null;
  default_lane_id?: LaneId | null;
  current_node?: string | null;
};

export type UpdateInfo = {
  version: string;
  notes?: string | null;
  url?: string | null;
};

/** 开机自启动状态。**`actual` 是系统事实，`desired` 只是配置期望**（09 §1.2 E3/E4）。 */
export type AutostartState = {
  desired: boolean;
  /** 从操作系统读到的实际状态 —— UI 开关显示的是这个。 */
  actual: boolean;
  drifted: boolean;
  mechanism: "windows-run-key" | "macos-launch-agent" | "linux-xdg" | "unsupported" | string;
  registered_command?: string | null;
  mode: AutostartMode;
  delay_s: number;
  unsupported_reason?: string | null;
};


export type ProviderView = {
  id: ProviderId;
  name: string;
  protocol: Protocol;
  base_url: string;
  enabled: boolean;
  /** 24h 成功率（0~1）。 */
  success_rate_24h?: number;
  avg_latency_ms?: number | null;
  /** 挂载了多少条通道。 */
  channel_count: number;
  status: "ok" | "warn" | "error" | "off";
};

export type CredentialView = {
  credential: Credential;
  provider_name: string;
  /** 挂载了多少条通道。 */
  channel_count: number;
  has_plaintext_secret?: boolean;
};

/**
 * 服务商模型目录里的一条（[docs/25]）：**扫描记录下来的**上游模型。
 *
 * 目录 = "这家服务商扫描到过哪些模型"的**观察记录**（存在 SQLite 里，不进配置真源）；
 * `added` = "已经接入这一家了吗"（配置里真有对应的 Model + 通道）。
 * 所以一条目录记录**不等于**一个通道 —— 那正是本轮要改的地方：
 * 扫描只记录，用户在服务商里勾一下才接入。
 *
 * **没有 `origin`：目录里的每一行都来自真实扫描。** 用户明确不要手动添加 ——
 * 手填一行会让"目录 = 上游真实提供了什么"这条不变量失效，而自动化全建在它上面。
 */
export type ModelCatalogEntry = {
  provider_id: ProviderId;
  /** 上游真实模型名（扫描给的）。 */
  upstream_model: string;
  /** 扫描派生出的**逻辑模型名**（客户端调用它；接入时以它建 `Model`）。 */
  suggested_id: string;
  /** 上一次扫描返回过它吗（本次未返回 = false；行还在，因为它可能已经接入）。 */
  seen_in_last_scan: boolean;
  first_seen_ms: number;
  last_seen_ms: number;
  /** 已经接入这一家了吗（该服务商下存在对应通道）。 */
  added: boolean;
  /**
   * 已接入时：这条目录行对应的**全部**通道 id（可能多条 —— 同一模型挂在不同凭据下）。
   *
   * 为什么是数组而不是"第一条 + 条数"：取消勾选要**把这几个都停用**，
   * 只停第一条会让勾选框下一帧又自己弹回勾上（`enabled` 看的是"有一个启用就算启用"）。
   */
  channel_ids: string[];
  /** 已接入的通道里**有启用的**（`added=false` 时为 false）。 */
  enabled: boolean;
  /**
   * **上游原样返回的对象**（[docs/28 §4]）：这条目录行对应的 `GET {base_url}/models` 里
   * `data[i]` 的**完整** JSON —— 我们不改写、不补字段，键序也保持原样。
   *
   * 用它干两件事：让用户看到"上游到底声明了什么"（`owned_by` / `created` / 中转自造字段都在），
   * 以及给能力字段提供**唯一**可核对的来源 —— [docs/28 §3] 定死了"上游没给的字段一个都不编"。
   *
   * 上游那一条不是对象、或那次没记到 → `{}`（**不是**我们编出来的对象）。
   * 类型是任意 JSON：Rust 侧 `serde_json::Value` 经 `specta(type = Any)` 出来，与 `IpcError.details` 同款。
   */
  raw: unknown;
};

export type ProbeResult = {
  ok: boolean;
  status: number;
  latency_ms: number;
  /**
   * 这一次探测**实际发的是什么**（[docs/19 §1.1]）：
   * - `"models"`：`GET {base_url}/models`（OpenAI 兼容的模型清单端点）；
   * - `"chat"`：`POST {base_url}/chat/completions` 的最小探测（有些中转没有 `/models`）；
   * - `"none"`：一次请求都没发出去（例如线路不可用/鉴权缺失）。
   *
   * **`ok=false` 不等于"服务商坏了"**：`status=404 && probe="models"` 的语义是
   * "上游没有这个端点"，UI 必须按 `probe` 分档说人话，不许把它渲染成"连接失败"。
   */
  probe: "models" | "chat" | "none";
  note?: string | null;
};

export type EchoResult = {
  sent_headers: Record<string, string>;
  upstream_headers: Record<string, string>;
  sent_body_shape: unknown;
  upstream_body?: unknown;
  latency_ms: number;
};

export type PlaintextFinding = {
  credential_id: CredentialId;
  label: string;
  /** 只给前缀，不给完整 key。 */
  hint: string;
};

/**
 * 一个模型**对外暴露的**信息（[docs/46 §304]）—— 客户端在 `/v1/models` 里看到的那一份。
 *
 * 后端只有**一份**聚合口径（`ConfigDoc::public_model_info`），网关与模型页信息面板都从它取，
 * 所以「查看模型信息」里显示的就是客户端拿到的那一份。
 */
export type PublicModelInfo = {
  /** 各线路声明过的上下文窗口取**最小**；一条都没声明 = `null`（**不是 0**）。 */
  context_window: number | null;
  /** 同上（单次最大输出）。 */
  max_output_tokens: number | null;
  /** 能力：只要有一条线路支持就算支持（并集）。 */
  vision: boolean;
  tools: boolean;
  reasoning: boolean;
  /** 输入模态（并集，已排序）。 */
  modalities: string[];
  /** 对外暴露的服务商标识（第一条线路所属服务商的 id）。 */
  owned_by: string;
};

export type ModelView = {
  model: Model;
  channel_count: number;
  /** 该模型在各通道上的上游名映射。 */
  upstream_names: string[];
  /** 对外暴露的信息（上下文/最大输出/能力/owned_by）。 */
  public_info: PublicModelInfo;
  /** 是否已定价（未定价会在看板提醒）。 */
  priced: boolean;
  cost_hint?: string | null;
};

export type ImportPreview = {
  preview_token: string;
  models: { upstream_model: string; suggested_id: string; exists: boolean }[];
  skipped: { upstream_model: string; reason: string }[];
};

export type ModelPatch = Partial<Pick<Model, "expose" | "aliases" | "display">> & {
  /**
   * 只改该模型（及其现有 channel）的网络线路；三态见 [docs/20 §1.3]：
   * 字段缺失 = 不动；`null` = 清空（回到继承链，末端就是直连）；具体 lane id = 写该线路。
   */
  lane?: LaneId | null;
};

/**
 * 我们网关对外的**访问密钥**（[docs/19 §3]）。客户端拿它调到 `127.0.0.1:8787`。
 *
 * **明文不在这个类型里**（表里只存 sha256）：明文只在 `api_key_create` 的返回里出现**一次**。
 */
export type ApiKeyView = {
  id: string;
  name: string;
  /** 展示用前缀（前 12 字符 + `…`）；列表里**不放复制按钮** —— 只存哈希，复制不出来。 */
  key_prefix: string;
  enabled: boolean;
  note?: string | null;
  created_at_ms: number;
  /** 网关最近一次用它鉴权成功的时间；从未用过 = null（**不是 0**）。 */
  last_used_at_ms?: number | null;
};

export type ApiKeyCreated = {
  key: ApiKeyView;
  /** **只此一次**。页面必须提示用户立刻保存，关掉后无法再取回。 */
  plaintext: string;
};

/**
 * 按 key × 日 × **token 种类** 聚合的用量。
 *
 * **形状刻意与既有的领域类型逐字一致**（`crates/ta-store/src/usage_store.rs:478`
 * 的 `ApiKeyUsageRow`，被 `record_api_key_usage` / 现有用量查询共用）：
 * 不在这里另造一份"分组后"的 DTO —— 那就是第二个真相源，字段一改就静默漂移。
 * 界面要按日汇总就自己在前端求和，**不要**在后端丢掉 `token_kind` / `currency`。
 */
export type ApiKeyUsageRow = {
  key_id: string;
  day: string;
  token_kind: string;
  tokens: number;
  requests: number;
  cost: number;
  currency: string;
};

export type MatrixView = {
  models: ModelRow[];
  credentials: CredentialCol[];
  cells: Cell[];
};

export type ModelRow = { model_id: ModelId; display_name: string; unpriced?: boolean };

export type CredentialCol = {
  credential_id: CredentialId;
  label: string;
  provider_name: string;
};

export type Cell = {
  model_id: ModelId;
  credential_id: CredentialId;
  channel_id?: ChannelId | null;
  lane_id?: LaneId | null;
  upstream_model?: string | null;
  priority?: number;
  weight?: number;
  tags: string[];
  enabled: boolean;
};

export type LaneView = {
  lane: Lane;
  type: LaneKind;
  node_count: number;
  current_node?: string | null;
  latency_ms?: number | null;
  health: "ok" | "degraded" | "down" | "unknown";
};

export type LaneTestResult = { node: string; latency_ms?: number | null; ok: boolean };

// [docs/55] `LaneAssignment` / `LanePickOption` 两个类型已删除 ——
// 它们只被 `lane_assignments` / `lane_pick_options` 用，而那条命令与那个页面都没了。
// 「生效线路来自哪一级」这个问题本身也不再成立：现在只有服务商级一处。

/**
 * [docs/28 §1.2] 线路（网络代理）下拉里的一项。
 *
 * 为什么要有它：这个下拉是用户**唯一**能给一条线路选网络的地方。如果没有它，
 * 用户"刚加好订阅、还没建线路"时那个下拉里什么都没得选 ——
 * 于是"给这家服务商切网络"在界面上根本做不到。
 * 这个清单把**用户真正拥有的网络**都给出来：已配置线路 + 订阅里解析出来的节点。
 *
 * * `kind: "lane"`：`value` 就是 lane id，选中直接写；
 * * `kind: "node"`：`value` 形如 `node:<订阅id>:<节点名>`，要先调
 *   `lane_ensure_for_node` 拿到线路 id（**已有等价线路就复用**），再写。
 *
 * [docs/55] 写进去的是**服务商**的 `lane`（`channel.lane` / `credential.lane` /
 * `policy.use_lane` 三个字段已删除）—— 每个服务商统一一个节点。
 *
 * 节点只来自**已采纳的订阅版本**：没有版本时这里一个节点都不出现（那是"还没解析"，
 * 不是"没有节点"）。
 */
export type LaneChoice = {
  /** `lane` 类 = lane id；`direct` 类 = `direct:`；`node` 类 = `node:<订阅id>:<节点名>` */
  value: string;
  /** 人能读的名字（节点名原样，可能含 emoji） */
  label: string;
  kind: "lane" | "direct" | "node";
  /** `lane` 类一定有；`node` 类若已有等价线路也有（选中 = 复用，不新建） */
  lane_id?: LaneId;
  subscription_id?: SubscriptionId;
  node?: string;
};

export type LaneChoiceGroup = {
  group: string;
  items: LaneChoice[];
};

/**
 * `subscription_list` 的一行：**配置实体 + 当前生效版本的解析状态**。
 *
 * 解析状态来自版本存储（`VersionKeeper` 的元数据）：**从来没有已采纳的版本时这几个字段
 * 一并缺失**，而不是给 `0` / 空串（[docs/18 §1.2] 的"未知不给数字"）。前端把"缺失"读成
 * `undefined`（未读取）、把"明确 null"读成 `null`（后端说没有）—— 两者不是一回事。
 */
export type SubscriptionView = {
  subscription: Subscription;
  provider_key: string;
  /** 生效版本的节点数；没有已采纳版本时**缺失**（不是 0）。 */
  node_count?: number;
  /** 生效版本的分组数；没有已采纳版本时**缺失**（不是 0）。 */
  group_count?: number;
  /** 不再回填：流量与更新时间统一走 `subscription_traffic`（[docs/18 §1.2] 的口径）。 */
  userinfo?: { upload?: number; download?: number; total?: number; expire_ms?: number } | null;
  /** 生效版本的抓取时刻（Unix 毫秒）；没有已采纳版本时缺失。 */
  last_update_ms?: number | null;
  /** 生效版本嗅探到的格式（`clash_yaml` / `base64_links` / `plain_links` / `html` / `unknown`）。 */
  parser?: string | null;
  /** 最新一次尝试被坏更新拦下时的说明；有值 = 后端处理过、这次没好结果。 */
  error?: string | null;
};

/**
 * 订阅服务商在响应头 `subscription-userinfo` 里给的流量信息（docs/18 §1.2）。
 *
 * 注意：**未知一律 `null`，绝不用 0 冒充**：
 * - 字段缺失 / 非数字 → `null`；
 * - `total_bytes` 的 `0` = 不限量 / 未声明 → `null`（不是"总量为零"）；
 * - `expire_at_s` 的 `0` = 不适用 → `null`；
 * - 而 `upload_bytes` / `download_bytes` 的 `0` 是**真实观测值**（真没用过），保留 `0`。
 *
 * `expire_at_s` 是 **Unix 秒**（不是毫秒）；`fetched_at_ms` 是 Unix 毫秒 —— 界面上的
 * 「更新时间」用它（前端按本地时区格式化；总量与百分比的口径见 docs/18 §1.3，不自己发明）。
 *
 * 头**不存在**时整个对象是 `null`（"这次没给"）；头在但字段都读不懂时是
 * `{ …, fetched_at_ms }` 且各字段 `null`（"给了但读不懂"）—— 两者不是一回事。
 */
export type SubscriptionTraffic = {
  upload_bytes: number | null;
  download_bytes: number | null;
  total_bytes: number | null;
  expire_at_s: number | null;
  fetched_at_ms: number;
};

/** `subscription_traffic` 的一行；`traffic: null` = 这份订阅**从未成功刷新过**。 */
export type SubscriptionTrafficRow = {
  id: SubscriptionId;
  traffic: SubscriptionTraffic | null;
};

export type RefreshResult = {
  ok: boolean;
  node_count: number;
  group_count: number;
  error?: string | null;
  /**
   * 本次刷新拿到的流量（**没给头就没有这个字段** → 按 `null` 处理）。
   *
   * 只有刷新**单份**订阅时才有值：多份订阅的字节数不能相加，那种情况下没有这个字段，
   * 逐份的数字请用 `subscription_traffic` 读"上次已知"。
   * 没到期而跳过刷新时，这里回的是**上次已知**（本次没打机场，不该显示成"没有数据"）。
   */
  traffic?: SubscriptionTraffic | null;
};

export type NodeView = {
  name: string;
  type: string;
  group?: string | null;
  latency_ms?: number | null;
  ok?: boolean;
  /** 这条节点用户有没有关掉（默认 true = 启用；用户第 202 轮）。 */
  enabled: boolean;
};

export type PreviewResult = {
  groups: string[];
  nodes: string[];
  sample: NodeView[];
};

export type TrayState = {
  status: "ok" | "warn" | "error" | "off";
  tooltip: string;
  policy_name?: string | null;
  node?: string | null;
  latency_ms?: number | null;
};

export type ConnectivityRow = {
  credential_id: CredentialId;
  lane_id: LaneId;
  ok: boolean;
  latency_ms?: number | null;
  status?: number | null;
  note?: string | null;
};

export type OnboardingInput = {
  kernel_path?: string | null;
  providers: { id: ProviderId; name: string; protocol: Protocol; base_url: string; secret: string }[];
  subscriptions: { id: SubscriptionId; name: string; url: string }[];
};

// ================================================================ 用量 / 成本 / 日志（docs/10 §2.4、§2）

/**
 * 时间范围（查询用）。
 *
 * **线上形状以 Rust 实现为准**：`ta_core::clock::TimeRange`
 * （`crates/ta-core/src/clock.rs:77-88`）是 `#[serde(rename_all = "snake_case")]` 的
 * **外部标记枚举**（externally tagged），所以合法的 JSON 是：
 *
 * - 预设：**裸字符串** `"today"` / `"last7d"` / `"last30d"` / `"this_month"` / `"all"`
 * - 显式区间：`{ "explicit": { "from_ms": …, "to_ms": … } }`（左闭右开，Unix 毫秒）
 *
 * **不合法**（真机上 serde 直接报错，实测见 `src/app/argsShape.guard.test.ts` 的文件头）：
 * - `{ "preset": "last7d" }` → `unknown variant \`preset\`` —— 枚举里**没有 `preset` 这个变体**
 * - `{ "from": …, "to": … }` → `invalid value: map, expected map with a single key`
 *
 * 这两个错形状都曾经写在本文件里，且浏览器里因为 `mock.ts` 忽略参数而"看起来能用"。
 * 现在由 `src/app/argsShape.guard.test.ts` 对着 `clock.rs` 机械比对，防止再次漂移。
 */
export type TimeRange =
  | "today"
  | "last7d"
  | "last30d"
  | "this_month"
  | "all"
  | { explicit: { from_ms: number; to_ms: number } };

export type Num = { value: number; prev?: number };

export type DashboardData = {
  status_bar: {
    gateway: "running" | "paused";
    /**
     * 注意：**可选**：Rust 的 `StatusBar.policy` / `.lane` 是 `Option<…>` 且带
     * `skip_serializing_if = "Option::is_none"` —— 没有策略/线路时**整个字段不出现**
     * （crates/ta-store/src/usage_store.rs 的 `StatusBar`）。
     * 早期手写版把这两个字段声明成必填，于是 tsc 放过了 `status_bar.policy.name`，
     * 真机 release 包上首页因 `Cannot read properties of undefined (reading 'name')`
     * 被 ErrorBoundary 接住、整页显示"出错了"（2026-09-22 真机 E2E 抓到）。
     */
    policy?: { id: PolicyId; name: string } | null;
    lane?: { id: LaneId; name: string; node?: string | null; latency_ms?: number | null } | null;
  };
  cards: {
    requests: Num;
    tokens: Num;
    cost: { amount: number; currency: string; prev?: number };
    success_rate: number;
  };
  sparklines: {
    requests: number[];
    tokens: number[];
    cost: number[];
    success_rate: number[];
  };
  trend: { bucket_ts: number; input: number; input_cached: number; output: number; cost: number }[];
  lanes: { lane_id: LaneId; name: string; node?: string | null; latency_ms?: number | null; ok: boolean }[];
  /** 最近 500 条。 */
  live: RequestSummary[];
  top_models: { model: string; tokens: number; cost: number }[];
  cache: { hit_rate: number; saved_amount: number; currency: string };
  unpriced_models: string[];
};

export type UsageTotals = {
  requests: number;
  tokens: number;
  cost: Money;
  success_rate: number;
};

export type UsageQuery = {
  range: TimeRange;
  group_by: ("time" | "credential" | "provider" | "model" | "channel" | "client" | "lane" | "node")[];
  time_bucket?: "hour" | "day" | "week" | "month";
  metrics: ("requests" | "cost" | "cache_hit_rate" | "avg_latency" | TokenKind)[];
  filters?: {
    model?: string[];
    credential?: string[];
    provider?: string[];
    client?: string[];
    status?: ("ok" | "retried" | "failed")[];
  };
  format: "table" | "stacked_bar" | "area";
  limit?: number;
};

export type UsageRow = {
  /** 分组维度的取值（如 `{ model: "gpt-4o" }`）。 */
  dims: Record<string, string | number>;
  metrics: Record<string, number>;
};

export type UsageTable = {
  columns: string[];
  rows: UsageRow[];
  total: number;
};

export type LogQuery = {
  range: TimeRange;
  model?: string[];
  channel?: string[];
  credential?: string[];
  client?: string[];
  status?: ("ok" | "retried" | "failed")[];
  search?: string;
  cursor?: string | null;
  limit: number;
};

export type TraceRow = {
  request_id: RequestId;
  ts_ms: number;
  client?: ClientId | null;
  model: string;
  upstream_model: string;
  channel_id: ChannelId;
  credential_id: CredentialId;
  lane_id?: LaneId | null;
  node?: string | null;
  status: number;
  outcome: "ok" | "retried" | "failed";
  latency_ms: number;
  ttfb_ms?: number | null;
  total_tokens: number;
  cost: Money;
  attempts: number;
  error_type?: string | null;
};

export type LogStats = {
  total: bigint;
  ok: bigint;
  failed: bigint;
  avg_total_ms: number | null;
};
export type LogPage = {
  rows: TraceRow[];
  total: number;
  cursor?: string | null;
};

export type TraceAttempt = {
  channel_id: ChannelId;
  lane_id?: LaneId | null;
  node?: string | null;
  status?: number | null;
  ms: number;
  reason?: string | null;
};

export type TraceDetail = {
  row: TraceRow;
  attempts: TraceAttempt[];
  /**
   * [docs/32] L6：这次请求的**按 kind token 明细**（来自用量表）。
   * `null`/缺省 = 用量侧没有这笔账 → 界面显示「—」，**不拿 `row.tokens` 凑分项**。
   */
  token_kinds?: Record<string, number> | null;
  /** 自检：客户端发的头 vs 上游收到的头。**正常应恒为空**（非空 = 网关有 bug）。 */
  header_diff?: Record<string, { client?: string; upstream?: string }> | null;
  body_shape_in?: unknown;
  body_shape_out?: unknown;
};

export type StorageStats = {
  rows: number;
  bytes: number;
  oldest_ts?: number | null;
  newest_ts?: number | null;
};

export type ClearMode = "all" | "bodies_only";

export type ResolvedPrice = {
  rates: Record<string, number>;
  multiplier: number;
  currency: string;
  source: PriceSource | "none";
  plan_fee?: PlanFee | null;
};

// ================================================================ 策略模拟器（docs/10 §2.2）

export type SimulateInput = {
  model: string;
  has_image: boolean;
  stream: boolean;
  est_input_tokens: number;
  headers?: Record<string, string>;
  session_key?: string;
  now?: number;
  /**
   * 各线路在时间窗内的 token 用量（[docs/48 §3]）—— 模拟"平均粘性"要用的运行态输入。
   *
   * 省略 = **不知道**用量（选路退化为按 `priority`）；给了就按它挑最轻的那条。
   * 键是 `ChannelId`，值是累计 token 数。
   */
  balance?: Record<ChannelId, number>;
};

export type SimulateResult = {
  matched_policy?: { id: PolicyId; name: string } | null;
  candidates: { channel_id: ChannelId; ok: boolean; reason?: string | null }[];
  chosen?: { channel_id: ChannelId; sticky_hit: boolean; sticky_key?: string | null } | null;
  failover_chain: ChannelId[];
  lane?: { id: LaneId; name: string; node?: string | null; latency_ms?: number | null } | null;
  /** 恒为 true（除 model/auth 外，发出的请求与客户端发的一致）。 */
  outgoing_equals_client: boolean;
  eliminated: { channel_id: ChannelId; reason: string }[];
};

// ================================================================ 事件（与 Rust `TaEvent` 一致）

export type EventNameValue =
  | "request:new"
  | "usage:tick"
  | "channel:state"
  | "lane:health"
  | "kernel:status"
  | "config:changed"
  | "subscription:updated"
  | "log:append"
  | "toast"
  | "tray:refresh";

export type EventPayload = {
  /** 批量数组（200ms 合并，最多 50 条/次）。 */
  "request:new": { items: RequestSummary[] };
  "usage:tick": { today_tokens: number; today_cost: Money };
  "channel:state": { channel_id: ChannelId; state: string; until_ms?: number | null };
  "lane:health": { lane_id: LaneId; node?: string | null; latency_ms?: number | null; ok: boolean };
  "kernel:status": { state: KernelState; version?: string | null };
  "config:changed": { source: "db" | "file"; applied: boolean };
  "subscription:updated": { id: SubscriptionId; ok: boolean; node_count: number; error?: string | null };
  "log:append": { items: unknown[] };
  toast: { level: "info" | "warn" | "error"; title: string; body: string; dedupe_key: string };
  "tray:refresh": { payload: unknown };
};

// ================================================================ 命令签名（docs/10 §2.2）

/**
 * 命令 → (入参, 返回值)。`api/commands.ts` 与 `api/mock.ts` 都按这张表对齐，
 * 所以**命令名或入参写错会被 tsc 拦下**。
 *
 * 注意：这是一张**手写契约**，**不等于 Rust 已经实现的命令面**。
 * 条数的唯一权威锚点是 `crates/ta-core/tests/command_surface_guard.rs` 的 `ARCHIVE_TOTAL`
 * （以及同目录的归档件 `docs/contracts/bindings-handwritten-reference.ts.txt`）——
 * **这个注释里不写具体数字**：数字会过期，而它以前就过期过三次。
 *
 * 已知的"有意推迟"命令（理由与解锁条件见 `src-tauri/src/commands/entity.rs` 的模块文档）：
 * **只剩 `provider_echo` 一条**（它要读上游响应体做"谁吞了字段"的逐字段回显，本轮没做）。
 * 曾与它并列推迟的三条已在 [docs/19] 的 GW-71 / DESK-71 里落地并注册：
 * `provider_test`（测试连接）、`model_import_from_provider`（一键扫描）、`model_apply_import`（添加选中）。
 * 另：本轮新增的 `api_key_*` 五条也已注册（`docs/19 §3`）。
 * 新增的 `api_key_*` 五条同理：先落契约，等 `src-tauri` 注册后页面才可引用。
 *
 * 机械保证：
 * - `src/app/commandSurface.guard.test.ts`：页面/外壳引用的每条命令都必须在 Rust 注册表里；
 * - `src/app/registryClaim.guard.test.ts`：源码里凡说"某某命令尚未注册"的，那条命令必须真的没注册
 *   （防止注册表前进后，页面继续对用户说"后端还没有"）；
 * - 契约全量与注册表的对齐由 E2 的 `command_surface_guard.rs` 负责，两边各管一段。
 * 上面那份 31 条清单是**快照**，权威始终是 `src-tauri/src/commands/mod.rs` 的 `collect_commands!`。
 */
export type CommandMap = {
  // ---- app
  app_status: { args: undefined; result: AppStatus };
  app_pause_gateway: { args: undefined; result: AppStatus };
  app_resume_gateway: { args: undefined; result: AppStatus };
  app_open_dir: { args: { kind: "logs" | "config" | "data" }; result: void };
  app_clipboard_access_info: { args: undefined; result: string };
  app_check_update: { args: undefined; result: UpdateInfo | null };
  app_get_autostart: { args: undefined; result: AutostartState };
  app_set_autostart: {
    args: { enabled: boolean; mode?: AutostartMode; delay_s?: number };
    result: AutostartState;
  };
  app_repair_autostart: { args: undefined; result: AutostartState };

  // ---- config
  config_get: { args: undefined; result: ConfigDoc };
  config_validate: { args: { doc: ConfigDoc }; result: ValidationReport };
  config_apply: { args: { doc: ConfigDoc }; result: ApplyResult };
  config_export_yaml: { args: { with_secrets: boolean }; result: string };
  config_import_yaml: { args: { yaml: string; mode: "merge" | "replace" }; result: ApplyResult };
  config_history_list: { args: { limit: number }; result: Snapshot[] };
  config_rollback: { args: { snapshot_id: string }; result: ApplyResult };
  config_set_file_watch_mode: { args: { enabled: boolean; path?: string }; result: ApplyResult };

  // ---- provider
  provider_list: { args: undefined; result: ProviderView[] };
  provider_get: { args: { id: ProviderId }; result: ProviderView };
  provider_upsert: { args: { provider: ProviderInput }; result: Provider };
  provider_delete: { args: { id: ProviderId; force: boolean }; result: void };
  provider_test: { args: { id: ProviderId; model?: string }; result: ProbeResult };
  provider_echo: {
    args: { id: ProviderId; model: string; with_image: boolean };
    result: EchoResult;
  };

  // ---- credential / secrets
  credential_list: { args: undefined; result: CredentialView[] };
  credential_upsert: { args: { credential: Credential }; result: Credential };
  credential_set_secret: { args: { id: CredentialId; plaintext: string }; result: void };
  credential_set_enabled: { args: { id: CredentialId; enabled: boolean }; result: void };
  secrets_scan_plaintext: { args: undefined; result: PlaintextFinding[] };
  secrets_migrate: { args: { names: string[] }; result: void };

  // ---- model
  model_list: { args: undefined; result: ModelView[] };
  model_upsert: { args: { model: Model }; result: Model };
  model_delete: { args: { id: ModelId; force: boolean }; result: void };
  model_import_from_provider: {
    args: { provider_id: ProviderId; prefix_filter?: string };
    result: ImportPreview;
  };
  model_apply_import: {
    args: { preview_token: string; selected: string[]; lane?: LaneId | null };
    result: ApplyResult;
  };
  model_bulk_update: { args: { ids: ModelId[]; patch: ModelPatch }; result: ApplyResult };
  /**
   * 这家服务商的**模型目录**（[docs/25]）：扫描记录下来的全部上游模型。
   *
   * 目录是"扫描到过什么"的观察记录（SQLite，不进配置）；`added` 才是"接入了没有"。
   * 界面据此画那一列勾选框 —— 勾 = 接入（`provider_model_attach`），取消 = 停用通道。
   */
  provider_model_catalog: { args: { provider_id: ProviderId }; result: ModelCatalogEntry[] };
  /**
   * 把目录里的这一条**接到**这家服务商上（[docs/25]）：建 `Model`（若缺）+ 建启用通道。
   *
   * 只给 `upstream_model`：逻辑名从**目录行**里取（单一真源，界面没有机会传错）。
   * 目录里没有这一条 → `TA-1004`（「先扫描一次」）；已经接入过 → 幂等。
   */
  provider_model_attach: {
    args: { provider_id: ProviderId; upstream_model: string };
    result: ApplyResult;
  };
  /**
   * [docs/28 §1.4] 「添加服务商就把**所有**线路建出来」：把上游目录里每条都建成线路，
   * **默认关闭**（`enabled: false`），之后无论向导还是详情都只用滑块控制启用/关闭。
   *
   * 幂等：已经有线路的条目**一个字节都不动**（不覆盖用户改过的 lane / enabled / 优先级）；
   * 一条都没补时**不写库**（`created: 0` 如实说"这次一条都没建"）。仍然**不建模型**。
   */
  provider_lines_create_all: {
    args: { provider_id: ProviderId; lane?: LaneId | null };
    result: LinesCreateAllResult;
  };

  // ---- channel / group
  // Rust 侧类型名是 `ChannelConfig`（crates/ta-core/src/domain/mod.rs:1009），与 TS 的 `Channel`
  // 是同一实体（字段逐一对应；手写契约用的是后者这个名字）。
  channel_list: { args: undefined; result: Channel[] };
  channel_matrix: { args: undefined; result: MatrixView };
  channel_upsert: { args: { channel: Channel }; result: Channel };
  channel_delete: { args: { id: ChannelId }; result: void };
  channel_set_enabled: { args: { id: ChannelId; enabled: boolean }; result: void };
  // [docs/55] `channel_bulk_assign_lane` 已删除：线路分配只剩**服务商级**一处
  //（用户："线路级别的分配逻辑我们已经放弃了，取而代之的，每个服务商统一一个节点"），
  // 而 `ChannelConfig.lane` 字段本身也删了 —— 这条命令已经没有可写的地方。
  /**
   * [docs/28 §1.4] 把一批线路**一起启用或一起关闭**（列表里的"全选"）。
   *
   * 语义就是 `channel_set_enabled` 的批量版（可逆，不删任何东西）。做成一条命令而不是前端循环：
   * 全选是**一次用户动作**，循环会让"改了 200 条"变成 200 个配置快照 + 200 次内核重载，
   * 中途失败还会留下半开半关的状态。
   */
  channel_bulk_set_enabled: {
    args: { channel_ids: ChannelId[]; enabled: boolean };
    result: ApplyResult;
  };
  channel_set_tags: {
    args: { channel_ids: ChannelId[]; add: string[]; remove: string[] };
    result: ApplyResult;
  };
  group_list: { args: undefined; result: ChannelGroup[] };
  group_upsert: { args: { group: ChannelGroup }; result: ChannelGroup };
  group_delete: { args: { id: ChannelGroupId }; result: void };
  group_set_sticky: { args: { id: ChannelGroupId; sticky: StickyConfig }; result: void };
  group_release_sticky: { args: { group_id: ChannelGroupId; key?: StickyKey }; result: void };

  // ---- policy
  policy_list: { args: undefined; result: Policy[] };
  policy_upsert: { args: { policy: Policy }; result: Policy };
  policy_delete: { args: { id: PolicyId }; result: void };
  policy_reorder: { args: { ordered_ids: PolicyId[] }; result: void };
  policy_set_enabled: { args: { id: PolicyId; enabled: boolean }; result: void };
  policy_simulate: { args: { input: SimulateInput }; result: SimulateResult };

  // ---- lane / subscription
  lane_list: { args: undefined; result: LaneView[] };
  lane_upsert: { args: { lane: Lane }; result: Lane };
  lane_delete: { args: { id: LaneId }; result: void };
  lane_test: { args: { id: LaneId; node?: string }; result: LaneTestResult[] };
  lane_set_node: { args: { id: LaneId; node: string }; result: void };
  // [docs/55] `lane_assignments` / `lane_reverse_lookup` / `lane_pick_options` 已删除。
  //
  // 它们只服务于"**线路级别的分配**"：把五级继承链（策略 → 通道 → 套餐 → 服务商 → 全局默认）
  // 算出来给界面看。用户裁决放弃那套逻辑（"取而代之的，每个服务商统一一个节点"）之后：
  //   * 生效线路只剩 `provider.lane ?? net.default_lane ?? direct`，没有"来自哪一级"可报；
  //   * 展示它们的页面（`/network/assignments`）整页删除，三条命令**没有任何调用方**。
  //
  // 留着它们只会返回一条恒为 `provider`/`default` 的"来源"—— 那是个过期的说法，比删掉更容易误导。
  /**
   * [docs/28 §1.2] 线路（网络代理）下拉的可选来源：**已配置线路 + 订阅节点**（按订阅分组）。
   * 节点那一项选中后要配合 `lane_ensure_for_node`。
   *
   * [docs/55] 这是**唯一**给一条线路选网络的地方（原来还有个 `lane_pick_options`
   * 给指派页用，随那页一起删了）。
   */
  lane_options: { args: undefined; result: LaneChoiceGroup[] };  /**
   * [docs/28 §1.2] 让"这个订阅的这个节点"在配置里有一条线路，返回那条线路（**幂等**：
   * 已有等价线路就复用）。节点必须真的在已采纳的订阅版本里，否则原样报错。
   */
  lane_ensure_for_node: {
    args: { subscription_id: SubscriptionId; node: string };
    result: Lane;
  };
  /**
   * [docs/28 §1.2] 让配置里**有一条直连线路**，返回那条线路（**幂等**：已有 `kind: direct`
   * 的线路就复用）。
   *
   * 为什么需要它：「直连」是网络代理里最重要的选项，而下拉里要能**明确选中**它 ——
   * 不是只能选一个叫"继承"的东西（继承会跟着凭据 / 服务商 / 默认线路走，
   * 而直连是把它钉死在本机网络上）。
   */
  lane_ensure_direct: { args: undefined; result: Lane };
  subscription_list: { args: undefined; result: SubscriptionView[] };
  subscription_upsert: { args: { subscription: Subscription }; result: Subscription };
  subscription_delete: { args: { id: SubscriptionId }; result: void };
  subscription_refresh: { args: { id?: SubscriptionId; force?: boolean }; result: RefreshResult };
  subscription_preview: {
    args: { url: string; user_agent?: string; format?: SubscriptionFormat };
    result: PreviewResult;
  };
  subscription_groups: {
    args: { id: SubscriptionId };
    result: { name: string; type: GroupType; node_count: number }[];
  };
  subscription_nodes: { args: { id: SubscriptionId; group?: string }; result: NodeView[] };
  /**
   * 批量设置若干节点的启用状态（用户第 202 轮：节点级开关 + 全选，默认全选）。
   * `nodes` 为空清单 → 后端**报错**（空清单不是「全部」）；返回**改完之后**的清单。
   */
  subscription_nodes_enabled: {
    args: { id: SubscriptionId; nodes: string[]; enabled: boolean };
    result: NodeView[];
  };
  subscription_rollback: { args: { id: SubscriptionId; version_index: number }; result: void };
  subscription_rules_raw: { args: { id: SubscriptionId }; result: string | null };
  subscription_traffic: { args: undefined; result: SubscriptionTrafficRow[] };

  // ---- kernel
  kernel_status: { args: undefined; result: KernelStatus };
  kernel_start: { args: undefined; result: KernelStatus };
  kernel_stop: { args: undefined; result: KernelStatus };
  kernel_restart: { args: undefined; result: KernelStatus };
  kernel_reload_config: { args: undefined; result: KernelStatus };
  kernel_generated_config: { args: undefined; result: string };
  kernel_log_tail: { args: { lines: number }; result: string };
  kernel_set_provider: {
    args: { provider: KernelProvider; path?: string };
    result: KernelStatus;
  };
  kernel_probe_path: { args: undefined; result: { found: boolean; path?: string | null; version?: string | null } };

  // ---- usage / pricing / log
  usage_dashboard: { args: { range: TimeRange }; result: DashboardData };
  usage_request_stream_snapshot: { args: { limit: number }; result: RequestSummary[] };
  usage_query: { args: { query: UsageQuery }; result: UsageTable };
  usage_export_csv: { args: { query: UsageQuery; path: string }; result: void };
  usage_token_kinds: { args: undefined; result: string[] };
  pricing_list: { args: undefined; result: PriceEntry[] };
  pricing_upsert: { args: { entry: PriceEntry }; result: PriceEntry };
  pricing_delete: { args: { index: number }; result: void };
  pricing_resolve: {
    args: { model: string; provider?: ProviderId; credential?: CredentialId };
    result: ResolvedPrice;
  };
  pricing_import_builtin: { args: undefined; result: ApplyResult };
  pricing_unpriced_models: { args: undefined; result: string[] };
  log_query: { args: { query: LogQuery }; result: LogPage };
  log_stats: { args: { query: LogQuery }; result: LogStats };
  /**
   * 日志查询能力位（[06 §10.2 D2]）。
   *
   * 形状与 Rust 侧 `LogQueryCapabilities`（src-tauri/src/commands/log.rs:76）一致：
   * `CapabilitySupport { enabled: bool, reason: Option<String> }` → `reason` 为 `string | null`，
   * **enabled = true 时 reason 为 null**。UI 的消费契约：`enabled === false` 时
   * 该筛选项禁用并把 `reason` **原文**显示出来（`DisabledReason`/`DisabledMenuItem`），
   * `outcome` 不支持时必须显示"未知"而不是"正常"（`outcomeDisplay`）。
   */
  log_query_capabilities: {
    args: undefined;
    result: {
      retried: { enabled: boolean; reason: string | null };
      outcome: { enabled: boolean; reason: string | null };
    };
  };
  log_get_trace: { args: { request_id: RequestId }; result: TraceDetail };
  log_set_mode: { args: { mode: LogMode }; result: void };
  log_set_retention: { args: { retention: Retention }; result: void };
  log_storage_stats: { args: undefined; result: StorageStats };
  log_clear: { args: { mode: ClearMode }; result: void };
  log_export: { args: { query: LogQuery; path: string }; result: LogExportReceipt };

  // ---- api key（[docs/19 §3]：我们网关对外的访问密钥）
  api_key_list: { args: undefined; result: ApiKeyView[] };
  api_key_create: { args: { name: string; note?: string | null }; result: ApiKeyCreated };
  api_key_set_enabled: { args: { id: string; enabled: boolean }; result: ApiKeyView };
  api_key_delete: { args: { id: string }; result: void };
  api_key_usage: { args: { id?: string | null; limit: number }; result: ApiKeyUsageRow[] };

  // ---- onboarding / tray
  onboarding_detect: {
    args: undefined;
    result: { kernel_found: boolean; existing_config: boolean; suggested_providers: string[] };
  };
  onboarding_apply: { args: { input: OnboardingInput }; result: ApplyResult };
  onboarding_test_all: { args: undefined; result: ConnectivityRow[] };
  tray_state: { args: undefined; result: TrayState };
  tray_set_active_policy: { args: { policy_id: PolicyId }; result: void };
  tray_set_global_lane: { args: { lane_id: LaneId }; result: void };
  tray_emergency: {
    args: { mode: "direct_all" | "pause_all" | "vision_only" | "restore" };
    result: void;
  };
};

export type CommandName = keyof CommandMap;
export type CommandArgs<K extends CommandName> = CommandMap[K]["args"];
export type CommandResult<K extends CommandName> = CommandMap[K]["result"];

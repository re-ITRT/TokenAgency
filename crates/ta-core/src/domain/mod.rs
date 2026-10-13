//! 领域模型（配置实体）。
//!
//! 字段与 `config.example.yaml` 一一对应。**改这里必须同步改接口文档 10/11 与示例配置。**
//!
//! 约定：
//! - 所有可选字段 `Option<T>` + `#[serde(default)]`，保证老配置能读新版本。
//! - 未知字段用 `#[serde(flatten)] extra: Map<String, Value>` **保留**（前向兼容）。
//! - 不在这里做 IO；校验逻辑放 `crate::config`。

/// 领域类型的**行为**：便捷构造函数、只读辅助、线路解析优先级、模型名 glob。
/// 契约在 `domain/mod.rs`（冻结）；行为在 `domain::behavior`（可扩展）。
pub mod behavior;

///「能力一致才可路由」的纯判定层（比较 + 按 model 分组求差异）。
pub mod capabilities;

/// 上游 `/models` 里**声明过的**能力（上下文窗口 / 最大输出 / 模态 / 工具），
/// 从我们逐字存下来的扫描原文里读出来 —— 线路"认领"它，网关再把它透传给客户端。
pub mod upstream_hints;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::*;

fn default_true() -> bool {
    true
}
fn default_one() -> f64 {
    1.0
}

// ================================================================ 根配置

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDoc {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub request_log: RequestLogConfig,
    #[serde(default)]
    pub pricing: PricingConfig,
    #[serde(default)]
    pub net: NetConfig,
    #[serde(default)]
    pub proxy_subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub lanes: Vec<Lane>,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub credentials: Vec<Credential>,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
    #[serde(default)]
    pub channel_groups: Vec<ChannelGroup>,
    #[serde(default)]
    pub policies: Vec<Policy>,
    #[serde(default)]
    pub system_integration: SystemIntegration,
    /// 保留未知字段，前向兼容
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_version() -> u32 {
    // 必须与 `config::migrate::CURRENT_CONFIG_VERSION` 一致（那条隐式契约由
    // `migrate.rs` 的 `current_version_matches_domain_default` 守着）。
    // v2 = 的关系翻转：`ChannelConfig.model` → `Model.lines`。
    // v3 =：放弃「订阅里分组」，`subscription_group` → 「自动选择」（显式 select）。
    3
}

/// 「自动」线路的**幂等建线决策**（D 期）。
///
/// 纯值：**不动配置、不落库** —— 调用方照它去复用或新建。抽出来的理由是决策里全是边界
/// （要不要复用、id 撞了怎么办），而这些边界靠"跑一次真机"是验不全的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoLanePlan {
    /// 已经有一条够格的「自动」线路 → 直接用它（**绝不新建第二条**）。
    Reuse(LaneId),
    /// 没有 → 按这个 id 与名字建一条 `Lane::automatic`。
    Create { id: LaneId, name: String },
}

impl ConfigDoc {
    /// 计划一条挂在这份订阅上的「自动」线路（[`AutoLanePlan`]）。
    ///
    /// `base_id`：希望用的 id —— 现在由**服务商**派生（`lane-<服务商>-auto`），因为
    /// 「自动」是**每家服务商自己的**出口（用户第 202 轮原话：「每一个服务商都有自己的『自动』节点，
    /// 相当于这个选哪个节点是记录在每一个服务商自身的」）。
    ///
    /// ★ **按 `base_id` 复用，不再"按订阅找第一条"**：旧规则会让两家服务商**共用同一条** lane
    /// ⇒ 各自按自己存的地址重算、写同一个组 ⇒ 组上的 `now` 变成"最后写的那家"的选择
    /// （真机实测：一家写"沿用 日本-优化2"、另一家写"剩余流量"）。
    /// 幂等由调用方保证：服务商**当前挂的那条**够格就复用它（`ensure_auto_lane` 的第 ① 步）。
    ///
    /// 如果 `base_id` 已经被**别的**线路占了，就在后面加 `-2`、`-3`… 直到空位 ——
    /// **绝不改别人已有的线路 id**（改 id 会让引用它的服务商指向不存在的线路，那是配置损坏，
    /// 不是"顺手重命名"）。
    pub fn plan_auto_lane(
        &self,
        subscription: &SubscriptionId,
        base_id: &str,
        name: &str,
    ) -> AutoLanePlan {
        if let Some(existing) = self.lanes.iter().find(|l| l.id.as_str() == base_id) {
            if auto_lane_qualifies(existing, subscription) {
                return AutoLanePlan::Reuse(existing.id.clone());
            }
        }
        let mut candidate = base_id.to_string();
        let mut n = 2;
        while self.lanes.iter().any(|l| l.id.as_str() == candidate) {
            candidate = format!("{base_id}-{n}");
            n += 1;
        }
        AutoLanePlan::Create { id: LaneId::new(candidate), name: name.to_string() }
    }

    /// **哪条线路够格当这家订阅的「自动」出口**（D 期幂等建线用）。
    ///
    /// 判据两条缺一不可：种类是 `subscription_auto`，**且组类型是显式 `select`** ——
    /// 只看种类会把老的「整份订阅择优」（`url-test`，会自己周期性换出口）当成「自动」，
    /// 那就是把用户不想要的东西冒充他要的那个。
    ///
    /// ⚠️ **这是"这条订阅上的第一条够格的"**，只用于只读判断/诊断。
    /// 建线**不要**用它（会把两家服务商的「自动」认成同一条）：用 [`ConfigDoc::plan_auto_lane`]
    /// + 服务商当前挂的 lane（`catalog::ensure_auto_lane`）。
    ///
    /// 多条同时满足时取**配置里靠前的**那条（确定性：同一份配置永远给同一个答案）。
    pub fn auto_lane_on(&self, subscription: &SubscriptionId) -> Option<&Lane> {
        self.lanes.iter().find(|lane| auto_lane_qualifies(lane, subscription))
    }

    /// 显式按 **id** 找一条够格的「自动」线路（同订阅 + `subscription_auto` + 显式 `select`）。
    pub fn auto_lane_by_id<'a>(
        &'a self,
        id: &LaneId,
        subscription: &SubscriptionId,
    ) -> Option<&'a Lane> {
        self.lanes.iter().find(|l| &l.id == id && auto_lane_qualifies(l, subscription))
    }
}

/// `lane` 够不够格当这份订阅的「自动」出口（`subscription_auto` + **显式** `select` + 订阅对得上）。
///
/// 只写一份：`plan_auto_lane` / `auto_lane_on` / `auto_lane_by_id` 三处**必须**同一个判据，
/// 否则"能找到"和"能复用"会分叉（一边认、一边不认 ⇒ 越用越多条）。
fn auto_lane_qualifies(lane: &Lane, subscription: &SubscriptionId) -> bool {
    lane.kind == LaneKind::SubscriptionAuto
        && lane.group_type == Some(GroupType::Select)
        && lane.subscription.as_ref() == Some(subscription)
}
impl Default for ConfigDoc {
    fn default() -> Self {
        Self {
            version: default_version(),
            gateway: Default::default(),
            request_log: Default::default(),
            pricing: Default::default(),
            net: Default::default(),
            proxy_subscriptions: vec![],
            lanes: vec![],
            providers: vec![],
            credentials: vec![],
            models: vec![],
            channels: vec![],
            channel_groups: vec![],
            policies: vec![],
            system_integration: Default::default(),
            extra: Map::new(),
        }
    }
}

// ================================================================ 网关

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GatewayConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_admin_listen")]
    pub admin_listen: String,
    /// 可选本地密钥；空/None 则不校验
    #[serde(default)]
    pub local_key: Option<String>,
    #[serde(default = "default_max_body_mb")]
    pub max_body_mb: usize,
    /// 是否返回 `x-tokenagency-*` 调试头
    #[serde(default = "default_true")]
    pub debug_headers: bool,
    /// 通配路径时兜底用哪个 provider（无法确定模型名时）
    #[serde(default)]
    pub fallback_provider: Option<ProviderId>,
    #[serde(default)]
    pub passthrough: PassthroughConfig,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub metering: MeteringConfig,
    /// **下游 API 家族开关**（用户第 326 轮："是打开关闭形式，按需求勾选"）。
    #[serde(default)]
    pub downstream: DownstreamConfig,
    /// **已删除的旧开关**：`expose_models_with_capabilities`。
    ///
    /// 它当年表达"要不要在 `/v1/models` 里注入上游能力标签"；现在 `/v1/models` 返回的是
    /// **我们自己的模型**（[`Model`]），能力字段一律来自我们记录的上游原文、没有开关这回事。
    /// 老配置里带着它**仍然能解析** —— 未知字段一律落进这里的 `extra` 原样保留（仓库一贯的
    /// 前向兼容纪律），但它**不再有任何运行时含义**。**别**把它再当开关读。
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_listen() -> String {
    "127.0.0.1:8787".into()
}
fn default_admin_listen() -> String {
    "127.0.0.1:8788".into()
}
fn default_max_body_mb() -> usize {
    32
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            admin_listen: default_admin_listen(),
            local_key: None,
            max_body_mb: default_max_body_mb(),
            debug_headers: true,
            fallback_provider: None,
            passthrough: Default::default(),
            retry: Default::default(),
            metering: Default::default(),
            downstream: Default::default(),
            extra: Map::new(),
        }
    }
}

/// **下游对外提供哪几种 API 家族** —— 每个一批（勾选式开关）。
///
/// ## 为什么要有它
///
/// 用户第 326 轮的需求原话："可以在设置页面选择我们下游是统一成 OPENAI 的两种还是
/// Anthropic 的"，补一句"是打开关闭形式，按需求勾选"。
///
/// 所以这不是"选一个"，而是**三个独立的开关**：客户端用哪套 API 由用户自己按需勾。
///
/// ## 关掉之后的行为
///
/// 关掉的端点**当场明确报错**（`TA-1004` + 指明是哪个开关），而不是被静默转发到上游 ——
/// "配错了客户端却收到一个来自上游的 404"是最难查的一类问题。
///
/// ## 为什么默认全开
///
/// 老配置里**没有**这个字段（`#[serde(default)]` → 走 `Default`），而升级前三种端点都是通的。
/// 默认全开 = 升级不改变任何既有行为；要收窄是**用户显式勾掉**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownstreamConfig {
    /// `POST /v1/chat/completions`（OpenAI 兼容客户端最常用的一种）
    #[serde(default = "default_true")]
    pub openai_chat: bool,
    /// `POST /v1/responses`（OpenAI 新的 Responses API）
    #[serde(default = "default_true")]
    pub openai_responses: bool,
    /// `POST /v1/messages`（Anthropic / Claude Code）
    #[serde(default = "default_true")]
    pub anthropic_messages: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for DownstreamConfig {
    fn default() -> Self {
        Self {
            openai_chat: true,
            openai_responses: true,
            anthropic_messages: true,
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PassthroughConfig {
    /// **默认且推荐 `Pass`**：客户端头整体转发，只做 4 处必须的修改
    #[serde(default)]
    pub header_mode: HeaderMode,
    #[serde(default)]
    pub body: BodyMode,
    #[serde(default = "default_true")]
    pub strip_hop_by_hop: bool,
    /// 自检：记录"客户端发的头"与"上游收到的头"的差异（非空 = 网关有 bug）
    #[serde(default = "default_true")]
    pub verify_no_diff: bool,
    /// 跨协议转换出现字段损失时的行为
    #[serde(default)]
    pub on_loss: OnLoss,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for PassthroughConfig {
    fn default() -> Self {
        Self {
            header_mode: HeaderMode::Pass,
            body: BodyMode::Preserve,
            strip_hop_by_hop: true,
            verify_no_diff: true,
            on_loss: OnLoss::Reject,
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeaderMode {
    #[default]
    Pass,
    /// 白名单——**会吞字段**，仅为"让人确信自己没被强制透传"而保留
    Allowlist,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyMode {
    #[default]
    Preserve,
    Strip,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnLoss {
    #[default]
    Reject,
    Warn,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// 流式默认更少（客户端等太久体验差）
    #[serde(default = "default_stream_max_attempts")]
    pub stream_max_attempts: u32,
    /// **恒为 false**：已向下游写出字节后绝不重试
    #[serde(default)]
    pub retry_after_first_byte: bool,
    #[serde(default = "default_cooldown_s")]
    pub cooldown_s: u32,
    #[serde(default)]
    pub backoff: Backoff,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_max_attempts() -> u32 {
    3
}
fn default_stream_max_attempts() -> u32 {
    2
}
fn default_cooldown_s() -> u32 {
    120
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            stream_max_attempts: 2,
            retry_after_first_byte: false,
            cooldown_s: 120,
            backoff: Default::default(),
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Backoff {
    None,
    #[default]
    Exponential,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MeteringConfig {
    /// 注意：开启会**修改客户端请求**（注入 `stream_options.include_usage`），默认关闭
    #[serde(default)]
    pub inject_stream_usage: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for MeteringConfig {
    fn default() -> Self {
        Self { inject_stream_usage: false, extra: Map::new() }
    }
}

// ================================================================ 请求日志

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RequestLogConfig {
    #[serde(default)]
    pub mode: LogMode,
    #[serde(default)]
    pub retention: RetentionDefault,
    #[serde(default = "default_max_rows")]
    pub max_rows: u64,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
    #[serde(default = "default_one")]
    pub sample_rate: f64,
    #[serde(default)]
    pub image_handling: ImageHandling,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_max_rows() -> u64 {
    500_000
}
fn default_max_size_mb() -> u64 {
    2048
}

/// `Retention` 的默认值是 30d，单独包一层以便 `Default`
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionDefault {
    #[serde(rename = "7d")]
    D7,
    /// 默认保留 30 天（见 [02 §3] 的保留策略）
    #[default]
    #[serde(rename = "30d")]
    D30,
    #[serde(rename = "90d")]
    D90,
    Forever,
}
impl From<RetentionDefault> for Retention {
    fn from(v: RetentionDefault) -> Self {
        match v {
            RetentionDefault::D7 => Retention::D7,
            RetentionDefault::D30 => Retention::D30,
            RetentionDefault::D90 => Retention::D90,
            RetentionDefault::Forever => Retention::Forever,
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageHandling {
    /// **只记 {type, media_type, bytes, sha256_prefix}，绝不记 base64**
    #[default]
    Hash,
    Full,
}

impl Default for RequestLogConfig {
    fn default() -> Self {
        Self {
            mode: LogMode::Meta,
            retention: RetentionDefault::D30,
            max_rows: default_max_rows(),
            max_size_mb: default_max_size_mb(),
            sample_rate: 1.0,
            image_handling: ImageHandling::Hash,
            extra: Map::new(),
        }
    }
}

// ================================================================ 网络

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NetConfig {
    #[serde(default)]
    pub kernel: KernelConfig,
    /// `kernel`（默认）| `builtin`（自研拨号，未实现，留后路）
    #[serde(default)]
    pub dialer: DialerKind,
    #[serde(default)]
    pub dns: DnsConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub default_lane: Option<LaneId>,
    #[serde(default)]
    pub system_proxy: bool,
    #[serde(default)]
    pub tun: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            kernel: Default::default(),
            dialer: DialerKind::Kernel,
            dns: Default::default(),
            health: Default::default(),
            default_lane: None,
            system_proxy: false,
            tun: false,
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DialerKind {
    #[default]
    Kernel,
    Builtin,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelConfig {
    #[serde(default)]
    pub provider: KernelProvider,
    /// `provider: external` 时的内核路径；空则用首启向导的查找结果
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_controller")]
    pub controller: String,
    #[serde(default = "default_lane_range")]
    pub lane_port_range: [u16; 2],
    /// 订阅数据来源：`kernel` 让内核自己拉（默认，避免双份请求）；`app` 由我们拉后落盘
    #[serde(default)]
    pub subscription_source: SubscriptionSource,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_controller() -> String {
    "127.0.0.1:39090".into()
}
fn default_lane_range() -> [u16; 2] {
    [39100, 39299]
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KernelProvider {
    /// 默认：用户自行获取内核，不分发，规避 GPL 分发问题
    #[default]
    External,
    Bundled,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionSource {
    #[default]
    Kernel,
    App,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            provider: KernelProvider::External,
            path: None,
            controller: default_controller(),
            lane_port_range: default_lane_range(),
            subscription_source: SubscriptionSource::Kernel,
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DnsConfig {
    #[serde(default = "default_enhanced_mode")]
    pub enhanced_mode: String,
    #[serde(default = "default_nameservers")]
    pub nameserver: Vec<String>,
    /// 注意：节点自身域名解析，必须配，否则节点都连不上
    #[serde(default = "default_nameservers")]
    pub proxy_server_nameserver: Vec<String>,
    #[serde(default = "default_default_ns")]
    pub default_nameserver: Vec<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_enhanced_mode() -> String {
    "redir-host".into()
}
fn default_nameservers() -> Vec<String> {
    vec!["https://223.5.5.5/dns-query".into(), "https://doh.pub/dns-query".into()]
}
fn default_default_ns() -> Vec<String> {
    vec!["223.5.5.5".into(), "119.29.29.29".into()]
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            enhanced_mode: default_enhanced_mode(),
            nameserver: default_nameservers(),
            proxy_server_nameserver: default_nameservers(),
            default_nameserver: default_default_ns(),
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthConfig {
    #[serde(default = "default_health_interval")]
    pub interval_s: u32,
    /// 用各 provider 的真实端点测速，而不是 generate_204
    #[serde(default = "default_true")]
    pub url_by_provider: bool,
    #[serde(default)]
    pub probe_url: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_health_interval() -> u32 {
    60
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self { interval_s: 60, url_by_provider: true, probe_url: None, extra: Map::new() }
    }
}

// ================================================================ 订阅

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub name: String,
    pub url: String,
    /// 注意：很多机场按 UA 返回不同格式，默认伪装 Clash 客户端
    #[serde(default = "default_subscription_ua")]
    pub user_agent: String,
    #[serde(default)]
    pub format: SubscriptionFormat,
    #[serde(default = "default_subscription_interval")]
    pub interval_s: u32,
    #[serde(default = "default_keep_versions")]
    pub keep_versions: u32,
    #[serde(default)]
    pub filter: SubscriptionFilter,
    #[serde(default)]
    pub health_check: SubscriptionHealthCheck,
    /// 省略 = 启用：**被列进配置就等价于"我要用它"**，停用必须显式写 `false`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 内核里的 provider key（由 id 派生，稳定）
    #[serde(default)]
    pub provider_key: Option<String>,
    /// **用户关掉的节点名**（省略/空 = 全部启用）。
    ///
    /// 用户第 202 轮：「订阅里面的节点，也将服务商的『模型』一样设置一个启用或关闭的功能」+
    /// 「这个也要那种有全选功能，默认全部选择」。所以存的是**关掉的那些**：
    /// 新节点（机场新增）默认就是启用的，不需要用户回来补勾 —— 与 `enabled`（省略 = 启用）
    /// 是同一套哲学。被关掉的节点**不进「自动」的候选**（`apply_auto_export`），
    /// 于是订阅里那几条展示用的信息节点（`剩余流量：…` / `套餐到期…` / `过滤掉 N 条线路`）
    /// 用户自己就能排除，而不用我们在代码里猜哪个名字是"真节点"。
    #[serde(default)]
    pub disabled_nodes: Vec<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_subscription_ua() -> String {
    "clash-verge/v2.0.0".into()
}
fn default_subscription_interval() -> u32 {
    6 * 3600
}
fn default_keep_versions() -> u32 {
    3
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionFormat {
    #[default]
    Auto,
    Clash,
    Base64Links,
    PlainLinks,
    /// 业务层把链接列表转成 Clash YAML 再给内核（兜底）
    ConvertLinks,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SubscriptionFilter {
    #[serde(default)]
    pub include: Option<String>,
    #[serde(default)]
    pub exclude: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SubscriptionHealthCheck {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_health_interval")]
    pub interval_s: u32,
    #[serde(default = "default_tolerance")]
    pub tolerance_ms: u32,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_tolerance() -> u32 {
    100
}

impl Default for SubscriptionHealthCheck {
    fn default() -> Self {
        Self { enabled: true, url: None, interval_s: 60, tolerance_ms: 100, extra: Map::new() }
    }
}

// ================================================================ 线路

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Lane {
    pub id: LaneId,
    pub name: String,
    pub kind: LaneKind,
    #[serde(default)]
    pub subscription: Option<SubscriptionId>,
    /// **遗留（第 321 轮取消）**：`kind: subscription_group` 时订阅里自带的组名。
    ///
    /// 用户裁决"放弃订阅里分组这一概念"之后，界面上不再产生它；v2 → v3 迁移会把带它的线路
    /// 转成「自动选择」并**清空这个字段**。保留字段本身是为了让老配置仍能反序列化（理由同
    /// [`LaneKind::SubscriptionGroup`]）。新配置里它必须是空的。
    #[serde(default)]
    pub group: Option<String>,
    /// 组类型；缺省时取订阅里该组的 type
    #[serde(default)]
    pub group_type: Option<GroupType>,
    /// `kind: subscription_node` 时的固定节点名
    #[serde(default)]
    pub node: Option<String>,
    /// lane 级二次收窄
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub exclude_filter: Option<String>,
    /// 省略 = 启用：**被列进配置就等价于"我要用它"**，停用必须显式写 `false`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    /// 直连（本机网络）
    Direct,
    /// **「自动选择」**：这条订阅里**所有启用节点**各自 ping 该服务商的 URL，取延迟最低的那条；
    /// 选中后**按地址粘性**（[`crate::domain::Lane::automatic`] 会把它钉成显式 `select` 组，
    /// 见）。只在"上次那条连不上"或用户手动刷新时才重挑。
    SubscriptionAuto,
    /// 固定某个节点
    SubscriptionNode,
    /// 正则筛选节点后自己择优
    Filter,
    /// **遗留（第 321 轮取消的概念）**：用订阅里自带的某个分组。
    ///
    /// 用户第 321 轮的裁决：*直接放弃订阅里面分组这一概念，只拿节点*。
    /// 所以**界面上不再有任何入口产生它**，新配置也不该出现。
    ///
    /// 但它**必须仍然能被反序列化** —— 老库里存着 `kind: subscription_group`，
    /// 而配置是"先反序列化、再跑迁移"（`state.rs::bootstrap`）：删掉这个变体，
    /// 老配置会**在迁移跑到之前就解析失败**，连"升级"的机会都没有。
    ///
    /// 迁移 v2 → v3（`config::migrate::v2_to_v3`）会把它转成
    /// [`LaneKind::SubscriptionAuto`]（即「自动选择」）+ 显式 `select`，
    /// 于是老用户的分组线路**升级后仍然能用，而且拿到了粘性**。
    /// 迁移之后再出现这个值就是配置损坏，校验器会点名报错。
    SubscriptionGroup,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GroupType {
    Select,
    UrlTest,
    Fallback,
    LoadBalance,
}

// ================================================================ 服务商

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Provider {
    pub id: ProviderId,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    #[serde(default)]
    pub auth: AuthConfig,
    /// 服务商级默认线路 —— **这是唯一一处线路分配入口**（第 322 轮）。
    ///
    /// 用户裁决："**线路级别的分配逻辑我们已经放弃了，取而代之的，每个服务商统一一个节点**"。
    /// 所以生效线路只有一条链：`provider.lane ?? net.default_lane ?? direct`。
    ///
    /// 这里曾有 `credential.lane` / `channel.lane` / `policy.use_lane` 三级覆盖，全部删除：
    /// 那三级**在界面上早已没有任何写入口**（相关页面都删了），却仍会被解析链读到 ——
    /// 老配置里一个看不见、也改不掉的 `channel.lane` 会**静默压过**用户当前选的服务商线路。
    /// 保留它们比删掉更危险，所以整条链收敛到这一处。
    #[serde(default)]
    pub lane: Option<LaneId>,
    /// 附加头：**默认只补缺，不覆盖客户端同名头**
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub on_header_conflict: HeaderConflict,
    #[serde(default)]
    pub passthrough: ProviderPassthrough,
    #[serde(default)]
    pub timeout: TimeoutConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    /// 如上游要求 `openai/gpt-4o`，在这里统一加前缀
    #[serde(default)]
    pub model_prefix: Option<String>,
    /// 省略 = 启用：**被列进配置就等价于"我要用它"**，停用必须显式写 `false`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthConfig {
    #[serde(default)]
    pub scheme: AuthScheme,
    /// `scheme: header` 时的头名，如 `x-api-key`
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self { scheme: AuthScheme::Bearer, header: None, prefix: None, extra: Map::new() }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    #[default]
    Bearer,
    Header,
    Query,
    None,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeaderConflict {
    #[default]
    KeepClient,
    Override,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderPassthrough {
    #[serde(default)]
    pub header_mode: Option<HeaderMode>,
    #[serde(default)]
    pub body: Option<BodyMode>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl Default for ProviderPassthrough {
    fn default() -> Self {
        Self { header_mode: None, body: None, extra: Map::new() }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimeoutConfig {
    #[serde(default = "default_connect_timeout")]
    pub connect_s: u32,
    #[serde(default = "default_read_timeout")]
    pub read_s: u32,
    #[serde(default)]
    pub total_s: Option<u32>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_connect_timeout() -> u32 {
    10
}
fn default_read_timeout() -> u32 {
    600
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self { connect_s: 10, read_s: 600, total_s: None, extra: Map::new() }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TlsConfig {
    #[serde(default)]
    pub insecure: bool,
    #[serde(default)]
    pub extra_ca_pem: Option<String>,
    #[serde(default)]
    pub min_version: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

// ================================================================ 凭据

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Credential {
    pub id: CredentialId,
    pub provider: ProviderId,
    pub label: String,
    /// `"${secret:name}"` 或 `"${env:VAR}"`；**绝不存明文**
    pub secret: String,
    // 这里曾有 `lane: Option<LaneId>`（"这份套餐默认走哪条线"）。
    //
    //（第 322 轮）整条删除：线路分配只剩**服务商级**一处（`Provider.lane`）。
    // 老配置里的 `credentials[].lane` 会被 `extra` 吸收（`flatten`，见本结构体末尾），
    // 解析不失败、值也不丢 —— 但它**不再参与任何解析**。
    #[serde(default)]
    pub guard: GuardConfig,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 省略 = 启用：**被列进配置就等价于"我要用它"**，停用必须显式写 `false`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

/// **套餐费**的计费周期（`PlanFee.window`）。
///
/// 注意它**不是**"额度窗口"：这里说的是"这笔套餐费按什么周期摊到每一天"
/// （`plan_fee_period_ms` / `amortize_plan_fee` 用它把月费摊成日成本）。
/// 早先它叫 `QuotaWindow`，与已经下线的额度功能同名，容易误伤 —— 现在改名说清楚。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanFeeCycle {
    Hour,
    Day,
    #[default]
    Month,
    None,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GuardConfig {
    #[serde(default)]
    pub max_rpm: Option<u32>,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
    #[serde(default)]
    pub on_exceed: OnExceed,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnExceed {
    #[default]
    Wait,
    NextCandidate,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            max_rpm: None,
            max_concurrency: None,
            max_input_tokens: None,
            on_exceed: OnExceed::Wait,
            extra: Map::new(),
        }
    }
}

// ================================================================ 模型 / 通道

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Model {
    /// 对外暴露的逻辑模型名
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default = "default_true")]
    pub expose: bool,
    /// **模型级总开关**。
    ///
    /// `false` = **彻底停用**：`GET /v1/models` 不再列出它，调用它直接按"模型不存在"拒绝。
    /// 关掉它不会删除任何东西 —— 这正是这个字段存在的理由：用户想"暂时下线一个模型"
    /// 时不必删掉它再重建（那是"频繁删除与创建"，用户明确不想这么干）。
    ///
    /// 与 `expose` 的分工：`expose` 管"**列不列进** `/v1/models`"，`enabled` 管
    /// "**能不能用**" —— 停用必须两者同时生效，所以判据是 `enabled && expose`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// **这个模型要用哪几条线路**—— 模型↔线路关系的**唯一真源**，由模型侧持有。
    ///
    /// 为什么不在 `ChannelConfig` 上写"我是哪个模型的"（老结构）：线路归服务商、模型只负责"用"，
    /// 而**一条线路可以服务多个模型**（两个模型都用某条火山线路）—— 一对一的字段表达不了这个。
    /// 放在模型侧还让删除语义自然成立：删模型不动线路；删线路把这条从各模型的清单里摘掉。
    ///
    /// **至少一条**（校验器强制）：零条线路的模型对外可调用却必然路由不到任何地方。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<ChannelId>,
    /// **已删除的旧字段**：`default_lane`。
    ///
    /// 它当年表达"这个模型的通道默认继承哪条线路 / 界面上显示哪条线"，是**第二份**回答
    /// "这个模型走哪个代理"的真相源；代理是**线路**（[`ChannelConfig::lane`]）的属性，
    /// 模型的线路也只由 [`Model::lines`] 决定，所以这个字段整条删掉了。
    /// 老配置/老导出里带着它**仍然能解析** —— 未知字段一律落进这里的 `extra` 原样保留
    /// （仓库一贯的前向兼容纪律），但它**没有**真字段、也**不会**被重新当字段写出去。
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DisplayConfig {
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub order: i32,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

/// 一个模型**对外暴露的**信息 —— 客户端在 `/v1/models` 里能看到的那一份。
///
/// **为什么要有这个类型**：同一份"对外信息"有两处要显示/发送 —— 网关 `/v1/models` 的响应，
/// 与模型页「查看模型信息」面板。两处各算一遍必然漂，所以聚合规则**只有一份**
/// （[`ConfigDoc::public_model_info`]），网关与命令都从它取。
///
/// 聚合口径（能力等价校验本应让各线路一致，这里仍往"不夸大"的方向兜底）：
/// * `context_window` / `max_output_tokens`：各线路声明过的值取**最小**；一条都没声明 ⇒ `None`
///   （**不编 0**，界面显示「未声明」）；
/// * `vision` / `tools` / `reasoning` / `modalities`：**并集**（只要有一条线路支持就算支持 ——
///   这是"上游没给的一个都不编"的正面表述：给了的才认）；
/// * `owned_by`：对外暴露的服务商标识（第一条线路所属服务商的 `id`），与网关同一个口径。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicModelInfo {
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub vision: bool,
    pub tools: bool,
    pub reasoning: bool,
    pub modalities: Vec<String>,
    pub owned_by: String,
}

/// 上游能力声明。///
/// **为什么需要它**：同一个对外模型可以由多条线路服务（Ollama 一条、CommandCode 一条），
/// 但只有两家上游暴露的能力**完全一致**时，路由到它们才是等价的；否则会出现
/// "有时有 vision、有时报错"这种不可预期行为。所以能力要**显式声明**、并由后端强制校验。
///
/// **默认值的语义是契约的一部分**（老配置不写 `capabilities` 也要能用）：
/// 全 `false`、`context_tokens = 0`、`max_output_tokens = None`（未声明）、`modalities = ["text"]`。
/// 注意 `[]`（显式声明没有模态）与"没写"（默认 `["text"]`）**不是一回事**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelCapabilities {
    /// 支持图片输入
    #[serde(default)]
    pub vision: bool,
    /// 支持函数调用 / 工具
    #[serde(default)]
    pub tools: bool,
    /// 支持推理 / 思考内容
    #[serde(default)]
    pub reasoning: bool,
    /// 上下文窗口（token）
    #[serde(default)]
    pub context_tokens: u64,
    /// 单次最大输出；`None` = **未声明**（比较时 `None` 只等于 `None`）
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// 模态，如 `["text", "image"]`；**比较时先排序去重**
    #[serde(default = "default_modalities")]
    pub modalities: Vec<String>,
}

fn default_modalities() -> Vec<String> {
    vec!["text".to_string()]
}

impl Default for ChannelCapabilities {
    fn default() -> Self {
        Self {
            vision: false,
            tools: false,
            reasoning: false,
            context_tokens: 0,
            max_output_tokens: None,
            modalities: default_modalities(),
        }
    }
}

/// 通道：一个逻辑模型经某个凭据发往某个上游的**一条具体路径**。
///
/// **Rust 类型名必须是 `ChannelConfig`（不能叫 `Channel`）**：`tauri-specta` 会拒绝把
/// 用户类型导出成 `Channel` —— 那个名字与 Tauri 自己的运行时类型（`tauri::ipc::Channel`）
/// 冲突，报错原文：`User defined type 'Channel' ... must be renamed so it doesn't
/// conflict with Tauri Specta runtime`。
///
/// **试过但走不通的两条捷径**（留给后来者，别再踩）：
/// ① `#[specta(rename = "…")]` —— rc.25 已废弃容器上的 rename，编译器直接报错
///    （`… is no longer supported on containers. Use #[serde(rename = "...")] instead`）；
/// ② `#[serde(rename = "…")]` —— 能编译，**但 tauri-specta 的冲突检测看的是 Rust 类型名**
///    （`tauri-specta/src/js_ts.rs` 里比对 `ndt.name`），导出时照样拒绝。
/// **所以只能真的改类型名。** 结构体名不进 JSON（JSON 里是字段名），**线上形态一字不变**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelConfig {
    pub id: ChannelId,
    /// **过渡期的遗留字段**：老配置里"这条线路属于哪个模型"。
    ///
    /// 关系已经翻转到 [`Model::lines`]（一条线路可以服务多个模型，一对一的字段表达不了）。
    /// 这个字段现在的角色只有两个：
    /// 1. **读取迁移**：`ConfigDoc` 从库里载入时把非空的 `model` 搬进对应模型的 `lines`
    ///    （见 `ta_core::config::migrate`），迁完就把它清空；
    /// 2. 让**老配置/老导出**仍能被解析（`#[serde(default)]`）。
    ///
    /// **新写入一律写空字符串**（写命令层强制），真源永远是 `Model.lines`。
    /// S2 会把这个字段连同它的解析一起删掉。
    #[serde(default)]
    pub model: String,
    /// 这条线路属于哪个**服务商**（：线路在服务商详情页里创建/编辑）。
    pub provider: ProviderId,
    /// 这条线路用这个服务商的哪一份凭据。 定死"一个服务商就一把密钥"，
    /// 所以实际上只有一份；字段名保留是为了不动既有配置与内核生成。
    pub credential: CredentialId,
    // 这里曾有 `lane: Option<LaneId>`（"不填则继承 credential.lane → provider.lane → …"）。
    //
    //（第 322 轮）整条删除：线路分配只剩**服务商级**一处（`Provider.lane`）。
    // 老配置里的 `channels[].lane` 会被 `extra` 吸收（`flatten`，见本结构体末尾），
    // 解析不失败、值也不丢 —— 但它**不再参与任何解析**。
    //
    // 这一步是必须的，不是顺手清理：这个字段在界面上**早已没有写入口**
    // （服务商详情页的线路行只有名字 + 启用开关），却仍在解析链的最前面。
    // 留着它 = 老配置里一个看不见、也改不掉的覆盖项会静默压过用户当前选的线路。
    pub upstream_model: String,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// 可选的能力标签：`vision` / `no-vision` / `cheap` / `official` / `cn-direct` / `no-log`
    /// **未标注 = 照发**（默认行为是发出去，不是拒绝）
    #[serde(default)]
    pub tags: Vec<String>,
    /// 少数确实要按域名走不同线路的场景
    #[serde(default)]
    pub target_override: Option<String>,
    /// 省略 = 启用：**被列进配置就等价于"我要用它"**，停用必须显式写 `false`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 上游能力声明。**`#[serde(default)]`：老配置不写也能继续用**，
    /// 读出来的是 [`ChannelCapabilities::default`]（全 false / 上下文 0 / 未声明 / `["text"]`）。
    #[serde(default)]
    pub capabilities: ChannelCapabilities,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_priority() -> i32 {
    100
}
fn default_weight() -> u32 {
    10
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Transform {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub headers_add: BTreeMap<String, String>,
    #[serde(default)]
    pub headers_remove: Vec<String>,
    #[serde(default)]
    pub headers_override: BTreeMap<String, String>,
    /// 定点 body patch（如强制 `stream_options.include_usage=true`）
    ///
    /// `#[specta(type = Any)]`：用户手写的**任意 JSON 补丁**，形状由用户决定；
    /// 而 `serde_json::Value` 是递归类型，specta 展开它会产出无限的 TS 类型
    /// （导出会报 `Found infinitely recursive inline named reference`）。
    #[cfg_attr(feature = "specta", specta(type = specta_typescript::Any))]
    #[serde(default)]
    pub body_patch: Map<String, Value>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

// ================================================================ 通道组

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelGroup {
    pub id: ChannelGroupId,
    pub model: String,
    pub members: Vec<ChannelId>,
    #[serde(default)]
    pub selector: Selector,
    #[serde(default)]
    pub sticky: StickyConfig,
    #[serde(default)]
    pub release_on: Vec<ReleaseReason>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

/// 选路策略。**现在只有一种**。
///
/// 为什么把另外几种删掉： 查清"各家的额度耗尽返回什么"之后发现，
/// **分码也分不干净**（同一家 429 既表限流也表额度耗尽、有的家把额度耗尽藏在 400 里），
/// 所以"引擎自动判断该用哪条线路"这条路走不通 —— 改成**用户用开关手动调度 + 引擎只做平均**。
/// 既然只剩一种语义，就不该留一个只有一种合法取值的用户可见字段。
///
/// **旧配置仍能读**：`alias` 收下全部历史取值，一律落到 `BalancedSticky`。
/// 迁移只增不改，老库里的 `"selector": "cheapest"` 不能让解析失败。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// **唯一策略（平均粘性）**：先查会话粘性；未命中挑 token 用量最少的线路。
    /// 见 `crate::routing::selector` 模块头。
    #[default]
    #[serde(
        alias = "priority",
        alias = "sticky_then_priority",
        alias = "cheapest",
        alias = "lowest_latency",
        alias = "weighted_random",
        alias = "round_robin"
    )]
    BalancedSticky,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StickyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_sticky_ttl_h")]
    pub ttl_h: u32,
    /// key 派生顺序，取值如 `header:x-session-id` / `metadata.user_id` / `body_hash`
    #[serde(default = "default_sticky_keys")]
    pub key: Vec<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_sticky_ttl_h() -> u32 {
    12
}
fn default_sticky_keys() -> Vec<String> {
    vec![
        "header:x-session-id".into(),
        "header:x-conversation-id".into(),
        "metadata.user_id".into(),
        "body_hash".into(),
    ]
}

impl Default for StickyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_h: default_sticky_ttl_h(),
            key: default_sticky_keys(),
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    HardError,
    Cooldown,
    UserPinChange,
    Ttl,
}

// ================================================================ 策略

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    pub id: PolicyId,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 数字小的先求值；**首个命中即生效**
    #[serde(default = "default_policy_priority")]
    pub priority: i32,
    #[serde(default)]
    pub r#match: PolicyMatch,
    #[serde(default)]
    pub action: PolicyAction,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_policy_priority() -> i32 {
    1000
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PolicyMatch {
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub model_matches: Vec<String>,
    /// 请求内容含图片块（只为选目的地，不改请求）
    #[serde(default)]
    pub has_image: Option<bool>,
    #[serde(default)]
    pub header_matches: BTreeMap<String, String>,
    #[serde(default)]
    pub ua_matches: Option<String>,
    #[serde(default)]
    pub channel_tags_any: Vec<String>,
    #[serde(default)]
    pub channel_tags_all: Vec<String>,
    #[serde(default)]
    pub input_tokens_lt: Option<u64>,
    #[serde(default)]
    pub input_tokens_gt: Option<u64>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub time_window: Option<String>,
    #[serde(default)]
    pub provider_in: Vec<ProviderId>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PolicyAction {
    #[serde(default)]
    pub use_group: Option<ChannelGroupId>,
    #[serde(default)]
    pub use_channels: Vec<ChannelId>,
    #[serde(default)]
    pub exclude_channels: Vec<ChannelId>,
    #[serde(default)]
    pub require_tag: Option<String>,
    #[serde(default)]
    pub exclude_tag: Option<String>,
    // 这里曾有 `selector: Option<Selector>`（策略级选路覆盖）。
    // 选路策略收敛为唯一一种之后，"用哪条策略"没有第二种可能，
    // 该字段整条删除。老配置里的 `"selector": "..."` 会被 `extra` 吸收（`flatten`），
    // 解析不失败。
    #[serde(default)]
    pub failover: Option<FailoverPolicy>,
    #[serde(default)]
    pub on_no_candidate: NoCandidateAction,
    #[serde(default)]
    pub error_code: Option<u16>,
    #[serde(default)]
    pub pin_sticky: Option<ChannelId>,
    // 这里曾有 `use_lane: Option<LaneId>`（"一键全切线路（最高优先级）"）。
    //
    //（第 322 轮）整条删除：线路分配只剩**服务商级**一处（`Provider.lane`）。
    // 老配置里的 `policies[].action.use_lane` 会被 `extra` 吸收（`flatten`），解析不失败。
    // 与上面 `selector` 的删除完全同一条路子。
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FailoverPolicy {
    #[serde(default = "default_failover_on")]
    pub on: Vec<FailoverTrigger>,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_cooldown_s")]
    pub cooldown_s: u32,
    #[serde(default)]
    pub backoff: Backoff,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_failover_on() -> Vec<FailoverTrigger> {
    vec![
        FailoverTrigger::Http429,
        FailoverTrigger::Http402,
        FailoverTrigger::Http5xx,
        FailoverTrigger::Timeout,
        FailoverTrigger::ConnectError,
        FailoverTrigger::ChannelTaggedOut,
    ]
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailoverTrigger {
    Http429,
    Http402,
    Http5xx,
    Timeout,
    ConnectError,
    ChannelTaggedOut,
    CapabilityMismatch,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoCandidateAction {
    /// 默认：明确报错，不静默降级
    #[default]
    Error,
    UseAny,
}

// ================================================================ 定价

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PricingConfig {
    #[serde(default = "default_pricing_currency")]
    pub currency: String,
    #[serde(default)]
    pub unit: PriceUnit,
    #[serde(default)]
    pub usd_to_cny: Option<f64>,
    #[serde(default)]
    pub entries: Vec<PriceEntry>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_pricing_currency() -> String {
    "USD".into()
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceUnit {
    #[default]
    #[serde(rename = "per_1m")]
    Per1M,
    #[serde(rename = "per_1k")]
    Per1K,
    #[serde(rename = "per_token")]
    PerToken,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            currency: default_pricing_currency(),
            unit: PriceUnit::Per1M,
            usd_to_cny: None,
            entries: vec![],
            extra: Map::new(),
        }
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PriceEntry {
    #[serde(default)]
    pub r#match: PriceMatch,
    #[serde(default)]
    pub rates: BTreeMap<String, f64>,
    #[serde(default = "default_one")]
    pub multiplier: f64,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub plan_fee: Option<PlanFee>,
    #[serde(default)]
    pub source: PriceSource,
    #[serde(default)]
    pub effective_from: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PriceMatch {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<ProviderId>,
    #[serde(default)]
    pub credential: Option<CredentialId>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanFee {
    pub amount: f64,
    pub currency: String,
    #[serde(default)]
    pub window: PlanFeeCycle,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    #[default]
    Builtin,
    Manual,
    Imported,
}

// ================================================================ 系统集成

/// 开机自启动后的行为（见 [02 §2.10]）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutostartMode {
    /// 静默进托盘（默认）—— autostart 的目的是让网关在后台待命，
    /// 客户端连 `127.0.0.1:8787` 不需要用户开窗口。
    #[default]
    Tray,
    /// 显示窗口（调试，或用户就想看见）。
    Window,
}

/// 系统集成开关。
///
/// **三类开关的默认值规则**（[09 §1.2] E1）：
/// - **会写操作系统状态的开关一律默认关**：`autostart` / `enable_tun` / `enable_system_proxy`；
/// - **"实体启用"语义的开关默认开**：见 `Provider.enabled` 等（列出来就是要用）；
/// - **无损 / 自检类默认开**：`tray_show_latency`。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SystemIntegration {
    /// **例外：默认关**（同 `autostart`：会写 OS 状态的开关一律默认关，[09 §1.2] E1）。
    /// 接管整机网络（TUN）是实验性高级模式，需要管理员权限；
    /// "默认不接管整机流量"是本产品的核心承诺，所以这里与"实体 enabled"的规则相反。
    #[serde(default)]
    pub enable_tun: bool,
    /// **例外：默认关**（同 `autostart`）。绝不主动碰系统代理。
    #[serde(default)]
    pub enable_system_proxy: bool,
    /// 开机自启动：**本项目唯一会写操作系统状态的功能**
    /// （Windows `Run` 键 / macOS `LaunchAgent` / Linux XDG autostart，见 [09 §1.2]）。
    ///
    /// **默认 false** —— 凡是会写 OS 状态的开关一律默认关：装完就静默加一个开机启动项，
    /// 正是用户最反感的那种"副作用"。与 `enable_tun` / `enable_system_proxy` 同属
    /// "核心承诺的例外"（显式 opt-in，E1）。
    ///
    /// 注意：这里只是**期望状态**，不是事实：系统实际状态才是真源。用户手动禁用后
    /// **绝不静默改回**，只在 UI 里提示漂移并提供「重新注册 / 清除」（E2–E4）。
    #[serde(default)]
    pub autostart: bool,
    /// 启动后静默进托盘（默认）还是显示窗口。
    /// 默认 `tray`：autostart 的目的是让网关在后台待命。
    #[serde(default)]
    pub autostart_mode: AutostartMode,
    /// 启动延迟秒数（0–300）。由**我们自己**实现（autostart 插件不支持延迟）：
    /// 开机时磁盘/网络繁忙，延迟可减少"内核首次就绪失败 → 退避重试"。
    #[serde(default)]
    pub autostart_delay_s: u32,
    #[serde(default = "default_true")]
    pub tray_show_latency: bool,
    /// 文件监听模式：以 YAML 为真源（GUI 写回会覆盖，需警告）。
    /// **默认关**：这是"换一种真源"的 opt-in 模式，不是实体启用开关，
    /// 与 `Provider.enabled` / `Channel.enabled` 之类不同语义 —— 不要跟着改成 `default_true`。
    #[serde(default)]
    pub file_watch_mode: bool,
    #[serde(default)]
    pub file_watch_path: Option<String>,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

/// 注意：**不要改用 `#[derive(Default)]`**：`ConfigDoc.system_integration` 上的
/// `#[serde(default)]` 在"整节缺失"时用的是**这个** `Default`，而"写了空节"
/// 用的是各字段的 `#[serde(default = ...)]`；两者必须逐字段一致
/// （`tray_show_latency` 是 `default_true`，三个 OS 状态开关是 `false`）。
/// 由 `config::tests::empty_section_equals_omitted_section` 守着。
impl Default for SystemIntegration {
    fn default() -> Self {
        Self {
            enable_tun: false,
            enable_system_proxy: false,
            autostart: false,
            autostart_mode: AutostartMode::Tray,
            autostart_delay_s: 0,
            tray_show_latency: true,
            file_watch_mode: false,
            file_watch_path: None,
            extra: Map::new(),
        }
    }
}

// ================================================================ 能力契约

#[cfg(test)]
mod capability_contract_tests {
    use super::*;

    /// 老配置（落地**之前**写的）没有 `capabilities` 这一节。
    fn old_channel() -> serde_json::Value {
        serde_json::json!({
            "id": "ch-a",
            "model": "deepseek-v4.1flash",
            "provider": "ollama",
            "credential": "ollama-personal",
            "upstream_model": "deepseek-v4.1flash",
        })
    }

    /// 文档写死的默认值：全 false、上下文 0、最大输出**未声明**、模态 `["text"]`。
    fn documented_default() -> ChannelCapabilities {
        ChannelCapabilities {
            vision: false,
            tools: false,
            reasoning: false,
            context_tokens: 0,
            max_output_tokens: None,
            modalities: vec!["text".to_string()],
        }
    }

    #[test]
    fn old_config_without_capabilities_reads_as_the_documented_default() {
        let channel: ChannelConfig =
            serde_json::from_value(old_channel()).expect("老配置必须还能读");
        assert_eq!(channel.capabilities, documented_default());
        // 自证：默认值断言不是对着一个"随便什么"变绿
        assert_eq!(ChannelCapabilities::default(), documented_default());
    }

    #[test]
    fn a_partial_capabilities_object_fills_the_missing_fields_from_defaults() {
        let mut raw = old_channel();
        raw["capabilities"] = serde_json::json!({ "vision": true, "context_tokens": 1_050_000 });
        let channel: ChannelConfig = serde_json::from_value(raw).expect("部分声明也要能读");

        assert!(channel.capabilities.vision);
        assert_eq!(channel.capabilities.context_tokens, 1_050_000);
        // 没写的字段按**文档默认**补齐，不是随便给一个
        assert!(!channel.capabilities.tools);
        assert!(!channel.capabilities.reasoning);
        assert_eq!(channel.capabilities.max_output_tokens, None);
        assert_eq!(channel.capabilities.modalities, vec!["text".to_string()]);
    }

    #[test]
    fn serialized_shape_is_stable_and_always_carries_every_field() {
        let channel = ChannelConfig::new("ch-a", "m", "p", "c", "m-up");
        let json = serde_json::to_value(&channel).expect("序列化");
        let caps = json
            .get("capabilities")
            .expect("capabilities 必须出现在线上形态里（后端永远填好再发）");

        assert_eq!(
            caps,
            &serde_json::json!({
                "vision": false,
                "tools": false,
                "reasoning": false,
                "context_tokens": 0,
                "max_output_tokens": null,
                "modalities": ["text"],
            })
        );
        // 键集固定：前端读 `channel.capabilities.vision` 不会拿到 undefined
        assert_eq!(caps.as_object().map(|o| o.len()), Some(6));
    }

    #[test]
    fn explicitly_empty_modalities_is_not_the_same_as_absent() {
        // `[]` 是**显式**声明"没有模态"；没写才是默认 `["text"]`。两者相比不相等。
        let mut raw = old_channel();
        raw["capabilities"] = serde_json::json!({ "modalities": [] });
        let channel: ChannelConfig = serde_json::from_value(raw).expect("读");
        assert!(channel.capabilities.modalities.is_empty());
        assert_ne!(channel.capabilities, documented_default());
    }

    #[test]
    fn new_channel_starts_from_the_default_capabilities() {
        let channel = ChannelConfig::new("ch-a", "m", "p", "c", "m-up");
        assert_eq!(channel.capabilities, documented_default());
    }

    /// `Model.default_lane` **已删除**（：代理是线路的属性，模型不挑代理）。
    ///
    /// 新的不变量是：老配置/老导出里带着 `default_lane` **必须仍能解析**，值原样落进
    /// `#[serde(flatten)] extra`（仓库一贯的前向兼容纪律，未知键不许吞）；因此它**只会
    /// 从 `extra` 原样回来**，`Model` 上不再有真字段，新写的模型线上形态里也不会凭空多出它。
    #[test]
    fn removed_model_default_lane_lands_in_extra_and_is_not_a_field() {
        let old: Model = serde_json::from_value(serde_json::json!({
            "id": "m-a",
            "lines": ["ch-a"],
            "default_lane": "lane-jp-auto",
        }))
        .expect("带已删除字段的老配置必须能解析");

        assert_eq!(
            old.extra.get("default_lane"),
            Some(&serde_json::json!("lane-jp-auto")),
            "已删除字段的值必须原样落进 extra，不许被吞掉"
        );

        // 来自 extra → 整体往返时从 extra 原样回去（未知键 round-trip 不丢）
        let value = serde_json::to_value(&old).expect("序列化");
        assert_eq!(value.get("default_lane"), Some(&serde_json::json!("lane-jp-auto")));

        // 没写过它的模型：线上形态里**不许**出现这个键（它已经不是字段了）
        let fresh = serde_json::to_value(Model::new("m-a")).expect("序列化");
        assert!(fresh.get("default_lane").is_none(), "已删除的字段不许再出现在线上形态里：{fresh}");
    }
}

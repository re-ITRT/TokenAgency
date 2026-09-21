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
    1
}

impl Default for ConfigDoc {
    fn default() -> Self {
        Self {
            version: 1,
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
    #[serde(default)]
    pub expose_models_with_capabilities: bool,
    /// 通配路径时兜底用哪个 provider（无法确定模型名时）
    #[serde(default)]
    pub fallback_provider: Option<ProviderId>,
    #[serde(default)]
    pub passthrough: PassthroughConfig,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub metering: MeteringConfig,
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
            expose_models_with_capabilities: false,
            fallback_provider: None,
            passthrough: Default::default(),
            retry: Default::default(),
            metering: Default::default(),
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
    /// `kind: subscription_group` 时，订阅里自带的组名；**可能含 emoji**（如用户订阅里的日本线路
    /// 分组），必须**原样**保存与展示 —— 改一个字就与内核里的名字对不上（[15 §2.2]）
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
    /// 整份订阅里自己择优
    SubscriptionAuto,
    /// 用订阅里自带的某个分组（保留机场语义）
    SubscriptionGroup,
    /// 固定某个节点
    SubscriptionNode,
    /// 正则筛选节点后自己择优
    Filter,
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
    /// 服务商级默认线路（可被 credential/channel 覆盖）
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
    /// 这份套餐默认走哪条线
    #[serde(default)]
    pub lane: Option<LaneId>,
    #[serde(default)]
    pub quota: QuotaConfig,
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

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuotaConfig {
    #[serde(default)]
    pub metric: QuotaMetric,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub window: QuotaWindow,
    #[serde(default = "default_reset_day")]
    pub reset_day: u32,
    #[serde(default = "default_soft_limit")]
    pub soft_limit: f64,
    #[serde(default = "default_hard_limit")]
    pub hard_limit: f64,
    #[serde(default)]
    pub reserve: f64,
    #[cfg_attr(feature = "specta", specta(skip))]
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_currency() -> String {
    "USD".into()
}
fn default_reset_day() -> u32 {
    1
}
fn default_soft_limit() -> f64 {
    0.8
}
fn default_hard_limit() -> f64 {
    1.0
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaMetric {
    #[default]
    Unknown,
    Tokens,
    Requests,
    Cost,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindow {
    Hour,
    Day,
    #[default]
    Month,
    None,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            metric: QuotaMetric::Unknown,
            limit: None,
            currency: default_currency(),
            window: QuotaWindow::Month,
            reset_day: 1,
            soft_limit: default_soft_limit(),
            hard_limit: default_hard_limit(),
            reserve: 0.0,
            extra: Map::new(),
        }
    }
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
    pub model: String,
    pub provider: ProviderId,
    pub credential: CredentialId,
    /// 不填则继承 credential.lane → provider.lane → net.default_lane → direct
    #[serde(default)]
    pub lane: Option<LaneId>,
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

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    Priority,
    /// **默认**：先查粘性，未命中按 priority 并写入粘性
    #[default]
    StickyThenPriority,
    QuotaFirst,
    Cheapest,
    LowestLatency,
    WeightedRandom,
    RoundRobin,
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
    QuotaExhausted,
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
    pub quota_remaining_lt: Option<f64>,
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
    #[serde(default)]
    pub selector: Option<Selector>,
    #[serde(default)]
    pub failover: Option<FailoverPolicy>,
    #[serde(default)]
    pub on_no_candidate: NoCandidateAction,
    #[serde(default)]
    pub error_code: Option<u16>,
    #[serde(default)]
    pub pin_sticky: Option<ChannelId>,
    /// 一键全切线路（最高优先级）
    #[serde(default)]
    pub use_lane: Option<LaneId>,
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
        FailoverTrigger::QuotaExhausted,
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
    QuotaExhausted,
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
    pub window: QuotaWindow,
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

//! 跨层共用的基础类型：ID、协议、token 类型、金额。
//!
//! `TokenKind` 是**开放集合**——上游随时可能加新的计费档，所以它必须能被序列化成
//! 一个普通的字符串 map key（`BTreeMap<TokenKind, u64>` → JSON object）。
//! 为此我们手写 `Serialize`/`Deserialize`，而不是依赖 derive 的枚举表示。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- ID 类型

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[cfg_attr(feature = "specta", derive(specta::Type))]
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
            Serialize, Deserialize, schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self { Self(s.to_string()) }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self { Self(s) }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str { &self.0 }
        }
    };
}

string_id!(/// 服务商
    ProviderId);
string_id!(/// 凭据（一个 key = 一份 tokenplan）
    CredentialId);
string_id!(/// 逻辑模型
    ModelId);
string_id!(/// 通道
    ChannelId);
string_id!(/// 通道组
    ChannelGroupId);
string_id!(/// 路由策略
    PolicyId);
string_id!(/// 线路
    LaneId);
string_id!(/// 机场订阅
    SubscriptionId);
string_id!(/// 请求 ID
    RequestId);
string_id!(/// 粘性 key
    StickyKey);
string_id!(/// 客户端（从 UA 推断）
    ClientId);

// ---------------------------------------------------------------- 协议

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Openai,
    OpenaiResponses,
    Anthropic,
    Gemini,
    /// 自定义兼容（按 openai 处理，但路径/鉴权可覆盖）
    Custom,
}

impl Protocol {
    /// 该协议的默认版本段路径前缀
    pub fn default_base_path(&self) -> &'static str {
        match self {
            Protocol::Openai | Protocol::Custom => "/v1",
            Protocol::OpenaiResponses => "/v1",
            Protocol::Anthropic => "/v1",
            Protocol::Gemini => "/v1beta",
        }
    }
}

// ---------------------------------------------------------------- Token 类型

/// 归一化后的 token 类型。**契约见 。**
///
/// specta 的类型由下方**手写 `impl Type`** 提供（不用 derive）：它是开放集合，
/// 线上形态是一个**普通字符串**（`"input"` / `"input_cached"` / 上游新类型原样），
/// 而且会作为 JSON **对象键**出现在 `TokenUsage` 里 —— 键只能是字符串。
/// derive 会生成 `"Input" | "InputCached" | { Other: string }` 这种与线上形态
/// 不符的联合类型，前端照着写会直接构造错误的数据。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, schemars::JsonSchema)]
#[schemars(with = "String")]
pub enum TokenKind {
    /// 标准输入（**已扣除 cached**）
    Input,
    /// 缓存命中读取
    InputCached,
    /// 缓存写入（默认 TTL）
    InputCacheWrite,
    /// 缓存写入（1h TTL，Anthropic）
    InputCacheWrite1h,
    InputAudio,
    InputImage,
    /// 标准输出（**已扣除 reasoning / audio**）
    Output,
    OutputReasoning,
    OutputAudio,
    OutputPredictionAccepted,
    OutputPredictionRejected,
    /// 上游出现的新类型，原样保留
    Other(String),
}

impl TokenKind {
    /// 用于 JSON map key 与数据库 `token_kind` 列
    pub fn as_key(&self) -> &str {
        match self {
            TokenKind::Input => "input",
            TokenKind::InputCached => "input_cached",
            TokenKind::InputCacheWrite => "input_cache_write",
            TokenKind::InputCacheWrite1h => "input_cache_write_1h",
            TokenKind::InputAudio => "input_audio",
            TokenKind::InputImage => "input_image",
            TokenKind::Output => "output",
            TokenKind::OutputReasoning => "output_reasoning",
            TokenKind::OutputAudio => "output_audio",
            TokenKind::OutputPredictionAccepted => "output_prediction_accepted",
            TokenKind::OutputPredictionRejected => "output_prediction_rejected",
            TokenKind::Other(s) => s,
        }
    }

    pub fn from_key(k: &str) -> Self {
        match k {
            "input" => TokenKind::Input,
            "input_cached" => TokenKind::InputCached,
            "input_cache_write" => TokenKind::InputCacheWrite,
            "input_cache_write_1h" => TokenKind::InputCacheWrite1h,
            "input_audio" => TokenKind::InputAudio,
            "input_image" => TokenKind::InputImage,
            "output" => TokenKind::Output,
            "output_reasoning" => TokenKind::OutputReasoning,
            "output_audio" => TokenKind::OutputAudio,
            "output_prediction_accepted" => TokenKind::OutputPredictionAccepted,
            "output_prediction_rejected" => TokenKind::OutputPredictionRejected,
            other => TokenKind::Other(other.to_string()),
        }
    }

    /// 是否计入"输入侧"
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            TokenKind::Input
                | TokenKind::InputCached
                | TokenKind::InputCacheWrite
                | TokenKind::InputCacheWrite1h
                | TokenKind::InputAudio
                | TokenKind::InputImage
        )
    }

    /// 是否计入"输出侧"
    pub fn is_output(&self) -> bool {
        !self.is_input()
    }
}

impl Serialize for TokenKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_key())
    }
}

impl<'de> Deserialize<'de> for TokenKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(TokenKind::from_key(&s))
    }
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_key())
    }
}

/// 手写 specta 类型：线上形态是字符串（见 `TokenKind` 的注释）。
#[cfg(feature = "specta")]
impl specta::Type for TokenKind {
    fn definition(_types: &mut specta::Types) -> specta::datatype::DataType {
        specta::datatype::DataType::Primitive(specta::datatype::Primitive::str)
    }
}

/// 一次请求的 token 用量。**未出现的类型就是 0，不写进 map。**
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TokenUsage(pub BTreeMap<TokenKind, u64>);

impl TokenUsage {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn set(&mut self, kind: TokenKind, n: u64) {
        if n == 0 {
            self.0.remove(&kind);
        } else {
            self.0.insert(kind, n);
        }
    }

    pub fn add(&mut self, kind: TokenKind, n: u64) {
        if n == 0 {
            return;
        }
        *self.0.entry(kind).or_insert(0) += n;
    }

    pub fn get(&self, kind: &TokenKind) -> u64 {
        self.0.get(kind).copied().unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.0.values().sum()
    }

    pub fn input_total(&self) -> u64 {
        self.0.iter().filter(|(k, _)| k.is_input()).map(|(_, v)| *v).sum()
    }

    pub fn output_total(&self) -> u64 {
        self.0.iter().filter(|(k, _)| k.is_output()).map(|(_, v)| *v).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.values().all(|v| *v == 0)
    }
}

// ---------------------------------------------------------------- 金额

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Money {
    pub amount: f64,
    /// ISO 4217，如 `USD` / `CNY`
    pub currency: String,
}

impl Money {
    pub fn new(amount: f64, currency: impl Into<String>) -> Self {
        Self { amount, currency: currency.into() }
    }
    pub fn zero(currency: impl Into<String>) -> Self {
        Self { amount: 0.0, currency: currency.into() }
    }
    pub fn is_zero(&self) -> bool {
        self.amount.abs() < f64::EPSILON
    }
}

// ---------------------------------------------------------------- 用量来源

/// usage 数据从哪来——**绝不用估算值冒充真实值**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// 上游返回了完整细分
    Upstream,
    /// 上游只返回了 input/output 总数，没有细分
    UpstreamPartial,
    /// 上游完全没返回，由本地估算
    Estimated,
}

/// 一次请求的用量记录（写账本用）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub request_id: RequestId,
    pub ts_ms: i64,
    pub credential_id: CredentialId,
    pub provider_id: ProviderId,
    pub channel_id: ChannelId,
    pub model: String,
    pub upstream_model: String,
    pub client: Option<ClientId>,
    pub lane_id: Option<LaneId>,
    pub node: Option<String>,
    pub status: u16,
    pub error_type: Option<String>,
    pub latency_ms: u32,
    pub ttfb_ms: Option<u32>,
    pub usage: TokenUsage,
    /// 上游原始 usage，用于对账与将来补新类型
    pub usage_raw: serde_json::Value,
    pub usage_source: UsageSource,
    pub cost: Money,
    /// 请求发生时的费率快照——**改价不改历史**
    pub price_snapshot: serde_json::Value,
}

// ---------------------------------------------------------------- 日志模式

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LogMode {
    Off,
    /// 默认：只记元数据 + token 细分 + 花费，不记 body
    #[default]
    Meta,
    /// meta + body 的字段结构（不记内容）
    Shape,
    /// meta + 头 + body（脱敏 + 压缩）
    Full,
}

impl LogMode {
    pub fn records_body(&self) -> bool {
        matches!(self, LogMode::Full)
    }
    pub fn records_shape(&self) -> bool {
        matches!(self, LogMode::Shape | LogMode::Full)
    }
    pub fn enabled(&self) -> bool {
        !matches!(self, LogMode::Off)
    }
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    #[serde(rename = "7d")]
    D7,
    #[serde(rename = "30d")]
    D30,
    #[serde(rename = "90d")]
    D90,
    Forever,
}

impl Retention {
    pub fn as_ms(&self) -> Option<i64> {
        match self {
            Retention::D7 => Some(7 * 86_400_000),
            Retention::D30 => Some(30 * 86_400_000),
            Retention::D90 => Some(90 * 86_400_000),
            Retention::Forever => None,
        }
    }
}

// ---------------------------------------------------------------- 请求摘要（事件/实时流）

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RequestSummary {
    pub request_id: RequestId,
    pub ts_ms: i64,
    pub model: String,
    pub upstream_model: String,
    pub channel_id: ChannelId,
    pub client: Option<ClientId>,
    pub lane_id: Option<LaneId>,
    pub node: Option<String>,
    pub status: u16,
    /// `ok` | `retried` | `failed`
    pub outcome: String,
    pub latency_ms: u32,
    pub ttfb_ms: Option<u32>,
    pub total_tokens: u64,
    pub cost: Money,
    pub attempts: u32,
    pub error_type: Option<String>,
}

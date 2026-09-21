//! 统一错误类型与错误码。
//!
//! **契约**：`docs/11-internal-contracts.md` §5 的错误码总表。
//! 新增错误必须同时更新那张表，否则前端与 CLI 无法识别。

use thiserror::Error;

pub type Result<T, E = TaError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum TaError {
    // ---- 请求层 ----
    #[error("本地密钥无效或缺失")]
    Unauthorized,
    #[error("请求过于频繁（超过本机 RPM 限制）")]
    RateLimited,
    #[error("请求体过大：{size} 字节 > 上限 {limit} 字节")]
    BodyTooLarge { size: usize, limit: usize },
    #[error("请求格式不合法：{0}")]
    MalformedRequest(String),

    // ---- 路由层 ----
    #[error("模型不存在：{0}")]
    ModelNotFound(String),
    #[error("通配路径无法确定模型名")]
    NoRoute,
    #[error("无可用通道：{message}")]
    NoViableChannel { message: String },
    #[error("全部通道额度已耗尽")]
    AllChannelsExhausted,
    #[error("全部通道失败：{message}")]
    AllChannelsFailed { message: String },
    #[error("跨协议转换会丢失内容，已拒绝转发：{0}")]
    CapabilityLoss(String),

    // ---- 网络层 ----
    #[error("内核未就绪，且没有可用的直连通道")]
    KernelNotReady,
    #[error("线路不可用：{lane}")]
    LaneUnavailable { lane: String },
    #[error("流式响应已开始输出后上游断连")]
    StreamBroken,
    #[error("凭据无效或已被封禁：{credential}")]
    CredentialInvalid { credential: String },

    // ---- 配置层 ----
    #[error("配置校验失败")]
    ConfigInvalid { errors: Vec<FieldError> },
    #[error("对象被引用，禁止删除：{what}")]
    Referenced { what: String },
    #[error("检测到明文密钥，请先迁移到系统钥匙串")]
    PlaintextSecret,

    // ---- 存储 / 内部 ----
    #[error("存储错误：{0}")]
    Store(String),
    #[error("内核错误：{0}")]
    Kernel(String),
    #[error("密钥存储错误：{0}")]
    Secret(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FieldError {
    /// 字段路径，如 `channels[2].credential`
    pub path: String,
    pub code: String,
    pub message: String,
}

impl TaError {
    /// [11 §5] 错误码
    pub fn code(&self) -> &'static str {
        use TaError::*;
        match self {
            Unauthorized => "TA-1001",
            RateLimited => "TA-1002",
            BodyTooLarge { .. } => "TA-1003",
            MalformedRequest(_) => "TA-1004",
            ModelNotFound(_) => "TA-2001",
            NoRoute => "TA-2002",
            NoViableChannel { .. } => "TA-2003",
            AllChannelsExhausted => "TA-2004",
            AllChannelsFailed { .. } => "TA-2005",
            CapabilityLoss(_) => "TA-2006",
            KernelNotReady => "TA-3001",
            LaneUnavailable { .. } => "TA-3002",
            StreamBroken => "TA-3003",
            CredentialInvalid { .. } => "TA-4001",
            ConfigInvalid { .. } => "TA-5001",
            Referenced { .. } => "TA-5002",
            PlaintextSecret => "TA-5003",
            Store(_) | Kernel(_) | Secret(_) | Internal(_) => "TA-9001",
        }
    }

    /// 错误信封里的 `type`（对外可见）
    pub fn type_str(&self) -> &'static str {
        use TaError::*;
        match self {
            Unauthorized => "tokenagency_unauthorized",
            RateLimited => "tokenagency_rate_limited",
            BodyTooLarge { .. } => "tokenagency_body_too_large",
            MalformedRequest(_) => "tokenagency_malformed_request",
            ModelNotFound(_) => "tokenagency_model_not_found",
            NoRoute => "tokenagency_no_route",
            NoViableChannel { .. } => "tokenagency_no_viable_channel",
            AllChannelsExhausted => "tokenagency_all_channels_exhausted",
            AllChannelsFailed { .. } => "tokenagency_all_channels_failed",
            CapabilityLoss(_) => "tokenagency_capability_loss",
            KernelNotReady => "tokenagency_kernel_not_ready",
            LaneUnavailable { .. } => "tokenagency_lane_unavailable",
            StreamBroken => "tokenagency_stream_broken",
            CredentialInvalid { .. } => "tokenagency_credential_invalid",
            ConfigInvalid { .. } => "tokenagency_config_invalid",
            Referenced { .. } => "tokenagency_referenced",
            PlaintextSecret => "tokenagency_plaintext_secret",
            Store(_) | Kernel(_) | Secret(_) | Internal(_) => "tokenagency_internal",
        }
    }

    pub fn http_status(&self) -> u16 {
        use TaError::*;
        match self {
            Unauthorized => 401,
            RateLimited => 429,
            BodyTooLarge { .. } => 413,
            MalformedRequest(_) | NoRoute => 400,
            ModelNotFound(_) => 404,
            NoViableChannel { .. } | CapabilityLoss(_) => 415,
            AllChannelsExhausted => 402,
            AllChannelsFailed { .. } => 502,
            KernelNotReady | LaneUnavailable { .. } => 503,
            CredentialInvalid { .. } => 401,
            Referenced { .. } => 409,
            ConfigInvalid { .. } | PlaintextSecret => 400,
            StreamBroken => 500,
            Store(_) | Kernel(_) | Secret(_) | Internal(_) => 500,
        }
    }

    pub fn retryable(&self) -> bool {
        use TaError::*;
        matches!(
            self,
            RateLimited | AllChannelsFailed { .. } | KernelNotReady | LaneUnavailable { .. }
        )
    }
}

impl From<std::io::Error> for TaError {
    fn from(e: std::io::Error) -> Self {
        TaError::Internal(format!("io: {e}"))
    }
}
impl From<serde_json::Error> for TaError {
    fn from(e: serde_json::Error) -> Self {
        TaError::MalformedRequest(format!("json: {e}"))
    }
}

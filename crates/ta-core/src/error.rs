//! 统一错误类型与错误码。
//!
//! **契约**： 的错误码总表。
//! 新增错误必须同时更新那张表，否则前端与 CLI 无法识别。

use thiserror::Error;

use crate::domain::capabilities::CapabilityMismatchDetail;

pub type Result<T, E = TaError> = std::result::Result<T, E>;

#[cfg_attr(feature = "specta", derive(specta::Type))]
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
    /// **同一条路由下的多条线路，上游能力声明不一致**。
    ///
    /// 这不是"暂时没路由"（那是 2003），而是**配置本身矛盾**：把请求路由到能力不同的
    /// 上游会出现"有时有 vision、有时报错"这种不可预期行为，所以必须在**写入时**拒绝。
    ///
    /// 取 TA-2007：家族上紧贴 TA-2006 `capability_loss`（同为"能力"语义）且未占用；
    /// 400（客户端配置错误）+ **不可重试**（重试一百次配置也不会变一致）。
    /// `details` 带结构化逐字段差异（哪个模型、哪几条线路、每个字段各自是什么值）。
    #[error("线路能力不一致，不能路由到一起：{detail}")]
    CapabilityMismatch { detail: CapabilityMismatchDetail },

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
    #[error("配置校验失败：{errors:?}")]
    ConfigInvalid { errors: Vec<FieldError> },
    #[error("对象被引用，禁止删除：{what}")]
    Referenced { what: String },
    #[error("检测到明文密钥，请先迁移到系统钥匙串")]
    PlaintextSecret,

    // ---- 存储 / 内部 ----
    /// **用户主动暂停了网关** —— 不是故障，也不是"上游没了"。
    ///
    /// 为什么要单独一个码（而不是复用 3001/9001）：这个错误是**用户自己按出来的**，
    /// 必须一眼可辨。三条候选都被否掉：
    /// `TA-3001 KernelNotReady`(503) 会让用户以为内核坏了；`TA-1002 RateLimited`(429) 是
    /// `retryable=true`，**客户端会对着暂停的网关猛重试**；`TA-9001 Internal` 的对外 type
    /// 叫 `tokenagency_internal`，读起来像"我们崩了"。所以给 503 + **不可重试** + 自己的 type。
    #[error("网关已被暂停：这是你自己的操作，不是故障。点「恢复网关」即可继续。")]
    GatewayPaused,
    #[error("存储错误：{0}")]
    Store(String),
    #[error("内核错误：{0}")]
    Kernel(String),
    #[error("密钥存储错误：{0}")]
    Secret(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FieldError {
    /// 字段路径，如 `channels[2].credential`
    pub path: String,
    pub code: String,
    pub message: String,
}

impl TaError {
    /// 错误码
    pub fn code(&self) -> &'static str {
        use TaError::*;
        match self {
            Unauthorized => "TA-1001",
            RateLimited => "TA-1002",
            BodyTooLarge { .. } => "TA-1003",
            MalformedRequest(_) => "TA-1004",
            GatewayPaused => "TA-1005",
            ModelNotFound(_) => "TA-2001",
            NoRoute => "TA-2002",
            NoViableChannel { .. } => "TA-2003",
            AllChannelsExhausted => "TA-2004",
            AllChannelsFailed { .. } => "TA-2005",
            CapabilityLoss(_) => "TA-2006",
            CapabilityMismatch { .. } => "TA-2007",
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
            GatewayPaused => "tokenagency_gateway_paused",
            ModelNotFound(_) => "tokenagency_model_not_found",
            NoRoute => "tokenagency_no_route",
            NoViableChannel { .. } => "tokenagency_no_viable_channel",
            AllChannelsExhausted => "tokenagency_all_channels_exhausted",
            AllChannelsFailed { .. } => "tokenagency_all_channels_failed",
            CapabilityLoss(_) => "tokenagency_capability_loss",
            CapabilityMismatch { .. } => "tokenagency_capability_mismatch",
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
            MalformedRequest(_) | NoRoute | CapabilityMismatch { .. } => 400,
            ModelNotFound(_) => 404,
            NoViableChannel { .. } | CapabilityLoss(_) => 415,
            AllChannelsExhausted => 402,
            AllChannelsFailed { .. } => 502,
            KernelNotReady | LaneUnavailable { .. } | GatewayPaused => 503,
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

    /// 这个错误要带出去的**结构化 details**（`IpcError.details`）。
    ///
    /// 唯一真相源在 `TaError` 自己这里，命令层只负责把它塞进 `IpcError`：
    /// * `ConfigInvalid` → 字段路径数组；
    /// * `CapabilityMismatch` → **逐字段差异**（模型 / 线路 id / 每个字段各自的值）——
    ///   路由向导与策略页把这份 JSON 直接显示出来，所以它必须"能读懂"，不能只有一句
    ///   "能力不一致"。
    pub fn details_json(&self) -> Option<serde_json::Value> {
        match self {
            Self::ConfigInvalid { errors } => serde_json::to_value(errors).ok(),
            Self::CapabilityMismatch { detail } => serde_json::to_value(detail).ok(),
            _ => None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::capabilities::{CapabilityFieldDiff, CapabilityValue};

    fn mismatch() -> TaError {
        TaError::CapabilityMismatch {
            detail: CapabilityMismatchDetail {
                model: "deepseek-v4.1flash".to_string(),
                channels: vec!["ch-a".to_string(), "ch-b".to_string()],
                fields: vec![CapabilityFieldDiff {
                    field: "vision".to_string(),
                    values: vec![
                        CapabilityValue { channel: "ch-a".to_string(), value: "true".to_string() },
                        CapabilityValue { channel: "ch-b".to_string(), value: "false".to_string() },
                    ],
                }],
            },
        }
    }

    /// 那一行的**全部四列**都要对（code / type / HTTP / 可重试）。
    #[test]
    fn capability_mismatch_row_matches_the_error_table() {
        let err = mismatch();
        assert_eq!(err.code(), "TA-2007");
        assert_eq!(err.type_str(), "tokenagency_capability_mismatch");
        assert_eq!(err.http_status(), 400);
        assert!(!err.retryable(), "配置矛盾重试一百次也不会变一致 → 不可重试");
    }

    #[test]
    fn capability_mismatch_details_are_structured_and_readable() {
        let err = mismatch();
        let details = err.details_json().expect("必须有 details");
        let text = details.to_string();

        // 路由向导要能直接展示：模型、线路 id、字段名、各自的值，一个都不能少
        assert!(text.contains("deepseek-v4.1flash"), "{text}");
        assert!(text.contains("ch-a") && text.contains("ch-b"), "{text}");
        assert!(text.contains("vision"), "{text}");
        assert!(
            text.contains("\"value\":\"true\"") && text.contains("\"value\":\"false\""),
            "{text}"
        );

        // 人话摘要里也要带上模型与字段（只显示 message 的地方不至于只有一句"不一致"）
        let msg = err.to_string();
        assert!(msg.contains("deepseek-v4.1flash") && msg.contains("vision"), "{msg}");
    }

    #[test]
    fn only_the_two_structured_variants_carry_details() {
        assert!(TaError::Unauthorized.details_json().is_none());
        assert!(TaError::Internal("x".into()).details_json().is_none());
    }
}

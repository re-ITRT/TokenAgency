//! 配置：加载、序列化、校验、版本迁移、默认值。
//!
//! 分工（依据、、）：
//!
//! - **形状层**：[`config_json_schema`] 用 `schemars::schema_for!(ConfigDoc)` 导出结构（给前端
//!   zod 生成 / 编辑器补全）；[`from_yaml`] / [`from_json`] 在反序列化时强制类型与枚举，
//!   并用 `serde_path_to_error` 把错误定位到具体字段路径。JSON Schema 表达不了的
//!   数值域 / 非空 / 正则规则由 [`TaConfigValidator`] 的"形状检查"补上。
//! - **语义层**：[`TaConfigValidator`] 手写引用完整性、ID 唯一性、定价匹配约束，
//!   产出 [`ValidationReport`]——每条错误/警告都带精确字段路径（如 `channels[2].credential`）。
//! - **版本层**：[`migrate`] 负责把老配置升到 [`CURRENT_CONFIG_VERSION`]。
//! - **默认值层**：[`default_doc`] 返回与 `config.example.yaml` 等价的默认配置（首启向导用）。

use serde::{Deserialize, Serialize};

use crate::domain::*;
use crate::error::{FieldError, Result, TaError};

pub mod defaults;
pub mod migrate;
pub mod validator;

pub use migrate::{
    migrate, migrate_to, needs_migration, MigrationError, MigrationStep, CURRENT_CONFIG_VERSION,
    NEXT_CONFIG_VERSION,
};
pub use validator::TaConfigValidator;

/// 与 `config.example.yaml` 等价的默认配置（首启向导预填 / 库里还没有配置时的兜底）。
///
/// 语义见 `config/defaults.rs`；产出一定能通过 [`validate`]。
pub fn default_doc() -> ConfigDoc {
    defaults::build_default_doc()
}

/// 校验结果。`ok == errors.is_empty()`；警告不阻断。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidationReport {
    pub ok: bool,
    pub errors: Vec<FieldError>,
    pub warnings: Vec<FieldError>,
}

impl ValidationReport {
    pub fn ok() -> Self {
        Self { ok: true, errors: vec![], warnings: vec![] }
    }

    /// 记一条**阻断性**错误（同时把 `ok` 置为 false）。
    pub fn push_error(
        &mut self,
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.ok = false;
        self.errors.push(FieldError {
            path: path.into(),
            code: code.into(),
            message: message.into(),
        });
    }

    /// 记一条**警告**（不影响 `ok`）。
    pub fn push_warning(
        &mut self,
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.warnings.push(FieldError {
            path: path.into(),
            code: code.into(),
            message: message.into(),
        });
    }

    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// 全部错误路径（前端"跳到出错字段"用，顺序稳定即声明顺序）。
    pub fn error_paths(&self) -> Vec<&str> {
        self.errors.iter().map(|e| e.path.as_str()).collect()
    }

    /// 转成 `TaError::ConfigInvalid`（TA-5001），方便命令层直接 `?`。
    pub fn into_result(self) -> Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(TaError::ConfigInvalid { errors: self.errors })
        }
    }
}

impl Default for ValidationReport {
    fn default() -> Self {
        Self::ok()
    }
}

/// 校验由 `ta-core::config` 实现（引用完整性 + 语义规则）。
/// JSON Schema 校验只负责"形状"，语义规则（引用是否存在）必须在这里补。
pub trait ConfigValidator: Send + Sync {
    fn validate(&self, doc: &ConfigDoc) -> ValidationReport;
}

/// 便捷入口：`TaConfigValidator::new().validate(doc)`。
pub fn validate(doc: &ConfigDoc) -> ValidationReport {
    TaConfigValidator::new().validate(doc)
}

/// 导出配置的 JSON Schema（给前端 zod 生成与编辑器补全用）。
pub fn config_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ConfigDoc)).unwrap_or(serde_json::Value::Null)
}

/// 从 YAML 读配置。**错误里带字段路径**（`serde_path_to_error`）。
pub fn from_yaml(s: &str) -> Result<ConfigDoc> {
    let de = serde_yaml_ng::Deserializer::from_str(s);
    serde_path_to_error::deserialize(de).map_err(|e| TaError::ConfigInvalid {
        errors: vec![FieldError {
            path: e.path().to_string(),
            code: "yaml".into(),
            message: e.inner().to_string(),
        }],
    })
}

pub fn to_yaml(doc: &ConfigDoc) -> Result<String> {
    serde_yaml_ng::to_string(doc).map_err(|e| TaError::Internal(format!("yaml: {e}")))
}

pub fn from_json(s: &str) -> Result<ConfigDoc> {
    let de = &mut serde_json::Deserializer::from_str(s);
    serde_path_to_error::deserialize(de).map_err(|e| TaError::ConfigInvalid {
        errors: vec![FieldError {
            path: e.path().to_string(),
            code: "json".into(),
            message: e.inner().to_string(),
        }],
    })
}

pub fn to_json(doc: &ConfigDoc) -> Result<String> {
    serde_json::to_string(doc).map_err(|e| TaError::Internal(format!("json: {e}")))
}

/// 语义等价：按序列化结果比较，忽略字段顺序与 `extra` 的插入顺序。
///
/// 用于 YAML/JSON 往返测试与"迁移是否改了东西"的判断。
pub fn semantic_eq(a: &ConfigDoc, b: &ConfigDoc) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ------------------------------------------------------------ 默认配置

    #[test]
    fn default_doc_satisfies_frozen_defaults() {
        let doc = default_doc();
        assert_eq!(doc.version, CURRENT_CONFIG_VERSION);
        assert_eq!(doc.gateway.listen, GatewayConfig::default().listen);
        assert_eq!(doc.request_log.retention, RetentionDefault::D30);
        assert_eq!(doc.net.kernel.controller, "127.0.0.1:39090");
        assert_eq!(doc.pricing.unit, PriceUnit::Per1M);
        // extras 为空：默认配置里不该塞任何"未知字段"
        assert!(doc.extra.is_empty());
        assert!(doc.gateway.extra.is_empty());
        assert!(doc.net.extra.is_empty());
    }

    // ------------------------------------------------------------ enabled 默认值

    /// 最小可用配置：所有实体都**省略** `enabled`。
    const ENABLED_OMITTED_YAML: &str = r#"
version: 1
proxy_subscriptions:
  - { id: sub-a, name: "机场A", url: "https://example.com/sub?token=x" }
lanes:
  - { id: lane-direct, name: "直连", kind: direct }
providers:
  - { id: p-a, name: "A", protocol: openai, base_url: "https://a.example/v1" }
credentials:
  - { id: c-a, provider: p-a, label: "A", secret: "${secret:a}" }
models:
  - { id: gpt-4o }
channels:
  - { id: ch-a, model: gpt-4o, provider: p-a, credential: c-a, upstream_model: gpt-4o }
channel_groups:
  - { id: grp-a, model: gpt-4o, members: [ch-a] }
policies:
  - { id: pol-a, name: "P", priority: 1000 }
"#;

    /// 同一份配置，但每个实体都**显式**写 `enabled: false`。
    const ENABLED_FALSE_YAML: &str = r#"
version: 1
proxy_subscriptions:
  - { id: sub-a, name: "机场A", url: "https://example.com/sub?token=x", enabled: false }
lanes:
  - { id: lane-direct, name: "直连", kind: direct, enabled: false }
providers:
  - { id: p-a, name: "A", protocol: openai, base_url: "https://a.example/v1", enabled: false }
credentials:
  - { id: c-a, provider: p-a, label: "A", secret: "${secret:a}", enabled: false }
models:
  - { id: gpt-4o }
channels:
  - { id: ch-a, model: gpt-4o, provider: p-a, credential: c-a, upstream_model: gpt-4o, enabled: false }
channel_groups:
  - { id: grp-a, model: gpt-4o, members: [ch-a] }
policies:
  - { id: pol-a, name: "P", priority: 1000, enabled: false }
"#;

    #[test]
    fn entity_enabled_defaults_to_true_when_omitted() {
        // 「我把它列进配置了」== 「我要用它」。停用必须显式写 false。
        let doc = from_yaml(ENABLED_OMITTED_YAML).expect("应当能读");
        assert!(doc.proxy_subscriptions[0].enabled, "Subscription 省略 enabled 应当为 true");
        assert!(doc.lanes[0].enabled, "Lane 省略 enabled 应当为 true");
        assert!(doc.providers[0].enabled, "Provider 省略 enabled 应当为 true");
        assert!(doc.credentials[0].enabled, "Credential 省略 enabled 应当为 true");
        assert!(doc.channels[0].enabled, "Channel 省略 enabled 应当为 true");
        assert!(doc.policies[0].enabled, "Policy 省略 enabled 应当为 true");
        // 组内开关（不是"实体启用"但同样是"列出来 = 要用"）
        assert!(doc.proxy_subscriptions[0].health_check.enabled);
        assert!(doc.channel_groups[0].sticky.enabled);
        assert!(doc.models[0].expose);
    }

    #[test]
    fn entity_enabled_false_is_respected() {
        let doc = from_yaml(ENABLED_FALSE_YAML).expect("应当能读");
        assert!(!doc.proxy_subscriptions[0].enabled);
        assert!(!doc.lanes[0].enabled);
        assert!(!doc.providers[0].enabled);
        assert!(!doc.credentials[0].enabled);
        assert!(!doc.channels[0].enabled);
        assert!(!doc.policies[0].enabled);
    }

    #[test]
    fn experimental_switches_stay_disabled_by_default() {
        // 只有两个例外：接管整机网络的开关，默认必须是关的（核心承诺）。
        let doc = from_yaml("version: 1").expect("应当能读");
        assert!(!doc.system_integration.enable_tun);
        assert!(!doc.system_integration.enable_system_proxy);

        // 显式开启仍然生效
        let doc = from_yaml(
            "version: 1\nsystem_integration: { enable_tun: true, enable_system_proxy: true }\n",
        )
        .expect("应当能读");
        assert!(doc.system_integration.enable_tun);
        assert!(doc.system_integration.enable_system_proxy);
    }

    /// 一致性自查（契约纪律）：**同一语义的字段，默认值必须一致**。
    ///
    /// 四类语义各自的默认值在这里被钉死；任何新加的 bool 字段都应当能归入其中一类。
    #[test]
    fn boolean_defaults_are_consistent_within_each_semantic_class() {
        let doc = from_yaml("version: 1").expect("应当能读");

        // A) 实体启用开关：省略 = true（实体由 `entity_enabled_defaults_to_true_when_omitted` 覆盖）
        // B) opt-in 的实验 / 危险 / 会改请求的开关：省略 = false
        assert!(!doc.net.system_proxy, "绝不碰系统代理");
        assert!(!doc.net.tun, "绝不接管整机流量");
        assert!(!doc.system_integration.enable_tun);
        assert!(!doc.system_integration.enable_system_proxy);
        assert!(!doc.system_integration.autostart, "会写 OS 状态的开关一律默认关（09 §1.2 E1）");
        assert!(!doc.system_integration.file_watch_mode, "换真源属于 opt-in");
        assert!(!doc.gateway.metering.inject_stream_usage, "会修改客户端请求");
        assert!(!doc.gateway.retry.retry_after_first_byte, "已输出字节后重试 = 重复计费");
        assert!(!TlsConfig::default().insecure, "默认必须校验证书");

        // C) 无损 / 自检类默认：省略 = true
        assert!(doc.gateway.debug_headers);
        assert!(doc.gateway.passthrough.verify_no_diff);
        assert!(doc.gateway.passthrough.strip_hop_by_hop);
        assert!(doc.net.health.url_by_provider);
        assert!(doc.system_integration.tray_show_latency);
    }

    /// ** E1**：三个"会写操作系统状态"的开关，省略时**必须全部为 false**。
    ///
    /// 放在一条测试里成组断言：将来有人（或某个 agent）只改错其中一个，
    /// 这条会连带报出来 —— 单个开关各写一条测试时，改错一个反而容易漏。
    #[test]
    fn os_state_writing_switches_default_off() {
        let doc = from_yaml("version: 1").expect("应当能读");
        assert!(!doc.system_integration.autostart, "autostart 默认必须是关的");
        assert!(!doc.system_integration.enable_tun, "enable_tun 默认必须是关的");
        assert!(!doc.system_integration.enable_system_proxy, "enable_system_proxy 默认必须是关的");

        // 显式打开仍然生效（我们是 opt-in，不是禁用）
        let on = from_yaml(
            "version: 1\nsystem_integration: { autostart: true, enable_tun: true, enable_system_proxy: true }\n",
        )
        .expect("应当能读");
        assert!(on.system_integration.autostart);
        assert!(on.system_integration.enable_tun);
        assert!(on.system_integration.enable_system_proxy);

        // 手写的 `Default` 也必须与 serde 字段默认逐字段一致（别踩"省略整节 ≠ 写空节"的坑）
        let manual = SystemIntegration::default();
        assert!(!manual.autostart);
        assert!(!manual.enable_tun);
        assert!(!manual.enable_system_proxy);
        assert!(manual.tray_show_latency);
    }

    /// 开机自启动的三个字段：省略走默认，显式值必须被尊重。
    #[test]
    fn autostart_fields_default_and_explicit() {
        // 省略 → false / Tray / 0
        let doc = from_yaml("version: 1\nsystem_integration: {}\n").expect("应当能读");
        assert!(!doc.system_integration.autostart);
        assert_eq!(doc.system_integration.autostart_mode, AutostartMode::Tray);
        assert_eq!(doc.system_integration.autostart_delay_s, 0);
        // 整节都不写，结果必须一样
        let omitted = from_yaml("version: 1\n").expect("应当能读");
        assert!(!omitted.system_integration.autostart);
        assert_eq!(omitted.system_integration.autostart_mode, AutostartMode::Tray);
        assert_eq!(omitted.system_integration.autostart_delay_s, 0);

        // 显式值 → 生效
        let doc = from_yaml(
            "version: 1\nsystem_integration: { autostart: true, autostart_mode: window, autostart_delay_s: 30 }\n",
        )
        .expect("应当能读");
        assert!(doc.system_integration.autostart);
        assert_eq!(doc.system_integration.autostart_mode, AutostartMode::Window);
        assert_eq!(doc.system_integration.autostart_delay_s, 30);

        // 显式写回默认值也要能读（示例配置会这么写）
        let doc = from_yaml(
            "version: 1\nsystem_integration: { autostart: false, autostart_mode: tray, autostart_delay_s: 0 }\n",
        )
        .expect("应当能读");
        assert!(!doc.system_integration.autostart);
        assert_eq!(doc.system_integration.autostart_mode, AutostartMode::Tray);
        assert_eq!(doc.system_integration.autostart_delay_s, 0);
    }

    /// `AutostartMode` 的枚举取值与默认值。
    #[test]
    fn autostart_mode_values_and_default() {
        assert_eq!(AutostartMode::default(), AutostartMode::Tray);
        // 序列化成 snake_case，前端 bindings 依赖这个取值
        assert_eq!(serde_json::to_value(AutostartMode::Tray).unwrap(), serde_json::json!("tray"));
        assert_eq!(
            serde_json::to_value(AutostartMode::Window).unwrap(),
            serde_json::json!("window")
        );
        assert!(from_yaml("version: 1\nsystem_integration: { autostart_mode: bogus }\n").is_err());
    }

    /// 自查的第二条（防同类事故）：
    /// `#[serde(default)]` 加在**整节**上时用的是该结构体的 Rust `Default`，
    /// 而不是各字段的 `#[serde(default = "...")]`。所以"整节不写"和"写了空节"
    /// 必须得到**逐字段等价**的结果 —— 否则就是第二处默认值不一致。
    ///
    /// （这条测试曾经抓到 `SystemIntegration`：serde 说 `autostart: true`，
    /// 而 `#[derive(Default)]` 给的是 `false`。加 `autostart_mode` /
    /// `autostart_delay_s` 两个新字段时，同样靠它守住。）
    #[test]
    fn empty_section_equals_omitted_section() {
        let omitted = from_yaml("version: 1\n").expect("应当能读");
        let empty_sections = from_yaml(
            "version: 1\ngateway: {}\nrequest_log: {}\npricing: {}\nnet: {}\nsystem_integration: {}\n",
        )
        .expect("应当能读");
        assert!(
            semantic_eq(&omitted, &empty_sections),
            "整节缺失与空节必须逐字段等价：\n{A}\n{B}",
            A = to_yaml(&omitted).unwrap(),
            B = to_yaml(&empty_sections).unwrap()
        );

        // 逐字段核对 system_integration 的每一个字段（含本次新增的三个）
        for (label, si) in [
            ("omitted", &omitted.system_integration),
            ("empty_section", &empty_sections.system_integration),
        ] {
            assert!(!si.autostart, "{label}: autostart");
            assert_eq!(si.autostart_mode, AutostartMode::Tray, "{label}: autostart_mode");
            assert_eq!(si.autostart_delay_s, 0, "{label}: autostart_delay_s");
            assert!(!si.enable_tun, "{label}: enable_tun");
            assert!(!si.enable_system_proxy, "{label}: enable_system_proxy");
            assert!(si.tray_show_latency, "{label}: tray_show_latency");
            assert!(!si.file_watch_mode, "{label}: file_watch_mode");
            assert!(si.file_watch_path.is_none(), "{label}: file_watch_path");
        }
    }

    // ------------------------------------------------------------ YAML / JSON 往返

    #[test]
    fn yaml_roundtrip_is_semantically_equivalent() {
        let doc = default_doc();
        let yaml = to_yaml(&doc).expect("序列化 YAML 失败");
        assert!(yaml.contains("channels:"), "YAML 应当含 channels：{yaml}");
        let back = from_yaml(&yaml).expect("读回 YAML 失败");
        assert!(semantic_eq(&doc, &back), "YAML 往返语义必须等价");
        assert_eq!(back.channels.len(), 4);
        assert_eq!(back.net.kernel.lane_port_range, [39100, 39299]);
    }

    #[test]
    fn json_roundtrip_is_semantically_equivalent() {
        let doc = default_doc();
        let json = to_json(&doc).expect("序列化 JSON 失败");
        let back = from_json(&json).expect("读回 JSON 失败");
        assert!(semantic_eq(&doc, &back), "JSON 往返语义必须等价");
    }

    #[test]
    fn unknown_fields_are_preserved_through_roundtrip() {
        let yaml = r#"
version: 1
future_top_level_flag: true
future_nested: { a: 1 }
gateway:
  listen: 127.0.0.1:9999
  future_gateway_field: "keep me"
providers:
  - id: p-x
    name: X
    protocol: openai
    base_url: https://x.example/v1
    future_provider_field: [1, 2, 3]
"#;
        let doc = from_yaml(yaml).expect("应当能读带未知字段的配置");
        assert_eq!(doc.extra.get("future_top_level_flag"), Some(&serde_json::json!(true)));
        assert_eq!(
            doc.gateway.extra.get("future_gateway_field"),
            Some(&serde_json::json!("keep me"))
        );
        assert_eq!(
            doc.providers[0].extra.get("future_provider_field"),
            Some(&serde_json::json!([1, 2, 3]))
        );

        // 写回再读，未知字段不能丢
        let back = from_yaml(&to_yaml(&doc).unwrap()).expect("二次读回失败");
        assert!(semantic_eq(&doc, &back));
        assert_eq!(back.extra.get("future_top_level_flag"), Some(&serde_json::json!(true)));
        assert_eq!(
            back.providers[0].extra.get("future_provider_field"),
            Some(&serde_json::json!([1, 2, 3]))
        );
        let yaml_out = to_yaml(&back).unwrap();
        assert!(yaml_out.contains("future_top_level_flag"), "{yaml_out}");
        assert!(yaml_out.contains("keep me"), "{yaml_out}");
    }

    #[test]
    fn semantic_eq_detects_a_difference() {
        let a = default_doc();
        let mut b = default_doc();
        assert!(semantic_eq(&a, &b));
        b.gateway.listen = "127.0.0.1:1".into();
        assert!(!semantic_eq(&a, &b));
    }

    // ------------------------------------------------------------ 解析错误定位

    #[test]
    fn from_yaml_reports_field_path_on_type_error() {
        let yaml = r#"
version: 1
gateway:
  max_body_mb: "not-a-number"
"#;
        let err = from_yaml(yaml).expect_err("类型不对必须报错");
        match err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, "yaml");
                assert!(
                    errors[0].path.contains("gateway") && errors[0].path.contains("max_body_mb"),
                    "路径应当定位到 gateway.max_body_mb，实际：{}",
                    errors[0].path
                );
                assert!(!errors[0].message.is_empty());
            }
            other => panic!("应当是 ConfigInvalid，实际 {other:?}"),
        }
        assert_eq!(TaError::ConfigInvalid { errors: vec![] }.code(), "TA-5001");
    }

    #[test]
    fn from_json_reports_field_path_on_type_error() {
        let json = r#"{"version":1,"channels":[{"id":"ch","model":"m","provider":"p","credential":"c","upstream_model":"u","priority":"high"}]}"#;
        let err = from_json(json).expect_err("priority 类型不对必须报错");
        match err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].code, "json");
                assert!(
                    errors[0].path.contains("channels") && errors[0].path.contains("priority"),
                    "实际路径：{}",
                    errors[0].path
                );
            }
            other => panic!("应当是 ConfigInvalid，实际 {other:?}"),
        }
    }

    // ------------------------------------------------------------ ValidationReport

    #[test]
    fn validation_report_helpers() {
        let mut report = ValidationReport::ok();
        assert!(report.is_ok());
        assert!(report.error_paths().is_empty());
        report.push_warning("gateway.listen", "invalid_value", "只是提醒");
        assert!(report.is_ok(), "警告不该影响 ok");
        assert_eq!(report.warnings.len(), 1);
        report.push_error("channels[2].credential", "not_found", "引用的凭据不存在");
        assert!(!report.is_ok());
        assert!(!report.ok);
        assert_eq!(report.error_paths(), vec!["channels[2].credential"]);

        let err = report.into_result().expect_err("有错误应当转成 TA-5001");
        assert_eq!(err.code(), "TA-5001");
        assert_eq!(err.http_status(), 400);
        assert!(ValidationReport::ok().into_result().is_ok());
        assert!(ValidationReport::default().is_ok());
    }

    // ------------------------------------------------------------ JSON Schema

    /// `gateway.expose_models_with_capabilities` 是**已删除的旧开关**。
    ///
    /// 不变量：老配置带着它必须**仍能解析**（值落进 `gateway.extra` 原样保留，前向兼容纪律），
    /// 但它已经不是结构体字段 —— 新写的配置线上形态里不会出现它，schema 里也不该再宣告它。
    #[test]
    fn removed_expose_models_with_capabilities_parses_but_is_no_longer_a_field() {
        let old = from_yaml("version: 1\ngateway:\n  expose_models_with_capabilities: true\n")
            .expect("带已删除开关的老配置必须仍能解析");
        assert_eq!(
            old.gateway.extra.get("expose_models_with_capabilities"),
            Some(&serde_json::Value::Bool(true)),
            "已删除开关的值必须原样落进 extra，不许被吞掉"
        );
        // 来自 extra → 整体往返时原样回去（未知键 round-trip 不丢）；关键是它**不是**字段
        let value = serde_json::to_value(&old).expect("序列化");
        assert_eq!(value["gateway"]["expose_models_with_capabilities"], serde_json::json!(true));

        // 没写过它的配置：线上形态里**不许**凭空多出这个键
        let bare = from_yaml("version: 1\n").expect("应当能读");
        let value = serde_json::to_value(&bare).expect("序列化");
        assert!(
            value["gateway"].get("expose_models_with_capabilities").is_none(),
            "已删除的开关不许再出现在线上形态里：{value}"
        );
        // schema 也不再宣告它（前端类型生成跟着它走）
        let schema = serde_json::to_string(&config_json_schema()).expect("schema 可序列化");
        assert!(
            !schema.contains("expose_models_with_capabilities"),
            "已删除的开关不该还在 JSON Schema 里"
        );
    }

    #[test]
    fn config_json_schema_declares_the_whole_tree() {
        let schema = config_json_schema();
        assert!(schema.is_object(), "schema 必须是 JSON object");
        let text = serde_json::to_string(&schema).unwrap();
        for needle in ["ConfigDoc", "Provider", "Credential", "Channel", "ChannelGroup", "Policy"] {
            assert!(text.contains(needle), "schema 里应当有 {needle}");
        }
        // 字段名冻结：前端类型由它生成
        for field in
            ["upstream_model", "target_override", "pricing", "proxy_subscriptions", "default_lane"]
        {
            assert!(text.contains(field), "schema 里应当有字段 {field}");
        }
        assert_eq!(serde_json::to_value(&schema).unwrap(), schema);
    }
}

//! 配置：加载、序列化、校验。

use serde::{Deserialize, Serialize};

use crate::domain::ConfigDoc;
use crate::error::{FieldError, Result, TaError};

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
}

/// 校验由 `ta-core::config` 实现（引用完整性 + 语义规则）。
/// JSON Schema 校验只负责"形状"，语义规则（引用是否存在）必须在这里补。
pub trait ConfigValidator: Send + Sync {
    fn validate(&self, doc: &ConfigDoc) -> ValidationReport;
}

/// 导出配置的 JSON Schema（给前端 zod 生成与编辑器补全用）。
pub fn config_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ConfigDoc))
        .unwrap_or(serde_json::Value::Null)
}

/// 从 YAML 读配置。**错误里带字段路径**（`serde_path_to_error`）。
pub fn from_yaml(s: &str) -> Result<ConfigDoc> {
    let de = serde_yaml_ng::Deserializer::from_str(s);
    serde_path_to_error::deserialize(de).map_err(|e| {
        TaError::ConfigInvalid {
            errors: vec![FieldError {
                path: e.path().to_string(),
                code: "yaml".into(),
                message: e.inner().to_string(),
            }],
        }
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

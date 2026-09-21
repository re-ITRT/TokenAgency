//! 环境变量密钥来源（`${env:VAR}`）。**只读**：CI / 脚本场景下由外部注入。
//!
//! 存在的意义：让同一条 `${env:OPENAI_API_KEY}` 在 CI 里能跑，而在桌面上
//! [`crate::composite::CompositeStore`] 仍优先用钥匙串。
//!
//! 设计取舍：
//! * `set` / `delete` 明确报错——进程环境是外部所有者的，写它只会制造"看起来生效、
//!   重启就没了"的幻觉，**不静默失败**。
//! * `list_names` 返回空：枚举整个进程环境会把无关变量（可能含别的程序的密钥）
//!   搬进 UI，属于越权。需要列环境变量来源的条目，请以配置里的引用为准。

use async_trait::async_trait;

use ta_core::error::{Result, TaError};

use crate::resolver::EnvSource;
use crate::SecretStore;

/// 环境变量密钥来源。
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvStore;

impl EnvStore {
    pub const fn new() -> Self {
        Self
    }

    /// 从进程环境读一个变量；空串按"未设置"处理。
    pub fn read(var: &str) -> Option<String> {
        match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => Some(v),
            _ => None,
        }
    }
}

#[async_trait]
impl SecretStore for EnvStore {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        crate::resolver::validate_env_var(name)
            .map_err(|why| TaError::Secret(format!("环境变量名不合法：{why}")))?;
        Ok(Self::read(name))
    }

    async fn set(&self, name: &str, _value: &str) -> Result<()> {
        // 只读来源：不吞掉写请求，明确报错。
        Err(TaError::Secret(format!(
            "环境变量来源是只读的，无法写入：{name}（请在 shell / CI 里设置该变量）"
        )))
    }

    async fn delete(&self, name: &str) -> Result<()> {
        Err(TaError::Secret(format!("环境变量来源是只读的，无法删除：{name}")))
    }

    async fn list_names(&self) -> Result<Vec<String>> {
        // 见模块文档：不枚举进程环境，避免把无关变量搬进 UI。
        Ok(Vec::new())
    }
}

impl EnvSource for EnvStore {
    fn get(&self, key: &str) -> Option<String> {
        Self::read(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::block_on;

    // `EnvStore` 同时实现 `SecretStore::get` 与 `EnvSource::get`（一个 async 一个同步），
    // 所以测试里用完整限定语法调用，避免 E0034 歧义。

    #[test]
    fn get_reads_process_env_without_writing_it() {
        // 只读一个确定存在、且不是密钥的变量，避免污染全局环境。
        let store = EnvStore::new();
        let var = "PATH";
        let direct = std::env::var(var).ok();
        let via_store = block_on(SecretStore::get(&store, var)).expect("读取不应失败");
        assert_eq!(via_store, direct.filter(|v| !v.trim().is_empty()));
    }

    #[test]
    fn get_missing_variable_is_none() {
        let store = EnvStore::new();
        let out =
            block_on(SecretStore::get(&store, "TOKENAGENCY_DEFINITELY_NOT_SET_VAR")).expect("ok");
        assert_eq!(out, None);
    }

    #[test]
    fn get_rejects_bad_names() {
        let store = EnvStore::new();
        let err = block_on(SecretStore::get(&store, "bad name")).expect_err("应报错");
        assert!(matches!(err, TaError::Secret(_)));
    }

    #[test]
    fn set_and_delete_are_explicit_errors() {
        let store = EnvStore::new();
        let err = block_on(SecretStore::set(&store, "FOO", "bar")).expect_err("只读来源必须报错");
        assert!(err.to_string().contains("只读"), "原因应可读：{err}");
        let err = block_on(SecretStore::delete(&store, "FOO")).expect_err("只读来源必须报错");
        assert!(err.to_string().contains("只读"), "原因应可读：{err}");
    }

    #[test]
    fn list_names_is_empty_by_design() {
        let store = EnvStore::new();
        assert!(block_on(SecretStore::list_names(&store)).expect("ok").is_empty());
    }

    #[test]
    fn implements_env_source() {
        let store = EnvStore::new();
        let source: &dyn EnvSource = &store;
        assert_eq!(source.get("TOKENAGENCY_DEFINITELY_NOT_SET_VAR"), None);
    }
}

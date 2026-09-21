//! `ta-secrets` — 密钥存储与引用解析。
//!
//! 契约：`docs/11-internal-contracts.md` §1.4。
//! **实现由工作包 P7 完成。**
//!
//! 硬约束：key **绝不进 SQLite**；`.yaml` 里只允许 `${secret:name}` / `${env:VAR}`。

pub mod env;
pub mod keyring_store;
pub mod resolver;

use async_trait::async_trait;

use ta_core::error::Result;

/// 密钥仓库。取值顺序：OS 钥匙串 → 便携模式加密文件 → 环境变量。
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<String>>;
    async fn set(&self, name: &str, value: &str) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
    async fn list_names(&self) -> Result<Vec<String>>;
}

//! `ta-secrets` — 密钥存储与引用解析。
//!
//! 契约：`docs/11-internal-contracts.md` §1.4。
//! **实现由工作包 P7 完成。**
//!
//! 硬约束：key **绝不进 SQLite**；`.yaml` 里只允许 `${secret:name}` / `${env:VAR}`。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`resolver`] | 引用语法解析、明文识别、`SecretString`（脱敏） |
//! | [`keyring_store`] | OS 钥匙串（Windows Credential Manager / macOS Keychain / Secret Service） |
//! | [`portable`] | 便携模式本地**加密**文件（v2 = argon2id + ChaCha20-Poly1305；v1 明文仅只读兼容） |
//! | [`env`] | 环境变量来源（只读） |
//! | [`composite`] | 钥匙串 → 便携文件 → 环境变量 的优先级链与降级 |
//! | [`migrate`] | 明文扫描 `scan_plaintext` 与迁移 `migrate` |
//! | [`mock`] | 内存 `SecretStore` 测试替身 + 极简 `block_on`（本 crate 无 tokio） |
//!
//! # 安全约定（务必遵守）
//!
//! 1. **错误消息里禁止出现密钥值**，也不回显引用原文（那可能就是误贴的明文 key）；
//!    只有*条目名*（`secret_meta` 的元数据）可以出现在消息里。
//! 2. 取值一律经 [`resolver::SecretString`]，它的 `Debug`/`Display` 已脱敏，
//!    `tracing` 里请只记录 [`mask_secret`] 的结果。
//! 3. 便携模式文件落在 `<AppData>/TokenAgency/secrets.enc`：**v2 是加密的**
//!    （口令派生密钥，没有口令就拒绝写入任何文件）；Windows 目录本身是用户级隔离，
//!    Unix 下写 0600（见 [`portable`]）。

pub mod composite;
pub mod env;
pub mod keyring_store;
pub mod migrate;
pub mod mock;
pub mod portable;
pub mod resolver;

use async_trait::async_trait;

use ta_core::error::Result;

pub use composite::{CompositeStore, NamedStore, SourceKind};
pub use env::EnvStore;
pub use keyring_store::{EntryFactory, KeyringStore, SERVICE};
pub use migrate::{
    migrate, migrate_with_report, scan_plaintext, suggested_names, MigratedSecret, MigrationReport,
    PlaintextFinding,
};
pub use portable::{DiskState, PortableStore};
pub use resolver::{
    classify_literal, is_reference, resolve_ref, resolve_spec, resolve_value, EnvSource, MapEnv,
    PlaintextKind, ProcessEnv, SecretRef, SecretString, ENV_SCHEME, SECRET_SCHEME,
};

/// 密钥仓库。取值顺序：OS 钥匙串 → 便携模式加密文件 → 环境变量。
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<String>>;
    async fn set(&self, name: &str, value: &str) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
    async fn list_names(&self) -> Result<Vec<String>>;
}

/// 把密钥值脱敏到"可安全写进日志/trace/DB"的程度。
///
/// 只保留前 4 个字符（够认人、不足以复原）。长度 ≤ 8 或为空时全遮。
///
/// ```
/// assert_eq!(ta_secrets::mask_secret("sk-abcdefghijklmn"), "sk-a***");
/// assert_eq!(ta_secrets::mask_secret("short"), "***");
/// assert_eq!(ta_secrets::mask_secret(""), "<空>");
/// ```
pub fn mask_secret(value: &str) -> String {
    let count = value.chars().count();
    if count == 0 {
        return "<空>".to_string();
    }
    if count <= 8 {
        return "***".to_string();
    }
    let head: String = value.chars().take(4).collect();
    format!("{head}***")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_never_returns_the_value() {
        let secret = "sk-abcdefghijkl";
        let masked = mask_secret(secret);
        assert!(!masked.contains("efghijkl"), "脱敏结果泄漏了密钥主体：{masked}");
        assert_eq!(masked, "sk-a***");
        assert_eq!(mask_secret("12345678"), "***");
        assert_eq!(mask_secret("123456789"), "1234***");
    }
}

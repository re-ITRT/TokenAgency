//! 组合密钥仓库：**钥匙串 → 便携文件 → 环境变量**。
//!
//! 取值按这个顺序短路；某个来源**报错**（headless Linux 没有 Secret Service、
//! 钥匙串被锁、便携文件损坏）时不整体失败，而是继续往下一级降级——
//! 但降级不是静默的：所有来源都不可用时会把每个来源的可读原因汇总成一个
//! [`TaError::Secret`]，UI 能直接展示"为什么取不到"。
//!
//! 语义细节（写死在测试里）：
//!
//! * `get`：某个来源明确回答"没有"（`Ok(None)`）就算"没找到"；
//!   只有在**所有**来源都出错（没有任何一个明确回答"没有"）时才返回 `Err`。
//! * `set`：写第一个**可用**来源；环境变量来源只读，直接跳过（不会被写）。
//! * `delete`：所有来源都尝试一遍（幂等），只要有一个成功就算成功。
//! * `list_names`：并集 + 排序去重；某个来源出错只记 debug，不影响其余来源。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ta_core::error::{Result, TaError};

use crate::portable::PortableStore;
use crate::{EnvStore, KeyringStore, SecretStore};

/// 来源种类。顺序即优先级（见模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// 系统钥匙串 / 便携文件链上的主来源。
    Keyring,
    /// 便携模式本地文件。
    Portable,
    /// 环境变量（只读）。
    Env,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Keyring => "keyring",
            SourceKind::Portable => "portable",
            SourceKind::Env => "env",
        }
    }

    /// 环境变量来源不接受写入。
    pub fn is_writable(&self) -> bool {
        !matches!(self, SourceKind::Env)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 带标签的来源。
#[derive(Clone)]
pub struct NamedStore {
    kind: SourceKind,
    store: Arc<dyn SecretStore>,
}

impl NamedStore {
    pub fn new<S: SecretStore + 'static>(kind: SourceKind, store: S) -> Self {
        Self { kind, store: Arc::new(store) }
    }

    pub fn shared(kind: SourceKind, store: Arc<dyn SecretStore>) -> Self {
        Self { kind, store }
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn store(&self) -> &Arc<dyn SecretStore> {
        &self.store
    }
}

impl fmt::Debug for NamedStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NamedStore({})", self.kind)
    }
}

/// 组合仓库。
#[derive(Clone)]
pub struct CompositeStore {
    sources: Vec<NamedStore>,
}

impl fmt::Debug for CompositeStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositeStore").field("sources", &self.sources).finish()
    }
}

impl CompositeStore {
    /// 按给定顺序组成优先级链。
    pub fn new(sources: Vec<NamedStore>) -> Self {
        Self { sources }
    }

    /// 生产默认链：钥匙串 → 便携文件 → 环境变量。
    pub fn with_defaults() -> Result<Self> {
        let portable = PortableStore::open(crate::portable::default_path()?);
        Ok(Self::new(vec![
            NamedStore::new(SourceKind::Keyring, KeyringStore::new()),
            NamedStore::new(SourceKind::Portable, portable),
            NamedStore::new(SourceKind::Env, EnvStore::new()),
        ]))
    }

    /// 用户显式选择"便携模式"：跳过钥匙串，只留便携文件 + 环境变量兜底。
    pub fn portable_mode(portable: PortableStore) -> Self {
        Self::new(vec![
            NamedStore::new(SourceKind::Portable, portable),
            NamedStore::new(SourceKind::Env, EnvStore::new()),
        ])
    }

    pub fn sources(&self) -> &[NamedStore] {
        &self.sources
    }

    pub fn kinds(&self) -> Vec<SourceKind> {
        self.sources.iter().map(|s| s.kind).collect()
    }
}

#[async_trait]
impl SecretStore for CompositeStore {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        let mut errors: Vec<String> = Vec::new();
        let mut miss = 0usize;

        for source in &self.sources {
            match source.store.get(name).await {
                Ok(Some(value)) if !value.trim().is_empty() => {
                    tracing::debug!(entry = %name, source = %source.kind, "密钥命中");
                    return Ok(Some(value));
                }
                Ok(Some(_)) => {
                    // 空值按"这个来源没有"处理，继续降级
                    tracing::debug!(entry = %name, source = %source.kind, "密钥条目为空，继续降级");
                    miss += 1;
                }
                Ok(None) => {
                    miss += 1;
                }
                Err(e) => {
                    tracing::debug!(entry = %name, source = %source.kind, "密钥来源不可用，降级");
                    errors.push(format!("{}: {e}", source.kind));
                }
            }
        }

        if !errors.is_empty() && miss == 0 {
            return Err(TaError::Secret(format!("所有密钥来源都不可用：{}", errors.join("；"))));
        }
        Ok(None)
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();

        for source in &self.sources {
            if !source.kind.is_writable() {
                tracing::debug!(entry = %name, source = %source.kind, "只读来源，跳过写入");
                continue;
            }
            match source.store.set(name, value).await {
                Ok(()) => {
                    tracing::debug!(entry = %name, source = %source.kind, "密钥已写入");
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!(entry = %name, source = %source.kind, "密钥写入失败，尝试下一来源");
                    errors.push(format!("{}: {e}", source.kind));
                }
            }
        }

        Err(TaError::Secret(format!(
            "没有可写入的密钥来源（环境变量来源只读）：{}",
            if errors.is_empty() {
                "链上没有可写来源".to_string()
            } else {
                errors.join("；")
            }
        )))
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();
        let mut ok = 0usize;

        for source in &self.sources {
            match source.store.delete(name).await {
                Ok(()) => ok += 1,
                Err(e) => errors.push(format!("{}: {e}", source.kind)),
            }
        }

        if ok > 0 {
            return Ok(());
        }
        Err(TaError::Secret(format!(
            "删除密钥条目失败：{}",
            if errors.is_empty() { "链上没有来源".to_string() } else { errors.join("；") }
        )))
    }

    async fn list_names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for source in &self.sources {
            match source.store.list_names().await {
                Ok(mut list) => names.append(&mut list),
                Err(e) => {
                    tracing::debug!(source = %source.kind, "来源无法枚举条目，跳过");
                    errors.push(format!("{}: {e}", source.kind));
                }
            }
        }

        names.sort();
        names.dedup();

        if names.is_empty() && errors.len() == self.sources.len() && !self.sources.is_empty() {
            return Err(TaError::Secret(format!("所有密钥来源都不可用：{}", errors.join("；"))));
        }
        Ok(names)
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{block_on, MockStore};

    fn chain(
        keyring: MockStore,
        portable: MockStore,
        env: MockStore,
    ) -> (CompositeStore, MockStore, MockStore, MockStore) {
        let composite = CompositeStore::new(vec![
            NamedStore::new(SourceKind::Keyring, keyring.clone()),
            NamedStore::new(SourceKind::Portable, portable.clone()),
            NamedStore::new(SourceKind::Env, env.clone()),
        ]);
        (composite, keyring, portable, env)
    }

    #[test]
    fn source_kinds_are_ordered_as_documented() {
        let composite = CompositeStore::new(vec![
            NamedStore::new(SourceKind::Keyring, MockStore::new()),
            NamedStore::new(SourceKind::Portable, MockStore::new()),
            NamedStore::new(SourceKind::Env, MockStore::new()),
        ]);
        assert_eq!(
            composite.kinds(),
            vec![SourceKind::Keyring, SourceKind::Portable, SourceKind::Env]
        );
        assert!(SourceKind::Keyring.is_writable());
        assert!(SourceKind::Portable.is_writable());
        assert!(!SourceKind::Env.is_writable(), "环境变量来源必须只读");
    }

    #[test]
    fn keyring_wins_over_portable_and_env() {
        let (composite, keyring, portable, env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        keyring.insert("k", "from-keyring-0123456789");
        portable.insert("k", "from-portable-0123456789");
        env.insert("k", "from-env-0123456789");
        assert_eq!(
            block_on(composite.get("k")).expect("ok"),
            Some("from-keyring-0123456789".to_string())
        );
    }

    #[test]
    fn falls_back_to_portable_when_keyring_has_no_entry() {
        let (composite, keyring, portable, env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        let _ = keyring;
        portable.insert("k", "from-portable-0123456789");
        env.insert("k", "from-env-0123456789");
        assert_eq!(
            block_on(composite.get("k")).expect("ok"),
            Some("from-portable-0123456789".to_string())
        );
    }

    #[test]
    fn falls_back_to_portable_when_keyring_errors() {
        // headless Linux / 钥匙串被锁 的情形
        let (composite, _keyring, portable, _env) = chain(
            MockStore::always_failing("无法访问系统钥匙串（headless）"),
            MockStore::new(),
            MockStore::new(),
        );
        portable.insert("k", "from-portable-0123456789");
        assert_eq!(
            block_on(composite.get("k")).expect("降级后应取到值"),
            Some("from-portable-0123456789".to_string())
        );
    }

    #[test]
    fn falls_back_to_env_when_keyring_errors_and_portable_misses() {
        let (composite, _keyring, _portable, env) =
            chain(MockStore::always_failing("钥匙串被锁"), MockStore::new(), MockStore::new());
        env.insert("k", "from-env-0123456789");
        assert_eq!(
            block_on(composite.get("k")).expect("ok"),
            Some("from-env-0123456789".to_string())
        );
    }

    #[test]
    fn empty_values_are_treated_as_missing() {
        let (composite, keyring, portable, _env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        keyring.insert("k", "   ");
        portable.insert("k", "from-portable-0123456789");
        assert_eq!(
            block_on(composite.get("k")).expect("ok"),
            Some("from-portable-0123456789".to_string())
        );
    }

    #[test]
    fn all_missing_is_none() {
        let (composite, _k, _p, _e) = chain(MockStore::new(), MockStore::new(), MockStore::new());
        assert_eq!(block_on(composite.get("nope")).expect("ok"), None);
    }

    #[test]
    fn all_unavailable_is_a_readable_error() {
        let (composite, _k, _p, _e) = chain(
            MockStore::always_failing("钥匙串被锁"),
            MockStore::always_failing("便携文件损坏"),
            MockStore::always_failing("环境变量只读"),
        );
        let err = block_on(composite.get("k")).expect_err("全链不可用应报错");
        let msg = err.to_string();
        assert!(msg.contains("所有密钥来源都不可用"), "实际：{msg}");
        assert!(msg.contains("钥匙串被锁") && msg.contains("便携文件损坏"), "实际：{msg}");
        assert!(matches!(err, TaError::Secret(_)));
    }

    #[test]
    fn one_miss_makes_it_none_even_if_others_error() {
        // 明确"没有"比"不可用"更有信息量：不该因为别的来源挂了就报错
        let (composite, _k, _p, _e) =
            chain(MockStore::always_failing("钥匙串被锁"), MockStore::new(), MockStore::new());
        assert_eq!(block_on(composite.get("k")).expect("ok"), None);
    }

    #[test]
    fn set_prefers_keyring() {
        let (composite, keyring, portable, env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        block_on(composite.set("k", "sk-new-0123456789")).expect("写入");
        assert_eq!(keyring.snapshot().get("k"), Some(&"sk-new-0123456789".to_string()));
        assert!(portable.snapshot().is_empty(), "钥匙串可用时不应写便携文件");
        assert!(env.snapshot().is_empty(), "环境变量来源永远不被写");
    }

    #[test]
    fn set_falls_back_to_portable_when_keyring_unavailable() {
        let (composite, _k, portable, _e) =
            chain(MockStore::always_failing("钥匙串被锁"), MockStore::new(), MockStore::new());
        block_on(composite.set("k", "sk-new-0123456789")).expect("应降级写入便携文件");
        assert_eq!(portable.snapshot().get("k"), Some(&"sk-new-0123456789".to_string()));
    }

    #[test]
    fn set_reports_clearly_when_nothing_is_writable() {
        let composite =
            CompositeStore::new(vec![NamedStore::new(SourceKind::Env, MockStore::new())]);
        let err = block_on(composite.set("k", "v")).expect_err("只读链应报错");
        assert!(err.to_string().contains("只读"), "实际：{err}");

        let composite = CompositeStore::new(vec![NamedStore::new(
            SourceKind::Keyring,
            MockStore::always_failing("钥匙串被锁"),
        )]);
        let err = block_on(composite.set("k", "v")).expect_err("应报错");
        assert!(err.to_string().contains("钥匙串被锁"), "实际：{err}");
    }

    #[test]
    fn delete_removes_from_every_source() {
        let (composite, keyring, portable, _env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        keyring.insert("k", "a-0123456789");
        portable.insert("k", "a-0123456789");
        block_on(composite.delete("k")).expect("删除");
        assert!(keyring.snapshot().is_empty());
        assert!(portable.snapshot().is_empty());
        // 幂等
        block_on(composite.delete("k")).expect("重复删除也应成功");
    }

    #[test]
    fn delete_errors_only_when_all_sources_fail() {
        let composite = CompositeStore::new(vec![NamedStore::new(
            SourceKind::Env,
            MockStore::always_failing("环境变量来源是只读的"),
        )]);
        let err = block_on(composite.delete("k")).expect_err("应报错");
        assert!(err.to_string().contains("只读"), "实际：{err}");
    }

    #[test]
    fn list_names_unions_and_dedups_ignoring_broken_sources() {
        let (composite, keyring, portable, env) =
            chain(MockStore::new(), MockStore::new(), MockStore::new());
        keyring.insert("b", "1");
        keyring.insert("a", "1");
        portable.insert("a", "1");
        portable.insert("c", "1");
        env.insert("d", "1");
        assert_eq!(block_on(composite.list_names()).expect("列"), vec!["a", "b", "c", "d"]);

        // 钥匙串挂了：其余来源照常列举
        let (composite, _k, portable, _e) =
            chain(MockStore::always_failing("钥匙串被锁"), MockStore::new(), MockStore::new());
        portable.insert("x", "1");
        assert_eq!(block_on(composite.list_names()).expect("列"), vec!["x"]);
    }

    #[test]
    fn list_names_errors_only_when_everything_is_broken() {
        let (composite, _k, _p, _e) = chain(
            MockStore::always_failing("钥匙串被锁"),
            MockStore::always_failing("便携文件损坏"),
            MockStore::new(),
        );
        // 环境变量来源是空但可用 → 它会明确回答"没有"，因此不算"全链不可用"
        assert_eq!(block_on(composite.list_names()).expect("ok"), Vec::<String>::new());

        // 三个来源全部报错 → 才返回错误
        let composite = CompositeStore::new(vec![
            NamedStore::new(SourceKind::Keyring, MockStore::always_failing("钥匙串被锁")),
            NamedStore::new(SourceKind::Portable, MockStore::always_failing("便携文件损坏")),
            NamedStore::new(SourceKind::Env, MockStore::always_failing("环境变量不可读")),
        ]);
        let err = block_on(composite.list_names()).expect_err("应报错");
        assert!(err.to_string().contains("所有密钥来源都不可用"));
    }
}

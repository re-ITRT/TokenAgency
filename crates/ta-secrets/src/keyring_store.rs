//! 系统钥匙串实现：Windows Credential Manager / macOS Keychain / Linux Secret Service。
//!
//! service 名固定为 [`SERVICE`]（`dev.tokenagency.app`），account = 条目名。
//!
//! # 无钥匙串环境
//!
//! headless Linux（没有 Secret Service / `DBUS_SESSION_BUS_ADDRESS`）、钥匙串被锁、
//! 或平台后端缺失时，`keyring` 会返回 `PlatformFailure` / `NoStorageAccess`。
//! 本实现把它翻译成 [`TaError::Secret`] 并附带**可读原因**，**绝不 panic**；
//! 上层 [`crate::composite::CompositeStore`] 会据此降级到便携文件 / 环境变量。
//!
//! # 关于 `list_names`
//!
//! `keyring` 3 没有枚举 API，因此我们在同一个 service 下维护一个**索引条目**
//! （account = [`INDEX_ACCOUNT`]），只存条目名（元数据，不含任何密钥值）。
//! 索引是 best-effort：索引写失败不会让 `set` 失败，只记一条 warn。
//!
//! # 线程模型
//!
//! 钥匙串是同步 API，且都是本地 IPC（微秒~毫秒级）。本 crate 没有 tokio 依赖，
//! 因此直接在 async fn 里同步调用（不做 `spawn_blocking`）；契约要求 async 签名，
//! 这点已在此显式记录。

use std::fmt;

use async_trait::async_trait;

use ta_core::error::{Result, TaError};

use crate::resolver::validate_entry_name;
use crate::SecretStore;

/// 钥匙串 service 名（固定）。
pub const SERVICE: &str = "dev.tokenagency.app";

/// 条目名索引所在的 account（保留名，不出现在 `list_names` 里）。
pub const INDEX_ACCOUNT: &str = "__tokenagency_index__";

/// 保留前缀：任何以此开头的条目名都不对外暴露。
const RESERVED_PREFIX: &str = "__";

/// 钥匙串条目工厂。抽出来是为了**测试时换成内存实现，绝不碰真钥匙串**。
pub trait EntryFactory: Send + Sync {
    fn entry(&self, service: &str, account: &str) -> keyring::Result<keyring::Entry>;
}

/// 默认工厂：真实平台后端。
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultEntryFactory;

impl EntryFactory for DefaultEntryFactory {
    fn entry(&self, service: &str, account: &str) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(service, account)
    }
}

/// 系统钥匙串密钥仓库。
pub struct KeyringStore {
    service: String,
    factory: Box<dyn EntryFactory>,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KeyringStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringStore").field("service", &self.service).finish()
    }
}

impl KeyringStore {
    /// 用固定 service 名 [`SERVICE`] 建一个真实钥匙串仓库。
    pub fn new() -> Self {
        Self::with_service(SERVICE)
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        Self { service: service.into(), factory: Box::new(DefaultEntryFactory) }
    }

    /// 注入自定义条目工厂（测试用）。
    pub fn with_factory<F: EntryFactory + 'static>(service: impl Into<String>, factory: F) -> Self {
        Self { service: service.into(), factory: Box::new(factory) }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    /// 探测钥匙串是否可用：读一个必然不存在的保留条目，
    /// `NoEntry` = 后端正常，其它错误 = 后端不可用（原因可读，可直接展示给用户）。
    pub fn probe(&self) -> Result<()> {
        match self.factory.entry(&self.service, INDEX_ACCOUNT) {
            Ok(entry) => match entry.get_password() {
                Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(map_keyring_err(INDEX_ACCOUNT, "探测", e)),
            },
            Err(e) => Err(map_keyring_err(INDEX_ACCOUNT, "探测", e)),
        }
    }

    /// 后端是否可用（不 panic，任何异常都算不可用）。
    pub fn is_available(&self) -> bool {
        self.probe().is_ok()
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        validate_entry_name(name)
            .map_err(|why| TaError::Secret(format!("密钥条目名不合法：{why}")))?;
        self.factory.entry(&self.service, name).map_err(|e| map_keyring_err(name, "打开", e))
    }

    /// 读取条目名索引。**只含条目名**，不含任何密钥值。
    fn index_load(&self) -> Result<Vec<String>> {
        let entry = self
            .factory
            .entry(&self.service, INDEX_ACCOUNT)
            .map_err(|e| map_keyring_err(INDEX_ACCOUNT, "读取索引", e))?;
        match entry.get_password() {
            Ok(json) => match serde_json::from_str::<Vec<String>>(&json) {
                Ok(mut names) => {
                    names.retain(|n| !n.starts_with(RESERVED_PREFIX));
                    names.sort();
                    names.dedup();
                    Ok(names)
                }
                Err(e) => {
                    // 索引坏了不该让整个钥匙串不可用：退回"列不出来"，但密钥仍能取。
                    tracing::warn!(error = %e, "钥匙串条目索引损坏，暂时无法枚举条目名");
                    Ok(Vec::new())
                }
            },
            Err(keyring::Error::NoEntry) => Ok(Vec::new()),
            Err(e) => Err(map_keyring_err(INDEX_ACCOUNT, "读取索引", e)),
        }
    }

    fn index_store(&self, names: &[String]) -> Result<()> {
        let entry = self
            .factory
            .entry(&self.service, INDEX_ACCOUNT)
            .map_err(|e| map_keyring_err(INDEX_ACCOUNT, "写入索引", e))?;
        let json = serde_json::to_string(names)
            .map_err(|e| TaError::Secret(format!("钥匙串条目索引序列化失败：{e}")))?;
        entry.set_password(&json).map_err(|e| map_keyring_err(INDEX_ACCOUNT, "写入索引", e))
    }

    fn index_add(&self, name: &str) -> Result<()> {
        let mut names = self.index_load()?;
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            names.sort();
            names.dedup();
            self.index_store(&names)?;
        }
        Ok(())
    }

    fn index_remove(&self, name: &str) -> Result<()> {
        let mut names = self.index_load()?;
        let before = names.len();
        names.retain(|n| n != name);
        if names.len() != before {
            self.index_store(&names)?;
        }
        Ok(())
    }
}

#[async_trait]
impl SecretStore for KeyringStore {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        let entry = self.entry(name)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => {
                let reason = map_keyring_err(name, "读取", e);
                tracing::debug!(entry = %name, "系统钥匙串读取失败，交由上层降级");
                Err(reason)
            }
        }
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        let entry = self.entry(name)?;
        // 注意：value 绝不进日志 / trace / 错误消息。
        entry.set_password(value).map_err(|e| map_keyring_err(name, "写入", e))?;
        // 索引 best-effort：密钥已经存进去了，索引失败只影响 list_names。
        if let Err(e) = self.index_add(name) {
            tracing::warn!(entry = %name, reason = %e, "钥匙串条目索引更新失败（list_names 可能不完整）");
        }
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let entry = self.entry(name)?;
        match entry.delete_credential() {
            // 幂等：本来就没有也算删成功
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(map_keyring_err(name, "删除", e)),
        }
        if let Err(e) = self.index_remove(name) {
            tracing::warn!(entry = %name, reason = %e, "钥匙串条目索引更新失败（list_names 可能不完整）");
        }
        Ok(())
    }

    async fn list_names(&self) -> Result<Vec<String>> {
        self.index_load()
    }
}

/// 把 `keyring::Error` 翻成带**可读原因**的 [`TaError::Secret`]（不 panic，不吞异常）。
fn map_keyring_err(name: &str, op: &str, err: keyring::Error) -> TaError {
    let reason = match &err {
        keyring::Error::PlatformFailure(e) => {
            format!("系统钥匙串后端调用失败（{e}）")
        }
        keyring::Error::NoStorageAccess(e) => {
            format!("无法访问系统钥匙串（{e}）；headless / 钥匙串被锁的环境请改用便携模式")
        }
        keyring::Error::NoEntry => "条目不存在".to_string(),
        keyring::Error::TooLong(attr, limit) => {
            format!("条目属性 {attr} 超过平台长度上限 {limit}")
        }
        keyring::Error::Invalid(attr, why) => format!("条目属性 {attr} 非法：{why}"),
        keyring::Error::BadEncoding(_) => "条目内容不是合法 UTF-8".to_string(),
        keyring::Error::Ambiguous(_) => "条目在钥匙串里匹配到多条记录".to_string(),
        other => other.to_string(),
    };
    TaError::Secret(format!("密钥条目 {name} {op}失败：{reason}"))
}

// ================================================================ 测试
//
// 全部走内存工厂（`MemFactory`）：**不碰真实系统钥匙串**。

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use keyring::credential::CredentialApi;

    use super::*;
    use crate::mock::block_on;

    type Cell = Arc<Mutex<Option<String>>>;

    /// 内存钥匙串后端（替代系统钥匙串）。
    #[derive(Debug, Default)]
    struct MemBackend {
        cells: Mutex<BTreeMap<String, Cell>>,
        seen_services: Mutex<Vec<String>>,
        failure: Mutex<Option<String>>,
    }

    impl MemBackend {
        fn cell(&self, account: &str) -> Cell {
            let mut cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
            cells.entry(account.to_string()).or_default().clone()
        }

        fn fail_with(&self, reason: &str) {
            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.to_string());
        }

        fn heal(&self) {
            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }

        fn services(&self) -> Vec<String> {
            self.seen_services.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[derive(Debug)]
    struct MemCredential {
        cell: Cell,
    }

    impl CredentialApi for MemCredential {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            let value = String::from_utf8(secret.to_vec())
                .map_err(|e| keyring::Error::BadEncoding(e.into_bytes()))?;
            *self.cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
            Ok(())
        }

        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            match self.cell.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                Some(v) => Ok(v.as_bytes().to_vec()),
                None => Err(keyring::Error::NoEntry),
            }
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            let mut guard = self.cell.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_some() {
                *guard = None;
                Ok(())
            } else {
                Err(keyring::Error::NoEntry)
            }
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct MemFactory {
        backend: Arc<MemBackend>,
    }

    impl EntryFactory for MemFactory {
        fn entry(&self, service: &str, account: &str) -> keyring::Result<keyring::Entry> {
            self.backend
                .seen_services
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(service.to_string());
            if let Some(reason) =
                self.backend.failure.lock().unwrap_or_else(|e| e.into_inner()).clone()
            {
                return Err(keyring::Error::PlatformFailure(reason.into()));
            }
            let cell = self.backend.cell(account);
            Ok(keyring::Entry::new_with_credential(Box::new(MemCredential { cell })))
        }
    }

    fn store() -> (KeyringStore, Arc<MemBackend>) {
        let backend = Arc::new(MemBackend::default());
        let store = KeyringStore::with_factory(SERVICE, MemFactory { backend: backend.clone() });
        (store, backend)
    }

    #[test]
    fn uses_fixed_service_name() {
        let (store, backend) = store();
        assert_eq!(store.service(), SERVICE);
        assert_eq!(SERVICE, "dev.tokenagency.app");
        block_on(store.set("my-key", "sk-value-0123456789")).expect("写入成功");
        assert!(
            backend.services().iter().all(|s| s == SERVICE),
            "必须固定用 {SERVICE}，实际：{:?}",
            backend.services()
        );
    }

    #[test]
    fn round_trip_set_get_list_delete() {
        let (store, _backend) = store();
        assert_eq!(block_on(store.get("missing")).expect("ok"), None);

        block_on(store.set("alpha", "sk-alpha-0123456789")).expect("写入");
        block_on(store.set("beta", "sk-beta-0123456789")).expect("写入");
        assert_eq!(
            block_on(store.get("alpha")).expect("读"),
            Some("sk-alpha-0123456789".to_string())
        );

        let names = block_on(store.list_names()).expect("列条目");
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(
            !names.iter().any(|n| n.starts_with(RESERVED_PREFIX)),
            "保留条目不得出现在 list_names 里：{names:?}"
        );

        // 覆盖写
        block_on(store.set("alpha", "sk-alpha-updated-01")).expect("覆盖");
        assert_eq!(
            block_on(store.get("alpha")).expect("读"),
            Some("sk-alpha-updated-01".to_string())
        );
        assert_eq!(block_on(store.list_names()).expect("列").len(), 2, "覆盖不应产生重复名");

        // 删除是幂等的
        block_on(store.delete("alpha")).expect("删除");
        block_on(store.delete("alpha")).expect("重复删除也应成功");
        assert_eq!(block_on(store.get("alpha")).expect("读"), None);
        assert_eq!(block_on(store.list_names()).expect("列"), vec!["beta".to_string()]);
    }

    #[test]
    fn rejects_invalid_entry_names_without_touching_backend() {
        let (store, backend) = store();
        let err = block_on(store.get("bad name")).expect_err("应报错");
        assert!(matches!(err, TaError::Secret(_)));
        assert!(err.to_string().contains("条目名不合法"));
        assert!(backend.services().is_empty(), "非法条目名不应触达后端");
    }

    #[test]
    fn degrades_gracefully_when_keychain_is_unavailable() {
        let (store, backend) = store();
        backend.fail_with("Cannot autolaunch D-Bus without X11 $DISPLAY");

        // 1) 不 panic，返回可读原因
        let err = block_on(store.get("any")).expect_err("不可用时应报错");
        match &err {
            TaError::Secret(msg) => {
                assert!(msg.contains("钥匙串"), "原因应可读：{msg}");
                assert!(msg.contains("D-Bus"), "应保留后端原因：{msg}");
            }
            other => panic!("应为 TaError::Secret，实际 {other:?}"),
        }
        assert_eq!(err.code(), "TA-9001");

        // 2) 写入 / 删除 / 列举同样只报错，不 panic
        assert!(block_on(store.set("any", "sk-x-0123456789")).is_err());
        assert!(block_on(store.delete("any")).is_err());
        assert!(block_on(store.list_names()).is_err());

        // 3) 探测接口给出同样的结论
        assert!(!store.is_available());
        assert!(store.probe().is_err());

        // 4) 后端恢复后一切正常（降级不是单向的）
        backend.heal();
        assert!(store.is_available());
        block_on(store.set("any", "sk-y-0123456789")).expect("恢复后写入");
        assert_eq!(
            block_on(store.get("any")).expect("恢复后读取"),
            Some("sk-y-0123456789".to_string())
        );
    }

    #[test]
    fn headless_error_is_readable_for_secret_service_failure() {
        let (store, backend) = store();
        backend.fail_with("org.freedesktop.Secret.Service not provided");
        let err = block_on(store.get("k")).expect_err("应报错");
        let msg = err.to_string();
        assert!(msg.contains("密钥条目 k"), "应点明是哪个条目：{msg}");
        assert!(msg.contains("失败"), "实际：{msg}");
        assert!(msg.contains("Secret.Service"), "应保留后端原因：{msg}");
        assert!(msg.contains("钥匙串"), "应说明是钥匙串的问题：{msg}");
    }

    #[test]
    fn index_corruption_does_not_break_reads() {
        let (store, backend) = store();
        block_on(store.set("alpha", "sk-alpha-0123456789")).expect("写入");
        // 手动把索引条目写坏
        *backend.cell(INDEX_ACCOUNT).lock().unwrap() = Some("not json".to_string());
        assert_eq!(block_on(store.list_names()).expect("列"), Vec::<String>::new());
        assert_eq!(
            block_on(store.get("alpha")).expect("读"),
            Some("sk-alpha-0123456789".to_string()),
            "索引坏了也不能影响取值"
        );
        // 再写一次会重建索引
        block_on(store.set("beta", "sk-beta-0123456789")).expect("写入");
        assert_eq!(block_on(store.list_names()).expect("列"), vec!["beta".to_string()]);
    }

    #[test]
    fn error_messages_never_contain_the_value() {
        let (store, backend) = store();
        backend.fail_with("boom");
        let value = "sk-super-secret-0123";
        let err = block_on(store.set("k", value)).expect_err("应报错");
        let msg = err.to_string();
        assert!(!msg.contains("super-secret"), "错误消息泄漏了密钥值：{msg}");
        assert!(matches!(err, TaError::Secret(_)));
    }

    #[test]
    fn real_default_factory_is_constructible_without_touching_keychain() {
        // 只构造，不调用：真实钥匙串留给手工/集成验证，测试里绝不写系统钥匙串。
        let store = KeyringStore::new();
        assert_eq!(store.service(), SERVICE);
        let _ = format!("{store:?}");
    }
}

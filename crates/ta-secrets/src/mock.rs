//! 测试替身：内存 [`SecretStore`] + 极简 `block_on`。
//!
//! 用途：让 `resolver` / `composite` / `migrate` 的逻辑可以**完全离线**验证，
//! 绝不触碰真实系统钥匙串（CI 上也不碰）。`keyring_store` 自己的测试另有一个
//! 内存 `EntryFactory`（见该模块 `MemFactory`），同样不碰系统。
//!
//! 本 crate 没有 `tokio` 依赖（Cargo.toml 冻结），所有实现都是"不 await 任何 IO"
//! 的即时 future，因此 [`block_on`] 用一个自旋 + no-op waker 就够——它**不是**通用
//! 执行器，只服务于本 crate 的测试与内部调用。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use ta_core::error::{Result, TaError};

use crate::SecretStore;

/// 由 [`MockStore`] 主动注入的故障模式。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Failure {
    #[default]
    None,
    /// 之后**每一次**操作都失败（模拟"钥匙串不可用"）。
    Always(String),
    /// 只有**下一次**操作失败（模拟偶发失败）。
    Once(String),
}

#[derive(Debug, Default)]
struct Inner {
    entries: BTreeMap<String, String>,
    failure: Failure,
}

/// 内存密钥仓库。`Clone` 共享同一份数据（内部是 `Arc`）。
#[derive(Debug, Clone, Default)]
pub struct MockStore {
    inner: Arc<Mutex<Inner>>,
}

impl MockStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从一个 `(名字, 值)` 列表建仓库。
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let store = Self::new();
        for (k, v) in pairs {
            store.insert(k, v);
        }
        store
    }

    /// 每次操作都失败（模拟 headless Linux 没有 Secret Service / 钥匙串被锁）。
    pub fn always_failing(reason: impl Into<String>) -> Self {
        let store = Self::new();
        store.fail_always(reason);
        store
    }

    /// 只有下一次操作失败。
    pub fn failing_once(reason: impl Into<String>) -> Self {
        let store = Self::new();
        store.fail_once(reason);
        store
    }

    pub fn fail_always(&self, reason: impl Into<String>) {
        self.lock().failure = Failure::Always(reason.into());
    }

    pub fn fail_once(&self, reason: impl Into<String>) {
        self.lock().failure = Failure::Once(reason.into());
    }

    pub fn heal(&self) {
        self.lock().failure = Failure::None;
    }

    /// 预置一条数据（同步，方便测试准备）。
    pub fn insert(&self, name: impl Into<String>, value: impl Into<String>) -> &Self {
        self.lock().entries.insert(name.into(), value.into());
        self
    }

    /// 当前内容快照（断言用）。
    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.lock().entries.clone()
    }

    pub fn names(&self) -> Vec<String> {
        self.lock().entries.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn check_failure(&self) -> Result<()> {
        let mut inner = self.lock();
        match std::mem::take(&mut inner.failure) {
            Failure::None => Ok(()),
            Failure::Always(msg) => {
                inner.failure = Failure::Always(msg.clone());
                Err(TaError::Secret(msg))
            }
            Failure::Once(msg) => Err(TaError::Secret(msg)),
        }
    }
}

#[async_trait]
impl SecretStore for MockStore {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        self.check_failure()?;
        Ok(self.lock().entries.get(name).cloned())
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        self.check_failure()?;
        self.lock().entries.insert(name.to_string(), value.to_string());
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        self.check_failure()?;
        self.lock().entries.remove(name);
        Ok(())
    }

    async fn list_names(&self) -> Result<Vec<String>> {
        self.check_failure()?;
        Ok(self.lock().entries.keys().cloned().collect())
    }
}

// ================================================================ block_on

struct NoopWake;

impl std::task::Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

/// 自旋执行一个**不会真正等待 IO** 的 future。
///
/// 本 crate 的 store 实现（keyring / 本地文件 / 内存）都在同一个 poll 里做完，
/// 所以不会饿死；仅用于测试与内部便捷调用，不要拿它跑网络 future。
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let waker = std::task::Waker::from(Arc::new(NoopWake));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => return v,
            std::task::Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::block_on;

    #[test]
    fn behaves_like_a_store() {
        let store = MockStore::from_pairs([("a", "1"), ("b", "2")]);
        assert_eq!(block_on(store.get("a")).expect("ok"), Some("1".to_string()));
        block_on(store.set("c", "3")).expect("写");
        assert_eq!(store.names(), vec!["a", "b", "c"]);
        block_on(store.delete("a")).expect("删");
        assert_eq!(block_on(store.list_names()).expect("列"), vec!["b", "c"]);
        assert_eq!(block_on(store.get("zzz")).expect("ok"), None);
    }

    #[test]
    fn clones_share_state() {
        let store = MockStore::new();
        let clone = store.clone();
        clone.insert("k", "v");
        assert_eq!(block_on(store.get("k")).expect("ok"), Some("v".to_string()));
    }

    #[test]
    fn failure_modes_work() {
        let always = MockStore::always_failing("boom");
        assert!(block_on(always.get("a")).is_err());
        assert!(block_on(always.get("a")).is_err(), "Always 模式应持续失败");
        assert!(block_on(always.list_names()).is_err());
        assert!(block_on(always.set("a", "1")).is_err());
        assert!(block_on(always.delete("a")).is_err());

        let once = MockStore::failing_once("blip");
        assert!(block_on(once.get("a")).is_err());
        assert_eq!(block_on(once.get("a")).expect("第二次应恢复"), None);
    }

    #[test]
    fn block_on_returns_immediately_for_ready_futures() {
        assert_eq!(block_on(async { 1 + 1 }), 2);
    }
}

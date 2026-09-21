//! 额度账本的**持久化后端**（只负责存取，不含任何状态机逻辑）。
//!
//! P3 提供内存实现（测试与首次启动就能用）；P4（`ta-store`）用 SQLite 实现同一个
//! trait，于是"四源合成 + 状态机 + 窗口滚动"这套纯逻辑**只有一份**，
//! 不会在内存版与库版之间跑偏。
//!
//! 契约：[11 §1.2](../../../docs/11-internal-contracts.md) 的 `QuotaLedger`；
//! 观测源定义见 [03 §4](../../../docs/03-routing-and-policy.md)。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::domain::QuotaConfig;
use crate::quota::QuotaState;
use crate::types::CredentialId;

/// 账本的持久化视图。
///
/// `QuotaState` 是对外契约（字段冻结），所以"凭据无效"和"硬性耗尽"这两个
/// **状态机内部标记**放在这里，由 [`crate::quota::ledger::QuotaLedgerImpl`] 维护。
#[derive(Debug, Clone)]
pub struct QuotaRecord {
    pub state: QuotaState,
    /// D 源/手动标记：上游明确说"额度没了"。只有窗口滚动、手动清零、
    /// 或响应头显示 remaining 回升才会解除。
    pub hard_exhausted: bool,
    /// 401/403 "key 失效"语义。**窗口滚动不会解除**（换周期不换 key）。
    pub invalid: bool,
}

impl QuotaRecord {
    pub fn new(cred: &CredentialId) -> Self {
        Self {
            state: QuotaState { credential_id: cred.clone(), ..Default::default() },
            hard_exhausted: false,
            invalid: false,
        }
    }

    /// 是否从未被任何观测源碰过（`unknown` 语义）。
    pub fn never_observed(&self) -> bool {
        self.state.updated_at_ms == 0
            && !self.hard_exhausted
            && !self.invalid
            && self.state.reported_remaining.is_none()
            && self.state.used_tokens == 0
            && self.state.used_requests == 0
            && self.state.used_cost == 0.0
    }
}

/// 额度账本的存取后端。
///
/// 所有方法都是同步的（`ta-core` 不做 IO）；P4 的 SQLite 实现内部用
/// `rusqlite`/`sqlx` 的同步 API 或阻塞池即可。
pub trait QuotaBackend: Send + Sync {
    fn get(&self, cred: &CredentialId) -> Option<QuotaRecord>;
    fn put(&self, rec: &QuotaRecord);
    /// 全量快照（窗口滚动要遍历所有凭据）。
    fn all(&self) -> Vec<QuotaRecord>;
    fn remove(&self, cred: &CredentialId);

    /// C 源：用户填的手工额度（存在 `Credential.quota` 里）。
    /// 默认 `None` —— 纯凭响应头/本地计量也能工作。
    fn quota_config(&self, _cred: &CredentialId) -> Option<QuotaConfig> {
        None
    }
    fn set_quota_config(&self, _cred: &CredentialId, _cfg: QuotaConfig) {}
}

// ---------------------------------------------------------------- 内存实现

/// 内存后端。`ta-gateway` 起服务时可直接用；也是单测的默认后端。
#[derive(Default)]
pub struct MemoryQuotaBackend {
    records: Mutex<HashMap<CredentialId, QuotaRecord>>,
    configs: Mutex<HashMap<CredentialId, QuotaConfig>>,
}

impl MemoryQuotaBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置一批手工额度配置（启动时从 `ConfigDoc.credentials[].quota` 灌进来）。
    pub fn with_configs(cfgs: impl IntoIterator<Item = (CredentialId, QuotaConfig)>) -> Self {
        let me = Self::default();
        for (cred, cfg) in cfgs {
            me.set_quota_config(&cred, cfg);
        }
        me
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // 账本是"绝不 panic"的关键路径：即使有线程 panic 也别把账本锁死
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl QuotaBackend for MemoryQuotaBackend {
    fn get(&self, cred: &CredentialId) -> Option<QuotaRecord> {
        lock(&self.records).get(cred).cloned()
    }

    fn put(&self, rec: &QuotaRecord) {
        lock(&self.records).insert(rec.state.credential_id.clone(), rec.clone());
    }

    fn all(&self) -> Vec<QuotaRecord> {
        lock(&self.records).values().cloned().collect()
    }

    fn remove(&self, cred: &CredentialId) {
        lock(&self.records).remove(cred);
    }

    fn quota_config(&self, cred: &CredentialId) -> Option<QuotaConfig> {
        lock(&self.configs).get(cred).cloned()
    }

    fn set_quota_config(&self, cred: &CredentialId, cfg: QuotaConfig) {
        lock(&self.configs).insert(cred.clone(), cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn memory_backend_roundtrip() {
        let b = MemoryQuotaBackend::new();
        let cred = CredentialId::new("c-1");
        assert!(b.get(&cred).is_none());
        let mut rec = QuotaRecord::new(&cred);
        assert!(rec.never_observed());
        rec.state.used_tokens = 10;
        b.put(&rec);
        assert_eq!(b.get(&cred).unwrap().state.used_tokens, 10);
        assert_eq!(b.all().len(), 1);
        b.set_quota_config(&cred, QuotaConfig::default());
        assert!(b.quota_config(&cred).is_some());
        b.remove(&cred);
        assert!(b.all().is_empty());
    }
}

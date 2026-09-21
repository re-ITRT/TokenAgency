//! 粘性记录的**存储后端**抽象。
//!
//! `StickyResolver` 负责"key 怎么派生 / 什么时候释放"这类**策略**，
//! `StickyBackend` 只负责"记录放哪儿"。P4 用 SQLite 实现它（重启后粘性不丢），
//! 测试与模拟器用内存实现。

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::sticky::StickyRecord;
use crate::types::{ChannelGroupId, StickyKey};

/// 粘性记录的持久化后端。**实现必须是 `Send + Sync`**（服务在多线程里跑）。
pub trait StickyBackend: Send + Sync {
    /// 取一条记录（**包含已过期的**：由 `StickyResolver` 决定怎么处理）。
    fn get(&self, group: &ChannelGroupId, key: &StickyKey) -> Option<StickyRecord>;
    /// 写入 / 覆盖。
    fn put(&self, rec: &StickyRecord);
    /// 删除单条（不存在则无副作用）。
    fn delete(&self, group: &ChannelGroupId, key: &StickyKey);
    /// 删除一个组的全部记录，返回删除条数（用户手动 pin 变更时用）。
    fn delete_group(&self, group: &ChannelGroupId) -> u64;
    /// 列出记录；`group = None` 表示全部。顺序必须稳定（便于测试与快照）。
    fn list(&self, group: Option<&ChannelGroupId>) -> Vec<StickyRecord>;
    /// 清理过期记录，返回清理条数。
    fn gc(&self, now_ms: i64) -> u64;

    fn len(&self) -> usize {
        self.list(None).len()
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 进程内实现（默认）。不落盘，重启即失 —— 但 P4 接上 SQLite 后同一套策略逻辑照用。
#[derive(Debug, Default)]
pub struct MemoryStickyStore {
    inner: Mutex<BTreeMap<(ChannelGroupId, StickyKey), StickyRecord>>,
}

impl MemoryStickyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StickyBackend for MemoryStickyStore {
    fn get(&self, group: &ChannelGroupId, key: &StickyKey) -> Option<StickyRecord> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(group.clone(), key.clone()))
            .cloned()
    }

    fn put(&self, rec: &StickyRecord) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((rec.group_id.clone(), rec.key.clone()), rec.clone());
    }

    fn delete(&self, group: &ChannelGroupId, key: &StickyKey) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(&(group.clone(), key.clone()));
    }

    fn delete_group(&self, group: &ChannelGroupId) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.len();
        g.retain(|(grp, _), _| grp != group);
        (before - g.len()) as u64
    }

    fn list(&self, group: Option<&ChannelGroupId>) -> Vec<StickyRecord> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.iter()
            .filter(|((grp, _), _)| group.map(|want| grp == want).unwrap_or(true))
            .map(|(_, rec)| rec.clone())
            .collect()
    }

    fn gc(&self, now_ms: i64) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.len();
        g.retain(|_, rec| rec.is_alive(now_ms));
        (before - g.len()) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChannelId;

    fn rec(group: &str, key: &str, ch: &str, expires: i64) -> StickyRecord {
        StickyRecord {
            key: StickyKey::from(key),
            group_id: ChannelGroupId::from(group),
            channel_id: ChannelId::from(ch),
            created_at_ms: 0,
            expires_at_ms: expires,
            hits: 0,
        }
    }

    #[test]
    fn memory_backend_crud_and_gc() {
        let b = MemoryStickyStore::new();
        assert!(b.is_empty());

        b.put(&rec("g1", "k1", "ch-a", 100));
        b.put(&rec("g1", "k2", "ch-b", 100));
        b.put(&rec("g2", "k1", "ch-c", 50));

        assert_eq!(
            b.get(&ChannelGroupId::from("g1"), &StickyKey::from("k1")).unwrap().channel_id.as_str(),
            "ch-a"
        );
        assert_eq!(b.len(), 3);
        assert_eq!(b.list(Some(&ChannelGroupId::from("g1"))).len(), 2);
        assert_eq!(b.list(None).len(), 3);

        // 覆盖
        b.put(&rec("g1", "k1", "ch-z", 100));
        assert_eq!(
            b.get(&ChannelGroupId::from("g1"), &StickyKey::from("k1")).unwrap().channel_id.as_str(),
            "ch-z"
        );

        // gc 只清过期
        assert_eq!(b.gc(60), 1);
        assert_eq!(b.len(), 2);

        assert_eq!(b.delete_group(&ChannelGroupId::from("g1")), 2);
        assert_eq!(b.delete_group(&ChannelGroupId::from("g1")), 0);
        assert_eq!(b.len(), 0);

        b.put(&rec("g1", "k1", "ch-a", 100));
        b.delete(&ChannelGroupId::from("g1"), &StickyKey::from("k1"));
        assert!(b.is_empty());
    }
}

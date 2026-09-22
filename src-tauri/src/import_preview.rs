//! `model_import_from_provider` 预览的**进程内存**缓存（[docs/19 §1.2]）。
//!
//! # 为什么是内存，而不是一张表
//!
//! 预览是"一次扫描的中间结果"：它描述的是**上游现在有什么**，而不是用户配置。
//! 落库会造出**第二个真相源** —— 库里存着一份"上游有这些模型"的快照，
//! 与真实的上游注定漂移（用户改了中转的模型清单，这份快照不会知道），
//! 而漂移时不报错。所以这里只留进程内存 + 5 分钟 TTL：
//! 过期就没了，用户重新扫一次的成本是一次 `GET /models`。
//!
//! # 与配置写路径的关系
//!
//! `model_apply_import` 从本缓存取回预览后，写库仍然走**唯一**的
//! [`crate::commands::catalog::save_validated`]：本模块只负责"记住扫描结果"，
//! 不碰配置、不碰库。
//!
//! # token 是什么
//!
//! 它是**缓存句柄**，不是鉴权凭据（IPC 只在本机回环上，且预览内容本来就是这台机器刚扫出来的）。
//! 但它仍然必须**不可猜**：`RandomState` 的种子来自操作系统，所以同一进程里不同次扫描
//! 拿到的 token 不会重复，外部也无法由"另一个 token"推出下一个。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 预览的有效期（5 分钟，[docs/19 §1.2]）。
pub const PREVIEW_TTL_MS: i64 = 5 * 60 * 1000;

/// 同时最多保留多少份预览。
///
/// 存在的理由：用户可能反复点"一键扫描"。没有上限的话，一个下午就能把整份上游清单
/// （每份最多 256 KiB 的响应体解析结果）留在内存里。超限时**先丢过期、再丢最早过期的**。
pub const MAX_PREVIEWS: usize = 32;

/// 预览里的一行：上游模型名 + 建议的逻辑 id + **扫描那一刻**是否已存在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewModel {
    pub upstream_model: String,
    pub suggested_id: String,
    pub exists: bool,
}

/// 派生不出合法 id 的上游模型（`reason` 是人话，直接给用户看）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSkip {
    pub upstream_model: String,
    pub reason: String,
}

/// 一份预览的内容（不含 token）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEntry {
    pub provider_id: String,
    pub models: Vec<PreviewModel>,
    pub skipped: Vec<PreviewSkip>,
}

struct Slot {
    token: String,
    expires_at_ms: i64,
    payload: PreviewEntry,
}

/// 进程内预览缓存。挂在 [`crate::state::AppState`] 上（**不是**每个命令各造一份）。
pub struct ImportPreviewCache {
    seq: AtomicU64,
    slots: Mutex<Vec<Slot>>,
}

impl Default for ImportPreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportPreviewCache {
    pub fn new() -> Self {
        Self { seq: AtomicU64::new(0), slots: Mutex::new(Vec::new()) }
    }

    /// 存一份预览，返回它的 token。
    pub fn put(
        &self,
        provider_id: &str,
        models: Vec<PreviewModel>,
        skipped: Vec<PreviewSkip>,
        now_ms: i64,
    ) -> String {
        let token = self.next_token(provider_id, now_ms);
        let mut slots = self.lock();
        // ① 先清掉过期的（它们再也不会被取到）；② 还超限就丢最早过期的那些
        slots.retain(|s| s.expires_at_ms > now_ms);
        while slots.len() >= MAX_PREVIEWS {
            let Some((oldest, _)) = slots.iter().enumerate().min_by_key(|(_, s)| s.expires_at_ms)
            else {
                break;
            };
            slots.remove(oldest);
        }
        slots.push(Slot {
            token: token.clone(),
            expires_at_ms: now_ms + PREVIEW_TTL_MS,
            payload: PreviewEntry { provider_id: provider_id.to_string(), models, skipped },
        });
        token
    }

    /// 按 token 取回预览。**取用不消耗**（用户可以分几次把扫描结果加进来，
    /// 只要还在 5 分钟内）；`None` = token 未知或已过期 —— 调用方必须让用户重新扫描。
    pub fn get(&self, token: &str, now_ms: i64) -> Option<PreviewEntry> {
        let mut slots = self.lock();
        slots.retain(|s| s.expires_at_ms > now_ms);
        slots.iter().find(|s| s.token == token).map(|s| s.payload.clone())
    }

    /// 当前保留下来的预览份数（测试用：证明"过期会被清掉 / 上限真的生效"）。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        // 锁中毒不该让整个命令面瘫掉：取回内部值继续用（本模块的状态是纯缓存）
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_token(&self, provider_id: &str, now_ms: i64) -> String {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut hasher = RandomState::new().build_hasher();
        hasher.write(provider_id.as_bytes());
        hasher.write_u64(seq);
        hasher.write_i64(now_ms);
        format!("pv_{:016x}{:08x}", hasher.finish(), seq as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(ids: &[(&str, &str, bool)]) -> Vec<PreviewModel> {
        ids.iter()
            .map(|(upstream, suggested, exists)| PreviewModel {
                upstream_model: (*upstream).to_string(),
                suggested_id: (*suggested).to_string(),
                exists: *exists,
            })
            .collect()
    }

    const T0: i64 = 1_700_000_000_000;

    /// 取回来的必须**就是**存进去的那份（不是"函数返回了个 token"就算过）。
    #[test]
    fn put_then_get_returns_the_same_preview() {
        let cache = ImportPreviewCache::new();
        let token = cache.put(
            "p1",
            models(&[("gpt-4o", "gpt-4o", false)]),
            vec![PreviewSkip { upstream_model: "!!!".into(), reason: "空 id".into() }],
            T0,
        );

        let got = cache.get(&token, T0).expect("刚存进去的预览必须取得到");
        assert_eq!(got.provider_id, "p1");
        assert_eq!(got.models, models(&[("gpt-4o", "gpt-4o", false)]));
        assert_eq!(got.skipped.len(), 1);
        assert_eq!(got.skipped[0].upstream_model, "!!!");
    }

    /// 两次扫描给两个不同的 token（否则第二次扫描会静默覆盖第一次的结果）。
    #[test]
    fn two_scans_get_two_tokens() {
        let cache = ImportPreviewCache::new();
        let first = cache.put("p1", models(&[("a", "a", false)]), vec![], T0);
        let second = cache.put("p1", models(&[("b", "b", false)]), vec![], T0);
        assert_ne!(first, second, "两次扫描的 token 不许相同");
        assert_eq!(cache.get(&first, T0).unwrap().models[0].upstream_model, "a");
        assert_eq!(cache.get(&second, T0).unwrap().models[0].upstream_model, "b");
    }

    /// TTL 的边界是**真的**边界：`TTL - 1` 毫秒还在，`TTL` 毫秒就没了。
    #[test]
    fn a_preview_expires_exactly_at_the_ttl() {
        let cache = ImportPreviewCache::new();
        let token = cache.put("p1", models(&[("a", "a", false)]), vec![], T0);

        assert!(cache.get(&token, T0 + PREVIEW_TTL_MS - 1).is_some(), "还没到期就不许失效");
        assert!(cache.get(&token, T0 + PREVIEW_TTL_MS).is_none(), "到期必须失效（不是'大概'过期）");
        assert_eq!(cache.len(), 0, "过期的条目要顺手清掉，不许在内存里越堆越多");
    }

    /// 未知 token → `None`（调用方据此报 `TA-1004`，而不是凭空写一批模型）。
    #[test]
    fn an_unknown_token_is_none() {
        let cache = ImportPreviewCache::new();
        cache.put("p1", models(&[("a", "a", false)]), vec![], T0);
        assert!(cache.get("pv_根本没有这个", T0).is_none());
        assert!(cache.get("", T0).is_none());
    }

    /// **取用不消耗**：同一个 token 可以支持"加几个、再加几个"（都在 TTL 内）。
    #[test]
    fn get_does_not_consume_the_preview() {
        let cache = ImportPreviewCache::new();
        let token = cache.put("p1", models(&[("a", "a", false)]), vec![], T0);
        assert!(cache.get(&token, T0 + 1).is_some());
        assert!(cache.get(&token, T0 + 2).is_some(), "取过一次不等于预览作废");
    }

    /// 上限真的生效：超过 `MAX_PREVIEWS` 时**丢最早过期的**，而不是无界增长。
    #[test]
    fn the_cache_stays_bounded_and_drops_the_earliest() {
        let cache = ImportPreviewCache::new();
        let mut tokens = Vec::new();
        for i in 0..(MAX_PREVIEWS + 3) {
            // 每份的到期时间都往后挪一点：最先被丢的应当是最早的那几份
            tokens.push((cache.put("p1", models(&[("a", "a", false)]), vec![], T0 + i as i64), i));
        }
        assert_eq!(cache.len(), MAX_PREVIEWS, "份数必须被上限夹住");

        let (first_token, _) = &tokens[0];
        assert!(cache.get(first_token, T0 + MAX_PREVIEWS as i64).is_none(), "最早的那份该被丢");
        let (last_token, _) = &tokens[MAX_PREVIEWS + 2];
        assert!(cache.get(last_token, T0 + MAX_PREVIEWS as i64).is_some(), "最新的那份必须在");
    }

    /// token 形态稳定（可读、可日志化，且不含任何密钥材料）。
    #[test]
    fn tokens_are_opaque_and_prefixed() {
        let cache = ImportPreviewCache::new();
        let token = cache.put("p1", vec![], vec![], T0);
        assert!(token.starts_with("pv_"), "{token}");
        assert!(token.len() > 20, "太短的 token 容易被猜：{token}");
        assert!(
            token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "token 只允许 ASCII：{token}"
        );
    }
}

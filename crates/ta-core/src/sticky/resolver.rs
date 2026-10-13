//! 会话粘性的**策略**实现：key 派生、命中、释放、复活。
//!
//! key 派生顺序严格按 `StickyConfig.key`（默认见）：
//! `header:x-session-id → header:x-conversation-id → metadata.user_id → body_hash → fallback:client_ip+ua`。
//!
//! `body_hash` 用 `derive_body_hash`（首条 system + 首条 user 前 512 字符）——
//! 这样同一会话的**多轮**请求也能稳定命中（整个 messages 的哈希每轮都会变）。
//!
//! 存储走 `StickyBackend`，因此 P4 换成 SQLite 后这套策略逻辑一行都不用改。

use std::sync::{Arc, Mutex};

use crate::clock::{Clock, SystemClock};
use crate::domain::{ChannelGroup, ReleaseReason};
use crate::routing::engine::header_lookup;
use crate::routing::RouteHints;
use crate::sticky::store::{MemoryStickyStore, StickyBackend};
use crate::sticky::{derive_body_hash, StickyRecord, StickyResolver};
use crate::types::{ChannelGroupId, ChannelId, StickyKey};

/// 粘性解析器（策略层）。
pub struct BackedStickyResolver<B: StickyBackend> {
    backend: B,
    clock: Arc<dyn Clock>,
    /// 释放审计：测试与排查"这个会话为什么换通道了"用
    releases: Mutex<Vec<(StickyKey, ReleaseReason)>>,
}

/// 默认的进程内粘性解析器。
pub type MemoryStickyResolver = BackedStickyResolver<MemoryStickyStore>;

impl<B: StickyBackend> BackedStickyResolver<B> {
    pub fn new(backend: B, clock: Arc<dyn Clock>) -> Self {
        Self { backend, clock, releases: Mutex::new(vec![]) }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// 释放审计（按发生顺序）。
    pub fn releases(&self) -> Vec<(StickyKey, ReleaseReason)> {
        self.releases.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 由 TTL 推出 `expires_at_ms`（`ttl_h` 为 0 时用兜底值）。
    pub fn ttl_ms_for(group: &ChannelGroup, fallback_ms: i64) -> i64 {
        if group.sticky.ttl_h == 0 {
            fallback_ms
        } else {
            group.sticky.ttl_h as i64 * 3_600_000
        }
    }

    fn log_release(&self, key: StickyKey, why: ReleaseReason) {
        self.releases.lock().unwrap_or_else(|e| e.into_inner()).push((key, why));
    }
}

impl MemoryStickyResolver {
    pub fn memory() -> Self {
        Self::new(MemoryStickyStore::new(), Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::new(MemoryStickyStore::new(), clock)
    }
}

impl<B: StickyBackend> StickyResolver for BackedStickyResolver<B> {
    fn key_for(&self, group: &ChannelGroup, hints: &RouteHints) -> Option<StickyKey> {
        for spec in &group.sticky.key {
            if let Some(v) = derive_key(spec, hints) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(StickyKey::new(v));
                }
            }
        }
        None
    }

    fn lookup(&self, key: &StickyKey, group: &ChannelGroupId) -> Option<StickyRecord> {
        let rec = self.backend.get(group, key)?;
        // 过期即"没有"；物理清理交给 gc（由清理任务统一跑）
        if rec.is_alive(self.clock.now_ms()) {
            Some(rec)
        } else {
            None
        }
    }

    fn store(&self, key: StickyKey, group: ChannelGroupId, channel: ChannelId, ttl_ms: i64) {
        let now = self.clock.now_ms();
        let existing = self.backend.get(&group, &key);
        let same_channel = existing.as_ref().map(|r| r.channel_id == channel).unwrap_or(false);
        let rec = StickyRecord {
            created_at_ms: if same_channel {
                existing.as_ref().map(|r| r.created_at_ms).unwrap_or(now)
            } else {
                now
            },
            expires_at_ms: now + ttl_ms.max(1),
            hits: if same_channel { existing.as_ref().map(|r| r.hits + 1).unwrap_or(1) } else { 1 },
            key,
            group_id: group,
            channel_id: channel,
        };
        self.backend.put(&rec);
    }

    fn release(&self, key: &StickyKey, why: ReleaseReason) {
        let victims: Vec<StickyRecord> =
            self.backend.list(None).into_iter().filter(|r| &r.key == key).collect();
        if victims.is_empty() {
            return;
        }
        for r in victims {
            self.backend.delete(&r.group_id, &r.key);
        }
        self.log_release(key.clone(), why);
    }

    fn release_group(&self, group: &ChannelGroupId, why: ReleaseReason) {
        let keys: Vec<StickyKey> =
            self.backend.list(Some(group)).into_iter().map(|r| r.key).collect();
        if keys.is_empty() {
            return;
        }
        self.backend.delete_group(group);
        for k in keys {
            self.log_release(k, why);
        }
    }

    fn gc(&self, now_ms: i64) -> u64 {
        self.backend.gc(now_ms)
    }
}

// ---------------------------------------------------------------- key 派生

/// 按一条 key 规格从请求里取值；取不到返回 `None`（调用方会继续下一条规格）。
pub fn derive_key(spec: &str, hints: &RouteHints) -> Option<String> {
    let spec = spec.trim();
    let lower = spec.to_ascii_lowercase();

    // header:<name>
    if let Some(rest) = lower.strip_prefix("header:") {
        let name = &spec[spec.len() - rest.len()..];
        if let Some(v) = header_lookup(hints, name) {
            if !v.trim().is_empty() {
                return Some(v.to_string());
            }
        }
        // 请求头缺失时，退回已解析好的同名字段（网关会把这些头提到 hints 里）
        return match rest {
            "x-session-id" => hints.session_id.clone(),
            "x-conversation-id" => hints.conversation_id.clone(),
            _ => None,
        }
        .filter(|v| !v.trim().is_empty());
    }

    match lower.as_str() {
        "metadata.user_id" | "body.user" | "user_id" | "user" => hints.user_id.clone(),
        "body_hash" | "body.hash" => hints.body_hash.clone(),
        "session_id" | "session" => hints.session_id.clone(),
        "conversation_id" | "conversation" => hints.conversation_id.clone(),
        "fallback:client_ip+ua" | "client_ip+ua" | "fallback" => Some(fallback_key(hints)),
        _ => None,
    }
    .filter(|v| !v.trim().is_empty())
}

/// 最后兜底：`client_ip|ua`。很粗，但比"完全没有粘性"好。
fn fallback_key(hints: &RouteHints) -> String {
    let ip = ["x-forwarded-for", "x-real-ip", "cf-connecting-ip"]
        .iter()
        .find_map(|h| header_lookup(hints, h))
        // x-forwarded-for 可能是 "client, proxy1, proxy2"
        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "noip".to_string());
    let ua = hints
        .ua
        .clone()
        .or_else(|| header_lookup(hints, "user-agent").map(|s| s.to_string()))
        .unwrap_or_else(|| "noua".to_string());
    format!("{ip}|{ua}")
}

/// `body_hash` 的唯一派生入口：从原始 body 里取"首条 system + 首条 user"。
///
/// 覆盖三种常见形状：OpenAI Chat（`messages`）、Anthropic（顶层 `system` + `messages`）、
/// OpenAI Responses（`instructions` + `input`）。**只读文本，不看图片**。
pub fn body_hash_from_body(body: &serde_json::Value) -> Option<String> {
    let system = first_role_text(body, &["system", "developer"])
        .or_else(|| body.get("system").and_then(text_of))
        .or_else(|| body.get("instructions").and_then(text_of));
    let user = first_role_text(body, &["user"])
        .or_else(|| body.get("input").and_then(text_of))
        .or_else(|| body.get("prompt").and_then(text_of));
    if system.is_none() && user.is_none() {
        return None;
    }
    Some(derive_body_hash(system.as_deref(), user.as_deref()))
}

fn first_role_text(body: &serde_json::Value, roles: &[&str]) -> Option<String> {
    let arr = body.get("messages")?.as_array()?;
    for m in arr {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if roles.iter().any(|r| r.eq_ignore_ascii_case(role)) {
            if let Some(t) = m.get("content").and_then(text_of) {
                return Some(t);
            }
        }
    }
    None
}

/// 把 `content` 里的文本抽出来：字符串 / `[{type:text,...}]` / `{content|text}`。
fn text_of(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(t) = text_of(p) {
                    out.push_str(&t);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        serde_json::Value::Object(o) => {
            // 未知 part 类型一律忽略（透传不受影响，这里只是 key 派生）
            o.get("text").and_then(text_of).or_else(|| o.get("content").and_then(text_of))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::domain::ConfigDoc;
    use crate::types::*;

    const YAML: &str = r#"
version: 1
channel_groups:
  - id: grp-1
    model: gpt-4o
    members: [ch-a]
    sticky: { enabled: true, ttl_h: 12, key: [header:x-session-id, header:x-conversation-id, metadata.user_id, body_hash] }
  - id: grp-fallback
    model: gpt-4o
    members: [ch-a]
    sticky: { enabled: true, ttl_h: 2, key: [fallback:client_ip+ua] }
  - id: grp-hash-only
    model: gpt-4o
    members: [ch-a]
    sticky: { enabled: true, ttl_h: 1, key: [body_hash] }
  - id: grp-unknown-spec
    model: gpt-4o
    members: [ch-a]
    sticky: { enabled: true, key: [not_a_real_spec, body_hash] }
  - id: grp-empty-key
    model: gpt-4o
    members: [ch-a]
    sticky: { enabled: true, key: [] }
"#;

    fn groups() -> ConfigDoc {
        config::from_yaml(YAML).expect("yaml")
    }

    fn g(doc: &ConfigDoc, id: &str) -> ChannelGroup {
        doc.channel_groups.iter().find(|g| g.id.as_str() == id).unwrap().clone()
    }

    fn hints() -> RouteHints {
        RouteHints::default()
    }

    // ------------------------------------------------------------ key 派生顺序

    #[test]
    fn key_priority_header_session_first() {
        let doc = groups();
        let r = MemoryStickyResolver::memory();
        let grp = g(&doc, "grp-1");

        // 全都给上 → 取 x-session-id
        let mut h = hints();
        h.headers.insert("x-session-id".into(), "sess-1".into());
        h.headers.insert("x-conversation-id".into(), "conv-1".into());
        h.user_id = Some("user-1".into());
        h.body_hash = Some("hash-1".into());
        assert_eq!(r.key_for(&grp, &h).unwrap().as_str(), "sess-1");

        // 头名大小写不敏感
        let mut h2 = hints();
        h2.headers.insert("X-Session-Id".into(), "sess-2".into());
        assert_eq!(r.key_for(&grp, &h2).unwrap().as_str(), "sess-2");

        // 退化到 conversation
        let mut h3 = hints();
        h3.headers.insert("x-conversation-id".into(), "conv-3".into());
        h3.user_id = Some("user-3".into());
        h3.body_hash = Some("hash-3".into());
        assert_eq!(r.key_for(&grp, &h3).unwrap().as_str(), "conv-3");

        // 退化到 user_id
        let mut h4 = hints();
        h4.user_id = Some("user-4".into());
        h4.body_hash = Some("hash-4".into());
        assert_eq!(r.key_for(&grp, &h4).unwrap().as_str(), "user-4");

        // 退化到 body_hash
        let mut h5 = hints();
        h5.body_hash = Some("hash-5".into());
        assert_eq!(r.key_for(&grp, &h5).unwrap().as_str(), "hash-5");

        // 什么都没有 → None
        assert!(r.key_for(&grp, &hints()).is_none());
    }

    #[test]
    fn key_falls_back_to_hints_fields_when_headers_absent() {
        let doc = groups();
        let r = MemoryStickyResolver::memory();
        let grp = g(&doc, "grp-1");
        let mut h = hints();
        h.session_id = Some("from-hints".into());
        assert_eq!(r.key_for(&grp, &h).unwrap().as_str(), "from-hints");

        // 空串不算命中
        let mut h2 = hints();
        h2.headers.insert("x-session-id".into(), "   ".into());
        h2.session_id = Some("real".into());
        assert_eq!(r.key_for(&grp, &h2).unwrap().as_str(), "real");
    }

    #[test]
    fn key_fallback_client_ip_plus_ua() {
        let doc = groups();
        let r = MemoryStickyResolver::memory();
        let grp = g(&doc, "grp-fallback");
        let mut h = hints();
        h.headers.insert("x-forwarded-for".into(), "1.2.3.4, 10.0.0.1".into());
        h.ua = Some("Cursor/0.42".into());
        assert_eq!(r.key_for(&grp, &h).unwrap().as_str(), "1.2.3.4|Cursor/0.42");

        let mut h2 = hints();
        h2.headers.insert("user-agent".into(), "curl/8".into());
        assert_eq!(r.key_for(&grp, &h2).unwrap().as_str(), "noip|curl/8");
    }

    #[test]
    fn key_skips_unknown_specs_and_empty_lists() {
        let doc = groups();
        let r = MemoryStickyResolver::memory();

        let mut h = hints();
        h.body_hash = Some("h".into());
        assert_eq!(r.key_for(&g(&doc, "grp-unknown-spec"), &h).unwrap().as_str(), "h");
        assert!(r.key_for(&g(&doc, "grp-empty-key"), &h).is_none());
    }

    #[test]
    fn body_hash_covers_multiple_protocol_shapes() {
        let openai = serde_json::json!({
            "messages": [
                {"role": "system", "content": "you are helpful"},
                {"role": "user", "content": "hello"}
            ]
        });
        let anthropic = serde_json::json!({
            "system": "you are helpful",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]
        });
        let responses = serde_json::json!({
            "instructions": "you are helpful",
            "input": "hello"
        });
        let a = body_hash_from_body(&openai).unwrap();
        let b = body_hash_from_body(&anthropic).unwrap();
        let c = body_hash_from_body(&responses).unwrap();
        assert_eq!(a, b, "同一段对话的不同协议形状应得到同一 body_hash");
        assert_eq!(a, c);
        assert_eq!(a.len(), 16, "derive_body_hash 取前 8 字节 → 16 个 hex 字符");

        // 多轮：只有 messages 增长，首条 system + 首条 user 不变 → key 稳定
        let round2 = serde_json::json!({
            "messages": [
                {"role": "system", "content": "you are helpful"},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"},
                {"role": "user", "content": "more"}
            ]
        });
        assert_eq!(body_hash_from_body(&round2).unwrap(), a);

        assert!(body_hash_from_body(&serde_json::json!({"model": "gpt-4o"})).is_none());
    }

    // ------------------------------------------------------------ 存 / 取 / 释放

    fn group_id(id: &str) -> ChannelGroupId {
        ChannelGroupId::from(id)
    }

    #[test]
    fn store_lookup_and_hits() {
        let clock = Arc::new(crate::clock::ManualClock::new(1_000));
        let r: MemoryStickyResolver = MemoryStickyResolver::with_clock(clock.clone());
        let key = StickyKey::from("k");
        let gid = group_id("grp-1");

        r.store(key.clone(), gid.clone(), ChannelId::from("ch-a"), 60_000);
        let rec = r.lookup(&key, &gid).unwrap();
        assert_eq!(rec.channel_id.as_str(), "ch-a");
        assert_eq!(rec.hits, 1);
        assert_eq!(rec.created_at_ms, 1_000);
        assert_eq!(rec.expires_at_ms, 61_000);

        // 同一通道再存 → hits+1，created_at 不变（"这个会话从什么时候开始粘的"）
        clock.advance(1_000);
        r.store(key.clone(), gid.clone(), ChannelId::from("ch-a"), 60_000);
        let rec = r.lookup(&key, &gid).unwrap();
        assert_eq!(rec.hits, 2);
        assert_eq!(rec.created_at_ms, 1_000);
        assert_eq!(rec.expires_at_ms, 62_000);

        // 换通道 → 重新计时
        r.store(key.clone(), gid.clone(), ChannelId::from("ch-b"), 60_000);
        let rec = r.lookup(&key, &gid).unwrap();
        assert_eq!(rec.channel_id.as_str(), "ch-b");
        assert_eq!(rec.hits, 1);
        assert_eq!(rec.created_at_ms, 2_000);

        // TTL 到期 → lookup 返回 None（记录还在后端，等 gc）
        clock.advance(60_001);
        assert!(r.lookup(&key, &gid).is_none());
        assert_eq!(r.gc(clock.now_ms()), 1);
        assert_eq!(r.backend().len(), 0);
    }

    #[test]
    fn release_and_release_group() {
        let r = MemoryStickyResolver::memory();
        let g1 = group_id("grp-1");
        let g2 = group_id("grp-2");

        r.store(StickyKey::from("k1"), g1.clone(), ChannelId::from("ch-a"), 60_000);
        r.store(StickyKey::from("k1"), g2.clone(), ChannelId::from("ch-c"), 60_000);
        r.store(StickyKey::from("k2"), g1.clone(), ChannelId::from("ch-b"), 60_000);

        // release 按 key 跨组清理
        r.release(&StickyKey::from("k1"), ReleaseReason::HardError);
        assert!(r.lookup(&StickyKey::from("k1"), &g1).is_none());
        assert!(r.lookup(&StickyKey::from("k1"), &g2).is_none());
        assert!(r.lookup(&StickyKey::from("k2"), &g1).is_some());
        assert_eq!(r.releases(), vec![(StickyKey::from("k1"), ReleaseReason::HardError)]);

        // release_group 清一个组
        r.release_group(&g1, ReleaseReason::UserPinChange);
        assert!(r.lookup(&StickyKey::from("k2"), &g1).is_none());
        assert_eq!(
            r.releases(),
            vec![
                (StickyKey::from("k1"), ReleaseReason::HardError),
                (StickyKey::from("k2"), ReleaseReason::UserPinChange)
            ]
        );

        // 释放不存在的 key / 组：无副作用、不记录
        r.release(&StickyKey::from("nope"), ReleaseReason::Ttl);
        r.release_group(&group_id("nope"), ReleaseReason::Ttl);
        assert_eq!(r.releases().len(), 2);
    }

    #[test]
    fn release_then_revive_by_priority() {
        // "复活"：释放后同一会话重新按 selector 选，可以回到原通道
        let r = MemoryStickyResolver::memory();
        let grp = group_id("grp-1");
        let key = StickyKey::from("s");
        r.store(key.clone(), grp.clone(), ChannelId::from("ch-a"), 60_000);
        assert!(r.lookup(&key, &grp).is_some());

        r.release(&key, ReleaseReason::Cooldown);
        assert!(r.lookup(&key, &grp).is_none());

        // 冷却结束后重新固化（由 selector 调用 store 完成）
        r.store(key.clone(), grp.clone(), ChannelId::from("ch-a"), 60_000);
        assert_eq!(r.lookup(&key, &grp).unwrap().channel_id.as_str(), "ch-a");
    }

    /// 自定义后端：证明 P4 可以直接换 SQLite 实现，策略逻辑复用
    #[derive(Default)]
    struct CountingBackend {
        inner: MemoryStickyStore,
        puts: Mutex<u64>,
    }

    impl StickyBackend for CountingBackend {
        fn get(&self, group: &ChannelGroupId, key: &StickyKey) -> Option<StickyRecord> {
            self.inner.get(group, key)
        }
        fn put(&self, rec: &StickyRecord) {
            *self.puts.lock().unwrap() += 1;
            self.inner.put(rec);
        }
        fn delete(&self, group: &ChannelGroupId, key: &StickyKey) {
            self.inner.delete(group, key);
        }
        fn delete_group(&self, group: &ChannelGroupId) -> u64 {
            self.inner.delete_group(group)
        }
        fn list(&self, group: Option<&ChannelGroupId>) -> Vec<StickyRecord> {
            self.inner.list(group)
        }
        fn gc(&self, now_ms: i64) -> u64 {
            self.inner.gc(now_ms)
        }
    }

    #[test]
    fn resolver_works_over_custom_backend() {
        let r = BackedStickyResolver::new(CountingBackend::default(), Arc::new(SystemClock));
        let doc = groups();
        let grp = g(&doc, "grp-1");
        let mut h = hints();
        h.session_id = Some("sess-x".into());
        let key = r.key_for(&grp, &h).unwrap();
        r.store(key.clone(), grp.id.clone(), ChannelId::from("ch-a"), 1000);
        assert!(r.lookup(&key, &grp.id).is_some());
        assert_eq!(*r.backend().puts.lock().unwrap(), 1);
        assert_eq!(BackedStickyResolver::<CountingBackend>::ttl_ms_for(&grp, 42), 12 * 3_600_000);
    }
}

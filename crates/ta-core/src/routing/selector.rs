//! 选择器：把 `RouteDecision.candidates` 变成一个**有序的失败转移链**。
//!
//! 七种选择器（`docs/02` §2.7）全部在这里实现：
//!
//! | selector | 语义 |
//! |---|---|
//! | `priority` | 严格按 `priority` 升序 |
//! | `sticky_then_priority` | 先查粘性；未命中按 priority 并写回粘性（默认） |
//! | `quota_first` | 剩余额度比例大的优先 |
//! | `cheapest` | 单价低的优先（需要 `PricingEngine`；缺失时退化为配置顺序） |
//! | `lowest_latency` | EWMA 延迟低的优先 |
//! | `weighted_random` | 按 `weight` 加权随机 |
//! | `round_robin` | 轮询 |
//!
//! **确定性**：`weighted_random` 的随机种子由 (now_ms, 会话, 候选集) 派生，
//! `round_robin` 的状态放在 `self` 里。两者都保证"同一输入 + 同一状态 → 同一结果"，
//! 模拟器因此可以复现真实决策。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::domain::*;
use crate::pricing::{PricingEngine, ResolvedPrice};
use crate::routing::engine::quota_remaining_of;
use crate::routing::index::RoutingIndex;
use crate::routing::{
    ChannelSelector, EliminateReason, RouteContext, RouteDecision, RouteHints, Selection,
};
use crate::sticky::StickyResolver;
use crate::types::*;

#[derive(Debug, Clone)]
pub struct SelectorOptions {
    /// `sticky.ttl_h == 0` 时使用的兜底 TTL
    pub default_ttl_ms: i64,
    /// `select` 内是否写回粘性表（真实链路：写；纯回放：关掉）
    pub sticky_write: bool,
}

impl Default for SelectorOptions {
    fn default() -> Self {
        Self { default_ttl_ms: 12 * 3_600_000, sticky_write: true }
    }
}

pub struct ChannelSelectorImpl {
    index: Arc<RoutingIndex>,
    sticky: Option<Arc<dyn StickyResolver>>,
    pricing: Option<Arc<dyn PricingEngine>>,
    opts: SelectorOptions,
    /// round_robin 计数器：路由键（组 id / 模型名）→ 已轮询次数
    rr: Mutex<BTreeMap<String, u64>>,
}

impl ChannelSelectorImpl {
    pub fn new(index: Arc<RoutingIndex>) -> Self {
        Self {
            index,
            sticky: None,
            pricing: None,
            opts: SelectorOptions::default(),
            rr: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_sticky(mut self, sticky: Arc<dyn StickyResolver>) -> Self {
        self.sticky = Some(sticky);
        self
    }

    pub fn with_pricing(mut self, pricing: Arc<dyn PricingEngine>) -> Self {
        self.pricing = Some(pricing);
        self
    }

    pub fn with_options(mut self, opts: SelectorOptions) -> Self {
        self.opts = opts;
        self
    }

    pub fn index(&self) -> &Arc<RoutingIndex> {
        &self.index
    }

    /// 生效的通道组：优先用模型对应的组，否则按"与候选集重叠最多"反推
    /// （`use_group` 覆盖到别的组时就是这个分支在起作用）。
    pub fn resolve_group<'a>(
        &'a self,
        decision: &RouteDecision,
        hints: &RouteHints,
    ) -> Option<&'a ChannelGroup> {
        if let Some(m) = hints.model.as_deref() {
            let canonical = self.index.canonical_model(m);
            if let Some(g) = self.index.group_for_model(&canonical) {
                if decision.candidates.is_empty()
                    || decision.candidates.iter().any(|c| g.members.contains(c))
                {
                    return Some(g);
                }
            }
        }
        let mut best: Option<(&ChannelGroup, usize)> = None;
        for g in self.index.groups.values() {
            let score = decision.candidates.iter().filter(|c| g.members.contains(c)).count();
            if score > 0 && best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
                best = Some((g, score));
            }
        }
        best.map(|(g, _)| g)
    }

    /// 该选择器实际使用的候选顺序（**不含**粘性命中/固定通道）：
    /// 这是"失败转移链"的排序依据。
    pub fn order_candidates(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
        group: Option<&ChannelGroup>,
    ) -> Vec<ChannelId> {
        let ids = decision.candidates.clone();
        match decision.selector {
            Selector::Priority | Selector::StickyThenPriority => self.by_priority(&ids),
            Selector::QuotaFirst => {
                let mut v = ids;
                v.sort_by(|a, b| {
                    let ra = self.quota_score(ctx, a);
                    let rb = self.quota_score(ctx, b);
                    rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                });
                v
            }
            Selector::Cheapest => {
                let mut v: Vec<(ChannelId, f64)> = ids
                    .into_iter()
                    .map(|id| {
                        let s = self.price_score(&id);
                        (id, s)
                    })
                    .collect();
                v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                v.into_iter().map(|(id, _)| id).collect()
            }
            Selector::LowestLatency => {
                let mut v = ids;
                v.sort_by_key(|id| ctx.latency_ms.get(id).copied().unwrap_or(u32::MAX));
                v
            }
            Selector::WeightedRandom => self.weighted_order(&ids, hints, ctx),
            Selector::RoundRobin => {
                let ordered = self.by_priority(&ids);
                let n = self.next_rr(&self.rr_key(decision, hints, group));
                rotate(ordered, n)
            }
        }
    }

    fn by_priority(&self, ids: &[ChannelId]) -> Vec<ChannelId> {
        let mut v: Vec<(usize, i32, ChannelId)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let p = self.index.channel(id).map(|c| c.priority).unwrap_or(i32::MAX);
                (i, p, id.clone())
            })
            .collect();
        // 稳定：priority 相同 → 保持候选集原顺序
        v.sort_by_key(|(i, p, _)| (*p, *i));
        v.into_iter().map(|(_, _, id)| id).collect()
    }

    fn quota_score(&self, ctx: &RouteContext, id: &ChannelId) -> f64 {
        self.index
            .channel(id)
            .and_then(|ch| quota_remaining_of(ctx, &ch.credential))
            .unwrap_or(-1.0)
    }

    fn price_score(&self, id: &ChannelId) -> f64 {
        let Some(pricing) = &self.pricing else { return 0.0 };
        let Some(ch) = self.index.channel(id) else { return f64::INFINITY };
        price_score(&pricing.resolve(&ch.model, &ch.provider, &ch.credential))
    }

    fn rr_key(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        group: Option<&ChannelGroup>,
    ) -> String {
        if let Some(g) = group {
            return format!("group:{}", g.id);
        }
        match (&decision.pinned, hints.model.as_deref()) {
            (Some(p), _) => format!("pin:{p}"),
            (None, Some(m)) => format!("model:{m}"),
            (None, None) => "default".to_string(),
        }
    }

    fn next_rr(&self, key: &str) -> u64 {
        let mut g = self.rr.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.entry(key.to_string()).or_insert(0);
        let cur = *e;
        *e = e.wrapping_add(1);
        cur
    }

    fn weighted_order(
        &self,
        ids: &[ChannelId],
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Vec<ChannelId> {
        let weights: Vec<u64> = ids
            .iter()
            .map(|id| self.index.channel(id).map(|c| (c.weight.max(1)) as u64).unwrap_or(1))
            .collect();
        let seed = weighted_seed(ids, hints, ctx.now_ms);
        weighted_shuffle(ids, &weights, SplitMix64::new(seed))
    }

    /// 粘性命中（含"命中但已不可用 → 按原因释放"）。
    ///
    /// 返回值：`(命中的通道, 派生出的 key, 是否本来就有一条活着的记录, 本次是否释放了它)`
    fn sticky_pick(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        group: &ChannelGroup,
    ) -> (Option<ChannelId>, Option<StickyKey>, bool, bool) {
        let Some(resolver) = &self.sticky else { return (None, None, false, false) };
        if !group.sticky.enabled {
            return (None, None, false, false);
        }
        let Some(key) = resolver.key_for(group, hints) else { return (None, None, false, false) };

        let mut released = false;
        if let Some(rec) = resolver.lookup(&key, &group.id) {
            if decision.candidates.contains(&rec.channel_id) {
                return (Some(rec.channel_id), Some(key), true, false);
            }
            // 粘住的通道这次不可用：能归因到运行态就立刻释放（额度耗尽 / 冷却 / 失效），
            // 纯策略收窄（标签排除、临时绕开）**不释放** —— 下个请求可能就不排除了。
            if let Some((_, reason)) =
                decision.eliminated.iter().find(|(c, _)| c == &rec.channel_id)
            {
                if let Some(why) = release_reason_for(*reason) {
                    resolver.release(&key, why);
                    released = true;
                }
            }
            return (None, Some(key), true, released);
        }
        (None, Some(key), false, false)
    }
}

impl ChannelSelector for ChannelSelectorImpl {
    fn select(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Option<Selection> {
        if decision.candidates.is_empty() {
            return None;
        }
        let group = self.resolve_group(decision, hints);

        // 粘性只在 sticky_then_priority 下参与（其它选择器有各自的明确语义）
        let mut sticky_hit = false;
        let mut sticky_key = None;
        let mut sticky_alive = false;
        let mut sticky_released = false;
        let mut chosen: Option<ChannelId> = None;
        if decision.selector == Selector::StickyThenPriority {
            if let Some(g) = group {
                let (hit, key, alive, released) = self.sticky_pick(decision, hints, g);
                sticky_hit = hit.is_some();
                sticky_alive = alive;
                sticky_released = released;
                chosen = hit;
                sticky_key = key;
            }
        }

        // 用户显式 pin 的通道优先级最高（engine 已保证它在候选集里）
        if let Some(p) = &decision.pinned {
            if decision.candidates.contains(p) {
                chosen = Some(p.clone());
                sticky_hit = false;
            }
        }

        let ordered = self.order_candidates(decision, hints, ctx, group);
        let mut chain: Vec<ChannelId> = Vec::with_capacity(ordered.len());
        if let Some(c) = &chosen {
            chain.push(c.clone());
        }
        for id in ordered {
            if Some(&id) != chosen.as_ref() {
                chain.push(id);
            }
        }
        let channel = chain.first()?.clone();

        // 写回粘性。
        // 例外：会话原本有活着的记录、只是这条规则把它临时收窄掉了 → **不覆盖**，
        // 这样 "临时绕开某家" 不会永久改掉整个会话的归属（下个请求可能就不绕开了）。
        let should_store = !sticky_alive || sticky_hit || sticky_released;
        if self.opts.sticky_write && should_store {
            if let (Some(resolver), Some(g), Some(key)) = (&self.sticky, group, &sticky_key) {
                let ttl = if g.sticky.ttl_h == 0 {
                    self.opts.default_ttl_ms
                } else {
                    g.sticky.ttl_h as i64 * 3_600_000
                };
                resolver.store(key.clone(), g.id.clone(), channel.clone(), ttl);
            }
        }

        Some(Selection { channel, sticky_hit, sticky_key, chain })
    }
}

/// 淘汰原因 → 粘性释放原因。返回 `None` 表示"这次不释放"。
pub fn release_reason_for(r: EliminateReason) -> Option<ReleaseReason> {
    match r {
        EliminateReason::QuotaExhausted => Some(ReleaseReason::QuotaExhausted),
        EliminateReason::Cooling => Some(ReleaseReason::Cooldown),
        EliminateReason::Disabled
        | EliminateReason::InvalidCredential
        | EliminateReason::LaneDown
        | EliminateReason::NoLane => Some(ReleaseReason::HardError),
        EliminateReason::TaggedOut
        | EliminateReason::ExcludedByPolicy
        | EliminateReason::NotInGroup => None,
    }
}

/// 单价的标量代理：所有费率之和 × 倍率 / 除数。未定价 = `+∞`（排最后）。
pub fn price_score(price: &ResolvedPrice) -> f64 {
    if !price.is_priced() || price.divisor <= 0.0 {
        return f64::INFINITY;
    }
    let sum: f64 = price.rates.values().sum();
    if sum <= 0.0 {
        f64::INFINITY
    } else {
        sum * price.multiplier / price.divisor
    }
}

fn rotate<T: Clone>(mut v: Vec<T>, n: u64) -> Vec<T> {
    if v.is_empty() {
        return v;
    }
    let k = (n % v.len() as u64) as usize;
    v.rotate_left(k);
    v
}

// ---------------------------------------------------------------- 确定性 PRNG

/// SplitMix64：小、快、无需 rand 依赖。
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// 种子 = SHA256(now_ms ‖ 会话标识 ‖ 候选集)。同一输入必然同一种子 → 可复现。
fn weighted_seed(ids: &[ChannelId], hints: &RouteHints, now_ms: i64) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(now_ms.to_le_bytes());
    if let Some(s) = &hints.session_id {
        h.update(s.as_bytes());
    }
    h.update(b"\x1f");
    if let Some(s) = &hints.conversation_id {
        h.update(s.as_bytes());
    }
    h.update(b"\x1f");
    if let Some(s) = &hints.user_id {
        h.update(s.as_bytes());
    }
    h.update(b"\x1f");
    if let Some(s) = &hints.body_hash {
        h.update(s.as_bytes());
    }
    for id in ids {
        h.update(b"\x1f");
        h.update(id.as_str().as_bytes());
    }
    let out = h.finalize();
    u64::from_le_bytes(out[..8].try_into().expect("sha256 至少 8 字节"))
}

/// 无放回加权洗牌 → 得到一条完整的有序链（不只是第一名）。
fn weighted_shuffle(ids: &[ChannelId], weights: &[u64], mut rng: SplitMix64) -> Vec<ChannelId> {
    let mut pool: Vec<(&ChannelId, u64)> =
        ids.iter().zip(weights.iter()).map(|(id, w)| (id, (*w).max(1))).collect();
    let mut out = Vec::with_capacity(pool.len());
    while !pool.is_empty() {
        let total: u64 = pool.iter().map(|(_, w)| *w).sum();
        let mut r = rng.next_u64() % total.max(1);
        let mut idx = pool.len() - 1;
        for (i, (_, w)) in pool.iter().enumerate() {
            if r < *w {
                idx = i;
                break;
            }
            r -= *w;
        }
        out.push(pool.remove(idx).0.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::domain::ConfigDoc;
    use crate::pricing::PriceSourceKind;
    use crate::sticky::resolver::MemoryStickyResolver;

    const YAML: &str = r#"
version: 1
net:
  default_lane: lane-direct
lanes:
  - { id: lane-direct, name: 直连, kind: direct }
providers:
  - { id: p-openai, name: OpenAI, protocol: openai, base_url: "https://api.openai.com/v1" }
  - { id: p-relay, name: Relay, protocol: openai, base_url: "https://relay.example/v1" }
credentials:
  - { id: c-a, provider: p-openai, label: a, secret: "${secret:a}" }
  - { id: c-b, provider: p-relay, label: b, secret: "${secret:b}" }
  - { id: c-c, provider: p-relay, label: c, secret: "${secret:c}" }
channels:
  - { id: ch-a, model: gpt-4o, provider: p-openai, credential: c-a, upstream_model: gpt-4o, priority: 10, weight: 10, enabled: true }
  - { id: ch-b, model: gpt-4o, provider: p-relay, credential: c-b, upstream_model: gpt-4o, priority: 20, weight: 5, enabled: true }
  - { id: ch-c, model: gpt-4o, provider: p-relay, credential: c-c, upstream_model: gpt-4o, priority: 30, weight: 1, enabled: true }
channel_groups:
  - id: grp-gpt-4o
    model: gpt-4o
    members: [ch-a, ch-b, ch-c]
    selector: sticky_then_priority
    sticky: { enabled: true, ttl_h: 12, key: [header:x-session-id, body_hash] }
    release_on: [quota_exhausted, hard_error]
"#;

    fn doc() -> ConfigDoc {
        config::from_yaml(YAML).expect("yaml")
    }

    fn idx(doc: &ConfigDoc) -> Arc<RoutingIndex> {
        Arc::new(RoutingIndex::build(doc))
    }

    /// 直接把候选集塞进 RouteDecision，隔离测选择器
    fn decision(sel: Selector, cands: &[&str]) -> RouteDecision {
        RouteDecision {
            candidates: cands.iter().map(|c| ChannelId::from(*c)).collect(),
            selector: sel,
            ..Default::default()
        }
    }

    fn hints_session(s: &str) -> RouteHints {
        RouteHints {
            model: Some("gpt-4o".into()),
            session_id: Some(s.into()),
            ..Default::default()
        }
    }

    fn ctx() -> RouteContext {
        RouteContext { now_ms: 1_699_920_000_000, ..Default::default() }
    }

    fn selector_with(doc: &ConfigDoc, sticky: Arc<dyn StickyResolver>) -> ChannelSelectorImpl {
        ChannelSelectorImpl::new(idx(doc)).with_sticky(sticky)
    }

    // ------------------------------------------------------------ 7 种选择器

    #[test]
    fn selector_priority_follows_channel_priority() {
        let sel = ChannelSelectorImpl::new(idx(&doc()));
        let d = decision(Selector::Priority, &["ch-c", "ch-a", "ch-b"]);
        let s = sel.select(&d, &hints_session("s1"), &ctx()).unwrap();
        assert_eq!(s.channel.as_str(), "ch-a");
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-a", "ch-b", "ch-c"]
        );
        assert!(!s.sticky_hit);
    }

    #[test]
    fn selector_sticky_then_priority_is_default_and_reuses_choice() {
        let d = doc();
        assert_eq!(
            d.channel_groups[0].selector,
            Selector::StickyThenPriority,
            "默认选择器必须是 sticky_then_priority"
        );
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let dec = decision(Selector::StickyThenPriority, &["ch-a", "ch-b", "ch-c"]);

        // 第一次：未命中 → 按 priority
        let s1 = sel.select(&dec, &hints_session("s1"), &ctx()).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");
        assert!(!s1.sticky_hit);
        assert_eq!(s1.sticky_key.as_ref().unwrap().as_str(), "s1");

        // 第二次同会话：命中
        let s2 = sel.select(&dec, &hints_session("s1"), &ctx()).unwrap();
        assert_eq!(s2.channel.as_str(), "ch-a");
        assert!(s2.sticky_hit);

        // 换会话 → 重新按 priority（此处仍然 a，但必须是"未命中"）
        let s3 = sel.select(&dec, &hints_session("s2"), &ctx()).unwrap();
        assert_eq!(s3.channel.as_str(), "ch-a");
        assert!(!s3.sticky_hit);
    }

    #[test]
    fn selector_quota_first_prefers_most_remaining() {
        let sel = ChannelSelectorImpl::new(idx(&doc()));
        let d = decision(Selector::QuotaFirst, &["ch-a", "ch-b", "ch-c"]);
        let mut c = ctx();
        c.quota_remaining.insert(CredentialId::from("c-a"), Some(0.9));
        c.quota_remaining.insert(CredentialId::from("c-b"), Some(0.2));
        c.quota_remaining.insert(CredentialId::from("c-c"), None);
        let s = sel.select(&d, &hints_session("s"), &c).unwrap();
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-a", "ch-b", "ch-c"],
            "额度大的优先，未知额度排最后"
        );
    }

    #[test]
    fn selector_cheapest_uses_pricing_engine() {
        struct FakePricing;
        impl PricingEngine for FakePricing {
            fn resolve(&self, _m: &str, provider: &ProviderId, _c: &CredentialId) -> ResolvedPrice {
                let rate = match provider.as_str() {
                    "p-openai" => 10.0,
                    "p-relay" => 1.0,
                    _ => 5.0,
                };
                let mut rates = BTreeMap::new();
                rates.insert(TokenKind::Input, rate);
                ResolvedPrice {
                    rates,
                    multiplier: 1.0,
                    currency: "USD".into(),
                    divisor: 1_000_000.0,
                    source: PriceSourceKind::Builtin,
                    plan_fee: None,
                    entry_index: 0,
                }
            }
        }

        let sel = ChannelSelectorImpl::new(idx(&doc())).with_pricing(Arc::new(FakePricing));
        let d = decision(Selector::Cheapest, &["ch-a", "ch-b", "ch-c"]);
        let s = sel.select(&d, &hints_session("s"), &ctx()).unwrap();
        assert_eq!(s.channel.as_str(), "ch-b");
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-b", "ch-c", "ch-a"]
        );

        // 没有定价引擎 → 保持配置顺序（退化为原顺序，不 panic）
        let sel2 = ChannelSelectorImpl::new(idx(&doc()));
        let s2 = sel2.select(&d, &hints_session("s"), &ctx()).unwrap();
        assert_eq!(s2.channel.as_str(), "ch-a");
    }

    #[test]
    fn selector_lowest_latency() {
        let sel = ChannelSelectorImpl::new(idx(&doc()));
        let d = decision(Selector::LowestLatency, &["ch-a", "ch-b", "ch-c"]);
        let mut c = ctx();
        c.latency_ms.insert(ChannelId::from("ch-a"), 300);
        c.latency_ms.insert(ChannelId::from("ch-b"), 80);
        // ch-c 未知延迟 → 排最后
        let s = sel.select(&d, &hints_session("s"), &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-b");
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-b", "ch-a", "ch-c"]
        );
    }

    #[test]
    fn selector_weighted_random_is_deterministic_and_weighted() {
        let sel = ChannelSelectorImpl::new(idx(&doc()));
        let d = decision(Selector::WeightedRandom, &["ch-a", "ch-b", "ch-c"]);
        let c = ctx();

        // 同一输入 → 同一结果
        let first = sel.select(&d, &hints_session("s"), &c).unwrap();
        for _ in 0..50 {
            let again = sel.select(&d, &hints_session("s"), &c).unwrap();
            assert_eq!(
                again.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                first.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>()
            );
        }

        // 权重生效：ch-a(10) 应该明显多于 ch-c(1)
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for i in 0..2000 {
            let mut cc = ctx();
            cc.now_ms += i;
            let s = sel.select(&d, &hints_session(&format!("s{i}")), &cc).unwrap();
            *counts.entry(s.channel.to_string()).or_default() += 1;
        }
        let a = counts["ch-a"];
        let c3 = counts["ch-c"];
        assert!(a > 500, "ch-a(weight 10) 命中 {a} 次，太少");
        assert!(a > c3 * 3, "权重未被尊重: a={a} c={c3}");
    }

    #[test]
    fn selector_round_robin_rotates_and_wraps() {
        let sel = ChannelSelectorImpl::new(idx(&doc()));
        let d = decision(Selector::RoundRobin, &["ch-a", "ch-b", "ch-c"]);
        let mut got = vec![];
        for _ in 0..4 {
            got.push(sel.select(&d, &hints_session("x"), &ctx()).unwrap().channel.to_string());
        }
        assert_eq!(got, vec!["ch-a", "ch-b", "ch-c", "ch-a"]);

        // 链也随起点旋转
        let s = sel.select(&d, &hints_session("x"), &ctx()).unwrap();
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-b", "ch-c", "ch-a"]
        );
    }

    // ------------------------------------------------------------ 粘性链路

    #[test]
    fn sticky_auto_releases_when_quota_exhausted_and_reselects() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let all = decision(Selector::StickyThenPriority, &["ch-a", "ch-b", "ch-c"]);
        let h = hints_session("sess-1");
        let c = ctx();

        let s1 = sel.select(&all, &h, &c).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");
        let s2 = sel.select(&all, &h, &c).unwrap();
        assert!(s2.sticky_hit);

        // ch-a 额度耗尽 → engine 会把它剔除并给原因
        let mut after = decision(Selector::StickyThenPriority, &["ch-b", "ch-c"]);
        after.eliminated.push((ChannelId::from("ch-a"), EliminateReason::QuotaExhausted));
        let s3 = sel.select(&after, &h, &c).unwrap();
        assert_eq!(s3.channel.as_str(), "ch-b", "释放后重选下一个候选");
        assert!(!s3.sticky_hit);
        assert_eq!(
            sticky.releases(),
            vec![(StickyKey::from("sess-1"), ReleaseReason::QuotaExhausted)]
        );

        // 同会话后续请求继续用新通道
        let s4 = sel.select(&after, &h, &c).unwrap();
        assert_eq!(s4.channel.as_str(), "ch-b");
        assert!(s4.sticky_hit, "新通道也必须被固化，否则会话会漂移");

        // 老通道复活也不抢回粘性（额度连贯 + prompt cache）
        let s5 = sel.select(&all, &h, &c).unwrap();
        assert_eq!(s5.channel.as_str(), "ch-b");
        assert!(s5.sticky_hit);
    }

    #[test]
    fn sticky_survives_policy_only_exclusion_without_release() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-2");
        let c = ctx();
        sel.select(&decision(Selector::StickyThenPriority, &["ch-a", "ch-b"]), &h, &c).unwrap();

        // ch-a 因标签被这条规则排除 → 不释放，也**不改写**会话归属（临时绕开而已）
        let mut dec = decision(Selector::StickyThenPriority, &["ch-b"]);
        dec.eliminated.push((ChannelId::from("ch-a"), EliminateReason::TaggedOut));
        let s = sel.select(&dec, &h, &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-b");
        assert!(sticky.releases().is_empty(), "策略收窄不应释放粘性");
        let rec = sticky
            .lookup(&StickyKey::from("sess-2"), &ChannelGroupId::from("grp-gpt-4o"))
            .expect("粘性记录仍在");
        assert_eq!(rec.channel_id.as_str(), "ch-a", "临时绕开不应改写会话归属");
    }

    #[test]
    fn sticky_expires_by_ttl_and_reselects_by_priority() {
        let d = doc();
        let clock = Arc::new(crate::clock::ManualClock::new(1_699_920_000_000));
        let sticky = Arc::new(MemoryStickyResolver::with_clock(clock.clone()));
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-ttl");
        let dec = decision(Selector::StickyThenPriority, &["ch-a", "ch-b"]);

        let s1 = sel.select(&dec, &h, &ctx()).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");

        // 12h 后过期 → 未命中 → 按 priority 重新选择（"复活"回 ch-a）
        clock.advance(12 * 3_600_000 + 1);
        let s2 = sel.select(&dec, &h, &ctx()).unwrap();
        assert!(!s2.sticky_hit);
        assert_eq!(s2.channel.as_str(), "ch-a");
    }

    #[test]
    fn sticky_is_ignored_for_non_sticky_selectors() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-3");
        sel.select(&decision(Selector::StickyThenPriority, &["ch-a", "ch-b"]), &h, &ctx()).unwrap();

        let s =
            sel.select(&decision(Selector::LowestLatency, &["ch-a", "ch-b"]), &h, &ctx()).unwrap();
        assert!(!s.sticky_hit);
        assert!(s.sticky_key.is_none());
    }

    #[test]
    fn pinned_channel_wins() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky);
        let mut dec = decision(Selector::StickyThenPriority, &["ch-c", "ch-a", "ch-b"]);
        dec.pinned = Some(ChannelId::from("ch-c"));
        let s = sel.select(&dec, &hints_session("s"), &ctx()).unwrap();
        assert_eq!(s.channel.as_str(), "ch-c");
        assert!(!s.sticky_hit);
        assert_eq!(
            s.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-c", "ch-a", "ch-b"]
        );
    }

    #[test]
    fn release_reason_mapping_is_explicit() {
        assert_eq!(
            release_reason_for(EliminateReason::QuotaExhausted),
            Some(ReleaseReason::QuotaExhausted)
        );
        assert_eq!(release_reason_for(EliminateReason::Cooling), Some(ReleaseReason::Cooldown));
        assert_eq!(
            release_reason_for(EliminateReason::InvalidCredential),
            Some(ReleaseReason::HardError)
        );
        assert_eq!(release_reason_for(EliminateReason::TaggedOut), None);
        assert_eq!(release_reason_for(EliminateReason::NotInGroup), None);
    }
}

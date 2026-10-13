//! 选择器：把 `RouteDecision.candidates` 变成一个**有序的失败转移链**。
//!
//! **只有一种策略：平均粘性**（[docs/48]）。规则就三条：
//!
//! 1. **粘**：按会话标识派生 key，命中且该线路仍可用 → 用它（同一会话固定走同一条线路）；
//! 2. **平**：没命中（新会话）→ 挑 **token 用量最少**的线路；并列时按 `priority`，
//!    再并列按候选集原序；
//! 3. **改粘**：这条线路硬失败/冷却 → **释放**粘性，下个请求回到第 2 步 ——
//!    于是那个会话自然"粘到另一个身上"（用户的原话）。
//!
//! ## 「没有会话 ID 就随机」
//!
//! 派生不出任何会话标识时**没有 key 可写**，每次请求都走第 2 步。此时若并列还按原序，
//! 一串无 ID 的并发请求会**全砸在同一条**上；所以无 key 时在"用量最少的那一档"里**随机**挑。
//! 有 key 时**必须确定** —— 否则随机本身会打碎粘性（同一会话两个请求落两条线）。
//! 这个区分是刻意的，不是随手加的。
//!
//! ## 「平均」的口径与边界
//!
//! 用量取自 [`RouteContext::balance`]（时间窗内各线路的 token 累计），由装配方
//! （[`crate::routing::BalanceSource`]）提供。
//!
//! * `balance` **空表** = 不知道用量 → 退化为按 `priority`（**不把"没查到"当成"最闲"**）；
//! * 表非空但**某条没有记录** = 这条时间窗内确实一次都没用过 → 当 `0`，
//!   新加的线路因此会先拿到流量（这正是想要的）；
//! * 平均只对**新会话**生效：老会话被粘住不动（这正是"粘"）。所以两条线路的用量
//!   **不会**严格相等，而是新会话持续流向当前较轻的那条。
//!
//! ## 与旧版的差异
//!
//! 旧版有七种选择器（`priority` / `sticky_then_priority` / `cheapest` / `lowest_latency` /
//! `weighted_random` / `round_robin`）。它们全部删除 —— `docs/47` 查清"各家额度耗尽返回什么"
//! 之后确认**自动判断走不通**（同一家 429 既表限流也表额度耗尽、有的家把额度耗尽藏在 400 里），
//! 于是改成**用户用开关手动调度 + 引擎只负责平均**（[docs/48]）。
//! 旧的 `price_score` / 轮询 / 加权随机 / 延迟排序随之消失：选路不再读价格与延迟。
//! 随机源 [`SplitMix64`] 保留，改用于无 key 时的档内随机。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::domain::*;
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
    opts: SelectorOptions,
    /// 无会话 key 时的档内随机**熵源**。
    ///
    /// 为什么不能只用 `ctx.now_ms` 当种子：同一个毫秒内到达的一串无 ID 请求会拿到
    /// 同一个种子、选出同一条线路 —— 那正是"随机"想避免的事。这个计数器保证每次
    /// `select` 的种子都不同。
    ///
    /// 它是**单调递增的**而不是"真随机"：这样"给定同一串调用顺序 → 结果可复现"，
    /// 测试与诊断都还能对得上（`docs/48 §3.1`）。
    entropy: AtomicU64,
}

impl ChannelSelectorImpl {
    pub fn new(index: Arc<RoutingIndex>) -> Self {
        Self {
            index,
            sticky: None,
            opts: SelectorOptions::default(),
            entropy: AtomicU64::new(0x5DEE_CE66_D1CE_4E5B),
        }
    }

    pub fn with_sticky(mut self, sticky: Arc<dyn StickyResolver>) -> Self {
        self.sticky = Some(sticky);
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
    ///
    /// `random_ties = false`（有会话 key）：用量 → priority → 候选集原序，**完全确定**。
    /// `random_ties = true`（无会话 key）：在"用量最少的那一档"内部打乱，
    /// 避免一串无 ID 的并发请求全砸在同一条上。
    pub fn order_candidates(
        &self,
        decision: &RouteDecision,
        _hints: &RouteHints,
        ctx: &RouteContext,
        _group: Option<&ChannelGroup>,
        random_ties: bool,
    ) -> Vec<ChannelId> {
        let ids = &decision.candidates;
        // 空表 = 不知道用量：整列按 0 处理，排序于是完全由 priority 决定。
        let known = !ctx.balance.is_empty();
        let mut v: Vec<(usize, i64, i32, ChannelId)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let used = if known { ctx.balance.get(id).copied().unwrap_or(0) } else { 0 };
                let p = self.index.channel(id).map(|c| c.priority).unwrap_or(i32::MAX);
                (i, used, p, id.clone())
            })
            .collect();
        // 稳定排序：用量 → priority → 候选集原序（`i` 保证"同用量同优先级"不抖动）
        v.sort_by_key(|(i, used, p, _)| (*used, *p, *i));
        let mut out: Vec<ChannelId> = v.into_iter().map(|(_, _, _, id)| id).collect();

        if random_ties && out.len() > 1 {
            let used_of =
                |id: &ChannelId| if known { ctx.balance.get(id).copied().unwrap_or(0) } else { 0 };
            // "最轻的那一档" = 排序后与首元素用量相同的前缀。
            let head = used_of(&out[0]);
            let tier = out.iter().take_while(|id| used_of(id) == head).count();
            if tier > 1 {
                let seed = self.next_entropy() ^ (ctx.now_ms as u64);
                let mut rng = SplitMix64::new(seed);
                // Fisher-Yates 只打乱前 `tier` 个，后面保持"次轻→最重"的有序性。
                for i in (1..tier).rev() {
                    let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                    out.swap(i, j);
                }
            }
        }
        out
    }

    fn next_entropy(&self) -> u64 {
        self.entropy.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
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
            // 粘住的通道这次不可用：能归因到运行态就立刻释放（冷却 / 失效 / 禁用），
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

        // 1) 粘性：现在是唯一策略，不再按 selector 分支（旧版这里要判 StickyThenPriority）。
        let mut sticky_hit = false;
        let mut sticky_key = None;
        let mut sticky_alive = false;
        let mut sticky_released = false;
        let mut chosen: Option<ChannelId> = None;
        if let Some(g) = group {
            let (hit, key, alive, released) = self.sticky_pick(decision, hints, g);
            sticky_hit = hit.is_some();
            sticky_alive = alive;
            sticky_released = released;
            chosen = hit;
            sticky_key = key;
        }

        // 2) 用户显式 pin 的通道优先级最高（engine 已保证它在候选集里）
        if let Some(p) = &decision.pinned {
            if decision.candidates.contains(p) {
                chosen = Some(p.clone());
                sticky_hit = false;
            }
        }

        // 没粘住 → 走平均顺序。**有 key 时确定、无 key 时档内随机**（见模块头注释）。
        let ordered = self.order_candidates(decision, hints, ctx, group, sticky_key.is_none());
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

// ---------------------------------------------------------------- 确定性 PRNG

/// SplitMix64：小、快、无需 rand 依赖。只用于"无会话 key 时档内打乱"。
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config;
    use crate::domain::ConfigDoc;
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
    selector: balanced_sticky
    sticky: { enabled: true, ttl_h: 12, key: [header:x-session-id, body_hash] }
    release_on: [hard_error]
"#;

    fn doc() -> ConfigDoc {
        config::from_yaml(YAML).expect("yaml")
    }

    fn idx(doc: &ConfigDoc) -> Arc<RoutingIndex> {
        Arc::new(RoutingIndex::build(doc))
    }

    /// 直接把候选集塞进 RouteDecision，隔离测选择器
    fn decision(cands: &[&str]) -> RouteDecision {
        RouteDecision {
            candidates: cands.iter().map(|c| ChannelId::from(*c)).collect(),
            selector: Selector::BalancedSticky,
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

    /// **没有任何会话标识**的请求（用来测"没有 ID 就随机"）。
    fn hints_no_key() -> RouteHints {
        RouteHints { model: Some("gpt-4o".into()), ..Default::default() }
    }

    fn ctx() -> RouteContext {
        RouteContext { now_ms: 1_699_920_000_000, ..Default::default() }
    }

    fn ctx_with_usage(pairs: &[(&str, i64)]) -> RouteContext {
        let mut c = ctx();
        for (id, used) in pairs {
            c.balance.insert(ChannelId::from(*id), *used);
        }
        c
    }

    fn selector_with(doc: &ConfigDoc, sticky: Arc<dyn StickyResolver>) -> ChannelSelectorImpl {
        ChannelSelectorImpl::new(idx(doc)).with_sticky(sticky)
    }

    fn chain_of(s: &Selection) -> Vec<&str> {
        s.chain.iter().map(|c| c.as_str()).collect()
    }

    // ------------------------------------------------------------ 只剩一种策略

    #[test]
    fn balanced_sticky_is_the_default_and_the_only_variant() {
        let d = doc();
        assert_eq!(
            d.channel_groups[0].selector,
            Selector::BalancedSticky,
            "配置里写的 balanced_sticky 必须解成 BalancedSticky"
        );
        assert_eq!(Selector::default(), Selector::BalancedSticky, "默认策略只有这一个");
    }

    /// 老配置里的历史取值必须**还能读**（一律落到唯一策略），否则老库升级即解析失败。
    #[test]
    fn legacy_selector_spellings_still_deserialize() {
        for old in [
            "\"priority\"",
            "\"sticky_then_priority\"",
            "\"cheapest\"",
            "\"lowest_latency\"",
            "\"weighted_random\"",
            "\"round_robin\"",
            "\"balanced_sticky\"",
        ] {
            let got: Selector =
                serde_json::from_str(old).unwrap_or_else(|e| panic!("{old} 应能解析：{e}"));
            assert_eq!(got, Selector::BalancedSticky, "{old} 应落到 BalancedSticky");
        }
    }

    // ------------------------------------------------------------ 平：按用量

    /// **题眼**：A 用得多、B 用得少 → 新会话给 B。
    #[test]
    fn new_session_picks_the_least_used_line() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky);

        // 用量：a 已经用了一大堆，b/c 空。候选顺序故意把 a 放前面，
        // 确保"选中 b"是因为**用量**，不是因为顺序。
        let c = ctx_with_usage(&[("ch-a", 10_000), ("ch-b", 0), ("ch-c", 0)]);
        let s = sel.select(&decision(&["ch-a", "ch-b", "ch-c"]), &hints_session("s1"), &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-b", "b/c 并列最轻 → 按 priority 取 b(20) 而非 c(30)");
        assert!(!s.sticky_hit);
        assert_eq!(chain_of(&s), vec!["ch-b", "ch-c", "ch-a"], "次轻在前、最重最后");
    }

    /// 用量不同但**最轻的那条不在候选集里** → 就在剩下的里挑最轻的。
    #[test]
    fn least_used_is_chosen_within_candidates_only() {
        let d = doc();
        let sel = selector_with(&d, Arc::new(MemoryStickyResolver::memory()));
        let c = ctx_with_usage(&[("ch-a", 5), ("ch-b", 900), ("ch-c", 40)]);
        let s = sel.select(&decision(&["ch-b", "ch-c"]), &hints_session("s"), &c).unwrap();
        assert_eq!(
            s.channel.as_str(),
            "ch-c",
            "ch-a 最轻但不在候选集 → 取 ch-c(40) 而非 ch-b(900)"
        );
    }

    /// 用量并列 → 按 priority（a=10 < b=20 < c=30）。
    #[test]
    fn equal_usage_falls_back_to_priority() {
        let d = doc();
        let sel = selector_with(&d, Arc::new(MemoryStickyResolver::memory()));
        let c = ctx_with_usage(&[("ch-a", 7), ("ch-b", 7), ("ch-c", 7)]);
        let s = sel.select(&decision(&["ch-c", "ch-b", "ch-a"]), &hints_session("s"), &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-a", "并列时按 priority，不看候选集顺序");
        assert_eq!(chain_of(&s), vec!["ch-a", "ch-b", "ch-c"]);
    }

    /// **空表 = 不知道**，不是"全是 0 所以随便挑"：必须退回 priority。
    #[test]
    fn empty_balance_falls_back_to_priority_not_zero() {
        let d = doc();
        let sel = selector_with(&d, Arc::new(MemoryStickyResolver::memory()));
        let c = ctx(); // balance 为空
        assert!(c.balance.is_empty());
        let s = sel.select(&decision(&["ch-c", "ch-b", "ch-a"]), &hints_session("s"), &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-a", "不知道用量时按 priority");
        assert_eq!(chain_of(&s), vec!["ch-a", "ch-b", "ch-c"]);
    }

    /// 有会话 key 时**必须确定**：同一会话 + 同一用量 → 每次都同一条（否则粘性会被打碎）。
    #[test]
    fn with_session_key_the_choice_is_deterministic() {
        let d = doc();
        let sel = selector_with(&d, Arc::new(MemoryStickyResolver::memory()));
        // 用量全并列：随机化若没被关掉，这里就会抖
        let c = ctx_with_usage(&[("ch-a", 3), ("ch-b", 3), ("ch-c", 3)]);
        let dec = decision(&["ch-a", "ch-b", "ch-c"]);
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..40 {
            let s = sel.select(&dec, &hints_session(&format!("s{i}")), &c).unwrap();
            seen.insert(s.channel.to_string());
        }
        assert_eq!(seen.len(), 1, "有会话 key 时并列必须稳定，实得 {seen:?}");
        assert_eq!(seen.iter().next().unwrap(), "ch-a");
    }

    // ------------------------------------------------------------ 随机（无 key）

    /// **题眼**：没有会话 ID → 在"最轻的那一档"里随机，且**只**从那一档里挑。
    #[test]
    fn without_session_key_spreads_within_the_lightest_tier() {
        let d = doc();
        let sel = selector_with(&d, Arc::new(MemoryStickyResolver::memory()));
        // a 很重，b/c 并列最轻 → 随机只该在 {b,c} 里
        let c = ctx_with_usage(&[("ch-a", 10_000), ("ch-b", 0), ("ch-c", 0)]);
        let dec = decision(&["ch-a", "ch-b", "ch-c"]);

        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for _ in 0..120 {
            let s = sel.select(&dec, &hints_no_key(), &c).unwrap();
            *counts.entry(s.channel.to_string()).or_default() += 1;
        }
        assert!(!counts.contains_key("ch-a"), "最重的那条不该被挑中：{counts:?}");
        assert!(
            counts.contains_key("ch-b") && counts.contains_key("ch-c"),
            "b/c 都该拿到活：{counts:?}"
        );
        // 均匀性：各约 60，给足容差只排除"几乎全给一条"
        let (b, c2) = (counts["ch-b"], counts["ch-c"]);
        assert!(b > 20 && c2 > 20, "档内应接近均匀，实得 b={b} c={c2}");
    }

    /// 无 key 时**不写**粘性表（没有 key 可写），所以也不该抱怨。
    #[test]
    fn without_session_key_nothing_is_stored() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let s = sel.select(&decision(&["ch-a", "ch-b"]), &hints_no_key(), &ctx()).unwrap();
        assert!(s.sticky_key.is_none(), "派生不出 key 就不该编一个");
        assert!(!s.sticky_hit);
        assert!(sticky.releases().is_empty());
    }

    // ------------------------------------------------------------ 粘

    #[test]
    fn sticky_reuses_choice_for_the_same_session() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let dec = decision(&["ch-a", "ch-b", "ch-c"]);

        // 第一次：未命中（用量都未知 → priority → ch-a）
        let s1 = sel.select(&dec, &hints_session("s1"), &ctx()).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");
        assert!(!s1.sticky_hit);
        assert_eq!(s1.sticky_key.as_ref().unwrap().as_str(), "s1");

        // 第二次同会话：命中 —— 即使后来 ch-a 变得最重，也必须**仍然走 ch-a**（这才叫粘）
        let heavy_a = ctx_with_usage(&[("ch-a", 99_999), ("ch-b", 0), ("ch-c", 0)]);
        let s2 = sel.select(&dec, &hints_session("s1"), &heavy_a).unwrap();
        assert_eq!(s2.channel.as_str(), "ch-a");
        assert!(s2.sticky_hit, "同一会话第二个请求必须命中粘性");

        // 换会话 → 未命中，且此时 a 最重 → 走 b 或 c
        let s3 = sel.select(&dec, &hints_session("s2"), &heavy_a).unwrap();
        assert!(!s3.sticky_hit);
        assert_ne!(s3.channel.as_str(), "ch-a", "新会话应避开最重的那条");
    }

    /// **题眼**：「这个会话的这个线路报错了，就粘到另一个身上」。
    #[test]
    fn hard_error_releases_sticky_so_the_session_moves_elsewhere() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-err");
        let c = ctx();

        // 会话先粘在 ch-a
        let s1 = sel.select(&decision(&["ch-a", "ch-b"]), &h, &c).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");

        // ch-a 这次被冷却剔除了（= 上游报错）→ 释放粘性、改用 ch-b
        let mut dec = decision(&["ch-b"]);
        dec.eliminated.push((ChannelId::from("ch-a"), EliminateReason::Cooling));
        let s2 = sel.select(&dec, &h, &c).unwrap();
        assert_eq!(s2.channel.as_str(), "ch-b");
        assert_eq!(
            sticky.releases(),
            vec![(StickyKey::from("sess-err"), ReleaseReason::Cooldown)],
            "冷却必须释放粘性"
        );

        // **粘到另一个身上**：下个请求（ch-a 还没回来）仍然走 ch-b，而且是**命中**
        let s3 = sel.select(&decision(&["ch-b"]), &h, &c).unwrap();
        assert_eq!(s3.channel.as_str(), "ch-b");
        assert!(s3.sticky_hit, "释放后应立刻把会话粘到新线路上，而不是回到未命中");
        assert_eq!(
            sticky
                .lookup(&StickyKey::from("sess-err"), &ChannelGroupId::from("grp-gpt-4o"))
                .expect("应有新记录")
                .channel_id
                .as_str(),
            "ch-b",
            "粘性记录必须已指向新线路"
        );
    }

    /// 凭据失效也算硬失败 → 也要释放。
    #[test]
    fn invalid_credential_also_releases_sticky() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-inv");
        let c = ctx();
        sel.select(&decision(&["ch-a", "ch-b"]), &h, &c).unwrap();

        let mut dec = decision(&["ch-b"]);
        dec.eliminated.push((ChannelId::from("ch-a"), EliminateReason::InvalidCredential));
        let s = sel.select(&dec, &h, &c).unwrap();
        assert_eq!(s.channel.as_str(), "ch-b");
        assert_eq!(
            sticky.releases(),
            vec![(StickyKey::from("sess-inv"), ReleaseReason::HardError)]
        );
    }

    #[test]
    fn sticky_survives_policy_only_exclusion_without_release() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-2");
        let c = ctx();
        sel.select(&decision(&["ch-a", "ch-b"]), &h, &c).unwrap();

        // ch-a 因标签被这条规则排除 → 不释放，也**不改写**会话归属（临时绕开而已）
        let mut dec = decision(&["ch-b"]);
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
    fn sticky_expires_by_ttl_and_reselects() {
        let d = doc();
        let clock = Arc::new(crate::clock::ManualClock::new(1_699_920_000_000));
        let sticky = Arc::new(MemoryStickyResolver::with_clock(clock.clone()));
        let sel = selector_with(&d, sticky.clone());
        let h = hints_session("sess-ttl");
        let dec = decision(&["ch-a", "ch-b"]);

        let s1 = sel.select(&dec, &h, &ctx()).unwrap();
        assert_eq!(s1.channel.as_str(), "ch-a");

        // 12h 后过期 → 未命中 → 按 priority 重新选（"复活"回 ch-a）
        clock.advance(12 * 3_600_000 + 1);
        let s2 = sel.select(&dec, &h, &ctx()).unwrap();
        assert!(!s2.sticky_hit);
        assert_eq!(s2.channel.as_str(), "ch-a");
    }

    #[test]
    fn pinned_channel_wins() {
        let d = doc();
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky);
        let mut dec = decision(&["ch-c", "ch-a", "ch-b"]);
        dec.pinned = Some(ChannelId::from("ch-c"));
        let s = sel.select(&dec, &hints_session("s"), &ctx()).unwrap();
        assert_eq!(s.channel.as_str(), "ch-c");
        assert!(!s.sticky_hit);
        assert_eq!(chain_of(&s), vec!["ch-c", "ch-a", "ch-b"]);
    }

    /// 粘性关掉（`sticky.enabled = false`）→ 每次都是"新会话"语义，不写表。
    #[test]
    fn disabled_sticky_means_every_request_is_fresh() {
        let mut d = doc();
        d.channel_groups[0].sticky.enabled = false;
        let sticky = Arc::new(MemoryStickyResolver::memory());
        let sel = selector_with(&d, sticky.clone());
        let dec = decision(&["ch-a", "ch-b"]);
        let s1 = sel.select(&dec, &hints_session("s"), &ctx()).unwrap();
        let s2 = sel.select(&dec, &hints_session("s"), &ctx()).unwrap();
        assert!(!s1.sticky_hit && !s2.sticky_hit);
        assert!(s1.sticky_key.is_none() && s2.sticky_key.is_none());
        assert!(sticky.releases().is_empty());
    }

    #[test]
    fn release_reason_mapping_is_explicit() {
        assert_eq!(release_reason_for(EliminateReason::Cooling), Some(ReleaseReason::Cooldown));
        assert_eq!(
            release_reason_for(EliminateReason::InvalidCredential),
            Some(ReleaseReason::HardError)
        );
        assert_eq!(release_reason_for(EliminateReason::Disabled), Some(ReleaseReason::HardError));
        assert_eq!(release_reason_for(EliminateReason::TaggedOut), None);
        assert_eq!(release_reason_for(EliminateReason::ExcludedByPolicy), None);
        assert_eq!(release_reason_for(EliminateReason::NotInGroup), None);
    }

    #[test]
    fn splitmix64_is_deterministic_and_not_constant() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let xs: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys, "同种子必须同序列（可复现）");
        assert!(xs.windows(2).all(|w| w[0] != w[1]), "不该吐出常数");
    }
}

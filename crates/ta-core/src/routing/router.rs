//! `Router`：把索引 / 策略引擎 / 选择器 / 粘性解析器绑成一份**可复用的路由运行时**。
//!
//! 存在的意义是"只有一条代码路径"：真实网关、托盘、策略模拟器都调用
//! `Router::decide → Router::select`，不存在第二份实现。
//!
//! 状态（粘性表、round_robin 计数）都在 `Router` 里。要可复现的回放
//! （模拟器 / 单测）就用 `Router::from_doc` 每次新建 —— 状态从零开始。

use std::sync::Arc;

use crate::clock::{Clock, SystemClock};
use crate::domain::ConfigDoc;
use crate::pricing::PricingEngine;
use crate::routing::engine::{EngineOptions, PolicyEngineImpl};
use crate::routing::index::RoutingIndex;
use crate::routing::selector::{ChannelSelectorImpl, SelectorOptions};
use crate::routing::{
    ChannelSelector, PolicyEngine, RouteContext, RouteDecision, RouteHints, Selection,
};
use crate::sticky::{MemoryStickyResolver, StickyResolver};

#[derive(Clone)]
pub struct RouterOptions {
    pub engine: EngineOptions,
    pub selector: SelectorOptions,
    /// 注入式时钟：粘性 TTL 与冷却判定都走它
    pub clock: Arc<dyn Clock>,
    /// 可选定价引擎（`cheapest` 选择器需要它；缺失时退化为配置顺序）
    pub pricing: Option<Arc<dyn PricingEngine>>,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            engine: EngineOptions::default(),
            selector: SelectorOptions::default(),
            clock: Arc::new(SystemClock),
            pricing: None,
        }
    }
}

pub struct Router {
    pub index: Arc<RoutingIndex>,
    pub engine: PolicyEngineImpl,
    pub selector: ChannelSelectorImpl,
    /// 进程内粘性表（P4 换成 SQLite 后，这里换 `BackedStickyResolver<SqliteStickyStore>` 即可）
    pub sticky: Arc<MemoryStickyResolver>,
}

impl Router {
    pub fn new(doc: &ConfigDoc, opts: RouterOptions) -> Self {
        let index = Arc::new(RoutingIndex::build(doc));
        let sticky = Arc::new(MemoryStickyResolver::with_clock(opts.clock.clone()));
        let sticky_dyn: Arc<dyn StickyResolver> = sticky.clone();
        let mut selector = ChannelSelectorImpl::new(index.clone())
            .with_sticky(sticky_dyn)
            .with_options(opts.selector);
        if let Some(p) = opts.pricing {
            selector = selector.with_pricing(p);
        }
        let engine = PolicyEngineImpl::with_options(index.clone(), opts.engine);
        Self { index, engine, selector, sticky }
    }

    /// 默认运行时（系统时钟、无定价引擎、默认选项）。
    pub fn from_doc(doc: &ConfigDoc) -> Self {
        Self::new(doc, RouterOptions::default())
    }

    /// 与 `from_doc` 同义，但**语义明确**：给模拟器 / 回放用，状态从零开始。
    pub fn ephemeral(doc: &ConfigDoc) -> Self {
        Self::from_doc(doc)
    }

    pub fn decide(&self, hints: &RouteHints, ctx: &RouteContext) -> RouteDecision {
        self.engine.decide(hints, ctx)
    }

    pub fn select(
        &self,
        decision: &RouteDecision,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> Option<Selection> {
        self.selector.select(decision, hints, ctx)
    }

    /// 完整的一次路由：`decide` + `select`。
    pub fn route(
        &self,
        hints: &RouteHints,
        ctx: &RouteContext,
    ) -> (RouteDecision, Option<Selection>) {
        let decision = self.decide(hints, ctx);
        let selection = self.select(&decision, hints, ctx);
        (decision, selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::domain::{ConfigDoc, ReleaseReason};
    use crate::routing::{ChannelRuntimeState, EliminateReason};
    use crate::types::*;

    const YAML: &str = r#"
version: 1
gateway:
  retry: { max_attempts: 3, stream_max_attempts: 2, cooldown_s: 120 }
net:
  default_lane: lane-direct
lanes:
  - { id: lane-direct, name: 直连（本机网络）, kind: direct }
  - { id: lane-jp, name: 日本 · 自动择优, kind: subscription_auto }
providers:
  - { id: p-openai, name: OpenAI 官方, protocol: openai, base_url: "https://api.openai.com/v1" }
  - { id: p-relay, name: 中转站, protocol: openai, base_url: "https://relay.example/v1" }
credentials:
  - { id: c-openai, provider: p-openai, label: 官方, secret: "${secret:o}", lane: lane-jp }
  - { id: c-relay-a, provider: p-relay, label: 中转A, secret: "${secret:a}", lane: lane-direct }
  - { id: c-relay-b, provider: p-relay, label: 中转B, secret: "${secret:b}", lane: lane-direct }
channels:
  - { id: ch-openai, model: gpt-4o, provider: p-openai, credential: c-openai, upstream_model: gpt-4o-2024-11-20, priority: 10, weight: 100, tags: [official], enabled: true }
  - { id: ch-relay-a, model: gpt-4o, provider: p-relay, credential: c-relay-a, upstream_model: gpt-4o, priority: 20, weight: 50, tags: [cheap], enabled: true }
  - { id: ch-relay-b, model: gpt-4o, provider: p-relay, credential: c-relay-b, upstream_model: gpt-4o, priority: 30, weight: 30, tags: [backup, no-vision], enabled: true }
channel_groups:
  - id: grp-gpt-4o
    model: gpt-4o
    members: [ch-openai, ch-relay-a, ch-relay-b]
    selector: sticky_then_priority
    sticky: { enabled: true, ttl_h: 12, key: [header:x-session-id, body_hash] }
    release_on: [quota_exhausted, hard_error, user_pin_change]
policies:
  - id: pol-skip-nonvision
    name: 带图请求跳过不识图的通道
    enabled: true
    priority: 5
    match: { has_image: true }
    action: { exclude_tag: no-vision, on_no_candidate: error, error_code: 415 }
  - id: pol-default
    name: 默认策略
    enabled: true
    priority: 1000
    match: { all: true }
    action: { use_group: grp-gpt-4o }
"#;

    fn doc() -> ConfigDoc {
        config::from_yaml(YAML).expect("yaml")
    }

    fn hints(session: &str) -> RouteHints {
        RouteHints {
            model: Some("gpt-4o".into()),
            session_id: Some(session.into()),
            est_input_tokens: 1500,
            ..Default::default()
        }
    }

    fn ctx() -> RouteContext {
        RouteContext { now_ms: 1_699_920_000_000, ..Default::default() }
    }

    #[test]
    fn route_is_decide_plus_select() {
        let r = Router::from_doc(&doc());
        let (dec, sel) = r.route(&hints("s"), &ctx());
        let sel2 = r.select(&dec, &hints("s"), &ctx());
        assert_eq!(sel.unwrap().channel, sel2.unwrap().channel);
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-default");
    }

    /// DoD：`sticky_then_priority` 的完整链路
    /// 命中 → 复用 → 额度耗尽释放 → 重选 → **同会话保持新通道**
    #[test]
    fn sticky_full_chain_hit_reuse_release_reselect_stick_to_new() {
        let r = Router::from_doc(&doc());
        let c = ctx();
        let h = hints("sess-abc");

        // 1) 首轮：未命中 → 按 priority 选官方
        let (d1, s1) = r.route(&h, &c);
        let s1 = s1.unwrap();
        assert_eq!(d1.candidates[0].as_str(), "ch-openai");
        assert_eq!(s1.channel.as_str(), "ch-openai");
        assert!(!s1.sticky_hit);
        assert_eq!(s1.sticky_key.as_ref().unwrap().as_str(), "sess-abc");
        assert_eq!(
            s1.chain.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["ch-openai", "ch-relay-a", "ch-relay-b"]
        );

        // 2) 同会话第二轮：粘性命中，复用同一通道
        let (_, s2) = r.route(&h, &c);
        let s2 = s2.unwrap();
        assert!(s2.sticky_hit);
        assert_eq!(s2.channel.as_str(), "ch-openai");

        // 3) 官方通道额度耗尽 → 被剔除（带具体原因），粘性自动释放并重选
        let mut c2 = ctx();
        c2.channel_state.insert(
            ChannelId::from("ch-openai"),
            ChannelRuntimeState { enabled: true, exhausted: true, ..Default::default() },
        );
        let (d3, s3) = r.route(&h, &c2);
        let s3 = s3.unwrap();
        assert!(d3
            .eliminated
            .iter()
            .any(|(c, rr)| c.as_str() == "ch-openai" && *rr == EliminateReason::QuotaExhausted));
        assert_eq!(s3.channel.as_str(), "ch-relay-a");
        assert!(!s3.sticky_hit);
        assert_eq!(
            r.sticky.releases(),
            vec![(StickyKey::from("sess-abc"), ReleaseReason::QuotaExhausted)]
        );

        // 4) 同会话后续请求继续用新通道（这就是"用完换下一个"且不漂移）
        let (_, s4) = r.route(&h, &c2);
        let s4 = s4.unwrap();
        assert!(s4.sticky_hit);
        assert_eq!(s4.channel.as_str(), "ch-relay-a");

        // 5) 官方恢复也不抢回粘性（prompt cache / 额度连贯）
        let (_, s5) = r.route(&h, &c);
        let s5 = s5.unwrap();
        assert!(s5.sticky_hit);
        assert_eq!(s5.channel.as_str(), "ch-relay-a");

        // 6) 新会话仍然从 priority 起点开始
        let (_, s6) = r.route(&hints("other-session"), &c);
        assert_eq!(s6.unwrap().channel.as_str(), "ch-openai");
    }

    #[test]
    fn vision_policy_excludes_tagged_channel_and_keeps_default_untagged() {
        let r = Router::from_doc(&doc());
        let mut h = hints("sess-img");
        h.has_image = true;
        let (dec, sel) = r.route(&h, &ctx());
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-skip-nonvision");
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, rr)| c.as_str() == "ch-relay-b" && *rr == EliminateReason::TaggedOut));
        assert_eq!(sel.unwrap().channel.as_str(), "ch-openai");

        // 未标注的通道（ch-relay-a 没有 vision 标签）不会被剔除 —— 默认是"照发"
        assert!(dec.candidates.contains(&ChannelId::from("ch-relay-a")));
    }

    #[test]
    fn ephemeral_routers_do_not_share_state() {
        let d = doc();
        let a = Router::from_doc(&d);
        let b = Router::from_doc(&d);
        a.route(&hints("s"), &ctx());
        let (_, s) = b.route(&hints("s"), &ctx());
        assert!(!s.unwrap().sticky_hit, "两个 Router 不应共享粘性状态");
    }

    #[test]
    fn fresh_config_swap_does_not_leak_old_channels() {
        let r = Router::from_doc(&doc());
        r.route(&hints("s"), &ctx());
        let other = Router::from_doc(&config::from_yaml("version: 1\n").unwrap());
        let (dec, sel) = other.route(&hints("s"), &ctx());
        assert!(dec.candidates.is_empty());
        assert!(sel.is_none());
    }
}

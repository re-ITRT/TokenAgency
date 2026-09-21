//! `Router`：把索引 / 策略引擎 / 选择器 / 粘性解析器绑成一份**可复用的路由运行时**。
//!
//! 存在的意义是"只有一条代码路径"：真实网关、托盘、策略模拟器都调用
//! `Router::decide → Router::select`，不存在第二份实现。
//!
//! **装配也只有一份**：index / engine / selector 的构造只在
//! [`Router::with_sticky`] 里写一次；[`Router::new`] 是它自造粘性表的特例。
//! 网关的 `LiveRouter` 配置换代时**必须**调 `with_sticky` 注入同一份粘性表，
//! 而不是自己再拼一遍那三件套 —— 否则"策略模拟器 = 真实决策"就会变成假话
//! （详见 `with_sticky` 的文档）。
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
    /// 注入式时钟。**只用于粘性 TTL**（[`Router::new`] 造内部粘性表时用）。
    /// 冷却判定**不经过它** —— 那读的是 `RouteContext.channel_state`（由调用方填）。
    /// 外部注入 sticky（[`Router::with_sticky`]）时本字段不参与装配：那份解析器自带时钟。
    ///
    /// **什么时候该拆这个 `opts`**（评审 E1 提的"一个参数在一条路径上没被用"的味道）：
    /// 目前只有一个字段这样，代价小于"与 `new` 不同形"带来的调用点复杂度；
    /// **等出现第二个"只在某一路径生效"的字段时再拆 options**，一个字段不值得拆。
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
    /// 生效的粘性表。类型是 `dyn`：它**可能是外部注入的那一份**
    /// （[`Router::with_sticky`]，网关跨配置换代就靠这个），也可能是 [`Router::new`] 自造的。
    pub sticky: Arc<dyn StickyResolver>,
}

impl Router {
    /// ★ **唯一的装配实现**：从一份配置绑出 index / engine / selector，并接上调用方给的粘性表。
    ///
    /// 为什么粘性表必须能**从外面给**（而不是永远自造一份）：
    /// 粘性对用户是 **12 小时级**的可见语义，配置热更新（重建 index/engine/selector）
    /// **不能**把它清零。`LiveRouter` 每次换代都要重建前三件套，但必须注入**同一份**粘性表。
    /// 如果这里不接受外部 sticky，网关就只能自己再拼一遍这套装配 —— 于是出现两份
    /// **手工同步**的装配：任何一侧加了新部件两边都不会红，而"策略模拟器 = 真实决策"
    /// 这个论断会**静默变成假话**（[docs/13 §4.5 L21] 那类"含糊处长出两套系统"）。
    ///
    /// `opts.clock` 只在 [`Router::new`] 造内部粘性表时用到；外部注入时那份解析器自带时钟。
    pub fn with_sticky(
        doc: &ConfigDoc,
        opts: RouterOptions,
        sticky: Arc<dyn StickyResolver>,
    ) -> Self {
        let index = Arc::new(RoutingIndex::build(doc));
        let mut selector = ChannelSelectorImpl::new(index.clone())
            .with_sticky(sticky.clone())
            .with_options(opts.selector);
        if let Some(p) = opts.pricing {
            selector = selector.with_pricing(p);
        }
        let engine = PolicyEngineImpl::with_options(index.clone(), opts.engine);
        Self { index, engine, selector, sticky }
    }

    /// 自造粘性表的便利构造（进程内、状态从零开始）。**它是 [`Router::with_sticky`] 的特例** ——
    /// 两条路径共用同一段装配代码，这就是"只有一条代码路径"的字面落实。
    pub fn new(doc: &ConfigDoc, opts: RouterOptions) -> Self {
        let sticky: Arc<dyn StickyResolver> =
            Arc::new(MemoryStickyResolver::with_clock(opts.clock.clone()));
        Self::with_sticky(doc, opts, sticky)
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
        // 用**注入**的粘性表（不是自造的那份）：这样 `releases()` 审计是对着
        // Router 真正在用的那个对象断言的 —— 断言的对象与装配的对象是同一个事实。
        let sticky = Arc::new(MemoryStickyResolver::with_clock(Arc::new(SystemClock)));
        let r = Router::with_sticky(&doc(), RouterOptions::default(), sticky.clone());
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
            sticky.releases(),
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

    // ------------------------------------------------ 装配只有一条路径（E5 的 bind 依赖它）

    /// **两条路径合一**：`Router::new` 必须就是 `with_sticky` + 自造解析器的特例。
    ///
    /// 比较方式分两层，**缺一不可**：
    /// 1. `Debug` 投影比较（辅助证据）—— 结构体自己的投影，将来新增字段自动纳入，
    ///    手写字段清单会漏。**但它不是契约**：哪天有人给这些类型手写/精简 `Debug`
    ///    （例如为了日志只打摘要），两个**不同**的值会比出相等，这条断言就会**静默失效**。
    /// 2. 显式字段断言（主证据）—— 把对用户可见的语义钉死，牙齿不依赖 `Debug` 的实现。
    ///
    /// **选项必须是非默认值**（评审 E1 之后补的红-绿教训）：全用 `default()` 时，
    /// "某条路径漏注入一个部件"根本看不出来 —— 漏掉 `.with_options` 会悄悄退回
    /// `sticky_write = true`，而默认值恰好也是 `true`。夹具因此显式设
    /// `sticky_write = false` 并**每会话跑两轮**：第二轮"不命中"就是这条漏注入的探针。
    #[test]
    fn new_is_the_self_resolver_special_case_of_with_sticky() {
        let d = doc();
        let opts = || RouterOptions {
            selector: SelectorOptions { default_ttl_ms: 7 * 3_600_000, sticky_write: false },
            ..RouterOptions::default()
        };
        let a = Router::new(&d, opts());
        let b = Router::with_sticky(
            &d,
            opts(),
            Arc::new(MemoryStickyResolver::with_clock(Arc::new(SystemClock))),
        );
        for session in ["s-1", "s-2", "s-3"] {
            for round in 1..=2 {
                let (da, sa) = a.route(&hints(session), &ctx());
                let (db, sb) = b.route(&hints(session), &ctx());
                // ---- 先自检夹具前提：`sticky_write = false` 时同会话第二轮不该命中。
                // 谁漏了这个选项，这里会先红，并直接指出原因（比"投影不同"可读）。
                assert_eq!(
                    sa.as_ref().map(|s| s.sticky_hit),
                    Some(false),
                    "第 {round} 轮：sticky_write=false 时不该命中粘性（session={session}）"
                );

                // ---- 辅助：整体投影
                assert_eq!(
                    format!("{da:?}"),
                    format!("{db:?}"),
                    "两次装配的 decide 投影必须相同（session={session} round={round}）"
                );
                assert_eq!(
                    sa.as_ref().map(|s| format!("{s:?}")),
                    sb.as_ref().map(|s| format!("{s:?}")),
                    "两次装配的 select 投影必须相同（session={session} round={round}）"
                );

                // ---- 主证据：显式字段（不依赖 Debug 实现）
                assert_eq!(da.candidates, db.candidates, "候选集不同（session={session}）");
                assert_eq!(
                    da.matched_policy.as_ref().map(|p| p.as_str()),
                    db.matched_policy.as_ref().map(|p| p.as_str()),
                    "命中的策略不同（session={session}）"
                );
                assert_eq!(da.eliminated, db.eliminated, "淘汰集与原因不同（session={session}）");
                assert_eq!(da.pinned, db.pinned, "pin 不同（session={session}）");
                assert_eq!(da.lane_override, db.lane_override, "线路覆盖不同（session={session}）");
                assert_eq!(da.error_code, db.error_code, "兜底状态码不同（session={session}）");

                assert_eq!(
                    sa.is_some(),
                    sb.is_some(),
                    "一次选出来了、另一次没有（session={session}）"
                );
                if let (Some(sa), Some(sb)) = (&sa, &sb) {
                    assert_eq!(sa.channel, sb.channel, "选中通道不同（session={session}）");
                    assert_eq!(
                        sa.sticky_hit, sb.sticky_hit,
                        "粘性命中判定不同（session={session}）"
                    );
                    assert_eq!(sa.sticky_key, sb.sticky_key, "粘性键不同（session={session}）");
                    assert_eq!(sa.chain, sb.chain, "失败转移链不同（session={session}）");
                }
            }
        }
    }

    /// [`Router::with_sticky`] **必须真的用注入的那一份**，而不是偷偷自造。
    ///
    /// 这条是 E5 的 `LiveRouter::bind` 的前提：换代时重建 index/engine/selector，
    /// 但粘性表要跨代保留。做法：先往注入的解析器里写一条"上一代留下的粘性"，
    /// 再拿一个**从没跑过请求的** Router 首轮求值 —— 命中就证明它用的是注入的那份。
    #[test]
    fn with_sticky_uses_the_injected_resolver() {
        let sticky = Arc::new(MemoryStickyResolver::with_clock(Arc::new(SystemClock)));
        let injected: Arc<dyn StickyResolver> = sticky.clone();
        // 模拟"换代之前那次请求留下的记录"（组与通道都取自夹具配置）
        injected.store(
            StickyKey::from("sess-injected"),
            ChannelGroupId::from("grp-gpt-4o"),
            ChannelId::from("ch-relay-b"),
            12 * 3_600_000,
        );

        let r = Router::with_sticky(&doc(), RouterOptions::default(), injected);
        let (_, s) = r.route(&hints("sess-injected"), &ctx());
        let s = s.unwrap();
        assert!(s.sticky_hit, "注入的粘性表必须生效，否则配置换代会把 12h 粘性清零");
        assert_eq!(s.channel.as_str(), "ch-relay-b", "命中的必须是注入记录里的那个通道");

        // 反向：没注入那一份的 Router 不该看到它（证明上面命中的确是"注入"带来的）
        let fresh = Router::from_doc(&doc());
        let (_, s_fresh) = fresh.route(&hints("sess-injected"), &ctx());
        assert!(!s_fresh.unwrap().sticky_hit, "另一份粘性表不该共享状态");
    }

    /// 跨**配置代次**保留粘性：同一份 sticky + 两次 `with_sticky`（两个不同配置代）。
    ///
    /// 这就是"配置换代重建 index/engine/selector，但粘性不清零"的字面测试。
    #[test]
    fn sticky_survives_a_config_generation_swap() {
        let sticky: Arc<dyn StickyResolver> =
            Arc::new(MemoryStickyResolver::with_clock(Arc::new(SystemClock)));

        let gen1 = Router::with_sticky(&doc(), RouterOptions::default(), sticky.clone());
        let (_, s1) = gen1.route(&hints("sess-gen"), &ctx());
        assert_eq!(s1.unwrap().channel.as_str(), "ch-openai");

        // 换代：换一份配置（结构相同即可 —— 关键是新建了 index/engine/selector）
        let gen2 = Router::with_sticky(&doc(), RouterOptions::default(), sticky.clone());
        let (_, s2) = gen2.route(&hints("sess-gen"), &ctx());
        let s2 = s2.unwrap();
        assert!(s2.sticky_hit, "换代不许把粘性丢掉（对用户是 12h 的可见语义）");
        assert_eq!(s2.channel.as_str(), "ch-openai");
        assert_eq!(s2.sticky_key.as_ref().unwrap().as_str(), "sess-gen");
    }
}

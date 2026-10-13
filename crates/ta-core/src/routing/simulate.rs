//! 策略模拟器：**"如果现在来这样一个请求，会怎么走？"**
//!
//! 这是 UI 的杀手功能：用户改一条规则之前能立刻看到后果。
//!
//! 两条硬要求：
//! 1. **纯函数**：`simulate(doc, hints, ctx)` 不依赖任何外部可变状态；
//! 2. **与真实决策同一条代码路径**：内部就是 `Router::decide` + `Router::select`，
//!    绝不另写一份"看起来差不多"的逻辑。
//!
//! 为了保证这两点，`simulate` 每次都用 `Router::ephemeral` 新建一份运行时状态
//! （粘性表、round_robin 计数都从零开始），因此**同一输入跑多少次结果都一致**。
//! 想看到"当前真实粘性"时用 `simulate_with(&live_router, ...)`。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::clock::{Clock, SystemClock};
use crate::domain::ConfigDoc;
use crate::routing::router::Router;
use crate::routing::{ChannelRuntimeState, RouteContext, RouteDecision, RouteHints, Selection};
use crate::types::*;

// ================================================================ 输入

/// `policy_simulate` 的入参（`SimulateInput`）。
///
/// 后四个字段是对 TS 契约的**扩展**（TS 里没有）：把"当前运行态"也喂进来，
/// 模拟器才能回答"官方这条正在冷却时会走谁"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SimulateInput {
    pub model: String,
    #[serde(default)]
    pub has_image: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub est_input_tokens: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub session_key: Option<String>,
    /// 毫秒时间戳；不传则用当前时间（`time_window` 会读它）
    #[serde(default)]
    pub now: Option<i64>,
    /// 各线路在时间窗内的 token 用量。空 = 不知道 → 按 priority 排。
    #[serde(default)]
    pub balance: BTreeMap<String, i64>,
    #[serde(default)]
    pub latency_ms: BTreeMap<String, u32>,
    #[serde(default)]
    pub channel_state: BTreeMap<String, ChannelRuntimeState>,
    #[serde(default)]
    pub lane_health: BTreeMap<String, bool>,
}

impl SimulateInput {
    pub fn to_hints(&self) -> RouteHints {
        RouteHints {
            model: Some(self.model.clone()),
            has_image: self.has_image,
            stream: self.stream,
            est_input_tokens: self.est_input_tokens,
            ua: header_lookup_raw(&self.headers, "user-agent").map(|s| s.to_string()),
            headers: self.headers.clone(),
            session_id: self.session_key.clone(),
            ..Default::default()
        }
    }

    pub fn to_context(&self) -> RouteContext {
        RouteContext {
            now_ms: self.now.unwrap_or_else(|| SystemClock.now_ms()),
            channel_state: self
                .channel_state
                .iter()
                .map(|(k, v)| (ChannelId::from(k.as_str()), v.clone()))
                .collect(),
            latency_ms: self
                .latency_ms
                .iter()
                .map(|(k, v)| (ChannelId::from(k.as_str()), *v))
                .collect(),
            lane_health: self
                .lane_health
                .iter()
                .map(|(k, v)| (LaneId::from(k.as_str()), *v))
                .collect(),
            concurrency: BTreeMap::new(),
            // 模拟器默认**不知道**用量（空表 → 退化为按 priority）。
            // 想模拟"平均粘性"的效果，调用方要在 `SimulateInput.balance` 里给出用量。
            balance: self.balance.iter().map(|(k, v)| (ChannelId::from(k.as_str()), *v)).collect(),
        }
    }
}

fn header_lookup_raw<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    if let Some(v) = headers.get(name) {
        return Some(v.as_str());
    }
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

// ================================================================ 输出

/// `policy_simulate` 的返回（严格对应  的 `SimulateResult` TS 定义）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulateResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_policy: Option<SimulatePolicyRef>,
    /// 被考虑过的通道（有序），`ok=false` 的带淘汰原因
    pub candidates: Vec<SimulateCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen: Option<SimulateChosen>,
    /// 有序失败转移链
    pub failover_chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<SimulateLane>,
    /// 恒为 true：除了 model 与鉴权头，发出的请求与客户端逐字段一致（全透传）
    pub outgoing_equals_client: bool,
    pub eliminated: Vec<SimulateEliminated>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatePolicyRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulateCandidate {
    pub channel_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulateChosen {
    pub channel_id: String,
    pub sticky_hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sticky_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulateLane {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulateEliminated {
    pub channel_id: String,
    pub reason: String,
}

// ================================================================ 模拟

/// 纯函数：**每次新建运行时状态**，因此同一输入必然得到同一结果。
pub fn simulate(doc: &ConfigDoc, hints: &RouteHints, ctx: &RouteContext) -> SimulateResult {
    simulate_with(&Router::ephemeral(doc), hints, ctx)
}

/// 用给定的（可能带真实运行态的）`Router` 跑一次模拟。
pub fn simulate_with(router: &Router, hints: &RouteHints, ctx: &RouteContext) -> SimulateResult {
    let decision = router.decide(hints, ctx);
    let selection = router.select(&decision, hints, ctx);
    build_result(router, hints, ctx, &decision, selection.as_ref())
}

/// `SimulateInput` → `SimulateResult`（IPC 直接用这个）。
pub fn simulate_input(doc: &ConfigDoc, input: &SimulateInput) -> SimulateResult {
    simulate_with(&Router::ephemeral(doc), &input.to_hints(), &input.to_context())
}

fn build_result(
    router: &Router,
    hints: &RouteHints,
    ctx: &RouteContext,
    decision: &RouteDecision,
    selection: Option<&Selection>,
) -> SimulateResult {
    let eliminated_map: BTreeMap<&ChannelId, &crate::routing::EliminateReason> =
        decision.eliminated.iter().map(|(c, r)| (c, r)).collect();

    // 展示顺序 = 组内顺序 → 配置顺序 → 其它（点名进来的）通道
    let mut seen = std::collections::BTreeSet::new();
    let mut order: Vec<ChannelId> = vec![];
    let push = |id: ChannelId,
                order: &mut Vec<ChannelId>,
                seen: &mut std::collections::BTreeSet<ChannelId>| {
        let considered = decision.candidates.contains(&id) || eliminated_map.contains_key(&id);
        if considered && seen.insert(id.clone()) {
            order.push(id);
        }
    };
    if let Some(g) = router.selector.resolve_group(decision, hints) {
        for m in &g.members {
            push(m.clone(), &mut order, &mut seen);
        }
    }
    for id in router.engine.considered_channels(hints) {
        push(id, &mut order, &mut seen);
    }
    for id in decision.candidates.iter().chain(decision.eliminated.iter().map(|(c, _)| c)) {
        push(id.clone(), &mut order, &mut seen);
    }

    let candidates: Vec<SimulateCandidate> = order
        .iter()
        .map(|id| {
            let reason = eliminated_map.get(id);
            SimulateCandidate {
                channel_id: id.to_string(),
                ok: reason.is_none(),
                reason: reason.map(|r| r.as_str().to_string()),
            }
        })
        .collect();

    let chosen = selection.map(|s| SimulateChosen {
        channel_id: s.channel.to_string(),
        sticky_hit: s.sticky_hit,
        sticky_key: s.sticky_key.as_ref().map(|k| k.to_string()),
    });

    let failover_chain: Vec<String> =
        selection.map(|s| s.chain.iter().map(|c| c.to_string()).collect()).unwrap_or_default();

    // 预计出口：生效 lane + 该通道的 EWMA 延迟（节点是内核运行态，纯函数拿不到）
    let lane = selection.and_then(|s| {
        let ch = router.index.channel(&s.channel)?;
        //：线路只看服务商那一个节点（策略级覆盖已随 `lane_override` 一起删除）
        let lane_id = router.index.resolve_lane(ch)?;
        let name = router
            .index
            .lanes
            .get(&lane_id)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| lane_id.to_string());
        Some(SimulateLane {
            id: lane_id.to_string(),
            name,
            node: None,
            latency_ms: ctx.latency_ms.get(&s.channel).copied(),
        })
    });

    let matched_policy = decision.matched_policy.as_ref().map(|id| {
        let name = router
            .index
            .policies
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string());
        SimulatePolicyRef { id: id.to_string(), name }
    });

    SimulateResult {
        matched_policy,
        candidates,
        chosen,
        failover_chain,
        lane,
        outgoing_equals_client: true,
        eliminated: decision
            .eliminated
            .iter()
            .map(|(c, r)| SimulateEliminated {
                channel_id: c.to_string(),
                reason: r.as_str().to_string(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    const YAML: &str = r#"
version: 1
net:
  default_lane: lane-jp
lanes:
  - { id: lane-jp, name: 日本 · 自动择优, kind: subscription_auto }
  - { id: lane-direct, name: 直连（本机网络）, kind: direct }
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
policies:
  - id: pol-skip-nonvision
    name: 带图请求跳过不识图的通道
    enabled: true
    priority: 5
    match: { has_image: true }
    action: { exclude_tag: no-vision }
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

    fn hints(has_image: bool) -> RouteHints {
        RouteHints {
            model: Some("gpt-4o".into()),
            has_image,
            stream: true,
            est_input_tokens: 1500,
            session_id: Some("abc".into()),
            headers: [("x-session-id".to_string(), "abc".to_string())].into_iter().collect(),
            ..Default::default()
        }
    }

    fn ctx() -> RouteContext {
        let mut c = RouteContext { now_ms: 1_699_920_000_000, ..Default::default() };
        c.latency_ms.insert(ChannelId::from("ch-openai"), 82);
        c
    }

    #[test]
    fn simulate_matches_docs_example_shape() {
        let r = simulate(&doc(), &hints(true), &ctx());
        assert_eq!(
            r.matched_policy,
            Some(SimulatePolicyRef {
                id: "pol-skip-nonvision".into(),
                name: "带图请求跳过不识图的通道".into()
            })
        );
        assert_eq!(
            r.candidates,
            vec![
                SimulateCandidate { channel_id: "ch-openai".into(), ok: true, reason: None },
                SimulateCandidate { channel_id: "ch-relay-a".into(), ok: true, reason: None },
                SimulateCandidate {
                    channel_id: "ch-relay-b".into(),
                    ok: false,
                    reason: Some("tagged_out".into())
                },
            ]
        );
        assert_eq!(
            r.chosen,
            Some(SimulateChosen {
                channel_id: "ch-openai".into(),
                sticky_hit: false,
                sticky_key: Some("abc".into())
            })
        );
        assert_eq!(r.failover_chain, vec!["ch-openai", "ch-relay-a"]);
        assert_eq!(
            r.lane,
            Some(SimulateLane {
                id: "lane-jp".into(),
                name: "日本 · 自动择优".into(),
                node: None,
                latency_ms: Some(82)
            })
        );
        assert!(r.outgoing_equals_client);
        assert_eq!(
            r.eliminated,
            vec![SimulateEliminated {
                channel_id: "ch-relay-b".into(),
                reason: "tagged_out".into()
            }]
        );
    }

    #[test]
    fn simulate_is_deterministic_over_100_runs() {
        let d = doc();
        let h = hints(true);
        let c = ctx();
        let first = simulate(&d, &h, &c);
        for i in 0..100 {
            assert_eq!(simulate(&d, &h, &c), first, "第 {i} 次模拟结果与第 1 次不一致");
        }
    }

    #[test]
    fn simulate_stays_deterministic_for_randomized_selectors() {
        // weighted_random / round_robin 也必须可复现
        let yaml = YAML.replace("selector: sticky_then_priority", "selector: weighted_random");
        let d = config::from_yaml(&yaml).unwrap();
        let first = simulate(&d, &hints(false), &ctx());
        for _ in 0..100 {
            assert_eq!(simulate(&d, &hints(false), &ctx()), first);
        }

        let yaml2 = YAML.replace("selector: sticky_then_priority", "selector: round_robin");
        let d2 = config::from_yaml(&yaml2).unwrap();
        let first2 = simulate(&d2, &hints(false), &ctx());
        for _ in 0..100 {
            assert_eq!(simulate(&d2, &hints(false), &ctx()), first2);
        }
    }

    #[test]
    fn simulate_uses_the_same_code_path_as_decide_and_select() {
        let d = doc();
        let h = hints(true);
        let c = ctx();

        // 同一个 Router，但关掉粘性写回 → 每一次调用都是"只读对照"
        let opts = crate::routing::router::RouterOptions {
            selector: crate::routing::selector::SelectorOptions {
                sticky_write: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let router = Router::new(&d, opts);
        let decision = router.decide(&h, &c);
        let selection = router.select(&decision, &h, &c).unwrap();
        let manual = build_result(&router, &h, &c, &decision, Some(&selection));

        // 同一输入 → 模拟器输出与 decide + select 完全一致
        assert_eq!(simulate(&d, &h, &c), manual);
        assert_eq!(simulate_with(&router, &h, &c), manual);

        // 与原始输出的逐项对齐
        assert_eq!(manual.chosen.as_ref().unwrap().channel_id, selection.channel.to_string());
        assert_eq!(manual.failover_chain.len(), selection.chain.len());
        assert_eq!(manual.eliminated.len(), decision.eliminated.len());
        assert_eq!(manual.candidates.len(), decision.candidates.len() + decision.eliminated.len());
    }

    #[test]
    fn simulate_shows_live_sticky_hit_when_given_live_router() {
        let d = doc();
        let h = hints(false);
        let c = ctx();
        let router = Router::from_doc(&d);

        // 真实链路先跑一次 → 粘性表里有记录
        let (_, s1) = router.route(&h, &c);
        assert_eq!(s1.unwrap().channel.as_str(), "ch-openai");

        let sim = simulate_with(&router, &h, &c);
        assert_eq!(
            sim.chosen,
            Some(SimulateChosen {
                channel_id: "ch-openai".into(),
                sticky_hit: true,
                sticky_key: Some("abc".into())
            })
        );

        // 纯函数版本没有共享状态 → 永远是"冷启动"视角
        assert!(!simulate(&d, &h, &c).chosen.unwrap().sticky_hit);
    }

    #[test]
    fn simulate_input_maps_ts_fields() {
        let input = SimulateInput {
            model: "gpt-4o".into(),
            has_image: true,
            stream: true,
            est_input_tokens: 1500,
            headers: [("x-session-id".to_string(), "abc".to_string())].into_iter().collect(),
            session_key: Some("abc".into()),
            now: Some(1_699_920_000_000),
            ..Default::default()
        };
        let r = simulate_input(&doc(), &input);
        assert_eq!(r.chosen.unwrap().sticky_key.as_deref(), Some("abc"));

        // 运行态注入：官方冷却 → 换通道，并给出原因
        let mut input2 = input.clone();
        input2.channel_state.insert(
            "ch-openai".into(),
            ChannelRuntimeState {
                enabled: true,
                cooling_until_ms: Some(1_699_920_000_000 + 60_000),
                ..Default::default()
            },
        );
        let r2 = simulate_input(&doc(), &input2);
        assert_eq!(r2.chosen.unwrap().channel_id, "ch-relay-a");
        assert_eq!(
            r2.eliminated,
            vec![
                SimulateEliminated { channel_id: "ch-openai".into(), reason: "cooling".into() },
                SimulateEliminated { channel_id: "ch-relay-b".into(), reason: "tagged_out".into() },
            ]
        );
    }

    #[test]
    fn simulate_result_serializes_with_ts_field_names() {
        let r = simulate(&doc(), &hints(true), &ctx());
        let v = serde_json::to_value(&r).unwrap();
        for key in [
            "matched_policy",
            "candidates",
            "chosen",
            "failover_chain",
            "lane",
            "outgoing_equals_client",
            "eliminated",
        ] {
            assert!(v.get(key).is_some(), "SimulateResult 缺少字段 {key}");
        }
        assert_eq!(v["chosen"]["sticky_hit"], serde_json::json!(false));
        assert_eq!(v["chosen"]["channel_id"], serde_json::json!("ch-openai"));
        assert_eq!(v["failover_chain"][0], serde_json::json!("ch-openai"));
        assert_eq!(v["candidates"][2]["reason"], serde_json::json!("tagged_out"));
        // 反序列化也要能回去（前端来回传）
        let back: SimulateResult = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn simulate_handles_no_candidate() {
        let yaml = format!("{YAML}\n").replace("enabled: true", "enabled: false");
        let d = config::from_yaml(&yaml).unwrap();
        let r = simulate(&d, &hints(false), &ctx());
        assert!(r.chosen.is_none());
        assert!(r.failover_chain.is_empty());
        assert_eq!(r.candidates.len(), 3);
        assert!(r.candidates.iter().all(|c| !c.ok));
        assert!(r.candidates.iter().all(|c| c.reason.as_deref() == Some("disabled")));
    }
}

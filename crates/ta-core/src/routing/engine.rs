//! 策略求值：**有序规则表，首个命中即生效，不做叠加。**
//!
//! 求值流程（见 `docs/03-routing-and-policy.md` §2）：
//!
//! ```text
//! for policy in enabled policies, priority asc:
//!     if policy.matches(hints, ctx): return apply(policy)
//! return builtin_default
//! ```
//!
//! 唯一允许"叠加"的是**单条规则内部**的收窄动作：
//! `exclude_channels` / `require_tag` / `exclude_tag` 组合生效。
//!
//! 另一个硬约束（`docs/03` §6.1）：**默认不过滤**。标签只有在某条规则显式写了
//! `require_tag` / `exclude_tag` 时才参与淘汰；"未标注"的通道永远照发。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::clock::{Clock, SystemClock};
use crate::domain::*;
use crate::pricing::glob_match;
use crate::routing::index::RoutingIndex;
use crate::routing::{EliminateReason, PolicyEngine, RouteContext, RouteDecision, RouteHints};
use crate::types::*;

/// 引擎的可调项。**不放进 `RouteContext`**，因为它们是"部署/环境"属性而不是请求属性。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// 本地时区偏移（分钟）。`time_window` 用它把 `ctx.now_ms` 换算成本地时间。
    /// 默认取进程构造时的本地偏移；测试里应显式设定，保证确定性。
    pub local_offset_minutes: i32,
    /// lane 引用了不存在的 lane id 时是否判 `NoLane`。
    /// P1 的校验器保证引用完整；手工构造的最小 doc 可以关掉。
    pub strict_lane_check: bool,
    /// `on_no_candidate: error` 且策略没写 `error_code` 时用的兜底 HTTP 状态码。
    ///
    /// 默认 415：`docs/11` §5 的错误表里 `TA-2003 tokenagency_no_viable_channel` 对应 HTTP 415，
    /// `config.example.yaml` 的 `pol-skip-nonvision` 也是写的 `error_code: 415` ——
    /// 因此 `error_code` 字段语义是**HTTP 状态码**，不是 TA-xxxx 里的数字。
    pub no_viable_channel_status: u16,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            local_offset_minutes: SystemClock.local_offset_minutes(),
            strict_lane_check: true,
            no_viable_channel_status: 415,
        }
    }
}

/// `PolicyEngine` 的默认实现。
pub struct PolicyEngineImpl {
    index: Arc<RoutingIndex>,
    opts: EngineOptions,
}

impl PolicyEngineImpl {
    pub fn new(index: Arc<RoutingIndex>) -> Self {
        Self { index, opts: EngineOptions::default() }
    }

    pub fn with_options(index: Arc<RoutingIndex>, opts: EngineOptions) -> Self {
        Self { index, opts }
    }

    pub fn index(&self) -> &Arc<RoutingIndex> {
        &self.index
    }

    pub fn options(&self) -> &EngineOptions {
        &self.opts
    }

    /// 该请求在配置层面会考虑的全部通道（用于 UI/模拟器解释"为什么是这几个"）。
    /// 只读、纯展示用途：真正的候选判定在 `decide` 里。
    pub fn considered_channels(&self, hints: &RouteHints) -> Vec<ChannelId> {
        match hints.model.as_deref() {
            Some(m) => self.index.all_channels_of_model(&self.index.canonical_model(m)),
            None => self.index.channel_order.clone(),
        }
    }
}

impl PolicyEngine for PolicyEngineImpl {
    fn decide(&self, hints: &RouteHints, ctx: &RouteContext) -> RouteDecision {
        self.decide_inner(hints, ctx)
    }
}

/// 一次求值中间态：宇宙 = 有序的候选范围。
struct Verdict {
    id: ChannelId,
    reason: Option<EliminateReason>,
}

impl PolicyEngineImpl {
    fn decide_inner(&self, hints: &RouteHints, ctx: &RouteContext) -> RouteDecision {
        let idx = &self.index;
        let canonical = hints.model.as_deref().map(|m| idx.canonical_model(m));

        // 1. 该模型在配置层面的全部通道（含未启用 → 会带原因进 eliminated，便于解释）
        let model_scope: Vec<ChannelId> = match &canonical {
            Some(m) => idx.all_channels_of_model(m),
            None => idx.channel_order.clone(),
        };

        // 2. 首个命中的规则生效
        let matched = idx.policies.iter().find(|p| {
            policy_matches(
                &p.r#match,
                hints,
                ctx,
                &model_scope,
                idx,
                self.opts.local_offset_minutes,
            )
        });
        let action = matched.map(|p| &p.action);

        // 3. 生效的通道组：`action.use_group` > 模型自带的组
        let group_id = self.effective_group(action, canonical.as_deref());

        // 4. 候选宇宙（use_channels 覆盖 > 组的成员 > 模型全部通道）
        let (universe, not_in_group) = self.build_universe(&model_scope, group_id.as_ref(), action);

        // 5. 逐个判定，带**具体原因**
        let mut verdicts: Vec<Verdict> = universe
            .iter()
            .map(|id| Verdict { id: id.clone(), reason: self.eliminate_reason(id, action, ctx) })
            .collect();

        // 6. on_no_candidate: use_any → 把"被策略排除"的通道放回来（运行态不可用仍不放回）
        if !verdicts.iter().any(|v| v.reason.is_none())
            && action.map(|a| a.on_no_candidate) == Some(NoCandidateAction::UseAny)
        {
            for v in verdicts.iter_mut() {
                if matches!(
                    v.reason,
                    Some(EliminateReason::TaggedOut) | Some(EliminateReason::ExcludedByPolicy)
                ) {
                    v.reason = None;
                }
            }
        }

        let mut candidates: Vec<ChannelId> = vec![];
        let mut eliminated: Vec<(ChannelId, EliminateReason)> = vec![];
        for v in verdicts {
            match v.reason {
                Some(r) => eliminated.push((v.id, r)),
                None => candidates.push(v.id),
            }
        }
        eliminated.extend(not_in_group);

        // 7. pin_sticky：用户显式钉住一个通道 → 强制进候选（运行态不可用则忽略）
        let pinned = action.and_then(|a| a.pin_sticky.clone());
        // [docs/55] `action` 不再传进来：`apply_pin` 只用它读 `use_lane`，那个字段已删除
        let pinned = self.apply_pin(pinned, ctx, &mut candidates, &mut eliminated);

        // 选路策略现在只有一种（[docs/48]）：不再由策略覆盖，也不再由组逐字段指定 ——
        // 全部落到 `BalancedSticky`。这里不再读 `action.selector`（该字段已删除），
        // `group.selector` 仍然读出来只是为了**如实反映配置**（老配置里可能是历史取值，
        // 反序列化时已统一成 `BalancedSticky`）。
        let selector =
            group_id.as_ref().and_then(|g| idx.group(g)).map(|g| g.selector).unwrap_or_default();

        let on_no_candidate = action.map(|a| a.on_no_candidate).unwrap_or_default();
        let error_code = action.and_then(|a| a.error_code).or_else(|| {
            if candidates.is_empty() && on_no_candidate == NoCandidateAction::Error {
                Some(self.opts.no_viable_channel_status)
            } else {
                None
            }
        });

        RouteDecision {
            candidates,
            selector,
            failover: action
                .and_then(|a| a.failover.clone())
                .unwrap_or_else(|| self.builtin_failover(hints)),
            on_no_candidate,
            error_code,
            matched_policy: matched.map(|p| p.id.clone()),
            pinned,
            eliminated,
        }
    }

    fn effective_group(
        &self,
        action: Option<&PolicyAction>,
        canonical: Option<&str>,
    ) -> Option<ChannelGroupId> {
        if let Some(g) = action.and_then(|a| a.use_group.as_ref()) {
            if self.index.groups.contains_key(g) {
                return Some(g.clone());
            }
            tracing::warn!(group = %g, "策略引用了不存在的通道组，退回模型默认组");
        }
        canonical.and_then(|m| self.index.group_of_model.get(m).cloned())
    }

    /// 返回 `(有序宇宙, 组外被剔除的通道)`。
    fn build_universe(
        &self,
        model_scope: &[ChannelId],
        group: Option<&ChannelGroupId>,
        action: Option<&PolicyAction>,
    ) -> (Vec<ChannelId>, Vec<(ChannelId, EliminateReason)>) {
        // 点名覆盖：直接以点名的通道为宇宙，不再解释"为什么别的通道没进来"
        if let Some(a) = action {
            if !a.use_channels.is_empty() {
                let mut seen = BTreeSet::new();
                let uni: Vec<ChannelId> =
                    a.use_channels.iter().filter(|c| seen.insert((*c).clone())).cloned().collect();
                return (uni, vec![]);
            }
        }

        let Some(gid) = group else {
            return (model_scope.to_vec(), vec![]);
        };
        let Some(g) = self.index.group(gid) else {
            return (model_scope.to_vec(), vec![]);
        };

        let members: BTreeSet<&ChannelId> = g.members.iter().collect();
        let outside: Vec<(ChannelId, EliminateReason)> = model_scope
            .iter()
            .filter(|c| !members.contains(c))
            .map(|c| (c.clone(), EliminateReason::NotInGroup))
            .collect();

        let mut seen = BTreeSet::new();
        let uni: Vec<ChannelId> =
            g.members.iter().filter(|c| seen.insert((*c).clone())).cloned().collect();
        (uni, outside)
    }

    /// 单个通道为什么被淘汰（`None` = 保留为候选）。
    fn eliminate_reason(
        &self,
        id: &ChannelId,
        action: Option<&PolicyAction>,
        ctx: &RouteContext,
    ) -> Option<EliminateReason> {
        let idx = &self.index;
        let Some(ch) = idx.channel(id) else {
            return Some(EliminateReason::NotInGroup);
        };

        // 配置层：未启用
        if !ch.enabled {
            return Some(EliminateReason::Disabled);
        }

        // 运行态：禁用 / 凭据失效 / 冷却
        if let Some(st) = ctx.channel_state.get(id) {
            if !st.enabled {
                return Some(EliminateReason::Disabled);
            }
            if st.invalid_credential {
                return Some(EliminateReason::InvalidCredential);
            }
            if st.cooling_until_ms.map(|t| t > ctx.now_ms).unwrap_or(false) {
                return Some(EliminateReason::Cooling);
            }
        }

        // 线路：**只看服务商那一个节点**（[docs/55]：policy/channel/credential 三级覆盖已删）
        let lane = idx.resolve_lane(ch);
        if let Some(l) = lane {
            if !idx.lanes.contains_key(&l) {
                if self.opts.strict_lane_check {
                    return Some(EliminateReason::NoLane);
                }
            } else if ctx.lane_health.get(&l) == Some(&false) {
                return Some(EliminateReason::LaneDown);
            }
        }

        // 策略收窄（单条规则内可组合）
        if let Some(a) = action {
            if a.exclude_channels.contains(id) {
                return Some(EliminateReason::ExcludedByPolicy);
            }
            if let Some(t) = &a.require_tag {
                if !ch.tags.iter().any(|x| x == t) {
                    return Some(EliminateReason::TaggedOut);
                }
            }
            if let Some(t) = &a.exclude_tag {
                if ch.tags.iter().any(|x| x == t) {
                    return Some(EliminateReason::TaggedOut);
                }
            }
        }

        None
    }

    fn apply_pin(
        &self,
        pinned: Option<ChannelId>,
        ctx: &RouteContext,
        candidates: &mut Vec<ChannelId>,
        eliminated: &mut Vec<(ChannelId, EliminateReason)>,
    ) -> Option<ChannelId> {
        let p = pinned?;
        let ch = self.index.channel(&p)?;
        // pin 只尊重"硬不可用"（未启用 / 冷却 / 耗尽 / 凭据失效 / 线路挂），
        // 不受标签与 exclude 收窄影响 —— 用户显式钉的通道就是最高意图。
        if !ch.enabled {
            return None;
        }
        if let Some(st) = ctx.channel_state.get(&p) {
            if !st.is_available(ctx.now_ms) {
                return None;
            }
        }
        // 线路：**只看服务商那一个节点**（[docs/55]：policy/channel/credential 三级覆盖已删）
        let lane = self.index.resolve_lane(ch);
        if let Some(l) = lane {
            if ctx.lane_health.get(&l) == Some(&false) {
                return None;
            }
        }
        eliminated.retain(|(c, _)| c != &p);
        candidates.retain(|c| c != &p);
        candidates.insert(0, p.clone());
        Some(p)
    }

    /// 内置默认 failover：来自 `gateway.retry`，流式请求自动降到更少的尝试次数
    /// （`docs/03` §5.2）。
    fn builtin_failover(&self, hints: &RouteHints) -> FailoverPolicy {
        let mut f: FailoverPolicy =
            serde_json::from_value(serde_json::json!({})).unwrap_or_else(|_| FailoverPolicy {
                on: vec![],
                max_attempts: 3,
                cooldown_s: 120,
                backoff: Backoff::Exponential,
                extra: Default::default(),
            });
        let retry = &self.index.retry;
        let attempts = if hints.stream { retry.stream_max_attempts } else { retry.max_attempts };
        f.max_attempts = attempts.max(1);
        f.cooldown_s = retry.cooldown_s;
        f.backoff = retry.backoff;
        f
    }
}

// ================================================================ 条件求值

/// `PolicyMatch` 的全部条件：**AND**（列表内部为 OR）。
///
/// 语义上容易踩坑的两条，显式写死在这里：
/// - `all: true` 是"恒真条件"（兜底规则）；默认的 `false` 表示**该项不约束**，
///   而不是"必须为假"。因此 `match: {}` 也等价于"匹配一切"。
/// - `provider_in` / `channel_tags_*` / `quota_remaining_lt` 看的是**候选通道集合**
///   （该模型在配置层面启用的通道），因为客户端请求里根本不带这些信息。
pub fn policy_matches(
    m: &PolicyMatch,
    hints: &RouteHints,
    ctx: &RouteContext,
    model_scope: &[ChannelId],
    idx: &RoutingIndex,
    local_offset_minutes: i32,
) -> bool {
    // 注意：`m.all == true` 是"恒真条件"；`false` 表示"不约束"。所以这里不需要任何分支。
    if !m.model_matches.is_empty() {
        let Some(model) = hints.model.as_deref() else { return false };
        if !m.model_matches.iter().any(|p| glob_match(p, model)) {
            return false;
        }
    }

    if let Some(want) = m.has_image {
        if hints.has_image != want {
            return false;
        }
    }

    if let Some(want) = m.stream {
        if hints.stream != want {
            return false;
        }
    }

    if let Some(lt) = m.input_tokens_lt {
        if !(hints.est_input_tokens < lt) {
            return false;
        }
    }
    if let Some(gt) = m.input_tokens_gt {
        if !(hints.est_input_tokens > gt) {
            return false;
        }
    }

    if !m.header_matches.is_empty() {
        for (name, pattern) in &m.header_matches {
            let Some(v) = header_lookup(hints, name) else { return false };
            if !glob_ci(pattern, v) {
                return false;
            }
        }
    }

    if let Some(re) = &m.ua_matches {
        let ua = hints
            .ua
            .clone()
            .or_else(|| header_lookup(hints, "user-agent").map(|s| s.to_string()))
            .unwrap_or_default();
        if !regex_matches(re, &ua) {
            return false;
        }
    }

    if !m.channel_tags_any.is_empty() || !m.channel_tags_all.is_empty() {
        let enabled: Vec<&ChannelConfig> =
            model_scope.iter().filter_map(|id| idx.channel(id)).filter(|c| c.enabled).collect();
        if !m.channel_tags_any.is_empty()
            && !enabled
                .iter()
                .any(|c| c.tags.iter().any(|t| m.channel_tags_any.iter().any(|w| w == t)))
        {
            return false;
        }
        if !m.channel_tags_all.is_empty()
            && !enabled
                .iter()
                .any(|c| m.channel_tags_all.iter().all(|w| c.tags.iter().any(|t| t == w)))
        {
            return false;
        }
    }

    if let Some(spec) = &m.time_window {
        match parse_time_window(spec) {
            Some(w) => {
                if !time_window_matches(&w, ctx.now_ms, local_offset_minutes) {
                    return false;
                }
            }
            None => {
                tracing::warn!(spec = %spec, "time_window 无法解析，按不命中处理");
                return false;
            }
        }
    }

    if !m.provider_in.is_empty() {
        let hit = model_scope
            .iter()
            .filter_map(|id| idx.channel(id))
            .filter(|c| c.enabled)
            .any(|c| m.provider_in.contains(&c.provider));
        if !hit {
            return false;
        }
    }

    true
}

/// 请求头查找，**大小写不敏感**（HTTP 头本来就大小写不敏感）。
pub fn header_lookup<'a>(hints: &'a RouteHints, name: &str) -> Option<&'a str> {
    if let Some(v) = hints.headers.get(name) {
        return Some(v.as_str());
    }
    hints.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// glob 匹配，先按原串，再按全小写（`x-client: Cursor` / `cursor` 都能匹配上）。
pub fn glob_ci(pattern: &str, s: &str) -> bool {
    glob_match(pattern, s) || glob_match(&pattern.to_ascii_lowercase(), &s.to_ascii_lowercase())
}

/// 带缓存的 regex 匹配；**非法正则 = 不命中**（绝不 panic 打断请求）。
pub fn regex_matches(pattern: &str, s: &str) -> bool {
    static CACHE: Lazy<Mutex<BTreeMap<String, Option<Regex>>>> =
        Lazy::new(|| Mutex::new(BTreeMap::new()));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let re = cache.entry(pattern.to_string()).or_insert_with(|| match Regex::new(pattern) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!(pattern = %pattern, error = %e, "ua_matches 正则非法");
            None
        }
    });
    re.as_ref().map(|r| r.is_match(s)).unwrap_or(false)
}

// ================================================================ 时间窗

/// `time_window` 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeWindow {
    /// 本地时间的一到多个 `[起, 止)` 分钟区间；起 > 止 表示跨零点
    Ranges(Vec<(u32, u32)>),
    /// 周一–周五
    Weekdays,
    /// 周六、周日
    Weekends,
}

/// `"09:00-19:00"` / `"01:00-07:00,12:00-13:00"` / `"weekdays"` / `"weekends"`。
pub fn parse_time_window(spec: &str) -> Option<TimeWindow> {
    let s = spec.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => return None,
        "weekdays" | "weekday" | "workdays" => return Some(TimeWindow::Weekdays),
        "weekends" | "weekend" => return Some(TimeWindow::Weekends),
        _ => {}
    }
    let mut ranges = vec![];
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (a, b) = part.split_once('-')?;
        ranges.push((parse_hm(a)?, parse_hm(b)?));
    }
    if ranges.is_empty() {
        None
    } else {
        Some(TimeWindow::Ranges(ranges))
    }
}

fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 24 || m > 59 {
        return None;
    }
    Some((h * 60 + m).min(24 * 60))
}

/// 把 `now_ms` 按本地偏移换算成本地时间再判定。
/// `start == end` 视作"全天"（`"00:00-00:00"` 这种写法很常见）。
pub fn time_window_matches(w: &TimeWindow, now_ms: i64, offset_minutes: i32) -> bool {
    let local_ms = now_ms + (offset_minutes as i64) * 60_000;
    let day = local_ms.div_euclid(86_400_000);
    let minute = (local_ms.rem_euclid(86_400_000) / 60_000) as u32;
    // 1970-01-01 是周四 → 0 = 周一
    let weekday = ((day + 3).rem_euclid(7)) as u32;
    match w {
        TimeWindow::Weekdays => weekday <= 4,
        TimeWindow::Weekends => weekday >= 5,
        TimeWindow::Ranges(rs) => rs.iter().any(|(s, e)| {
            if s == e {
                true
            } else if s < e {
                minute >= *s && minute < *e
            } else {
                minute >= *s || minute < *e
            }
        }),
    }
}

// ================================================================ 内部小工具

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::routing::ChannelRuntimeState;

    // ------------------------------------------------------------ 测试脚手架

    fn doc(yaml: &str) -> ConfigDoc {
        config::from_yaml(yaml).unwrap_or_else(|e| panic!("yaml 解析失败: {e:?}"))
    }

    fn engine(doc: &ConfigDoc) -> PolicyEngineImpl {
        // UTC+8，测试统一
        let opts = EngineOptions { local_offset_minutes: 480, ..Default::default() };
        PolicyEngineImpl::with_options(Arc::new(RoutingIndex::build(doc)), opts)
    }

    /// 三通道 + 一个组的最小配置：ch-a(prio 10, official) / ch-b(prio 20, cheap) / ch-c(prio 30)
    const BASE: &str = r#"
version: 1
net:
  default_lane: lane-direct
lanes:
  - { id: lane-direct, name: 直连, kind: direct }
  - { id: lane-jp, name: 日本, kind: subscription_auto }
providers:
  - { id: p-openai, name: OpenAI, protocol: openai, base_url: "https://api.openai.com/v1" }
  - { id: p-relay, name: Relay, protocol: openai, base_url: "https://relay.example/v1" }
credentials:
  - { id: c-openai, provider: p-openai, label: openai, secret: "${secret:o}", lane: lane-direct }
  - { id: c-relay-a, provider: p-relay, label: relay-a, secret: "${secret:a}", lane: lane-direct }
  - { id: c-relay-b, provider: p-relay, label: relay-b, secret: "${secret:b}", lane: lane-direct }
channels:
  - id: ch-a
    model: gpt-4o
    provider: p-openai
    credential: c-openai
    upstream_model: gpt-4o-2024-11-20
    priority: 10
    weight: 10
    tags: [official]
    enabled: true
  - id: ch-b
    model: gpt-4o
    provider: p-relay
    credential: c-relay-a
    upstream_model: gpt-4o
    priority: 20
    weight: 5
    tags: [cheap, vision]
    enabled: true
  - id: ch-c
    model: gpt-4o
    provider: p-relay
    credential: c-relay-b
    upstream_model: gpt-4o
    priority: 30
    weight: 1
    tags: [no-vision]
    enabled: true
models:
  - id: gpt-4o
    aliases: [gpt-4o-latest]
"#;

    /// 只有一条通道的最小配置（用于单通道的淘汰原因测试）
    const MINI: &str = r#"
version: 1
net:
  default_lane: lane-direct
lanes:
  - { id: lane-direct, name: 直连, kind: direct }
providers:
  - { id: p-openai, name: OpenAI, protocol: openai, base_url: "https://api.openai.com/v1" }
credentials:
  - { id: c-openai, provider: p-openai, label: openai, secret: "${secret:o}" }
channels:
  - id: ch-a
    model: gpt-4o
    provider: p-openai
    credential: c-openai
    upstream_model: gpt-4o
    enabled: true
"#;

    /// 2023-11-14（周二）12:00:00 UTC
    const TUE_NOON_MS: i64 = 1_699_963_200_000;
    /// 2023-11-19（周日）12:00:00 UTC
    const SUN_NOON_MS: i64 = 1_700_355_200_000;
    /// 2023-11-14（周二）00:00:00 UTC —— 整除 86400000，便于构造"当天几点"
    const TUE_MIDNIGHT_MS: i64 = 1_699_920_000_000;

    fn hints(model: &str) -> RouteHints {
        RouteHints { model: Some(model.into()), ..Default::default() }
    }

    fn ctx() -> RouteContext {
        RouteContext { now_ms: TUE_NOON_MS, ..Default::default() }
    }

    fn ids(d: &RouteDecision) -> Vec<&str> {
        d.candidates.iter().map(|c| c.as_str()).collect()
    }

    // ------------------------------------------------------------ 规则顺序

    #[test]
    fn first_matching_policy_wins_and_priority_is_ascending() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-priority-20
    name: 后写的但优先级小
    priority: 20
    match: {{ all: true }}
    action: {{ exclude_channels: [ch-b, ch-c] }}
  - id: pol-priority-30
    name: 先写的但优先级大
    priority: 30
    match: {{ all: true }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-priority-20");
        assert_eq!(ids(&dec), vec!["ch-a"]);
    }

    #[test]
    fn no_stacking_across_rules() {
        // 第二条规则的 exclude 不得被叠加（首个命中即生效）
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-1
    name: 只排除 c
    priority: 10
    match: {{ all: true }}
    action: {{ exclude_channels: [ch-c] }}
  - id: pol-2
    name: 只排除 a
    priority: 20
    match: {{ all: true }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b"]);
    }

    #[test]
    fn invalid_priority_order_is_stable() {
        let d = doc(&format!(
            "{BASE}
policies:
  - {{ id: pol-x, name: x, priority: 10, match: {{ all: true }}, action: {{ exclude_channels: [ch-b] }} }}
  - {{ id: pol-y, name: y, priority: 10, match: {{ all: true }}, action: {{ exclude_channels: [ch-a] }} }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-x");
    }

    #[test]
    fn disabled_policy_is_skipped() {
        let d = doc(&format!(
            "{BASE}
policies:
  - {{ id: pol-off, name: off, enabled: false, priority: 1, match: {{ all: true }}, action: {{ exclude_channels: [ch-a, ch-b] }} }}
  - {{ id: pol-on, name: on, priority: 10, match: {{ all: true }}, action: {{ exclude_channels: [ch-c] }} }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-on");
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b"]);
    }

    #[test]
    fn no_matching_policy_falls_back_to_builtin_default() {
        let d = doc(&format!("{BASE}\npolicies: []\n"));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert!(dec.matched_policy.is_none());
        assert_eq!(dec.selector, Selector::BalancedSticky);
        assert_eq!(dec.on_no_candidate, NoCandidateAction::Error);
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b", "ch-c"]);
        assert!(dec.eliminated.is_empty());
    }

    // ------------------------------------------------------------ 每条 match

    #[test]
    fn match_model_matches_glob() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-glob
    name: glob
    priority: 10
    match: {{ model_matches: [\"gpt-4o*\", \"claude-*\"] }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        assert_eq!(
            ids(&e.decide(&hints("gpt-4o-latest"), &ctx())),
            vec!["ch-b", "ch-c"],
            "别名也应命中 gpt-4o*"
        );
        assert!(e.decide(&hints("gpt-3.5"), &ctx()).matched_policy.is_none());
        // 没有 model 的请求 → model_matches 不命中
        assert!(e.decide(&RouteHints::default(), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_has_image() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-img
    name: img
    priority: 10
    match: {{ has_image: true }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        let mut h = hints("gpt-4o");
        h.has_image = true;
        assert_eq!(ids(&e.decide(&h, &ctx())), vec!["ch-b", "ch-c"]);
        assert!(e.decide(&hints("gpt-4o"), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_stream() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-stream
    name: stream
    priority: 10
    match: {{ stream: true }}
    action: {{ exclude_channels: [ch-c] }}
"
        ));
        let e = engine(&d);
        let mut h = hints("gpt-4o");
        h.stream = true;
        let dec = e.decide(&h, &ctx());
        assert_eq!(dec.matched_policy.unwrap().as_str(), "pol-stream");
        // 流式请求内置 failover 尝试次数降到 stream_max_attempts(2)
        assert_eq!(dec.failover.max_attempts, 2);
        let dec2 = e.decide(&hints("gpt-4o"), &ctx());
        assert!(dec2.matched_policy.is_none());
        assert_eq!(dec2.failover.max_attempts, 3);
    }

    #[test]
    fn match_header_matches_is_case_insensitive() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-header
    name: header
    priority: 10
    match:
      header_matches: {{ X-Client: cursor }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        let mut h = hints("gpt-4o");
        h.headers.insert("x-client".into(), "Cursor".into());
        assert_eq!(ids(&e.decide(&h, &ctx())), vec!["ch-b", "ch-c"]);

        let mut h2 = hints("gpt-4o");
        h2.headers.insert("x-client".into(), "other".into());
        assert!(e.decide(&h2, &ctx()).matched_policy.is_none());

        // 缺头 → 不命中
        assert!(e.decide(&hints("gpt-4o"), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_ua_matches_regex() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-ua
    name: ua
    priority: 10
    match: {{ ua_matches: \"(?i)cursor|claude-code\" }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        let mut h = hints("gpt-4o");
        h.ua = Some("Cursor/0.42 (darwin)".into());
        assert_eq!(ids(&e.decide(&h, &ctx())), vec!["ch-b", "ch-c"]);

        let mut h2 = hints("gpt-4o");
        h2.ua = Some("curl/8.0".into());
        assert!(e.decide(&h2, &ctx()).matched_policy.is_none());

        // 非法正则 → 不命中，不 panic
        let bad = doc(&format!(
            "{BASE}
policies:
  - {{ id: pol-bad, name: bad, priority: 10, match: {{ ua_matches: \"([\" }}, action: {{}} }}
"
        ));
        assert!(engine(&bad).decide(&hints("gpt-4o"), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_channel_tags_any_and_all() {
        // any：只要候选里有一个带该标签的通道就命中
        let d_any = doc(&format!(
            "{BASE}
policies:
  - id: pol-any
    name: any
    priority: 10
    match: {{ channel_tags_any: [cheap] }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        assert_eq!(ids(&engine(&d_any).decide(&hints("gpt-4o"), &ctx())), vec!["ch-b", "ch-c"]);

        // all：需要**同一个通道**同时具备全部标签
        let d_all = doc(&format!(
            "{BASE}
policies:
  - id: pol-all
    name: all
    priority: 10
    match: {{ channel_tags_all: [cheap, vision] }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        assert_eq!(ids(&engine(&d_all).decide(&hints("gpt-4o"), &ctx())), vec!["ch-b", "ch-c"]);

        let d_all2 = doc(&format!(
            "{BASE}
policies:
  - id: pol-all2
    name: all2
    priority: 10
    match: {{ channel_tags_all: [cheap, official] }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        assert!(engine(&d_all2).decide(&hints("gpt-4o"), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_input_tokens_lt_gt() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-small
    name: small
    priority: 10
    match: {{ input_tokens_lt: 1000 }}
    action: {{ exclude_channels: [ch-c] }}
  - id: pol-big
    name: big
    priority: 20
    match: {{ input_tokens_gt: 20000 }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        let mut small = hints("gpt-4o");
        small.est_input_tokens = 999;
        assert_eq!(e.decide(&small, &ctx()).matched_policy.unwrap().as_str(), "pol-small");

        let mut big = hints("gpt-4o");
        big.est_input_tokens = 20001;
        assert_eq!(e.decide(&big, &ctx()).matched_policy.unwrap().as_str(), "pol-big");

        // 边界：严格大于 / 严格小于
        let mut edge = hints("gpt-4o");
        edge.est_input_tokens = 1000;
        assert!(e.decide(&edge, &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_provider_in() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-provider
    name: provider
    priority: 10
    match: {{ provider_in: [p-relay] }}
    action: {{ exclude_channels: [ch-a] }}
"
        ));
        let e = engine(&d);
        let dec = e.decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec.matched_policy.as_ref().unwrap().as_str(), "pol-provider");
        assert_eq!(ids(&dec), vec!["ch-b", "ch-c"]);

        // 模型换成只有 openai 通道时 → 不命中
        let d2 = doc(&format!(
            "{BASE}
policies:
  - id: pol-provider
    name: provider
    priority: 10
    match: {{ provider_in: [p-nonexistent] }}
    action: {{}}
"
        ));
        assert!(engine(&d2).decide(&hints("gpt-4o"), &ctx()).matched_policy.is_none());
    }

    #[test]
    fn match_time_window_ranges_and_weekdays() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-window
    name: window
    priority: 10
    match: {{ time_window: \"09:00-19:00\" }}
    action: {{ exclude_channels: [ch-c] }}
"
        ));
        let e = engine(&d);

        // 周二 12:00 UTC：UTC+8 → 20:00 → 不在窗口内
        let c = ctx();
        assert_eq!(c.now_ms, TUE_NOON_MS);
        assert!(e.decide(&hints("gpt-4o"), &c).matched_policy.is_none());

        // UTC+0 → 12:00 → 在窗口内
        let opts = EngineOptions { local_offset_minutes: 0, ..Default::default() };
        let e0 = PolicyEngineImpl::with_options(Arc::new(RoutingIndex::build(&d)), opts);
        assert_eq!(e0.decide(&hints("gpt-4o"), &c).matched_policy.unwrap().as_str(), "pol-window");

        // weekdays：周二命中
        let d_week = doc(&format!(
            "{BASE}
policies:
  - {{ id: pol-weekdays, name: wd, priority: 10, match: {{ time_window: weekdays }}, action: {{ exclude_channels: [ch-c] }} }}
"
        ));
        let ew = PolicyEngineImpl::with_options(
            Arc::new(RoutingIndex::build(&d_week)),
            EngineOptions { local_offset_minutes: 0, ..Default::default() },
        );
        assert_eq!(
            ew.decide(&hints("gpt-4o"), &c).matched_policy.unwrap().as_str(),
            "pol-weekdays"
        );

        // 周日 12:00 UTC → weekdays 不命中
        let cs = RouteContext { now_ms: SUN_NOON_MS, ..Default::default() };
        assert!(ew.decide(&hints("gpt-4o"), &cs).matched_policy.is_none());
    }

    #[test]
    fn time_window_wraps_midnight_and_parses_multi_range() {
        let base = TUE_MIDNIGHT_MS;
        let w = parse_time_window("22:00-06:00").unwrap();
        assert!(time_window_matches(&w, base + 23 * 3_600_000, 0));
        assert!(time_window_matches(&w, base + 2 * 3_600_000, 0));
        assert!(!time_window_matches(&w, base + 12 * 3_600_000, 0));

        let w2 = parse_time_window("01:00-07:00,12:00-13:00").unwrap();
        assert!(time_window_matches(&w2, base + 6 * 3_600_000, 0));
        assert!(time_window_matches(&w2, base + 12 * 3_600_000, 0));
        assert!(!time_window_matches(&w2, base + 9 * 3_600_000, 0));

        // 全天
        assert!(time_window_matches(
            &parse_time_window("00:00-00:00").unwrap(),
            base + 5 * 3_600_000,
            0
        ));
        // 非法写法
        assert!(parse_time_window("25:00-99:00").is_none());
        assert!(parse_time_window("nonsense").is_none());
    }

    #[test]
    fn time_window_uses_local_offset() {
        let w = parse_time_window("09:00-19:00").unwrap();
        // UTC 01:00 → UTC+8 是 09:00（窗口起点，闭区间）
        assert!(time_window_matches(&w, TUE_MIDNIGHT_MS + 3_600_000, 480));
        // UTC 01:00 → UTC-5 是前一天 20:00 → 不在窗口
        assert!(!time_window_matches(&w, TUE_MIDNIGHT_MS + 3_600_000, -300));
    }

    // ------------------------------------------------------------ 每条 action

    #[test]
    fn action_use_group_expands_members_in_order() {
        let d = doc(&format!(
            "{BASE}
channel_groups:
  - id: grp-cheap-first
    model: gpt-4o
    members: [ch-c, ch-b]
    selector: balanced_sticky
policies:
  - id: pol-group
    name: group
    priority: 10
    match: {{ all: true }}
    action: {{ use_group: grp-cheap-first }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-c", "ch-b"]);
        assert_eq!(dec.selector, Selector::BalancedSticky);
        // ch-a 不在组里 → NotInGroup 可解释
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, r)| c.as_str() == "ch-a" && *r == EliminateReason::NotInGroup));
    }

    #[test]
    fn group_is_authoritative_by_default_and_explains_non_members() {
        let d = doc(&format!(
            "{BASE}
channel_groups:
  - id: grp-gpt-4o
    model: gpt-4o
    members: [ch-a, ch-b]
policies: []
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b"]);
        assert_eq!(dec.eliminated, vec![(ChannelId::from("ch-c"), EliminateReason::NotInGroup)]);
    }

    #[test]
    fn action_use_channels_overrides_candidate_set() {
        let d = doc(&format!(
            "{BASE}
channel_groups:
  - id: grp-gpt-4o
    model: gpt-4o
    members: [ch-a]
policies:
  - id: pol-pick
    name: pick
    priority: 10
    match: {{ all: true }}
    action: {{ use_channels: [ch-c, ch-b] }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-c", "ch-b"]);
        assert!(dec.eliminated.is_empty());
    }

    #[test]
    fn action_exclude_channels_and_tags_combine() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-narrow
    name: narrow
    priority: 10
    match: {{ all: true }}
    action:
      exclude_channels: [ch-c]
      exclude_tag: cheap
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-a"]);
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, r)| c.as_str() == "ch-b" && *r == EliminateReason::TaggedOut));
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, r)| c.as_str() == "ch-c" && *r == EliminateReason::ExcludedByPolicy));
    }

    #[test]
    fn action_require_tag_keeps_only_tagged() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-vision
    name: vision
    priority: 10
    match: {{ all: true }}
    action: {{ require_tag: vision }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-b"]);
        assert_eq!(dec.eliminated.len(), 2);
    }

    #[test]
    fn default_does_not_filter_by_tags() {
        // 铁律：没有策略要求时，带 no-vision 的通道照样是候选
        let d = doc(&format!("{BASE}\npolicies: []\n"));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert!(dec.candidates.contains(&ChannelId::from("ch-c")));
        assert!(!dec.eliminated.iter().any(|(_, r)| *r == EliminateReason::TaggedOut));
    }

    #[test]
    fn vision_policy_singled_out_by_exclude_tag() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-skip-nonvision
    name: 带图请求跳过不识图的通道
    priority: 5
    match: {{ has_image: true }}
    action: {{ exclude_tag: no-vision, on_no_candidate: error, error_code: 415 }}
  - id: pol-default
    name: 默认
    priority: 1000
    match: {{ all: true }}
    action: {{}}
"
        ));
        let e = engine(&d);
        let mut h = hints("gpt-4o");
        h.has_image = true;
        let dec = e.decide(&h, &ctx());
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b"]);
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, r)| c.as_str() == "ch-c" && *r == EliminateReason::TaggedOut));

        // 不带图 → 默认规则，ch-c 仍在候选里
        let dec2 = e.decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec2.matched_policy.unwrap().as_str(), "pol-default");
        assert!(dec2.candidates.contains(&ChannelId::from("ch-c")));
    }

    /// 策略里的 `selector` 键**已经下线**（[docs/48]）：老配置里还留着它，
    /// 必须**不报错、不生效**（被 `PolicyAction.extra` 吸收），而同一段里**其它**字段照旧生效。
    ///
    /// 这条用例同时钉住两件事，缺一不可：兼容性（解析不失败）与"已下线"（不再改选路）。
    #[test]
    fn legacy_action_selector_is_ignored_while_failover_still_overrides() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-override
    name: override
    priority: 10
    match: {{ all: true }}
    action:
      selector: lowest_latency
      failover:
        on: [http429, timeout]
        max_attempts: 7
        cooldown_s: 5
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(
            dec.selector,
            Selector::BalancedSticky,
            "老配置里的 selector 不该再改变选路（它已下线）"
        );
        assert_eq!(dec.failover.max_attempts, 7);
        assert_eq!(dec.failover.cooldown_s, 5);
        assert_eq!(dec.failover.on, vec![FailoverTrigger::Http429, FailoverTrigger::Timeout]);
    }

    #[test]
    fn action_on_no_candidate_error_gets_default_error_code() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-none
    name: none
    priority: 10
    match: {{ all: true }}
    action: {{ exclude_channels: [ch-a, ch-b, ch-c] }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert!(dec.candidates.is_empty());
        assert_eq!(dec.on_no_candidate, NoCandidateAction::Error);
        assert_eq!(dec.error_code, Some(415));
        assert_eq!(dec.eliminated.len(), 3);
    }

    #[test]
    fn action_on_no_candidate_use_any_restores_policy_exclusions_only() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-any
    name: any
    priority: 10
    match: {{ all: true }}
    action:
      exclude_tag: no-vision
      exclude_channels: [ch-a, ch-b]
      on_no_candidate: use_any
"
        ));
        let mut c = ctx();
        // ch-b 凭据失效：即使 use_any 也不能放回来（运行态不可用）
        c.channel_state.insert(
            ChannelId::from("ch-b"),
            ChannelRuntimeState { enabled: true, invalid_credential: true, ..Default::default() },
        );
        let dec = engine(&d).decide(&hints("gpt-4o"), &c);
        assert_eq!(ids(&dec), vec!["ch-a", "ch-c"]);
        assert!(dec
            .eliminated
            .iter()
            .any(|(c, r)| c.as_str() == "ch-b" && *r == EliminateReason::InvalidCredential));
        assert_eq!(dec.error_code, None);
    }

    #[test]
    fn action_pin_sticky_forces_channel_first() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-pin
    name: pin
    priority: 10
    match: {{ all: true }}
    action: {{ pin_sticky: ch-c }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert_eq!(dec.pinned.as_ref().unwrap().as_str(), "ch-c");
        assert_eq!(ids(&dec), vec!["ch-c", "ch-a", "ch-b"]);
    }

    #[test]
    fn action_pin_sticky_ignored_when_channel_unavailable() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-pin
    name: pin
    priority: 10
    match: {{ all: true }}
    action: {{ pin_sticky: ch-c, exclude_tag: no-vision }}
"
        ));
        let mut c = ctx();
        c.channel_state.insert(
            ChannelId::from("ch-c"),
            ChannelRuntimeState { enabled: false, ..Default::default() },
        );
        let dec = engine(&d).decide(&hints("gpt-4o"), &c);
        assert!(dec.pinned.is_none());
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b"]);
    }

    /// [docs/55]（第 322 轮）策略级 `use_lane` **已删除**：老配置里写着它，也**不再有任何作用**。
    ///
    /// 这是"放弃线路级别分配"的反回归闸门 —— 它与旧断言（策略覆盖会变成
    /// `RouteDecision.lane_override`）**方向相反**，所以必须显式钉住"它不生效"，
    /// 否则哪天有人把那条链接回来，测试不会红。
    #[test]
    fn legacy_policy_use_lane_is_ignored() {
        let d = doc(&format!(
            "{BASE}
policies:
  - id: pol-lane
    name: lane
    priority: 10
    match: {{ all: true }}
    action: {{ use_lane: lane-jp }}
"
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        // 三条候选一个都没被淘汰：策略里的 use_lane 不再锁定任何东西
        assert_eq!(ids(&dec), vec!["ch-a", "ch-b", "ch-c"]);
    }

    /// [docs/55] 生效线路只看服务商 → **服务商那条线路 down 就全部淘汰**。
    ///
    /// 旧版本这条测试靠 `policy.use_lane` 把线路钉到 `lane-jp`；那条路已经删了，
    /// 所以改成直接给两个服务商配 `lane: lane-jp` —— 这才是现在唯一能让它成立的方式。
    #[test]
    fn provider_lane_down_eliminates_everything() {
        let mut d = doc(BASE);
        for p in d.providers.iter_mut() {
            p.lane = Some(LaneId::from("lane-jp"));
        }
        let mut c = ctx();
        c.lane_health.insert(LaneId::from("lane-jp"), false);
        let dec = engine(&d).decide(&hints("gpt-4o"), &c);
        assert!(dec.candidates.is_empty());
        assert_eq!(dec.eliminated.len(), 3);
        assert!(dec.eliminated.iter().all(|(_, r)| *r == EliminateReason::LaneDown));
    }

    // ------------------------------------------------------------ 淘汰原因

    #[test]
    fn eliminate_reasons_are_specific() {
        let d = doc(&format!("{MINI}\npolicies: []\n"));
        let e = engine(&d);

        let mut c = ctx();
        c.channel_state.insert(
            ChannelId::from("ch-a"),
            ChannelRuntimeState {
                enabled: true,
                cooling_until_ms: Some(c.now_ms + 1000),
                ..Default::default()
            },
        );
        assert_eq!(
            e.decide(&hints("gpt-4o"), &c).eliminated,
            vec![(ChannelId::from("ch-a"), EliminateReason::Cooling)]
        );

        c.channel_state.insert(
            ChannelId::from("ch-a"),
            ChannelRuntimeState { enabled: false, ..Default::default() },
        );
        assert_eq!(
            e.decide(&hints("gpt-4o"), &c).eliminated,
            vec![(ChannelId::from("ch-a"), EliminateReason::Disabled)]
        );

        c.channel_state.insert(
            ChannelId::from("ch-a"),
            ChannelRuntimeState { enabled: true, invalid_credential: true, ..Default::default() },
        );
        assert_eq!(
            e.decide(&hints("gpt-4o"), &c).eliminated,
            vec![(ChannelId::from("ch-a"), EliminateReason::InvalidCredential)]
        );

        c.channel_state.insert(
            ChannelId::from("ch-a"),
            ChannelRuntimeState { enabled: false, ..Default::default() },
        );
        assert_eq!(
            e.decide(&hints("gpt-4o"), &c).eliminated,
            vec![(ChannelId::from("ch-a"), EliminateReason::Disabled)]
        );

        // 冷却已过期 → 恢复候选
        let mut c2 = ctx();
        c2.channel_state.insert(
            ChannelId::from("ch-a"),
            ChannelRuntimeState {
                enabled: true,
                cooling_until_ms: Some(c2.now_ms - 1),
                ..Default::default()
            },
        );
        assert_eq!(ids(&e.decide(&hints("gpt-4o"), &c2)), vec!["ch-a"]);
    }

    #[test]
    fn disabled_channel_is_eliminated_with_disabled() {
        let d =
            doc(&format!("{}\npolicies: []\n", MINI.replace("enabled: true", "enabled: false")));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert!(dec.candidates.is_empty());
        assert_eq!(dec.eliminated, vec![(ChannelId::from("ch-a"), EliminateReason::Disabled)]);
        assert_eq!(dec.error_code, Some(415));
    }

    #[test]
    fn lane_down_eliminates_only_that_lane() {
        let d = doc(&format!("{BASE}\npolicies: []\n"));
        let e = engine(&d);
        let mut c = ctx();
        c.lane_health.insert(LaneId::from("lane-direct"), false);
        let dec = e.decide(&hints("gpt-4o"), &c);
        assert!(dec.candidates.is_empty());
        assert!(dec.eliminated.iter().all(|(_, r)| *r == EliminateReason::LaneDown));
    }

    #[test]
    fn missing_lane_reference_is_no_lane_but_can_be_lenient() {
        let d = doc(&format!(
            "{}\npolicies: []\n",
            MINI.replace("default_lane: lane-direct", "default_lane: lane-ghost")
        ));
        let dec = engine(&d).decide(&hints("gpt-4o"), &ctx());
        assert!(dec.candidates.is_empty());
        assert_eq!(dec.eliminated, vec![(ChannelId::from("ch-a"), EliminateReason::NoLane)]);

        let opts = EngineOptions { strict_lane_check: false, ..Default::default() };
        let lenient = PolicyEngineImpl::with_options(Arc::new(RoutingIndex::build(&d)), opts);
        assert_eq!(lenient.decide(&hints("gpt-4o"), &ctx()).candidates.len(), 1);
    }

    #[test]
    fn unknown_model_has_no_candidates() {
        let d = doc(&format!("{BASE}\npolicies: []\n"));
        let dec = engine(&d).decide(&hints("nope"), &ctx());
        assert!(dec.candidates.is_empty());
        assert!(dec.eliminated.is_empty());
        assert_eq!(dec.error_code, Some(415));
    }

    #[test]
    fn model_without_group_uses_all_its_channels() {
        let yaml = format!(
            "{MINI}
policies: []
"
        )
        .replace("model: gpt-4o", "model: claude-sonnet-4-5");
        let dec = engine(&doc(&yaml)).decide(&hints("claude-sonnet-4-5"), &ctx());
        assert_eq!(ids(&dec), vec!["ch-a"]);
    }
}

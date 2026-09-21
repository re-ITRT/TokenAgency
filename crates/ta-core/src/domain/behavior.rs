//! 领域类型的**行为**：便捷构造函数、只读辅助、线路解析优先级、模型名 glob。
//!
//! 冻结契约（字段名 / 字段类型 / trait 签名）在 `domain/mod.rs` 与 `types.rs`；
//! 本模块**只加行为，不改契约**：
//!
//! - 便捷构造函数与 builder：`ChannelConfig::new(..).with_lane(..)`
//! - 只读辅助：`Model::matches`、`ChannelConfig::effective_lane`、`ConfigDoc::find_model`
//! - 线路解析优先级（[04 §8.1]）：
//!   `policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`
//! - 模型名 glob（[`matches_glob`]）：校验器与定价引擎共用同一套匹配语义，
//!   避免"校验说能匹配、定价说匹配不上"这种漂移。

use std::collections::BTreeMap;

use serde_json::Map;

use super::*;
use crate::types::*;

// ================================================================ glob

/// 模型名 / 标签名 glob 匹配：`*` = 任意多字符（含空），`?` = 任意单字符，其余按字面量。
///
/// 区分大小写。用于 `pricing.entries[].match.model` 与 `policy.match.model_matches`。
///
/// ```
/// # use ta_core::domain::behavior::matches_glob;
/// assert!(matches_glob("gpt-4o*", "gpt-4o-mini"));
/// assert!(matches_glob("*sonnet*", "claude-sonnet-4-5"));
/// assert!(!matches_glob("gpt-4o", "gpt-4o-mini"));
/// ```
pub fn matches_glob(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();

    let mut pi = 0usize;
    let mut vi = 0usize;
    // 最近一次命中的 `*`：(pattern 下标, 该 `*` 尝试消费到的 value 下标)
    let mut star: Option<(usize, usize)> = None;

    while vi < v.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, vi));
            pi += 1;
        } else if let Some((sp, sv)) = star {
            pi = sp + 1;
            vi = sv + 1;
            star = Some((sp, sv + 1));
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ================================================================ Model

impl Model {
    /// 新逻辑模型：`expose = true`（出现在 `/v1/models`），无别名、不分组。
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            aliases: Vec::new(),
            display: DisplayConfig::default(),
            expose: true,
            extra: Map::new(),
        }
    }

    pub fn with_aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.aliases = aliases.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_display(mut self, group: impl Into<String>, order: i32) -> Self {
        self.display = DisplayConfig { group: Some(group.into()), order, extra: Map::new() };
        self
    }

    /// `name`（id 或任一 alias）是否指向这个逻辑模型——`channel.model` 的合法性判据。
    pub fn matches(&self, name: &str) -> bool {
        self.id == name || self.aliases.iter().any(|a| a == name)
    }

    /// 对外可用的全部名字：id + aliases（id 恒在首位）。
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        std::iter::once(self.id.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

// ================================================================ Lane

impl LaneKind {
    /// 是否必须引用一份订阅（`direct` 是唯一不需要的）。
    pub fn needs_subscription(&self) -> bool {
        !matches!(self, LaneKind::Direct)
    }

    /// 生成的线路名/日志用的稳定短名。
    pub fn as_str(&self) -> &'static str {
        match self {
            LaneKind::Direct => "direct",
            LaneKind::SubscriptionAuto => "subscription_auto",
            LaneKind::SubscriptionGroup => "subscription_group",
            LaneKind::SubscriptionNode => "subscription_node",
            LaneKind::Filter => "filter",
        }
    }
}

impl Lane {
    /// 通用构造：`enabled = true`，其余可选项为空。
    pub fn new(id: impl Into<LaneId>, name: impl Into<String>, kind: LaneKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            subscription: None,
            group: None,
            group_type: None,
            node: None,
            filter: None,
            exclude_filter: None,
            enabled: true,
            extra: Map::new(),
        }
    }

    /// `kind: direct`——直连（本机网络）。
    pub fn direct(id: impl Into<LaneId>, name: impl Into<String>) -> Self {
        Self::new(id, name, LaneKind::Direct)
    }

    /// `kind: subscription_auto`——在整份订阅里自己择优。
    pub fn subscription_auto(
        id: impl Into<LaneId>,
        name: impl Into<String>,
        subscription: impl Into<SubscriptionId>,
    ) -> Self {
        let mut lane = Self::new(id, name, LaneKind::SubscriptionAuto);
        lane.subscription = Some(subscription.into());
        lane
    }

    /// `kind: subscription_group`——直接用订阅里自带的组（保留机场分组语义）。
    pub fn subscription_group(
        id: impl Into<LaneId>,
        name: impl Into<String>,
        subscription: impl Into<SubscriptionId>,
        group: impl Into<String>,
    ) -> Self {
        let mut lane = Self::new(id, name, LaneKind::SubscriptionGroup);
        lane.subscription = Some(subscription.into());
        lane.group = Some(group.into());
        lane
    }

    /// `kind: subscription_node`——固定某个节点。
    pub fn subscription_node(
        id: impl Into<LaneId>,
        name: impl Into<String>,
        subscription: impl Into<SubscriptionId>,
        node: impl Into<String>,
    ) -> Self {
        let mut lane = Self::new(id, name, LaneKind::SubscriptionNode);
        lane.subscription = Some(subscription.into());
        lane.node = Some(node.into());
        lane
    }

    /// `kind: filter`——正则筛选节点后自己择优。
    pub fn filtered(
        id: impl Into<LaneId>,
        name: impl Into<String>,
        subscription: impl Into<SubscriptionId>,
        include: impl Into<String>,
    ) -> Self {
        let mut lane = Self::new(id, name, LaneKind::Filter);
        lane.subscription = Some(subscription.into());
        lane.filter = Some(include.into());
        lane
    }

    /// lane 级二次收窄（include 正则）。
    pub fn with_filter(mut self, include: impl Into<String>) -> Self {
        self.filter = Some(include.into());
        self
    }

    /// lane 级二次收窄（exclude 正则）。
    pub fn with_exclude_filter(mut self, exclude: impl Into<String>) -> Self {
        self.exclude_filter = Some(exclude.into());
        self
    }

    /// 覆盖订阅里该组的 type。
    pub fn with_group_type(mut self, group_type: GroupType) -> Self {
        self.group_type = Some(group_type);
        self
    }

    pub fn is_direct(&self) -> bool {
        matches!(self.kind, LaneKind::Direct)
    }

    /// `kind` 语义上要求、但当前缺失的字段名（`direct` 恒为 `None`）。
    ///
    /// 依据 [04 §7.1]：非 `direct` 必须有 `subscription`；
    /// `subscription_group` 要有 `group`；`subscription_node` 要有 `node`；`filter` 要有 `filter`。
    pub fn missing_kind_field(&self) -> Option<&'static str> {
        match self.kind {
            LaneKind::Direct => None,
            LaneKind::SubscriptionAuto => self.missing_subscription(),
            LaneKind::SubscriptionGroup => {
                self.missing_subscription().or_else(|| missing_text(self.group.as_deref(), "group"))
            }
            LaneKind::SubscriptionNode => {
                self.missing_subscription().or_else(|| missing_text(self.node.as_deref(), "node"))
            }
            LaneKind::Filter => self
                .missing_subscription()
                .or_else(|| missing_text(self.filter.as_deref(), "filter")),
        }
    }

    fn missing_subscription(&self) -> Option<&'static str> {
        if self.subscription.is_some() {
            None
        } else {
            Some("subscription")
        }
    }

    /// 组类型：lane 覆盖值优先，否则用订阅里该组自带的 type。
    pub fn effective_group_type(&self, from_subscription: Option<GroupType>) -> Option<GroupType> {
        self.group_type.or(from_subscription)
    }
}

fn missing_text(value: Option<&str>, field: &'static str) -> Option<&'static str> {
    match value {
        Some(v) if !v.trim().is_empty() => None,
        _ => Some(field),
    }
}

// ================================================================ Provider

impl Provider {
    /// 新服务商：`enabled = true`，鉴权用协议默认（`bearer`）。
    pub fn new(
        id: impl Into<ProviderId>,
        name: impl Into<String>,
        protocol: Protocol,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            protocol,
            base_url: base_url.into(),
            auth: AuthConfig::default(),
            lane: None,
            headers: BTreeMap::new(),
            on_header_conflict: HeaderConflict::default(),
            passthrough: ProviderPassthrough::default(),
            timeout: TimeoutConfig::default(),
            tls: TlsConfig::default(),
            model_prefix: None,
            enabled: true,
            extra: Map::new(),
        }
    }

    pub fn with_auth(mut self, auth: AuthConfig) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_lane(mut self, lane: impl Into<LaneId>) -> Self {
        self.lane = Some(lane.into());
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn with_model_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.model_prefix = Some(prefix.into());
        self
    }

    pub fn with_timeout(mut self, timeout: TimeoutConfig) -> Self {
        self.timeout = timeout;
        self
    }
}

// ================================================================ Credential

impl Credential {
    /// 新凭据：`secret` 必须是 `"${secret:name}"` / `"${env:VAR}"` 引用，**绝不写明文**。
    pub fn new(
        id: impl Into<CredentialId>,
        provider: impl Into<ProviderId>,
        label: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            label: label.into(),
            secret: secret.into(),
            lane: None,
            quota: QuotaConfig::default(),
            guard: GuardConfig::default(),
            tags: Vec::new(),
            enabled: true,
            extra: Map::new(),
        }
    }

    pub fn with_lane(mut self, lane: impl Into<LaneId>) -> Self {
        self.lane = Some(lane.into());
        self
    }

    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_quota(mut self, quota: QuotaConfig) -> Self {
        self.quota = quota;
        self
    }

    pub fn with_guard(mut self, guard: GuardConfig) -> Self {
        self.guard = guard;
        self
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

impl QuotaConfig {
    /// 是否配了手工额度（`metric` 非 `unknown` 才会被判额度）。
    pub fn is_manual(&self) -> bool {
        !matches!(self.metric, QuotaMetric::Unknown)
    }

    pub fn has_limit(&self) -> bool {
        self.limit.is_some()
    }
}

// ================================================================ Channel

impl ChannelConfig {
    /// 新通道：`enabled = true`，`priority = 100`、`weight = 10`（与 serde 默认值一致）。
    pub fn new(
        id: impl Into<ChannelId>,
        model: impl Into<String>,
        provider: impl Into<ProviderId>,
        credential: impl Into<CredentialId>,
        upstream_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            model: model.into(),
            provider: provider.into(),
            credential: credential.into(),
            lane: None,
            upstream_model: upstream_model.into(),
            transform: Transform::default(),
            priority: 100,
            weight: 10,
            tags: Vec::new(),
            target_override: None,
            enabled: true,
            extra: Map::new(),
        }
    }

    pub fn with_lane(mut self, lane: impl Into<LaneId>) -> Self {
        self.lane = Some(lane.into());
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_upstream_model(mut self, upstream_model: impl Into<String>) -> Self {
        self.upstream_model = upstream_model.into();
        self
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// 线路解析（**不含**策略级覆盖与隐含 `direct`）：
    /// `channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane`
    ///
    /// 传 `None` 表示"该凭据/服务商不存在或没查"，会自然跳过那一级。
    /// 需要完整五级解析请用 [`resolve_lane`]。
    ///
    /// 依据 [04 §8.1] / [02 §2.3]。
    pub fn effective_lane<'a>(
        &'a self,
        credential: Option<&'a Credential>,
        provider: Option<&'a Provider>,
        net: &'a NetConfig,
    ) -> Option<&'a LaneId> {
        self.lane
            .as_ref()
            .or(credential.and_then(|c| c.lane.as_ref()))
            .or(provider.and_then(|p| p.lane.as_ref()))
            .or(net.default_lane.as_ref())
    }
}

/// 线路解析结果：显式线路，或兜底的 `direct`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneResolution<'a> {
    /// 命中了某条已配置的 lane
    Lane(&'a LaneId),
    /// 五级都没指定 → 直连（本机网络）
    Direct,
}

impl<'a> LaneResolution<'a> {
    pub fn lane_id(&self) -> Option<&'a LaneId> {
        match self {
            LaneResolution::Lane(id) => Some(id),
            LaneResolution::Direct => None,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, LaneResolution::Direct)
    }

    /// 日志 / 追踪用的稳定字符串。
    pub fn as_str(&self) -> &'a str {
        match self {
            LaneResolution::Lane(id) => id.as_str(),
            LaneResolution::Direct => "direct",
        }
    }
}

/// 完整五级线路解析（含策略级覆盖 + `direct` 兜底）：
/// `policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`
///
/// 依据 [04 §8.1]。
pub fn resolve_lane<'a>(
    policy_lane: Option<&'a LaneId>,
    channel: &'a ChannelConfig,
    credential: Option<&'a Credential>,
    provider: Option<&'a Provider>,
    net: &'a NetConfig,
) -> LaneResolution<'a> {
    match policy_lane.or_else(|| channel.effective_lane(credential, provider, net)) {
        Some(lane) => LaneResolution::Lane(lane),
        None => LaneResolution::Direct,
    }
}

// ================================================================ ChannelGroup

impl ChannelGroup {
    /// 新通道组：默认选择器 `sticky_then_priority`（[02 §2.7] 的默认值）。
    pub fn new<I, S>(id: impl Into<ChannelGroupId>, model: impl Into<String>, members: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<ChannelId>,
    {
        Self {
            id: id.into(),
            model: model.into(),
            members: members.into_iter().map(Into::into).collect(),
            selector: Selector::StickyThenPriority,
            sticky: StickyConfig::default(),
            release_on: Vec::new(),
            extra: Map::new(),
        }
    }

    pub fn with_selector(mut self, selector: Selector) -> Self {
        self.selector = selector;
        self
    }

    pub fn with_sticky(mut self, sticky: StickyConfig) -> Self {
        self.sticky = sticky;
        self
    }

    pub fn with_release_on<I>(mut self, reasons: I) -> Self
    where
        I: IntoIterator<Item = ReleaseReason>,
    {
        self.release_on = reasons.into_iter().collect();
        self
    }

    pub fn contains(&self, channel: &ChannelId) -> bool {
        self.members.iter().any(|m| m == channel)
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

// ================================================================ Policy

impl Policy {
    /// 新策略：`enabled = true`、`priority = 1000`、空 match / 空 action。
    pub fn new(id: impl Into<PolicyId>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            enabled: true,
            priority: 1000,
            r#match: PolicyMatch::default(),
            action: PolicyAction::default(),
            extra: Map::new(),
        }
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_match(mut self, r#match: PolicyMatch) -> Self {
        self.r#match = r#match;
        self
    }

    pub fn with_action(mut self, action: PolicyAction) -> Self {
        self.action = action;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

impl PolicyMatch {
    /// `match: { all: true }`——兜底规则。
    pub fn match_all() -> Self {
        Self { all: true, ..Default::default() }
    }

    /// 是否"什么条件都没写"（这种规则永远命中不了，通常是想写 `all: true`）。
    pub fn is_unconstrained(&self) -> bool {
        !self.all
            && self.model_matches.is_empty()
            && self.has_image.is_none()
            && self.header_matches.is_empty()
            && self.ua_matches.is_none()
            && self.channel_tags_any.is_empty()
            && self.channel_tags_all.is_empty()
            && self.input_tokens_lt.is_none()
            && self.input_tokens_gt.is_none()
            && self.stream.is_none()
            && self.quota_remaining_lt.is_none()
            && self.time_window.is_none()
            && self.provider_in.is_empty()
    }
}

impl PolicyAction {
    /// 该动作是否真的会改候选集（空 action = 沿用上一级）。
    pub fn narrows_candidates(&self) -> bool {
        self.use_group.is_some()
            || !self.use_channels.is_empty()
            || !self.exclude_channels.is_empty()
            || self.require_tag.is_some()
            || self.exclude_tag.is_some()
            || self.pin_sticky.is_some()
            || self.use_lane.is_some()
    }

    pub fn references_group(&self, group: &ChannelGroupId) -> bool {
        self.use_group.as_ref() == Some(group)
    }

    pub fn references_lane(&self, lane: &LaneId) -> bool {
        self.use_lane.as_ref() == Some(lane)
    }

    /// 是否点名/排除了某个通道（`use_channels` / `exclude_channels` / `pin_sticky`）。
    pub fn references_channel(&self, channel: &ChannelId) -> bool {
        self.use_channels.iter().any(|c| c == channel)
            || self.exclude_channels.iter().any(|c| c == channel)
            || self.pin_sticky.as_ref() == Some(channel)
    }
}

// ================================================================ 定价

impl PriceMatch {
    /// 空匹配——**会吃掉所有匹配**，校验器会拒绝（[02 §2.9] 的定价条目必须有限定字段）。
    pub fn unconstrained() -> Self {
        Self::default()
    }

    pub fn model(model: impl Into<String>) -> Self {
        Self { model: Some(model.into()), ..Default::default() }
    }

    pub fn provider(provider: impl Into<ProviderId>) -> Self {
        Self { provider: Some(provider.into()), ..Default::default() }
    }

    pub fn credential(credential: impl Into<CredentialId>) -> Self {
        Self { credential: Some(credential.into()), ..Default::default() }
    }

    pub fn with_provider(mut self, provider: impl Into<ProviderId>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_credential(mut self, credential: impl Into<CredentialId>) -> Self {
        self.credential = Some(credential.into());
        self
    }

    /// 至少有一个限定字段（`model` / `provider` / `credential`）。
    pub fn has_qualifier(&self) -> bool {
        let present =
            |s: &Option<String>| s.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false);
        present(&self.model) || self.provider.is_some() || self.credential.is_some()
    }

    /// 反例：没有任何限定字段 → 会匹配所有请求。
    pub fn is_unconstrained(&self) -> bool {
        !self.has_qualifier()
    }

    /// 这条定价是否命中某次路由结果。`model` 支持 [`matches_glob`]（`*` / `?`）。
    pub fn matches(&self, model: &str, provider: &ProviderId, credential: &CredentialId) -> bool {
        let model_hit = match self.model.as_deref() {
            None => true,
            Some("") => true,
            Some(pattern) => matches_glob(pattern, model),
        };
        let provider_hit = self.provider.as_ref().map(|p| p == provider).unwrap_or(true);
        let credential_hit = self.credential.as_ref().map(|c| c == credential).unwrap_or(true);
        model_hit && provider_hit && credential_hit
    }
}

impl PriceEntry {
    /// 新定价条目：倍率 1.0、无费率、`source = builtin`。
    pub fn new(r#match: PriceMatch) -> Self {
        Self {
            r#match,
            rates: BTreeMap::new(),
            multiplier: 1.0,
            currency: None,
            plan_fee: None,
            source: PriceSource::Builtin,
            effective_from: None,
            extra: Map::new(),
        }
    }

    /// 单条费率（每 1M tokens）。内部按 [`TokenKind::as_key`] 存字符串键，
    /// 这样上游新增 token 类型不需要改结构体。
    pub fn with_rate(mut self, kind: TokenKind, price: f64) -> Self {
        self.rates.insert(kind.as_key().to_string(), price);
        self
    }

    pub fn with_rates<I>(mut self, rates: I) -> Self
    where
        I: IntoIterator<Item = (TokenKind, f64)>,
    {
        self.rates.extend(rates.into_iter().map(|(k, v)| (k.as_key().to_string(), v)));
        self
    }

    /// 直接用 `token_kind` 字符串键（`"input"` / `"output"` / 上游的新类型）。
    pub fn with_rate_key(mut self, kind: impl Into<String>, price: f64) -> Self {
        self.rates.insert(kind.into(), price);
        self
    }

    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = multiplier;
        self
    }

    pub fn with_currency(mut self, currency: impl Into<String>) -> Self {
        self.currency = Some(currency.into());
        self
    }

    pub fn with_plan_fee(mut self, fee: PlanFee) -> Self {
        self.plan_fee = Some(fee);
        self
    }

    pub fn with_source(mut self, source: PriceSource) -> Self {
        self.source = source;
        self
    }

    pub fn has_rates(&self) -> bool {
        !self.rates.is_empty()
    }
}

impl PlanFee {
    pub fn new(amount: f64, currency: impl Into<String>, window: QuotaWindow) -> Self {
        Self { amount, currency: currency.into(), window, extra: Map::new() }
    }
}

/// `PlanFee` 的字段都是必填的，但 `..Default::default()` 在构造测试数据时非常常用
/// （`pricing` 包也在用），所以这里补一个语义明确的默认值：0 元 / USD / 按月。
impl Default for PlanFee {
    fn default() -> Self {
        Self::new(0.0, "USD", QuotaWindow::Month)
    }
}

// ================================================================ Subscription

impl Subscription {
    /// 新订阅：`enabled = true`、UA 伪装成 Clash 客户端（[02 §2.4]）。
    pub fn new(
        id: impl Into<SubscriptionId>,
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            url: url.into(),
            user_agent: "clash-verge/v2.0.0".into(),
            format: SubscriptionFormat::Auto,
            interval_s: 6 * 3600,
            keep_versions: 3,
            filter: SubscriptionFilter::default(),
            health_check: SubscriptionHealthCheck::default(),
            enabled: true,
            provider_key: None,
            extra: Map::new(),
        }
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn with_filter(mut self, filter: SubscriptionFilter) -> Self {
        self.filter = filter;
        self
    }

    /// 内核里的 provider key（由 id 派生，稳定）。
    pub fn with_provider_key(mut self, key: impl Into<String>) -> Self {
        self.provider_key = Some(key.into());
        self
    }

    pub fn with_health_check(mut self, health_check: SubscriptionHealthCheck) -> Self {
        self.health_check = health_check;
        self
    }

    pub fn has_provider_key(&self) -> bool {
        self.provider_key.as_deref().map(|k| !k.trim().is_empty()).unwrap_or(false)
    }
}

// ================================================================ 从属辅助

impl RequestLogConfig {
    /// `retention` 转成运行态枚举（`RetentionDefault` 只是为了让 `Default` 能用）。
    pub fn effective_retention(&self) -> Retention {
        self.retention.into()
    }
}

impl RetryConfig {
    /// 流式请求用 `stream_max_attempts`，非流式用 `max_attempts`。
    pub fn attempts_for(&self, stream: bool) -> u32 {
        if stream {
            self.stream_max_attempts.max(1)
        } else {
            self.max_attempts.max(1)
        }
    }
}

impl GatewayConfig {
    /// 是否要求客户端带本地密钥。
    pub fn requires_local_key(&self) -> bool {
        self.local_key.as_deref().map(|k| !k.trim().is_empty()).unwrap_or(false)
    }
}

impl NetConfig {
    /// `lane_port_range` 能容纳几条 lane 入站。
    pub fn lane_port_capacity(&self) -> usize {
        let [start, end] = self.kernel.lane_port_range;
        if end < start {
            0
        } else {
            (end - start) as usize + 1
        }
    }

    /// 需要占端口（非 `direct`）且启用的 lane 数量。
    pub fn lanes_needing_port(&self, lanes: &[Lane]) -> usize {
        lanes.iter().filter(|l| l.enabled && l.kind.needs_subscription()).count()
    }
}

// ================================================================ 开机自启动（[09 §1.2] E1–E5）

/// 启动延迟上限（秒）。见 [02 §2.10]（0–300）。
///
/// 由 `config::validator` 强制：一个手滑写成 `3000` 的配置不会报错，
/// 只会让用户看到"开机后快一小时才启动"，然后怀疑软件坏了。
pub const MAX_AUTOSTART_DELAY_S: u32 = 300;

/// 自启动的**期望状态**（配置）与**系统实际状态**的比对结果（[09 §1.2] E3/E4）。
///
/// 对应 `AutostartState.drifted`（[10 §2.2]）。
///
/// 注意：**只是判定结果，不带任何动作**：`MissingOnSystem` 不等于"该去注册"、
/// `UnexpectedOnSystem` 不等于"该去删除"。动作只能由用户点击
/// `app_set_autostart` / `app_repair_autostart` 触发 —— 用户手动禁用后
/// 我们**绝不偷偷加回来**（E3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartDrift {
    /// 期望与实际一致。
    InSync,
    /// 期望开、系统里没有 —— UI 必须提示，**绝不自动注册**。
    MissingOnSystem,
    /// 期望关、系统里却有 —— UI 必须提示，**绝不自动删除**。
    UnexpectedOnSystem,
    /// 平台不支持（Linux 无桌面会话等）—— 开关置灰 + 显示原因。
    Unsupported,
}

impl AutostartDrift {
    /// 配置与系统不一致（直接对应 `AutostartState.drifted`）。
    pub fn is_drifted(&self) -> bool {
        matches!(self, AutostartDrift::MissingOnSystem | AutostartDrift::UnexpectedOnSystem)
    }

    /// 平台不支持 —— UI 置灰，**不报成"漂移"**。
    pub fn is_unsupported(&self) -> bool {
        matches!(self, AutostartDrift::Unsupported)
    }

    /// 日志 / 事件（`TaEvent`）用的稳定字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            AutostartDrift::InSync => "in_sync",
            AutostartDrift::MissingOnSystem => "missing_on_system",
            AutostartDrift::UnexpectedOnSystem => "unexpected_on_system",
            AutostartDrift::Unsupported => "unsupported",
        }
    }
}

/// 判定自启动的期望状态与系统实际状态是否漂移（[09 §1.2] E3/E4）。
///
/// **纯函数：只判定，不做任何动作。** 调用方（T2）也不得据此自动写注册表或删条目：
/// 用户手动禁用后我们绝不偷偷加回来（E3），用户配了没生效也不替他做主（E4）——
/// 只在 UI 里显示漂移并给「重新注册 / 清除」按钮。
///
/// `supported = false`（Linux 无桌面会话、非桌面环境等）**优先于一切**：
/// 此时既不返回 `MissingOnSystem` 也不返回 `UnexpectedOnSystem`，否则 UI 会显示
/// "系统里没有 → 点重新注册"，而用户点了也不会成功（[10 §2.2] 规则 5）。
pub fn evaluate_autostart_drift(desired: bool, actual: bool, supported: bool) -> AutostartDrift {
    if !supported {
        return AutostartDrift::Unsupported;
    }
    match (desired, actual) {
        (true, true) | (false, false) => AutostartDrift::InSync,
        (true, false) => AutostartDrift::MissingOnSystem,
        (false, true) => AutostartDrift::UnexpectedOnSystem,
    }
}

// ================================================================ ConfigDoc 索引

impl ConfigDoc {
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id.as_str() == id)
    }

    pub fn credential(&self, id: &str) -> Option<&Credential> {
        self.credentials.iter().find(|c| c.id.as_str() == id)
    }

    pub fn lane(&self, id: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.id.as_str() == id)
    }

    pub fn channel(&self, id: &str) -> Option<&ChannelConfig> {
        self.channels.iter().find(|c| c.id.as_str() == id)
    }

    pub fn channel_group(&self, id: &str) -> Option<&ChannelGroup> {
        self.channel_groups.iter().find(|g| g.id.as_str() == id)
    }

    pub fn policy(&self, id: &str) -> Option<&Policy> {
        self.policies.iter().find(|p| p.id.as_str() == id)
    }

    pub fn subscription(&self, id: &str) -> Option<&Subscription> {
        self.proxy_subscriptions.iter().find(|s| s.id.as_str() == id)
    }

    /// 逻辑模型查找：id 或任一 alias 都能命中。
    pub fn find_model(&self, name: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.matches(name))
    }

    pub fn has_model(&self, name: &str) -> bool {
        self.find_model(name).is_some()
    }

    /// 挂在这个逻辑模型（id 或 alias）下的全部通道。
    pub fn channels_for_model<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a ChannelConfig> + 'a {
        self.channels
            .iter()
            .filter(move |c| self.find_model(name).map(|m| m.matches(&c.model)).unwrap_or(false))
    }

    /// 生效策略表：只保留 `enabled`，按 `priority` 升序（同优先级保持声明顺序）。
    ///
    /// [02 §2.8]：数字小的先求值，**首个命中即生效**。
    pub fn enabled_policies_in_order(&self) -> Vec<&Policy> {
        let mut out: Vec<&Policy> = self.policies.iter().filter(|p| p.enabled).collect();
        out.sort_by_key(|p| p.priority);
        out
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> ConfigDoc {
        crate::config::default_doc()
    }

    #[test]
    fn glob_matches_star_and_question() {
        assert!(matches_glob("gpt-4o*", "gpt-4o-mini"));
        assert!(matches_glob("gpt-4o*", "gpt-4o"));
        assert!(matches_glob("*sonnet*", "claude-sonnet-4-5"));
        assert!(matches_glob("claude-?-5", "claude-4-5"));
        assert!(matches_glob("*", "anything"));
        assert!(matches_glob("", ""));
        assert!(!matches_glob("", "x"));
        assert!(!matches_glob("gpt-4o", "gpt-4o-mini"));
        assert!(!matches_glob("gpt-5*", "gpt-4o"));
        assert!(matches_glob("a*b*c", "aXXbYYc"));
        assert!(!matches_glob("a*b*c", "aXXcYYb"));
    }

    #[test]
    fn model_matches_id_and_aliases() {
        let m = Model::new("gpt-4o").with_aliases(["gpt-4o-2024-11-20", "openai/gpt-4o"]);
        assert!(m.matches("gpt-4o"));
        assert!(m.matches("gpt-4o-2024-11-20"));
        assert!(!m.matches("gpt-4o-mini"));
        assert_eq!(m.names().count(), 3);
    }

    #[test]
    fn lane_kind_required_fields() {
        assert_eq!(Lane::direct("l", "d").missing_kind_field(), None);
        assert_eq!(
            Lane::new("l", "a", LaneKind::SubscriptionAuto).missing_kind_field(),
            Some("subscription")
        );
        assert_eq!(Lane::subscription_group("l", "g", "sub", "🇯🇵 日本").missing_kind_field(), None);
        assert_eq!(Lane::subscription_node("l", "n", "sub", "东京-03").missing_kind_field(), None);
        assert_eq!(Lane::filtered("l", "f", "sub", "(?i)jp").missing_kind_field(), None);
        // kind=filter 但只给了 subscription，没有正则
        let mut lane = Lane::subscription_auto("l", "f", "sub");
        lane.kind = LaneKind::Filter;
        assert_eq!(lane.missing_kind_field(), Some("filter"));
    }

    #[test]
    fn channel_effective_lane_levels() {
        let net =
            NetConfig { default_lane: Some(LaneId::new("lane-default")), ..Default::default() };
        let provider =
            Provider::new("p", "P", Protocol::Openai, "https://x/v1").with_lane("lane-provider");
        let credential = Credential::new("c", "p", "C", "${secret:c}").with_lane("lane-credential");
        let bare_credential = Credential::new("c", "p", "C", "${secret:c}");
        let bare_provider = Provider::new("p", "P", Protocol::Openai, "https://x/v1");

        let mut channel = ChannelConfig::new("ch", "gpt-4o", "p", "c", "gpt-4o");

        // 1) 凭据/服务商都没配 → net.default_lane
        assert_eq!(
            channel.effective_lane(Some(&bare_credential), Some(&bare_provider), &net),
            Some(&LaneId::new("lane-default"))
        );

        // 2) provider.lane 覆盖 net.default_lane
        assert_eq!(
            channel.effective_lane(Some(&bare_credential), Some(&provider), &net),
            Some(&LaneId::new("lane-provider"))
        );

        // 3) credential.lane 覆盖 provider.lane
        assert_eq!(
            channel.effective_lane(Some(&credential), Some(&provider), &net),
            Some(&LaneId::new("lane-credential"))
        );

        // 4) channel.lane 优先级最高
        channel.lane = Some(LaneId::new("lane-channel"));
        assert_eq!(
            channel.effective_lane(Some(&credential), Some(&provider), &net),
            Some(&LaneId::new("lane-channel"))
        );

        // 5) 五级里前四级都没写、net 也没有 → None（由 resolve_lane 兜底成 direct）
        channel.lane = None;
        assert_eq!(
            channel.effective_lane(
                Some(&bare_credential),
                Some(&bare_provider),
                &NetConfig::default()
            ),
            None
        );
    }

    #[test]
    fn resolve_lane_policy_wins_and_direct_fallback() {
        let net = NetConfig::default();
        let channel = ChannelConfig::new("ch", "gpt-4o", "p", "c", "gpt-4o");
        let lane = LaneId::new("lane-policy");
        let resolved = resolve_lane(Some(&lane), &channel, None, None, &net);
        assert_eq!(resolved, LaneResolution::Lane(&lane));
        assert_eq!(resolved.as_str(), "lane-policy");

        let direct = resolve_lane(None, &channel, None, None, &net);
        assert!(direct.is_direct());
        assert_eq!(direct.as_str(), "direct");
    }

    #[test]
    fn config_doc_index_helpers() {
        let doc = doc();
        assert_eq!(doc.lane("lane-direct").map(|l| l.name.as_str()), Some("直连（本机网络）"));
        assert!(doc.find_model("gpt-4o").is_some());
        assert!(doc.find_model("openai/gpt-4o").is_some());
        assert!(doc.find_model("nope").is_none());
        assert_eq!(doc.channels_for_model("gpt-4o").count(), 3);
        let order: Vec<&str> =
            doc.enabled_policies_in_order().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["pol-default"]);
    }

    #[test]
    fn price_match_qualifier_and_matching() {
        assert!(PriceMatch::unconstrained().is_unconstrained());
        assert!(PriceMatch::model("gpt-4o*").has_qualifier());

        let m = PriceMatch::model("gpt-4o*").with_provider("p-mytoken");
        assert!(m.matches("gpt-4o-mini", &ProviderId::new("p-mytoken"), &CredentialId::new("c")));
        assert!(!m.matches("gpt-4o-mini", &ProviderId::new("p-openai"), &CredentialId::new("c")));
        assert!(!m.matches("claude-3", &ProviderId::new("p-mytoken"), &CredentialId::new("c")));
    }

    #[test]
    fn net_port_capacity() {
        let net = doc().net;
        assert_eq!(net.lane_port_capacity(), 200);
        assert_eq!(net.lanes_needing_port(&doc().lanes), 3);
    }

    #[test]
    fn retry_attempts_by_stream() {
        let retry = RetryConfig::default();
        assert_eq!(retry.attempts_for(false), 3);
        assert_eq!(retry.attempts_for(true), 2);
    }

    // ------------------------------------------------------------ 自启动漂移判定（E3/E4）

    #[test]
    fn autostart_drift_four_branches() {
        // 期望与实际一致（两种都算一致）
        assert_eq!(evaluate_autostart_drift(true, true, true), AutostartDrift::InSync);
        assert_eq!(evaluate_autostart_drift(false, false, true), AutostartDrift::InSync);
        // 期望开、系统里没有 → 提示，但绝不自动注册
        assert_eq!(evaluate_autostart_drift(true, false, true), AutostartDrift::MissingOnSystem);
        // 期望关、系统里却有 → 提示，但绝不自动删除
        assert_eq!(evaluate_autostart_drift(false, true, true), AutostartDrift::UnexpectedOnSystem);
    }

    #[test]
    fn autostart_drift_unsupported_wins_over_everything() {
        // 平台不支持优先于一切：否则 UI 会显示"系统里没有 → 点重新注册"，而点了也不会成功。
        for (desired, actual) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                evaluate_autostart_drift(desired, actual, false),
                AutostartDrift::Unsupported,
                "desired={desired} actual={actual} 都应当是 Unsupported"
            );
        }
    }

    #[test]
    fn autostart_drift_helpers_and_labels() {
        let cases = [
            (AutostartDrift::InSync, false, false, "in_sync"),
            (AutostartDrift::MissingOnSystem, true, false, "missing_on_system"),
            (AutostartDrift::UnexpectedOnSystem, true, false, "unexpected_on_system"),
            (AutostartDrift::Unsupported, false, true, "unsupported"),
        ];
        for (drift, drifted, unsupported, label) in cases {
            assert_eq!(drift.is_drifted(), drifted, "{label}");
            assert_eq!(drift.is_unsupported(), unsupported, "{label}");
            assert_eq!(drift.as_str(), label);
        }
    }

    #[test]
    fn autostart_delay_max_matches_doc() {
        // [02 §2.10]：0–300 秒。校验器引用这个常量，别各自写死。
        assert_eq!(MAX_AUTOSTART_DELAY_S, 300);
    }
}

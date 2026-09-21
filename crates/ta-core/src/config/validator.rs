//! 配置校验：[`TaConfigValidator`]。
//!
//! 分两层（见 [11 §1.2]、[02 §4]）：
//!
//! - **形状层**：类型/枚举由反序列化强制（`from_yaml` / `from_json` + `serde_path_to_error`），
//!   结构由 `schemars::schema_for!(ConfigDoc)` 导出给前端（[`super::config_json_schema`]）。
//!   JSON Schema 表达不了的数值域 / 非空 / 正则规则在这里手写（[`check_shape`]）。
//! - **语义层**：引用完整性、ID 唯一性、定价匹配约束、以及不阻断的警告，全部手写。
//!
//! 每条错误/警告都带**精确字段路径**，如 `channels[2].credential`、
//! `channel_groups[0].members[1]`、`pricing.entries[3].match`。
//! 反例见本文件底部的单测——每条规则都至少有一个正例 + 一个反例。

use std::collections::BTreeMap;

use regex::Regex;

use super::migrate::CURRENT_CONFIG_VERSION;
use super::{ConfigValidator, ValidationReport};
use crate::domain::*;

/// `FieldError.code` 的取值。前端按 code 做文案与"跳到哪个字段"。
pub mod codes {
    /// 必填字段缺失 / 为空
    pub const MISSING_FIELD: &str = "missing_field";
    /// 引用不存在（引用完整性）
    pub const NOT_FOUND: &str = "not_found";
    /// 同一集合内 ID 重复
    pub const DUPLICATE_ID: &str = "duplicate_id";
    /// 模型别名与别的 id/别名撞名 → 路由结果不确定
    pub const AMBIGUOUS_ALIAS: &str = "ambiguous_alias";
    /// 定价条目的 match 没有任何限定字段 → 会吃掉所有匹配
    pub const UNCONSTRAINED_MATCH: &str = "unconstrained_match";
    /// 数值/取值不在合法域内（含空正则这种形态问题）
    pub const INVALID_VALUE: &str = "invalid_value";
    /// 正则不合法
    pub const INVALID_REGEX: &str = "invalid_regex";
    /// 配置版本高于本程序
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// 警告：引用的对象当前被禁用
    pub const DISABLED_REF: &str = "disabled_ref";
    /// 警告：模型没有任何定价覆盖
    pub const UNPRICED_MODEL: &str = "unpriced_model";
    /// 通道组成员为空
    pub const EMPTY_MEMBERS: &str = "empty_members";
}

/// 默认校验器。无状态、`Send + Sync`，可以直接放进 `AppState`。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TaConfigValidator;

impl TaConfigValidator {
    pub fn new() -> Self {
        Self
    }
}

impl ConfigValidator for TaConfigValidator {
    fn validate(&self, doc: &ConfigDoc) -> ValidationReport {
        validate_doc(doc)
    }
}

/// 便捷入口：等价于 `TaConfigValidator::new().validate(doc)`。
pub fn validate_doc(doc: &ConfigDoc) -> ValidationReport {
    let mut report = ValidationReport::ok();
    let index = Index::build(doc, &mut report);
    check_version(doc, &mut report);
    check_shape(doc, &mut report);
    check_references(doc, &index, &mut report);
    check_warnings(doc, &index, &mut report);
    report
}

// ================================================================ 索引（唯一性 + 引用解析）

#[derive(Debug, Default)]
struct Index<'a> {
    providers: BTreeMap<&'a str, usize>,
    credentials: BTreeMap<&'a str, usize>,
    lanes: BTreeMap<&'a str, usize>,
    channels: BTreeMap<&'a str, usize>,
    groups: BTreeMap<&'a str, usize>,
    subscriptions: BTreeMap<&'a str, usize>,
    /// 逻辑模型的 id + 全部 alias → models 下标
    models: BTreeMap<&'a str, usize>,
}

impl<'a> Index<'a> {
    fn build(doc: &'a ConfigDoc, r: &mut ValidationReport) -> Self {
        let mut ix = Self::default();

        for (i, p) in doc.providers.iter().enumerate() {
            insert_unique(
                &mut ix.providers,
                p.id.as_str(),
                i,
                format!("providers[{i}].id"),
                "providers",
                r,
            );
        }
        for (i, c) in doc.credentials.iter().enumerate() {
            insert_unique(
                &mut ix.credentials,
                c.id.as_str(),
                i,
                format!("credentials[{i}].id"),
                "credentials",
                r,
            );
        }
        for (i, l) in doc.lanes.iter().enumerate() {
            insert_unique(&mut ix.lanes, l.id.as_str(), i, format!("lanes[{i}].id"), "lanes", r);
        }
        for (i, c) in doc.channels.iter().enumerate() {
            insert_unique(
                &mut ix.channels,
                c.id.as_str(),
                i,
                format!("channels[{i}].id"),
                "channels",
                r,
            );
        }
        for (i, g) in doc.channel_groups.iter().enumerate() {
            insert_unique(
                &mut ix.groups,
                g.id.as_str(),
                i,
                format!("channel_groups[{i}].id"),
                "channel_groups",
                r,
            );
        }
        // 策略与 provider_key 不参与"引用解析"，只需要唯一性检查 → 用局部表。
        let mut seen_policies: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, p) in doc.policies.iter().enumerate() {
            insert_unique(
                &mut seen_policies,
                p.id.as_str(),
                i,
                format!("policies[{i}].id"),
                "policies",
                r,
            );
        }
        let mut seen_provider_keys: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, s) in doc.proxy_subscriptions.iter().enumerate() {
            insert_unique(
                &mut ix.subscriptions,
                s.id.as_str(),
                i,
                format!("proxy_subscriptions[{i}].id"),
                "proxy_subscriptions",
                r,
            );
            // 内核 `proxy-providers` 的 key 必须全局唯一，否则两份订阅互相覆盖
            if let Some(key) = s.provider_key.as_deref().filter(|k| !k.trim().is_empty()) {
                insert_unique(
                    &mut seen_provider_keys,
                    key,
                    i,
                    format!("proxy_subscriptions[{i}].provider_key"),
                    "proxy_subscriptions 的 provider_key",
                    r,
                );
            }
        }
        for (i, m) in doc.models.iter().enumerate() {
            insert_unique(&mut ix.models, m.id.as_str(), i, format!("models[{i}].id"), "models", r);
            for (j, alias) in m.aliases.iter().enumerate() {
                if alias.trim().is_empty() {
                    continue; // 空别名由形状检查报错
                }
                if let Some(prev) = ix.models.get(alias.as_str()) {
                    r.push_error(
                        format!("models[{i}].aliases[{j}]"),
                        codes::AMBIGUOUS_ALIAS,
                        format!(
                            "别名 `{alias}` 与 models[{prev}] 的 id/别名重复，`channel.model` 的解析结果不确定"
                        ),
                    );
                } else {
                    ix.models.insert(alias.as_str(), i);
                }
            }
        }

        ix
    }
}

fn insert_unique<'a>(
    map: &mut BTreeMap<&'a str, usize>,
    key: &'a str,
    index: usize,
    path: String,
    what: &str,
    r: &mut ValidationReport,
) {
    match map.get(key) {
        Some(prev) => r.push_error(
            path,
            codes::DUPLICATE_ID,
            format!("{what} 里 id `{key}` 重复（首次出现在第 {prev} 项）；ID 必须全局唯一"),
        ),
        None => {
            map.insert(key, index);
        }
    }
}

// ================================================================ 版本

fn check_version(doc: &ConfigDoc, r: &mut ValidationReport) {
    if doc.version == 0 {
        r.push_error("version", codes::UNSUPPORTED_VERSION, "version 必须 >= 1");
    } else if doc.version > CURRENT_CONFIG_VERSION {
        r.push_error(
            "version",
            codes::UNSUPPORTED_VERSION,
            format!(
                "配置版本 {} 高于本程序支持的 v{}，请升级 TokenAgency（不会自动降级）",
                doc.version, CURRENT_CONFIG_VERSION
            ),
        );
    }
}

// ================================================================ 形状（JSON Schema 表达不了的部分）

fn check_shape(doc: &ConfigDoc, r: &mut ValidationReport) {
    check_gateway(&doc.gateway, r);
    check_request_log(&doc.request_log, r);
    check_pricing(&doc.pricing, r);
    check_net(&doc.net, doc, r);
    check_providers(doc, r);
    check_credentials(doc, r);
    check_subscriptions(doc, r);
    check_models(doc, r);
    check_channels(doc, r);
    check_channel_groups(doc, r);
    check_policies(doc, r);
    check_lanes(doc, r);
    check_system_integration(&doc.system_integration, r);
}

fn check_gateway(g: &GatewayConfig, r: &mut ValidationReport) {
    check_listen(&g.listen, "gateway.listen", r);
    check_listen(&g.admin_listen, "gateway.admin_listen", r);
    if g.max_body_mb == 0 {
        r.push_error("gateway.max_body_mb", codes::INVALID_VALUE, "max_body_mb 必须 > 0");
    }
    if g.local_key.as_deref().map(|k| k.trim().is_empty()).unwrap_or(false) {
        r.push_error(
            "gateway.local_key",
            codes::INVALID_VALUE,
            "要么删掉该字段（= 不校验本地密钥），要么给一个非空值",
        );
    }
    if g.retry.max_attempts == 0 {
        r.push_error("gateway.retry.max_attempts", codes::INVALID_VALUE, "max_attempts 必须 >= 1");
    }
    if g.retry.stream_max_attempts == 0 {
        r.push_error(
            "gateway.retry.stream_max_attempts",
            codes::INVALID_VALUE,
            "stream_max_attempts 必须 >= 1",
        );
    }
    if g.retry.stream_max_attempts > g.retry.max_attempts {
        r.push_warning(
            "gateway.retry.stream_max_attempts",
            codes::INVALID_VALUE,
            "流式重试次数多于非流式：客户端会等很久（建议 stream <= 非流式）",
        );
    }
}

fn check_request_log(cfg: &RequestLogConfig, r: &mut ValidationReport) {
    if !cfg.sample_rate.is_finite() || cfg.sample_rate < 0.0 || cfg.sample_rate > 1.0 {
        r.push_error(
            "request_log.sample_rate",
            codes::INVALID_VALUE,
            format!("sample_rate 必须在 0.0 ~ 1.0 之间，实际 {}", cfg.sample_rate),
        );
    }
    if cfg.max_size_mb == 0 {
        r.push_error("request_log.max_size_mb", codes::INVALID_VALUE, "max_size_mb 必须 > 0");
    }
}

fn check_pricing(p: &PricingConfig, r: &mut ValidationReport) {
    if p.currency.trim().is_empty() {
        r.push_error("pricing.currency", codes::MISSING_FIELD, "currency 不能为空");
    }
    if let Some(rate) = p.usd_to_cny {
        if !rate.is_finite() || rate <= 0.0 {
            r.push_error("pricing.usd_to_cny", codes::INVALID_VALUE, "usd_to_cny 必须是正数");
        }
    }
    for (i, e) in p.entries.iter().enumerate() {
        // ★ 没有任何限定字段的 match 会命中所有请求，后面的条目全成死代码
        if e.r#match.is_unconstrained() {
            r.push_error(
                format!("pricing.entries[{i}].match"),
                codes::UNCONSTRAINED_MATCH,
                "match 至少要有一个限定字段（model / provider / credential），否则会吃掉所有匹配",
            );
        }
        if !e.multiplier.is_finite() || e.multiplier <= 0.0 {
            r.push_error(
                format!("pricing.entries[{i}].multiplier"),
                codes::INVALID_VALUE,
                format!("multiplier 必须是正数，实际 {}", e.multiplier),
            );
        }
        for (kind, price) in &e.rates {
            if !price.is_finite() || *price < 0.0 {
                r.push_error(
                    format!("pricing.entries[{i}].rates.{kind}"),
                    codes::INVALID_VALUE,
                    format!("费率不能为负或 NaN，实际 {price}"),
                );
            }
        }
        if let Some(fee) = &e.plan_fee {
            if !fee.amount.is_finite() || fee.amount < 0.0 {
                r.push_error(
                    format!("pricing.entries[{i}].plan_fee.amount"),
                    codes::INVALID_VALUE,
                    "套餐费不能为负",
                );
            }
            if fee.currency.trim().is_empty() {
                r.push_error(
                    format!("pricing.entries[{i}].plan_fee.currency"),
                    codes::MISSING_FIELD,
                    "套餐费必须写币种（如 CNY / USD）",
                );
            }
        }
    }
}

fn check_net(net: &NetConfig, doc: &ConfigDoc, r: &mut ValidationReport) {
    check_listen(&net.kernel.controller, "net.kernel.controller", r);

    let [start, end] = net.kernel.lane_port_range;
    if start == 0 {
        r.push_error("net.kernel.lane_port_range", codes::INVALID_VALUE, "端口不能是 0");
    } else if end < start {
        r.push_error(
            "net.kernel.lane_port_range",
            codes::INVALID_VALUE,
            format!("区间上界 {end} 小于下界 {start}"),
        );
    } else {
        let capacity = (end - start) as usize + 1;
        let needed = net.lanes_needing_port(&doc.lanes);
        if capacity < needed {
            r.push_error(
                "net.kernel.lane_port_range",
                codes::INVALID_VALUE,
                format!(
                    "端口范围只有 {capacity} 个端口，但有 {needed} 条非直连 lane 需要内核入站端口"
                ),
            );
        }
    }

    if net.dns.nameserver.is_empty() {
        r.push_error("net.dns.nameserver", codes::MISSING_FIELD, "至少要配一个 DoH/DNS 服务器");
    }
    if net.dns.proxy_server_nameserver.is_empty() {
        r.push_error(
            "net.dns.proxy_server_nameserver",
            codes::MISSING_FIELD,
            "★ 节点自身域名解析必须配，否则节点都连不上",
        );
    }
    if net.dns.default_nameserver.is_empty() {
        r.push_warning(
            "net.dns.default_nameserver",
            codes::MISSING_FIELD,
            "建议保留 bootstrap DNS（用于解析 DoH 域名本身）",
        );
    }
    if net.health.interval_s == 0 {
        r.push_error("net.health.interval_s", codes::INVALID_VALUE, "健康检查间隔必须 > 0");
    }
    if net.system_proxy || net.tun {
        r.push_warning(
            if net.tun { "net.tun" } else { "net.system_proxy" },
            codes::INVALID_VALUE,
            "整机接管属于高级模式，需要管理员权限；默认应当是关的",
        );
    }
}

fn check_providers(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, p) in doc.providers.iter().enumerate() {
        require_text(&p.id.0, &format!("providers[{i}].id"), "id", r);
        require_text(&p.name, &format!("providers[{i}].name"), "name", r);
        require_text(&p.base_url, &format!("providers[{i}].base_url"), "base_url", r);
        if !p.base_url.trim().is_empty() && !is_http_url(&p.base_url) {
            r.push_error(
                format!("providers[{i}].base_url"),
                codes::INVALID_VALUE,
                format!(
                    "`{}` 必须是 http(s):// 开头（写到版本段为止，如 https://api.openai.com/v1）",
                    p.base_url
                ),
            );
        }
        // header / query 鉴权必须有头名（如 x-api-key / api-key）
        if matches!(p.auth.scheme, AuthScheme::Header | AuthScheme::Query)
            && p.auth.header.as_deref().is_none_or(|h| h.trim().is_empty())
        {
            r.push_error(
                format!("providers[{i}].auth.header"),
                codes::MISSING_FIELD,
                "auth.scheme 为 header/query 时必须给 auth.header（如 x-api-key / api-key）",
            );
        }
        if p.timeout.connect_s == 0 {
            r.push_error(
                format!("providers[{i}].timeout.connect_s"),
                codes::INVALID_VALUE,
                "connect 超时必须 > 0",
            );
        }
        if p.timeout.read_s == 0 {
            r.push_error(
                format!("providers[{i}].timeout.read_s"),
                codes::INVALID_VALUE,
                "read 超时必须 > 0（流式要留足）",
            );
        }
        if p.headers.keys().any(|k| k.trim().is_empty()) {
            r.push_error(
                format!("providers[{i}].headers"),
                codes::INVALID_VALUE,
                "附加头不能有空名字",
            );
        }
        if let Some(prefix) = &p.model_prefix {
            if prefix.is_empty() {
                r.push_warning(
                    format!("providers[{i}].model_prefix"),
                    codes::INVALID_VALUE,
                    "空字符串前缀等于不写，建议直接删掉该字段",
                );
            }
        }
    }
}

fn check_credentials(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, c) in doc.credentials.iter().enumerate() {
        require_text(&c.id.0, &format!("credentials[{i}].id"), "id", r);
        require_text(&c.label, &format!("credentials[{i}].label"), "label", r);
        require_text(&c.secret, &format!("credentials[{i}].secret"), "secret", r);

        let q = &c.quota;
        if !q.soft_limit.is_finite() || q.soft_limit < 0.0 || q.soft_limit > 1.0 {
            r.push_error(
                format!("credentials[{i}].quota.soft_limit"),
                codes::INVALID_VALUE,
                format!("soft_limit 是 0~1 的比率，实际 {}", q.soft_limit),
            );
        }
        if !q.hard_limit.is_finite() || q.hard_limit <= 0.0 {
            r.push_error(
                format!("credentials[{i}].quota.hard_limit"),
                codes::INVALID_VALUE,
                format!("hard_limit 是 > 0 的比率，实际 {}", q.hard_limit),
            );
        }
        if q.soft_limit.is_finite() && q.hard_limit.is_finite() && q.soft_limit > q.hard_limit {
            r.push_error(
                format!("credentials[{i}].quota.soft_limit"),
                codes::INVALID_VALUE,
                "soft_limit（告警线）不能大于 hard_limit（耗尽线）",
            );
        }
        if !q.reserve.is_finite() || q.reserve < 0.0 || q.reserve >= 1.0 {
            r.push_error(
                format!("credentials[{i}].quota.reserve"),
                codes::INVALID_VALUE,
                format!("reserve 是 0~1 的比率，实际 {}", q.reserve),
            );
        }
        if q.is_manual() {
            match q.limit {
                None => r.push_error(
                    format!("credentials[{i}].quota.limit"),
                    codes::MISSING_FIELD,
                    "metric 不是 unknown 时必须给 limit（否则无法判定额度）",
                ),
                Some(limit) if !limit.is_finite() || limit <= 0.0 => r.push_error(
                    format!("credentials[{i}].quota.limit"),
                    codes::INVALID_VALUE,
                    format!("limit 必须 > 0，实际 {limit}"),
                ),
                _ => {}
            }
        }
        if q.reset_day == 0 || q.reset_day > 31 {
            r.push_error(
                format!("credentials[{i}].quota.reset_day"),
                codes::INVALID_VALUE,
                "reset_day 必须在 1 ~ 31",
            );
        }
        if c.guard.max_rpm == Some(0) {
            r.push_error(
                format!("credentials[{i}].guard.max_rpm"),
                codes::INVALID_VALUE,
                "max_rpm = 0 会让该凭据永远不可用",
            );
        }
        if c.guard.max_concurrency == Some(0) {
            r.push_error(
                format!("credentials[{i}].guard.max_concurrency"),
                codes::INVALID_VALUE,
                "max_concurrency = 0 会让该凭据永远不可用",
            );
        }
    }
}

fn check_subscriptions(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, s) in doc.proxy_subscriptions.iter().enumerate() {
        require_text(&s.id.0, &format!("proxy_subscriptions[{i}].id"), "id", r);
        require_text(&s.name, &format!("proxy_subscriptions[{i}].name"), "name", r);
        require_text(&s.url, &format!("proxy_subscriptions[{i}].url"), "url", r);
        if !s.url.trim().is_empty() && !is_http_url(&s.url) {
            r.push_error(
                format!("proxy_subscriptions[{i}].url"),
                codes::INVALID_VALUE,
                "订阅地址必须是 http(s)://",
            );
        }
        if s.user_agent.trim().is_empty() {
            r.push_error(
                format!("proxy_subscriptions[{i}].user_agent"),
                codes::INVALID_VALUE,
                "★ 很多机场按 UA 返回不同格式，不能留空",
            );
        }
        if s.interval_s == 0 {
            r.push_error(
                format!("proxy_subscriptions[{i}].interval_s"),
                codes::INVALID_VALUE,
                "更新周期必须 > 0 秒",
            );
        }
        if s.keep_versions == 0 {
            r.push_error(
                format!("proxy_subscriptions[{i}].keep_versions"),
                codes::INVALID_VALUE,
                "keep_versions 必须 >= 1（否则坏更新无法回滚）",
            );
        }
        check_regex(
            s.filter.include.as_deref(),
            format!("proxy_subscriptions[{i}].filter.include"),
            r,
        );
        check_regex(
            s.filter.exclude.as_deref(),
            format!("proxy_subscriptions[{i}].filter.exclude"),
            r,
        );
        if s.health_check.interval_s == 0 {
            r.push_error(
                format!("proxy_subscriptions[{i}].health_check.interval_s"),
                codes::INVALID_VALUE,
                "健康检查间隔必须 > 0",
            );
        }
    }
}

fn check_models(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, m) in doc.models.iter().enumerate() {
        require_text(&m.id, &format!("models[{i}].id"), "id", r);
        for (j, alias) in m.aliases.iter().enumerate() {
            if alias.trim().is_empty() {
                r.push_error(
                    format!("models[{i}].aliases[{j}]"),
                    codes::INVALID_VALUE,
                    "别名不能为空字符串",
                );
            }
        }
    }
}

fn check_channels(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, c) in doc.channels.iter().enumerate() {
        require_text(&c.id.0, &format!("channels[{i}].id"), "id", r);
        require_text(&c.model, &format!("channels[{i}].model"), "model", r);
        require_text(
            &c.upstream_model,
            &format!("channels[{i}].upstream_model"),
            "upstream_model",
            r,
        );
        if c.weight == 0 {
            r.push_error(
                format!("channels[{i}].weight"),
                codes::INVALID_VALUE,
                "weight = 0 会让 weighted_random 永远选不到这条通道",
            );
        }
        if c.transform.headers_add.keys().any(|k| k.trim().is_empty()) {
            r.push_error(
                format!("channels[{i}].transform.headers_add"),
                codes::INVALID_VALUE,
                "追加头不能有空名字",
            );
        }
        if c.transform.headers_override.keys().any(|k| k.trim().is_empty()) {
            r.push_error(
                format!("channels[{i}].transform.headers_override"),
                codes::INVALID_VALUE,
                "覆盖头不能有空名字",
            );
        }
        for (j, tag) in c.tags.iter().enumerate() {
            if tag.trim().is_empty() {
                r.push_error(
                    format!("channels[{i}].tags[{j}]"),
                    codes::INVALID_VALUE,
                    "标签不能为空字符串",
                );
            }
        }
    }
}

fn check_channel_groups(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, g) in doc.channel_groups.iter().enumerate() {
        require_text(&g.id.0, &format!("channel_groups[{i}].id"), "id", r);
        require_text(&g.model, &format!("channel_groups[{i}].model"), "model", r);

        if g.members.is_empty() {
            r.push_error(
                format!("channel_groups[{i}].members"),
                codes::EMPTY_MEMBERS,
                "通道组至少要有一个成员",
            );
        }
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (j, m) in g.members.iter().enumerate() {
            if let Some(prev) = seen.get(m.as_str()) {
                r.push_error(
                    format!("channel_groups[{i}].members[{j}]"),
                    codes::DUPLICATE_ID,
                    format!("成员 `{m}` 重复（第 {prev} 项已有），会让加权随机偏向它"),
                );
            } else {
                seen.insert(m.as_str(), j);
            }
        }
        if g.sticky.enabled && g.sticky.ttl_h == 0 {
            r.push_error(
                format!("channel_groups[{i}].sticky.ttl_h"),
                codes::INVALID_VALUE,
                "粘性开启时 ttl_h 必须 > 0（否则写入即过期）",
            );
        }
    }
}

fn check_policies(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, p) in doc.policies.iter().enumerate() {
        require_text(&p.id.0, &format!("policies[{i}].id"), "id", r);
        require_text(&p.name, &format!("policies[{i}].name"), "name", r);

        if p.enabled && p.r#match.is_unconstrained() {
            r.push_warning(
                format!("policies[{i}].match"),
                codes::INVALID_VALUE,
                "match 没有任何条件，这条规则永远不会命中（想兜底请写 all: true）",
            );
        }
        if let (Some(lt), Some(gt)) = (p.r#match.input_tokens_lt, p.r#match.input_tokens_gt) {
            if lt > gt {
                r.push_error(
                    format!("policies[{i}].match.input_tokens_lt"),
                    codes::INVALID_VALUE,
                    format!("input_tokens_lt（{lt}）不能大于 input_tokens_gt（{gt}）"),
                );
            }
        }
        if let Some(win) = p.r#match.time_window.as_deref() {
            if win.trim().is_empty() {
                r.push_error(
                    format!("policies[{i}].match.time_window"),
                    codes::INVALID_VALUE,
                    "time_window 不能为空（如 \"09:00-19:00\" / weekdays）",
                );
            }
        }
        if let Some(ua) = p.r#match.ua_matches.as_deref() {
            check_regex(Some(ua), format!("policies[{i}].match.ua_matches"), r);
        }
        if let Some(code) = p.action.error_code {
            if !(400..=599).contains(&code) {
                r.push_error(
                    format!("policies[{i}].action.error_code"),
                    codes::INVALID_VALUE,
                    format!("error_code 必须是 400~599 的 HTTP 状态码，实际 {code}"),
                );
            }
        }
        if let Some(f) = &p.action.failover {
            if f.max_attempts == 0 {
                r.push_error(
                    format!("policies[{i}].action.failover.max_attempts"),
                    codes::INVALID_VALUE,
                    "max_attempts 必须 >= 1",
                );
            }
            if f.on.is_empty() {
                r.push_warning(
                    format!("policies[{i}].action.failover.on"),
                    codes::MISSING_FIELD,
                    "failover.on 为空 → 任何错误都不会换通道",
                );
            }
        }
        if let Some(tag) = p.action.require_tag.as_deref() {
            if tag.trim().is_empty() {
                r.push_error(
                    format!("policies[{i}].action.require_tag"),
                    codes::INVALID_VALUE,
                    "require_tag 不能为空",
                );
            }
        }
        if let Some(tag) = p.action.exclude_tag.as_deref() {
            if tag.trim().is_empty() {
                r.push_error(
                    format!("policies[{i}].action.exclude_tag"),
                    codes::INVALID_VALUE,
                    "exclude_tag 不能为空",
                );
            }
        }
    }

    // 同优先级 → 求值顺序不确定，值得警告
    let mut by_priority: BTreeMap<i32, usize> = BTreeMap::new();
    for (i, p) in doc.policies.iter().enumerate() {
        if !p.enabled {
            continue;
        }
        if let Some(prev) = by_priority.get(&p.priority) {
            r.push_warning(
                format!("policies[{i}].priority"),
                codes::INVALID_VALUE,
                format!(
                    "与 policies[{prev}] 同为 priority {}：首个命中即生效，顺序将由声明顺序决定，建议拉开优先级",
                    p.priority
                ),
            );
        } else {
            by_priority.insert(p.priority, i);
        }
    }
}

fn check_lanes(doc: &ConfigDoc, r: &mut ValidationReport) {
    for (i, l) in doc.lanes.iter().enumerate() {
        require_text(&l.id.0, &format!("lanes[{i}].id"), "id", r);
        require_text(&l.name, &format!("lanes[{i}].name"), "name", r);

        if let Some(field) = l.missing_kind_field() {
            r.push_error(
                format!("lanes[{i}].{field}"),
                codes::MISSING_FIELD,
                format!("kind = `{}` 时必须给 `{field}`（见 04 §7.1）", l.kind.as_str()),
            );
        }
        check_regex(l.filter.as_deref(), format!("lanes[{i}].filter"), r);
        check_regex(l.exclude_filter.as_deref(), format!("lanes[{i}].exclude_filter"), r);
    }
}

fn check_system_integration(si: &SystemIntegration, r: &mut ValidationReport) {
    let path_ok = si.file_watch_path.as_deref().map(|p| !p.trim().is_empty()).unwrap_or(false);
    if si.file_watch_mode && !path_ok {
        r.push_error(
            "system_integration.file_watch_path",
            codes::MISSING_FIELD,
            "开启文件监听模式时必须给 file_watch_path",
        );
    }
    if !si.file_watch_mode && path_ok {
        r.push_warning(
            "system_integration.file_watch_path",
            codes::INVALID_VALUE,
            "未开启文件监听模式，这个路径不会生效",
        );
    }
}

// ================================================================ 引用完整性

fn check_references(doc: &ConfigDoc, ix: &Index<'_>, r: &mut ValidationReport) {
    // ---- 网关级
    if let Some(p) = &doc.gateway.fallback_provider {
        ref_exists(&ix.providers, p.as_str(), "gateway.fallback_provider", "providers[].id", r);
    }
    if let Some(l) = &doc.net.default_lane {
        ref_exists(&ix.lanes, l.as_str(), "net.default_lane", "lanes[].id", r);
    }

    // ---- 服务商
    for (i, p) in doc.providers.iter().enumerate() {
        if let Some(l) = &p.lane {
            ref_exists(&ix.lanes, l.as_str(), &format!("providers[{i}].lane"), "lanes[].id", r);
        }
    }

    // ---- 凭据
    for (i, c) in doc.credentials.iter().enumerate() {
        ref_exists(
            &ix.providers,
            c.provider.as_str(),
            &format!("credentials[{i}].provider"),
            "providers[].id",
            r,
        );
        if let Some(l) = &c.lane {
            ref_exists(&ix.lanes, l.as_str(), &format!("credentials[{i}].lane"), "lanes[].id", r);
        }
    }

    // ---- 线路 → 订阅
    for (i, l) in doc.lanes.iter().enumerate() {
        if let Some(s) = &l.subscription {
            ref_exists(
                &ix.subscriptions,
                s.as_str(),
                &format!("lanes[{i}].subscription"),
                "proxy_subscriptions[].id",
                r,
            );
        }
    }

    // ---- 通道
    for (i, c) in doc.channels.iter().enumerate() {
        ref_exists(
            &ix.providers,
            c.provider.as_str(),
            &format!("channels[{i}].provider"),
            "providers[].id",
            r,
        );
        ref_exists(
            &ix.credentials,
            c.credential.as_str(),
            &format!("channels[{i}].credential"),
            "credentials[].id",
            r,
        );
        if let Some(l) = &c.lane {
            ref_exists(&ix.lanes, l.as_str(), &format!("channels[{i}].lane"), "lanes[].id", r);
        }
        ref_exists(
            &ix.models,
            c.model.as_str(),
            &format!("channels[{i}].model"),
            "models[].id 或 models[].aliases",
            r,
        );
    }

    // ---- 通道组
    for (i, g) in doc.channel_groups.iter().enumerate() {
        ref_exists(
            &ix.models,
            g.model.as_str(),
            &format!("channel_groups[{i}].model"),
            "models[].id 或 models[].aliases",
            r,
        );
        for (j, m) in g.members.iter().enumerate() {
            ref_exists(
                &ix.channels,
                m.as_str(),
                &format!("channel_groups[{i}].members[{j}]"),
                "channels[].id",
                r,
            );
        }
    }

    // ---- 策略
    for (i, p) in doc.policies.iter().enumerate() {
        if let Some(g) = &p.action.use_group {
            ref_exists(
                &ix.groups,
                g.as_str(),
                &format!("policies[{i}].action.use_group"),
                "channel_groups[].id",
                r,
            );
        }
        if let Some(l) = &p.action.use_lane {
            ref_exists(
                &ix.lanes,
                l.as_str(),
                &format!("policies[{i}].action.use_lane"),
                "lanes[].id",
                r,
            );
        }
        for (j, c) in p.action.use_channels.iter().enumerate() {
            ref_exists(
                &ix.channels,
                c.as_str(),
                &format!("policies[{i}].action.use_channels[{j}]"),
                "channels[].id",
                r,
            );
        }
        for (j, c) in p.action.exclude_channels.iter().enumerate() {
            ref_exists(
                &ix.channels,
                c.as_str(),
                &format!("policies[{i}].action.exclude_channels[{j}]"),
                "channels[].id",
                r,
            );
        }
        if let Some(c) = &p.action.pin_sticky {
            ref_exists(
                &ix.channels,
                c.as_str(),
                &format!("policies[{i}].action.pin_sticky"),
                "channels[].id",
                r,
            );
        }
        for (j, pid) in p.r#match.provider_in.iter().enumerate() {
            ref_exists(
                &ix.providers,
                pid.as_str(),
                &format!("policies[{i}].match.provider_in[{j}]"),
                "providers[].id",
                r,
            );
        }
    }

    // ---- 定价条目的限定字段也要真实存在（否则是永不命中的死条目）
    for (i, e) in doc.pricing.entries.iter().enumerate() {
        if let Some(pid) = &e.r#match.provider {
            ref_exists(
                &ix.providers,
                pid.as_str(),
                &format!("pricing.entries[{i}].match.provider"),
                "providers[].id",
                r,
            );
        }
        if let Some(cid) = &e.r#match.credential {
            ref_exists(
                &ix.credentials,
                cid.as_str(),
                &format!("pricing.entries[{i}].match.credential"),
                "credentials[].id",
                r,
            );
        }
    }
}

fn ref_exists(
    map: &BTreeMap<&str, usize>,
    key: &str,
    path: &str,
    expected: &str,
    r: &mut ValidationReport,
) {
    if !map.contains_key(key) {
        r.push_error(
            path,
            codes::NOT_FOUND,
            format!("引用的 `{key}` 不存在（应在 {expected} 里）"),
        );
    }
}

// ================================================================ 警告（不阻断）

fn check_warnings(doc: &ConfigDoc, ix: &Index<'_>, r: &mut ValidationReport) {
    // 未定价模型：有通道但没有一条定价条目命中它
    for (i, m) in doc.models.iter().enumerate() {
        let channels: Vec<&Channel> = doc.channels.iter().filter(|c| m.matches(&c.model)).collect();
        if channels.is_empty() {
            continue;
        }
        let priced = channels.iter().any(|c| {
            doc.pricing
                .entries
                .iter()
                .any(|e| e.r#match.matches(&c.model, &c.provider, &c.credential))
        });
        if !priced {
            r.push_warning(
                format!("models[{i}]"),
                codes::UNPRICED_MODEL,
                format!(
                    "模型 `{}` 没有任何定价条目覆盖，成本中心会按 0 计费（可在 pricing.entries 里补）",
                    m.id
                ),
            );
        }
    }

    // 通道指向被禁用的凭据 / 服务商
    for (i, c) in doc.channels.iter().enumerate() {
        if let Some(ci) = ix.credentials.get(c.credential.as_str()) {
            if !doc.credentials[*ci].enabled {
                r.push_warning(
                    format!("channels[{i}].credential"),
                    codes::DISABLED_REF,
                    format!("凭据 `{}` 当前被禁用，这条通道不会生效", c.credential),
                );
            }
        }
        if let Some(pi) = ix.providers.get(c.provider.as_str()) {
            if !doc.providers[*pi].enabled {
                r.push_warning(
                    format!("channels[{i}].provider"),
                    codes::DISABLED_REF,
                    format!("服务商 `{}` 当前被禁用，这条通道不会生效", c.provider),
                );
            }
        }
    }

    // lane 指向被禁用的订阅
    for (i, l) in doc.lanes.iter().enumerate() {
        if let Some(si) = l.subscription.as_ref().and_then(|s| ix.subscriptions.get(s.as_str())) {
            if !doc.proxy_subscriptions[*si].enabled {
                r.push_warning(
                    format!("lanes[{i}].subscription"),
                    codes::DISABLED_REF,
                    format!(
                        "订阅 `{}` 当前被禁用，这条线路拿不到节点",
                        l.subscription.as_ref().map(|s| s.as_str()).unwrap_or("")
                    ),
                );
            }
        }
    }

    // 通道组里的成员被禁用
    for (i, g) in doc.channel_groups.iter().enumerate() {
        for (j, m) in g.members.iter().enumerate() {
            if let Some(ci) = ix.channels.get(m.as_str()) {
                if !doc.channels[*ci].enabled {
                    r.push_warning(
                        format!("channel_groups[{i}].members[{j}]"),
                        codes::DISABLED_REF,
                        format!("成员通道 `{m}` 当前被禁用"),
                    );
                }
            }
        }
    }
}

// ================================================================ 小工具

fn require_text(value: &str, path: &str, field: &str, r: &mut ValidationReport) {
    if value.trim().is_empty() {
        r.push_error(path, codes::MISSING_FIELD, format!("{field} 不能为空"));
    }
}

fn check_regex(value: Option<&str>, path: String, r: &mut ValidationReport) {
    if let Some(pattern) = value {
        if pattern.trim().is_empty() {
            r.push_error(path, codes::INVALID_VALUE, "正则不能为空");
            return;
        }
        if let Err(e) = Regex::new(pattern) {
            r.push_error(path, codes::INVALID_REGEX, format!("正则不合法：{e}"));
        }
    }
}

fn check_listen(value: &str, path: &str, r: &mut ValidationReport) {
    if value.trim().is_empty() {
        r.push_error(path, codes::MISSING_FIELD, "监听地址不能为空");
    } else if !looks_like_host_port(value) {
        r.push_error(
            path,
            codes::INVALID_VALUE,
            format!("`{value}` 不是 host:port（仅允许 loopback 或指定地址）"),
        );
    }
}

fn looks_like_host_port(value: &str) -> bool {
    match value.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
        None => false,
    }
}

fn is_http_url(value: &str) -> bool {
    let v = value.trim();
    v.starts_with("http://") || v.starts_with("https://")
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_doc;
    // 测试里要用 ID 类型与 TokenKind（生产代码只需要 domain::*）
    use crate::types::*;
    use pretty_assertions::assert_eq;

    fn doc() -> ConfigDoc {
        default_doc()
    }

    fn check(doc: &ConfigDoc) -> ValidationReport {
        TaConfigValidator::new().validate(doc)
    }

    fn assert_clean(r: &ValidationReport) {
        assert!(r.errors.is_empty(), "期望校验通过，实际 errors = {:#?}", r.errors);
        assert!(r.ok, "errors 为空时 ok 必须为 true");
    }

    fn assert_error(r: &ValidationReport, path: &str, code: &str) {
        match r.errors.iter().find(|e| e.path == path) {
            Some(e) => {
                assert_eq!(e.code, code, "路径 {path} 的 code 不符，全部 errors = {:#?}", r.errors)
            }
            None => panic!("期望 {path} 报错，实际 errors = {:#?}", r.errors),
        }
        assert!(!r.ok, "有错误时 ok 必须为 false");
    }

    fn assert_warning(r: &ValidationReport, path: &str, code: &str) {
        match r.warnings.iter().find(|w| w.path == path) {
            Some(w) => assert_eq!(
                w.code, code,
                "路径 {path} 的警告 code 不符，全部 warnings = {:#?}",
                r.warnings
            ),
            None => panic!("期望 {path} 警告，实际 warnings = {:#?}", r.warnings),
        }
    }

    fn assert_no_error_at(r: &ValidationReport, path: &str) {
        assert!(
            r.errors.iter().all(|e| e.path != path),
            "{path} 不应报错，实际 errors = {:#?}",
            r.errors
        );
    }

    fn error_paths(r: &ValidationReport) -> Vec<String> {
        r.errors.iter().map(|e| e.path.clone()).collect()
    }

    // ------------------------------------------------------------ 总体

    #[test]
    fn default_doc_is_valid_without_warnings() {
        let report = check(&doc());
        assert_clean(&report);
        assert!(report.warnings.is_empty(), "默认配置不应有任何警告：{:#?}", report.warnings);
    }

    #[test]
    fn validator_is_usable_as_trait_object() {
        let doc = doc();
        let validator: Box<dyn ConfigValidator> = Box::new(TaConfigValidator);
        assert_clean(&validator.validate(&doc));
    }

    #[test]
    fn error_paths_are_exact_and_specific() {
        let mut d = doc();
        d.channels.push(Channel::new("ch-broken", "gpt-4o", "p-missing", "c-missing", "gpt-4o"));
        d.channels.last_mut().unwrap().lane = Some(LaneId::new("lane-missing"));
        d.channels.last_mut().unwrap().model = "model-missing".into();
        let report = check(&d);
        let idx = d.channels.len() - 1;
        let paths = error_paths(&report);
        for expected in [
            format!("channels[{idx}].provider"),
            format!("channels[{idx}].credential"),
            format!("channels[{idx}].lane"),
            format!("channels[{idx}].model"),
        ] {
            assert!(paths.contains(&expected), "缺少 {expected}：{paths:#?}");
        }
    }

    // ------------------------------------------------------------ 版本

    #[test]
    fn version_zero_and_future_are_rejected() {
        let mut d = doc();
        d.version = 0;
        assert_error(&check(&d), "version", codes::UNSUPPORTED_VERSION);

        let mut d = doc();
        d.version = CURRENT_CONFIG_VERSION + 1;
        assert_error(&check(&d), "version", codes::UNSUPPORTED_VERSION);

        let mut d = doc();
        d.version = CURRENT_CONFIG_VERSION;
        assert_no_error_at(&check(&d), "version");
    }

    // ------------------------------------------------------------ 引用完整性

    #[test]
    fn channel_provider_must_exist() {
        let mut d = doc();
        d.channels[0].provider = ProviderId::new("p-nope");
        assert_error(&check(&d), "channels[0].provider", codes::NOT_FOUND);

        let mut d = doc();
        d.channels[0].provider = d.providers[1].id.clone();
        assert_no_error_at(&check(&d), "channels[0].provider");
    }

    #[test]
    fn channel_credential_must_exist() {
        let mut d = doc();
        d.channels[2].credential = CredentialId::new("c-nope");
        assert_error(&check(&d), "channels[2].credential", codes::NOT_FOUND);
        assert_no_error_at(&check(&doc()), "channels[2].credential");
    }

    #[test]
    fn channel_lane_must_exist_when_set() {
        let mut d = doc();
        d.channels[1].lane = Some(LaneId::new("lane-nope"));
        assert_error(&check(&d), "channels[1].lane", codes::NOT_FOUND);

        // 不填 lane 是合法的（继承 credential → provider → net.default_lane）
        assert_no_error_at(&check(&doc()), "channels[3].lane");
    }

    #[test]
    fn credential_provider_and_lane_must_exist() {
        let mut d = doc();
        d.credentials[0].provider = ProviderId::new("p-nope");
        assert_error(&check(&d), "credentials[0].provider", codes::NOT_FOUND);

        let mut d = doc();
        d.credentials[1].lane = Some(LaneId::new("lane-nope"));
        assert_error(&check(&d), "credentials[1].lane", codes::NOT_FOUND);
        assert_no_error_at(&check(&doc()), "credentials[1].lane");
    }

    #[test]
    fn provider_lane_must_exist() {
        let mut d = doc();
        d.providers[2].lane = Some(LaneId::new("lane-nope"));
        assert_error(&check(&d), "providers[2].lane", codes::NOT_FOUND);
        assert_no_error_at(&check(&doc()), "providers[2].lane");
    }

    #[test]
    fn lane_subscription_must_exist() {
        let mut d = doc();
        d.lanes[1].subscription = Some(SubscriptionId::new("sub-nope"));
        assert_error(&check(&d), "lanes[1].subscription", codes::NOT_FOUND);

        // 正例：lane-us-fixed 引用已存在的 sub-jp
        assert_no_error_at(&check(&doc()), "lanes[3].subscription");
    }

    #[test]
    fn subscription_provider_key_must_be_unique() {
        // 正例：provider_key 唯一（默认配置里只有一份订阅）
        assert_no_error_at(&check(&doc()), "proxy_subscriptions[0].provider_key");

        // 反例：两份订阅用同一个内核 provider key
        let mut d = doc();
        d.proxy_subscriptions[0].provider_key = Some("shared-key".into());
        let mut second = d.proxy_subscriptions[0].clone();
        second.id = SubscriptionId::new("sub-jp-2");
        second.name = "机场B".into();
        d.proxy_subscriptions.push(second);
        assert_error(&check(&d), "proxy_subscriptions[1].provider_key", codes::DUPLICATE_ID);

        // 正例：两份订阅、不同 key
        let mut d = doc();
        d.proxy_subscriptions[0].provider_key = Some("key-a".into());
        let mut second = d.proxy_subscriptions[0].clone();
        second.id = SubscriptionId::new("sub-jp-2");
        second.provider_key = Some("key-b".into());
        d.proxy_subscriptions.push(second);
        assert_no_error_at(&check(&d), "proxy_subscriptions[1].provider_key");
    }

    #[test]
    fn channel_model_must_be_known_id_or_alias() {
        let mut d = doc();
        d.channels[0].model = "gpt-nope".into();
        assert_error(&check(&d), "channels[0].model", codes::NOT_FOUND);

        // 正例：别名也能命中（openai/gpt-4o 是 gpt-4o 的别名）
        let mut d = doc();
        d.channels[0].model = "openai/gpt-4o".into();
        assert_no_error_at(&check(&d), "channels[0].model");
    }

    #[test]
    fn channel_group_model_and_members_must_exist() {
        let mut d = doc();
        d.channel_groups[0].model = "gpt-nope".into();
        d.channel_groups[0].members[1] = ChannelId::new("ch-nope");
        let report = check(&d);
        assert_error(&report, "channel_groups[0].model", codes::NOT_FOUND);
        assert_error(&report, "channel_groups[0].members[1]", codes::NOT_FOUND);

        assert_no_error_at(&check(&doc()), "channel_groups[0].members[0]");
    }

    #[test]
    fn policy_action_references_must_exist() {
        let mut d = doc();
        d.policies[3].action.use_group = Some(ChannelGroupId::new("grp-nope"));
        assert_error(&check(&d), "policies[3].action.use_group", codes::NOT_FOUND);

        let mut d = doc();
        d.policies[1].action.use_channels = vec![ChannelId::new("ch-nope")];
        assert_error(&check(&d), "policies[1].action.use_channels[0]", codes::NOT_FOUND);

        let mut d = doc();
        d.policies[2].action.exclude_channels = vec![ChannelId::new("ch-nope")];
        assert_error(&check(&d), "policies[2].action.exclude_channels[0]", codes::NOT_FOUND);

        // 正例：默认策略引用的组与通道都存在
        let report = check(&doc());
        assert_no_error_at(&report, "policies[3].action.use_group");
        assert_no_error_at(&report, "policies[1].action.use_channels[0]");
        assert_no_error_at(&report, "policies[2].action.exclude_channels[0]");
    }

    #[test]
    fn policy_extra_references_and_match_provider_in() {
        let mut d = doc();
        d.policies[0].action.use_lane = Some(LaneId::new("lane-nope"));
        d.policies[0].action.pin_sticky = Some(ChannelId::new("ch-nope"));
        d.policies[0].r#match.provider_in = vec![ProviderId::new("p-nope")];
        let report = check(&d);
        assert_error(&report, "policies[0].action.use_lane", codes::NOT_FOUND);
        assert_error(&report, "policies[0].action.pin_sticky", codes::NOT_FOUND);
        assert_error(&report, "policies[0].match.provider_in[0]", codes::NOT_FOUND);
    }

    #[test]
    fn net_default_lane_must_exist() {
        let mut d = doc();
        d.net.default_lane = Some(LaneId::new("lane-nope"));
        assert_error(&check(&d), "net.default_lane", codes::NOT_FOUND);
        assert_no_error_at(&check(&doc()), "net.default_lane");
    }

    #[test]
    fn gateway_fallback_provider_must_exist() {
        let mut d = doc();
        d.gateway.fallback_provider = Some(ProviderId::new("p-nope"));
        assert_error(&check(&d), "gateway.fallback_provider", codes::NOT_FOUND);

        let mut d = doc();
        d.gateway.fallback_provider = Some(ProviderId::new("p-openai"));
        assert_no_error_at(&check(&d), "gateway.fallback_provider");
    }

    #[test]
    fn pricing_match_references_must_exist() {
        let mut d = doc();
        d.pricing.entries[2].r#match.provider = Some(ProviderId::new("p-nope"));
        assert_error(&check(&d), "pricing.entries[2].match.provider", codes::NOT_FOUND);

        let mut d = doc();
        d.pricing.entries[3].r#match.credential = Some(CredentialId::new("c-nope"));
        assert_error(&check(&d), "pricing.entries[3].match.credential", codes::NOT_FOUND);
        assert_no_error_at(&check(&doc()), "pricing.entries[2].match.provider");
    }

    // ------------------------------------------------------------ ID 唯一性

    #[test]
    fn duplicate_ids_reported_in_every_collection() {
        // providers
        let mut d = doc();
        d.providers.push(d.providers[0].clone());
        assert_error(&check(&d), "providers[3].id", codes::DUPLICATE_ID);

        // credentials
        let mut d = doc();
        d.credentials.push(d.credentials[0].clone());
        assert_error(&check(&d), "credentials[4].id", codes::DUPLICATE_ID);

        // models
        let mut d = doc();
        d.models.push(d.models[0].clone());
        assert_error(&check(&d), "models[3].id", codes::DUPLICATE_ID);

        // channels
        let mut d = doc();
        d.channels.push(d.channels[0].clone());
        assert_error(&check(&d), "channels[4].id", codes::DUPLICATE_ID);

        // channel_groups
        let mut d = doc();
        d.channel_groups.push(d.channel_groups[0].clone());
        assert_error(&check(&d), "channel_groups[1].id", codes::DUPLICATE_ID);

        // policies
        let mut d = doc();
        d.policies.push(d.policies[0].clone());
        assert_error(&check(&d), "policies[4].id", codes::DUPLICATE_ID);

        // lanes
        let mut d = doc();
        d.lanes.push(d.lanes[0].clone());
        assert_error(&check(&d), "lanes[4].id", codes::DUPLICATE_ID);

        // proxy_subscriptions
        let mut d = doc();
        d.proxy_subscriptions.push(d.proxy_subscriptions[0].clone());
        assert_error(&check(&d), "proxy_subscriptions[1].id", codes::DUPLICATE_ID);
    }

    #[test]
    fn unique_ids_produce_no_duplicate_errors() {
        let report = check(&doc());
        assert!(
            !report.errors.iter().any(|e| e.code == codes::DUPLICATE_ID),
            "默认配置不应有重复 ID：{:#?}",
            report.errors
        );
    }

    #[test]
    fn ambiguous_model_alias_is_rejected() {
        let mut d = doc();
        d.models[2].aliases = vec!["gpt-4o".into()]; // 与 models[0].id 撞名
        assert_error(&check(&d), "models[2].aliases[0]", codes::AMBIGUOUS_ALIAS);

        let mut d = doc();
        d.models[2].aliases = vec!["gpt-4o-mini-preview".into()];
        assert_no_error_at(&check(&d), "models[2].aliases[0]");
    }

    // ------------------------------------------------------------ 定价匹配

    #[test]
    fn pricing_entry_without_qualifier_is_rejected() {
        let mut d = doc();
        d.pricing
            .entries
            .push(PriceEntry::new(PriceMatch::unconstrained()).with_rate(TokenKind::Input, 1.0));
        let idx = d.pricing.entries.len() - 1;
        assert_error(
            &check(&d),
            &format!("pricing.entries[{idx}].match"),
            codes::UNCONSTRAINED_MATCH,
        );
    }

    #[test]
    fn qualified_pricing_entries_are_accepted() {
        let mut d = doc();
        d.pricing.entries.push(
            PriceEntry::new(PriceMatch::model("gpt-4o-mini*")).with_rate(TokenKind::Input, 0.1),
        );
        d.pricing.entries.push(PriceEntry::new(PriceMatch::provider("p-mytoken")));
        d.pricing.entries.push(PriceEntry::new(PriceMatch::credential("c-relay-a")));
        let report = check(&d);
        assert!(
            !report.errors.iter().any(|e| e.code == codes::UNCONSTRAINED_MATCH),
            "有限定字段的条目不应报错：{:#?}",
            report.errors
        );
    }

    // ------------------------------------------------------------ 形状

    #[test]
    fn empty_required_fields_are_reported() {
        let mut d = doc();
        d.providers[0].id = ProviderId::new("");
        d.credentials[0].label = String::new();
        d.models[0].id = String::new();
        d.channels[0].upstream_model = String::new();
        d.lanes[0].name = String::new();
        d.policies[0].name = String::new();
        d.channel_groups[0].id = ChannelGroupId::new("");
        d.proxy_subscriptions[0].url = String::new();
        let report = check(&d);
        for path in [
            "providers[0].id",
            "credentials[0].label",
            "models[0].id",
            "channels[0].upstream_model",
            "lanes[0].name",
            "policies[0].name",
            "channel_groups[0].id",
            "proxy_subscriptions[0].url",
        ] {
            assert_error(&report, path, codes::MISSING_FIELD);
        }
    }

    #[test]
    fn bad_urls_and_listens_are_reported() {
        let mut d = doc();
        d.providers[0].base_url = "api.openai.com".into();
        d.gateway.listen = "not-a-host-port".into();
        d.net.kernel.controller = String::new();
        let report = check(&d);
        assert_error(&report, "providers[0].base_url", codes::INVALID_VALUE);
        assert_error(&report, "gateway.listen", codes::INVALID_VALUE);
        assert_error(&report, "net.kernel.controller", codes::MISSING_FIELD);

        let mut d = doc();
        d.gateway.listen = "127.0.0.1:8787".into();
        assert_no_error_at(&check(&d), "gateway.listen");
    }

    #[test]
    fn auth_header_required_for_header_scheme() {
        let mut d = doc();
        d.providers[1].auth.header = None;
        assert_error(&check(&d), "providers[1].auth.header", codes::MISSING_FIELD);

        // bearer 不需要 header
        assert_no_error_at(&check(&doc()), "providers[0].auth.header");
    }

    #[test]
    fn invalid_regex_is_reported_with_path() {
        let mut d = doc();
        d.proxy_subscriptions[0].filter.include = Some("(?i)(日本".into());
        d.lanes[1].filter = Some("[unclosed".into());
        d.policies[0].r#match.ua_matches = Some("(cursor".into());
        let report = check(&d);
        assert_error(&report, "proxy_subscriptions[0].filter.include", codes::INVALID_REGEX);
        assert_error(&report, "lanes[1].filter", codes::INVALID_REGEX);
        assert_error(&report, "policies[0].match.ua_matches", codes::INVALID_REGEX);

        // 正例：默认配置里的正则都合法
        let report = check(&doc());
        assert!(
            !report.errors.iter().any(|e| e.code == codes::INVALID_REGEX),
            "默认配置的正则应全部合法：{:#?}",
            report.errors
        );
    }

    #[test]
    fn lane_kind_requires_matching_fields() {
        let mut d = doc();
        d.lanes[2].group = None; // subscription_group 没有 group
        assert_error(&check(&d), "lanes[2].group", codes::MISSING_FIELD);

        let mut d = doc();
        d.lanes[3].node = None; // subscription_node 没有 node
        assert_error(&check(&d), "lanes[3].node", codes::MISSING_FIELD);

        let mut d = doc();
        d.lanes[1].subscription = None; // subscription_auto 没有 subscription
        assert_error(&check(&d), "lanes[1].subscription", codes::MISSING_FIELD);

        // 正例：lane-direct 什么都不需要
        assert_no_error_at(&check(&doc()), "lanes[0].subscription");
    }

    #[test]
    fn lane_port_range_must_fit_lanes() {
        let mut d = doc();
        d.net.kernel.lane_port_range = [39100, 39101]; // 2 个端口，但需要 3 条
        assert_error(&check(&d), "net.kernel.lane_port_range", codes::INVALID_VALUE);

        let mut d = doc();
        d.net.kernel.lane_port_range = [39299, 39100];
        assert_error(&check(&d), "net.kernel.lane_port_range", codes::INVALID_VALUE);

        let mut d = doc();
        d.net.kernel.lane_port_range = [39100, 39102];
        assert_no_error_at(&check(&d), "net.kernel.lane_port_range");
    }

    #[test]
    fn dns_and_health_shape() {
        let mut d = doc();
        d.net.dns.proxy_server_nameserver.clear();
        assert_error(&check(&d), "net.dns.proxy_server_nameserver", codes::MISSING_FIELD);

        let mut d = doc();
        d.net.health.interval_s = 0;
        assert_error(&check(&d), "net.health.interval_s", codes::INVALID_VALUE);
    }

    #[test]
    fn request_log_sample_rate_range() {
        let mut d = doc();
        d.request_log.sample_rate = 1.5;
        assert_error(&check(&d), "request_log.sample_rate", codes::INVALID_VALUE);

        let mut d = doc();
        d.request_log.sample_rate = 0.1;
        assert_no_error_at(&check(&d), "request_log.sample_rate");
    }

    #[test]
    fn quota_shape_rules() {
        let mut d = doc();
        d.credentials[2].quota.limit = None; // metric = tokens 但没有 limit
        assert_error(&check(&d), "credentials[2].quota.limit", codes::MISSING_FIELD);

        let mut d = doc();
        d.credentials[0].quota.soft_limit = 1.2;
        assert_error(&check(&d), "credentials[0].quota.soft_limit", codes::INVALID_VALUE);

        let mut d = doc();
        d.credentials[0].quota.soft_limit = 0.9;
        d.credentials[0].quota.hard_limit = 0.5;
        assert_error(&check(&d), "credentials[0].quota.soft_limit", codes::INVALID_VALUE);

        let mut d = doc();
        d.credentials[0].quota.reserve = 1.0;
        assert_error(&check(&d), "credentials[0].quota.reserve", codes::INVALID_VALUE);

        let mut d = doc();
        d.credentials[0].quota.reset_day = 32;
        assert_error(&check(&d), "credentials[0].quota.reset_day", codes::INVALID_VALUE);

        // 正例：unknown metric 不需要 limit
        assert_no_error_at(&check(&doc()), "credentials[0].quota.limit");
    }

    #[test]
    fn channel_weight_and_group_members() {
        let mut d = doc();
        d.channels[0].weight = 0;
        assert_error(&check(&d), "channels[0].weight", codes::INVALID_VALUE);

        let mut d = doc();
        d.channel_groups[0].members.clear();
        assert_error(&check(&d), "channel_groups[0].members", codes::EMPTY_MEMBERS);

        let mut d = doc();
        d.channel_groups[0].members.push(ChannelId::new("ch-relay-a")); // 重复成员
        assert_error(&check(&d), "channel_groups[0].members[3]", codes::DUPLICATE_ID);

        let mut d = doc();
        d.channel_groups[0].sticky.ttl_h = 0;
        assert_error(&check(&d), "channel_groups[0].sticky.ttl_h", codes::INVALID_VALUE);
    }

    #[test]
    fn policy_error_code_and_failover_shape() {
        let mut d = doc();
        d.policies[0].action.error_code = Some(200);
        assert_error(&check(&d), "policies[0].action.error_code", codes::INVALID_VALUE);

        let mut d = doc();
        d.policies[3].action.failover.as_mut().unwrap().max_attempts = 0;
        assert_error(&check(&d), "policies[3].action.failover.max_attempts", codes::INVALID_VALUE);

        let mut d = doc();
        d.policies[0].r#match.input_tokens_lt = Some(100);
        d.policies[0].r#match.input_tokens_gt = Some(10);
        assert_error(&check(&d), "policies[0].match.input_tokens_lt", codes::INVALID_VALUE);
    }

    #[test]
    fn pricing_numeric_shape() {
        let mut d = doc();
        d.pricing.entries[0].rates.insert("input".into(), -1.0);
        assert_error(&check(&d), "pricing.entries[0].rates.input", codes::INVALID_VALUE);

        let mut d = doc();
        d.pricing.entries[2].multiplier = 0.0;
        assert_error(&check(&d), "pricing.entries[2].multiplier", codes::INVALID_VALUE);

        let mut d = doc();
        d.pricing.entries[3].plan_fee = Some(PlanFee::new(99.0, "", QuotaWindow::Month));
        assert_error(&check(&d), "pricing.entries[3].plan_fee.currency", codes::MISSING_FIELD);
    }

    #[test]
    fn system_integration_file_watch_rules() {
        let mut d = doc();
        d.system_integration.file_watch_mode = true;
        assert_error(&check(&d), "system_integration.file_watch_path", codes::MISSING_FIELD);

        let mut d = doc();
        d.system_integration.file_watch_path = Some("D:/ta.yaml".into());
        assert_warning(&check(&d), "system_integration.file_watch_path", codes::INVALID_VALUE);

        let mut d = doc();
        d.system_integration.file_watch_mode = true;
        d.system_integration.file_watch_path = Some("D:/ta.yaml".into());
        let report = check(&d);
        assert_no_error_at(&report, "system_integration.file_watch_path");
        assert!(report.warnings.is_empty(), "{:#?}", report.warnings);
    }

    // ------------------------------------------------------------ 警告

    #[test]
    fn unpriced_model_is_a_warning() {
        let mut d = doc();
        d.models.push(Model::new("some-unpriced-model"));
        d.channels.push(Channel::new(
            "ch-unpriced",
            "some-unpriced-model",
            "p-openai",
            "c-openai-personal",
            "some-unpriced-model",
        ));
        let idx = d.models.len() - 1;
        let report = check(&d);
        assert_clean(&report); // 只警告，不阻断
        assert_warning(&report, &format!("models[{idx}]"), codes::UNPRICED_MODEL);

        // 正例：默认配置的模型都有定价覆盖
        let report = check(&doc());
        assert!(
            !report.warnings.iter().any(|w| w.code == codes::UNPRICED_MODEL),
            "默认配置不应有未定价警告：{:#?}",
            report.warnings
        );
    }

    #[test]
    fn disabled_credential_referenced_by_channel_is_a_warning() {
        let mut d = doc();
        d.credentials[0].enabled = false; // c-openai-personal ← channels[0]
        let report = check(&d);
        assert_clean(&report);
        assert_warning(&report, "channels[0].credential", codes::DISABLED_REF);

        // 正例
        let report = check(&doc());
        assert!(
            !report.warnings.iter().any(|w| w.code == codes::DISABLED_REF),
            "默认配置不应有禁用引用警告：{:#?}",
            report.warnings
        );
    }

    #[test]
    fn disabled_subscription_referenced_by_lane_is_a_warning() {
        let mut d = doc();
        d.proxy_subscriptions[0].enabled = false;
        let report = check(&d);
        assert_clean(&report);
        assert_warning(&report, "lanes[1].subscription", codes::DISABLED_REF);
        assert_warning(&report, "lanes[2].subscription", codes::DISABLED_REF);
        assert_warning(&report, "lanes[3].subscription", codes::DISABLED_REF);
        // direct lane 不引用订阅 → 不该有警告
        assert!(!report.warnings.iter().any(|w| w.path == "lanes[0].subscription"));
    }

    #[test]
    fn disabled_channel_in_group_and_provider_are_warnings() {
        let mut d = doc();
        d.channels[1].enabled = false;
        d.providers[2].enabled = false; // p-mytoken ← channels[1], channels[2]
        let report = check(&d);
        assert_clean(&report);
        assert_warning(&report, "channel_groups[0].members[1]", codes::DISABLED_REF);
        assert_warning(&report, "channels[1].provider", codes::DISABLED_REF);
    }

    #[test]
    fn policy_priority_tie_is_a_warning() {
        let mut d = doc();
        d.policies[1].enabled = true;
        d.policies[1].priority = d.policies[3].priority;
        let report = check(&d);
        assert_clean(&report);
        // 警告落在"后声明"的那条上（声明顺序即求值顺序）
        assert_warning(&report, "policies[3].priority", codes::INVALID_VALUE);
    }

    // ------------------------------------------------------------ 例子配置

    #[test]
    fn example_yaml_reports_a_field_path_on_failure() {
        // `config.example.yaml` 是**文档用**的示例，和冻结的结构体存在历史差异
        // （`system_proxy: off` 在 YAML 1.2 里是字符串而不是 bool；`lanes[].filter`
        // 在示例里是 mapping 而结构体里是正则字符串）。规则：能读就说明没漂移；
        // 读不了必须给出**具体字段路径**，而不是一句"解析失败"。
        let example = include_str!("../../../../config.example.yaml");
        match crate::config::from_yaml(example) {
            Ok(doc) => assert!(doc.version >= 1),
            Err(err) => {
                let paths = error_paths_from_error(&err);
                assert!(!paths.is_empty(), "解析失败必须带字段路径：{err:?}");
                let path = &paths[0];
                assert!(
                    path.contains('.') || path.contains('['),
                    "路径应当是可定位的字段路径，实际：{path}"
                );
            }
        }
    }

    fn error_paths_from_error(err: &crate::error::TaError) -> Vec<String> {
        match err {
            crate::error::TaError::ConfigInvalid { errors } => {
                errors.iter().map(|e| e.path.clone()).collect()
            }
            _ => Vec::new(),
        }
    }
}

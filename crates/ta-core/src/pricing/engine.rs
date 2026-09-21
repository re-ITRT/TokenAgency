//! `PricingEngine` 实现：**manual > imported > builtin**，精确匹配优先于通配。
//!
//! 规则来源：[07 §6.2](../../../docs/07-ui-pages-and-cost.md)。
//!
//! # 匹配顺序（可预测、可解释）
//!
//! 1. 按 **来源分档**：`manual`（用户填）→ `imported`（导入的表）→ `builtin`（内置）
//!    → 最后才是 [`crate::pricing::builtin`] 的静态官方价表。
//! 2. **同一档内**：先试"精确匹配"（`match` 里没有任何 `*`），再试通配；
//!    各自都按**声明顺序首个命中生效**（与策略引擎"首个命中即生效"一致）。
//! 3. **倍率与套餐费独立解析**：只写 `multiplier: 0.8` 而不写 `rates` 的条目
//!    （中转站倍率的典型写法）会和更低优先级的费率组合：`官方价 × 0.8`。
//! 4. **`plan_fee` 不摊进单次请求**：`cost_of` 只算按量成本，套餐费由
//!    [`crate::pricing::amortize_plan_fee`] 单独摊销给成本中心展示。

use std::collections::BTreeMap;

use crate::domain::{PriceEntry, PriceMatch, PriceSource, PriceUnit, PricingConfig};
use crate::pricing::builtin;
use crate::pricing::{
    glob_match, unit_divisor, PlanFeeResolved, PriceSourceKind, PricingEngine, ResolvedPrice,
};
use crate::types::{CredentialId, ProviderId, TokenKind, TokenUsage};

/// 一个优先级档位。
struct Tier<'a> {
    entries: &'a [(i64, PriceEntry)],
    kind: PriceSourceKind,
    /// 0 = 最高优先级（manual）
    rank: usize,
}

/// 一次命中。
struct Hit<'a> {
    entry: &'a PriceEntry,
    /// 配置里的下标（`-1` = 内置静态表）
    index: i64,
    kind: PriceSourceKind,
    rank: usize,
}

/// 定价引擎。持有配置的一份克隆，改价后重建即可（配置层用 `ArcSwap` 替换）。
pub struct PricingEngineImpl {
    manual: Vec<(i64, PriceEntry)>,
    imported: Vec<(i64, PriceEntry)>,
    builtin_cfg: Vec<(i64, PriceEntry)>,
    default_currency: String,
    default_unit: PriceUnit,
    use_builtin_table: bool,
}

impl PricingEngineImpl {
    pub fn new(cfg: &PricingConfig) -> Self {
        let mut me = Self {
            manual: Vec::new(),
            imported: Vec::new(),
            builtin_cfg: Vec::new(),
            default_currency: cfg.currency.clone(),
            default_unit: cfg.unit,
            use_builtin_table: true,
        };
        for (i, e) in cfg.entries.iter().enumerate() {
            match e.source {
                PriceSource::Manual => me.manual.push((i as i64, e.clone())),
                PriceSource::Imported => me.imported.push((i as i64, e.clone())),
                PriceSource::Builtin => me.builtin_cfg.push((i as i64, e.clone())),
            }
        }
        me
    }

    /// 只信用户配置：连内置官方价表也不用（"我不想看到任何猜的价格"）。
    pub fn without_builtin_table(mut self) -> Self {
        self.use_builtin_table = false;
        self
    }

    fn tiers(&self) -> [Tier<'_>; 3] {
        [
            Tier { entries: &self.manual, kind: PriceSourceKind::Manual, rank: 0 },
            Tier { entries: &self.imported, kind: PriceSourceKind::Imported, rank: 1 },
            Tier { entries: &self.builtin_cfg, kind: PriceSourceKind::Builtin, rank: 2 },
        ]
    }

    fn find<'a>(
        &'a self,
        tiers: &[Tier<'a>],
        model: &str,
        provider: &ProviderId,
        cred: &CredentialId,
        pred: impl Fn(&PriceEntry) -> bool,
    ) -> Option<Hit<'a>> {
        for t in tiers {
            for exact in [true, false] {
                for (idx, e) in t.entries {
                    if is_exact(&e.r#match) != exact {
                        continue;
                    }
                    if !matches_request(&e.r#match, model, provider, cred) {
                        continue;
                    }
                    if !pred(e) {
                        continue;
                    }
                    return Some(Hit { entry: e, index: *idx, kind: t.kind, rank: t.rank });
                }
            }
        }
        None
    }
}

impl PricingEngine for PricingEngineImpl {
    fn resolve(&self, model: &str, provider: &ProviderId, cred: &CredentialId) -> ResolvedPrice {
        let tiers = self.tiers();

        // ---- 费率：最高优先级的"有 rates 的命中" ----
        let rates_hit = self.find(&tiers, model, provider, cred, |e| !e.rates.is_empty());
        // ---- 倍率 / 套餐费：独立解析（典型场景是中转站条目只写倍率）----
        let mult_hit = self.find(&tiers, model, provider, cred, |e| {
            e.multiplier.is_finite() && (e.multiplier - 1.0).abs() > f64::EPSILON
        });
        let fee_hit = self.find(&tiers, model, provider, cred, |e| e.plan_fee.is_some());

        // ---- 内置静态表只在"用户完全没配价"时兜底 ----
        let builtin_hit = if rates_hit.is_none() && self.use_builtin_table {
            builtin::builtin_lookup(model)
        } else {
            None
        };

        let (rates, currency, divisor) = if let Some(h) = rates_hit.as_ref() {
            (
                rates_of_entry(h.entry),
                h.entry.currency.clone().unwrap_or_else(|| self.default_currency.clone()),
                unit_divisor(entry_unit(h.entry).unwrap_or(self.default_unit)),
            )
        } else if let Some(b) = builtin_hit {
            (builtin::rates_of(b), builtin::BUILTIN_CURRENCY.to_string(), builtin::BUILTIN_DIVISOR)
        } else {
            (BTreeMap::new(), self.default_currency.clone(), unit_divisor(self.default_unit))
        };

        // ---- 谁是"主条目"：优先级最高的贡献者；同档内费率条目优先于修饰条目 ----
        // rank 3 = 内置静态表（比配置里的 builtin 档更低）
        let mut primary: Option<(usize, u8, PriceSourceKind, i64)> = None;
        if let Some(h) = rates_hit.as_ref() {
            primary = best(primary, (h.rank, 0, h.kind, h.index));
        } else if builtin_hit.is_some() {
            primary = best(primary, (3, 0, PriceSourceKind::Builtin, -1));
        }
        if let Some(h) = mult_hit.as_ref() {
            primary = best(primary, (h.rank, 1, h.kind, h.index));
        }
        if let Some(h) = fee_hit.as_ref() {
            primary = best(primary, (h.rank, 1, h.kind, h.index));
        }
        let (source, entry_index) = match primary {
            Some((_, _, kind, idx)) => (kind, idx),
            None => (PriceSourceKind::None, -1),
        };

        let multiplier = mult_hit
            .as_ref()
            .map(|h| h.entry.multiplier)
            .or_else(|| rates_hit.as_ref().map(|h| h.entry.multiplier))
            .filter(|m| m.is_finite() && *m > 0.0)
            .unwrap_or(1.0);

        let plan_fee = fee_hit
            .as_ref()
            .and_then(|h| h.entry.plan_fee.as_ref())
            .map(|f| PlanFeeResolved { amount: f.amount, currency: f.currency.clone() });

        ResolvedPrice { rates, multiplier, currency, divisor, source, plan_fee, entry_index }
    }

    fn cost(&self, usage: &TokenUsage, price: &ResolvedPrice) -> crate::types::Money {
        price.cost_of(usage)
    }
}

// ---------------------------------------------------------------- 匹配助手

/// `match` 里没有任何通配符 = 精确匹配。
fn is_exact(m: &PriceMatch) -> bool {
    let no_star = |s: Option<&str>| s.map(|v| !v.contains('*')).unwrap_or(true);
    no_star(m.model.as_deref()) && no_star(m.provider.as_ref().map(|p| p.as_str()))
}

/// 三要素全都要匹配；没写的字段 = 不过滤（`match: {}` 是"全命中"）。
fn matches_request(
    m: &PriceMatch,
    model: &str,
    provider: &ProviderId,
    cred: &CredentialId,
) -> bool {
    let model_ok = m.model.as_deref().map(|p| glob_match(p, model)).unwrap_or(true);
    let provider_ok =
        m.provider.as_ref().map(|p| glob_match(p.as_str(), provider.as_str())).unwrap_or(true);
    // credential 不做通配：ID 是我们自己生成的，写错就是写错
    let cred_ok = m.credential.as_ref().map(|c| c == cred).unwrap_or(true);
    model_ok && provider_ok && cred_ok
}

/// 条目自带的 `unit`（写在 `extra` 里，`rates` 的计价单位可以逐条不同）。
fn entry_unit(e: &PriceEntry) -> Option<PriceUnit> {
    e.extra.get("unit").and_then(|v| v.as_str()).and_then(crate::pricing::parse_unit)
}

fn rates_of_entry(e: &PriceEntry) -> BTreeMap<TokenKind, f64> {
    e.rates
        .iter()
        .filter(|(_, v)| v.is_finite())
        .map(|(k, v)| (TokenKind::from_key(k), *v))
        .collect()
}

/// 选更优的贡献者：先比档位（小 = 优先），再比"费率条目优先于修饰条目"。
fn best(
    cur: Option<(usize, u8, PriceSourceKind, i64)>,
    cand: (usize, u8, PriceSourceKind, i64),
) -> Option<(usize, u8, PriceSourceKind, i64)> {
    match cur {
        None => Some(cand),
        Some(c) => {
            if (cand.0, cand.1) < (c.0, c.1) {
                Some(cand)
            } else {
                Some(c)
            }
        }
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PlanFee, QuotaWindow};
    use crate::pricing::{cache_savings, cost_of_snapshot};
    use pretty_assertions::assert_eq;

    fn provider() -> ProviderId {
        ProviderId::new("p-relay")
    }
    fn cred() -> CredentialId {
        CredentialId::new("c-1")
    }

    fn entry(
        model: Option<&str>,
        source: PriceSource,
        rates: &[(&str, f64)],
        multiplier: f64,
    ) -> PriceEntry {
        PriceEntry {
            r#match: PriceMatch { model: model.map(|s| s.to_string()), ..Default::default() },
            rates: rates.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            multiplier,
            currency: None,
            plan_fee: None,
            source,
            effective_from: None,
            extra: Default::default(),
        }
    }

    fn engine(entries: Vec<PriceEntry>) -> PricingEngineImpl {
        PricingEngineImpl::new(&PricingConfig { entries, ..Default::default() })
    }

    fn usage(pairs: &[(TokenKind, u64)]) -> TokenUsage {
        let mut u = TokenUsage::new();
        for (k, v) in pairs {
            u.set(k.clone(), *v);
        }
        u
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    // ---------------------------------------------------- 优先级

    #[test]
    fn manual_beats_imported_beats_builtin() {
        let all = vec![
            entry(Some("gpt-4o"), PriceSource::Builtin, &[("input", 1.0)], 1.0),
            entry(Some("gpt-4o"), PriceSource::Imported, &[("input", 2.0)], 1.0),
            entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 9.0)], 1.0),
        ];
        let p = engine(all.clone()).resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Manual);
        approx(p.rates[&TokenKind::Input], 9.0);

        // 去掉 manual → imported 顶上来（注意 config 下标会变）
        let p = engine(all[..2].to_vec()).resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Imported);
        approx(p.rates[&TokenKind::Input], 2.0);

        // 只剩 builtin（配置里的）→ 仍是 Builtin，但用的是配置值而不是内置表
        let p = engine(all[..1].to_vec()).resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Builtin);
        approx(p.rates[&TokenKind::Input], 1.0);
        assert_eq!(p.entry_index, 0);
    }

    #[test]
    fn exact_match_beats_wildcard_even_if_declared_later() {
        let e = engine(vec![
            entry(Some("gpt-4o*"), PriceSource::Manual, &[("input", 1.0)], 1.0),
            entry(Some("gpt-4o-mini"), PriceSource::Manual, &[("input", 7.0)], 1.0),
        ]);
        let mini = e.resolve("gpt-4o-mini", &provider(), &cred());
        approx(mini.rates[&TokenKind::Input], 7.0);
        assert_eq!(mini.entry_index, 1);

        // 只有通配能命中的模型走通配
        let dated = e.resolve("gpt-4o-2024-11-20", &provider(), &cred());
        approx(dated.rates[&TokenKind::Input], 1.0);
        assert_eq!(dated.entry_index, 0);
    }

    #[test]
    fn wildcard_uses_declaration_order_first_hit_wins() {
        let e = engine(vec![
            entry(Some("claude-*"), PriceSource::Manual, &[("input", 1.0)], 1.0),
            entry(Some("claude-sonnet-*"), PriceSource::Manual, &[("input", 2.0)], 1.0),
        ]);
        let p = e.resolve("claude-sonnet-4-5", &provider(), &cred());
        approx(p.rates[&TokenKind::Input], 1.0);
        assert_eq!(p.entry_index, 0, "同档通配按顺序首个命中");
    }

    #[test]
    fn higher_priority_tier_wins_over_a_more_specific_lower_tier() {
        // 通配的 manual 依然赢过精确的 builtin —— 优先级先于具体度
        let e = engine(vec![
            entry(Some("gpt-4o"), PriceSource::Builtin, &[("input", 1.0)], 1.0),
            entry(Some("gpt-4o*"), PriceSource::Manual, &[("input", 5.0)], 1.0),
        ]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Manual);
        approx(p.rates[&TokenKind::Input], 5.0);
    }

    #[test]
    fn builtin_table_is_the_last_resort() {
        let e = engine(vec![]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Builtin);
        assert_eq!(p.entry_index, -1, "静态表不是配置条目");
        assert_eq!(p.currency, "USD");
        assert_eq!(p.divisor, 1_000_000.0);
        approx(p.rates[&TokenKind::Input], 2.50);
        approx(p.rates[&TokenKind::Output], 10.00);

        // 关掉内置表 → 未定价
        let e = engine(vec![]).without_builtin_table();
        let p = e.resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::None);
        assert!(!p.is_priced());
        assert_eq!(p.entry_index, -1);
    }

    #[test]
    fn user_configured_builtin_entry_beats_the_static_table() {
        let e = engine(vec![entry(Some("gpt-4o*"), PriceSource::Builtin, &[("input", 0.5)], 1.0)]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        approx(p.rates[&TokenKind::Input], 0.5);
        assert_eq!(p.entry_index, 0);
    }

    #[test]
    fn unpriced_model_is_flagged_not_guessed() {
        let e = engine(vec![entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 1.0)], 1.0)]);
        let p = e.resolve("some-local-model", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::None);
        assert!(!p.has_rates());
        approx(p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)])).amount, 0.0);
    }

    // ---------------------------------------------------- 倍率（中转站）

    #[test]
    fn relay_multiplier_combines_with_builtin_rates() {
        // 07 §6.2 的例 3：只写 provider + multiplier，不写 rates
        let mut e_manual = entry(None, PriceSource::Manual, &[], 0.8);
        e_manual.r#match.provider = Some(provider());
        let e = engine(vec![e_manual]);

        let p = e.resolve("gpt-4o", &provider(), &cred());
        // 费率来自内置表（USD / 1M）
        approx(p.rates[&TokenKind::Input], 2.50);
        approx(p.rates[&TokenKind::Output], 10.00);
        // 倍率来自 manual 条目
        approx(p.multiplier, 0.8);
        assert_eq!(p.source, PriceSourceKind::Manual, "倍率来自 manual，来源就该是 manual");
        assert_eq!(p.entry_index, 0);

        // 倍率参与成本计算：1M input + 1M output = (2.5 + 10) × 0.8
        let u = usage(&[(TokenKind::Input, 1_000_000), (TokenKind::Output, 1_000_000)]);
        approx(e.cost(&u, &p).amount, 12.5 * 0.8);
    }

    #[test]
    fn higher_priority_multiplier_wins() {
        let mut manual = entry(None, PriceSource::Manual, &[], 0.5);
        manual.r#match.provider = Some(provider());
        let imported = entry(None, PriceSource::Imported, &[], 0.9);
        let e = engine(vec![
            entry(Some("gpt-4o"), PriceSource::Builtin, &[("input", 2.0)], 1.0),
            imported,
            manual,
        ]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        approx(p.multiplier, 0.5);
        approx(p.rates[&TokenKind::Input], 2.0);
        assert_eq!(p.source, PriceSourceKind::Manual);
    }

    #[test]
    fn multiplier_in_the_rates_entry_itself_applies() {
        let e = engine(vec![entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 3.0)], 0.9)]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        approx(p.multiplier, 0.9);
        approx(p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)])).amount, 2.7);
    }

    #[test]
    fn bogus_multiplier_falls_back_to_one() {
        let e = engine(vec![entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 3.0)], 0.0)]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        approx(p.multiplier, 1.0);
    }

    // ---------------------------------------------------- provider / credential 作用域

    #[test]
    fn provider_and_credential_scoping() {
        let mut by_provider = entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 4.0)], 1.0);
        by_provider.r#match.provider = Some(ProviderId::new("p-mytoken"));
        let mut by_cred = entry(Some("gpt-4o"), PriceSource::Imported, &[("input", 6.0)], 1.0);
        by_cred.r#match.credential = Some(cred());
        let e = engine(vec![by_provider, by_cred]);

        // 别的 provider + 别的凭据 → 只能落到内置表
        let other = e.resolve("gpt-4o", &ProviderId::new("p-other"), &CredentialId::new("c-x"));
        assert_eq!(other.source, PriceSourceKind::Builtin);

        // 命中 provider
        let p = e.resolve("gpt-4o", &ProviderId::new("p-mytoken"), &CredentialId::new("c-x"));
        approx(p.rates[&TokenKind::Input], 4.0);

        // 命中凭据（manual 优先，但 provider 条目这次不匹配）
        let p = e.resolve("gpt-4o", &ProviderId::new("p-other"), &cred());
        approx(p.rates[&TokenKind::Input], 6.0);
        assert_eq!(p.source, PriceSourceKind::Imported);
    }

    #[test]
    fn provider_pattern_supports_wildcards() {
        let mut e1 = entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 4.0)], 1.0);
        e1.r#match.provider = Some(ProviderId::new("p-relay-*"));
        let e = engine(vec![e1]);
        let p = e.resolve("gpt-4o", &ProviderId::new("p-relay-b"), &cred());
        approx(p.rates[&TokenKind::Input], 4.0);
    }

    #[test]
    fn catch_all_entry_matches_everything() {
        let e = engine(vec![entry(None, PriceSource::Manual, &[("input", 1.5)], 1.0)]);
        let p = e.resolve("whatever-model", &provider(), &cred());
        assert_eq!(p.source, PriceSourceKind::Manual);
        approx(p.rates[&TokenKind::Input], 1.5);
    }

    // ---------------------------------------------------- unit

    #[test]
    fn per_entry_unit_switches_the_divisor() {
        let mut e1 = entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 0.5)], 1.0);
        e1.extra.insert("unit".into(), serde_json::json!("per_1k"));
        let e = engine(vec![e1]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        approx(p.divisor, 1_000.0);
        // 每 1K 0.5 → 2K token = 1.0
        approx(p.cost_of(&usage(&[(TokenKind::Input, 2000)])).amount, 1.0);
    }

    #[test]
    fn per_token_unit_is_supported() {
        let mut e1 = entry(Some("m"), PriceSource::Manual, &[("input", 0.000003)], 1.0);
        e1.extra.insert("unit".into(), serde_json::json!("per_token"));
        let e = engine(vec![e1]);
        let p = e.resolve("m", &provider(), &cred());
        approx(p.divisor, 1.0);
        approx(p.cost_of(&usage(&[(TokenKind::Input, 10_000)])).amount, 0.03);
    }

    #[test]
    fn config_unit_is_the_default_divisor() {
        let cfg = PricingConfig {
            unit: PriceUnit::Per1K,
            entries: vec![entry(Some("m"), PriceSource::Manual, &[("input", 0.02)], 1.0)],
            ..Default::default()
        };
        let p = PricingEngineImpl::new(&cfg).resolve("m", &provider(), &cred());
        approx(p.divisor, 1_000.0);
        approx(p.cost_of(&usage(&[(TokenKind::Input, 500)])).amount, 0.01);
    }

    // ---------------------------------------------------- plan_fee

    #[test]
    fn plan_fee_is_resolved_but_not_charged_per_request() {
        // 07 §6.2 的例 4：只写 plan_fee，不写 rates
        let mut by_cred = entry(None, PriceSource::Manual, &[], 1.0);
        by_cred.r#match.credential = Some(cred());
        by_cred.plan_fee = Some(PlanFee {
            amount: 99.0,
            currency: "CNY".into(),
            window: QuotaWindow::Month,
            ..Default::default()
        });
        let e = engine(vec![by_cred]);

        let p = e.resolve("gpt-4o", &provider(), &cred());
        assert!(p.plan_fee.is_some(), "套餐费要能被成本中心看到");
        let fee = p.plan_fee.as_ref().unwrap();
        approx(fee.amount, 99.0);
        assert_eq!(fee.currency, "CNY", "套餐费用它自己在 plan_fee 上的币种");
        // ResolvedPrice.currency 描述的是**按量费率**的币种：条目没写 currency
        // → 回退到 pricing.defaults.currency（USD），不能被 plan_fee 的币种带跑
        assert_eq!(p.currency, "USD");
        assert_eq!(p.source, PriceSourceKind::Manual);
        // 单次请求只付按量成本（这里费率来自内置兜底 2.50/M），**绝不把 ¥99 摊进一次请求**
        let one_request = p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)]));
        approx(one_request.amount, 2.50);
        assert!(one_request.amount < 99.0);
    }

    /// 单次成本与 `plan_fee` 完全无关：改套餐费，`cost_of` 一分不变。
    #[test]
    fn cost_of_is_independent_of_plan_fee() {
        let mut e1 =
            entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 2.5), ("output", 10.0)], 1.0);
        let u = usage(&[(TokenKind::Input, 1_000_000), (TokenKind::Output, 1_000_000)]);
        let e_no_fee = engine(vec![e1.clone()]);
        let before = e_no_fee.resolve("gpt-4o", &provider(), &cred()).cost_of(&u);

        e1.plan_fee = Some(PlanFee {
            amount: 9999.0,
            currency: "CNY".into(),
            window: QuotaWindow::Month,
            ..Default::default()
        });
        let e_fee = engine(vec![e1]);
        let p = e_fee.resolve("gpt-4o", &provider(), &cred());
        assert!(p.plan_fee.is_some());
        approx(p.cost_of(&u).amount, before.amount);
        approx(before.amount, 12.5);
    }

    /// 条目自己写了 `currency` 就用它（"币种跟着用户配置走"）。
    #[test]
    fn entry_currency_overrides_the_default() {
        let mut e1 = entry(Some("gpt-4o"), PriceSource::Manual, &[("input", 2.5)], 1.0);
        e1.currency = Some("CNY".into());
        let p = engine(vec![e1]).resolve("gpt-4o", &provider(), &cred());
        assert_eq!(p.currency, "CNY");
        assert_eq!(p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)])).currency, "CNY");
    }

    #[test]
    fn plan_fee_only_without_builtin_table_is_priced_but_rateless() {
        let mut by_cred = entry(None, PriceSource::Manual, &[], 1.0);
        by_cred.r#match.credential = Some(cred());
        by_cred.plan_fee = Some(PlanFee {
            amount: 99.0,
            currency: "CNY".into(),
            window: QuotaWindow::Month,
            ..Default::default()
        });
        let e = engine(vec![by_cred]).without_builtin_table();
        let p = e.resolve("gpt-4o", &provider(), &cred());
        assert!(p.is_priced(), "命中了套餐条目 → 不是'未定价'");
        assert!(!p.has_rates(), "但确实没有按量费率，UI 要能区分");
        approx(p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)])).amount, 0.0);
        approx(p.plan_fee.unwrap().amount, 99.0);
    }

    #[test]
    fn plan_fee_and_rates_can_coexist() {
        let mut e1 = entry(
            Some("claude-sonnet-4-5"),
            PriceSource::Manual,
            &[("input", 3.0), ("output", 15.0)],
            1.0,
        );
        e1.plan_fee = Some(PlanFee {
            amount: 199.0,
            currency: "CNY".into(),
            window: QuotaWindow::Month,
            ..Default::default()
        });
        let e = engine(vec![e1]);
        let p = e.resolve("claude-sonnet-4-5", &provider(), &cred());
        assert!(p.has_rates());
        approx(p.cost_of(&usage(&[(TokenKind::Input, 1_000_000)])).amount, 3.0);
        approx(p.plan_fee.unwrap().amount, 199.0);
    }

    // ---------------------------------------------------- 快照不变性

    #[test]
    fn changing_prices_does_not_change_history() {
        let mut cfg = PricingConfig {
            entries: vec![entry(
                Some("gpt-4o"),
                PriceSource::Manual,
                &[("input", 2.5), ("input_cached", 1.25), ("output", 10.0)],
                0.8,
            )],
            ..Default::default()
        };
        let u = usage(&[
            (TokenKind::Input, 1_200),
            (TokenKind::InputCached, 51_200),
            (TokenKind::Output, 386),
        ]);

        // ① 请求发生时：解析 + 计算 + 快照
        let before = PricingEngineImpl::new(&cfg).resolve("gpt-4o", &provider(), &cred());
        let historical_cost = before.cost_of(&u);
        let savings_then = cache_savings(&u, &before);
        let snapshot = before.to_snapshot();

        // ② 用户改价（input 涨 100 倍，倍率也改）
        cfg.entries[0].rates.insert("input".into(), 250.0);
        cfg.entries[0].multiplier = 2.0;
        let after = PricingEngineImpl::new(&cfg).resolve("gpt-4o", &provider(), &cred());
        let new_cost = after.cost_of(&u);
        assert!(
            new_cost.amount > historical_cost.amount * 5.0,
            "改价后新请求的成本必须变（{new_cost:?} vs {historical_cost:?}）"
        );

        // ③ 历史记录的成本用快照重算 → 一分不变
        let restored = ResolvedPrice::from_snapshot(&snapshot).expect("快照要能还原");
        approx(restored.cost_of(&u).amount, historical_cost.amount);
        approx(cost_of_snapshot(&snapshot, &u).unwrap().amount, historical_cost.amount);
        approx(cache_savings(&u, &restored).amount, savings_then.amount);
        approx(restored.multiplier, 0.8);
        assert_eq!(restored.rates[&TokenKind::Input], 2.5);
    }

    #[test]
    fn snapshot_survives_serde_roundtrip() {
        let e = engine(vec![entry(
            Some("gpt-4o"),
            PriceSource::Imported,
            &[("input", 2.0), ("output", 8.0)],
            1.25,
        )]);
        let p = e.resolve("gpt-4o", &provider(), &cred());
        let snap = p.to_snapshot();
        let text = serde_json::to_string(&snap).unwrap();
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        let restored = ResolvedPrice::from_snapshot(&back).unwrap();
        let u = usage(&[(TokenKind::Input, 10_000), (TokenKind::Output, 5_000)]);
        approx(restored.cost_of(&u).amount, p.cost_of(&u).amount);
        assert_eq!(restored.source, PriceSourceKind::Imported);
        assert_eq!(restored.entry_index, 0);
    }

    // ---------------------------------------------------- 端到端：一份真实配置

    #[test]
    fn realistic_config_resolves_like_the_docs_example() {
        // 07 §6.2 的定价表
        let mut claude = entry(
            Some("claude-sonnet-4-5"),
            PriceSource::Builtin,
            &[
                ("input", 3.00),
                ("input_cached", 0.30),
                ("input_cache_write", 3.75),
                ("input_cache_write_1h", 6.00),
                ("output", 15.00),
            ],
            1.0,
        );
        claude.r#match.provider = Some(ProviderId::new("p-anthropic"));

        let gpt = entry(
            Some("gpt-4o*"),
            PriceSource::Builtin,
            &[("input", 2.50), ("input_cached", 1.25), ("output", 10.00)],
            1.0,
        );

        let mut relay = entry(None, PriceSource::Manual, &[], 0.8);
        relay.r#match.provider = Some(ProviderId::new("p-mytoken"));

        let mut plan = entry(None, PriceSource::Manual, &[], 1.0);
        plan.r#match.credential = Some(CredentialId::new("c-relay-a"));
        plan.plan_fee = Some(PlanFee {
            amount: 99.0,
            currency: "CNY".into(),
            window: QuotaWindow::Month,
            ..Default::default()
        });

        let e = engine(vec![claude, gpt, relay, plan]);
        let credits = CredentialId::new("c-1");

        // Claude 走官方价
        let p = e.resolve("claude-sonnet-4-5", &ProviderId::new("p-anthropic"), &credits);
        approx(p.rates[&TokenKind::Input], 3.0);
        approx(p.multiplier, 1.0);
        assert!(p.plan_fee.is_none());

        // 中转站：配置里的 gpt-4o* 价 × 0.8（配置的 builtin 条目优先于静态表）
        let p = e.resolve("gpt-4o-2024-11-20", &ProviderId::new("p-mytoken"), &credits);
        approx(p.rates[&TokenKind::Input], 2.50);
        approx(p.multiplier, 0.8);

        // 固定套餐：c-relay-a 上带月费
        let p = e.resolve(
            "claude-sonnet-4-5",
            &ProviderId::new("p-anthropic"),
            &CredentialId::new("c-relay-a"),
        );
        approx(p.plan_fee.unwrap().amount, 99.0);

        // 成本：gpt-4o 走中转倍率
        let u = usage(&[(TokenKind::Input, 1_000_000), (TokenKind::Output, 100_000)]);
        let p = e.resolve("gpt-4o", &ProviderId::new("p-mytoken"), &credits);
        let expect = (2.5 * 1_000_000.0 + 10.0 * 100_000.0) * 0.8 / 1_000_000.0;
        approx(e.cost(&u, &p).amount, expect);
        assert_eq!(e.cost(&u, &p).currency, "USD");
    }
}

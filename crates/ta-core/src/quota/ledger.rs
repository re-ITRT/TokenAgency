//! 额度账本的**实现**：四个观测源合成（取最保守值）+ 状态机 + 窗口滚动。
//!
//! 契约与规则来源：[03 §4](../../../docs/03-routing-and-policy.md)。
//!
//! ## 四个观测源
//!
//! | 源 | 入口 | 精度 |
//! |---|---|---|
//! | A 上游响应头 | [`QuotaObservation::Header`] | 高（若提供） |
//! | B 本地计量 | [`QuotaObservation::Local`] | 中 |
//! | C 手工额度（`Credential.quota`） | 构造时注入 [`crate::domain::QuotaConfig`] | 由用户保证 |
//! | D 状态码语义 | [`QuotaObservation::StatusCode`] | 高（事后） |
//!
//! ## 合成规则（保守优先，绝不允许乐观）
//!
//! ```text
//! remaining_ratio = min( 手工剩余比例, 响应头剩余比例, 1 − 本地累计/limit )
//! used_ratio      = 1 − remaining_ratio
//! ```
//!
//! 手工剩余比例 = `(limit − 已用 − reserve) / limit`，所以配了 `reserve` 预留时，
//! 手工源会比"本地累计"更保守 —— 这正是"留一点余量别撞墙"的实现。
//!
//! ## 状态机
//!
//! ```text
//! unknown ──首次观测──> ok ──used_ratio ≥ soft_limit──> soft ──≥ hard_limit──> exhausted
//!                       ↑                                                       │
//!                       └──── 窗口滚动 / 手动清零 / 响应头 remaining 回升 ──────┘
//! ```

use std::sync::Arc;

use chrono::{Datelike, TimeZone, Utc};

use crate::clock::{Clock, SystemClock};
use crate::domain::{QuotaConfig, QuotaMetric, QuotaWindow};
use crate::quota::{QuotaState, QuotaStateKind};
use crate::types::CredentialId;

use super::backend::{MemoryQuotaBackend, QuotaBackend, QuotaRecord};
use super::{
    looks_like_invalid_credential, looks_like_quota_exhausted, QuotaLedger, QuotaObservation,
};

/// 一次错误观测对账本产生的效果（网关据此决定"换凭据/通知用户"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaEffect {
    /// 只是限流或普通错误，账本不变
    None,
    /// 额度耗尽 → 换凭据
    Exhausted,
    /// key 失效/被封 → 换凭据 + 通知（TA-4001）
    Invalid,
}

// ---------------------------------------------------------------- 窗口计算

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// 当前窗口的 `[start, end)`（Unix 毫秒），**按 UTC 对齐**。`QuotaWindow::None` → `(0, None)`。
///
/// 真实使用请用 [`window_bounds_local`]：`reset_day` 是"**本地**每月几号重置"，
/// 按 UTC 算的话北京时间每月 1 号 0–8 点这段会判错（额度还没刷新就先被标耗尽）。
pub fn window_bounds(window: QuotaWindow, reset_day: u32, now_ms: i64) -> (i64, Option<i64>) {
    window_bounds_local(window, reset_day, now_ms, 0)
}

/// 当前窗口的 `[start, end)`，按**本地时区**（`Clock::local_offset_minutes`）对齐。
///
/// - `Hour` / `Day`：本地整点 / 本地零点
/// - `Month`：本地 `reset_day` 号零点（1–28，避免 2 月 29/30 日的坑）
pub fn window_bounds_local(
    window: QuotaWindow,
    reset_day: u32,
    now_ms: i64,
    local_offset_minutes: i32,
) -> (i64, Option<i64>) {
    let off = local_offset_minutes as i64 * 60_000;
    // 先平移到"本地时间轴"上，按整点/整月对齐后再平移回来
    let shifted = now_ms.saturating_add(off);
    let (s_local, e_local) = match window {
        QuotaWindow::None => return (0, None),
        QuotaWindow::Hour => {
            let s = shifted - shifted.rem_euclid(HOUR_MS);
            (s, s + HOUR_MS)
        }
        QuotaWindow::Day => {
            let s = shifted - shifted.rem_euclid(DAY_MS);
            (s, s + DAY_MS)
        }
        QuotaWindow::Month => {
            let day = reset_day.clamp(1, 28);
            let s = month_reset_ms(shifted, day, false);
            let dt = chrono::DateTime::<Utc>::from_timestamp_millis(s).unwrap_or_default();
            let (ny, nm) = next_month(dt.year(), dt.month());
            (s, month_day_ms(ny, nm, day))
        }
    };
    (s_local - off, Some(e_local - off))
}

fn month_day_ms(year: i32, month: u32, day: u32) -> i64 {
    let d = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .or_else(|| chrono::NaiveDate::from_ymd_opt(year, month, 1))
        .unwrap_or_default();
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()).timestamp_millis()
}

/// 距离 `now_ms` 最近的一个 `day` 号 00:00；`next=true` 时取之后那个。
fn month_reset_ms(now_ms: i64, day: u32, next: bool) -> i64 {
    let now = chrono::DateTime::<Utc>::from_timestamp_millis(now_ms).unwrap_or_default();
    let cur = month_day_ms(now.year(), now.month(), day);
    if !next && cur <= now_ms {
        return cur;
    }
    if next && cur > now_ms {
        return cur;
    }
    let (y, m) = if next {
        next_month(now.year(), now.month())
    } else {
        prev_month(now.year(), now.month())
    };
    month_day_ms(y, m, day)
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month <= 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

/// 该 metric 口径下的"已用量"。
///
/// `Unknown`（用户没填口径）时按 输入侧 token → 请求数 → 金额 的顺序取第一个有值的，
/// 保证"只管配了 limit 就能生效"，不至于因为漏填口径就完全失效。
fn metric_used(metric: QuotaMetric, st: &QuotaState) -> f64 {
    match metric {
        QuotaMetric::Tokens => st.used_tokens as f64,
        QuotaMetric::Requests => st.used_requests as f64,
        QuotaMetric::Cost => st.used_cost,
        QuotaMetric::Unknown => {
            if st.used_tokens > 0 {
                st.used_tokens as f64
            } else if st.used_requests > 0 {
                st.used_requests as f64
            } else {
                st.used_cost
            }
        }
    }
}

// ---------------------------------------------------------------- 账本实现

/// 内存态账本。`Arc<QuotaLedgerImpl>` 直接满足 `Arc<dyn QuotaLedger>`。
pub struct QuotaLedgerImpl {
    backend: Arc<dyn QuotaBackend>,
    clock: Arc<dyn Clock>,
    /// 配置没给阈值时的兜底（与 `QuotaConfig` 的默认值一致）
    default_soft_limit: f64,
    default_hard_limit: f64,
}

impl QuotaLedgerImpl {
    pub fn new(backend: Arc<dyn QuotaBackend>) -> Self {
        Self::with_clock(backend, Arc::new(SystemClock))
    }

    pub fn with_clock(backend: Arc<dyn QuotaBackend>, clock: Arc<dyn Clock>) -> Self {
        Self { backend, clock, default_soft_limit: 0.8, default_hard_limit: 1.0 }
    }

    /// 纯内存账本（无库、无配置注入也能跑：只靠响应头 + 本地计量）。
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryQuotaBackend::new()))
    }

    pub fn with_limits(mut self, soft: f64, hard: f64) -> Self {
        if soft > 0.0 {
            self.default_soft_limit = soft;
        }
        if hard > 0.0 {
            self.default_hard_limit = hard;
        }
        self
    }

    /// C 源：写入用户手工额度（`Credential.quota`）。
    pub fn set_quota_config(&self, cred: &CredentialId, cfg: QuotaConfig) {
        self.backend.set_quota_config(cred, cfg);
    }

    pub fn quota_config(&self, cred: &CredentialId) -> QuotaConfig {
        self.backend.quota_config(cred).unwrap_or_default()
    }

    /// 该凭据是否被判为"key 失效/被封"（账本本身不路由，这是给人的提示位）。
    pub fn is_invalid(&self, cred: &CredentialId) -> bool {
        self.backend.get(cred).map(|r| r.invalid).unwrap_or(false)
    }

    /// 便捷入口：用上游错误体更新账本，并返回账本判定结果（G4 接线点）。
    pub fn observe_error(
        &self,
        cred: &CredentialId,
        status: u16,
        error_code: Option<&str>,
        message: &str,
    ) -> QuotaEffect {
        let body = format!("{} {}", error_code.unwrap_or_default(), message);
        let invalid = looks_like_invalid_credential(status, &body);
        let exhausted = !invalid && looks_like_quota_exhausted(status, &body);
        self.observe(
            cred,
            QuotaObservation::StatusCode {
                status,
                error_code: error_code.map(|s| s.to_string()),
                message: Some(message.to_string()),
            },
        );
        if invalid {
            QuotaEffect::Invalid
        } else if exhausted {
            QuotaEffect::Exhausted
        } else {
            QuotaEffect::None
        }
    }

    // ------------------------------------------------------------ 内部

    /// 本进程本地时区下的窗口边界。
    fn bounds(&self, cfg: &QuotaConfig, now_ms: i64) -> (i64, Option<i64>) {
        window_bounds_local(cfg.window, cfg.reset_day, now_ms, self.clock.local_offset_minutes())
    }

    /// 读取 + 惰性滚动 + 重算（不落库除非真的变了）。
    fn refresh(&self, cred: &CredentialId, now_ms: i64) -> Option<QuotaRecord> {
        let mut rec = self.backend.get(cred)?;
        let cfg = self.quota_config(cred);
        let before = (
            rec.state.window_start_ms,
            rec.state.kind,
            rec.state.used_tokens,
            rec.state.used_ratio,
        );
        self.sync_window(&mut rec, &cfg, now_ms);
        self.recompute(&mut rec, &cfg, now_ms);
        let after = (
            rec.state.window_start_ms,
            rec.state.kind,
            rec.state.used_tokens,
            rec.state.used_ratio,
        );
        if before != after {
            self.backend.put(&rec);
        }
        Some(rec)
    }

    /// 窗口对齐：没初始化就写当前窗口；跨窗口就清零（= 复活）。
    fn sync_window(&self, rec: &mut QuotaRecord, cfg: &QuotaConfig, now_ms: i64) {
        if cfg.window == QuotaWindow::None {
            return;
        }
        let (start, end) = self.bounds(cfg, now_ms);
        if rec.state.window_start_ms == 0 {
            rec.state.window_start_ms = start;
            rec.state.window_end_ms = end;
        } else if rec.state.window_start_ms != start {
            Self::clear_counters(rec);
            rec.state.window_start_ms = start;
            rec.state.window_end_ms = end;
            // 新周期 → 解除硬性耗尽；但 key 失效与周期无关，不解除
            rec.hard_exhausted = false;
        }
    }

    fn clear_counters(rec: &mut QuotaRecord) {
        rec.state.used_tokens = 0;
        rec.state.used_requests = 0;
        rec.state.used_cost = 0.0;
        rec.state.reported_remaining = None;
        rec.state.reported_reset_at_ms = None;
    }

    fn observed(rec: &QuotaRecord) -> bool {
        !rec.never_observed()
    }

    /// 四源合成 + 状态机判定。
    fn recompute(&self, rec: &mut QuotaRecord, cfg: &QuotaConfig, now_ms: i64) {
        if cfg.window != QuotaWindow::None && rec.state.window_start_ms == 0 {
            let (s, e) = self.bounds(cfg, now_ms);
            rec.state.window_start_ms = s;
            rec.state.window_end_ms = e;
        }
        if !cfg.currency.is_empty() {
            rec.state.currency = cfg.currency.clone();
        }

        let used = metric_used(cfg.metric, &rec.state);
        let limit = cfg.limit.filter(|l| *l > 0.0 && l.is_finite());

        // ---- 四源合成：每一源各自给出"剩余比例"，取最小值 ----
        let mut remaining: Vec<f64> = Vec::with_capacity(3);
        if let Some(l) = limit {
            // C 源：手工额度（扣除 reserve 预留）
            let manual_left = (l - used - cfg.reserve.max(0.0)).max(0.0);
            remaining.push((manual_left / l).clamp(0.0, 1.0));
            // B 源：本地累计
            remaining.push((1.0 - used / l).clamp(0.0, 1.0));
        }
        if let Some(r) = rec.state.reported_remaining {
            // A 源：响应头。没有 limit 时无法换算成比例 → 只能表达"有/没有"
            remaining.push(match limit {
                Some(l) => (r.max(0.0) / l).clamp(0.0, 1.0),
                None => {
                    if r > 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
            });
        }
        rec.state.used_ratio = if remaining.is_empty() {
            None
        } else {
            let min_left = remaining.iter().cloned().fold(f64::INFINITY, f64::min);
            Some((1.0 - min_left).clamp(0.0, 1.0))
        };

        // ---- 状态机 ----
        rec.state.kind = if !Self::observed(rec) {
            QuotaStateKind::Unknown
        } else if rec.invalid || rec.hard_exhausted {
            QuotaStateKind::Exhausted
        } else {
            match rec.state.used_ratio {
                // 有观测但没有任何可量化口径（既没 limit 也没响应头）→ 视作可用
                None => QuotaStateKind::Ok,
                Some(u) => {
                    let soft =
                        if cfg.soft_limit > 0.0 { cfg.soft_limit } else { self.default_soft_limit };
                    let hard =
                        if cfg.hard_limit > 0.0 { cfg.hard_limit } else { self.default_hard_limit };
                    if u >= hard {
                        QuotaStateKind::Exhausted
                    } else if u >= soft {
                        QuotaStateKind::Soft
                    } else {
                        QuotaStateKind::Ok
                    }
                }
            }
        };
    }
}

impl QuotaLedger for QuotaLedgerImpl {
    fn state(&self, cred: &CredentialId) -> QuotaState {
        let now = self.clock.now_ms();
        match self.refresh(cred, now) {
            Some(rec) => rec.state,
            None => QuotaState { credential_id: cred.clone(), ..Default::default() },
        }
    }

    fn observe(&self, cred: &CredentialId, obs: QuotaObservation) {
        let now = self.clock.now_ms();
        let cfg = self.quota_config(cred);
        let mut rec = self.backend.get(cred).unwrap_or_else(|| QuotaRecord::new(cred));
        // 观测前先对齐窗口，避免跨周期的观测被计进旧窗口
        self.sync_window(&mut rec, &cfg, now);

        match obs {
            QuotaObservation::Header { remaining, reset_at_ms } => {
                if let Some(r) = remaining {
                    // 响应头显示还有剩余 = 上游已重置 → 自动复活（§4.2）
                    if r > 0.0 {
                        rec.hard_exhausted = false;
                        rec.invalid = false;
                    }
                    rec.state.reported_remaining = Some(r);
                }
                if let Some(t) = reset_at_ms {
                    rec.state.reported_reset_at_ms = Some(t);
                }
            }
            QuotaObservation::Local { tokens, requests, cost } => {
                rec.state.used_tokens = rec.state.used_tokens.saturating_add(tokens);
                rec.state.used_requests = rec.state.used_requests.saturating_add(requests);
                rec.state.used_cost += cost;
            }
            QuotaObservation::ManualReset => {
                Self::clear_counters(&mut rec);
                rec.hard_exhausted = false;
                rec.invalid = false;
                // 用户明确说"我充值了/清零" → 就是一次观测，状态从 unknown 进入 ok
                rec.state.updated_at_ms = now;
            }
            QuotaObservation::StatusCode { status, error_code, message } => {
                let body =
                    format!("{} {}", error_code.unwrap_or_default(), message.unwrap_or_default());
                if looks_like_invalid_credential(status, &body) {
                    rec.invalid = true;
                    rec.hard_exhausted = true;
                } else if looks_like_quota_exhausted(status, &body) {
                    rec.hard_exhausted = true;
                }
            }
        }

        self.recompute(&mut rec, &cfg, now);
        rec.state.updated_at_ms = now;
        self.backend.put(&rec);
    }

    fn mark_exhausted(&self, cred: &CredentialId, hint_reset_at_ms: Option<i64>) {
        let now = self.clock.now_ms();
        let cfg = self.quota_config(cred);
        let mut rec = self.backend.get(cred).unwrap_or_else(|| QuotaRecord::new(cred));
        self.sync_window(&mut rec, &cfg, now);
        rec.hard_exhausted = true;
        if let Some(h) = hint_reset_at_ms {
            rec.state.reported_reset_at_ms = Some(h);
            rec.state.window_end_ms = Some(h);
        }
        self.recompute(&mut rec, &cfg, now);
        rec.state.updated_at_ms = now;
        self.backend.put(&rec);
    }

    fn mark_invalid(&self, cred: &CredentialId) {
        let now = self.clock.now_ms();
        let cfg = self.quota_config(cred);
        let mut rec = self.backend.get(cred).unwrap_or_else(|| QuotaRecord::new(cred));
        self.sync_window(&mut rec, &cfg, now);
        rec.invalid = true;
        rec.hard_exhausted = true;
        self.recompute(&mut rec, &cfg, now);
        rec.state.updated_at_ms = now;
        self.backend.put(&rec);
    }

    fn roll_window(&self, now_ms: i64) -> Vec<CredentialId> {
        let mut revived = Vec::new();
        for mut rec in self.backend.all() {
            let cred = rec.state.credential_id.clone();
            let cfg = self.quota_config(&cred);
            if cfg.window == QuotaWindow::None {
                continue;
            }
            let (start, end) = self.bounds(&cfg, now_ms);
            if rec.state.window_start_ms == 0 {
                // 首次：只初始化窗口，不动状态机
                rec.state.window_start_ms = start;
                rec.state.window_end_ms = end;
                self.backend.put(&rec);
                continue;
            }
            if rec.state.window_start_ms == start {
                continue;
            }
            let was_observed = Self::observed(&rec);
            let prev_kind = rec.state.kind;
            Self::clear_counters(&mut rec);
            rec.state.window_start_ms = start;
            rec.state.window_end_ms = end;
            rec.hard_exhausted = false;
            self.recompute(&mut rec, &cfg, now_ms);
            rec.state.updated_at_ms = now_ms;
            self.backend.put(&rec);
            // 回升 = 之前是耗尽/告警，滚完又能用了
            if was_observed
                && matches!(prev_kind, QuotaStateKind::Exhausted | QuotaStateKind::Soft)
                && rec.state.is_usable()
            {
                revived.push(cred);
            }
        }
        revived
    }

    fn remaining_ratio(&self, cred: &CredentialId) -> Option<f64> {
        let now = self.clock.now_ms();
        let rec = self.refresh(cred, now)?;
        match rec.state.kind {
            // 耗尽必须给出 0：策略 `quota_remaining_lt: 0.2` 与 `quota_first` 依赖它
            QuotaStateKind::Exhausted => Some(0.0),
            _ => rec.state.used_ratio.map(|u| (1.0 - u).clamp(0.0, 1.0)),
        }
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn ts(rfc3339: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp_millis()
    }

    struct Fixture {
        ledger: QuotaLedgerImpl,
        clock: Arc<crate::clock::ManualClock>,
        cred: CredentialId,
    }

    fn fixture(t0: &str, cfg: QuotaConfig) -> Fixture {
        let clock = Arc::new(crate::clock::ManualClock::new(ts(t0)));
        let ledger = QuotaLedgerImpl::with_clock(
            Arc::new(MemoryQuotaBackend::new()),
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let cred = CredentialId::new("c-test");
        ledger.set_quota_config(&cred, cfg);
        Fixture { ledger, clock, cred }
    }

    fn tokens_cfg(limit: f64) -> QuotaConfig {
        QuotaConfig {
            metric: QuotaMetric::Tokens,
            limit: Some(limit),
            window: QuotaWindow::Month,
            reset_day: 1,
            soft_limit: 0.8,
            hard_limit: 1.0,
            reserve: 0.0,
            currency: "USD".into(),
            ..Default::default()
        }
    }

    fn local(f: &Fixture, tokens: u64) {
        f.ledger.observe(&f.cred, QuotaObservation::Local { tokens, requests: 1, cost: 0.0 });
    }

    // ---------------------------------------------------- 基础 / 状态机

    #[test]
    fn unknown_credential_is_unknown_without_ratio() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Unknown);
        assert_eq!(st.used_ratio, None);
        assert_eq!(f.ledger.remaining_ratio(&f.cred), None);
        assert!(st.is_usable());
    }

    #[test]
    fn completely_unknown_credential_defaults_to_unknown() {
        let ledger = QuotaLedgerImpl::in_memory();
        let st = ledger.state(&CredentialId::new("nobody"));
        assert_eq!(st.kind, QuotaStateKind::Unknown);
        assert_eq!(st.credential_id.as_str(), "nobody");
    }

    #[test]
    fn first_observation_moves_unknown_to_ok_then_soft_then_exhausted() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        local(&f, 100);
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Ok);
        assert_eq!(st.used_tokens, 100);
        assert!((st.used_ratio.unwrap() - 0.1).abs() < 1e-9);

        local(&f, 750); // 850 / 1000 = 85% ≥ soft 80%
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Soft);
        assert!(st.is_usable(), "soft 仍可路由，由策略决定绕开");

        local(&f, 150); // 1000 / 1000 = 100%
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Exhausted);
        assert!(st.is_exhausted());
        assert_eq!(f.ledger.remaining_ratio(&f.cred), Some(0.0));
    }

    #[test]
    fn local_observations_accumulate_tokens_requests_and_cost() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        for _ in 0..3 {
            f.ledger
                .observe(&f.cred, QuotaObservation::Local { tokens: 10, requests: 2, cost: 0.25 });
        }
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.used_tokens, 30);
        assert_eq!(st.used_requests, 6);
        assert!((st.used_cost - 0.75).abs() < 1e-9);
    }

    // ---------------------------------------------------- 四源取最保守

    #[test]
    fn four_sources_take_the_most_conservative_value() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        local(&f, 850); // B 源：本地 85%
        let after_local = f.ledger.state(&f.cred);
        assert!((after_local.used_ratio.unwrap() - 0.85).abs() < 1e-9);

        // A 源说只剩 100 → 比本地更保守 → 采信 A
        f.ledger.observe(
            &f.cred,
            QuotaObservation::Header { remaining: Some(100.0), reset_at_ms: None },
        );
        let st = f.ledger.state(&f.cred);
        assert!((st.used_ratio.unwrap() - 0.90).abs() < 1e-9, "取 min(0.15, 0.10) = 0.10");
        assert_eq!(st.reported_remaining, Some(100.0));

        // A 源回升到 400 → 本地仍是 85% → 采信本地（更保守）
        f.ledger.observe(
            &f.cred,
            QuotaObservation::Header { remaining: Some(400.0), reset_at_ms: None },
        );
        let st = f.ledger.state(&f.cred);
        assert!((st.used_ratio.unwrap() - 0.85).abs() < 1e-9);
    }

    #[test]
    fn reserve_makes_the_manual_source_more_conservative() {
        let mut cfg = tokens_cfg(1000.0);
        cfg.reserve = 100.0; // 预留 100 → 手工剩余到 900 就用完
        let f = fixture("2026-01-15T00:00:00Z", cfg);
        local(&f, 900);
        let st = f.ledger.state(&f.cred);
        assert_eq!(
            st.kind,
            QuotaStateKind::Exhausted,
            "本地只用了 90%，但预留额度已触底 → 必须按最保守处理"
        );
        assert_eq!(f.ledger.remaining_ratio(&f.cred), Some(0.0));
    }

    #[test]
    fn header_only_reports_exhaustion_even_without_limit() {
        // 没配手工额度：完全依赖响应头
        let f = fixture("2026-01-15T00:00:00Z", QuotaConfig::default());
        f.ledger
            .observe(&f.cred, QuotaObservation::Header { remaining: Some(0.0), reset_at_ms: None });
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 响应头回升 → 复活
        let cred2 = CredentialId::new("c-2");
        f.ledger.set_quota_config(&cred2, QuotaConfig::default());
        f.ledger.observe(
            &cred2,
            QuotaObservation::Header { remaining: Some(5000.0), reset_at_ms: None },
        );
        assert_eq!(f.ledger.state(&cred2).kind, QuotaStateKind::Ok);
    }

    #[test]
    fn requests_and_cost_metrics_are_supported() {
        let mut req_cfg = tokens_cfg(10.0);
        req_cfg.metric = QuotaMetric::Requests;
        let f = fixture("2026-01-15T00:00:00Z", req_cfg);
        for _ in 0..9 {
            local(&f, 1_000_000); // token 很多，但口径是请求数 → 不受影响
        }
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Soft); // 9/10 = 90%

        let mut cost_cfg = tokens_cfg(20.0);
        cost_cfg.metric = QuotaMetric::Cost;
        let g = fixture("2026-01-15T00:00:00Z", cost_cfg);
        g.ledger.observe(&g.cred, QuotaObservation::Local { tokens: 0, requests: 0, cost: 19.0 });
        assert_eq!(g.ledger.state(&g.cred).kind, QuotaStateKind::Soft);
    }

    // ---------------------------------------------------- D 源 / 错误语义

    #[test]
    fn status_code_insufficient_quota_marks_exhausted() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        let eff = f.ledger.observe_error(
            &f.cred,
            429,
            Some("insufficient_quota"),
            "You exceeded your current quota, please check your plan and billing details.",
        );
        assert_eq!(eff, QuotaEffect::Exhausted);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);
        assert!(!f.ledger.is_invalid(&f.cred));
    }

    #[test]
    fn status_code_402_and_403_balance_mark_exhausted() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        assert_eq!(
            f.ledger.observe_error(&f.cred, 402, None, "Payment Required"),
            QuotaEffect::Exhausted
        );
        let g = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        assert_eq!(
            g.ledger.observe_error(&g.cred, 403, None, "您的账户余额不足，请充值"),
            QuotaEffect::Exhausted
        );
    }

    #[test]
    fn status_code_401_and_403_api_key_mark_invalid() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        assert_eq!(
            f.ledger.observe_error(&f.cred, 401, None, "Unauthorized"),
            QuotaEffect::Invalid
        );
        assert!(f.ledger.is_invalid(&f.cred));
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        let g = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        assert_eq!(
            g.ledger.observe_error(&g.cred, 403, Some("invalid_api_key"), "密钥无效"),
            QuotaEffect::Invalid
        );
    }

    #[test]
    fn plain_rate_limit_is_not_quota_exhaustion() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        let eff = f.ledger.observe_error(
            &f.cred,
            429,
            Some("rate_limit_exceeded"),
            "Rate limit reached for gpt-4o in organization org-x on requests per min (RPM): Limit 500.",
        );
        assert_eq!(eff, QuotaEffect::None);
        assert_ne!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);
    }

    #[test]
    fn mark_exhausted_and_mark_invalid_set_state_directly() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        f.ledger.mark_exhausted(&f.cred, Some(ts("2026-02-01T00:00:00Z")));
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Exhausted);
        assert_eq!(st.reported_reset_at_ms, Some(ts("2026-02-01T00:00:00Z")));

        let g = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        g.ledger.mark_invalid(&g.cred);
        assert!(g.ledger.is_invalid(&g.cred));
        assert!(g.ledger.state(&g.cred).is_exhausted());
    }

    #[test]
    fn header_rebound_revives_a_hard_exhausted_credential() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        f.ledger.mark_exhausted(&f.cred, None);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 上游其实重置了：响应头显示还剩 500
        f.ledger.observe(
            &f.cred,
            QuotaObservation::Header {
                remaining: Some(500.0),
                reset_at_ms: Some(ts("2026-02-01T00:00:00Z")),
            },
        );
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Ok, "响应头回升 → 自动复活");
        assert!((st.used_ratio.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(st.reported_reset_at_ms, Some(ts("2026-02-01T00:00:00Z")));
    }

    // ---------------------------------------------------- 窗口滚动 / 复活

    #[test]
    fn roll_window_revives_exhausted_at_month_boundary() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        local(&f, 1000);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 同月内滚动：什么都没发生
        f.clock.advance(5 * DAY_MS); // → 2026-01-20
        assert!(f.ledger.roll_window(f.clock.now_ms()).is_empty());
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 跨月（reset_day = 1）→ 复活
        f.clock.advance(12 * DAY_MS); // → 2026-02-01
        let revived = f.ledger.roll_window(f.clock.now_ms());
        assert_eq!(revived, vec![f.cred.clone()]);
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Ok);
        assert_eq!(st.used_tokens, 0);
        assert_eq!(st.used_requests, 0);
        assert_eq!(st.used_ratio, Some(0.0));
        assert_eq!(f.ledger.remaining_ratio(&f.cred), Some(1.0));
        // 窗口按**本地时区**（FixedClock/ManualClock 默认 +08:00）对齐：
        // 本地 2/1 00:00 = UTC 1/31 16:00
        assert_eq!(st.window_start_ms, ts("2026-01-31T16:00:00Z"));
    }

    #[test]
    fn roll_window_works_for_hour_window_and_soft_state() {
        let mut cfg = tokens_cfg(100.0);
        cfg.window = QuotaWindow::Hour;
        let f = fixture("2026-01-15T10:00:00Z", cfg);
        local(&f, 85);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Soft);

        f.clock.advance(HOUR_MS);
        let revived = f.ledger.roll_window(f.clock.now_ms());
        assert_eq!(revived, vec![f.cred.clone()], "soft 也是回升");
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Ok);
    }

    #[test]
    fn lazy_window_roll_happens_even_without_explicit_call() {
        let mut cfg = tokens_cfg(100.0);
        cfg.window = QuotaWindow::Day;
        let f = fixture("2026-01-15T10:00:00Z", cfg);
        local(&f, 100);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 调度器忘了调 roll_window 也不能永久卡在 exhausted
        f.clock.advance(DAY_MS);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Ok);
    }

    #[test]
    fn invalid_credential_does_not_revive_on_window_roll() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        f.ledger.mark_invalid(&f.cred);
        f.clock.advance(17 * DAY_MS); // → 2026-02-01
        let revived = f.ledger.roll_window(f.clock.now_ms());
        assert!(revived.is_empty(), "换周期不换 key，失效凭据不该复活");
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);
        assert!(f.ledger.is_invalid(&f.cred));
    }

    #[test]
    fn manual_reset_revives_immediately() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        local(&f, 1000);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        f.ledger.observe(&f.cred, QuotaObservation::ManualReset);
        let st = f.ledger.state(&f.cred);
        assert_eq!(st.kind, QuotaStateKind::Ok);
        assert_eq!(st.used_tokens, 0);
        assert_eq!(f.ledger.remaining_ratio(&f.cred), Some(1.0));
    }

    #[test]
    fn window_none_never_rolls() {
        let mut cfg = tokens_cfg(100.0);
        cfg.window = QuotaWindow::None;
        let f = fixture("2026-01-15T00:00:00Z", cfg);
        local(&f, 100);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);
        assert!(f.ledger.roll_window(ts("2027-01-01T00:00:00Z")).is_empty());
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);
    }

    // ---------------------------------------------------- 窗口边界计算

    #[test]
    fn window_bounds_are_computed_correctly() {
        let t = ts("2026-01-15T10:30:45Z");
        assert_eq!(window_bounds(QuotaWindow::None, 1, t), (0, None));
        assert_eq!(
            window_bounds(QuotaWindow::Hour, 1, t),
            (ts("2026-01-15T10:00:00Z"), Some(ts("2026-01-15T11:00:00Z")))
        );
        assert_eq!(
            window_bounds(QuotaWindow::Day, 1, t),
            (ts("2026-01-15T00:00:00Z"), Some(ts("2026-01-16T00:00:00Z")))
        );
        // reset_day = 15：15 号当天已进入新周期
        assert_eq!(
            window_bounds(QuotaWindow::Month, 15, t),
            (ts("2026-01-15T00:00:00Z"), Some(ts("2026-02-15T00:00:00Z")))
        );
        // reset_day = 20：还没到本月的 20 号 → 周期从去年 12-20 开始
        assert_eq!(
            window_bounds(QuotaWindow::Month, 20, t),
            (ts("2025-12-20T00:00:00Z"), Some(ts("2026-01-20T00:00:00Z")))
        );
    }

    #[test]
    fn reset_day_is_clamped_to_28() {
        let t = ts("2026-01-15T00:00:00Z");
        // 31 号会被夹到 28，避免 2 月没有 31 号导致窗口跳飞
        assert_eq!(
            window_bounds(QuotaWindow::Month, 31, t),
            (ts("2025-12-28T00:00:00Z"), Some(ts("2026-01-28T00:00:00Z")))
        );
    }

    /// 跨时区边界：`reset_day = 1` 指的是**本地**每月 1 号零点。
    ///
    /// 反例（按 UTC 算会出的错）：北京时间 3 月 1 日 01:00（= UTC 2 月 28 日 17:00）
    /// 早就该用新周期了，按 UTC 会继续吃 2 月的旧额度。
    #[test]
    fn month_window_follows_local_timezone() {
        // 北京 +08:00
        let t_utc_feb_end = ts("2026-02-28T17:00:00Z"); // 本地 2026-03-01 01:00
        let (start, end) = window_bounds_local(QuotaWindow::Month, 1, t_utc_feb_end, 480);
        assert_eq!(start, ts("2026-02-28T16:00:00Z"), "本地 3/1 00:00");
        assert_eq!(end, Some(ts("2026-03-31T16:00:00Z")), "本地 4/1 00:00");

        // 同一时刻按 UTC 算还在 2 月 —— 正是要避免的错
        let (utc_start, _) = window_bounds(QuotaWindow::Month, 1, t_utc_feb_end);
        assert_eq!(utc_start, ts("2026-02-01T00:00:00Z"));
        assert_ne!(start, utc_start);

        // 本地 2/28 23:00（= UTC 2/28 15:00）仍属于 2 月周期
        let (start_feb, _) =
            window_bounds_local(QuotaWindow::Month, 1, ts("2026-02-28T15:00:00Z"), 480);
        assert_eq!(start_feb, ts("2026-01-31T16:00:00Z"), "本地 2/1 00:00");

        // 日窗口同理：本地零点对齐
        let (day_start, day_end) =
            window_bounds_local(QuotaWindow::Day, 1, ts("2026-02-28T17:00:00Z"), 480);
        assert_eq!(day_start, ts("2026-02-28T16:00:00Z"));
        assert_eq!(day_end, Some(ts("2026-03-01T16:00:00Z")));
    }

    /// 跨时区的"月度复活"端到端：本地时区进入新周期就必须复活。
    #[test]
    fn month_roll_revives_at_local_month_boundary() {
        let f = fixture("2026-02-10T00:00:00Z", tokens_cfg(100.0));
        local(&f, 100);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 本地 2/28 23:00 → 还在 2 月周期
        f.clock.advance(ts("2026-02-28T15:00:00Z") - ts("2026-02-10T00:00:00Z"));
        assert!(f.ledger.roll_window(f.clock.now_ms()).is_empty());
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Exhausted);

        // 本地 3/1 01:00（UTC 2/28 17:00）→ 已进入新周期，必须复活
        f.clock.advance(2 * HOUR_MS);
        let revived = f.ledger.roll_window(f.clock.now_ms());
        assert_eq!(revived, vec![f.cred.clone()]);
        assert_eq!(f.ledger.state(&f.cred).kind, QuotaStateKind::Ok);
    }

    #[test]
    fn remaining_ratio_shape_is_what_strategy_expects() {
        let f = fixture("2026-01-15T00:00:00Z", tokens_cfg(1000.0));
        // 未知凭据：None（策略里当作"不构成否决理由"）
        assert_eq!(f.ledger.remaining_ratio(&CredentialId::new("nope")), None);
        local(&f, 200);
        assert!((f.ledger.remaining_ratio(&f.cred).unwrap() - 0.8).abs() < 1e-9);
        local(&f, 800);
        assert_eq!(f.ledger.remaining_ratio(&f.cred), Some(0.0));
    }
}

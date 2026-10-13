//! 订阅的**自动更新节奏**（[docs/29 §2.3]）：判断"此刻该刷新哪些订阅"。
//!
//! 这是目标里「设定网络订阅自动更新时间」的**判断部分**：`Subscription.interval_s`
//! 早就存在、也能在界面上改，但此前**没有任何地方按它自动刷新**（只有手动「刷新」）。
//! 把判断抽成纯函数放在这里，是因为它必须能被穷举测到 —— 时间相关的分支最容易写出
//! "看起来对、线上永远不刷新"的代码。
//!
//! 边界（写死，都有测试）：
//!
//! * **关掉的订阅永不自动刷新**：`enabled = false` 是用户明确说"别碰它"；
//! * **`interval_s == 0` 视为不自动刷新**：配置校验（`ta-core::config::validator`）本来就会
//!   把 0 判成非法，这里不 panic、也不解释成"每秒都刷"（那会把机场打成 DDoS）；
//! * **从没成功拉过 → 到期**：否则新加的订阅要等到下一次手动刷新才会自己动；
//! * **时钟回拨（`last > now`）→ 到期**：宁可多拉一次，也不要因为时钟问题永远不更新
//!   （这条是"宁可多做一次也不静默失效"的取舍，和本仓库其余地方的取舍一致）。

use std::collections::HashMap;

/// 判断用的输入：一条订阅的**计划**（不是整份配置，免得把网络层拖进来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionSchedule<'a> {
    pub id: &'a str,
    pub enabled: bool,
    pub interval_s: u32,
}

/// 返回**该刷新**的订阅 id（按输入顺序，便于"先到先刷"与日志可读）。
///
/// `last_fetch_ms`：每条订阅**上一次成功采纳**的时间（epoch 毫秒）。
/// 没有记录 = 从没成功拉过 → 到期。
pub fn due_subscriptions(
    schedules: &[SubscriptionSchedule<'_>],
    last_fetch_ms: &HashMap<String, u64>,
    now_ms: u64,
) -> Vec<String> {
    let mut due = Vec::new();
    for s in schedules {
        if !s.enabled || s.interval_s == 0 {
            continue;
        }
        let interval_ms = u64::from(s.interval_s).saturating_mul(1000);
        match last_fetch_ms.get(s.id) {
            // 从没拉过 → 到期
            None => due.push(s.id.to_string()),
            Some(&last) => {
                let clock_skewed = last > now_ms;
                if clock_skewed || now_ms.saturating_sub(last) >= interval_ms {
                    due.push(s.id.to_string());
                }
            }
        }
    }
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(id: &str, enabled: bool, interval_s: u32) -> SubscriptionSchedule<'_> {
        SubscriptionSchedule { id, enabled, interval_s }
    }

    fn last(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    const HOUR_MS: u64 = 3600 * 1000;

    #[test]
    fn a_disabled_subscription_is_never_due() {
        let schedules = [sched("off", false, 60)];
        // 就算它从没拉过、就算早过了周期，也不许动它
        assert!(due_subscriptions(&schedules, &last(&[]), 10 * HOUR_MS).is_empty());
        assert!(due_subscriptions(&schedules, &last(&[("off", 0)]), 10 * HOUR_MS).is_empty());
    }

    #[test]
    fn a_zero_interval_means_no_auto_refresh_not_every_second() {
        let schedules = [sched("zero", true, 0)];
        assert!(due_subscriptions(&schedules, &last(&[("zero", 0)]), 10 * HOUR_MS).is_empty());
        // 连"从没拉过"也不刷：0 是配置错误，别把它变成高频拉取
        assert!(due_subscriptions(&schedules, &last(&[]), 10 * HOUR_MS).is_empty());
    }

    #[test]
    fn never_fetched_is_due_otherwise_a_new_subscription_waits_for_a_manual_refresh() {
        let schedules = [sched("new", true, 6 * 3600)];
        assert_eq!(due_subscriptions(&schedules, &last(&[]), 1000), vec!["new"]);
    }

    #[test]
    fn due_exactly_at_the_interval_not_before() {
        let schedules = [sched("sub", true, 3600)];
        let last = last(&[("sub", 0)]);
        // 差 1 毫秒：还没到
        assert!(due_subscriptions(&schedules, &last, HOUR_MS - 1).is_empty());
        // 正好到点：到期（边界取 `>=`，免得永远差那么一点）
        assert_eq!(due_subscriptions(&schedules, &last, HOUR_MS), vec!["sub"]);
        // 早过点了
        assert_eq!(due_subscriptions(&schedules, &last, 5 * HOUR_MS), vec!["sub"]);
    }

    #[test]
    fn a_backwards_clock_is_treated_as_due() {
        // 时钟回拨（last 在未来）：宁可多拉一次，也不要因为时钟问题永远不更新
        let schedules = [sched("sub", true, 3600)];
        let last = last(&[("sub", 10 * HOUR_MS)]);
        assert_eq!(due_subscriptions(&schedules, &last, 1000), vec!["sub"]);
    }

    #[test]
    fn order_follows_the_input_and_only_due_ones_come_back() {
        let schedules = [
            sched("a", true, 3600),
            sched("b", true, 3600),
            sched("c", false, 3600),
            sched("d", true, 3600),
        ];
        // b 刚拉过（`last == now`）；d 从没拉过 → 该刷的是 a、d（顺序稳定）
        let last = last(&[("a", 0), ("b", 2 * HOUR_MS), ("c", 0)]);
        assert_eq!(due_subscriptions(&schedules, &last, 2 * HOUR_MS), vec!["a", "d"]);
    }

    #[test]
    fn a_huge_interval_never_overflows() {
        // `interval_s` 是 u32，最大值 × 1000 会溢出 u64？不会（u32::MAX×1000 ≈ 4.3e12 < u64 上限），
        // 但还是要钉住：不许 panic、也不许因为溢出变成"每秒都到点"
        let schedules = [sched("big", true, u32::MAX)];
        let last = last(&[("big", 0)]);
        assert!(due_subscriptions(&schedules, &last, 5 * HOUR_MS).is_empty());
    }
}

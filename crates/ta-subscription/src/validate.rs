//! 坏更新拦截（契约：[11 §1.6] `SubscriptionValidator`、[04 §5.6]）。
//!
//! **这是产品级防线，不是内部细节。** 一次坏订阅更新（机场返回 HTML 错误页、临时空列表、
//! 节点被误删）如果被采纳，用户的所有线路会一起消失 —— 而且他不知道为什么。
//! 所以采纳前必须比对"上一版**已采纳**的内容"，异常就 [`AcceptDecision::Reject`]，
//! 由调用方保留上版（见 [`crate::sync`]）。
//!
//! 拒绝的五种情况（按顺序，命中即拒）：
//! 1. `format == Html` —— 错误页 / 需登录 / 被墙
//! 2. `format` 不可用（Unknown）—— 内容完全认不出来
//! 3. 节点数为 0 —— 空列表（也可能是 filter 把节点全剔了）
//! 4. **节点数骤降** —— 相对上版跌幅超过 `max_node_drop_ratio` **且**新版数量低于
//!    `min_nodes_after_drop`（双重保护：只有"跌幅大"或只有"数量少"都不拒）
//! 5. 分组全部消失 —— 指向订阅分组的 lane 会失效
//!
//! 判据都是**相对上一版已采纳的内容**，所以"机场慢慢增删节点"不会被误拦；
//! 而"一次掉一半以上且只剩个位数"会被拦。

use crate::{AcceptDecision, DetectedFormat, FetchOutcome, SubscriptionValidator};

/// 拦截阈值。
///
/// 「节点数骤降」用**两条互相独立的保护**（任一命中即拒），而不是"必须同时满足" ——
/// 后者会让"100 → 20"这种明显的坏更新溜过去：
/// * **比例**：跌幅超过 `max_node_drop_ratio`（默认掉一半以上）
/// * **下限**：新版低于 `min_nodes_after_drop`（默认 3 个）—— 只剩 1~2 个节点的订阅
///   对"择优"型 lane 等于不可用，这时候保留上版更安全
///
/// 另外用 `min_prev_nodes_for_ratio` 保护小订阅：上版本来就只有两三个节点时，
/// 任何变化都是 33%/50% 的"骤降"，不该按比例判。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValidatorConfig {
    /// 相对上版允许的节点跌幅（`0.5` = 掉一半以上才算异常）。
    /// 设成 `1.0` 表示关掉比例判据（下限与空列表检查仍然生效）。
    pub max_node_drop_ratio: f64,
    /// 绝对下限：新版节点数低于这个数就拒（不管跌了多少）
    pub min_nodes_after_drop: usize,
    /// 上版节点数少于这个数时，**不按比例判**（避免小订阅被误伤）
    pub min_prev_nodes_for_ratio: usize,
    /// 组数相对上版允许的跌幅
    pub max_group_drop_ratio: f64,
    /// 上版有分组、新版一个分组都没有 → 一律拦（指向订阅分组的 lane 会失效）
    pub reject_when_all_groups_gone: bool,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            max_node_drop_ratio: 0.5,
            min_nodes_after_drop: 3,
            min_prev_nodes_for_ratio: 4,
            max_group_drop_ratio: 0.5,
            reject_when_all_groups_gone: true,
        }
    }
}

/// [`SubscriptionValidator`] 的默认实现。
#[derive(Debug, Clone, Default)]
pub struct DefaultSubscriptionValidator {
    cfg: ValidatorConfig,
}

impl DefaultSubscriptionValidator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(cfg: ValidatorConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &ValidatorConfig {
        &self.cfg
    }
}

impl SubscriptionValidator for DefaultSubscriptionValidator {
    fn accept(&self, prev: Option<&FetchOutcome>, next: &FetchOutcome) -> AcceptDecision {
        // 1) HTML 错误页
        if next.format == DetectedFormat::Html {
            return reject(format!(
                "订阅返回的是 HTML（错误页 / 需登录 / 被墙），{} 字节",
                next.raw.len()
            ));
        }
        // 2) 认不出来的格式
        if !next.format.is_usable() {
            return reject(format!(
                "无法识别的订阅格式（{}，{} 字节）",
                next.format.as_str(),
                next.raw.len()
            ));
        }
        // 3) 空列表
        if next.nodes.is_empty() {
            return reject(format!(
                "订阅里没有任何可用节点（格式 {}，{} 字节；也可能是 filter 把节点都剔掉了）",
                next.format.as_str(),
                next.raw.len()
            ));
        }

        let Some(prev) = prev else {
            // 首次拿到有效内容：接受
            return AcceptDecision::Accept;
        };

        // 4) 绝对下限：只剩个位数节点（1~2 个）时"择优"已无意义，保留上版更安全
        let before = prev.nodes.len();
        let after = next.nodes.len();
        if after < self.cfg.min_nodes_after_drop {
            return reject(format!(
                "节点数过少：上版 {before} → 本次 {after}（少于 {} 个）",
                self.cfg.min_nodes_after_drop
            ));
        }
        // 5) 比例骤降：只对"上版节点数够多"的订阅判，避免小订阅被误伤
        if before >= self.cfg.min_prev_nodes_for_ratio && self.cfg.max_node_drop_ratio < 1.0 {
            let keep_floor =
                ((before as f64) * (1.0 - self.cfg.max_node_drop_ratio)).floor() as usize;
            if after < keep_floor {
                return reject(format!(
                    "节点数骤降：上版 {before} → 本次 {after}（跌幅 {:.0}%，超过允许的 {:.0}%）",
                    (1.0 - after as f64 / before as f64) * 100.0,
                    self.cfg.max_node_drop_ratio * 100.0,
                ));
            }
        }

        // 6) 分组
        if self.cfg.reject_when_all_groups_gone && !prev.groups.is_empty() && next.groups.is_empty()
        {
            return reject(format!(
                "分组全部消失（上版 {} 个）—— 指向订阅分组的线路会失效",
                prev.groups.len()
            ));
        }
        let gb = prev.groups.len();
        let ga = next.groups.len();
        if gb >= 2 && self.cfg.max_group_drop_ratio < 1.0 {
            let floor = ((gb as f64) * (1.0 - self.cfg.max_group_drop_ratio)).floor() as usize;
            if ga < floor {
                return reject(format!("分组数骤降：上版 {gb} → 本次 {ga}"));
            }
        }

        AcceptDecision::Accept
    }
}

fn reject(why: impl Into<String>) -> AcceptDecision {
    AcceptDecision::Reject(why.into())
}

/// 便捷入口：不需要保有 validator 实例时直接用默认策略判定
pub fn accept_default(prev: Option<&FetchOutcome>, next: &FetchOutcome) -> AcceptDecision {
    DefaultSubscriptionValidator::new().accept(prev, next)
}

/// 给 UI 的一句话结论
pub fn describe(decision: &AcceptDecision) -> String {
    match decision {
        AcceptDecision::Accept => "订阅已更新".to_string(),
        AcceptDecision::Reject(why) => format!("订阅未采纳，保留上一版：{why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect;
    use crate::parse;
    use crate::NodeMeta;

    fn outcome(yaml: &str) -> FetchOutcome {
        let raw = yaml.as_bytes().to_vec();
        let format = detect::detect_format(&raw);
        let extracted = parse::extract(&raw, format);
        FetchOutcome {
            raw,
            format,
            userinfo: None,
            traffic: None,
            suggested_interval_s: None,
            nodes: extracted.nodes,
            groups: extracted.groups,
        }
    }

    fn nodes(n: usize) -> Vec<NodeMeta> {
        (0..n)
            .map(|i| NodeMeta {
                name: format!("节点{i}"),
                kind: "ss".into(),
                server: format!("h{i}.example.com"),
                port: 443,
            })
            .collect()
    }

    /// 造一个"看起来正常"的 outcome（直接控制节点/组数，避免被解析细节干扰）
    fn shaped(node_count: usize, group_count: usize, format: DetectedFormat) -> FetchOutcome {
        FetchOutcome {
            raw: b"proxies: []\n".to_vec(),
            format,
            userinfo: None,
            traffic: None,
            suggested_interval_s: None,
            nodes: nodes(node_count),
            groups: (0..group_count)
                .map(|i| crate::GroupMeta {
                    name: format!("组{i}"),
                    kind: "select".into(),
                    node_count,
                })
                .collect(),
        }
    }

    #[test]
    fn html_is_rejected_even_when_previous_was_bad_too() {
        let v = DefaultSubscriptionValidator::new();
        let html = outcome("<!DOCTYPE html><html><body>502</body></html>");
        assert!(matches!(v.accept(None, &html), AcceptDecision::Reject(_)));
        let why = v.accept(Some(&shaped(100, 5, DetectedFormat::ClashYaml)), &html);
        assert!(why.reason().unwrap().contains("HTML"), "{why:?}");
    }

    #[test]
    fn unknown_format_and_empty_list_are_rejected() {
        let v = DefaultSubscriptionValidator::new();
        let unknown = outcome("just some words");
        assert_eq!(unknown.format, DetectedFormat::Unknown);
        assert!(v.accept(None, &unknown).reason().unwrap().contains("无法识别"));
        // 空列表（真实解析出来是 0 个节点）
        let empty = outcome("proxies: []\nproxy-groups: []\n");
        assert_eq!(empty.format, DetectedFormat::ClashYaml);
        assert_eq!(empty.node_count(), 0);
        let why = v.accept(Some(&shaped(50, 3, DetectedFormat::ClashYaml)), &empty);
        assert!(why.reason().unwrap().contains("没有任何可用节点"), "{why:?}");
    }

    #[test]
    fn node_count_collapse_is_rejected() {
        let v = DefaultSubscriptionValidator::new();
        let why = v.accept(
            Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
            &shaped(20, 5, DetectedFormat::ClashYaml),
        );
        let reason = why.reason().expect("100 → 20 必须拒绝");
        assert!(reason.contains("骤降"), "{reason}");
        assert!(reason.contains("100") && reason.contains("20"), "{reason}");
    }

    #[test]
    fn normal_small_shrink_is_accepted() {
        let v = DefaultSubscriptionValidator::new();
        // 100 → 80（跌 20%）：正常轮换，不能拦
        assert!(v
            .accept(
                Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
                &shaped(80, 5, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 100 → 60（刚好跌 40%）：仍在允许范围
        assert!(v
            .accept(
                Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
                &shaped(60, 4, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 小幅增长当然接受
        assert!(v
            .accept(
                Some(&shaped(10, 2, DetectedFormat::ClashYaml)),
                &shaped(12, 2, DetectedFormat::ClashYaml)
            )
            .is_accept());
    }

    #[test]
    fn small_subscriptions_are_not_falsely_rejected() {
        // 4 → 3（跌 25%）：正常波动，放行
        let v = DefaultSubscriptionValidator::new();
        assert!(v
            .accept(
                Some(&shaped(4, 1, DetectedFormat::ClashYaml)),
                &shaped(3, 1, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 3 → 3：不动，当然放行
        assert!(v
            .accept(
                Some(&shaped(3, 1, DetectedFormat::ClashYaml)),
                &shaped(3, 1, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 3 → 2：**拒绝** —— 命中绝对下限（< 3）。这是刻意的：只剩 2 个节点时
        // "择优"已无意义，保留上版更安全；嫌严可以调低 min_nodes_after_drop。
        let why = v.accept(
            Some(&shaped(3, 1, DetectedFormat::ClashYaml)),
            &shaped(2, 1, DetectedFormat::ClashYaml),
        );
        assert!(why.reason().unwrap().contains("节点数过少"), "{why:?}");
        // 调低下限后放行（可配置）
        let relaxed = DefaultSubscriptionValidator::with_config(ValidatorConfig {
            min_nodes_after_drop: 1,
            ..Default::default()
        });
        assert!(relaxed
            .accept(
                Some(&shaped(3, 1, DetectedFormat::ClashYaml)),
                &shaped(2, 1, DetectedFormat::ClashYaml)
            )
            .is_accept());
    }

    #[test]
    fn both_protections_are_independent() {
        let v = DefaultSubscriptionValidator::new();
        // 跌幅大（1000 → 400）但绝对数量不少 → 靠比例拦
        let why = v.accept(
            Some(&shaped(1000, 5, DetectedFormat::ClashYaml)),
            &shaped(400, 5, DetectedFormat::ClashYaml),
        );
        assert!(why.reason().unwrap().contains("骤降"), "{why:?}");
        // 跌幅不大（8 → 6，跌 25%）且数量不少 → 放行
        assert!(v
            .accept(
                Some(&shaped(8, 2, DetectedFormat::ClashYaml)),
                &shaped(6, 2, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 跌幅不大但掉到下限以下（8 → 2）→ 靠下限拦
        let why = v.accept(
            Some(&shaped(8, 2, DetectedFormat::ClashYaml)),
            &shaped(2, 2, DetectedFormat::ClashYaml),
        );
        assert!(why.reason().unwrap().contains("节点数过少"), "{why:?}");
    }

    #[test]
    fn threshold_is_configurable() {
        // 放宽到 0.9：100 → 20 也接受（只剩 10% 以下才拦）
        let loose = DefaultSubscriptionValidator::with_config(ValidatorConfig {
            max_node_drop_ratio: 0.9,
            ..Default::default()
        });
        assert!(loose
            .accept(
                Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
                &shaped(20, 5, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 收紧到 0.1：100 → 85 就拦
        let strict = DefaultSubscriptionValidator::with_config(ValidatorConfig {
            max_node_drop_ratio: 0.1,
            ..Default::default()
        });
        let why = strict.accept(
            Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
            &shaped(85, 5, DetectedFormat::ClashYaml),
        );
        assert!(why.reason().unwrap().contains("骤降"), "{why:?}");
        // 比例判据关掉（1.0）+ 下限关到 1：100 → 5 也接受
        let off = DefaultSubscriptionValidator::with_config(ValidatorConfig {
            max_node_drop_ratio: 1.0,
            min_nodes_after_drop: 1,
            ..Default::default()
        });
        assert!(off
            .accept(
                Some(&shaped(100, 5, DetectedFormat::ClashYaml)),
                &shaped(5, 5, DetectedFormat::ClashYaml)
            )
            .is_accept());
    }

    #[test]
    fn groups_disappearing_is_rejected() {
        let v = DefaultSubscriptionValidator::new();
        let why = v.accept(
            Some(&shaped(50, 4, DetectedFormat::ClashYaml)),
            &shaped(50, 0, DetectedFormat::ClashYaml),
        );
        assert!(why.reason().unwrap().contains("分组全部消失"), "{why:?}");
        // 本来就平铺（上版也没有分组）→ 不该因为"没有分组"被拦
        assert!(v
            .accept(
                Some(&shaped(50, 0, DetectedFormat::ClashYaml)),
                &shaped(50, 0, DetectedFormat::ClashYaml)
            )
            .is_accept());
        // 组数小幅减少 → 放行
        assert!(v
            .accept(
                Some(&shaped(50, 4, DetectedFormat::ClashYaml)),
                &shaped(50, 3, DetectedFormat::ClashYaml)
            )
            .is_accept());
    }

    #[test]
    fn first_fetch_with_valid_content_is_accepted() {
        let v = DefaultSubscriptionValidator::new();
        assert!(v.accept(None, &shaped(1, 0, DetectedFormat::ClashYaml)).is_accept());
        assert!(v
            .accept(None, &outcome("proxies:\n  - {name: a, type: ss, server: h.com, port: 443}\n"))
            .is_accept());
    }

    #[test]
    fn describe_is_ui_ready() {
        assert_eq!(describe(&AcceptDecision::Accept), "订阅已更新");
        let d = accept_default(None, &outcome("<!DOCTYPE html><html></html>"));
        assert!(describe(&d).starts_with("订阅未采纳，保留上一版："));
    }
}

//! `subscription-userinfo` 响应头 → 流量 / 到期（**未知一律 null，绝不用 0 冒充**）。
//!
//! 契约：(../../../)、
//!(../../../) 的 `SubscriptionTraffic`。
//!
//! 这个头是 Clash 生态的约定：单位**字节**，`expire` 是 **Unix 秒**。
//!
//! ```text
//! subscription-userinfo: upload=123456789; download=98765432100; total=139586437120; expire=1893456000
//! ```
//!
//! ★ **为什么不复用 [`crate::SubUserinfo`]**：那个类型的字段是 `u64`，解析时"缺失 / 非法"
//! 一律塞 0。而 0 在这套语义里是一个**合法的观测值**（机场真的可能报 `upload=0`）——
//! 混在一起之后，UI 永远分不清"没用过"和"不知道"，那正是  明令禁止的。
//! `SubUserinfo` 继续服务版本留档，本类型只服务"给用户看"这条链。
//!
//! 红先绿后的证据留在测试里：先按旧口径（`parse_userinfo`）接线，下面五处断言一起红，
//! 报的都是同一件事 ——`left: Some(0)` / `right: None`（**拿 0 冒充未知**）；
//!
//! ## 判定的全部规则（逐条都有测试）
//!
//! | 输入 | 结果 | 为什么 |
//! |---|---|---|
//! | 响应头**不存在** | `FetchOutcome.traffic == None` | "这次没给" ≠ "给了但字段未知" |
//! | 字段缺失 / 非数字 / 负数 | 该字段 `None` | 未知不许冒充 0 |
//! | `total=0` | `total_bytes = None` | 0 表示"不限量 / 未声明"，不是"总量为零" |
//! | `expire=0` | `expire_at_s = None` | 0 表示"不适用"，不是 1970 年到期 |
//! | `upload=0` / `download=0` | `Some(0)` | **真实观测值**：确实一个字节都没用 |
//! | `expire` 是毫秒量级（> 1e10） | 换算成秒 | 个别服务端给毫秒（既有 `parse_userinfo` 也认这一形态） |

use serde::{Deserialize, Serialize};

/// 订阅服务商在响应头里给的流量信息。**未知一律 `None`。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionTraffic {
    pub upload_bytes: Option<u64>,
    pub download_bytes: Option<u64>,
    /// 总量；缺失 / 非数字 / `0` → `None`（0 = 不限量 / 未声明）。
    pub total_bytes: Option<u64>,
    /// 到期时刻（**Unix 秒**）；缺失 / 非数字 / `0` → `None`。
    pub expire_at_s: Option<i64>,
    /// 这一次拿到的时刻（Unix 毫秒）。**必填** —— UI 的"更新时间"就显示它。
    pub fetched_at_ms: i64,
}

impl SubscriptionTraffic {
    /// 已用流量：`upload + download`，**两者都有才算**（任一未知 → `None`，不拿另一项冒充）。
    pub fn used_bytes(&self) -> Option<u64> {
        match (self.upload_bytes, self.download_bytes) {
            (Some(up), Some(down)) => Some(up.saturating_add(down)),
            _ => None,
        }
    }

    /// 一个可用的数都没有（`fetched_at_ms` 不算数）。UI 据此决定要不要显示数字。
    pub fn is_all_unknown(&self) -> bool {
        self.upload_bytes.is_none()
            && self.download_bytes.is_none()
            && self.total_bytes.is_none()
            && self.expire_at_s.is_none()
    }
}

/// `expire` 超过这个量级就按**毫秒**理解（一亿秒 ≈ 1973 年，任何真实到期时间都在其之后）。
const EXPIRE_MS_THRESHOLD: i64 = 10_000_000_000;

/// 解析 `subscription-userinfo`。
///
/// 调用方负责"头在不在"：头不存在时不要调它，直接给 `FetchOutcome.traffic = None`。
pub fn parse_traffic(header: &str, fetched_at_ms: i64) -> SubscriptionTraffic {
    let mut upload_bytes = None;
    let mut download_bytes = None;
    let mut total_bytes = None;
    let mut expire_at_s = None;

    for part in header.split([';', ',']) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let raw = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "upload" => upload_bytes = bytes_of(raw),
            "download" => download_bytes = bytes_of(raw),
            // `total=0` = 不限量 / 未声明（不是"总量为零"）→ 未知
            "total" => total_bytes = bytes_of(raw).filter(|n| *n > 0),
            "expire" => expire_at_s = expire_seconds_of(raw),
            // 服务商可能加别的键（`reset_day` 之类）：忽略，不影响已认出的字段。
            _ => {}
        }
    }

    SubscriptionTraffic { upload_bytes, download_bytes, total_bytes, expire_at_s, fetched_at_ms }
}

/// 字节数：能解析成非负整数就是**已知值**。
///
/// `0` 也算已知 —— 机场真的可能报"上传 0 字节"，那是观测值，不是缺失。
/// 缺失 / 非数字 / 负数 / 空串一律 `None`。
fn bytes_of(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok()
}

/// `expire` → Unix **秒**。`0`（不适用）与非正值 → `None`；毫秒量级归一到秒。
fn expire_seconds_of(raw: &str) -> Option<i64> {
    let n = raw.trim().parse::<i64>().ok()?;
    if n <= 0 {
        return None;
    }
    Some(if n > EXPIRE_MS_THRESHOLD { n / 1000 } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_700_000_000_000;

    // ------------------------------------------------------------ 完整头

    #[test]
    fn full_header_parses_all_four_fields() {
        let t = parse_traffic(
            "upload=123456789; download=98765432100; total=139586437120; expire=1893456000",
            NOW_MS,
        );
        assert_eq!(t.upload_bytes, Some(123_456_789));
        assert_eq!(t.download_bytes, Some(98_765_432_100));
        assert_eq!(t.total_bytes, Some(139_586_437_120));
        assert_eq!(t.expire_at_s, Some(1_893_456_000));
        assert_eq!(t.fetched_at_ms, NOW_MS, "fetched_at_ms 必填，原样带上");
        assert_eq!(t.used_bytes(), Some(123_456_789 + 98_765_432_100));
        assert!(!t.is_all_unknown());
    }

    /// 字段顺序 / 空格 / 逗号分隔 / 大小写都不该影响结果（真实机场三种写法都有）。
    #[test]
    fn field_order_spacing_and_case_do_not_matter() {
        let t = parse_traffic(" Download = 10 ,Total=100; Upload=5;Expire=1893456000 ", NOW_MS);
        assert_eq!(t.upload_bytes, Some(5));
        assert_eq!(t.download_bytes, Some(10));
        assert_eq!(t.total_bytes, Some(100));
        assert_eq!(t.expire_at_s, Some(1_893_456_000));
        // 不认识的键忽略，不影响别的字段
        let t = parse_traffic("upload=1; foo=bar; total=2", NOW_MS);
        assert_eq!((t.upload_bytes, t.total_bytes), (Some(1), Some(2)));
    }

    // ------------------------------------------------------------ 未知一律 null

    #[test]
    fn missing_fields_are_null_not_zero() {
        let t = parse_traffic("upload=10; total=100", NOW_MS);
        assert_eq!(t.upload_bytes, Some(10));
        assert_eq!(t.download_bytes, None, "字段缺失必须是 None，不是 0");
        assert_eq!(t.total_bytes, Some(100));
        assert_eq!(t.expire_at_s, None, "字段缺失必须是 None，不是 0");
        assert_eq!(t.used_bytes(), None, "upload/download 任一缺失就不算已用");
    }

    #[test]
    fn non_numeric_values_are_null_not_zero() {
        let t = parse_traffic("upload=abc; download=1x2; total=; expire=soon", NOW_MS);
        assert_eq!(t.upload_bytes, None, "非数字不许冒充 0");
        assert_eq!(t.download_bytes, None);
        assert_eq!(t.total_bytes, None);
        assert_eq!(t.expire_at_s, None);
        // 负数（字节数没有负数）
        let t = parse_traffic("upload=-1; download=-99; total=-5; expire=-1", NOW_MS);
        assert_eq!(
            (t.upload_bytes, t.download_bytes, t.total_bytes, t.expire_at_s),
            (None, None, None, None)
        );
    }

    #[test]
    fn zero_total_and_zero_expire_are_not_applicable() {
        let t = parse_traffic("upload=10; download=20; total=0; expire=0", NOW_MS);
        assert_eq!(t.total_bytes, None, "total=0 = 不限量/未声明，不是总量为零");
        assert_eq!(t.expire_at_s, None, "expire=0 = 不适用，不是 1970 年到期");
        // 但 upload / download 的 0 是**真实观测值**，必须留下
        let t = parse_traffic("upload=0; download=0; total=100; expire=1893456000", NOW_MS);
        assert_eq!(t.upload_bytes, Some(0));
        assert_eq!(t.download_bytes, Some(0));
        assert_eq!(t.used_bytes(), Some(0), "真的一个字节都没用 —— 这是已知值");
        assert_eq!(t.total_bytes, Some(100));
    }

    #[test]
    fn empty_or_garbage_header_is_all_unknown() {
        let t = parse_traffic("", NOW_MS);
        assert!(t.is_all_unknown(), "{t:?}");
        assert_eq!(t.fetched_at_ms, NOW_MS, "头在（只是没用）→ 时间仍要留着");
        let t = parse_traffic("nothing here", NOW_MS);
        assert!(t.is_all_unknown(), "{t:?}");
    }

    // ------------------------------------------------------------ 单位归一

    /// 个别服务端把 `expire` 给成毫秒；契约要 Unix 秒，所以要归一到秒。
    #[test]
    fn expire_in_milliseconds_is_normalized_to_seconds() {
        let t = parse_traffic("expire=1893456000000", NOW_MS);
        assert_eq!(t.expire_at_s, Some(1_893_456_000), "毫秒 → 秒");
    }
}

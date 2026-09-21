//! 时间分桶与时间范围解析（**一律按本地时区**，02/07 都强调"时间轴永远本地时区"）。
//!
//! 时区偏移由 `ta_core::clock::Clock::local_offset_minutes()` 注入，所以生产用系统时区、
//! 测试用 `FixedClock`，逻辑完全一致、可确定性断言。
//!
//! bucket key 的定义（全部是整数，便于 SQL GROUP BY 与 Rust 侧一致地对齐）：
//!
//! | bucket | key | 说明 |
//! |---|---|---|
//! | hour  | `floor((ts+off)/1h)` | |
//! | day   | `floor((ts+off)/1d)` | 本地日序号 |
//! | week  | `floor((day+3)/7)` | **周一为一周起点**（1970-01-01 是周四） |
//! | month | `YYYY*100+MM` | 本地日历年月 |

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use ta_core::clock::TimeRange;

pub const MS_HOUR: i64 = 3_600_000;
pub const MS_DAY: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBucket {
    Hour,
    Day,
    Week,
    Month,
}

impl TimeBucket {
    pub fn as_str(&self) -> &'static str {
        match self {
            TimeBucket::Hour => "hour",
            TimeBucket::Day => "day",
            TimeBucket::Week => "week",
            TimeBucket::Month => "month",
        }
    }
}

pub fn offset_ms(offset_minutes: i32) -> i64 {
    offset_minutes as i64 * 60_000
}

/// SQL 表达式：把 `col`（unix 毫秒列）换算成本地时区的 bucket key。
///
/// `off_ms` 来自 `Clock`，是**我们自己的整数**，不存在注入面。
pub fn bucket_key_sql(col: &str, bucket: TimeBucket, off_ms: i64) -> String {
    let shifted = format!("({col} + {off_ms})");
    match bucket {
        TimeBucket::Hour => format!("(CAST({shifted} / {MS_HOUR} AS INTEGER))"),
        TimeBucket::Day => format!("(CAST({shifted} / {MS_DAY} AS INTEGER))"),
        TimeBucket::Week => {
            format!("((CAST({shifted} / {MS_DAY} AS INTEGER) + 3) / 7)")
        }
        TimeBucket::Month => {
            format!("(CAST(strftime('%Y%m', {shifted} / 1000, 'unixepoch') AS INTEGER))")
        }
    }
}

/// Rust 侧的 bucket key，必须与 `bucket_key_sql` 完全一致。
pub fn bucket_key_from_ms(ts_ms: i64, bucket: TimeBucket, off_ms: i64) -> i64 {
    let s = ts_ms + off_ms;
    match bucket {
        TimeBucket::Hour => s.div_euclid(MS_HOUR),
        TimeBucket::Day => s.div_euclid(MS_DAY),
        TimeBucket::Week => (s.div_euclid(MS_DAY) + 3).div_euclid(7),
        TimeBucket::Month => {
            let dt = to_utc(s);
            dt.year() as i64 * 100 + dt.month() as i64
        }
    }
}

/// bucket key → 该 bucket 的起始时间（真实 unix 毫秒，本地对齐）。
pub fn bucket_start_ms(key: i64, bucket: TimeBucket, off_ms: i64) -> i64 {
    match bucket {
        TimeBucket::Hour => key * MS_HOUR - off_ms,
        TimeBucket::Day => key * MS_DAY - off_ms,
        TimeBucket::Week => (key * 7 - 3) * MS_DAY - off_ms,
        TimeBucket::Month => {
            let (y, m) = (key / 100, key % 100);
            date_utc_ms(y as i32, m.max(1) as u32, 1) - off_ms
        }
    }
}

/// bucket key → UI 上的本地时间标签（列头会带 GMT+8 提示，见 07 §1.3）。
pub fn bucket_label(key: i64, bucket: TimeBucket, off_ms: i64) -> String {
    match bucket {
        TimeBucket::Month => {
            let (y, m) = (key / 100, key % 100);
            format!("{y:04}-{m:02}")
        }
        TimeBucket::Hour => to_utc(bucket_start_ms(key, bucket, off_ms) + off_ms)
            .format("%Y-%m-%d %H:00")
            .to_string(),
        _ => to_utc(bucket_start_ms(key, bucket, off_ms) + off_ms).format("%Y-%m-%d").to_string(),
    }
}

/// 本地日期字符串 `YYYY-MM-DD`（`usage_daily.day` 用这个口径）。
pub fn local_day_str(ts_ms: i64, off_ms: i64) -> String {
    to_utc(ts_ms + off_ms).format("%Y-%m-%d").to_string()
}

/// `usage_daily.day`（本地日期字符串）→ bucket key。
///
/// 锚在当天 12:00 再换算，避免 ±时区把日期挤到前后一天。
pub fn day_to_bucket_key(day: &str, bucket: TimeBucket, off_ms: i64) -> i64 {
    match NaiveDate::parse_from_str(day, "%Y-%m-%d") {
        Ok(d) => {
            let anchor =
                d.and_hms_opt(12, 0, 0).map(|dt| dt.and_utc().timestamp_millis()).unwrap_or(0);
            bucket_key_from_ms(anchor, bucket, off_ms)
        }
        Err(_) => 0,
    }
}

/// 解析 `TimeRange` → 左闭右开区间（本地时区语义）。
pub fn range_bounds(range: &TimeRange, now_ms: i64, off_ms: i64) -> (i64, i64) {
    let today_start = (now_ms + off_ms).div_euclid(MS_DAY) * MS_DAY - off_ms;
    match range {
        TimeRange::Explicit { from_ms, to_ms } => (*from_ms, *to_ms),
        TimeRange::Today => (today_start, today_start + MS_DAY),
        TimeRange::Last7d => (today_start - 6 * MS_DAY, today_start + MS_DAY),
        TimeRange::Last30d => (today_start - 29 * MS_DAY, today_start + MS_DAY),
        TimeRange::ThisMonth => {
            let now = to_utc(now_ms + off_ms);
            let (y, m) = (now.year(), now.month());
            let start = date_utc_ms(y, m, 1) - off_ms;
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            let end = date_utc_ms(ny, nm, 1) - off_ms;
            (start, end)
        }
        TimeRange::All => (0, i64::MAX / 2),
    }
}

/// 供 `usage_daily` 用的本地日期闭开区间 `[from_day, to_day)`。
pub fn day_bounds(range: &TimeRange, now_ms: i64, off_ms: i64) -> (String, String) {
    if matches!(range, TimeRange::All) {
        return ("0000-01-01".to_string(), "9999-12-31".to_string());
    }
    let (from, to) = range_bounds(range, now_ms, off_ms);
    (local_day_str(from, off_ms), local_day_str(to, off_ms))
}

/// 区间是否按本地日对齐（对齐才能用 `usage_daily` 表，否则会多算）。
pub fn is_day_aligned(from_ms: i64, to_ms: i64, off_ms: i64) -> bool {
    (from_ms + off_ms).rem_euclid(MS_DAY) == 0 && (to_ms + off_ms).rem_euclid(MS_DAY) == 0
}

/// 默认分桶：短区间按小时，长区间按天。
pub fn default_bucket(from_ms: i64, to_ms: i64) -> TimeBucket {
    let span = to_ms.saturating_sub(from_ms);
    if span <= 2 * MS_DAY {
        TimeBucket::Hour
    } else {
        TimeBucket::Day
    }
}

fn to_utc(shifted_ms: i64) -> DateTime<Utc> {
    // 越界时间戳（比如 `TimeRange::All` 的边界）不该 panic：回退到 epoch 即可，
    // 上层只会得到一个"明显不对"的标签，而不是整条查询挂掉。
    DateTime::from_timestamp_millis(shifted_ms).unwrap_or_default()
}

fn date_utc_ms(y: i32, m: u32, d: u32) -> i64 {
    NaiveDate::from_ymd_opt(y, m, d)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
    }

    #[test]
    fn day_bucket_respects_local_offset() {
        let off = offset_ms(480); // UTC+8
                                  // UTC 2026-02-10 20:00 → 本地 2026-02-11 04:00
        let late = ms("2026-02-10T20:00:00Z");
        // UTC 2026-02-10 15:59 → 本地 2026-02-10 23:59
        let before = ms("2026-02-10T15:59:00Z");
        assert_eq!(local_day_str(late, off), "2026-02-11");
        assert_eq!(local_day_str(before, off), "2026-02-10");
        assert_ne!(
            bucket_key_from_ms(late, TimeBucket::Day, off),
            bucket_key_from_ms(before, TimeBucket::Day, off)
        );
        // 与 SQL 表达式的一致性由 usage_store 的集成测试覆盖
        assert_eq!(
            bucket_label(bucket_key_from_ms(late, TimeBucket::Day, off), TimeBucket::Day, off),
            "2026-02-11"
        );
    }

    #[test]
    fn week_starts_monday_local() {
        let off = offset_ms(480);
        // 2026-02-09 是周一（本地），2026-02-08 是周日
        let sun = ms("2026-02-08T04:00:00Z"); // 本地 2026-02-08 12:00
        let mon = ms("2026-02-09T04:00:00Z"); // 本地 2026-02-09 12:00
        let k_sun = bucket_key_from_ms(sun, TimeBucket::Week, off);
        let k_mon = bucket_key_from_ms(mon, TimeBucket::Week, off);
        assert_eq!(k_mon, k_sun + 1);
        assert_eq!(bucket_label(k_mon, TimeBucket::Week, off), "2026-02-09");
    }

    #[test]
    fn month_bucket_and_day_string_roundtrip() {
        let off = offset_ms(480);
        let t = ms("2026-02-11T02:00:00Z");
        let key = bucket_key_from_ms(t, TimeBucket::Month, off);
        assert_eq!(key, 202602);
        assert_eq!(bucket_label(key, TimeBucket::Month, off), "2026-02");
        assert_eq!(
            day_to_bucket_key("2026-02-11", TimeBucket::Day, off),
            bucket_key_from_ms(t, TimeBucket::Day, off)
        );
        assert_eq!(day_to_bucket_key("2026-02-11", TimeBucket::Month, off), 202602);
        assert_eq!(
            day_to_bucket_key("2026-02-11", TimeBucket::Week, off),
            bucket_key_from_ms(t, TimeBucket::Week, off)
        );
    }

    #[test]
    fn ranges_are_local_midnight_aligned() {
        let off = offset_ms(480);
        let now = ms("2026-02-11T02:00:00Z"); // 本地 2026-02-11 10:00
        let (from, to) = range_bounds(&TimeRange::Today, now, off);
        assert_eq!(local_day_str(from, off), "2026-02-11");
        assert_eq!(local_day_str(to, off), "2026-02-12");
        assert!(is_day_aligned(from, to, off));

        let (from, to) = range_bounds(&TimeRange::Last7d, now, off);
        assert_eq!(local_day_str(from, off), "2026-02-05");
        assert!(is_day_aligned(from, to, off));

        let (from, to) = range_bounds(&TimeRange::ThisMonth, now, off);
        assert_eq!(local_day_str(from, off), "2026-02-01");
        assert_eq!(local_day_str(to, off), "2026-03-01");

        let (from, to) =
            range_bounds(&TimeRange::Explicit { from_ms: now, to_ms: now + MS_HOUR }, now, off);
        assert!(!is_day_aligned(from, to, off));
    }
}

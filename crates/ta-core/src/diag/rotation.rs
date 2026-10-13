//! 日志轮转与保留策略—— **纯决策，不做 IO**。
//!
//! [14 §4.3] 明确要求：策略是**纯逻辑**（给定「当前文件大小、已有份数、时刻」→「该做什么」），
//! 所以它在这里、有单测，而不是埋在文件 IO 中间件里。删文件由 `src-tauri/src/logging.rs` 执行。
//!
//! 三条不许含糊的规则：
//! 1. **只删我们自己生成的归档文件**（`tokenagency.<时间戳>.log`）。目录里可能有用户自己放的
//!    东西 —— 删了就是事故（[14 §13] 验收第 9 条专门看这个）。
//! 2. **`crash-*.log` 永不自动删除**（[14 §8.2]）。
//! 3. 跨天判定用**本地时区**（[14 §4.2]），否则东八区凌晨写的日志会归到前一天。

/// 单文件上限：8 MiB（[14 §4.2]）
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 保留份数：7（不含正在写的那个）
pub const DEFAULT_KEEP_FILES: usize = 7;
/// 保留总量上限：64 MiB
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// 当前正在写的日志文件名。
pub const CURRENT_FILE_NAME: &str = "tokenagency.log";

/// 轮转与保留策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// 单文件上限（字节）。0 = 不按大小轮转
    pub max_bytes: u64,
    /// 保留几份归档（含本次轮转出来的那份）
    pub keep_files: usize,
    /// 归档总量上限（字节）。**0 = 不限制**（刻意用 0 表示"不限"，避免误删）
    pub max_total_bytes: u64,
    /// 跨天是否轮转
    pub rotate_daily: bool,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            keep_files: DEFAULT_KEEP_FILES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            rotate_daily: true,
        }
    }
}

/// 日志目录里一个文件属于谁。**分类决定能不能删。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// `tokenagency.log`（正在写）
    Current,
    /// `tokenagency.<yyyymmdd-HHmmss>.log`（我们轮转出来的，可删）
    Rotated,
    /// `crash-<yyyymmdd-HHmmss>.log`（我们的，但**永不自动删除**）
    Crash,
    /// 其它一切：用户自己放进日志目录的东西 —— **绝不删除**
    Foreign,
}

/// 按文件名分类。
pub fn classify_file_name(name: &str) -> FileKind {
    if name == CURRENT_FILE_NAME {
        return FileKind::Current;
    }
    if let Some(mid) = name.strip_prefix("tokenagency.").and_then(|s| s.strip_suffix(".log")) {
        if is_timestamp(mid) {
            return FileKind::Rotated;
        }
    }
    if name.starts_with("crash-") && name.ends_with(".log") {
        return FileKind::Crash;
    }
    FileKind::Foreign
}

/// `yyyymmdd-HHmmss`
fn is_timestamp(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 15
        && bytes[8] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 8 || b.is_ascii_digit())
}

/// 归档文件名（**本地时间**，与跨天判定同一套时区口径）。
pub fn archive_name_local(now_ms: i64, local_offset_minutes: i32) -> String {
    format!("tokenagency.{}.log", stamp_local(now_ms, local_offset_minutes))
}

/// 崩溃文件名（[14 §8.2]）。
pub fn crash_name_local(now_ms: i64, local_offset_minutes: i32) -> String {
    format!("crash-{}.log", stamp_local(now_ms, local_offset_minutes))
}

fn stamp_local(now_ms: i64, local_offset_minutes: i32) -> String {
    let shifted = now_ms.saturating_add(local_offset_minutes as i64 * 60_000);
    match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(shifted) {
        Some(dt) => dt.format("%Y%m%d-%H%M%S").to_string(),
        // 时间戳超出 chrono 可表示范围（不可能来自系统时钟）→ 给个可读的兜底，绝不 panic
        None => format!("ts{shifted}"),
    }
}

/// 目录里已有文件的最小元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFileMeta {
    pub name: String,
    pub bytes: u64,
    /// 修改时间（Unix 毫秒）——"最老先删"的依据
    pub modified_ms: i64,
}

/// 为什么轮转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotateReason {
    Size,
    Day,
    /// 跨天与超限同时发生
    SizeAndDay,
    /// 用户点了 [立即轮转]
    Manual,
}

/// 一次决策的输入（全部来自调用方采样，函数本身不碰磁盘）。
#[derive(Debug, Clone)]
pub struct RotationInput {
    pub now_ms: i64,
    /// 本机时区偏移（分钟）
    pub local_offset_minutes: i32,
    /// 当前正在写的文件大小
    pub current_bytes: u64,
    /// 当前文件开始写的时间（Unix 毫秒）
    pub current_started_ms: i64,
    /// 日志目录里**除当前文件外**已有的一切（含 Foreign / Crash）
    pub existing: Vec<LogFileMeta>,
    /// 用户点了 [立即轮转]
    pub manual: bool,
}

/// 要删的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneTarget {
    /// 删 `existing` 里的这个归档文件
    Named(String),
    /// 删"本次刚轮转出来的那一份"（`keep_files = 0` 或总量极小时会发生）
    NewArchive,
}

/// 决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationDecision {
    /// 什么都不做
    Keep,
    /// 轮转：把当前文件改名归档（调用方生成名字），然后按 `prune` 顺序删除
    Rotate { reason: RotateReason, prune: Vec<PruneTarget> },
}

/// 纯决策：**该不该轮转、轮转后该删哪些**。
pub fn decide(policy: &RotationPolicy, input: &RotationInput) -> RotationDecision {
    let size_hit = policy.max_bytes > 0 && input.current_bytes >= policy.max_bytes;
    let day_hit = policy.rotate_daily
        && local_day(input.now_ms, input.local_offset_minutes)
            != local_day(input.current_started_ms, input.local_offset_minutes);

    let reason = if input.manual {
        RotateReason::Manual
    } else {
        match (size_hit, day_hit) {
            (true, true) => RotateReason::SizeAndDay,
            (true, false) => RotateReason::Size,
            (false, true) => RotateReason::Day,
            (false, false) => return RotationDecision::Keep,
        }
    };

    // 归档候选集 = 目录里已有的归档（不含 Foreign/Crash）+ 本次刚轮转出来的那份
    let mut set: Vec<(PruneTarget, u64, i64, String)> = input
        .existing
        .iter()
        .filter(|f| classify_file_name(&f.name) == FileKind::Rotated)
        .map(|f| (PruneTarget::Named(f.name.clone()), f.bytes, f.modified_ms, f.name.clone()))
        .collect();
    set.push((PruneTarget::NewArchive, input.current_bytes, input.now_ms, String::new()));
    // 最老先删：modified_ms 升序，相同时按名字升序（保证结果可复现、可测）
    set.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.3.cmp(&b.3)));

    let mut total: u64 = set.iter().map(|(_, bytes, _, _)| *bytes).sum();
    let mut count = set.len();
    let mut prune: Vec<PruneTarget> = Vec::new();
    let mut idx = 0usize;
    while (count > policy.keep_files)
        || (policy.max_total_bytes > 0 && total > policy.max_total_bytes)
    {
        if idx >= set.len() {
            break;
        }
        let (target, bytes, _, _) = &set[idx];
        prune.push(target.clone());
        total = total.saturating_sub(*bytes);
        count -= 1;
        idx += 1;
    }

    RotationDecision::Rotate { reason, prune }
}

/// 本地日期序号（自 epoch 起的天数）。
fn local_day(ms: i64, local_offset_minutes: i32) -> i64 {
    ms.saturating_add(local_offset_minutes as i64 * 60_000).div_euclid(86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn ts(rfc3339: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339).map(|d| d.timestamp_millis()).unwrap()
    }

    fn rotated(name: &str, bytes: u64, modified_ms: i64) -> LogFileMeta {
        LogFileMeta { name: name.to_string(), bytes, modified_ms }
    }

    fn input(now_ms: i64, current_bytes: u64, started_ms: i64) -> RotationInput {
        RotationInput {
            now_ms,
            local_offset_minutes: 480,
            current_bytes,
            current_started_ms: started_ms,
            existing: vec![],
            manual: false,
        }
    }

    /// 清理相关的用例：先把"轮转确实发生了"这件事摆好，再验删哪些。
    fn rotating_input(now_ms: i64, current_bytes: u64) -> RotationInput {
        let mut i = input(now_ms, current_bytes, now_ms);
        i.manual = true;
        i
    }

    fn prune_names(d: &RotationDecision) -> Vec<String> {
        match d {
            RotationDecision::Keep => vec![],
            RotationDecision::Rotate { prune, .. } => prune
                .iter()
                .map(|p| match p {
                    PruneTarget::Named(n) => n.clone(),
                    PruneTarget::NewArchive => "<new>".to_string(),
                })
                .collect(),
        }
    }

    // ------------------------------------------------ 分类

    #[test]
    fn classify_covers_every_shape() {
        assert_eq!(classify_file_name("tokenagency.log"), FileKind::Current);
        assert_eq!(classify_file_name("tokenagency.20260211-120422.log"), FileKind::Rotated);
        assert_eq!(classify_file_name("crash-20260211-120422.log"), FileKind::Crash);
        for foreign in [
            "tokenagency.log.bak",
            "tokenagency.2026-02-11.log",
            "notes.txt",
            "crash.log",
            "用户自己放的.txt",
            "tokenagency.2026021-120422.log",
        ] {
            assert_eq!(classify_file_name(foreign), FileKind::Foreign, "{foreign} 必须算外人");
        }
    }

    #[test]
    fn generated_names_classify_as_expected() {
        let now = ts("2026-02-11T12:04:22Z");
        let a = archive_name_local(now, 480);
        assert_eq!(a, "tokenagency.20260211-200422.log", "本地 +08:00 的小时数要加上去");
        assert_eq!(classify_file_name(&a), FileKind::Rotated);
        let c = crash_name_local(now, 480);
        assert_eq!(c, "crash-20260211-200422.log");
        assert_eq!(classify_file_name(&c), FileKind::Crash);
    }

    // ------------------------------------------------ 该不该轮转

    #[test]
    fn stays_put_when_under_limit_and_same_day() {
        let d = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-11T12:00:00Z"), 1_000, ts("2026-02-11T08:00:00Z")),
        );
        assert_eq!(d, RotationDecision::Keep);
    }

    #[test]
    fn exactly_at_the_limit_rotates() {
        // 恰好等于上限：下一次写就破线，所以现在就轮转（刻意的选择）
        let d = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-11T12:00:00Z"), DEFAULT_MAX_BYTES, ts("2026-02-11T08:00:00Z")),
        );
        assert_eq!(d, RotationDecision::Rotate { reason: RotateReason::Size, prune: vec![] });
    }

    #[test]
    fn size_and_day_change_together_are_reported_as_both() {
        let d = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-12T01:00:00Z"), DEFAULT_MAX_BYTES, ts("2026-02-11T08:00:00Z")),
        );
        assert_eq!(d, RotationDecision::Rotate { reason: RotateReason::SizeAndDay, prune: vec![] });
    }

    #[test]
    fn day_change_alone_rotates() {
        let d = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-12T01:00:00Z"), 10, ts("2026-02-11T08:00:00Z")),
        );
        assert_eq!(d, RotationDecision::Rotate { reason: RotateReason::Day, prune: vec![] });
    }

    #[test]
    fn day_boundary_follows_the_local_timezone() {
        // 北京 2026-02-11 23:00（UTC 15:00）与 北京 2026-02-12 00:00（UTC 16:00）
        let before = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-11T15:00:00Z"), 10, ts("2026-02-11T01:00:00Z")),
        );
        assert_eq!(before, RotationDecision::Keep, "本地还是 11 号");

        let after = decide(
            &RotationPolicy::default(),
            &input(ts("2026-02-11T16:00:00Z"), 10, ts("2026-02-11T01:00:00Z")),
        );
        assert_eq!(
            after,
            RotationDecision::Rotate { reason: RotateReason::Day, prune: vec![] },
            "本地已经 12 号 → 必须轮转"
        );
    }

    #[test]
    fn daily_rotation_can_be_disabled() {
        let policy = RotationPolicy { rotate_daily: false, ..Default::default() };
        let d = decide(&policy, &input(ts("2026-02-12T01:00:00Z"), 10, ts("2026-02-11T08:00:00Z")));
        assert_eq!(d, RotationDecision::Keep);
    }

    #[test]
    fn manual_rotation_wins_over_everything_else() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 10);
        i.manual = true;
        let d = decide(&RotationPolicy::default(), &i);
        assert_eq!(d, RotationDecision::Rotate { reason: RotateReason::Manual, prune: vec![] });
    }

    // ------------------------------------------------ 清理

    #[test]
    fn keeps_only_the_configured_number_of_archives() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 100);
        i.existing = (1..=10)
            .map(|n| rotated(&format!("tokenagency.20260201-{n:06}.log"), 10, 1_000 + n))
            .collect();
        let policy = RotationPolicy { keep_files: 3, ..Default::default() };
        // 10 份已有 + 本次 1 份 = 11 → 删掉最老的 8 份
        let d = decide(&policy, &i);
        let names = prune_names(&d);
        assert_eq!(names.len(), 8);
        assert_eq!(names[0], "tokenagency.20260201-000001.log", "最老先删");
        assert!(!names.contains(&"<new>".to_string()), "最新的那份要留着");
    }

    #[test]
    fn keep_files_zero_deletes_even_the_new_archive() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 100);
        i.existing = vec![rotated("tokenagency.20260201-000001.log", 10, 1_000)];
        let policy = RotationPolicy { keep_files: 0, ..Default::default() };
        let names = prune_names(&decide(&policy, &i));
        assert_eq!(names, vec!["tokenagency.20260201-000001.log", "<new>"]);
    }

    #[test]
    fn keep_files_one_keeps_exactly_one() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 100);
        i.existing = vec![
            rotated("tokenagency.20260201-000001.log", 10, 1_000),
            rotated("tokenagency.20260202-000001.log", 10, 2_000),
        ];
        let policy = RotationPolicy { keep_files: 1, ..Default::default() };
        let names = prune_names(&decide(&policy, &i));
        assert_eq!(
            names,
            vec!["tokenagency.20260201-000001.log", "tokenagency.20260202-000001.log"]
        );
    }

    #[test]
    fn foreign_and_crash_files_are_never_deleted() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 10);
        i.existing = vec![
            // 用户自己放的东西，而且很大 —— 不管总量怎么算都不许删
            rotated("我的笔记.txt", 500 * 1024 * 1024, 1),
            rotated("tokenagency.20260201-000001.log", 10, 1_000),
            rotated("crash-20260201-000001.log", 900 * 1024 * 1024, 2),
            rotated("tokenagency.log.bak", 1024, 3),
        ];
        let policy = RotationPolicy { keep_files: 0, max_total_bytes: 1, ..Default::default() };
        let names = prune_names(&decide(&policy, &i));
        assert_eq!(
            names,
            vec!["tokenagency.20260201-000001.log", "<new>"],
            "只许删我们自己轮转出来的归档"
        );
    }

    #[test]
    fn prunes_by_total_bytes_even_when_count_is_fine() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 0);
        i.existing = (1..=6)
            .map(|n| {
                rotated(&format!("tokenagency.2026020{n}-000000.log"), 10 * 1024 * 1024, 1_000 + n)
            })
            .collect();
        let policy = RotationPolicy {
            keep_files: 7,                     // 份数没超
            max_total_bytes: 32 * 1024 * 1024, // 但 60 MiB > 32 MiB
            ..Default::default()
        };
        let names = prune_names(&decide(&policy, &i));
        assert_eq!(names.len(), 3, "6 份 × 10 MiB 删到 3 份（30 MiB）才 ≤ 32 MiB");
        assert_eq!(names[0], "tokenagency.20260201-000000.log");
    }

    #[test]
    fn zero_total_limit_means_unlimited() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 0);
        i.existing = vec![rotated("tokenagency.20260201-000000.log", u64::MAX / 2, 1_000)];
        let policy = RotationPolicy { max_total_bytes: 0, ..Default::default() };
        assert!(prune_names(&decide(&policy, &i)).is_empty(), "0 = 不限制总量，所以一个都不删");
    }

    #[test]
    fn realistic_scenario_keeps_the_directory_bounded() {
        // 11 份各 7 MiB 的归档 + 当前文件 8 MiB（正好触顶）
        let mut i = input(ts("2026-02-11T12:00:00Z"), 8 * 1024 * 1024, ts("2026-02-11T08:00:00Z"));
        i.existing = (1..=11)
            .map(|n| {
                rotated(&format!("tokenagency.202602{n:02}-000000.log"), 7 * 1024 * 1024, 1_000 + n)
            })
            .collect();
        let policy = RotationPolicy::default(); // 8 MiB / 7 份 / 64 MiB
        match decide(&policy, &i) {
            RotationDecision::Rotate { reason, prune } => {
                assert_eq!(reason, RotateReason::Size);
                // 轮转后归档 = 12 份 → 删到 7 份（删 5 份）
                assert_eq!(prune.len(), 5);
                let kept = 12 - prune.len();
                assert_eq!(kept, policy.keep_files);
                assert!(kept as u64 * 7 * 1024 * 1024 <= policy.max_total_bytes);
            }
            other => panic!("应轮转，实际 {other:?}"),
        }
    }

    #[test]
    fn decisions_are_deterministic_for_equal_timestamps() {
        let mut i = rotating_input(ts("2026-02-11T12:00:00Z"), 0);
        i.existing = vec![
            rotated("tokenagency.20260201-bbbbbb.log", 1, 5_000),
            rotated("tokenagency.20260201-aaaaaa.log", 1, 5_000),
        ];
        // 名字本身不合法（不是时间戳）→ 算外人，不参与清理；这里用合法名字再测一次顺序
        i.existing = vec![
            rotated("tokenagency.20260101-000000.log", 1, 5_000),
            rotated("tokenagency.20251231-000000.log", 1, 5_000),
        ];
        let policy = RotationPolicy { keep_files: 0, ..Default::default() };
        let a = prune_names(&decide(&policy, &i));
        let b = prune_names(&decide(&policy, &i));
        assert_eq!(a, b, "同一输入必须给出同一结果");
        assert_eq!(a[0], "tokenagency.20251231-000000.log", "时间相同时按名字升序");
    }
}

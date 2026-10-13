//! ★ **C2 / C4 判定** + **D2 的诚实提示契约** —— 对**运行时日志行**的解析与筛选。
//!
//! 与 [`super::scan`] 的分工：`scan` 解析**源码**（守卫测试用），本模块解析
//! **`tracing_subscriber::fmt` 写出来的行**（应用日志页与诊断包用）。
//!
//! ## 为什么"筛选"也需要判定函数
//!
//! 的守卫能保证**字段名**不漂移，但保证不了**筛选语义正确**。
//! 最典型的错法是子串匹配：`req_abc` 是 `req_abcdef` 的前缀，
//! 用 `line.contains(request_id)` 会把**两次请求**的轨迹混在一起 ——
//! 而这种错**在测试里很难暴露**（日志看起来更"全"了），却会让人误信。
//! 所以这里一律按 `字段名=值` 的**精确 token** 匹配（见 [`line_has_request_id`]）。
//!
//! ## C4 的现状（重要）
//!
//! 判定函数在这里，但**目前没有可筛的行**：全仓没有任何生产代码在 `tracing` 里带
//! `request_id`（只有守卫测试的示例字符串里有）。所以 C4 = 判定就绪 + **接线缺口在
//! 应用层/网关侧**（E3/E5），本模块提供"一行日志长什么样才算合格"的判据。

use std::borrow::Cow;

use crate::types::LogMode;

use super::fields;

// ---------------------------------------------------------------- 行解析

/// `tracing` 级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    /// 解析级别。**不是 `const fn`**：`PartialEq` 对 `str` 不是 const-stable
    /// （`E0658`），而这里根本不需要编译期求值。
    pub fn parse(s: &str) -> Option<LogLevel> {
        // `tracing_subscriber` 默认把级别右对齐到 5 字符（` WARN`），所以要 trim
        match s.trim() {
            "TRACE" => Some(LogLevel::Trace),
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            _ => None,
        }
    }

    /// 是不是"出问题了"的那两级（错误摘要只统计这两级）。
    pub const fn is_problem(&self) -> bool {
        matches!(self, LogLevel::Warn | LogLevel::Error)
    }
}

/// 一行日志拆出来的三部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineParts<'a> {
    /// 时间戳后的级别（识别不出就是 `None`）
    pub level: Option<LogLevel>,
    /// `tracing` 的 target（`ta_gateway::ingress` 这类），即"哪个 crate 的哪个模块"
    pub target: Option<&'a str>,
    /// 人话部分（target 之后）
    pub message: &'a str,
}

/// 去掉 ANSI 转义序列（文件层应关闭颜色，但 UI 拿到的不一定）。
pub fn strip_ansi(input: &str) -> Cow<'_, str> {
    if !input.contains('\u{1b}') {
        return Cow::Borrowed(input);
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // CSI：ESC [ ... 直到一个 0x40..=0x7e 的终止字节
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i = i.saturating_add(1);
            }
            continue;
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..(i + ch_len).min(input.len())]);
        i += ch_len;
    }
    Cow::Owned(out)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// 解析一行 `tracing_subscriber::fmt` 输出。
///
/// 期望形态（`with_target(true)`、默认 timer）：
///
/// ```text
/// 2026-02-11T12:04:22.123456Z ERROR ta_gateway::inbound: 全部通道失败 request_id=req_01H
/// ```
///
/// **识别不出就返回 `None` 字段，不猜**：日志行格式变了要能被看出来，
/// 而不是悄悄把 message 当成 target（那会让 C4 的覆盖度统计变成假数据）。
pub fn parse_line(line: &str) -> LineParts<'_> {
    let mut it = line.split_whitespace();
    let (Some(first), maybe_second) = (it.next(), it.next()) else {
        return LineParts { level: None, target: None, message: line };
    };
    // 两种形态：`<时间戳> LEVEL target: msg`（我们写文件时的形态）与 `LEVEL target: msg`
    let (level, target_token) = if let Some(l) = LogLevel::parse(first) {
        (Some(l), maybe_second)
    } else if looks_like_timestamp(first) {
        let level = maybe_second.and_then(LogLevel::parse);
        let target = if level.is_some() { it.next() } else { None };
        (level, target)
    } else {
        (None, None)
    };
    let Some(tok) = target_token else {
        return LineParts { level, target: None, message: line };
    };
    let Some(target) = tok.strip_suffix(':') else {
        return LineParts { level, target: None, message: line };
    };
    // message = target token 之后剩下的全部（按字节偏移切，保留原始空白内的中文）
    let start = match line.find(tok) {
        Some(s) => s + tok.len(),
        None => line.len(),
    };
    LineParts { level, target: Some(target), message: line[start..].trim_start() }
}

fn looks_like_timestamp(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() >= 20
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
}

// ---------------------------------------------------------------- 字段精确匹配

/// 取 `field=value` 里 value 的**精确**值。
///
/// 规则（这是"精确"的定义，别放松它）：
/// - `field` 左侧必须是行首或**非标识符字符**（`x_request_id=` 不会被当成 `request_id`）；
/// - `field` 右侧必须是 `=`（`request_id:` 不算）；
/// - 值到空白或 `,;)]}` 为止，两侧引号去掉。
pub fn field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    if field.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = line[from..].find(field) {
        let at = from + rel;
        let end_name = at + field.len();
        let left_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let right_ok = bytes.get(end_name) == Some(&b'=');
        if left_ok && right_ok {
            let rest = &line[end_name + 1..];
            return Some(trim_field_value(rest));
        }
        from = at + 1;
        if from >= line.len() {
            break;
        }
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn trim_field_value(rest: &str) -> &str {
    let rest = rest.trim_start();
    if let Some(inner) = rest.strip_prefix('"') {
        return match inner.find('"') {
            Some(i) => &inner[..i],
            None => inner,
        };
    }
    if let Some(inner) = rest.strip_prefix('\'') {
        return match inner.find('\'') {
            Some(i) => &inner[..i],
            None => inner,
        };
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']' | '}'))
        .unwrap_or(rest.len());
    rest[..end].trim_end_matches(['"', '\''])
}

/// 这一行是否属于 `request_id` 这次请求（**精确相等**）。
pub fn line_has_request_id(line: &str, request_id: &str) -> bool {
    field_value(line, fields::REQUEST_ID) == Some(request_id)
}

/// 按 `request_id` 精确筛行（验收第 4 条的能力）。
pub fn filter_by_request_id<'a, I>(lines: I, request_id: &str) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    lines.into_iter().filter(|l| line_has_request_id(l, request_id)).collect()
}

/// 按 `job` 精确筛行（后台任务没有 `request_id`，用 `job` 关联 ——）。
pub fn filter_by_job<'a, I>(lines: I, job: &str) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    lines.into_iter().filter(|l| field_value(l, fields::JOB) == Some(job)).collect()
}

// ---------------------------------------------------------------- C2：错误码

/// 从一行里抽出所有 `TA-xxxx`（**严格四位数**）。
pub fn find_codes(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 7 <= bytes.len() {
        if &bytes[i..i + 3] == b"TA-" && bytes[i + 3..i + 7].iter().all(u8::is_ascii_digit) {
            // 五位以上不算（`TA-20051` 不是一个错误码）
            let extra = bytes.get(i + 7).copied().unwrap_or(b' ');
            let left_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            if left_ok && !extra.is_ascii_digit() {
                // SAFETY：ASCII 范围内，切片一定是合法 UTF-8 边界
                out.push(&line[i..i + 7]);
                i += 7;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 是不是一个合法的错误码（形状判定，不认识具体含义）。
pub fn is_error_code(s: &str) -> bool {
    s.len() == 7 && s.as_bytes()[..3] == *b"TA-" && s.as_bytes()[3..].iter().all(u8::is_ascii_digit)
}

/// 这一行是否提到某个错误码（用 [`find_codes`] 的严格口径，不是子串）。
pub fn line_mentions_code(line: &str, code: &str) -> bool {
    find_codes(line).contains(&code)
}

/// 按错误码精确筛行。
pub fn filter_by_code<'a, I>(lines: I, code: &str) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    lines.into_iter().filter(|l| line_mentions_code(l, code)).collect()
}

/// 错误摘要里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeCount {
    pub code: String,
    pub count: u64,
}

/// 的"错误摘要"：**只统计 `WARN`/`ERROR` 行**里的码，次数降序、同次按码升序。
///
/// 识别不出级别的行**不统计**（宁可少一条，也不把 INFO 里的提示语算成故障）。
pub fn error_code_summary<'a, I>(lines: I, max_codes: usize) -> Vec<CodeCount>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut counts: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for line in lines {
        let level = parse_line(line).level;
        if !level.map(|l| l.is_problem()).unwrap_or(false) {
            continue;
        }
        for code in find_codes(line) {
            *counts.entry(code).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<CodeCount> = counts
        .into_iter()
        .map(|(code, count)| CodeCount { code: code.to_string(), count })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.code.cmp(&b.code)));
    rows.truncate(max_codes);
    rows
}

// ---------------------------------------------------------------- C4：轨迹覆盖度

/// 列的请求轨迹阶段（按经过顺序）：
/// `ingress → pipeline → routing → kernel → dispatch → meter → trace`。
///
/// **这是建议口径**，不是硬契约：真实 target 名由各 crate 决定，
/// 所以 [`trail_coverage_with`] 允许调用方传入自己的阶段表。
pub const TRAIL_STAGES: &[&str] =
    &["ingress", "pipeline", "routing", "kernel", "dispatch", "meter", "trace"];

/// 一次请求的轨迹覆盖度（C4 的判据：**完整**轨迹）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailCoverage {
    pub request_id: String,
    /// 期望的阶段数
    pub total: usize,
    /// 命中的阶段（按期望顺序）
    pub found: Vec<String>,
    /// 缺失的阶段
    pub missing: Vec<String>,
}

impl TrailCoverage {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty() && !self.found.is_empty()
    }

    /// 给 UI/日志用的一句话（不含 emoji，[15]）。
    pub fn summary(&self) -> String {
        format!(
            "request_id={} 轨迹 {}/{}；缺失：{}",
            self.request_id,
            self.found.len(),
            self.total,
            if self.missing.is_empty() { "无".to_string() } else { self.missing.join(",") }
        )
    }
}

/// 用 [`TRAIL_STAGES`] 统计覆盖度。
pub fn trail_coverage<'a, I>(lines: I, request_id: &str) -> TrailCoverage
where
    I: IntoIterator<Item = &'a str>,
{
    trail_coverage_with(TRAIL_STAGES, lines, request_id)
}

/// 用自定义阶段表统计覆盖度。
///
/// 阶段命中规则：该行属于这个 `request_id`，且它的 target 里**某一段**
/// 等于阶段名或以阶段名开头（`ta_core::metering` 命中 `meter`）。
pub fn trail_coverage_with<'a, I>(stages: &[&str], lines: I, request_id: &str) -> TrailCoverage
where
    I: IntoIterator<Item = &'a str>,
{
    let mut found: Vec<String> = Vec::new();
    for line in lines {
        if !line_has_request_id(line, request_id) {
            continue;
        }
        let Some(target) = parse_line(line).target else {
            continue;
        };
        for stage in stages {
            if found.iter().any(|f| f == stage) {
                continue;
            }
            if stage_matches(target, stage) {
                found.push((*stage).to_string());
            }
        }
    }
    let missing: Vec<String> = stages
        .iter()
        .filter(|s| !found.iter().any(|f| f.as_str() == **s))
        .map(|s| (*s).to_string())
        .collect();
    TrailCoverage { request_id: request_id.to_string(), total: stages.len(), found, missing }
}

fn stage_matches(target: &str, stage: &str) -> bool {
    target.split("::").any(|seg| seg == stage || seg.starts_with(stage))
}

// ---------------------------------------------------------------- D2：诚实提示契约

/// 一个筛选项/展示项在当前日志模式下**能不能判定**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Support {
    pub enabled: bool,
    /// 不可判定的**诚实原因**（给 UI 直接显示；`enabled = true` 时为 `None`）
    pub reason: Option<&'static str>,
}

impl Support {
    const fn on() -> Self {
        Support { enabled: true, reason: None }
    }
    const fn off(reason: &'static str) -> Self {
        Support { enabled: false, reason: Some(reason) }
    }
}

/// 的诚实提示文案。**不要改写成"未知即正常"** ——
/// 那是拿不确定冒充确定（铁律 4 的同一条道理）。
pub const RETRIED_UNKNOWN_REASON: &str =
    "请求日志已关闭（log.mode=off），没有尝试链可看，无法判断是否发生过重试；开启请求日志后才能筛选重试请求";

/// 日志页"重试"筛选项在当前模式下是否可用。
///
/// 保留 `ta-store` 里"未知即 ok"的实现（裁决），
/// 但 UI **必须**按本函数禁用该筛选项并显示 [`Support::reason`]。
pub fn retried_filter_support(mode: LogMode) -> Support {
    if mode.enabled() {
        Support::on()
    } else {
        Support::off(RETRIED_UNKNOWN_REASON)
    }
}

/// 请求列表里的 `outcome` 在当前模式下是否**可判定**。
///
/// `false` 时 UI 必须把它显示成"未知"，**不能显示成"正常"** ——
/// 数据库里那一列是按"未知即 ok"写的（实现保留），显示层不能再抄一遍这个近似。
pub fn outcome_display_support(mode: LogMode) -> Support {
    if mode.enabled() {
        Support::on()
    } else {
        Support::off("请求日志已关闭，本条请求是否重试过无从得知，显示为“未知”")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::fields;
    use pretty_assertions::assert_eq;

    const L_ERROR: &str = "2026-02-11T12:04:22.123456Z ERROR ta_gateway::inbound: 全部通道失败 \
                           request_id=req_01HQZX channel_id=ch-relay-a attempt=2 code=TA-2005";

    // ------------------------------------------------ 行解析

    #[test]
    fn parses_the_fmt_output_shape() {
        let p = parse_line(L_ERROR);
        assert_eq!(p.level, Some(LogLevel::Error));
        assert_eq!(p.target, Some("ta_gateway::inbound"));
        assert!(p.message.starts_with("全部通道失败"), "{}", p.message);
    }

    #[test]
    fn unknown_shape_yields_none_instead_of_guessing() {
        let p = parse_line("这是一行没有时间戳和级别的文本");
        assert_eq!(p.level, None);
        assert_eq!(p.target, None);
        assert_eq!(p.message, "这是一行没有时间戳和级别的文本");
    }

    #[test]
    fn ansi_colored_lines_are_stripped() {
        let colored = "\u{1b}[32m2026-02-11T12:04:22.123456Z\u{1b}[0m \u{1b}[1mINFO\u{1b}[0m ta_core::quota: ok";
        let plain = strip_ansi(colored);
        assert!(!plain.contains('\u{1b}'));
        let p = parse_line(&plain);
        assert_eq!(p.level, Some(LogLevel::Info));
        assert_eq!(p.target, Some("ta_core::quota"));
    }

    // ------------------------------------------------ 精确筛选（本模块的核心纪律）

    #[test]
    fn request_id_filter_is_exact_not_substring() {
        let lines = [
            "2026-02-11T12:04:22Z INFO a::b: x request_id=req_abc",
            "2026-02-11T12:04:22Z INFO a::b: x request_id=req_abcdef",
            "2026-02-11T12:04:22Z INFO a::b: x prev_request_id=req_abc",
            "2026-02-11T12:04:22Z INFO a::b: request_id=\"req_abc\" 带引号也算",
        ];
        let hit = filter_by_request_id(lines, "req_abc");
        assert_eq!(hit.len(), 2, "只应命中完全相等的两行：{hit:#?}");
        assert!(hit.iter().all(|l| !l.contains("req_abcdef")));

        // 证明"子串匹配是错的"：它会多命中一行前缀请求 + 一行别名的字段
        let naive: Vec<&&str> = lines.iter().filter(|l| l.contains("req_abc")).collect();
        assert_eq!(
            naive.len(),
            4,
            "子串匹配会混入 req_abcdef 与 prev_request_id —— 这正是不能放松成 contains 的原因"
        );
    }

    #[test]
    fn field_value_needs_the_field_to_start_at_a_boundary() {
        assert_eq!(field_value("request_id=req_1", fields::REQUEST_ID), Some("req_1"));
        assert_eq!(field_value("xrequest_id=req_1", fields::REQUEST_ID), None);
        assert_eq!(field_value("prev_request_id=req_1", fields::REQUEST_ID), None);
        assert_eq!(field_value("request_id: req_1", fields::REQUEST_ID), None);
        assert_eq!(field_value("a=1 request_id=req_1 b=2", fields::REQUEST_ID), Some("req_1"));
        // 逗号/分号结尾要剥掉，否则 `req_1,` 比不过 `req_1`
        assert_eq!(field_value("request_id=req_1,", fields::REQUEST_ID), Some("req_1"));
        assert_eq!(field_value("request_id=req_1;", fields::REQUEST_ID), Some("req_1"));
    }

    #[test]
    fn job_filter_uses_the_same_exactness() {
        let lines = [
            "2026-02-11T12:04:22Z WARN ta_kernel::supervisor: restart job=kernel_supervisor pid=9",
            "2026-02-11T12:04:22Z WARN ta_kernel::supervisor: restart sub_job=kernel_supervisor",
        ];
        assert_eq!(filter_by_job(lines, "kernel_supervisor").len(), 1);
    }

    // ------------------------------------------------ C2：错误码

    #[test]
    fn codes_are_found_strictly_four_digits() {
        assert_eq!(find_codes(L_ERROR), vec!["TA-2005"]);
        assert_eq!(find_codes("TA-20051 不是错误码"), Vec::<&str>::new());
        assert_eq!(find_codes("TA-200"), Vec::<&str>::new());
        assert_eq!(find_codes("xTA-2005"), Vec::<&str>::new());
        assert!(is_error_code("TA-4001"));
        assert!(!is_error_code("TA-40011"));
        assert!(!is_error_code("ta-4001"));
    }

    #[test]
    fn code_filter_finds_multiple_codes_in_one_line() {
        let line = "2026-02-11T12:04:22Z ERROR ta_core::routing: 先 TA-3002 后 TA-2005";
        assert_eq!(find_codes(line), vec!["TA-3002", "TA-2005"]);
        assert_eq!(filter_by_code([line], "TA-3002").len(), 1);
        assert_eq!(filter_by_code([line], "TA-2006").len(), 0);
    }

    #[test]
    fn error_summary_counts_only_warn_and_error() {
        let lines = [
            L_ERROR,
            "2026-02-11T12:04:23Z ERROR ta_gateway::inbound: 又失败 code=TA-2005",
            "2026-02-11T12:04:24Z WARN ta_core::quota: 快用完 code=TA-1002",
            "2026-02-11T12:04:25Z INFO ta_gateway::trace: 重试中 code=TA-2005", // INFO 不算故障
            "2026-02-11T12:04:26Z ERROR ta_store::trace: 写库失败",             // 没码，不统计
        ];
        let s = error_code_summary(lines, 5);
        assert_eq!(
            s,
            vec![
                CodeCount { code: "TA-2005".into(), count: 2 },
                CodeCount { code: "TA-1002".into(), count: 1 }
            ]
        );
        assert_eq!(error_code_summary(lines, 1).len(), 1, "max_codes 要生效（诊断包只留前 N 条）");
    }

    // ------------------------------------------------ C4：轨迹覆盖度

    #[test]
    fn trail_coverage_reports_the_missing_stages() {
        let lines = [
            "2026-02-11T12:04:22Z INFO ta_gateway::ingress: 收到 request_id=req_1",
            "2026-02-11T12:04:22Z INFO ta_gateway::pipeline: 准备 request_id=req_1",
            "2026-02-11T12:04:22Z INFO ta_core::routing: 选路 request_id=req_1 lane_id=lane-a",
            "2026-02-11T12:04:22Z INFO ta_core::metering: 记账 request_id=req_1",
            "2026-02-11T12:04:22Z INFO ta_gateway::ingress: 别的一次请求 request_id=req_2",
        ];
        let c = trail_coverage(lines, "req_1");
        assert_eq!(c.total, TRAIL_STAGES.len());
        assert!(c.found.contains(&"ingress".to_string()));
        assert!(c.found.contains(&"meter".to_string()), "metering 要命中 meter");
        assert!(c.missing.contains(&"dispatch".to_string()));
        assert!(!c.is_complete());
        let s = c.summary();
        assert!(s.contains("request_id=req_1") && s.contains("dispatch"), "{s}");
        assert!(!s.contains("req_2"), "别的请求的行不能混进覆盖度：{s}");
    }

    #[test]
    fn complete_trail_is_recognized() {
        let lines: Vec<String> = TRAIL_STAGES
            .iter()
            .map(|s| format!("2026-02-11T12:04:22Z INFO ta_x::{s}: go request_id=req_9"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let c = trail_coverage(refs, "req_9");
        assert!(c.is_complete(), "{c:?}");
        assert_eq!(c.found.len(), TRAIL_STAGES.len());
    }

    #[test]
    fn trail_coverage_refuses_to_call_an_empty_result_complete() {
        // 一行都没有时不算"完整"——否则"没有日志"会被当成"轨迹齐全"
        let c = trail_coverage(Vec::<&str>::new(), "req_none");
        assert!(!c.is_complete());
        assert_eq!(c.found.len(), 0);
    }

    // ------------------------------------------------ D2：诚实提示

    #[test]
    fn retried_filter_is_disabled_and_explained_when_logs_are_off() {
        let off = retried_filter_support(LogMode::Off);
        assert!(!off.enabled);
        let reason = off.reason.expect("禁用必须给原因（否则用户以为功能坏了）");
        assert!(reason.contains("无法判断"), "{reason}");
        assert!(reason.contains("重试"), "{reason}");

        for m in [LogMode::Meta, LogMode::Shape, LogMode::Full] {
            assert!(retried_filter_support(m).enabled, "{m:?} 下应可判定");
            assert_eq!(retried_filter_support(m).reason, None);
        }
    }

    #[test]
    fn outcome_display_never_pretends_unknown_is_ok() {
        let off = outcome_display_support(LogMode::Off);
        assert!(!off.enabled);
        let reason = off.reason.unwrap();
        assert!(reason.contains("未知"), "{reason}");
        assert!(!reason.contains("正常"), "未知不许说成正常：{reason}");
    }
}

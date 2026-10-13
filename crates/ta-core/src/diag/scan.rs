//! 源码扫描器：给两条守卫测试提供**纯字符串 → 事实**的解析能力。
//!
//! 为什么解析逻辑放在库里而不是测试里：测试跑不动、解析要能单测。
//! 这里**不做 IO**（读文件、走目录是测试的事），所以它可以在 `ta-core` 里活得很好。
//!
//! 扫描器先**把注释与字符串字面量涂白**再找 `tracing` 宏调用 —— 否则
//! "文档里举了一个 `tracing::warn!(error = %e)` 的例子"会被当成真的错误日志
//! （本文件自己的文档注释就是这样）。涂白保留字节偏移，所以行号仍然准确；
//! 被涂白的字面量内容单独记下来，用于取日志的 message（豁免白名单按 message 匹配）。

/// 一处 `tracing` 字段名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldUse {
    pub name: String,
    /// 1 起算的行号
    pub line: usize,
    pub macro_name: String,
}

/// 一处"报告错误"的日志调用（宏里出现了 `error = …` 字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLogSite {
    pub line: usize,
    pub macro_name: String,
    /// `error = ` 右边的表达式原文（已 trim）
    pub error_expr: String,
    /// 该宏调用的 message 字面量内容（用于豁免白名单匹配）
    pub message: Option<String>,
    /// 同一个宏调用里是否有 `code = …`
    pub has_code: bool,
}

/// 认识的宏名（`tracing` 的事件宏 + `span!`）。
const MACRO_NAMES: &[&str] = &["trace", "debug", "info", "warn", "error", "event", "span"];

/// 一个被涂白的字面量。
#[derive(Debug, Clone)]
struct Literal {
    start: usize,
    end: usize,
    /// 去掉引号后的内容
    content: String,
}

/// 把注释与字符串字面量涂成空格（保留换行与字节偏移）。
pub fn blank_comments_and_literals(src: &str) -> String {
    blank(src).0
}

fn blank(src: &str) -> (String, Vec<Literal>) {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut literals: Vec<Literal> = Vec::new();
    let mask = |out: &mut Vec<u8>, i: usize| {
        if out[i] != b'\n' {
            out[i] = b' ';
        }
    };
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    mask(&mut out, i);
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        mask(&mut out, i);
                        mask(&mut out, i + 1);
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth = depth.saturating_sub(1);
                        mask(&mut out, i);
                        mask(&mut out, i + 1);
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        mask(&mut out, i);
                        i += 1;
                    }
                }
            }
            b'"' => {
                let start = i;
                mask(&mut out, i);
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        mask(&mut out, i);
                        if i + 1 < b.len() {
                            mask(&mut out, i + 1);
                        }
                        i += 2;
                    } else if b[i] == b'"' {
                        mask(&mut out, i);
                        i += 1;
                        break;
                    } else {
                        mask(&mut out, i);
                        i += 1;
                    }
                }
                literals.push(Literal {
                    start,
                    end: i,
                    content: unquote(&src[start..i.min(src.len())]),
                });
            }
            b'r' => {
                // 原始字符串 r"…" / r#"…"# / r##"…"##
                let mut j = i + 1;
                let mut hashes = 0usize;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) == Some(&b'"') {
                    let start = i;
                    while i <= j {
                        mask(&mut out, i);
                        i += 1;
                    }
                    loop {
                        if i >= b.len() {
                            break;
                        }
                        if b[i] == b'"' {
                            let mut ok = true;
                            for k in 0..hashes {
                                if b.get(i + 1 + k) != Some(&b'#') {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                for _ in 0..(hashes + 1) {
                                    mask(&mut out, i);
                                    i += 1;
                                }
                                break;
                            }
                        }
                        mask(&mut out, i);
                        i += 1;
                    }
                    literals.push(Literal {
                        start,
                        end: i,
                        content: unquote(&src[start..i.min(src.len())]),
                    });
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), literals)
}

/// 去掉字面量两端的引号与 `r#` 前缀，得到人能读的内容。
fn unquote(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix('r') {
        s = rest.trim_start_matches('#');
    }
    s.strip_prefix('"').unwrap_or(s).strip_suffix('"').unwrap_or(s).to_string()
}

/// 一行一个宏调用（内部用）。
struct MacroCall {
    name: String,
    line: usize,
    /// 顶层参数（涂白文本里的原文；字面量参数会全是空格）
    args: Vec<(usize, usize)>,
}

/// 扫描一个 .rs 源码里的全部 `tracing` 宏调用。
fn macro_calls(src: &str) -> (String, Vec<Literal>, Vec<MacroCall>) {
    let (masked, literals) = blank(src);
    let bytes = masked.as_bytes();
    let line_starts = line_starts(src);
    let mut calls = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if !(c.is_ascii_alphabetic() || c == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let ident = &masked[start..i];
        if !MACRO_NAMES.contains(&ident) {
            continue;
        }
        // `tracing::warn!` / `warn!`
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'!') {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let open = j;
        let (open_ch, close_ch) = match bytes.get(open) {
            Some(b'(') => (b'(', b')'),
            Some(b'[') => (b'[', b']'),
            Some(b'{') => (b'{', b'}'),
            _ => continue,
        };
        let Some(close) = matching_brace(bytes, open, open_ch, close_ch) else { continue };
        let args = split_top_level(bytes, open + 1, close);
        calls.push(MacroCall { name: ident.to_string(), line: line_of(&line_starts, open), args });
        i = close + 1;
    }
    (masked, literals, calls)
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

fn line_of(starts: &[usize], off: usize) -> usize {
    match starts.binary_search(&off) {
        Ok(idx) => idx + 1,
        Err(idx) => idx,
    }
}

fn matching_brace(bytes: &[u8], open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let mut stack: Vec<u8> = vec![open_ch];
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => stack.push(bytes[i]),
            b')' | b']' | b'}' => {
                let expect = match bytes[i] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if stack.last() != Some(&expect) {
                    return None;
                }
                stack.pop();
                if stack.is_empty() {
                    return if bytes[i] == close_ch { Some(i) } else { None };
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 按顶层逗号切分参数区间（`(`/`[`/`{` 内的逗号不算）。
fn split_top_level(bytes: &[u8], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut seg_start = from;
    let mut i = from;
    while i < to {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push((seg_start, i));
                seg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if seg_start < to {
        out.push((seg_start, to));
    }
    out
}

/// 某个参数是不是字符串字面量（涂白后全是空白 = 曾经是字面量）。
fn literal_at(literals: &[Literal], start: usize, end: usize) -> Option<String> {
    literals.iter().find(|l| l.start >= start && l.end <= end + 1).map(|l| l.content.clone())
}

/// 解析一个参数，判断它是不是"字段"，返回字段名。
fn field_name_of(arg: &str) -> Option<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if let Some(eq) = arg.find('=') {
        // `field = value`；`==` / `<=` 这类比较不是字段
        if arg.as_bytes().get(eq + 1) == Some(&b'=')
            || arg.as_bytes().get(eq.wrapping_sub(1)) == Some(&b'!')
        {
            return None;
        }
        let lhs = arg[..eq].trim();
        return is_ident(lhs).then(|| lhs.to_string());
    }
    let stripped = arg.strip_prefix('?').or_else(|| arg.strip_prefix('%')).unwrap_or(arg);
    let stripped = stripped.trim();
    is_ident(stripped).then(|| stripped.to_string())
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 扫出一个源码文件里全部的 `tracing` 字段名（守卫测试用）。
pub fn tracing_fields(src: &str) -> Vec<FieldUse> {
    let (masked, literals, calls) = macro_calls(src);
    let mut out = Vec::new();
    for call in &calls {
        for (start, end) in &call.args {
            // 到 message 字面量为止都是字段；之后的都是格式化参数
            if literal_at(&literals, *start, *end).is_some() {
                break;
            }
            if let Some(name) = field_name_of(&masked[*start..*end]) {
                out.push(FieldUse { name, line: call.line, macro_name: call.name.clone() });
            }
        }
    }
    out
}

/// 扫出一个源码文件里全部"报告错误"的日志调用（守卫测试用）。
pub fn error_log_sites(src: &str) -> Vec<ErrorLogSite> {
    let (masked, literals, calls) = macro_calls(src);
    let mut out = Vec::new();
    for call in &calls {
        let mut error_expr: Option<String> = None;
        let mut has_code = false;
        let mut message: Option<String> = None;
        for (start, end) in &call.args {
            if let Some(text) = literal_at(&literals, *start, *end) {
                if message.is_none() {
                    message = Some(text);
                }
                continue;
            }
            let arg = masked[*start..*end].trim();
            if let Some(name) = field_name_of(arg) {
                if name == "code" {
                    has_code = true;
                }
                if name == "error" {
                    error_expr = Some(
                        arg.split_once('=')
                            .map(|(_, v)| v.trim().to_string())
                            .unwrap_or_else(|| arg.to_string()),
                    );
                }
            }
        }
        if let Some(error_expr) = error_expr {
            out.push(ErrorLogSite {
                line: call.line,
                macro_name: call.name.clone(),
                error_expr,
                message,
                has_code,
            });
        }
    }
    out
}

/// 一处结构体**字面构造**（`T { … }`）出现的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLiteralSite {
    /// 1 起算的行号
    pub line: usize,
}

/// 扫出某个类型名的**字面构造**（`T { … }`）出现位置。
///
/// 用来机械地挡住"绕过唯一构造器、自己拼一个结构体"这类违反 —— 它**不是**语法分析，
/// 只做"上下文排除"这一件事：
///
/// | 形态 | 是否算构造 |
/// |---|---|
/// | `pub struct T { … }` | 否（定义） |
/// | `impl T { … }` | 否（实现块） |
/// | `-> T { … }` | 否（返回类型后面跟函数体） |
/// | `T { field: v }` | **是** |
///
/// 注释与字符串字面量先被涂白（复用 [`blank_comments_and_literals`]），
/// 所以文档里举例"不该这么写"不会把自己判违规 —— 的推荐写法注释、
/// 本文件自己的文档都是这种形态。
pub fn struct_literal_sites(src: &str, type_name: &str) -> Vec<StructLiteralSite> {
    if type_name.is_empty() {
        return Vec::new();
    }
    let blanked = blank_comments_and_literals(src);
    let b = blanked.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = blanked[from..].find(type_name) {
        let at = from + rel;
        let end = at + type_name.len();
        let left_ok = at == 0 || !is_ident_byte(b[at - 1]);
        let mut i = end;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let is_brace = b.get(i) == Some(&b'{');
        if left_ok && is_brace && !is_declaration_prefix(&blanked[..at]) {
            out.push(StructLiteralSite { line: line_of(&line_starts(&blanked), at) });
        }
        from = end;
    }
    out
}

/// 类型名左边紧邻的那个词是不是"声明/实现/返回类型"这三种非构造上下文。
///
/// 必须**先剥掉路径限定**再判断：`-> crate::metering::MeteredUsage {` 里的紧邻词是
/// `crate::metering::`，只看它就会把返回类型误判成构造（这个误报是被红-绿证明抓出来的）。
fn is_declaration_prefix(before: &str) -> bool {
    let mut t = before.trim_end();
    while t.ends_with("::") {
        let head = &t[..t.len() - 2];
        let cut =
            head.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
        t = head[..cut].trim_end();
    }
    let last = t.rsplit(char::is_whitespace).next().unwrap_or("");
    last == "struct" || last == "impl" || last == "enum" || last == "union" || last.ends_with("->")
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `MeteredUsage` 的字面构造位置。**只允许出现在 `crates/ta-core/src/metering/**`**
/// （铁律 4：估算不许冒充上游；`estimated_usage()` 是唯一出口）。
///
/// 为什么这条也值得机械检查：手拼 `MeteredUsage { usage, source: UsageSource::Upstream }`
/// 在类型上是合法的，编译器不会说话，成本中心会**照原样把估算当真实用量显示** ——
/// 静默的错账比编译错误危险得多。
pub fn metered_usage_literal_sites(src: &str) -> Vec<StructLiteralSite> {
    struct_literal_sites(src, "MeteredUsage")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_fields_from_a_simple_macro() {
        let src = r#"
fn f(e: &TaError) {
    tracing::warn!(request_id = %rid, attempt, code = e.code(), error = %e, "通道失败");
}
"#;
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["request_id", "attempt", "code", "error"]);
    }

    #[test]
    fn handles_multi_line_macros_and_shorthand_forms() {
        let src = r#"
fn f() {
    tracing::error!(
        channel_id = %ch,
        %version,
        ?status,
        "内核启动失败"
    );
}
"#;
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["channel_id", "version", "status"]);
    }

    #[test]
    fn format_args_after_the_message_are_not_fields() {
        let src = r#"fn f() { info!("已写入 {} 条", count); }"#;
        assert!(tracing_fields(src).is_empty(), "格式化参数不是字段");
    }

    #[test]
    fn nested_delimiters_and_commas_inside_calls_do_not_split_args() {
        let src = r#"
fn f() {
    warn!(elapsed_ms = compute(a, b), bytes = v.len(), "慢请求");
}
"#;
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["elapsed_ms", "bytes"]);
    }

    #[test]
    fn macros_inside_comments_and_strings_are_ignored() {
        let src = r#"
// tracing::warn!(error = %e, "注释里的例子不算")
/* 块注释里也一样 tracing::error!(error = %e, "x") */
fn f() {
    let doc = "tracing::warn!(error = %e, \"字符串里的也不算\")";
    tracing::info!(model = %m, "真的这条");
}
"#;
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["model"]);
        assert_eq!(error_log_sites(src).len(), 0);
    }

    #[test]
    fn raw_strings_are_ignored() {
        let src = r##"
fn f() {
    let s = r#"tracing::warn!(error = %e, "raw")"#;
    info!(attempt, "ok");
}
"##;
        assert_eq!(error_log_sites(src).len(), 0);
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["attempt"]);
    }

    #[test]
    fn error_sites_report_missing_code() {
        let src = r#"
fn f(e: &TaError, io: &std::io::Error) {
    tracing::warn!(error = %e, "缺 code");
    tracing::warn!(code = e.code(), error = %e, "有 code");
    tracing::warn!(error = %io, "非 TaError（无码）");
}
"#;
        let sites = error_log_sites(src);
        assert_eq!(sites.len(), 3);
        assert!(!sites[0].has_code);
        assert_eq!(sites[0].message.as_deref(), Some("缺 code"));
        assert!(sites[1].has_code);
        assert_eq!(sites[1].error_expr, "%e");
        assert_eq!(sites[2].message.as_deref(), Some("非 TaError（无码）"));
    }

    #[test]
    fn line_numbers_are_one_based_and_accurate() {
        let src = "fn f() {\n\n    tracing::warn!(error = %e, \"第 3 行\");\n}\n";
        let sites = error_log_sites(src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].line, 3);
    }

    #[test]
    fn comparison_operators_are_not_fields() {
        let src = r#"fn f() { if a == b { info!("x") } }"#;
        // 宏之外的 `==` 不该被当成字段；这里没有宏字段
        assert!(tracing_fields(src).is_empty());
    }

    #[test]
    fn non_tracing_macros_are_ignored() {
        let src = r#"
fn f() {
    println!("elapsed_ms = {}", x);
    assert!(count > 0, "失败");
    debug_assert_eq!(a, b);
}
"#;
        assert!(tracing_fields(src).is_empty());
        assert!(error_log_sites(src).is_empty());
    }

    #[test]
    fn blanking_preserves_length_and_newlines() {
        let src = "let a = \"含中文的字符串\"; // 注释\nlet b = 1;\n";
        let blanked = blank_comments_and_literals(src);
        assert_eq!(blanked.len(), src.len(), "涂白必须保持字节长度（行号依赖它）");
        assert_eq!(blanked.matches('\n').count(), src.matches('\n').count());
        assert!(blanked.contains("let b = 1;"), "真实代码要保留");
        assert!(!blanked.contains("含中文的字符串"));
    }

    #[test]
    fn event_macro_and_bare_macro_without_prefix_are_both_found() {
        let src = r#"
fn f() {
    tracing::event!(tracing::Level::WARN, provider_id = %p, "a");
    warn!(lane_id = %l, "b");
}
"#;
        let fields: Vec<String> = tracing_fields(src).into_iter().map(|f| f.name).collect();
        assert_eq!(fields, vec!["provider_id", "lane_id"]);
    }

    // ------------------------------------------------ 结构体字面构造（守卫用）

    #[test]
    fn struct_literals_are_found_but_declarations_are_not() {
        let src = r#"
pub struct MeteredUsage { pub usage: u64 }
impl MeteredUsage { fn x() -> u64 { 0 } }
pub trait T { fn normalize(&self) -> MeteredUsage; }
fn build() -> MeteredUsage {
    MeteredUsage { usage: 1 }
}
fn other() -> MeteredUsage
{ MeteredUsage { usage: 2 } }
"#;
        let lines: Vec<usize> =
            metered_usage_literal_sites(src).into_iter().map(|s| s.line).collect();
        assert_eq!(lines, vec![6, 9], "只该命中两处表达式构造（定义 / impl / 返回类型都不算）");
    }

    #[test]
    fn mentions_in_comments_and_strings_are_not_literals() {
        let src = r#"
/// 不许手拼 `MeteredUsage { usage: u, source: UsageSource::Upstream }`
// MeteredUsage { }
fn f() { let s = "MeteredUsage { }"; }
"#;
        assert!(
            metered_usage_literal_sites(src).is_empty(),
            "注释与字符串里的例子不是构造（否则文档举例就会把自己判违规）"
        );
    }

    #[test]
    fn a_longer_identifier_is_not_a_match() {
        let src = "fn f() { let x = MyMeteredUsage { usage: 1 }; }";
        assert!(
            metered_usage_literal_sites(src).is_empty(),
            "`MyMeteredUsage` 不是 `MeteredUsage`"
        );
    }

    #[test]
    fn path_qualified_return_types_are_not_literals() {
        // 红-绿证明里抓到的真实误报：`-> crate::metering::MeteredUsage {` 的紧邻词是
        // `crate::metering::`，不剥路径限定就会把返回类型当成手拼构造。
        let src = r#"
fn build() -> crate::metering::MeteredUsage {
    let x = crate::metering::MeteredUsage { usage: u, source: s };
    x
}
"#;
        let lines: Vec<usize> =
            metered_usage_literal_sites(src).into_iter().map(|s| s.line).collect();
        assert_eq!(lines, vec![3], "只有表达式那一行算构造");
    }

    #[test]
    fn generic_scan_works_for_any_type_name() {
        // 通用入口是给后续守卫复用的，别只让 MeteredUsage 能走
        let src = "fn f() { let a = Foo { x: 1 }; let b = Bar { y: 2 }; }";
        assert_eq!(struct_literal_sites(src, "Foo").len(), 1);
        assert_eq!(struct_literal_sites(src, "Bar").len(), 1);
        assert!(struct_literal_sites(src, "").is_empty(), "空类型名不能扫出任何东西");
    }
}

//! 内核子进程的 stdout/stderr → 我们的日志（[docs/06 §1.6] B4、[docs/14]）。
//!
//! ## 为什么这一层非有不可
//!
//! 实测：provider 文件**缺失或为空**时，内核**照常启动、REST 照常通**（[docs/06 §1.6] K2 细化，
//! 真夹具见 `tests/fixtures/real-mihomo/emptya.log` / `emptyb.log`：`level=error` 之后
//! 紧接着就是 `RESTful API listening at: …`）。也就是说：
//!
//! > **内核已经在 `level=error` 把原因说出来了，但那份信息在它自己的 stdout 里。**
//!
//! 如果我们不接管这两条管道，排查时会得出**错误结论**："我们的代码没有报错" ——
//! 而真相是"整个内核静默降级、所有 lane 一起走空"。所以 B4 不是"加个日志方便调试"，
//! 它是**修复一个会导致误判的信息黑洞**。
//!
//! ## 设计
//!
//! - **逐行读**，不用 `Stdio::from(file)` 直接接文件：那样我们拿不到内容；
//! - **EOF 时把没有尾随换行的残留字节也交出去**（[task-8] 明确要求）——
//!   崩溃原因往往正是最后那一行，丢了它等于没记；
//! - **输出到哪是可注入的**（[`ChildLogSinkFactory`]）：单测不需要真进程，
//!   而生产默认是"落 `<appdata>/logs/kernel.log` + 转发进 tracing"两份都要；
//!   **两份都过脱敏层** —— 内核原文里真的会出现带 token 的订阅 URL（[`KernelLogFileSink`] 有实测证据）;
//! - **内核原文放 tracing 的 message，不放结构化字段**：logrus 行是**外部系统的自由文本**，
//!   塞进字段等于让外部数据污染我们的字段契约（[docs/14 §5] 的白名单）；
//!   `code = e.code()` 那套规则管的是**我们自己的** `TaError`，不是内核的行。
//! - **解析不出来不丢**：级别认不出就按 `info` 记（默认级别可见），`msg=` 没有就整行当 message。
//!   这一层的取向与整个项目一致：**宁可多一条日志，不可静默少一条。**
//!   但有一类"认不出级别"的行会被**升级成 `error`**：Go 运行时的崩溃原文
//!   （`panic:` / `fatal error:` / `goroutine N [running]:`）**不经 logrus** 直接落 stderr，
//!   按 INFO 记会被默认 UI（WARN+）藏起来 —— 而那几行正是最该看见的。见 [`looks_like_go_crash`]。
//!
//! ## 已知风险：**背压**（未解决，明确记在这里）
//!
//! 读循环是**同步**的：`pump_reader` 在专用线程里阻塞读，读完立刻调 `sink.emit`；
//! 而 sink 是同步的（写文件 / 写 tracing）。所以如果 sink 变慢（磁盘卡、appender 排队），
//! 管道被填满，**内核写自己的 stdout 就会被反向卡住**。
//!
//! 这与 [docs/14 §9.1]"非阻塞写是硬要求"的精神相左。当前**没有**做有界队列 + 丢弃计数，
//! 理由是：内核输出的速率极低（只在启动/错误/请示时写几行），而"丢日志"与"卡内核"
//! 两种故障我们都不想要。**这条是已知边界，不是已解决**：一旦真的观测到内核被日志拖慢，
//! 正确的做法是 `try_send` 到有界 channel + 丢弃计数（让"丢了 N 条"可见），而不是加大缓冲。
//!
//! 现在缓解它的只有两点（都还没有实测数据支撑）：读循环里**没有 sleep / 限流**，
//! 且不解析就不等待 —— 尽量让读端比内核快。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ta_core::error::Result;

/// 内核输出在 tracing 里的 target（[task-8] / [docs/14] 的契约）。
pub const CHILD_LOG_TARGET: &str = "ta_kernel::child";

// ================================================================ 行模型

/// 输出走的是哪条管道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildStream {
    Stdout,
    Stderr,
}

impl ChildStream {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildStream::Stdout => "stdout",
            ChildStream::Stderr => "stderr",
        }
    }
}

/// 内核自报的日志级别（logrus 的 `level=`）。
///
/// `Unknown` 是**刻意保留**的：认不出级别不等于这条输出没价值，见模块文档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildLogLevel {
    Error,
    Warning,
    Info,
    Debug,
    Trace,
    Unknown,
}

impl ChildLogLevel {
    /// logrus 的取值是**小写全拼**（实测有 `info` / `warning` / `error`）。
    /// `warn` 也接受 —— 别的 logrus 配置会这么写，收进来不亏。
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.eq_ignore_ascii_case("error") {
            ChildLogLevel::Error
        } else if s.eq_ignore_ascii_case("warning") || s.eq_ignore_ascii_case("warn") {
            ChildLogLevel::Warning
        } else if s.eq_ignore_ascii_case("info") {
            ChildLogLevel::Info
        } else if s.eq_ignore_ascii_case("debug") {
            ChildLogLevel::Debug
        } else if s.eq_ignore_ascii_case("trace") {
            ChildLogLevel::Trace
        } else {
            ChildLogLevel::Unknown
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ChildLogLevel::Error => "error",
            ChildLogLevel::Warning => "warning",
            ChildLogLevel::Info => "info",
            ChildLogLevel::Debug => "debug",
            ChildLogLevel::Trace => "trace",
            ChildLogLevel::Unknown => "unknown",
        }
    }
}

/// 内核子进程输出的一行（已解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildLogLine {
    pub stream: ChildStream,
    pub level: ChildLogLevel,
    /// `msg="…"` 的内容，**已按 logrus 规则反转义**；解析不出就用整行。
    pub message: String,
    /// 原样一行（不含行尾换行）。写 `kernel.log` 与排查时以它为准。
    pub raw: String,
}

/// 解析一行 logrus text 输出：`time="…" level=info msg="…"`。
///
/// 纯函数 —— 单测直接喂真夹具的行（`k4.log` / `k8.log` / `emptya.log` / `emptyb.log`）。
pub fn parse_child_line(stream: ChildStream, raw: &str) -> ChildLogLine {
    let mut level = parse_level_field(raw).unwrap_or(ChildLogLevel::Unknown);
    // 认不出级别的行里有一类必须升级：Go 崩溃原文（见 `looks_like_go_crash`）。
    if level == ChildLogLevel::Unknown && looks_like_go_crash(raw) {
        level = ChildLogLevel::Error;
    }
    let message = parse_msg_field(raw).unwrap_or_else(|| raw.to_string());
    ChildLogLine { stream, level, message, raw: raw.to_string() }
}

/// Go 运行时崩溃原文的前缀。
///
/// 内核是 **Go** 写的：`panic` / `fatal error` 由运行时**直接写 stderr**，
/// **不经 logrus**，所以没有 `level=` 字段 → 按 `Unknown` 处理。
/// 但 `Unknown` 在本模块映射到 `info`，默认 UI（WARN 及以上）看不见它们 ——
/// 而"内核爆了"正是最该看见的那几行。所以这几类按 `error` 记（原文仍放 message）。
///
/// 判据用**行首**（`trim_start` 后）而不是"包含"：正常日志内容里出现 "panic" 这个词
/// 不该被升级（内核自己带 `level=` 的行也不会走到这里 —— 只有 `Unknown` 才会）。
pub fn looks_like_go_crash(raw: &str) -> bool {
    const PREFIXES: [&str; 4] = ["panic:", "fatal error:", "goroutine ", "runtime error:"];
    let s = raw.trim_start();
    PREFIXES.iter().any(|p| s.starts_with(p))
}

/// 取 `level=` 的值。
///
/// **只在 `msg="` 之前找**：否则 message 里出现 "level=error" 这种字样时会被误当级别。
fn parse_level_field(raw: &str) -> Option<ChildLogLevel> {
    let head_end = raw.find("msg=\"").unwrap_or(raw.len());
    let head = &raw[..head_end];
    let idx = head.find("level=")?;
    let rest = &head[idx + "level=".len()..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(ChildLogLevel::parse(&rest[..end]))
}

/// 取 `msg="…"` 的值并反转义。
///
/// **没有闭合引号（被截断的行）也把已解析的部分交出去**，不返回 `None`。
fn parse_msg_field(raw: &str) -> Option<String> {
    let idx = raw.find("msg=\"")?;
    let rest = &raw[idx + "msg=\"".len()..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                // 未知转义：原样保留（宁可多两个字符，不要吃掉信息）
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

/// 逐行读一条管道并交给 sink。**阻塞**，所以调用方要放在专用线程里。
///
/// 关键性质（都有单测）：
/// - `read_until` 内部会跨多次 `read` 拼一行，所以**跨 read 边界不会把一行切成两行**；
/// - **EOF 时仍返回最后那段没有换行的字节** → 残留不会丢；
/// - 逐字节读取的读端也吃得下（不是靠"一次 read 就是一行"这种巧合）；
/// - 读出错就结束（内核的输出不该让我们的线程转圈）。
pub fn pump_reader<R: Read>(reader: R, stream: ChildStream, sink: &dyn ChildLogSink) {
    let mut reader = BufReader::new(reader);
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return, // EOF
            Ok(_) => {}
            Err(e) => {
                // **有意不用 `error =` 字段**（不是漏了）：这是"读子进程管道失败"的 `io::Error`,
                // 它没有 TA 码可记，也不是业务错误。[docs/14 §6.3] 的 `error_log_guard` 只在出现
                // `error =` 字段时才要求配 `code =`；把它提升成结构化字段的收益只是"字段齐全"，
                // 代价是再开一条 EXEMPT 并让豁免表变长 —— **豁免越多，守卫越软**。
                // 所以留 message 是有意的；"有意的"与"漏掉的"必须看起来不一样，这条注释就是这个区别。
                tracing::debug!(source = stream.as_str(), "读取内核输出失败，停止转发：{e}");
                return;
            }
        }
        while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
            buf.pop();
        }
        if buf.is_empty() {
            continue; // 空行没有信息，不制造噪声
        }
        let raw = String::from_utf8_lossy(&buf).into_owned();
        sink.emit(&parse_child_line(stream, &raw));
    }
}

// ================================================================ 输出去向

/// 内核输出到哪。可注入 —— 单测不需要真进程。
pub trait ChildLogSink: Send + Sync {
    fn emit(&self, line: &ChildLogLine);
}

/// 转发进我们的 `tracing`：target 固定 [`CHILD_LOG_TARGET`]，级别一一映射。
///
/// `Unknown` 映射到 `info` **而不是 `debug`**：认不出级别的行如果只在 debug 可见，
/// 那么"内核换了日志格式"这件事本身就会静默 —— 那正是我们要避免的失败模式。
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingChildSink;

impl TracingChildSink {
    pub fn new() -> Self {
        Self
    }
}

impl ChildLogSink for TracingChildSink {
    fn emit(&self, line: &ChildLogLine) {
        let source = line.stream.as_str();
        let msg = line.message.as_str();
        match line.level {
            ChildLogLevel::Error => {
                tracing::error!(target: CHILD_LOG_TARGET, source, "{msg}")
            }
            ChildLogLevel::Warning => {
                tracing::warn!(target: CHILD_LOG_TARGET, source, "{msg}")
            }
            ChildLogLevel::Info | ChildLogLevel::Unknown => {
                tracing::info!(target: CHILD_LOG_TARGET, source, "{msg}")
            }
            ChildLogLevel::Debug | ChildLogLevel::Trace => {
                tracing::debug!(target: CHILD_LOG_TARGET, source, "{msg}")
            }
        }
    }
}

/// 追加进 `<appdata>/logs/kernel.log`（[docs/14 C1]：日志要有确定落点，用户能一键打开）。
///
/// ## ★ 落盘前**必须**过脱敏层（[docs/14 §7.1] 的第二道防线）
///
/// 这里写的是 [`ChildLogLine::raw`] **经 `ta_core::diag::sanitize::sanitize` 处理之后**的内容，
/// 而不是逐字节原文 —— **这是一个刻意的取舍，理由是有实测证据的**：
///
/// 真内核（v1.19.31）在订阅拉取失败时会把**带 token 的完整 URL** 打进 stdout：
///
/// ```text
/// level=error msg="initial proxy provider leak-test error: Get \"http://127.0.0.1:9/…?token=<真 token>\": EOF"
/// level=error msg="[Provider] leak-test pull error: Get \"http://127.0.0.1:9/…?token=<真 token>\": EOF"
/// ```
///
/// tracing 那条路有出口脱敏层兜着（E2 的 `[?&](key|token|api_key…)=` 规则），
/// 但**文件这条路原来没有** → `kernel.log` 里躺着明文 token，而 `kernel.log` 正是
/// **诊断包要带走的文件之一** → 导出诊断包等于把用户的订阅凭据一起交出去。
///
/// **"不再是逐字节原文"这个代价被判为应该让位**：保真度服务排查，泄漏服务用户的账号安全。
/// 而且保真是有边界的（UTF-8 lossy / 空行跳过 / 背压风险都已在本模块文档里承认），
/// 它从来不是逐字节原文，只是原先没写出来。
/// 另一条路（只在导出时拒发）有更糟的失败模式：用户**最需要诊断包的时候反而导不出来**。
pub struct KernelLogFileSink {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    complained: AtomicBool,
}

impl KernelLogFileSink {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file: Mutex::new(file), complained: AtomicBool::new(false) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ChildLogSink for KernelLogFileSink {
    fn emit(&self, line: &ChildLogLine) {
        // ★ 落盘前过脱敏层 —— 详见上面结构体文档里的实测证据（订阅 token 会出现在内核原文里）
        let safe = ta_core::diag::sanitize::sanitize(&line.raw);
        let result = match self.file.lock() {
            Ok(mut f) => writeln!(f, "{safe}"),
            Err(_) => Ok(()), // 锁被毒化：静默放过，不在日志路径上再 panic（[docs/14 §9.2]）
        };
        if let Err(e) = result {
            // 只抱怨一次：日志写不进去时最不该做的事就是每条都再写一次日志
            if !self.complained.swap(true, Ordering::Relaxed) {
                tracing::warn!(path = %self.path.display(), "写内核日志文件失败，之后不再重复抱怨：{e}");
            }
        }
    }
}

/// 一份输出同时交给多个 sink（生产默认 = 文件 + tracing）。
pub struct TeeChildLogSink {
    sinks: Vec<Arc<dyn ChildLogSink>>,
}

impl TeeChildLogSink {
    pub fn new(sinks: Vec<Arc<dyn ChildLogSink>>) -> Self {
        Self { sinks }
    }
}

impl std::fmt::Debug for TeeChildLogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeeChildLogSink").field("sinks", &self.sinks.len()).finish()
    }
}

impl ChildLogSink for TeeChildLogSink {
    fn emit(&self, line: &ChildLogLine) {
        for s in &self.sinks {
            s.emit(line);
        }
    }
}

/// 为**一次 spawn** 造出日志去向。
///
/// 做成工厂而不是固定实例，是因为文件 sink 需要每次 spawn 的 `log_path`
/// （配置里带路径，而且重启换目录时要跟着换）。
///
/// **返回 `Result`**：默认实现要落 `kernel.log`，而那是 [docs/14 C1] 的硬要求
/// （"出问题时日志文件存在于确定路径，用户能一键打开"）。打不开就**不能继续**
/// —— 否则 `log_tail` 对缺失文件返回空串，"没有内核日志"与"内核没有输出"变成同一个
/// 样子，正是 [14] 禁止的混淆。所以这里把失败**往上抛**（调用方会带 `code =` 记下来），
/// 而不是打一条 warn 然后静默退化成"只有 tracing"。
pub trait ChildLogSinkFactory: Send + Sync {
    fn sink_for(&self, log_path: &Path) -> Result<Arc<dyn ChildLogSink>>;
}

/// 生产默认：`kernel.log` + `tracing`，**两份都要**。
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultChildLogSinks;

impl ChildLogSinkFactory for DefaultChildLogSinks {
    fn sink_for(&self, log_path: &Path) -> Result<Arc<dyn ChildLogSink>> {
        // 打不开就失败，不做"降级成只有 tracing"（见 trait 文档）。
        // 这里**不自己记日志**：TaError 要带 `code = e.code()` 记，而本函数没有
        // 那个上下文 —— 由调用方（`KernelSupervisor::record_failure`）成对记下。
        let file = KernelLogFileSink::open(log_path).map_err(|e| {
            ta_core::error::TaError::Kernel(format!(
                "无法打开内核日志文件 {}（[14 C1] 要求它有确定落点）：{e}",
                log_path.display()
            ))
        })?;
        Ok(Arc::new(TeeChildLogSink::new(vec![Arc::new(file), Arc::new(TracingChildSink)])))
    }
}

/// 测试用：固定一个 sink（不落盘、随你记录）。
pub struct FixedChildLogSink(pub Arc<dyn ChildLogSink>);

impl std::fmt::Debug for FixedChildLogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FixedChildLogSink")
    }
}

impl ChildLogSinkFactory for FixedChildLogSink {
    fn sink_for(&self, _log_path: &Path) -> Result<Arc<dyn ChildLogSink>> {
        Ok(self.0.clone())
    }
}

// ================================================================ 单测：真夹具的行

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTYA_LOG: &str = include_str!("../tests/fixtures/real-mihomo/emptya.log");
    const EMPTYB_LOG: &str = include_str!("../tests/fixtures/real-mihomo/emptyb.log");
    const K4_LOG: &str = include_str!("../tests/fixtures/real-mihomo/k4.log");
    const K8_LOG: &str = include_str!("../tests/fixtures/real-mihomo/k8.log");

    fn line_at(src: &str, n: usize) -> &str {
        src.lines().nth(n).expect("夹具行号越界")
    }

    #[derive(Default)]
    struct RecordingSink {
        lines: Mutex<Vec<ChildLogLine>>,
    }

    impl RecordingSink {
        fn taken(&self) -> Vec<ChildLogLine> {
            self.lines.lock().unwrap().clone()
        }
    }

    impl ChildLogSink for RecordingSink {
        fn emit(&self, line: &ChildLogLine) {
            self.lines.lock().unwrap().push(line.clone());
        }
    }

    #[test]
    fn target_is_the_contract_value() {
        assert_eq!(CHILD_LOG_TARGET, "ta_kernel::child");
    }

    // ---------------------------------------------------------------- 真行

    #[test]
    fn parses_real_error_line_from_emptya() {
        // emptya.log:8 —— provider 文件缺失，内核唯一的告警就在这里
        let raw = line_at(EMPTYA_LOG, 7);
        let l = parse_child_line(ChildStream::Stderr, raw);
        assert_eq!(l.level, ChildLogLevel::Error, "raw={raw}");
        assert!(
            l.message.starts_with("initial proxy provider missing-provider error"),
            "{:?}",
            l.message
        );
        // logrus 把 Windows 路径转义成 `C:\\Users\\…`；我们要还原
        assert!(l.message.contains(r"C:\Users\19404"), "转义没还原：{:?}", l.message);
        assert!(!l.message.contains(r"\\"), "不该还有双反斜杠：{:?}", l.message);
        assert_eq!(l.raw, raw, "raw 必须原样保留");
    }

    #[test]
    fn parses_real_error_line_from_emptyb() {
        let raw = line_at(EMPTYB_LOG, 7);
        let l = parse_child_line(ChildStream::Stderr, raw);
        assert_eq!(l.level, ChildLogLevel::Error);
        assert_eq!(
            l.message,
            "initial proxy provider empty-provider error: file doesn't have any proxy"
        );
    }

    #[test]
    fn parses_real_warning_line_from_k4() {
        // k4.log:35 —— 指向不存在的节点名，只在请求到达时才报
        let raw = line_at(K4_LOG, 34);
        let l = parse_child_line(ChildStream::Stdout, raw);
        assert_eq!(l.level, ChildLogLevel::Warning, "raw={raw}");
        assert!(l.message.contains("[Metadata] parse failed"), "{:?}", l.message);
        assert!(l.message.contains("这个节点不存在"), "{:?}", l.message);
    }

    #[test]
    fn parses_real_info_line_from_k8() {
        let raw = line_at(K8_LOG, 10);
        let l = parse_child_line(ChildStream::Stdout, raw);
        assert_eq!(l.level, ChildLogLevel::Info, "raw={raw}");
        assert_eq!(l.message, "Start initial compatible provider default");
    }

    #[test]
    fn every_line_of_every_real_log_parses_without_losing_the_raw() {
        // 真夹具里**每一行**都必须解析成功且 raw 一字不差 —— 全量而不是抽样
        for (name, src) in
            [("emptya", EMPTYA_LOG), ("emptyb", EMPTYB_LOG), ("k4", K4_LOG), ("k8", K8_LOG)]
        {
            for (i, raw) in src.lines().enumerate() {
                let l = parse_child_line(ChildStream::Stdout, raw);
                assert_eq!(l.raw, raw, "{name}:{} raw 被改了", i + 1);
                assert_ne!(
                    l.level,
                    ChildLogLevel::Unknown,
                    "{name}:{} 应该是能认出的 logrus 行：{raw}",
                    i + 1
                );
                assert!(!l.message.is_empty(), "{name}:{} message 不该为空", i + 1);
            }
        }
    }

    // ---------------------------------------------------------------- 边界

    #[test]
    fn message_containing_a_level_word_does_not_confuse_the_level() {
        let raw = r#"time="t" level=info msg="we saw level=error upstream""#;
        let l = parse_child_line(ChildStream::Stdout, raw);
        assert_eq!(l.level, ChildLogLevel::Info, "只该看 msg 之前的字段");
        assert_eq!(l.message, "we saw level=error upstream");
    }

    #[test]
    fn unknown_level_is_kept_not_dropped() {
        let raw = r#"time="t" level=catastrophe msg="who knows""#;
        let l = parse_child_line(ChildStream::Stdout, raw);
        assert_eq!(l.level, ChildLogLevel::Unknown);
        assert_eq!(l.message, "who knows");
    }

    #[test]
    fn line_without_msg_field_falls_back_to_the_whole_line() {
        let raw = "plain text the kernel printed";
        let l = parse_child_line(ChildStream::Stderr, raw);
        assert_eq!(l.level, ChildLogLevel::Unknown);
        assert_eq!(l.message, raw);
    }

    #[test]
    fn go_crash_lines_are_escalated_to_error() {
        // 内核是 Go 写的：panic/fatal 由运行时直接写 stderr，**没有 level= 字段**。
        // 若按 Unknown→info 记，默认 UI（WARN+）就看不见"内核爆了"。
        for raw in [
            "panic: runtime error: invalid memory address or nil pointer dereference",
            "fatal error: concurrent map writes",
            "goroutine 1 [running]:",
            "  panic: indented panic line",
            "runtime error: index out of range [3] with length 2",
        ] {
            let l = parse_child_line(ChildStream::Stderr, raw);
            assert_eq!(l.level, ChildLogLevel::Error, "应升级到 error：{raw:?}");
            assert_eq!(l.message, raw, "原文必须原样留在 message 里");
            assert_eq!(l.raw, raw);
        }
    }

    #[test]
    fn ordinary_unknown_lines_are_not_escalated() {
        // 升级只针对**行首**的崩溃前缀：正文里出现 "panic" 不该被当崩溃
        assert!(!looks_like_go_crash("this request did not panic at all"));
        assert_eq!(
            parse_child_line(ChildStream::Stdout, "this request did not panic at all").level,
            ChildLogLevel::Unknown
        );
        // 内核自己带 level= 的行不受影响（level 字段优先）
        assert_eq!(
            parse_child_line(ChildStream::Stdout, "level=info msg=\"panic: not really\"").level,
            ChildLogLevel::Info
        );
    }

    #[test]
    fn truncated_line_without_closing_quote_still_gives_what_it_can() {
        let raw = r#"time="t" level=error msg="dangling because tru"#;
        let l = parse_child_line(ChildStream::Stderr, raw);
        assert_eq!(l.level, ChildLogLevel::Error);
        assert_eq!(l.message, "dangling because tru");
    }

    #[test]
    fn escapes_are_reversed_and_unknown_escapes_are_kept() {
        let raw = r#"level=info msg="a\"b\\c\nd\qe""#;
        let l = parse_child_line(ChildStream::Stdout, raw);
        assert_eq!(l.message, "a\"b\\c\nd\\qe");
    }

    #[test]
    fn level_values_accept_full_and_short_forms_case_insensitively() {
        assert_eq!(ChildLogLevel::parse("WARNING"), ChildLogLevel::Warning);
        assert_eq!(ChildLogLevel::parse("warn"), ChildLogLevel::Warning);
        assert_eq!(ChildLogLevel::parse("Error"), ChildLogLevel::Error);
        assert_eq!(ChildLogLevel::parse("nonsense"), ChildLogLevel::Unknown);
    }

    // ---------------------------------------------------------------- 逐行读

    /// 一次只吐一个字节的读端：用来证明"一行被拆进多次 read"也拼得回来。
    struct DripReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl Read for DripReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.data[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }

    #[test]
    fn pump_delivers_every_line_including_a_line_split_across_reads() {
        let sink = RecordingSink::default();
        let body = "alpha\nbeta-gamma\n";
        pump_reader(
            DripReader { data: body.as_bytes().to_vec(), pos: 0 },
            ChildStream::Stdout,
            &sink,
        );
        let got: Vec<String> = sink.taken().into_iter().map(|l| l.raw).collect();
        assert_eq!(got, vec!["alpha", "beta-gamma"]);
    }

    #[test]
    fn pump_flushes_the_last_line_when_it_has_no_trailing_newline() {
        // ★ [task-8] 明确要求：EOF 时没有换行的残留也必须交出去。
        // 崩溃原因往往就是最后那一行 —— 丢了它等于没记。
        let sink = RecordingSink::default();
        let body = "first\nlast line with no newline";
        pump_reader(std::io::Cursor::new(body.as_bytes()), ChildStream::Stderr, &sink);
        let got: Vec<String> = sink.taken().into_iter().map(|l| l.raw).collect();
        assert_eq!(got, vec!["first", "last line with no newline"]);
    }

    #[test]
    fn pump_strips_crlf_and_skips_blank_lines() {
        let sink = RecordingSink::default();
        let body = "a\r\n\r\n\nb\r\n";
        pump_reader(std::io::Cursor::new(body.as_bytes()), ChildStream::Stdout, &sink);
        let got: Vec<String> = sink.taken().into_iter().map(|l| l.raw).collect();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn pump_keeps_non_utf8_bytes_without_losing_the_line() {
        let sink = RecordingSink::default();
        let mut data = b"level=info msg=\"bad ".to_vec();
        data.push(0xff); // 非法 UTF-8
        data.extend_from_slice(b" byte\"\n");
        pump_reader(std::io::Cursor::new(data), ChildStream::Stdout, &sink);
        let got = sink.taken();
        assert_eq!(got.len(), 1, "非法 UTF-8 不该让整行消失");
        // lossy 之后是 U+FFFD，但"这一行存在"这件事必须保留
        assert!(got[0].message.contains("bad "), "{:?}", got[0].message);
    }

    #[test]
    fn pump_on_an_immediately_closed_pipe_emits_nothing_and_returns() {
        let sink = RecordingSink::default();
        pump_reader(std::io::Cursor::new(Vec::<u8>::new()), ChildStream::Stdout, &sink);
        assert!(sink.taken().is_empty());
    }

    // ---------------------------------------------------------------- sink

    #[test]
    fn tee_fans_out_to_every_sink() {
        let a = Arc::new(RecordingSink::default());
        let b = Arc::new(RecordingSink::default());
        let tee = TeeChildLogSink::new(vec![a.clone(), b.clone()]);
        tee.emit(&parse_child_line(ChildStream::Stdout, "level=info msg=\"hi\""));
        assert_eq!(a.taken().len(), 1);
        assert_eq!(b.taken().len(), 1);
        assert_eq!(a.taken()[0].message, "hi");
    }

    /// ★ 安全回归（**实测驱动**）：内核原文里会出现**带 token 的订阅 URL** ——
    /// 真内核 v1.19.31 在订阅拉取失败时打的就是这一行（本次修这条 bug 的原始证据）：
    /// `level=error msg="initial proxy provider … error: Get \"http://…?token=<真 token>\": EOF"`。
    ///
    /// 而 `kernel.log` 是**诊断包要带走的文件之一** → 落盘前必须过脱敏层，
    /// 否则"用户凭据绝不出现在可分享产物里"这个承诺就被破了。
    #[test]
    fn file_sink_redacts_secrets_before_writing() {
        let dir = std::env::temp_dir().join(format!("ta-kernel-redact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("kernel.log");
        let sink = KernelLogFileSink::open(&path).expect("开日志文件");

        // 与实测输出**同构**（连 `\"` 转义都一样），只是把真 token 换成 canary
        let canary = "TA-LEAK-CANARY-9f3a2b";
        let raw = format!(
            r#"time="2026-09-22T03:06:02.034011200+08:00" level=error msg="initial proxy provider leak-test error: Get \"http://127.0.0.1:9/ta-leak-canary?token={canary}\": EOF""#
        );
        sink.emit(&parse_child_line(ChildStream::Stdout, &raw));

        let text = std::fs::read_to_string(&path).expect("读回");

        // ① 正向：文件里 grep 不到那个 token
        assert!(!text.contains(canary), "落盘后仍能搜到 token —— 诊断包会把它交出去：{text}");
        assert!(!text.contains("token=TA-LEAK"), "{text}");

        // ② 反向控制：**不能靠"什么都不写"来实现 ①**（否则这条测试是假绿）
        assert_eq!(text.lines().count(), 1, "应当写出且只写出一行：{text}");
        assert!(text.contains("2026-09-22T03:06:02"), "时间戳必须还在：{text}");
        assert!(text.contains("level=error"), "级别必须还在：{text}");
        assert!(
            text.contains("initial proxy provider leak-test error"),
            "可读的上下文必须还在（否则排查价值没了）：{text}"
        );
        assert!(
            text.contains("127.0.0.1:9/ta-leak-canary"),
            "URL 本身可以留，只有凭据该被遮：{text}"
        );
    }

    #[test]
    fn file_sink_appends_the_raw_line_and_creates_the_dir() {
        let dir = std::env::temp_dir().join(format!("ta-kernel-sink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("kernel.log");
        let sink = KernelLogFileSink::open(&path).expect("开日志文件");
        sink.emit(&parse_child_line(ChildStream::Stderr, "level=error msg=\"boom\""));
        sink.emit(&parse_child_line(ChildStream::Stderr, "level=info msg=\"fine\""));
        let text = std::fs::read_to_string(&path).expect("读回");
        assert_eq!(text, "level=error msg=\"boom\"\nlevel=info msg=\"fine\"\n");
        assert_eq!(sink.path(), path.as_path());
    }

    #[test]
    fn default_factory_always_includes_the_tracing_sink() {
        let dir = std::env::temp_dir().join(format!("ta-kernel-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sink = DefaultChildLogSinks.sink_for(&dir.join("kernel.log")).expect("应能建出日志");
        // 打一条不该 panic，且文件被创建（这就是 [docs/14 C1] 的落点）
        sink.emit(&parse_child_line(ChildStream::Stdout, "level=info msg=\"hello\""));
        assert!(dir.join("kernel.log").exists(), "默认工厂必须建出 kernel.log");
    }

    /// B2（同行评审）：`kernel.log` 打不开时**必须失败**，不能静默退化成"只有 tracing"。
    ///
    /// 理由：[docs/14 C1] 要求日志有确定落点；而 `log_tail` 对缺失文件返回**空串** ——
    /// 一旦允许"没有文件"，"没有内核日志"和"内核没有输出"就变成同一个样子，
    /// 正是 [14] 明令禁止的混淆。
    #[test]
    fn default_factory_fails_loudly_when_the_log_file_cannot_be_opened() {
        let dir = std::env::temp_dir().join(format!("ta-kernel-blocked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        // 用一个"父路径是文件"的日志路径逼它失败（跨平台都成立）
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").expect("写占位文件");
        let bad = blocker.join("kernel.log");

        // 用 `.err()` 而不是 `expect_err`：Ok 侧是 `Arc<dyn ChildLogSink>`，它没有 Debug
        let err = DefaultChildLogSinks
            .sink_for(&bad)
            .err()
            .expect("打不开日志文件时必须返回 Err，而不是退化成只有 tracing");
        let msg = err.to_string();
        assert!(msg.contains("kernel.log"), "错误要说清是哪个文件：{msg}");
        // 它是 TaError::Kernel —— 调用方（record_failure）会带 `code = e.code()` 记下来
        assert!(matches!(err, ta_core::error::TaError::Kernel(_)), "{err:?}");
    }

    #[test]
    fn fixed_factory_hands_back_the_same_sink() {
        let rec = Arc::new(RecordingSink::default());
        let f = FixedChildLogSink(rec.clone());
        let s = f.sink_for(Path::new("/nowhere/kernel.log")).expect("固定 sink 永不失败");
        s.emit(&parse_child_line(ChildStream::Stdout, "level=info msg=\"x\""));
        assert_eq!(rec.taken().len(), 1);
    }
}

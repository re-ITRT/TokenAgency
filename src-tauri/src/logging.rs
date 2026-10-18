//! 日志落盘（确定路径、§7.2 写入时脱敏、§9.1 非阻塞写）。
//!
//! **为什么需要这个模块**（第 11 轮补的缺口）：
//! `init_tracing()` 原来**只有 stdout 一层** —— 而这是个**托盘应用：没有控制台**。
//! 于是用户出问题时**根本没有日志文件可看、可导出**， 的 C1
//! （"日志文件存在于确定路径，用户能一键打开"）事实上**没有实现**。
//! 更糟的是它不会报错：日志"打出来了"（进了一个没人看的 stdout），
//! 而诊断里最需要的那些行**从来没有落到盘上**。
//!
//! ## 两条硬约束
//!
//! 1. **必须非阻塞**：文件 IO 绝不允许发生在请求路径上 ——
//!    日志写一旦卡住，请求就跟着卡。所以走 `tracing_appender::non_blocking`
//!    的专用写线程 + 有界队列，而不是让调用线程直接写文件。
//! 2. **写入时就必须已脱敏**：这个文件是**诊断包要带走的**，
//!    所以脱敏不能等到导出时才做（那是"有 token 就拒绝导出"——用户**最需要
//!    诊断包时反而导不出来**）。这里在**写入路径**上过 `diag::sanitize`。
//!
//! > 与 `kernel.log` 同一条规矩：**文件落点没有"逐字节原文"的豁免。**
//! > 代价是日志行可能被改写（`?token=***`），收益是它**可以放心地被分享**。

use std::io::{self, Write};
use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;

/// 日志文件名前缀。实际文件形如 `tokenagency.log.2026-09-22`（按日轮转）。
pub const FILE_PREFIX: &str = "tokenagency.log";

/// 把我们自己的日志目录**定死在确定路径**（C1 的"确定"就是这个意思）。
///
/// 与 `state.rs::AppState::default_db_path()` 同一个根（`state::AppState::app_data_root()`）：
/// `<AppData>/TokenAgency/logs`，或 `TA_DATA_DIR` 覆盖时 `<TA_DATA_DIR>/logs`。
/// 与 `commands::app` 的 `"logs"` 分支、`boot.rs` 的内核日志目录**一致** ——
/// 目录解析只能有一个来源，否则"一键打开日志目录"会打开一个空目录。
pub fn log_dir() -> Option<std::path::PathBuf> {
    crate::state::AppState::app_data_root().ok().map(|root| root.join("logs"))
}

/// 把写入也过一遍脱敏层。
///
/// `W` 是内层 writer（生产是 `non_blocking` 的句柄；测试里是内存缓冲）。
#[derive(Clone)]
pub struct Sanitizing<W> {
    inner: W,
}

impl<W: Write + Clone> Sanitizing<W> {
    pub const fn new(inner: W) -> Self {
        Self { inner }
    }
}

/// 每一次 `write` 都被改写的 writer。
pub struct SanitizingWriter<W> {
    inner: W,
}

impl<W: Write> Write for SanitizingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let clean = ta_core::diag::sanitize::sanitize(&text);
        self.inner.write_all(clean.as_bytes())?;
        // ⚠️ 报告"消费了全部输入"，即使输出被改写。否则调用方会以为没写完而重试，
        // 结果是同一行被写两遍（而其中一遍可能是脱敏前的形态）。
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, W: Write + Clone> MakeWriter<'a> for Sanitizing<W> {
    type Writer = SanitizingWriter<W>;

    fn make_writer(&'a self) -> Self::Writer {
        SanitizingWriter { inner: self.inner.clone() }
    }
}

/// 生产用的文件 writer：按日轮转 + 非阻塞 + 写入时脱敏。
///
/// 返回 `None` 表示"落不了盘"（拿不到数据目录、建不了目录、**建不了文件**）——
/// 调用方**降级到只有 stdout**，但**绝不 panic**（日志系统本身不该拖垮应用启动）。
///
/// 这里**必须用 builder**：`tracing_appender::rolling::daily(...)` 是便捷构造，
/// 内部就是 `.build(...).expect(...)` —— **失败即 panic**。
/// 曾经正是它把"日志文件建不了"升级成"应用起不来"：真机上双击后**闪退**，
/// 而且 GUI 子系统没有控制台，用户**什么提示都看不到**（2026-10-14 真机复现）。
pub fn file_writer() -> Option<Sanitizing<tracing_appender::non_blocking::NonBlocking>> {
    file_writer_in(&log_dir()?)
}

/// [`file_writer`] 的内核：目录由参数传入，测试可以喂一个**必然失败**的路径。
fn file_writer_in(dir: &Path) -> Option<Sanitizing<tracing_appender::non_blocking::NonBlocking>> {
    match appender_in(dir) {
        Ok(writer) => Some(writer),
        Err(e) => {
            // 主目录落不了盘：**先大声说**，再试备用目录。
            //
            // 真机上出现过"数据目录里其它文件都能写（库能写），偏偏日志建不了"的情况
            // （同一个文件我用 PowerShell 能建、应用建不了）。根因还没锁定，
            // 但"完全没有日志"会把下一步排查也毁掉 —— 所以宁可写到一个次选位置，
            // 也要留下痕迹，并把偏差说清楚。
            let fallback = dir.with_file_name("logs-fallback");
            eprintln!(
                "TokenAgency: 日志目录 {} 不可用（{e}）；改写到 {}",
                dir.display(),
                fallback.display()
            );
            match appender_in(&fallback) {
                Ok(writer) => Some(writer),
                Err(e2) => {
                    eprintln!(
                        "TokenAgency: 备用日志目录 {} 也不可用（{e2}）；降级为只写 stdout",
                        fallback.display()
                    );
                    None
                }
            }
        }
    }
}

/// 在指定目录建按日轮转的 appender。用 **builder**（返回 `Result`）而不是
/// `rolling::daily(...)`（内部 `.expect(...)`）—— 后者失败即 panic，
/// 会把"日志落不了盘"升级成"应用起不来"。
fn appender_in(
    dir: &Path,
) -> Result<Sanitizing<tracing_appender::non_blocking::NonBlocking>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建不了目录：{e}"))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .build(dir)
        .map_err(|e| format!("建不了文件：{e}"))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    // guard 决定后台写线程的生命周期：**进程活着期间必须不 drop**，
    // 否则日志写线程会被关掉、之后的日志静默丢失（比"没有日志"更难查）。
    std::mem::forget(guard);
    Ok(Sanitizing::new(writer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 落不了盘 ⇒ 返回 `None`，**绝不 panic**。
    ///
    /// 这条测试对着一个真实缺陷：这里原来用 `tracing_appender::rolling::daily(...)`
    /// （内部就是 `.build(...).expect(...)`），于是"日志文件建不了"会把**整个应用**
    /// panic 掉 —— 真机表现是**双击后闪退、屏幕上什么提示都没有**（GUI 子系统没有控制台）。
    /// 主目录不可写 ⇒ 退到 `<同级>/logs-fallback`，**不是**直接放弃。
    #[test]
    fn unwritable_primary_dir_falls_back_instead_of_giving_up() {
        let base = std::env::temp_dir().join(format!("ta-log-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("建临时根");
        // 主目录的位置先占成一个**文件**：`create_dir_all` 必然失败
        let primary = base.join("logs");
        std::fs::write(&primary, b"x").expect("占位文件");
        let got = file_writer_in(&primary);
        let fallback = base.join("logs-fallback");
        let exists = fallback.is_dir();
        let _ = std::fs::remove_dir_all(&base);
        assert!(got.is_some(), "主目录不可写时必须退到备用目录，而不是没有日志");
        assert!(exists, "备用目录应当被真的建出来");
    }

    /// **主目录与备用目录都不可写** ⇒ 才返回 `None`（并只写 stdout）。
    ///
    /// 这里把"根"本身做成一个文件：`<根>/logs` 与 `<根>/logs-fallback` 都落在文件下面，
    /// `create_dir_all` 必然失败 —— 这才是"真的落不了盘"。
    #[test]
    fn unwritable_dir_degrades_instead_of_panicking() {
        let base = std::env::temp_dir().join(format!("ta-log-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::write(&base, b"x").expect("把根占成文件");
        let got = file_writer_in(&base.join("logs"));
        let _ = std::fs::remove_file(&base);
        assert!(
            got.is_none(),
            "主目录与备用目录都不可写时，必须返回 None（降级到 stdout），不能 panic"
        );
    }

    /// 一个只存在内存里的 writer，用来验证"写入时脱敏"这件事本身。
    #[derive(Clone, Default)]
    struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 脱敏真的发生在**写入路径**上 —— 不是"导出时才做"。
    #[test]
    fn secrets_are_redacted_before_hitting_the_sink() {
        let sink = SharedBuf::default();
        let mut w = Sanitizing::new(sink.clone()).make_writer();
        w.write_all(b"GET /api?token=sk-live-abcdefghijklmnopqrstuvwxyz HTTP/1.1\n").unwrap(); // ta-allow-fake-secret
        w.flush().unwrap();

        let out = sink.text();
        assert!(
            !out.contains("sk-live-abcdefghijklmnopqrstuvwxyz"), // ta-allow-fake-secret
            "密钥不该进落点：{out}"
        );
        // 反向控制：不许用"什么都不写"来通过上面那条 —— 上下文必须还在
        assert!(out.contains("GET /api?token="), "行不能被整条吃掉：{out}");
        assert!(out.contains("HTTP/1.1"), "行尾上下文必须保留：{out}");
    }

    /// 写入必须报告"消费了全部输入"，否则 tracing 会重试并导致重复行。
    #[test]
    fn write_reports_full_consumption_even_when_rewritten() {
        // ⚠️ 这里**不能写死长度**：第一版我写的是 `assert_eq!(n, 42)` 而输入其实是 40 字节 ——
        // 那正是我反复criticize 的"断言抄了一个实现细节"。长度必须由输入推导。
        let input = b"token=sk-live-abcdefghijklmnopqrstuvwxyz"; // ta-allow-fake-secret
        let sink = SharedBuf::default();
        let mut w = Sanitizing::new(sink.clone()).make_writer();
        let n = w.write(input).unwrap();
        assert_eq!(n, input.len(), "必须报告消费了全部输入");
    }

    /// 日志目录必须是确定路径，且与库路径同一个根（`<AppData>/TokenAgency/`）。
    #[test]
    fn log_dir_is_deterministic_and_under_the_app_data_root() {
        let dir = log_dir().expect("测试机应当有数据目录");
        assert!(dir.ends_with(std::path::Path::new("TokenAgency").join("logs")), "{dir:?}");
    }
}

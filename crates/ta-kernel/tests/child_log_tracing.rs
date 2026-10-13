//! 证据测试（B4）：**内核子进程的 stdout/stderr 必须进我们的 tracing**。
//!
//! ## 为什么这条测试值得存在
//!
//! 实测（K2 细化、本目录 `emptya.log` / `emptyb.log`）：
//! provider 文件**缺失或为空**时，内核**照常启动、REST 照常通**，只在**它自己的**
//! `level=error` 里说了一句 `initial proxy provider … error: file doesn't have any proxy`
//! （`emptyb.log:8`），紧接着 `emptyb.log:9` 就是 `RESTful API listening at: …`。
//!
//! 改动前 `SystemSpawner::spawn` 把子进程的 stdout/stderr 直接 `Stdio::from(file)` 落到
//! `kernel.log` —— **一行都不进 tracing**。后果不是"少了一条日志"，而是排查时会得出
//! **错误结论**：我们的代码确实没报错，于是"内核为什么静默降级"这条线索彻底消失。
//!
//! ## 为什么放在 `tests/` 而不是 `src/` 的单测里
//!
//! `tracing` 的全局 subscriber **一个进程只能设一次**。放在 `src/` 的单测里会和同进程的
//! 其它测试抢 global default（并行跑），互相污染。独立测试目标 = 独立进程 = 干净。
//!
//! ## 这个测试用的是**真子进程 + 真 logrus 行**
//!
//! 真行直接从真夹具里取（`fixtures/real-mihomo/*.log`），不手写 —— 它同时证明
//! 两件事："我们读得到真输出" + "我们解得对"（只测 parser 是证明不了前者的）。
//! 依据：夹具是断言的**输入**。

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ta_kernel::childlog::CHILD_LOG_TARGET;
use ta_kernel::process::{KernelProcessConfig, ProcessSpawner, SystemSpawner};

/// 契约来自 [task-8] /：内核输出的 tracing target。
/// 这里**同时**持有字面量与库里的常量，并断言两者相等 ——
/// 常量被改错时这条测试会红（否则"测试与实现一起漂移"就没人发现）。
const CHILD_TARGET: &str = "ta_kernel::child";

// ================================================================ 真夹具行

const EMPTYA_LOG: &str = include_str!("fixtures/real-mihomo/emptya.log");
const K8_LOG: &str = include_str!("fixtures/real-mihomo/k8.log");

/// `emptya.log:8` —— provider 文件缺失时内核的 `level=error`
const EMPTYA_ERROR_LINE: usize = 7;
/// `k8.log:11` —— 一条普通的 `level=info`
const K8_INFO_LINE: usize = 10;

fn fixture_line(src: &str, zero_based: usize) -> String {
    src.lines()
        .nth(zero_based)
        .unwrap_or_else(|| panic!("夹具里没有第 {} 行（0 基）", zero_based))
        .to_string()
}

// ================================================================ 捕获 tracing 输出

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture 锁被毒化")).to_string()
    }

    /// 有界等待某段文字出现。管道是本机的，10s 已远超必要；
    /// 用等待而不是 sleep 固定时长，是为了在慢机器上也不 flake。
    async fn wait_for(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.text().contains(needle) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture 锁被毒化").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 装一次全局 subscriber（本测试目标是独立进程，只有一个测试用它）。
fn install_capture() -> Capture {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_target(true)
        .with_level(true)
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || CaptureWriter(writer.0.clone()))
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("本测试目标里 global subscriber 只应被设置一次");
    cap
}

// ================================================================ 真子进程

fn work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ta-kernel-childlog-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 把文件内容打到 stdout（`to_stderr = false`）或 stderr（`true`）。
/// 用真进程（`cmd /c type` / `sh -c cat`）而不是注入假读端：
/// 这样连"我们真的接管了管道"一起 prove 了。
///
/// 注意**命令与路径分成两个参数**：`cmd.exe` 不认 `\"` 这种转义，把
/// `type "path"` 整串塞进一个参数会被它解析成非法文件名。
fn printer_command(path: &std::path::Path, to_stderr: bool) -> (PathBuf, Vec<String>) {
    let p = path.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let mut args = vec!["/c".to_string(), "type".to_string(), p];
        if to_stderr {
            args.push("1>&2".to_string());
        }
        (PathBuf::from("cmd"), args)
    }
    #[cfg(not(windows))]
    {
        // `sh -c` 只把**紧邻的那一个**参数当命令串，所以这里必须是一整串（不能用分开的参数）
        let redir = if to_stderr { " 1>&2" } else { "" };
        (PathBuf::from("sh"), vec!["-c".to_string(), format!("cat '{p}'{redir}")])
    }
}

// ================================================================ 测试

#[tokio::test]
async fn child_stdout_and_stderr_reach_tracing_with_the_child_target() {
    // 契约自检：库里的常量必须就是任务要求的那个 target
    assert_eq!(CHILD_LOG_TARGET, CHILD_TARGET, "CHILD_LOG_TARGET 被改错了");

    let capture = install_capture();
    let dir = work_dir();

    // ---- stdout：一条 info，且**最后一行没有尾随换行**（覆盖 EOF 时的残留字节）
    let stdout_file = dir.join("stdout.txt");
    let info = fixture_line(K8_LOG, K8_INFO_LINE);
    std::fs::write(&stdout_file, format!("{info}\nTA-EOF-FLUSH-no-trailing-newline"))
        .expect("写 stdout 夹具");

    // ---- stderr：真 `level=error` 行（含 Windows 路径的转义反斜杠）
    let stderr_file = dir.join("stderr.txt");
    let error_line = fixture_line(EMPTYA_LOG, EMPTYA_ERROR_LINE);
    std::fs::write(&stderr_file, format!("{error_line}\n")).expect("写 stderr 夹具");

    // 前提自检：夹具确实是我们要的那两行（防止夹具被换掉后测试静默变成空转）
    assert!(error_line.contains("level=error"), "夹具行不是 error：{error_line}");
    assert!(error_line.contains("cannot find the file specified"), "夹具行不对：{error_line}");
    assert!(info.contains("level=info"), "夹具行不是 info：{info}");

    let spawner = SystemSpawner::new();

    let (program, args) = printer_command(&stdout_file, false);
    let cfg = KernelProcessConfig::new(program, dir.clone(), dir.clone(), "127.0.0.1:1", "s")
        .with_args(args);
    let stdout_pid = spawner.spawn(&cfg).await.expect("拉起 stdout 假内核");

    let (program, args) = printer_command(&stderr_file, true);
    let cfg = KernelProcessConfig::new(program, dir.clone(), dir.clone(), "127.0.0.1:1", "s")
        .with_args(args);
    let stderr_pid = spawner.spawn(&cfg).await.expect("拉起 stderr 假内核");

    // ---- 断言 1：stdout 的 info 行进来了，且级别映射成 INFO、target 是我们的
    assert!(
        capture.wait_for("Start initial compatible provider default").await,
        "stdout 的内核原文没有进 tracing。捕获到的内容：\n{}",
        capture.text()
    );
    let text = capture.text();
    let info_out = text
        .lines()
        .find(|l| l.contains("Start initial compatible provider default"))
        .expect("应有 stdout 那行");
    assert!(info_out.contains("ta_kernel::child"), "target 不对：{info_out}");
    assert!(info_out.contains("INFO"), "level=info 应映射成 INFO：{info_out}");

    // ---- 断言 2：stderr 的 error 行进来了，级别映射成 ERROR
    assert!(
        capture.wait_for("initial proxy provider missing-provider error").await,
        "stderr 的内核原文没有进 tracing。捕获到的内容：\n{}",
        capture.text()
    );
    let text = capture.text();
    let err_out = text
        .lines()
        .find(|l| l.contains("initial proxy provider missing-provider error"))
        .expect("应有 stderr 那行");
    assert!(err_out.contains("ta_kernel::child"), "target 不对：{err_out}");
    assert!(err_out.contains("ERROR"), "level=error 应映射成 ERROR：{err_out}");

    // ---- 断言 3：logrus 引号内的转义被还原（夹具原文是 `C:\\Users\\…`）
    assert!(
        err_out.contains(r"C:\Users\19404"),
        "引号内的转义没有还原，Windows 路径会解析错：{err_out}"
    );

    // ---- 断言 4：**没有尾随换行**的最后一行也被交出来了（EOF flush）
    assert!(
        capture.wait_for("TA-EOF-FLUSH-no-trailing-newline").await,
        "没有尾随换行的最后一行丢了 —— 而崩溃原因往往正是最后那一行"
    );

    // ---- 断言 5：文件那一份没被牺牲（：日志要有确定落点）
    let kernel_log = std::fs::read_to_string(dir.join("kernel.log")).expect("kernel.log 应存在");
    assert!(
        kernel_log.contains("initial proxy provider missing-provider error"),
        "kernel.log 里应有内核原文，实际：\n{kernel_log}"
    );

    // ---- 断言 6：文件那一份**两条流都在**（文件 sink 是 tee 的一半，不许只留一条流）
    assert!(
        kernel_log.contains("Start initial compatible provider default"),
        "kernel.log 里丢了 stdout 那一份：\n{kernel_log}"
    );

    spawner.terminate(stdout_pid, Duration::from_millis(300)).await.expect("收 stdout 进程");
    spawner.terminate(stderr_pid, Duration::from_millis(300)).await.expect("收 stderr 进程");
    assert!(!spawner.is_alive(stdout_pid));
    assert!(!spawner.is_alive(stderr_pid));
}

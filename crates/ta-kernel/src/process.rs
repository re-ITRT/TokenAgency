//! 内核进程生命周期。契约：。
//!
//! | 阶段 | 约定 |
//! |---|---|
//! | 启动 | `mihomo -d <appdata>/kernel -f kernel.generated.yaml`，**Windows 绑定 Job Object** 防孤儿 |
//! | 就绪 | 轮询 `GET /version`（带 secret），超时 5s |
//! | 失败 | 指数退避重启 1s→2s→4s→8s→16s（上限 30s），连续 5 次 → `state=failed` |
//! | 停止 | 先 TERM（Windows 先 `taskkill`），等 2s，再强杀 |
//! | 状态机 | `stopped → starting → ready → (restarting → ready)* | failed` |
//!
//! 设计要点：
//! - 进程操作走 [`ProcessSpawner`] / 等待走 [`Sleeper`]，所以看护逻辑（退避、放弃、掉线重启）
//!   可以在**毫秒级**的单测里跑完整分支，不需要真 mihomo；
//! - [`SystemSpawner`] 是真的实现：stdout/stderr 追加进 `<appdata>/logs/kernel.log`，
//!   Windows 上用 `KILL_ON_JOB_CLOSE` 的 Job Object 绑住子进程（TokenAgency 一退，内核必被带走）；
//! - 这里**绝不**做任何系统级网络设置（不写系统代理、不改路由表、不装 TUN、不开 53 端口）。

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ta_core::domain::{ConfigDoc, KernelProvider};
use ta_core::error::{FieldError, Result, TaError};
use ta_core::types::LaneId;

use crate::childlog::{
    pump_reader, ChildLogSink, ChildLogSinkFactory, ChildStream, DefaultChildLogSinks,
};
use crate::config_gen::KernelConfigSource;
use crate::rest::{normalize_controller, KernelRest};
use crate::{KernelController, KernelState, KernelStatus, ProvidersSnapshot};

/// 内核进程 + 看护策略的全部参数。
#[derive(Debug, Clone)]
pub struct KernelProcessConfig {
    /// `net.kernel.path`（`provider: external` 时由用户提供）
    pub program: PathBuf,
    /// 默认 `["-d", <work_dir>, "-f", <config_path>]`
    pub args: Vec<String>,
    /// `-d` 的工作目录（`<appdata>/kernel`）
    pub work_dir: PathBuf,
    /// 生成的配置绝对路径
    pub config_path: PathBuf,
    /// `<appdata>/logs/kernel.log`
    pub log_path: PathBuf,
    /// `net.kernel.controller`（仅 loopback）
    pub controller: String,
    /// 生成配置里写的 secret，REST 用它做 Bearer
    pub secret: String,
    /// 就绪探测总超时（默认 5s）
    pub ready_timeout: Duration,
    /// 就绪探测间隔
    pub ready_poll: Duration,
    /// provider 就绪预算（默认 30s）。
    ///
    /// 内核起来之后要**自己去抓订阅**，这一步可能比 `/version` 慢得多。所以 provider 检查
    /// 有独立的、更宽的预算：进程+REST 就绪 ≠ provider 就绪。
    pub provider_ready_timeout: Duration,
    /// 停止时 TERM 后等多久再强杀（默认 2s）
    pub terminate_grace: Duration,
    /// ready 状态下的存活轮询间隔
    pub alive_poll: Duration,
    /// 连续失败多少次后 `state=failed`（默认 5）
    pub max_consecutive_failures: u32,
    /// 退避基数（默认 1s）
    pub backoff_base: Duration,
    /// 退避倍数（默认 2.0）
    pub backoff_factor: f64,
    /// 退避上限（默认 30s）
    pub backoff_cap: Duration,
}

impl KernelProcessConfig {
    pub fn new(
        program: impl Into<PathBuf>,
        kernel_dir: impl Into<PathBuf>,
        logs_dir: impl Into<PathBuf>,
        controller: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        let work_dir = kernel_dir.into();
        let logs_dir = logs_dir.into();
        let config_path = work_dir.join("kernel.generated.yaml");
        let args = vec![
            "-d".to_string(),
            work_dir.to_string_lossy().to_string(),
            "-f".to_string(),
            config_path.to_string_lossy().to_string(),
        ];
        Self {
            program: program.into(),
            args,
            work_dir,
            config_path,
            log_path: logs_dir.join("kernel.log"),
            controller: controller.into(),
            secret: secret.into(),
            ready_timeout: Duration::from_secs(5),
            ready_poll: Duration::from_millis(200),
            provider_ready_timeout: Duration::from_secs(30),
            terminate_grace: Duration::from_secs(2),
            alive_poll: Duration::from_secs(1),
            max_consecutive_failures: 5,
            backoff_base: Duration::from_secs(1),
            backoff_factor: 2.0,
            backoff_cap: Duration::from_secs(30),
        }
    }

    /// 覆盖启动参数（测试里用 `cmd /c ...`、`sleep` 这种假内核）
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// 生成配置里的 secret（REST Bearer）；由 [`crate::config_gen::MhConfigGen`] 产出
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = secret.into();
        self
    }

    /// 直接从领域配置建：**程序路径来自 `net.kernel.path`**（`provider: external`），
    /// 控制面来自 `net.kernel.controller`。`appdata` 相关的两个目录由调用方给。
    pub fn from_doc(
        doc: &ConfigDoc,
        kernel_dir: impl Into<PathBuf>,
        logs_dir: impl Into<PathBuf>,
    ) -> Self {
        let k = &doc.net.kernel;
        if k.provider != KernelProvider::External {
            tracing::warn!(
                "net.kernel.provider 不是 external：内核路径仍需指向随包/自备的可执行文件"
            );
        }
        Self::new(
            k.path.clone().unwrap_or_default(),
            kernel_dir,
            logs_dir,
            k.controller.clone(),
            String::new(),
        )
    }

    /// 只要外部内核路径没给，就是配置错误（`provider: external`）
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();
        if self.program.as_os_str().is_empty() {
            errors.push(FieldError {
                path: "net.kernel.path".into(),
                code: "kernel_path_missing".into(),
                message: "没有配置内核可执行文件路径（provider: external 时必须指定）".into(),
            });
        }
        if let Err(TaError::ConfigInvalid { errors: mut e2 }) =
            normalize_controller(&self.controller)
        {
            errors.append(&mut e2);
        }
        if self.config_path.as_os_str().is_empty() {
            errors.push(FieldError {
                path: "net.kernel.config_path".into(),
                code: "config_path_missing".into(),
                message: "内核配置文件路径为空".into(),
            });
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(TaError::ConfigInvalid { errors })
        }
    }
}

// ================================================================ 进程操作抽象

/// 真正的进程操作。抽出来是为了让看护逻辑可测（测试用假 spawner）。
#[async_trait]
pub trait ProcessSpawner: Send + Sync {
    /// 拉起进程，返回 pid
    async fn spawn(&self, cfg: &KernelProcessConfig) -> Result<u32>;
    /// 该 pid 现在是否还活着
    fn is_alive(&self, pid: u32) -> bool;
    /// 先 TERM（等 `grace`），还活着就强杀
    async fn terminate(&self, pid: u32, grace: Duration) -> Result<()>;

    /// 等这次 spawn 的日志读取线程把管道读到 EOF（**带超时**）。返回是否全部读完。
    ///
    /// 为什么需要：读取线程是 detach 的。进程刚死时，**崩溃现场的最后几行可能还在管道里**
    /// —— 而 B4 的全部意义就是那几行。所以关停路径必须能**有界地**等它们落完；
    /// 早先"直接丢句柄"的写法让最后几行有机会丢在缓冲区里。
    ///
    /// 没读完（超时）返回 `false`：剩下的线程会在 EOF 后自行结束，**不 join**
    /// （孙进程握着管道时 join 会无限阻塞，那会让 stop 卡死）。
    /// 默认实现返回 `true`：假 spawner 没有读线程。
    fn drain_logs(&self, _pid: u32, _timeout: Duration) -> bool {
        true
    }
}

/// 日志读取线程的**落盘宽限**：进程确认已死之后，最多等这么久把残留读出来。
/// 管道已 EOF 时线程会立刻结束，所以正常路径下这个等待是**微秒级**的。
const LOG_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// 一个 pid 的读线程 + 它们的完成信号（用于"有界地等读完"）。
struct PumpGroup {
    handles: Vec<std::thread::JoinHandle<()>>,
    progress: Arc<PumpProgress>,
}

/// 读线程的存活计数：归零 = 全部把管道读到了 EOF。
#[derive(Default)]
struct PumpProgress {
    live: Mutex<usize>,
    done: std::sync::Condvar,
}

impl PumpProgress {
    /// 开一个读线程**之前**先加一（否则线程可能先结束、计数变负）。
    fn add(&self) {
        *self.live.lock().expect("读线程计数锁被毒化") += 1;
    }

    fn finish(&self) {
        let mut live = self.live.lock().expect("读线程计数锁被毒化");
        *live = live.saturating_sub(1);
        self.done.notify_all();
    }

    fn count(&self) -> usize {
        *self.live.lock().expect("读线程计数锁被毒化")
    }
}

/// 用 `std::process::Child` 真拉进程；Windows 上绑 Job Object。
///
/// **stdout/stderr 不走 `Stdio::from(file)`**：那样内核的输出只落文件，一行都到不了我们的
/// `tracing` —— 于是"内核自己在 `level=error` 说了原因，而我们的日志里什么都没有"
/// （B4；真夹具 `emptya.log` / `emptyb.log` 就是这个场景）。
/// 现在两条管道由 [`crate::childlog`] 的读取线程逐行接管，再交给可注入的 sink。
pub struct SystemSpawner {
    children: Mutex<BTreeMap<u32, ManagedChild>>,
    /// 内核输出的去向。生产默认是"落 `kernel.log` + 转发进 tracing"两份都要。
    log_sinks: Arc<dyn ChildLogSinkFactory>,
    /// 每个 pid 的读线程与完成信号（见 [`SystemSpawner::drain_logs`] 的语义）。
    log_pumps: Mutex<BTreeMap<u32, PumpGroup>>,
}

impl Default for SystemSpawner {
    fn default() -> Self {
        Self {
            children: Mutex::new(BTreeMap::new()),
            log_sinks: Arc::new(DefaultChildLogSinks),
            log_pumps: Mutex::new(BTreeMap::new()),
        }
    }
}

struct ManagedChild {
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
}

impl std::fmt::Debug for SystemSpawner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.children.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("SystemSpawner").field("children", &n).finish()
    }
}

impl SystemSpawner {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入内核输出的去向（测试用；默认 = `kernel.log` + `tracing` 两份）。
    pub fn with_log_sinks(mut self, log_sinks: Arc<dyn ChildLogSinkFactory>) -> Self {
        self.log_sinks = log_sinks;
        self
    }

    /// 还有多少条读线程没读完（测试 / 诊断用）。
    pub fn pump_count(&self) -> usize {
        self.log_pumps.lock().expect("spawner 锁被毒化").values().map(|g| g.progress.count()).sum()
    }

    /// 关停路径用：把所有 pid 的日志读完（每个最多等 `timeout`），返回**没读完的 pid 数**。
    ///
    /// 应用退出前该调它（或在 `stop()` 路径上依赖 `terminate` 的自动 drain）——
    /// 否则"内核崩溃现场的最后几行"可能还在管道里。
    pub fn shutdown_logs(&self, timeout: Duration) -> usize {
        let pids: Vec<u32> =
            self.log_pumps.lock().expect("spawner 锁被毒化").keys().copied().collect();
        pids.into_iter().filter(|pid| !self.drain_logs(*pid, timeout)).count()
    }

    /// 起一个专用线程逐行转发一条管道。
    ///
    /// `pump_reader` 是阻塞 IO，绝不能挂在 tokio 的工作线程上（会把整个异步运行时拖住）。
    ///
    /// 线程创建失败**不 panic**：那时读端随闭包一起被丢掉（等于关掉我们这一端），
    /// 内核写它会拿到 EPIPE，而不是被堵死。相比"日志线程没起来就把内核卡住"，这是更小的问题。
    fn spawn_log_pump<R>(
        &self,
        pid: u32,
        reader: R,
        stream: ChildStream,
        sink: Arc<dyn ChildLogSink>,
        progress: Arc<PumpProgress>,
    ) -> Option<std::thread::JoinHandle<()>>
    where
        R: std::io::Read + Send + 'static,
    {
        progress.add();
        // 线程里用一份克隆：`Err` 分支还要用 `progress` 把计数还回去
        let in_thread = progress.clone();
        match std::thread::Builder::new()
            .name(format!("ta-kernel-log-{pid}-{}", stream.as_str()))
            .spawn(move || {
                pump_reader(reader, stream, sink.as_ref());
                // 读到 EOF（或出错）才算完 —— 这是 drain_logs 唯一等的信号
                in_thread.finish();
            }) {
            Ok(handle) => Some(handle),
            Err(e) => {
                // 线程没起来就不会有人 finish，手动把计数还回去，否则 drain_logs 会白等超时
                progress.finish();
                tracing::warn!(
                    pid,
                    "起内核日志读取线程失败，这条流的输出不会被转发（内核仍可运行）：{e}"
                );
                None
            }
        }
    }

    /// 被管进程数（测试 / 诊断用）
    pub fn managed(&self) -> usize {
        self.children.lock().expect("spawner 锁被毒化").len()
    }

    /// 该 pid 是否被绑进了 Job Object（Windows；其它平台恒 `false`）
    pub fn job_bound(&self, pid: u32) -> bool {
        #[cfg(windows)]
        {
            return self
                .children
                .lock()
                .expect("spawner 锁被毒化")
                .get(&pid)
                .map(|c| c.job.is_some())
                .unwrap_or(false);
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            false
        }
    }
}

#[async_trait]
impl ProcessSpawner for SystemSpawner {
    async fn spawn(&self, cfg: &KernelProcessConfig) -> Result<u32> {
        cfg.validate()?;
        std::fs::create_dir_all(&cfg.work_dir)?;
        // 每次 spawn 按当前 log_path 造 sink（默认会把 kernel.log 建出来 —— 的落点）。
        // **打不开就失败**：不能静默退化成"只有 tracing"（那样 log_tail 的空串会同时表示
        // "没有日志文件"和"内核没输出"，[14] 明确禁止这种混淆）。
        let sink = self.log_sinks.sink_for(&cfg.log_path)?;

        let mut cmd = Command::new(&cfg.program);
        cmd.args(&cfg.args)
            .current_dir(&cfg.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 不给子进程继承我们的环境代理设置（内核自己管出站）
        cmd.env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy");
        // 不给这个标志，内核的控制台窗口会**弹出来并抢前台**（真机反馈，见 `no_window`）
        no_window(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            TaError::Kernel(format!("拉起内核失败（{}）：{e}", cfg.program.display()))
        })?;
        let pid = child.id();

        // 两条管道各起一个专用线程逐行转发。放在 Job Object 绑定之前也要放在
        // `children` 插入之前 —— 越早开始读，越不容易在内核猛刷日志时把管道塞满。
        let progress = Arc::new(PumpProgress::default());
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
        if let Some(out) = child.stdout.take() {
            if let Some(handle) =
                self.spawn_log_pump(pid, out, ChildStream::Stdout, sink.clone(), progress.clone())
            {
                handles.push(handle);
            }
        }
        if let Some(err) = child.stderr.take() {
            if let Some(handle) =
                self.spawn_log_pump(pid, err, ChildStream::Stderr, sink, progress.clone())
            {
                handles.push(handle);
            }
        }
        if !handles.is_empty() {
            self.log_pumps
                .lock()
                .expect("spawner 锁被毒化")
                .insert(pid, PumpGroup { handles, progress });
        }

        #[cfg(windows)]
        let job = {
            use std::os::windows::io::AsRawHandle;
            let handle = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            match win_job::bind(handle) {
                Some(j) => Some(j),
                None => {
                    tracing::warn!(
                        pid,
                        "Job Object 绑定失败（进程仍可被 TERM/强杀，但极端情况下可能留孤儿）"
                    );
                    None
                }
            }
        };

        self.children.lock().expect("spawner 锁被毒化").insert(
            pid,
            ManagedChild {
                child,
                #[cfg(windows)]
                job,
            },
        );
        tracing::info!(pid, program = %cfg.program.display(), "内核进程已拉起");
        Ok(pid)
    }

    fn is_alive(&self, pid: u32) -> bool {
        // 先只动 `children` 的锁；`log_pumps` 在锁外回收 ——
        // 两把锁的获取顺序必须与 `spawn` 一致，否则并发时会死锁。
        let exited = {
            let mut map = self.children.lock().expect("spawner 锁被毒化");
            match map.get_mut(&pid) {
                Some(mc) => match mc.child.try_wait() {
                    Ok(None) => false,
                    Ok(Some(status)) => {
                        tracing::info!(pid, ?status, "内核进程已退出");
                        map.remove(&pid);
                        true
                    }
                    Err(e) => {
                        tracing::warn!(pid, error = %e, "查询内核进程状态失败，按已退出处理");
                        map.remove(&pid);
                        true
                    }
                },
                None => return false,
            }
        };
        if exited {
            // 进程没了 → 管道 EOF → 读线程很快结束。有界等它们读完，
            // 否则"内核退出前最后那几行"可能还在管道缓冲里（B4 的意义正是那几行）。
            self.drain_logs(pid, LOG_DRAIN_TIMEOUT);
        }
        !exited
    }

    /// 有界地等某个 pid 的读线程读完（见 trait 文档）。
    fn drain_logs(&self, pid: u32, timeout: Duration) -> bool {
        let group = match self.log_pumps.lock().expect("spawner 锁被毒化").remove(&pid) {
            Some(group) => group,
            None => return true, // 没有读线程（或已经 drain 过）→ 当作已完成
        };
        let deadline = Instant::now() + timeout;
        {
            let mut live = group.progress.live.lock().expect("读线程计数锁被毒化");
            while *live > 0 {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (guard, _) = group
                    .progress
                    .done
                    .wait_timeout(live, deadline - now)
                    .expect("读线程计数锁被毒化");
                live = guard;
            }
        }
        let all_done = group.progress.count() == 0;
        if all_done {
            // 已经结束 → join 立刻返回；**只在确认结束时 join**，否则会无限阻塞
            for handle in group.handles {
                let _ = handle.join();
            }
        }
        all_done
    }

    async fn terminate(&self, pid: u32, grace: Duration) -> Result<()> {
        // 注意：guard 必须在 await 之前释放，否则 future 不是 Send
        let taken = { self.children.lock().expect("spawner 锁被毒化").remove(&pid) };
        let mut mc = match taken {
            Some(mc) => mc,
            None => {
                // 不是我们拉起来的：仍然尽力按 pid 干掉（防止上次崩溃留下的）
                graceful_signal(pid);
                wait_gone(pid, grace).await;
                force_kill_pid(pid);
                // 进程没了就有界地把它的输出读完（见 drain_logs）
                self.drain_logs(pid, LOG_DRAIN_TIMEOUT);
                return Ok(());
            }
        };

        // 1) 先礼：TERM
        graceful_signal(pid);
        // 2) 等 grace
        let deadline = Instant::now() + grace;
        loop {
            match mc.child.try_wait() {
                Ok(Some(_)) => {
                    tracing::info!(pid, "内核进程已优雅退出");
                    self.drain_logs(pid, LOG_DRAIN_TIMEOUT);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(pid, error = %e, "try_wait 失败");
                    break;
                }
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // 3) 后兵：强杀
        tracing::warn!(pid, "内核进程未响应 TERM，强杀");
        force_kill_pid(pid);
        let _ = mc.child.kill();
        let _ = mc.child.wait();
        // 进程已确认死亡 → 有界地把它的输出读完（B4 的意义就是那最后几行）
        self.drain_logs(pid, LOG_DRAIN_TIMEOUT);
        Ok(())
    }
}

/// Windows：给"我们会短暂拉起来的控制台程序"（内核本身、`taskkill`）加 `CREATE_NO_WINDOW`。
///
/// 真机反馈（2026-09-27）：装完打开应用会**弹出一个黑色 cmd 窗口、还抢到前台** ——
/// 那就是内核 `mihomo.exe`（控制台程序）用默认创建标志被拉起时的控制台窗口；
/// `Stdio::null()/piped()` 只换句柄，**挡不住窗口创建**，必须给创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

#[cfg(windows)]
fn graceful_signal(pid: u32) {
    // Windows 没有 SIGTERM：先用不带 /F 的 taskkill（能优雅就优雅），失败也无所谓
    let mut cmd = Command::new("taskkill");
    cmd.args(["/PID", &pid.to_string(), "/T"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let _ = cmd.status();
}

#[cfg(unix)]
fn graceful_signal(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(windows, unix)))]
fn graceful_signal(pid: u32) {
    let _ = pid;
}

#[cfg(windows)]
fn force_kill_pid(pid: u32) {
    let mut cmd = Command::new("taskkill");
    cmd.args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let _ = cmd.status();
}

#[cfg(unix)]
fn force_kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(windows, unix)))]
fn force_kill_pid(pid: u32) {
    let _ = pid;
}

async fn wait_gone(pid: u32, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 该 pid 现在是否还存在（用于"不是我们拉起来的进程"这条兜底路径）
pub fn process_exists(pid: u32) -> bool {
    #[cfg(windows)]
    {
        win_job::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
        // Unix：kill -0
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ================================================================ Windows Job Object

#[cfg(windows)]
mod win_job {
    use core::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub struct JobHandle(HANDLE);

    // HANDLE 是内核对象句柄，跨线程持有/关闭是安全的
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// 把子进程绑进一个 `KILL_ON_JOB_CLOSE` 的 Job。
    ///
    /// 只要 TokenAgency 自己退出（哪怕是崩溃），Job 句柄一关，内核进程会被内核一起收走 ——
    /// 这是"绝不留孤儿进程"的硬保证。Win8+ 支持嵌套 Job，所以在别人的 Job 里也能绑成功。
    pub fn bind(process: HANDLE) -> Option<JobHandle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return None;
            }
            if AssignProcessToJobObject(job, process) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(JobHandle(job))
        }
    }

    /// 进程是否还活着（用新句柄查，不依赖我们有没有持有 Child）
    pub fn process_alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && (code as i32) == STILL_ACTIVE
        }
    }
}

// ================================================================ 等待抽象

/// 让退避/轮询在测试里可以"不真的等"
#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, d: Duration);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSleeper;

#[async_trait]
impl Sleeper for SystemSleeper {
    async fn sleep(&self, d: Duration) {
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
}

/// 退避序列：`retry` 从 1 开始 → `base * factor^(retry-1)`，上限 `cap`。
///
/// 默认参数：1s → 2s → 4s → 8s → 16s → 30s → 30s…
pub fn backoff_delay(retry: u32, base: Duration, factor: f64, cap: Duration) -> Duration {
    if retry == 0 {
        return Duration::ZERO;
    }
    let exp = (retry - 1) as i32;
    let millis = (base.as_secs_f64() * factor.powi(exp)) * 1000.0;
    if !millis.is_finite() || millis <= 0.0 {
        return base.min(cap);
    }
    Duration::from_millis(millis.round() as u64).min(cap)
}

// ================================================================ 看护

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuperviseOutcome {
    /// 内核活着，本轮什么都没做
    Idle,
    /// 首次拉起并已就绪
    Started,
    /// 掉线后重启成功
    Restarted,
    /// 本轮尝试失败（已计入退避）
    AttemptFailed { attempt: u32, message: String },
    /// 进程活着、REST 也通，但 provider 还没加载到节点 —— **不报就绪、也不重启**
    /// （内核没错，只是订阅还没抓完；`verify_providers` 会继续复查）
    WaitingForProviders { message: String },
    /// 连续失败达到上限，进入 `failed`，不再重试
    GaveUp { attempts: u32, message: String },
}

impl SuperviseOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, SuperviseOutcome::AttemptFailed { .. } | SuperviseOutcome::GaveUp { .. })
    }
}

#[derive(Debug)]
struct Inner {
    state: KernelState,
    version: Option<String>,
    pid: Option<u32>,
    spawn_attempts: u32,
    consecutive_failures: u32,
    last_error: Option<String>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            state: KernelState::Stopped,
            version: None,
            pid: None,
            spawn_attempts: 0,
            consecutive_failures: 0,
            last_error: None,
        }
    }
}

/// `KernelController` 的真实现：拉起 / 就绪探测 / 退避重启 / Job Object / REST 转发。
pub struct KernelSupervisor {
    cfg: KernelProcessConfig,
    spawner: Arc<dyn ProcessSpawner>,
    rest: Arc<KernelRest>,
    source: Arc<dyn KernelConfigSource>,
    sleeper: Arc<dyn Sleeper>,
    inner: Mutex<Inner>,
    /// 「拉起一个进程」这条路径的**串行闸**。
    ///
    /// 为什么必须有它（真机实测的缺陷）：`start()` 原本的幂等判据只有 `state == Ready`，
    /// 而**冷启动期间**状态是 `Starting`（进程已起、在等 REST + 等 provider，可能几十秒）——
    /// 此时两个调用方同时进来会**各起一个 mihomo**。启动编排（`BootOrchestrator` → `apply()`）
    /// 与「订阅数据就绪后再拉一次」（`kick_kernel` → `apply()`）正好在冷启动那一秒撞车，
    /// 第二个内核抢不到端口，只在 `kernel.log` 里刷 `bind: Only one usage of each socket address`。
    ///
    /// 闸的持有范围 = **一次完整的拉起尝试**（含等就绪），因为"尝试"与"结果"必须成对：
    /// 只锁 spawn 那一下的话，第二个调用方会在第一个还没得出结论时又去 spawn。
    start_gate: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for KernelSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelSupervisor")
            .field("controller", &self.cfg.controller)
            .field("status", &self.status_sync())
            .finish()
    }
}

impl KernelSupervisor {
    pub fn new(
        cfg: KernelProcessConfig,
        spawner: Arc<dyn ProcessSpawner>,
        rest: Arc<KernelRest>,
        source: Arc<dyn KernelConfigSource>,
    ) -> Self {
        Self {
            cfg,
            spawner,
            rest,
            source,
            sleeper: Arc::new(SystemSleeper),
            inner: Mutex::new(Inner::default()),
            start_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// 用 [`SystemSpawner`] 的便捷构造
    pub fn with_system_spawner(
        cfg: KernelProcessConfig,
        rest: Arc<KernelRest>,
        source: Arc<dyn KernelConfigSource>,
    ) -> Self {
        Self::new(cfg, Arc::new(SystemSpawner::new()), rest, source)
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    pub fn config(&self) -> &KernelProcessConfig {
        &self.cfg
    }

    pub fn rest(&self) -> &KernelRest {
        &self.rest
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("kernel 状态锁被毒化")
    }

    /// 等内核自己走到 `Ready`（或先走到终态），**全程不拉起任何进程**。
    ///
    /// 给 `apply()` 在「别人正在拉起」时用：那一刻**唯一正确的做法是等**，
    /// 再 spawn 一个只会得到两个内核抢同一批端口（见 `KernelSupervisor::start_gate` 的说明）。
    ///
    /// 返回 `true` = 已经就绪，可以 reload 配置了。
    /// * 走到 `Failed` / `Stopped` 就**立刻**返回 `false`：终态不会再变 Ready，
    ///   继续耗满超时预算只是让调用方白等（那些秒数是真机上看得见的卡顿）。
    async fn wait_until_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.state() {
                KernelState::Ready => return true,
                KernelState::Failed | KernelState::Stopped => return false,
                // 还在被拉起的过程中：继续等
                KernelState::Starting | KernelState::Restarting => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.sleeper.sleep(self.cfg.ready_poll).await;
        }
    }

    /// 同步取状态（[`KernelController::status`] 的同步版）
    pub fn status_sync(&self) -> KernelStatus {
        let i = self.lock();
        KernelStatus {
            state: i.state.clone(),
            version: i.version.clone(),
            pid: i.pid,
            restarts: i.spawn_attempts.saturating_sub(1),
            last_error: i.last_error.clone(),
        }
    }

    pub fn state(&self) -> KernelState {
        self.lock().state.clone()
    }

    pub fn pid(&self) -> Option<u32> {
        self.lock().pid
    }

    /// 连续失败计数（诊断用）
    pub fn consecutive_failures(&self) -> u32 {
        self.lock().consecutive_failures
    }

    fn record_failure(&self, e: &TaError) {
        let mut i = self.lock();
        i.pid = None;
        i.consecutive_failures += 1;
        i.last_error = Some(e.to_string());
        i.state = if i.consecutive_failures >= self.cfg.max_consecutive_failures {
            tracing::error!(
                failures = i.consecutive_failures,
                error = %e,
                code = e.code(),
                "内核连续启动失败，进入 failed（通道降级为仅 direct）"
            );
            KernelState::Failed
        } else {
            tracing::warn!(failures = i.consecutive_failures, code = e.code(), error = %e, "内核启动失败，将退避重启");
            // **让失败自己说话**：`ConfigInvalid` 的 Display 只有一句"配置校验失败"，
            // 看不到是哪个字段 —— 真机排查时这一条信息决定"改数据"还是"改代码"。
            if let TaError::ConfigInvalid { errors } = &e {
                for f in errors {
                    tracing::warn!(
                        path = %f.path,
                        code = %f.code,
                        reason = %f.message,
                        "生成配置未通过校验（逐条）"
                    );
                }
            }
            KernelState::Restarting
        };
    }

    /// 一次"拉起 + 等就绪"
    async fn start_attempt(&self) -> Result<()> {
        {
            let mut i = self.lock();
            i.state = KernelState::Starting;
            i.pid = None;
        }
        let pid = match self.spawner.spawn(&self.cfg).await {
            Ok(pid) => pid,
            Err(e) => {
                self.record_failure(&e);
                return Err(e);
            }
        };
        {
            let mut i = self.lock();
            i.pid = Some(pid);
            i.spawn_attempts += 1;
        }
        match self.wait_rest_ready(pid).await {
            Ok(version) => {
                // 第二关：provider 必须真的加载到节点。**失败时不杀进程、不计失败** ——
                // 内核本身是对的，只是订阅还没抓完；杀掉它会让慢订阅永远起不来。
                match self.wait_providers().await {
                    Ok(()) => {
                        let mut i = self.lock();
                        i.state = KernelState::Ready;
                        i.version = Some(version.clone());
                        i.consecutive_failures = 0;
                        i.last_error = None;
                        tracing::info!(pid, %version, "内核已就绪（provider 已加载节点）");
                        Ok(())
                    }
                    Err(e) => {
                        {
                            let mut i = self.lock();
                            i.state = KernelState::Starting;
                            i.version = Some(version);
                            i.last_error = Some(e.to_string());
                        }
                        tracing::error!(pid, code = e.code(), error = %e, "内核进程活着但 provider 没有节点：不报就绪");
                        Err(e)
                    }
                }
            }
            Err(e) => {
                // 起不来就把这个半死的进程收掉，别留着占端口
                if let Err(te) = self.spawner.terminate(pid, self.cfg.terminate_grace).await {
                    tracing::warn!(pid, code = te.code(), error = %te, "清理未就绪的内核进程失败");
                }
                self.record_failure(&e);
                Err(e)
            }
        }
    }

    /// 轮询 `GET /version`，超时 `ready_timeout`（默认 5s）—— 只证明"进程活着、REST 通了"
    async fn wait_rest_ready(&self, pid: u32) -> Result<String> {
        let deadline = Instant::now() + self.cfg.ready_timeout;
        loop {
            if !self.spawner.is_alive(pid) {
                return Err(TaError::Kernel(format!("内核进程在就绪前退出（pid {pid}）")));
            }
            let last = match self.rest.version().await {
                Ok(v) => return Ok(v),
                Err(e) => e.to_string(),
            };
            if Instant::now() >= deadline {
                return Err(TaError::Kernel(format!(
                    "内核就绪超时（{}ms）：{last}",
                    self.cfg.ready_timeout.as_millis()
                )));
            }
            self.sleeper.sleep(self.cfg.ready_poll).await;
        }
    }

    /// ★ **就绪的第二关**：我们生成的每个 provider 真的加载到节点了吗？
    ///
    /// 为什么必须有这一关：provider 加载失败（文件缺失 / `proxies: []` / 解析错）时，
    /// 内核**一边 `level=error` 一边照常启动**，REST 也通 —— 只看"进程在不在"会把
    /// **整个内核静默降级**报成"已就绪"，然后所有 lane 一起走空（配合
    /// `empty-fallback: REJECT` 是 502，没配就是静默直连 = 境外流量裸奔）。
    ///
    /// 可**重复调用**：订阅同步完成后 provider 才可能有效，所以这不是"启动时查一次就锁死"。
    pub async fn verify_providers(&self) -> Result<()> {
        let expected = self.source.expected_providers()?;
        if expected.is_empty() {
            return Ok(()); // 没有 provider（全直连的配置）→ 没什么可查
        }
        let counts = self.rest.provider_node_counts().await?;
        let empty: Vec<String> = expected
            .iter()
            .filter(|key| counts.get(key.as_str()).copied().unwrap_or(0) == 0)
            .cloned()
            .collect();
        if !empty.is_empty() {
            return Err(TaError::Kernel(format!(
                "内核进程与 REST 都正常，但 provider 没有节点：{}（内核日志里通常有 \
                 `file doesn't have any proxy` / `initial proxy provider ... error`）—— \
                 因此**不报就绪**",
                empty.join("、")
            )));
        }
        Ok(())
    }

    /// 在 `provider_ready_timeout` 内反复检查 provider（内核抓订阅可能比 REST 慢很多）
    async fn wait_providers(&self) -> Result<()> {
        let deadline = Instant::now() + self.cfg.provider_ready_timeout;
        loop {
            match self.verify_providers().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    tracing::debug!(code = e.code(), error = %e, "provider 还没加载到节点，稍后再查");
                    self.sleeper.sleep(self.cfg.ready_poll).await;
                }
            }
        }
    }

    /// 看护一步：掉线就重启，失败就退避。测试直接调它，不用起后台任务。
    pub async fn supervise_once(&self) -> SuperviseOutcome {
        let (state, pid, failures) = {
            let i = self.lock();
            (i.state.clone(), i.pid, i.consecutive_failures)
        };

        match state {
            KernelState::Failed => {
                let i = self.lock();
                return SuperviseOutcome::GaveUp {
                    attempts: i.consecutive_failures,
                    message: i.last_error.clone().unwrap_or_default(),
                };
            }
            KernelState::Ready => match pid {
                Some(pid) if self.spawner.is_alive(pid) => return SuperviseOutcome::Idle,
                _ => {
                    let mut i = self.lock();
                    i.state = KernelState::Restarting;
                    i.pid = None;
                    i.last_error = Some("内核进程已退出，准备重启".to_string());
                    tracing::warn!("内核进程掉线，准备重启");
                }
            },
            KernelState::Starting => {
                // 进程还活着 → 只复查 provider，**绝不重新拉起**（否则会同时跑两个内核）
                if let Some(pid) = pid {
                    if self.spawner.is_alive(pid) {
                        return match self.verify_providers().await {
                            Ok(()) => {
                                let mut i = self.lock();
                                i.state = KernelState::Ready;
                                i.consecutive_failures = 0;
                                i.last_error = None;
                                tracing::info!(pid, "provider 已加载到节点，内核转为就绪");
                                SuperviseOutcome::Started
                            }
                            Err(e) => {
                                self.lock().last_error = Some(e.to_string());
                                SuperviseOutcome::WaitingForProviders { message: e.to_string() }
                            }
                        };
                    }
                }
                // 进程没了 → 落到下面的重启流程
                {
                    let mut i = self.lock();
                    i.state = KernelState::Restarting;
                    i.pid = None;
                }
            }
            _ => {}
        }

        // 进到这里说明需要（重新）拉起
        let failures = {
            let i = self.lock();
            i.consecutive_failures.max(failures)
        };
        if failures >= self.cfg.max_consecutive_failures {
            {
                let mut i = self.lock();
                i.state = KernelState::Failed;
            }
            let i = self.lock();
            return SuperviseOutcome::GaveUp {
                attempts: i.consecutive_failures,
                message: i.last_error.clone().unwrap_or_default(),
            };
        }
        if failures > 0 {
            let delay = backoff_delay(
                failures,
                self.cfg.backoff_base,
                self.cfg.backoff_factor,
                self.cfg.backoff_cap,
            );
            tracing::info!(
                attempt = failures + 1,
                delay_ms = delay.as_millis() as u64,
                "退避后重启内核"
            );
            self.sleeper.sleep(delay).await;
        }

        let restarting = self.lock().spawn_attempts > 0;
        match self.start_attempt().await {
            Ok(()) => {
                if restarting {
                    SuperviseOutcome::Restarted
                } else {
                    SuperviseOutcome::Started
                }
            }
            Err(e) => {
                let (state, attempts) = {
                    let i = self.lock();
                    (i.state.clone(), i.consecutive_failures)
                };
                if state == KernelState::Failed {
                    SuperviseOutcome::GaveUp { attempts, message: e.to_string() }
                } else {
                    SuperviseOutcome::AttemptFailed { attempt: attempts, message: e.to_string() }
                }
            }
        }
    }

    /// 看护循环，直到 `shutdown` 被取消或进入 `failed`。
    pub async fn supervise(&self, shutdown: &CancellationToken) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            let outcome = self.supervise_once().await;
            if matches!(outcome, SuperviseOutcome::GaveUp { .. }) {
                tracing::error!("内核看护放弃：连续失败已达上限");
                return;
            }
            let pause = if matches!(
                outcome,
                SuperviseOutcome::Idle | SuperviseOutcome::WaitingForProviders { .. }
            ) {
                self.cfg.alive_poll
            } else {
                Duration::ZERO
            };
            if !pause.is_zero() {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = self.sleeper.sleep(pause) => {}
                }
            }
        }
    }

    // ---------------------------------------------------------------- 额外能力

    /// 读 `<appdata>/logs/kernel.log` 的尾部 N 行
    pub fn log_tail(&self, lines: u32) -> Result<String> {
        tail_file(&self.cfg.log_path, lines)
    }

    /// 内核进程死没死（诊断/测试）
    pub fn process_alive(&self) -> bool {
        self.pid().map(|p| self.spawner.is_alive(p)).unwrap_or(false)
    }

    /// 期望的配置来源（给测试与 UI 用）
    pub fn source(&self) -> &Arc<dyn KernelConfigSource> {
        &self.source
    }
}

#[async_trait]
impl KernelController for KernelSupervisor {
    async fn status(&self) -> KernelStatus {
        self.status_sync()
    }

    async fn start(&self) -> Result<()> {
        // 串行闸：同一时刻**只允许一个调用方真的去拉进程**。
        //
        // 没有它就会得到两个 mihomo：启动编排与「订阅就绪再拉一次」在冷启动那一秒撞车时，
        // 两边读到的状态都不是 Ready（是 Starting / Stopped），于是各 spawn 一个，
        // 第二个抢不到端口（真机 `kernel.log` 里的 bind error 就是它）。
        let _gate = self.start_gate.lock().await;

        // 拿闸之后**重新判断**：等闸期间别人可能已经把内核拉起来了。
        if self.state() == KernelState::Ready {
            if let Some(pid) = self.pid() {
                if self.spawner.is_alive(pid) {
                    return Ok(());
                }
            }
        }

        // 已经在启动中（进程活着、在等就绪）：**绝不能再 spawn 一个**。
        //
        // 这里是"幂等"的正确边界：`start()` 的契约是"保证进程在跑"，而 `Starting` 且进程存活
        // 就是"正在跑"。想要"就绪"的调用方自己去等（编排侧有 `wait_kernel_ready`），
        // 不要把"我还没就绪"翻译成"我再起一个"。
        if self.state() == KernelState::Starting {
            if let Some(pid) = self.pid() {
                if self.spawner.is_alive(pid) {
                    tracing::info!(pid, "内核正在启动中：复用已在拉起的那个，不重复拉起");
                    return Ok(());
                }
            }
        }

        {
            // 显式启动 = 用户手动重试，重置连续失败计数
            let mut i = self.lock();
            i.consecutive_failures = 0;
            i.last_error = None;
        }
        self.start_attempt().await
    }

    async fn stop(&self) -> Result<()> {
        let pid = {
            let mut i = self.lock();
            i.pid.take()
        };
        if let Some(pid) = pid {
            self.spawner.terminate(pid, self.cfg.terminate_grace).await?;
            // terminate 内部已经 drain 过一次；这里显式再收一次，让"关停 = 日志读完"这条
            // 语义在 stop 这个入口上也看得见（幂等：读线程已结束时会立刻返回）
            self.spawner.drain_logs(pid, LOG_DRAIN_TIMEOUT);
        }
        let mut i = self.lock();
        i.state = KernelState::Stopped;
        i.version = None;
        i.consecutive_failures = 0;
        tracing::info!("内核已停止");
        Ok(())
    }

    async fn restart(&self) -> Result<()> {
        // 与 `start()` 共用同一道闸：重启也是"拉起一个进程"，
        // 不和并发的 start/apply 抢着各起一个（见 `start_gate` 的说明）。
        //
        // 注意**不能**在这里转调 `self.start()`：那会重入同一把 tokio Mutex（不自旋、会死锁）。
        let _gate = self.start_gate.lock().await;

        let pid = {
            let mut i = self.lock();
            i.pid.take()
        };
        if let Some(pid) = pid {
            self.spawner.terminate(pid, self.cfg.terminate_grace).await?;
        }
        {
            let mut i = self.lock();
            i.state = KernelState::Restarting;
            i.consecutive_failures = 0;
            i.last_error = None;
        }
        self.start_attempt().await
    }

    async fn apply(&self) -> Result<KernelStatus> {
        // 1) 生成 + 原子落盘
        let path = self.source.write_generated()?;
        let abs = path.to_string_lossy().to_string();

        // 1.5) 别人正在拉起（`Starting` / `Restarting`）：**等它就绪，绝不另起一个**。
        //
        // 这正是"两个 mihomo"的那条路：`apply()` 原来对任何非 Ready 状态都落到 `start()`，
        // 而冷启动那一秒状态就是 `Starting` —— 于是"订阅数据就绪后再拉一次"
        // （`kick_kernel`）会在启动编排还没拉完时又 spawn 一个，第二个抢不到端口。
        //
        // 磁盘上的配置在第 1 步已经更新，所以这里等就绪后**接着往下走 reload 分支**：
        // 修掉重复进程的同时，"新配置照样生效"这件事不能丢 —— 那件事故原本是被
        // 第二个进程**意外**做到的（它用新配置启动），不能靠继续犯错来维持。
        if matches!(self.state(), KernelState::Starting | KernelState::Restarting) {
            if self.wait_until_ready(self.cfg.provider_ready_timeout).await {
                tracing::info!(
                    config = %abs,
                    "内核已就绪（此前有人在拉起，本调用没有重复 spawn）：继续落这次配置"
                );
                // 落到下面的 `Ready` 分支：reload 这份新配置
            } else {
                // 还没就绪，或已经进了终态。如实把当前状态交回去给上层判断
                // （编排侧有自己的 `wait_kernel_ready` 超时窗口，不在这里替它下结论）。
                tracing::warn!(
                    config = %abs,
                    "内核仍在启动中（{:?}）：这次配置留待下一次 apply / reload，不重复拉起进程",
                    self.state()
                );
                return Ok(self.status_sync());
            }
        }

        // 2) 就绪 → reload；没起来 → 直接起
        match self.state() {
            KernelState::Ready => {
                if let Err(e) = self.rest.reload_config(&abs).await {
                    tracing::warn!(code = e.code(), error = %e, "内核 reload 失败，改为重启");
                    self.restart().await?;
                } else {
                    tracing::info!(config = %abs, "内核配置已 reload");
                    // reload 之后重新做第二关检查：新配置可能引入了新的 provider
                    if let Err(e) = self.verify_providers().await {
                        tracing::error!(code = e.code(), error = %e, "reload 后 provider 没有节点：回落为未就绪");
                        let mut i = self.lock();
                        i.state = KernelState::Starting;
                        i.last_error = Some(e.to_string());
                    }
                }
            }
            _ => {
                self.start().await?;
            }
        }
        Ok(self.status_sync())
    }

    async fn set_node(&self, group: &str, node: &str) -> Result<()> {
        self.rest.set_node(group, node).await
    }

    /// lane → 我们生成/引用的目标名（组名 / 节点名 / `DIRECT`），再 `PUT /proxies/{target}`。
    ///
    /// 上层（托盘、线路下拉、`lowest_latency` 选择器）只认 lane；「lane 对应哪个内核组」
    /// 由 [`KernelConfigSource::lane_targets`] 给出，是内核层的映射细节。
    async fn set_select(&self, lane: &LaneId, node: &str) -> Result<()> {
        let targets = self.source.lane_targets()?;
        let target =
            targets.get(lane).ok_or_else(|| TaError::LaneUnavailable { lane: lane.to_string() })?;
        // 直连 lane 没有可切换的组
        if target == "DIRECT" {
            return Err(TaError::LaneUnavailable { lane: lane.to_string() });
        }
        self.rest.set_node(target, node).await
    }

    async fn delay_of(&self, name: &str, timeout_ms: u64) -> Result<u32> {
        self.rest.delay_of(name, timeout_ms).await
    }

    async fn provider_delays(
        &self,
        key: &str,
        probe_url: &str,
        wait_ms: u64,
    ) -> Result<std::collections::BTreeMap<String, Option<u32>>> {
        self.rest.provider_delays(key, probe_url, wait_ms).await
    }

    async fn refresh_provider(&self, key: &str) -> Result<()> {
        self.rest.refresh_provider(key).await
    }

    async fn snapshot(&self) -> Result<ProvidersSnapshot> {
        self.rest.snapshot().await
    }

    async fn log_tail(&self, lines: u32) -> Result<String> {
        tail_file(&self.cfg.log_path, lines)
    }
}

fn tail_file(path: &Path, lines: u32) -> Result<String> {
    if lines == 0 {
        return Ok(String::new());
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => {
            return Err(TaError::Kernel(format!("读取内核日志 {} 失败：{e}", path.display())))
        }
    };
    // 只读尾部一块，避免把几百 MB 日志全塞进内存
    const CHUNK: u64 = 256 * 1024;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(CHUNK);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        // 第一行可能是被截断的半行
        all.remove(0);
    }
    let n = lines as usize;
    let take = if all.len() > n { &all[all.len() - n..] } else { &all[..] };
    let mut out = take.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::childlog::{ChildLogLine, FixedChildLogSink};
    use crate::config_gen::KernelConfigSource;
    use crate::testutil::TempDir;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    /// 记录内核输出行的 sink（B3 的断言用）。
    #[derive(Default)]
    struct RecordingChildSink {
        lines: Mutex<Vec<String>>,
    }

    impl RecordingChildSink {
        fn taken(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }
    }

    impl crate::childlog::ChildLogSink for RecordingChildSink {
        fn emit(&self, line: &ChildLogLine) {
            self.lines.lock().unwrap().push(line.raw.clone());
        }
    }

    // ------------------------------------------------------------ 假 spawner

    #[derive(Default)]
    struct ScriptedSpawner {
        inner: Mutex<ScriptState>,
    }

    #[derive(Default)]
    struct ScriptState {
        alive: std::collections::BTreeSet<u32>,
        spawned: Vec<u32>,
        terminated: Vec<u32>,
        fail_spawn: bool,
        next_pid: u32,
    }

    impl ScriptedSpawner {
        fn new() -> Self {
            Self::default()
        }

        fn kill_all(&self) {
            self.inner.lock().unwrap().alive.clear();
        }

        fn set_fail_spawn(&self, fail: bool) {
            self.inner.lock().unwrap().fail_spawn = fail;
        }

        fn spawn_count(&self) -> usize {
            self.inner.lock().unwrap().spawned.len()
        }

        fn terminated(&self) -> Vec<u32> {
            self.inner.lock().unwrap().terminated.clone()
        }
    }

    #[async_trait]
    impl ProcessSpawner for ScriptedSpawner {
        async fn spawn(&self, _cfg: &KernelProcessConfig) -> Result<u32> {
            let mut s = self.inner.lock().unwrap();
            if s.fail_spawn {
                return Err(TaError::Kernel("假 spawner：spawn 失败".to_string()));
            }
            s.next_pid += 1;
            let pid = s.next_pid;
            s.alive.insert(pid);
            s.spawned.push(pid);
            Ok(pid)
        }

        fn is_alive(&self, pid: u32) -> bool {
            self.inner.lock().unwrap().alive.contains(&pid)
        }

        async fn terminate(&self, pid: u32, _grace: Duration) -> Result<()> {
            let mut s = self.inner.lock().unwrap();
            s.alive.remove(&pid);
            s.terminated.push(pid);
            Ok(())
        }
    }

    /// 不发请求、只记录被 sleep 了多久
    #[derive(Default)]
    struct RecordingSleeper {
        sleeps: Mutex<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn taken(&self) -> Vec<Duration> {
            self.sleeps.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, d: Duration) {
            self.sleeps.lock().unwrap().push(d);
        }
    }

    // ------------------------------------------------------------ 假配置源

    struct TestSource {
        dir: PathBuf,
        body: String,
        targets: BTreeMap<LaneId, String>,
        /// 我们"写进配置"的 provider key（就绪第二关要逐个查）
        providers: Vec<String>,
    }

    impl TestSource {
        fn new(dir: &TempDir) -> Self {
            Self {
                dir: dir.path().to_path_buf(),
                body: "# GENERATED BY TokenAgency — DO NOT EDIT\nmode: rule\n".to_string(),
                targets: BTreeMap::new(),
                providers: Vec::new(),
            }
        }

        fn with_target(mut self, lane: &str, target: &str) -> Self {
            self.targets.insert(LaneId::new(lane), target.to_string());
            self
        }

        fn with_provider(mut self, key: &str) -> Self {
            self.providers.push(key.to_string());
            self
        }
    }

    impl KernelConfigSource for TestSource {
        fn write_generated(&self) -> Result<PathBuf> {
            let p = self.dir.join("kernel.generated.yaml");
            std::fs::write(&p, &self.body)?;
            Ok(p)
        }
        fn lane_targets(&self) -> Result<BTreeMap<LaneId, String>> {
            Ok(self.targets.clone())
        }
        fn expected_providers(&self) -> Result<Vec<String>> {
            Ok(self.providers.clone())
        }
    }

    // ------------------------------------------------------------ 脚手架

    struct Harness {
        supervisor: Arc<KernelSupervisor>,
        spawner: Arc<ScriptedSpawner>,
        sleeper: Arc<RecordingSleeper>,
        _server: MockServer,
        _dir: TempDir,
    }

    async fn ready_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "version": "v1.18.4",
                "meta": true
            })))
            .mount(&server)
            .await;
        server
    }

    async fn harness() -> Harness {
        let server = ready_server().await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("proc");
        let source = Arc::new(TestSource::new(&dir));
        let spawner = Arc::new(ScriptedSpawner::new());
        let sleeper = Arc::new(RecordingSleeper::default());
        let cfg =
            KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s3cret")
                .with_args(Vec::<String>::new());
        let sup = KernelSupervisor::new(
            cfg,
            spawner.clone(),
            rest,
            source as Arc<dyn KernelConfigSource>,
        )
        .with_sleeper(sleeper.clone());
        Harness { supervisor: Arc::new(sup), spawner, sleeper, _server: server, _dir: dir }
    }

    /// **就绪慢**的 harness：`GET /version` 会先延迟 `delay` 再回 200。
    ///
    /// 用途是造出"冷启动那几秒"的窗口 —— 木有这段延迟，`start_attempt()` 会在
    /// 第一个 await 之前就把状态推到 `Ready`，并发调用根本重叠不起来，
    /// 也就复现不出"两个 mihomo"（真机上窗口是几秒，因为内核要抓订阅）。
    async fn harness_slow_ready(delay: Duration) -> Harness {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(json!({ "version": "v1.18.4", "meta": true })),
            )
            .mount(&server)
            .await;
        // reload 必须也挂上：`apply()` 的就绪分支会 `PUT /configs`，
        // 没挂的话 reload 失败会**回落成 restart**，于是多多出一个进程 ——
        // 那是本 harness 的缺陷，会被误读成"重复拉起没修好"。
        Mock::given(method("PUT"))
            .and(path("/configs"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("proc-slow");
        let source = Arc::new(TestSource::new(&dir));
        let spawner = Arc::new(ScriptedSpawner::new());
        let sleeper = Arc::new(RecordingSleeper::default());
        let mut cfg =
            KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s3cret")
                .with_args(Vec::<String>::new());
        // 轮询要够密，才在延迟窗里多探几次（`RecordingSleeper` 的 sleep 是瞬时的）
        cfg.ready_poll = Duration::from_millis(1);
        let sup = KernelSupervisor::new(
            cfg,
            spawner.clone(),
            rest,
            source as Arc<dyn KernelConfigSource>,
        )
        .with_sleeper(sleeper.clone());
        Harness { supervisor: Arc::new(sup), spawner, sleeper, _server: server, _dir: dir }
    }

    /// 等到"确实已经有人拉起了进程、而且还没就绪"（即 `Starting` 且进程已 spawn）。
    ///
    /// 用**真实等待 + 墙钟上限**，不用"固定 2000 次 yield"：后者只要几十微秒就跑完了，
    /// 在多线程运行时上往往赶不上另一个 worker 把任务调起来 —— 于是用例偶发地报
    /// "等不到 Starting"，那是脚手架的问题，不是被测逻辑的问题。
    async fn wait_until_starting(h: &Harness) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if h.spawner.spawn_count() > 0 && h.supervisor.state() == KernelState::Starting {
                return;
            }
            // 也有人一次就跑到 Ready（延迟很短时）——那同样说明"已经有人在拉起"，
            // 只是快得没让我们看见中间态，这时不必再等。
            if h.spawner.spawn_count() > 0 && h.supervisor.state() == KernelState::Ready {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "等不到 `Starting`（5s）：spawn 次数={}，状态={:?}",
                h.spawner.spawn_count(),
                h.supervisor.state()
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 指向一个必然连不上的端口，用来模拟"内核起不来"
    async fn harness_unreachable_rest(max_failures: u32, ready_timeout: Duration) -> Harness {
        let server = MockServer::start().await;
        let rest = Arc::new(KernelRest::new("127.0.0.1:1", "s3cret").unwrap());
        let dir = TempDir::new("proc-fail");
        let source = Arc::new(TestSource::new(&dir));
        let spawner = Arc::new(ScriptedSpawner::new());
        let sleeper = Arc::new(RecordingSleeper::default());
        let mut cfg =
            KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s3cret")
                .with_args(Vec::<String>::new());
        cfg.ready_timeout = ready_timeout;
        cfg.ready_poll = Duration::from_secs(60);
        cfg.max_consecutive_failures = max_failures;
        let sup = KernelSupervisor::new(
            cfg,
            spawner.clone(),
            rest,
            source as Arc<dyn KernelConfigSource>,
        )
        .with_sleeper(sleeper.clone());
        Harness { supervisor: Arc::new(sup), spawner, sleeper, _server: server, _dir: dir }
    }

    // ------------------------------------------------------------ 退避公式

    #[test]
    fn backoff_ladder_matches_contract() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(30);
        let got: Vec<u64> = (1..=8).map(|r| backoff_delay(r, base, 2.0, cap).as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff_delay(0, base, 2.0, cap), Duration::ZERO);
        // 自定义基数/倍数
        assert_eq!(
            backoff_delay(3, Duration::from_millis(10), 3.0, cap),
            Duration::from_millis(90)
        );
    }

    // ------------------------------------------------------------ 启动 / 就绪

    #[tokio::test]
    async fn start_reaches_ready_and_probes_version() {
        let h = harness().await;
        assert_eq!(h.supervisor.state(), KernelState::Stopped);

        let outcome = h.supervisor.supervise_once().await;
        assert_eq!(outcome, SuperviseOutcome::Started);

        let st = h.supervisor.status_sync();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(st.version.as_deref(), Some("v1.18.4"));
        assert_eq!(st.pid, Some(1));
        assert_eq!(st.restarts, 0);
        assert_eq!(h.spawner.spawn_count(), 1);

        // 已就绪 → 幂等
        assert_eq!(h.supervisor.supervise_once().await, SuperviseOutcome::Idle);
        assert_eq!(h.spawner.spawn_count(), 1);
    }

    #[tokio::test]
    async fn detects_death_and_restarts_without_backoff() {
        let h = harness().await;
        h.supervisor.supervise_once().await;
        assert_eq!(h.supervisor.state(), KernelState::Ready);

        // 内核被外部杀掉
        h.spawner.kill_all();

        let outcome = h.supervisor.supervise_once().await;
        assert_eq!(outcome, SuperviseOutcome::Restarted);
        let st = h.supervisor.status_sync();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(st.pid, Some(2));
        assert_eq!(st.restarts, 1);
        assert_eq!(h.spawner.spawn_count(), 2);
        // 掉线重启前不该等退避（第一次立刻起）
        assert!(h.sleeper.taken().is_empty(), "不该有退避等待：{:?}", h.sleeper.taken());
    }

    #[tokio::test]
    async fn start_twice_is_idempotent_via_trait() {
        let h = harness().await;
        KernelController::start(h.supervisor.as_ref()).await.unwrap();
        KernelController::start(h.supervisor.as_ref()).await.unwrap();
        assert_eq!(h.spawner.spawn_count(), 1);
        assert_eq!(h.supervisor.state(), KernelState::Ready);
    }

    /// **回归：冷启动期间并发的 `start()` 只能起一个内核进程。**
    ///
    /// 真机上的缺陷就是这条：`start()` 原本只幂等 `Ready`，而冷启动那几秒状态是
    /// `Starting`，于是两个调用方各 spawn 一个 mihomo，第二个抢不到端口
    /// （`kernel.log` 里刷 `bind: Only one usage of each socket address`）。
    ///
    /// 用 `join!` 让两个 future **真的并发**：顺序 await 两次是测不出这条的
    /// （第二次进来时状态已经是 `Ready`，老代码也会通过）。
    ///
    /// `multi_thread` 是**必须的**，不是随手加的：生产路径跑在 Tauri 的多线程运行时上，
    /// 而本文件的 `RecordingSleeper` 是"瞬时"的（只记账、不真的睡）——
    /// 单线程运行时下，等就绪的轮询循环会**永不 yield**，把另一个任务整个饿死，
    /// 于是失败原因变成"测试脚手架把并发饿没了"，而不是被测的那条逻辑。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_starts_spawn_exactly_one_process() {
        let h = harness_slow_ready(Duration::from_millis(300)).await;

        let (a, b) = (h.supervisor.clone(), h.supervisor.clone());
        let (r1, r2) =
            tokio::join!(KernelController::start(a.as_ref()), KernelController::start(b.as_ref()),);
        r1.unwrap();
        r2.unwrap();

        assert_eq!(
            h.spawner.spawn_count(),
            1,
            "并发的 start 只能拉起一个内核进程（第二个会抢不到端口）"
        );
        assert_eq!(h.supervisor.state(), KernelState::Ready);
    }

    /// **回归：`apply()` 撞上"正在启动"时不许再 spawn 一个。**
    ///
    /// 这条正是真机那条路的形状：启动编排在拉内核（`apply()` → `start()`），
    /// 一秒后"订阅数据就绪再拉一次"（`kick_kernel` → `apply()`）进来。
    /// 老代码对任何非 Ready 状态都落进 `start()`，于是在 `Starting` 时又起一个。
    ///
    /// 同时钉住"修掉重复进程**不等于**丢掉配置热更新"：`apply()` 在等就绪之后
    /// 必须仍然把这次生成的新配置 reload 下去（那件事故本来是被第二个进程
    /// *意外*做到的，不能靠继续犯错来维持）。
    ///
    /// 同样用 `multi_thread`：理由见上一条用例。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_during_start_does_not_spawn_a_second_kernel() {
        let h = harness_slow_ready(Duration::from_millis(300)).await;

        let starter = {
            let sup = h.supervisor.clone();
            tokio::spawn(async move { KernelController::start(sup.as_ref()).await })
        };

        // 精确等到"进程已起、还没就绪"，而不是赌一个固定 sleep
        wait_until_starting(&h).await;

        let status = KernelController::apply(h.supervisor.as_ref()).await.unwrap();
        starter.await.unwrap().unwrap();

        assert_eq!(
            h.spawner.spawn_count(),
            1,
            "apply() 在启动期间**不许**再 spawn 一个内核（那会得到两个抢同一批端口的进程）"
        );
        assert_eq!(status.state, KernelState::Ready, "apply 应等到就绪后如实报 Ready");
        assert_eq!(h.supervisor.state(), KernelState::Ready);

        // 配置热更新没有丢：`apply()` 走的是 reload 分支（`PUT /configs`）
        let reloads = h
            ._server
            .received_requests()
            .await
            .expect("mock server 应记录请求")
            .iter()
            .filter(|r| r.method.as_str() == "PUT" && r.url.path() == "/configs")
            .count();
        assert_eq!(reloads, 1, "等就绪之后必须把新配置 reload 下去，不能因为不重复 spawn 就丢掉");
    }

    // ------------------------------------------------------------ 失败与退避

    #[tokio::test]
    async fn unreachable_kernel_backs_off_then_gives_up() {
        let h = harness_unreachable_rest(5, Duration::ZERO).await;

        let mut outcomes = Vec::new();
        for _ in 0..5 {
            outcomes.push(h.supervisor.supervise_once().await);
        }
        assert_eq!(h.spawner.spawn_count(), 5);
        // 前 4 次是 AttemptFailed(1..4)，第 5 次到上限 → 直接 GaveUp
        for (n, o) in outcomes.iter().take(4).enumerate() {
            match o {
                SuperviseOutcome::AttemptFailed { attempt, .. } => {
                    assert_eq!(*attempt, n as u32 + 1)
                }
                other => panic!("第 {} 次应是 AttemptFailed，得到 {other:?}", n + 1),
            }
        }
        assert!(
            matches!(outcomes[4], SuperviseOutcome::GaveUp { attempts: 5, .. }),
            "{:?}",
            outcomes[4]
        );
        // 退避：1s → 2s → 4s → 8s（第一次不等待，第 5 次前等 8s）
        let sleeps = h.sleeper.taken();
        assert_eq!(
            sleeps,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
            ],
            "退避序列不对"
        );
        assert_eq!(h.supervisor.state(), KernelState::Failed);

        // failed 之后是稳定的，不会自己复活
        assert!(matches!(
            h.supervisor.supervise_once().await,
            SuperviseOutcome::GaveUp { attempts: 5, .. }
        ));
        assert_eq!(h.spawner.spawn_count(), 5);

        // 半死的进程必须被收掉
        assert!(h.spawner.terminated().contains(&1));

        // 托盘红：状态可读，last_error 有原因
        let st = h.supervisor.status_sync();
        assert_eq!(st.state, KernelState::Failed);
        assert!(st.last_error.unwrap().contains("就绪超时"));
    }

    #[tokio::test]
    async fn backoff_ladder_uses_16s_when_limit_allows() {
        let h = harness_unreachable_rest(6, Duration::ZERO).await;
        for _ in 0..5 {
            h.supervisor.supervise_once().await;
        }
        assert_eq!(
            h.sleeper.taken(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
            ]
        );
        let outcome = h.supervisor.supervise_once().await; // 第 6 次
        assert!(matches!(outcome, SuperviseOutcome::GaveUp { attempts: 6, .. }), "{outcome:?}");
        assert_eq!(
            h.sleeper.taken().last().copied(),
            Some(Duration::from_secs(16)),
            "第 6 次重启前应退避 16s"
        );
        assert_eq!(h.supervisor.state(), KernelState::Failed);
    }

    #[tokio::test]
    async fn spawn_failure_is_counted_too() {
        let h = harness().await;
        h.spawner.set_fail_spawn(true);
        let outcome = h.supervisor.supervise_once().await;
        match outcome {
            SuperviseOutcome::AttemptFailed { attempt, message } => {
                assert_eq!(attempt, 1);
                assert!(message.contains("spawn 失败"));
            }
            other => panic!("期望 AttemptFailed，得到 {other:?}"),
        }
        assert_eq!(h.supervisor.state(), KernelState::Restarting);

        // 恢复后能自己起来，且计数归零
        h.spawner.set_fail_spawn(false);
        assert_eq!(h.supervisor.supervise_once().await, SuperviseOutcome::Started);
        assert_eq!(h.supervisor.consecutive_failures(), 0);
        assert_eq!(h.supervisor.state(), KernelState::Ready);
    }

    #[tokio::test]
    async fn supervise_loop_stops_on_gave_up_and_shutdown() {
        let h = harness_unreachable_rest(2, Duration::ZERO).await;
        let token = CancellationToken::new();
        // 连续失败上限 2 → 循环自己退出
        h.supervisor.supervise(&token).await;
        assert_eq!(h.supervisor.state(), KernelState::Failed);
        assert_eq!(h.spawner.spawn_count(), 2);

        // 取消 token 后循环立刻返回，不会去拉进程
        let h2 = harness().await;
        let token2 = CancellationToken::new();
        token2.cancel();
        h2.supervisor.supervise(&token2).await;
        assert_eq!(h2.spawner.spawn_count(), 0);
        assert_eq!(h2.supervisor.state(), KernelState::Stopped);
    }

    // ------------------------------------------------------------ stop / restart / apply

    #[tokio::test]
    async fn stop_terminates_and_resets_state() {
        let h = harness().await;
        KernelController::start(h.supervisor.as_ref()).await.unwrap();
        KernelController::stop(h.supervisor.as_ref()).await.unwrap();
        assert_eq!(h.supervisor.state(), KernelState::Stopped);
        assert_eq!(h.supervisor.pid(), None);
        assert_eq!(h.spawner.terminated(), vec![1]);
        assert!(!h.supervisor.process_alive());
    }

    #[tokio::test]
    async fn restart_stops_then_starts_new_pid() {
        let h = harness().await;
        KernelController::start(h.supervisor.as_ref()).await.unwrap();
        KernelController::restart(h.supervisor.as_ref()).await.unwrap();
        let st = h.supervisor.status_sync();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(st.pid, Some(2));
        assert_eq!(st.restarts, 1);
    }

    #[tokio::test]
    async fn apply_writes_config_and_reloads_when_ready() {
        let server = ready_server().await;
        Mock::given(method("PUT"))
            .and(path("/configs"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("apply");
        let source = Arc::new(TestSource::new(&dir));
        let spawner = Arc::new(ScriptedSpawner::new());
        let cfg = KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(Vec::<String>::new());
        let sup = KernelSupervisor::new(cfg, spawner.clone(), rest, source);
        KernelController::start(&sup).await.unwrap();

        let st = KernelController::apply(&sup).await.unwrap();
        assert_eq!(st.state, KernelState::Ready);
        // 配置真的落盘了
        assert!(dir.join("kernel.generated.yaml").exists());
        // 走的是 reload 而不是重启
        assert_eq!(spawner.spawn_count(), 1);
        let reqs = server.received_requests().await.unwrap();
        let reloads = reqs.iter().filter(|r| r.url.path() == "/configs").count();
        assert_eq!(reloads, 1);
    }

    #[tokio::test]
    async fn apply_restarts_when_reload_fails() {
        let server = ready_server().await;
        Mock::given(method("PUT"))
            .and(path("/configs"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("apply-restart");
        let source = Arc::new(TestSource::new(&dir));
        let spawner = Arc::new(ScriptedSpawner::new());
        let cfg = KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(Vec::<String>::new());
        let sup = KernelSupervisor::new(cfg, spawner.clone(), rest, source);
        KernelController::start(&sup).await.unwrap();

        let st = KernelController::apply(&sup).await.unwrap();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(spawner.spawn_count(), 2, "reload 失败应改为重启");
        assert_eq!(st.pid, Some(2));
    }

    #[tokio::test]
    async fn apply_starts_kernel_when_stopped() {
        let h = harness().await;
        let st = KernelController::apply(h.supervisor.as_ref()).await.unwrap();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(h.spawner.spawn_count(), 1);
    }

    // ------------------------------------------------------------ ★ 就绪第二关：provider

    fn provider_json(key: &str, nodes: usize) -> serde_json::Value {
        let proxies: Vec<serde_json::Value> =
            (0..nodes).map(|i| json!({ "name": format!("节点{i}") })).collect();
        json!({ "providers": { key: { "name": "机场A", "vehicleType": "HTTP", "proxies": proxies } } })
    }

    /// 起一个"REST 就绪 + provider 正常/为空"的假内核，并禁用 provider 等待（一查即判）
    async fn sup_with_providers(
        server: &MockServer,
        dir: &TempDir,
        keys: &[&str],
    ) -> (Arc<KernelSupervisor>, Arc<ScriptedSpawner>) {
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let mut source = TestSource::new(dir);
        for k in keys {
            source = source.with_provider(k);
        }
        let spawner = Arc::new(ScriptedSpawner::new());
        let mut cfg =
            KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s3cret")
                .with_args(Vec::<String>::new());
        cfg.provider_ready_timeout = Duration::ZERO;
        let sup = KernelSupervisor::new(
            cfg,
            spawner.clone(),
            rest,
            Arc::new(source) as Arc<dyn KernelConfigSource>,
        )
        .with_sleeper(Arc::new(RecordingSleeper::default()));
        (Arc::new(sup), spawner)
    }

    async fn ready_server_with_providers(key: &str, nodes: usize) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "version": "v1.19.31", "meta": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/providers/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(provider_json(key, nodes)))
            .mount(&server)
            .await;
        server
    }

    /// ★ provider 没节点时**不许报就绪**，但也**不许杀进程**（内核没错，只是订阅没抓完）
    #[tokio::test]
    async fn empty_provider_blocks_readiness_without_killing_the_kernel() {
        let server = ready_server_with_providers("sub-jp", 0).await;
        let dir = TempDir::new("provider-empty");
        let (sup, spawner) = sup_with_providers(&server, &dir, &["sub-jp"]).await;

        let err = KernelController::start(sup.as_ref()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("provider"), "{msg}");
        assert!(msg.contains("sub-jp"), "要说清是哪个 provider：{msg}");

        let st = sup.status_sync();
        assert_eq!(st.state, KernelState::Starting, "REST 通了但 provider 空 → 不是 Ready");
        assert_eq!(st.pid, Some(1), "进程必须留着");
        assert!(st.last_error.unwrap().contains("sub-jp"));
        // ★ 关键：没有把它当成"启动失败"去杀进程/重试
        assert!(spawner.terminated().is_empty(), "不该杀这个进程");
        assert_eq!(spawner.spawn_count(), 1);
        assert!(sup.verify_providers().await.is_err(), "复查仍应报错");
    }

    /// ★ provider 稍后才加载好 → 自己转 Ready，而且**不会重新拉起**（否则会跑两个内核）
    #[tokio::test]
    async fn providers_appearing_later_flip_starting_to_ready_without_respawn() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "v1" })))
            .mount(&server)
            .await;
        // 第一次查是空的，之后有 3 个节点
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        Mock::given(method("GET"))
            .and(path("/providers/proxies"))
            .respond_with(move |_req: &Request| {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .set_body_json(provider_json("sub-jp", if n == 0 { 0 } else { 3 }))
            })
            .mount(&server)
            .await;

        let dir = TempDir::new("provider-later");
        let (sup, spawner) = sup_with_providers(&server, &dir, &["sub-jp"]).await;

        assert!(KernelController::start(sup.as_ref()).await.is_err());
        assert_eq!(sup.state(), KernelState::Starting);

        // 再走一轮看护：provider 好了 → Ready，且**没有新进程**
        let outcome = sup.supervise_once().await;
        assert_eq!(outcome, SuperviseOutcome::Started, "{outcome:?}");
        assert_eq!(sup.state(), KernelState::Ready);
        assert_eq!(spawner.spawn_count(), 1, "不许重新拉起");
        assert!(spawner.terminated().is_empty());
        assert!(sup.verify_providers().await.is_ok());
    }

    #[tokio::test]
    async fn missing_provider_key_counts_as_empty() {
        // 内核只报了一个别的 provider → 我们那个还是"没节点"
        let server = ready_server_with_providers("other-provider", 5).await;
        let dir = TempDir::new("provider-missing");
        let (sup, _) = sup_with_providers(&server, &dir, &["sub-jp"]).await;
        let err = sup.verify_providers().await.unwrap_err();
        assert!(err.to_string().contains("sub-jp"), "{err}");
    }

    #[tokio::test]
    async fn provider_check_is_skipped_when_none_configured() {
        // 全直连（没有 provider）→ 不做这一关，正常就绪
        let server = ready_server().await;
        let dir = TempDir::new("provider-none");
        let (sup, spawner) = sup_with_providers(&server, &dir, &[]).await;
        KernelController::start(sup.as_ref()).await.unwrap();
        assert_eq!(sup.state(), KernelState::Ready);
        assert_eq!(spawner.spawn_count(), 1);
    }

    #[tokio::test]
    async fn provider_check_lists_every_empty_provider() {
        let server = ready_server_with_providers("sub-a", 2).await;
        let dir = TempDir::new("provider-multi");
        let (sup, _) = sup_with_providers(&server, &dir, &["sub-a", "sub-b"]).await;
        let err = sup.verify_providers().await.unwrap_err().to_string();
        assert!(err.contains("sub-b"), "{err}");
        assert!(!err.contains("sub-a"), "有节点的那个不该被点名：{err}");
    }

    // ------------------------------------------------------------ set_select / log_tail

    #[tokio::test]
    async fn set_select_maps_lane_to_group() {
        let server = ready_server().await;
        Mock::given(method("PUT"))
            .and(path("/proxies/LANE-lane-jp-auto"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("set-select");
        let source = Arc::new(
            TestSource::new(&dir)
                .with_target("lane-jp-auto", "LANE-lane-jp-auto")
                .with_target("lane-direct", "DIRECT"),
        );
        let spawner = Arc::new(ScriptedSpawner::new());
        let cfg = KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(Vec::<String>::new());
        let sup = KernelSupervisor::new(cfg, spawner, rest, source);

        // ★ 走 trait 对象调用（网关/托盘只会拿到 &dyn KernelController）
        let ctl: &dyn KernelController = &sup;
        ctl.set_select(&LaneId::new("lane-jp-auto"), "东京-03").await.unwrap();
        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body, json!({ "name": "东京-03" }));

        // 不存在的 lane
        let err = ctl.set_select(&LaneId::new("nope"), "x").await.unwrap_err();
        assert!(matches!(err, TaError::LaneUnavailable { .. }));
        // 直连 lane 没有可切换的组
        let err = ctl.set_select(&LaneId::new("lane-direct"), "x").await.unwrap_err();
        assert!(matches!(err, TaError::LaneUnavailable { .. }));
    }

    #[test]
    fn log_tail_reads_last_lines() {
        let dir = TempDir::new("log");
        let cfg = KernelProcessConfig::new("mihomo", dir.path(), dir.path(), "127.0.0.1:1", "s");
        let rest = Arc::new(KernelRest::new("127.0.0.1:1", "s").unwrap());
        let sup = KernelSupervisor::new(
            cfg,
            Arc::new(ScriptedSpawner::new()),
            rest,
            Arc::new(TestSource::new(&dir)),
        );
        // 日志还没生成 → 空串而不是报错
        assert_eq!(sup.log_tail(10).unwrap(), "");

        let log = dir.join("kernel.log");
        let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&log, body).unwrap();
        assert_eq!(sup.log_tail(3).unwrap(), "line 8\nline 9\nline 10\n");
        assert_eq!(sup.log_tail(100).unwrap().lines().count(), 10);
        assert_eq!(sup.log_tail(0).unwrap(), "");
    }

    #[test]
    fn config_validation_reports_missing_path() {
        let mut cfg = KernelProcessConfig::new("", "d", "d", "127.0.0.1:1", "s");
        cfg.program = PathBuf::new();
        let err = cfg.validate().unwrap_err();
        match err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].path, "net.kernel.path");
            }
            other => panic!("期望 ConfigInvalid，得到 {other:?}"),
        }
        // 非 loopback 控制面必须被拒绝
        let cfg2 = KernelProcessConfig::new("mihomo", "d", "d", "0.0.0.0:39090", "s");
        match cfg2.validate().unwrap_err() {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].path, "net.kernel.controller")
            }
            other => panic!("期望 ConfigInvalid，得到 {other:?}"),
        }
    }

    #[test]
    fn from_doc_reads_net_kernel_fields() {
        let doc: ConfigDoc = serde_yaml_ng::from_str(
            r#"
net:
  kernel:
    provider: external
    path: "C:/tools/mihomo.exe"
    controller: 127.0.0.1:39090
    lane_port_range: [39100, 39299]
"#,
        )
        .unwrap();
        let cfg = KernelProcessConfig::from_doc(&doc, "C:/appdata/kernel", "C:/appdata/logs");
        assert_eq!(cfg.program, PathBuf::from("C:/tools/mihomo.exe"));
        assert_eq!(cfg.controller, "127.0.0.1:39090");
        assert_eq!(cfg.work_dir, PathBuf::from("C:/appdata/kernel"));
        assert_eq!(
            cfg.config_path,
            PathBuf::from("C:/appdata/kernel").join("kernel.generated.yaml")
        );
        assert_eq!(cfg.log_path, PathBuf::from("C:/appdata/logs").join("kernel.log"));
        //  的启动命令：-d <work_dir> -f <config_path>
        assert_eq!(cfg.args.len(), 4);
        assert_eq!(cfg.args[0], "-d");
        assert_eq!(cfg.args[1], "C:/appdata/kernel");
        assert_eq!(cfg.args[2], "-f");
        assert_eq!(cfg.args[3], cfg.config_path.to_string_lossy());
        cfg.validate().unwrap();

        // provider: external 但没给 path → 配置错误
        let no_path: ConfigDoc =
            serde_yaml_ng::from_str("net: {kernel: {provider: external}}").unwrap();
        let cfg2 = KernelProcessConfig::from_doc(&no_path, "d", "d");
        assert!(cfg2.validate().is_err());
    }

    // ------------------------------------------------------------ 真进程

    /// 跨平台的"假内核"：一个能被拉起、能自己活一阵子的进程
    fn dummy_command() -> (PathBuf, Vec<String>) {
        #[cfg(windows)]
        {
            (PathBuf::from("cmd"), vec!["/c".to_string(), "ping -n 60 127.0.0.1 > nul".to_string()])
        }
        #[cfg(not(windows))]
        {
            (PathBuf::from("sleep"), vec!["60".to_string()])
        }
    }

    /// 把两行文字写进文件，再让一个真进程把它打到 stdout（跨平台）。
    /// 用文件而不是 `echo "a & echo b"`：命令行引号在 `cmd.exe` 下不可靠。
    fn printer_command(body: &str, dir: &TempDir) -> (PathBuf, Vec<String>) {
        let file = dir.join("child.log");
        std::fs::write(&file, body).expect("写子进程输出夹具");
        let p = file.to_string_lossy().to_string();
        #[cfg(windows)]
        {
            // 命令与路径分两个参数（`cmd.exe` 不认 `\"` 转义）
            (PathBuf::from("cmd"), vec!["/c".to_string(), "type".to_string(), p])
        }
        #[cfg(not(windows))]
        {
            (PathBuf::from("sh"), vec!["-c".to_string(), format!("cat '{p}'")])
        }
    }

    /// B3（同行评审）：关停路径必须**有界地**把内核最后几行读完。
    ///
    /// 读线程是 detach 的，早先的写法直接丢句柄 → 进程刚死时管道缓冲里那几行有机会丢，
    /// 而"崩溃现场的最后一行"正是 B4 存在的全部理由。现在 `is_alive` / `terminate`
    /// 都会调 `drain_logs`（带超时）；这里用真进程 + 真管道断言**最后一行没丢**。
    #[tokio::test]
    async fn child_output_is_drained_before_the_spawner_lets_go() {
        let dir = TempDir::new("drain");
        let sink = Arc::new(RecordingChildSink::default());
        let spawner =
            SystemSpawner::new().with_log_sinks(Arc::new(FixedChildLogSink(sink.clone())));

        // 故意让最后一行**没有尾随换行**：EOF 残留也必须交出来
        let (program, args) = printer_command("first-line\nlast-line-before-exit", &dir);
        let cfg = KernelProcessConfig::new(program, dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(args);
        let pid = spawner.spawn(&cfg).await.expect("拉起假内核");

        // 等它自己退出（`is_alive` 检测到退出时会 drain）
        let mut exited = false;
        for _ in 0..200 {
            if !spawner.is_alive(pid) {
                exited = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(exited, "子进程应当自己退出");

        assert!(spawner.drain_logs(pid, Duration::from_secs(5)), "不该超时");
        assert_eq!(spawner.pump_count(), 0, "读线程必须已全部结束");
        let lines = sink.taken();
        assert_eq!(
            lines,
            vec!["first-line".to_string(), "last-line-before-exit".to_string()],
            "两行都要在，尤其最后那行"
        );
        assert_eq!(spawner.shutdown_logs(Duration::from_millis(50)), 0, "没有未读完的 pid");
    }

    #[tokio::test]
    async fn system_spawner_runs_and_kills_real_process() {
        let dir = TempDir::new("real-proc");
        let (program, args) = dummy_command();
        let spawner = SystemSpawner::new();
        let cfg = KernelProcessConfig::new(program, dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(args);

        let pid = spawner.spawn(&cfg).await.unwrap();
        assert!(pid > 0);
        assert!(spawner.is_alive(pid), "假内核应该活着");
        assert!(cfg.log_path.exists(), "stdout/stderr 应落到 kernel.log");
        assert_eq!(spawner.managed(), 1);

        // ★ 防孤儿：Windows 上必须绑进 Job Object
        #[cfg(windows)]
        assert!(spawner.job_bound(pid), "Job Object 未绑定");

        spawner.terminate(pid, Duration::from_millis(300)).await.unwrap();
        assert!(!spawner.is_alive(pid), "TERM/强杀之后不该还活着");
        assert_eq!(spawner.managed(), 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn dropping_spawner_kills_child_via_job_object() {
        let dir = TempDir::new("orphan");
        let (program, args) = dummy_command();
        let cfg = KernelProcessConfig::new(program, dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(args);
        let pid = {
            let spawner = SystemSpawner::new();
            let pid = spawner.spawn(&cfg).await.unwrap();
            assert!(process_exists(pid));
            // 不 terminate，直接丢掉 spawner（模拟 TokenAgency 退出）
            pid
        };
        // Job 句柄一关，内核跟着走
        let mut gone = false;
        for _ in 0..40 {
            if !process_exists(pid) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "spawner 被 drop 后子进程应被 Job Object 收走（pid {pid} 仍存在）");
    }

    #[tokio::test]
    async fn system_spawner_supervises_real_process_end_to_end() {
        // 用 wiremock 当"假内核 REST"，真进程当"假内核"
        let server = ready_server().await;
        let rest = Arc::new(KernelRest::new(&server.uri(), "s3cret").unwrap());
        let dir = TempDir::new("e2e");
        let source = Arc::new(TestSource::new(&dir));
        let (program, args) = dummy_command();
        let mut cfg = KernelProcessConfig::new(program, dir.path(), dir.path(), "127.0.0.1:1", "s")
            .with_args(args);
        // 测试里别真等 2s
        cfg.terminate_grace = Duration::from_millis(300);
        let sup = KernelSupervisor::with_system_spawner(cfg, rest, source);

        KernelController::start(&sup).await.unwrap();
        let st = sup.status_sync();
        assert_eq!(st.state, KernelState::Ready);
        assert_eq!(st.version.as_deref(), Some("v1.18.4"));
        assert!(sup.process_alive());

        KernelController::stop(&sup).await.unwrap();
        assert_eq!(sup.state(), KernelState::Stopped);
        assert!(!sup.process_alive());
    }
}

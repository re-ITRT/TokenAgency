//! 内核进程生命周期。契约：`docs/11-internal-contracts.md` §2.1。
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

use crate::config_gen::KernelConfigSource;
use crate::rest::{normalize_controller, KernelRest};
use crate::{KernelController, KernelState, KernelStatus, ProvidersSnapshot};

/// 内核进程 + 看护策略的全部参数。
#[derive(Debug, Clone)]
pub struct KernelProcessConfig {
    /// `net.kernel.path`（`provider: external` 时由用户提供）
    pub program: PathBuf,
    /// 默认 `["-d", <work_dir>, "-f", <config_path>]`（`docs/11` §2.1）
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
}

/// 用 `std::process::Child` 真拉进程；Windows 上绑 Job Object。
#[derive(Default)]
pub struct SystemSpawner {
    children: Mutex<BTreeMap<u32, ManagedChild>>,
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
        if let Some(dir) = cfg.log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&cfg.log_path)?;
        let err_log = log.try_clone()?;

        let mut cmd = Command::new(&cfg.program);
        cmd.args(&cfg.args)
            .current_dir(&cfg.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err_log));
        // 不给子进程继承我们的环境代理设置（内核自己管出站）
        cmd.env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy");

        let child = cmd.spawn().map_err(|e| {
            TaError::Kernel(format!("拉起内核失败（{}）：{e}", cfg.program.display()))
        })?;
        let pid = child.id();

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
        let mut map = self.children.lock().expect("spawner 锁被毒化");
        match map.get_mut(&pid) {
            Some(mc) => match mc.child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    tracing::info!(pid, ?status, "内核进程已退出");
                    map.remove(&pid);
                    false
                }
                Err(e) => {
                    tracing::warn!(pid, error = %e, "查询内核进程状态失败，按已退出处理");
                    map.remove(&pid);
                    false
                }
            },
            None => false,
        }
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
        Ok(())
    }
}

#[cfg(windows)]
fn graceful_signal(pid: u32) {
    // Windows 没有 SIGTERM：先用不带 /F 的 taskkill（能优雅就优雅），失败也无所谓
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
                "内核连续启动失败，进入 failed（通道降级为仅 direct）"
            );
            KernelState::Failed
        } else {
            tracing::warn!(failures = i.consecutive_failures, error = %e, "内核启动失败，将退避重启");
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
        match self.wait_ready(pid).await {
            Ok(version) => {
                let mut i = self.lock();
                i.state = KernelState::Ready;
                i.version = Some(version.clone());
                i.consecutive_failures = 0;
                i.last_error = None;
                tracing::info!(pid, %version, "内核已就绪");
                Ok(())
            }
            Err(e) => {
                // 起不来就把这个半死的进程收掉，别留着占端口
                if let Err(te) = self.spawner.terminate(pid, self.cfg.terminate_grace).await {
                    tracing::warn!(pid, error = %te, "清理未就绪的内核进程失败");
                }
                self.record_failure(&e);
                Err(e)
            }
        }
    }

    /// 轮询 `GET /version`，超时 `ready_timeout`（默认 5s）
    async fn wait_ready(&self, pid: u32) -> Result<String> {
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
            let pause = if matches!(outcome, SuperviseOutcome::Idle) {
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

    /// 读 `<appdata>/logs/kernel.log` 的尾部 N 行（`docs/11` §2.3）
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
        if self.state() == KernelState::Ready {
            if let Some(pid) = self.pid() {
                if self.spawner.is_alive(pid) {
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
        }
        let mut i = self.lock();
        i.state = KernelState::Stopped;
        i.version = None;
        i.consecutive_failures = 0;
        tracing::info!("内核已停止");
        Ok(())
    }

    async fn restart(&self) -> Result<()> {
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
        // 2) 就绪 → reload；没起来 → 直接起
        match self.state() {
            KernelState::Ready => {
                if let Err(e) = self.rest.reload_config(&abs).await {
                    tracing::warn!(error = %e, "内核 reload 失败，改为重启");
                    self.restart().await?;
                } else {
                    tracing::info!(config = %abs, "内核配置已 reload");
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
    use crate::config_gen::KernelConfigSource;
    use crate::testutil::TempDir;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    }

    impl TestSource {
        fn new(dir: &TempDir) -> Self {
            Self {
                dir: dir.path().to_path_buf(),
                body: "# GENERATED BY TokenAgency — DO NOT EDIT\nmode: rule\n".to_string(),
                targets: BTreeMap::new(),
            }
        }

        fn with_target(mut self, lane: &str, target: &str) -> Self {
            self.targets.insert(LaneId::new(lane), target.to_string());
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
        // docs/11 §2.1 的启动命令：-d <work_dir> -f <config_path>
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

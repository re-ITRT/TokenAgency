//! 开机自启动 —— **本项目唯一一处会写操作系统持久状态的代码**。
//!
//! 依据： I2 的**唯一例外**（E1–E5）、 的 IPC 语义、 ⑥。
//!
//! 单独成文件（而不是塞进 `lifecycle.rs`）是刻意的：整个项目里只有这一处会写注册表 /
//! LaunchAgent，它必须能被**单独审查、单独测试、单独 diff**。
//!
//! # 边界规则（E1–E5）在这里怎么落地
//!
//! | 规则 | 落地方式 |
//! |---|---|
//! | E1 显式 opt-in、默认关 | 配置默认 `autostart: false`（ta-core 已保证）；这里**没有任何"启动时自动注册"的代码路径** |
//! | E2 完全可逆 | [`AutostartController::remove`] / `set(false)` 删除条目；卸载由 NSIS `customUnInstall` 调 `--uninstall-autostart`（见模块末尾） |
//! | E3 绝不静默重新注册 | [`AutostartProbe::state`] **类型上拿不到写能力**（它只持有 `&dyn AutostartRead`），所以"顺手修一下"根本写不出来 |
//! | E4 OS 是事实真源 | `actual` 来自 [`AutostartRead::read`]（真的去读系统），配置只是 `desired` |
//! | E5 不变量观察项 | 本模块**从不**在启动/退出时写系统；只有 `set()/repair()/remove()` 会写，且每个写调用都留 `info!` 审计日志 |
//!
//! # 平台机制
//!
//! * **Windows**：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `TokenAgency`，
//!   数据 = `"<exe>" --autostart --autostart-mode=<m> --autostart-delay=<s>`。
//!   **HKCU 免提权**，全程不弹 UAC、不碰 HKLM。读写走 `reg.exe`（`winreg`/`windows-sys`
//!   当前不是本 crate 的声明的依赖，见"已知限制"）。
//! * **macOS**：`~/Library/LaunchAgents/dev.tokenagency.app.plist`（`MacosLauncher::LaunchAgent` 等价物）。
//! * **Linux**：`~/.config/autostart/tokenagency.desktop`；没有桌面会话时**报 unsupported 并给原因**，
//!   不假装成功（规则 5）。
//!
//! # 已知限制（不藏着）
//!
//! 1. **非 ASCII 安装路径在非 UTF-8 控制台代码页下，`reg query` 回读可能乱码。**
//!    这只影响 `registered_command` 的**展示**：`actual`（是否注册）只看值名是否存在，
//!    `drifted` 只由 `actual` 决定，因此不会误报漂移。根治需要 `winreg` 依赖。
//! 2. **启动延迟必须在注册时写进命令行**：用户改了 `autostart_delay_s` 后，
//!    T1 的 `app_set_autostart` 需要再调一次 [`AutostartController::set`] 重新注册
//!    （用 [`AutostartProbe::matches_expected`] 可以检测"命令行里的参数和配置不一致"）。
//! 3. **卸载钩子（NSIS）不在本文件**：它属于打包配置（`tauri.conf.json` 的
//!    `bundle.windows.nsis.installerHooks` + `src-tauri/nsis/hooks.nsh`），见模块末尾的配方。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use ta_core::domain::behavior::{evaluate_autostart_drift, AutostartDrift, MAX_AUTOSTART_DELAY_S};
use ta_core::domain::{AutostartMode, ConfigDoc};
use ta_core::error::{FieldError, Result as TaResult, TaError};

/// Windows Run 键里的值名（也是不变量脚本 `Test-IsOurAutostartEntry` 的匹配依据）。
pub const AUTOSTART_VALUE_NAME: &str = "TokenAgency";

/// 应用标识（macOS LaunchAgent 的 Label / Linux desktop 文件名）。
pub const APP_ID: &str = "dev.tokenagency.app";

/// 自启动启动时带的标记参数。
pub const CLI_FLAG: &str = "--autostart";
/// 启动模式参数前缀（`--autostart-mode=tray|window`）。
pub const CLI_MODE_PREFIX: &str = "--autostart-mode=";
/// 启动延迟参数前缀（`--autostart-delay=<秒>`）。
pub const CLI_DELAY_PREFIX: &str = "--autostart-delay=";
/// 卸载钩子用的参数：只删条目，不起任何进程逻辑。
pub const CLI_UNINSTALL_FLAG: &str = "--uninstall-autostart";

// ================================================================ 只读 / 可写 分离

/// **只读**能力。E3 的机械保证从这里开始：拿到 `&dyn AutostartRead` 就写不了系统。
pub trait AutostartRead: Send + Sync {
    /// 平台机制：`windows-run-key` / `macos-launch-agent` / `linux-xdg` / `unsupported`。
    fn mechanism(&self) -> &'static str;

    /// 平台是否支持。`Err(原因)` = 不支持（UI 置灰并显示原因）。
    fn support(&self) -> std::result::Result<(), String>;

    /// 系统里**实际**注册的命令行；`Ok(None)` = 没注册。
    fn read(&self) -> std::result::Result<Option<String>, String>;
}

/// 写能力。只有 [`AutostartController`] 与卸载路径拿得到。
pub trait AutostartWrite: Send + Sync {
    fn write(&self, command: &str) -> std::result::Result<(), String>;
    fn remove(&self) -> std::result::Result<(), String>;
}

/// 后端 = 读 + 写。注意：[`AutostartProbe`] 只接受 `&dyn AutostartRead`。
pub trait AutostartBackend: AutostartRead + AutostartWrite {}
impl<T: AutostartRead + AutostartWrite> AutostartBackend for T {}

// ================================================================ 期望状态

/// 配置里的**期望**状态（E4：配置是期望，系统是事实）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartDesired {
    pub enabled: bool,
    pub mode: AutostartMode,
    pub delay_s: u32,
}

impl AutostartDesired {
    pub fn new(enabled: bool, mode: AutostartMode, delay_s: u32) -> Self {
        Self { enabled, mode, delay_s }
    }

    /// 从配置取期望状态。
    pub fn from_config(doc: &ConfigDoc) -> Self {
        let si = &doc.system_integration;
        Self { enabled: si.autostart, mode: si.autostart_mode, delay_s: si.autostart_delay_s }
    }

    /// 校验。**字段路径与错误码刻意与 `ta-core::config::validator` 保持一致**，
    /// 这样前端拿到的错误定位和 `config_apply` 时是同一条
    /// （`autostart_delay_s = 301` 在这里也必须被拒，而不是只被配置校验拒）。
    pub fn validate(&self) -> TaResult<()> {
        if self.delay_s > MAX_AUTOSTART_DELAY_S {
            return Err(TaError::ConfigInvalid {
                errors: vec![FieldError {
                    path: "system_integration.autostart_delay_s".to_string(),
                    code: DELAY_INVALID_CODE.to_string(),
                    message: format!(
                        "启动延迟必须在 0 ~ {MAX_AUTOSTART_DELAY_S} 秒之间，实际 {}（写这么大只会让开机后很久才启动）",
                        self.delay_s
                    ),
                }],
            });
        }
        Ok(())
    }

    /// 注册命令行里的模式参数值。
    fn mode_arg(&self) -> &'static str {
        match self.mode {
            AutostartMode::Tray => "tray",
            AutostartMode::Window => "window",
        }
    }
}

/// 与 ta-core `codes::INVALID_VALUE` 对齐（单元测试会守着两者相等）。
const DELAY_INVALID_CODE: &str = "invalid_value";

// ================================================================ IPC DTO

/// `app_get_autostart` / `app_set_autostart` / `app_repair_autostart` 的返回值
/// （字段一模一样，UI 开关显示的是 `actual`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartState {
    /// 配置里的期望状态
    pub desired: bool,
    /// **从操作系统读到的**实际状态
    pub actual: bool,
    /// `desired != actual`（E3：只提示，绝不自动改回）
    pub drifted: bool,
    /// `windows-run-key` | `macos-launch-agent` | `linux-xdg` | `unsupported`
    pub mechanism: String,
    /// 实际注册的命令行（便于排查"指向了一个已经不存在的 exe"）
    pub registered_command: Option<String>,
    /// 启动模式（配置值；命令行的真实值以 `registered_command` 为准）
    pub mode: AutostartMode,
    /// 启动延迟秒（配置值；同上）
    pub delay_s: u32,
    /// 平台不支持时的可读原因
    pub unsupported_reason: Option<String>,
}

// ================================================================ 只读探针

/// **只能读**的自启动视图。
///
/// 这是 E3 的机械保证：`state()` / `registered_command()` 都长在这个类型上，而它
/// 只持有 `&dyn AutostartRead` —— 想"顺手把系统改回配置"的人在这里找不到写方法。
#[derive(Clone, Copy)]
pub struct AutostartProbe<'a> {
    backend: &'a dyn AutostartRead,
    exe: &'a Path,
}

impl std::fmt::Debug for AutostartProbe<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutostartProbe")
            .field("mechanism", &self.backend.mechanism())
            .field("exe", &self.exe)
            .finish()
    }
}

impl<'a> AutostartProbe<'a> {
    pub fn mechanism(&self) -> &'static str {
        self.backend.mechanism()
    }

    pub fn support(&self) -> std::result::Result<(), String> {
        self.backend.support()
    }

    /// 系统里实际注册的命令行（真的去读系统）。
    pub fn registered_command(&self) -> std::result::Result<Option<String>, String> {
        self.backend.read()
    }

    /// 读系统 + 比对配置 → [`AutostartState`]。**绝不写系统**（E3）。
    ///
    /// 系统读取失败（注册表被权限锁死等）时：`actual=false` + `drifted` 由纯函数判定，
    /// 并把失败原因写进 `unsupported_reason` —— 不 panic，也不假装"没注册就是没开"。
    pub fn state(&self, desired: &AutostartDesired) -> AutostartState {
        let support = self.backend.support();
        let supported = support.is_ok();

        let (actual, registered_command, read_error) = if supported {
            match self.backend.read() {
                Ok(cmd) => (cmd.is_some(), cmd, None),
                Err(e) => (false, None, Some(e)),
            }
        } else {
            (false, None, None)
        };

        let drift = evaluate_autostart_drift(desired.enabled, actual, supported);

        AutostartState {
            desired: desired.enabled,
            actual,
            drifted: drift.is_drifted(),
            mechanism: self.backend.mechanism().to_string(),
            registered_command,
            mode: desired.mode,
            delay_s: desired.delay_s,
            unsupported_reason: support.err().or(read_error),
        }
    }

    /// 注册的命令行是否与当前期望（exe / 模式 / 延迟）完全一致。
    ///
    /// 与 `drifted` 的区别：`drifted` 只看"在不在"；这里看"是不是指向**我们这个 exe**
    /// 且参数一致"（换过安装目录、改过延迟的经典症状）。
    pub fn matches_expected(&self, desired: &AutostartDesired) -> bool {
        match self.backend.read() {
            Ok(Some(cmd)) => cmd.trim() == expected_command(self.exe, desired).trim(),
            _ => false,
        }
    }

    /// 漂移的**分类**（`in_sync` / `missing_on_system` / `unexpected_on_system` / `unsupported`），
    /// 供日志与事件使用（`TaEvent`）。
    pub fn drift(&self, desired: &AutostartDesired) -> AutostartDrift {
        let supported = self.backend.support().is_ok();
        let actual = supported && matches!(self.backend.read(), Ok(Some(_)));
        evaluate_autostart_drift(desired.enabled, actual, supported)
    }
}

// ================================================================ 控制器（唯一能写的地方）

/// 自启动控制器。**只有它（和卸载路径）持有写能力。**
pub struct AutostartController {
    backend: Box<dyn AutostartBackend>,
    exe: PathBuf,
}

impl std::fmt::Debug for AutostartController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutostartController")
            .field("mechanism", &self.backend.mechanism())
            .field("exe", &self.exe)
            .finish()
    }
}

impl AutostartController {
    pub fn new(backend: Box<dyn AutostartBackend>, exe: impl Into<PathBuf>) -> Self {
        Self { backend, exe: exe.into() }
    }

    /// 按当前平台选后端（可执行文件路径用 [`std::env::current_exe`]）。
    pub fn detect() -> Self {
        Self::new(detect_backend(), current_exe())
    }

    /// 只读视图（[`AutostartProbe`] 拿不到写能力）。
    pub fn probe(&self) -> AutostartProbe<'_> {
        AutostartProbe { backend: self.backend.as_ref(), exe: &self.exe }
    }

    pub fn mechanism(&self) -> &'static str {
        self.backend.mechanism()
    }

    /// 期望状态下应该注册的命令行。
    pub fn command_line(&self, desired: &AutostartDesired) -> String {
        expected_command(&self.exe, desired)
    }

    /// 用户显式开 / 关（对应 `app_set_autostart`）。
    ///
    /// * `enabled = true`：校验 → 写系统 → **重新读系统**填 `actual`（规则 6：不信写入结果）
    /// * `enabled = false`：删系统条目 → 重新读
    /// * 平台不支持且要开 → 直接报错并带原因（规则 5：不假装成功）
    ///
    /// 配置的落库由 T1 在这一步**成功之后**做 —— 这样"配置说开、系统没开"的假象
    /// 在结构上就不可能出现（规则 2）。
    pub fn set(&self, desired: &AutostartDesired, enabled: bool) -> TaResult<AutostartState> {
        desired.validate()?;

        if let Err(reason) = self.backend.support() {
            if enabled {
                return Err(TaError::Internal(format!(
                    "本平台不支持开机自启动：{reason}（机制：{}）",
                    self.backend.mechanism()
                )));
            }
            // 关：系统里本来就不会有条目，返回如实状态即可
            tracing::info!(mechanism = self.backend.mechanism(), "自启动：平台不支持，无需删除");
            return Ok(self.probe().state(&AutostartDesired { enabled: false, ..*desired }));
        }

        if enabled {
            let command = self.command_line(desired);
            self.backend.write(&command).map_err(|e| {
                TaError::Internal(format!(
                    "写入开机自启动条目失败（{}）：{e}",
                    self.backend.mechanism()
                ))
            })?;
            // 审计（E5）：每次主动写系统都留一条，便于和注册表变化对上
            tracing::info!(
                mechanism = self.backend.mechanism(),
                mode = ?desired.mode,
                delay_s = desired.delay_s,
                "自启动：已按用户操作注册"
            );
        } else {
            self.backend.remove().map_err(|e| {
                TaError::Internal(format!(
                    "删除开机自启动条目失败（{}）：{e}",
                    self.backend.mechanism()
                ))
            })?;
            tracing::info!(mechanism = self.backend.mechanism(), "自启动：已按用户操作删除");
        }

        // 规则 6：写完重新读，不假定成功
        let actual_state = self.probe().state(desired);
        Ok(actual_state)
    }

    /// 用户显式"重新对齐"（`app_repair_autostart`）：把系统状态改成与配置一致。
    ///
    /// 只有用户点了这个按钮才会执行 —— 它就是 E3 说的"交给 UI 提示 + 提供重新注册/清除"。
    pub fn repair(&self, desired: &AutostartDesired) -> TaResult<AutostartState> {
        self.set(desired, desired.enabled)
    }

    /// 删除系统条目（卸载路径 / `set(false)` 内部）。幂等。
    pub fn remove(&self, desired: &AutostartDesired) -> TaResult<AutostartState> {
        self.set(desired, false)
    }
}

/// 注册命令行：`"<exe>" --autostart --autostart-mode=<m> --autostart-delay=<s>`。
///
/// exe 路径**必须加引号**：`C:\Program Files\...` 不加引号在 Run 键里会被空格截断，
/// 表现为"开机时找不到文件"而没有任何报错。
pub fn expected_command(exe: &Path, desired: &AutostartDesired) -> String {
    format!(
        "\"{}\" {CLI_FLAG} {CLI_MODE_PREFIX}{} {CLI_DELAY_PREFIX}{}",
        exe.display(),
        desired.mode_arg(),
        desired.delay_s
    )
}

fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tokenagency"))
}

/// 按平台选后端。
pub fn detect_backend() -> Box<dyn AutostartBackend> {
    #[cfg(windows)]
    {
        Box::new(WindowsRunKey::production())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(LaunchAgentBackend::production())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(XdgAutostartBackend::production())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Box::new(UnsupportedBackend::new("本平台没有实现开机自启动机制"))
    }
}

// ================================================================ 平台后端：Windows

/// 永远不支持的后端（也给测试用）。
pub struct UnsupportedBackend {
    reason: String,
}

impl UnsupportedBackend {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

impl AutostartRead for UnsupportedBackend {
    fn mechanism(&self) -> &'static str {
        "unsupported"
    }
    fn support(&self) -> std::result::Result<(), String> {
        Err(self.reason.clone())
    }
    fn read(&self) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }
}

impl AutostartWrite for UnsupportedBackend {
    fn write(&self, _command: &str) -> std::result::Result<(), String> {
        Err(self.reason.clone())
    }
    fn remove(&self) -> std::result::Result<(), String> {
        Err(self.reason.clone())
    }
}

/// Windows：`HKCU\...\Run` 里的一个字符串值。
///
/// **key 路径可注入**（测试用临时键，绝不碰用户真实的 `Run` 键 —— 见 `tests`）。
pub struct WindowsRunKey {
    key_path: String,
    value_name: String,
}

impl WindowsRunKey {
    /// 生产用：当前用户 Run 键 / 值名 `TokenAgency`。
    pub fn production() -> Self {
        Self::new(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", AUTOSTART_VALUE_NAME)
    }

    /// 任意键（测试隔离用）。
    pub fn new(key_path: impl Into<String>, value_name: impl Into<String>) -> Self {
        Self { key_path: key_path.into(), value_name: value_name.into() }
    }

    pub fn key_path(&self) -> &str {
        &self.key_path
    }

    pub fn value_name(&self) -> &str {
        &self.value_name
    }

    /// 确保键存在（只有测试的临时键需要；生产键永远已存在）。
    pub fn ensure_key_exists(&self) -> std::result::Result<(), String> {
        self.reg(&["add", &self.key_path, "/f"]).map(|_| ())
    }

    /// 删除整个键（**只给测试清理临时键用**）。
    pub fn delete_key(&self) -> std::result::Result<(), String> {
        self.reg(&["delete", &self.key_path, "/f"]).map(|_| ())
    }

    /// 跑 `reg.exe`，返回 stdout。失败时把 stderr 带回来（可读原因）。
    fn reg(&self, args: &[&str]) -> std::result::Result<String, String> {
        let mut cmd = Command::new("reg");
        cmd.args(args);
        // GUI 程序里 spawn reg.exe 会闪一下黑窗口 —— 必须禁掉
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd.output().map_err(|e| format!("无法执行 reg.exe（{e}）"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let detail = if err.is_empty() { stdout } else { err };
            return Err(format!("reg {} 失败：{detail}", args.join(" ")));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl AutostartRead for WindowsRunKey {
    fn mechanism(&self) -> &'static str {
        "windows-run-key"
    }

    fn support(&self) -> std::result::Result<(), String> {
        // HKCU 是当前用户级：不需要提权，也不需要 HKLM —— 这是 明确要求的
        Ok(())
    }

    fn read(&self) -> std::result::Result<Option<String>, String> {
        // 键不存在 / 值不存在都返回 Ok(None)（= 没注册），不是错误
        match self.reg(&["query", &self.key_path, "/v", &self.value_name]) {
            Ok(stdout) => Ok(parse_reg_query(&stdout, &self.value_name)),
            Err(_) => Ok(None),
        }
    }
}

impl AutostartWrite for WindowsRunKey {
    fn write(&self, command: &str) -> std::result::Result<(), String> {
        self.reg(&[
            "add",
            &self.key_path,
            "/v",
            &self.value_name,
            "/t",
            "REG_SZ",
            "/d",
            command,
            "/f",
        ])
        .map(|_| ())
    }

    fn remove(&self) -> std::result::Result<(), String> {
        // 值不存在时 reg delete 会报错，但那是幂等语义下的成功
        let _ = self.reg(&["delete", &self.key_path, "/v", &self.value_name, "/f"]);
        Ok(())
    }
}

/// 解析 `reg query` 的输出：`    TokenAgency    REG_SZ    "C:\...\app.exe" --autostart ...`
///
/// 只在**值名匹配**时取值，避免把同名子串的别的值读进来。
/// 数据里可能自带空格与引号，所以按 `REG_SZ` 之后的**全部内容**取。
fn parse_reg_query(stdout: &str, value_name: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(value_name) else {
            continue;
        };
        // 值名后必须紧跟空白（否则是 TokenAgencyOther 之类的前缀撞车）
        if !rest.starts_with(' ') && !rest.starts_with('\t') {
            continue;
        }
        for kind in ["REG_SZ", "REG_EXPAND_SZ"] {
            if let Some(idx) = rest.find(kind) {
                let value = rest[idx + kind.len()..].trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

// ================================================================ 平台后端：macOS

/// macOS：`~/Library/LaunchAgents/<APP_ID>.plist`。
pub struct LaunchAgentBackend {
    path: PathBuf,
}

impl LaunchAgentBackend {
    pub fn production() -> Self {
        let dir =
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Library/LaunchAgents");
        Self::new(dir.join(format!("{APP_ID}.plist")))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn render(&self, command: &str) -> String {
        let args = split_command_line(command);
        let mut items = String::new();
        for a in &args {
            items.push_str(&format!("    <string>{}</string>\n", xml_escape(a)));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{APP_ID}</string>
  <key>ProgramArguments</key>
  <array>
{items}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#
        )
    }
}

impl AutostartRead for LaunchAgentBackend {
    fn mechanism(&self) -> &'static str {
        "macos-launch-agent"
    }

    fn support(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    fn read(&self) -> std::result::Result<Option<String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(body) => {
                Ok(parse_plist_program_arguments(&body).map(|args| join_command_line(&args)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("读取 {} 失败：{e}", self.path.display())),
        }
    }
}

impl AutostartWrite for LaunchAgentBackend {
    fn write(&self, command: &str) -> std::result::Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败：{e}", parent.display()))?;
        }
        std::fs::write(&self.path, self.render(command))
            .map_err(|e| format!("写入 {} 失败：{e}", self.path.display()))
    }

    fn remove(&self) -> std::result::Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("删除 {} 失败：{e}", self.path.display())),
        }
    }
}

/// 从 plist 里抠出 `ProgramArguments` 的 `<string>` 列表。
///
/// 刻意不引 XML 依赖：我们只写自己认识的结构，读也只读这一个数组。
fn parse_plist_program_arguments(body: &str) -> Option<Vec<String>> {
    let start = body.find("<key>ProgramArguments</key>")?;
    let after = &body[start..];
    let arr_start = after.find("<array>")? + "<array>".len();
    let arr_end = after[arr_start..].find("</array>")? + arr_start;
    let array = &after[arr_start..arr_end];

    let mut out = Vec::new();
    let mut rest = array;
    while let Some(s) = rest.find("<string>") {
        let after_s = &rest[s + "<string>".len()..];
        let Some(e) = after_s.find("</string>") else { break };
        out.push(xml_unescape(&after_s[..e]));
        rest = &after_s[e + "</string>".len()..];
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// ================================================================ 平台后端：Linux

/// Linux：`~/.config/autostart/<APP_ID>.desktop`（XDG autostart）。
pub struct XdgAutostartBackend {
    path: PathBuf,
    /// 有没有桌面会话。headless 时 `support()` 返回 Err 原因（规则 5）。
    desktop_session: bool,
}

impl XdgAutostartBackend {
    /// 生产用：`~/.config/autostart/...`，桌面会话按环境变量判断。
    pub fn production() -> Self {
        let dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("autostart");
        Self::new(dir.join(format!("{APP_ID}.desktop")), detect_desktop_session())
    }

    pub fn new(path: impl Into<PathBuf>, desktop_session: bool) -> Self {
        Self { path: path.into(), desktop_session }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn render(&self, command: &str) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=TokenAgency\n\
             Comment=TokenAgency 本地 LLM 中转站\n\
             Exec={command}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    }
}

/// 有没有图形会话。headless 服务器上装自启动条目是没有意义的（也不会被执行），
/// 所以这里如实报 unsupported，而不是假装成功。
fn detect_desktop_session() -> bool {
    ["XDG_CURRENT_DESKTOP", "DESKTOP_SESSION", "WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false))
}

impl AutostartRead for XdgAutostartBackend {
    fn mechanism(&self) -> &'static str {
        "linux-xdg"
    }

    fn support(&self) -> std::result::Result<(), String> {
        if self.desktop_session {
            Ok(())
        } else {
            Err("当前会话没有桌面环境（headless / 未设置 XDG_CURRENT_DESKTOP、DISPLAY）"
                .to_string())
        }
    }

    fn read(&self) -> std::result::Result<Option<String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(body) => Ok(parse_desktop_exec(&body)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("读取 {} 失败：{e}", self.path.display())),
        }
    }
}

impl AutostartWrite for XdgAutostartBackend {
    fn write(&self, command: &str) -> std::result::Result<(), String> {
        self.support()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败：{e}", parent.display()))?;
        }
        std::fs::write(&self.path, self.render(command))
            .map_err(|e| format!("写入 {} 失败：{e}", self.path.display()))
    }

    fn remove(&self) -> std::result::Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("删除 {} 失败：{e}", self.path.display())),
        }
    }
}

fn parse_desktop_exec(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("Exec=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ================================================================ 小工具

/// 把参数重新拼成命令行：**第一个参数（程序路径）总是加引号**，其余含空白的加引号。
///
/// 与 [`expected_command`] 的写法保持一致，这样"写出去再读回来"能逐字符相等
/// （否则 `matches_expected` 会把每个自己写的条目都判成漂移）。
fn join_command_line(args: &[String]) -> String {
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            if i == 0 || a.chars().any(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把命令行按"引号内不切分"拆成参数（只处理我们自己写出去的形式）。
fn split_command_line(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in command.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&gt;", ">").replace("&lt;", "<").replace("&amp;", "&")
}

// ================================================================ 卸载钩子配方
//
// E2：**卸载必须删条目**，否则就是"装了自启、卸了还在"，用户会直接骂人。
//
// W5 需要在两处配置（都在我范围外，故只给配方）：
//
// 1) `src-tauri/tauri.conf.json`：
//      "bundle": { "windows": { "nsis": { "installerHooks": "nsis/hooks.nsh" } } }
// 2) `src-tauri/nsis/hooks.nsh`：
//
//      !macro customUnInstall
//        ; 卸载时删除 HKCU Run 条目（免提权，与 E1 的 opt-in 一致）
//        DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "TokenAgency"
//        ; 保险：直接调用应用自己的清理入口（幂等，且不依赖注册表路径写死在脚本里）
//        ExecWait '"$INSTDIR\tokenagency.exe" --uninstall-autostart'
//      !macroend
//
// 为什么两件事都做：`DeleteRegValue` 覆盖"应用已被删掉"的情况，`ExecWait` 覆盖
// "值名/键路径以后变了"的情况（改一处代码即可，不用同步改打包脚本）。

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    // ---------------------------------------------------------- 临时目录（不引 dev-dependency）

    /// 测试用临时目录（带 Drop 清理）。
    ///
    /// 刻意不用 `tempfile`：`src-tauri/Cargo.toml` 不在 T2 的范围里（T1 正在改它加 specta），
    /// 为一个测试辅助去争那个文件不值得。用标准库 + 进程号隔离即可。
    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!("ta-t2-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("创建临时目录");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    // ---------------------------------------------------------- 测试用假后端
    //
    // 计数用 Arc 共享：控制器会吃掉 backend 的所有权（`Box<dyn ...>`），
    // 测试要能在外面继续观察"到底写了几次"（E3 的关键断言）。

    #[derive(Clone, Default)]
    struct FakeBackend {
        registered: Arc<Mutex<Option<String>>>,
        writes: Arc<AtomicUsize>,
        removes: Arc<AtomicUsize>,
        supported: bool,
        fail_write: bool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self { supported: true, ..Default::default() }
        }

        fn unsupported() -> Self {
            Self { supported: false, ..Default::default() }
        }

        fn failing_write() -> Self {
            Self { supported: true, fail_write: true, ..Default::default() }
        }

        fn registers(&self, cmd: &str) {
            *self.registered.lock().unwrap() = Some(cmd.to_string());
        }

        fn write_count(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        fn remove_count(&self) -> usize {
            self.removes.load(Ordering::SeqCst)
        }

        fn registered(&self) -> Option<String> {
            self.registered.lock().unwrap().clone()
        }
    }

    impl AutostartRead for FakeBackend {
        fn mechanism(&self) -> &'static str {
            "windows-run-key"
        }
        fn support(&self) -> std::result::Result<(), String> {
            if self.supported {
                Ok(())
            } else {
                Err("本平台不支持（测试）".to_string())
            }
        }
        fn read(&self) -> std::result::Result<Option<String>, String> {
            Ok(self.registered())
        }
    }

    impl AutostartWrite for FakeBackend {
        fn write(&self, command: &str) -> std::result::Result<(), String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_write {
                return Err("模拟写入失败".to_string());
            }
            *self.registered.lock().unwrap() = Some(command.to_string());
            Ok(())
        }
        fn remove(&self) -> std::result::Result<(), String> {
            self.removes.fetch_add(1, Ordering::SeqCst);
            *self.registered.lock().unwrap() = None;
            Ok(())
        }
    }

    fn desired(enabled: bool) -> AutostartDesired {
        AutostartDesired::new(enabled, AutostartMode::Tray, 0)
    }

    fn controller(backend: FakeBackend) -> AutostartController {
        AutostartController::new(Box::new(backend), r"C:\Program Files\TokenAgency\tokenagency.exe")
    }

    // ---------------------------------------------------------- 命令行 / 参数

    #[test]
    fn command_line_quotes_exe_and_carries_mode_and_delay() {
        let ctl = controller(FakeBackend::new());
        let cmd = ctl.command_line(&AutostartDesired::new(true, AutostartMode::Tray, 30));
        assert_eq!(
            cmd,
            r#""C:\Program Files\TokenAgency\tokenagency.exe" --autostart --autostart-mode=tray --autostart-delay=30"#
        );
        // 引号是必须的：不加引号 Run 键会在空格处截断
        assert!(cmd.starts_with('"') && cmd.contains("\" --autostart"));
        let win = ctl.command_line(&AutostartDesired::new(true, AutostartMode::Window, 0));
        assert!(win.contains("--autostart-mode=window"), "{win}");
    }

    #[test]
    fn split_command_line_keeps_quoted_path_whole() {
        let parts =
            split_command_line(r#""C:\Program Files\A B\app.exe" --autostart --autostart-delay=5"#);
        assert_eq!(
            parts,
            vec![r"C:\Program Files\A B\app.exe", "--autostart", "--autostart-delay=5"]
        );
    }

    // ---------------------------------------------------------- 延迟校验（端到端也拒绝）

    #[test]
    fn delay_over_max_is_rejected_here_too() {
        let ctl = controller(FakeBackend::new());
        let too_big = AutostartDesired::new(true, AutostartMode::Tray, MAX_AUTOSTART_DELAY_S + 1);
        let err = too_big.validate().expect_err(">300 必须被拒");
        match &err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].path, "system_integration.autostart_delay_s");
                assert_eq!(errors[0].code, DELAY_INVALID_CODE);
            }
            other => panic!("应是 ConfigInvalid，实际 {other:?}"),
        }
        // set() 也必须拒（不只是 validate() 单独拒）
        assert!(ctl.set(&too_big, true).is_err());
        assert_eq!(MAX_AUTOSTART_DELAY_S, 300, "上限来自 ta-core，不该在这里漂");
    }

    #[test]
    fn delay_error_matches_ta_core_validator_exactly() {
        // 同一条错误必须和 config_apply 时定位到同一个字段、同一个码 —— 否则前端
        // 会出现"从设置页报 A、从配置页报 B"的割裂
        let mut doc = ConfigDoc::default();
        doc.system_integration.autostart_delay_s = MAX_AUTOSTART_DELAY_S + 1;
        let report = ta_core::config::validator::validate_doc(&doc);
        let core_err = report
            .errors
            .iter()
            .find(|e| e.path == "system_integration.autostart_delay_s")
            .expect("ta-core 必须报这条错");
        let mine = AutostartDesired::from_config(&doc).validate().expect_err("我也必须报");
        match mine {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].path, core_err.path);
                assert_eq!(errors[0].code, core_err.code);
                assert_eq!(errors[0].message, core_err.message, "文案也要一致");
            }
            other => panic!("应是 ConfigInvalid，实际 {other:?}"),
        }
        // 边界内必须通过
        doc.system_integration.autostart_delay_s = MAX_AUTOSTART_DELAY_S;
        assert!(AutostartDesired::from_config(&doc).validate().is_ok());
        assert_eq!(
            AutostartDesired::from_config(&doc),
            AutostartDesired::new(false, AutostartMode::Tray, MAX_AUTOSTART_DELAY_S)
        );
    }

    // ---------------------------------------------------------- E3/E4：只读、绝不自动改回

    #[test]
    fn state_never_writes_and_reports_all_four_drift_branches() {
        // 分支 1：desired=true / actual=false → 漂移（"配置说要开，系统里却没有"）
        let b1 = FakeBackend::new();
        let c1 = AutostartController::new(Box::new(b1.clone()), PathBuf::from("app.exe"));
        let st = c1.probe().state(&desired(true));
        assert_eq!((st.desired, st.actual, st.drifted), (true, false, true));
        assert_eq!(st.mechanism, "windows-run-key");
        assert_eq!(b1.write_count(), 0, "读状态绝不能写系统（E3）");
        assert_eq!(b1.remove_count(), 0, "读状态更不能删条目（E3）");

        // 分支 2：desired=false / actual=true → 漂移（用户在别处加了条目）
        let b2 = FakeBackend::new();
        b2.registers("app.exe --autostart");
        let c2 = AutostartController::new(Box::new(b2.clone()), PathBuf::from("app.exe"));
        let st = c2.probe().state(&desired(false));
        assert_eq!((st.desired, st.actual, st.drifted), (false, true, true));
        assert_eq!(b2.remove_count(), 0, "就算多出条目也不许自己删（E3）");

        // 分支 3：一致
        let b3 = FakeBackend::new();
        b3.registers("app.exe --autostart");
        let c3 = AutostartController::new(Box::new(b3), PathBuf::from("app.exe"));
        assert!(!c3.probe().state(&desired(true)).drifted);

        // 分支 4：平台不支持 → Unsupported，不算漂移
        let b4 = FakeBackend::unsupported();
        let c4 = AutostartController::new(Box::new(b4), PathBuf::from("app.exe"));
        let st = c4.probe().state(&desired(true));
        assert!(!st.drifted);
        assert_eq!(st.mechanism, "windows-run-key");
        assert!(st.unsupported_reason.is_some());
        assert_eq!(c4.probe().drift(&desired(true)), AutostartDrift::Unsupported);
    }

    #[test]
    fn reading_state_does_not_touch_the_system_even_when_drifted() {
        // 这就是主管要验的那条："手工删掉注册表项后，app_get_autostart 必须报漂移，
        // 且不能自己把键写回去"。两种漂移方向都试一遍，写/删计数必须都是 0。
        let backend = FakeBackend::new();
        let ctl = AutostartController::new(Box::new(backend.clone()), PathBuf::from("app.exe"));

        // 配置说开、系统里没有
        let st = ctl.probe().state(&desired(true));
        assert!(st.drifted && st.desired && !st.actual);

        // 配置说关、系统里有（用户手动加的）
        backend.registers("app.exe --autostart");
        let st = ctl.probe().state(&desired(false));
        assert!(st.drifted && !st.desired && st.actual);

        assert_eq!(backend.write_count(), 0, "读状态期间不允许发生任何写入");
        assert_eq!(backend.remove_count(), 0, "读状态期间不允许发生任何删除");
        // 系统内容保持用户手动设置的样子，没有被"顺手修好"
        assert_eq!(backend.registered().as_deref(), Some("app.exe --autostart"));
    }

    // ---------------------------------------------------------- set / repair

    #[test]
    fn set_true_writes_then_rereads_from_system() {
        let ctl = controller(FakeBackend::new());
        let st = ctl.set(&desired(true), true).expect("注册应成功");
        assert!(st.actual, "actual 必须来自重新读系统");
        assert!(!st.drifted);
        assert_eq!(
            st.registered_command.as_deref(),
            Some(ctl.command_line(&desired(true)).as_str())
        );
        assert!(ctl.probe().matches_expected(&desired(true)));
        // 期望状态在 DTO 里如实带出
        assert_eq!((st.mode, st.delay_s), (AutostartMode::Tray, 0));
    }

    #[test]
    fn set_false_removes_and_is_idempotent() {
        let ctl = controller(FakeBackend::new());
        ctl.set(&desired(true), true).expect("先注册");
        let st = ctl.set(&desired(false), false).expect("删除应成功");
        assert!(!st.actual && !st.drifted);
        // 再删一次也不报错
        ctl.set(&desired(false), false).expect("幂等");
        assert!(ctl.probe().registered_command().unwrap().is_none());
    }

    #[test]
    fn set_reports_write_failure_instead_of_faking_success() {
        let ctl = AutostartController::new(
            Box::new(FakeBackend::failing_write()),
            PathBuf::from("app.exe"),
        );
        let err = ctl.set(&desired(true), true).expect_err("写入失败必须上抛");
        assert!(err.to_string().contains("写入开机自启动条目失败"), "{err}");
        // 失败后系统里没有条目 —— 不能出现"配置说开、系统没开"的假象
        assert!(ctl.probe().registered_command().unwrap().is_none());
    }

    #[test]
    fn unsupported_platform_refuses_to_enable_but_agrees_to_disable() {
        let ctl = AutostartController::new(
            Box::new(FakeBackend::unsupported()),
            PathBuf::from("app.exe"),
        );
        let err = ctl.set(&desired(true), true).expect_err("不支持时开启必须报错");
        assert!(err.to_string().contains("不支持"), "{err}");
        // 关闭是安全的（本来就没有）
        let st = ctl.set(&desired(false), false).expect("关闭不该报错");
        assert!(!st.actual);
    }

    #[test]
    fn repair_aligns_system_with_config_in_both_directions() {
        let ctl = controller(FakeBackend::new());
        // 配置开、系统没有 → repair 注册
        let st = ctl.repair(&desired(true)).expect("对齐");
        assert!(st.actual && !st.drifted);

        // 配置关、系统有 → repair 删除
        let st = ctl.repair(&desired(false)).expect("对齐");
        assert!(!st.actual && !st.drifted);
    }

    #[test]
    fn matches_expected_detects_stale_path_or_delay() {
        let ctl = controller(FakeBackend::new());
        ctl.set(&desired(true), true).expect("注册");
        assert!(ctl.probe().matches_expected(&desired(true)));
        // 延迟变了 → 命令行不再匹配（需要重新注册）
        let other = AutostartDesired::new(true, AutostartMode::Tray, 30);
        assert!(!ctl.probe().matches_expected(&other));
        // 模式变了 → 同样不匹配
        assert!(!ctl.probe().matches_expected(&AutostartDesired::new(
            true,
            AutostartMode::Window,
            0
        )));
    }

    // ---------------------------------------------------------- 文件型后端（macOS / Linux）
    // 这两个测试**在任意平台都能跑**：后端接受注入路径，测试用 tempdir，
    // 因此既验证了真实读写，又完全不碰用户的 ~/Library 与 ~/.config。

    #[test]
    fn launch_agent_round_trip_in_tempdir() {
        let dir = ScratchDir::new("autostart");
        let backend = LaunchAgentBackend::new(dir.path().join("dev.tokenagency.app.plist"));
        let ctl = AutostartController::new(
            Box::new(backend),
            PathBuf::from("/Applications/TokenAgency.app"),
        );

        assert_eq!(ctl.mechanism(), "macos-launch-agent");
        assert!(ctl.probe().registered_command().unwrap().is_none(), "初始未注册");

        let d = AutostartDesired::new(true, AutostartMode::Tray, 15);
        let st = ctl.set(&d, true).expect("写入 plist");
        assert!(st.actual, "写完必须重新读出来");
        assert_eq!(st.registered_command.as_deref(), Some(ctl.command_line(&d).as_str()));

        // 内容检查：Label / RunAtLoad / 参数
        let path = dir.path().join("dev.tokenagency.app.plist");
        let body = std::fs::read_to_string(&path).expect("读 plist");
        assert!(body.contains("<key>Label</key>"));
        assert!(body.contains(APP_ID));
        assert!(body.contains("<key>RunAtLoad</key>"));
        assert!(body.contains("--autostart-delay=15"));

        // 删除 → 文件消失
        let st = ctl.set(&d, false).expect("删除");
        assert!(!st.actual);
        assert!(!path.exists(), "删除后不该残留 plist");
        // 幂等
        ctl.remove(&d).expect("再删一次也不报错");
    }

    #[test]
    fn xdg_round_trip_and_headless_is_unsupported() {
        let dir = ScratchDir::new("autostart");
        let path = dir.path().join("dev.tokenagency.app.desktop");

        // 有桌面会话
        let backend = XdgAutostartBackend::new(&path, true);
        let ctl =
            AutostartController::new(Box::new(backend), PathBuf::from("/usr/bin/tokenagency"));
        let d = AutostartDesired::new(true, AutostartMode::Window, 0);
        let st = ctl.set(&d, true).expect("写入 desktop");
        assert!(st.actual);
        assert_eq!(st.registered_command.as_deref(), Some(ctl.command_line(&d).as_str()));
        let body = std::fs::read_to_string(&path).expect("读 desktop");
        assert!(body.contains("[Desktop Entry]"));
        assert!(body.contains("--autostart-mode=window"));
        assert!(body.contains("X-GNOME-Autostart-enabled=true"));
        ctl.set(&d, false).expect("删除");
        assert!(!path.exists());

        // headless：必须报 unsupported + 可读原因，且不写文件、不假装成功
        let headless = XdgAutostartBackend::new(dir.path().join("headless.desktop"), false);
        let ctl2 =
            AutostartController::new(Box::new(headless), PathBuf::from("/usr/bin/tokenagency"));
        let st = ctl2.probe().state(&desired(true));
        assert!(st.unsupported_reason.is_some());
        assert_eq!(st.mechanism, "linux-xdg");
        assert!(ctl2.set(&desired(true), true).is_err(), "headless 开启必须报错");
        assert!(!dir.path().join("headless.desktop").exists(), "不支持时不该写文件");
    }

    // ---------------------------------------------------------- Windows 注册表（真实读写，隔离键）

    #[cfg(windows)]
    #[test]
    fn windows_run_key_round_trip_in_scratch_key() {
        // 用临时键，**绝不碰用户真实的 Run 键**（主管机器上有 18 个既存条目）
        let scratch = format!(r"HKCU\Software\TokenAgency\Tests\Run-{}", std::process::id());
        let value_name = "TokenAgencyTestOnly";
        let backend = WindowsRunKey::new(&scratch, value_name);
        // **环境不允许就如实跳过**（本仓库口径：SKIP ≠ PASS，但环境缺能力不该把整条门禁弄红）。
        //
        // 这台机器上出现过 `reg add HKCU\... 拒绝访问`（沙箱/策略限制）——它不是产品缺陷，
        // 却会让 `cargo test --workspace` 整个红掉，进而挡住 `build.ps1` 出安装包。
        // 探测方式就是**真的试着建一次**：能建就清掉继续；不能建就说清原因并跳过本条。
        match backend.ensure_key_exists() {
            Ok(()) => {
                let _ = backend.delete_key();
            }
            Err(e) => {
                eprintln!(
                    "SKIP windows_run_key_round_trip_in_scratch_key：本环境不允许写 HKCU（{e}）"
                );
                return;
            }
        }
        backend.ensure_key_exists().expect("创建临时键");

        // 生产键的当前内容先记下来，最后要断言它没被我们改动
        let prod = WindowsRunKey::production();
        let prod_before = prod.read().expect("读生产键");

        let exe = PathBuf::from(r"C:\Program Files\TokenAgency\tokenagency.exe");
        let ctl = AutostartController::new(Box::new(backend), &exe);
        assert_eq!(ctl.mechanism(), "windows-run-key");

        // 初始：临时键里没有值 → None（不是错误）
        assert!(ctl.probe().registered_command().expect("查询").is_none());

        let d = AutostartDesired::new(true, AutostartMode::Tray, 20);
        let st = ctl.set(&d, true).expect("真实写入注册表");
        assert!(st.actual, "写入后必须能从系统读回来");
        let expected = ctl.command_line(&d);
        assert_eq!(
            st.registered_command.as_deref(),
            Some(expected.as_str()),
            "回读的命令行要与写进去的一致"
        );
        assert!(ctl.probe().matches_expected(&d));

        // 幂等删除 + 真实消失
        let st = ctl.set(&d, false).expect("删除");
        assert!(!st.actual);
        assert!(ctl.probe().registered_command().expect("查询").is_none());

        // 生产键必须原样（我们只写了临时键）
        assert_eq!(prod_before, prod.read().expect("读生产键"), "测试污染了真实 Run 键！");

        // 清理临时键
        WindowsRunKey::new(&scratch, value_name).delete_key().expect("清理临时键");
    }

    #[test]
    fn parse_reg_query_extracts_value_after_type() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    TokenAgency    REG_SZ    \"C:\\Program Files\\TokenAgency\\tokenagency.exe\" --autostart --autostart-delay=30\r\n\r\n";
        assert_eq!(
            parse_reg_query(out, "TokenAgency").as_deref(),
            Some(
                r#""C:\Program Files\TokenAgency\tokenagency.exe" --autostart --autostart-delay=30"#
            )
        );
        // 前缀撞车不能误取（TokenAgencyOther 不是 TokenAgency）
        let other = "    TokenAgencyOther    REG_SZ    nope\r\n";
        assert_eq!(parse_reg_query(other, "TokenAgency"), None);
        // 找不到就是 None
        assert_eq!(parse_reg_query("ERROR: 找不到指定的注册表项或值。\r\n", "TokenAgency"), None);
    }

    #[test]
    fn plist_and_desktop_parsers_round_trip() {
        let dir = ScratchDir::new("autostart");
        let plist = LaunchAgentBackend::new(dir.path().join("a.plist"));
        let cmd = r#""/Applications/TA.app/Contents/MacOS/ta" --autostart --autostart-mode=tray --autostart-delay=5"#;
        plist.write(cmd).expect("写");
        assert_eq!(plist.read().expect("读").as_deref(), Some(cmd));

        let xdg = XdgAutostartBackend::new(dir.path().join("a.desktop"), true);
        xdg.write(cmd).expect("写");
        assert_eq!(xdg.read().expect("读").as_deref(), Some(cmd));
    }

    #[test]
    fn unsupported_backend_reports_reason_on_every_read() {
        let b = UnsupportedBackend::new("本平台没有实现");
        assert_eq!(b.mechanism(), "unsupported");
        assert!(b.support().is_err());
        assert_eq!(b.read().unwrap(), None);
    }

    #[test]
    fn missing_files_are_treated_as_not_registered() {
        let dir = ScratchDir::new("autostart");
        let p = LaunchAgentBackend::new(dir.path().join("missing.plist"));
        assert_eq!(p.read().expect("读"), None);
        let x = XdgAutostartBackend::new(dir.path().join("missing.desktop"), true);
        assert_eq!(x.read().expect("读"), None);
    }
}

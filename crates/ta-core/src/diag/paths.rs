//! ★ **C1 判定**：日志落点—— 纯决策，不做 IO。
//!
//! 为什么 C1 需要一个"判定函数"而不是一句"记得把日志写到用户的日志目录"：
//! 托盘应用**没有控制台**，日志一旦写错地方就等于没有日志，
//! 而且这件事**不会报错** —— 程序照常跑，只是出问题时谁也找不到文件。
//! 所以三平台路径在这里成为**可被引用的常量级事实**，由
//! `src-tauri/src/logging.rs`（E3）传真实 env 进来落地。
//!
//! **本模块不读环境变量**：路径规则读 `LOCALAPPDATA` / `HOME` 是 IO 边界的事，
//! 由调用方传进来 —— 这样规则本身可以被穷举测试（三平台 × 缺 env）。
//!
//! | 平台 | 目录 |
//! |---|---|
//! | Windows | `%LOCALAPPDATA%\TokenAgency\logs\` |
//! | macOS | `~/Library/Logs/TokenAgency/` |
//! | Linux | `~/.local/state/tokenagency/logs/` |

use std::path::{Path, PathBuf};

use super::rotation;

/// 目标平台。**不由 `cfg!` 决定**（那就是"无法测试的规则"）——
/// 由调用方（应用层）按 `cfg!(target_os)` 选一个传进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    /// 本机平台。应用层用它喂 [`log_dir`]。
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::MacOs => "macos",
            Platform::Linux => "linux",
        }
    }

    /// 该平台日志目录依赖哪个环境变量（缺失时用来生成可读的告警）。
    pub const fn base_env(&self) -> &'static str {
        match self {
            Platform::Windows => "LOCALAPPDATA",
            Platform::MacOs | Platform::Linux => "HOME",
        }
    }
}

/// 日志目录算不出来的原因。**宁可显式失败，也不猜一个目录写进去** ——
/// 猜错的地方写日志，等于把"用户能一键打开"变成"用户永远找不到"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingBaseDir {
    pub platform: Platform,
    pub env: &'static str,
}

impl std::fmt::Display for MissingBaseDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "环境变量 {} 缺失，无法确定日志目录（平台 {}）", self.env, self.platform.as_str())
    }
}

/// 日志目录。
///
/// - Windows 用 `LOCALAPPDATA`（**不是** `APPDATA`：日志不该跟着漫游配置到处跑）；
/// - macOS 用 `~/Library/Logs/TokenAgency`（Console.app 认这个位置）；
/// - Linux 用 `~/.local/state/tokenagency/logs`。
pub fn log_dir(
    platform: Platform,
    local_app_data: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, MissingBaseDir> {
    match platform {
        Platform::Windows => {
            let base = local_app_data.ok_or(MissingBaseDir { platform, env: "LOCALAPPDATA" })?;
            Ok(base.join("TokenAgency").join("logs"))
        }
        Platform::MacOs => {
            let base = home.ok_or(MissingBaseDir { platform, env: "HOME" })?;
            Ok(base.join("Library").join("Logs").join("TokenAgency"))
        }
        Platform::Linux => {
            let base = home.ok_or(MissingBaseDir { platform, env: "HOME" })?;
            Ok(base.join(".local").join("state").join("tokenagency").join("logs"))
        }
    }
}

/// 正在写的日志文件的**全路径**（文件名见 [`rotation::CURRENT_FILE_NAME`]）。
pub fn current_log_file(dir: &Path) -> PathBuf {
    dir.join(rotation::CURRENT_FILE_NAME)
}

/// 轮转归档的全路径（本地时区，与 [`rotation::archive_name_local`] 同口径）。
pub fn archive_log_file(dir: &Path, now_ms: i64, local_offset_minutes: i32) -> PathBuf {
    dir.join(rotation::archive_name_local(now_ms, local_offset_minutes))
}

/// 崩溃文件的全路径（`crash-<yyyymmdd-HHmmss>.log`）。
pub fn crash_log_file(dir: &Path, now_ms: i64, local_offset_minutes: i32) -> PathBuf {
    dir.join(rotation::crash_name_local(now_ms, local_offset_minutes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 把路径规范化成 `a/b/c` 形式，便于与 的表格逐字比对。
    fn norm(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    fn windows_dir() -> PathBuf {
        log_dir(
            Platform::Windows,
            Some(Path::new(r"C:\Users\somebody\AppData\Local")),
            Some(Path::new(r"C:\Users\somebody")),
        )
        .expect("Windows 有 LOCALAPPDATA 时必须能算出日志目录")
    }

    fn macos_dir() -> PathBuf {
        log_dir(Platform::MacOs, None, Some(Path::new("/Users/somebody"))).expect("macOS 有 HOME")
    }

    fn linux_dir() -> PathBuf {
        log_dir(Platform::Linux, None, Some(Path::new("/home/somebody"))).expect("Linux 有 HOME")
    }

    #[test]
    fn three_platforms_match_the_contract_table() {
        assert_eq!(
            norm(&windows_dir()),
            "C:/Users/somebody/AppData/Local/TokenAgency/logs",
            "：Windows 是 %LOCALAPPDATA%\\TokenAgency\\logs（不是 APPDATA）"
        );
        assert_eq!(
            norm(&macos_dir()),
            "/Users/somebody/Library/Logs/TokenAgency",
            "：macOS 是 ~/Library/Logs/TokenAgency"
        );
        assert_eq!(
            norm(&linux_dir()),
            "/home/somebody/.local/state/tokenagency/logs",
            "：Linux 是 ~/.local/state/tokenagency/logs"
        );
    }

    /// Windows 的判据是 `LOCALAPPDATA` —— 传 `APPDATA` 进来会算出另一个目录，
    /// 这条测试把"用哪个环境变量"固定下来（改错了会静默把日志写到漫游配置里）。
    #[test]
    fn windows_uses_local_app_data_not_roaming() {
        let roaming = Path::new(r"C:\Users\somebody\AppData\Roaming");
        let dir = log_dir(Platform::Windows, Some(roaming), Some(Path::new(r"C:\x"))).unwrap();
        assert!(
            norm(&dir).contains("/AppData/Roaming/"),
            "传入什么基目录就用什么 —— 规则本身不猜；选 LOCALAPPDATA 是 E3 的调用点责任"
        );
        assert_eq!(Platform::Windows.base_env(), "LOCALAPPDATA");
    }

    #[test]
    fn missing_base_dir_is_an_explicit_error_not_a_guess() {
        let e = log_dir(Platform::Windows, None, Some(Path::new(r"C:\x"))).unwrap_err();
        assert_eq!(e.env, "LOCALAPPDATA");
        assert_eq!(e.platform, Platform::Windows);
        assert!(e.to_string().contains("LOCALAPPDATA"));

        for p in [Platform::MacOs, Platform::Linux] {
            let e = log_dir(p, None, None).unwrap_err();
            assert_eq!(e.env, "HOME");
        }
    }

    #[test]
    fn file_names_come_from_rotation_constants() {
        let d = windows_dir();
        assert_eq!(d.join("tokenagency.log"), current_log_file(&d));
        assert_eq!(
            norm(&current_log_file(&d)),
            "C:/Users/somebody/AppData/Local/TokenAgency/logs/tokenagency.log",
            "：文件名是 tokenagency.log"
        );
        assert_eq!(
            current_log_file(&d).file_name().and_then(|s| s.to_str()),
            Some(rotation::CURRENT_FILE_NAME)
        );
    }

    #[test]
    fn archive_and_crash_names_are_local_time_stamped() {
        // 2026-02-11T03:04:05Z，东八区 → 20260211-110405
        let ms = chrono::DateTime::parse_from_rfc3339("2026-02-11T03:04:05Z")
            .unwrap()
            .timestamp_millis();
        let d = Path::new("/logs");
        assert_eq!(
            archive_log_file(d, ms, 480).file_name().and_then(|s| s.to_str()),
            Some("tokenagency.20260211-110405.log")
        );
        assert_eq!(
            crash_log_file(d, ms, 480).file_name().and_then(|s| s.to_str()),
            Some("crash-20260211-110405.log")
        );
        // 轮转分类认识自己生成的名字（否则"该删的没删/不该删的删了"）
        assert_eq!(
            rotation::classify_file_name("tokenagency.20260211-110405.log"),
            rotation::FileKind::Rotated
        );
    }

    /// 契约一致性：本模块的规则必须能在日志字段表里找到依据。
    ///
    /// 只断言"文档确实写着这三个路径"，不解析表格 —— 表格换排版不该让守卫变红，
    /// 但**删掉/改掉路径规则**必须让守卫变红。
    #[test]
    fn the_doc_still_states_these_three_paths() {
        let doc = repo_doc("docs/14-observability.md");
        for needle in [
            "%LOCALAPPDATA%\\TokenAgency\\logs",
            "~/Library/Logs/TokenAgency/",
            "~/.local/state/tokenagency/logs/",
            "tokenagency.log",
        ] {
            assert!(
                doc.contains(needle),
                "日志字段契约里找不到 `{needle}` —— 文档与本模块的规则不同步了"
            );
        }
    }

    fn repo_doc(rel: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
        std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("读不到 {}（合约文件缺失，守卫失去依据）：{e}", p.display()))
    }
}

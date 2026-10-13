//! 两条守卫测试（`log_field_guard` / `error_log_guard`）共用的仓库扫描工具。
//!
//! 放在 `tests/common/`（不是 `tests/common.rs`）是 cargo 的约定：
//! 这个目录**不会**被当成独立测试目标，只有 `mod common;` 引用它时才编译。

// 共享测试工具模块的标准做法：每个引用它的测试目标只用到其中一部分，
// 未用到的那部分会各报一次 `dead_code`。**这个文件本身不是产品代码**，
// 让"另一个测试用不到某个 helper"变成编译告警没有意义 —— 只会逼人为了消警而写假用途。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 一个待扫描的源文件。
pub struct SourceFile {
    /// 相对仓库根的路径，**统一用 `/` 分隔**（断言消息里要给人看）
    pub rel: String,
    pub text: String,
}

/// 仓库根（`crates/ta-core/` 往上两层）。
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// 契约要求扫描的范围：`crates/**/*.rs` 与 `src-tauri/src/**/*.rs`（[14 §5.5]）。
pub fn source_files() -> Vec<SourceFile> {
    let root = repo_root();
    let mut out = Vec::new();
    for base in ["crates", "src-tauri/src"] {
        walk(&root.join(base), &root, &["rs"], &mut out);
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// 前端源码：`src/**/*.ts` 与 `src/**/*.tsx`（命令面守卫要数"页面真实引用了哪些命令"）。
///
/// 与 [`source_files`] **共用同一份遍历实现**（`walk`）—— 多一套遍历就会多一套忽略规则，
/// 那正是"两个真相源"的老问题。
pub fn frontend_source_files() -> Vec<SourceFile> {
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &root, &["ts", "tsx"], &mut out);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// 读一个仓库内文件。
pub fn read_repo_file(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

/// 按扩展名遍历（`exts` 不带点）。**全仓只有这一份遍历实现**。
fn walk(dir: &Path, root: &Path, exts: &[&str], out: &mut Vec<SourceFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // 生成的/第三方的目录不扫：扫了只会制造噪声
            if matches!(name.as_str(), "target" | "node_modules" | ".git" | "dist" | "gen") {
                continue;
            }
            walk(&path, root, exts, out);
            continue;
        }
        if !exts.iter().any(|e| name.ends_with(&format!(".{e}"))) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        out.push(SourceFile { rel, text });
    }
}

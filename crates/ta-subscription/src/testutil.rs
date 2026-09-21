//! 测试用临时目录。
//!
//! `ta-subscription` 的 `Cargo.toml` 里没有 `tempfile`，所以这里给测试一个够用的
//! 临时目录：唯一名字 + `Drop` 清理。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("ta-sub-test-{tag}-{}-{n}", std::process::id()));
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        std::fs::create_dir_all(&path).expect("创建测试临时目录失败");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn temp_dir_is_unique_and_cleaned() {
    let a = TempDir::new("x");
    let b = TempDir::new("x");
    assert_ne!(a.path(), b.path());
    assert!(a.path().is_dir());
    let f = a.join("f.txt");
    std::fs::write(&f, b"hi").unwrap();
    drop(a);
    assert!(!f.exists());
    drop(b);
}

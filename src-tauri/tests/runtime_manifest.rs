//! 加载期清单守卫（Windows）。
//!
//! 存在理由：`comctl32.dll` 的 `TaskDialogIndirect` 只由 Common-Controls v6 导出，
//! 而 v6 必须靠应用程序清单里的 `dependentAssembly` 才会被激活。`tauri-build` 只给
//! bin 目标嵌清单，测试目标拿不到 —— 于是测试进程在加载期就以 0xc0000139
//! (`STATUS_ENTRYPOINT_NOT_FOUND`) 退出，一个测试都跑不到。
//!
//! `src-tauri/build.rs` 通过 `cargo:rustc-link-arg-tests` 补上
//! `windows-test.manifest`。本文件是那个指令生效所必需的真实 Test 目标，
//! 同时验证清单确实进了当前映像。

/// `TaskDialogIndirect` 属于 comctl32 v6；若映像加载时激活的是 v5，本进程根本
/// 起不来，任何断言都不会执行。所以这里断言的就是"清单已生效"这一事实本身：
/// 能拿到进程映像里的 RT_MANIFEST 资源，并且里面声明了 Common-Controls 6.0。
#[cfg(windows)]
#[test]
fn test_image_declares_common_controls_v6() {
    let manifest = read_own_manifest().expect("当前测试映像应当内嵌 RT_MANIFEST 资源");
    assert!(
        manifest.contains("Microsoft.Windows.Common-Controls"),
        "测试映像的清单里缺少 Common-Controls 依赖，运行时会在加载 comctl32 时崩溃"
    );
    assert!(
        manifest.contains("6.0.0.0"),
        "Common-Controls 依赖版本必须 >= 6.0.0.0，否则 TaskDialogIndirect 不可用"
    );
}

/// 从自身映像读 RT_MANIFEST（资源类型 24，ID 1）。
///
/// 用 Win32 资源 API 而不是解析 PE 文件，避免把测试绑死在文件布局上。
#[cfg(windows)]
fn read_own_manifest() -> Option<String> {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn FindResourceW(module: *mut c_void, name: *const u16, kind: *const u16) -> *mut c_void;
        fn LoadResource(module: *mut c_void, res: *mut c_void) -> *mut c_void;
        fn LockResource(res: *mut c_void) -> *mut c_void;
        fn SizeofResource(module: *mut c_void, res: *mut c_void) -> u32;
    }

    // MAKEINTRESOURCEW(24) = RT_MANIFEST，MAKEINTRESOURCEW(1) = CREATEPROCESS_MANIFEST_RESOURCE_ID
    const RT_MANIFEST: *const u16 = 24usize as *const u16;
    const CREATEPROCESS_MANIFEST_RESOURCE_ID: *const u16 = 1usize as *const u16;

    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        if module.is_null() {
            return None;
        }
        let res = FindResourceW(module, CREATEPROCESS_MANIFEST_RESOURCE_ID, RT_MANIFEST);
        if res.is_null() {
            return None;
        }
        let loaded = LoadResource(module, res);
        if loaded.is_null() {
            return None;
        }
        let data = LockResource(loaded) as *const u8;
        let len = SizeofResource(module, res) as usize;
        if data.is_null() || len == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, len);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[cfg(not(windows))]
fn read_own_manifest() -> Option<String> {
    None
}

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut attributes = tauri_build::Attributes::new();

    if target_os == "windows" {
        // Windows 清单必须由“链接参数”统一供给，而不是交给 tauri-build 嵌进 bin。
        //
        // 背景：comctl32.dll 的 TaskDialogIndirect 只由 Common-Controls v6 导出，v6 必须
        // 靠应用程序清单里的 dependentAssembly 才会被激活。tauri-build 默认只把清单塞进
        // `cargo:rustc-link-arg-bins=.../resource.lib`，于是 bin 有清单、测试目标没有：
        // 测试进程加载到 comctl32 v5 → 缺 TaskDialogIndirect → 在跑第一个测试之前就以
        // 0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND) 退出。`cargo test -p tokenagency --lib`
        // 以前必须靠 `--exclude tokenagency` 绕开，就是这个问题。
        //
        // 为什么用通用 rustc-link-arg：`cargo:rustc-link-arg-tests` 只作用于 [[test]] 集成
        // 目标，覆盖不到 lib 单元测试 harness（target kind = lib）——实测过，那条指令下
        // harness 依旧 0xc0000139。通用指令是唯一能到达它的通道，代价是它同样作用于 bin，
        // 所以必须同时让 tauri-build 不再嵌清单，否则 bin 会拿到两份 MANIFEST 资源，
        // 链接期直接 CVTRES CVT1100 / LNK1123。图标与版本信息仍由 tauri-build 的
        // resource.lib 提供，不受影响。
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());

        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }

    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

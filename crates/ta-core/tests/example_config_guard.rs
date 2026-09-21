//! 防漂移守卫：`config.example.yaml` 必须与当前 Rust 类型保持一致。
//!
//! **为什么需要这个**：示例配置原本是手写的，结果被员工 P1 实测抓出
//! 三处硬错误（`net.system_proxy: off` 在 YAML 1.2 里是字符串不是 bool、
//! `lanes[].filter` 写成 mapping 而类型是正则串、`failover.on` 用了 `http_429`
//! 而枚举实际是 `http429`），外加一批字段名不对、**静默落进 `extra` 未知字段、
//! 运行时不生效**却没有报错的历史字段。
//!
//! 这类"不报错但不生效"的配置错误最阴险 —— 用户以为配上了，其实没有。
//! 所以这里加两道守卫，改结构体忘了同步示例 → CI 直接红。

use std::path::PathBuf;

fn example_path() -> PathBuf {
    // crates/ta-core/ -> 仓库根
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("config.example.yaml")
}

fn read_example() -> String {
    let p = example_path();
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("读不到 config.example.yaml（{}）：{e}", p.display()))
}

/// 守卫 1：示例必须是**当前类型能读通**的合法配置。
#[test]
fn example_config_parses() {
    let text = read_example();
    let doc = ta_core::config::from_yaml(&text).unwrap_or_else(|e| {
        panic!("config.example.yaml 无法被当前类型解析 —— 字段名/类型已漂移：{e}")
    });

    // 形状与引用完整性都不能有错误
    let report = ta_core::config::validate(&doc);
    assert!(report.errors.is_empty(), "示例配置存在校验错误（路径：{:?}）", report.error_paths());
}

/// 守卫 2：示例里**不能有静默失效的未知字段**。
///
/// 手法：`semantic_eq` 做全字段（含各实体的 `extra`）语义比对。
/// 如果示例里有任何字段因为名字写错而落进了 `extra`，就会与 `default_doc()` 不等。
#[test]
fn example_config_has_no_unknown_fields() {
    let doc = ta_core::config::from_yaml(&read_example())
        .expect("示例配置应能解析（若这条失败，先看 parses 那个测试的报错）");
    let canonical = ta_core::config::default_doc();

    assert!(
        ta_core::config::semantic_eq(&doc, &canonical),
        "config.example.yaml 与 default_doc() 语义不一致。\n\
         最可能的原因：某处字段名与结构体不符，值落进了 `extra` 未知字段 —— \
         这不会报错，但运行时不生效。\n\
         修复：pwsh -File scripts/gen-example-config.ps1 重新生成。"
    );
}

/// 守卫 3：生成物必须是**幂等**的 —— 生成器与守卫配合才有意义。
#[test]
fn example_config_roundtrip_is_stable() {
    let doc = ta_core::config::from_yaml(&read_example()).expect("示例应能解析");
    let yaml1 = ta_core::config::to_yaml(&doc).expect("应能序列化");
    let doc2 = ta_core::config::from_yaml(&yaml1).expect("二次解析应成功");
    let yaml2 = ta_core::config::to_yaml(&doc2).expect("应能再次序列化");
    assert_eq!(yaml1, yaml2, "配置序列化必须是幂等的（否则提交会产生噪声 diff）");
}

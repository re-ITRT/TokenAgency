//! 从 `config::default_doc()` 生成配置示例的 YAML。
//!
//! 用途：**示例配置的唯一真源是 Rust 类型**，手工维护必然漂移
//! （W0 手写的那版就有三处硬错误 + 一批静默失效字段，被员工 P1 实测抓出来）。
//!
//! 用法：
//!   cargo run -p ta-core --example dump_default_config
//!
//! 配套的防漂移测试在 `tests/example_config_guard.rs`。

fn main() {
    let doc = ta_core::config::default_doc();
    match ta_core::config::to_yaml(&doc) {
        Ok(yaml) => {
            // 头注释由调用方（脚本）添加，这里只输出纯 YAML，
            // 以便与 config.example.yaml 的内容逐行可比对。
            print!("{yaml}");
        }
        Err(e) => {
            eprintln!("生成 YAML 失败: {e}");
            std::process::exit(1);
        }
    }
}

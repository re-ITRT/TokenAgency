//! 守卫：**报告一个 `TaError` 就必须同时记 `code`**。
//!
//! 为什么需要它：`TaError` 的 `Display` 只有中文文案（这是刻意的， ——
//! 给用户看的文案里不该出现内部编号），所以 `tracing::warn!(error = %e, …)`
//! 打出来的日志**搜不到 `TA-4001`**。而错误码总表才是故障索引：
//! 排查的第一步就是"搜这个码，看所有相关日志"。
//!
//! 这条守卫把"我们决定要能按码检索"从一个**愿望**变成一条**会让 CI 红的规则**。

mod common;

use ta_core::diag::scan;

/// 允许不记 `code` 的既有站点，每一项都必须写清"为什么没有码"。
///
/// 键是 `(仓库内路径后缀, message 片段)`。用 message 而不是行号：
/// 行号会因为无关改动漂移，message 变了说明这里的语义真的改了，**应该重新审一遍**。
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "crates/ta-core/src/routing/engine.rs",
        "ua_matches 正则非法",
        "regex::Error 没有 TA 码；配置写错的正则不值得单开一个码",
    ),
    (
        "crates/ta-kernel/src/process.rs",
        "查询内核进程状态失败",
        "std::process::Child::try_wait 的 io::Error 没有 TA 码",
    ),
    (
        "crates/ta-kernel/src/process.rs",
        "try_wait 失败",
        "std::process::Child::try_wait 的 io::Error 没有 TA 码",
    ),
    (
        "crates/ta-secrets/src/keyring_store.rs",
        "钥匙串条目索引损坏",
        "serde_json::Error 没有 TA 码（索引坏了要降级继续跑，不是致命错误）",
    ),
    (
        "crates/ta-subscription/src/fetch.rs",
        "订阅下载失败（不可重试）",
        "AttemptError（本 crate 私有枚举）没有 TA 码；转成 TaError 的那一层才有",
    ),
    (
        "crates/ta-subscription/src/fetch.rs",
        "订阅下载失败，退避后重试",
        "AttemptError（本 crate 私有枚举）没有 TA 码；转成 TaError 的那一层才有",
    ),
    (
        "crates/ta-subscription/src/fetch.rs",
        "订阅下载最终失败",
        "AttemptError（本 crate 私有枚举）没有 TA 码",
    ),
    ("crates/ta-subscription/src/parse.rs", "订阅过滤正则非法", "regex::Error 没有 TA 码"),
    (
        "src-tauri/src/lifecycle.rs",
        "广播 toast 事件失败",
        "tauri::Error 没有 TA 码（属 T2 的文件，OB1 不落笔）",
    ),
    (
        "src-tauri/src/lifecycle.rs",
        "系统通知发送失败",
        "tauri::Error 没有 TA 码（属 T2 的文件，OB1 不落笔）",
    ),
    // ⚠️ `src-tauri/src/boot.rs` 的两条豁免**不在这里** —— 它们随 `boot.rs` 一起落地。
    //
    // 起因（我犯的顺序错误）：`d843946` 先把这两条登记进 master，而 `boot.rs` 只存在于当时
    // **尚未合并**的 `wp/e3` 里 → 本文件末尾"豁免不许腐烂"那条守卫在干净 master 上**直接红**
    // （每条豁免都必须命中一处真实日志）。这是"契约条目依赖未合并文件"的典型顺序耦合，
    // 由 wp/e3 的评审员从源码推出并报了回来。
    //
    // 规则：**EXEMPT 条目必须与其引用的文件在同一个提交/同一个合并里落地。**
    //
    // 下面是**环境/系统层**失败：`tauri::Error` 没有 TA 码（它包的是 Windows/WebView2
    // 的错误），而这几条日志的正文已经写清"是什么失败 + 常见原因 + 下一步"，比一个自造的码有用。
    (
        "src-tauri/src/lib.rs",
        "主窗口创建失败",
        "tauri::Error 没有 TA 码；窗口建不出来是环境问题（WebView2 运行时缺失 / 数据目录不可写）",
    ),
    (
        "src-tauri/src/lib.rs",
        "托盘不可用（装不上）",
        "tauri::Error 没有 TA 码；托盘是便利功能，装不上就降级继续跑",
    ),
    (
        "src-tauri/src/lib.rs",
        "托盘数据源切换失败",
        "tauri::Error 没有 TA 码；同样只是托盘状态不刷新，不影响界面与网关",
    ),
];

fn exemption_for(rel: &str, message: Option<&str>) -> Option<&'static str> {
    let msg = message.unwrap_or("");
    EXEMPT
        .iter()
        .find(|(path, needle, _)| rel.ends_with(path) && msg.contains(needle))
        .map(|(_, _, why)| *why)
}

#[test]
fn error_logs_must_carry_a_code() {
    let mut violations: Vec<String> = Vec::new();
    let mut exempted = 0usize;
    for file in common::source_files() {
        for site in scan::error_log_sites(&file.text) {
            if site.has_code {
                continue;
            }
            match exemption_for(&file.rel, site.message.as_deref()) {
                Some(_) => exempted += 1,
                None => violations.push(format!(
                    "{}:{}  {}!(error = {}, {:?})",
                    file.rel,
                    site.line,
                    site.macro_name,
                    site.error_expr,
                    site.message.as_deref().unwrap_or("<无 message>")
                )),
            }
        }
    }
    assert!(
        violations.is_empty(),
        "发现 {} 处 `error = …` 但缺少 `code = …` 的错误日志：\n{}\n\n\
         修法（首选）：把 `TaError` 的码一起记上 ——\n\
         tracing::warn!(request_id = %rid, attempt, code = e.code(), retryable = e.retryable(), error = %e, \"…\");\n\
         若这个错误**不是** TaError（io/serde/regex/tauri…），把它写进本文件的 EXEMPT 白名单并注明理由\
         （当前已豁免 {} 处）。",
        violations.len(),
        violations.join("\n"),
        exempted
    );
    assert!(exempted > 0, "豁免清单不该是空的空转；若确实没有豁免项，请删掉 EXEMPT 里的死条目");
}

/// 守卫必须**真的能红**：故意写一处缺 `code` 的错误日志，它要能认出来。
#[test]
fn the_guard_actually_detects_a_missing_code() {
    let bad = r#"
fn f(e: &TaError) {
    tracing::warn!(error = %e, "缺 code 的错误日志");
    tracing::warn!(code = e.code(), error = %e, "有 code");
}
"#;
    let sites = scan::error_log_sites(bad);
    assert_eq!(sites.len(), 2);
    assert!(!sites[0].has_code, "第一处缺 code → 必须被标成违规");
    assert!(sites[1].has_code, "第二处有 code → 不该误报");
}

/// 豁免清单不能腐烂：每条豁免都必须**仍然命中一处真实的日志**，
/// 否则说明代码变了、这条豁免该删了（否则白名单会越滚越大，最后形同虚设）。
#[test]
fn every_exemption_still_matches_a_real_site() {
    let files = common::source_files();
    let mut stale: Vec<String> = Vec::new();
    for (path, needle, _) in EXEMPT {
        let hit = files.iter().any(|f| {
            f.rel.ends_with(path)
                && scan::error_log_sites(&f.text)
                    .iter()
                    .any(|s| s.message.as_deref().unwrap_or("").contains(needle))
        });
        if !hit {
            stale.push(format!("{path} :: {needle}"));
        }
    }
    assert!(
        stale.is_empty(),
        "这些豁免已经不再命中任何真实日志（代码变了或 message 改了），请删掉或更新：\n{}",
        stale.join("\n")
    );
}

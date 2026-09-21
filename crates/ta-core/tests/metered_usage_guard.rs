//! 守卫：**`MeteredUsage { … }` 的字面构造只允许出现在 `crates/ta-core/src/metering/**`**
//! （[11 §6] 铁律 4"不猜不估算"）。
//!
//! 为什么需要它：`estimated_usage()` 是**约定上的**唯一估算出口，但 `MeteredUsage` 的字段是
//! `pub`，任何地方都能手拼 `MeteredUsage { usage, source: UsageSource::Upstream }` 把估算
//! **冒充成上游给的数据**。这件事在类型上合法、编译器不会说话，而成本中心会照原样把估算
//! 当真实用量显示给用户看 —— **静默的错账比编译错误危险得多**。
//!
//! 这条守卫挡的是**新增**的手拼路径（让"违反"机械可见）；它挡不住已经存在的绕过路径，
//! 那是"字段私有化 + 只留构造器"那一步的事（跨 `types.rs` 与 E5 的调用点，另行裁决）。
//!
//! 扫描范围与 [`common::source_files`] 一致：`crates/**/*.rs` 与 `src-tauri/src/**/*.rs`
//! （[14 §5.5] 的既有约定），**不新增第二套遍历**。
//!
//! ## 政策：**测试代码同样在范围内**（评审 E1 要求写明确）
//!
//! `#[cfg(test)]` 模块、集成测试、夹具里手拼 `MeteredUsage` **一律算违规** ——
//! 本守卫的红-绿探针就是打在 `quota/mod.rs` 的 `mod tests` 里才被判违规的。
//! 理由：测试里手拼同样会把口径散出去（今天写 fixture，明天就有人照着抄到生产路径），
//! 而"只在测试里破例"会让守卫退化成"生产代码的约定"，那正是它最该守住的地方之外的噪声。
//!
//! 想豁免只有一条正路：**让类型本身无法被手拼**（字段私有化 + 只留构造器），
//! 而不是在守卫里开一个测试用的后门。

mod common;

use ta_core::diag::scan;

/// `MeteredUsage` 的所有者目录。只有这里允许出现字面构造。
const OWNER_DIR: &str = "crates/ta-core/src/metering/";

#[test]
fn metered_usage_literals_stay_inside_metering() {
    let mut violations: Vec<String> = Vec::new();
    for file in common::source_files() {
        if file.rel.starts_with(OWNER_DIR) {
            continue;
        }
        for site in scan::metered_usage_literal_sites(&file.text) {
            violations.push(format!("{}:{}  `MeteredUsage {{ … }}`", file.rel, site.line));
        }
    }
    assert!(
        violations.is_empty(),
        "发现 {} 处 metering 之外手拼 `MeteredUsage` 的地方：\n{}\n\n\
         为什么这是违规：估算必须带 `UsageSource::Estimated`，而手拼最容易写成 \
         `source: UsageSource::Upstream` —— 那会把本地估算冒充成上游数据，\
         成本中心与看板会照原样当真（[11 §6] 铁律 4）。\n\
         修法：走 `ta_core::metering::estimated_usage(&request_body)`（估算）\
         或对应协议的 `normalize_*` / `DefaultMeter`（真实归一化），不要自己构造。\n\
         确实需要在别处构造时：先问 [11 §6] 是否允许，再改本守卫，两处必须同改。",
        violations.len(),
        violations.join("\n")
    );
}

/// 守卫**真的能红**：从别处手拼一处，它必须认出来并给出行号。
///
/// 一条从没红过的守卫和没有守卫是一样的 —— 这个测试就是"它会红"的证据。
#[test]
fn the_guard_actually_detects_an_outside_literal() {
    let bad = r#"
fn smuggle(usage: TokenUsage) -> MeteredUsage {
    MeteredUsage { usage, source: UsageSource::Upstream }
}
"#;
    let sites = scan::metered_usage_literal_sites(bad);
    assert_eq!(sites.len(), 1, "metering 之外的字面构造必须被认出来");
    assert_eq!(sites[0].line, 3, "要给出准确行号（否则改起来要人肉找）");
}

/// 守卫的覆盖范围必须真的是两个根，而不是某天被悄悄收窄。
///
/// 没有这条，"范围被改小"会让守卫继续绿 —— 那是守卫最隐蔽的失效方式。
#[test]
fn the_guard_scans_both_repository_roots() {
    let files = common::source_files();
    let has_crates = files.iter().any(|f| f.rel.starts_with("crates/"));
    let has_desktop = files.iter().any(|f| f.rel.starts_with("src-tauri/src/"));
    assert!(has_crates, "扫描集里没有 crates/**，守卫等于没扫");
    assert!(has_desktop, "扫描集里没有 src-tauri/src/**，守卫等于没扫");
    // 而 metering 自己也必须在扫描集里（否则"所有者目录"的判断无从谈起）
    assert!(
        files.iter().any(|f| f.rel.starts_with(OWNER_DIR)),
        "扫描集里没有 {OWNER_DIR} —— 守卫把所有者目录也漏了"
    );
}

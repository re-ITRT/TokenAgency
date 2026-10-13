//! 守卫：**`tracing` 字段名必须来自 [14 §5] 的白名单**（[14 §5.5]）。
//!
//! 为什么值得加这条：字段名漂移是**静默**发生的 —— 没人会因为写了
//! `subscription_id` 而不是 `sub_id` 而收到任何报错，但"按 `request_id` 筛出一次请求的
//! 全部轨迹"这个能力就**部分失效**了，且失效时不会有任何提示。
//! 这类约定必须机械检查，不能靠自觉。
//!
//! 白名单来源：–§5.4（`fields.rs` 的常量与它对齐，
//! 由本文件里的 `canonical_fields_match_the_doc` 守着）。

mod common;

use std::collections::BTreeMap;

use ta_core::diag::{fields, scan};

/// 白名单之外**额外允许**的字段名，每一项都必须写理由。
///
/// 只有两类能进来：
/// 1. 契约正文里明确使用的字段（例如 [14 §6.2] 的推荐写法用了 `error`）；
/// 2. §5 定稿前就已经写下的既有调用点（**只减不增**，新代码一律用 §5 的名字）。
const EXTRA_ALLOWED: &[(&str, &str)] =
    &[("error", "[14 §6.2] 推荐写法：`error = %e` 记人话（§5 表格未列，但 §6 强制成对记录）")];

/// §5 定稿前的既有调用点用过的字段名：**历史遗留，只减不增**。
///
/// 它们分散在 P2/P4/P5/P6/P7 的 crate 里（不属于 OB1 的文件），
/// 迁移要各自的所有者动；OB1 把它们登记在这里，让守卫**从今天起**能拦住新的漂移。
/// 迁移建议已写进 OB1 的汇报（E 块）。
const LEGACY_ALLOWED: &[&str] = &[];

fn allowed(name: &str) -> Option<&'static str> {
    if fields::is_canonical(name) {
        return Some("§5.1–§5.4 约定字段");
    }
    if fields::is_local_context(name) {
        return Some("§5.6 本地上下文字段（已登记）");
    }
    if let Some((_, why)) = EXTRA_ALLOWED.iter().find(|(n, _)| *n == name) {
        return Some(why);
    }
    LEGACY_ALLOWED.contains(&name).then_some("§5 定稿前的既有调用点（仅存量）")
}

#[test]
fn tracing_field_names_must_be_in_the_whitelist() {
    let mut violations: Vec<String> = Vec::new();
    for file in common::source_files() {
        for f in scan::tracing_fields(&file.text) {
            if allowed(&f.name).is_none() {
                violations
                    .push(format!("{}:{}  `{}!({} = …)`", file.rel, f.line, f.macro_name, f.name));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "发现 {} 处不在 [14 §5] 白名单里的 tracing 字段名：\n{}\n\n\
         修法（二选一）：① 改用 §5 里的名字（首选，例如 request_id / channel_id / attempt / code）；\
         ② 如果这个字段确实该进契约，先改 docs/14-observability.md §5，再更新 diag::fields 常量与守卫白名单。",
        violations.len(),
        violations.join("\n")
    );
}

/// 白名单与**契约正文**必须一致：文档是唯一真相源，代码不能偷偷多一个字段。
#[test]
fn canonical_fields_match_the_doc() {
    let doc = common::read_repo_file("docs/14-observability.md")
        .expect("读不到 docs/14-observability.md —— 契约文件缺失，守卫失去依据");

    let section_51 = section_table_first_column(&doc, "## 5.1", "## 5.2");
    let section_52 = section_table_first_column(&doc, "## 5.2", "## 5.3");
    let section_53 = section_table_first_column(&doc, "## 5.3", "## 5.4");
    let section_54 = section_table_first_column(&doc, "## 5.4", "## 5.5");

    let mut doc_fields: Vec<String> = Vec::new();
    doc_fields.extend(section_51);
    doc_fields.extend(section_52);
    doc_fields.extend(section_54);
    doc_fields.sort();
    doc_fields.dedup();

    let mut code_fields: Vec<String> = fields::CANONICAL.iter().map(|s| s.to_string()).collect();
    code_fields.sort();

    assert_eq!(
        code_fields, doc_fields,
        "`diag::fields::CANONICAL` 与 [14 §5.1/§5.2/§5.4] 的表格不一致。\n\
         顺序说明：先改文档、再改常量（文档是唯一真相源）。"
    );

    let mut doc_jobs = section_53;
    doc_jobs.sort();
    let mut code_jobs: Vec<String> = fields::JOBS.iter().map(|s| s.to_string()).collect();
    code_jobs.sort();
    assert_eq!(code_jobs, doc_jobs, "`diag::fields::JOBS` 与 [14 §5.3] 的表格不一致");
}

/// [14 §5.6] 的登记表与 `diag::fields::LOCAL_CONTEXT` 必须一致。
///
/// 与 `canonical_fields_match_the_doc` 同一机制：**文档是唯一真相源**。
/// 没有这条，"§5.6 必须登记"就会退化成一句口号。
#[test]
fn local_context_fields_match_the_doc() {
    let doc = common::read_repo_file("docs/14-observability.md")
        .expect("读不到 docs/14-observability.md");
    let mut doc_fields =
        section_table_first_column(&doc, "**当前已登记的本地上下文字段**", "\n# 6.");
    doc_fields.sort();
    doc_fields.dedup();

    let mut code_fields: Vec<String> =
        fields::LOCAL_CONTEXT.iter().map(|s| s.to_string()).collect();
    code_fields.sort();

    assert_eq!(
        code_fields, doc_fields,
        "`diag::fields::LOCAL_CONTEXT` 与 [14 §5.6] 的登记表不一致。\n\
         顺序说明：先改文档、再改常量（文档是唯一真相源）。"
    );
}

/// 两层字段表**不能重叠**：一个名字要么是契约字段，要么是本地上下文，不能既是又是。
#[test]
fn canonical_and_local_context_do_not_overlap() {
    for n in fields::CANONICAL {
        assert!(
            !fields::is_local_context(n),
            "`{n}` 同时出现在 CANONICAL 与 LOCAL_CONTEXT —— 两层表语义不同，重叠会让守卫失去区分能力"
        );
    }
    let uniq: std::collections::BTreeSet<&str> = fields::LOCAL_CONTEXT.iter().copied().collect();
    assert_eq!(uniq.len(), fields::LOCAL_CONTEXT.len(), "LOCAL_CONTEXT 里有重复项");
}

/// 守卫必须**真的能红**：给它一段故意写错的源码，它要能认出来。
///
/// 一条从没红过的守卫和没有守卫是一样的 —— 这个测试就是"它会红"的证据。
#[test]
fn the_guard_actually_detects_a_bad_field_name() {
    let bad = r#"
fn f() {
    tracing::warn!(request_id = %rid, subscription_id = %s, "订阅同步失败");
}
"#;
    let names: Vec<String> = scan::tracing_fields(bad).into_iter().map(|f| f.name).collect();
    assert!(names.contains(&"request_id".to_string()), "正常字段要被认出来");
    assert!(
        names.iter().any(|n| allowed(n).is_none()),
        "`subscription_id` 不在白名单里，守卫必须判它违规（实际解析出：{names:?}）"
    );
}

/// 允许清单里不能有"已经不存在的字段名"——避免白名单悄悄腐烂成一个摆设。
#[test]
fn the_whitelist_has_no_dead_entries() {
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for file in common::source_files() {
        for f in scan::tracing_fields(&file.text) {
            *used.entry(f.name).or_insert(0) += 1;
        }
    }
    let dead: Vec<&str> =
        fields::CANONICAL.iter().copied().filter(|n| !used.contains_key(*n)).collect();
    // 允许"契约里有、暂时没人用"（例如 status），但要在报告里能看出来
    println!("当前仓库用到 {} 种字段名；契约里暂未使用：{dead:?}", used.len());
}

/// 取某个小节里所有表格行的第一列，**并拆开成对写法**。
///
/// [14 §5.2] 把成对字段写成一行（`` `channel_id` / `channel_name` ``），
/// 因为它们总是同时出现、读起来更清楚。所以这里必须拆 `/`、
/// 并去掉**每一个**反引号（只 trim 首尾会留下中间那对）。
fn section_table_first_column(doc: &str, from: &str, to: &str) -> Vec<String> {
    let Some(start) = doc.find(from) else { return Vec::new() };
    let rest = &doc[start..];
    let end = rest.find(to).unwrap_or(rest.len());
    let mut out = Vec::new();
    for line in rest[..end].lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let Some(cell) = trimmed.trim_start_matches('|').split('|').next() else { continue };
        let cell = cell.trim();
        if cell.is_empty()
            || cell == "字段"
            || cell == "取值"
            || cell.starts_with("---")
            || cell.contains("—")
        {
            continue;
        }
        for part in cell.split('/') {
            let name = part.trim().replace('`', "");
            let name = name.trim();
            if name.is_empty() || name == "字段" || name == "取值" || name.starts_with("---") {
                continue;
            }
            out.push(name.to_string());
        }
    }
    out.dedup();
    out
}

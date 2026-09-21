//! 守卫：**命令面三条规则** —— 不漏、不多、总量不丢。
//!
//! 用户的要求是"提交的必须是完整可用、经过验证的，不要留『待开发』"，而"T1 做了多少"
//! 此前只能靠人肉数。本守卫把它变成机械检查。
//!
//! ## 方向（第 12 轮主管修正，很重要）
//!
//! E1 的审计事实：契约里**页面真正引用的只是一小部分**（个位数到十几条），其中
//! `StatusBar` 的 `app_pause_gateway` / `app_resume_gateway` **没有注册**（这就是唯一要消灭的红）；
//! 其余只活在 `commands.ts` **类型化门面**里、没有任何消费方引用。
//! **具体条数一律以守卫自己的输出为准**（它会打印"扫了多少文件、引用了几条"），
//! 文档里不写死 —— 写死的数字必然漂。
//!
//! > "契约条数 vs 已注册条数"这个差值**不是要消灭的红，是要如实标注的计划面**。
//!
//! 因此本守卫**不做**"契约 ⊆ 注册表"这种断言 —— 那把计划面当成了谎言面，方向是错的。
//!
//! ## 三个来源
//!
//! | 角色 | 路径 |
//! |---|---|
//! | **活契约**（真相源） | `src/shared/bindings.ts` 的 `export type CommandMap` |
//! | **实现** | `src-tauri/src/commands/mod.rs` 的 `collect_commands![ … ]` |
//! | **总量锚点**（**不是真相源**） | `docs/contracts/bindings-handwritten-reference.ts.txt` |
//!
//! ## 三条规则
//!
//! 1. [`referenced_commands_are_all_registered`] —— **不漏**：`src/**` 里**消费方**引用到的
//!    每个命令名都必须在注册表里。这是主牙（指向"页面会调、后端没有"）。
//! 2. [`every_registered_command_is_in_the_contract`] —— **不多**：注册了就得在契约里
//!    （契约覆盖**实现**，不是覆盖当前调用点）。
//! 3. [`the_contract_total_matches_the_archive_anchor`] —— **总量不丢**：活契约的键集
//!    必须等于归档锚点，`ARCHIVE_TOTAL` 是显式的条数锚点。
//!
//! 外加两条**元守卫**与一条机制自证：守卫最隐蔽的失效方式是"自己恒绿"，
//! 而"消费方扫描器扫不到文件"会让规则 1 永远成立 —— 所以元守卫 2 专门钉住
//! "扫到了多少文件、引用集里必须有已知命令"。
//!
//! ## "消费方"的定义（[`is_consumer_source`]）
//!
//! `src/**` 的 `.ts/.tsx`，**排除**：
//! - 测试与夹具（`*.test.*` / `*.spec.*` / `__mocks__` / `fixtures/`）；
//! - **调用管道** `src/shared/api/**`（含 `commands.ts` 门面、`client.ts`、`mock.ts`）；
//! - **契约文件本身** `src/shared/bindings.ts`。
//!
//! 为什么必须排除门面：`commands.ts` 给每条契约命令都写了一个包装函数，
//! 把它算成"页面引用"会让 105 条全部变成"被引用"（E1 的审计正是这件事）。
//!
//! **只读**：本文件不碰 `docs/**`、`src/**`、`src-tauri/**`，只读它们的文本。

mod common;

/// 活契约（真相源）。
const LIVE_CONTRACT: &str = "src/shared/bindings.ts";
/// 实现侧：唯一的命令注册点。
const REGISTRY: &str = "src-tauri/src/commands/mod.rs";
/// 归档件：**只当总量锚点**。
const ARCHIVE: &str = "docs/contracts/bindings-handwritten-reference.ts.txt";

/// 归档件的条数锚点。**契约增删命令必须同步改这里** —— 这正是让"契约缩水/解析器收缩"
/// 都得留下显式修改痕迹的那道闸。
///
/// 校准史：105 → **108**（主管裁决补进 `channel_list`，以及 E3 已实现的
/// `log_query_capabilities` / `quota_limit_gaps`；归档件已由主管同步）。
/// 校准依据是守卫自己的输出、不是目测：`归档件里解析出 108 条命令，期望 105 条` → 改这里。
const ARCHIVE_TOTAL: usize = 108;

// ---------------------------------------------------------------- 解析（纯字符串 → 集合）

/// 命令名的形状：小写字母开头 + 小写/数字/下划线。
fn is_command_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 解析 TS 里 `export type <type_name> = { … }` 的命令名。`None` = 类型不存在或形态异常。
///
/// 只认**恰好两空格缩进**的 `name: {` 行 —— 值类型内部的 `args: { … }` 是四空格缩进，
/// 不能被当成命令名（那会让条数虚高，元守卫会立刻红）。
fn ts_command_map(text: &str, type_name: &str) -> Option<Vec<String>> {
    let decl = format!("export type {type_name} = {{");
    let mut lines = text.lines();
    if !lines.by_ref().any(|l| l.starts_with(&decl)) {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with("};") {
            return Some(out);
        }
        if !line.starts_with("  ") || line.starts_with("   ") {
            continue;
        }
        let body = &line[2..];
        let Some((name, rest)) = body.split_once(':') else { continue };
        if !rest.trim_start().starts_with('{') || !is_command_name(name) {
            continue;
        }
        out.push(name.to_string());
    }
    None // 声明行在、收尾不在 → 形态变了，宁可 None 让元守卫红
}

/// 实现侧 `collect_commands![ … ]` 注册的命令名（取 `module::name` 的 `name`）。
fn registered_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if !inside {
            if line.contains("collect_commands![") {
                inside = true;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("])") {
            break;
        }
        let entry = trimmed.trim_end_matches(',');
        if let Some((_, name)) = entry.rsplit_once("::") {
            if is_command_name(name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// 抽出一段 TS/TSX 里的字符串字面量内容（单双引号都认，跳过转义）。
fn string_literals(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let q = bytes[i];
        if q != b'"' && q != b'\'' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j] != q && bytes[j] != b'\n' {
            if bytes[j] == b'\\' {
                j += 1; // 跳过转义字符
            }
            j += 1;
        }
        if j < bytes.len() && bytes[j] == q {
            out.push(&text[start..j]);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn dedup_sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// `expected` 有、`actual` 没有的。
fn missing(expected: &[String], actual: &[String]) -> Vec<String> {
    dedup_sorted(expected.iter().filter(|n| !actual.contains(n)).cloned().collect())
}

/// `actual` 有、`expected` 没有的。
fn extra(expected: &[String], actual: &[String]) -> Vec<String> {
    dedup_sorted(actual.iter().filter(|n| !expected.contains(n)).cloned().collect())
}

fn bullets(names: &[String]) -> String {
    names.iter().map(|n| format!("  - {n}")).collect::<Vec<_>>().join("\n")
}

/// 读一个必须存在的来源，并确认它含自己的锚点（防路径写错后对着空集断言而恒绿）。
fn read_source(rel: &str, anchor: &str) -> String {
    let text = common::read_repo_file(rel)
        .unwrap_or_else(|| panic!("读不到来源 {rel} —— 守卫失去依据（路径写错？文件搬了？）"));
    assert!(
        text.contains(anchor),
        "{rel} 里没有锚点 `{anchor}` —— 来源形态变了，解析器要跟着改（否则守卫恒绿）"
    );
    text
}

fn contract_commands() -> Vec<String> {
    let text = read_source(LIVE_CONTRACT, "export type CommandMap");
    let parsed = ts_command_map(&text, "CommandMap")
        .expect("活契约里找不到 `export type CommandMap` —— 形态变了，解析器要跟着改");
    assert!(!parsed.is_empty(), "活契约的 CommandMap 解析出空集 —— 解析器失效（不是契约为空）");
    parsed
}

fn registry_commands() -> Vec<String> {
    registered_commands(&read_source(REGISTRY, "collect_commands!["))
}

fn archive_commands() -> Vec<String> {
    ts_command_map(&read_source(ARCHIVE, "export type CommandMap"), "CommandMap")
        .expect("归档件里找不到 CommandMap")
}

/// 消费方源码判定（见模块文档"消费方的定义"）。
fn is_consumer_source(rel: &str) -> bool {
    let is_ts = rel.ends_with(".ts") || rel.ends_with(".tsx");
    let is_test_or_fixture = rel.contains(".test.")
        || rel.contains(".spec.")
        || rel.contains("__mocks__")
        || rel.contains("/fixtures/");
    let is_plumbing = rel.starts_with("src/shared/api/"); // 门面/调用管道，见模块文档
    let is_contract = rel == LIVE_CONTRACT;
    is_ts && !is_test_or_fixture && !is_plumbing && !is_contract
}

/// 扫描消费方，返回（扫过的文件数，引用到的命令名集合）。
fn consumer_references() -> (usize, Vec<String>) {
    let contract = contract_commands();
    let mut files = 0usize;
    let mut referenced: Vec<String> = Vec::new();
    for f in common::frontend_source_files() {
        if !is_consumer_source(&f.rel) {
            continue;
        }
        files += 1;
        for lit in string_literals(&f.text) {
            // 只认"契约里的名字"：页面里还有路由/文案等大量其它字符串，
            // 而 `tsc` 已经保证命令名只能来自 CommandMap（见模块文档）。
            if contract.iter().any(|c| c == lit) {
                referenced.push(lit.to_string());
            }
        }
    }
    (files, dedup_sorted(referenced))
}

// ---------------------------------------------------------------- 1) 主牙：不漏

/// **规则 1（主牙）：消费方引用到的每个命令都必须在注册表里。**
///
/// 这条指向"页面会调、后端没有"——用户视角就是"点了没反应"。今天是红的，且只该红两条：
/// `app_pause_gateway` / `app_resume_gateway`（E3 的 task-13 正在实现，落地后自动转绿）。
#[test]
fn referenced_commands_are_all_registered() {
    let (files, referenced) = consumer_references();
    let registered = registry_commands();

    let unregistered = missing(&referenced, &registered);
    assert!(
        unregistered.is_empty(),
        "有 {} 条被前端消费方引用的命令**没有注册**（扫了 {files} 个消费方文件，\
         共引用 {} 条命令）：\n{}\n\
         这才是「要消灭的红」：页面会调、后端不存在。\n\
         （契约里其余没被引用的命令属于**计划面**，本守卫不对它们断言 —— 见模块文档的方向说明。）",
        unregistered.len(),
        referenced.len(),
        bullets(&unregistered)
    );
}

// ---------------------------------------------------------------- 2) 不多

/// **规则 2：注册了就必须在契约里。**
///
/// 契约覆盖**实现**，不是覆盖当前调用点 —— `channel_list` 在 `catalog.rs` 有真实实现、
/// 只是前端还没调用；`quota_limit_gaps` / `log_query_capabilities` 同理。处置是补进契约。
#[test]
fn every_registered_command_is_in_the_contract() {
    let contract = contract_commands();
    let registered = registry_commands();

    let undeclared = extra(&contract, &registered);
    assert!(
        undeclared.is_empty(),
        "有 {} 条命令注册了、但契约（{LIVE_CONTRACT} 的 CommandMap）里没有：\n{}\n\
         处置：补进契约（它是真相源）。用 `channel_matrix` 之外的通道列表命令做例子最好：\n\
         契约要覆盖**实现**，不是覆盖当前调用点。",
        undeclared.len(),
        bullets(&undeclared)
    );
}

// ---------------------------------------------------------------- 3) 总量不丢

/// **规则 3：活契约的键集 == 归档锚点；条数 == [`ARCHIVE_TOTAL`]。**
///
/// 归档件从此只是**总量锚点**（活契约怎么拆分都行，但一条都不能少）。
#[test]
fn the_contract_total_matches_the_archive_anchor() {
    let contract = contract_commands();
    let archive = archive_commands();

    let lost = missing(&archive, &contract);
    let added = extra(&archive, &contract);
    assert!(
        lost.is_empty() && added.is_empty(),
        "契约与归档锚点不一致（活契约 {} 条、归档 {} 条）：\n\
         A. 归档里有、活契约里没有（**丢命令**，{} 条）：\n{}\n\
         B. 活契约里有、归档里没有（新增命令 —— 请同步归档锚点与 ARCHIVE_TOTAL，{} 条）：\n{}",
        contract.len(),
        archive.len(),
        lost.len(),
        bullets(&lost),
        added.len(),
        bullets(&added)
    );
    assert_eq!(contract.len(), ARCHIVE_TOTAL, "活契约条数与 ARCHIVE_TOTAL 不一致");
}

// ---------------------------------------------------------------- 4) 元守卫与自证

/// 元守卫 1：归档件的条数必须是**具体数**，且解析器没有静默漏项。
#[test]
fn the_archive_still_has_the_pinned_total() {
    let parsed = archive_commands();
    assert_eq!(
        parsed.len(),
        ARCHIVE_TOTAL,
        "归档件里解析出 {} 条命令，期望 {ARCHIVE_TOTAL} 条。\n\
         两种可能：① 契约**真的**增删了命令 —— 那就同步改 ARCHIVE_TOTAL，让变更留下显式痕迹；\n\
         ② 解析器被写法变化骗了 —— 那是守卫静默失效，修解析器别改数字。\n\
         解析到的前 3 条：{:?}",
        parsed.len(),
        &parsed[..parsed.len().min(3)]
    );
    for anchor in ["app_status", "app_check_update", "log_query", "usage_export_csv"] {
        assert!(
            parsed.iter().any(|c| c == anchor),
            "归档件解析结果里没有 `{anchor}` —— 解析器只对了数量、没对内容"
        );
    }
}

/// 元守卫 2：**消费方扫描器必须真的扫到东西**。
///
/// 没有这条，规则 1 会以"一个文件都没扫到 → 引用集为空 → 零缺失"的方式**恒绿** ——
/// 那是守卫最隐蔽的失效方式。所以这里钉住"扫到多少文件"和"引用集里必须有已知命令"。
#[test]
fn the_guard_really_scans_consumers_and_sources() {
    // 三个来源都要真读到（路径写错会让所有断言对着空集成立）
    let _live = read_source(LIVE_CONTRACT, "export type CommandMap");
    let _archive = read_source(ARCHIVE, "export type CommandMap");
    let _registry = read_source(REGISTRY, "collect_commands![");

    let (files, referenced) = consumer_references();
    assert!(
        files >= 10,
        "只扫到 {files} 个消费方文件 —— 扫描范围/排除规则写错了，规则 1 会因此恒绿"
    );
    assert!(
        referenced.len() >= 5,
        "只解析出 {} 条被引用的命令 —— 字面量解析或消费方范围失效，规则 1 会因此恒绿",
        referenced.len()
    );
    // 已知被消费方引用的命令（StatusBar 的 config_get / lane_list / policy_list、
    // dashboard 的 usage_dashboard）—— 钉住"扫描器还在工作"。
    // 注意 `app_status` **不在**此列：它走的是 `useAppStatus()` 专用 hook，消费方里没有字面量。
    for anchor in ["config_get", "lane_list", "policy_list", "usage_dashboard"] {
        assert!(
            referenced.iter().any(|c| c == anchor),
            "消费方引用集里没有 `{anchor}` —— 说明扫描器漏了文件（规则 1 正在静默失效）"
        );
    }
    assert!(
        registry_commands().len() >= 30,
        "实现侧解析出的命令太少 —— 注册点形态变了，会让规则 2 失真"
    );
}

/// 机制自证：比较逻辑单独证明一次，红/绿由缺项决定而不是由环境决定。
#[test]
fn the_diff_helpers_detect_missing_and_extra() {
    let expected = ["a_one", "a_two", "a_three"].map(String::from);
    let actual = ["a_one", "a_three", "z_extra"].map(String::from);

    assert_eq!(missing(&expected, &actual), vec!["a_two".to_string()]);
    assert_eq!(extra(&expected, &actual), vec!["z_extra".to_string()]);
    assert!(missing(&expected, &expected).is_empty());
    assert!(extra(&expected, &expected).is_empty());
    // 重复项不该让清单变长
    let dup = ["a_one", "a_one"].map(String::from);
    assert_eq!(missing(&expected, &dup), vec!["a_three".to_string(), "a_two".to_string()]);

    // 字面量解析：转义与单引号也要认，跨行不硬断
    let lits = string_literals(r#"useCommandQuery("app_status", x); f('config_get'); s = "a\"b";"#);
    assert!(lits.contains(&"app_status"), "{lits:?}");
    assert!(lits.contains(&"config_get"), "{lits:?}");
    assert!(lits.contains(&r#"a\"b"#), "{lits:?}");

    // 消费方判定：测试/夹具/门面/契约都不算消费方
    assert!(is_consumer_source("src/app/StatusBar.tsx"));
    assert!(is_consumer_source("src/pages/dashboard/index.tsx"));
    assert!(!is_consumer_source("src/app/dashboard.test.tsx"));
    assert!(!is_consumer_source("src/shared/api/commands.ts"));
    assert!(!is_consumer_source("src/shared/api/mock.ts"));
    assert!(!is_consumer_source("src/shared/bindings.ts"));
}

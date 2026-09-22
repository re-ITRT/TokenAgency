//! 守卫：`#[tauri::command]` 必须显式声明 `rename_all = "snake_case"`。
//!
//! ## 为什么（这不是风格问题，是真机必失败）
//!
//! `tauri-macros 2.6.3` 的 `WrapperAttributes::default()` 里写死了
//! `argument_case: ArgumentCase::Camel`（见该 crate `src/command/wrapper.rs`）——
//! 也就是说，Rust 侧参数 `delay_s: u64` 在 **IPC 入参对象里被查找的 key 是 `delayS`**。
//! 而本项目的契约（`docs/10`）、`src/shared/bindings.ts` 的 `CommandMap`、
//! `src/shared/api/commands.ts` 门面**全部是 snake_case**，前端发出的就是 `delay_s`。
//!
//! 后果：**凡是有多词参数的命令，真机上直接报缺 key**（单个词的参数不受影响，
//! 所以只有一部分命令会坏 —— 这类"一部分坏了"最容易在浏览器 mock 下被掩盖：
//! mock 忽略入参，看起来一切正常）。
//!
//! 修法只有一条路：每条命令都显式写 `#[tauri::command(rename_all = "snake_case")]`。
//! 本守卫**不区分**哪些命令"现在有下划线参数"——统一加、统一查，
//! 因为"参数改成多词"是随时可能发生的重构，规则统一才不会被漏掉。
//!
//! 对照：`command_surface_guard.rs` 管"注册表 ↔ 契约一致"，本文件管"入参大小写一致"，
//! 两者各管一段。
//!
//! 已知边界（诚实写出来）：本守卫是**源码文本级**的（和 `command_surface_guard.rs` 同法），
//! 它拦的是"忘了写这条属性"；它**不**验证 Tauri 宏真的按我们期望的方式派生
//! （那要真跑一个 IPC 调用，属于端到端验证的职责）。

use std::fs;
use std::path::{Path, PathBuf};

/// 唯一允许的形态。
const REQUIRED: &str = "rename_all = \"snake_case\"";

/// 递归收集 `.rs` 文件。
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 一条 `#[tauri::command]` 属性所在行是不是写对形态了。
///
/// 判定只看**代码行**：行首（去空白后）必须是 `#[tauri::command`，
/// 这样 `//! ...` / `// ...` 这类注释里"提到"属性不会被误判
/// （`net.rs` 顶部恰好有一段解释这个属性的文档注释）。
fn attribute_line_missing_snake_case(line: &str) -> bool {
    let t = line.trim_start();
    if !t.starts_with("#[tauri::command") {
        return false;
    }
    !t.contains(REQUIRED)
}

/// 只是"这一行是不是一处命令属性"（不管写没写对）。
fn is_command_attr(line: &str) -> bool {
    line.trim_start().starts_with("#[tauri::command")
}

fn scan(source: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .enumerate()
        .filter(|(_, l)| attribute_line_missing_snake_case(l))
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect()
}

#[test]
fn guard_itself_has_teeth() {
    // 正面：写对了不许报
    assert!(
        !attribute_line_missing_snake_case(r#"    #[tauri::command(rename_all = "snake_case")]"#),
        "写对形态的必须放行"
    );
    // 反面：漏了必须报
    assert!(
        attribute_line_missing_snake_case("    #[tauri::command]"),
        "漏了 rename_all 的必须被抓"
    );
    // 注释里的"提到"不算：否则守卫会被自己的文档注释绊倒
    assert!(
        !attribute_line_missing_snake_case(r#"//! `#[tauri::command(rename_all = "snake_case")]`"#),
        "文档注释里的属性不算一处命令"
    );
    assert!(
        !attribute_line_missing_snake_case(
            r#"/// 反例：`#[tauri::command]` 会按 camelCase 找 key"#
        ),
        "行注释里的反例不算一处命令"
    );
    // 合成一段源码：两条坏的必须都被点名
    let synthetic = "#[tauri::command]\nfn a() {}\n#[tauri::command(rename_all = \"snake_case\")]\nfn b() {}\n#[tauri::command]\nfn c() {}\n";
    let found = scan(synthetic);
    assert_eq!(found.len(), 2, "合成样本里应有两条漏网：{found:?}");
    assert_eq!(found[0].0, 1);
    assert_eq!(found[1].0, 5);
}

#[test]
fn every_tauri_command_declares_snake_case_args() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/src");
    let root = root.canonicalize().unwrap_or(root);

    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();

    let mut total = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for f in &files {
        let Ok(text) = fs::read_to_string(f) else { continue };
        // 「命令总数」必须**独立**于「漏网数」来数：漏网数修到 0 是目标，
        // 一旦拿它当自检基数，修好后守卫就会自己误报（第一版就是这么错的）。
        total += text.lines().filter(|l| is_command_attr(l)).count();
        for (line, body) in scan(&text) {
            let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
            offenders.push(format!("  src-tauri/src/{rel}:{line}  {body}"));
        }
    }

    // 不许对着空集断言然后绿：一个命令都没扫到说明路径写错了
    assert!(
        total > 60,
        "只扫到 {total} 处 #[tauri::command]（应远多于 60）—— 路径或解析多半错了，别对着空集断言"
    );

    assert!(
        offenders.is_empty(),
        "以下命令没写 `{REQUIRED}`：Tauri 2 默认按 camelCase 查入参 key，\
         而契约与前端全是 snake_case —— 这些命令在真机上会报缺 key（浏览器 mock 掩盖得住）：\n{}\n\n\
         修法：`#[tauri::command]` → `#[tauri::command(rename_all = \"snake_case\")]`",
        offenders.join("\n")
    );
}

//! 守卫：**已应用的迁移是不可变字节**。
//!
//! # 为什么必须有这条守卫
//!
//! sqlx 把每个迁移文件的 SHA-384 记进 `_sqlx_migrations`。只要改动**已应用过**的迁移文件
//! 一个字节，所有老用户的库在启动时都会失败：
//!
//! ```text
//! 存储错误：run migrations: migration 1 was previously applied but has been modified
//! ```
//!
//! 而**编译、clippy、本仓其它测试全是绿的** —— 只有在真机上启动才会炸，用户看到的是
//! "双击后闪退"。这类错误在 CI 里完全隐形。
//!
//! 2026-10-14 真实发生过：一次"清理注释"的批量改动顺手改了 `0001`–`0007` 七个迁移文件
//! （删掉注释里的文档编号引用），于是打包出来的安装包装完**启动即崩**。当时没有任何测试会红。
//!
//! # 规矩
//!
//! * **只许追加**新迁移；已发布过的文件**一个字节都不许改**（连注释、连空行都不行）。
//! * 新增迁移后，把它的 SHA-384 追加到下面的 `APPLIED` 表；测试会告诉你缺哪一条。
//! * 正因如此，`crates/ta-store/migrations/**` 里的注释**保持历史原样**，
//!   它不参与"文档编号引用"那条棘轮 —— 那条是给"还能改的代码"用的。
//! * 需要改 schema 时**新加一个迁移文件**，不要回头改老的。

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha384};

/// `version` → 已发布迁移文件的 SHA-384（大写十六进制）。
///
/// 这些数值与真实库 `_sqlx_migrations.checksum` **逐字节一致**
/// （2026-10-14 拿本机的真库逐个核对过 8/8）。**只许往下追加，不许修改已有的行。**
const APPLIED: &[(u32, &str)] = &[
    (
        1,
        "F1E28110D008968374B9FF36C5A59DFBAE5A780E7E6F8092913336821BCEE78171268BBFDB92DA4B8DD52B5845DC7C7A",
    ),
    (
        2,
        "7E19E556063EFEF59E755DCD029BDE6BE0ECF9F3B0B766E94A3D63D5F3495B2FE7166C2585BD3DA7870F43AED57CCDE0",
    ),
    (
        3,
        "82B3A8673D3AF643035ED3B33C2C376746ED9864EA1A40BD6A65971AA6B25A29C2D8BDEEF009F5ABEF402EB147DC26B6",
    ),
    (
        4,
        "465EA591878C6912DCBDBF5178566206F26623138500A241F21B67D9FF7DF2A6C01ED1DA7C3B4F356C5D97C70C85023D",
    ),
    (
        5,
        "408407102E5017F3A236ED9248A76FC59784354942791E459E39C8EEF2B9523B237D4BA19E3F8C4BBBD5B8A7B3F558CB",
    ),
    (
        6,
        "BB773E79CBC4751689DFFE74A7167503EA0BEAAA5D4340FE60981BBF8612615EE0F569C40733F10F9A1AA9FF73DD1BD9",
    ),
    (
        7,
        "7A0435C10BE0C8544D7D2C1C895A264758CFB0A169F4C9C31DA52038EA22C2D8ABB966ACA585B5463A4C816D1B8583A8",
    ),
    (
        8,
        "16C9B92F2094906FE477B1841EA6A392D53B93DB6C7AA82055D09EE753B0CC9323962BC56AB8FB1A0A555E4C5888F32E",
    ),
];

fn sha384_hex(bytes: &[u8]) -> String {
    Sha384::digest(bytes).iter().map(|b| format!("{b:02X}")).collect()
}

fn migrations_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// 读目录里所有 `NNNN_*.sql`，返回 `(version, 文件名, sha384)`，按 version 排序。
fn read_migrations() -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(migrations_dir()).expect("读不到 migrations 目录") {
        let path = entry.expect("目录项读失败").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).expect("文件名不是 UTF-8").to_string();
        let version: u32 = name
            .split('_')
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("迁移文件名不以数字编号开头：{name}"));
        let bytes = fs::read(&path).expect("迁移文件读失败");
        out.push((version, name, sha384_hex(&bytes)));
    }
    out.sort_by_key(|(v, _, _)| *v);
    out
}

#[test]
fn applied_migrations_are_byte_frozen() {
    let found = read_migrations();

    // 防空转：目录空 / 读法写错时，下面的"逐条比对"会因为找不到而误报，这里先钉住规模。
    assert!(
        found.len() >= APPLIED.len(),
        "只读到 {} 个迁移文件，登记表里有 {} 条 —— 读法或目录结构变了",
        found.len(),
        APPLIED.len()
    );

    let mut problems: Vec<String> = Vec::new();

    // 1) 每一条已登记的指纹都必须原样存在
    for (want_v, want_hash) in APPLIED {
        match found.iter().find(|(v, _, _)| v == want_v) {
            None => problems.push(format!("迁移 {want_v} 不见了")),
            Some((_, name, got)) if got != want_hash => problems.push(format!(
                "迁移 {want_v}（{name}）的字节被改过：\n      \
                 登记 {want_hash}\n      \
                 现在 {got}\n      \
                 → 已应用的迁移**不许改**（改了会让所有老用户的库启动即失败）。\n        \
                 要改 schema 就新增一个迁移文件，别回头改这个。"
            )),
            Some(_) => {}
        }
    }

    // 2) 反向：新迁移必须登记（否则没人会记得补指纹）
    for (v, name, hash) in &found {
        if !APPLIED.iter().any(|(av, _)| av == v) {
            problems.push(format!(
                "迁移 {v}（{name}）还没有登记指纹。把它追加到 APPLIED：\n      ({v}, \"{hash}\"),"
            ));
        }
    }

    assert!(problems.is_empty(), "迁移文件的不可变性被破坏了：\n  - {}\n", problems.join("\n  - "));
}

/// 自检：摘要函数**真的**对一字节改动敏感（否则上面那条守卫可能只是"恒绿"）。
#[test]
fn digest_is_sensitive_to_a_single_byte() {
    let a = sha384_hex(b"-- 0001 init\n");
    let b = sha384_hex(b"-- 0001 init\n ");
    assert_ne!(a, b, "多一个空格就应当改变摘要");
    assert_eq!(a.len(), 96, "SHA-384 的十六进制应当是 96 个字符");
    assert_eq!(a, sha384_hex(b"-- 0001 init\n"), "同内容必须得同摘要");
}

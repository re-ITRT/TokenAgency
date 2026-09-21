//! ★ **C3 判定**：诊断包"用户能放心发出去"（[14 §11](../../../docs/14-observability.md)）—— 纯逻辑，不碰文件系统。
//!
//! C3 的判据不是"日志写入时脱敏了"，而是**打出来的那个包里搜不到密钥**。
//! 两者的差别就是 [14 §11.3] 存在的理由：诊断包还会**新加入**配置、内核配置、
//! 环境信息等**不经过日志路径**的内容 —— 写入时脱敏管不到它们。
//!
//! 所以这个模块把 [14 §11.3] 那句"打包**结束后**对整个包做一次全文扫描，
//! 命中即**中止导出并报错**"落成三段可测的纯逻辑：
//!
//! 1. [`scan_for_secrets`]：打包**前**扫输入的每个文件；
//! 2. [`build_package`]：按 [14 §11.2] 组包（`manifest.json` + 错误摘要 + 内容）并写出 ZIP；
//! 3. [`verify_and_scan_archive`]：**对写出来的字节重新解压**、校验 CRC、再扫一遍 ——
//!    这一遍才是 [14 §11.3] 要的那一遍：它同时证明"包本身可解压、没写坏"。
//!
//! **错误里绝不回显命中的原文**（那等于把密钥搬进错误日志）—— 只报文件、类型、偏移与长度。

use serde_json::json;
use sha2::{Digest, Sha256};

use super::query::CodeCount;
use super::sanitize::{self, SecretKind};
use super::zipstore::{self, ZipError};

/// [14 §11.2] 的 `manifest.json`。
pub const MANIFEST_NAME: &str = "manifest.json";
/// [14 §11.2] 的"最近 N 条 WARN/ERROR 的码与计数"。
pub const ERROR_SUMMARY_NAME: &str = "error-summary.json";

/// 待打进包里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    /// 归档内路径（`/` 分隔），例如 `logs/tokenagency.log`
    pub name: String,
    pub content: Vec<u8>,
}

impl PackageFile {
    pub fn new(name: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), content: content.into() }
    }

    pub fn text(name: impl Into<String>, text: &str) -> Self {
        Self::new(name, text.as_bytes().to_vec())
    }
}

/// 组包失败的原因。**宁可报错，也不交出一个用户以为安全的包。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// [14 §11.3]：扫到敏感形态 → 中止导出。**不回显原文**（只报位置与类型）。
    Secret { name: String, kind: SecretKind, offset: usize, len: usize },
    /// 归档里有重名条目（解压时会互相覆盖 → 用户看到的内容与我们的清单不一致）
    DuplicateName(String),
    /// 归档内路径不合法（空 / 含 `..` / 绝对路径）
    BadName(String),
    /// 写出来的字节解不开或不完整（自己都不信这个包）
    Corrupt(&'static str),
    /// ZIP 写入失败
    Zip(ZipError),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::Secret { name, kind, offset, len } => write!(
                f,
                "诊断包里检测到敏感内容（{} 形态，位于 {} 偏移 {} 长度 {}）—— 已中止导出",
                kind.as_str(),
                name,
                offset,
                len
            ),
            PackageError::DuplicateName(n) => write!(f, "诊断包里出现重名条目：{n}"),
            PackageError::BadName(n) => write!(f, "诊断包条目名不合法：{n:?}"),
            PackageError::Corrupt(why) => write!(f, "诊断包字节不可用：{why}"),
            PackageError::Zip(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PackageError {}

impl From<ZipError> for PackageError {
    fn from(e: ZipError) -> Self {
        PackageError::Zip(e)
    }
}

/// 单个文件的 sha256（十六进制小写）。`manifest.json` 用它（[14 §11.2]）。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `manifest.json` 里的一个文件条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

impl ManifestEntry {
    pub fn of(file: &PackageFile) -> Self {
        Self {
            name: file.name.clone(),
            bytes: file.content.len() as u64,
            sha256: sha256_hex(&file.content),
        }
    }
}

/// 扫一段字节里有没有敏感形态。**非 UTF-8 也不放过**（按有损解码扫）。
pub fn scan_bytes(name: &str, bytes: &[u8]) -> Result<(), PackageError> {
    let text = String::from_utf8_lossy(bytes);
    if let Some(hit) = sanitize::find_secret(&text) {
        return Err(PackageError::Secret {
            name: name.to_string(),
            kind: hit.kind,
            offset: hit.start,
            len: hit.len,
        });
    }
    Ok(())
}

/// 打包**前**：每个输入文件都要干净（[14 §11.3] 的第一半）。
pub fn scan_for_secrets(files: &[PackageFile]) -> Result<(), PackageError> {
    for f in files {
        scan_bytes(&f.name, &f.content)?;
    }
    Ok(())
}

/// `manifest.json` 的内容（[14 §11.2]：生成时间、版本、每个文件的 sha256）。
///
/// 自己**不进清单**（清单没法包含自己的哈希），由调用方在文档里说明 ——
/// 这是唯一诚实的做法，硬塞一个占位哈希反而会让人以为校验通过。
pub fn manifest_json(
    entries: &[ManifestEntry],
    generated_ms: i64,
    app_version: &str,
    core_version: &str,
) -> String {
    let files: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| json!({ "name": e.name, "bytes": e.bytes, "sha256": e.sha256 }))
        .collect();
    let value = json!({
        "generated_at_ms": generated_ms,
        "app_version": app_version,
        "core_version": core_version,
        "note": "manifest.json 自身不在清单内（无法包含自己的哈希）",
        "files": files,
    });
    let mut s = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// 错误摘要文件的内容（[14 §11.2] 的"最近 N 条 WARN/ERROR 的码与计数"）。
pub fn error_summary_json(codes: &[CodeCount]) -> String {
    let rows: Vec<serde_json::Value> =
        codes.iter().map(|c| json!({ "code": c.code, "count": c.count })).collect();
    let value = json!({ "warn_error_codes": rows });
    let mut s = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// 按 [14 §11.2] 组包并写出 ZIP 字节。**任何一步命中敏感内容都返回 `Err`，不产出包。**
///
/// 归档结构：
///
/// ```text
/// manifest.json          ← 生成时间 / 版本 / 每个文件的 sha256
/// error-summary.json     ← 最近 WARN/ERROR 的码与计数（[14 §11.2]）
/// <调用方传进来的文件>    ← 日志、崩溃文件、脱敏后的配置、环境信息…
/// ```
///
/// 调用方（E3）负责：日志文件**已经**是脱敏后的内容、配置**已经**过
/// [`super::sanitize::sanitize_config_yaml`]。本函数是**最后那道闸**，不是第一道。
pub fn build_package(
    files: &[PackageFile],
    generated_ms: i64,
    app_version: &str,
    core_version: &str,
    error_summary: &[CodeCount],
) -> Result<Vec<u8>, PackageError> {
    check_unique_names(files)?;
    // 第一遍：输入的每个文件
    scan_for_secrets(files)?;

    let mut entries: Vec<PackageFile> = Vec::with_capacity(files.len() + 2);
    for f in files {
        entries.push(f.clone());
    }
    let manifest_entries: Vec<ManifestEntry> = files.iter().map(ManifestEntry::of).collect();
    let manifest = PackageFile::text(
        MANIFEST_NAME,
        &manifest_json(&manifest_entries, generated_ms, app_version, core_version),
    );
    let summary = PackageFile::text(ERROR_SUMMARY_NAME, &error_summary_json(error_summary));
    // 我们自己生成的这两个也扫（成本可忽略；"生成的所以一定干净"是假设，不是证据）
    scan_bytes(&manifest.name, &manifest.content)?;
    scan_bytes(&summary.name, &summary.content)?;
    entries.insert(0, manifest);
    entries.insert(1, summary);

    let borrowed: Vec<(&str, &[u8])> =
        entries.iter().map(|e| (e.name.as_str(), e.content.as_slice())).collect();
    let archive = zipstore::write_zip_with_mtime(&borrowed, generated_ms / 1000)?;

    // 第二遍（[14 §11.3]）：对**写出来的字节**解压 → 校验 CRC → 再全文扫一次。
    verify_and_scan_archive(&archive)?;
    Ok(archive)
}

fn check_unique_names(files: &[PackageFile]) -> Result<(), PackageError> {
    for f in files {
        if f.name == MANIFEST_NAME || f.name == ERROR_SUMMARY_NAME {
            return Err(PackageError::DuplicateName(f.name.clone()));
        }
    }
    let mut seen: Vec<&str> = Vec::with_capacity(files.len());
    for f in files {
        if seen.contains(&f.name.as_str()) {
            return Err(PackageError::DuplicateName(f.name.clone()));
        }
        seen.push(&f.name);
    }
    Ok(())
}

/// [14 §11.3] 的那一遍：**解压写出来的字节**，逐条校验 CRC，再扫内容。
///
/// 返回 `(归档内路径, 字节数)`，便于调用方写进自己的日志/UI。
/// 只支持我们自己写出的形态（`store`、无数据描述符）；遇到别的形态**报错而不是猜**。
pub fn verify_and_scan_archive(archive: &[u8]) -> Result<Vec<(String, usize)>, PackageError> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut o = 0usize;
    let mut total: u64 = 0;
    loop {
        let sig = read_u32(archive, o).ok_or(PackageError::Corrupt("读到中央目录之前就截断了"))?;
        if sig == 0x0201_4B50 {
            break; // 中央目录开始 → 本地条目结束
        }
        if sig != 0x0403_4B50 {
            return Err(PackageError::Corrupt("不是本地文件头（包被截断或不是我们写的）"));
        }
        let flags = read_u16(archive, o + 6).ok_or(PackageError::Corrupt("头被截断"))?;
        if flags & 0x0008 != 0 {
            return Err(PackageError::Corrupt("条目使用了数据描述符，本读取器不支持"));
        }
        let method = read_u16(archive, o + 8).ok_or(PackageError::Corrupt("头被截断"))?;
        if method != 0 {
            return Err(PackageError::Corrupt("条目不是 store 方式（本读取器不支持压缩）"));
        }
        let crc = read_u32(archive, o + 14).ok_or(PackageError::Corrupt("头被截断"))?;
        let size = read_u32(archive, o + 18).ok_or(PackageError::Corrupt("头被截断"))? as usize;
        let name_len = read_u16(archive, o + 26).ok_or(PackageError::Corrupt("头被截断"))? as usize;
        let extra_len =
            read_u16(archive, o + 28).ok_or(PackageError::Corrupt("头被截断"))? as usize;
        let name_start = o + 30;
        let data_start = name_start + name_len + extra_len;
        let data_end = data_start + size;
        if data_end > archive.len() {
            return Err(PackageError::Corrupt("条目数据超出包尾"));
        }
        let name = String::from_utf8(archive[name_start..name_start + name_len].to_vec())
            .map_err(|_| PackageError::Corrupt("条目名不是 UTF-8"))?;
        let data = &archive[data_start..data_end];
        if zipstore::crc32(data) != crc {
            return Err(PackageError::Corrupt("条目 CRC 不匹配（包写坏了）"));
        }
        scan_bytes(&name, data)?;
        total += size as u64;
        out.push((name, size));
        o = data_end;
    }
    if out.is_empty() && total == 0 {
        return Err(PackageError::Corrupt("包里一个条目都没有"));
    }
    Ok(out)
}

fn read_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 假密钥统一带 `ta-allow-fake-secret` 标记（避免撞上 pre-commit 的密钥扫描）。
    const FAKE_KEY: &str = "sk-ta-allow-fake-secret-9f3a2b1c4d5e";

    fn config_with_fake_key() -> String {
        format!("providers:\n  - id: p-openai\n    auth:\n      secret: {FAKE_KEY}\n")
    }

    fn build_with_sanitized_config() -> Vec<u8> {
        let raw_config = config_with_fake_key();
        let files = vec![
            PackageFile::text("config/config.yaml", &sanitize::sanitize_config_yaml(&raw_config)),
            PackageFile::text(
                "logs/tokenagency.log",
                "2026-02-11T12:04:22Z ERROR ta_gateway::inbound: 失败 request_id=req_1 code=TA-2005\n",
            ),
            PackageFile::text("env.json", "{\"os\":\"windows\",\"arch\":\"x86_64\"}\n"),
        ];
        build_package(
            &files,
            1_770_000_000_000,
            "0.1.0",
            crate::CORE_VERSION,
            &[CodeCount { code: "TA-2005".into(), count: 3 }],
        )
        .expect("脱敏后的内容必须能打出包")
    }

    // ------------------------------------------------ C3 正例：包里搜不到密钥

    #[test]
    fn diagnostic_package_contains_no_secret_end_to_end() {
        let archive = build_with_sanitized_config();
        // ① 原始字节里搜不到（store 方式 → 内容原样可见，等价于"解压后也搜不到"）
        let raw = String::from_utf8_lossy(&archive);
        assert!(!raw.contains(FAKE_KEY), "诊断包字节里出现了密钥");
        assert_eq!(sanitize::find_secret(&raw), None, "整包全文扫描必须零命中");
        // ② 解压后逐条再扫一遍（[14 §11.3] 的那一遍）
        let entries = verify_and_scan_archive(&archive).expect("自己写的包必须能解开");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "manifest.json",
                "error-summary.json",
                "config/config.yaml",
                "logs/tokenagency.log",
                "env.json"
            ]
        );
    }

    #[test]
    fn manifest_lists_every_input_file_with_its_sha256() {
        let archive = build_with_sanitized_config();
        let raw = String::from_utf8_lossy(&archive).into_owned();
        assert!(raw.contains("manifest.json"));
        let cleaned = sanitize::sanitize_config_yaml(&config_with_fake_key());
        let hash = sha256_hex(cleaned.as_bytes());
        assert!(raw.contains(&hash), "manifest 里应有该文件内容的 sha256");
        assert!(
            raw.contains("\"code\": \"TA-2005\"") && raw.contains("\"count\": 3"),
            "错误摘要缺失：{raw}"
        );
        // manifest 自身不进清单（否则就是一个永远对不上的哈希）
        assert!(raw.contains("manifest.json 自身不在清单内"));
    }

    // ------------------------------------------------ C3 反例：真有密钥就导不出来

    #[test]
    fn a_plaintext_secret_in_the_package_aborts_the_export() {
        let files = vec![PackageFile::text("config/config.yaml", &config_with_fake_key())];
        let err = build_package(&files, 1_770_000_000_000, "0.1.0", crate::CORE_VERSION, &[])
            .expect_err("含明文密钥时必须中止导出，而不是悄悄发出去");
        match err {
            PackageError::Secret { ref name, kind, .. } => {
                assert_eq!(name, "config/config.yaml");
                assert_eq!(kind, SecretKind::SkToken);
            }
            other => panic!("应当是 Secret 错误，实际：{other:?}"),
        }
        // 错误信息里**不许回显密钥原文**（否则错误日志本身就是泄漏点）
        assert!(!err.to_string().contains(FAKE_KEY), "{err}");
    }

    /// **守卫必须有证明力**：证明"包里确实会有密钥"这件事能被 [`verify_and_scan_archive`] 抓到 ——
    /// 也就是说上面那条 `Ok` 不是因为扫描器根本扫不出东西。
    #[test]
    fn the_postbuild_scan_actually_detects_a_leak() {
        // 故意绕过 build_package 的闸门，手工造一个"有人把明文配置直接塞进包"的包
        let files = [("config/config.yaml", config_with_fake_key())];
        let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(n, c)| (*n, c.as_bytes())).collect();
        let archive = zipstore::write_zip(&borrowed).unwrap();

        // 原始字节里**确实**有密钥（不然下面的判定是空的）
        assert!(
            String::from_utf8_lossy(&archive).contains(FAKE_KEY),
            "手造包应当真的把密钥写进去了，否则这条测试没有证明力"
        );
        // 而 §11.3 的那一遍必须抓住它
        let err = verify_and_scan_archive(&archive).expect_err("打包后扫描必须命中");
        assert!(matches!(err, PackageError::Secret { .. }), "{err:?}");
    }

    #[test]
    fn generated_files_do_not_false_positive() {
        // manifest 里全是 sha256 十六进制串，不许被脱敏规则误判成密钥
        let entries =
            [ManifestEntry { name: "a.log".into(), bytes: 3, sha256: sha256_hex(b"abc") }];
        let text = manifest_json(&entries, 1, "0.1.0", "0.1.0");
        assert_eq!(sanitize::find_secret(&text), None, "{text}");
        assert_eq!(sanitize::find_secret(&error_summary_json(&[])), None);
    }

    // ------------------------------------------------ 自检：坏包、重名

    #[test]
    fn duplicate_and_reserved_names_are_rejected() {
        let dup = vec![PackageFile::text("logs/a.log", "x"), PackageFile::text("logs/a.log", "y")];
        assert_eq!(
            build_package(&dup, 0, "0.1.0", "0.1.0", &[]).unwrap_err(),
            PackageError::DuplicateName("logs/a.log".into())
        );
        let reserved = vec![PackageFile::text(MANIFEST_NAME, "{}")];
        assert_eq!(
            build_package(&reserved, 0, "0.1.0", "0.1.0", &[]).unwrap_err(),
            PackageError::DuplicateName(MANIFEST_NAME.into())
        );
        let bad = vec![PackageFile::text("../escape.log", "x")];
        assert!(matches!(
            build_package(&bad, 0, "0.1.0", "0.1.0", &[]).unwrap_err(),
            PackageError::Zip(ZipError::BadName(_))
        ));
    }

    #[test]
    fn a_truncated_archive_is_reported_not_accepted() {
        let archive = build_with_sanitized_config();
        let cut = &archive[..archive.len() / 2];
        assert!(matches!(verify_and_scan_archive(cut), Err(PackageError::Corrupt(_))));
        assert!(verify_and_scan_archive(&[]).is_err());
    }

    #[test]
    fn crc_mismatch_is_caught() {
        let archive = build_with_sanitized_config();
        let mut damaged = archive.clone();
        // 改第一个条目（manifest.json）的数据区第一个字节：header 30 字节 + 名字 13 字节之后
        let pos = 30 + MANIFEST_NAME.len();
        assert!(pos < damaged.len());
        damaged[pos] = damaged[pos].wrapping_add(1);
        match verify_and_scan_archive(&damaged) {
            Err(PackageError::Corrupt(_)) | Err(PackageError::Secret { .. }) => {}
            other => panic!("改坏字节必须被抓住，实际：{other:?}"),
        }
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

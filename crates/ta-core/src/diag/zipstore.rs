//! 极简 ZIP 打包（**store 方式，不压缩**）—— 诊断包导出用（[14 §11](../../../docs/14-observability.md)）。
//!
//! 为什么手写：诊断包要能被用户丢进资源管理器 / `Expand-Archive` 解压，
//! 而"不许新增依赖"是本次的硬约束（没有 `zip` crate 可用，std 也没有 deflate）。
//! **代价**：包不压缩（诊断包主要是日志文本，会大一些）。这一点写进函数文档，
//! 免得以后有人以为它压过了。
//!
//! 只做**写出**：读回是 Windows/7-Zip 的事；测试里带一个最小的解析器证明写出来的
//! 字节符合规范（中央目录 + 本地头 + CRC 都能被独立解析）。

use std::io::Write as _;

/// 打包失败的原因。**宁可报错，也不写一个坏包**（坏包会让用户以为"日志没东西"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// 归档内路径为空、含 NUL，或以 `/` 之外的方式以分隔符结尾
    BadName(String),
    /// 单个条目或整体超过 ZIP 的 32 位限制（4 GiB / 65535 条目）
    TooLarge(&'static str),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::BadName(n) => write!(f, "归档内路径不合法：{n:?}"),
            ZipError::TooLarge(what) => write!(f, "诊断包超过 ZIP 限制：{what}"),
        }
    }
}

/// IEEE 802.3 CRC-32（ZIP 用的反射多项式 `0xEDB88320`）。
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: once_cell::sync::Lazy<[u32; 256]> = once_cell::sync::Lazy::new(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = TABLE[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// 打包成 ZIP（store）。`entries` 是 `(归档内路径, 内容)`；以 `/` 结尾的是目录。
///
/// 条目修改时间统一用 `mtime_unix_secs`（诊断包内所有文件同一个时间戳即可，
/// 反正 `manifest.json` 才是权威）。
pub fn write_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, ZipError> {
    write_zip_with_mtime(entries, 0)
}

/// 同 [`write_zip`]，但指定修改时间（Unix 秒，UTC）。
pub fn write_zip_with_mtime(
    entries: &[(&str, &[u8])],
    mtime_unix_secs: i64,
) -> Result<Vec<u8>, ZipError> {
    if entries.len() > u16::MAX as usize {
        return Err(ZipError::TooLarge("条目数超过 65535"));
    }
    let (dos_time, dos_date) = dos_datetime(mtime_unix_secs);
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();

    for (name, data) in entries {
        validate_name(name)?;
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| ZipError::TooLarge("条目名过长"))?;
        let size =
            u32::try_from(data.len()).map_err(|_| ZipError::TooLarge("单个条目超过 4 GiB"))?;
        let offset =
            u32::try_from(out.len()).map_err(|_| ZipError::TooLarge("归档总大小超过 4 GiB"))?;
        let crc = crc32(data);

        // ---- Local File Header (30 字节固定部分) ----
        push_u32(&mut out, 0x0403_4B50);
        push_u16(&mut out, 20); // version needed
        push_u16(&mut out, 0x0800); // 通用标志位：UTF-8 名字
        push_u16(&mut out, 0); // method = store
        push_u16(&mut out, dos_time);
        push_u16(&mut out, dos_date);
        push_u32(&mut out, crc);
        push_u32(&mut out, size); // compressed == uncompressed（store）
        push_u32(&mut out, size);
        push_u16(&mut out, name_len);
        push_u16(&mut out, 0); // extra len
        out.write_all(name_bytes).map_err(|_| ZipError::TooLarge("写入失败"))?;
        out.write_all(data).map_err(|_| ZipError::TooLarge("写入失败"))?;

        // ---- Central Directory File Header (46 字节固定部分) ----
        push_u32(&mut central, 0x0201_4B50);
        push_u16(&mut central, 20); // version made by
        push_u16(&mut central, 20); // version needed
        push_u16(&mut central, 0x0800);
        push_u16(&mut central, 0);
        push_u16(&mut central, dos_time);
        push_u16(&mut central, dos_date);
        push_u32(&mut central, crc);
        push_u32(&mut central, size);
        push_u32(&mut central, size);
        push_u16(&mut central, name_len);
        push_u16(&mut central, 0); // extra
        push_u16(&mut central, 0); // comment
        push_u16(&mut central, 0); // disk number start
        push_u16(&mut central, 0); // internal attrs
        push_u32(&mut central, external_attrs(name));
        push_u32(&mut central, offset);
        central.write_all(name_bytes).map_err(|_| ZipError::TooLarge("写入失败"))?;
    }

    let cd_offset =
        u32::try_from(out.len()).map_err(|_| ZipError::TooLarge("归档总大小超过 4 GiB"))?;
    let cd_size = u32::try_from(central.len()).map_err(|_| ZipError::TooLarge("中央目录过大"))?;
    out.extend_from_slice(&central);

    // ---- End of Central Directory (22 字节) ----
    let count = u16::try_from(entries.len()).map_err(|_| ZipError::TooLarge("条目数超过 65535"))?;
    push_u32(&mut out, 0x0605_4B50);
    push_u16(&mut out, 0); // this disk
    push_u16(&mut out, 0); // disk with CD
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, cd_size);
    push_u32(&mut out, cd_offset);
    push_u16(&mut out, 0); // comment len
    Ok(out)
}

fn validate_name(name: &str) -> Result<(), ZipError> {
    if name.is_empty() || name.contains('\0') || name.starts_with('/') || name.contains("..") {
        return Err(ZipError::BadName(name.to_string()));
    }
    Ok(())
}

/// 目录条目要有目录位，否则解压器会把它当空文件。
fn external_attrs(name: &str) -> u32 {
    if name.ends_with('/') {
        0o040755u32 << 16 | 0x10
    } else {
        0o100644u32 << 16
    }
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Unix 秒 → DOS 日期/时间（ZIP 用，UTC，秒只有 2 秒精度）。
///
/// 可表示范围是 1980-01-01..2107-12-31；超出（含 `0`）一律落到 1980-01-01 00:00:00，
/// 因为 ZIP 没有"未知时间"的表示，而写一个越界的位段会产生坏包。
fn dos_datetime(unix_secs: i64) -> (u16, u16) {
    const MIN: i64 = 315_532_800; // 1980-01-01T00:00:00Z
    const MAX: i64 = 4_354_819_199; // 2107-12-31T23:59:59Z
    let secs = if (MIN..=MAX).contains(&unix_secs) { unix_secs } else { MIN };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hour = rem / 3_600;
    let min = (rem % 3_600) / 60;
    let sec = rem % 60;
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = ((hour as u16) << 11) | ((min as u16) << 5) | (sec as u16 / 2);
    (time, date)
}

/// 天数（自 1970-01-01）→ 公历年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ------------------------------------------------ CRC

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    // ------------------------------------------------ 最小解析器（证明写出来的是合法 ZIP）

    #[derive(Debug, PartialEq, Eq)]
    struct Entry {
        name: String,
        crc: u32,
        size: u32,
        offset: u32,
    }

    fn parse(zip: &[u8]) -> Vec<Entry> {
        // EOCD 在最后 22 字节（我们没有注释）
        let eocd = zip.len() - 22;
        assert_eq!(u32::from_le_bytes(zip[eocd..eocd + 4].try_into().unwrap()), 0x0605_4B50);
        let count = u16::from_le_bytes(zip[eocd + 10..eocd + 12].try_into().unwrap()) as usize;
        let cd_offset = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let mut p = cd_offset;
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32::from_le_bytes(zip[p..p + 4].try_into().unwrap()), 0x0201_4B50);
            let crc = u32::from_le_bytes(zip[p + 16..p + 20].try_into().unwrap());
            let size = u32::from_le_bytes(zip[p + 24..p + 28].try_into().unwrap());
            let name_len = u16::from_le_bytes(zip[p + 28..p + 30].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(zip[p + 30..p + 32].try_into().unwrap()) as usize;
            let comment_len = u16::from_le_bytes(zip[p + 32..p + 34].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(zip[p + 42..p + 46].try_into().unwrap());
            let name = String::from_utf8(zip[p + 46..p + 46 + name_len].to_vec()).unwrap();
            out.push(Entry { name, crc, size, offset });
            p += 46 + name_len + extra_len + comment_len;
        }
        out
    }

    fn read_entry(zip: &[u8], e: &Entry) -> Vec<u8> {
        let p = e.offset as usize;
        assert_eq!(u32::from_le_bytes(zip[p..p + 4].try_into().unwrap()), 0x0403_4B50);
        let name_len = u16::from_le_bytes(zip[p + 26..p + 28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(zip[p + 28..p + 30].try_into().unwrap()) as usize;
        let start = p + 30 + name_len + extra_len;
        zip[start..start + e.size as usize].to_vec()
    }

    // ------------------------------------------------ 结构

    #[test]
    fn empty_archive_is_a_valid_eocd_only_zip() {
        let zip = write_zip(&[]).expect("空包也要能写");
        assert_eq!(zip.len(), 22);
        assert_eq!(&zip[..4], &0x0605_4B50u32.to_le_bytes());
        assert!(parse(&zip).is_empty());
    }

    #[test]
    fn single_entry_roundtrips_with_matching_crc() {
        let data = "2026-02-11 INFO tokenagency 启动\n".as_bytes();
        let zip = write_zip(&[("tokenagency.log", data)]).expect("应能打包");
        let entries = parse(&zip);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "tokenagency.log");
        assert_eq!(entries[0].size, data.len() as u32);
        assert_eq!(entries[0].crc, crc32(data));
        assert_eq!(read_entry(&zip, &entries[0]), data.to_vec());
    }

    #[test]
    fn multiple_entries_and_directories_roundtrip() {
        let entries = [
            ("manifest.json", br#"{"version":"0.1.0"}"#.as_slice()),
            ("logs/", b"".as_slice()),
            ("logs/tokenagency.log", b"line1\nline2\n".as_slice()),
            ("logs/crash-20260211-120422.log", b"panic!\n".as_slice()),
            ("config/redacted.yaml", b"gateway:\n  port: 8787\n".as_slice()),
        ];
        let zip = write_zip(&entries).expect("应能打包");
        let parsed = parse(&zip);
        assert_eq!(parsed.len(), entries.len());
        for (i, (name, data)) in entries.iter().enumerate() {
            assert_eq!(parsed[i].name, *name);
            assert_eq!(read_entry(&zip, &parsed[i]), data.to_vec());
            assert_eq!(parsed[i].crc, crc32(data));
        }
    }

    #[test]
    fn utf8_names_use_byte_length() {
        let name = "日志/中文文件名.log";
        let zip = write_zip(&[(name, b"x".as_slice())]).expect("应能打包");
        let parsed = parse(&zip);
        assert_eq!(parsed[0].name, name, "UTF-8 名字要按字节写长度，否则解压出来是乱码");
        assert!(name.len() > name.chars().count(), "这条测试必须真的包含多字节字符");
    }

    #[test]
    fn directory_entries_carry_the_directory_bit() {
        assert_eq!(external_attrs("logs/") & 0x10, 0x10);
        assert_eq!(external_attrs("logs/tokenagency.log") & 0x10, 0);
    }

    #[test]
    fn bad_names_are_rejected_instead_of_written_broken() {
        assert_eq!(write_zip(&[("", b"x".as_slice())]), Err(ZipError::BadName(String::new())));
        assert!(matches!(
            write_zip(&[("../../etc/passwd", b"x".as_slice())]),
            Err(ZipError::BadName(_))
        ));
        assert!(matches!(write_zip(&[("/abs", b"x".as_slice())]), Err(ZipError::BadName(_))));
    }

    #[test]
    fn binary_content_is_preserved_byte_for_byte() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let zip = write_zip(&[("blob.bin", data.as_slice())]).expect("应能打包");
        let parsed = parse(&zip);
        assert_eq!(read_entry(&zip, &parsed[0]), data);
    }

    // ------------------------------------------------ 时间字段

    #[test]
    fn dos_time_conversion() {
        // 1980-01-01T00:00:00Z → date 0x0021（年 0/月 1/日 1）, time 0
        assert_eq!(dos_datetime(315_532_800), (0, 0x0021));
        // 2026-02-11T12:04:22Z（UTC）→ 年 46, 月 2, 日 11, 12:04:22 → 秒 22/2 = 11
        let (t, d) = dos_datetime(1_770_811_462);
        assert_eq!(d, ((2026 - 1980) << 9) | (2 << 5) | 11);
        assert_eq!(t, (12 << 11) | (4 << 5) | 11);
    }

    #[test]
    fn out_of_range_timestamps_fall_back_to_1980() {
        assert_eq!(dos_datetime(0), (0, 0x0021));
        assert_eq!(dos_datetime(-1), (0, 0x0021));
        assert_eq!(dos_datetime(i64::MAX), (0, 0x0021));
    }

    #[test]
    fn manifest_text_is_scannable_after_roundtrip() {
        // 诊断包导出后要能对每个条目做"再扫一遍密钥"，所以内容必须能原样取回
        let manifest =
            r#"{"generated_at_ms":1770811462000,"files":{"logs/tokenagency.log":"ab12"}}"#;
        let zip = write_zip(&[("manifest.json", manifest.as_bytes())]).expect("应能打包");
        let parsed = parse(&zip);
        let back = String::from_utf8(read_entry(&zip, &parsed[0])).expect("必须是 UTF-8");
        assert_eq!(back, manifest);
    }
}

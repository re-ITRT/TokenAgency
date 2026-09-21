//! 内部小工具：错误映射、JSON、zstd、哈希、CSV、数字格式化。
//!
//! 全部只在 crate 内使用（`pub(crate)`），不构成对外契约。

use std::fmt::Display;

use sha2::{Digest, Sha256};

use ta_core::error::{Result, TaError};

pub(crate) fn store_err<E: Display>(e: E) -> TaError {
    TaError::Store(e.to_string())
}

/// 把任意 `Result<T, E: Display>` 收敛成 `Result<T, TaError>`。
pub(crate) trait StoreResultExt<T> {
    fn store_err(self) -> Result<T>;
}

impl<T, E: Display> StoreResultExt<T> for std::result::Result<T, E> {
    fn store_err(self) -> Result<T> {
        self.map_err(store_err)
    }
}

/// u64 → i64 的**饱和**转换（SQLite 的 INTEGER 是 i64）。
///
/// 直接 `as i64` 在超过 i64::MAX 时会变成负数（静默翻转），对 token 数/金额这类
/// 账目数据是不可接受的；这里宁可钉在上限，也不要翻转。
pub(crate) fn to_i64(v: u64) -> i64 {
    if v > i64::MAX as u64 {
        i64::MAX
    } else {
        v as i64
    }
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).store_err()
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).store_err()
}

/// 解析 JSON，失败时回退到默认值（库里存的历史脏数据不该让整个看板崩掉）。
pub(crate) fn from_json_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

pub(crate) fn parse_json_or_null(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
}

// ------------------------------------------------------------------ 压缩

/// zstd 压缩（07 §2.2：body 存压缩 BLOB，聊天文本常见 5–10×）。
pub(crate) fn compress(data: &[u8]) -> Result<Vec<u8>> {
    zstd::stream::encode_all(data, 3).store_err()
}

pub(crate) fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    zstd::stream::decode_all(data).store_err()
}

// ------------------------------------------------------------------ 哈希

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 图片只记 sha256 前 N 位（默认 8），足以去重与比对，无法逆推内容。
pub(crate) fn sha256_prefix(data: &[u8], n: usize) -> String {
    let full = sha256_hex(data);
    full.chars().take(n.min(full.len())).collect()
}

// ------------------------------------------------------------------ 数字 / CSV

/// 大数字缩写：`12400000` → `12.4M`（07 §1.3 数字排版）。
pub(crate) fn human_tokens(v: f64) -> String {
    let a = v.abs();
    let fmt = |x: f64, unit: &str| {
        let s = format!("{x:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        format!("{s}{unit}")
    };
    if a >= 1e12 {
        fmt(v / 1e12, "T")
    } else if a >= 1e9 {
        fmt(v / 1e9, "B")
    } else if a >= 1e6 {
        fmt(v / 1e6, "M")
    } else if a >= 1e3 {
        fmt(v / 1e3, "K")
    } else {
        format!("{}", v.round() as i64)
    }
}

/// 计数千分位：`1284` → `1,284`（07 §1.3）。
pub(crate) fn human_count(v: f64) -> String {
    let n = v.round() as i64;
    let neg = n < 0;
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

pub(crate) fn fmt_amount(v: f64) -> String {
    format!("{v:.4}")
}

/// CSV 字段转义（含逗号/引号/换行时加引号，内部引号翻倍）。
pub(crate) fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// CSV 数字：整数不带小数点，浮点保留 6 位有效小数并去掉尾零。
pub(crate) fn csv_num(v: f64) -> String {
    if v.fract().abs() < f64::EPSILON && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_formats() {
        assert_eq!(human_tokens(12_400_000.0), "12.4M");
        assert_eq!(human_tokens(842_000.0), "842K");
        assert_eq!(human_tokens(999.0), "999");
        assert_eq!(human_count(1284.0), "1,284");
        assert_eq!(human_count(999.0), "999");
    }

    #[test]
    fn csv_escapes() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("he said \"hi\""), "\"he said \"\"hi\"\"\"");
        assert_eq!(csv_num(1.0), "1");
        assert_eq!(csv_num(0.5), "0.5");
    }

    #[test]
    fn compress_roundtrip_and_hash() {
        let data = b"hello hello hello hello".repeat(100);
        let z = compress(&data).unwrap();
        assert!(z.len() < data.len());
        assert_eq!(decompress(&z).unwrap(), data);
        assert_eq!(sha256_prefix(b"abc", 8).len(), 8);
    }
}

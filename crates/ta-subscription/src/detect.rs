//! 格式嗅探（[11 §3] 响应解析）。
//!
//! 判定顺序（内容永远比文件名可信）：
//! 1. `<!DOCTYPE` / `<html` → [`DetectedFormat::Html`]
//! 2. 行首 `proxies:` / `proxy-groups:` / `proxy-providers:` → [`DetectedFormat::ClashYaml`]
//! 3. 整段 base64 可解码（容忍缺 padding、URL-safe 变体）且解码后含 `://` → [`DetectedFormat::Base64Links`]
//! 4. 含 `://` → [`DetectedFormat::PlainLinks`]
//! 5. 否则 [`DetectedFormat::Unknown`]
//!
//! `Content-Disposition` 的文件名只作**兜底提示**：内容嗅探失败（Unknown）时才用它。
//! 理由：文件名是 `config.yaml` 而内容是 HTML 错误页的情况很常见，此时必须判 Html。

use std::sync::OnceLock;

use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use base64::Engine as _;
use regex::Regex;

use crate::DetectedFormat;

/// 内容嗅探（不看文件名）。
pub fn detect_format(raw: &[u8]) -> DetectedFormat {
    sniff(raw)
}

/// 内容嗅探 + 文件名兜底提示。
pub fn detect_format_with_hint(raw: &[u8], filename_hint: Option<&str>) -> DetectedFormat {
    let content = sniff(raw);
    if content != DetectedFormat::Unknown {
        return content;
    }
    hint_format(filename_hint).unwrap_or(DetectedFormat::Unknown)
}

/// 按文件名/扩展名给出的"弱提示"。返回 `None` 表示这个扩展名无法判定。
pub fn hint_format(filename: Option<&str>) -> Option<DetectedFormat> {
    let name = filename?.trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }
    let base = name.split(['?', '#']).next().unwrap_or(name);
    let ext = base
        .rsplit('/')
        .next()
        .unwrap_or(base)
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())?;
    match ext.as_str() {
        "yaml" | "yml" => Some(DetectedFormat::ClashYaml),
        "html" | "htm" => Some(DetectedFormat::Html),
        // `.txt` 既可能是 base64 链接也可能是明文链接，交给内容判断。
        _ => None,
    }
}

fn sniff(raw: &[u8]) -> DetectedFormat {
    let body = strip_bom(raw);
    if body.is_empty() {
        return DetectedFormat::Unknown;
    }
    if looks_like_html(body) {
        return DetectedFormat::Html;
    }
    let text = String::from_utf8_lossy(body);
    if looks_like_clash_yaml(&text) {
        return DetectedFormat::ClashYaml;
    }
    if let Some(decoded) = decode_base64_text(&text) {
        if decoded.contains("://") {
            return DetectedFormat::Base64Links;
        }
    }
    if text.contains("://") {
        return DetectedFormat::PlainLinks;
    }
    DetectedFormat::Unknown
}

fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw)
}

/// HTML 错误页 / 登录页 / 被墙页。
pub fn looks_like_html(raw: &[u8]) -> bool {
    // 只看前 4 KiB：错误页的 doctype/html 一定在前面，避免大 body 全量扫。
    let head = &raw[..raw.len().min(4096)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    text.contains("<!doctype") || text.contains("<html")
}

/// Clash 配置：行首（允许少量缩进）出现 `proxies:` / `proxy-groups:` / `proxy-providers:`。
pub fn looks_like_clash_yaml(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]{0,2}(proxies|proxy-groups|proxy-providers)[ \t]*:")
            .expect("静态正则必须合法")
    });
    re.is_match(text)
}

/// 宽松 base64 解码：忽略空白、容忍缺失 padding、同时接受标准与 URL-safe 变体。
pub fn decode_base64_lenient(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    let trimmed = cleaned.trim_end_matches('=');
    let mut padded = String::with_capacity(trimmed.len() + 3);
    padded.push_str(trimmed);
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    STANDARD.decode(padded.as_bytes()).ok().or_else(|| URL_SAFE.decode(padded.as_bytes()).ok())
}

/// 整段文本就是 base64 且解码后是合法 UTF-8 时，返回解码结果。
pub fn decode_base64_text(s: &str) -> Option<String> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    // 只可能是 base64 的字符集；出现 `:` 之类说明本来就是明文订阅。
    if cleaned.len() < 12 || !cleaned.chars().all(is_base64_char) {
        return None;
    }
    let bytes = decode_base64_lenient(&cleaned)?;
    let text = String::from_utf8(bytes).ok()?;
    // 解码成功但含控制字符（二进制乱码）时不算命中。
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t') {
        return None;
    }
    Some(text)
}

/// 把订阅原文变成"可解析的文本"：Clash YAML / 明文链接直接 lossy 解码，
/// base64 链接先解码一次。嗅探失败时返回 lossy 原文（不报错，交给校验层拒）。
pub fn to_parseable_text(raw: &[u8], format: DetectedFormat) -> String {
    let text = String::from_utf8_lossy(strip_bom(raw)).into_owned();
    match format {
        DetectedFormat::Base64Links => decode_base64_text(&text).unwrap_or(text),
        _ => text,
    }
}

fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        STANDARD.encode(s.as_bytes())
    }

    #[test]
    fn html_wins_over_other_signals() {
        let body = b"<!DOCTYPE html>\n<html><head><title>502</title></head>\n\
                     <body>proxies: nope https://x</body></html>";
        assert_eq!(detect_format(body), DetectedFormat::Html);
    }

    #[test]
    fn html_is_case_insensitive_and_bom_tolerant() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice(b"\n\n<HTML><body>temporarily unavailable</body></HTML>");
        assert_eq!(detect_format(&body), DetectedFormat::Html);
    }

    #[test]
    fn yaml_is_detected_by_top_level_key() {
        let body = b"proxies:\n  - {name: a, type: ss}\nproxy-groups: []\n";
        assert_eq!(detect_format(body), DetectedFormat::ClashYaml);
        assert!(looks_like_clash_yaml("proxy-groups:\n  - name: x"));
    }

    #[test]
    fn yaml_key_must_be_a_key_not_a_substring() {
        // 链接里的备注含 "proxies:"，不能因此判成 YAML
        let body = b"ss://YWVzOnh4@1.2.3.4:8388#my%20proxies:%20test";
        assert_eq!(detect_format(body), DetectedFormat::PlainLinks);
    }

    #[test]
    fn base64_links_are_detected_in_all_variants() {
        let payload = "ss://YWVzLTI1Ni1nY206cGFzcw@1.2.3.4:8388#HK\ntrojan://p@h.example.com:443";
        // 标准 + padding
        assert_eq!(detect_format(b64(payload).as_bytes()), DetectedFormat::Base64Links);
        // 缺 padding
        let no_pad = b64(payload).trim_end_matches('=').to_string();
        assert_eq!(detect_format(no_pad.as_bytes()), DetectedFormat::Base64Links);
        // URL-safe
        let url_safe = no_pad.replace('+', "-").replace('/', "_");
        assert_eq!(detect_format(url_safe.as_bytes()), DetectedFormat::Base64Links);
        // 带换行（真实机场经常 64 列折行）
        let wrapped = b64(payload)
            .as_bytes()
            .chunks(48)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n");
        assert_eq!(detect_format(wrapped.as_bytes()), DetectedFormat::Base64Links);
    }

    #[test]
    fn plain_links_and_unknown() {
        assert_eq!(detect_format(b"vmess://eyJ2IjoiMiJ9\n# comment\n"), DetectedFormat::PlainLinks);
        assert_eq!(detect_format(b"just some words"), DetectedFormat::Unknown);
        assert_eq!(detect_format(b""), DetectedFormat::Unknown);
    }

    #[test]
    fn filename_hint_only_used_when_content_is_unknown() {
        // 内容是 HTML，文件名却说 yaml → 内容赢
        assert_eq!(
            detect_format_with_hint(b"<html>err</html>", Some("config.yaml")),
            DetectedFormat::Html
        );
        // 内容无法判定 → 用文件名
        assert_eq!(
            detect_format_with_hint(b"\x01\x02\x03 not a subscription", Some("a.yml")),
            DetectedFormat::ClashYaml
        );
        assert_eq!(detect_format_with_hint(b"\x01\x02\x03", Some("a.html")), DetectedFormat::Html);
        assert_eq!(
            detect_format_with_hint(b"\x01\x02\x03", Some("a.txt")),
            DetectedFormat::Unknown
        );
        assert_eq!(hint_format(Some("/dl/sub.yaml?t=1")), Some(DetectedFormat::ClashYaml));
        assert_eq!(hint_format(None), None);
        assert_eq!(hint_format(Some("noext")), None);
    }

    #[test]
    fn base64_decoder_tolerates_noise() {
        assert_eq!(decode_base64_lenient("aGk").as_deref(), Some(b"hi".as_ref()));
        assert_eq!(decode_base64_lenient("aGk=").as_deref(), Some(b"hi".as_ref()));
        assert_eq!(decode_base64_lenient("aGk=\n").as_deref(), Some(b"hi".as_ref()));
        assert!(decode_base64_lenient("!!!!").is_none());
        assert!(decode_base64_lenient("").is_none());
    }

    #[test]
    fn parseable_text_decodes_base64_payloads() {
        let payload = "ss://YWVzOnh4@1.2.3.4:8388#HK";
        let text = to_parseable_text(b64(payload).as_bytes(), DetectedFormat::Base64Links);
        assert_eq!(text, payload);
        // YAML 原样返回
        let yaml = "proxies: []\n";
        assert_eq!(to_parseable_text(yaml.as_bytes(), DetectedFormat::ClashYaml), yaml);
    }
}

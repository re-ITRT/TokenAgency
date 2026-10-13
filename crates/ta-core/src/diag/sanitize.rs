//! ★ 脱敏规则—— 应用日志与诊断包的**唯一出口防线**。
//!
//! 为什么要有这一层：`ta-secrets` 的 `SecretString` 只在**类型**层面脱敏，
//! 它挡不住"有人把密钥拼进了 message 字符串"这类绕过。**对最终输出的字符串再扫一遍，
//! 是唯一能兜住未知写法的一层** —— 而日志文件是"用户随手就能发出去"的东西。
//!
//! 覆盖的 6 类形态：
//!
//! | 形态 | 处理 |
//! |---|---|
//! | `sk-…` 长 token / `Bearer <token>` | `***` |
//! | `x-api-key` / `api-key` / `authorization` / `cookie` 的值 | `***`（保留键名，排查仍能看出"这里有个 key"） |
//! | `data:image/…;base64,…` | `data:image/…;base64,<len=N sha256=xxxxxxxx>`（与 同口径） |
//! | JWT（`eyJ….….…` 三段） | `***` |
//! | URL 里的 `?key=` / `&token=` / `&api_key=` / `&password=` 值 | `***` |
//! | 便携密钥库密文特征 | `***` |
//!
//! **反例同样重要**：过度脱敏会让日志失去排查价值，那也是一种失败。
//! 所以 `tests` 里既有"密钥搜不到"，也有"普通中文/代码/不含密钥的 URL 原样通过"。

use once_cell::sync::Lazy;
use regex::Regex;
use sha2::{Digest, Sha256};

/// 便携密钥库（`ta-secrets::portable`）的文件头特征。
const KEYSTORE_FORMAT_TAG: &str = "tokenagency.portable-secrets";

/// 六类敏感形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// `sk-…` / `sk-proj-…` / `sk-ant-…`
    SkToken,
    /// `Bearer <token>`
    Bearer,
    /// `x-api-key:` / `api_key:` / `authorization:` / `cookie:` 的值
    AuthHeader,
    /// `data:image/…;base64,…`（既是隐私又是体积杀手）
    DataUrlImage,
    /// JWT
    Jwt,
    /// URL query 里的 key/token/password
    UrlQuery,
    /// 便携密钥库的密文
    PortableKeystore,
}

impl SecretKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretKind::SkToken => "sk_token",
            SecretKind::Bearer => "bearer",
            SecretKind::AuthHeader => "auth_header",
            SecretKind::DataUrlImage => "data_url_image",
            SecretKind::Jwt => "jwt",
            SecretKind::UrlQuery => "url_query",
            SecretKind::PortableKeystore => "portable_keystore",
        }
    }
}

/// 命中一处敏感内容（诊断包导出前的自检用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub kind: SecretKind,
    /// 在输入里的字节偏移
    pub start: usize,
    pub len: usize,
}

/// 把文本里所有敏感形态替换掉。**幂等**：`sanitize(sanitize(x)) == sanitize(x)`。
pub fn sanitize(input: &str) -> String {
    sanitize_with(&RULES, input)
}

/// 是否含敏感形态（返回第一处命中）。诊断包"打完之后再扫一遍"用它。
pub fn find_secret(input: &str) -> Option<SecretHit> {
    find_secret_with(&RULES, input)
}

/// 配置 YAML 脱敏：**保留结构，只把密钥类字段的值换成 `***`**。
///
/// 要的是"密钥替换为 ***，但保留通道/线路结构 —— 结构才是排查要看的"，
/// 所以这里逐行只动 `键: 值` 里的值，不碰键、不碰缩进、不碰 `${secret:name}` 这类引用
/// （引用本身不含密钥材料）。
pub fn sanitize_config_yaml(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    for line in yaml.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        out.push_str(&sanitize_yaml_line(body));
        out.push_str(nl);
    }
    // 第二道：行级规则管不到的地方（URL 里的 key、内联 JSON）再按通用规则扫一遍
    sanitize(&out)
}

fn sanitize_yaml_line(line: &str) -> String {
    let trimmed = line.trim_start();
    // 只处理 `键: 值`，且值不是块状结构（保持 YAML 可读）
    let Some(colon) = trimmed.find(':') else { return line.to_string() };
    let key = &trimmed[..colon];
    if key.is_empty() || key.contains(' ') || !is_sensitive_key(key) {
        return line.to_string();
    }
    let value_raw = &trimmed[colon + 1..];
    let value = value_raw.trim();
    if value.is_empty()
        || value.starts_with('{')
        || value.starts_with('[')
        || value.starts_with('|')
    {
        return line.to_string();
    }
    // `${secret:name}` / `${env:VAR}` 是引用，不是密钥材料 → 原样保留（引用进诊断包是安全的）
    let unquoted = value.trim_matches('"').trim_matches('\'');
    if unquoted.starts_with("${") && unquoted.ends_with('}') {
        return line.to_string();
    }
    // 精确重建：保留缩进、键名与值前后的空白，**只替换值本身**
    // （不能用 `line.len() - value.len()` 反推，值后面若有空格就会切到值中间）
    let indent = &line[..line.len() - trimmed.len()];
    let lead = value_raw.len() - value_raw.trim_start().len();
    let tail = &value_raw[lead + value.len()..];
    format!("{indent}{}{}***{tail}", &trimmed[..=colon], &value_raw[..lead])
}

/// 键名是否"值里应该放密钥"（大小写不敏感，允许 `-`/`_`）。
fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    let k = k.trim_matches(|c: char| c == '"' || c == '\'');
    [
        "secret",
        "api_key",
        "apikey",
        "api-key",
        "token",
        "password",
        "passwd",
        "authorization",
        "cookie",
        "credential",
    ]
    .iter()
    .any(|needle| k == *needle || k.ends_with(needle) || k.starts_with(needle))
}

// ---------------------------------------------------------------- 规则实现

struct Rules {
    sk: Option<Regex>,
    bearer: Option<Regex>,
    header: Option<Regex>,
    data_url: Option<Regex>,
    jwt: Option<Regex>,
    url_query: Option<Regex>,
    keystore_field: Option<Regex>,
    blob: Option<Regex>,
}

fn compile(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

impl Rules {
    fn compile() -> Self {
        Self {
            sk: compile(r"(?i)\bsk-[A-Za-z0-9_\-]{8,}"),
            // ★ 用 `[ \t]+` 而不是 `\s+`：`\s` 会跨过换行，把下一行的首个单词当成令牌吞掉
            // （测试 `bearer_word_at_end_of_line_does_not_eat_the_next_line` 抓的就是这个）
            bearer: compile(r"(?i)\b(bearer)[ \t]+[A-Za-z0-9._~+/=\-]{8,}"),
            // 键名后允许一个引号，这样 JSON 形态 `"api_key": "…"` 也能命中
            header: compile(
                r#"(?i)(\b(?:x-api-key|api[_-]?key|authorization|cookie)\b"?\s*[:=]\s*)("?)([^\s"',;\r\n]+)"#,
            ),
            data_url: compile(r"(?i)data:image/([a-z0-9.+-]+);base64,([A-Za-z0-9+/=\s]{32,})"),
            jwt: compile(r"\beyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}"),
            url_query: compile(
                r#"(?i)([?&](?:key|token|api[_-]?key|apikey|access_token|password|passwd|secret)=)([^&\s"'#]+)"#,
            ),
            keystore_field: compile(
                r#"(?i)("(?:ciphertext|salt|nonce|key|data|value)"\s*:\s*")([A-Za-z0-9+/=]{16,})(")"#,
            ),
            blob: compile(r"([A-Za-z0-9+/]{40,}={0,2})"),
        }
    }

    fn any_missing(&self) -> bool {
        self.sk.is_none()
            || self.bearer.is_none()
            || self.header.is_none()
            || self.data_url.is_none()
            || self.jwt.is_none()
            || self.url_query.is_none()
            || self.keystore_field.is_none()
            || self.blob.is_none()
    }
}

/// 内置规则表。**全部模式由单测保证能编译**；若真有编译不了的（不可达），
/// [`sanitize`] 会降级为**最大脱敏**而不是放行 —— 宁可日志难读，也不能泄密。
static RULES: Lazy<Rules> = Lazy::new(Rules::compile);

/// 降级模式：把所有 ≥ 24 字符的 token 串整体遮掉（中文与普通英文不受影响）。
const FAIL_CLOSED_PATTERN: &str = r"[A-Za-z0-9+/=_.\-]{24,}";

fn sanitize_with(rules: &Rules, input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    if rules.any_missing() {
        return match compile(FAIL_CLOSED_PATTERN) {
            Some(re) => re.replace_all(input, "***").into_owned(),
            None => String::from("***"),
        };
    }
    let mut out = input.to_string();
    // 顺序有讲究：先抹"独立形态"（令牌本身），最后才是兜底的"认证头键值对"。
    // 反过来做的话，`Authorization: Bearer sk-x` 会先把值截成 `***`，
    // 后面的 Bearer 规则就再也看不到那个令牌了（测试 `bearer_token_is_redacted` 抓的就是这个 bug）。
    if let Some(re) = rules.sk.as_ref() {
        out = re.replace_all(&out, "***").into_owned();
    }
    if let Some(re) = rules.bearer.as_ref() {
        out = re.replace_all(&out, "${1} ***").into_owned();
    }
    if let Some(re) = rules.jwt.as_ref() {
        out = re.replace_all(&out, "***").into_owned();
    }
    if let Some(re) = rules.url_query.as_ref() {
        out = re.replace_all(&out, "${1}***").into_owned();
    }
    // 图片 base64：只留长度与哈希（体积与隐私同时解决）
    if let Some(re) = rules.data_url.as_ref() {
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                let mime = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let payload = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                format!("data:image/{};base64,{}", mime, image_placeholder(payload))
            })
            .into_owned();
    }
    // 便携密钥库密文
    if let Some(re) = rules.keystore_field.as_ref() {
        out = re.replace_all(&out, "${1}***${3}").into_owned();
    }
    if out.contains(KEYSTORE_FORMAT_TAG) {
        if let Some(re) = rules.blob.as_ref() {
            out = re.replace_all(&out, "***").into_owned();
        }
    }
    // 兜底：认证头 / query 的键值对。方案词（Bearer/Basic）本身不是秘密，留着好读。
    if let Some(re) = rules.header.as_ref() {
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let quote = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let value = caps.get(3).map(|m| m.as_str()).unwrap_or("");
                if is_auth_scheme_word(value) {
                    format!("{key}{quote}{value}")
                } else {
                    format!("{key}{quote}***")
                }
            })
            .into_owned();
    }
    out
}

/// `Bearer` / `Basic` / `***` 这类"值"不含密钥材料，抹掉反而让人看不懂这里原本是什么。
fn is_auth_scheme_word(value: &str) -> bool {
    let v = value.trim_matches(|c: char| c == '"' || c == '\'').to_ascii_lowercase();
    matches!(v.as_str(), "bearer" | "basic" | "token" | "***" | "*" | "redacted")
}

fn find_secret_with(rules: &Rules, input: &str) -> Option<SecretHit> {
    if rules.any_missing() {
        // 降级模式：报"有敏感内容"，但说不清是哪一类
        let re = compile(FAIL_CLOSED_PATTERN)?;
        return re.find(input).map(|m| SecretHit {
            kind: SecretKind::SkToken,
            start: m.start(),
            len: m.len(),
        });
    }
    let mut hits: Vec<SecretHit> = Vec::new();
    let mut push = |re: Option<&Regex>, kind: SecretKind| {
        if let Some(re) = re {
            if let Some(m) = re.find(input) {
                hits.push(SecretHit { kind, start: m.start(), len: m.len() });
            }
        }
    };
    push(rules.sk.as_ref(), SecretKind::SkToken);
    push(rules.bearer.as_ref(), SecretKind::Bearer);
    push(rules.header.as_ref(), SecretKind::AuthHeader);
    push(rules.data_url.as_ref(), SecretKind::DataUrlImage);
    push(rules.jwt.as_ref(), SecretKind::Jwt);
    push(rules.url_query.as_ref(), SecretKind::UrlQuery);
    push(rules.keystore_field.as_ref(), SecretKind::PortableKeystore);
    if input.contains(KEYSTORE_FORMAT_TAG) {
        push(rules.blob.as_ref(), SecretKind::PortableKeystore);
    }
    hits.into_iter().min_by_key(|h| h.start)
}

/// `<len=N sha256=xxxxxxxx>`：N 是 **base64 解码后的字节数**（与 的 `bytes` 同口径）。
fn image_placeholder(base64_payload: &str) -> String {
    let compact: String = base64_payload.chars().filter(|c| !c.is_whitespace()).collect();
    let padding = compact.chars().rev().take_while(|c| *c == '=').count();
    let decoded_len = (compact.len() / 4).saturating_mul(3).saturating_sub(padding);
    let mut hasher = Sha256::new();
    hasher.update(compact.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(8);
    for b in digest.iter().take(4) {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("<len={decoded_len} sha256={hex}>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 假密钥统一带 `ta-allow-fake-secret` 标记，避免被仓库的密钥扫描钩子拦下。
    const FAKE_OPENAI: &str = "sk-ta-allow-fake-secret-9f3a2b1c4d5e";
    const FAKE_ANTHROPIC: &str = "sk-ant-ta-allow-fake-secret-7c1d2e3f4a5b";
    const FAKE_JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0YS1hbGxvdy1mYWtlIn0.abcdefghijklmnop";

    #[test]
    fn all_builtin_patterns_compile() {
        let r = Rules::compile();
        assert!(!r.any_missing(), "内置正则必须全部可编译；失败会降级为最大脱敏（日志变不可读）");
    }

    // ------------------------------------------------ 正例：六类形态都被抹掉

    #[test]
    fn sk_token_is_redacted() {
        let line = format!("调用失败 key={FAKE_OPENAI} model=gpt-4o");
        let out = sanitize(&line);
        assert!(!out.contains(FAKE_OPENAI), "密钥必须被抹掉：{out}");
        assert!(out.contains("model=gpt-4o"), "非敏感字段要保留：{out}");
        assert_eq!(find_secret(&line).map(|h| h.kind), Some(SecretKind::SkToken));
    }

    #[test]
    fn anthropic_style_sk_token_is_redacted() {
        let out = sanitize(&format!("x={FAKE_ANTHROPIC}"));
        assert!(!out.contains("7c1d2e3f4a5b"));
        assert!(out.contains("***"));
    }

    #[test]
    fn bearer_token_is_redacted() {
        let out = sanitize("Authorization: Bearer ta-allow-fake-secret-abcdefghijkl");
        assert!(!out.contains("ta-allow-fake-secret-abcdefghijkl"), "{out}");
        assert!(
            out.to_ascii_lowercase().contains("bearer"),
            "保留 Bearer 让人知道这里本来是个令牌"
        );
    }

    #[test]
    fn auth_header_values_are_redacted_in_yaml_json_and_logfmt() {
        for line in [
            "x-api-key: ta-allow-fake-secret-value1",
            "api_key=ta-allow-fake-secret-value2",
            r#"{"api_key": "ta-allow-fake-secret-value3"}"#,
            "cookie: session=ta-allow-fake-secret-value4",
        ] {
            let out = sanitize(line);
            assert!(!out.contains("ta-allow-fake-secret-value"), "未脱敏：{line} -> {out}");
            assert!(out.contains("***"), "{out}");
        }
        // 键名保留（排查时要能看出"这里有个 key"）
        assert!(sanitize("x-api-key: abcdefghij").contains("x-api-key"));
    }

    #[test]
    fn data_url_image_keeps_only_len_and_hash() {
        let payload = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg";
        let line =
            format!("content: [{{type:image_url, url:\"data:image/png;base64,{payload}\"}}]");
        let out = sanitize(&line);
        assert!(!out.contains(payload), "base64 本体必须消失（既隐私又撑爆日志）");
        assert!(
            out.contains("data:image/png;base64,<len=") && out.contains("sha256="),
            "要留下长度与哈希前 8 位：{out}"
        );
        assert_eq!(find_secret(&line).map(|h| h.kind), Some(SecretKind::DataUrlImage));
    }

    #[test]
    fn jwt_is_redacted() {
        let out = sanitize(&format!("token={FAKE_JWT}"));
        assert!(!out.contains("eyJzdWIiOiJ0YS1hbGxvdy1mYWtlIn0"));
        assert_eq!(find_secret(&format!("t {FAKE_JWT}")).map(|h| h.kind), Some(SecretKind::Jwt));
    }

    #[test]
    fn url_query_secrets_are_redacted() {
        let url = "https://relay.example.com/v1/models?key=ta-allow-fake-secret-urlvalue&limit=10";
        let out = sanitize(url);
        assert!(!out.contains("ta-allow-fake-secret-urlvalue"), "{out}");
        assert!(out.contains("limit=10"), "非敏感参数要保留：{out}");
        assert!(out.contains("?key=***"), "{out}");

        for q in ["&token=x1y2z3w4v5", "&api_key=abcdefghijkl", "&password=hunter2hunter"] {
            let out = sanitize(&format!("https://a.b/c{q}"));
            assert!(out.contains("***"), "{q} -> {out}");
        }
    }

    #[test]
    fn portable_keystore_ciphertext_is_redacted() {
        let blob = "A".repeat(64);
        let json = format!(
            r#"{{"format":"{KEYSTORE_FORMAT_TAG}","version":2,"ciphertext":"{blob}","salt":"{blob}","nonce":"{blob}"}}"#
        );
        let out = sanitize(&json);
        assert!(!out.contains(&blob), "便携密钥库密文必须被抹掉");
        assert!(out.contains(KEYSTORE_FORMAT_TAG), "格式标记本身不是密钥，保留有助于排查");
        assert_eq!(find_secret(&json).map(|h| h.kind), Some(SecretKind::PortableKeystore));
    }

    // ------------------------------------------------ 反例：正常内容不许被误伤

    #[test]
    fn bearer_word_at_end_of_line_does_not_eat_the_next_line() {
        // 真事故：`\s+` 会跨行，把下一行的 `credentials` 当成令牌吃掉
        let text = "auth:\n  scheme: bearer\ncredentials:\n  - id: c-1\n";
        let out = sanitize(text);
        assert!(out.contains("credentials:"), "Bearer 规则不许跨行吞掉下一行：{out}");
        assert!(out.contains("id: c-1"), "{out}");
    }

    #[test]
    fn ordinary_chinese_text_passes_through_untouched() {
        let text = "订阅下载失败（不可重试），保留上版：机场返回 502，稍后会自动重试。";
        assert_eq!(sanitize(text), text);
        assert_eq!(find_secret(text), None);
    }

    #[test]
    fn ordinary_code_passes_through_untouched() {
        let code = "let total = price * count + shipping; // 计算总价\nfn build_headers() -> Vec<String> { vec![] }";
        assert_eq!(sanitize(code), code, "普通代码不能被当成密钥抹掉");
    }

    #[test]
    fn url_without_secrets_passes_through_untouched() {
        for url in [
            "https://api.openai.com/v1/chat/completions",
            "https://api.openai.com/v1/models?limit=10&after=model_abc",
            "http://127.0.0.1:39103/lane/health",
        ] {
            assert_eq!(sanitize(url), url, "不含密钥的 URL 必须原样通过：{url}");
            assert_eq!(find_secret(url), None);
        }
    }

    #[test]
    fn word_containing_sk_dash_is_not_a_key() {
        // `task-…` 里的 `sk-` 前面是字母，不是词边界
        let text = "task-1234567890abcdef 已完成";
        assert_eq!(sanitize(text), text);
    }

    #[test]
    fn short_placeholders_and_short_base64_are_left_alone() {
        assert_eq!(sanitize("key=sk-abc"), "key=sk-abc", "过短的 sk- 是占位符，不是密钥");
        assert_eq!(
            sanitize("data:image/png;base64,iVBORw0KGgo="),
            "data:image/png;base64,iVBORw0KGgo="
        );
    }

    #[test]
    fn plain_base64_blob_is_kept_when_it_is_not_a_keystore() {
        // 没有便携密钥库标记时，不把普通 base64 当密文（否则会误伤 ID/哈希）
        let line = "etag=YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=";
        assert_eq!(sanitize(line), line);
    }

    #[test]
    fn sanitize_is_idempotent_and_preserves_surrounding_text() {
        let line = format!("前缀 request_id=req_01H 中缀 key={FAKE_OPENAI} 后缀");
        let once = sanitize(&line);
        assert_eq!(sanitize(&once), once, "脱敏必须幂等（日志可能被二次处理）");
        assert!(once.starts_with("前缀 request_id=req_01H 中缀"));
        assert!(once.ends_with("后缀"));
    }

    #[test]
    fn realistic_full_log_line_end_to_end() {
        let line = format!(
            "2026-02-11T12:04:22.123Z ERROR ta_gateway: 通道失败 request_id=req_01HQZX channel_id=ch-relay-a \
             attempt=1 code=TA-2005 url=https://relay.example.com/v1/chat/completions?key={FAKE_OPENAI} \
             authorization=\"Bearer {FAKE_OPENAI}\" error=全部通道失败"
        );
        let out = sanitize(&line);
        assert!(!out.contains(FAKE_OPENAI), "整行里任何位置的密钥都要被抹掉：{out}");
        // 排查需要的东西一个都不能少
        for keep in [
            "request_id=req_01HQZX",
            "channel_id=ch-relay-a",
            "attempt=1",
            "code=TA-2005",
            "error=全部通道失败",
        ] {
            assert!(out.contains(keep), "把不该抹的抹了：{keep} 不在 {out}");
        }
    }

    // ------------------------------------------------ 配置脱敏

    #[test]
    fn config_yaml_keeps_structure_and_masks_values() {
        let yaml = "providers:\n  - id: p-openai\n    auth:\n      secret: ta-allow-fake-secret-plaintext\n      scheme: bearer\ncredentials:\n  - id: c-1\n    secret: \"${secret:openai-main}\"\n    quota:\n      limit: 1000000\nlanes:\n  - id: lane-jp\n";
        let out = sanitize_config_yaml(yaml);
        assert!(!out.contains("ta-allow-fake-secret-plaintext"), "明文密钥必须被抹掉：{out}");
        assert!(out.contains("secret: ***"), "{out}");
        // 结构必须完整保留
        for keep in [
            "providers:",
            "id: p-openai",
            "credentials:",
            "id: c-1",
            "quota:",
            "limit: 1000000",
            "lanes:",
            "id: lane-jp",
        ] {
            assert!(out.contains(keep), "结构被破坏了：{keep} 不在 {out}");
        }
        // `${secret:name}` 是引用不是材料 → 原样保留
        assert!(out.contains("${secret:openai-main}"), "{out}");
    }

    #[test]
    fn config_yaml_without_secrets_is_unchanged() {
        let yaml = "gateway:\n  port: 8787\n  model: gpt-4o\n";
        assert_eq!(sanitize_config_yaml(yaml), yaml);
    }

    // ------------------------------------------------ 降级（fail-closed）

    #[test]
    fn broken_pattern_set_fails_closed() {
        // 规则表编译不出来时（不可达），宁可把整段 token 遮掉也不放行
        let broken = Rules {
            sk: None,
            bearer: None,
            header: None,
            data_url: None,
            jwt: None,
            url_query: None,
            keystore_field: None,
            blob: None,
        };
        let out = sanitize_with(&broken, "key=ta-allow-fake-secret-abcdefghijklmnop 中文保留");
        assert!(!out.contains("ta-allow-fake-secret-abcdefghijklmnop"));
        assert!(out.contains("中文保留"));
        assert!(find_secret_with(&broken, "x=ta-allow-fake-secret-abcdefghijklmnop").is_some());
    }

    #[test]
    fn empty_input_is_handled() {
        assert_eq!(sanitize(""), "");
        assert_eq!(find_secret(""), None);
    }

    #[test]
    fn secret_kinds_have_stable_names() {
        // 这些名字会出现在诊断包的"错误摘要"里，改名等于改契约
        assert_eq!(SecretKind::SkToken.as_str(), "sk_token");
        assert_eq!(SecretKind::PortableKeystore.as_str(), "portable_keystore");
    }
}

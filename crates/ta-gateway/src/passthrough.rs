//! **全透传（本项目最重要的一个模块）。** 工作包 G1 负责实现。
//!
//! 只允许 4 处修改：`host` / `content-length` / 鉴权头 / hop-by-hop 剥离。
//! **禁止**用 `#[derive(Deserialize)]` 定义请求体结构——那会静默吞掉未知字段，
//! 也就是"把视觉干成瞎子"的根因。
//!
//! ## 实现纪律（docs/03 §7.2 / docs/10 §1.3）
//!
//! - 头：`HeaderMap` **整体复制**，再定点增删改；**绝不白名单式重建**
//!   （白名单就是 `x-stainless-*` / `anthropic-beta` 被吞的地方）。
//! - 体：`serde_json::Value` 定点 patch；**解析失败原样返回原字节**。
//!   请求体只能改 `model`（映射为 `Channel::upstream_model`）+ 用户显式配置的
//!   `Channel::transform.body_patch`；其余字段（含未知字段、未知 part）一个都不动。
//! - `diff_headers` 是**自检**不是策略：豁免 4 处契约例外后，任何差异都说明网关自己漏了字段。

use std::collections::{BTreeMap, BTreeSet};

use http::{header, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

use ta_core::domain::{
    AuthConfig, AuthScheme, Channel, ConfigDoc, HeaderConflict, HeaderMode, Provider,
};
use ta_core::error::{Result, TaError};

/// 客户端头 vs 上游收到的头的差异。**非空 = 网关有 bug**（自检，不是策略）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HeaderDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<(String, String, String)>,
}

impl HeaderDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

pub trait PassthroughPolicy: Send + Sync {
    /// 整体转发 + 定点修改
    fn build_headers(
        &self,
        cfg: &ConfigDoc,
        client: &HeaderMap,
        channel: &Channel,
        secret: &str,
        upstream_url: &str,
    ) -> HeaderMap;

    /// `serde_json::Value` 定点 patch；解析失败则返回原字节（不阻断）
    fn patch_body(&self, raw: &[u8], channel: &Channel) -> Result<Vec<u8>>;

    fn diff_headers(&self, client: &HeaderMap, upstream: &HeaderMap) -> HeaderDiff;

    fn header_mode(&self, cfg: &ConfigDoc, channel: &Channel) -> HeaderMode;
}

// ================================================================ 常量表

/// hop-by-hop 头：按 RFC 9110 §7.6.1 必须由代理剥离（`proxy-*` 前缀另算）。
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
];

/// 鉴权头：这三个是"换成通道凭据的 key"的例外（docs/10 §1.3）。
pub const AUTH_HEADERS: [&str; 3] = ["authorization", "x-api-key", "api-key"];

/// `diff_headers` 的豁免名单：契约允许变化的头。
///
/// 豁免 hop-by-hop（协议要求剥离）+ `host` / `content-length` / 三个鉴权头（必须重写）。
/// **其余任何差异都必须报出来** —— 那是网关吞了字段。
const SANCTIONED_CHANGES: &[&str] =
    &["host", "content-length", "authorization", "x-api-key", "api-key"];

/// `HeaderMode::Allowlist` 用的白名单。
///
/// ⚠️ 白名单**一定会吞字段**，所以默认永远是 `Pass`；这个模式只是给
/// "想确认自己没被强制全透传"的用户一个开关（docs/03 §7 / `HeaderMode` 文档）。
const ALLOWLIST: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "content-type",
    "content-length",
    "user-agent",
    "authorization",
    "x-api-key",
    "api-key",
];

/// 头名是否属于 hop-by-hop（含 `proxy-*` 前缀）。
pub fn is_hop_by_hop(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("proxy-") || HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

fn is_sanctioned(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_hop_by_hop(&lower) || SANCTIONED_CHANGES.contains(&lower.as_str())
}

// ================================================================ 实现

/// 严格全透传：`HeaderMode::Pass` 下的唯一正确实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct FullPassthrough;

impl FullPassthrough {
    pub fn new() -> Self {
        Self
    }

    /// 便于装配 `Arc<dyn PassthroughPolicy>`（T2/G2 用）。
    pub fn shared() -> std::sync::Arc<dyn PassthroughPolicy> {
        std::sync::Arc::new(Self)
    }

    /// 与 [`PassthroughPolicy::build_headers`] 同义，但**知道实际 body 长度**。
    ///
    /// trait 签名里拿不到 body，所以 `build_headers` 只能**删掉** `content-length`
    /// （让 HTTP 客户端按真实 body 重算，绝不把客户端的旧值带上去）；
    /// 调用方在 patch 完 body 之后用这个入口，才能真正"按实际 body 重算"。
    pub fn build_headers_with_body(
        &self,
        cfg: &ConfigDoc,
        client: &HeaderMap,
        channel: &Channel,
        secret: &str,
        upstream_url: &str,
        body_len: Option<usize>,
    ) -> HeaderMap {
        let provider = cfg.provider(channel.provider.as_str());
        let mut out = match self.header_mode(cfg, channel) {
            // ✅ 整体复制
            HeaderMode::Pass => client.clone(),
            // ⚠️ 会吞字段，但这是用户显式选的
            HeaderMode::Allowlist => allowlist_from(client),
        };

        // 例外 4：hop-by-hop 剥离（含 `Connection` 里点名的那些）
        strip_hop_by_hop(&mut out);

        // 用户配置的头增删：
        // remove → provider.headers → headers_add → headers_override → headers_remove。
        // **"删"放在最后**：显式删除是更强的意图，必须能删掉 provider/通道刚加上的头
        // （否则"通道层面屏蔽某个 provider 头"根本做不到）。
        let conflict = provider.map(|p| p.on_header_conflict).unwrap_or_default();
        if let Some(provider) = provider {
            for (name, value) in &provider.headers {
                put_config_header(&mut out, name, value, conflict);
            }
        }
        for (name, value) in &channel.transform.headers_add {
            put_config_header(&mut out, name, value, conflict);
        }
        for (name, value) in &channel.transform.headers_override {
            put_config_header(&mut out, name, value, HeaderConflict::Override);
        }
        for name in &channel.transform.headers_remove {
            out.remove(name.as_str());
        }

        // 例外 3：鉴权头换成通道凭据的 key
        rewrite_auth(&mut out, provider, secret);

        // 例外 1：host 重写为上游 host
        if let Some(authority) = authority_of(upstream_url) {
            if let Ok(value) = HeaderValue::from_str(&authority) {
                out.insert(header::HOST, value);
            }
        }

        // 例外 2：content-length 按实际 body 重算（拿不到 body 就先删掉）
        out.remove(header::CONTENT_LENGTH);
        if let Some(len) = body_len {
            set_content_length(&mut out, len);
        }

        out
    }
}

impl PassthroughPolicy for FullPassthrough {
    fn build_headers(
        &self,
        cfg: &ConfigDoc,
        client: &HeaderMap,
        channel: &Channel,
        secret: &str,
        upstream_url: &str,
    ) -> HeaderMap {
        self.build_headers_with_body(cfg, client, channel, secret, upstream_url, None)
    }

    fn patch_body(&self, raw: &[u8], channel: &Channel) -> Result<Vec<u8>> {
        // 非 JSON（multipart 上传、SSE、畸形体…）→ 原样透传。
        // 这里**不能**报错：有些客户端会发非 JSON 的畸形体，透传比拒绝更尊重用户。
        let Ok(mut value) = serde_json::from_slice::<Value>(raw) else {
            return Ok(raw.to_vec());
        };

        let mut changed = false;
        if value.is_object() {
            // 只改 model；**不改**其它任何字段，也**不注入**原本没有的字段
            let upstream_model = channel.upstream_model.clone();
            if let Some(obj) = value.as_object_mut() {
                let needs = match obj.get("model") {
                    Some(Value::String(current)) => current != &upstream_model,
                    Some(_) => true,
                    None => false,
                };
                if needs {
                    obj.insert("model".to_string(), Value::String(upstream_model));
                    changed = true;
                }
            }
            for (key, patch) in &channel.transform.body_patch {
                if set_path(&mut value, key, patch.clone()) {
                    changed = true;
                }
            }
        }

        // 没有任何改动 → 返回**原始字节**：连数字格式与空白都不动（保真最大化）
        if !changed {
            return Ok(raw.to_vec());
        }
        serde_json::to_vec(&value).map_err(|e| TaError::Internal(format!("请求体序列化失败：{e}")))
    }

    fn diff_headers(&self, client: &HeaderMap, upstream: &HeaderMap) -> HeaderDiff {
        let mut diff = HeaderDiff::default();
        // `Connection: x-foo` 点名的头也是 hop-by-hop，剥离它们是协议要求，不算 bug
        let listed = connection_listed(client);

        for name in client.keys() {
            if is_sanctioned(name.as_str()) || listed.contains(name.as_str()) {
                continue;
            }
            let client_values = values_of(client, name);
            if !upstream.contains_key(name) {
                diff.removed.push(name.as_str().to_string());
            } else {
                let upstream_values = values_of(upstream, name);
                if client_values != upstream_values {
                    diff.changed.push((
                        name.as_str().to_string(),
                        client_values.join(", "),
                        upstream_values.join(", "),
                    ));
                }
            }
        }
        for name in upstream.keys() {
            if is_sanctioned(name.as_str()) || listed.contains(name.as_str()) {
                continue;
            }
            if !client.contains_key(name) {
                diff.added.push(name.as_str().to_string());
            }
        }

        // 稳定输出：便于快照与人工比对
        diff.added.sort();
        diff.removed.sort();
        diff.changed.sort();
        diff
    }

    fn header_mode(&self, cfg: &ConfigDoc, channel: &Channel) -> HeaderMode {
        // provider 级覆盖 > gateway 全局；默认恒为 Pass
        cfg.provider(channel.provider.as_str())
            .and_then(|p| p.passthrough.header_mode)
            .unwrap_or(cfg.gateway.passthrough.header_mode)
    }
}

// ================================================================ 头部细节

fn allowlist_from(client: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in ALLOWLIST {
        for value in client.get_all(*name).iter() {
            if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                out.append(name, value.clone());
            }
        }
    }
    out
}

/// `Connection: keep-alive, x-foo` → `{x-foo}`（`close` / `keep-alive` 不是头名，跳过）。
fn connection_listed(client: &HeaderMap) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for value in client.get_all(header::CONNECTION).iter() {
        let Ok(text) = value.to_str() else { continue };
        for token in text.split(',') {
            let token = token.trim().to_ascii_lowercase();
            if token.is_empty() || token == "close" || token == "keep-alive" {
                continue;
            }
            out.insert(token);
        }
    }
    out
}

fn strip_hop_by_hop(out: &mut HeaderMap) {
    let listed = connection_listed(out);
    let victims: Vec<HeaderName> = out
        .keys()
        .filter(|name| is_hop_by_hop(name.as_str()) || listed.contains(name.as_str()))
        .cloned()
        .collect();
    for name in victims {
        out.remove(&name);
    }
}

/// 写一个配置来的头。非法头名/值（RFC 不允许的字符）只能跳过——签名不返回 `Result`，
/// 这种配置错误应当由配置校验拦住（见交付清单 E 块）。
fn put_config_header(out: &mut HeaderMap, name: &str, value: &str, conflict: HeaderConflict) {
    let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else { return };
    if conflict == HeaderConflict::KeepClient && out.contains_key(&name) {
        return;
    }
    let Ok(value) = HeaderValue::from_str(value) else { return };
    out.insert(name, value);
}

/// 鉴权头替换：**provider.auth.scheme 决定用哪个头**（`None` / `Query` 表示不写头）。
///
/// 先把三个已知鉴权头全部删掉，再按 scheme 写回通道凭据 —— 客户端的旧 key
/// 绝不允许漏到上游。
fn rewrite_auth(out: &mut HeaderMap, provider: Option<&Provider>, secret: &str) {
    for name in AUTH_HEADERS {
        out.remove(name);
    }
    let auth = provider.map(|p| p.auth.clone()).unwrap_or_default();
    let (name, value) = match auth.scheme {
        AuthScheme::Bearer => {
            ("authorization", format!("{}{}", auth.prefix.as_deref().unwrap_or("Bearer "), secret))
        }
        AuthScheme::Header => {
            let name = auth.header.as_deref().unwrap_or("x-api-key");
            (name, format!("{}{}", auth.prefix.as_deref().unwrap_or(""), secret))
        }
        // 上游不要鉴权 / key 走 URL query（见 `apply_query_auth`）
        AuthScheme::None | AuthScheme::Query => return,
    };
    put_config_header(out, name, &value, HeaderConflict::Override);
}

fn values_of(map: &HeaderMap, name: &HeaderName) -> Vec<String> {
    map.get_all(name)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect()
}

/// 供 trace / 快照使用的稳定头视图（排序 + 多值展开）。
pub fn canonical_headers(map: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in map.keys() {
        out.insert(name.as_str().to_string(), values_of(map, name));
    }
    out
}

/// `content-length` 按实际 body 重算（例外 2）。
pub fn set_content_length(headers: &mut HeaderMap, len: usize) {
    if let Ok(value) = HeaderValue::from_str(&len.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
}

/// `upstream_url` 的 authority（host[:port]），用于重写 `host`（例外 1）。
pub fn authority_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        None
    } else {
        Some(authority.to_string())
    }
}

// ================================================================ body 定点 patch

/// 写 `a.b.c` 这样的点分路径。
///
/// 返回"是否真的改了"。若中间层已存在**非对象**的值（客户端自己的 `stream_options`
/// 是个字符串之类），**跳过而不是覆盖** —— 用户配置的 patch 不该毁掉客户端数据。
fn set_path(root: &mut Value, key: &str, value: Value) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return false;
    }
    set_path_at(root, &parts, value)
}

/// `set_path` 的递归实现（用递归而不是循环移动 `&mut`，避免同一处可变借用活两次）。
fn set_path_at(cursor: &mut Value, parts: &[&str], value: Value) -> bool {
    let Some(part) = parts.first() else { return false };
    if parts.len() == 1 {
        let Some(obj) = cursor.as_object_mut() else { return false };
        let previous = obj.insert((*part).to_string(), value);
        return match previous {
            Some(old) => Some(&old) != obj.get(*part),
            None => true,
        };
    }
    let Some(obj) = cursor.as_object_mut() else { return false };
    let next = match obj.get_mut(*part) {
        Some(slot) if slot.is_object() => slot,
        Some(slot) if slot.is_null() => {
            *slot = Value::Object(Map::new());
            slot
        }
        // 中间层是别的东西（字符串/数字/数组）→ 跳过这个 patch，绝不覆盖客户端数据
        Some(_) => return false,
        None => {
            obj.insert((*part).to_string(), Value::Object(Map::new()));
            match obj.get_mut(*part) {
                Some(slot) => slot,
                None => return false,
            }
        }
    };
    set_path_at(next, &parts[1..], value)
}

// ================================================================ URL 工具（egress 侧）

/// 把 `base_url` 与客户端 `path` 拼成上游 URL，并处理**版本段重复**。
///
/// 例：`https://api.anthropic.com/v1` + `/v1/messages` → `.../v1/messages`
/// （取 base 末尾与 path 开头的最长公共段并去掉 path 的那一段；否则会拼成 `/v1/v1/...`）。
pub fn join_upstream_url(base_url: &str, path: &str, query: Option<&str>) -> String {
    let base = base_url.trim_end_matches('/');
    let (base_origin, base_path) = match base.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('/') {
            Some((authority, tail)) => (format!("{scheme}://{authority}"), format!("/{tail}")),
            None => (base.to_string(), String::new()),
        },
        None => match base.split_once('/') {
            Some((authority, tail)) => (authority.to_string(), format!("/{tail}")),
            None => (base.to_string(), String::new()),
        },
    };

    let base_segments: Vec<&str> = base_path.split('/').filter(|s| !s.is_empty()).collect();
    let path_segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // 最长公共段：base 的末尾 k 段 == path 的开头 k 段
    let max_k = base_segments.len().min(path_segments.len());
    let mut overlap = 0;
    for k in 1..=max_k {
        if base_segments[base_segments.len() - k..] == path_segments[..k] {
            overlap = k;
        }
    }
    let rest: Vec<&str> = path_segments[overlap..].to_vec();

    let mut url = base_origin;
    if !base_path.is_empty() {
        url.push_str(&base_path);
    }
    for segment in rest {
        url.push('/');
        url.push_str(segment);
    }
    if let Some(query) = query {
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
    }
    url
}

/// `auth.scheme: query` 时把 key 追加到 URL（参数名取 `auth.header`，默认 `key`）。
pub fn apply_query_auth(url: &str, auth: &AuthConfig, secret: &str) -> String {
    if !matches!(auth.scheme, AuthScheme::Query) {
        return url.to_string();
    }
    let param = auth.header.as_deref().unwrap_or("key");
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{}={}", percent_encode(param), percent_encode(secret))
}

/// 查询串/URL 组件用的百分号编码（保守：只放行 unreserved）。
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 与 [`percent_encode`] 配对：解码 `%XX` 与 `+`（表单语义）。
///
/// 非法转义原样保留（不报错）—— 管理 API 的查询串宁可口径宽松。
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    // 非 UTF-8 字节用替换字符兜底，绝不 panic
    String::from_utf8_lossy(&out).into_owned()
}

// ================================================================ 测试

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ta_core::types::Protocol;

    fn channel() -> Channel {
        Channel::new(
            "ch-relay-a",
            "claude-sonnet-4-5",
            "p-relay",
            "c-relay-a",
            "claude-sonnet-4-5-20250929",
        )
    }

    fn provider() -> Provider {
        Provider::new("p-relay", "中转A", Protocol::Openai, "https://relay.example.com/v1")
    }

    fn cfg_with(provider: Provider, channel: Channel) -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        doc.providers.push(provider);
        doc.channels.push(channel);
        doc
    }

    /// DoD 的 golden 输入（`ingress` 的端到端测试复用同一份，避免两处漂移）。
    pub(crate) struct GoldenFixture {
        pub provider: Provider,
        pub channel: Channel,
        pub headers: HeaderMap,
        pub body: Value,
    }

    pub(crate) fn golden_fixture() -> GoldenFixture {
        GoldenFixture {
            provider: provider(),
            channel: channel(),
            headers: golden_client_headers(),
            body: golden_body(),
        }
    }

    const UPSTREAM_URL: &str = "https://relay.example.com/v1/messages";

    /// 客户端请求（DoD 的 golden 输入）：
    /// 图片块带 `detail`、未知 part 类型、未知顶层字段、`anthropic-beta`、
    /// 自定义 UA、`x-stainless-*`、自定义头、hop-by-hop、以及一个过期的 content-length。
    fn golden_client_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8787".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer sk-client-key".parse().unwrap());
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(header::CONTENT_LENGTH, "999999".parse().unwrap());
        headers.insert(header::USER_AGENT, "claude-cli/1.0.60 (external, cli)".parse().unwrap());
        headers.insert("x-stainless-lang", "js".parse().unwrap());
        headers.insert("x-stainless-package-version", "0.60.0".parse().unwrap());
        headers.insert("x-stainless-os", "Windows".parse().unwrap());
        headers.insert("x-stainless-runtime", "node".parse().unwrap());
        headers.insert(
            "anthropic-beta",
            "prompt-caching-2024-07-31,computer-use-2025-01-24".parse().unwrap(),
        );
        headers.insert("x-session-id", "sess-8f3a2b".parse().unwrap());
        headers.insert(header::REFERER, "https://console.example.com/".parse().unwrap());
        headers.insert(header::ORIGIN, "https://console.example.com".parse().unwrap());
        headers.insert("x-weirdly-named-custom", "keep-me".parse().unwrap());
        headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("proxy-connection", "keep-alive".parse().unwrap());
        headers
    }

    fn golden_body() -> Value {
        serde_json::json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 4096,
            "temperature": 0.7,
            "stream": true,
            "future_top_level_field": { "nested": [1, 2, {"deep": true}] },
            "metadata": { "user_id": "u-1" },
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "这张图里是什么？" },
                    { "type": "image_url", "image_url": {
                        "url": "data:image/png;base64,iVBORw0KGgo=", "detail": "high" } },
                    { "type": "brand_new_part_type", "payload": { "unknown": [1, 2, 3] } }
                ] }
            ]
        })
    }

    // ---------------------------------------------------------- DoD：golden 快照

    #[test]
    fn golden_passthrough_is_field_and_header_exact() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let client = golden_client_headers();
        let raw = serde_json::to_vec(&golden_body()).unwrap();

        let patched = policy.patch_body(&raw, &channel()).unwrap();
        let upstream = policy.build_headers_with_body(
            &cfg,
            &client,
            &channel(),
            "sk-relay-a-secret",
            UPSTREAM_URL,
            Some(patched.len()),
        );

        // ---- 4 处例外，逐个正例 + 反例
        // 例外 1：host 重写
        assert_eq!(upstream.get(header::HOST).unwrap(), "relay.example.com");
        assert_ne!(upstream.get(header::HOST).unwrap(), client.get(header::HOST).unwrap());
        // 例外 2：content-length 按实际 body 重算（客户端给的是 999999）
        assert_eq!(
            upstream.get(header::CONTENT_LENGTH).unwrap(),
            patched.len().to_string().as_str(),
            "content-length 必须等于 **patch 之后** 的真实字节数（model 变长 9 字节）"
        );
        assert_eq!(patched.len(), raw.len() + 9, "模型名从 17 字节换成 26 字节");
        assert_ne!(upstream.get(header::CONTENT_LENGTH).unwrap(), "999999");
        // 例外 3：鉴权头换成通道凭据
        assert_eq!(upstream.get(header::AUTHORIZATION).unwrap(), "Bearer sk-relay-a-secret");
        assert!(!upstream.contains_key("x-api-key"), "客户端没发就不许凭空加");
        // 例外 4：hop-by-hop 剥离
        for gone in [
            "connection",
            "keep-alive",
            "proxy-connection",
            "transfer-encoding",
            "te",
            "upgrade",
            "trailer",
        ] {
            assert!(!upstream.contains_key(gone), "{gone} 必须剥离");
        }

        // ---- 其余头**逐个头原样**（agent 标识就是靠这条不丢）
        for keep in [
            "user-agent",
            "x-stainless-lang",
            "x-stainless-package-version",
            "x-stainless-os",
            "x-stainless-runtime",
            "anthropic-beta",
            "x-session-id",
            "referer",
            "origin",
            "x-weirdly-named-custom",
            "content-type",
        ] {
            assert_eq!(
                upstream.get(keep).map(|v| v.as_bytes()),
                client.get(keep).map(|v| v.as_bytes()),
                "{keep} 必须逐字节一致"
            );
        }
        assert_eq!(upstream.len(), client.len() - 3, "只允许少掉 hop-by-hop（3 个）");

        // ---- body：只改 model，未知字段/未知 part/图片 detail 全部保留
        let patched_value: Value = serde_json::from_slice(&patched).unwrap();
        let mut expected = golden_body();
        expected["model"] = Value::String("claude-sonnet-4-5-20250929".into());
        assert_eq!(patched_value, expected, "除 model 外一个字段都不许变");
        assert_eq!(
            patched_value["messages"][0]["content"][1]["image_url"]["detail"],
            Value::String("high".into()),
            "图片的 detail 字段必须原样到达上游"
        );
        assert_eq!(
            patched_value["messages"][0]["content"][2]["type"],
            Value::String("brand_new_part_type".into()),
            "未知 part 类型必须原样到达上游"
        );
        assert_eq!(patched_value["future_top_level_field"]["nested"][2]["deep"], Value::Bool(true));

        // ---- diff_headers 是自检：正常全透传必须恒空
        let diff = policy.diff_headers(&client, &upstream);
        assert!(diff.is_empty(), "正常透传不应有任何差异：{diff:?}");

        // ---- golden 快照（insta）：把"上游收到的东西"整份钉住
        let view = serde_json::json!({
            "headers": canonical_headers(&upstream),
            "body": patched_value,
        });
        insta::assert_json_snapshot!(view, @r###"
        {
          "headers": {
            "anthropic-beta": [
              "prompt-caching-2024-07-31,computer-use-2025-01-24"
            ],
            "authorization": [
              "Bearer sk-relay-a-secret"
            ],
            "content-length": [
              "419"
            ],
            "content-type": [
              "application/json"
            ],
            "host": [
              "relay.example.com"
            ],
            "origin": [
              "https://console.example.com"
            ],
            "referer": [
              "https://console.example.com/"
            ],
            "user-agent": [
              "claude-cli/1.0.60 (external, cli)"
            ],
            "x-session-id": [
              "sess-8f3a2b"
            ],
            "x-stainless-lang": [
              "js"
            ],
            "x-stainless-os": [
              "Windows"
            ],
            "x-stainless-package-version": [
              "0.60.0"
            ],
            "x-stainless-runtime": [
              "node"
            ],
            "x-weirdly-named-custom": [
              "keep-me"
            ]
          },
          "body": {
            "model": "claude-sonnet-4-5-20250929",
            "max_tokens": 4096,
            "temperature": 0.7,
            "stream": true,
            "future_top_level_field": {
              "nested": [
                1,
                2,
                {
                  "deep": true
                }
              ]
            },
            "metadata": {
              "user_id": "u-1"
            },
            "messages": [
              {
                "role": "user",
                "content": [
                  {
                    "type": "text",
                    "text": "这张图里是什么？"
                  },
                  {
                    "type": "image_url",
                    "image_url": {
                      "url": "data:image/png;base64,iVBORw0KGgo=",
                      "detail": "high"
                    }
                  },
                  {
                    "type": "brand_new_part_type",
                    "payload": {
                      "unknown": [
                        1,
                        2,
                        3
                      ]
                    }
                  }
                ]
              }
            ]
          }
        }
        "###);
    }

    // ---------------------------------------------------------- build_headers

    #[test]
    fn build_headers_removes_stale_content_length_when_body_unknown() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let client = golden_client_headers();
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-x", UPSTREAM_URL);
        assert!(
            !upstream.contains_key(header::CONTENT_LENGTH),
            "拿不到 body 时必须删掉，绝不能把客户端的旧值带上去"
        );
    }

    #[test]
    fn build_headers_keeps_client_headers_by_default() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let mut client = HeaderMap::new();
        client.insert("some-day-invented-header", "hello".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-x", UPSTREAM_URL);
        assert_eq!(upstream.get("some-day-invented-header").unwrap(), "hello");
        // 客户端没发鉴权头时，按 scheme 补一个（多套餐复用的前提）
        assert_eq!(upstream.get(header::AUTHORIZATION).unwrap(), "Bearer sk-x");
    }

    #[test]
    fn build_headers_replaces_all_three_auth_headers() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let mut client = HeaderMap::new();
        client.insert(header::AUTHORIZATION, "Bearer sk-old".parse().unwrap());
        client.insert("x-api-key", "sk-old-2".parse().unwrap());
        client.insert("api-key", "sk-old-3".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-new", UPSTREAM_URL);
        assert_eq!(upstream.get(header::AUTHORIZATION).unwrap(), "Bearer sk-new");
        assert!(!upstream.contains_key("x-api-key"), "客户端 key 绝不能漏到上游");
        assert!(!upstream.contains_key("api-key"), "客户端 key 绝不能漏到上游");
    }

    #[test]
    fn build_headers_header_scheme_uses_configured_header_and_prefix() {
        let policy = FullPassthrough::new();
        let mut provider = provider();
        provider.auth = AuthConfig {
            scheme: AuthScheme::Header,
            header: Some("x-api-key".into()),
            prefix: None,
            extra: Map::new(),
        };
        let cfg = cfg_with(provider, channel());
        let mut client = HeaderMap::new();
        client.insert(header::AUTHORIZATION, "Bearer sk-old".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-ant-new", UPSTREAM_URL);
        assert_eq!(upstream.get("x-api-key").unwrap(), "sk-ant-new");
        assert!(!upstream.contains_key(header::AUTHORIZATION));
    }

    #[test]
    fn build_headers_none_scheme_strips_auth() {
        let policy = FullPassthrough::new();
        let mut provider = provider();
        provider.auth.scheme = AuthScheme::None;
        let cfg = cfg_with(provider, channel());
        let mut client = HeaderMap::new();
        client.insert(header::AUTHORIZATION, "Bearer sk-old".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-new", UPSTREAM_URL);
        assert!(!upstream.contains_key(header::AUTHORIZATION));
    }

    #[test]
    fn build_headers_applies_provider_headers_and_channel_transform() {
        let policy = FullPassthrough::new();
        let mut provider = provider();
        provider.headers.insert("X-Relay-Channel".into(), "default".into());
        provider.headers.insert("X-Provider-Only".into(), "p".into());
        let mut channel = channel();
        channel.transform.headers_add.insert("X-Transform-Add".into(), "t".into());
        channel.transform.headers_add.insert("X-Relay-Channel".into(), "from-transform".into());
        channel.transform.headers_override.insert("X-Force".into(), "forced".into());
        channel.transform.headers_remove.push("X-Provider-Only".into());
        let cfg = cfg_with(provider, channel.clone());

        let mut client = HeaderMap::new();
        client.insert("x-relay-channel", "client-wins".parse().unwrap());
        client.insert("x-force", "client-value".parse().unwrap());
        client.insert("x-provider-only", "client-value".parse().unwrap());

        let upstream = policy.build_headers(&cfg, &client, &channel, "sk-x", UPSTREAM_URL);
        assert_eq!(
            upstream.get("x-relay-channel").unwrap(),
            "client-wins",
            "keep_client 默认不覆盖"
        );
        assert_eq!(upstream.get("x-transform-add").unwrap(), "t");
        assert_eq!(upstream.get("x-force").unwrap(), "forced", "headers_override 必须覆盖");
        assert!(!upstream.contains_key("x-provider-only"), "headers_remove 必须生效");
    }

    #[test]
    fn build_headers_override_conflict_replaces_client_value() {
        let policy = FullPassthrough::new();
        let mut provider = provider();
        provider.on_header_conflict = HeaderConflict::Override;
        provider.headers.insert("X-Relay-Channel".into(), "provider".into());
        let cfg = cfg_with(provider, channel());
        let mut client = HeaderMap::new();
        client.insert("x-relay-channel", "client".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-x", UPSTREAM_URL);
        assert_eq!(upstream.get("x-relay-channel").unwrap(), "provider");
    }

    #[test]
    fn build_headers_allowlist_mode_drops_unknown_headers() {
        let policy = FullPassthrough::new();
        let mut cfg = cfg_with(provider(), channel());
        cfg.gateway.passthrough.header_mode = HeaderMode::Allowlist;
        let mut client = HeaderMap::new();
        client.insert("x-stainless-lang", "js".parse().unwrap());
        client.insert(header::USER_AGENT, "cli/1.0".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-x", UPSTREAM_URL);
        assert!(!upstream.contains_key("x-stainless-lang"), "白名单模式会吞字段（已知代价）");
        assert_eq!(upstream.get(header::USER_AGENT).unwrap(), "cli/1.0");
    }

    #[test]
    fn provider_header_mode_overrides_global_and_default_is_pass() {
        let policy = FullPassthrough::new();
        let channel = channel();
        // 默认：全局 Pass
        let cfg = cfg_with(provider(), channel.clone());
        assert_eq!(policy.header_mode(&cfg, &channel), HeaderMode::Pass);
        // provider 级覆盖
        let mut provider_cfg = provider();
        provider_cfg.passthrough.header_mode = Some(HeaderMode::Allowlist);
        let cfg = cfg_with(provider_cfg, channel.clone());
        assert_eq!(policy.header_mode(&cfg, &channel), HeaderMode::Allowlist);
        // 全局设为 Allowlist 时，provider 没写就跟随全局
        let mut cfg = cfg_with(provider(), channel.clone());
        cfg.gateway.passthrough.header_mode = HeaderMode::Allowlist;
        assert_eq!(policy.header_mode(&cfg, &channel), HeaderMode::Allowlist);
    }

    #[test]
    fn build_headers_strips_connection_listed_headers() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let mut client = HeaderMap::new();
        client.insert(header::CONNECTION, "x-hop-token".parse().unwrap());
        client.insert("x-hop-token", "should-be-stripped".parse().unwrap());
        client.insert("x-keep", "should-stay".parse().unwrap());
        let upstream = policy.build_headers(&cfg, &client, &channel(), "sk-x", UPSTREAM_URL);
        assert!(!upstream.contains_key("x-hop-token"), "Connection 点名的头也是 hop-by-hop");
        assert_eq!(upstream.get("x-keep").unwrap(), "should-stay");
        // 而且这不算"网关漏字段"
        assert!(policy.diff_headers(&client, &upstream).is_empty());
    }

    // ---------------------------------------------------------- patch_body

    #[test]
    fn patch_body_only_changes_model() {
        let policy = FullPassthrough::new();
        let raw = serde_json::to_vec(&golden_body()).unwrap();
        let patched = policy.patch_body(&raw, &channel()).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["model"], Value::String("claude-sonnet-4-5-20250929".into()));
        assert_eq!(value["messages"], golden_body()["messages"]);
        assert_eq!(value["future_top_level_field"], golden_body()["future_top_level_field"]);
    }

    #[test]
    fn patch_body_is_byte_exact_when_nothing_changes() {
        let policy = FullPassthrough::new();
        let mut channel = channel();
        channel.upstream_model = "claude-sonnet-4-5".into(); // 与客户端一致 → 不需要改写
                                                             // 故意用非标准空白与数字字面量，验证"没改动就一个字节都不动"
        let raw = br#"{ "model" : "claude-sonnet-4-5",  "temperature": 0.70 }"#.to_vec();
        let patched = policy.patch_body(&raw, &channel).unwrap();
        assert_eq!(patched, raw, "无需 patch 时必须返回原始字节");
    }

    #[test]
    fn patch_body_passes_non_json_through_untouched() {
        let policy = FullPassthrough::new();
        // 畸形 JSON / multipart / SSE 片段：一律原样透传，绝不因为解析不了就拒绝请求
        let mut channel = channel();
        channel
            .transform
            .body_patch
            .insert("stream_options.include_usage".into(), Value::Bool(true));
        for raw in [
            &b"not json at all"[..],
            &b"--boundary\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n"[..],
            &b"data: {\"partial\": "[..],
            &b""[..],
        ] {
            assert_eq!(policy.patch_body(raw, &channel).unwrap(), raw);
        }
    }

    #[test]
    fn patch_body_applies_dotted_body_patch_without_touching_others() {
        let policy = FullPassthrough::new();
        let mut channel = channel();
        channel.upstream_model = "claude-sonnet-4-5".into();
        channel
            .transform
            .body_patch
            .insert("stream_options.include_usage".into(), Value::Bool(true));
        channel.transform.body_patch.insert("top_level_flag".into(), Value::Bool(false));
        let raw = br#"{"model":"claude-sonnet-4-5","stream_options":{"foo":"bar"},"messages":[]}"#;
        let patched = policy.patch_body(raw, &channel).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["stream_options"]["include_usage"], Value::Bool(true));
        assert_eq!(value["stream_options"]["foo"], Value::String("bar".into()));
        assert_eq!(value["top_level_flag"], Value::Bool(false));
        assert_eq!(value["messages"], serde_json::json!([]));
    }

    #[test]
    fn patch_body_never_destroys_client_data_to_apply_a_patch() {
        let policy = FullPassthrough::new();
        let mut channel = channel();
        channel.upstream_model = "claude-sonnet-4-5".into();
        channel
            .transform
            .body_patch
            .insert("stream_options.include_usage".into(), Value::Bool(true));
        // 客户端的 stream_options 是个字符串：宁可跳过 patch，也不覆盖客户端数据
        let raw = br#"{"model":"claude-sonnet-4-5","stream_options":"client-value"}"#;
        let patched = policy.patch_body(raw, &channel).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["stream_options"], Value::String("client-value".into()));
    }

    #[test]
    fn patch_body_does_not_inject_model_when_absent() {
        let policy = FullPassthrough::new();
        // /v1/files 之类没有 model 的体：不许凭空注入
        let raw = br#"{"purpose":"fine-tune","file":"abc"}"#;
        let patched = policy.patch_body(raw, &channel()).unwrap();
        let value: Value = serde_json::from_slice(&patched).unwrap();
        assert!(value.get("model").is_none());
    }

    // ---------------------------------------------------------- diff_headers

    #[test]
    fn diff_headers_is_empty_on_faithful_passthrough() {
        let policy = FullPassthrough::new();
        let cfg = cfg_with(provider(), channel());
        let client = golden_client_headers();
        let upstream = policy.build_headers_with_body(
            &cfg,
            &client,
            &channel(),
            "sk-x",
            UPSTREAM_URL,
            Some(10),
        );
        assert!(policy.diff_headers(&client, &upstream).is_empty());
    }

    #[test]
    fn diff_headers_reports_dropped_and_injected_headers() {
        let policy = FullPassthrough::new();
        let mut client = HeaderMap::new();
        client.insert("anthropic-beta", "prompt-caching".parse().unwrap());
        let mut upstream = HeaderMap::new();
        upstream.insert("x-injected", "nope".parse().unwrap());
        let diff = policy.diff_headers(&client, &upstream);
        assert_eq!(diff.removed, vec!["anthropic-beta".to_string()]);
        assert_eq!(diff.added, vec!["x-injected".to_string()]);
        assert!(diff.changed.is_empty());
        assert!(!diff.is_empty());
    }

    #[test]
    fn diff_headers_reports_changed_value() {
        let policy = FullPassthrough::new();
        let mut client = HeaderMap::new();
        client.insert("x-stainless-lang", "js".parse().unwrap());
        let mut upstream = HeaderMap::new();
        upstream.insert("x-stainless-lang", "python".parse().unwrap());
        let diff = policy.diff_headers(&client, &upstream);
        assert_eq!(
            diff.changed,
            vec![("x-stainless-lang".to_string(), "js".to_string(), "python".to_string())]
        );
    }

    // ---------------------------------------------------------- URL 工具

    #[test]
    fn join_upstream_url_handles_version_overlap() {
        assert_eq!(
            join_upstream_url("https://api.anthropic.com/v1", "/v1/messages", None),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_upstream_url("https://relay.example.com/v1", "/v1/chat/completions", Some("a=1")),
            "https://relay.example.com/v1/chat/completions?a=1"
        );
        assert_eq!(
            join_upstream_url("https://relay.example.com", "/v1/models", None),
            "https://relay.example.com/v1/models"
        );
        assert_eq!(
            join_upstream_url("https://relay.example.com/openai/v1/", "/v1/models/gpt-4o", None),
            "https://relay.example.com/openai/v1/models/gpt-4o"
        );
        assert_eq!(
            join_upstream_url(
                "https://relay.example.com/v1beta",
                "/v1beta/models/gemini:gen",
                None
            ),
            "https://relay.example.com/v1beta/models/gemini:gen"
        );
    }

    #[test]
    fn query_auth_and_percent_roundtrip() {
        let auth = AuthConfig {
            scheme: AuthScheme::Query,
            header: Some("key".into()),
            prefix: None,
            extra: Map::new(),
        };
        let url = apply_query_auth("https://x.example/v1/messages", &auth, "sk-a b&c");
        assert_eq!(url, "https://x.example/v1/messages?key=sk-a%20b%26c");
        let appended = apply_query_auth("https://x.example/v1/messages?beta=1", &auth, "k");
        assert_eq!(appended, "https://x.example/v1/messages?beta=1&key=k");

        let bearer = AuthConfig::default();
        assert_eq!(apply_query_auth("https://x/v1", &bearer, "sk"), "https://x/v1");

        assert_eq!(percent_decode(&percent_encode("a b&c/d?e=中")), "a b&c/d?e=中");
        assert_eq!(percent_decode("a+b%2Fc"), "a b/c");
        assert_eq!(percent_decode("bad%zzescape"), "bad%zzescape");
    }

    #[test]
    fn authority_of_handles_ports_and_userinfo() {
        assert_eq!(authority_of("https://relay.example.com/v1/x").unwrap(), "relay.example.com");
        assert_eq!(authority_of("http://127.0.0.1:9999/v1").unwrap(), "127.0.0.1:9999");
        assert_eq!(
            authority_of("https://user:pw@relay.example.com/v1").unwrap(),
            "relay.example.com"
        );
        assert!(authority_of("").is_none());
    }

    #[test]
    fn canonical_headers_expands_multi_values_in_order() {
        let mut headers = HeaderMap::new();
        headers.append("x-multi", "a".parse().unwrap());
        headers.append("x-multi", "b".parse().unwrap());
        let view = canonical_headers(&headers);
        assert_eq!(view.get("x-multi").unwrap(), &vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn set_content_length_uses_byte_length() {
        let mut headers = HeaderMap::new();
        set_content_length(&mut headers, 1234);
        assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "1234");
    }
}

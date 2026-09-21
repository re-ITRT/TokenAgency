//! 节点 / 分组提取 + 订阅响应头解析（**只为预览、计数、防坏更新**）。
//!
//! 内核（mihomo）才是真正解析并连接节点的那个；这里的目标是：
//! * 从 Clash YAML 里拿到 `NodeMeta` / `GroupMeta`（用于 UI 预览与坏更新比对）；
//! * 从链接清单（base64 / 明文）里拿到 `NodeMeta`；
//! * **遇到不认识的协议只跳过并记录，绝不让整份订阅失败**（[04 §5.6]）。

use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value};

use ta_core::domain::SubscriptionFilter;

use crate::detect;
use crate::{DetectedFormat, GroupMeta, NodeMeta, SubUserinfo};

/// 链接清单里被跳过的行（协议不支持 / 字段缺失）。**只记录，不报错。**
#[derive(Debug, Clone, Default)]
pub struct ExtractResult {
    pub nodes: Vec<NodeMeta>,
    pub groups: Vec<GroupMeta>,
    pub skipped: Vec<String>,
}

impl ExtractResult {
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}

/// 按已嗅探的格式提取节点与分组。**任何格式都不会返回 `Err`**。
pub fn extract(raw: &[u8], format: DetectedFormat) -> ExtractResult {
    match format {
        DetectedFormat::ClashYaml => {
            let text = detect::to_parseable_text(raw, format);
            parse_clash_yaml(&text)
        }
        DetectedFormat::Base64Links | DetectedFormat::PlainLinks => {
            let text = detect::to_parseable_text(raw, format);
            parse_links(&text)
        }
        DetectedFormat::Html | DetectedFormat::Unknown => ExtractResult::default(),
    }
}

// ================================================================ Clash YAML

/// 解析 Clash 配置的 `proxies` / `proxy-providers[].proxies` / `proxy-groups`。
///
/// YAML 本身解析失败时退回**宽松逐行扫描**（真实机场偶尔会给出非法 YAML）：
/// 宁可少认几个节点，也不能因为一个坏字符把整份订阅判死。
pub fn parse_clash_yaml(text: &str) -> ExtractResult {
    let mut out = ExtractResult::default();
    match parse_first_document(text) {
        Some(Value::Mapping(root)) => extract_from_root(&root, &mut out),
        Some(_) => {
            out.skipped.push("YAML 顶层不是映射（mapping）".into());
            return out;
        }
        None => {
            out.skipped.push("YAML 解析失败，改用宽松扫描".into());
            return parse_clash_yaml_lenient(text, out);
        }
    }
    if out.nodes.is_empty() {
        // 有些订阅把节点放在更奇怪的层级；宽松扫描兜底。
        let lenient = parse_clash_yaml_lenient(text, ExtractResult::default());
        if lenient.nodes.len() > out.nodes.len() {
            out.skipped.push("YAML 结构里找不到节点，改用宽松扫描".into());
            out.nodes = lenient.nodes;
            if out.groups.is_empty() {
                out.groups = lenient.groups;
            }
        }
    }
    out
}

fn parse_first_document(text: &str) -> Option<Value> {
    // `Deserializer` 必须**按值**传（`&mut` 不实现 `serde::Deserializer`）；
    // 按值传还有个好处：多文档 YAML 只取第一份，不会因为尾部的 `---` 直接失败。
    Value::deserialize(serde_yaml_ng::Deserializer::from_str(text)).ok()
}

fn extract_from_root(root: &Mapping, out: &mut ExtractResult) {
    // 1) 顶层 proxies
    collect_nodes(root.get("proxies"), out);
    // 2) proxy-providers 里内联的节点（有些订阅把节点藏在 provider 里）
    if let Some(Value::Mapping(providers)) = root.get("proxy-providers") {
        for (_key, provider) in providers {
            let Value::Mapping(p) = provider else { continue };
            collect_nodes(p.get("proxies"), out);
        }
    }
    // 3) proxy-groups
    let node_names: Vec<String> = out.nodes.iter().map(|n| n.name.clone()).collect();
    if let Some(Value::Sequence(groups)) = root.get("proxy-groups") {
        for g in groups {
            let Value::Mapping(m) = g else { continue };
            let Some(name) = sget(m, "name") else { continue };
            let kind = sget(m, "type").unwrap_or_else(|| "select".into());
            let count = group_node_count(m, &node_names);
            out.groups.push(GroupMeta { name, kind, node_count: count });
        }
    }
}

fn collect_nodes(seq: Option<&Value>, out: &mut ExtractResult) {
    let Some(Value::Sequence(items)) = seq else { return };
    for item in items {
        let Value::Mapping(m) = item else {
            out.skipped.push("proxies 项不是映射".into());
            continue;
        };
        let (Some(name), Some(server)) = (sget(m, "name"), sget(m, "server")) else {
            out.skipped.push("proxies 项缺 name/server".into());
            continue;
        };
        let Some(port) = m.get("port").and_then(port_of) else {
            out.skipped.push(format!("节点 {name} 缺 port 或 port 非法"));
            continue;
        };
        if server.trim().is_empty() {
            out.skipped.push(format!("节点 {name} 的 server 为空"));
            continue;
        }
        out.nodes.push(NodeMeta {
            name,
            kind: sget(m, "type").unwrap_or_else(|| "unknown".into()).to_lowercase(),
            server,
            port,
        });
    }
}

/// 组里"真正可选中的节点数"：只数真的存在于 `proxies` 里的名字
/// （`DIRECT`/`REJECT` 与嵌套组名不算），`use:` / `include-all*` 引用的 provider 节点无法在这里
/// 精确定位时，按"自己没列过的节点数"给一个下限。
fn group_node_count(g: &Mapping, node_names: &[String]) -> usize {
    let mut count = 0usize;
    let mut listed: Vec<String> = Vec::new();
    if let Some(Value::Sequence(items)) = g.get("proxies") {
        for it in items {
            let Some(n) = sval(it) else { continue };
            if node_names.iter().any(|x| x == &n) {
                count += 1;
                listed.push(n);
            }
        }
    }
    let uses_provider = g.get("use").and_then(|v| v.as_sequence()).is_some()
        || g.get("include-all").and_then(|v| v.as_bool()) == Some(true)
        || g.get("include-all-proxies").and_then(|v| v.as_bool()) == Some(true);
    if uses_provider {
        let rest = node_names.iter().filter(|n| !listed.contains(n)).count();
        count = count.max(rest);
    }
    count
}

/// 宽松扫描：只认 `proxies:` 块下的 `- name/type/server/port`（YAML 不合法时的兜底）。
fn parse_clash_yaml_lenient(text: &str, mut out: ExtractResult) -> ExtractResult {
    static RE_KEY: OnceLock<Regex> = OnceLock::new();
    static RE_TOP: OnceLock<Regex> = OnceLock::new();
    static RE_ITEM: OnceLock<Regex> = OnceLock::new();
    let re_key = RE_KEY.get_or_init(|| {
        Regex::new(r#"^\s*(?:-\s*)?([A-Za-z0-9_-]+):\s*(?:"([^"]*)"|'([^']*)'|(.*?))\s*(?:#.*)?$"#)
            .expect("静态正则必须合法")
    });
    let re_top = RE_TOP.get_or_init(|| Regex::new(r"^[A-Za-z_-]").expect("静态正则必须合法"));
    let re_item = RE_ITEM.get_or_init(|| Regex::new(r"^\s*-\s").expect("静态正则必须合法"));

    type Pending = (Option<String>, Option<String>, Option<String>, Option<u16>);
    let mut in_proxies = false;
    let mut cur: Option<Pending> = None;
    fn flush(cur: &mut Option<Pending>, out: &mut ExtractResult) {
        if let Some((name, kind, server, port)) = cur.take() {
            match (name, server, port) {
                (Some(name), Some(server), Some(port)) if !server.is_empty() => {
                    out.nodes.push(NodeMeta {
                        name,
                        kind: kind.unwrap_or_else(|| "unknown".into()).to_lowercase(),
                        server,
                        port,
                    })
                }
                _ => out.skipped.push("宽松扫描：条目字段不全".into()),
            }
        }
    }

    for line in text.lines() {
        if re_top.is_match(line) {
            let is_proxies = line.trim_start().starts_with("proxies:");
            if in_proxies && !is_proxies {
                flush(&mut cur, &mut out);
            }
            in_proxies = is_proxies;
            continue;
        }
        if !in_proxies {
            continue;
        }
        if re_item.is_match(line) {
            flush(&mut cur, &mut out);
            cur = Some((None, None, None, None));
        }
        let Some((name, kind, server, port)) = cur.as_mut() else { continue };
        let Some(caps) = re_key.captures(line) else { continue };
        let Some(key) = caps.get(1) else { continue };
        let val = caps
            .get(2)
            .or_else(|| caps.get(3))
            .or_else(|| caps.get(4))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        match key.as_str().to_ascii_lowercase().as_str() {
            "name" => *name = Some(val),
            "type" => *kind = Some(val),
            "server" => *server = Some(val),
            "port" => *port = val.parse::<u16>().ok(),
            _ => {}
        }
    }
    flush(&mut cur, &mut out);
    out
}

// ================================================================ 链接清单

/// 解析明文/base64 解出来的链接清单。**一行一个链接，不认识就记录并跳过。**
pub fn parse_links(text: &str) -> ExtractResult {
    let mut out = ExtractResult::default();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((scheme, rest)) = split_scheme(line) else {
            out.skipped.push(format!("第 {} 行不是链接", idx + 1));
            continue;
        };
        match parse_link(&scheme, rest) {
            Ok(node) => out.nodes.push(node),
            Err(reason) => out.skipped.push(format!("第 {} 行 {scheme}：{reason}", idx + 1)),
        }
    }
    out
}

fn split_scheme(line: &str) -> Option<(String, &str)> {
    let (scheme, rest) = line.split_once("://")?;
    let scheme = scheme.trim().to_ascii_lowercase();
    if scheme.is_empty()
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
    {
        return None;
    }
    Some((scheme, rest))
}

/// 支持 `ss / ssr / vmess / vless / trojan / hysteria2 / tuic`（+ `hy2` 别名）。
pub fn parse_link(scheme: &str, rest: &str) -> std::result::Result<NodeMeta, String> {
    match scheme {
        "ss" => parse_ss(rest),
        "ssr" => parse_ssr(rest),
        "vmess" => parse_vmess(rest),
        "vless" => parse_uri_like("vless", rest),
        "trojan" => parse_uri_like("trojan", rest),
        "hysteria2" | "hy2" => parse_uri_like("hysteria2", rest),
        "tuic" => parse_uri_like("tuic", rest),
        other => Err(format!("暂不支持的协议（已跳过）: {other}")),
    }
}

/// `ss://`：SIP002（`base64(method:pass)@host:port`）、老格式（整体 base64）、带插件查询参数。
fn parse_ss(rest: &str) -> std::result::Result<NodeMeta, String> {
    let (body, name) = split_fragment(rest);
    let body = strip_query(body);
    let endpoint = match body.rsplit_once('@') {
        // SIP002：userinfo 是 base64，只有 host:port 是明文
        Some((_userinfo, endpoint)) => authority(endpoint).to_string(),
        None => {
            // 老格式：整体 base64 的 `method:password@host:port`（base64 可能含 `/`，不能按 `/` 切）
            let decoded =
                detect::decode_base64_lenient(body).ok_or_else(|| "ss 链接无法解码".to_string())?;
            let decoded =
                String::from_utf8(decoded).map_err(|_| "ss 链接解码后不是文本".to_string())?;
            let (_, endpoint) =
                decoded.rsplit_once('@').ok_or_else(|| "ss 链接缺 host:port".to_string())?;
            authority(endpoint).to_string()
        }
    };
    let (server, port) =
        parse_host_port(&endpoint).ok_or_else(|| "ss 链接 host:port 非法".to_string())?;
    Ok(node("ss", name, &server, port))
}

/// `ssr://base64(host:port:protocol:method:obfs:pass/?obfsparam=&remarks=base64)`
fn parse_ssr(rest: &str) -> std::result::Result<NodeMeta, String> {
    let (body, name) = split_fragment(rest);
    let decoded = detect::decode_base64_lenient(strip_query(body))
        .ok_or_else(|| "ssr 链接不是合法 base64".to_string())?;
    let decoded = String::from_utf8(decoded).map_err(|_| "ssr 链接解码后不是文本".to_string())?;
    let (main, query) = match decoded.split_once("/?") {
        Some((m, q)) => (m, Some(q)),
        None => (decoded.as_str(), None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() < 6 {
        return Err("ssr 链接字段数不足".into());
    }
    let port = parts[1].parse::<u16>().map_err(|_| "ssr 端口非法".to_string())?;
    let name = name.or_else(|| {
        query
            .and_then(|q| query_param(q, "remarks"))
            .and_then(|r| detect::decode_base64_lenient(&r))
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });
    Ok(node("ssr", name, parts[0], port))
}

/// `vmess://`：v2rayN 的 base64(JSON)（字段名有 `ps/add/port/id/aid/scy/net/tls/host/path` 等多套变体），
/// 也容忍 `vmess://uuid@host:port?...#name` 这种 URI 形态。
fn parse_vmess(rest: &str) -> std::result::Result<NodeMeta, String> {
    let (body, fragment_name) = split_fragment(rest);
    let body = strip_query(body);
    if body.contains('@') {
        // URI 形态
        let (_, endpoint) =
            body.rsplit_once('@').ok_or_else(|| "vmess 缺 host:port".to_string())?;
        let (server, port) = parse_host_port(authority(endpoint))
            .ok_or_else(|| "vmess host:port 非法".to_string())?;
        return Ok(node("vmess", fragment_name, &server, port));
    }
    let decoded = detect::decode_base64_lenient(body)
        .ok_or_else(|| "vmess 链接不是合法 base64".to_string())?;
    let json: serde_json::Value =
        serde_json::from_slice(&decoded).map_err(|e| format!("vmess JSON 解析失败: {e}"))?;
    let get = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| json.get(*k)).and_then(json_str)
    };
    let server = get(&["add", "address", "server", "host"])
        .ok_or_else(|| "vmess JSON 缺 add/server".to_string())?;
    let port_s = get(&["port"]).ok_or_else(|| "vmess JSON 缺 port".to_string())?;
    let port = port_s.trim().parse::<u16>().map_err(|_| "vmess JSON 端口非法".to_string())?;
    if server.trim().is_empty() {
        return Err("vmess JSON 的 add 为空".into());
    }
    let name = get(&["ps", "remarks", "remark", "name", "title"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or(fragment_name);
    Ok(node("vmess", name, &server, port))
}

/// `vless:// / trojan:// / hysteria2:// / tuic://`：标准 URI 形态
/// （`scheme://userinfo@host:port?params#name`；`hysteria2` 的 `insecure=1` 这类查询参数不影响取 host/port）。
fn parse_uri_like(kind: &str, rest: &str) -> std::result::Result<NodeMeta, String> {
    let (body, name) = split_fragment(rest);
    let body = strip_query(body);
    let endpoint = match body.rsplit_once('@') {
        Some((_userinfo, endpoint)) => endpoint,
        // 少数写法是 `host:port`（无 userinfo）
        None => body,
    };
    let (server, port) = parse_host_port(authority(endpoint))
        .ok_or_else(|| format!("{kind} 链接 host:port 非法"))?;
    Ok(node(kind, name, &server, port))
}

fn node(kind: &str, name: Option<String>, server: &str, port: u16) -> NodeMeta {
    let name = name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{kind}-{server}:{port}"));
    NodeMeta { name, kind: kind.to_string(), server: server.trim().to_string(), port }
}

/// 拆出 `#fragment`（节点名，做百分号解码）。
fn split_fragment(s: &str) -> (&str, Option<String>) {
    match s.split_once('#') {
        Some((body, frag)) => {
            let name = percent_decode(frag).trim().to_string();
            (body, (!name.is_empty()).then_some(name))
        }
        None => (s, None),
    }
}

/// 只切 `?query`（**不能按 `/` 切**：ss/vmess 的 base64 里可能含 `/`）。
fn strip_query(s: &str) -> &str {
    s.split('?').next().unwrap_or(s)
}

/// 从 `host:port/path` 形态里取 `host:port`（query 已切掉）。
fn authority(s: &str) -> &str {
    s.split('/').next().unwrap_or(s)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// `host:port` / `[v6]:port`。
pub fn parse_host_port(s: &str) -> Option<(String, u16)> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?.trim().parse::<u16>().ok()?;
        return (!host.is_empty()).then(|| (host.to_string(), port));
    }
    let (host, port) = s.rsplit_once(':')?;
    let port = port.trim().parse::<u16>().ok()?;
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// 百分号解码（订阅里的中文节点名基本都是 `%E9%A6%99%E6%B8%AF`）。
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ================================================================ 订阅级过滤

/// [04 §5.3] 第 4 条：剔除"剩余流量/到期/官网/防失联"这类假节点。
/// 返回被剔除的节点（调用方可以记日志）。正则非法时**只记警告、不过滤**。
pub fn apply_node_filter(nodes: &mut Vec<NodeMeta>, filter: &SubscriptionFilter) -> Vec<NodeMeta> {
    let include = compile_filter(filter.include.as_deref(), "include");
    let exclude = compile_filter(filter.exclude.as_deref(), "exclude");
    if include.is_none() && exclude.is_none() {
        return Vec::new();
    }
    let mut removed = Vec::new();
    let mut kept = Vec::with_capacity(nodes.len());
    for n in nodes.drain(..) {
        let keep = include.as_ref().map(|re| re.is_match(&n.name)).unwrap_or(true)
            && !exclude.as_ref().map(|re| re.is_match(&n.name)).unwrap_or(false);
        if keep {
            kept.push(n);
        } else {
            removed.push(n);
        }
    }
    *nodes = kept;
    removed
}

fn compile_filter(pattern: Option<&str>, which: &str) -> Option<Regex> {
    let p = pattern?.trim();
    if p.is_empty() {
        return None;
    }
    match Regex::new(p) {
        Ok(re) => Some(re),
        Err(e) => {
            tracing::warn!(which, pattern = p, error = %e, "订阅过滤正则非法，本次忽略该过滤条件");
            None
        }
    }
}

// ================================================================ 响应头

/// `Subscription-Userinfo: upload=0; download=123; total=456; expire=1780000000`
pub fn parse_userinfo(header: &str) -> Option<SubUserinfo> {
    let mut info = SubUserinfo::default();
    let mut seen = false;
    for part in header.split([';', ',']) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        match key.as_str() {
            "upload" => {
                info.upload = val.parse().unwrap_or(0);
                seen = true;
            }
            "download" => {
                info.download = val.parse().unwrap_or(0);
                seen = true;
            }
            "total" => {
                info.total = val.parse().unwrap_or(0);
                seen = true;
            }
            "expire" => {
                // 约定是 Unix 秒；个别服务端给毫秒，按数量级判定。
                if let Ok(n) = val.parse::<i64>() {
                    info.expire_ms =
                        Some(if n > 10_000_000_000 { n } else { n.saturating_mul(1000) });
                    seen = true;
                }
            }
            _ => {}
        }
    }
    seen.then_some(info)
}

/// `Profile-Update-Interval`：Clash 生态里这个头的值是**小时**（内核默认 6），
/// 但契约（[11 §3]）要求返回秒。经验判定：`<= 168`（一周）按小时算，否则按秒算。
pub fn parse_update_interval(header: &str) -> Option<u64> {
    let n: u64 = header.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(if n <= 168 { n * 3600 } else { n })
}

/// `Content-Disposition: attachment; filename="xxx.yaml"`（支持 RFC 5987 的 `filename*`）。
pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    let mut fallback = None;
    for part in header.split(';') {
        let part = part.trim();
        if part.to_ascii_lowercase().starts_with("filename*=") {
            if let Some((_, raw)) = part.split_once('=') {
                // `UTF-8''%E9%85%8D%E7%BD%AE.yaml`
                let val = raw.trim().trim_matches('"');
                let val = val.rsplit("''").next().unwrap_or(val);
                let decoded = percent_decode(val);
                if !decoded.trim().is_empty() {
                    return Some(decoded.trim().to_string());
                }
            }
        } else if part.to_ascii_lowercase().starts_with("filename=") {
            if let Some((_, raw)) = part.split_once('=') {
                let val = raw.trim().trim_matches('"').trim();
                if !val.is_empty() {
                    fallback = Some(val.to_string());
                }
            }
        }
    }
    fallback
}

// ================================================================ YAML / JSON 取值小工具

fn sget(m: &Mapping, key: &str) -> Option<String> {
    m.get(key).and_then(sval)
}

fn sval(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn port_of(v: &Value) -> Option<u16> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|x| u16::try_from(x).ok()),
        Value::String(s) => s.trim().parse::<u16>().ok(),
        _ => None,
    }
}

fn json_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 相对 `src/parse.rs`：`../tests/fixtures/` → `crates/ta-subscription/tests/fixtures/`
    const YAML: &str = include_str!("../tests/fixtures/clash_airport.yaml");
    const B64: &str = include_str!("../tests/fixtures/links_base64.txt");
    const PLAIN: &str = include_str!("../tests/fixtures/links_plain.txt");

    fn base64_of(s: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    }

    /// ⚠️ 这个夹具是**合成的** —— `tests/fixtures/clash_airport.yaml` 自己的头部就写着
    /// "域名/密码全是编的，只为测试解析与计数"。
    ///
    /// **它不是真实机场订阅**，所以 [docs/06 §2.1] 的 **D2（真实形态脱敏样本）仍未闭**。
    /// 名字里带 `synthetic_` 就是为了不让后来人看到 "airport_fixture" 就以为已经闭了 ——
    /// [docs/13 L19]：从断言反推出来的夹具只能证明"代码与断言自洽"。
    #[test]
    fn clash_yaml_synthetic_airport_fixture() {
        let r = parse_clash_yaml(YAML);
        assert_eq!(r.group_count(), 5, "分组数");
        assert_eq!(r.node_count(), 15, "节点数，skipped={:?}", r.skipped);
        // 假节点（剩余流量/到期/官网）也在里面 —— 过滤是订阅级 filter 的活
        assert!(r.nodes.iter().any(|n| n.name.contains("剩余流量")));
        assert!(r.nodes.iter().any(|n| n.server == "hk01.example-airport.com" && n.port == 443));
        assert!(r.nodes.iter().any(|n| n.kind == "hysteria2"));
        assert!(r.nodes.iter().any(|n| n.kind == "tuic"));
        // provider 里内联的节点也算进来
        assert!(r.nodes.iter().any(|n| n.name == "🇹🇷 土耳其 01"));
        let auto = r.groups.iter().find(|g| g.name == "♻️ 自动选择").expect("自动选择组");
        assert_eq!(auto.kind, "url-test");
        assert!(auto.node_count >= 6, "自动选择组节点数 {}", auto.node_count);
        // DIRECT 不算节点
        let sel = r.groups.iter().find(|g| g.name == "🚀 节点选择").unwrap();
        assert!(sel.node_count >= 3);
    }

    // ---------------------------------------------------------- D2：真实形态样本

    const REAL_AIRPORT: &str = include_str!("../tests/fixtures/real-airport/airport.yaml");

    /// **独立**抽出文件里出现过的 `type:` 取值。
    ///
    /// 刻意不复用解析器的任何东西：这正是 [docs/13 L19] 的判据 ——
    /// 数字必须能"从文件直接数出来"，独立于实现；否则测试就退化成同义反复。
    fn types_in_text(text: &str) -> std::collections::BTreeSet<String> {
        let re = regex::Regex::new(r#"type:\s*['"]?([a-z0-9][a-z0-9-]*)['"]?"#).expect("正则");
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    /// [docs/06 §2.1] **D2**：真实机场订阅（脱敏）的解析一致性。
    ///
    /// 夹具是**真实形态**的（`tests/fixtures/real-airport/README.md` 记了来源、脱敏脚本与
    /// 第一版脚本的翻车教训）。**不断言节点数量** —— 数量不代表任何真实值，
    /// 只断言解析与分类的**行为**。
    ///
    /// 这份样本带来的、合成夹具**没有**的三个形态：**flow 风格**（`- { … }`）、
    /// 协议 **`anytls`**、16 个 `select` + 1 个 `url-test` 的嵌套分组。
    #[test]
    fn real_airport_fixture_parses_without_dropping_anything() {
        let r = parse_clash_yaml(REAL_AIRPORT);

        // ① 一个都不能静默丢：真实订阅里没有任何一条该被跳过
        assert!(r.skipped.is_empty(), "真实订阅不该有被丢的条目：{:?}", r.skipped);
        assert!(r.node_count() > 0, "真实订阅必须解析出节点");

        // ② 协议分类：文件里出现的协议（独立抽出）必须都出现在解析结果里
        let mut expected = types_in_text(REAL_AIRPORT);
        for group_kind in ["select", "url-test", "fallback", "load-balance", "relay"] {
            expected.remove(group_kind);
        }
        assert!(
            expected.contains("anytls"),
            "这份真样本应当含 anytls（否则夹具被换过）：{expected:?}"
        );
        let got: std::collections::BTreeSet<String> =
            r.nodes.iter().map(|n| n.kind.clone()).collect();
        for proto in &expected {
            assert!(got.contains(proto), "文件里有 {proto} 节点，解析结果却没有：{got:?}");
        }

        // ③ flow 风格确实存在（脱敏脚本第一版就是在这儿翻车的），而解析器吃住了
        assert!(REAL_AIRPORT.contains("- {"), "这份样本应当是 flow 风格");

        // ④ 每个节点的端点都可用（解析没有把服务端字段搞丢）
        for n in &r.nodes {
            assert!(!n.server.is_empty(), "节点 {} 的 server 为空", n.name);
            assert!(n.port > 0, "节点 {} 的 port 非法：{}", n.name, n.port);
        }

        // ⑤ 分组：真样本两种组都有，且组名不会被当成节点
        assert!(r.groups.iter().any(|g| g.kind == "select"), "应有 select 组");
        assert!(r.groups.iter().any(|g| g.kind == "url-test"), "应有 url-test 组");
        let node_names: std::collections::BTreeSet<&str> =
            r.nodes.iter().map(|n| n.name.as_str()).collect();
        for g in &r.groups {
            assert!(!node_names.contains(g.name.as_str()), "组名 {} 被当成了节点", g.name);
        }

        // ⑥ **边界**（写进测试，防止把"这一份样本"当成"全覆盖"）：
        //    这份真实订阅没有 `proxy-providers` / `use:`，也没有 ss/vless/trojan/tuic/ssr
        //    —— 那几类形态**仍然只有合成夹具覆盖**。真样本是补充，不是替代。
        assert!(
            !REAL_AIRPORT.contains("proxy-providers:"),
            "边界变了：真样本开始有 provider 了，请更新本条注释与 README"
        );
        for absent in ["ss", "vless", "trojan", "tuic", "ssr"] {
            assert!(
                !expected.contains(absent),
                "边界变了：真样本开始有 {absent} 了，请更新本条注释与 README"
            );
        }
    }

    #[test]
    fn clash_yaml_filter_drops_fake_nodes_from_fixture() {
        let mut r = parse_clash_yaml(YAML);
        let filter = SubscriptionFilter {
            include: None,
            exclude: Some("剩余|到期|官网|防失联|订阅".into()),
            extra: Default::default(),
        };
        let removed = apply_node_filter(&mut r.nodes, &filter);
        assert!(removed.len() >= 3, "应剔掉 3 个假节点，实际 {}", removed.len());
        assert_eq!(r.node_count(), 12);
    }

    #[test]
    fn clash_yaml_tolerates_broken_yaml() {
        // 缩进错乱 + 非法 flow 映射 → 结构化解析失败，宽松扫描兜底
        let broken = "proxies:\n  - name: 香港01\n    type: ss\n    server: hk1.example.com\n    port: 443\n   bad: [ { unclosed\n";
        let r = parse_clash_yaml(broken);
        assert_eq!(r.node_count(), 1, "skipped={:?}", r.skipped);
        assert_eq!(r.nodes[0].name, "香港01");
        assert_eq!(r.nodes[0].port, 443);
    }

    #[test]
    fn clash_yaml_skips_bad_entries_without_failing() {
        let yaml = "proxies:\n  - {name: ok, type: ss, server: a.com, port: 443}\n  \
                    - {name: no-port, type: ss, server: b.com}\n  - {type: ss, server: c.com, port: 1}\n";
        let r = parse_clash_yaml(yaml);
        assert_eq!(r.node_count(), 1);
        assert_eq!(r.skipped.len(), 2);
    }

    #[test]
    fn base64_fixture_links() {
        let text = detect::to_parseable_text(B64.as_bytes(), DetectedFormat::Base64Links);
        let r = parse_links(&text);
        assert_eq!(r.node_count(), 12, "skipped={:?}", r.skipped);
        assert_eq!(r.skipped.len(), 1, "skipped={:?}", r.skipped);
        assert!(r.skipped[0].contains("snell"));
        let kinds: Vec<&str> = r.nodes.iter().map(|n| n.kind.as_str()).collect();
        for k in ["ss", "ssr", "vmess", "vless", "trojan", "hysteria2", "tuic"] {
            assert!(kinds.contains(&k), "缺 {k}：{kinds:?}");
        }
        // 中文 emoji 名字要能解出来
        assert!(r.nodes.iter().any(|n| n.name == "🇭🇰 香港 01"));
        assert!(r.nodes.iter().any(|n| n.name == "🇯🇵 日本 01"));
    }

    #[test]
    fn plain_fixture_links() {
        let r = parse_links(PLAIN);
        assert_eq!(r.node_count(), 7, "skipped={:?}", r.skipped);
        assert!(r.nodes.iter().all(|n| n.port > 0 && !n.server.is_empty()));
    }

    #[test]
    fn vmess_field_variants() {
        // v2rayN 变体：ps/add/port/id/aid/scy/net/tls/host/path（port 是字符串）
        let j = r#"{"v":"2","ps":"日本 01","add":"jp01.example.com","port":"443","id":"b831381d-6324-4d53-ad4f-8cda48b30811","aid":"0","scy":"auto","net":"ws","tls":"tls","host":"jp01.example.com","path":"/vm"}"#;
        let n = parse_link("vmess", &base64_of(j)).unwrap();
        assert_eq!(n.name, "日本 01");
        assert_eq!(n.server, "jp01.example.com");
        assert_eq!(n.port, 443);
        // 变体：remarks/address + port 是数字
        let j2 = r#"{"remarks":"HK","address":"hk.example.com","port":8443,"id":"x"}"#;
        let n2 = parse_link("vmess", &base64_of(j2)).unwrap();
        assert_eq!((n2.name.as_str(), n2.server.as_str(), n2.port), ("HK", "hk.example.com", 8443));
        // 变体：只有 server，没有名字 → 用 server:port 兜底
        let j3 = r#"{"server":"sg.example.com","port":"2053","id":"x"}"#;
        let n3 = parse_link("vmess", &base64_of(j3)).unwrap();
        assert_eq!(n3.name, "vmess-sg.example.com:2053");
        // URI 形态（少数客户端）
        let n4 = parse_link("vmess", "b831381d@uri.example.com:443?type=ws#URI%20Node").unwrap();
        assert_eq!(
            (n4.server.as_str(), n4.port, n4.name.as_str()),
            ("uri.example.com", 443, "URI Node")
        );
    }

    #[test]
    fn hysteria2_with_insecure_and_tuic() {
        let n = parse_link(
            "hysteria2",
            "passwd@hy2.example.com:443?sni=hy2.example.com&insecure=1&obfs=salamander&obfs-password=x#%F0%9F%87%AD%F0%9F%87%B0%20HK",
        )
        .unwrap();
        assert_eq!(n.kind, "hysteria2");
        assert_eq!(n.server, "hy2.example.com");
        assert_eq!(n.port, 443);
        assert_eq!(n.name, "🇭🇰 HK");
        // hy2 别名
        let n = parse_link("hy2", "a@h.example.com:8443#x").unwrap();
        assert_eq!((n.kind.as_str(), n.port), ("hysteria2", 8443));
        // tuic 的 uuid:password@host:port
        let n = parse_link("tuic", "uuid-1:pass@tuic.example.com:443?congestion_control=bbr#T")
            .unwrap();
        assert_eq!((n.kind.as_str(), n.server.as_str(), n.port), ("tuic", "tuic.example.com", 443));
    }

    #[test]
    fn ss_and_ssr_forms() {
        // SIP002：userinfo 是 base64
        let n = parse_link("ss", "YWVzLTI1Ni1nY206cGFzcw@1.2.3.4:8388#Node%201").unwrap();
        assert_eq!((n.server.as_str(), n.port, n.name.as_str()), ("1.2.3.4", 8388, "Node 1"));
        // 老格式：整段 base64（含 `/` 的 base64 不能被截断）
        let legacy = base64_of("aes-256-gcm:pass@5.6.7.8:8388");
        let n = parse_link("ss", &legacy).unwrap();
        assert_eq!((n.server.as_str(), n.port), ("5.6.7.8", 8388));
        // 插件查询参数不影响 host:port
        let n = parse_link("ss", "YWVzOnB3@9.9.9.9:443?plugin=obfs-local%3Bobfs%3Dhttp").unwrap();
        assert_eq!((n.server.as_str(), n.port), ("9.9.9.9", 443));
        // ssr：名字在 remarks 里（base64）
        let ssr = base64_of(
            "ssr.example.com:1234:auth_aes128_md5:aes-256-cfb:plain:cGFzcw==/?remarks=U1NSIE5vZGU",
        );
        let n = parse_link("ssr", &ssr).unwrap();
        assert_eq!(n.kind, "ssr");
        assert_eq!(
            (n.server.as_str(), n.port, n.name.as_str()),
            ("ssr.example.com", 1234, "SSR Node")
        );
    }

    #[test]
    fn ipv6_and_bad_links() {
        let n = parse_link("trojan", "p@[2001:db8::1]:443#v6").unwrap();
        assert_eq!((n.server.as_str(), n.port), ("2001:db8::1", 443));
        assert!(parse_link("vless", "uuid@noport").is_err());
        assert!(parse_link("socks5", "1.2.3.4:1080").is_err());
        assert!(parse_link("vmess", "not-base64!!!").is_err());
        assert!(parse_link("vmess", &base64_of("{}")).is_err());
    }

    #[test]
    fn filter_removes_fake_nodes() {
        let mut nodes = vec![
            NodeMeta {
                name: "🇭🇰 香港 01".into(), kind: "ss".into(), server: "a".into(), port: 1
            },
            NodeMeta {
                name: "剩余流量：287.35 GB".into(),
                kind: "ss".into(),
                server: "b".into(),
                port: 1,
            },
            NodeMeta {
                name: "套餐到期：2026-01-01".into(),
                kind: "ss".into(),
                server: "c".into(),
                port: 1,
            },
            NodeMeta {
                name: "官网 example.com".into(),
                kind: "ss".into(),
                server: "d".into(),
                port: 1,
            },
        ];
        let filter = SubscriptionFilter {
            include: None,
            exclude: Some("剩余|到期|官网|防失联".into()),
            extra: Default::default(),
        };
        let removed = apply_node_filter(&mut nodes, &filter);
        assert_eq!(removed.len(), 3);
        assert_eq!(nodes.len(), 1);
        // include 生效
        let filter = SubscriptionFilter {
            include: Some("香港".into()),
            exclude: None,
            extra: Default::default(),
        };
        apply_node_filter(&mut nodes, &filter);
        assert_eq!(nodes.len(), 1);
        // 非法正则 → 不过滤（不崩）
        let bad = SubscriptionFilter {
            include: Some("([".into()),
            exclude: None,
            extra: Default::default(),
        };
        let mut n2 = nodes.clone();
        assert!(apply_node_filter(&mut n2, &bad).is_empty());
        assert_eq!(n2.len(), 1);
        // 没有 filter → 原样
        let mut n3 = nodes.clone();
        assert!(apply_node_filter(&mut n3, &SubscriptionFilter::default()).is_empty());
        assert_eq!(n3.len(), 1);
    }

    #[test]
    fn userinfo_header() {
        let info =
            parse_userinfo("upload=1024; download=2048; total=107374182400; expire=1780000000")
                .unwrap();
        assert_eq!(info.upload, 1024);
        assert_eq!(info.download, 2048);
        assert_eq!(info.total, 107_374_182_400);
        assert_eq!(info.expire_ms, Some(1_780_000_000_000));
        assert_eq!(info.used(), 3072);
        assert!(info.remaining_ratio().unwrap() > 0.99);
        // 毫秒形态 + 缺字段 + 空格
        let info = parse_userinfo("upload = 1 , total = 0 , expire=1780000000000").unwrap();
        assert_eq!(info.expire_ms, Some(1_780_000_000_000));
        assert_eq!(info.remaining_ratio(), None);
        assert!(parse_userinfo("nothing here").is_none());
        assert!(SubUserinfo::default().remaining_ratio().is_none());
    }

    #[test]
    fn interval_header_hours_vs_seconds() {
        assert_eq!(parse_update_interval("6"), Some(21_600));
        assert_eq!(parse_update_interval("24"), Some(86_400));
        assert_eq!(parse_update_interval("21600"), Some(21_600));
        assert_eq!(parse_update_interval("0"), None);
        assert_eq!(parse_update_interval("abc"), None);
    }

    #[test]
    fn content_disposition() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"airport.yaml\"").as_deref(),
            Some("airport.yaml")
        );
        assert_eq!(
            filename_from_content_disposition("inline; filename*=UTF-8''%E9%85%8D%E7%BD%AE.yaml")
                .as_deref(),
            Some("配置.yaml")
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=plain.txt;").as_deref(),
            Some("plain.txt")
        );
        assert_eq!(filename_from_content_disposition("attachment"), None);
    }

    #[test]
    fn percent_decode_works() {
        assert_eq!(percent_decode("%E9%A6%99%E6%B8%AF"), "香港");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(parse_host_port("h:0"), Some(("h".into(), 0)));
        assert_eq!(parse_host_port(""), None);
        assert_eq!(parse_host_port("h:99999"), None);
    }

    #[test]
    fn extract_dispatches_by_format() {
        assert_eq!(extract(b"<html>x</html>", DetectedFormat::Html).node_count(), 0);
        assert_eq!(extract(b"garbage", DetectedFormat::Unknown).node_count(), 0);
        assert_eq!(extract(PLAIN.as_bytes(), DetectedFormat::PlainLinks).node_count(), 7);
        assert_eq!(extract(B64.as_bytes(), DetectedFormat::Base64Links).node_count(), 12);
        assert_eq!(extract(YAML.as_bytes(), DetectedFormat::ClashYaml).node_count(), 15);
    }
}

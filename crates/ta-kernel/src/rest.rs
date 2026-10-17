//! 内核 REST 客户端（`/configs`、`/proxies`、`/connections` 等）。
//!
//! 硬性约束：
//! - 鉴权 `Authorization: Bearer <secret>`；
//! - **仅 loopback**：`net.kernel.controller` 不是回环地址直接拒绝（无系统副作用）；
//! - **绝不走系统/环境代理**（`reqwest::ClientBuilder::no_proxy`），否则等于把内核控制面
//!   暴露给了一个不存在的代理。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use ta_core::error::{FieldError, Result, TaError};

use crate::{
    DanglingRef, DelaySample, NodeRef, NodeSource, ProviderEntry, ProvidersSnapshot, ProxyEntry,
};

/// 测速默认探测 URL。 建议**优先用该 lane 真实会用到的上游端点**，
/// 调用方通过 [`KernelRest::with_probe_url`] 覆盖。
pub const DEFAULT_PROBE_URL: &str = "https://www.gstatic.com/generate_204";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 出错时把响应体截断到这个长度再塞进错误信息
const BODY_SNIPPET: usize = 200;

#[derive(Debug, Clone)]
pub struct KernelRest {
    client: reqwest::Client,
    /// 归一化后的 `host:port`（无 scheme），如 `127.0.0.1:39090`
    controller: String,
    secret: String,
    probe_url: String,
}

impl KernelRest {
    /// `controller` 形如 `127.0.0.1:39090`（也接受 `http://127.0.0.1:39090`）。
    pub fn new(controller: &str, secret: impl Into<String>) -> Result<Self> {
        let controller = normalize_controller(controller)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(DEFAULT_READ_TIMEOUT)
            .user_agent(concat!("TokenAgency/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| TaError::Kernel(format!("构建内核 REST 客户端失败：{e}")))?;
        Ok(Self {
            client,
            controller,
            secret: secret.into(),
            probe_url: DEFAULT_PROBE_URL.to_string(),
        })
    }

    pub fn with_probe_url(mut self, url: impl Into<String>) -> Self {
        self.probe_url = url.into();
        self
    }

    /// 归一化的 `host:port`
    pub fn controller(&self) -> &str {
        &self.controller
    }

    pub fn probe_url(&self) -> &str {
        &self.probe_url
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.controller, path)
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.secret.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.secret)
        }
    }

    /// 发请求 + 状态码检查（不读 body）
    async fn send(&self, what: &str, rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = rb.send().await.map_err(|e| TaError::Kernel(format!("{what} 请求失败：{e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(status_error(what, status.as_u16(), &body));
        }
        Ok(resp)
    }

    /// 发请求 + 解析 JSON
    async fn send_json<T: DeserializeOwned>(
        &self,
        what: &str,
        rb: reqwest::RequestBuilder,
    ) -> Result<T> {
        let resp = self.send(what, rb).await?;
        let bytes =
            resp.bytes().await.map_err(|e| TaError::Kernel(format!("{what} 读取响应失败：{e}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| TaError::Kernel(format!("{what} 响应解析失败：{e}{}", snippet(&bytes))))
    }

    // ---------------------------------------------------------------- 端点

    /// `GET /version` —— 就绪探测与版本
    pub async fn version(&self) -> Result<String> {
        let resp =
            self.send("GET /version", self.auth(self.client.get(self.url("/version")))).await?;
        let text = resp
            .text()
            .await
            .map_err(|e| TaError::Kernel(format!("GET /version 读取响应失败：{e}")))?;
        // 正常形态：{"version":"v1.18.4","meta":true}
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or_default();
            if ver.is_empty() {
                return Err(TaError::Kernel("GET /version 响应里没有 version 字段".to_string()));
            }
            return Ok(ver.to_string());
        }
        // 极端兜底：非 JSON 但 200，端口确实活着
        let t = text.trim();
        if t.is_empty() {
            return Err(TaError::Kernel("GET /version 返回空响应".to_string()));
        }
        Ok(t.to_string())
    }

    /// `PUT /configs?force=true` body `{"path": "<abs>"}`
    pub async fn reload_config(&self, abs_path: &str) -> Result<()> {
        if !std::path::Path::new(abs_path).is_absolute() {
            return Err(TaError::ConfigInvalid {
                errors: vec![FieldError {
                    path: "net.kernel.path".into(),
                    code: "config_path_not_absolute".into(),
                    message: format!("内核 reload 需要绝对路径，收到：{abs_path}"),
                }],
            });
        }
        let rb =
            self.client.put(self.url("/configs?force=true")).json(&json!({ "path": abs_path }));
        self.send("PUT /configs", self.auth(rb)).await?;
        Ok(())
    }

    /// `PUT /proxies/{group}` body `{"name": "<node>"}` —— 组内切节点
    pub async fn set_node(&self, group: &str, node: &str) -> Result<()> {
        let url = self.url(&format!("/proxies/{}", encode_path_segment(group)));
        let rb = self.client.put(url).json(&json!({ "name": node }));
        self.send(&format!("PUT /proxies/{group}"), self.auth(rb)).await?;
        Ok(())
    }

    /// `GET /proxies/{name}/delay?timeout=5000&url=<probe>`
    pub async fn delay_of(&self, name: &str, timeout_ms: u64) -> Result<u32> {
        #[derive(Deserialize)]
        struct DelayBody {
            delay: u32,
        }
        let mut url = reqwest::Url::parse(
            &self.url(&format!("/proxies/{}/delay", encode_path_segment(name))),
        )
        .map_err(|e| TaError::Kernel(format!("构造测速 URL 失败：{e}")))?;
        url.query_pairs_mut()
            .append_pair("timeout", &timeout_ms.to_string())
            .append_pair("url", &self.probe_url);
        let rb = self
            .client
            .get(url)
            // 内核自己的测速超时 + 一点余量，避免我们的客户端先放弃
            .timeout(Duration::from_millis(timeout_ms.max(1000) + 2_000));
        let body: DelayBody =
            self.send_json(&format!("GET /proxies/{name}/delay"), self.auth(rb)).await?;
        Ok(body.delay)
    }

    /// `PUT /providers/proxies/{key}` —— 刷新订阅
    pub async fn refresh_provider(&self, key: &str) -> Result<()> {
        let url = self.url(&format!("/providers/proxies/{}", encode_path_segment(key)));
        let rb = self.client.put(url);
        self.send(&format!("PUT /providers/proxies/{key}"), self.auth(rb)).await?;
        Ok(())
    }

    /// `GET /providers/proxies/{key}/healthcheck?url=<probe>` —— 触发**整份订阅**的体检。
    ///
    /// ★ **为什么不能用 `GET /proxies/{name}/delay` 测 provider 的节点**（实测，
    /// mihomo v1.19.31）：proxy-provider 的节点**不在 `/proxies` 顶层** —— 顶层只有内核内置的
    /// 8 项（`COMPATIBLE/DIRECT/GLOBAL/PASS/PASS-RULE/REJECT/REJECT-DROP` + 我们建的组）。
    /// 四条命名形式 `/proxies/<name>/delay`、`/proxies/<provider>/<name>/delay`、
    /// `/proxies/<provider>|<name>/delay`、`/providers/proxies/<provider>/<name>/delay`
    /// **全部 404**。provider 的节点只走 provider 命名空间：先体检，再读
    /// [`KernelRest::provider_node_delays`] 里的 `history[].delay`。
    pub async fn provider_healthcheck(
        &self,
        key: &str,
        probe_url: &str,
        timeout_ms: u64,
    ) -> Result<()> {
        let mut url = reqwest::Url::parse(
            &self.url(&format!("/providers/proxies/{}/healthcheck", encode_path_segment(key))),
        )
        .map_err(|e| TaError::Kernel(format!("构造 provider 体检 URL 失败：{e}")))?;
        url.query_pairs_mut()
            .append_pair("timeout", &timeout_ms.to_string())
            .append_pair("url", probe_url);
        let rb = self
            .client
            .get(url)
            // 内核要一家一家去拨（34 条节点），给它比单节点测速宽得多的余量；
            // 我们的客户端超时必须**晚于**内核收工，否则会把"慢"误判成"控制面坏了"。
            .timeout(Duration::from_millis(timeout_ms.max(1_000).saturating_add(15_000)));
        self.send(&format!("GET /providers/proxies/{key}/healthcheck"), self.auth(rb)).await?;
        Ok(())
    }

    /// `GET /providers/proxies/{key}` → 节点名 → 内核测出来的延迟（`None` = 测不到）。
    pub async fn provider_node_delays(&self, key: &str) -> Result<BTreeMap<String, Option<u32>>> {
        let rb =
            self.client.get(self.url(&format!("/providers/proxies/{}", encode_path_segment(key))));
        let body: serde_json::Value =
            self.send_json(&format!("GET /providers/proxies/{key}"), self.auth(rb)).await?;
        parse_provider_delays(&body, key)
    }

    /// **「自动」出口要的那张延迟表**：先触发一次全 provider 体检，再读到"体检跑完"为止。
    ///
    /// 为什么要轮询：内核的 `healthcheck` 是**并发**拨号的，`204` 回来时不一定每个节点都写好了
    /// `history`。判据**不是**"睡够多久"，而是下面两条之一：
    ///
    /// 1. `measured == total`（全都量到了 ⇒ 表是完整的，立刻用）；
    /// 2. 连续 **1 秒**（4 次 × 250ms）没有任何新进展、且至少量到一条 ⇒ 剩下的确实量不出来。
    ///
    /// ★ 为什么 ② 不能写成"连续两次读数相同"：那只要 **250 ms** 就成立，而并发拨号里节点是
    /// **陆续**写 `history` 的 —— 半路下结论会把**真最快那条**当成"还没测到"而错过它
    /// （真机实测：应用只量到 27/34 就裁决，选出来的那条随后被量出 2308ms，而真最快是 137ms）。
    ///
    /// 到点仍然一条都没测出来时**如实返回空/全 `None`** —— 由上层落成 `all_unreachable`，
    /// 绝不在这里编一个延迟出来。
    pub async fn provider_delays(
        &self,
        key: &str,
        probe_url: &str,
        wait_ms: u64,
    ) -> Result<BTreeMap<String, Option<u32>>> {
        self.provider_healthcheck(key, probe_url, DEFAULT_READ_TIMEOUT.as_millis() as u64).await?;
        let deadline = std::time::Instant::now() + Duration::from_millis(wait_ms);
        let mut prev: Option<usize> = None;
        let mut stable = 0u32;
        loop {
            let map = self.provider_node_delays(key).await?;
            let total = map.len();
            // 空 provider（还没拉到节点）不必等：等下去也不会自己长出节点来。
            if total == 0 {
                return Ok(map);
            }
            let measured = map.values().filter(|v| v.is_some()).count();
            stable = if prev == Some(measured) { stable + 1 } else { 0 };
            if measured == total || (stable >= 4 && measured > 0) {
                return Ok(map);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(map);
            }
            prev = Some(measured);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// `GET /proxies`（原始 JSON）
    pub async fn proxies(&self) -> Result<serde_json::Value> {
        let rb = self.client.get(self.url("/proxies"));
        self.send_json("GET /proxies", self.auth(rb)).await
    }

    /// `GET /providers/proxies`（原始 JSON）
    pub async fn providers(&self) -> Result<serde_json::Value> {
        let rb = self.client.get(self.url("/providers/proxies"));
        self.send_json("GET /providers/proxies", self.auth(rb)).await
    }

    /// `GET /proxies` + `GET /providers/proxies` → UI 用的线路快照
    pub async fn snapshot(&self) -> Result<ProvidersSnapshot> {
        let proxies = self.proxies().await?;
        let providers = self.providers().await?;
        parse_snapshot(&proxies, &providers)
    }

    /// `GET /providers/proxies` → provider key → 节点数（就绪判定用）
    pub async fn provider_node_counts(&self) -> Result<BTreeMap<String, usize>> {
        parse_provider_counts(&self.providers().await?)
    }
}

// ================================================================ 纯解析

/// 把内核两个端点的响应拼成 [`ProvidersSnapshot`]。纯函数，便于用 fixture 测。
///
/// 三件事按顺序做（7-a ~ 7-d）：
/// 1. `/proxies` 顶层进索引，来源标 [`NodeSource::TopLevel`]；
/// 2. `/providers/proxies` 里每个 provider 的节点**补进**索引（来源 `Provider(key)`）——
///    **顶层优先**，因为 provider 那边还会把组镜射一遍；
/// 3. 索引建好**之后**才解析组的 `all`，解析不到的名字进 [`ProvidersSnapshot::dangling`]。
///
/// 顺序很关键：先算 `all` 再合并，就会把 provider 节点全判成悬空（这正是 7-c）。
pub fn parse_snapshot(
    proxies: &serde_json::Value,
    providers: &serde_json::Value,
) -> Result<ProvidersSnapshot> {
    let proxies_env = ProxiesEnvelope::deserialize(proxies)
        .map_err(|e| TaError::Kernel(format!("GET /proxies 形态不符：{e}")))?;
    let providers_env = ProvidersEnvelope::deserialize(providers)
        .map_err(|e| TaError::Kernel(format!("GET /providers/proxies 形态不符：{e}")))?;

    // ---- 1) /proxies 顶层
    let mut nodes: BTreeMap<String, NodeRef> = BTreeMap::new();
    let mut out_proxies = Vec::with_capacity(proxies_env.proxies.len());
    for (name, raw) in proxies_env.proxies {
        nodes.entry(name.clone()).or_insert_with(|| NodeRef {
            name: name.clone(),
            kind: raw.kind.clone(),
            alive: raw.alive,
            source: NodeSource::TopLevel,
        });
        out_proxies.push(ProxyEntry {
            name,
            kind: raw.kind,
            alive: raw.alive,
            now: raw.now,
            all: raw.all.unwrap_or_default(),
            history: raw
                .history
                .into_iter()
                .filter(|h| !h.time.is_empty() || h.delay > 0)
                .map(|h| DelaySample { time: h.time, delay: h.delay })
                .collect(),
        });
    }

    // ---- 2) providers（7-a：是个**对象**；7-c：它们的节点不在 `/proxies` 顶层）
    let mut out_providers = Vec::with_capacity(providers_env.providers.len());
    for (key, raw) in providers_env.providers {
        let mut provider_nodes = Vec::with_capacity(raw.proxies.len());
        for node in raw.proxies {
            let node_ref = NodeRef {
                name: node.name,
                kind: node.kind,
                alive: node.alive,
                source: NodeSource::Provider(key.clone()),
            };
            // ★ 顶层优先：provider 侧的镜射项不许覆盖已有来源（真机上 DIRECT / TA-LANE-* 两边都有）
            nodes.entry(node_ref.name.clone()).or_insert_with(|| node_ref.clone());
            provider_nodes.push(node_ref);
        }
        out_providers.push(ProviderEntry {
            name: raw.name.unwrap_or_else(|| key.clone()),
            key,
            vehicle_type: raw.vehicle_type.unwrap_or_default(),
            updated_at: raw.updated_at,
            node_count: provider_nodes.len(),
            nodes: provider_nodes,
        });
    }

    // ---- 3) 索引建好之后才解析 `all`：解析不到的**不静默丢**
    let mut dangling = Vec::new();
    for p in &out_proxies {
        for member in &p.all {
            if !nodes.contains_key(member) {
                dangling.push(DanglingRef { group: p.name.clone(), name: member.clone() });
            }
        }
    }

    Ok(ProvidersSnapshot { proxies: out_proxies, providers: out_providers, nodes, dangling })
}

/// `GET /providers/proxies` → provider key → 节点数。
///
/// **key 才是 provider 的身份**（我们生成的 `sub-jp` 这种），`name` 是给人看的机场名。
/// 就绪判定必须按 key 查 —— 内核在 provider 加载失败（文件缺失 / 为空 / 解析错）时
/// **一边 `level=error` 一边照常启动**，此时那个 key 的节点数就是 0。
pub fn parse_provider_counts(providers: &serde_json::Value) -> Result<BTreeMap<String, usize>> {
    let env = ProvidersEnvelope::deserialize(providers)
        .map_err(|e| TaError::Kernel(format!("GET /providers/proxies 形态不符：{e}")))?;
    Ok(env.providers.into_iter().map(|(key, raw)| (key, raw.proxies.len())).collect())
}

/// `GET /providers/proxies/{key}` → 节点名 → 内核测出来的延迟（`None` = 测不到）。
///
/// 判据与 [`NodeRef::latest_delay`] 同源：**只信内核给的 `alive` + 末次 `history[].delay`**。
/// 三条都要落成 `None`：`alive == false`、`history` 为空（没测过）、末次 `delay == 0`
/// （内核测失败时写 0）。**绝不用 0 冒充"最快"** —— 那会选出一条其实不通的出口。
pub fn parse_provider_delays(
    body: &serde_json::Value,
    key: &str,
) -> Result<BTreeMap<String, Option<u32>>> {
    #[derive(Deserialize)]
    struct OneProvider {
        #[serde(default)]
        proxies: Vec<ProviderNodeRaw>,
    }
    let one: OneProvider = serde_json::from_value(body.clone())
        .map_err(|e| TaError::Kernel(format!("GET /providers/proxies/{key} 形态不符：{e}")))?;
    Ok(one
        .proxies
        .into_iter()
        .map(|n| {
            let delay = node_delay(&n);
            (n.name, delay)
        })
        .collect())
}

/// 一个 provider 节点"测出来的延迟"。
fn node_delay(n: &ProviderNodeRaw) -> Option<u32> {
    if !n.alive {
        return None;
    }
    match n.history.last() {
        Some(h) if h.delay > 0 => Some(h.delay),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct ProxiesEnvelope {
    #[serde(default)]
    proxies: BTreeMap<String, ProxyRaw>,
}

#[derive(Debug, Deserialize)]
struct ProxyRaw {
    #[serde(rename = "type", default)]
    kind: String,
    /// ★ 内核给的健康信号。真机上**每个节点与组都有**（附带验证）。
    #[serde(default)]
    alive: bool,
    #[serde(default)]
    now: Option<String>,
    #[serde(default)]
    all: Option<Vec<String>>,
    #[serde(default)]
    history: Vec<HistoryRaw>,
}

#[derive(Debug, Deserialize)]
struct HistoryRaw {
    #[serde(default)]
    time: String,
    #[serde(default)]
    delay: u32,
}

#[derive(Debug, Deserialize)]
struct ProvidersEnvelope {
    #[serde(default)]
    providers: BTreeMap<String, ProviderRaw>,
}

#[derive(Debug, Deserialize)]
struct ProviderRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "vehicleType", default)]
    vehicle_type: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(default)]
    proxies: Vec<ProviderNodeRaw>,
}

/// provider 里带的节点。字段与 [`ProxyRaw`] 同源（同一个 mihomo 节点结构），
/// 这里只取索引与测速要用的字段 —— 其余字段由 serde 忽略，**不多写就是不多猜**。
#[derive(Debug, Deserialize)]
struct ProviderNodeRaw {
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    alive: bool,
    /// ★ provider 节点的延迟**只在这里**（`/proxies` 顶层没有它们，`delay` 路由也 404）。
    #[serde(default)]
    history: Vec<HistoryRaw>,
}

// ================================================================ 工具

/// 把 `net.kernel.controller` 归一化成 `host:port` 并强制 loopback。
pub fn normalize_controller(controller: &str) -> Result<String> {
    let raw = controller.trim();
    let hostport =
        raw.strip_prefix("http://").or_else(|| raw.strip_prefix("https://")).unwrap_or(raw);
    let hostport = hostport.trim_end_matches('/');

    let (host, port) = split_host_port(hostport).ok_or_else(|| {
        controller_error(format!("无法解析内核控制地址：{controller:?}（应为 host:port）"))
    })?;
    let port: u16 = port
        .parse()
        .map_err(|_| controller_error(format!("内核控制端口不是合法端口：{port:?}")))?;
    if port == 0 {
        return Err(controller_error("内核控制端口不能为 0".to_string()));
    }
    let host = host.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return Err(controller_error(format!(
            "内核控制面必须只监听 loopback，拒绝 {host:?}（ 不允许任何 0.0.0.0 监听）"
        )));
    }
    if host == "localhost" {
        return Ok(format!("127.0.0.1:{port}"));
    }
    if host == "::1" {
        return Ok(format!("[::1]:{port}"));
    }
    Ok(format!("{host}:{port}"))
}

fn split_host_port(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix('[') {
        // IPv6 字面量：[::1]:39090
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?;
        return Some((host, port));
    }
    let (host, port) = s.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

fn controller_error(message: String) -> TaError {
    TaError::ConfigInvalid {
        errors: vec![FieldError {
            path: "net.kernel.controller".into(),
            code: "not_loopback".into(),
            message,
        }],
    }
}

fn status_error(what: &str, status: u16, body: &str) -> TaError {
    let body = body.trim();
    let body = if body.is_empty() {
        String::new()
    } else {
        format!("，响应：{}", snippet(body.as_bytes()))
    };
    if status == 401 || status == 403 {
        TaError::Kernel(format!("{what} 鉴权失败：HTTP {status}（secret 与内核不一致）{body}"))
    } else {
        TaError::Kernel(format!("{what} 失败：HTTP {status}{body}"))
    }
}

fn snippet(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    if s.len() <= BODY_SNIPPET {
        format!("（原文：{s}）")
    } else {
        let mut cut = BODY_SNIPPET;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("（原文：{}…）", &s[..cut])
    }
}

/// URL 路径段百分号编码。**只保留 unreserved 字符**，`/`、`?`、`#`、空格、emoji
/// 全部转义 —— 节点名里出现 `/` 时不会打穿路径。
pub fn encode_path_segment(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// 供 UI 透传的原始快照（给需要未归一化 JSON 的调用方）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RawSnapshot {
    pub proxies: serde_json::Value,
    pub providers: serde_json::Value,
}

impl RawSnapshot {
    pub async fn fetch(rest: &KernelRest) -> Result<Self> {
        Ok(Self { proxies: rest.proxies().await?, providers: rest.providers().await? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn rest(server: &MockServer) -> KernelRest {
        KernelRest::new(&server.uri(), "test-secret").expect("loopback 校验应通过")
    }

    #[tokio::test]
    async fn raw_snapshot_fetches_both_endpoints() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "proxies": {} })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/providers/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "providers": {} })))
            .mount(&server)
            .await;
        let raw = RawSnapshot::fetch(&rest(&server)).await.unwrap();
        assert!(raw.proxies.get("proxies").is_some());
        assert!(raw.providers.get("providers").is_some());
    }

    #[test]
    fn controller_must_be_loopback() {
        for bad in ["0.0.0.0:39090", "192.168.1.5:39090", "example.com:39090", "39090"] {
            let err = normalize_controller(bad).unwrap_err();
            match err {
                TaError::ConfigInvalid { errors } => {
                    assert_eq!(errors[0].path, "net.kernel.controller", "bad={bad}");
                }
                other => panic!("期望 ConfigInvalid，得到 {other:?}"),
            }
        }
        assert_eq!(normalize_controller("127.0.0.1:39090").unwrap(), "127.0.0.1:39090");
        assert_eq!(normalize_controller("http://127.0.0.1:39090/").unwrap(), "127.0.0.1:39090");
        assert_eq!(normalize_controller("localhost:39090").unwrap(), "127.0.0.1:39090");
        assert_eq!(normalize_controller("[::1]:39090").unwrap(), "[::1]:39090");
    }

    #[test]
    fn path_segment_encoding() {
        assert_eq!(encode_path_segment("sub-jp"), "sub-jp");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("东京 03"), "%E4%B8%9C%E4%BA%AC%2003");
        assert_eq!(encode_path_segment("?x=1#y"), "%3Fx%3D1%23y");
    }

    #[tokio::test]
    async fn version_sends_bearer_and_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("authorization", "Bearer test-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "version": "v1.18.4",
                "meta": true
            })))
            .mount(&server)
            .await;

        let v = rest(&server).version().await.unwrap();
        assert_eq!(v, "v1.18.4");
    }

    #[tokio::test]
    async fn bad_secret_is_reported_as_auth_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let err = rest(&server).version().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("鉴权失败"), "msg={msg}");
        assert!(matches!(err, TaError::Kernel(_)));
    }

    #[tokio::test]
    async fn reload_config_puts_absolute_path() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/configs"))
            .and(query_param("force", "true"))
            .and(header("authorization", "Bearer test-secret"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let abs = std::env::temp_dir().join("ta-kernel-test/kernel.generated.yaml");
        rest(&server).reload_config(&abs.to_string_lossy()).await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["path"], json!(abs.to_string_lossy()));
    }

    #[tokio::test]
    async fn reload_config_rejects_relative_path() {
        let err = KernelRest::new("127.0.0.1:39090", "s")
            .unwrap()
            .reload_config("kernel.generated.yaml")
            .await
            .unwrap_err();
        match err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].code, "config_path_not_absolute")
            }
            other => panic!("期望 ConfigInvalid，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_node_encodes_unicode_group_name() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(wiremock::matchers::path_regex(r"^/proxies/.*$"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        rest(&server).set_node("🇯🇵 日本", "东京-03").await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs[0].url.path(), "/proxies/%F0%9F%87%AF%F0%9F%87%B5%20%E6%97%A5%E6%9C%AC");
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body, json!({ "name": "东京-03" }));
    }

    #[tokio::test]
    async fn delay_of_parses_and_passes_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxies/DIRECT/delay"))
            .and(query_param("timeout", "5000"))
            .and(query_param("url", DEFAULT_PROBE_URL))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "delay": 88 })))
            .mount(&server)
            .await;

        let d = rest(&server).delay_of("DIRECT", 5000).await.unwrap();
        assert_eq!(d, 88);
    }

    #[tokio::test]
    async fn delay_of_surfaces_kernel_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(r"^/proxies/.*/delay$"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "message": "timeout" })))
            .mount(&server)
            .await;

        let err = rest(&server).delay_of("东京-03", 3000).await.unwrap_err();
        assert!(err.to_string().contains("HTTP 400"), "{err}");
    }

    #[tokio::test]
    async fn refresh_provider_puts_key() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/providers/proxies/sub-jp"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        rest(&server).refresh_provider("sub-jp").await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn provider_delays_triggers_healthcheck_then_reads_history() {
        // ★ 这是「自动」出口测速的**唯一**正确路径：
        //   `/proxies/<name>/delay` 对 provider 节点是全 404 的（顶层没有它们）。
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/providers/proxies/sub-jp/healthcheck"))
            .and(query_param("url", "https://ark.example/v3/models"))
            .and(query_param("timeout", "10000"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/providers/proxies/sub-jp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "机场A",
                "vehicleType": "HTTP",
                "proxies": [
                    { "name": "东京-03", "type": "Vmess", "alive": true,
                      "history": [{ "time": "t1", "delay": 0 }, { "time": "t2", "delay": 120 }] },
                    { "name": "大阪-01", "type": "Vmess", "alive": true, "history": [] },
                    { "name": "首尔-01", "type": "Vmess", "alive": false,
                      "history": [{ "time": "t1", "delay": 300 }] }
                ]
            })))
            .mount(&server)
            .await;

        let map = rest(&server)
            .provider_delays("sub-jp", "https://ark.example/v3/models", 3_000)
            .await
            .unwrap();

        assert_eq!(map.get("东京-03"), Some(&Some(120)), "末次 120ms 要读出来");
        assert_eq!(map.get("大阪-01"), Some(&None), "history 为空 = 没测过 = None");
        assert_eq!(map.get("首尔-01"), Some(&None), "alive=false 一律 None（哪怕历史里有数）");

        // 体检必须**先**发生（顺序），而且带上了我们要探测的那个上游端点。
        let reqs = server.received_requests().await.unwrap();
        assert!(reqs[0].url.path().ends_with("/healthcheck"), "第一个请求应是 healthcheck");
    }

    #[test]
    fn parse_provider_delays_never_invents_a_delay() {
        let body = json!({
            "proxies": [
                { "name": "没测过", "alive": true, "history": [] },
                { "name": "测失败", "alive": true, "history": [{ "time": "t", "delay": 0 }] },
                { "name": "不通", "alive": false, "history": [{ "time": "t", "delay": 40 }] },
                { "name": "通了", "alive": true, "history": [{ "time": "t", "delay": 40 }] }
            ]
        });
        let map = parse_provider_delays(&body, "sub-jp").unwrap();
        assert_eq!(map.get("没测过"), Some(&None));
        assert_eq!(map.get("测失败"), Some(&None));
        assert_eq!(map.get("不通"), Some(&None));
        assert_eq!(map.get("通了"), Some(&Some(40)));
        // 形态不符要报错，不能静默当成"全都测不到"（那会把一次解析错误说成"节点全不通"）。
        assert!(parse_provider_delays(&json!({ "proxies": 7 }), "sub-jp").is_err());
    }

    #[tokio::test]
    async fn snapshot_parses_real_shaped_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "proxies": {
                    "DIRECT": { "type": "Direct", "history": [], "udp": true },
                    "🇯🇵 日本": {
                        "type": "URLTest",
                        "now": "东京-03",
                        "all": ["东京-03", "大阪-01"],
                        "history": [
                            { "time": "2025-01-01T00:00:00.000Z", "delay": 88 },
                            { "time": "2025-01-01T00:01:00.000Z", "delay": 0 }
                        ]
                    },
                    "东京-03": {
                        "type": "Vmess",
                        "history": [{ "time": "2025-01-01T00:00:00.000Z", "delay": 120 }]
                    }
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/providers/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "providers": {
                    "sub-jp": {
                        "name": "机场A",
                        "vehicleType": "HTTP",
                        "updatedAt": "2025-01-01T00:00:00.000Z",
                        "proxies": [{"name": "东京-03"}, {"name": "大阪-01"}]
                    },
                    "sub-us": { "vehicleType": "HTTP", "proxies": [] }
                }
            })))
            .mount(&server)
            .await;

        let snap = rest(&server).snapshot().await.unwrap();
        // BTreeMap → 名字排序稳定：DIRECT < 东京-03 < 🇯🇵 日本（按 UTF-8 字节）
        assert_eq!(snap.proxies.len(), 3);
        let jp = snap.proxies.iter().find(|p| p.name == "🇯🇵 日本").unwrap();
        assert_eq!(jp.kind, "URLTest");
        assert_eq!(jp.now.as_deref(), Some("东京-03"));
        assert_eq!(jp.all, vec!["东京-03", "大阪-01"]);
        assert_eq!(jp.history.len(), 2);
        assert_eq!(jp.history[0].delay, 88);
        let direct = snap.proxies.iter().find(|p| p.name == "DIRECT").unwrap();
        assert!(direct.all.is_empty());

        assert_eq!(snap.providers.len(), 2);
        let p = snap.providers.iter().find(|p| p.name == "机场A").unwrap();
        assert_eq!(p.vehicle_type, "HTTP");
        assert_eq!(p.node_count, 2);
        assert_eq!(p.updated_at.as_deref(), Some("2025-01-01T00:00:00.000Z"));
        // 没有 name 时回退到 map key
        let p2 = snap.providers.iter().find(|p| p.name == "sub-us").unwrap();
        assert_eq!(p2.node_count, 0);
    }

    #[test]
    fn snapshot_tolerates_missing_fields_and_bad_shape() {
        let snap = parse_snapshot(
            &json!({ "proxies": { "x": { "type": "Select" } } }),
            &json!({ "providers": {} }),
        )
        .unwrap();
        assert_eq!(snap.proxies[0].kind, "Select");
        assert!(snap.proxies[0].now.is_none());
        assert!(snap.providers.is_empty());

        // 形态完全不对 → 报错而不是静默丢数据
        assert!(parse_snapshot(&json!([1, 2, 3]), &json!({})).is_err());
    }
}

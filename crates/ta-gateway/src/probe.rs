//! 面向 UI 的**受控出站探测**（GW-71 /[§6]）：测试连接、拉上游模型清单。
//!
//! # 为什么要有这一层，而不是让命令层自己发 HTTP
//!
//! `src-tauri` 不依赖 `reqwest`，而且 **唯一合规的出站通道是既有 lane 通道**
//! （`OutboundDialer` → 内核 listener → 线路策略）。命令层自己发请求 = 绕过 lane 策略，
//! 也就是"第二条出站路径" —— 那是硬约束里零容忍的事。所以这里把
//! "一次性上游请求 + **有界**读响应体" 做成原语，走的就是网关自己那条拨号通道。
//!
//! # 三条不许违反的规则
//!
//! 1. **不另开出站路径**：只经 [`OutboundDialer`]，与管线用同一个 dialer 实例。
//! 2. **响应体必须有上限**（[`MAX_PROBE_BODY_BYTES`]）：探测是给界面用的，一个畸形响应
//!    不许把内存吃掉。超限就**截断并如实标注**（[`FetchOutcome::truncated`]），
//!    不假装读到了完整体。
//! 3. **不猜不估算**：拿不到响应体、或体不是 OpenAI 的 `/models` 形状，就**如实报错**；
//!    **绝不**拿状态码去编一个"模型清单"（`list_upstream_models` 在任何非 2xx 上返回 `Err`）。
//!
//! # `probe` 这一档为什么重要
//!
//! `ok=false` **不等于**"服务商坏了"：`status=404 && probe="models"` 的语义是
//! "上游没有这个端点"（很多中转只实现 `/chat/completions`）。界面必须按 `probe` 分档说人话 ——
//! 把它渲染成"连接失败"会让用户去查网络，方向完全错了（契约注释）。

use std::sync::Arc;
use std::time::Instant;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::HeaderMap;
use ta_core::domain::{AuthConfig, ConfigDoc};
use ta_core::error::{Result, TaError};
use ta_core::types::{LaneId, ProviderId};

use crate::dispatch::{ByteStream, OutboundDialer, UpstreamRequest};
use crate::passthrough;

/// 探测响应体的上限（256 KiB）。超过就**截断并标注**，绝不无界攒 big body。
pub const MAX_PROBE_BODY_BYTES: usize = 256 * 1024;

/// 探测时用的总超时（秒）。探测是给界面点的，卡住比失败更糟。
pub const PROBE_TIMEOUT_S: u32 = 15;

/// note 里带上游原文时的截断长度（上游原文只作为 message，不进结构化字段）。
const NOTE_EXCERPT_CHARS: usize = 200;

/// 这次探测**实际发出去的是什么**（取值与 `src/shared/bindings.ts` 的 `ProbeResult.probe` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// `GET {base_url}/models`
    Models,
    /// `POST {base_url}/chat/completions` 的最小探测（有些中转没有 `/models`）
    Chat,
    /// **一次请求都没发出去**（线路不可用 / 连不通）
    None,
}

impl ProbeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProbeKind::Models => "models",
            ProbeKind::Chat => "chat",
            ProbeKind::None => "none",
        }
    }
}

/// 探测结果（字段与 `ProbeResult` 契约一一对应）。
///
/// `status` 在一次请求都没发出去时是 **0**（不是"编一个 200"）；`latency_ms` 是**真实测量**
/// （单调时钟），失败路径测的是"到失败为止"的真实耗时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u32,
    pub probe: ProbeKind,
    pub note: Option<String>,
    /// 响应体是否被上限截断（`true` 时 `note` 会写清）
    pub truncated: bool,
}

/// 一次受控请求的原始结果（读 body 的上限在这里生效）。
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub truncated: bool,
    /// 拿到响应头的真实耗时（毫秒，单调时钟）
    pub latency_ms: u32,
}

/// `/models` 端点路径（拼在 base_url 后面）。
pub const MODELS_PATH: &str = "/models";

/// `/chat/completions` 端点路径（`/models` 不可用时的最小 chat 探测）。
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// [`UpstreamProbe::probe_provider`] 的入参。
pub struct ProbeProviderRequest<'a> {
    /// 走哪条 lane（由 [`probe_lane`] 从配置解析，或调用方显式指定）
    pub lane: &'a LaneId,
    /// 服务商 base_url（`/models` 拼在它后面，拼接走网关同一份 `join_upstream_url`）
    pub base_url: &'a str,
    /// 发出去的头（鉴权头用 [`passthrough::auth_headers`]，与转发路径同一份实现）
    pub headers: HeaderMap,
    /// `provider.auth.scheme == query` 时把密钥追加到 URL —— 同样走转发路径那份
    /// [`passthrough::apply_query_auth`]。不传 = 那条腿的鉴权不在 URL 上。
    ///
    /// 为什么要有这个字段：`auth_headers` 对 query 鉴权是**故意什么都不写**的
    /// （见 `passthrough::rewrite_auth`），只带 headers 去探测，出来的一定是 401/403，
    /// 用户会以为密钥错了。
    pub query_auth: Option<(&'a AuthConfig, &'a str)>,
    /// 可选：`/models` 不可用时，用它做一次**最小 chat 探测**（不发就不传）
    pub model: Option<&'a str>,
}

/// 上游 `/models` 里的一条：**清单身份**（`id`）+ 上游原样返回的那个 JSON 对象。
///
/// `raw` 存在的理由：线路上记的模型信息 = 扫描时把上游那一条**逐字**记下来 ——
/// 能读到的字段存，读不到的就没有。只留 `id` 会让"上游当时到底给了什么"永久丢失，
/// 之后要么编字段、要么再拉一次；两者都不行。
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamModelEntry {
    /// 清单身份（`data[i].id`，已 `trim`）
    pub id: String,
    /// `data[i]` 的**原样对象**（含我们没建模的字段：`object` / `owned_by` / `created` /
    /// `permission` / 上游自定义字段…），一字不改。
    /// workspace 的 `serde_json` 开了 `preserve_order`，所以**键序**也保留。
    pub raw: serde_json::Value,
}

/// 受控出站探测。**持有的是网关那一个 `OutboundDialer`**，没有别的出站方式。
pub struct UpstreamProbe {
    dialer: Arc<dyn OutboundDialer>,
    body_limit: usize,
    timeout_s: u32,
}

impl UpstreamProbe {
    pub fn new(dialer: Arc<dyn OutboundDialer>) -> Self {
        Self { dialer, body_limit: MAX_PROBE_BODY_BYTES, timeout_s: PROBE_TIMEOUT_S }
    }

    /// 改响应体上限（测试用它证明"超限会截断"，而不是让上限变成一句口号）。
    pub fn with_body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = bytes;
        self
    }

    pub fn with_timeout_s(mut self, seconds: u32) -> Self {
        self.timeout_s = seconds;
        self
    }

    /// **受控的一次性上游请求**：经既有 dialer 发出，body **有界**读回。
    ///
    /// 拿不到响应体（流中断）→ `Err`：调用方只能如实报错，拿不到半个体去猜。
    pub async fn fetch(
        &self,
        lane: &LaneId,
        method: &str,
        url: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<FetchOutcome> {
        let req = UpstreamRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body,
            // 探测要的是"完整但有限的一段体"，不是流式透传
            stream: false,
            total_timeout_s: Some(self.timeout_s),
        };
        let started = Instant::now();
        let resp = self.dialer.execute(lane, req).await?;
        // 延迟只算到"拿到响应头"（TTFB）：界面上的"延迟 N ms"就该是这一件事的耗时，
        // 把读体也计进去会让一个 8 MB 的清单看起来像"网络很慢"。
        let latency_ms = elapsed_ms(started);
        let status = resp.status.as_u16();
        let headers = resp.headers;
        let (body, truncated) = read_body_bounded(resp.body, self.body_limit).await?;
        Ok(FetchOutcome { status, headers, body, truncated, latency_ms })
    }

    /// 测试连接。语义见：
    /// 2xx → `ok=true, probe="models"`；**404/405/403 → `probe="models", ok=false`**（"上游没有这个端点"，
    /// 不是"连接失败"）；连不通 → `probe="none"`；给了 `model` 且 `/models` 不可用 → 退一步做最小 chat 探测。
    pub async fn probe_provider(&self, req: ProbeProviderRequest<'_>) -> ProbeResult {
        let models_url = self.url_for(&req, MODELS_PATH);
        let started = Instant::now();
        let outcome =
            match self.fetch(req.lane, "GET", &models_url, req.headers.clone(), Bytes::new()).await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    // 一个字节的响应都没拿到 —— 这才是"连不通"。probe 必须是 "none"：
                    // 报成 "models" 会让界面说"上游没有 /models 端点"，方向完全错。
                    return ProbeResult {
                        ok: false,
                        status: 0,
                        latency_ms: elapsed_ms(started),
                        probe: ProbeKind::None,
                        note: Some(format!("连不通这条线路（网络不可达/线路不可用）：{err}")),
                        truncated: false,
                    };
                }
            };

        if (200..300).contains(&outcome.status) {
            // 2xx = 端点存在、连接可用。响应体形状只影响 note：通就是通，
            // 不能因为形状不认识就把它报成"连不通"。
            let shape_ok = parse_model_entries(&outcome.body).is_some();
            let note = if outcome.truncated {
                Some(format!(
                    "连接是通的，但响应体超过 {} 字节上限已截断，清单不完整",
                    self.body_limit
                ))
            } else if !shape_ok {
                Some(format!(
                    "HTTP {} 通了，但响应体不是 OpenAI 的模型清单形状（data[].id）。{}",
                    outcome.status,
                    excerpt_text(&outcome.body)
                ))
            } else {
                None
            };
            return ProbeResult {
                ok: true,
                status: outcome.status,
                latency_ms: outcome.latency_ms,
                probe: ProbeKind::Models,
                note,
                truncated: outcome.truncated,
            };
        }

        // 端点不存在这一档（403 权限不足 / 404 路由不存在 / 405 方法不允许 —— 三种都表示
        // "这条腿上拿不到清单"）：给了模型名就退一步做最小 chat 探测（很多中转只有 chat）。
        // 注意 `probe` 说的是"实际发的是哪条腿"，403 不是"连不通"。
        if matches!(outcome.status, 403..=405) {
            if let Some(model) = req.model {
                return self.probe_chat(&req, model).await;
            }
        }

        ProbeResult {
            ok: false,
            status: outcome.status,
            latency_ms: outcome.latency_ms,
            probe: ProbeKind::Models,
            note: Some(models_note(&outcome)),
            truncated: outcome.truncated,
        }
    }

    /// 拉上游模型清单：只认 OpenAI 的 `data[].id`；**任何非 2xx / 解析失败都返回 `Err`**。
    ///
    /// **绝不拿状态码编清单**：非 2xx 一律 `Err`，`data` 形状不对也 `Err`，
    /// 体被上限截断同样 `Err`（半份清单比报错更坏：用户会以为"上游就这些模型"）。
    ///
    /// 这版把每条的**原文对象**一起带回来（：扫描要逐字记录上游那一条模型对象）。
    /// 只要名字的调用方用 [`Self::list_upstream_models`]（它是本方法的**薄包装**）——
    /// 解析实现只有一份，测试钉住的也是同一份行为。
    pub async fn list_upstream_models_with_raw(
        &self,
        lane: &LaneId,
        base_url: &str,
        headers: HeaderMap,
        query_auth: Option<(&AuthConfig, &str)>,
    ) -> Result<Vec<UpstreamModelEntry>> {
        let url =
            url_with_auth(passthrough::join_upstream_url(base_url, MODELS_PATH, None), query_auth);
        let outcome = self.fetch(lane, "GET", &url, headers, Bytes::new()).await?;

        if !(200..300).contains(&outcome.status) {
            let message = format!("拉取上游模型清单失败：{}", models_note(&outcome));
            return Err(match outcome.status {
                // 端点根本不存在 / 没权限（403/404/405）：**不可重试**的错。用
                // AllChannelsFailed(502, retryable) 会让调用方对着一个不存在的端点反复重试。
                403..=405 => TaError::MalformedRequest(message),
                _ => TaError::AllChannelsFailed { message },
            });
        }
        if outcome.truncated {
            return Err(TaError::AllChannelsFailed {
                message: format!(
                    "上游 /models 响应体超过 {} 字节上限已截断，不能当作完整模型清单",
                    self.body_limit
                ),
            });
        }
        parse_model_entries(&outcome.body).ok_or_else(|| TaError::AllChannelsFailed {
            message: format!(
                "上游 /models 返回 HTTP {}，但响应体不是 {{ data: [{{ id }}] }} 形状，不敢猜清单。{}",
                outcome.status,
                excerpt_text(&outcome.body)
            ),
        })
    }

    /// 只要模型名的版本（与 [`Self::list_upstream_models_with_raw`] 同一条腿、同一套错误口径）。
    pub async fn list_upstream_models(
        &self,
        lane: &LaneId,
        base_url: &str,
        headers: HeaderMap,
        query_auth: Option<(&AuthConfig, &str)>,
    ) -> Result<Vec<String>> {
        Ok(self
            .list_upstream_models_with_raw(lane, base_url, headers, query_auth)
            .await?
            .into_iter()
            .map(|entry| entry.id)
            .collect())
    }

    /// 拼出这次探测要打的 URL（版本段重叠处理 + query 鉴权，都是转发路径那两份实现）。
    fn url_for(&self, req: &ProbeProviderRequest<'_>, path: &str) -> String {
        url_with_auth(passthrough::join_upstream_url(req.base_url, path, None), req.query_auth)
    }

    /// `/models` 不可用时的最后一步：`POST {base_url}/chat/completions`，`max_tokens: 1`。
    ///
    /// 只发一个最小请求 —— 这一步的花费必须接近零，否则"测试连接"会变成偷偷花钱。
    async fn probe_chat(&self, req: &ProbeProviderRequest<'_>, model: &str) -> ProbeResult {
        let url = self.url_for(req, CHAT_COMPLETIONS_PATH);
        let payload = serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
        });
        let body = Bytes::from(serde_json::to_vec(&payload).expect("探测请求体是字面量 JSON"));
        let mut headers = req.headers.clone();
        headers
            .insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
        passthrough::set_content_length(&mut headers, body.len());

        let started = Instant::now();
        match self.fetch(req.lane, "POST", &url, headers, body).await {
            Ok(outcome) if (200..300).contains(&outcome.status) => {
                let note = if outcome.truncated {
                    Some(format!("chat 探测通了，但响应体超过 {} 字节上限已截断", self.body_limit))
                } else if serde_json::from_slice::<serde_json::Value>(&outcome.body).is_err() {
                    Some(format!(
                        "chat 探测 HTTP {} 通了，但响应体不是 JSON。{}",
                        outcome.status,
                        excerpt_text(&outcome.body)
                    ))
                } else {
                    None
                };
                ProbeResult {
                    ok: true,
                    status: outcome.status,
                    latency_ms: outcome.latency_ms,
                    probe: ProbeKind::Chat,
                    note,
                    truncated: outcome.truncated,
                }
            }
            Ok(outcome) => ProbeResult {
                ok: false,
                status: outcome.status,
                latency_ms: outcome.latency_ms,
                probe: ProbeKind::Chat,
                note: Some(format!(
                    "/models 不可用，改用最小 chat 探测；上游返回 HTTP {}。{}",
                    outcome.status,
                    excerpt_text(&outcome.body)
                )),
                truncated: outcome.truncated,
            },
            Err(err) => ProbeResult {
                ok: false,
                // chat 这条腿确实发出去了（/models 那次拿到了 404），所以 probe 是 "chat" 不是 "none"；
                // 但没拿到任何响应，status 只能是 0。
                status: 0,
                latency_ms: elapsed_ms(started),
                probe: ProbeKind::Chat,
                note: Some(format!("/models 不可用，改用最小 chat 探测，但没拿到响应：{err}")),
                truncated: false,
            },
        }
    }
}

/// 把 query 鉴权追加到 URL（`auth.scheme` 不是 query 时原样返回）。
fn url_with_auth(url: String, query_auth: Option<(&AuthConfig, &str)>) -> String {
    match query_auth {
        Some((auth, secret)) => passthrough::apply_query_auth(&url, auth, secret),
        None => url,
    }
}

/// 从 OpenAI 形状的清单体里取**每条**（`data[i].id` + 那条的**原文对象**），
/// **按出现顺序去重**（重复 id 只留**首次出现**的那条 `raw`）。
///
/// 不宽松兜底：`data` 不在、不是数组、或数组里有条目缺少非空字符串 `id` → `None`
/// （"形状不对"必须与"上游一个模型都没有"（`data: []` → `Some(vec![])`）分开）。
fn parse_model_entries(body: &Bytes) -> Option<Vec<UpstreamModelEntry>> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let data = value.get("data")?.as_array()?;
    let mut out: Vec<UpstreamModelEntry> = Vec::new();
    for item in data {
        let id = item.get("id")?.as_str()?.trim();
        if id.is_empty() {
            return None;
        }
        if !out.iter().any(|existing| existing.id == id) {
            // `item` 就是 `data[i]` 本身 —— 不重建对象，字段与键序都不会被我们动过
            out.push(UpstreamModelEntry { id: id.to_string(), raw: item.clone() });
        }
    }
    Some(out)
}

/// `/models` 非 2xx 的人话 note：**按档说清是哪一类**（这一档错了，用户就会去查错方向）。
fn models_note(outcome: &FetchOutcome) -> String {
    let excerpt = excerpt_text(&outcome.body);
    match outcome.status {
        403 => format!(
            "上游没有可用的 /models 端点（HTTP 403：多半是这把密钥没有清单权限）—— 不是连不通。{excerpt}"
        ),
        404 | 405 => format!(
            "上游没有 /models 端点（HTTP {}）：这类中转通常只实现 /chat/completions —— 不是连不通。{excerpt}",
            outcome.status
        ),
        500..=599 => format!(
            "上游 /models 自己出错了（HTTP {}）：服务商侧的故障，不是本地线路问题。{excerpt}",
            outcome.status
        ),
        status => format!("上游 /models 返回 HTTP {status}。{excerpt}"),
    }
}

fn excerpt_text(body: &Bytes) -> String {
    match body_excerpt(body) {
        Some(excerpt) => format!("上游原文：{excerpt}"),
        None => String::new(),
    }
}

/// 有界读取一个字节流：到 `limit` 就**停止读取**并返回 `truncated=true`。
///
/// 这是"可以看，不能无界攒"的唯一实现：网关的非流式路径（`pipeline::collect_body`）
/// 与探测路径都走它，所以"上限"不会在两处各写一套而漂移。
pub async fn read_body_bounded(mut stream: ByteStream, limit: usize) -> Result<(Bytes, bool)> {
    let mut buf = BytesMut::new();
    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(err) => {
                return Err(TaError::AllChannelsFailed {
                    message: format!("读取上游响应体失败：{err}"),
                })
            }
        };
        if buf.len() + chunk.len() > limit {
            buf.extend_from_slice(&chunk[..limit.saturating_sub(buf.len())]);
            return Ok((buf.freeze(), true));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((buf.freeze(), false))
}

/// 这次探测该走哪条 lane：**provider.lane → net.default_lane → direct**。
///
/// 与 `ChannelConfig::effective_lane` 的 provider 段同序；探测没有 channel/credential，
/// 所以只能从 provider 这一段起算（这也是向导第 1 步的实际上下文）。
pub fn probe_lane(cfg: &ConfigDoc, provider: &ProviderId) -> LaneId {
    cfg.provider(provider.as_str())
        .and_then(|p| p.lane.clone())
        .or_else(|| cfg.net.default_lane.clone())
        .unwrap_or_else(|| LaneId::new("direct"))
}

/// 上游原文摘一段进 note（**只作为 message**，不进任何结构化字段）。
pub fn body_excerpt(body: &Bytes) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut out: String = collapsed.chars().take(NOTE_EXCERPT_CHARS).collect();
    if collapsed.chars().count() > NOTE_EXCERPT_CHARS {
        out.push('…');
    }
    Some(out)
}

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis() as u32
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use crate::dispatch::{
        byte_stream_from_bytes, BoxError, DialerOptions, KernelDialer, StaticLanePorts,
    };

    // ---------------------------------------------------------------- 假上游

    /// 一条脚本化的响应。
    #[derive(Clone)]
    struct Reply {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: String,
        delay_ms: u64,
    }

    impl Reply {
        fn json(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                reason: "OK",
                content_type: "application/json",
                body: body.into(),
                delay_ms: 0,
            }
        }

        fn status(status: u16) -> Self {
            Self {
                status,
                reason: "Error",
                content_type: "text/plain",
                body: format!("upstream says {status}"),
                delay_ms: 0,
            }
        }

        fn delayed(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
    }

    /// 上游实际收到的请求（头 + 体）——探测发出去什么，必须能被逐字节核对。
    struct SeenRequest {
        head: String,
        body: String,
    }

    impl SeenRequest {
        fn line(&self) -> &str {
            self.head.lines().next().unwrap_or_default()
        }

        fn has_header(&self, name_values: &str) -> bool {
            self.head.to_ascii_lowercase().contains(&name_values.to_ascii_lowercase())
        }
    }

    /// **真的在本机监听一个 HTTP 上游**：探测是网络代码，用 mock dialer 自证等于什么都没测。
    struct MockUpstream {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<SeenRequest>>>,
        connections: Arc<AtomicU32>,
    }

    impl MockUpstream {
        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn base_url_with_version(&self) -> String {
            format!("http://{}/v1", self.addr)
        }

        fn connection_count(&self) -> u32 {
            self.connections.load(Ordering::SeqCst)
        }

        fn seen(&self) -> Vec<SeenRequest> {
            let mut out = Vec::new();
            for request in self.seen.lock().expect("seen 锁").iter() {
                out.push(SeenRequest { head: request.head.clone(), body: request.body.clone() });
            }
            out
        }
    }

    async fn spawn_mock(replies: Vec<Reply>) -> MockUpstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假上游");
        let addr = listener.local_addr().expect("本地地址");
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let seen: Arc<Mutex<Vec<SeenRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicU32::new(0));

        // 服务端各拿一份 clone，宿主结构体留着给断言用
        let server_queue = Arc::clone(&queue);
        let server_seen = Arc::clone(&seen);
        let server_connections = Arc::clone(&connections);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                server_connections.fetch_add(1, Ordering::SeqCst);
                let (head, body) = read_whole_request(&mut socket).await;
                server_seen.lock().expect("seen 锁").push(SeenRequest { head, body });

                let reply =
                    server_queue.lock().expect("队列锁").pop_front().unwrap_or(Reply::status(500));
                if reply.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
                }
                let head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.status,
                    reply.reason,
                    reply.content_type,
                    reply.body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(reply.body.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });

        MockUpstream { addr, seen, connections }
    }

    /// 读到完整请求（头 + 按 content-length 的体），供断言"发出去的是什么"。
    async fn read_whole_request(socket: &mut TcpStream) -> (String, String) {
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; 4096];
        let head_end = loop {
            let n = socket.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return (String::from_utf8_lossy(&acc).into_owned(), String::new());
            }
            acc.extend_from_slice(&buf[..n]);
            if let Some(pos) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&acc[..head_end]).into_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut body = acc[head_end..].to_vec();
        while body.len() < content_length {
            let n = socket.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        body.truncate(content_length);
        (head, String::from_utf8_lossy(&body).into_owned())
    }

    // ---------------------------------------------------------------- 夹具

    fn direct_lane() -> LaneId {
        LaneId::new("direct")
    }

    fn dialer() -> Arc<dyn OutboundDialer> {
        Arc::new(KernelDialer::new(
            Arc::new(StaticLanePorts::default()),
            DialerOptions { connect_timeout_s: 2, read_timeout_s: 5 },
        ))
    }

    /// 探测永远拿同一个 dialer —— 没有第二个出口可以选。
    fn probe() -> UpstreamProbe {
        UpstreamProbe::new(dialer())
    }

    fn models_body(ids: &[&str]) -> String {
        let data: Vec<serde_json::Value> =
            ids.iter().map(|id| serde_json::json!({ "id": id, "object": "model" })).collect();
        serde_json::json!({ "object": "list", "data": data }).to_string()
    }

    fn request<'a>(
        lane: &'a LaneId,
        base_url: &'a str,
        model: Option<&'a str>,
    ) -> ProbeProviderRequest<'a> {
        ProbeProviderRequest { lane, base_url, headers: HeaderMap::new(), query_auth: None, model }
    }

    fn cfg(json: &str) -> ConfigDoc {
        serde_json::from_str(json).expect("测试配置必须能解析")
    }

    // ---------------------------------------------------------------- 有界读

    #[tokio::test]
    async fn bounded_reader_stops_at_the_limit_and_says_so() {
        let stream = byte_stream_from_bytes(Bytes::from_static(b"0123456789"));
        let (body, truncated) = read_body_bounded(stream, 4).await.expect("读得到");
        assert_eq!(&body[..], b"0123", "超限必须在上限处停住");
        assert!(truncated, "截断了却不说 = 把半个体当完整体");
    }

    #[tokio::test]
    async fn bounded_reader_does_not_flag_a_body_that_fits() {
        let stream = byte_stream_from_bytes(Bytes::from_static(b"0123"));
        let (body, truncated) = read_body_bounded(stream, 4).await.expect("读得到");
        assert_eq!(&body[..], b"0123");
        assert!(!truncated, "刚好等于上限不算截断");
    }

    #[tokio::test]
    async fn bounded_reader_reports_a_broken_stream_instead_of_half_a_body() {
        let stream: ByteStream = Box::pin(futures_util::stream::iter(vec![
            Ok(Bytes::from_static(b"half")),
            Err(Box::new(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "上游掐断了"))
                as BoxError),
        ]));
        let err = read_body_bounded(stream, 1024).await.expect_err("断流不能当成功");
        assert_eq!(err.code(), "TA-2005");
        assert!(format!("{err}").contains("读取上游响应体失败"));
    }

    // ---------------------------------------------------------------- probe_provider

    #[tokio::test]
    async fn models_endpoint_2xx_is_ok_and_uses_the_models_lane() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1", "m-2"]))]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;

        assert!(result.ok, "{result:?}");
        assert_eq!(result.status, 200);
        assert_eq!(result.probe, ProbeKind::Models);
        assert_eq!(result.probe.as_str(), "models", "契约里 probe 的取值就是这个字符串");
        assert!(result.note.is_none(), "通了就不该编一个 note：{:?}", result.note);
        assert!(!result.truncated);
        assert!(upstream.seen()[0].line().starts_with("GET /models"));
    }

    #[tokio::test]
    async fn probe_hits_the_base_url_path_prefix_not_the_root() {
        // base_url 带 /v1（真实服务商都这样）时，探测不许把前缀吃掉打到 https://host/models。
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let base = upstream.base_url_with_version();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;
        assert!(result.ok, "{result:?}");
        assert_eq!(upstream.seen()[0].line(), "GET /v1/models HTTP/1.1");
    }

    #[tokio::test]
    async fn latency_is_measured_from_the_clock_not_hardcoded() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"])).delayed(120)]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;
        assert!(result.ok, "{result:?}");
        assert!(
            result.latency_ms >= 100,
            "真延迟 120ms，报出来 {}ms 说明这个数是编的",
            result.latency_ms
        );
    }

    #[tokio::test]
    async fn missing_models_endpoint_is_not_a_connection_failure() {
        let upstream = spawn_mock(vec![Reply {
            status: 404,
            reason: "Not Found",
            content_type: "application/json",
            body: r#"{"error":{"message":"no such route"}}"#.to_string(),
            delay_ms: 0,
        }])
        .await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;

        assert!(!result.ok);
        assert_eq!(result.status, 404);
        assert_eq!(result.probe, ProbeKind::Models, "请求发出去了、上游答了 404 —— 这档不是 none");
        let note = result.note.unwrap_or_default();
        assert!(note.contains("没有 /models 端点"), "note 必须说清是哪一档：{note}");
        assert!(note.contains("HTTP 404"), "{note}");
        assert!(note.contains("不是连不通"), "{note}");
        assert!(note.contains("no such route"), "上游原文要带出来：{note}");
    }

    #[tokio::test]
    async fn upstream_5xx_blames_the_upstream_not_the_local_lane() {
        let upstream = spawn_mock(vec![Reply::status(500)]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;
        assert!(!result.ok);
        assert_eq!(result.status, 500);
        assert_eq!(result.probe, ProbeKind::Models);
        let note = result.note.unwrap_or_default();
        assert!(note.contains("服务商侧"), "{note}");
    }

    #[tokio::test]
    async fn unreachable_lane_reports_probe_none_with_status_zero() {
        // 非 direct lane 且没有端口 = 请求根本发不出去（不是"上游没有端点"）
        let lane = LaneId::new("lane-jp");
        let result = probe().probe_provider(request(&lane, "http://127.0.0.1:1", None)).await;
        assert!(!result.ok);
        assert_eq!(result.status, 0, "一个响应都没拿到，status 只能是 0");
        assert_eq!(result.probe, ProbeKind::None);
        let note = result.note.unwrap_or_default();
        assert!(note.contains("连不通"), "{note}");
        assert!(!note.contains("没有 /models 端点"), "连不通不能报成'没有端点'：{note}");
    }

    #[tokio::test]
    async fn two_xx_with_an_unexpected_shape_is_still_ok_but_says_so() {
        let upstream = spawn_mock(vec![Reply::json(200, "<html>not a model list</html>")]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;
        assert!(result.ok, "通了就是通了：{}", result.note.unwrap_or_default());
        assert_eq!(result.probe, ProbeKind::Models);
        let note = result.note.unwrap_or_default();
        assert!(note.contains("不是 OpenAI 的模型清单形状"), "{note}");
        assert!(note.contains("not a model list"), "要带上游原文：{note}");
    }

    #[tokio::test]
    async fn oversized_body_is_marked_truncated_not_reported_as_complete() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let base = upstream.base_url();
        let result =
            probe().with_body_limit(8).probe_provider(request(&direct_lane(), &base, None)).await;
        assert!(result.truncated, "{result:?}");
        assert!(result.ok, "截断不等于连不通");
        let note = result.note.unwrap_or_default();
        assert!(note.contains("已截断"), "{note}");
    }

    #[tokio::test]
    async fn chat_fallback_runs_when_models_is_missing_and_a_model_was_given() {
        let chat = r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"pong"}}]}"#;
        let upstream = spawn_mock(vec![Reply::status(404), Reply::json(200, chat)]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, Some("m-1"))).await;

        assert_eq!(upstream.connection_count(), 2, "第一次 /models 挂了，第二次才该是 chat");
        assert!(result.ok, "{result:?}");
        assert_eq!(result.probe, ProbeKind::Chat, "实际发的是 chat 这一腿");
        assert_eq!(result.status, 200);

        let seen = upstream.seen();
        let second = &seen[1];
        assert!(second.line().starts_with("POST /chat/completions"), "{}", second.line());
        assert!(second.has_header("content-type: application/json"), "{}", second.head);
        let body: serde_json::Value = serde_json::from_str(&second.body).expect("探测体是 JSON");
        assert_eq!(body["model"], "m-1");
        assert_eq!(body["max_tokens"], 1, "最小探测：max_tokens 必须是 1");
        assert!(body["messages"].is_array());
    }

    #[tokio::test]
    async fn chat_fallback_is_not_fired_when_models_works() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, Some("m-1"))).await;
        assert_eq!(result.probe, ProbeKind::Models);
        assert_eq!(upstream.connection_count(), 1, "通了就不要再多花一次 chat 请求");
    }

    #[tokio::test]
    async fn chat_fallback_without_a_model_name_does_not_guess_one() {
        let upstream = spawn_mock(vec![Reply::status(404)]).await;
        let base = upstream.base_url();
        let result = probe().probe_provider(request(&direct_lane(), &base, None)).await;
        assert_eq!(upstream.connection_count(), 1, "没有模型名就没有 chat 可发");
        assert_eq!(result.probe, ProbeKind::Models);
        assert!(!result.ok);
    }

    #[tokio::test]
    async fn query_auth_is_put_in_the_url_because_headers_cannot_carry_it() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let base = upstream.base_url();
        let config = cfg(
            r#"{"providers":[{"id":"p-gemini","name":"G","protocol":"gemini","base_url":"http://x","auth":{"scheme":"query","header":"key"}}]}"#,
        );
        let provider = config.provider("p-gemini").expect("夹具里的服务商");
        let lane = direct_lane();
        let mut req = request(&lane, &base, None);
        req.query_auth = Some((&provider.auth, "sk-a b"));
        let result = probe().probe_provider(req).await;

        assert!(result.ok, "{result:?}");
        let line = upstream.seen()[0].line().to_string();
        assert!(line.starts_with("GET /models?key=sk-a%20b"), "{line}");
        assert!(!upstream.seen()[0].has_header("authorization:"), "query 鉴权不该再写一个头");
    }

    #[tokio::test]
    async fn the_probe_sends_the_same_auth_header_the_forwarding_path_sends() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1"]))]).await;
        let base = upstream.base_url();
        let config = cfg(
            r#"{"providers":[{"id":"p1","name":"P","protocol":"openai","base_url":"http://x"}]}"#,
        );
        let provider = config.provider("p1").expect("夹具里的服务商");
        let lane = direct_lane();
        let mut req = request(&lane, &base, None);
        req.headers = passthrough::auth_headers(Some(provider), "sk-secret");
        let result = probe().probe_provider(req).await;

        assert!(result.ok, "{result:?}");
        assert!(
            upstream.seen()[0].has_header("authorization: Bearer sk-secret"),
            "{}",
            upstream.seen()[0].head
        );
    }

    // ---------------------------------------------------------------- list_upstream_models

    #[tokio::test]
    async fn model_list_comes_from_data_ids_in_order_and_deduped() {
        let body = models_body(&["b-model", "a-model", "b-model", "c-model"]);
        let upstream = spawn_mock(vec![Reply::json(200, body)]).await;
        let base = upstream.base_url();
        let ids = probe()
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect("2xx 形状正确就该出清单");
        assert_eq!(ids, vec!["b-model", "a-model", "c-model"], "顺序按上游出现顺序，重复要去掉");
    }

    ///：扫描要把上游那一条模型对象**逐字**记下来。
    /// 我们没建模的字段（`object` / `owned_by` / `created` / `permission` / 上游自定义字段）
    /// 一个都不许丢、不许改；`preserve_order` 开着时键序也要原样。
    #[tokio::test]
    async fn model_entries_keep_the_upstream_object_verbatim() {
        let body = r#"{"object":"list","data":[
            {"id":"m-1","object":"model","created":1700000000,"owned_by":"upstream-org","permission":[{"id":"perm-1"}],"custom_x":1},
            {"id":"m-2","object":"model","created":1700000001,"owned_by":"upstream-org"}
        ]}"#;
        let upstream = spawn_mock(vec![Reply::json(200, body)]).await;
        let base = upstream.base_url();
        let entries = probe()
            .list_upstream_models_with_raw(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect("2xx 形状正确就该出清单");

        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["m-1", "m-2"]);
        assert_eq!(
            entries[0].raw,
            serde_json::json!({
                "id": "m-1",
                "object": "model",
                "created": 1700000000,
                "owned_by": "upstream-org",
                "permission": [{"id": "perm-1"}],
                "custom_x": 1,
            }),
            "原文对象必须逐字段相同"
        );
        // 键序也保留（否则下次导出会把上游的原文重排成我们碰过的样子）
        assert_eq!(
            serde_json::to_string(&entries[0].raw).expect("能序列化"),
            r#"{"id":"m-1","object":"model","created":1700000000,"owned_by":"upstream-org","permission":[{"id":"perm-1"}],"custom_x":1}"#
        );
    }

    /// 去重留下的必须是**首次出现**的那条的原文（后来的同 id 条目不许覆盖它）。
    #[tokio::test]
    async fn duplicate_ids_keep_the_first_occurrence_and_its_object() {
        let body = r#"{"object":"list","data":[
            {"id":"b","object":"model","owned_by":"first"},
            {"id":"a","object":"model","owned_by":"only"},
            {"id":"b","object":"model","owned_by":"second"}
        ]}"#;
        let upstream = spawn_mock(vec![Reply::json(200, body)]).await;
        let base = upstream.base_url();
        let entries = probe()
            .list_upstream_models_with_raw(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect("2xx 形状正确就该出清单");

        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(
            entries[0].raw.get("owned_by").and_then(|v| v.as_str()),
            Some("first"),
            "同 id 重复时留下首次出现的那条原文"
        );
    }

    #[tokio::test]
    async fn an_empty_data_array_is_an_honest_empty_list_not_an_error() {
        let upstream = spawn_mock(vec![Reply::json(200, r#"{"object":"list","data":[]}"#)]).await;
        let base = upstream.base_url();
        let ids = probe()
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect("上游确实一个模型都没有，不该报错");
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn a_404_never_becomes_a_model_list() {
        let upstream = spawn_mock(vec![Reply::status(404)]).await;
        let base = upstream.base_url();
        let err = probe()
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect_err("404 绝不能变成清单");
        assert_eq!(err.code(), "TA-1004");
        assert!(!err.retryable(), "端点不存在不是可重试错误，重试一百次也还是不存在");
        let text = format!("{err}");
        assert!(text.contains("没有 /models 端点"), "{text}");
    }

    #[tokio::test]
    async fn a_5xx_never_becomes_a_model_list() {
        let upstream = spawn_mock(vec![Reply::status(503)]).await;
        let base = upstream.base_url();
        let err = probe()
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect_err("5xx 绝不能变成清单");
        assert_eq!(err.code(), "TA-2005");
        let text = format!("{err}");
        assert!(text.contains("服务商侧"), "{text}");
    }

    #[tokio::test]
    async fn an_unparseable_body_is_an_error_not_an_invented_list() {
        let upstream = spawn_mock(vec![Reply::json(200, r#"{"models":["gpt-4o"]}"#)]).await;
        let base = upstream.base_url();
        let err = probe()
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect_err("形状不对就不许猜");
        assert_eq!(err.code(), "TA-2005");
        assert!(format!("{err}").contains("不敢猜清单"));
    }

    #[tokio::test]
    async fn a_truncated_body_is_an_error_because_half_a_list_looks_like_the_whole_list() {
        let upstream = spawn_mock(vec![Reply::json(200, models_body(&["m-1", "m-2"]))]).await;
        let base = upstream.base_url();
        let err = probe()
            .with_body_limit(8)
            .list_upstream_models(&direct_lane(), &base, HeaderMap::new(), None)
            .await
            .expect_err("半份清单比报错更坏");
        assert_eq!(err.code(), "TA-2005");
        assert!(format!("{err}").contains("已截断"));
    }

    #[tokio::test]
    async fn a_transport_failure_propagates_instead_of_returning_an_empty_list() {
        let lane = LaneId::new("lane-jp");
        let err = probe()
            .list_upstream_models(&lane, "http://127.0.0.1:1", HeaderMap::new(), None)
            .await
            .expect_err("连不通不能返回空清单");
        assert_eq!(err.code(), "TA-3002");
    }

    // ---------------------------------------------------------------- lane 解析

    #[test]
    fn probe_lane_falls_back_to_direct_when_nothing_is_configured() {
        assert_eq!(probe_lane(&ConfigDoc::default(), &ProviderId::new("p1")).as_str(), "direct");
    }

    #[test]
    fn probe_lane_uses_net_default_then_provider_override() {
        let net_only = cfg(r#"{"net":{"default_lane":"lane-net"}}"#);
        assert_eq!(probe_lane(&net_only, &ProviderId::new("p1")).as_str(), "lane-net");

        let both = cfg(
            r#"{"net":{"default_lane":"lane-net"},"providers":[{"id":"p1","name":"P","protocol":"openai","base_url":"http://x","lane":"lane-p"}]}"#,
        );
        assert_eq!(
            probe_lane(&both, &ProviderId::new("p1")).as_str(),
            "lane-p",
            "provider.lane 必须压过 net.default_lane（与 resolve_lane 同序）"
        );
    }

    // ---------------------------------------------------------------- 硬约束守卫

    /// **不许有第二条出站路径**：探测只能经 `dialer.execute`。
    ///
    /// 这条守卫有牙的证据：在生产段里插一句 `reqwest::Client::new()` 就变红。
    #[test]
    fn production_slice_has_no_second_egress_path() {
        let source = include_str!("probe.rs");
        let production = source.split("#[cfg(test)]").next().expect("有生产段");

        for forbidden in ["reqwest::", "TcpStream", "TcpListener", "ureq::", "hyper::Client"] {
            assert!(
                !production.contains(forbidden),
                "探测的生产代码里出现了 `{forbidden}` —— 这就是绕过 lane 策略的第二条出站路径"
            );
        }
        assert!(
            production.contains("self.dialer.execute("),
            "探测必须经 `dialer.execute(lane, ...)` 出去（线路策略就在那后面）"
        );
    }
}

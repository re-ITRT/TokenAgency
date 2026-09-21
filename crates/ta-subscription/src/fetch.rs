//! 订阅下载（契约：[11 §3]）。
//!
//! ★ **独立 Client，不走 lane** —— 这是鸡生蛋问题：还没拿到订阅时，我们根本不知道
//! 有哪些节点可用，所以下载**必须**直连（或用户显式指定的引导代理）。
//! 结构上保证：本类型的构造函数**拿不到任何 lane / dialer**，只能拿到 [`FetchOptions`]。
//!
//! 契约要点（[11 §3]）：
//! * 请求头：`User-Agent` 来自 `subscription.user_agent`（默认 `clash-verge/v2.0.0`）、`Accept: */*`
//! * 超时：connect 10s / read 30s
//! * 重试 2 次（`backon` 指数退避，**有上限**）
//! * 响应解析：`Content-Disposition` 文件名只作**兜底提示** → 内容嗅探说了算（内容永远更可信）
//! * 响应头：`Subscription-Userinfo` → 流量/到期；`Profile-Update-Interval` → 建议周期
//! * 失败：任何失败**保留上版**（由调用方负责），这里只把原因带回去
//!
//! 两个现实坑，这里都处理了：
//! 1. **undeclared gzip**：个别机场不设 `Content-Encoding` 却发 gzip 字节 → 按 magic bytes 嗅探后解压；
//! 2. **HTML 错误页**（被墙/需登录/502）→ 判成 [`DetectedFormat::Html`] 而不是解析失败，
//!    交给 [`crate::validate`] 拒绝采纳（这样"为什么没采纳"有原因可显示，而不是一句"解析失败"）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use backon::{BackoffBuilder, ExponentialBuilder};
use ta_core::domain::{Subscription, SubscriptionFormat};
use ta_core::error::{Result, TaError};

use crate::{detect, parse, DetectedFormat, FetchOutcome, SubscriptionFetcher};

/// connect 超时（[11 §3]）
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 读超时（[11 §3]：订阅文件小，30s 足够）
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 总尝试次数 = 1 次首试 + 2 次重试（[11 §3]「重试 2 次」）
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// 退避基数
pub const DEFAULT_RETRY_MIN_DELAY: Duration = Duration::from_millis(500);
/// 退避上限：再糟也不会让用户干等更久
pub const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(8);
/// 订阅原文上限（防超大响应 / 解压炸弹）
pub const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;

/// 下载走哪条网络路径。
///
/// **默认 [`ProxyMode::Direct`]**：第一次拿订阅时内核还没起来、一个节点都没有，
/// 所以默认必须是直连；跟随系统/环境代理只在用户显式选择时启用。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProxyMode {
    /// 直连（默认）。**明确忽略**环境变量里的 `HTTP_PROXY` 等。
    #[default]
    Direct,
    /// 用户显式指定的引导代理，如 `http://127.0.0.1:7890`
    Explicit(String),
    /// 跟随环境变量（`HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY`）
    FromEnv,
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// 总尝试次数（含首次）
    pub max_attempts: u32,
    pub retry_min_delay: Duration,
    pub retry_max_delay: Duration,
    pub proxy: ProxyMode,
    pub max_bytes: usize,
    /// 允许的跳转次数（机场常见 302 到 CDN）
    pub max_redirects: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_min_delay: DEFAULT_RETRY_MIN_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
            proxy: ProxyMode::Direct,
            max_bytes: DEFAULT_MAX_BYTES,
            max_redirects: 5,
        }
    }
}

/// 退避睡眠。默认真 sleep；测试注入记录器，**一个字节的时间都不真等**。
pub trait Sleep: Send + Sync {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleep;

impl Sleep for TokioSleep {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(d))
    }
}

/// 一次尝试的失败。区分「可重试」与「重试也没用」，避免对 401/404 白等三遍。
#[derive(Debug, Clone)]
enum AttemptError {
    /// 连接失败 / 超时 / 5xx / 429：值得退避重试
    Transient(String),
    /// 4xx（除 429）/ URL 非法 / 响应过大：重试没意义
    Fatal(String),
}

impl AttemptError {
    fn is_transient(&self) -> bool {
        matches!(self, AttemptError::Transient(_))
    }

    fn message(&self) -> &str {
        match self {
            AttemptError::Transient(m) | AttemptError::Fatal(m) => m,
        }
    }
}

impl std::fmt::Display for AttemptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// 走 HTTP(S) 的订阅下载器。
///
/// 注意字段：**没有** lane / dialer / 内核句柄 —— 结构上不可能走到 lane 上去。
pub struct HttpSubscriptionFetcher {
    client: reqwest::Client,
    opts: FetchOptions,
    sleeper: Arc<dyn Sleep>,
}

impl std::fmt::Debug for HttpSubscriptionFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSubscriptionFetcher")
            .field("timeouts", &(self.opts.connect_timeout, self.opts.read_timeout))
            .field("max_attempts", &self.opts.max_attempts)
            .field("proxy", &self.opts.proxy)
            .finish()
    }
}

impl HttpSubscriptionFetcher {
    /// 默认配置：**直连、忽略环境代理**。
    pub fn new() -> Result<Self> {
        Self::with_options(FetchOptions::default())
    }

    pub fn with_options(opts: FetchOptions) -> Result<Self> {
        let client = Self::build_client(&opts)?;
        Ok(Self { client, opts, sleeper: Arc::new(TokioSleep) })
    }

    /// 注入退避睡眠（测试/自定义调度）
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleep>) -> Self {
        self.sleeper = sleeper;
        self
    }

    fn build_client(opts: &FetchOptions) -> Result<reqwest::Client> {
        // ★ 先 no_proxy()：**明确不读环境变量**，否则用户机器上恰好设了 HTTP_PROXY
        //   就会把"鸡生蛋"的第一次订阅请求穿过一个还不存在的代理。
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(opts.connect_timeout)
            .read_timeout(opts.read_timeout)
            .timeout(opts.read_timeout)
            .redirect(reqwest::redirect::Policy::limited(opts.max_redirects));
        match &opts.proxy {
            ProxyMode::Direct => {}
            ProxyMode::Explicit(url) => {
                let proxy = reqwest::Proxy::all(url)
                    .map_err(|e| TaError::Internal(format!("引导代理地址非法（{url}）：{e}")))?;
                b = b.proxy(proxy);
            }
            ProxyMode::FromEnv => {
                // 显式跟随环境变量
                b = reqwest::Client::builder()
                    .connect_timeout(opts.connect_timeout)
                    .read_timeout(opts.read_timeout)
                    .timeout(opts.read_timeout)
                    .redirect(reqwest::redirect::Policy::limited(opts.max_redirects));
            }
        }
        b.build().map_err(|e| TaError::Internal(format!("构建订阅下载客户端失败：{e}")))
    }

    pub fn options(&self) -> &FetchOptions {
        &self.opts
    }

    /// 结构自证：当前用的是哪条网络路径（UI/测试可读；`Direct` = 没配任何代理）
    pub fn proxy_mode(&self) -> &ProxyMode {
        &self.opts.proxy
    }

    /// 一次尝试：取响应头 + body（不做格式判定）
    async fn attempt(&self, sub: &Subscription) -> std::result::Result<RawResponse, AttemptError> {
        let ua = if sub.user_agent.trim().is_empty() {
            crate::DEFAULT_USER_AGENT
        } else {
            sub.user_agent.trim()
        };
        let mut resp = self
            .client
            .get(sub.url.trim())
            .header(reqwest::header::USER_AGENT, ua)
            .header(reqwest::header::ACCEPT, "*/*")
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AttemptError::Transient(format!("请求超时：{e}"))
                } else if e.is_connect() {
                    AttemptError::Transient(format!("连接失败：{e}"))
                } else if e.is_redirect() {
                    AttemptError::Transient(format!("跳转过多：{e}"))
                } else {
                    AttemptError::Transient(format!("请求失败：{e}"))
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let msg = format!("HTTP {status}");
            // 429/408 与 5xx 值得重试；其它 4xx 重试没意义
            return Err(
                if status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error() {
                    AttemptError::Transient(msg)
                } else {
                    AttemptError::Fatal(msg)
                },
            );
        }

        let headers = resp.headers().clone();
        let body = read_body(&mut resp, self.opts.max_bytes).await?;
        Ok(RawResponse { headers, body })
    }
}

struct RawResponse {
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

/// 流式读 body，超过上限立刻中止（不先把它全读进内存）
async fn read_body(
    resp: &mut reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, AttemptError> {
    if let Some(len) = resp.content_length() {
        if len > max_bytes as u64 {
            return Err(AttemptError::Fatal(format!(
                "订阅响应过大：{len} 字节 > 上限 {max_bytes} 字节"
            )));
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) =
        resp.chunk().await.map_err(|e| AttemptError::Transient(format!("读取响应体失败：{e}")))?
    {
        buf.extend_from_slice(&chunk);
        if buf.len() > max_bytes {
            return Err(AttemptError::Fatal(format!("订阅响应超过上限 {max_bytes} 字节，已中止")));
        }
    }
    Ok(buf)
}

#[async_trait]
impl SubscriptionFetcher for HttpSubscriptionFetcher {
    async fn fetch(&self, s: &Subscription) -> Result<FetchOutcome> {
        let attempts = self.opts.max_attempts.max(1);
        // 退避序列由 backon 生成（指数 + 上限）；循环自己驱动 —— 这样睡眠可注入，
        // 也不必把 `Retry` 的泛型/生命周期塞进 async_trait 的装箱 future 里。
        let mut backoff = ExponentialBuilder::default()
            .with_min_delay(self.opts.retry_min_delay)
            .with_max_delay(self.opts.retry_max_delay)
            .with_factor(2.0)
            .with_max_times(attempts.saturating_sub(1) as usize)
            .build();

        let mut last: Option<AttemptError> = None;
        for attempt in 1..=attempts {
            match self.attempt(s).await {
                Ok(response) => return Ok(build_outcome(s, response)),
                Err(e) if !e.is_transient() => {
                    tracing::warn!(subscription = %s.id, error = %e, "订阅下载失败（不可重试），保留上版");
                    return Err(fetch_error(&s.id, &e));
                }
                Err(e) => {
                    let Some(delay) = backoff.next() else {
                        last = Some(e);
                        break;
                    };
                    tracing::warn!(
                        subscription = %s.id,
                        attempt,
                        error = %e,
                        delay_ms = delay.as_millis() as u64,
                        "订阅下载失败，退避后重试"
                    );
                    last = Some(e);
                    self.sleeper.sleep(delay).await;
                }
            }
        }
        let e = last.unwrap_or_else(|| AttemptError::Transient("未发起任何请求".into()));
        tracing::warn!(subscription = %s.id, attempts, error = %e, "订阅下载最终失败，保留上版");
        Err(fetch_error(&s.id, &e))
    }
}

fn fetch_error(id: &ta_core::types::SubscriptionId, e: &AttemptError) -> TaError {
    TaError::Internal(format!("订阅 {id} 下载失败：{e}"))
}

/// 响应 → [`FetchOutcome`]（纯函数，便于单测）
fn build_outcome(sub: &Subscription, response: RawResponse) -> FetchOutcome {
    let filename_hint = response
        .headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse::filename_from_content_disposition);

    // undeclared gzip：个别机场不设 Content-Encoding 却发 gzip 字节
    let raw = match maybe_decompress(&response.body, DEFAULT_MAX_BYTES) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(subscription = %sub.id, error = %e, "响应解压失败，按原文处理");
            response.body
        }
    };

    let format = resolve_format(sub, &raw, filename_hint.as_deref());
    let extracted = parse::extract(&raw, format);
    let mut nodes = extracted.nodes;
    // 订阅级过滤（[04 §5.3] 第 4 条）：与下发给内核的 `proxy-providers[].filter` 保持一致，
    // 这样 UI 里看到的节点数就是内核真正会用的节点数。
    let removed = parse::apply_node_filter(&mut nodes, &sub.filter);
    if !removed.is_empty() {
        tracing::debug!(
            subscription = %sub.id,
            removed = removed.len(),
            names = ?removed.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            "订阅级 filter 剔除了假节点"
        );
    }

    FetchOutcome {
        raw,
        format,
        userinfo: response
            .headers
            .get("subscription-userinfo")
            .and_then(|v| v.to_str().ok())
            .and_then(parse::parse_userinfo),
        suggested_interval_s: response
            .headers
            .get("profile-update-interval")
            .and_then(|v| v.to_str().ok())
            .and_then(parse::parse_update_interval),
        nodes,
        groups: extracted.groups,
    }
}

/// 声明的 `format` 与内容冲突时，**内容赢**。
///
/// 唯一例外是 HTML：无论配置里声明了什么格式，HTML 都必须是 HTML —— 否则一份
/// 502 错误页会被当成"合法的空订阅"，那正是坏更新拦截要防的事。
fn resolve_format(sub: &Subscription, raw: &[u8], filename_hint: Option<&str>) -> DetectedFormat {
    if detect::looks_like_html(raw) {
        return DetectedFormat::Html;
    }
    match sub.format {
        SubscriptionFormat::Auto | SubscriptionFormat::ConvertLinks => {
            detect::detect_format_with_hint(raw, filename_hint)
        }
        SubscriptionFormat::Clash => DetectedFormat::ClashYaml,
        SubscriptionFormat::Base64Links => DetectedFormat::Base64Links,
        SubscriptionFormat::PlainLinks => DetectedFormat::PlainLinks,
    }
}

/// 按 magic bytes 判断是否需要解压。reqwest 只处理**声明了** `Content-Encoding` 的情况。
pub fn maybe_decompress(raw: &[u8], max_bytes: usize) -> Result<Vec<u8>> {
    use std::io::Read as _;
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        // take(max+1)：解压炸弹不会把我们撑爆
        flate2::read::GzDecoder::new(raw)
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| TaError::Internal(format!("gzip 解压失败：{e}")))?;
        if out.len() > max_bytes {
            return Err(TaError::Internal(format!("解压后超过上限 {max_bytes} 字节，已中止")));
        }
        return Ok(out);
    }
    // zlib（78 01/5e/9c/da 是常见的几个合法头）
    if raw.len() > 2 && raw[0] == 0x78 && matches!(raw[1], 0x01 | 0x5e | 0x9c | 0xda) {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(raw)
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| TaError::Internal(format!("zlib 解压失败：{e}")))?;
        if out.len() > max_bytes {
            return Err(TaError::Internal(format!("解压后超过上限 {max_bytes} 字节，已中止")));
        }
        return Ok(out);
    }
    Ok(raw.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const YAML: &str = "proxies:\n  - {name: \"🇭🇰 香港 01\", type: ss, server: hk01.example.com, port: 443}\n  - {name: \"🇯🇵 日本 01\", type: vmess, server: jp01.example.com, port: 443}\nproxy-groups:\n  - {name: \"♻️ 自动选择\", type: url-test, proxies: [\"🇭🇰 香港 01\"]}\n";

    fn sub_of(yaml: &str) -> Subscription {
        serde_yaml_ng::from_str(yaml).unwrap_or_else(|e| panic!("测试订阅配置应能解析：{e}"))
    }

    /// 基本订阅：只有 url/UA，其余用默认值
    fn sub_for(url: &str) -> Subscription {
        sub_of(&format!("id: sub-jp\nname: 机场A\nurl: \"{url}\"\n"))
    }

    /// 记录退避时长、不真等
    #[derive(Default)]
    struct RecordingSleep {
        taken: Mutex<Vec<Duration>>,
    }

    impl RecordingSleep {
        fn taken(&self) -> Vec<Duration> {
            self.taken.lock().unwrap().clone()
        }
    }

    impl Sleep for RecordingSleep {
        fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.taken.lock().unwrap().push(d);
            Box::pin(async {})
        }
    }

    /// 快测配置：退避很小、可注入
    fn fast_opts() -> FetchOptions {
        FetchOptions {
            retry_min_delay: Duration::from_millis(10),
            retry_max_delay: Duration::from_millis(50),
            ..Default::default()
        }
    }

    fn fetcher(opts: FetchOptions) -> (HttpSubscriptionFetcher, Arc<RecordingSleep>) {
        let sleeper = Arc::new(RecordingSleep::default());
        let f = HttpSubscriptionFetcher::with_options(opts).unwrap().with_sleeper(sleeper.clone());
        (f, sleeper)
    }

    /// 第 n 次请求才成功（前 n-1 次给 status）
    async fn server_failing_times(
        status: u16,
        fail_times: usize,
        body: &'static str,
    ) -> (MockServer, Arc<AtomicUsize>) {
        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        Mock::given(method("GET"))
            .respond_with(move |_req: &Request| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n < fail_times {
                    ResponseTemplate::new(status)
                } else {
                    ResponseTemplate::new(200).set_body_string(body)
                }
            })
            .mount(&server)
            .await;
        (server, hits)
    }

    // ------------------------------------------------------------ 正常路径

    #[tokio::test]
    async fn fetches_clash_yaml_with_headers_and_ua() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("user-agent", "clash-verge/v2.0.0"))
            .and(header("accept", "*/*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "Subscription-Userinfo",
                        "upload=1024; download=2048; total=107374182400; expire=1780000000",
                    )
                    .insert_header("Profile-Update-Interval", "12")
                    .set_body_string(YAML),
            )
            .mount(&server)
            .await;

        let (f, sleeper) = fetcher(fast_opts());
        let s = sub_for(&server.uri());
        let out = f.fetch(&s).await.unwrap();

        assert_eq!(out.format, DetectedFormat::ClashYaml);
        assert_eq!(out.node_count(), 2);
        assert_eq!(out.group_count(), 1);
        assert_eq!(out.nodes[0].name, "🇭🇰 香港 01");
        assert_eq!(out.nodes[1].server, "jp01.example.com");
        let ui = out.userinfo.unwrap();
        assert_eq!((ui.upload, ui.download, ui.total), (1024, 2048, 107_374_182_400));
        assert_eq!(ui.expire_ms, Some(1_780_000_000_000));
        // Profile-Update-Interval 是小时 → 秒
        assert_eq!(out.suggested_interval_s, Some(12 * 3600));
        assert!(out.format.is_usable());
        assert!(sleeper.taken().is_empty(), "成功路径不该退避");
    }

    #[tokio::test]
    async fn custom_user_agent_is_sent_and_default_used_when_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("user-agent", "my-custom-ua/9.9"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YAML))
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let s = sub_of(&format!(
            "id: s\nname: n\nurl: \"{}\"\nuser_agent: \"my-custom-ua/9.9\"\n",
            server.uri()
        ));
        assert_eq!(f.fetch(&s).await.unwrap().node_count(), 2);

        // UA 留空 → 用默认（伪装 Clash 客户端）
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("user-agent", crate::DEFAULT_USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_string(YAML))
            .mount(&server2)
            .await;
        let mut s2 = sub_for(&server2.uri());
        s2.user_agent = "   ".into();
        assert_eq!(f.fetch(&s2).await.unwrap().node_count(), 2);
    }

    #[tokio::test]
    async fn base64_links_are_decoded() {
        use base64::Engine as _;
        let payload = format!(
            "ss://YWVzOnB3@hk01.example.com:443#{}\ntrojan://pw@jp01.example.com:443#{}\n",
            "%F0%9F%87%AD%F0%9F%87%B0%20%E9%A6%99%E6%B8%AF",
            "%F0%9F%87%AF%F0%9F%87%B5%20%E6%97%A5%E6%9C%AC"
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());
        // 折行，模拟真实机场
        let wrapped = b64
            .as_bytes()
            .chunks(48)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(wrapped))
            .mount(&server)
            .await;

        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.format, DetectedFormat::Base64Links);
        assert_eq!(out.node_count(), 2);
        assert_eq!(out.nodes[0].name, "🇭🇰 香港");
    }

    #[tokio::test]
    async fn html_error_page_is_an_outcome_not_a_hard_error() {
        // 被墙/需登录/502：要让「拒绝采纳」这一层能给出原因，所以 fetch 不直接报错
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<!DOCTYPE html><html><body>502 Bad Gateway</body></html>"),
            )
            .mount(&server)
            .await;

        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.format, DetectedFormat::Html);
        assert!(!out.format.is_usable());
        assert_eq!(out.node_count(), 0);
        assert_eq!(out.group_count(), 0);
    }

    #[tokio::test]
    async fn declared_format_cannot_override_html() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let s = sub_of(&format!("id: s\nname: n\nurl: \"{}\"\nformat: clash\n", server.uri()));
        assert_eq!(f.fetch(&s).await.unwrap().format, DetectedFormat::Html);
    }

    #[tokio::test]
    async fn declared_format_wins_over_sniffing() {
        // 用户声明 clash，内容是链接清单 → 按 clash 解析（0 节点），交给校验层拒绝
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("ss://YWVzOnB3@h.example.com:443"),
            )
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let s = sub_of(&format!("id: s\nname: n\nurl: \"{}\"\nformat: clash\n", server.uri()));
        let out = f.fetch(&s).await.unwrap();
        assert_eq!(out.format, DetectedFormat::ClashYaml);
        assert_eq!(out.node_count(), 0);
    }

    #[tokio::test]
    async fn content_disposition_hint_used_only_when_content_is_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Disposition", "attachment; filename=\"config.yaml\"")
                    .set_body_bytes(b"\x01\x02\x03 not a subscription".to_vec()),
            )
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.format, DetectedFormat::ClashYaml, "内容是垃圾，按文件名兜底");
    }

    #[tokio::test]
    async fn subscription_filter_drops_fake_nodes() {
        let body = "proxies:\n  - {name: \"🇭🇰 香港 01\", type: ss, server: a.com, port: 443}\n  - {name: \"剩余流量：1GB\", type: ss, server: b.com, port: 443}\n  - {name: \"套餐到期：2026-01-01\", type: ss, server: c.com, port: 443}\n";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let s = sub_of(&format!(
            "id: s\nname: n\nurl: \"{}\"\nfilter: {{exclude: \"剩余|到期|官网|防失联\"}}\n",
            server.uri()
        ));
        let out = f.fetch(&s).await.unwrap();
        assert_eq!(out.node_count(), 1);
        assert_eq!(out.nodes[0].name, "🇭🇰 香港 01");
    }

    #[tokio::test]
    async fn absent_headers_and_empty_body_are_not_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.format, DetectedFormat::Unknown);
        assert!(out.userinfo.is_none());
        assert!(out.suggested_interval_s.is_none());
        assert_eq!(out.node_count(), 0);
    }

    // ------------------------------------------------------------ 重试边界

    #[tokio::test]
    async fn retries_on_5xx_then_succeeds() {
        let (server, hits) = server_failing_times(500, 2, YAML).await;
        let (f, sleeper) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();

        assert_eq!(out.node_count(), 2);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "首试 + 2 次重试");
        assert_eq!(
            sleeper.taken(),
            vec![Duration::from_millis(10), Duration::from_millis(20)],
            "指数退避 10ms → 20ms"
        );
    }

    #[tokio::test]
    async fn retries_on_429_and_408() {
        for status in [429u16, 408] {
            let (server, hits) = server_failing_times(status, 1, YAML).await;
            let (f, sleeper) = fetcher(fast_opts());
            assert_eq!(f.fetch(&sub_for(&server.uri())).await.unwrap().node_count(), 2);
            assert_eq!(hits.load(Ordering::SeqCst), 2, "status={status}");
            assert_eq!(sleeper.taken().len(), 1, "status={status}");
        }
    }

    #[tokio::test]
    async fn gives_up_after_two_retries_without_waiting_forever() {
        let (server, hits) = server_failing_times(503, usize::MAX, YAML).await;
        let (f, sleeper) = fetcher(fast_opts());
        let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();

        assert!(err.to_string().contains("下载失败"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 3, "总共只尝试 3 次");
        assert_eq!(sleeper.taken(), vec![Duration::from_millis(10), Duration::from_millis(20)]);
    }

    #[tokio::test]
    async fn plain_4xx_is_fatal_and_not_retried() {
        for status in [400u16, 401, 403, 404] {
            let (server, hits) = server_failing_times(status, usize::MAX, YAML).await;
            let (f, sleeper) = fetcher(fast_opts());
            let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();
            assert!(err.to_string().contains(&format!("HTTP {status}")), "{err}");
            assert_eq!(hits.load(Ordering::SeqCst), 1, "status={status} 不该重试");
            assert!(sleeper.taken().is_empty(), "status={status} 不该退避");
        }
    }

    #[tokio::test]
    async fn backoff_is_capped() {
        // min=1s、factor=2 的默认策略在 2 次重试下是 1s/2s；把 min 调到 3s、cap 4s 看上限生效
        let opts = FetchOptions {
            retry_min_delay: Duration::from_secs(3),
            retry_max_delay: Duration::from_secs(4),
            max_attempts: 4,
            ..Default::default()
        };
        let (server, hits) = server_failing_times(500, usize::MAX, YAML).await;
        let (f, sleeper) = fetcher(opts);
        assert!(f.fetch(&sub_for(&server.uri())).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 4);
        assert_eq!(
            sleeper.taken(),
            vec![
                Duration::from_secs(3),
                // 3*2=6s → 被 ceil 到上限 4s
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[tokio::test]
    async fn read_timeout_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(400))
                    .set_body_string(YAML),
            )
            .mount(&server)
            .await;
        let opts = FetchOptions {
            read_timeout: Duration::from_millis(60),
            max_attempts: 1,
            ..Default::default()
        };
        let (f, _) = fetcher(opts);
        let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();
        assert!(err.to_string().contains("超时"), "{err}");
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let server = MockServer::start().await;
        let big = "x".repeat(4096);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(big))
            .mount(&server)
            .await;
        let opts = FetchOptions { max_bytes: 512, ..fast_opts() };
        let (f, _) = fetcher(opts);
        let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();
        assert!(err.to_string().contains("上限"), "{err}");
    }

    // ------------------------------------------------------------ 压缩

    #[tokio::test]
    async fn undeclared_gzip_is_sniffed_by_magic_bytes() {
        use std::io::Write as _;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(YAML.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(&gz[..2], &[0x1f, 0x8b]);

        let server = MockServer::start().await;
        // ★ 故意不设 Content-Encoding
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(gz))
            .mount(&server)
            .await;

        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.format, DetectedFormat::ClashYaml);
        assert_eq!(out.node_count(), 2);
        assert!(out.raw.starts_with(b"proxies:"), "raw 应是解压后的内容");
    }

    #[tokio::test]
    async fn declared_gzip_is_handled_by_reqwest() {
        use std::io::Write as _;
        let server = MockServer::start().await;
        // wiremock 不做压缩，这里手动放 gzip 体并声明 header → reqwest 自己解
        // （reqwest 的 gzip 解压依赖 workspace 里的 "gzip" feature）
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(YAML.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Encoding", "gzip")
                    .set_body_bytes(gz),
            )
            .mount(&server)
            .await;
        let (f, _) = fetcher(fast_opts());
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.node_count(), 2, "reqwest 已解压，不该二次解压或乱码");
    }

    #[test]
    fn maybe_decompress_passes_plain_bytes_through() {
        // 即使以 `x` 开头（0x78 是 zlib 的常见首字节）也不能误判
        let plain = b"x not compressed at all";
        assert_eq!(maybe_decompress(plain, 1024).unwrap(), plain);
        // 解压炸弹：超过上限要报错
        use std::io::Write as _;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&vec![b'a'; 100_000]).unwrap();
        let gz = enc.finish().unwrap();
        let err = maybe_decompress(&gz, 1024).unwrap_err();
        assert!(err.to_string().contains("上限"), "{err}");
    }

    // ------------------------------------------------------------ 独立 Client（不走 lane）

    #[test]
    fn default_client_is_direct_and_has_no_proxy() {
        let f = HttpSubscriptionFetcher::new().unwrap();
        assert_eq!(f.proxy_mode(), &ProxyMode::Direct);
        assert_eq!(f.options().max_attempts, 3);
        assert_eq!(f.options().connect_timeout, Duration::from_secs(10));
        assert_eq!(f.options().read_timeout, Duration::from_secs(30));
    }

    /// 环境变量是进程级全局状态：这几个测试串行跑，避免互相污染
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_dead_env_proxy<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            std::env::set_var(k, "http://127.0.0.1:1");
        }
        let out = f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        out
    }

    #[tokio::test]
    async fn direct_mode_ignores_env_proxy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YAML))
            .mount(&server)
            .await;
        // ★ 在「环境里挂着死代理」的情况下**构建**客户端：Direct 必须无视它
        let f = with_dead_env_proxy(|| HttpSubscriptionFetcher::with_options(fast_opts()).unwrap());
        assert_eq!(f.proxy_mode(), &ProxyMode::Direct);
        let out = f.fetch(&sub_for(&server.uri())).await.unwrap();
        assert_eq!(out.node_count(), 2, "Direct 模式必须能直连拿到订阅");
    }

    #[tokio::test]
    async fn from_env_mode_does_use_env_proxy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YAML))
            .mount(&server)
            .await;
        // 同样的环境，FromEnv 会把请求送去死代理 → 失败（反证 Direct 那个测试真的在验证代理）
        let f = with_dead_env_proxy(|| {
            HttpSubscriptionFetcher::with_options(FetchOptions {
                proxy: ProxyMode::FromEnv,
                max_attempts: 1,
                ..fast_opts()
            })
            .unwrap()
        });
        let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();
        assert!(err.to_string().contains("下载失败"), "{err}");
    }

    #[tokio::test]
    async fn explicit_proxy_is_honored() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YAML))
            .mount(&server)
            .await;
        let opts = FetchOptions {
            proxy: ProxyMode::Explicit("http://127.0.0.1:1".into()),
            max_attempts: 1,
            ..fast_opts()
        };
        let (f, _) = fetcher(opts);
        assert_eq!(f.proxy_mode(), &ProxyMode::Explicit("http://127.0.0.1:1".into()));
        // 指向死代理 → 必须失败（证明这个代理确实被用上了，而不是被忽略）
        let err = f.fetch(&sub_for(&server.uri())).await.unwrap_err();
        assert!(err.to_string().contains("下载失败"), "{err}");

        // 非法代理地址 → 构造期就报错
        let bad = HttpSubscriptionFetcher::with_options(FetchOptions {
            proxy: ProxyMode::Explicit("not a url".into()),
            ..fast_opts()
        });
        assert!(bad.is_err());
    }

    #[test]
    fn struct_has_no_lane_dependency() {
        // 结构自证：构造函数只接受 FetchOptions，类型里没有任何 lane/dialer/内核字段。
        let _: fn(FetchOptions) -> Result<HttpSubscriptionFetcher> =
            HttpSubscriptionFetcher::with_options;
        let _: fn() -> Result<HttpSubscriptionFetcher> = HttpSubscriptionFetcher::new;
    }
}

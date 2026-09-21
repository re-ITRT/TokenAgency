//! 分发：取 lane 的 client、发送、SSE 零缓冲转发、失败转移。**工作包 G2 负责实现**。
//!
//! 铁律：**已向客户端写出第一个字节后绝不重试**（只记 `stream_broken`）。
//!
//! ## lane 模型（`docs/09 §5.2` / `docs/11 §2.2`）
//!
//! 每条 lane 在内核里对应一个 `mixed` listener（`127.0.0.1:3910x`，`proxy: "LANE-<id>"`）。
//! 所以"走哪条线"= **这个请求用哪个本地代理端口**：每个 lane 一个 `reqwest::Client`，
//! `direct` lane 不带代理（本机网络）。端口来自 `ta_kernel::PortMap`（`LanePorts` 适配）。
//!
//! ## 三个必须写死的 reqwest 设置（都是为了透传保真，不是性能调优）
//!
//! 1. **关闭自动解压**（`no_gzip` / `no_brotli` / `no_deflate`）：
//!    否则 reqwest 会在客户端没发 `accept-encoding` 时**自己加一个**（凭空多一个头 =
//!    破坏"除 4 处例外一律原样转发"），而且会解压后重编码 body（`docs/10 §1.6`
//!    明确要求 `content-encoding` **不透明转发**）。
//! 2. **不跟随重定向**：302 必须原样交给客户端。跟随重定向 = 悄悄改掉上游的响应。
//! 3. **不用环境变量里的代理**（`direct` lane 用 `no_proxy`）：本产品自己的出网
//!    必须由 lane 决定，而不是被用户 shell 里的 `HTTP_PROXY` 左右。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use http::{HeaderMap, StatusCode};

use ta_core::error::{Result, TaError};
use ta_core::types::LaneId;

/// 发往上游的一次请求（已经过 `passthrough` 的定点改写）。
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: String,
    pub url: String,
    pub headers: HeaderMap,
    /// 原始 body 字节（透传路径下是"原样 + 定点 patch"的结果）
    pub body: bytes::Bytes,
    pub stream: bool,
    /// 整请求超时（`provider.timeout.total_s`）；流式请求通常为 `None`
    /// —— 给长流设总超时会在用户看得正起劲时把连接掐断。
    pub total_timeout_s: Option<u32>,
}

pub struct UpstreamResponse {
    pub status: http::StatusCode,
    pub headers: HeaderMap,
    /// 流式响应体。**这里是 `ByteStream` 而不是 `reqwest::Response`**：
    /// 拨号器的契约就是"状态 + 头 + 一个逐 chunk 的流"，把 reqwest 的具体类型
    /// 泄出去会让上层没法在测试里造响应（换通道/断流这些分支就永远测不到）。
    pub body: ByteStream,
}

/// 手写 `Debug`：只打状态与头，**绝不碰 body**（流式体一旦被格式化就会把整条流读掉）。
impl std::fmt::Debug for UpstreamResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &"<streaming>")
            .finish()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthReport {
    pub lane_id: LaneId,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}

#[async_trait]
pub trait OutboundDialer: Send + Sync {
    async fn execute(&self, lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse>;
    async fn health(&self, lane: &LaneId) -> HealthReport;
}

// ================================================================ lane → 端口

/// lane → 内核 listener 端口。G2 只认这个 trait，不直接依赖端口分配的细节。
pub trait LanePorts: Send + Sync {
    fn port_of(&self, lane: &LaneId) -> Option<u16>;
}

/// `ta_kernel::PortMap` 直接就是 lane → 端口表。
impl LanePorts for ta_kernel::PortMap {
    fn port_of(&self, lane: &LaneId) -> Option<u16> {
        self.get(lane)
    }
}

/// 测试 / 未接入内核时用：显式给出 lane → 端口。
#[derive(Debug, Clone, Default)]
pub struct StaticLanePorts(pub std::collections::BTreeMap<LaneId, u16>);

impl StaticLanePorts {
    pub fn new<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (LaneId, u16)>,
    {
        Self(pairs.into_iter().collect())
    }
}

impl LanePorts for StaticLanePorts {
    fn port_of(&self, lane: &LaneId) -> Option<u16> {
        self.0.get(lane).copied()
    }
}

// ================================================================ 拨号器

/// 拨号器选项（超时一律显式，绝不依赖库的默认值）。
#[derive(Debug, Clone, Copy)]
pub struct DialerOptions {
    /// 与 `TimeoutConfig::connect_s` 默认值一致
    pub connect_timeout_s: u32,
    /// 与 `TimeoutConfig::read_s` 默认值一致；**读超时按"两次读到数据之间"计算**，
    /// 所以它不会掐断活跃的长流，只会掐死真正卡住的连接。
    pub read_timeout_s: u32,
}

impl Default for DialerOptions {
    fn default() -> Self {
        Self { connect_timeout_s: 10, read_timeout_s: 600 }
    }
}

/// 走内核 lane 的出站拨号器：**每条 lane 一个 client**。
pub struct KernelDialer {
    ports: Arc<dyn LanePorts>,
    options: DialerOptions,
    clients: Mutex<HashMap<LaneId, reqwest::Client>>,
}

impl KernelDialer {
    pub fn new(ports: Arc<dyn LanePorts>, options: DialerOptions) -> Self {
        Self { ports, options, clients: Mutex::new(HashMap::new()) }
    }

    /// 建 client。`port = None` 表示不走代理（`direct` lane）。
    ///
    /// 这里**没有任何**"顺手"的默认头/UA/重试：出站请求的全部内容都由
    /// `passthrough` 决定，拨号器只负责"从哪个口出去"。
    ///
    /// ## 一处已知偏离：`accept: */*`
    ///
    /// 客户端**没发** `accept` 时，上游仍会看到 `accept: */*` —— 它不是我们写的：
    /// `reqwest-0.12.28/src/async_impl/client.rs:286` 的 `ClientBuilder::new()` 就把这条
    /// 塞进了默认头，而 `default_headers()`（:1163）只做合并、**没法把它清掉**。
    /// 这与"关自动解压"防的是同一类问题（凭空多一个客户端没发的头），但那条能关、这条不能。
    ///
    /// 处置：**不静默容忍**。`pipeline::tests::a2_upstream_receives_the_golden_request_byte_for_byte`
    /// 里有一条断言把"上游多看到的头集合"钉成**恰好** `{accept}` ——
    /// 将来 reqwest 再多塞一个默认头、或我们误加了头，那条会红。
    /// 若哪天要把 I3 收回到"零注入"，这里得换成手写 hyper 传输（代价是 TLS/代理 CONNECT/
    /// HTTP2/流式取消都要自己做），属于要主管裁决的独立工作包。
    fn build_client(&self, port: Option<u16>) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(self.options.connect_timeout_s as u64))
            .read_timeout(Duration::from_secs(self.options.read_timeout_s as u64))
            .tcp_nodelay(true);
        builder = match port {
            // lane 的内核 listener 就是一个本地 HTTP 代理口
            Some(port) => builder.proxy(
                reqwest::Proxy::http(format!("http://127.0.0.1:{port}"))
                    .map_err(|e| TaError::Internal(format!("lane 代理端口不合法：{e}")))?,
            ),
            // direct：绕过环境里的 HTTP_PROXY，走本机网络
            None => builder.no_proxy(),
        };
        builder.build().map_err(|e| TaError::Internal(format!("构造 lane client 失败：{e}")))
    }

    /// 取（或建）该 lane 的 client。`direct` lane 不需要端口。
    fn client_for(&self, lane: &LaneId) -> Result<reqwest::Client> {
        let port = if is_direct_lane(lane) { None } else { Some(self.require_port(lane)?) };
        let mut guard = match self.clients.lock() {
            Ok(guard) => guard,
            // 锁中毒不该让网关瘫痪：取回内部值继续用
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(client) = guard.get(lane) {
            return Ok(client.clone());
        }
        let client = self.build_client(port)?;
        guard.insert(lane.clone(), client.clone());
        Ok(client)
    }

    fn require_port(&self, lane: &LaneId) -> Result<u16> {
        self.ports.port_of(lane).ok_or_else(|| TaError::LaneUnavailable {
            lane: format!("{lane}（内核尚未分配 listener 端口）"),
        })
    }
}

/// `direct` 是保留的 lane id（`docs/02`：五级线路解析的兜底值就是 `direct`）。
pub fn is_direct_lane(lane: &LaneId) -> bool {
    lane.as_str().eq_ignore_ascii_case("direct")
}

#[async_trait]
impl OutboundDialer for KernelDialer {
    async fn execute(&self, lane: &LaneId, req: UpstreamRequest) -> Result<UpstreamResponse> {
        let client = self.client_for(lane)?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| TaError::Internal(format!("请求方法不合法：{}（{e}）", req.method)))?;
        let mut builder = client.request(method, &req.url);
        for (name, value) in req.headers.iter() {
            // 逐个 header 原样塞进去：这里**不做**任何筛选（筛选是 passthrough 的职责，
            // 而且它已经把该删的删掉了）
            builder = builder.header(name, value);
        }
        builder = builder.body(req.body.clone());
        if let Some(secs) = req.total_timeout_s {
            builder = builder.timeout(Duration::from_secs(secs as u64));
        }

        let response = builder.send().await.map_err(|e| classify_send_error(lane, &e))?;
        let status = response.status();
        let headers = response.headers().clone();
        Ok(UpstreamResponse { status, headers, body: upstream_byte_stream(response) })
    }

    async fn health(&self, lane: &LaneId) -> HealthReport {
        if is_direct_lane(lane) {
            // 直连不需要探测：没有中间进程可以"不健康"
            return HealthReport {
                lane_id: lane.clone(),
                ok: true,
                latency_ms: Some(0),
                error: None,
            };
        }
        let Some(port) = self.ports.port_of(lane) else {
            return HealthReport {
                lane_id: lane.clone(),
                ok: false,
                latency_ms: None,
                error: Some("内核尚未分配 listener 端口".into()),
            };
        };
        let started = Instant::now();
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let outcome = tokio::time::timeout(
            Duration::from_millis(1500),
            tokio::net::TcpStream::connect(target),
        )
        .await;
        match outcome {
            Ok(Ok(_stream)) => HealthReport {
                lane_id: lane.clone(),
                ok: true,
                latency_ms: Some(started.elapsed().as_millis() as u32),
                error: None,
            },
            Ok(Err(e)) => HealthReport {
                lane_id: lane.clone(),
                ok: false,
                latency_ms: None,
                error: Some(format!("连接 127.0.0.1:{port} 失败：{e}")),
            },
            Err(_) => HealthReport {
                lane_id: lane.clone(),
                ok: false,
                latency_ms: None,
                error: Some("连接 lane listener 超时（1.5s）".into()),
            },
        }
    }
}

/// 把 reqwest 的失败映射成本项目的错误码：
/// **连接层失败 → `TA-3002 lane_unavailable`（换 lane）**，
/// 其它（超时/协议）→ `TA-2005 all_channels_failed`（换通道）。
/// 这个区分是 `docs/03 §5.1` 的要求：lane 连不上不该把整个通道判死。
fn classify_send_error(lane: &LaneId, err: &reqwest::Error) -> TaError {
    if err.is_connect() {
        return TaError::LaneUnavailable { lane: lane.to_string() };
    }
    TaError::AllChannelsFailed { message: format!("lane {lane}：{err}") }
}

// ================================================================ 响应方向转发

/// 流式响应的字节流类型（`BoxError` 由 axum 的 `Body` 要求）。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type ByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, BoxError>> + Send>>;

/// 把上游响应的 body 变成"逐 chunk"的字节流 —— **这一步就是零缓冲的关键**：
/// 不 `collect()`、不聚合、不重编码。
pub fn upstream_byte_stream(response: reqwest::Response) -> ByteStream {
    Box::pin(response.bytes_stream().map(|chunk| chunk.map_err(|e| Box::new(e) as BoxError)))
}

/// 已经在内存里的字节 → 单元素流（非流式路径 / 测试用）。
pub fn byte_stream_from_bytes(body: Bytes) -> ByteStream {
    Box::pin(futures_util::stream::once(async move { Ok(body) }))
}

/// **SSE 零缓冲转发**：把上游响应原样变成给客户端的响应。
///
/// - 状态码与全部头**逐字段照搬**（只剥离 hop-by-hop —— 那是协议要求）；
/// - body 直通，**不 `collect()`、不重新分帧、不重新编码**；
/// - 上游中途断连 → 流自然结束（管线记 `TA-3003`），**绝不重试**。
pub fn into_client_response(upstream: UpstreamResponse) -> axum::response::Response {
    let UpstreamResponse { status, headers, body } = upstream;
    client_response_from_parts(status, headers, body)
}

/// 上游状态码是否值得换通道（`docs/03 §5.1` 的触发条件映射）。
///
/// **429 / 402 / 5xx** 是"这个通道现在不行"；**4xx（其余）是请求本身的问题**，
/// 换通道只会把同一个错误再演一遍，所以原样透传给客户端。
pub fn is_failover_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::PAYMENT_REQUIRED
        || status.is_server_error()
}

/// 用给定的字节流构造给客户端的响应：状态码与头逐字段照搬（只剥离 hop-by-hop）。
pub fn client_response_from_parts(
    status: StatusCode,
    headers: HeaderMap,
    stream: ByteStream,
) -> axum::response::Response {
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(&headers, response.headers_mut());
    response
}

/// 非流式路径：body 已经完整拿到（交给计量之后再吐给客户端）。
pub fn client_response_from_bytes(
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let mut response = axum::response::Response::new(axum::body::Body::from(body));
    *response.status_mut() = status;
    copy_response_headers(&headers, response.headers_mut());
    response
}

fn copy_response_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (name, value) in from.iter() {
        if crate::passthrough::is_hop_by_hop(name.as_str()) {
            continue;
        }
        to.append(name.clone(), value.clone());
    }
}

// ================================================================ 测试

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 一个手写的"假上游"：直接写原始 HTTP 字节。
    ///
    /// 为什么不用 wiremock：**"输出 3 个 chunk 后断连"必须由我们自己控制** ——
    /// wiremock 发的是完整响应，模拟不出"半路掐断"。
    pub(crate) struct FakeUpstream {
        pub addr: std::net::SocketAddr,
        pub connections: Arc<AtomicU32>,
        /// 每个连接收到的原始请求头（用于逐字节核对透传）
        pub seen_headers: Arc<Mutex<Vec<String>>>,
    }

    impl FakeUpstream {
        pub(crate) async fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.addr)
        }

        pub(crate) fn connection_count(&self) -> u32 {
            self.connections.load(Ordering::SeqCst)
        }

        pub(crate) fn last_request_headers(&self) -> String {
            match self.seen_headers.lock() {
                Ok(guard) => guard.last().cloned().unwrap_or_default(),
                Err(poisoned) => poisoned.into_inner().last().cloned().unwrap_or_default(),
            }
        }
    }

    /// 启动假上游。`script` 拿到 (连接序号, 已读到的请求头, 写半边) 后自己决定怎么回。
    pub(crate) async fn spawn_fake_upstream<F, Fut>(script: F) -> FakeUpstream
    where
        F: Fn(u32, String, tokio::net::tcp::OwnedWriteHalf) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假上游");
        let addr = listener.local_addr().expect("本地地址");
        let connections = Arc::new(AtomicU32::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let counter = connections.clone();
        let seen_headers = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let index = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let head = read_request_head(&mut socket).await;
                match seen_headers.lock() {
                    Ok(mut guard) => guard.push(head.clone()),
                    Err(poisoned) => poisoned.into_inner().push(head.clone()),
                }
                let (_, write) = socket.into_split();
                script(index, head, write).await;
            }
        });
        FakeUpstream { addr, connections, seen_headers: seen }
    }

    /// 读到 `\r\n\r\n` 为止（假上游只需要头，body 我们不关心）。
    async fn read_request_head(socket: &mut tokio::net::TcpStream) -> String {
        let mut buf = vec![0u8; 8192];
        let mut acc = Vec::new();
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&acc).into_owned()
    }

    fn direct() -> LaneId {
        LaneId::new("direct")
    }

    // ---------------------------------------------------------- A2 用的"记录型"假上游

    /// 记录**完整原始报文**（请求头 + body）的假上游，用于 A2 的 wire 级证据。
    ///
    /// 与 [`spawn_fake_upstream`] 的区别：那个只读请求头（够验证头部透传），
    /// 而"图片块 / 未知字段逐字节到达"必须看 **body 的原始字节** ——
    /// 用 `serde_json::Value` 比对只能证明"解析后等价"，证明不了 wire 上的字节。
    pub(crate) struct RecordingUpstream {
        pub addr: std::net::SocketAddr,
        pub requests: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl RecordingUpstream {
        /// 第 `index` 次收到的原始报文（头 + body）。
        pub(crate) fn raw(&self, index: usize) -> Vec<u8> {
            match self.requests.lock() {
                Ok(guard) => guard.get(index).cloned().unwrap_or_default(),
                Err(poisoned) => poisoned.into_inner().get(index).cloned().unwrap_or_default(),
            }
        }
    }

    /// 读一个**完整**请求：读到 `\r\n\r\n` 后，再按 `content-length` 读满 body。
    async fn read_full_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            if let Some(head_end) = acc.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4) {
                let head = String::from_utf8_lossy(&acc[..head_end]).to_ascii_lowercase();
                let declared = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if acc.len() >= head_end + declared {
                    return acc;
                }
            }
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => return acc,
                Ok(n) => acc.extend_from_slice(&buf[..n]),
            }
        }
    }

    /// 每次都回 `200 {"ok":true}`，并把收到的原始字节留存下来。
    pub(crate) async fn spawn_recording_upstream() -> RecordingUpstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假上游");
        let addr = listener.local_addr().expect("本地地址");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let raw = read_full_request(&mut socket).await;
                match sink.lock() {
                    Ok(mut guard) => guard.push(raw),
                    Err(poisoned) => poisoned.into_inner().push(raw),
                }
                let (_, mut write) = socket.into_split();
                let _ = write
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                          content-length: 11\r\n\r\n{\"ok\":true}",
                    )
                    .await;
                let _ = write.flush().await;
            }
        });
        RecordingUpstream { addr, requests }
    }

    fn dialer() -> KernelDialer {
        KernelDialer::new(Arc::new(StaticLanePorts::default()), DialerOptions::default())
    }

    fn request(url: String, stream: bool) -> UpstreamRequest {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "claude-cli/1.0.60".parse().unwrap());
        headers.insert("x-stainless-lang", "js".parse().unwrap());
        headers.insert("anthropic-beta", "prompt-caching-2024-07-31".parse().unwrap());
        UpstreamRequest {
            method: "POST".into(),
            url,
            headers,
            body: Bytes::from_static(br#"{"model":"claude-sonnet-4-5","stream":true}"#),
            stream,
            total_timeout_s: None,
        }
    }

    // ---------------------------------------------------------- DoD ②：重试边界

    #[tokio::test]
    async fn aborted_stream_after_three_chunks_is_never_retried() {
        // 假上游：发 3 个 chunk，然后**直接关连接**（不发终止块）
        let upstream = spawn_fake_upstream(|_index, _head, mut write| async move {
            let _ = write
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      transfer-encoding: chunked\r\n\r\n",
                )
                .await;
            for i in 1..=3 {
                let chunk = format!("data: {{\"i\":{i}}}\n\n");
                let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                let _ = write.write_all(frame.as_bytes()).await;
                let _ = write.flush().await;
            }
            // 半路掐断：不发 "0\r\n\r\n"
            drop(write);
        })
        .await;

        let response = dialer()
            .execute(&direct(), request(upstream.url("/v1/messages").await, true))
            .await
            .expect("上游应该回 200");
        assert_eq!(response.status, StatusCode::OK);

        // 客户端侧：拿到 3 个 chunk，然后流结束（而不是被重试）
        let mut client_response = into_client_response(response);
        let mut received = Vec::new();
        while let Some(chunk) = client_response.body_mut().frame().await {
            let Ok(frame) = chunk else { break };
            if let Ok(data) = frame.into_data() {
                received.extend_from_slice(&data);
            }
        }
        let text = String::from_utf8_lossy(&received).to_string();
        for i in 1..=3 {
            assert!(
                text.contains(&format!("data: {{\"i\":{i}}}")),
                "第 {i} 个 chunk 必须到达客户端：{text}"
            );
        }
        // **只有 1 次上游请求** —— 这就是 DoD 的数字
        assert_eq!(upstream.connection_count(), 1, "输出 3 个 chunk 后断连，绝不允许重试");
    }

    // ---------------------------------------------------------- DoD ①：SSE 零缓冲

    #[tokio::test]
    async fn sse_is_forwarded_without_buffering() {
        // 假上游：发第 1 个 chunk 后**等测试确认客户端已收到**，再发第 2 个。
        // 如果网关缓冲整个响应，第 1 个 chunk 不会立刻到达 → 上游会一直等 → 测试超时。
        // （超时就是证据；这个测试不会死锁。）
        let gate = Arc::new(tokio::sync::Notify::new());
        let server_gate = gate.clone();
        let upstream = spawn_fake_upstream(move |_index, _head, mut write| {
            let gate = server_gate.clone();
            async move {
                let _ = write
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          transfer-encoding: chunked\r\n\r\n",
                    )
                    .await;
                for i in 1..=2 {
                    let chunk = format!("data: {{\"i\":{i}}}\n\n");
                    let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                    let _ = write.write_all(frame.as_bytes()).await;
                    let _ = write.flush().await;
                    // 等测试说"客户端已经收到第 i 个 chunk 了"
                    gate.notified().await;
                }
                let _ = write.write_all(b"0\r\n\r\n").await;
                let _ = write.flush().await;
            }
        })
        .await;

        let response = dialer()
            .execute(&direct(), request(upstream.url("/v1/messages").await, true))
            .await
            .expect("上游应该回 200");
        let status = response.status;
        let headers = response.headers.clone();
        let mut client_response = into_client_response(response);

        // 响应头必须原样出现（响应方向的 golden）
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-type").unwrap(), "text/event-stream");
        assert!(!client_response.headers().contains_key("transfer-encoding"), "hop-by-hop 剥离");

        let first =
            tokio::time::timeout(Duration::from_secs(2), client_response.body_mut().frame())
                .await
                .expect("首字节必须立刻到达（零缓冲）")
                .expect("应当有帧")
                .expect("帧应当可读");
        let first_text =
            String::from_utf8_lossy(&first.into_data().unwrap_or_default()).to_string();
        assert!(first_text.contains("data: {\"i\":1}"), "{first_text}");

        // 客户端确认收到 → 上游才发第 2 个
        gate.notify_one();
        let second =
            tokio::time::timeout(Duration::from_secs(2), client_response.body_mut().frame())
                .await
                .expect("第 2 个 chunk 也必须能到达（说明转发是逐个 chunk 的）")
                .expect("应当有帧")
                .expect("帧应当可读");
        let second_text =
            String::from_utf8_lossy(&second.into_data().unwrap_or_default()).to_string();
        assert!(second_text.contains("data: {\"i\":2}"), "{second_text}");
        gate.notify_one();
    }

    // ---------------------------------------------------------- 透传保真（请求方向）

    #[tokio::test]
    async fn upstream_sees_client_headers_verbatim_and_no_injected_accept_encoding() {
        let upstream = spawn_fake_upstream(|_index, _head, mut write| async move {
            let _ = write.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
        })
        .await;
        let response = dialer()
            .execute(&direct(), request(upstream.url("/v1/messages").await, false))
            .await
            .expect("应当成功");
        drop(into_client_response(response));

        let head = upstream.last_request_headers();
        // reqwest 不许替我们加 accept-encoding（那会凭空多一个客户端没发的头）
        assert!(
            !head.to_ascii_lowercase().contains("accept-encoding"),
            "拨号器不许注入 accept-encoding：\n{head}"
        );
        // 客户端的标识头必须逐个到达
        assert!(head.contains("user-agent: claude-cli/1.0.60"), "{head}");
        assert!(head.contains("x-stainless-lang: js"), "{head}");
        assert!(head.contains("anthropic-beta: prompt-caching-2024-07-31"), "{head}");
        // host 只出现一次（不许重复）
        let host_count = head.to_ascii_lowercase().matches("\r\nhost:").count()
            + usize::from(head.to_ascii_lowercase().starts_with("host:"));
        assert_eq!(host_count, 1, "host 头只能有一个：\n{head}");
    }

    #[tokio::test]
    async fn client_response_keeps_upstream_status_and_headers_verbatim() {
        let upstream = spawn_fake_upstream(|_index, _head, mut write| async move {
            let _ = write
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\n\
                      anthropic-ratelimit-requests-remaining: 0\r\n\
                      retry-after: 12\r\nx-upstream-invented-header: keep-me\r\n\
                      connection: close\r\ncontent-length: 2\r\n\r\n{}",
                )
                .await;
        })
        .await;
        let response = dialer()
            .execute(&direct(), request(upstream.url("/v1/messages").await, false))
            .await
            .expect("429 也是成功的一次 HTTP 交换");
        assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(is_failover_status(response.status), "429 必须触发换通道");

        let client_response = into_client_response(response);
        let headers = client_response.headers();
        assert_eq!(headers.get("anthropic-ratelimit-requests-remaining").unwrap(), "0");
        assert_eq!(headers.get("retry-after").unwrap(), "12");
        assert_eq!(headers.get("x-upstream-invented-header").unwrap(), "keep-me");
        assert_eq!(headers.get("content-length").unwrap(), "2");
        assert!(!headers.contains_key("connection"), "hop-by-hop 必须剥离");
    }

    #[tokio::test]
    async fn upstream_redirect_is_passed_through_not_followed() {
        let upstream = spawn_fake_upstream(|_index, _head, mut write| async move {
            let _ = write
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: https://elsewhere.example/v1/messages\r\n\
                      content-length: 0\r\n\r\n",
                )
                .await;
        })
        .await;
        let response = dialer()
            .execute(&direct(), request(upstream.url("/v1/messages").await, false))
            .await
            .expect("应当成功");
        assert_eq!(response.status, StatusCode::FOUND);
        assert_eq!(upstream.connection_count(), 1, "绝不能自己跟去第二个地址");
        let client_response = into_client_response(response);
        assert_eq!(
            client_response.headers().get("location").unwrap(),
            "https://elsewhere.example/v1/messages"
        );
    }

    // ---------------------------------------------------------- lane 端口与健康

    #[tokio::test]
    async fn non_direct_lane_without_port_is_unavailable() {
        let err = dialer()
            .execute(&LaneId::new("lane-jp"), request("http://127.0.0.1:1/x".into(), false))
            .await
            .expect_err("没有端口就该报 lane_unavailable");
        assert_eq!(err.code(), "TA-3002");
        assert_eq!(err.http_status(), 503);
        assert!(err.retryable());
    }

    #[tokio::test]
    async fn direct_lane_is_healthy_without_probing() {
        let report = dialer().health(&direct()).await;
        assert!(report.ok);
        assert_eq!(report.lane_id.as_str(), "direct");
    }

    #[tokio::test]
    async fn lane_health_probes_the_listener_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    return;
                }
            }
        });
        let lane = LaneId::new("lane-jp");
        let dialer = KernelDialer::new(
            Arc::new(StaticLanePorts::new([(lane.clone(), port)])),
            DialerOptions::default(),
        );
        let report = dialer.health(&lane).await;
        assert!(report.ok, "{:?}", report.error);
        assert!(report.latency_ms.is_some());

        // 没人监听的端口 → 明确不健康（而不是"未知"）
        let dead = LaneId::new("lane-dead");
        let dialer = KernelDialer::new(
            Arc::new(StaticLanePorts::new([(dead.clone(), 1)])),
            DialerOptions::default(),
        );
        let report = dialer.health(&dead).await;
        assert!(!report.ok);
        assert!(report.error.is_some());
    }

    #[tokio::test]
    async fn connect_failure_is_lane_unavailable_not_channel_failure() {
        let lane = LaneId::new("lane-dead");
        // 端口 1 上几乎一定没有监听者 → 连接失败
        let dialer = KernelDialer::new(
            Arc::new(StaticLanePorts::new([(lane.clone(), 1)])),
            DialerOptions { connect_timeout_s: 1, ..Default::default() },
        );
        let err = dialer
            .execute(&lane, request("http://127.0.0.1:9/v1/messages".into(), false))
            .await
            .expect_err("连接失败必须报错");
        assert_eq!(err.code(), "TA-3002", "lane 连不上 = 换 lane，而不是把通道判死：{err:?}");
    }

    #[test]
    fn failover_status_classification() {
        for status in [429u16, 402, 500, 502, 503] {
            assert!(
                is_failover_status(StatusCode::from_u16(status).unwrap()),
                "{status} 应当触发换通道"
            );
        }
        for status in [200u16, 400, 401, 403, 404, 413, 415, 422] {
            assert!(
                !is_failover_status(StatusCode::from_u16(status).unwrap()),
                "{status} 是请求本身的问题，换通道只会重演同一个错误"
            );
        }
    }
}

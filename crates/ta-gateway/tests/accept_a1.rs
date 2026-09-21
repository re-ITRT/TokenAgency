//! **A1 端到端验收**（[docs/06 §2.1]）：同一个逻辑模型挂两个套餐，首选套餐额度耗尽后自动切到下
//! 一个，而且**切过去意味着出口（线路）真的变了**。
//!
//! 判据原文：配两个 credential 指向同一 model（一个 lane=direct、一个 lane=走代理），把 A 的额度
//! 设小 → 跑请求直到切到 B → **断言容器/出口确实变了**（不是只看日志说切了），且同一会话保持粘性。
//!
//! ## 怎么让"出口真的变了"可被观测
//!
//! 不能把两个套餐都指向互联网 —— 那样"走没走代理"根本无法断言。这里把**出口**造成两个**本地**
//! 可观测端点，两个 credential 使用同一个假上游（它就是"模型服务"）：
//!
//! ```text
//!                    ┌───────────────────────────┐
//!  请求 1..4 ───────▶│ ta-gateway：路由 → 管线    │  （真 axum router + 真 Pipeline）
//!  (x-session-id)    └────────────┬──────────────┘
//!                                 │ KernelDialer（每条 lane 一个 reqwest client）
//!                 lane-a-direct ──┤                lane-b-proxy ──┐
//!                                 ▼                               ▼
//!                    mihomo listener :Pa              mihomo listener :Pb
//!                       proxy: DIRECT                    proxy: LANE-b（组）
//!                                 │                               │
//!                                 │                               ▼
//!                                 │                   【假代理】127.0.0.1:Pp（记录每次请求）
//!                                 │                               │
//!                                 └───────────────┬───────────────┘
//!                                                 ▼
//!                                    【假上游】127.0.0.1:Pu（记录每次请求）
//! ```
//!
//! 三条断言（缺一不可，见 `assert_*` 处的注释）：
//! 1. **切换前**：假上游收到请求、假代理**零记录** → 走的是直连；
//! 2. **切换后**：假代理**有记录**（请求真的从代理口出去）且假上游**仍然**收到请求；
//! 3. **粘性**：同一个 `x-session-id` 在切换后继续走 B，**不来回抖**；
//!    而且这一条是**可证伪的**：切换完再热更新一次配置，多挂一个**优先级更高**的直连套餐 C，
//!    老会话必须仍走 B（代理口），换代后新开的会话必须走 C（直连口）。只断言"又走了 B"
//!    是没用的 —— A 已耗尽、B 是唯一候选时，粘性完全失效也照样成立。
//!
//! 第 1、2 条里的"假代理零记录 / 有记录"就是"出口真的变了"的**物理**证据：它不看任何一方的
//! 自述 —— 网关说"我切了"不算，得看请求真的从哪个 socket 出去了。
//!
//! ## 实测到的、写下来以免被当成猜测的两件事
//!
//! - **内核走的是 `CONNECT` 隧道**（mihomo v1.19.31）：假代理实际收到的是
//!   `CONNECT 127.0.0.1:<上游端口>`，不是 forward 形式的绝对 URI。假代理两种形态都支持，
//!   记录里 `method` 字段就是它的原话（别把它当"应该是 forward"）。
//! - **`lane-a-direct` 的请求确实经过了内核**：靠的是结构而不是抓包 —— 该 lane 的端口
//!   (`StaticLanePorts`) 就是内核 listener 端口，而 `KernelDialer` 对非保留 id 的 lane
//!   一律把请求交给 `http://127.0.0.1:<该端口>`；那个端口上只有 mihomo 在监听，
//!   所以"回的是上游真响应而不是 502"已经蕴含"经过了内核"。本测试没有单独抓包证明这一点。
//! - **`KernelDialer` 的 `direct` 是保留 id**（`dispatch::is_direct_lane` 只认字符串 `direct`）。
//!   所以本测试两条 lane 的 id 都**不是** `direct`，两条都经过内核 listener ——
//!   这正是"有出口可断言"的前提，也说明生产里名为 `lane-direct` 的直连 lane 同样走内核。
//!
//! ## 这条测试**不证**什么（逐条列清，别读成更多）
//!
//! - **不证 `config_gen` 会生成这份内核配置**：内核配置是这里**手写的最小等价配置**
//!   （任务允许）。手写配置与 `ta_kernel::config_gen::MhConfigGen` 输出的等价性**未验证**。
//! - **不证 G4 的计量正确性**：G4（`ResponseMeter` → 额度账本）**还没接线**，所以这里注入了一个
//!   测试用 `LedgerMeter`，把上游响应的 `usage.total_tokens` 记进**真账本**
//!   （`QuotaLedgerImpl` + `Credential.quota.limit`）。这条测试证的是
//!   "账本判耗尽 → 路由换候选 → 出口真的变了 → 粘性不抖"，
//!   **不是**"生产上喂给账本的 token 数是对的"。
//! - **不证真订阅/真节点**：假代理是我们自己的 loopback HTTP 代理，不是机场节点；
//!   也不涉及 `proxy-providers` / 订阅拉取 / url-test 健康检查。
//! - **不证跨进程/跨重启**：全部在同一个测试进程内（mihomo 是**真进程**）。
//! - **不证流式**：用非流式请求断言；流式路径的换通道/粘性是另一条路径。
//! - **不证 `direct` 兜底**：两条 lane 都显式声明，没有走五级解析的 `direct` 兜底分支。
//! - **不证多进程并发**：请求是串行发的（并发下的粘性竞争不在本判据里）。

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ta_core::clock::{Clock, ManualClock};
use ta_core::domain::{
    ChannelConfig, ChannelGroup, ConfigDoc, Credential, Lane, LaneKind, Model, Provider,
    QuotaConfig, QuotaMetric, QuotaWindow, Selector,
};
use ta_core::pricing::{PricingEngine, ResolvedPrice};
use ta_core::quota::{
    MemoryQuotaBackend, QuotaLedger, QuotaLedgerImpl, QuotaObservation, QuotaStateKind,
};
use ta_core::types::{
    CredentialId, LaneId, Money, Protocol, ProviderId, RequestSummary, TokenUsage, UsageRecord,
};

use ta_gateway::dispatch::{ByteStream, DialerOptions, StaticLanePorts};
use ta_gateway::events::TaEvent;
use ta_gateway::ingress::{self, KernelProbe, KernelState};
use ta_gateway::pipeline::{
    AttemptRecord, AttemptSink, MeterContext, ResponseMeter, StaticSecrets,
};
use ta_gateway::traits_store::GatewayStore;
use ta_gateway::{build_runtime, kernel_dialer, GatewayDeps, RuntimeParts};

// ================================================================ 夹具常量

const MODEL: &str = "claude-sonnet-4-5";
/// 两条 lane 的 id。**故意不叫 `direct`**：`direct` 是保留 id（`dispatch::is_direct_lane`），
/// 那条路会绕过内核直接出网。这里两条 lane 都要经过内核 listener —— 这才有"出口"可断言。
const LANE_DIRECT: &str = "lane-a-direct";
const LANE_PROXY: &str = "lane-b-proxy";
/// 内核配置里那个「组 → 假代理」的组名（`listeners[].proxy` 指向它）。
const PROXY_GROUP: &str = "LANE-b";

const CH_A: &str = "ch-a";
const CH_B: &str = "ch-b";
/// 配置换代后才出现的第三个套餐（优先级最高、直连）——粘性的"反证探针"。
const CH_C: &str = "ch-c";
const CRED_A: &str = "c-a";
const CRED_B: &str = "c-b";
const CRED_C: &str = "c-c";

const SESSION: &str = "sess-a1-sticky";
/// 换代之后新开一个会话：它**没有**粘性记录，必须按新优先级走 C（直连）。
const FRESH_SESSION: &str = "sess-a1-fresh";

/// 套餐 A 的额度上限（token）。
///
/// **这个数字是 A1 的"关键条件"**：调大到任何一次请求都吃不完时，A 永远不会被判耗尽，
/// 测试必须红在"切换后假代理有记录"那一条上（见交付证据里的红跑）。
const A_LIMIT_TOKENS: f64 = 20.0;

/// 假上游每次响应报的 usage（`total_tokens` = 12）。两次请求就把 A 的 20 吃穿。
const UPSTREAM_BODY: &str = r#"{"id":"chatcmpl-a1","object":"chat.completion","model":"claude-sonnet-4-5","choices":[],"usage":{"prompt_tokens":6,"completion_tokens":6,"total_tokens":12}}"#;
const UPSTREAM_TOKENS: u64 = 12;

// ================================================================ 假端点（假上游 / 假代理）
//
// 都是**手写的最小 HTTP/1.1 服务**：不引额外依赖（`Cargo.toml` 是 E5 的文件，不许改），
// 也不假装是通用实现 —— 只够本测试用，解析不了就返回 4xx/5xx 并在日志里看得出来。

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Clone)]
struct HttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// 读一个完整的 HTTP/1.1 请求（请求行 + 头 + `content-length` 指定的 body）。
async fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "读完请求头之前对端就关了"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let target = request_line.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();

    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Ok(HttpRequest { method, target, headers, body })
}

/// 记录的"谁来过"的一条。**这就是物理证据**：请求真的到过这个 socket。
#[derive(Debug, Clone)]
struct Hit {
    method: String,
    target: String,
    host: Option<String>,
}

#[derive(Debug, Default)]
struct HitLog {
    hits: Mutex<Vec<Hit>>,
}

impl HitLog {
    fn record(&self, req: &HttpRequest) {
        let hit = Hit {
            method: req.method.clone(),
            target: req.target.clone(),
            host: req.header("host").map(str::to_string),
        };
        self.hits.lock().expect("记录锁被毒化").push(hit);
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().expect("记录锁被毒化").clone()
    }

    fn len(&self) -> usize {
        self.hits.lock().expect("记录锁被毒化").len()
    }
}

/// 一个 loopback 假端点。`Drop` 时 abort 服务任务 → 监听 socket 随之释放。
struct FakeEndpoint {
    addr: SocketAddr,
    log: Arc<HitLog>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeEndpoint {
    fn drop(&mut self) {
        // RAII：断言失败 panic 时也要把端口收掉（否则下一次跑测试会撞端口）
        self.task.abort();
    }
}

async fn bind_loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback:0");
    let addr = listener.local_addr().expect("取本地地址");
    (listener, addr)
}

/// 假上游 = "模型服务"。每个请求记一笔，然后回一份固定 JSON（含 usage）。
async fn start_fake_upstream(body: &'static str) -> FakeEndpoint {
    let (listener, addr) = bind_loopback().await;
    let log = Arc::new(HitLog::default());
    let log_in_task = log.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let log = log_in_task.clone();
            tokio::spawn(async move {
                let Ok(req) = read_http_request(&mut sock).await else { return };
                log.record(&req);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    FakeEndpoint { addr, log, task }
}

/// `http://host:port/path` → `(host:port, /path)`
fn split_absolute_target(target: &str) -> Option<(String, String)> {
    let rest = target.strip_prefix("http://")?;
    match rest.find('/') {
        Some(i) => Some((rest[..i].to_string(), rest[i..].to_string())),
        None => Some((rest.to_string(), "/".to_string())),
    }
}

/// `host:port`（CONNECT 的目标）
fn split_authority(target: &str) -> Option<String> {
    let (host, port) = target.rsplit_once(':')?;
    if host.is_empty() || port.parse::<u16>().is_err() {
        return None;
    }
    Some(target.to_string())
}

/// 假代理 = 我们的"出口"。只做两件事：**记录** + 转发（含 CONNECT 兜底）。
async fn start_fake_proxy() -> FakeEndpoint {
    let (listener, addr) = bind_loopback().await;
    let log = Arc::new(HitLog::default());
    let log_in_task = log.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let log = log_in_task.clone();
            tokio::spawn(async move {
                let Ok(req) = read_http_request(&mut sock).await else { return };
                // ★ 记录先于任何转发：只要客户端"从代理口出去"，这里必然有一笔
                log.record(&req);

                // CONNECT 分支：内核若选择隧道模式，把目标原样放进 `target`
                if req.method.eq_ignore_ascii_case("CONNECT") {
                    let Some(authority) = split_authority(&req.target) else {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\
                                  connection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    };
                    let Ok(mut up) = TcpStream::connect(&authority).await else {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\
                                  connection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    };
                    let _ = sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
                    return;
                }

                // 转发模式：absolute-form → origin-form，原样把响应字节搬回去
                let Some((authority, origin_form)) = split_absolute_target(&req.target) else {
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\
                              connection: close\r\n\r\n",
                        )
                        .await;
                    return;
                };
                let Ok(mut up) = TcpStream::connect(&authority).await else {
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\
                              connection: close\r\n\r\n",
                        )
                        .await;
                    return;
                };
                let mut out = format!("{} {} HTTP/1.1\r\n", req.method, origin_form);
                for (name, value) in &req.headers {
                    if name == "proxy-connection" {
                        continue;
                    }
                    out.push_str(&format!("{name}: {value}\r\n"));
                }
                out.push_str("\r\n");
                if up.write_all(out.as_bytes()).await.is_err() {
                    return;
                }
                if up.write_all(&req.body).await.is_err() {
                    return;
                }
                let _ = up.flush().await;

                let mut response = Vec::new();
                // 假上游回 `connection: close`，所以读到 EOF 就是整个响应
                let read =
                    tokio::time::timeout(Duration::from_secs(10), up.read_to_end(&mut response))
                        .await;
                if matches!(read, Ok(Ok(_))) {
                    let _ = sock.write_all(&response).await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    FakeEndpoint { addr, log, task }
}

// ================================================================ 真 mihomo（RAII）

/// 临时目录。`Drop` 无条件删（断言失败也要删）。
struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).expect("系统时间").as_nanos();
        let path = std::env::temp_dir().join(format!("ta-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 找 mihomo 二进制：`TA_MIHOMO` 优先，其次 M0 用过的默认路径。
///
/// **找不到就返回 Err 让测试失败**（不静默跳过 —— 静默跳过等于没有证据）。
fn find_mihomo() -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("TA_MIHOMO") {
        let path = PathBuf::from(&raw);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("TA_MIHOMO 指向的文件不存在：{raw}"));
    }
    let known = PathBuf::from(
        r"C:\Users\19404\workspace\.ta-tools\mihomo\mihomo-windows-amd64-compatible.exe",
    );
    if known.is_file() {
        return Ok(known);
    }
    Err(format!(
        "本机没有 mihomo 二进制：设 TA_MIHOMO，或把它放到 {}  —— \
         这条测试要证明\"出口真的变了\"，没有真内核就没有这个证据",
        known.display()
    ))
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 临时端口");
    let port = listener.local_addr().expect("取本地端口").port();
    drop(listener);
    port
}

/// 真 mihomo 进程。`Drop` 先杀进程再删目录（**留孤儿 = 留下占着 REST/listener 端口的进程**）。
struct Mihomo {
    // 字段顺序 = drop 顺序：child 先于 dir（其实自定义 Drop 已经先杀了，双保险）
    child: Child,
    pid: u32,
    controller: String,
    secret: String,
    dir: TempTree,
}

impl Drop for Mihomo {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mihomo {
    async fn start(binary: &PathBuf, config: &str) -> Result<Self, String> {
        let dir = TempTree::new("a1-kernel");
        let config_path = dir.path().join("kernel.generated.yaml");
        std::fs::write(&config_path, config).map_err(|e| format!("写内核配置失败：{e}"))?;
        let log_path = dir.path().join("kernel.log");
        let log_file =
            std::fs::File::create(&log_path).map_err(|e| format!("建日志文件失败：{e}"))?;

        let child = Command::new(binary)
            .arg("-d")
            .arg(dir.path())
            .arg("-f")
            .arg(&config_path)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(log_file))
            .spawn()
            .map_err(|e| format!("拉起 mihomo 失败（{}）：{e}", binary.display()))?;
        let pid = child.id();
        // 从生成配置里读回 controller/secret，避免"测试硬编码了另一份"
        Ok(Self { child, pid, controller: String::new(), secret: String::new(), dir }.with(config))
    }

    fn with(mut self, config: &str) -> Self {
        self.controller = extract_scalar(config, "external-controller");
        self.secret = extract_scalar(config, "secret");
        self
    }

    fn log_tail(&self, lines: usize) -> String {
        let path = self.dir.path().join("kernel.log");
        let Ok(text) = std::fs::read_to_string(path) else {
            return "(读不到内核日志)".into();
        };
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].join("\n")
    }

    /// 等 REST `/version` 通 + 每条 lane 的 listener 端口可连。
    async fn wait_ready(&self, listener_ports: &[u16], timeout: Duration) -> Result<(), String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        let mut last;
        loop {
            let rest_ok = client
                .get(format!("http://{}/version", self.controller))
                .bearer_auth(&self.secret)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            let mut listeners_ok = true;
            for port in listener_ports {
                let addr = SocketAddr::from(([127, 0, 0, 1], *port));
                let ok = tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(addr))
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);
                listeners_ok &= ok;
            }
            if rest_ok && listeners_ok {
                return Ok(());
            }
            last = format!("REST /version 通={rest_ok}，全部 listener 可连={listeners_ok}");
            if Instant::now() >= deadline {
                return Err(format!(
                    "mihomo 在 {timeout:?} 内没有就绪（{last}）\n内核日志尾部：\n{}",
                    self.log_tail(40)
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// 从自己写的配置里读一个顶层标量（避免测试里再硬编码一份 controller/secret）。
fn extract_scalar(config: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    config
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|rest| rest.trim().trim_matches('"').to_string())
        .unwrap_or_default()
}

/// 手写的**最小等价**内核配置。刻意与我们生成的配置同构：
/// 隔离项写死、`listeners[].proxy` 指向组名/DIRECT、组里选 `fake-proxy`。
fn mihomo_config(
    controller_port: u16,
    secret: &str,
    lane_direct_port: u16,
    lane_proxy_port: u16,
    fake_proxy_port: u16,
) -> String {
    format!(
        r#"# GENERATED BY TokenAgency — A1 验收手写的最小等价配置
mode: rule
log-level: info
mixed-port: 0
allow-lan: false
bind-address: 127.0.0.1
ipv6: false
find-process-mode: off
external-controller: 127.0.0.1:{controller_port}
secret: "{secret}"
profile: {{store-selected: true, store-fake-ip: false}}
tun: {{enable: false}}

proxies:
  - {{name: "fake-proxy", type: http, server: 127.0.0.1, port: {fake_proxy_port}, udp: false}}

proxy-groups:
  - {{name: "{PROXY_GROUP}", type: select, proxies: ["fake-proxy"]}}

listeners:
  - {{name: "lane-a-direct", type: mixed, listen: 127.0.0.1, port: {lane_direct_port}, proxy: "DIRECT", udp: false}}
  - {{name: "lane-b-proxy", type: mixed, listen: 127.0.0.1, port: {lane_proxy_port}, proxy: "{PROXY_GROUP}", udp: false}}

rules:
  - MATCH,DIRECT
"#
    )
}

// ================================================================ 网关侧替身

struct NoStore;

#[async_trait]
impl GatewayStore for NoStore {
    async fn record_usage(&self, _rec: UsageRecord) -> ta_core::error::Result<()> {
        Ok(())
    }
    async fn recent_summaries(&self, _limit: usize) -> ta_core::error::Result<Vec<RequestSummary>> {
        Ok(vec![])
    }
}

struct NoEvents;

impl ta_gateway::events::EventBus for NoEvents {
    fn publish(&self, _e: TaEvent) {}
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaEvent> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
}

struct NoPricing;

impl PricingEngine for NoPricing {
    fn resolve(&self, _model: &str, _provider: &ProviderId, _cred: &CredentialId) -> ResolvedPrice {
        ResolvedPrice::unpriced("CNY", 1_000_000.0)
    }
    fn cost(&self, _usage: &TokenUsage, price: &ResolvedPrice) -> Money {
        Money::new(0.0, price.currency.clone())
    }
}

/// 固定内核状态：本测试不测内核探针（真内核在另一条进程里，由 `Mihomo` 管）。
struct FixedKernel(KernelState);

impl KernelProbe for FixedKernel {
    fn state(&self) -> KernelState {
        self.0
    }
}

/// 记录每一次尝试（通道 / lane / 状态）。用来断言"这一请求走了哪条线"。
#[derive(Default)]
struct RecordingSink {
    attempts: Mutex<Vec<AttemptRecord>>,
}

impl RecordingSink {
    fn snapshot(&self) -> Vec<AttemptRecord> {
        self.attempts.lock().expect("sink 锁被毒化").clone()
    }
}

#[async_trait]
impl AttemptSink for RecordingSink {
    async fn on_attempt(&self, record: AttemptRecord) {
        self.attempts.lock().expect("sink 锁被毒化").push(record);
    }
}

/// **G4 的替身**：把上游响应的 `usage.total_tokens` 记进真账本。
///
/// 为什么必须有它：G4 还没接线，网关自己不会把用量喂给账本（`pipeline` 只在收到 402 时
/// 才写账本）。没有它，"把 A 的额度设小 → 跑请求直到切到 B"就无从发生。
/// **它只搬运数字，不做任何判定** —— 判定完全由 `QuotaLedgerImpl` 做。
struct LedgerMeter {
    ledger: Arc<QuotaLedgerImpl>,
    observed: Mutex<Vec<(String, u64)>>,
}

impl LedgerMeter {
    fn new(ledger: Arc<QuotaLedgerImpl>) -> Self {
        Self { ledger, observed: Mutex::new(Vec::new()) }
    }

    fn observed(&self) -> Vec<(String, u64)> {
        self.observed.lock().expect("meter 锁被毒化").clone()
    }
}

impl ResponseMeter for LedgerMeter {
    fn observe_full(&self, ctx: MeterContext, body: &Bytes, _status: u16) {
        let tokens = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("usage")?.get("total_tokens")?.as_u64())
            .unwrap_or(0);
        if tokens == 0 {
            return;
        }
        self.ledger.observe(
            &ctx.credential_id,
            QuotaObservation::Local { tokens, requests: 1, cost: 0.0 },
        );
        self.observed.lock().expect("meter 锁被毒化").push((ctx.credential_id.to_string(), tokens));
    }

    fn observe_stream(&self, _ctx: MeterContext, stream: ByteStream) -> ByteStream {
        // 本测试只跑非流式；这条保持**直通**（契约要求：可以看，不能攒）
        stream
    }
}

// ================================================================ 配置与请求

fn build_cfg(upstream: SocketAddr, with_backup: bool) -> ConfigDoc {
    let mut doc = ConfigDoc::default();
    doc.models.push(Model::new(MODEL));

    // 两条 lane 都要显式声明：`EngineOptions::default().strict_lane_check == true`，
    // 指向未声明 lane 的通道会被 `EliminateReason::NoLane` 淘汰（那正是我们想要的响亮失败）。
    doc.lanes.push(Lane::new(LANE_DIRECT, "线路A：本机直连（内核 DIRECT）", LaneKind::Direct));
    doc.lanes.push(Lane::new(
        LANE_PROXY,
        "线路B：走代理（内核组 → fake-proxy）",
        LaneKind::SubscriptionAuto,
    ));

    // 两个套餐指向**同一个假上游**（= 同一个"模型服务"），差别只在 lane。
    let base_url = format!("http://{upstream}/v1");
    doc.providers.push(Provider::new("p-a", "套餐A 上游", Protocol::Openai, base_url.clone()));
    doc.providers.push(Provider::new("p-b", "套餐B 上游", Protocol::Openai, base_url.clone()));

    doc.credentials.push(
        Credential::new(CRED_A, "p-a", "套餐A（额度很小）", "${secret:a}").with_quota(
            QuotaConfig {
                metric: QuotaMetric::Tokens,
                limit: Some(A_LIMIT_TOKENS),
                window: QuotaWindow::Month,
                soft_limit: 0.8,
                hard_limit: 1.0,
                ..Default::default()
            },
        ),
    );
    doc.credentials.push(Credential::new(CRED_B, "p-b", "套餐B（走代理）", "${secret:b}"));

    doc.channels.push(
        ChannelConfig::new(CH_A, MODEL, "p-a", CRED_A, MODEL)
            .with_lane(LANE_DIRECT)
            .with_priority(10),
    );
    doc.channels.push(
        ChannelConfig::new(CH_B, MODEL, "p-b", CRED_B, MODEL)
            .with_lane(LANE_PROXY)
            .with_priority(20),
    );

    // 组：默认选择器 `sticky_then_priority`，默认粘性 key 首项就是 `header:x-session-id`
    // （`StickyConfig::default()`）—— 这正是用户会用的形状，不用测试自己发明 key。
    let mut members = vec![CH_A, CH_B];
    if with_backup {
        // 配置换代用：多挂一个**优先级更高**的直连套餐 C。
        // 它存在的唯一理由是让"粘性"变成**可证伪**的：换代之后新会话会优先选 C，
        // 若老会话也跟着漂到 C，说明粘性根本没生效（B 只是"没得选"而已）。
        doc.providers.push(Provider::new("p-c", "套餐C 上游", Protocol::Openai, base_url.clone()));
        doc.credentials.push(Credential::new(
            CRED_C,
            "p-c",
            "套餐C（直连、额度充足）",
            "${secret:c}",
        ));
        doc.channels.push(
            ChannelConfig::new(CH_C, MODEL, "p-c", CRED_C, MODEL)
                .with_lane(LANE_DIRECT)
                .with_priority(5),
        );
        members.push(CH_C);
    }
    doc.channel_groups.push(
        ChannelGroup::new("grp-a1", MODEL, members).with_selector(Selector::StickyThenPriority),
    );
    doc
}

/// 打一条真请求进网关（走真 axum router → 真 Pipeline → 真 KernelDialer）。
async fn call_gateway(router: &axum::Router, session: &str) -> (u16, String) {
    let body = format!(
        r#"{{"model":"{MODEL}","messages":[{{"role":"user","content":"ping"}}],"stream":false}}"#
    );
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header("x-session-id", session)
        .body(axum::body::Body::from(body))
        .expect("构造入站请求");
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        tower::ServiceExt::oneshot(router.clone(), request),
    )
    .await
    .expect("网关必须在 30s 内回应（超时是卡死，不是业务失败）")
    .expect("路由必须能处理请求");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("读响应体");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn attempt_lane(record: &AttemptRecord) -> String {
    record.lane_id.as_ref().map(|l| l.to_string()).unwrap_or_else(|| "(无)".into())
}

// ================================================================ 测试本体

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a1_quota_exhaustion_switches_to_the_proxied_lane_and_the_exit_really_changes() {
    let binary = match find_mihomo() {
        Ok(path) => path,
        Err(why) => panic!("A1 验收起不来：{why}"),
    };

    // ---- 0. 两个可观测的本地端点
    let upstream = start_fake_upstream(UPSTREAM_BODY).await;
    let proxy = start_fake_proxy().await;
    println!(
        "[A1] 假上游 127.0.0.1:{}（模型服务），假代理 127.0.0.1:{}（出口）",
        upstream.addr.port(),
        proxy.addr.port()
    );

    // ---- 1. 真 mihomo：两条 lane = 两个 listener，一条 DIRECT、一条走 fake-proxy
    let controller_port = free_port();
    let lane_direct_port = free_port();
    let lane_proxy_port = free_port();
    let secret = "ta-a1-secret";
    let config = mihomo_config(
        controller_port,
        secret,
        lane_direct_port,
        lane_proxy_port,
        proxy.addr.port(),
    );
    let mut kernel =
        Mihomo::start(&binary, &config).await.unwrap_or_else(|e| panic!("拉起 mihomo 失败：{e}"));
    kernel
        .wait_ready(&[lane_direct_port, lane_proxy_port], Duration::from_secs(25))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    // 证据：内核是**真进程**，配置真的落盘了
    assert!(kernel.pid > 0, "内核必须有 pid（说明是真进程）");
    assert!(kernel.is_alive(), "内核进程不该在测试期间退出");
    let written = std::fs::read_to_string(kernel.dir.path().join("kernel.generated.yaml"))
        .expect("读回落盘的内核配置");
    assert!(written.contains("fake-proxy"), "落盘的配置里必须有假代理：\n{written}");
    println!(
        "[A1] mihomo pid={} listener: {}→DIRECT，{}→{}（组→fake-proxy）",
        kernel.pid, lane_direct_port, lane_proxy_port, PROXY_GROUP
    );

    // ---- 2. 网关：真装配（build_runtime），只有"计量"是 G4 的替身
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(1_700_000_000_000));
    let ledger =
        Arc::new(QuotaLedgerImpl::with_clock(Arc::new(MemoryQuotaBackend::new()), clock.clone()));
    ledger.set_quota_config(
        &CredentialId::new(CRED_A),
        QuotaConfig {
            metric: QuotaMetric::Tokens,
            limit: Some(A_LIMIT_TOKENS),
            window: QuotaWindow::Month,
            soft_limit: 0.8,
            hard_limit: 1.0,
            ..Default::default()
        },
    );
    let quota: Arc<dyn QuotaLedger> = ledger.clone();

    let deps = Arc::new(GatewayDeps {
        config: Arc::new(arc_swap::ArcSwap::from_pointee(build_cfg(upstream.addr, false))),
        store: Arc::new(NoStore),
        dialer: kernel_dialer(
            Arc::new(StaticLanePorts::new([
                (LaneId::new(LANE_DIRECT), lane_direct_port),
                (LaneId::new(LANE_PROXY), lane_proxy_port),
            ])),
            DialerOptions::default(),
        ),
        events: Arc::new(NoEvents),
        clock: clock.clone(),
        pricing: Arc::new(NoPricing),
        quota,
    });
    let sink = Arc::new(RecordingSink::default());
    let meter = Arc::new(LedgerMeter::new(ledger.clone()));
    let parts = RuntimeParts::new(
        Arc::new(StaticSecrets::new([
            ("${secret:a}", "sk-a"),
            ("${secret:b}", "sk-b"),
            ("${secret:c}", "sk-c"),
        ])),
        Arc::new(FixedKernel(KernelState::Ready)),
    )
    .with_sink(sink.clone())
    .with_meter(meter.clone());
    let runtime = build_runtime(&deps, parts);
    let router = ingress::router(runtime.state.clone());

    let cred_a = CredentialId::new(CRED_A);
    let cred_b = CredentialId::new(CRED_B);
    assert_eq!(
        ledger.state(&cred_a).kind,
        QuotaStateKind::Unknown,
        "起手时 A 还没被观测过 —— 不能被当成已耗尽（否则第一个请求就不会走 A）"
    );

    // ============================================================ 阶段一：切换前
    //
    // A 的额度上限 = 20 token，每次响应 12 token：第 1 次用完 12（60%），第 2 次用完 24 → 判耗尽。
    // 也就是说"跑请求直到切到 B"真的跑了 2 个请求，而不是一上来就切。

    let r1 = call_gateway(&router, SESSION).await;
    assert_eq!(r1.0, 200, "第 1 次请求必须成功：{}", r1.1);
    assert!(
        r1.1.contains("chatcmpl-a1"),
        "客户端拿到的必须是**上游**的响应体（证明请求真的到了模型服务）：{}",
        r1.1
    );

    // ★ 断言 1：切换前，上游收到、代理零记录 = 真的走直连
    assert_eq!(upstream.log.len(), 1, "假上游必须收到第 1 次请求");
    assert_eq!(
        proxy.log.len(),
        0,
        "第 1 个请求走的是 lane=direct，假代理必须是零记录。实际记录：{:?}",
        proxy.log.hits()
    );
    let after_1 = sink.snapshot();
    assert_eq!(after_1.len(), 1, "第 1 次请求应该只有一次尝试：{after_1:?}");
    assert_eq!(after_1[0].channel_id.as_str(), CH_A);
    assert_eq!(attempt_lane(&after_1[0]), LANE_DIRECT);
    assert_eq!(after_1[0].status, Some(200));
    assert_eq!(
        ledger.state(&cred_a).kind,
        QuotaStateKind::Ok,
        "12/20 = 60%，还没到 soft(80%)，A 必须仍然可用"
    );

    let r2 = call_gateway(&router, SESSION).await;
    assert_eq!(r2.0, 200, "第 2 次请求必须成功：{}", r2.1);
    assert_eq!(upstream.log.len(), 2, "假上游必须收到第 2 次请求");
    assert_eq!(proxy.log.len(), 0, "第 2 次仍然走直连：假代理必须还是零记录");
    let after_2 = sink.snapshot();
    assert_eq!(after_2.len(), 2, "第 2 次请求也只有一次尝试：{after_2:?}");
    assert_eq!(after_2[1].channel_id.as_str(), CH_A, "第 2 次还在用 A（额度没吃穿）");

    // 额度判定的**真账本**证据：24/20 > hard_limit(1.0) → 耗尽
    assert_eq!(
        ledger.state(&cred_a).kind,
        QuotaStateKind::Exhausted,
        "两次 12 token 之后 A 必须被判耗尽（上限 {A_LIMIT_TOKENS}）"
    );
    assert_eq!(
        meter.observed(),
        vec![(CRED_A.to_string(), UPSTREAM_TOKENS), (CRED_A.to_string(), UPSTREAM_TOKENS)],
        "被记进账本的用量必须来自上游响应（B 源），不能是猜的"
    );

    // ============================================================ 阶段二：切换后
    //
    // 从这里开始："出口真的变了"。注意断言的分工 ——
    //   * 假代理有记录 = **物理**证据（请求真的从代理口出去了）；
    //   * sink 里的 lane = 网关自己说走了哪条线（自述，单独不算证据）。

    let r3 = call_gateway(&router, SESSION).await;
    assert_eq!(r3.0, 200, "切换后的第 1 次请求必须成功：{}", r3.1);
    assert!(r3.1.contains("chatcmpl-a1"), "切换后客户端仍然拿到上游响应体：{}", r3.1);

    // ★ 断言 2：假代理有记录（= 出口真的变了），且假上游**仍然**收到请求
    assert_eq!(
        proxy.log.len(),
        1,
        "A 额度耗尽后必须换到走代理的 lane：假代理零记录意味着**出口根本没变**。\
         实际假上游 {} 笔、假代理 {} 笔",
        upstream.log.len(),
        proxy.log.len()
    );
    assert_eq!(upstream.log.len(), 3, "走了代理也必须真的到达模型服务（代理只是通道，不是终点）");
    let via_proxy = &proxy.log.hits()[0];
    let upstream_authority = upstream.addr.to_string();
    assert_eq!(
        via_proxy.host.as_deref(),
        Some(upstream_authority.as_str()),
        "假代理看到的 host 必须是假上游 —— 否则\"出口变了\"只是换了个地方失败"
    );
    let after_3 = sink.snapshot();
    assert_eq!(after_3.len(), 3, "第 3 次请求应该只有一次尝试：{after_3:?}");
    assert_eq!(after_3[2].channel_id.as_str(), CH_B, "第 3 次必须选到 B");
    assert_eq!(attempt_lane(&after_3[2]), LANE_PROXY, "B 的 lane 必须是走代理的那条");

    // ============================================================ 阶段三：粘性（可证伪的那种）
    //
    // ⚠️ 这里刻意**不**满足于"第 3、4 次都走了 B"：A 已经耗尽，B 是当时唯一可用候选，
    // 所以"又走了 B"在**粘性完全失效**时也照样成立 —— 那样的断言等于没断言。
    //
    // 做法：热更新配置，多挂一个**优先级更高**的直连套餐 C（也进同一个组），逼"粘性"表态：
    //   * 老会话（已有粘性记录 → B）：**必须继续走 B**（代理口）；
    //   * 换代后新开的会话（无粘性记录）：按新优先级走 C（直连口）。
    // 两者一起才证明"是老会话的粘性把它按在 B 上"，而不是"只有 B 可选"。
    // 顺带覆盖 `runtime.rs` 承诺的性质：**粘性跨配置换代保留**（不能因改配置就清零）。

    let r4 = call_gateway(&router, SESSION).await;
    assert_eq!(r4.0, 200, "第 4 次请求必须成功：{}", r4.1);
    assert_eq!(proxy.log.len(), 2, "第 4 次（同会话）仍在代理口上：{:?}", proxy.log.hits());

    let generation_before = runtime.router.generation();
    deps.config.store(Arc::new(build_cfg(upstream.addr, true)));
    let r5 = call_gateway(&router, SESSION).await;
    assert_eq!(r5.0, 200, "换代后老会话的请求必须成功：{}", r5.1);
    assert_ne!(
        runtime.router.generation(),
        generation_before,
        "热更新必须真的换代（否则下面的粘性断言是空的）"
    );

    // ★ 断言 3a：老会话被粘性按在 B 上（走代理口），尽管新配置里 C 的优先级更高
    let after_5 = sink.snapshot();
    assert_eq!(after_5.len(), 5, "第 5 次请求应该只有一次尝试：{after_5:?}");
    assert_eq!(
        after_5[4].channel_id.as_str(),
        CH_B,
        "老会话必须继续走 B（粘性），而不是漂到优先级更高的 C"
    );
    assert_eq!(attempt_lane(&after_5[4]), LANE_PROXY, "粘住的会话仍在走代理的那条线上");
    assert_eq!(proxy.log.len(), 3, "第 5 次请求也必须从代理口出去：{:?}", proxy.log.hits());

    // ★ 断言 3b：反证 —— 换代后**新**会话没有粘性记录，必须按新优先级走 C（直连口）
    let r6 = call_gateway(&router, FRESH_SESSION).await;
    assert_eq!(r6.0, 200, "换代后新会话的请求必须成功：{}", r6.1);
    let after_6 = sink.snapshot();
    assert_eq!(after_6.len(), 6, "第 6 次请求应该只有一次尝试：{after_6:?}");
    assert_eq!(
        after_6[5].channel_id.as_str(),
        CH_C,
        "新会话必须选到优先级更高的 C（否则说明换代没生效，粘性断言也就不成立）"
    );
    assert_eq!(attempt_lane(&after_6[5]), LANE_DIRECT);
    assert_eq!(
        proxy.log.len(),
        3,
        "新会话走的是直连的 C，绝不该在代理口留下记录：{:?}",
        proxy.log.hits()
    );
    assert_eq!(upstream.log.len(), 6, "6 个请求最终都到了模型服务");

    // ★ 断言 3c：不来回抖 —— 每个请求各自只有一次尝试，通道/线路序列完全确定
    let all = sink.snapshot();
    let channels: Vec<&str> = all.iter().map(|r| r.channel_id.as_str()).collect();
    assert_eq!(
        channels,
        vec![CH_A, CH_A, CH_B, CH_B, CH_B, CH_C],
        "通道序列必须恰好是 A,A,B,B,B,C"
    );
    let lanes: Vec<String> = all.iter().map(attempt_lane).collect();
    assert_eq!(
        lanes,
        vec![LANE_DIRECT, LANE_DIRECT, LANE_PROXY, LANE_PROXY, LANE_PROXY, LANE_DIRECT],
        "线路序列必须恰好是 直连,直连,代理,代理,代理,直连"
    );
    assert_eq!(proxy.log.len(), 3, "只有走 B 的 3 个请求从代理口出去");
    assert_eq!(upstream.log.len(), 6, "6 个请求最终都到了模型服务");
    assert_eq!(ledger.state(&cred_b).kind, QuotaStateKind::Ok, "B 没被观测过，不该被误判为耗尽");

    // ---- 物理证据：把假代理记录到的请求打出来（`--nocapture` 可见）
    println!("[A1 证据] 假代理（出口）记录到 {} 笔请求：", proxy.log.len());
    for (i, hit) in proxy.log.hits().iter().enumerate() {
        println!("  #{i} {} {}  host={:?}", hit.method, hit.target, hit.host);
    }
    println!("[A1 证据] 假上游（模型服务）共收到 {} 笔请求", upstream.log.len());
    println!("[A1 证据] 通道序列 = {channels:?}，线路序列 = {lanes:?}");
    println!(
        "[A1 证据] 账本 c-a={:?} used_tokens={} used_ratio={:?}",
        ledger.state(&cred_a).kind,
        ledger.state(&cred_a).used_tokens,
        ledger.state(&cred_a).used_ratio
    );
}

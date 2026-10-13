# ta-gateway — 本地网关本体（入站 / 全透传 / 路由管线 / 分发 / 协议适配）

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `crates/ta-gateway/src/lib.rs` | crate 根；`GatewayDeps` 是全部子模块取依赖的**唯一**入口，模块头写着 G1–G4 子包划分 |
| `crates/ta-gateway/src/ingress.rs` | axum 路由表、端点分类（`classify`）、本地密钥鉴权、`PreparedRequest` 的**唯一**构造点 |
| `crates/ta-gateway/src/passthrough.rs` | 全透传的唯一定点改写（头/体只改该改的那几处） |
| `crates/ta-gateway/src/pipeline.rs` | 12 步请求管线 + 失败转移（`Pipeline` / `PipelineDeps`） |
| `crates/ta-gateway/src/dispatch.rs` | 出站拨号：每 lane 一个 client，`KernelDialer` 在这里 |
| `crates/ta-gateway/src/adapt/mod.rs` | 协议适配入口：边缘 N 种协议 → 内部 1 种 IR |
| `crates/ta-gateway/src/runtime.rs` | 生产装配：`build_runtime` / `GatewayRuntime` / `LiveRouter` |
| `crates/ta-gateway/src/admin.rs` | 管理 API（回环内的本地接口），与用户流量分开 |

## 公开 API（对外符号）
| 符号 | 位置 | 作用 |
| --- | --- | --- |
| `GatewayDeps` / `traits::GatewayStore` / `traits::OutboundDialer` / `traits::EventBus` | `crates/ta-gateway/src/lib.rs` | 依赖注入的根；所有接缝都在这里，子模块不持有全局变量 |
| `Endpoint` / `classify` / `model_from_path` / `RequestSnapshot` / `BodyHints` / `body_hints` | `crates/ta-gateway/src/ingress.rs` | 入站分类与只读 hints（分类表与注册路由有测试守住不漂移） |
| `build_prepared_request` / `PrepareInput` / `PreparedRequest` / `upstream_path_for` | `crates/ta-gateway/src/ingress.rs` | 造上游请求的**唯一**实现（总体转发 + 定点改写 + 重算 `content-length`） |
| `Router` / `IngressState` / `IngressOptions` / `RequestPipeline` / `GatewayGate` | `crates/ta-gateway/src/ingress.rs` | 入站装配与"管线"接缝；`router` 绑的路由表就是接口契约 |
| `PassthroughPolicy` / `FullPassthrough` / `HOP_BY_HOP_HEADERS` / `is_hop_by_hop` | `crates/ta-gateway/src/passthrough.rs` | 透传策略与跳头处理 |
| `auth_headers` / `join_upstream_url` / `apply_query_auth` / `canonical_headers` / `set_content_length` | `crates/ta-gateway/src/passthrough.rs` | 转发路径与探测路径**共用**的 URL/鉴权/头实现（两处口径只有一份） |
| `Pipeline` / `PipelineDeps` / `SecretResolver` / `StaticSecrets` | `crates/ta-gateway/src/pipeline.rs` | 管线本体与密钥解析接缝 |
| `AttemptSink` / `AttemptRecord` / `RequestTraceSummary` / `ResponseMeter` / `MeterContext` | `crates/ta-gateway/src/pipeline.rs` | 落账与 trace 的接缝（`meter: None` = 计量没接上） |
| `OutboundDialer` / `UpstreamRequest` / `UpstreamResponse` / `HealthReport` | `crates/ta-gateway/src/dispatch.rs` | 出站接缝：**全仓唯一的出网动作**都在它后面 |
| `KernelDialer` / `LanePorts` / `SharedPorts` / `StaticLanePorts` / `DialerOptions` | `crates/ta-gateway/src/dispatch.rs` | 经内核 lane 端口出站（`is_direct_lane` 判直连） |
| `ProtocolAdapter` / `adapter_for` / `can_translate` / `translate_request` / `translate_response` / `verify_no_loss` | `crates/ta-gateway/src/adapt/mod.rs` | 协议适配与"转换会不会丢内容"的判定 |
| `IrRequest` / `IrMessage` / `IrPart` / `IrTool` / `IrResponse` / `IrStopReason` | `crates/ta-gateway/src/adapt/ir.rs` | 内部中间表示（跨协议那条路才走它） |
| `StreamAdapter` / `stream_adapter_for` / `translate_stream_event` / `IrStreamEvent` | `crates/ta-gateway/src/adapt/stream.rs` | 流式适配（事件翻译、SSE 分帧） |
| `SseTranslator` / `translate_sse_stream` | `crates/ta-gateway/src/adapt/session.rs` | SSE 会话级翻译（跨协议流的会话状态） |
| `Loss` / `describe` / `fatal_losses` | `crates/ta-gateway/src/adapt/loss.rs` | 转换损失：致命损失拒绝转发，不许静默丢 |
| `TaEvent` / `EventBus` / `BatchingEventBus` | `crates/ta-gateway/src/events.rs` | 后端 → 前端事件（事件名是契约，有守卫逐字核对） |
| `UsageMeter` | `crates/ta-gateway/src/meter.rs` | 计量实现（响应 usage → 账本） |
| `LoggingSink` / `TraceStoreSink` / `TeeAttemptSink` | `crates/ta-gateway/src/trace.rs` | 尝试记录扇出（日志 / 落库 / 两者） |
| `UpstreamProbe` / `ProbeProviderRequest` / `ProbeResult` / `UpstreamModelEntry` / `probe_lane` / `read_body_bounded` | `crates/ta-gateway/src/probe.rs` | 受控出站探测（拉上游模型清单 / 测连接），只经 dialer |
| `RuntimeParts` / `RoutingPlanner` / `PolicySelectorPlanner` / `LiveRouter` / `GatewayRuntime` / `build_runtime` / `kernel_dialer` | `crates/ta-gateway/src/runtime.rs` | 生产装配：一次绑定一次求值、跟配置换代 |
| `AdminBackend` / `AdminState` / `check_admin_token` | `crates/ta-gateway/src/admin.rs` | 管理 API 的接缝与令牌校验 |

## 依赖
- **上游 crate**：`ta-core`（领域类型 / 路由 / 定价 / 计量）、`ta-store`（配置与账本，只经 `GatewayStore` 接缝）、`ta-kernel`（lane 端口）、`ta-subscription`（订阅下载）、`ta-secrets`（密钥解析）。
- **关键第三方**：`axum` / `hyper` / `http` / `http-body-util`（入站与响应）、`tokio` / `tokio-stream` / `tokio-util`（异步与流式）、`reqwest` + `rustls`（出站，唯一 HTTP client）、`tower` / `tower-http`（路由与服务层）、`arc-swap`（配置热换代）、`bytes`（零拷贝体）、`async-trait`（接缝 trait）、`serde` / `serde_json`（协议体）、`tracing`（trace/日志）、`regex`、`chrono`、`nanoid`（`request_id`）、`sha2` / `base64`、`zstd`（压缩体）、`indexmap`（保序）。
- **不许有**：`tauri`（外壳只依赖网关，反过来不行）。

## OWNER
- 整体：`docs/12-work-breakdown.md` §1.2 的 **E5 网关官**（主岗文件范围就是 `crates/ta-gateway/**`）。
- 子包划分（见 `crates/ta-gateway/src/lib.rs` 的模块头）：**G1** `ingress` / `passthrough` / `admin`；**G2** `pipeline` / `dispatch`；**G3** `adapt`；**G4** `trace` / `events` / `meter`。
- G1–G4 清单之外的三个文件：`crates/ta-gateway/src/probe.rs` / `crates/ta-gateway/src/runtime.rs` / `crates/ta-gateway/src/traits_store.rs` → **未标注**（lib.rs 的 G1–G4 清单里没有它们）。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | `crates/ta-gateway/src/**` 内联 `#[cfg(test)]`：17 个文件，共 286 条 `#[test]` / `#[tokio::test]`；最重的是 `crates/ta-gateway/src/ingress.rs`（48 条）、`crates/ta-gateway/src/pipeline.rs`（30 条）、`crates/ta-gateway/src/probe.rs`（28 条） |
| 集成测试 | `crates/ta-gateway/tests/` 目录**当前为空**（原真机/端到端用例已删除）；真机 e2e 走 `scripts/e2e/smoke.mjs` |
| 守卫 | 见下（这批守卫是"跨切面断言"，坏了就是设计约束破了） |

点名几个有牙的守卫：
- `protocol_pair_availability_is_declared_not_guessed`（`crates/ta-gateway/src/adapt/mod.rs`）—— 协议对能不能转必须是**声明**出来的，不许靠猜。
- `gemini_is_never_a_client_protocol`（`crates/ta-gateway/src/adapt/stream.rs`）—— Gemini 只作上游协议，不做客户端协议。
- `production_slice_has_no_second_egress_path`（`crates/ta-gateway/src/probe.rs`）—— 生产段不许出现第二条出网路径（`reqwest::` / `TcpStream` 等）。
- `runtime_has_no_second_router_assembly`（`crates/ta-gateway/src/runtime.rs`）—— 路由装配只有一处。
- `bind_address_must_be_loopback`（`crates/ta-gateway/src/ingress.rs`）—— 监听只许回环（`0.0.0.0` 永不允许）。

## 关键数据流 / 边界
- 请求链：`crates/ta-gateway/src/ingress.rs`（分类 → 鉴权 → 限流 → 快照）→ `crates/ta-gateway/src/pipeline.rs`（hints → 策略 → 选线路/通道）→ `build_prepared_request`（定点改写）→ `crates/ta-gateway/src/dispatch.rs`（经 lane 出站）→ 响应回来走 `meter` 落账 + `trace` 记录。
- **协议适配**只在跨协议时进场：`crates/ta-gateway/src/adapt/mod.rs` 把客户端请求译进 `IrRequest`、把上游响应译回客户端协议；**同协议走 RAW 零改动透传**，不碰 IR。流式那条路在 `crates/ta-gateway/src/adapt/stream.rs`（按事件翻译，不许攒整条流）。
- **本地作答**：`/v1/models`（含不带 `/v1` 的形态）不再透传上游，直接用我们自己的模型回答 —— 见 `crates/ta-gateway/src/ingress.rs` 里 `models_response` / `models_list_body` 一带（依据 [docs/28](../../docs/28-lines-belong-to-providers.md) §4）。
- 边界：网关**不碰 UI、不直接读写文件**；持久化只经 `GatewayStore`（实现由 `crates/ta-store` 提供）；出网**只有** `OutboundDialer` 一条路（线路策略就在它后面）。

## 已知陷阱
- **全透传只许在 `crates/ta-gateway/src/passthrough.rs` 定点改写**：在 `crates/ta-gateway/src/pipeline.rs` 自己拼头/拼 body 就是把这条铁律交给第二个人守（必然漂移）。
- `crates/ta-gateway/src/adapt/` 里的 `stream_is_out_of_band` 是废弃路径，Gemini 永不是客户端协议 —— 别为不可达场景加代码（有守卫钉住）。
- `meter: None` 是合法装配（计量没接上），但生产装配必须给 `Some(...)`：`None` 会让"有数据"变成假的。
- `header_diff` 正常必须为空；非空 = 网关自己动了客户端的头（[docs/14-observability.md](../../docs/14-observability.md)）。
- `request_id` 是贯穿全链路的关联 id：排查时用它追日志行，字段名受 `docs/14-observability.md` §5 白名单约束，别随手新造字段名。

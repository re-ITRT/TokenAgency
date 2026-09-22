# 10 · 接口文档（一）：对外契约与前端契约

> **自顶向下第 1 层。** 本文定义"外部世界能看到的一切"：客户端 → 网关的 HTTP 契约、GUI → Rust 的 IPC 契约、CLI → 管理 API 的契约。
> 下一层（crate 之间的 Rust trait、内核接口、存储接口、错误码）见 [11 内部契约](11-internal-contracts.md)；据此拆分的工作包见 [12 工作分解](12-work-breakdown.md)。

---

# 0. 接口全景（自顶向下的稳定度排序）

| 层 | 接口 | 谁 ↔ 谁 | 稳定度 | 本文档 |
|---|---|---|---|---|
| **L0** | 网关 HTTP API | LLM 客户端 ↔ TokenAgency | ★★★ 最稳定，改动即破坏兼容 | §1 |
| **L1** | Tauri IPC（命令 + 事件） | 前端 ↔ Rust | ★★ 常改，由 specta 生成类型 | §2 |
| **L2** | 管理 HTTP API | CLI / 脚本 ↔ TokenAgency | ★★ 稳定，供自动化 | §3 |
| **L3** | Rust crate trait | crate ↔ crate | ★★ | [11](11-internal-contracts.md) §1 |
| **L4** | 内核接口 | TokenAgency ↔ mihomo | ★ 随内核版本 | [11](11-internal-contracts.md) §2 |
| **L5** | 上游 LLM API | TokenAgency ↔ 服务商 | ★★★ 由上游决定 | §1.6（透传规则） |
| **L6** | 订阅服务器 | TokenAgency ↔ 机场 | ★★ | [11](11-internal-contracts.md) §3 |
| **L7** | 存储 | TokenAgency ↔ SQLite | ★★ | [11](11-internal-contracts.md) §4 |
| **L8** | 错误码 | 全层共用 | ★★★ | [11](11-internal-contracts.md) §5 |

**设计原则**：越靠上越稳定。L0 一旦定稿，L1–L7 的实现怎么换都不该影响客户端。

---

# 1. L0 · 网关对外 HTTP API

## 1.1 端点与监听

| 项 | 值 |
|---|---|
| 监听 | `127.0.0.1:8787`（**只绑 loopback，永不监听 0.0.0.0**） |
| 协议 | HTTP/1.1 与 HTTP/2（先不支持 h2c 明文升级的边界情况） |
| 鉴权 | 可选：`Authorization: Bearer <local_key>` 或 `x-api-key: <local_key>`；未配置 `local_key` 时不校验 |
| 客户端 base_url | OpenAI 风格填 `http://127.0.0.1:8787/v1`；Anthropic 风格填 `http://127.0.0.1:8787` |

## 1.2 路由表

| 方法 | 路径 | 处理 |
|---|---|---|
| POST | `/v1/chat/completions` | OpenAI Chat Completions → 路由 → 透传/适配 |
| POST | `/v1/responses` | OpenAI Responses API |
| POST | `/v1/messages` | Anthropic Messages |
| POST | `/v1/embeddings` | 透传（不参与粘性/额度账本以外的特殊处理） |
| POST | `/v1/images/generations`、`/v1/audio/*`、`/v1/files` | **纯透传**：按模型名选通道后原样转发 |
| GET | `/v1/models` | 默认**原样透传上游**；`gateway.expose_models_with_capabilities=true` 时注入能力字段 |
| GET | `/v1/models/{id}` | 同上 |
| GET | `/healthz` | 本地健康检查（不走上游）：`{"status":"ok","kernel":"ready"}` |
| ANY | `/*` | 通配透传：按 `x-tokenagency-model` 头或路径中提取的模型名选通道 |

> 通配规则：若无法确定模型名 → 用 `net.default_lane` + `gateway.fallback_provider`；若也没有 → 返回 `no_route` 错误（400）。

### `/healthz` 的确切语义（第 9 轮补充，G1 实现后定稿）

文档原来只写了路径与返回体，没写三件容易被实现者各写一套的事。**定稿如下**：

1. **不校验本地密钥，恒 `200`。** 它是给外部探活工具和用户 `curl` 用的 —— 要求带密钥就失去意义。
   它只绑回环，不构成暴露面。**内核状态用 `kernel` 字段体现，不用 HTTP 状态码体现。**
2. `kernel` 取值域见 [11 §2] 的 `KernelState`（如 `ready` / `starting` / `failed` / `stopped`）。
3. **未注册的路径 + 未注册的方法不会落到 `ANY /*`。** 已注册路径配错误方法 → **`405`**。
   理由：把"客户端用错了 API"悄悄当通配请求转发给上游，会产生**看起来像上游故障**的假象，
   而 405 能立刻说清问题。**这条是有意选择的，不是 axum 的默认行为泄漏。**

## 1.3 请求处理契约（对客户端可见的行为）

**必须保证**：

1. **全透传**。除下表 4 处，客户端请求的**每一个字节**都要到达上游。

| 例外 | 处理 |
|---|---|
| `host` | 重写为上游 host |
| `content-length` | 按实际 body 重算 |
| `authorization` / `x-api-key` / `api-key` | 替换为该通道凭据的 key |
| hop-by-hop：`connection`、`keep-alive`、`transfer-encoding`、`upgrade`、`proxy-*`、`te`、`trailer` | 剥离 |

2. **body 只做定点 patch**：默认只改 `model`（映射为 `upstream_model`）。其余字段、未知字段、未知 part 类型**一律保留**。

3. **流式**：`stream: true` 时，响应以 SSE 零缓冲转发；**不聚合、不重排、不重写 chunk**。

4. **不注入**。默认**不**给请求加任何字段。特例：`metering.inject_stream_usage: true` 时会给 OpenAI 流式请求加 `stream_options.include_usage=true` —— 这是**显式的请求修改**，默认关闭，UI 上标注。

5. **不重试已输出的响应**。已向客户端写出第一个字节后，任何上游失败都只记录，不再重试。

## 1.4 响应头（TokenAgency 附加）

| 头 | 说明 | 开关 |
|---|---|---|
| `x-tokenagency-request-id` | 请求 ID，与 trace 对应 | 默认开 |
| `x-tokenagency-channel` | 实际使用的通道 ID | 默认开 |
| `x-tokenagency-lane` | 实际使用的线路 ID | 默认开 |
| `x-tokenagency-attempts` | 尝试次数 | 默认开 |
| `x-tokenagency-usage-source` | `upstream` \| `estimated` | 默认开 |

> 全部可通过 `gateway.debug_headers: false` 关闭（有些客户端对未知头敏感）。
> **绝不修改上游返回的头**（除 hop-by-hop 与 `content-length`）。

## 1.5 错误信封

所有由 TokenAgency 自身产生的错误（非上游透传）统一为：

```json
{
  "error": {
    "type": "tokenagency_no_viable_channel",
    "message": "模型 gpt-4o 无可用通道：3 个候选均被策略排除",
    "code": "TA-2003",
    "retryable": false,
    "request_id": "req_01HQZX8V2K",
    "trace_hint": "ta trace req_01HQZX8V2K",
    "details": { "eliminated": [ { "channel": "ch-relay-b", "reason": "tagged_out:no-vision" } ] }
  }
}
```

- **上游错误原样透传**（状态码、body、头都不动），只附加 `x-tokenagency-*` 头。
- **只有网关自身判断失败**才用上面的信封，且 `type` 一律以 `tokenagency_` 前缀标明来源。完整错误码表见 [11 §5](11-internal-contracts.md)。

## 1.6 透传语义细则（L5 边界）

| 情况 | 行为 |
|---|---|
| 客户端协议 == 上游协议 | **RAW 路径**：只用 `serde_json::Value` 解析 → 定点 patch → 原样序列化。**不做字段核对**（因为没有转换） |
| 客户端协议 != 上游协议 | **IR 路径**：转 canonical → 目标协议编码 → **字段核对**。有 loss 时默认 `on_loss: reject`（返回 415），不转发残缺请求 |
| 图片块无法映射 | 直接拒绝（415）。**绝不替换为 `"[image]"` 文本** |
| `content-encoding` | 不透明转发；不解压再压缩 |
| 大 body（> `gateway.max_body_mb`，默认 32MB） | 拒绝 413，避免内存爆 |

---

# 2. L1 · Tauri IPC 契约

## 2.1 命名与类型规则

- 命令名：`snake_case`，按资源前缀：`<resource>_<verb>`，如 `provider_list`、`channel_upsert`。
- 类型：Rust 结构体经 `tauri-specta` 生成 `src/shared/bindings.ts`。**前端禁止手写这些类型，禁止手改生成文件**。
- 所有命令返回 `Result<T, IpcError>`；`IpcError` 结构：

```ts
type IpcError = {
  code: string;          // 见 11 §5 错误码表
  message: string;       // 面向用户的中文可读信息
  details?: unknown;     // 结构化细节（字段路径、冲突项等）
}
```

- **前端不直连 8788**；所有管理操作走 IPC。
- **命令体不超过 20 行**：超过就下沉到 crate（硬规定）。

## 2.2 命令清单（按资源）

### app / 应用

| 命令 | 入参 | 返回 |
|---|---|---|
| `app_status` | — | `AppStatus { gateway: Running\|Stopped, kernel: KernelStatus, version, uptime_s, active_policy_id, default_lane_id, current_node }` |
| `app_pause_gateway` / `app_resume_gateway` | — | `AppStatus` |
| `app_open_dir` | `kind: "logs"\|"config"\|"data"` | `void` |
| `app_clipboard_access_info` | — | `string`（含 base_url 与 local_key 的可粘贴文本） |
| `app_check_update` | — | `UpdateInfo?` |
| `app_get_autostart` | — | `AutostartState`（见下） |
| `app_set_autostart` | `enabled: bool, mode?: "tray"\|"window", delay_s?: u32` | `AutostartState` |
| `app_repair_autostart` | — | `AutostartState`（把系统状态重新对齐到期望状态） |

### 开机自启动的 IPC 语义（特别说明）

**这个功能的接口设计有个关键点：配置值 ≠ 系统实际状态。** 用户可能在任务管理器里禁用了启动项，注册表条目也可能被别的清理软件删掉。

```ts
type AutostartState = {
  /** 配置里的期望状态 */
  desired: boolean;
  /** 从操作系统读到的实际状态 —— UI 开关显示的是这个 */
  actual: boolean;
  /** 两者不一致 */
  drifted: boolean;
  /** 平台机制：windows-run-key | macos-launch-agent | linux-xdg | unsupported */
  mechanism: string;
  /** 实际注册的命令行（便于排查"指向了一个已经不存在的 exe"） */
  registered_command?: string;
  mode: "tray" | "window";
  delay_s: number;
  /** 平台不支持时的可读原因（Linux 无桌面环境等） */
  unsupported_reason?: string;
};
```

规则（对应 [09 §1.2](09-architecture-design.md) 的 E1–E5）：

1. **`app_get_autostart` 的 `actual` 必须真的去读操作系统**，不许直接回显配置值。
2. **`app_set_autostart(true)`** = 写系统 + 写配置；失败时**回滚配置**，返回错误，不留"配置说开、系统没开"的假象。
3. **`app_set_autostart(false)`** = 删系统条目 + 写配置。
4. `drifted: true` 时 UI 必须显式提示，并提供 `app_repair_autostart`，**绝不自动改回**（E3）。
5. `unsupported` 时开关置灰并显示 `unsupported_reason`，而不是假装成功。
6. `app_set_autostart` 成功后要**重新读一次系统状态**填 `actual`，而不是假定写入成功。

> **第 10 轮勘误**：本节末尾曾残留一行 `| app_set_autostart | enabled: bool | void |` 的表行，
> 与本命令在 §2.2 命令清单里的正规定义**自相矛盾**（入参与返回值都不同）。
> 已删除 —— 实现者若照那行写会产生签名不一致。**残留表行比缺文档危险**：它看起来是权威的。

### config / 配置

| 命令 | 入参 | 返回 |
|---|---|---|
| `config_get` | — | `ConfigDoc`（完整配置树） |
| `config_validate` | `doc: ConfigDoc` | `ValidationReport { ok, errors: [{path, code, message}], warnings: [...] }` |
| `config_apply` | `doc: ConfigDoc` | `ApplyResult { applied, restart_required: bool, kernel_reload: bool }` |
| `config_export_yaml` | `with_secrets: bool` | `string` |
| `config_import_yaml` | `yaml: string, mode: "merge"\|"replace"` | `ApplyResult` |
| `config_history_list` | `limit: u32` | `Snapshot[]` |
| `config_rollback` | `snapshot_id: string` | `ApplyResult` |
| `config_set_file_watch_mode` | `enabled: bool, path?: string` | `ApplyResult` |

### provider / 服务商

| 命令 | 入参 | 返回 |
|---|---|---|
| `provider_list` | — | `ProviderView[]`（含 24h 成功率、延迟、挂载数、状态灯） |
| `provider_get` | `id` | `ProviderView` |
| `provider_upsert` | `provider: Provider` | `Provider` |
| `provider_delete` | `id, force: bool` | `void`（有引用且未 force → 返回 `referenced` 错误） |
| `provider_test` | `id, model?: string` | `ProbeResult { ok, status, latency_ms, note }` |
| `provider_echo` | `id, model, with_image: bool` | `EchoResult { sent_headers, upstream_headers, sent_body_shape, upstream_body?, latency_ms }` ← **"谁吞了字段"的答案页** |

### credential / 凭据

| 命令 | 入参 | 返回 |
|---|---|---|
| `credential_list` | — | `CredentialView[]`（含额度状态、已用量） |
| `credential_upsert` | `credential: Credential` | `Credential` |
| `credential_delete` | `id, force` | `void` |
| `credential_set_secret` | `id, plaintext: string` | `void`（写入钥匙串，不入库） |
| `credential_clear_quota` | `id` | `QuotaState`（用户手动"我充值了"） |
| `credential_set_enabled` | `id, enabled: bool` | `void` |
| `secrets_scan_plaintext` | — | `PlaintextFinding[]` |
| `secrets_migrate` | `names: string[]` | `void` |

### model / 模型目录

| 命令 | 入参 | 返回 |
|---|---|---|
| `model_list` | — | `ModelView[]`（含通道数、上游名映射、价格、暴露开关） |
| `model_upsert` / `model_delete` | | |
| `model_import_from_provider` | `provider_id, prefix_filter?` | `ImportPreview { models: [...], skipped: [...] }` |
| `model_apply_import` | `preview_token, selected: string[]` | `ApplyResult` |
| `model_bulk_update` | `ids: string[], patch: ModelPatch` | `ApplyResult` |

### channel / 通道

| 命令 | 入参 | 返回 |
|---|---|---|
| `channel_matrix` | — | `MatrixView { models: ModelRow[], credentials: CredentialCol[], cells: Cell[] }` |
| `channel_upsert` | `channel: Channel` | `Channel` |
| `channel_delete` | `id` | `void` |
| `channel_set_enabled` | `id, enabled` | `void` |
| `channel_bulk_assign_lane` | `channel_ids: string[], lane_id: string` | `ApplyResult` |
| `channel_set_tags` | `channel_ids: string[], add: string[], remove: string[]` | `ApplyResult` |

### group / 通道组

| 命令 | 入参 | 返回 |
|---|---|---|
| `group_list` / `group_upsert` / `group_delete` | | |
| `group_set_sticky` | `id, sticky: StickyConfig` | `void` |
| `group_release_sticky` | `group_id, key?` | `void`（清粘性） |

### policy / 路由策略

| 命令 | 入参 | 返回 |
|---|---|---|
| `policy_list` | — | `Policy[]`（按 priority 排序） |
| `policy_upsert` / `policy_delete` | | |
| `policy_reorder` | `ordered_ids: string[]` | `void` → 重写 priority |
| `policy_set_enabled` | `id, enabled` | `void` |
| `policy_simulate` | `SimulateInput` | `SimulateResult` |

```ts
type SimulateInput = {
  model: string; has_image: boolean; stream: boolean; est_input_tokens: number;
  headers?: Record<string,string>; session_key?: string; now?: number;
}
type SimulateResult = {
  matched_policy?: { id: string; name: string };
  candidates: { channel_id: string; ok: boolean; reason?: string }[];
  chosen?: { channel_id: string; sticky_hit: boolean; sticky_key?: string };
  failover_chain: string[];
  lane?: { id: string; name: string; node?: string; latency_ms?: number };
  outgoing_equals_client: boolean;   // 恒为 true（除 model/auth 外）
  eliminated: { channel_id: string; reason: string }[];
}
```

### lane / 线路

| 命令 | 入参 | 返回 |
|---|---|---|
| `lane_list` | — | `LaneView[]`（含 type、节点数、当前节点、延迟、健康） |
| `lane_upsert` / `lane_delete` | | |
| `lane_test` | `id, node?` | `LaneTestResult { node, latency_ms, ok }[]` |
| `lane_set_node` | `id, node: string` | `void`（`select` 组手动钉节点） |
| `lane_assignments` | — | `{ model_id, channel_id, lane_id, source: "channel"\|"credential"\|"provider"\|"default" }[]` |
| `lane_reverse_lookup` | `lane_id` | `{ channel_ids: string[]; model_ids: string[] }` ← 删订阅前看这个 |
| `lane_pick_options` | — | `LanePickOption[]`（通道矩阵下拉的选项树，已按订阅分组，含延迟） |

### subscription / 订阅

| 命令 | 入参 | 返回 |
|---|---|---|
| `subscription_list` | — | `SubscriptionView[]`（节点数/组数/流量/到期/最后更新/解析器） |
| `subscription_upsert` / `subscription_delete` | | |
| `subscription_refresh` | `id?, force?: bool` | `RefreshResult { ok, node_count, group_count, error? }` |
| `subscription_preview` | `url, user_agent?, format?` | `PreviewResult { groups: string[], nodes: string[], sample: Node[] }` |
| `subscription_groups` | `id` | `{ name, type, node_count }[]` |
| `subscription_nodes` | `id, group?` | `NodeView[]` |
| `subscription_rollback` | `id, version_index` | `void` |
| `subscription_rules_raw` | `id` | `string?`（只读展示订阅原本的 rules） |

### kernel / 内核

| 命令 | 入参 | 返回 |
|---|---|---|
| `kernel_status` | — | `KernelStatus { state: "stopped"\|"starting"\|"ready"\|"restarting"\|"failed", version?, pid?, restarts, last_error? }` |
| `kernel_start` / `kernel_stop` / `kernel_restart` | — | `KernelStatus` |
| `kernel_reload_config` | — | `KernelStatus` |
| `kernel_generated_config` | — | `string`（只读预览生成的 YAML） |
| `kernel_log_tail` | `lines: u32` | `string` |
| `kernel_set_provider` | `provider: "external"\|"bundled", path?: string` | `KernelStatus` |
| `kernel_probe_path` | — | `{ found: bool, path?: string, version?: string }` |

### usage / 用量与成本中心

| 命令 | 入参 | 返回 |
|---|---|---|
| `usage_dashboard` | `range: TimeRange` | `DashboardData`（见 §2.4） |
| `usage_request_stream_snapshot` | `limit` | `RequestSummary[]`（首次填充，之后靠事件） |
| `usage_query` | `UsageQuery` | `UsageTable`（见下） |
| `usage_export_csv` | `UsageQuery, path` | `void` |
| `usage_token_kinds` | — | `string[]`（当前出现过的所有 token 类型） |
| `pricing_list` / `pricing_upsert` / `pricing_delete` | | |
| `pricing_resolve` | `model, provider?, credential?` | `ResolvedPrice { rates, multiplier, source, currency }` |
| `pricing_import_builtin` | — | `ApplyResult` |
| `pricing_unpriced_models` | — | `string[]` |

```ts
type UsageQuery = {
  range: TimeRange;                       // { from, to } | { preset: "today"|"7d"|"30d"|"month" }
  group_by: ("time"|"credential"|"provider"|"model"|"channel"|"client"|"lane"|"node")[];
  time_bucket?: "hour"|"day"|"week"|"month";
  metrics: ("requests"|"cost"|"cache_hit_rate"|"avg_latency"| TokenKind)[];  // TokenKind 见 11 §5
  filters?: { model?: string[]; credential?: string[]; provider?: string[];
              client?: string[]; status?: ("ok"|"retried"|"failed")[] };
  format: "table"|"stacked_bar"|"area";
  limit?: number;
}
```

### log / 请求日志

| 命令 | 入参 | 返回 |
|---|---|---|
| `log_query` | `LogQuery` | `LogPage { rows: TraceRow[], total, cursor? }` |
| `log_get_trace` | `request_id` | `TraceDetail`（尝试链 + 耗时瀑布 + header_diff + body 结构） |
| `log_set_mode` | `mode: "off"\|"meta"\|"shape"\|"full"` | `void` |
| `log_set_retention` | `retention: "7d"\|"30d"\|"90d"\|"forever"` | `void` |
| `log_storage_stats` | — | `{ rows, bytes, oldest_ts, newest_ts }` |
| `log_clear` | `mode: "all"\|"bodies_only"` | `void` |
| `log_export` | `LogQuery, path` | `LogExportReceipt` |

`LogExportReceipt`（`log_export` 的回执）：

```ts
type LogExportReceipt = {
  /** **实际落盘路径** —— 后端解析后的最终值。传目录进来时后端会补
   *  `tokenagency-diagnostic-<时间戳>.zip`，所以它**不等于**入参 path */
  path: string;
  /** 磁盘上的真实字节数 */
  bytes: number;
};
```

> **为什么不回 `void`**：界面需要如实显示"导出到了哪里"。沿用 `void` 时前端只能显示自己
> 传进去的路径，而那个路径上可能并没有文件（传目录时确实没有）—— 用户会以为后端确认了。
> 导出失败时按 `TaError` 报错，**不返回半成品回执**。

### onboarding / 首启向导

| 命令 | 入参 | 返回 |
|---|---|---|
| `onboarding_detect` | — | `{ kernel_found, existing_config, suggested_providers }` |
| `onboarding_apply` | `OnboardingInput` | `ApplyResult` |
| `onboarding_test_all` | — | `ConnectivityRow[]`（key × lane 的连通性矩阵） |

### tray / 托盘（前端也可调用，保证逻辑只有一份）

| 命令 | 入参 | 返回 |
|---|---|---|
| `tray_state` | — | `TrayState`（图标色、tooltip 文案、菜单项状态） |
| `tray_set_active_policy` | `policy_id` | `void` |
| `tray_set_global_lane` | `lane_id` | `void` |
| `tray_emergency` | `mode: "direct_all"\|"pause_all"\|"vision_only"\|"restore"` | `void` |

## 2.3 事件清单（Rust → 前端）

| 事件名 | 载荷 | 频率控制 |
|---|---|---|
| `request:new` | `RequestSummary[]`（**批量数组**） | 环形缓冲 + **200ms 合并**，最多 50 条/次 |
| `usage:tick` | `{ today: UsageTotals, quota: QuotaDelta[] }` | 1s 节流 |
| `channel:state` | `{ channel_id, state, until? }` | 状态变化时 |
| `lane:health` | `{ lane_id, node?, latency_ms, ok }` | 测速完成时 |
| `kernel:status` | `KernelStatus` | 状态变化时 |
| `config:changed` | `{ source: "db"\|"file", applied: bool }` | 变更时 |
| `subscription:updated` | `{ id, ok, node_count, error? }` | 更新完成时 |
| `log:append` | `TraceRow[]` | **仅在日志页打开时订阅** |
| `toast` | `{ level, title, body, dedupe_key }` | 同 key 10 分钟内去重 |
| `tray:refresh` | `TrayState` | 托盘菜单即将展开时 |

## 2.4 关键 DTO（`DashboardData`）

```ts
type DashboardData = {
  status_bar: { gateway: "running"|"paused", policy: {id,name}, lane: {id,name,node?,latency_ms?} };
  cards: { requests: Num, tokens: Num, cost: {amount:number,currency:string}, success_rate: number };
  sparklines: { requests: number[]; tokens: number[]; cost: number[]; success_rate: number[] };
  trend: { bucket_ts: number; input: number; input_cached: number; output: number; cost: number }[];
  quota: { credential_id, label, used_pct, state: "ok"|"soft"|"exhausted"|"unknown", detail: string }[];
  lanes: { lane_id, name, node?, latency_ms, ok }[];
  live: RequestSummary[];              // 最近 500 条
  top_models: { model: string; tokens: number; cost: number }[];
  cache: { hit_rate: number; saved_amount: number; currency: string };
  unpriced_models: string[];
}
```

---

# 3. L2 · 管理 HTTP API（8788，给 CLI / 脚本）

- 监听 `127.0.0.1:8788`；鉴权 `Authorization: Bearer <admin_token>`（首次启动随机生成，存钥匙串）。
- **与 IPC 共用同一 service 层**，只是入口不同；行为必须一致。
- 只暴露自动化真正需要的子集（避免维护两份完整 CRUD）：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/status` | 等价 `app_status` |
| GET | `/config` | 导出当前配置（JSON）；`?format=yaml` 导 YAML |
| PUT | `/config` | 全量替换（等价 `config_apply`） |
| POST | `/config/validate` | 校验 |
| GET | `/usage/query` | query string 版 `UsageQuery`（供脚本/curl） |
| GET | `/usage/dashboard` | 看板数据 |
| GET | `/usage/export.csv` | 直接下载 CSV |
| GET | `/logs` | 分页查询 trace |
| GET | `/logs/{request_id}` | 单条详情；**不存在时返回 `200` + body `null`**（见下） |
| POST | `/logs/mode` | 改日志模式/保留期 |
| GET | `/pricing` / PUT `/pricing` | 定价表读写 |
| GET | `/lanes` | 线路列表与健康 |
| POST | `/lanes/{id}/test` | 测速 |
| POST | `/subscriptions/{id}/refresh` | 刷新订阅 |
| POST | `/kernel/restart` | 重启内核 |
| GET | `/kernel/generated-config` | 预览生成的 YAML |

**不做**：Provider/Credential/Model/Channel/Policy 的写操作 —— 这些是"设计决策"，走 GUI；CLI 只做只读 + 少数运维动作（这样 L2 永远不会成为第二个需要同步的前端）。

### 未配置 `admin_token` 时：**拒绝一切**（fail closed）

第 9 轮 G1 实现时确认的语义，补入契约：**`admin_token` 为空 → 不接受的不是"放行"，而是"全部拒绝"。**
管理 API 是运维通道，默认关闭比默认敞开安全。

### `GET /logs/{id}` 记录不存在 → `200` + `null`（第 9 轮裁决）

实现者提出：错误码总表（[11 §5]）里**没有"记录不存在"这一码**，怎么办。**裁决：返回 `200` + body `null`，不发明新错误码。**

理由：
1. 错误码表是**冻结契约**，为这一个场景新增一码要走变更流程 + 全团队广播，代价与收益不成比例。
2. `null` 在 JSON 里**无歧义** —— 它不会被误读成"查到了一条空记录"（那会是一个对象）。
3. 管理 API 只服务**我们自己的 UI 与 CLI**（[10 §3] 开头已写"与 IPC 共用同一 service 层"），
   不存在第三方按 REST 惯例期待 404 而踩坑的情况。

**消费方（前端 F2 的日志页）必须显式处理 `null`**，并给出"该请求日志已被清理或不存在"的人话提示，
不要显示空白详情面板。

---

# 4. 契约变更流程（给自己定的规矩）

1. **L0 变更 = 破坏兼容**：必须写迁移说明 + 在 CHANGELOG 标注 + 提供 `gateway.api_compat: "v1"` 之类的开关。
2. **L1 变更**：改 Rust 类型 → 重新生成 `bindings.ts` → 前端编译报错就是最好的检查。禁止手改生成文件。
3. **任何新增字段**：默认可选 + 有默认值，禁止要求前端立刻适配。
4. **接口文档是唯一真源**：实现与文档不一致时，先改文档再改代码（或在 PR 里同步改）。

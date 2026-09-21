/**
 * 浏览器开发用的 mock 数据 —— **数字全部来自 [03 §0] 的基准场景，不是随便编的 `foo/bar`**。
 *
 * 基准场景（[03 §0]）：同一个模型 `deepseek-v4.1flash` 有两份套餐 ——
 *   ① 字节方舟：**直连**，500 万 token/月，1 号重置（当前 82% = 410 万/500 万）
 *   ② command code：**走梯子（日本线）**，按量计费（当前 ¥4.2 / ¥20）
 * 期望行为：先用 ①，用完自动切 ②，同一会话不来回跳。
 *
 * 为什么要这么做（[13] L19）：如果 mock 用编造的数字，页面会"看起来对"但经不起核对；
 * 用基准场景的数字，F2/F3/F4 写出来的页面能直接拿来对产品语义。
 *
 * 页面可以自己追加 mock（同一个命令后注册的覆盖先注册的）：
 * ```ts
 * if (import.meta.env.DEV && !isTauri()) registerMock("provider_list", () => [...]);
 * ```
 */

import type {
  AppStatus,
  AutostartState,
  ChannelGroup,
  CommandArgs,
  CommandName,
  CommandResult,
  ConfigDoc,
  CredentialView,
  DashboardData,
  KernelStatus,
  LaneAssignment,
  LaneId,
  LaneView,
  LogPage,
  MatrixView,
  ModelView,
  Policy,
  ProviderView,
  RequestSummary,
  SubscriptionView,
  TrayState,
} from "@/shared/bindings";

import { isTauri, registerMock } from "./client";

// ---------------------------------------------------------------- 基准场景常量

const HOUR = 3_600_000;
const MODEL = "deepseek-v4.1flash";

/** 场景里的两条线路。 */
const LANE_DIRECT: LaneId = "lane-direct";
const LANE_JP: LaneId = "lane-jp";

const now = () => Date.now();

/** 类型化 mock 注册（命令名/返回类型都与 `CommandMap` 对齐）。 */
function mockCommand<K extends CommandName>(
  cmd: K,
  handler: (args: CommandArgs<K>) => CommandResult<K> | Promise<CommandResult<K>>,
): void {
  registerMock(cmd, (args) => handler(args as CommandArgs<K>));
}

// ---------------------------------------------------------------- 各命令的 mock

const kernelStatus: KernelStatus = {
  state: "ready",
  version: "v1.18.10-tokenagency",
  pid: 20_140,
  restarts: 0,
  last_error: null,
};

const appStatus: AppStatus = {
  gateway: "running",
  kernel: kernelStatus,
  version: "0.1.0",
  uptime_s: 3 * 3600 + 12 * 60,
  active_policy_id: "pol-default",
  default_lane_id: LANE_DIRECT,
  current_node: "东京-03",
};

/**
 * 自启动 mock：**必须体现 [09 §1.2] E3/E4 的语义**（desired 是配置、actual 是系统事实）。
 *
 * `app_get_autostart` 刻意返回一个**漂移**样例（配置开、系统里没有），
 * 这样 F4 的设置页能对着真实的漂移提示 + 「重新注册」按钮做开发，而不是等真机。
 */
function autostartState(desired: boolean, actual: boolean = desired): AutostartState {
  return {
    desired,
    actual,
    drifted: desired !== actual,
    mechanism: "windows-run-key",
    registered_command: actual ? '"C:\\TokenAgency\\TokenAgency.exe" --autostart --mode=tray' : undefined,
    mode: "tray",
    delay_s: 0,
  };
}

/** 4 张数字卡 + sparkline：与 [07 §1.2] 的版面一致（今日 1,284 请求 / 12.4M token / ¥18.62 / 99.2%）。 */
const dashboard: DashboardData = {
  status_bar: {
    gateway: "running",
    policy: { id: "pol-default", name: "默认（先用字节直连，用完切日本线）" },
    lane: { id: LANE_JP, name: "日本线", node: "东京-03", latency_ms: 82 },
  },
  cards: {
    requests: { value: 1_284, prev: 1_120 },
    tokens: { value: 12_400_000, prev: 11_050_000 },
    cost: { amount: 18.62, currency: "CNY", prev: 16.4 },
    success_rate: 0.992,
  },
  sparklines: {
    requests: [42, 51, 48, 63, 71, 66, 58, 74, 82, 79, 91, 88, 76, 69, 83, 95, 102, 98, 87, 79, 71, 84, 92, 88],
    tokens: [0.4, 0.5, 0.45, 0.62, 0.71, 0.66, 0.58, 0.74, 0.82, 0.79, 0.91, 0.88, 0.76, 0.69, 0.83, 0.95, 1.02, 0.98, 0.87, 0.79, 0.71, 0.84, 0.92, 0.88],
    cost: [0.6, 0.7, 0.65, 0.8, 0.9, 0.85, 0.75, 0.95, 1.05, 1.0, 1.15, 1.1, 0.95, 0.88, 1.02, 1.2, 1.28, 1.22, 1.08, 0.98, 0.9, 1.05, 1.15, 1.1],
    success_rate: [0.98, 0.99, 0.995, 0.99, 0.97, 0.99, 1, 0.99, 0.98, 0.99, 0.995, 1, 0.99, 0.97, 0.98, 0.99, 0.99, 1, 0.995, 0.99, 0.98, 0.99, 0.99, 0.992],
  },
  trend: Array.from({ length: 24 }, (_, i) => ({
    bucket_ts: now() - (23 - i) * HOUR,
    input: 320_000 + i * 4_000,
    input_cached: 180_000 + i * 6_000,
    output: 60_000 + i * 900,
    cost: 0.5 + i * 0.02,
  })),
  quota: [
    {
      credential_id: "c-bytedance",
      label: "字节方舟 · 500万 token/月",
      used_pct: 0.82,
      state: "soft",
      detail: "410万 / 500万 · 额度/月 · 每月 1 号重置",
    },
    {
      credential_id: "c-commandcode",
      label: "command code · 按量",
      used_pct: 0.21,
      state: "ok",
      detail: "¥4.2 / ¥20 · 按量",
    },
  ],
  lanes: [
    { lane_id: LANE_DIRECT, name: "直连（本机网络）", node: undefined, latency_ms: 18, ok: true },
    { lane_id: LANE_JP, name: "日本线", node: "东京-03", latency_ms: 82, ok: true },
  ],
  live: [],
  top_models: [
    { model: MODEL, tokens: 11_200_000, cost: 16.9 },
    { model: "claude-sonnet-4-5", tokens: 900_000, cost: 1.42 },
    { model: "gpt-4o-mini", tokens: 300_000, cost: 0.3 },
  ],
  cache: { hit_rate: 0.68, saved_amount: 42.1, currency: "CNY" },
  unpriced_models: ["command-code-preview"],
};

/** 最近的请求流（基准场景：字节直连为主，偶尔因额度软限切到日本线）。 */
const live: RequestSummary[] = Array.from({ length: 12 }, (_, i) => {
  const onJp = i % 5 === 4; // 每 5 条有一条走了日本线
  const retried = i === 3;
  const failed = i === 7;
  return {
    request_id: `req_mock_${String(i).padStart(4, "0")}`,
    ts_ms: now() - i * 7_000,
    model: MODEL,
    upstream_model: MODEL,
    channel_id: onJp ? "ch-commandcode" : "ch-bytedance",
    client: "cursor",
    lane_id: onJp ? LANE_JP : LANE_DIRECT,
    node: onJp ? "东京-03" : null,
    status: failed ? 504 : 200,
    outcome: failed ? "failed" : retried ? "retried" : "ok",
    latency_ms: failed ? 30_000 : 900 + i * 120,
    ttfb_ms: failed ? null : 320 + i * 40,
    total_tokens: 2_400 + i * 130,
    cost: { amount: 0.0032 + i * 0.0002, currency: "CNY" },
    attempts: retried ? 2 : 1,
    error_type: failed ? "connect_timeout" : null,
  };
});

/** 日志行 = 请求摘要 + `credential_id`（`TraceRow` 比 `RequestSummary` 多这一列）。 */
function traceRows(): (RequestSummary & { credential_id: string })[] {
  return live.map((r) => ({
    ...r,
    credential_id: r.channel_id === "ch-commandcode" ? "c-commandcode" : "c-bytedance",
  }));
}

const credentials: CredentialView[] = [
  {
    credential: {
      id: "c-bytedance",
      provider: "p-bytedance",
      label: "字节 · 500万 token/月",
      secret: "${secret:bytedance}",
      lane: LANE_DIRECT,
      quota: {
        metric: "tokens",
        limit: 5_000_000,
        currency: "CNY",
        window: "month",
        reset_day: 1,
        soft_limit: 0.8,
        hard_limit: 0.98,
        reserve: 0.05,
      },
      guard: { max_rpm: 60, max_concurrency: 8, max_input_tokens: null, on_exceed: "wait" },
      tags: ["cn-direct"],
      enabled: true,
    },
    provider_name: "字节方舟",
    quota: {
      credential_id: "c-bytedance",
      state: "soft",
      used_tokens: 4_100_000,
      updated_at_ms: now() - 12_000,
    },
    channel_count: 1,
  },
  {
    credential: {
      id: "c-commandcode",
      provider: "p-commandcode",
      label: "command code · 按量",
      secret: "${secret:commandcode}",
      lane: LANE_JP,
      quota: {
        metric: "cost",
        limit: 20,
        currency: "CNY",
        window: "month",
        reset_day: 1,
        soft_limit: 0.8,
        hard_limit: 1,
        reserve: 0,
      },
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "next_candidate" },
      tags: ["backup"],
      enabled: true,
    },
    provider_name: "command code",
    quota: {
      credential_id: "c-commandcode",
      state: "ok",
      used_cost: 4.2,
      updated_at_ms: now() - 40_000,
    },
    channel_count: 1,
  },
];

const providers: ProviderView[] = [
  {
    id: "p-bytedance",
    name: "字节方舟",
    protocol: "openai",
    base_url: "https://ark.cn-beijing.volces.com/api/v3",
    enabled: true,
    success_rate_24h: 0.998,
    avg_latency_ms: 18,
    channel_count: 1,
    status: "ok",
  },
  {
    id: "p-commandcode",
    name: "command code",
    protocol: "anthropic",
    base_url: "https://api.commandcode.ai/v1",
    enabled: true,
    success_rate_24h: 0.981,
    avg_latency_ms: 82,
    channel_count: 1,
    status: "warn",
  },
];

const lanes: LaneView[] = [
  {
    lane: {
      id: LANE_DIRECT,
      name: "直连（本机网络）",
      kind: "direct",
      subscription: null,
      group: null,
      group_type: null,
      node: null,
      filter: null,
      exclude_filter: null,
      enabled: true,
    },
    type: "direct",
    node_count: 0,
    current_node: null,
    latency_ms: 18,
    health: "ok",
  },
  {
    lane: {
      id: LANE_JP,
      name: "日本线",
      kind: "subscription_group",
      subscription: "sub-jp",
      group: "🇯🇵 日本",
      group_type: "url-test",
      node: null,
      filter: null,
      exclude_filter: null,
      enabled: true,
    },
    type: "subscription_group",
    node_count: 3,
    current_node: "东京-03",
    latency_ms: 82,
    health: "ok",
  },
];

const subscriptions: SubscriptionView[] = [
  {
    subscription: {
      id: "sub-jp",
      name: "机场A",
      url: "https://example.com/api/v1/client/subscribe?token=REPLACE_ME",
      user_agent: "clash-verge/v2.0.0",
      format: "auto",
      interval_s: 6 * 3600,
      keep_versions: 3,
      filter: { include: "(?i)(日本|JP|Tokyo|Osaka)", exclude: null },
      health_check: {
        enabled: true,
        url: "https://api.commandcode.ai/v1/models",
        interval_s: 60,
        tolerance_ms: 100,
      },
      enabled: true,
      provider_key: "sub-jp",
    },
    provider_key: "sub-jp",
    node_count: 12,
    group_count: 4,
    userinfo: { upload: 0, download: 8_400_000_000, total: 100_000_000_000 },
    last_update_ms: now() - 22 * 60_000,
    parser: "clash-yaml@1",
  },
];

const models: ModelView[] = [
  {
    model: { id: MODEL, aliases: [], display: { group: "字节", order: 10 }, expose: true },
    channel_count: 2,
    upstream_names: [MODEL],
    priced: false,
    cost_hint: null,
  },
];

const groups: ChannelGroup[] = [
  {
    id: "grp-deepseek",
    model: MODEL,
    members: ["ch-bytedance", "ch-commandcode"],
    selector: "sticky_then_priority",
    sticky: {
      enabled: true,
      ttl_h: 12,
      key: [
        "header:x-session-id",
        "header:x-conversation-id",
        "metadata.user_id",
        "body_hash",
      ],
    },
    release_on: ["quota_exhausted", "hard_error"],
  },
];

const policies: Policy[] = [
  {
    id: "pol-default",
    name: "默认（先用字节直连，用完切日本线）",
    enabled: true,
    priority: 1000,
    match: {
      all: true,
      model_matches: [],
      has_image: null,
      header_matches: {},
      ua_matches: null,
      channel_tags_any: [],
      channel_tags_all: [],
      input_tokens_lt: null,
      input_tokens_gt: null,
      stream: null,
      quota_remaining_lt: null,
      time_window: null,
      provider_in: [],
    },
    action: {
      use_group: "grp-deepseek",
      use_channels: [],
      exclude_channels: [],
      require_tag: null,
      exclude_tag: null,
      selector: null,
      failover: {
        on: ["http429", "http402", "http5xx", "timeout", "connect_error", "quota_exhausted"],
        max_attempts: 3,
        cooldown_s: 120,
        backoff: "exponential",
      },
      on_no_candidate: "error",
      error_code: null,
      pin_sticky: null,
      use_lane: null,
    },
  },
];

const matrix: MatrixView = {
  models: [{ model_id: MODEL, display_name: MODEL, unpriced: true }],
  credentials: credentials.map((c) => ({
    credential_id: c.credential.id,
    label: c.credential.label,
    provider_name: c.provider_name,
    quota: c.quota,
  })),
  cells: [
    {
      model_id: MODEL,
      credential_id: "c-bytedance",
      channel_id: "ch-bytedance",
      lane_id: LANE_DIRECT,
      upstream_model: MODEL,
      priority: 10,
      weight: 100,
      tags: ["cn-direct"],
      enabled: true,
    },
    {
      model_id: MODEL,
      credential_id: "c-commandcode",
      channel_id: "ch-commandcode",
      lane_id: LANE_JP,
      upstream_model: MODEL,
      priority: 20,
      weight: 50,
      tags: ["backup"],
      enabled: true,
    },
  ],
};

const assignments: LaneAssignment[] = [
  { model_id: MODEL, channel_id: "ch-bytedance", lane_id: LANE_DIRECT, source: "credential" },
  { model_id: MODEL, channel_id: "ch-commandcode", lane_id: LANE_JP, source: "credential" },
];

// ---------------------------------------------------------------- 注册

/**
 * 注册基准场景 mock。**幂等**（重复调用只是再覆盖一遍同样的实现）。
 *
 * 只在浏览器开发环境（非 Tauri 且 `import.meta.env.DEV`）自动注册，见 `registerDevMocks()`。
 */
export function registerBaselineMocks(): void {
  mockCommand("app_status", () => appStatus);
  mockCommand("app_pause_gateway", () => ({ ...appStatus, gateway: "paused" as const }));
  mockCommand("app_resume_gateway", () => appStatus);
  mockCommand("app_clipboard_access_info", () => "BASE_URL=http://127.0.0.1:8787/v1\nAPI_KEY=ta-local-dev");
  mockCommand("app_check_update", () => null);
  mockCommand("app_get_autostart", () => autostartState(true, false));
  mockCommand("app_set_autostart", (args) => autostartState(args.enabled, args.enabled));
  mockCommand("app_repair_autostart", () => autostartState(true, true));

  mockCommand("kernel_status", () => kernelStatus);
  mockCommand("kernel_start", () => kernelStatus);
  mockCommand("kernel_stop", () => ({ ...kernelStatus, state: "stopped" as const, pid: null }));
  mockCommand("kernel_restart", () => kernelStatus);
  mockCommand("kernel_reload_config", () => kernelStatus);
  mockCommand("kernel_generated_config", () => "# GENERATED BY TokenAgency — DO NOT EDIT\nmode: rule\n");
  mockCommand("kernel_log_tail", () => "[mock] kernel log tail\n");
  mockCommand("kernel_probe_path", () => ({ found: true, path: "C:\\\\TokenAgency\\\\kernel\\\\mihomo.exe", version: "v1.18.10" }));

  mockCommand("usage_dashboard", () => ({ ...dashboard, live }));
  mockCommand("usage_request_stream_snapshot", (args) =>
    live.slice(0, Math.max(0, args.limit)),
  );
  mockCommand("usage_token_kinds", () => [
    "input",
    "input_cached",
    "input_cache_write",
    "output",
    "output_reasoning",
  ]);
  mockCommand("usage_query", () => ({
    columns: ["model", "requests", "input", "input_cached", "output", "cost"],
    rows: [
      {
        dims: { model: MODEL },
        metrics: {
          requests: 1_284,
          input: 6_400_000,
          input_cached: 5_200_000,
          output: 800_000,
          cost: 16.9,
        },
      },
    ],
    total: 1,
  }));

  mockCommand("credential_list", () => credentials);
  mockCommand("credential_clear_quota", (args) => ({
    credential_id: args.id,
    state: "ok" as const,
    used_tokens: 0,
    used_cost: 0,
    updated_at_ms: now(),
  }));
  mockCommand("provider_list", () => providers);
  mockCommand("provider_test", () => ({ ok: true, status: 200, latency_ms: 82, note: "mock" }));
  mockCommand("model_list", () => models);
  mockCommand("group_list", () => groups);
  mockCommand("policy_list", () => policies);
  mockCommand("channel_matrix", () => matrix);
  mockCommand("lane_list", () => lanes);
  mockCommand("lane_assignments", () => assignments);
  mockCommand("lane_pick_options", () => [
    {
      group: "直连",
      options: [{ lane_id: LANE_DIRECT, name: "直连（本机网络）", kind: "direct" as const }],
    },
    {
      group: "机场A",
      options: [
        {
          lane_id: LANE_JP,
          name: "🇯🇵 日本（订阅自带分组）",
          kind: "subscription_group" as const,
          node_count: 3,
          latency_ms: 82,
        },
      ],
    },
  ]);
  mockCommand("lane_test", () => [
    { node: "东京-03", latency_ms: 82, ok: true },
    { node: "大阪-01", latency_ms: 96, ok: true },
    { node: "首尔-02", latency_ms: null, ok: false },
  ]);
  mockCommand("subscription_list", () => subscriptions);
  mockCommand("subscription_refresh", () => ({ ok: true, node_count: 12, group_count: 4 }));
  mockCommand("subscription_groups", () => [
    { name: "🇯🇵 日本", type: "url-test" as const, node_count: 3 },
    { name: "🇭🇰 香港", type: "url-test" as const, node_count: 5 },
    { name: "♻️ 自动选择", type: "url-test" as const, node_count: 12 },
  ]);
  mockCommand("subscription_nodes", () => [
    { name: "东京-03", type: "vmess", group: "🇯🇵 日本", latency_ms: 82, ok: true },
  ]);

  mockCommand("log_query", () => {
    const rows = traceRows();
    return { rows, total: rows.length } satisfies LogPage;
  });
  mockCommand("log_get_trace", () => {
    const rows = traceRows();
    const first = rows[0];
    const row = first ?? {
      ...live[0]!,
      credential_id: "c-bytedance",
    };
    return {
      row: { ...row, attempts: row.attempts },
      attempts: [
        {
          channel_id: row.channel_id,
          lane_id: row.lane_id ?? null,
          node: row.node ?? null,
          status: row.status,
          ms: row.latency_ms,
          reason: row.error_type ?? null,
        },
      ],
      header_diff: {},
      body_shape_in: { model: "string", messages: "array[3]", stream: "boolean" },
      body_shape_out: { id: "string", choices: "array[1]" },
    };
  });

  mockCommand("log_storage_stats", () => ({
    rows: 12_840,
    bytes: 386 * 1024 * 1024,
    oldest_ts: now() - 30 * 24 * HOUR,
    newest_ts: now(),
  }));

  mockCommand("pricing_unpriced_models", () => ["command-code-preview"]);
  mockCommand("pricing_list", () => []);
  mockCommand("secrets_scan_plaintext", () => []);

  mockCommand("config_get", () => baselineConfigDoc());
  mockCommand("config_validate", () => ({ ok: true, errors: [], warnings: [] }));
  mockCommand("config_apply", () => ({ applied: true, restart_required: false, kernel_reload: false }));
  mockCommand("config_history_list", () => []);

  mockCommand("tray_state", (): TrayState => ({
    status: "ok",
    tooltip: "TokenAgency · 运行中 · 字节直连 82%",
    policy_name: "默认（先用字节直连，用完切日本线）",
    node: "东京-03",
    latency_ms: 82,
  }));

  mockCommand("onboarding_detect", () => ({
    kernel_found: true,
    existing_config: false,
    suggested_providers: ["字节方舟", "command code"],
  }));
  mockCommand("onboarding_test_all", () => [
    { credential_id: "c-bytedance", lane_id: LANE_DIRECT, ok: true, latency_ms: 18, status: 200 },
    { credential_id: "c-commandcode", lane_id: LANE_JP, ok: true, latency_ms: 82, status: 200 },
  ]);
}

/**
 * 非 Tauri 环境（浏览器 `npm run dev`）下自动注册。
 * **幂等，可在 App 启动时无脑调用。**
 */
export function registerDevMocks(): void {
  if (isTauri()) return;
  if (!import.meta.env.DEV) return;
  registerBaselineMocks();
}

/** 最小可用的配置树（与 `ta-core::config::default_doc()` 的语义一致，只保留场景相关部分）。 */
function baselineConfigDoc(): ConfigDoc {
  return {
    version: 1,
    gateway: {
      listen: "127.0.0.1:8787",
      admin_listen: "127.0.0.1:8788",
      local_key: "${secret:gateway-local-key}",
      max_body_mb: 32,
      debug_headers: true,
      expose_models_with_capabilities: false,
      fallback_provider: null,
      passthrough: {
        header_mode: "pass",
        body: "preserve",
        strip_hop_by_hop: true,
        verify_no_diff: true,
        on_loss: "reject",
      },
      retry: {
        max_attempts: 3,
        stream_max_attempts: 2,
        retry_after_first_byte: false,
        cooldown_s: 120,
        backoff: "exponential",
      },
      metering: { inject_stream_usage: false },
    },
    request_log: {
      mode: "meta",
      retention: "30d",
      max_rows: 500_000,
      max_size_mb: 2048,
      sample_rate: 1,
      image_handling: "hash",
    },
    pricing: { currency: "CNY", unit: "per_1m", usd_to_cny: 7.25, entries: [] },
    net: {
      kernel: {
        provider: "external",
        path: null,
        controller: "127.0.0.1:39090",
        lane_port_range: [39100, 39299],
        subscription_source: "kernel",
      },
      dialer: "kernel",
      dns: {
        enhanced_mode: "redir-host",
        nameserver: ["https://223.5.5.5/dns-query"],
        proxy_server_nameserver: ["https://223.5.5.5/dns-query"],
        default_nameserver: ["223.5.5.5", "119.29.29.29"],
      },
      health: { interval_s: 60, url_by_provider: true, probe_url: null },
      default_lane: LANE_DIRECT,
      system_proxy: false,
      tun: false,
    },
    proxy_subscriptions: subscriptions.map((s) => s.subscription),
    lanes: lanes.map((l) => l.lane),
    providers: [],
    credentials: credentials.map((c) => c.credential),
    models: models.map((m) => m.model),
    channels: [],
    channel_groups: groups,
    policies,
    system_integration: {
      enable_tun: false,
      enable_system_proxy: false,
      autostart: false,
      autostart_mode: "tray",
      autostart_delay_s: 0,
      tray_show_latency: true,
      file_watch_mode: false,
      file_watch_path: null,
    },
  };
}

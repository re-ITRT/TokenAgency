/**
 * 请求日志页（/）的**纯逻辑**：没有 JSX、没有 React、没有 IPC。
 *
 * 把这个页面最容易被写错的两件事收在这一个文件里，并用单测钉住：
 *
 * 1. **送进后端的查询形状**（`buildLogQuery` / `logQueryArgs`）
 * 2. **读后端的行**（`readLogRow`）—— 契约 `src/shared/bindings.ts` 与 Rust 实现
 *    `ta_store::trace_store` 的字段名**对不上**，见下面「契约与实现的两处偏差」。
 *
 * ## 契约与实现的两处偏差（这是事实，不是猜测；来源已逐条标注）
 *
 * | 项 | 冻结契约（`src/shared/bindings.ts`，本任务不许改） | Rust 实现（权威） |
 * |---|---|---|
 * | `log_query` 参数名 | `{ query }`（bindings.ts:1127） | `q`（`src-tauri/src/commands/log.rs:17-20`） |
 * | 时间窗 | `LogQuery.range: TimeRange`（bindings.ts:848/767，线上值 = 裸 snake_case 字符串） | `LogQuery.from_ms` / `to_ms`（`crates/ta-store/src/trace_store.rs:189-216`） |
 * | 行字段 | `TraceRow.{channel_id, credential_id, lane_id, node, upstream_model, latency_ms, total_tokens, cost, error_type}`（bindings.ts:859-877） | `TraceRow.{client, protocol_in, protocol_out, stream, policy_id, group_id, chosen_channel, attempts: u32, total_ms, ttfb_ms, tokens, error, mode, has_body, body_bytes}`（trace_store.rs:130-158） |
 *
 * 所以本页：
 * - **发查询时两个参数名都发**（`query` + `q`），时间窗**两种形状都发**（`from_ms`/`to_ms` + `range`）。
 *   Rust 的 `LogQuery` 全字段带 `#[serde(default)]` 且没有 `deny_unknown_fields`，
 *   多余键被忽略；而**少发 `q` / 少发 `from_ms` 会让真机上的筛选静默失效**（那是"假筛选"）。
 * - **读行时按两套名字的并集读**（`pickStr(row, "channel_id", "chosen_channel")`）。
 *   任何一边缺失都显示"—/未知"，**不猜**。
 *
 * 这些偏差是**契约生成（T1 的 `npm run gen:bindings`）落地前**的临时现实；
 * 生成物覆盖 `bindings.ts` 之后，本文件的并集读法仍然成立（多认一个名字而已）。
 */

import dayjs from "dayjs";

import type {
  CommandArgs,
  LogMode,
  Retention,
  StorageStats,
  TraceDetail,
  TraceRow,
} from "@/shared/bindings";
// 读后端原始对象的唯一实现（原先这里自己写了 pickStr/pickNum/pickBool 三份）
import { pickBool, pickNum, pickStr, type Row } from "@/shared/row";

const DAY_MS = 24 * 60 * 60 * 1000;

// ================================================================ 常量

/** 每页条数（`log_query` 的 `limit`；后端把 1..=1000 之外的钳住）。 */
export const PAGE_SIZE = 100;

/**
 * 检索模式下的候选扫描上限 —— 与 `crates/ta-store/src/trace_store.rs:40` 的
 * `SEARCH_SCAN_ROWS` 一致。**这是要告诉用户的限制**：检索只在最近这么多条候选里匹配，
 * 更早的匹配不会出现在结果里（否则用户会以为"日志里没有"）。
 */
export const SEARCH_SCAN_ROWS = 2_000;

/** 时间范围预设（**界面词汇**）。线上词汇见下面的 `RANGE_TO_TIME_RANGE`。 */
export type RangePreset = "today" | "7d" | "30d" | "month";
export const RANGE_PRESETS: readonly RangePreset[] = ["today", "7d", "30d", "month"];

/**
 * 界面范围词 → `TimeRange` 的**线上值**（`crates/ta-core/src/clock.rs:77-88`，
 * `rename_all = "snake_case"` 的外部标记枚举，所以是**裸字符串**）。
 *
 * 注意：本页的时间窗**真正生效靠 `from_ms`/`to_ms`**（见 `buildLogQuery`）；`range` 这个键
 * 是冻结契约里的字段（`bindings.ts` 的 `LogQuery.range`），而 Rust 的
 * `ta_store::LogQuery` **没有这个字段**（`trace_store.rs:189-216`）→ 它被 serde 忽略。
 * 即便如此也必须发**正确形状**：一个发不出去的形状在真机上要么报错、要么在
 * 后端将来接上 `range` 时静默失效 —— 两种都不是我们想要的。
 */
const RANGE_TO_TIME_RANGE: Record<RangePreset, string> = {
  today: "today",
  "7d": "last7d",
  "30d": "last30d",
  month: "this_month",
};

/** 「结果级别」筛选 —— 与契约 `LogQuery.status`（`ok | retried | failed`）一致。 */
export type Level = "ok" | "retried" | "failed";
export const LEVELS: readonly Level[] = ["ok", "retried", "failed"];

/** 记录模式：off / meta / shape / full。 */
export const LOG_MODES: readonly LogMode[] = ["off", "meta", "shape", "full"];

/** 保留期：7d | 30d | 90d | forever。 */
export const RETENTIONS: readonly Retention[] = ["7d", "30d", "90d", "forever"];

// ================================================================ 筛选条件

/** 页面上的查询条件（草稿与已应用是两份，见 `index.tsx` 的 `draft` / `applied`）。 */
export type LogsFilter = {
  preset: RangePreset;
  /** 请求 ID（`req_…`）。 */
  requestId: string;
  /** 错误码（`TA-xxxx`）或错误文本。 */
  errorCode: string;
  /** 结果级别；空数组 = 不限。 */
  levels: Level[];
};

/** 默认条件：近 7 天、不限级别、不检索。 */
export const DEFAULT_FILTER: LogsFilter = {
  preset: "7d",
  requestId: "",
  errorCode: "",
  levels: [],
};

/**
 * 时间窗 → `[from_ms, to_ms)`（**本地时区**，与「时间轴永远本地时区」一致）。
 * 后端是 `ts >= from_ms AND ts < to_ms`（trace_store.rs:767-774）。
 */
export function rangeWindow(
  preset: RangePreset,
  nowMs: number = Date.now(),
): { from_ms: number; to_ms: number } {
  const start = dayjs(nowMs);
  if (preset === "today") return { from_ms: start.startOf("day").valueOf(), to_ms: nowMs };
  if (preset === "month") return { from_ms: start.startOf("month").valueOf(), to_ms: nowMs };
  const days = preset === "7d" ? 7 : 30;
  return { from_ms: nowMs - days * DAY_MS, to_ms: nowMs };
}

/**
 * 唯一被送进后端的检索串（`LogQuery.search`）。
 *
 * **后端只有一个检索串**（trace_store.rs:210），所以请求 ID 与错误码**互斥**：
 * 请求 ID 优先。UI 层也按这条规则互相禁用输入框 —— 两处同一规则，不出现"填了两个但只生效一个"。
 */
export function searchTerm(filter: LogsFilter): string | undefined {
  const requestId = filter.requestId.trim();
  if (requestId !== "") return requestId;
  const errorCode = filter.errorCode.trim();
  return errorCode === "" ? undefined : errorCode;
}

/** `TA-xxxx` 形状判定（与 `ta-core::diag::query::is_error_code` 同口径：**严格四位**）。 */
export function isErrorCodeShape(value: string): boolean {
  return /^TA-\d{4}$/.test(value.trim());
}

/** 查询条件是"纯文本检索"还是"按码检索"——只用于给用户看的一句提示，不参与筛选。 */
export type SearchKind = "none" | "request_id" | "error_code" | "text";
export function searchKind(filter: LogsFilter): SearchKind {
  const term = searchTerm(filter);
  if (term === undefined) return "none";
  if (filter.requestId.trim() !== "") return "request_id";
  if (isErrorCodeShape(term)) return "error_code";
  return "text";
}

// ================================================================ 查询 → 线上形状

/** 送进 `invoke` 的那个对象（键名是后端的事，所以按 `Record` 处理并逐条注明来源）。 */
export type LogQueryWire = Record<string, unknown>;

/**
 * 构造 `log_query` 的查询体。**两种形状一起发**（理由见文件头）。
 */
export function buildLogQuery(
  filter: LogsFilter,
  options: { cursor?: string | null; limit?: number; nowMs?: number } = {},
): LogQueryWire {
  const { from_ms, to_ms } = rangeWindow(filter.preset, options.nowMs);
  const term = searchTerm(filter);

  const wire: LogQueryWire = {
    // Rust 的真实字段（ta-store trace_store.rs:189-216）
    from_ms,
    to_ms,
    limit: options.limit ?? PAGE_SIZE,
    status: [...filter.levels],
  };
  if (term !== undefined) wire.search = term;
  if (options.cursor !== undefined && options.cursor !== null && options.cursor !== "") {
    wire.cursor = options.cursor;
  }
  // 冻结契约里的 `range`（bindings.ts:848）。Rust 侧没有这个字段 → 被忽略；
  // 但**只发它不发 from_ms 就等于没有时间筛选**，所以两个都发。
  // 形状用 `TimeRange` 的线上值（裸字符串）—— 不是 `{preset:…}`（那会被 serde 判成未知变体）。
  wire.range = RANGE_TO_TIME_RANGE[filter.preset];
  return wire;
}

/**
 * `useCommandQuery("log_query", args)` 的入参。
 *
 * 参数名两写：Rust 取 `q`，冻结契约写 `query`。缺 `q` 时真机直接报
 * `missing required key q`（不是静默），而缺 `query` 会让 `tsc` 报错 —— 两个都发最省事。
 */
export function logQueryArgs(
  filter: LogsFilter,
  options: { cursor?: string | null; limit?: number; nowMs?: number } = {},
): CommandArgs<"log_query"> {
  const q = buildLogQuery(filter, options);
  return { query: q, q } as unknown as CommandArgs<"log_query">;
}

/** query key 指纹：条件进 key，条件变了自然重查（[`keys.ts`] 的约定）。 */
export function filterFingerprint(filter: LogsFilter): string {
  return [
    filter.preset,
    filter.requestId.trim(),
    filter.errorCode.trim(),
    [...filter.levels].sort().join(","),
  ].join("|");
}

/**
 * 条件的一句话摘要（结果区显示"当前条件"，让用户看得见实际生效的是什么）。
 *
 * `levelValue` 由调用方渲染好（页面把它翻成界面语言）—— 这样纯函数不必知道 i18n，
 * 而摘要里也不会漏出 `failed` 这种线上值。
 */
export function filterSummary(
  filter: LogsFilter,
  labels: {
    preset: string;
    levelLabel: string;
    levelValue: string;
    requestId: string;
    errorCode: string;
    none: string;
  },
): string {
  const parts = [labels.preset];
  if (filter.levels.length > 0) parts.push(`${labels.levelLabel}: ${labels.levelValue}`);
  const term = searchTerm(filter);
  if (term !== undefined) {
    parts.push(
      searchKind(filter) === "request_id"
        ? `${labels.requestId}: ${term}`
        : `${labels.errorCode}: ${term}`,
    );
  }
  if (parts.length === 1) parts.push(labels.none);
  return parts.join(" · ");
}

// ================================================================ 行 → 视图

/**
 * 归一化后的一行（列表列与展开区都用它）。
 * **每个字段都标出它可能来自哪个后端字段名**；都缺就是 `undefined` → 画"—"，不填 0。
 */
export type LogRowView = {
  requestId: string;
  tsMs?: number;
  model?: string;
  /** 契约是 `upstream_model`；Rust 的 `TraceRow` 只有 `model`（上游名在通道上）。 */
  upstreamModel?: string;
  client?: string;
  /** 契约是 `channel_id`；Rust 是 `chosen_channel`。 */
  channel?: string;
  credential?: string;
  lane?: string;
  node?: string;
  status?: number;
  /** `ok` | `retried` | `failed`（Rust 的 `outcome` 恒有；契约也有）。 */
  outcome?: string;
  attempts?: number;
  /** 契约是 `latency_ms`；Rust 是 `total_ms`。 */
  latencyMs?: number;
  ttfbMs?: number;
  /** 契约是 `total_tokens`；Rust 是 `tokens`。 */
  tokens?: number;
  cost?: { amount: number; currency: string };
  /** 契约是 `error_type`；Rust 是 `error`（人话文案）。 */
  error?: string;
  /** Rust 才有：这一行**写入时**的记录模式（off 不会产生行）。 */
  mode?: string;
  hasBody?: boolean;
  bodyBytes?: number;
  protocolIn?: string;
  protocolOut?: string;
  stream?: boolean;
  policyId?: string;
  groupId?: string;
  /** 后端返回的**原始对象**（展开区"原始字段"用；用户数据原样呈现，不清洗）。 */
  raw: Row;
};

/** 读一行的原始对象。入参是契约类型，运行时可能是契约的形状或 Rust 的形状。 */
export function readLogRow(raw: TraceRow): LogRowView {
  const row = raw as unknown as Row;
  const costAmount = pickNum(row, "cost_amount");
  const costObj = row["cost"];
  const money =
    typeof costObj === "object" && costObj !== null
      ? (costObj as { amount?: unknown; currency?: unknown })
      : undefined;

  return {
    requestId: pickStr(row, "request_id") ?? "",
    tsMs: pickNum(row, "ts_ms", "ts"),
    model: pickStr(row, "model"),
    upstreamModel: pickStr(row, "upstream_model"),
    client: pickStr(row, "client"),
    channel: pickStr(row, "channel_id", "chosen_channel"),
    credential: pickStr(row, "credential_id"),
    lane: pickStr(row, "lane_id", "lane"),
    node: pickStr(row, "node"),
    status: pickNum(row, "status"),
    outcome: pickStr(row, "outcome"),
    attempts: pickNum(row, "attempts"),
    latencyMs: pickNum(row, "latency_ms", "total_ms"),
    ttfbMs: pickNum(row, "ttfb_ms"),
    tokens: pickNum(row, "total_tokens", "tokens"),
    cost:
      typeof money?.amount === "number" && typeof money.currency === "string"
        ? { amount: money.amount, currency: money.currency }
        : costAmount !== undefined
          ? { amount: costAmount, currency: pickStr(row, "currency") ?? "" }
          : undefined,
    error: pickStr(row, "error_type", "error"),
    mode: pickStr(row, "mode"),
    hasBody: pickBool(row, "has_body"),
    bodyBytes: pickNum(row, "body_bytes"),
    protocolIn: pickStr(row, "protocol_in"),
    protocolOut: pickStr(row, "protocol_out"),
    stream: pickBool(row, "stream"),
    policyId: pickStr(row, "policy_id"),
    groupId: pickStr(row, "group_id"),
    raw: row,
  };
}

/** 合并两批行：**按 request_id 去重**（翻页期间新请求挤进第一页时会出现重复）。 */
export function mergeLogRows(first: readonly TraceRow[], extra: readonly TraceRow[]): TraceRow[] {
  const seen = new Set<string>();
  const out: TraceRow[] = [];
  for (const row of [...first, ...extra]) {
    const id = pickStr(row as unknown as Row, "request_id") ?? "";
    if (id !== "" && seen.has(id)) continue;
    if (id !== "") seen.add(id);
    out.push(row);
  }
  return out;
}

/**
 * 最近一条日志行**记录时**的模式（Rust 的 `TraceRow.mode`）。
 *
 * 为什么读它而不是读"当前模式"：命令面里**没有读取当前模式的接口**
 * （`log_set_mode` 只写不读；`log_query_capabilities` 已在 Rust 注册但**不在冻结契约里**，
 * 本页不许调用契约外的命令）。所以这里只陈述**有证据的事实**：这些行是以什么模式落库的。
 * 一行都没有时返回 `undefined`（→ UI 显示"未知"），**不推断**当前模式。
 */
export function newestRecordedMode(rows: readonly LogRowView[]): string | undefined {
  return rows.find((r) => r.mode !== undefined)?.mode;
}

/** 这一行的结果是否可以判定（Rust 在 `map_trace_row` 里恒算 outcome；缺这一列 = 不可判定）。 */
export function outcomeKnown(view: LogRowView): boolean {
  return view.outcome !== undefined;
}

// ================================================================ 占用（log_storage_stats）

/** 归一化后的占用信息。缺的字段留 `undefined` → UI 画"—"，不画 0。 */
export type StorageView = {
  rows?: number;
  /** 库文件总大小（Rust `StorageStats.bytes`）。 */
  bytes?: number;
  /** body 与头压缩后合计（Rust 才有这一列）。 */
  bodyBytes?: number;
  oldestTs?: number;
  newestTs?: number;
};

export function readStorageStats(stats: StorageStats | undefined): StorageView | undefined {
  if (stats === undefined) return undefined;
  const row = stats as unknown as Row;
  return {
    rows: pickNum(row, "rows"),
    bytes: pickNum(row, "bytes"),
    bodyBytes: pickNum(row, "body_bytes"),
    oldestTs: pickNum(row, "oldest_ts"),
    newestTs: pickNum(row, "newest_ts"),
  };
}

// ================================================================ 日志导出（log_export）

/**
 * 导出结果。**两种形态都要能显示**，因为后端契约正在从 `void` 升级成回执对象（task-25）：
 * - `written`：后端回执了 `{ path, bytes }` → 这才是"后端确认写到哪"；
 * - `requested`：后端当前不回报（`result: void`）→ 只能如实说"已请求导出到 <path>"。
 *
 * **为什么要有这个纯函数**：升级落地的**同一天**界面就该自动显示真值，而不需要再改一次 UI。
 * 这里刻意不去猜"大概写成功了"：只有后端回执了对象才算 `written`。
 */
export type ExportOutcome =
  | { kind: "written"; path: string; bytes?: number }
  | { kind: "requested"; path: string };

/**
 * 归一化 `log_export` 的返回。`requestedPath` 是**我们自己传进去**的路径
 * （注册表里没有保存对话框命令，所以路径由用户填/核对；见 [task-23] B 裁决）。
 *
 * 判定规则（宁少不猜）：
 * - 返回对象且带**非空 `path`** → `written`（用后端给的 path；`bytes` 是数字才带上）；
 * - 其它一切（`undefined` / `null` / 非对象 / 对象但没 path）→ `requested`。
 */
export function exportOutcome(result: unknown, requestedPath: string): ExportOutcome {
  if (result !== null && typeof result === "object") {
    const row = result as Record<string, unknown>;
    const path = row["path"];
    if (typeof path === "string" && path.length > 0) {
      const bytes = row["bytes"];
      return typeof bytes === "number"
        ? { kind: "written", path, bytes }
        : { kind: "written", path };
    }
  }
  return { kind: "requested", path: requestedPath };
}

// ================================================================ 轨迹（log_get_trace）

/** 一次尝试（契约是 `channel_id`/`lane_id`，Rust 是 `channel`/`lane`）。 */
export type AttemptView = {
  channel?: string;
  lane?: string;
  node?: string;
  status?: number;
  ms?: number;
  /** Rust 里这里放的是**网关自己判定的 TA 码**（`ta-gateway/src/trace.rs:99-101`）。 */
  reason?: string;
};

/** 头差异归一化结果。`undefined` = 没有这一项（未知），`count === 0` = 无差异。 */
export type HeaderDiffView = {
  added: string[];
  removed: string[];
  changed: [string, string, string][];
  count: number;
};

export function readHeaderDiff(value: unknown): HeaderDiffView | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const obj = value as Record<string, unknown>;

  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

  // ① Rust 的 `ta_core::trace::HeaderDiff`：{ added: string[], removed: string[], changed: [name, client, upstream][] }
  if ("added" in obj || "removed" in obj || "changed" in obj) {
    const changed: [string, string, string][] = Array.isArray(obj["changed"])
      ? (obj["changed"] as unknown[])
          .filter((x): x is unknown[] => Array.isArray(x) && x.length >= 3)
          .map((x) => [String(x[0]), String(x[1]), String(x[2])] as [string, string, string])
      : [];
    const added = strings(obj["added"]);
    const removed = strings(obj["removed"]);
    return { added, removed, changed, count: added.length + removed.length + changed.length };
  }

  // ② 冻结契约的形状：Record<头名, { client?, upstream? }>（bindings.ts:898）
  const changed: [string, string, string][] = [];
  for (const [name, v] of Object.entries(obj)) {
    if (typeof v === "object" && v !== null) {
      const pair = v as { client?: unknown; upstream?: unknown };
      changed.push([name, pair.client === undefined ? "" : String(pair.client), pair.upstream === undefined ? "" : String(pair.upstream)]);
    }
  }
  // `{}` 与"只有不认识的键"都表示没有差异；`{}` 是契约里"正常"的取值
  return { added: [], removed: [], changed, count: changed.length };
}

/** 规范化后的单条轨迹。 */
export type TraceView = {
  requestId: string;
  row: LogRowView;
  attempts: AttemptView[];
  /** `undefined` = 记录里没有这一项（未知）。 */
  headerDiff?: HeaderDiffView;
  /**
   * L6：按 kind 的 token 明细（来自用量表）。
   * `undefined`/`null` = **用量账本里没有这次请求**（日志保留期可能比用量长）→ 界面如实说"没有"。
   */
  tokenKinds?: Record<string, number> | null;
  /** Rust 是 `body_shape`；契约是 `body_shape_in` / `body_shape_out`。 */
  bodyShape?: unknown;
  bodyShapeOut?: unknown;
  requestHeaders?: unknown;
  responseHeaders?: unknown;
  requestBody?: unknown;
  responseBody?: unknown;
  /** 后端返回的整条记录（展开区"原始字段"用；用户数据原样呈现）。 */
  raw: Row;
  /** 后端**多出来**的字段名（契约里没有的）——用于让"原始字段"覆盖完整。 */
  extraKeys: string[];
};

/** 契约 `TraceDetail` 里出现过的键（除此之外的键就是后端多给的，展开区一律照显）。 */
const KNOWN_DETAIL_KEYS = new Set([
  "row",
  "attempts",
  "header_diff",
  "body_shape",
  "body_shape_in",
  "body_shape_out",
  "request_headers",
  "response_headers",
  "request_body",
  "response_body",
]);

export function readTraceDetail(detail: TraceDetail): TraceView {
  const raw = detail as unknown as Row;
  const attemptList = Array.isArray(raw["attempts"]) ? (raw["attempts"] as unknown[]) : [];
  const attempts: AttemptView[] = attemptList
    .filter((a): a is Row => typeof a === "object" && a !== null)
    .map((a) => ({
      channel: pickStr(a, "channel_id", "channel"),
      lane: pickStr(a, "lane_id", "lane"),
      node: pickStr(a, "node"),
      status: pickNum(a, "status"),
      ms: pickNum(a, "ms"),
      reason: pickStr(a, "reason"),
    }));

  const rowRaw = raw["row"];
  return {
    requestId:
      typeof rowRaw === "object" && rowRaw !== null
        ? (pickStr(rowRaw as Row, "request_id") ?? "")
        : "",
    row: readLogRow((rowRaw ?? {}) as TraceRow),
    attempts,
    // 认不出的形状一律当"没有"（`null`），由界面如实显示 —— 不在这里编一份出来
    tokenKinds: typeof raw["token_kinds"] === "object" && raw["token_kinds"] !== null
      ? (raw["token_kinds"] as Record<string, number>)
      : null,
    headerDiff: readHeaderDiff(raw["header_diff"]),
    // 契约里请求体的结构叫 `body_shape_in`，Rust 里只有一个 `body_shape`（请求体）
    bodyShape: raw["body_shape"] ?? raw["body_shape_in"],
    bodyShapeOut: raw["body_shape_out"],
    requestHeaders: raw["request_headers"],
    responseHeaders: raw["response_headers"],
    requestBody: raw["request_body"],
    responseBody: raw["response_body"],
    raw,
    extraKeys: Object.keys(raw).filter((k) => !KNOWN_DETAIL_KEYS.has(k)),
  };
}

/**
 * 尝试链的耗时合计（**只算有记录的**）。
 *
 * 后端的 `TraceAttempt` 没有"第几次尝试发生在什么时刻"，只有一个 `ms`
 * （`ta-store trace_store.rs:46-58`）—— 所以这里给不了真实的时间轴，只能给合计，
 * 并如实标注"部分尝试没有记录耗时"。
 */
export function attemptsTotalMs(attempts: readonly AttemptView[]): {
  total?: number;
  partial: boolean;
} {
  let total = 0;
  let seen = false;
  let partial = false;
  for (const a of attempts) {
    if (a.ms === undefined) {
      partial = true;
      continue;
    }
    total += a.ms;
    seen = true;
  }
  return { total: seen ? total : undefined, partial };
}

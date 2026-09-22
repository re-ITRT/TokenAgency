/**
 * 服务商页（[07 §3]）的**纯逻辑**：没有 JSX、没有 React、没有 IPC。
 *
 * 把这个页面最容易被写错的一件事收在这里并用单测钉住：**读后端的服务商与凭据**。
 *
 * ## 契约与实现不一致（这是事实，不是猜测；来源逐条标注）
 *
 * | 命令 | 冻结契约（`src/shared/bindings.ts`，本任务不许改） | Rust 实现（权威） |
 * |---|---|---|
 * | `provider_list` | `ProviderView[]`（bindings.ts:1018） | `Vec<Provider>` —— **完整的配置实体**（`src-tauri/src/commands/catalog.rs:53`） |
 * | `provider_get` | `ProviderView`（bindings.ts:1019） | `Option<Provider>` —— 可能为 `null`（catalog.rs:59） |
 * | `credential_list` | `CredentialView.quota` + `provider_name` + `channel_count`（bindings.ts:603-610） | `{ credential, quota_state, used_pct }`（catalog.rs:21-28） |
 * | `lane_list` | `LaneView.{type, node_count, health…}`（bindings.ts:679-686） | `{ lane, latest, channel_count }`（catalog.rs:39-45） |
 *
 * 两边**各有对方没有的列**：契约视图有 `status` / `success_rate_24h` / `avg_latency_ms` /
 * `channel_count`，Rust 有 `auth` / `timeout` / `tls` / `headers` / `passthrough` / `lane` /
 * `model_prefix`。所以这里按**并集**读，每一列都标出它可能来自哪一边；
 * **都读不到就是 `undefined`**（页面画「—」或「未知」），不填 0、不拿别的列推断
 * （[06 §2.1] B2：不拿不确定冒充确定）。
 *
 * 这两处偏差是**契约生成（T1 的 `npm run gen:bindings`）落地前**的现实；
 * 生成物覆盖 `bindings.ts` 之后，并集读法仍然成立（多认一个名字而已）。
 *
 * ## 为什么详情页也从 `provider_list` 取那一行
 *
 * `provider_get` 与 `provider_list` 在 Rust 里读的是**同一份内存快照**
 * （`state.doc().providers`，catalog.rs:53-62），取哪一条数据都一样。
 * 但 `provider_list` 给出的是**完整目录**，于是"id 不在目录里"是一个有意义的结论
 * （页面据此显示"目录里没有这个服务商"），而不是"某个接口没返回"。
 * 浏览器开发路径（`npm run dev`，无 Tauri）只 mock 了 `provider_list`，
 * 走同一条命令也让两个页面在两种环境下行为一致 —— 一处取数、一处口径。
 */

/** 后端返回的原始对象（键名是后端的事，所以按 `Record` 处理并逐条注明来源）。 */
type Row = Record<string, unknown>;

function asRow(value: unknown): Row {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Row)
    : {};
}

function pickStr(row: Row, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const v = row[key];
    if (typeof v === "string" && v !== "") return v;
  }
  return undefined;
}

function pickNum(row: Row, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const v = row[key];
    if (typeof v === "number" && Number.isFinite(v)) return v;
  }
  return undefined;
}

function pickBool(row: Row, ...keys: string[]): boolean | undefined {
  for (const key of keys) {
    const v = row[key];
    if (typeof v === "boolean") return v;
  }
  return undefined;
}

// ================================================================ 服务商

/** 一条附加头。名字与值都**原样**（用户数据不做清洗，[15 §2.2]）。 */
export type HeaderPair = { name: string; value: string };

/** 归一化后的一条服务商。**每个可选字段都标了它来自哪一边**；缺 = 未知，不是 0。 */
export type ProviderRow = {
  id: string;
  name: string;
  protocol?: string;
  baseUrl?: string;
  enabled?: boolean;

  // ---- 契约 / mock 的视图形状专有（Rust 的 `provider_list` 没有这几列）
  /** `ok | warn | error | off`。**没有这一列时不显示"正常"**。 */
  status?: string;
  /** 近 24h 成功率（0~1）。 */
  successRate24h?: number;
  avgLatencyMs?: number;
  /** 挂载的通道数（视图列；Rust 侧要靠通道列表算，本页不猜）。 */
  channelCount?: number;

  // ---- Rust 的完整 `Provider` 专有（浏览器 mock 的视图形状没有）
  lane?: string;
  modelPrefix?: string;
  authScheme?: string;
  authHeader?: string;
  authPrefix?: string;
  headers: HeaderPair[];
  onHeaderConflict?: string;
  passthroughHeaderMode?: string;
  passthroughBody?: string;
  timeoutConnectS?: number;
  timeoutReadS?: number;
  timeoutTotalS?: number;
  tlsInsecure?: boolean;
  /** **只记录"配没配"**：额外 CA 是私密材料，页面只显示是否配置，不打印内容。 */
  tlsExtraCa: boolean;
  tlsMinVersion?: string;

  /**
   * 后端是否返回了连接配置字段（`auth` / `timeout` / `tls` / `headers` / `passthrough` /
   * `lane` / `model_prefix` 里**任何一个**）。
   *
   * `false` = 拿到的是视图（只有名称、协议、地址、健康度）→ 连接区应当**明说不可用**，
   * 而不是排一排空占位值。
   */
  hasConnectionConfig: boolean;

  /** 后端返回的原始对象（"原始字段"区原样打印）。 */
  raw: Row;
};

/** 命中任意一个就说明后端给的是**完整 `Provider`** 而不是视图。 */
const CONFIG_KEYS = [
  "auth",
  "lane",
  "headers",
  "on_header_conflict",
  "passthrough",
  "timeout",
  "tls",
  "model_prefix",
] as const;

/** `BTreeMap<String, String>` → 有序数组；非字符串的值丢掉（不 `String()` 硬转）。 */
function readHeaders(value: unknown): HeaderPair[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [];
  return Object.entries(value as Row)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .map(([name, headerValue]) => ({ name, value: headerValue }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * 读一条服务商。入参是契约类型，运行时可能是契约的视图形状或 Rust 的完整实体。
 */
export function readProvider(raw: unknown): ProviderRow {
  const row = asRow(raw);
  const auth = asRow(row["auth"]);
  const timeout = asRow(row["timeout"]);
  const tls = asRow(row["tls"]);
  const passthrough = asRow(row["passthrough"]);
  const extraCa = tls["extra_ca_pem"];

  return {
    id: pickStr(row, "id") ?? "",
    name: pickStr(row, "name") ?? "",
    protocol: pickStr(row, "protocol"),
    baseUrl: pickStr(row, "base_url"),
    enabled: pickBool(row, "enabled"),

    status: pickStr(row, "status"),
    successRate24h: pickNum(row, "success_rate_24h"),
    avgLatencyMs: pickNum(row, "avg_latency_ms"),
    channelCount: pickNum(row, "channel_count"),

    lane: pickStr(row, "lane"),
    modelPrefix: pickStr(row, "model_prefix"),
    authScheme: pickStr(auth, "scheme"),
    authHeader: pickStr(auth, "header"),
    authPrefix: pickStr(auth, "prefix"),
    headers: readHeaders(row["headers"]),
    onHeaderConflict: pickStr(row, "on_header_conflict"),
    passthroughHeaderMode: pickStr(passthrough, "header_mode"),
    passthroughBody: pickStr(passthrough, "body"),
    timeoutConnectS: pickNum(timeout, "connect_s"),
    timeoutReadS: pickNum(timeout, "read_s"),
    timeoutTotalS: pickNum(timeout, "total_s"),
    tlsInsecure: pickBool(tls, "insecure"),
    tlsExtraCa: typeof extraCa === "string" && extraCa !== "",
    tlsMinVersion: pickStr(tls, "min_version"),

    hasConnectionConfig: CONFIG_KEYS.some((key) => key in row),
    raw: row,
  };
}

// ================================================================ 凭据

/** 归一化后的一条凭据（额度列分两种后端形状，见类型注释）。 */
export type CredentialRow = {
  id: string;
  /** 挂载关系写在这里（属于哪个服务商）。 */
  provider?: string;
  label: string;
  /** `${secret:name}` / `${env:VAR}` 之类的**引用**；明文永远不该出现在这里。 */
  secret?: string;
  lane?: string;
  enabled?: boolean;
  tags: string[];

  // 额度配置（`credential.quota`，两种形状都有）
  metric?: string;
  /** `undefined` = **未设上限**（[05 §2.1] 要求显式显示）。 */
  limit?: number;
  currency?: string;
  window?: string;
  resetDay?: number;

  // 额度运行态：Rust 给 `{quota_state, used_pct}`，契约/mock 给 `{quota: QuotaState}`
  /** 已用比率 0~1。**只有后端给了才有值**（前端不自己除）。 */
  usedPct?: number;
  /** `ok | soft | exhausted | unknown`。 */
  quotaState?: string;
  usedTokens?: number;
  usedRequests?: number;
  usedCost?: number;

  /** 契约/mock 才有：这份凭据里是不是明文密钥。 */
  plaintextSecret?: boolean;

  raw: Row;
};

export function readCredential(raw: unknown): CredentialRow {
  const row = asRow(raw);
  const credential = asRow(row["credential"]);
  const quota = asRow(credential["quota"]);
  const state = asRow(row["quota_state"] ?? row["quota"]);
  const tags = credential["tags"];

  return {
    id: pickStr(credential, "id") ?? "",
    provider: pickStr(credential, "provider"),
    label: pickStr(credential, "label") ?? "",
    secret: pickStr(credential, "secret"),
    lane: pickStr(credential, "lane"),
    enabled: pickBool(credential, "enabled"),
    tags: Array.isArray(tags) ? tags.filter((x): x is string => typeof x === "string") : [],

    metric: pickStr(quota, "metric"),
    limit: pickNum(quota, "limit"),
    currency: pickStr(quota, "currency"),
    window: pickStr(quota, "window"),
    resetDay: pickNum(quota, "reset_day"),

    usedPct: pickNum(row, "used_pct"),
    quotaState: pickStr(state, "state"),
    usedTokens: pickNum(state, "used_tokens"),
    usedRequests: pickNum(state, "used_requests"),
    usedCost: pickNum(state, "used_cost"),

    plaintextSecret: pickBool(row, "has_plaintext_secret"),
    raw: row,
  };
}

// ================================================================ 挂载统计

/** 挂载情况。`undefined` = **读不到**（页面显示"未知"），`0` = 确实是 0。 */
export type MountSummary = { credentials?: number; channels?: number };

/**
 * 一条服务商挂了多少凭据 / 通道。
 *
 * - 凭据数来自 `credential_list`（按 `credential.provider` 归并）：列表读不到就是 `undefined`；
 * - 通道数只取后端视图给的 `channel_count`（Rust 的 `provider_list` 没有这一列 → `undefined`）。
 */
export function mountOf(
  row: ProviderRow,
  credentials: readonly CredentialRow[] | undefined,
): MountSummary {
  return {
    credentials:
      credentials === undefined
        ? undefined
        : credentials.filter((c) => c.provider === row.id).length,
    channels: row.channelCount,
  };
}

/** 属于某个服务商的凭据（顺序保持后端给的顺序）。 */
export function credentialsOf(
  credentials: readonly CredentialRow[] | undefined,
  providerId: string,
): CredentialRow[] {
  return (credentials ?? []).filter((c) => c.provider === providerId);
}

// ================================================================ 线路（只用来把 lane id 换成名字）

/** 线路下拉/显示需要的两个字段（契约与 Rust 的 `LaneView` 都包着 `lane`）。 */
export type LaneOption = { id?: string; name?: string };

export function readLane(raw: unknown): LaneOption {
  const lane = asRow(asRow(raw)["lane"]);
  return { id: pickStr(lane, "id"), name: pickStr(lane, "name") };
}

/** `lane` id → 线路名。读不到时返回 `undefined`（页面退回显示 id，不编名字）。 */
export function laneNameOf(
  lanes: readonly LaneOption[],
  laneId: string | undefined,
): string | undefined {
  if (laneId === undefined) return undefined;
  return lanes.find((l) => l.id === laneId)?.name;
}

/**
 * 网络三页（`/network`、`/network/nodes`、`/network/assignments`）的**纯逻辑**：
 * 没有 JSX、没有 React、没有 IPC。这个页面组最容易被写错的三件事收在这一个文件里，并用单测钉住。
 *
 * ## 一、契约与实现的形状偏差（这是事实，来源已逐条标注）
 *
 * `src/shared/bindings.ts` 是**手写契约**（105 条命令的签名），而真正注册的命令返回的是
 * Rust 侧结构体。两者在"线路 / 凭据 / 服务商"这三处**对不上**（订阅那一处曾经也对不上，
 * 第 71 轮已由后端补齐 `SubscriptionView`，见下表）：
 *
 * | 命令 | 冻结契约 | Rust 实际返回（权威） |
 * |---|---|---|
 * | `lane_list` | `LaneView { lane, type, node_count, current_node, latency_ms, health }` | `LaneView { lane, latest: HealthSample?, channel_count }`（`src-tauri/src/commands/catalog.rs`） |
 * | `subscription_list` | `SubscriptionView[]`（带 `node_count` / `group_count` / `last_update_ms` / `parser`） | **已对齐**：`net.rs::views_of_subscriptions` 返回 `SubscriptionView`，解析状态取自版本存储元数据（**没刷过 = 字段缺失**，不给 0）。第 71 轮之前这里返回裸 `Subscription`，于是节点页的硬前置（[19 §2.3]）恒判"没解析过"、节点清单恒空 |
 * | `credential_list` | `CredentialView { credential, provider_name, quota, channel_count }` | `CredentialView { credential, quota_state, used_pct }` |
 * | `provider_list` | `ProviderView`（**没有 `lane` 字段**） | `Vec<Provider>`（有 `lane`） |
 *
 * 所以本模块的读法是**两套名字的并集**，并且严格区分三种"没有值"：
 *
 * 1. **字段不存在** → 后端这一版没给这个信息 → UI 显示「未读取」（`readable: false`）；
 * 2. **字段存在但为 `null`** → 后端明确说"没有" → UI 显示「未指定」（如 `lane: null` = 继承上一级）；
 * 3. **数组为空** → 配置里确实是空的 → UI 显示诚实的空状态。
 *
 * 把 1 和 3 混为一谈（"没有通道" vs "读不到通道"）就是拿不确定冒充确定 —— 那正是本页要避免的事。
 *
 * ## 二、两条硬纪律
 *
 * - **不保留订阅原始对象**：`Subscription.url` 里带订阅 token，所以 `SubscriptionRow`
 *   刻意**不提供** `raw` 逃生口 —— 原始地址没有任何进入 DOM 的路径（`maskSubscriptionUrl` 是唯一出口）。
 * - **不自己算"最终生效线路"**：五级继承（[04 §8.1]）的解析归后端（`lane_assignments`）所有。
 *   前端再实现一遍就是两个真相源。这里只做**配置里显式写了什么**的收集，以及它的倒排。
 *
 * ## 三、契约生成落地之后
 *
 * T1 的 `npm run gen:bindings` 覆盖 `bindings.ts` 之后，并集读法与三态区分**仍然成立**
 * （顶多多认一个名字、少一次「未读取」），不需要改这个文件的形状约定。
 */

import type { I18nKey } from "@/shared/i18n";
import type { SemStatus } from "@/shared/components";
import type { GroupType, Lane, LaneKind, Subscription, SubscriptionFormat } from "@/shared/bindings";

// ================================================================ 基础读取件

type Row = Record<string, unknown>;

/** 把任意值收窄成可安全取值的一层对象（不是对象就给空对象）。 */
function asRow(value: unknown): Row {
  return typeof value === "object" && value !== null ? (value as Row) : {};
}

/** 键**存在**（哪怕值是 `null`）—— 用来区分「未读取」与「未指定」。 */
function hasKey(row: Row, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(row, key);
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

function pickArr(row: Row, key: string): unknown[] | undefined {
  const v = row[key];
  return Array.isArray(v) ? v : undefined;
}

/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
export const DASH = "—";

// ================================================================ 网络代理是不是**在用**

/**
 * `net.kernel.path` 的读取（[18 §2.1]，[19 §2.2]）。
 *
 * **注**：上一版这里还有一个 `networkScope()`，用"内核路径 / 订阅 / 线路三项是否都空"来决定
 * `/network` 要不要整页隐藏（[18 §2.2] 的"选装入口"）。那个口径**已被 [19 §2.2] 取代**：
 * 网络代理默认开启（默认直连），这一页**永远渲染**各区块，没有"启用网络代理"这个动作。
 * 所以那个函数连同它的类型一起删掉了 —— 留一个不再决定任何事情的"门控"只会让下一个人读错。
 */
/**
 * 从 `config_get` 的整份配置里读 `net.kernel.path`（**不改写后端给的值**）。
 *
 * - 返回 `undefined` = **整份配置还没读到**（不是"用户没配"）；
 * - 返回 `""` = 读到了，但用户没填（`null` / 缺键 / 空串）；
 * - 纯空白原样返回 —— "算不算没配"由界面在一处判定（`trim()` 只在那里做一次）。
 */
export function readKernelPath(doc: unknown): string | undefined {
  const d = asRow(doc);
  if (Object.keys(d).length === 0) return undefined;
  const kernel = asRow(asRow(d["net"])["kernel"]);
  return pickStr(kernel, "path") ?? "";
}

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 五种线路（[04 §7.1]）。 */
export const LANE_KINDS = [
  "direct",
  "subscription_auto",
  "subscription_group",
  "subscription_node",
  "filter",
] as const;

const LANE_KIND_LABEL: Record<string, I18nKey> = {
  direct: "network.laneKind.direct",
  subscription_auto: "network.laneKind.subscription_auto",
  subscription_group: "network.laneKind.subscription_group",
  subscription_node: "network.laneKind.subscription_node",
  filter: "network.laneKind.filter",
};

/** 后端给的类型名 → 文案 key。**契约外的取值原样显示**（是后端说的话，不改写）。 */
export function laneKindLabel(kind: string | undefined): I18nKey | undefined {
  if (kind === undefined) return undefined;
  return LANE_KIND_LABEL[kind];
}

const SUBSCRIPTION_FORMAT_LABEL: Record<string, I18nKey> = {
  auto: "network.format.auto",
  clash: "network.format.clash",
  base64_links: "network.format.base64_links",
  plain_links: "network.format.plain_links",
  convert_links: "network.format.convert_links",
};

export function subscriptionFormatLabel(format: string | undefined): I18nKey | undefined {
  if (format === undefined) return undefined;
  return SUBSCRIPTION_FORMAT_LABEL[format];
}

/** 线路健康（[04 §7.2] 的判定 + `unknown`）。 */
export type LaneHealth = "ok" | "degraded" | "down" | "unknown";

export const HEALTH_LABEL: Record<LaneHealth, I18nKey> = {
  ok: "network.health.ok",
  degraded: "network.health.degraded",
  down: "network.health.down",
  unknown: "network.health.unknown",
};

/** 全站颜色语义唯一（[15 §3.2]）：绿=正常 · 黄=警告 · 红=失败 · 灰=未知。 */
export const HEALTH_STATUS: Record<LaneHealth, SemStatus> = {
  ok: "ok",
  degraded: "warn",
  down: "error",
  unknown: "off",
};

function asHealth(value: string | undefined): LaneHealth | undefined {
  return value === "ok" || value === "degraded" || value === "down" || value === "unknown"
    ? value
    : undefined;
}

/** 后端给的健康取值是否在我们的词表里。**不在词表里就原样显示**（不改写成"未知"）。 */
export function isKnownHealth(value: string): boolean {
  return asHealth(value) !== undefined;
}

/**
 * 线路 id → 用户能认的名字。解析不出来返回 `undefined`（调用方显示 id，**不猜一个名字**）。
 * 只看后端给的这一份线路列表，不查配置、不推断。
 */
export function laneNameOf(laneId: string, lanes: readonly LaneRow[]): string | undefined {
  return lanes.find((lane) => lane.id === laneId)?.name;
}

// ================================================================ 订阅

/**
 * 归一化后的订阅。**刻意不带 `raw`**：原始对象里有完整订阅地址（含 token），
 * 不给它任何进入 DOM 的机会（见文件头「二」）。
 */
export type SubscriptionRow = {
  id: string;
  name?: string;
  /** **已遮蔽**密钥参数的地址（`maskSubscriptionUrl` 是唯一出口）。 */
  url: string;
  format?: string;
  /** Rust 侧 `#[serde(default)]` 的语义：省略 = 启用。 */
  enabled: boolean;
  intervalS?: number;
  keepVersions?: number;
  /** 配置里声明的节点过滤（正则，原样）。 */
  include?: string;
  exclude?: string;
  healthEnabled?: boolean;
  healthUrl?: string;
  healthIntervalS?: number;
  healthToleranceMs?: number;
  /** 以下来自**订阅解析结果**：只有契约的 `SubscriptionView` 形状才有。 */
  nodeCount?: number;
  groupCount?: number;
  lastUpdateMs?: number;
  parser?: string;
  error?: string;
  /**
   * **上次已知的流量**（来自 `subscription_traffic` 的持久化观测值，不是这份配置实体自带的）：
   * - `undefined` = 还没读到（该命令还没回来 / 没有这一行）→ 界面「未读取」；
   * - `null` = 后端明确说"从没成功刷新过" → 界面「尚未获取」；
   * - 对象 = 上次已知（各字段仍可能缺失）。
   *
   * 为什么不再读契约里的 `SubscriptionView.userinfo`：Rust 返回的是**裸实体**（没有这个字段），
   * 那份数据永远读不到；而流量是**运行时观测值**，真源是后端持久化列（[18 §1.2]）。
   */
  traffic?: TrafficRow | null;
};

/** 会写进日志 / 截图就泄露的参数名（按名字判定，保守覆盖）。 */
const SENSITIVE_PARAM = /(token|key|secret|passwd|password|auth|uuid)/i;

/** 遮蔽用的占位（固定长度，不泄漏原值长度）。 */
export const MASKED = "***";

/**
 * 遮蔽订阅地址里的密钥类查询参数。**不是"清洗用户数据"**：
 * 组名/节点名（会与内核里的名字比对）一律原样；这里遮的是**凭据**，
 * 且只遮查询串里按名字判定敏感的那些值。不是 URL 形状的字符串原样返回。
 */
export function maskSubscriptionUrl(url: string): string {
  if (url === "") return "";
  const question = url.indexOf("?");
  if (question < 0) return url;
  const head = url.slice(0, question + 1);
  const query = url.slice(question + 1);
  const parts = query.split("&").map((part) => {
    const eq = part.indexOf("=");
    if (eq <= 0) return part;
    const name = part.slice(0, eq);
    return SENSITIVE_PARAM.test(name) ? `${name}=${MASKED}` : part;
  });
  return head + parts.join("&");
}

/**
 * 读一条订阅。运行时可能是契约的 `SubscriptionView`（实体包在 `subscription` 里），
 * 也可能是 Rust 的裸 `Subscription`。
 */
export function readSubscription(raw: unknown): SubscriptionRow {
  const outer = asRow(raw);
  const innerRaw = outer["subscription"];
  const wrapped = typeof innerRaw === "object" && innerRaw !== null;
  const inner = wrapped ? asRow(innerRaw) : outer;

  const filter = asRow(inner["filter"]);
  const health = asRow(inner["health_check"]);
  // 解析结果只在**外层**（`SubscriptionView` 把它们放在实体旁边）
  const parsed = wrapped ? outer : {};

  return {
    id: pickStr(inner, "id") ?? "",
    name: pickStr(inner, "name"),
    url: maskSubscriptionUrl(pickStr(inner, "url") ?? ""),
    format: pickStr(inner, "format"),
    enabled: pickBool(inner, "enabled") ?? true,
    intervalS: pickNum(inner, "interval_s"),
    keepVersions: pickNum(inner, "keep_versions"),
    include: pickStr(filter, "include"),
    exclude: pickStr(filter, "exclude"),
    healthEnabled: pickBool(health, "enabled"),
    healthUrl: pickStr(health, "url"),
    healthIntervalS: pickNum(health, "interval_s"),
    healthToleranceMs: pickNum(health, "tolerance_ms"),
    nodeCount: pickNum(parsed, "node_count"),
    groupCount: pickNum(parsed, "group_count"),
    lastUpdateMs: pickNum(parsed, "last_update_ms"),
    parser: pickStr(parsed, "parser"),
    error: pickStr(parsed, "error"),
    // `traffic` 由页面用 `subscription_traffic`（另一条命令）补上 —— 见 `readTrafficMap`
  };
}

// ================================================================ 订阅流量（[18 §1]，等价于 Clash 面板那一行）

/**
 * 一份订阅**上次已知**的流量观测值（来自 `subscription_traffic` 的持久化列）。
 *
 * **契约是「未知 = null」，这里是「未知 = `undefined`」**：读取层把后端的 `null` 收成
 * `undefined`（界面只需要"有值 / 没值"两种）。但 **`0` 必须原样保留** —— 契约明写
 * `upload_bytes` / `download_bytes` 的 `0` 是**真实观测值**（真没用过），
 * 把它当成"未知"与"把缺失当 0"是同一类错误的两面。
 */
export type TrafficRow = {
  /** `fetched_at_ms`（Unix 毫秒）→ 界面上的「更新时间」。 */
  fetchedAtMs?: number;
  uploadBytes?: number;
  downloadBytes?: number;
  /** **后端给的**总量；前端**不自己算**（再算一遍就是第二个真相源）。 */
  totalBytes?: number;
  /** `expire_at_s`（**Unix 秒**）；契约里 `0` = 不适用 → 这里就是 `undefined`。 */
  expireAtS?: number;
};

/**
 * `subscription_traffic` 的返回 → `id → 流量`（三态，**不能混**）：
 * - 键**不存在** → 没读到（命令还没回来 / 后端没这一行）→ 界面「未读取」；
 * - 值是 `null` → 后端明确说"**从没成功刷新过**" → 界面「尚未获取」；
 * - 值是对象 → 上次已知的观测值（字段仍可能缺失 → 缺哪样不显示哪样）。
 */
export function readTrafficMap(rows: unknown): Map<string, TrafficRow | null> {
  const out = new Map<string, TrafficRow | null>();
  if (!Array.isArray(rows)) return out;
  for (const item of rows) {
    const outer = asRow(item);
    const id = pickStr(outer, "id");
    if (id === undefined) continue;
    const raw = outer["traffic"];
    if (raw === null || raw === undefined) {
      out.set(id, null);
      continue;
    }
    const t = asRow(raw);
    const expireAtS = pickNum(t, "expire_at_s");
    out.set(id, {
      fetchedAtMs: pickNum(t, "fetched_at_ms"),
      uploadBytes: pickNum(t, "upload_bytes"),
      downloadBytes: pickNum(t, "download_bytes"),
      totalBytes: pickNum(t, "total_bytes"),
      // 契约里 `expire_at_s: 0` = "不适用"，与缺失同义（不显示「到期」）
      expireAtS: expireAtS === 0 ? undefined : expireAtS,
    });
  }
  return out;
}

/**
 * 一行流量 → 界面上要显示的三样（[18 §1.3]）。**纯函数、不做 i18n**（文案由页面拼）。
 *
 * 规则（"缺哪样就不显示那一样"）：
 * - `used = upload + download` —— **两者都有**才算（缺一个就没有"已用"，**不填 0**）；
 * - `total` 只用后端给的值；缺失 → 没有总量，也没有百分比；
 * - `usedPct = round(used / total * 100)`；`total` 缺失或为 0 → 没有百分比（不除零）；
 * - `fetchedAtMs` / `expireAtS` 各自独立，缺了就不显示那一条。
 */
export type TrafficSummary =
  | { kind: "none" }
  | {
      kind: "known";
      fetchedAtMs?: number;
      usedBytes?: number;
      totalBytes?: number;
      usedPct?: number;
      expireAtS?: number;
    };

/** `null`（从没成功刷新过）与 `undefined`（还没读到）都由**调用方**决定怎么画；这里只算"已知"的那份。 */
export function summarizeTraffic(traffic: TrafficRow | null | undefined): TrafficSummary {
  if (traffic === null || traffic === undefined) return { kind: "none" };
  const { uploadBytes, downloadBytes, totalBytes } = traffic;
  const usedBytes =
    uploadBytes === undefined || downloadBytes === undefined
      ? undefined
      : uploadBytes + downloadBytes;
  const usedPct =
    usedBytes === undefined || totalBytes === undefined || totalBytes === 0
      ? undefined
      : Math.round((usedBytes / totalBytes) * 100);
  return {
    kind: "known",
    fetchedAtMs: traffic.fetchedAtMs,
    usedBytes,
    totalBytes,
    usedPct,
    expireAtS: traffic.expireAtS,
  };
}

// ================================================================ 写载荷（订阅）

/** 契约里 `SubscriptionFormat` 的五个取值（表单的格式下拉用它，顺序即显示顺序）。 */
export const SUBSCRIPTION_FORMATS: readonly SubscriptionFormat[] = [
  "auto",
  "clash",
  "base64_links",
  "plain_links",
  "convert_links",
];

/**
 * 订阅表单的原始输入。数字字段**保持字符串**：`""`（用户没填）与 `0` 不是一回事 ——
 * 前者不发这个字段（让后端默认值生效），后者是"我确实要 0"。
 */
export type SubscriptionDraft = {
  id: string;
  name: string;
  url: string;
  format: SubscriptionFormat;
  intervalS: string;
  keepVersions: string;
  include: string;
  exclude: string;
  enabled: boolean;
};

/**
 * 数字字段的解析，三态：
 * - `undefined` = 没填 → **不发这个字段**（后端默认值说了算）；
 * - `null` = 填了但不是非负整数 → 调用方报错，**不静默丢掉**（静默丢 = 用户以为改了）；
 * - `number` = 合法值。
 */
export function parseOptionalCount(text: string): number | undefined | null {
  const trimmed = text.trim();
  if (trimmed === "") return undefined;
  const value = Number(trimmed);
  if (!Number.isInteger(value) || value < 0) return null;
  return value;
}

/** 三个没有 serde 默认值的字段（`id` / `name` / `url`）是否都填了。空值不发给后端。 */
export function subscriptionDraftComplete(draft: SubscriptionDraft): boolean {
  return draft.id.trim() !== "" && draft.name.trim() !== "" && draft.url.trim() !== "";
}

/**
 * 表单草稿 → `subscription_upsert` 的载荷。
 *
 * **只放用户真的填了的字段**：契约的 `Subscription` 把 `user_agent` / `health_check` 也标成必填，
 * 但 Rust 侧对它们有 `#[serde(default)]`（`crates/ta-core/src/domain/mod.rs`）—— **权威在 Rust**。
 * 前端补一份默认值就是把后端默认值复刻成第二个真相源：后端改了默认值，前端还在发旧值，
 * 而且不会报错。缺的字段交给后端填 —— 下面这一处 `as` 就是那个决定的唯一落点。
 */
export function subscriptionPayload(draft: SubscriptionDraft): Subscription {
  const payload: Record<string, unknown> = {
    id: draft.id.trim(),
    name: draft.name.trim(),
    url: draft.url.trim(),
    format: draft.format,
    enabled: draft.enabled,
  };
  const intervalS = parseOptionalCount(draft.intervalS);
  if (intervalS !== undefined && intervalS !== null) payload.interval_s = intervalS;
  const keepVersions = parseOptionalCount(draft.keepVersions);
  if (keepVersions !== undefined && keepVersions !== null) payload.keep_versions = keepVersions;

  const include = draft.include.trim();
  const exclude = draft.exclude.trim();
  if (include !== "" || exclude !== "") {
    const filter: Record<string, string> = {};
    if (include !== "") filter.include = include;
    if (exclude !== "") filter.exclude = exclude;
    payload.filter = filter;
  }
  return payload as unknown as Subscription;
}

// ================================================================ 线路

/**
 * 归一化后的一条线路。字段注释标出它可能来自哪一套名字。
 *
 * 只建模**界面会用到**的字段（`provider_key` / `user_agent` 之类没地方显示的东西不进来）：
 * 一个读了却从不显示的字段，读错了也没人会发现。
 */
export type LaneRow = {
  id: string;
  name?: string;
  /** 契约是 `lane.kind`；Rust 也是 `lane.kind`（`type` 只是契约里的冗余字段）。 */
  kind?: string;
  subscriptionId?: string;
  /** `kind: subscription_group` 时订阅里自带的组名 —— **可能含 emoji，原样**。 */
  group?: string;
  /** 该组的策略（内核的组类型：select / url-test / fallback / load-balance）。 */
  groupType?: string;
  /** `kind: subscription_node` 时的固定节点名 —— **可能含 emoji，原样**。 */
  node?: string;
  filter?: string;
  excludeFilter?: string;
  enabled: boolean;
  health: LaneHealth;
  /** 后端原样给的健康取值（契约外的取值要能原样显示，而不是被改写成"未知"）。 */
  healthRaw?: string;
  /** 契约是 `latency_ms`；Rust 是 `latest.latency_ms`。 */
  latencyMs?: number;
  /** 契约是 `current_node`；Rust 是 `latest.node`。 */
  currentNode?: string;
  /** 契约的 `node_count` = 订阅解析出的节点数（需要 `subscription_nodes`）。 */
  nodeCount?: number;
  /** Rust 的 `channel_count` = **显式**把 lane 指向这条线的通道数（与节点数不是一回事）。 */
  channelCount?: number;
  sampledAtMs?: number;
  sampleStatus?: number;
};

/**
 * 读一条线路。运行时可能是契约的 `LaneView { lane, type, node_count, current_node, latency_ms, health }`，
 * 也可能是 Rust 的 `LaneView { lane, latest, channel_count }`。
 */
export function readLane(raw: unknown): LaneRow {
  const outer = asRow(raw);
  const laneRaw = outer["lane"];
  const lane = typeof laneRaw === "object" && laneRaw !== null ? asRow(laneRaw) : outer;
  const latest = asRow(outer["latest"]);
  const hasSample = Object.keys(latest).length > 0;

  const healthRaw = pickStr(outer, "health");
  const okFlag = pickBool(latest, "ok");
  // 契约直接给 health；Rust 只给一个 `HealthSample` —— 从它的 `ok` 如实推出 ok/down，
  // **没有样本就是 unknown**（不因为"没报错"就画绿点）。
  const derived: LaneHealth = okFlag === undefined ? "unknown" : okFlag ? "ok" : "down";
  const health = asHealth(healthRaw) ?? (hasSample ? derived : "unknown");

  return {
    id: pickStr(lane, "id") ?? "",
    name: pickStr(lane, "name"),
    kind: pickStr(lane, "kind") ?? pickStr(outer, "type"),
    subscriptionId: pickStr(lane, "subscription"),
    group: pickStr(lane, "group"),
    groupType: pickStr(lane, "group_type"),
    node: pickStr(lane, "node"),
    filter: pickStr(lane, "filter"),
    excludeFilter: pickStr(lane, "exclude_filter"),
    enabled: pickBool(lane, "enabled") ?? true,
    health,
    healthRaw,
    latencyMs: pickNum(outer, "latency_ms") ?? pickNum(latest, "latency_ms"),
    currentNode: pickStr(outer, "current_node") ?? pickStr(latest, "node"),
    nodeCount: pickNum(outer, "node_count"),
    channelCount: pickNum(outer, "channel_count"),
    sampledAtMs: pickNum(latest, "ts_ms"),
    sampleStatus: pickNum(latest, "status"),
  };
}

// ================================================================ 写载荷（线路）

/** 组策略（内核的组类型）。**kebab-case** —— 与其它枚举不同，`bindings.ts` 专门注明了。 */
export const LANE_GROUP_TYPES: readonly GroupType[] = [
  "select",
  "url-test",
  "fallback",
  "load-balance",
];

/** 组策略 → 文案 key。取值表就是内核的那四个（kebab-case）。 */
export const GROUP_TYPE_LABEL: Record<string, I18nKey> = {
  select: "network.groupType.select",
  "url-test": "network.groupType.url-test",
  fallback: "network.groupType.fallback",
  "load-balance": "network.groupType.load-balance",
};

/** 线路表单的原始输入（可选字段保持字符串：`""` = 没填 → **不发这个字段**）。 */
export type LaneDraft = {
  id: string;
  name: string;
  kind: LaneKind;
  subscription: string;
  group: string;
  groupType: string;
  node: string;
  filter: string;
  excludeFilter: string;
  enabled: boolean;
};

/**
 * 后端 `Lane::missing_kind_field()`（`crates/ta-core/src/domain/behavior.rs`）的**前端镜像**：
 * 非 `direct` 必须有 `subscription`；`subscription_group` 要 `group`；`subscription_node` 要 `node`；
 * `filter` 要 `filter`。返回缺的那个**契约字段名**（给用户看的句子由 i18n 拼）。
 *
 * 这不是"前端自己发明规则"：是同一条规则提前检查一次，免得把注定被拒的请求发出去 ——
 * 规则改动时两边会一起露出来（后端那边有 `missing_kind_field` 的单测）。
 */
export function laneMissingField(draft: LaneDraft): string | undefined {
  if (draft.id.trim() === "") return "id";
  if (draft.name.trim() === "") return "name";
  if (draft.kind === "direct") return undefined;
  if (draft.subscription.trim() === "") return "subscription";
  if (draft.kind === "subscription_group") return draft.group.trim() === "" ? "group" : undefined;
  if (draft.kind === "subscription_node") return draft.node.trim() === "" ? "node" : undefined;
  if (draft.kind === "filter") return draft.filter.trim() === "" ? "filter" : undefined;
  return undefined;
}

/**
 * 草稿 → `lane_upsert` 的载荷：**只带用户真的填了的字段**（其余交后端 `#[serde(default)]`）。
 * 契约的 `Lane` 把可选字段写成 `string | null`，Rust 侧是 `Option<String>` —— 不填就是 `None`，
 * 与显式发一个空字符串**不是**一回事（后者会覆盖掉订阅里已有的名字）。
 */
export function lanePayload(draft: LaneDraft): Lane {
  const payload: Record<string, unknown> = {
    id: draft.id.trim(),
    name: draft.name.trim(),
    kind: draft.kind,
    enabled: draft.enabled,
  };
  const optionalFields: [string, string][] = [
    ["subscription", draft.subscription],
    ["group", draft.group],
    ["group_type", draft.groupType],
    ["node", draft.node],
    ["filter", draft.filter],
    ["exclude_filter", draft.excludeFilter],
  ];
  for (const [key, value] of optionalFields) {
    if (value.trim() !== "") payload[key] = value.trim();
  }
  return payload as unknown as Lane;
}

// ================================================================ 线路分配（五级继承的**显式值**）

/** 一条线路的引用（id + 可解析出的名字）。`laneName` 缺失 = 配置里没有这条线路。 */
export type LaneRef = { laneId: string; laneName?: string };

export type PolicyLaneRow = {
  id: string;
  name: string;
  priority?: number;
  enabled: boolean;
  lane: LaneRef;
};

export type ChannelLaneRow = {
  id: string;
  model?: string;
  providerId?: string;
  providerName?: string;
  credentialId?: string;
  credentialLabel?: string;
  lane?: LaneRef;
  /** `false` = 后端返回的对象里**没有** `lane` 这个键 → 「未读取」。 */
  laneReadable: boolean;
  enabled: boolean;
};

export type CredentialLaneRow = {
  id: string;
  label: string;
  providerId?: string;
  providerName?: string;
  lane?: LaneRef;
  laneReadable: boolean;
};

export type ProviderLaneRow = {
  id: string;
  name: string;
  lane?: LaneRef;
  laneReadable: boolean;
};

/** 五级继承里某一级的名字（`lane_assignments` 的 `source` → 文案 key）。 */
const LANE_SOURCE_LABEL: Record<string, I18nKey> = {
  policy: "network.assignments.source.policy",
  channel: "network.assignments.source.channel",
  credential: "network.assignments.source.credential",
  provider: "network.assignments.source.provider",
  default: "network.assignments.source.default",
};

/** **后端**说这条结论来自哪一级。词表外的取值原样显示（是后端说的话，不改写）。 */
export function laneSourceLabel(source: string | undefined): I18nKey | undefined {
  if (source === undefined) return undefined;
  return LANE_SOURCE_LABEL[source];
}

/** 倒排的一行：谁**显式**引用了这条线。 */
export type ReverseLaneRow = {
  laneId: string;
  laneName?: string;
  channels: string[];
  credentials: string[];
  providers: string[];
  policies: string[];
};

export type AssignmentView = {
  policies: PolicyLaneRow[];
  channels: ChannelLaneRow[];
  credentials: CredentialLaneRow[];
  providers: ProviderLaneRow[];
  defaultLane?: LaneRef;
  reverse: ReverseLaneRow[];
  /** `config_get` 是否给出了这些字段。`false` = **未读取**（不是"配置里是空的"）。 */
  has: {
    net: boolean;
    lanes: boolean;
    channels: boolean;
    credentials: boolean;
    providers: boolean;
    policies: boolean;
  };
};

/**
 * 读整份配置，收集**五级继承里每一级显式写了什么**，以及它的倒排。
 *
 * **不解析继承、不产出"最终生效线路"** —— 那是后端 `lane_assignments` 的职责
 * （[04 §8.1]）。前端算一遍就会与后端漂，而漂了不会报错。
 */
export function readAssignments(doc: unknown): AssignmentView {
  const d = asRow(doc);
  const net = asRow(d["net"]);
  const lanesRaw = pickArr(d, "lanes");
  const channelsRaw = pickArr(d, "channels");
  const credentialsRaw = pickArr(d, "credentials");
  const providersRaw = pickArr(d, "providers");
  const policiesRaw = pickArr(d, "policies");

  // 线路 id → 名字（引用只有 id 时用它解析成用户能认的名字）
  const laneNames = new Map<string, string>();
  for (const item of lanesRaw ?? []) {
    const row = asRow(item);
    const id = pickStr(row, "id");
    if (id !== undefined) laneNames.set(id, pickStr(row, "name") ?? id);
  }

  const ref = (laneId: string | undefined): LaneRef | undefined =>
    laneId === undefined ? undefined : { laneId, laneName: laneNames.get(laneId) };

  // ---- 服务商 / 套餐 的名字映射（通道行要用人能认的名字，不是 id）
  const providerNames = new Map<string, string>();
  for (const item of providersRaw ?? []) {
    const row = asRow(item);
    const id = pickStr(row, "id");
    if (id !== undefined) providerNames.set(id, pickStr(row, "name", "id") ?? id);
  }
  const credentialLabels = new Map<string, string>();
  for (const item of credentialsRaw ?? []) {
    const row = asRow(item);
    const id = pickStr(row, "id");
    if (id !== undefined) credentialLabels.set(id, pickStr(row, "label", "id") ?? id);
  }

  const policies: PolicyLaneRow[] = [];
  for (const item of policiesRaw ?? []) {
    const row = asRow(item);
    const lane = ref(pickStr(asRow(row["action"]), "use_lane"));
    // 只列**显式**指定了的：use_lane 为空 = 这一级没说，不算一条分配
    if (lane === undefined) continue;
    policies.push({
      id: pickStr(row, "id") ?? "",
      name: pickStr(row, "name") ?? "",
      priority: pickNum(row, "priority"),
      enabled: pickBool(row, "enabled") ?? true,
      lane,
    });
  }

  const channels: ChannelLaneRow[] = (channelsRaw ?? []).map((item) => {
    const row = asRow(item);
    const providerId = pickStr(row, "provider");
    const credentialId = pickStr(row, "credential");
    return {
      id: pickStr(row, "id") ?? "",
      model: pickStr(row, "model"),
      providerId,
      providerName: providerId === undefined ? undefined : providerNames.get(providerId),
      credentialId,
      credentialLabel: credentialId === undefined ? undefined : credentialLabels.get(credentialId),
      lane: ref(pickStr(row, "lane")),
      laneReadable: hasKey(row, "lane"),
      enabled: pickBool(row, "enabled") ?? true,
    };
  });

  const credentials: CredentialLaneRow[] = (credentialsRaw ?? []).map((item) => {
    const row = asRow(item);
    const providerId = pickStr(row, "provider");
    return {
      id: pickStr(row, "id") ?? "",
      label: pickStr(row, "label") ?? "",
      providerId,
      providerName: providerId === undefined ? undefined : providerNames.get(providerId),
      lane: ref(pickStr(row, "lane")),
      laneReadable: hasKey(row, "lane"),
    };
  });

  const providers: ProviderLaneRow[] = (providersRaw ?? []).map((item) => {
    const row = asRow(item);
    return {
      id: pickStr(row, "id") ?? "",
      name: pickStr(row, "name") ?? "",
      lane: ref(pickStr(row, "lane")),
      // 契约的 `ProviderView` **没有** lane 字段：键不在 = 未读取（不是"未指定"）
      laneReadable: hasKey(row, "lane"),
    };
  });

  const defaultLaneId = pickStr(net, "default_lane");
  const defaultLane = ref(defaultLaneId);

  // ---- 倒排：配置里**显式**引用了每条线的四个来源。
  // 先把配置里的线路按顺序放进去（没人引用的也要出现 —— 否则用户会以为它被用了）。
  const reverseMap = new Map<string, ReverseLaneRow>();
  const ensure = (laneId: string): ReverseLaneRow => {
    const existing = reverseMap.get(laneId);
    if (existing !== undefined) return existing;
    const row: ReverseLaneRow = {
      laneId,
      laneName: laneNames.get(laneId),
      channels: [],
      credentials: [],
      providers: [],
      policies: [],
    };
    reverseMap.set(laneId, row);
    return row;
  };
  for (const laneId of laneNames.keys()) ensure(laneId);
  for (const c of channels) if (c.lane !== undefined) ensure(c.lane.laneId).channels.push(c.id);
  for (const c of credentials) {
    if (c.lane !== undefined) ensure(c.lane.laneId).credentials.push(c.label);
  }
  for (const p of providers) if (p.lane !== undefined) ensure(p.lane.laneId).providers.push(p.name);
  for (const p of policies) if (p.lane !== undefined) ensure(p.lane.laneId).policies.push(p.name);

  return {
    policies,
    channels,
    credentials,
    providers,
    defaultLane,
    reverse: [...reverseMap.values()],
    has: {
      net: Object.keys(net).length > 0,
      lanes: lanesRaw !== undefined,
      channels: channelsRaw !== undefined,
      credentials: credentialsRaw !== undefined,
      providers: providersRaw !== undefined,
      policies: policiesRaw !== undefined,
    },
  };
}

// ================================================================ 给展示层用的小工具

/** `include` / `exclude` 两条正则拼成一句可读的过滤描述（都空 = 没有过滤）。 */
export function filterSummary(
  include: string | undefined,
  exclude: string | undefined,
  none: string,
): string {
  const parts: string[] = [];
  if (include !== undefined) parts.push(`+${include}`);
  if (exclude !== undefined) parts.push(`-${exclude}`);
  return parts.length === 0 ? none : parts.join(" ");
}

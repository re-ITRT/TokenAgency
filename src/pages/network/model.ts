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
/**
 * 缺值占位符。
 *
 * **真源已收敛到 [`@/shared/format`]**（：这个字符原先在 7 个文件各写一遍）。
 * 这里保留一次**再导出**：`./index`、`./assignments`、`./nodes` 三个兄弟模块都是从
 * `./model` 取它的 —— 删掉再导出会让十几个调用点全部编译失败。
 * 新代码请直接 `import { DASH } from "@/shared/format"`。
 */
export { DASH } from "@/shared/format";
// 读后端原始对象的唯一实现（原先本文件自己写了 6 个读件）
// `hasKey` / `pickArr` 随 `readAssignments` 一起不再被本文件使用 —— 去掉导入，
// 需要它们的模块直接从 `@/shared/row` 取。
import { asRow, pickBool, pickNum, pickStr } from "@/shared/row";
import type { Subscription, SubscriptionFormat } from "@/shared/bindings";

// ================================================================ 基础读取件
//
// 全部来自 `@/shared/row` —— 这里原来自己写了一份（`asRow` / `hasKey` /
// `pickStr` / `pickNum` / `pickBool` / `pickArr`），而 `asRow` 的口径与
// `providers/*` 那份**已经不一样了**（这份收数组，那份挡数组）。收敛到一处之后
// 全仓只有一种口径：**数组不是一行**。

/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */

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

/**
 * 线路种类（[04 §7.1]）—— **界面认得的词表**。
 *
 * 用户第 321 轮："**直接放弃订阅里面分组这一概念，只拿节点**" ——
 * `subscription_group`（订阅里自带的组）**从界面上彻底消失**，所以这里不再是五种。
 * 后端仍然能反序列化老值（否则老配置在迁移前就解析失败），但那是**迁移用的遗留口**，
 * 不是可选项 —— 校验器会把它当配置损坏点名报错。
 *
 *（第 325 轮）之后**没有"类型下拉"了**（手动建线路的界面整块删除），
 * 但这个词表仍然有职责：`laneKindLabel` 的取值集合与它一一对应，
 * 列表里显示的就是这四种名字；`network.test.tsx` 用它挡住"遗留值悄悄回到选项里"。
 */
export const LANE_KINDS = [
  "direct",
  "subscription_auto",
  "subscription_node",
  "filter",
] as const;

const LANE_KIND_LABEL: Record<string, I18nKey> = {
  direct: "network.laneKind.direct",
  subscription_auto: "network.laneKind.subscription_auto",
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
  /**
   * **编辑时的回带底稿**：配置里那条订阅本身，但**已剔除 `url`**。
   *
   * 为什么必须有它：`subscription_upsert` 是**整条覆盖写**（Rust 侧 `*slot = subscription.clone()`），
   * 而表单里没有、也不该有的字段（`user_agent` / `disabled_nodes` / `provider_key` / `extra`…）
   * 一旦不回带，保存就会把它们**重置成类型默认值**。最要命的是 `disabled_nodes`
   * —— 那是 逐节点开关的存档，被清空 = 用户关掉的节点**全部被重新打开**，
   * 而且保存那一刻不报错、还显示"已保存"。这与 为 `provider_upsert` 根治的是同一个缺陷，
   * 纪律逐字相同：**以原始记录为底，只覆盖用户真的改过的那些键**（见 `./providers/editor.ts`）。
   *
   * 剔除 `url` 的原因见本类型上方的注释：它是**唯一**不能进 DOM 的字段，底稿里也不留它。
   * 编辑时地址留空 = 不改，由后端保证。
   */
  base: Record<string, unknown>;
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

  // 编辑回带的底稿 = 实体本身，**剔除 `url`**（它是唯一不能进 DOM 的字段）。
  // 注意取的是 `inner`（实体）而不是 `outer`：`outer` 在 `SubscriptionView` 形状下还带着
  // `node_count` / `parser` / `error` 这些**视图字段** —— 把它们发回去会被 Rust 的
  // `#[serde(flatten)] extra` 收下、**写进配置**（配置里凭空多出几个运行时字段）。
  const base: Record<string, unknown> = { ...inner };
  delete base["url"];

  return {
    id: pickStr(inner, "id") ?? "",
    name: pickStr(inner, "name"),
    url: maskSubscriptionUrl(pickStr(inner, "url") ?? ""),
    base,
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
  /**
   * 健康检查地址（**选填**）。
   * 留空 ⇒ **不发这个字段**（Rust 侧 `health_check` 有 `#[serde(default)]`，用后端默认）；
   * 填了 ⇒ 用它替换默认探测目标（无公网环境里指向本机地址，就能测出真实延迟）。
   */
  healthUrl: string;
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
export function subscriptionDraftComplete(
  draft: SubscriptionDraft,
  /**
   * 编辑时 `url` **允许为空**：地址里带 token，界面从不回显原文
   *（[`SubscriptionRow.base`]），所以编辑时那个框只能是空的 —— 留空 = 不改地址
   *（由后端保证，同 `credential_set_secret` 的"留空 = 不改密钥"）。新建时仍然必填。
   */
  options: { urlRequired?: boolean } = {},
): boolean {
  const urlRequired = options.urlRequired ?? true;
  return (
    draft.id.trim() !== "" &&
    draft.name.trim() !== "" &&
    (!urlRequired || draft.url.trim() !== "")
  );
}

/**
 * 一行订阅 → **编辑**表单的初始草稿。
 *
 * **地址刻意留空**：`row.url` 是遮蔽过的（`token=***`）。把它填进输入框，
 * 用户一保存就会把 `***` 当成真实地址写回配置 —— 那是个"保存成功、下次刷新全挂"的坑。
 * 所以编辑时地址框是空的，语义是「留空 = 不改」（与 `providers.edit.keyHint` 同一条口径）。
 */
export function subscriptionDraftFromRow(row: SubscriptionRow): SubscriptionDraft {
  const format = row.format;
  return {
    id: row.id,
    name: row.name ?? "",
    url: "",
    format: (SUBSCRIPTION_FORMATS as readonly string[]).includes(format ?? "")
      ? (format as SubscriptionFormat)
      : "auto",
    intervalS: row.intervalS === undefined ? "" : String(row.intervalS),
    keepVersions: row.keepVersions === undefined ? "" : String(row.keepVersions),
    include: row.include ?? "",
    exclude: row.exclude ?? "",
    enabled: row.enabled,
    healthUrl: row.healthUrl ?? "",
  };
}

/**
 * **编辑**的载荷：以原始记录为底（`base`），只覆盖表单真正管的那些键。
 *
 * 与 `subscriptionPayload`（新建）的区别只有两处，两处都是必须的：
 *
 * 1. **带上底稿 `base`** —— 不回带就会把表单里没有的字段重置成类型默认值。
 *    最要命的是 `disabled_nodes`（逐节点开关）：它被清空 = 用户关掉的节点
 *    **全部被重新打开**，而保存那一刻不报错、还显示"已保存"。
 * 2. **地址可以留空**（留空 = 不改）—— 因为界面拿不到明文地址。
 */
export function subscriptionEditPayload(
  draft: SubscriptionDraft,
  base: Record<string, unknown>,
): Subscription {
  const payload: Record<string, unknown> = { ...base };
  // 底稿里本来就没有 url（`readSubscription` 剔掉了）；这里再删一次是双保险。
  delete payload["url"];
  payload["id"] = draft.id.trim();
  payload["name"] = draft.name.trim();
  payload["format"] = draft.format;
  payload["enabled"] = draft.enabled;
  // 留空 ⇒ 后端保持原值—— 这是**唯一**允许留空的字段
  payload["url"] = draft.url.trim();

  // 数字：草稿已回填真值。清空 = 交回后端默认 → **删掉这个键**（不是发 0）。
  const intervalS = parseOptionalCount(draft.intervalS);
  if (intervalS === undefined) delete payload["interval_s"];
  else if (intervalS !== null) payload["interval_s"] = intervalS;
  const keepVersions = parseOptionalCount(draft.keepVersions);
  if (keepVersions === undefined) delete payload["keep_versions"];
  else if (keepVersions !== null) payload["keep_versions"] = keepVersions;

  // 过滤：清空是**合法意图**（= 不要过滤）→ 必须显式覆盖成空对象。
  // 这里不能照抄新建的"留空就不发"：那样底稿里的旧正则会被留下来
  //（用户明明删掉了过滤条件，保存后它还在）。
  const include = draft.include.trim();
  const exclude = draft.exclude.trim();
  const filter: Record<string, string> = {};
  if (include !== "") filter.include = include;
  if (exclude !== "") filter.exclude = exclude;
  payload["filter"] = filter;

  // 健康检查：保留底稿里的 enabled / interval_s / tolerance_ms，只换 url；
  // 地址清空 = 交回后端默认探测目标（**删掉 url 键**，不是发空串）。
  const health: Record<string, unknown> = { ...asRow(base["health_check"]) };
  const healthUrl = draft.healthUrl.trim();
  if (healthUrl === "") delete health["url"];
  else health["url"] = healthUrl;
  payload["health_check"] = health;

  return payload as unknown as Subscription;
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
  // 健康检查地址：**只在用户真填了才发**（留空 = 交给 Rust 的 `#[serde(default)]`）。
  // 这里**只放 `url`**，不补 `enabled`/`interval_s`/`tolerance_ms` —— 前端复刻后端默认值
  // 就等于把默认值变成第二个真相源（理由见本函数上方的注释）。
  const healthUrl = draft.healthUrl.trim();
  if (healthUrl !== "") {
    payload.health_check = { url: healthUrl };
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

/*
 *（第 325 轮）这里整节是**线路表单**的草稿类型与载荷构造器：
 * `LaneDraft` / `laneMissingField` / `lanePayload` / `laneDraftFromRow` / `laneEditPayload`。
 *
 * 用户裁决「这个功能好像没有什么用，可以在界面上删了」之后，手动建/改线路的界面整块删除，
 * 这些函数**一个调用方都没有了**。留着它们不只是死代码 —— 它们会让人以为
 * "线路还能手建，只是这一页没画入口"，而事实是线路由后端在服务商选中时按需建出来。
 *
 * 载荷纪律本身没有丢：订阅那份还在（`subscriptionEditPayload`），
 * 而线路的三种建法各有自己的后端命令（`lane_ensure_for_node` / `lane_ensure_direct` /
 * `ensure_auto_lane`），它们是"这条线路该长什么样"的唯一真相源。
 */
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

/*
 *（第 322 轮）这里曾有 `LANE_SOURCE_LABEL` / `laneSourceLabel` / `ReverseLaneRow` /
 * `AssignmentView` / `readAssignments` —— 「线路分配」页（`/network/assignments`）的整套读件。
 *
 * 那一页随"放弃线路级别的分配"整页删除（用户："取而代之的，每个服务商统一一个节点"），
 * 所以这些读件没有任何调用方了。留着的害处不只是死代码：它们会让人以为
 * "五级继承链还在，只是这一页没画" —— 而事实是前三级已经没有字段、没有写入口、也不参与解析。
 *
 * 现在要看/改走哪个节点，只有一处：服务商详情页的「网络代理」下拉（读 `lane_options`）。
 */

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

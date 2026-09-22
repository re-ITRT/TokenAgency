/**
 * 套餐与线路页（`/channels`，[07 §5] / [05 §2.1]）的**纯逻辑层**。
 *
 * 三条纪律：
 *
 * 1. **线路继承只认读得到的层级**。完整链条是
 *    `channel.lane -> credential.lane -> provider.lane -> net.default_lane -> direct`（[04 §8.1]），
 *    而本页只读得到前两级（`channel_list` / `credential_list`）。所以 `LaneResolution.source`
 *    必须如实标注是哪一级；更上面几级一律 `inherited`——**不替后端挑一条线路**。
 * 2. **配额占用比优先用后端给的数**（真机 `CredentialView.used_pct` / `quota_state.used_ratio`）；
 *    后端没给、但口径与上限都读得到时才由本页算，并把"这是本页算的"标出来（`usedPctSource`）。
 *    两者都没有 → `null`（=「未设上限」），**绝不退化成 0**：0% 会被读成"一点没用"。
 * 3. **页面里没有的就不编**：服务商 / 凭据 / 通道组 / 线路名字查不到就 `undefined`，
 *    由页面退回显示 id。
 *
 * ## 契约与实现的偏差（事实，逐条标了出处；与 `pages/logs/model.ts` 同一套纪律）
 *
 * | 项 | 冻结契约（`src/shared/bindings.ts`，不许改） | Rust 实现（权威） |
 * |---|---|---|
 * | `provider_list` 的元素 | `ProviderView`（bindings.ts:589-601：`status` / `success_rate_24h` / `channel_count`） | 直接是 `Provider` 配置实体（`catalog.rs:54`）——**没有 `status`**，只有 `enabled` |
 * | `credential_list` 的元素 | `CredentialView.quota: QuotaState`（bindings.ts:603-610） | `quota_state: Option<QuotaStateRow>` + `used_pct: Option<f64>`（`catalog.rs:23-30`） |
 * | `credential_list` 的用量字段名 | `used_tokens` / `used_requests` / `used_cost` / `state` | **同名**（`crates/ta-store/src/usage_store.rs:422-437`） | 
 * | `lane_list` 的元素 | `LaneView.{type, node_count, current_node, latency_ms, health}`（bindings.ts:679-686） | `{ lane, latest?: HealthSample, channel_count }`（`catalog.rs:41-47`）——健康证据在 `latest` 里 |
 *
 * 所以本文件：**两套形状都读**（`quota` 或 `quota_state`、`status` 或 `enabled`、
 * `current_node` 或 `latest.node`），任何一边缺失都显示「未知/—」，**不猜**。
 */

import type {
  Channel,
  ChannelCapabilities,
  ChannelGroup,
  Credential,
  CredentialView,
  LaneKind,
  LaneView,
  PriceEntry,
  ProviderView,
  QuotaConfig,
  QuotaMetric,
  QuotaStateValue,
  QuotaWindow,
  Selector,
} from "@/shared/bindings";

/** 数值缺失一律画破折号（**不画 0**）。 */
export const DASH = "—";

// ================================================================ 两套字段名都认

const QUOTA_STATES: readonly QuotaStateValue[] = ["ok", "soft", "exhausted", "unknown"];

function num(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function quotaStateValue(value: unknown): QuotaStateValue | undefined {
  return typeof value === "string" && (QUOTA_STATES as readonly string[]).includes(value)
    ? (value as QuotaStateValue)
    : undefined;
}

/**
 * 用量状态：契约叫 `quota`（bindings.ts:605），真机叫 `quota_state`（catalog.rs:25）。
 * 两者的 `used_*` 与 `state` 字段名一致，所以归一化成一个形状即可。
 */
export type QuotaUsage = {
  state?: QuotaStateValue;
  used_tokens?: number;
  used_requests?: number;
  used_cost?: number;
};

export function quotaUsageOf(view: CredentialView): QuotaUsage | undefined {
  const raw = view as unknown as { quota?: unknown; quota_state?: unknown };
  const src = (raw.quota ?? raw.quota_state) as Record<string, unknown> | undefined;
  if (src === undefined || src === null) return undefined;

  const usage: QuotaUsage = {
    state: quotaStateValue(src["state"]),
    used_tokens: num(src["used_tokens"]),
    used_requests: num(src["used_requests"]),
    used_cost: num(src["used_cost"]),
  };
  const any =
    usage.state !== undefined ||
    usage.used_tokens !== undefined ||
    usage.used_requests !== undefined ||
    usage.used_cost !== undefined;
  return any ? usage : undefined;
}

/** 后端自己算好的占用比：真机的 `used_pct`（`catalog.rs:29`）优先，其次 `quota_state.used_ratio`。 */
export function backendUsedPct(view: CredentialView): number | undefined {
  const raw = view as unknown as {
    used_pct?: unknown;
    quota_state?: { used_ratio?: unknown } | null;
  };
  const direct = num(raw.used_pct);
  if (direct !== undefined && direct >= 0) return direct;
  const ratio = num(raw.quota_state?.used_ratio);
  if (ratio !== undefined && ratio >= 0) return ratio;
  return undefined;
}

// ================================================================ 线路

export type LaneSource = "channel" | "credential" | "inherited";

/** 线路的最近健康样本（真机 `lane_list[].latest`，`ta_store::HealthSample`）。 */
export type LaneSample = {
  node: string | undefined;
  latencyMs: number | undefined;
  ok: boolean | undefined;
};

export type LaneResolution = {
  /** 这条线路是在哪一级定下的。 */
  source: LaneSource;
  /** `inherited` 时为 `undefined`（本页读不到更上一级）。 */
  laneId: string | undefined;
  /** 只在 `lane_list` 里查得到时才有值。 */
  name: string | undefined;
  /** 契约形状才有（bindings.ts:685）；真机没有这一列 → `undefined`。 */
  health: LaneView["health"] | undefined;
  /** 最近样本：契约的 `current_node` / `latency_ms`，或真机的 `latest`。 */
  sample: LaneSample | undefined;
  nodeCount: number | undefined;
};

const INHERITED: LaneResolution = {
  source: "inherited",
  laneId: undefined,
  name: undefined,
  health: undefined,
  sample: undefined,
  nodeCount: undefined,
};

/** 从 `LaneView` 里取健康证据（两套形状都认；都没有就是 `undefined`）。 */
function sampleOf(view: LaneView): LaneSample | undefined {
  const raw = view as unknown as {
    current_node?: unknown;
    latency_ms?: unknown;
    latest?: { node?: unknown; latency_ms?: unknown; ok?: unknown } | null;
  };
  const node =
    typeof raw.latest?.node === "string"
      ? raw.latest.node
      : typeof raw.current_node === "string"
        ? raw.current_node
        : undefined;
  const latencyMs = num(raw.latest?.latency_ms) ?? num(raw.latency_ms);
  const ok = typeof raw.latest?.ok === "boolean" ? raw.latest.ok : undefined;
  if (node === undefined && latencyMs === undefined && ok === undefined) return undefined;
  return { node, latencyMs, ok };
}

function laneAt(
  laneId: string,
  lanes: readonly LaneView[],
  source: Exclude<LaneSource, "inherited">,
): LaneResolution {
  const found = lanes.find((l) => l.lane.id === laneId);
  return {
    source,
    laneId,
    name: found?.lane.name,
    health: found?.health,
    sample: found === undefined ? undefined : sampleOf(found),
    nodeCount: found?.node_count,
  };
}

function nonEmpty(id: string | null | undefined): string | undefined {
  return id === undefined || id === null || id === "" ? undefined : id;
}

/** 通道级 -> 凭据级 -> 继承（更上面几级本页不读，故不猜）。 */
export function resolveLane(
  channel: Pick<Channel, "lane">,
  credential: Credential | undefined,
  lanes: readonly LaneView[],
): LaneResolution {
  const own = nonEmpty(channel.lane);
  if (own !== undefined) return laneAt(own, lanes, "channel");
  const inherited = nonEmpty(credential?.lane);
  if (inherited !== undefined) return laneAt(inherited, lanes, "credential");
  return INHERITED;
}

// ================================================================ 额度

/** 占用比是哪来的 —— 页面据此决定要不要标"这是本页算的"。 */
export type UsedPctSource = "backend" | "derived" | "none";

export type QuotaParts = {
  metric: QuotaMetric;
  window: QuotaWindow;
  currency: string;
  /** 已用量（口径与 `metric` 一致时才有值）。 */
  used: number | undefined;
  /** 上限；**只在有限正数时才有值**（0 / 负数 / NaN / Infinity 一律视为"没配"）。 */
  limit: number | undefined;
  /** `null` = 算不出占用比（未设上限、口径对不上、或后端也没给）—— 页面显示「未设上限」。 */
  usedPct: number | null;
  usedPctSource: UsedPctSource;
  /**
   * **没有任何用量记录**（真机 `CredentialView.quota_state` 缺失）。
   *
   * 这一项必须和「未设上限」分开：缺的可能是**用量**而不是上限 ——
   * 那时画一个"未设上限"的额度条就是假信息（配置里明明写了上限）。
   */
  usageMissing: boolean;
};

/** 按口径取对应的用量字段。`metric: "unknown"` 时**不四选一**（口径未声明，猜哪个都是错）。 */
function usedFor(metric: QuotaMetric, state: QuotaUsage): number | undefined {
  if (metric === "tokens") return state.used_tokens;
  if (metric === "requests") return state.used_requests;
  if (metric === "cost") return state.used_cost;
  return undefined;
}

function usableLimit(limit: number | null | undefined): number | undefined {
  return typeof limit === "number" && Number.isFinite(limit) && limit > 0 ? limit : undefined;
}

/**
 * 额度整形。
 *
 * @param backendRatio 后端自己给的比例（`backendUsedPct`）；**有它就一定用它**，
 *                     因为它才是权威（而且避免同一页出现两个算出来的数）。
 */
export function quotaParts(
  quota: QuotaConfig | undefined,
  state: QuotaUsage | undefined,
  backendRatio?: number,
): QuotaParts | undefined {
  if (quota === undefined) return undefined;

  const used = state === undefined ? undefined : usedFor(quota.metric, state);
  const limit = usableLimit(quota.limit);

  let usedPct: number | null = null;
  let usedPctSource: UsedPctSource = "none";
  if (backendRatio !== undefined) {
    usedPct = backendRatio;
    usedPctSource = "backend";
  } else if (used !== undefined && limit !== undefined) {
    usedPct = used / limit;
    usedPctSource = "derived";
  }

  return {
    metric: quota.metric,
    window: quota.window,
    currency: quota.currency,
    used,
    limit,
    usedPct,
    usedPctSource,
    usageMissing: state === undefined,
  };
}

// ================================================================ 通道行

/** 后端 `quota_limit_gaps` 的一条原文（判定与文案都来自后端，本页一个字不改）。 */
export type QuotaLimitGap = { credential_id: string; message: string };

export type ChannelGroupRef = { id: string; selector: Selector };

export type ChannelRowView = {
  id: string;
  model: string;
  upstreamModel: string;
  providerId: string;
  providerName: string | undefined;
  /** 契约形状的健康列；真机没有 → `undefined`（页面就不点状态灯）。 */
  providerStatus: ProviderView["status"] | undefined;
  /** 配置里的启用事实（两套形状都有这一列）。 */
  providerEnabled: boolean | undefined;
  credentialId: string;
  credentialLabel: string | undefined;
  credentialEnabled: boolean | undefined;
  lane: LaneResolution;
  priority: number;
  weight: number;
  tags: readonly string[];
  targetOverride: string | undefined;
  enabled: boolean;
  /** 这条通道属于哪些通道组（`group_list[].members` 里含它）。 */
  groups: readonly ChannelGroupRef[];
  quotaState: QuotaStateValue | undefined;
  quotaParts: QuotaParts | undefined;
  limitGapMessage: string | undefined;
};

export type ChannelRowContext = {
  providers: readonly ProviderView[];
  credentials: readonly CredentialView[];
  lanes: readonly LaneView[];
  groups: readonly ChannelGroup[];
  limitGaps: readonly QuotaLimitGap[];
};

export function readChannelRow(channel: Channel, ctx: ChannelRowContext): ChannelRowView {
  const provider = ctx.providers.find((p) => p.id === channel.provider);
  const credentialView = ctx.credentials.find((c) => c.credential.id === channel.credential);
  const credential = credentialView?.credential;
  const usage = credentialView === undefined ? undefined : quotaUsageOf(credentialView);
  const backendRatio = credentialView === undefined ? undefined : backendUsedPct(credentialView);

  return {
    id: channel.id,
    model: channel.model,
    upstreamModel: channel.upstream_model,
    providerId: channel.provider,
    providerName: provider?.name,
    providerStatus: provider?.status,
    providerEnabled: provider?.enabled,
    credentialId: channel.credential,
    credentialLabel: credential?.label,
    credentialEnabled: credential?.enabled,
    lane: resolveLane(channel, credential, ctx.lanes),
    priority: channel.priority,
    weight: channel.weight,
    tags: channel.tags,
    targetOverride: channel.target_override ?? undefined,
    enabled: channel.enabled,
    groups: ctx.groups
      .filter((g) => g.members.includes(channel.id))
      .map((g) => ({ id: g.id, selector: g.selector })),
    quotaState: usage?.state,
    quotaParts: quotaParts(credential?.quota, usage, backendRatio),
    limitGapMessage: ctx.limitGaps.find((g) => g.credential_id === channel.credential)?.message,
  };
}

export function readChannelRows(
  channels: readonly Channel[],
  ctx: ChannelRowContext,
): ChannelRowView[] {
  return channels.map((c) => readChannelRow(c, ctx));
}

// ================================================================ 分组与概览

export type ProviderGroup = {
  providerId: string;
  providerName: string | undefined;
  status: ProviderView["status"] | undefined;
  enabled: boolean | undefined;
  rows: ChannelRowView[];
};

/** 按服务商分组（**保持后端给的通道顺序**，不额外排序 —— 顺序是用户的配置意图）。 */
export function groupByProvider(rows: readonly ChannelRowView[]): ProviderGroup[] {
  const map = new Map<string, ProviderGroup>();
  for (const row of rows) {
    const existing = map.get(row.providerId);
    if (existing === undefined) {
      map.set(row.providerId, {
        providerId: row.providerId,
        providerName: row.providerName,
        status: row.providerStatus,
        enabled: row.providerEnabled,
        rows: [row],
      });
      continue;
    }
    existing.rows.push(row);
    // 同 provider 的通道一定给同一个名字；缺失时后面补上也合理
    existing.providerName = existing.providerName ?? row.providerName;
    existing.status = existing.status ?? row.providerStatus;
    existing.enabled = existing.enabled ?? row.providerEnabled;
  }
  return [...map.values()];
}

export type ChannelsSummary = {
  total: number;
  enabled: number;
  disabled: number;
  /** 涉及的服务商数（去重）。 */
  providers: number;
  /** 有"未配可用额度"提醒的凭据数（去重）。 */
  limitGapCredentials: number;
};

export function summarizeChannels(rows: readonly ChannelRowView[]): ChannelsSummary {
  const providers = new Set<string>();
  const gaps = new Set<string>();
  for (const row of rows) {
    providers.add(row.providerId);
    if (row.limitGapMessage !== undefined) gaps.add(row.credentialId);
  }
  const enabled = rows.filter((r) => r.enabled).length;
  return {
    total: rows.length,
    enabled,
    disabled: rows.length - enabled,
    providers: providers.size,
    limitGapCredentials: gaps.size,
  };
}

/** 未配额度提醒里那条凭据的显示名：优先通道行里查到的凭据名，其次给 id（原样，不改写）。 */
export function credentialLabelFor(credentialId: string, rows: readonly ChannelRowView[]): string {
  return rows.find((r) => r.credentialId === credentialId)?.credentialLabel ?? credentialId;
}

// ================================================================ 添加线路向导（task-33）
//
// 「线路」= `Channel`（model x provider x credential x lane x upstream_model x 优先级/权重/能力），
// 写命令 `channel_upsert({ channel })`；填了计费再补一条 `pricing_upsert({ entry })`。
// 这一层只做**纯逻辑**：草稿 → 载荷、派生 id、选项过滤、本地预检。
// 三条纪律：
// 1. **只发用户真的填了的字段**：`channel_upsert` 收的 `ChannelConfig` 里 `lane` / `transform` /
//    `priority` / `weight` / `tags` / `enabled` 都有 `#[serde(default)]`
//    （`crates/ta-core/src/domain/mod.rs:1058`），所以没填的字段**一个都不发**，
//    由 Rust 的 `Default` 决定 —— 前端不复制一份默认值（那是两份真相源）。
//    **`capabilities` 是唯一的例外**（永远发全六个字段），理由见下面第 4 条与 `channelPayload`。
// 2. **id 是派生的**：本轮的向导没有 id 输入框（固定 testid 里没有），所以 id 由
//    「模型 x 服务商 x 凭据」派生。这三样才是**一条线的身份**；换上游名 / 网络 / 优先级 / 权重
//    都还是同一条线（`channel_upsert` 按 id 覆盖，正是幂等编辑的语义）。
//    凭据进 id 是必须的：同一个服务商的两份套餐（两份 key）是两条独立的线。
// 3. **本地预检只挡后端也会挡的必填项**（服务商 / 凭据 / 模型 / 上游模型名）**与本地就能判定的
//    非法数字**（`contextTokens` 非负整数、价格非负数字），
//    其余（模型是否存在、lane 是否存在、能力是否与同模型别的线路一致、weight 是否为 0）
//    一律交给后端，错误原样显示（能力不一致是 TA-2007，带结构化 details）。
// 4. **能力声明的默认值照抄 Rust**：`ChannelCapabilities::default()` 是
//    全 false / `context_tokens` 0 / `max_output_tokens` `None` / `modalities ["text"]`
//    （`crates/ta-core/src/domain/mod.rs:1033`）。默认值本身是契约的一部分，前端**不另造一套**：
//    用户没碰过能力块时，发出去的就是这六个字段的逐字默认值。
//    `max_output_tokens` 本页不做输入 -> 恒发 `null`（未声明），**不放禁用占位控件**。

/** 线路表单的原始输入。可空的字段保持字符串：`""` = 没填 → **不发这个字段**。 */
export type ChannelDraft = {
  provider: string;
  credential: string;
  model: string;
  upstreamModel: string;
  /** 空字符串 = 不指定 lane（按「凭据 -> 服务商 -> 默认 -> 直连」继承）。 */
  lane: string;
  priority: string;
  weight: string;
  /** 上游能力声明：三个开关默认全 `false`（= Rust 默认）。 */
  vision: boolean;
  tools: boolean;
  reasoning: boolean;
  /** 上下文窗口：非负整数文本，默认 `"0"`（= Rust 默认）。 */
  contextTokens: string;
  /** 模态：逗号分隔文本，默认 `"text"`（= Rust 默认 `["text"]`）。 */
  modalities: string;
  /** 计费（美元 / 百万 token）：`""` = 这一项不计价。三项全空 -> 完全不写定价条目。 */
  priceInput: string;
  priceOutput: string;
  priceCached: string;
};

export const EMPTY_CHANNEL_DRAFT: ChannelDraft = {
  provider: "",
  credential: "",
  model: "",
  upstreamModel: "",
  lane: "",
  priority: "",
  weight: "",
  vision: false,
  tools: false,
  reasoning: false,
  contextTokens: "0",
  modalities: "text",
  priceInput: "",
  priceOutput: "",
  priceCached: "",
};

/** id 里只允许小写字母、数字与短横线（与 `channels-add-id` 那类向导的 `wizard.invalidId` 同一口径）。 */
function idSlug(raw: string): string {
  return raw
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

/**
 * 派生通道 id：`ch-<service provider>-<credential>-<model>`。
 *
 * **为什么不是 `Date.now()` 之类**：id 必须是"同一条线"的稳定指纹，否则每次保存都会新增一条
 * 而不是覆盖（`channel_upsert` 是同 id 覆盖语义），用户会得到一堆重复线路。
 * 为什么不含上游模型名：上游模型名是这条线的**属性**，改名不该变成另一条线。
 */
export function channelIdFor(
  draft: Pick<ChannelDraft, "provider" | "credential" | "model">,
): string {
  return ["ch", idSlug(draft.provider), idSlug(draft.credential), idSlug(draft.model)]
    .filter((part) => part !== "")
    .join("-");
}

/** 非整数字符串一律当作**没填**（输入框本身只放数字字符进去，这里只是最后一道闸）。 */
export function parseOptionalInt(raw: string): number | undefined {
  const text = raw.trim();
  if (!/^-?\d+$/.test(text)) return undefined;
  const value = Number(text);
  return Number.isInteger(value) ? value : undefined;
}

// ---------------------------------------------------------------- 能力声明（[docs/17 §2]）

/**
 * 上下文窗口：**非负整数**。
 *
 * - `""`（没填）-> `0`（= Rust 默认，不是"未声明"：`context_tokens` 是 `u64`，没有 null）；
 * - 非非负整数（`12.5` / `-1` / 非数字）-> `undefined`（页面据此点名，**绝不当数字用**）。
 *
 * 输入框走 `intInputFilter`，非法字符根本进不来；这一层仍然自己判一次 ——
 * 页面的净化与纯逻辑的校验是两道闸，不能互相代替（返回 `undefined` 而不是 `NaN`：
 * `NaN` 会一路静默地进载荷）。
 */
export function parseContextTokens(raw: string): number | undefined {
  const text = raw.trim();
  if (text === "") return 0;
  if (!/^\d+$/.test(text)) return undefined;
  const value = Number(text);
  return Number.isSafeInteger(value) ? value : undefined;
}

/**
 * 模态：逗号分隔文本 -> trim -> 去空 -> **排序去重**。
 *
 * 排序去重是 Rust `capabilities_equal` 的口径（`crates/ta-core/src/domain/capabilities.rs:25`）：
 * `["text","image"]` 与 `["image","text","image"]` 必须判成一致，否则同一模型的两条等价线路
 * 会被后端以 TA-2007 拒掉。
 *
 * **诚实注记**：Rust 的 `normalized_modalities` 只排序去重、**不 trim 也不丢空串**
 * （那里的理由是"不许把 `\" image\"` 与 `\"image\"` 判成相等"）。前端这一层多做了 trim/去空，
 * 因为输入是用户手打的逗号串：`"text, image"` 里的空格是**打字习惯**而不是"另一种模态"，
 * 不 trim 会让用户莫名其妙地拿到不一致。清空输入框 = 显式声明"没有模态"（`[]`），
 * 这与 Rust 契约一致（`[]` 不是"没写"，没写才是默认 `["text"]`）。
 */
export function parseModalities(raw: string): string[] {
  const seen = new Set<string>();
  for (const part of raw.split(",")) {
    const modality = part.trim();
    if (modality !== "") seen.add(modality);
  }
  return [...seen].sort();
}

/** 草稿里的能力声明 -> 契约的 `ChannelCapabilities`（`max_output_tokens` 恒 `null` = 未声明）。 */
export function capabilitiesPayload(draft: ChannelDraft): ChannelCapabilities {
  return {
    vision: draft.vision,
    tools: draft.tools,
    reasoning: draft.reasoning,
    context_tokens: parseContextTokens(draft.contextTokens) ?? 0,
    max_output_tokens: null,
    modalities: parseModalities(draft.modalities),
  };
}

// ---------------------------------------------------------------- 计费（`pricing_upsert`）

/** 一个价格输入：`""` = 这一项不计价；非负数字才认（负号 / 半截小数 / 非数字一律 `undefined`）。 */
export function parsePrice(raw: string): number | undefined {
  const text = raw.trim();
  if (text === "") return undefined;
  if (!/^\d+(\.\d+)?$/.test(text)) return undefined;
  const value = Number(text);
  return Number.isFinite(value) ? value : undefined;
}

/** 三项计费里**有任意一项非空**（= 要写定价条目）。`"0"` 是非空：0 美元也是明确的价。 */
export function hasPrice(
  draft: Pick<ChannelDraft, "priceInput" | "priceOutput" | "priceCached">,
): boolean {
  return (
    draft.priceInput.trim() !== "" ||
    draft.priceOutput.trim() !== "" ||
    draft.priceCached.trim() !== ""
  );
}

const PRICE_FIELDS = ["priceInput", "priceOutput", "priceCached"] as const;

/**
 * `rates` 只放**填了的**键，键名是 ta-core `TokenKind::as_key()`
 * （`crates/ta-core/src/types.rs:127`）：`input` / `output` / `input_cached`。
 * 没填的键**一个都不放** —— 放 0 会把"没定价"写成"免费"。
 */
export function priceRates(
  draft: Pick<ChannelDraft, "priceInput" | "priceOutput" | "priceCached">,
): Record<string, number> {
  const rates: Record<string, number> = {};
  const input = parsePrice(draft.priceInput);
  if (input !== undefined) rates["input"] = input;
  const output = parsePrice(draft.priceOutput);
  if (output !== undefined) rates["output"] = output;
  const cached = parsePrice(draft.priceCached);
  if (cached !== undefined) rates["input_cached"] = cached;
  return rates;
}

/**
 * 草稿 -> `pricing_upsert({ entry })` 的载荷（[docs/17 §3.3]）：
 * `match` = 所选模型 + 所选服务商；`source: "manual"`（这是用户手填的价，不是内置表）；
 * `currency` / `effective_from` 显式 `null`（用全局配置的币种、立即生效，不替用户挑一个）。
 */
export function priceEntryFor(
  draft: Pick<ChannelDraft, "model" | "provider" | "priceInput" | "priceOutput" | "priceCached">,
): PriceEntry {
  return {
    match: { model: draft.model.trim(), provider: draft.provider.trim() },
    rates: priceRates(draft),
    multiplier: 1,
    currency: null,
    source: "manual",
    effective_from: null,
  };
}

/** 填了但**不是**合法非负数字的价格字段（页面据此点名，且一次请求都不发）。 */
export function invalidPriceFields(
  draft: Pick<ChannelDraft, "priceInput" | "priceOutput" | "priceCached">,
): (typeof PRICE_FIELDS)[number][] {
  return PRICE_FIELDS.filter(
    (field) => draft[field].trim() !== "" && parsePrice(draft[field]) === undefined,
  );
}

/**
 * 草稿 → `channel_upsert` 的载荷：**只带用户真的填了的字段**（其余交后端 `#[serde(default)]`）。
 *
 * 返回类型写的是契约的 `Channel`，但 TS 侧的 `Channel` 把 `transform` / `tags` / `enabled`
 * 写成了必填（读取形状），而 Rust 侧它们都有默认值 —— 所以这里和 `network/model.ts` 的
 * `lanePayload` 同一手法：先拼 `Record<string, unknown>`，最后断言成 `Channel`。
 * 单测（`channels.test.tsx` ⑨）钉住"这些字段一个都不在载荷里"。
 *
 * `capabilities` 是**例外：永远发**（六个字段都发全）。理由是它的默认值有语义
 * （`[]` 与"没写"不是一回事，见 `parseModalities`），所以"用户没碰过"要发成**逐字的默认值**，
 * 而不是省略 —— 省略会让后端各自决定默认，前端就再也说不清那条线声明的是什么。
 */
export function channelPayload(draft: ChannelDraft): Channel {
  const payload: Record<string, unknown> = {
    id: channelIdFor(draft),
    model: draft.model.trim(),
    provider: draft.provider.trim(),
    credential: draft.credential.trim(),
    upstream_model: draft.upstreamModel.trim(),
    capabilities: capabilitiesPayload(draft),
  };

  const lane = draft.lane.trim();
  if (lane !== "") payload["lane"] = lane;

  const priority = parseOptionalInt(draft.priority);
  if (priority !== undefined) payload["priority"] = priority;

  const weight = parseOptionalInt(draft.weight);
  if (weight !== undefined) payload["weight"] = weight;

  return payload as unknown as Channel;
}

/** 本地预检能点名的字段（文案在页面层映射到既有 i18n 键）。 */
export type ChannelDraftField = "provider" | "credential" | "model" | "upstreamModel";

/** 数值类字段：非空时必须合法（`contextTokens` 非负整数、价格非负数字）。 */
export type ChannelNumberField = "contextTokens" | "priceInput" | "priceOutput" | "priceCached";

/** `required` = 必填没填；`number` = 填了但不是合法的非负数字 —— 两句文案不能说混。 */
export type ChannelDraftProblem = "required" | "number";

export type ChannelDraftIssue = {
  field: ChannelDraftField | ChannelNumberField;
  problem: ChannelDraftProblem;
};

/**
 * 本地就能判定的问题（顺序 = 用户填写的顺序：必填在前，数字在后）。
 * 空数组 = 可以发。**其余一律交给后端**，错误原样显示（能力不一致也一样）。
 */
export function channelDraftIssues(draft: ChannelDraft): ChannelDraftIssue[] {
  const issues: ChannelDraftIssue[] = [];
  if (draft.provider.trim() === "") issues.push({ field: "provider", problem: "required" });
  if (draft.credential.trim() === "") issues.push({ field: "credential", problem: "required" });
  if (draft.model.trim() === "") issues.push({ field: "model", problem: "required" });
  if (draft.upstreamModel.trim() === "") issues.push({ field: "upstreamModel", problem: "required" });

  if (draft.contextTokens.trim() !== "" && parseContextTokens(draft.contextTokens) === undefined) {
    issues.push({ field: "contextTokens", problem: "number" });
  }
  for (const field of invalidPriceFields(draft)) {
    issues.push({ field, problem: "number" });
  }
  return issues;
}

/** 一个凭据选项（值 = 契约里的 `CredentialId`）。 */
export type CredentialOption = { id: string; label: string; providerId: string };

/**
 * 凭据下拉的选项：**按 provider 过滤**（`CredentialView.credential.provider`）。
 *
 * `providerId` 为空（还没选服务商）→ 返回空数组：**不列别的服务商的凭据** ——
 * 那会让用户以为可以给 A 服务商配 B 服务商的 key，真机上必被引用完整性拒绝。
 */
export function credentialOptionsFor(
  views: readonly CredentialView[],
  providerId: string,
): CredentialOption[] {
  if (providerId === "") return [];
  return views
    .filter((v) => v.credential.provider === providerId)
    .map((v) => ({ id: v.credential.id, label: v.credential.label, providerId: v.credential.provider }));
}

/** 一个线路选项（值 = 契约里的 `LaneId`）。 */
export type LaneOption = { id: string; name: string; kind: LaneKind };

/** 网络下拉的选项 = `lane_list` 里配置好的线路（直连也是其中一条 `kind: direct` 的线路）。 */
export function laneOptionsOf(lanes: readonly LaneView[]): LaneOption[] {
  return lanes.map((l) => ({ id: l.lane.id, name: l.lane.name, kind: l.lane.kind }));
}

/**
 * 默认网络 = **配置里那条启用的直连线路**（用户原话里的「网络 直连」）。
 *
 * 找不到直连线路时返回 `""`（= 不指定 lane）：此时按继承链走，最终仍然是直连 ——
 * 但这是**后端的语义**，不是本页替用户挑的，所以页面会把这条继承说明显示出来。
 * **绝不**退而选第一条订阅线路：那会让流量悄悄绕道代理。
 */
export function defaultLaneId(lanes: readonly LaneView[]): string {
  const direct = lanes.find((l) => l.lane.enabled && l.lane.kind === "direct");
  return direct?.lane.id ?? "";
}

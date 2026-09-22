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
  ChannelGroup,
  Credential,
  CredentialView,
  LaneKind,
  LaneView,
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
// 「线路」= `Channel`（model x provider x credential x lane x upstream_model x 优先级/权重），
// 写命令 `channel_upsert({ channel })`。这一层只做**纯逻辑**：草稿 → 载荷、派生 id、选项过滤。
// 三条纪律：
// 1. **只发用户真的填了的字段**：`channel_upsert` 收的 `ChannelConfig` 里 `lane` / `transform` /
//    `priority` / `weight` / `tags` / `enabled` 都有 `#[serde(default)]`
//    （`crates/ta-core/src/domain/mod.rs:1058`），所以没填的字段**一个都不发**，
//    由 Rust 的 `Default` 决定 —— 前端不复制一份默认值（那是两份真相源）。
// 2. **id 是派生的**：本轮的向导没有 id 输入框（固定 testid 里没有），所以 id 由
//    「模型 x 服务商 x 凭据」派生。这三样才是**一条线的身份**；换上游名 / 网络 / 优先级 / 权重
//    都还是同一条线（`channel_upsert` 按 id 覆盖，正是幂等编辑的语义）。
//    凭据进 id 是必须的：同一个服务商的两份套餐（两份 key）是两条独立的线。
// 3. **本地预检只挡后端也会挡的必填项**（服务商 / 凭据 / 模型 / 上游模型名），
//    其余（模型是否存在、lane 是否存在、weight 是否为 0）一律交给后端，错误原样显示。

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
};

export const EMPTY_CHANNEL_DRAFT: ChannelDraft = {
  provider: "",
  credential: "",
  model: "",
  upstreamModel: "",
  lane: "",
  priority: "",
  weight: "",
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

/**
 * 草稿 → `channel_upsert` 的载荷：**只带用户真的填了的字段**（其余交后端 `#[serde(default)]`）。
 *
 * 返回类型写的是契约的 `Channel`，但 TS 侧的 `Channel` 把 `transform` / `tags` / `enabled`
 * 写成了必填（读取形状），而 Rust 侧它们都有默认值 —— 所以这里和 `network/model.ts` 的
 * `lanePayload` 同一手法：先拼 `Record<string, unknown>`，最后断言成 `Channel`。
 * 单测（`channels.test.tsx` ⑨）钉住"这些字段一个都不在载荷里"。
 */
export function channelPayload(draft: ChannelDraft): Channel {
  const payload: Record<string, unknown> = {
    id: channelIdFor(draft),
    model: draft.model.trim(),
    provider: draft.provider.trim(),
    credential: draft.credential.trim(),
    upstream_model: draft.upstreamModel.trim(),
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

/** 缺哪些必填项（顺序 = 用户填写的顺序）。空数组 = 可以发。 */
export function channelDraftIssues(draft: ChannelDraft): ChannelDraftField[] {
  const missing: ChannelDraftField[] = [];
  if (draft.provider.trim() === "") missing.push("provider");
  if (draft.credential.trim() === "") missing.push("credential");
  if (draft.model.trim() === "") missing.push("model");
  if (draft.upstreamModel.trim() === "") missing.push("upstreamModel");
  return missing;
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

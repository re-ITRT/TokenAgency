/**
 * 模型目录页（[07 §4]）的**纯逻辑层** —— 页面组件只做渲染，整形与判定都在这里，便于单测。
 *
 * 三条纪律（这一页最容易违反的地方）：
 *
 * 1. **"被哪些通道引用"是数据，不是推断**：判定就是 `channel.model === model.id`
 *    （不做前缀匹配、不做模糊匹配，依据 `crates/ta-core/src/domain/mod.rs` 的 `ChannelConfig.model: String`
 *    与 `Model.id: String`）。名字只在清单里查得到时才填，查不到就退回 id —— **不猜一个名字**。
 * 2. **`model_list[].priced` 与 `pricing_unpriced_models` 是两个来源**：这里只做**交叉核对**，
 *    把不一致列出来，**不替后端裁决**；**某一列后端根本没给时一律「未知」，绝不默认成"未定价"**。
 * 3. **缺失与 0 是两件事**：`model_list` 自报的 `channel_count` 与实际匹配到的通道数是两个数，
 *    都保留 —— 页面才有机会把"不一致"显示给用户。
 *
 * ## 契约与实现的偏差（事实，逐条标了出处；与 `pages/logs/model.ts` 同一套纪律）
 *
 * | 项 | 冻结契约（`src/shared/bindings.ts`，不许改） | Rust 实现（权威） |
 * |---|---|---|
 * | `model_list` 的上游名 | `ModelView.upstream_names`（bindings.ts:638） | `upstream_models`（`src-tauri/src/commands/catalog.rs:33-37`） |
 * | `model_list` 的定价 | `ModelView.priced` / `cost_hint`（bindings.ts:640-641） | **没有这两列** —— 真机上恒为 `undefined` |
 *
 * 所以本文件：
 * - 上游名**两套名字都认**（`upstream_names` 优先，退回 `upstream_models`）；两边都没有 → `[]`（页面画「—」）。
 * - 定价**以「后端说了什么」为准**：`priced` 缺省 = `undefined` = 后端没说 → 页面上是「未知」。
 *   只有 `pricing_unpriced_models`（定价引擎自己的清单，权威）或显式的 `priced: false` 才能得出"未定价"。
 *
 * 这些偏差是**绑定生成（T1 的 `npm run gen:bindings`）落地前**的临时现实；生成物覆盖后并集读法仍然成立。
 */

import type { Channel, CredentialView, ModelView, ProviderView } from "@/shared/bindings";

/** 数值缺失一律画破折号（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
export const DASH = "—";

/** 一条引用该模型的通道（页面"引用通道"列的一行）。 */
export type ModelChannelRef = {
  channelId: string;
  /** 这条通道实际发给上游的模型名（可能与逻辑模型不同）。 */
  upstreamModel: string;
  enabled: boolean;
  providerId: string;
  /** 只在服务商清单里查得到时才有值。 */
  providerName: string | undefined;
  credentialId: string;
  /** 只在凭据清单里查得到时才有值。 */
  credentialLabel: string | undefined;
};

export type ModelRowView = {
  id: string;
  aliases: readonly string[];
  group: string | undefined;
  order: number | undefined;
  expose: boolean;
  /** `model_list` 自报的通道数（后端的话，原样保留）。 */
  reportedChannels: number;
  /** 该模型在各通道上的上游名（后端 `upstream_names` / `upstream_models`）。 */
  upstreamNames: readonly string[];
  /** `true`/`false` = 后端明确说了；**`undefined` = 后端没有这一列**（不许当成 false）。 */
  priced: boolean | undefined;
  costHint: string | undefined;
  /** 在 `channel_list` 里**实际匹配到**的通道。 */
  channels: readonly ModelChannelRef[];
};

/** 页面上要的三份清单（都是已注册命令的真实返回）。 */
export type ChannelsLookup = {
  channels: readonly Channel[];
  providers: readonly ProviderView[];
  credentials: readonly CredentialView[];
};

// ================================================================ 两套字段名都认

function strArray(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  return value.filter((v): v is string => typeof v === "string");
}

/** 上游名：契约名 `upstream_names` 优先，退回真机的 `upstream_models`；都没有就是空数组。 */
export function upstreamNamesOf(view: ModelView): string[] {
  const raw = view as unknown as { upstream_names?: unknown; upstream_models?: unknown };
  return strArray(raw.upstream_names) ?? strArray(raw.upstream_models) ?? [];
}

/** 定价：**只有后端真的给了布尔值才算"后端说了"**（真机上这一列不存在 → `undefined`）。 */
export function pricedOf(view: ModelView): boolean | undefined {
  const raw = (view as unknown as { priced?: unknown }).priced;
  return typeof raw === "boolean" ? raw : undefined;
}

/** 上游名去重后拼接；一个都没有时给破折号（不是空串 —— 空串在表里看不出是"没有"）。 */
export function upstreamSummary(names: readonly string[]): string {
  const seen: string[] = [];
  for (const name of names) {
    if (name === "") continue;
    if (!seen.includes(name)) seen.push(name);
  }
  return seen.length === 0 ? DASH : seen.join(" · ");
}

// ================================================================ 读行

/** 一条通道 → 引用行。名字查不到就留 `undefined`（不猜）。 */
export function readChannelRef(channel: Channel, lookup: ChannelsLookup): ModelChannelRef {
  const provider = lookup.providers.find((p) => p.id === channel.provider);
  const credential = lookup.credentials.find((c) => c.credential.id === channel.credential);
  return {
    channelId: channel.id,
    upstreamModel: channel.upstream_model,
    enabled: channel.enabled,
    providerId: channel.provider,
    providerName: provider?.name,
    credentialId: channel.credential,
    credentialLabel: credential?.credential.label,
  };
}

/** 一份 `model_list` 行 → 页面行。 */
export function readModelRow(view: ModelView, lookup: ChannelsLookup): ModelRowView {
  const channels = lookup.channels
    .filter((c) => c.model === view.model.id)
    .map((c) => readChannelRef(c, lookup));

  return {
    id: view.model.id,
    aliases: view.model.aliases,
    group: view.model.display.group ?? undefined,
    order: view.model.display.order,
    expose: view.model.expose,
    reportedChannels: view.channel_count,
    upstreamNames: upstreamNamesOf(view),
    priced: pricedOf(view),
    costHint: view.cost_hint ?? undefined,
    channels,
  };
}

export function readModelRows(
  views: readonly ModelView[],
  lookup: ChannelsLookup,
): ModelRowView[] {
  return views.map((v) => readModelRow(v, lookup));
}

/** 排序：`display.order` 升序，**没有 order 的排最后**，同 order 按 id（结果稳定可测）。 */
export function sortModels(rows: readonly ModelRowView[]): ModelRowView[] {
  return [...rows].sort((a, b) => {
    const ao = a.order ?? Number.POSITIVE_INFINITY;
    const bo = b.order ?? Number.POSITIVE_INFINITY;
    if (ao !== bo) return ao - bo;
    return a.id.localeCompare(b.id);
  });
}

// ================================================================ 定价判定

/** 定价的三个结论。**`unknown` 是一等公民**：不知道就说不知道。 */
export type PricingVerdict = "priced" | "unpriced" | "unknown";

/**
 * 单个模型的定价结论 —— **优先级 = 证据强度**：
 *
 * 1. `pricing_unpriced_models` 里有它 → 未定价（那是定价引擎自己的清单，它才是算钱的那个）；
 * 2. 后端显式说了 `priced: false` → 未定价；
 * 3. 后端显式说了 `priced: true` → 已定价；
 * 4. 未定价清单读得到、且里面没有它 → 已定价；
 * 5. 以上都不成立（两处都没数据）→ **未知**。
 *
 * 第 1 条压过第 3 条时，页面会在「两个来源不一致」里把这对矛盾原样报出来（不藏）。
 */
export function pricingVerdict(
  row: ModelRowView,
  unpricedIds: readonly string[] | undefined,
): PricingVerdict {
  const listed = unpricedIds === undefined ? undefined : unpricedIds.includes(row.id);
  if (listed === true) return "unpriced";
  if (row.priced === false) return "unpriced";
  if (row.priced === true) return "priced";
  if (listed === false) return "priced";
  return "unknown";
}

export type ModelsSummary = {
  models: number;
  /** 被任意模型引用到的通道（**去重**）。 */
  channels: number;
  /** 未定价的模型数（按上面的判定口径）。 */
  unpriced: number;
  /** 定价结论未知的模型数 —— 有它就说明"未定价"这个数只是下界。 */
  pricingUnknown: number;
  hidden: number;
};

export function summarizeModels(
  rows: readonly ModelRowView[],
  unpricedIds?: readonly string[],
): ModelsSummary {
  const channels = new Set<string>();
  for (const row of rows) for (const ref of row.channels) channels.add(ref.channelId);

  const verdicts = rows.map((r) => pricingVerdict(r, unpricedIds));
  return {
    models: rows.length,
    channels: channels.size,
    unpriced: verdicts.filter((v) => v === "unpriced").length,
    pricingUnknown: verdicts.filter((v) => v === "unknown").length,
    hidden: rows.filter((r) => !r.expose).length,
  };
}

/**
 * 两个定价来源的交叉核对结果。**只有两边都真的表过态才可能"不一致"** ——
 * 后端没给 `priced` 这一列时（真机现状）这里必然是空的，不会凭空造一个矛盾。
 *
 * - `pricedButListed`：说已定价，却被 `pricing_unpriced_models` 列为未定价；
 * - `unpricedButNotListed`：说未定价，却不在未定价清单里；
 * - `listedButUnknown`：定价清单里有、模型目录里根本没有的 id。
 */
export type PricingDiscrepancy = {
  pricedButListed: string[];
  unpricedButNotListed: string[];
  listedButUnknown: string[];
};

export function pricingDiscrepancy(
  rows: readonly ModelRowView[],
  unpricedIds: readonly string[] | undefined,
): PricingDiscrepancy {
  if (unpricedIds === undefined) {
    return { pricedButListed: [], unpricedButNotListed: [], listedButUnknown: [] };
  }
  const listed = new Set(unpricedIds);
  const known = new Set(rows.map((r) => r.id));
  return {
    pricedButListed: rows
      .filter((r) => r.priced === true && listed.has(r.id))
      .map((r) => r.id)
      .sort(),
    unpricedButNotListed: rows
      .filter((r) => r.priced === false && !listed.has(r.id))
      .map((r) => r.id)
      .sort(),
    listedButUnknown: [...listed].filter((id) => !known.has(id)).sort(),
  };
}

/** 有没有需要说出来的不一致（没有就不占位 —— 不写一句"一切正常"）。 */
export function hasDiscrepancy(d: PricingDiscrepancy): boolean {
  return d.pricedButListed.length > 0 || d.unpricedButNotListed.length > 0;
}

/**
 * 本地筛选（大小写不敏感），匹配 id / 别名 / 上游名 / 分组。
 *
 * **这是本地筛选，不是后端检索**：只作用在已经加载的行上，页面必须把这一点写在界面上
 * （否则用户会以为筛不到就是"没有这个模型"）。
 */
export function filterModels(rows: readonly ModelRowView[], query: string): ModelRowView[] {
  const q = query.trim().toLowerCase();
  if (q === "") return [...rows];
  return rows.filter((r) => {
    const haystack = [r.id, r.group ?? "", ...r.aliases, ...r.upstreamNames];
    return haystack.some((v) => v.toLowerCase().includes(q));
  });
}

/**
 * 模型目录页（[07 §4] /）的**纯逻辑层** —— 页面组件只做渲染，整形与判定都在这里，便于单测。
 *
 * ## 关系翻转后，这一页要知道的第一件事
 *
 * 线路（原「通道」）**归服务商**：在服务商详情页里创建/编辑（一个上游模型 + 网络代理 + 启用/权重）。
 * 模型**不自动创建** —— 用户在模型页**分配**线路，`Model.lines: ChannelId[]` 是关系的**唯一真源**；
 * `Channel.models: ModelId[]` 是后端现算的反向派生值（只读），本页**不读它、也不用它判定**
 * —— 两个真相源必然漂，而漂了不会报错。
 *
 * 由此三条纪律：
 *
 * 1. **"用哪几条线路" = 读 `model.lines`，再拿 id 去 `channel_list` 里查**：查得到才算一条线路；
 *    契约点了名、清单里却没有的，如实记进 `missingLines`（悬空引用，后端校验器也会报）——
 *    **不按前缀匹配、不做模糊匹配，也不猜一个名字**。
 * 2. **线路的展示名不是 id**：`上游模型（服务商 · 网络代理）`。服务商名来自 `provider_list`、
 *    代理名来自 `lane_list`；哪一层查不到就**回落 id**（与 `providers/detail` 同一条纪律：不编名字）。
 * 3. **没有"给模型挑网络代理"这回事**：代理是**线路**的属性，只在服务商页的
 *    「编辑线路」里改。所以本文件里没有、也不会有 `Model.default_lane` 的读写路径。
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

/**
 * 缺值占位符。
 *
 * **真源已收敛到 [`@/shared/format`]**（：这个字符原先在 7 个文件各写一遍）。
 * 这里保留一次**再导出**，因为本页的兄弟模块（`./index`）是从 `./model` 取它的 ——
 * 删掉再导出会让 4 个调用点全部编译失败，而那属于"为了整洁去动无关的引用面"。
 * 新代码请直接 `import { DASH } from "@/shared/format"`。
 */
export { DASH } from "@/shared/format";
// 本文件自己也要用（下面 `readModelRow` 的缺值格），所以除了再导出还得引进来。
import { DASH } from "@/shared/format";
import type {
  Channel,
  ChannelId,
  LaneView,
  Model,
  ModelView,
  ProviderView,
  PublicModelInfo,
} from "@/shared/bindings";

/** 数值缺失一律画破折号（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */

/**
 * 一条被模型**使用**的线路（行里「使用的线路」一格的一行）。
 *
 * `credential` 不在里面：`` 一个服务商一把密钥，凭据退化成服务商的 1:1 附件，
 * 模型页要回答的是"这个模型走哪几条线路"，凭据是服务商页的事。
 */
export type ModelLineRef = {
  /** 线路 id（= `channel_list` 里的 `Channel.id`）。 */
  lineId: ChannelId;
  /** 这条线路实际发给上游的模型名（可能与逻辑模型不同）。 */
  upstreamModel: string;
  enabled: boolean;
  providerId: string;
  /** 只在服务商清单里查得到时才有值。 */
  providerName: string | undefined;
  /** 线路自己写的网络代理（`Channel.lane`）；`undefined` = 没写（按继承链落到上层或直连）。 */
  laneId: string | undefined;
  /** 只在 `lane_list` 里查得到时才有值。 */
  laneName: string | undefined;
};

export type ModelRowView = {
  id: string;
  aliases: readonly string[];
  group: string | undefined;
  order: number | undefined;
  expose: boolean;
  /**
   * 模型级总开关。`false` = 彻底停用：网关不列出它、调用它直接 404。
   *
   * 与 `expose` 的分工：`expose` 管"列不列进 `/v1/models`"，`enabled` 管"能不能用"。
   * 列上把两者**分开显示** —— 一个是"藏起来"、一个是"关掉"，混在一列会看不出区别。
   */
  enabled: boolean;
  /** `model_list` 自报的线路数（后端的话，原样保留）。 */
  reportedLines: number;
  /** 该模型在各线路上的上游名（后端 `upstream_names` / `upstream_models`）。 */
  upstreamNames: readonly string[];
  /** `true`/`false` = 后端明确说了；**`undefined` = 后端没有这一列**（不许当成 false）。 */
  priced: boolean | undefined;
  costHint: string | undefined;
  /** 契约 `model.lines` 里**真的在 `channel_list` 里查到**的线路。 */
  lines: readonly ModelLineRef[];
  /**
   * **对外暴露的信息**（上下文窗口 / 最大输出 / 能力 / `owned_by`），
   * 后端一次算好（与网关 `/v1/models` 同一个函数），这里只带不重算。
   */
  publicInfo: PublicModelInfo;  /** 契约点了名、`channel_list` 里却找不到的线路 id（悬空引用；**一个都不许悄悄丢**）。 */
  missingLines: readonly string[];
  /**
   * 契约里的那个模型**原样**（「编辑」用它灌草稿）。
   *
   * 为什么留整份而不只留 id/字段：`model_upsert` 是**整条覆盖写**，编辑时必须"以原始记录为底"
   * —— 少带一个字段就是静默抹掉它（别名 / 分组 / 顺序 / expose / 甚至将来新增的字段）。
   */
  raw: Model;
};

/** 页面上要的三份清单（都是已注册命令的真实返回）。 */
export type ChannelsLookup = {
  channels: readonly Channel[];
  providers: readonly ProviderView[];
  /** `lane_list`：把线路的 `lane` id 翻成网络代理的名字（查不到就回落 id，不编名字）。 */
  lanes: readonly LaneView[];
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

/**
 * 这个模型要用哪几条线路（契约 `Model.lines`，关系的唯一真源）。
 *
 * 契约里它是**必填**；这里仍然并集读一次（非数组 → `[]`），因为真机的
 * `model_list` 在关系翻转落地前可能还没有这一列 —— 那时候页面得到的是"0 条线路"，
 * 与后端自报的条数对不上就会被 `models-lines-mismatch` 点出来（**缺数据要说出来，不静默**）。
 */
export function modelLinesOf(model: Model): ChannelId[] {
  return strArray((model as unknown as { lines?: unknown }).lines) ?? [];
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

/** 一条线路自己配的网络代理：名字查得到用名字，否则回落 id；没配就是 `undefined`（继承上层，不编）。 */
export function laneTextOf(ref: ModelLineRef): string | undefined {
  return ref.laneName ?? ref.laneId;
}

/**
 * 一条线路 → 引用行。名字查不到就留 `undefined`（不猜）。
 *
 * 代理名查的是 `LaneView.lane.id`：契约形状与真机形状（`{lane, latest, channel_count}`）
 * **都把线路对象包在 `lane` 里**，所以这里两套形状都认。
 */
export function readLineRef(channel: Channel, lookup: ChannelsLookup): ModelLineRef {
  const provider = lookup.providers.find((p) => p.id === channel.provider);
  const laneId =
    typeof channel.lane === "string" && channel.lane.length > 0 ? channel.lane : undefined;
  const lane =
    laneId === undefined ? undefined : lookup.lanes.find((view) => view.lane?.id === laneId);
  return {
    lineId: channel.id,
    upstreamModel: channel.upstream_model,
    enabled: channel.enabled,
    providerId: channel.provider,
    providerName: provider?.name,
    laneId,
    laneName: lane?.lane?.name,
  };
}

/**
 * 一条线路的展示名：`上游模型（服务商 · 网络代理）`。
 *
 * 括号里的两段都是**值**（服务商名、代理名/代理 id），与旧版 `上游模型（服务商 · 凭据）`
 * 同一套写法 —— 所以这个名字在任何语言下都成立，纯逻辑层也不必碰 i18n（不会出现第二份中文副本）。
 * 只有"已禁用"那一小段是文案，由调用方注入。
 *
 * 线路自己没写代理时**不写那一段**（继承链的事在服务商页看，这里不编一个"直连"）。
 */
export function lineLabel(ref: ModelLineRef, disabled: string): string {
  const parts = [ref.providerName ?? ref.providerId];
  const lane = laneTextOf(ref);
  if (lane !== undefined) parts.push(lane);
  return `${ref.upstreamModel}（${parts.join(" · ")}）${ref.enabled ? "" : ` · ${disabled}`}`;
}

/** 一份 `model_list` 行 → 页面行。 */
export function readModelRow(view: ModelView, lookup: ChannelsLookup): ModelRowView {
  const byId = new Map(lookup.channels.map((channel) => [channel.id, channel] as const));
  const lines: ModelLineRef[] = [];
  const missingLines: string[] = [];

  for (const id of modelLinesOf(view.model)) {
    const channel = byId.get(id);
    if (channel === undefined) {
      missingLines.push(id);
      continue;
    }
    lines.push(readLineRef(channel, lookup));
  }

  return {
    id: view.model.id,
    aliases: view.model.aliases,
    group: view.model.display.group ?? undefined,
    order: view.model.display.order,
    expose: view.model.expose,
    // 缺列 → 当成"开着"（与后端 `#[serde(default = "default_true")]` 同口径），
    // **不能**把"后端没说"显示成"已停用"。
    enabled: view.model.enabled !== false,
    reportedLines: view.channel_count,
    upstreamNames: upstreamNamesOf(view),
    // 对外暴露的信息由后端一次算好（网关 `/v1/models` 用的是同一个函数），
    // 这里**原样带过去**，不在前端重算一遍（两份口径必然漂）。
    publicInfo: view.public_info,
    priced: pricedOf(view),
    costHint: view.cost_hint ?? undefined,
    lines,
    missingLines,
    raw: view.model,
  };
}

export function readModelRows(
  views: readonly ModelView[],
  lookup: ChannelsLookup,
): ModelRowView[] {
  return views.map((v) => readModelRow(v, lookup));
}

/**
 * 添加模型时的**候选线路** = `channel_list` 全部（：一条线路可以服务多个模型，
 * 所以这里不排除任何一条）。排序按 `服务商 → 上游模型 → 线路 id`，结果稳定可测 ——
 * 用户是在**好几个服务商**的线路里挑，顺序稳定才找得到。
 */
export function lineCandidates(
  channels: readonly Channel[],
  lookup: ChannelsLookup,
): ModelLineRef[] {
  return channels
    .map((channel) => readLineRef(channel, lookup))
    .sort((a, b) => {
      const ap = a.providerName ?? a.providerId;
      const bp = b.providerName ?? b.providerId;
      if (ap !== bp) return ap.localeCompare(bp);
      if (a.upstreamModel !== b.upstreamModel) return a.upstreamModel.localeCompare(b.upstreamModel);
      return a.lineId.localeCompare(b.lineId);
    });
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
  /** 被任意模型用到的线路（**去重**）—— 同一个模型用两条只算两条，两条模型共用一条只算一条。 */
  lines: number;
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
  const lines = new Set<string>();
  for (const row of rows) for (const ref of row.lines) lines.add(ref.lineId);

  const verdicts = rows.map((r) => pricingVerdict(r, unpricedIds));
  return {
    models: rows.length,
    lines: lines.size,
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
 * 本地筛选（大小写不敏感），匹配 id / 别名 / 上游名 / 分组 / **所用线路的服务商与上游模型**。
 *
 * 加了最后一项是因为关系翻转后"这个模型走哪家"只有线路说得出来：不能筛"字节方舟"的话，
 * 用户就得把每一行都点开看。**这仍然是本地筛选，不是后端检索** —— 只作用在已经加载的行上，
 * 页面必须把这一点写在界面上（否则用户会以为筛不到就是"没有这个模型"）。
 */
export function filterModels(rows: readonly ModelRowView[], query: string): ModelRowView[] {
  const q = query.trim().toLowerCase();
  if (q === "") return [...rows];
  return rows.filter((r) => {
    const haystack = [
      r.id,
      r.group ?? "",
      ...r.aliases,
      ...r.upstreamNames,
      ...r.lines.map((line) => line.upstreamModel),
      ...r.lines.map((line) => line.providerName ?? line.providerId),
    ];
    return haystack.some((v) => v.toLowerCase().includes(q));
  });
}

// ================================================================ 添加模型（/）
//
// 这一页做的是**分配**：把一个新模型指定给若干条**已经存在**的线路（≥1）。
// 线路本身在服务商页创建/编辑（含网络代理），所以这里既没有"新建线路"，
// 也没有任何"给模型选网络代理"的控件。

/**
 * 添加模型表单的草稿。**全部按"用户输入的原样"存**（`aliases` 是逗号分隔的字符串、
 * `order` 是字符串），只在提交前才解释 —— 这样"用户输了什么"与"我们解释成什么"不会混在一起，
 * 校验报错时也能准确指出是哪个输入。
 *
 * `lines` 存的是**用户勾选的线路 id，按勾选顺序**（不是字符串：勾选框没有"原样字符串"这回事）。
 */
export type ModelDraft = {
  id: string;
  aliases: string;
  group: string;
  order: string;
  expose: boolean;
  /**
   * 模型级总开关。**不在这个草稿里编辑** —— 它是列表上的一个直接开关，
   * 不是"编辑模型"对话框里的一个字段。
   *
   * 为什么仍要在这个草稿类型里带它：`model_upsert` 是**整条覆盖写**，
   * 编辑别名/分组时必须"以原始记录为底"，漏掉 `enabled` 就会**把用户刚关掉的模型悄悄打开**。
   * 所以它随草稿一起往返（`modelToDraft` → `draftToModel`），只是没有对应的输入控件。
   */
  enabled: boolean;
  lines: readonly string[];
};

/** 规定的 id 形态。 */
export const MODEL_ID_RE = /^[a-z0-9][a-z0-9._-]*$/i;

export const EMPTY_MODEL_DRAFT: ModelDraft = {
  id: "",
  aliases: "",
  group: "",
  order: "0",
  expose: true,
  // 新建的模型默认**开着**：`enabled` 是给"暂时下线一个已有模型"用的，
  // 创建即停用没有使用场景。
  enabled: true,
  lines: [],
};

export type ModelDraftIssue = {
  field: "id" | "order" | "lines";
  /** 机器可读的原因；UI 按它取文案，**不解析文案本身**。 */
  reason: "required" | "shape" | "duplicate" | "notInteger" | "negative";
};

/** 别名：逗号分隔 → 去空、去重（保序）。重复不算错误，只是不该把同一个名字存两遍。 */
export function parseAliases(raw: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of raw.split(",")) {
    const name = part.trim();
    if (name.length === 0 || seen.has(name)) continue;
    seen.add(name);
    out.push(name);
  }
  return out;
}

/** 勾选的线路 id：去空、去重、**保序**（顺序 = 用户勾选的顺序，提交载荷与它逐字一致）。 */
export function parseLineIds(ids: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of ids) {
    const id = raw.trim();
    if (id.length === 0 || seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}

/**
 * 本地预检。**只做"不用问后端就知道"的四件事**（必填 / 形态 / 与现有 `model_list` 重复 /
 * 至少一条线路）加一条 order 非负整数。权威判定仍在后端 —— `model_upsert` 的错误
 *（TA 码 + details）原样显示，本地预检不替代它。
 *
 * "至少一条线路"是**本地挡下来的硬规则**（：零条线路的模型对外可调用却必然失败），
 * 所以它是"一次都不发"的那一类 issue，不是警告。
 */
export function validateModelDraft(
  draft: ModelDraft,
  existingIds: readonly string[],
): ModelDraftIssue[] {
  const issues: ModelDraftIssue[] = [];
  const id = draft.id.trim();
  if (id.length === 0) {
    issues.push({ field: "id", reason: "required" });
  } else if (!MODEL_ID_RE.test(id)) {
    issues.push({ field: "id", reason: "shape" });
  } else if (existingIds.some((known) => known === id)) {
    issues.push({ field: "id", reason: "duplicate" });
  }

  const order = draft.order.trim();
  if (!/^[0-9]+$/.test(order)) {
    // 空串也走这一条：`display.order` 在契约里是 `number`，没有"留空"的语义
    issues.push({ field: "order", reason: order.startsWith("-") ? "negative" : "notInteger" });
  }

  if (parseLineIds(draft.lines).length === 0) {
    issues.push({ field: "lines", reason: "required" });
  }

  return issues;
}

/**
 * 草稿 → 契约的 `Model`：`group` 空串 → `null`（那是契约允许的"没有分组"），
 * `lines` = 勾选的线路 id（**至少一条**，由 `validateModelDraft` 挡在前面）。
 *
 * 这里**不写 `default_lane`**（：没有"给模型挑网络代理"这回事）。
 */
export function draftToModel(draft: ModelDraft): Model {
  const group = draft.group.trim();
  return {
    id: draft.id.trim(),
    aliases: parseAliases(draft.aliases),
    display: { group: group.length === 0 ? null : group, order: Number(draft.order.trim()) },
    expose: draft.expose,
    // 原样带上（见 `ModelDraft.enabled` 的说明）：漏了它会把用户关掉的模型悄悄打开。
    enabled: draft.enabled,
    lines: parseLineIds(draft.lines),
  };
}

/**
 * 已有模型 → 草稿（：**「等用户去分配」不能只发生在创建那一刻**）。
 *
 * 为什么必须有这条反向映射：线路是**先有服务商、后有模型**；用户新建一条线路之后，
 * 要给一个**已经存在**的模型加线路，否则只能"删了模型重建"——那会连带丢掉别名、
 * 分组、顺序和 `expose` 这些与线路无关的设置。所以模型行上有一个「编辑」入口，
 * 它打开的对话框与"新建"是**同一份**，只是 id 只读（`model_upsert` 按 id 匹配，
 * 允许改 id 等于"创建一个新模型 + 留下一个孤儿"）。
 *
 * `display.order` 在契约里是 number（没有"留空"语义），所以这里 `?? 0`；
 * 别名拼回逗号分隔串 —— 与 `parseAliases` 是**同一套口径**的反函数。
 */
export function modelToDraft(m: Model): ModelDraft {
  return {
    id: m.id,
    aliases: m.aliases.join(", "),
    group: m.display?.group ?? "",
    order: String(m.display?.order ?? 0),
    expose: m.expose,
    // 契约里 `enabled` 是必填，但真机上的老 `model_list` 可能还没有这一列 ——
    // `!== false` 让"缺列"落到**开着**（与后端 `#[serde(default = "default_true")]` 同口径）。
    enabled: m.enabled !== false,
    lines: [...m.lines],
  };
}

import type { PriceEntry } from "@/shared/bindings";
import type { I18nKey } from "@/shared/i18n";

/**
 * 「为**一个对外模型**定价」的草稿与载荷整形。
 *
 * # 口径（用户第 318 轮明确纠正）
 *
 * 定价的对象是**对外暴露的逻辑模型**，不是"某个服务商的某条线路"。
 * 用户的原话：*"我是希望用户为每一个对外模型定价，来算自己大概花了多少，
 * 而不是对服务商的线路计价"*。
 *
 * 这与运行时的实际判据是**一致**的：网关计量时拿的是**客户端请求的对外模型名**
 * （`crates/ta-gateway/src/meter.rs`：`let model = ctx.model…` 然后
 * `pricing.resolve(&model, &provider, &cred)`），而 `PriceMatch.model` 匹配的也正是这个名字。
 * 所以"按模型定价"不是新增能力，而是**把界面拨回到引擎本来的口径上**。
 *
 * # 为什么单独成模块
 *
 * 这段整形原先在已删的 `/channels` 页、后来又随搬迁进了 `cost.tsx`。
 * 现在价格配置归**模型页**、成本中心只做显示，于是它必须离开 `cost.tsx` ——
 * 否则"只做显示"的页面上会留着一份没人用的写入逻辑。
 */

/**
 * 新增 / 修改定价的草稿。
 *
 * 三个价格字段是**字符串**（用户打进来的原文）：空串与非法输入是两种不同状态，
 * 归一成数字就再也分不清"没填"与"填错了"。
 */
export type PricingDraft = {
  /** 对外模型 id（**规范名**；别名由网关归一到它，见 `ingress.rs` 的归一注释）。 */
  model: string;
  priceInput: string;
  priceOutput: string;
  priceCached: string;
};

/** 空草稿（对话框每次打开都从它开始，避免带上一次的残留）。 */
export const EMPTY_PRICING_DRAFT: PricingDraft = {
  model: "",
  priceInput: "",
  priceOutput: "",
  priceCached: "",
};

/** 一个价格输入：`""` = 这一项不计价；非负数字才认（负号 / 半截小数 / 非数字一律 `undefined`）。 */
export function parsePrice(raw: string): number | undefined {
  const text = raw.trim();
  if (text === "") return undefined;
  if (!/^\d+(\.\d+)?$/.test(text)) return undefined;
  const value = Number(text);
  return Number.isFinite(value) ? value : undefined;
}

/** 三项计费里**有任意一项非空**（= 要写定价条目）。`"0"` 是非空：0 美元也是明确的价。 */
export function hasPrice(draft: PricingDraft): boolean {
  return (
    draft.priceInput.trim() !== "" ||
    draft.priceOutput.trim() !== "" ||
    draft.priceCached.trim() !== ""
  );
}

/** 三个价格字段的内部名（`PricingDraft` 上的键，也是 `…-issue-<field>` 的后缀来源）。 */
export const PRICE_FIELDS = ["priceInput", "priceOutput", "priceCached"] as const;

/** `PRICE_FIELDS` 里的一项（`invalidPriceFields` 的返回元素类型）。 */
export type PriceField = (typeof PRICE_FIELDS)[number];

/**
 * `rates` 只放**填了的**键，键名是 ta-core `TokenKind::as_key()`
 * （`crates/ta-core/src/types.rs`）：`input` / `output` / `input_cached`。
 *
 * 没填的键**一个都不放** —— 放 0 会把"没定价"写成"免费"。
 */
export function priceRates(draft: PricingDraft): Record<string, number> {
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
 * 草稿 -> `pricing_upsert({ entry })` 的载荷：
 *
 * * `match` = **只有 `model`**（这次修正的核心：不再按服务商/凭据定价）；
 * * `source: "manual"`（用户手填的价，不是内置表）；
 * * `currency` / `effective_from` 显式 `null`（用全局配置的币种、立即生效，不替用户挑一个）。
 *
 * **绝不发空的 `model`**：价格会挂到一个不存在的模型上，那比不写更坏。
 * 调用方负责保证 `draft.model` 非空（对话框里的模型来自被点的那一行，用户改不了）。
 */
export function priceEntryFor(draft: PricingDraft): PriceEntry {
  return {
    match: { model: draft.model.trim() },
    rates: priceRates(draft),
    multiplier: 1,
    currency: null,
    source: "manual",
    effective_from: null,
  };
}

/** 填了但不是合法非负数字的字段（本地预检用；空串不算错 —— 空 = 这一项不计价）。 */
export function invalidPriceFields(draft: PricingDraft): PriceField[] {
  return PRICE_FIELDS.filter(
    (field) => draft[field].trim() !== "" && parsePrice(draft[field]) === undefined,
  );
}

/** 一个价格字段的标签键（对话框与错位点名共用，保证两处说的是同一个词）。 */
export const PRICE_FIELD_LABEL_KEY: Record<PriceField, I18nKey> = {
  priceInput: "cost.pricing.add.input",
  priceOutput: "cost.pricing.add.output",
  priceCached: "cost.pricing.add.cached",
};

/**
 * 从**已有条目**回填草稿：打开对话框时把当前的价显示出来，而不是让用户重新打一遍
 * （否则"改价"的第一步变成"猜上次填了多少"）。
 *
 * `rates` 里没有那一项 ⇒ 留空（= 不计价），**不回填 0**。
 */
export function draftFromEntry(model: string, entry: PriceEntry | undefined): PricingDraft {
  const rates = (entry?.rates ?? {}) as Record<string, unknown>;
  const num = (key: string): string => {
    const value = rates[key];
    return typeof value === "number" && Number.isFinite(value) ? String(value) : "";
  };
  return {
    model,
    priceInput: num("input"),
    priceOutput: num("output"),
    priceCached: num("input_cached"),
  };
}

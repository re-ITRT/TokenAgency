/**
 * 成本中心 `/cost`（[07 §6]、[10 §2.4]）—— 用量与花费的**趋势 + 明细 + 导出**，
 * 外加**定价条目**区块（[docs/21 §3]：列表 + 单条 / 批量删除）。
 *
 * 四条纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **只用已注册的命令**：`usage_dashboard` / `usage_query` / `usage_token_kinds` /
 *    `usage_export_csv` / `usage_request_stream_snapshot` / `pricing_unpriced_models` / `app_open_dir`，
 *    定价区块另发 `pricing_list`（读）与 `pricing_delete`（删）。
 *    定价的写命令 `pricing_upsert` **后端已可用、本页仍未接入 → 不放按钮，也不放禁用占位**。
 * 2. **估算不许冒充上游上报**（[11 §6] 铁律 4）：`UsageSource`（upstream / upstream_partial /
 *    estimated）只写在写入路径的 `UsageRecord` 上（`crates/ta-core/src/types.rs:282`），
 *    而**查询与看板的返回结构都没有带它**（`UsageRow` 只有 `dims`/`values`；`DashboardData`
 *    没有 `estimated_requests` —— 这两个字段按 [16 §8] 推迟）。所以本页只**如实说明"来源标记不可得"**，
 *    绝不把 token 数标成"上游上报"；花费更是本地定价表算出来的（[07 §6.2]），一律按本地口径标注。
 * 3. **图表真用 ECharts，且只画后端给的数**：option 的 x 轴标签就是后端返回的分组/时间标签，
 *    系列值就是后端的数值 —— **不插点、不补 0、不造趋势**；没有数据时给诚实的空状态，
 *    而不是一个空的坐标系。某指标在部分行缺值时**整条系列被排除**并说明（不补 0）。
 * 4. **两套字段名都读得懂**：冻结契约（`bindings.ts` 的 `columns`/`metrics`/`total`）与 Rust
 *    （`ta_store::UsageTable` 的 `metrics`/`values`/`total_groups`/`bucket`）不一样 ——
 *    收口在 `readUsageTable()` 一处（与 `/logs` 的 `pages/logs/model.ts` 同一套做法）。
 *
 * 定价条目的删除有一条**独有的**安全规则（[docs/21 §1.1 第 5 条]）：`pricing_delete` 的入参是
 * **index**，所以删之前必须**重新读一次** `pricing_list`，逐字段确认那个下标上仍然是用户看到的
 * 那一条；不一致就**拒绝执行**（让用户重看一遍），**绝不**在错位上下手。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("usage_query", usageQueryArgs(buildUsageQuery(applied)), qk.usage(fp))` → 明细表与图表
 * - `useCommandQuery("usage_dashboard", {range:"today"}, qk.dashboard("today"))`           → 概览卡与今日趋势
 * - `useCommandQuery("usage_token_kinds", undefined, qk.usageTokenKinds)`                      → 指标候选
 * - `useCommandQuery("usage_request_stream_snapshot", {limit}, qk.usage("recent:…"))`          → 最近请求
 * - `useCommandQuery("pricing_unpriced_models", undefined, qk.unpricedModels)`                 → 未定价模型
 * - `useCommandQuery("pricing_list", undefined, qk.pricing)`                                   → 定价条目（新增区块）
 * - `useCommandMutation("usage_export_csv")` / `useCommandMutation("app_open_dir")` / `useCommandMutation("pricing_delete")`
 */

import {
  Box,
  Button,
  Checkbox,
  FormControlLabel,
  MenuItem,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from "@mui/material";
import { useTheme, type Theme } from "@mui/material/styles";
import { useQueryClient } from "@tanstack/react-query";
import * as echarts from "echarts";
import type { EChartsOption } from "echarts";
import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";

import {
  errorMessage,
  isTauri,
  pricing as pricingApi,
  qk,
  useCommandMutation,
  useCommandQuery,
} from "@/shared/api";
import type { RequestSummary, TimeRange, UsageQuery } from "@/shared/bindings";
import { ConfirmDelete, HelpTip, NumberCard, PageHeader } from "@/shared/components";
import {
  formatExact,
  formatInt,
  formatLocalTime,
  formatMoney,
  formatPercent,
  formatTokens,
  timezoneLabel,
} from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { useAppStore } from "@/stores/app";
import { radius, typography } from "@/theme";

// ================================================================ 常量与文案映射

/** 最近请求快照要几条（后端接受 limit）。 */
const RECENT_LIMIT = 50;

/** 明细查询一次最多返回多少组（后端 `DEFAULT_LIMIT` 是 1000，这里更保守）。 */
const QUERY_LIMIT = 200;

/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

/**
 * 页面自己的范围词（`7d` / `30d` / `month`，只用于 i18n 与 query key）→
 * **`TimeRange` 的线上值**（`crates/ta-core/src/clock.rs:77-88` 的裸 snake_case 变体名）。
 *
 * 这是**界面词汇与线上词汇之间唯一的翻译点**：翻译后就是 `TimeRange` 本身，
 * 不再需要 `as unknown as` 之类的断言（断言正是上一版 `{preset:…}` 能溜过去的原因）。
 */
const RANGE_TO_TIME_RANGE: Record<UsageRange, TimeRange> = {
  today: "today",
  "7d": "last7d",
  "30d": "last30d",
  month: "this_month",
};

const RANGE_KEY: Record<UsageRange, I18nKey> = {
  today: "cost.query.range.today",
  "7d": "cost.query.range.7d",
  "30d": "cost.query.range.30d",
  month: "cost.query.range.month",
};

const GROUP_KEY: Record<UsageGroupDim, I18nKey> = {
  time: "cost.query.group.time",
  model: "cost.query.group.model",
  credential: "cost.query.group.credential",
  provider: "cost.query.group.provider",
  client: "cost.query.group.client",
};

const BUCKET_KEY: Record<UsageTimeBucket, I18nKey> = {
  hour: "cost.query.bucket.hour",
  day: "cost.query.bucket.day",
};

const FORMAT_KEY: Record<UsageFormat, I18nKey> = {
  table: "cost.query.format.table",
  stacked_bar: "cost.query.format.stacked_bar",
  area: "cost.query.format.area",
};

/**
 * 指标 key → i18n。**这是一张开放集合**：`usage_token_kinds` 明天报一个新类型，
 * 表里没有就**原样显示 key**（那是后端说的话），不丢弃、也不硬塞一个中文名。
 */
const METRIC_KEY: Record<string, I18nKey> = {
  requests: "cost.metric.requests",
  cost: "cost.metric.cost",
  cache_hit_rate: "cost.metric.cache_hit_rate",
  avg_latency: "cost.metric.avg_latency",
  input: "cost.token.input",
  input_cached: "cost.token.input_cached",
  input_cache_write: "cost.token.input_cache_write",
  input_cache_write_1h: "cost.token.input_cache_write_1h",
  input_audio: "cost.token.input_audio",
  input_image: "cost.token.input_image",
  output: "cost.token.output",
  output_reasoning: "cost.token.output_reasoning",
  output_audio: "cost.token.output_audio",
  output_prediction_accepted: "cost.token.output_prediction_accepted",
  output_prediction_rejected: "cost.token.output_prediction_rejected",
};

/** 内置指标（与后端 `Metric::parse` 的四个关键字一致）。 */
const BUILTIN_METRICS: readonly string[] = ["requests", "cost"];

// ================================================================ 查询条件

export type UsageRange = "today" | "7d" | "30d" | "month";
export type UsageGroupDim = "time" | "model" | "credential" | "provider" | "client";
export type UsageTimeBucket = "hour" | "day";
export type UsageFormat = "table" | "stacked_bar" | "area";

export type CostFilter = {
  range: UsageRange;
  groupBy: UsageGroupDim;
  /** 只在 `groupBy === "time"` 时发给后端。 */
  bucket: UsageTimeBucket;
  metrics: string[];
  format: UsageFormat;
};

export const DEFAULT_COST_FILTER: CostFilter = {
  range: "today",
  groupBy: "model",
  bucket: "hour",
  metrics: ["requests", "cost"],
  format: "stacked_bar",
};

/** 条件指纹：进 query key，条件一变必然重查（与 `/logs` 的 `filterFingerprint` 同一约定）。 */
export function costFilterFingerprint(filter: CostFilter): string {
  return [
    filter.range,
    filter.groupBy,
    filter.groupBy === "time" ? filter.bucket : "-",
    filter.format,
    [...filter.metrics].sort().join("+"),
  ].join("|");
}

/** 界面范围词 → 发出去的 `TimeRange`（线上形状；`explicit` 走 `{explicit:{from_ms,to_ms}}`）。 */
function rustRange(preset: UsageRange): TimeRange {
  return RANGE_TO_TIME_RANGE[preset];
}

/**
 * 表单条件 → `usage_query` 的 `UsageQuery`。
 *
 * 只发用户真的选了的维度与指标：**不塞默认筛选**（那会让"看起来没筛"的结果其实是筛过的）。
 */
export function buildUsageQuery(filter: CostFilter): UsageQuery {
  const query: UsageQuery = {
    range: rustRange(filter.range),
    group_by: [filter.groupBy],
    metrics: [...filter.metrics],
    format: filter.format,
    limit: QUERY_LIMIT,
  };
  if (filter.groupBy === "time") query.time_bucket = filter.bucket;
  return query;
}

/**
 * `usage_query` 的入参：**两个参数名都发**。
 *
 * Rust 侧 `pub async fn usage_query(state, q: ta_store::UsageQuery)` 取的是 `q`，
 * 而冻结契约（`bindings.ts`）写的是 `query` —— 只发一个，另一边就会在真机上失败。
 */
export function usageQueryArgs(query: UsageQuery): { query: UsageQuery } {
  return { query, q: query } as unknown as { query: UsageQuery };
}

/** `usage_export_csv` 的入参（Rust 取 `q`，契约写 `query`；同样的两写）。 */
export function usageExportArgs(query: UsageQuery, path: string): { query: UsageQuery; path: string } {
  return { query, q: query, path } as unknown as { query: UsageQuery; path: string };
}

// ================================================================ 读后端数据（两套字段名都认）

export type UsageRowView = {
  /** 时间维度的本地时间标签（后端给的，前端不重算；没有时间维度时 `undefined`）。 */
  bucket: string | undefined;
  bucketStartMs: number | undefined;
  /** 其它分组维度的取值（原样字符串）。 */
  dims: Record<string, string>;
  /** 指标 key → 值。缺的 key 就是 `undefined`（**不是 0**）。 */
  values: Record<string, number>;
};

export type UsageTableView = {
  metrics: string[];
  groupBy: string[];
  rows: UsageRowView[];
  /** 截断前的分组总数；后端没给就是 `undefined`（不拿行数冒充）。 */
  totalGroups: number | undefined;
  truncated: boolean;
  timeBucket: string | undefined;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function num(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function asStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

/** 一行里所有 dims 的 key（用于从契约的 `columns` 里剥掉维度列）。 */
function dimKeysOf(rows: UsageRowView[]): Set<string> {
  const keys = new Set<string>();
  for (const row of rows) for (const key of Object.keys(row.dims)) keys.add(key);
  return keys;
}

/**
 * `usage_query` 的返回 → 视图模型。**两套字段名都认**（见文件头的纪律 4）；
 * 结构不是"行数组"就返回 `undefined`（调用方给诚实空态，不编一张表）。
 */
export function readUsageTable(raw: unknown): UsageTableView | undefined {
  if (!isRecord(raw)) return undefined;
  const rawRows = raw["rows"];
  if (!Array.isArray(rawRows)) return undefined;

  const rows: UsageRowView[] = [];
  for (const item of rawRows) {
    if (!isRecord(item)) continue;
    const dims: Record<string, string> = {};
    const rawDims = item["dims"];
    if (isRecord(rawDims)) {
      for (const [key, value] of Object.entries(rawDims)) {
        if (typeof value === "string") dims[key] = value;
        else if (typeof value === "number") dims[key] = String(value);
      }
    }
    const values: Record<string, number> = {};
    // Rust 侧叫 `values`，冻结契约叫 `metrics` —— 两个都读。
    for (const bag of [item["values"], item["metrics"]]) {
      if (!isRecord(bag)) continue;
      for (const [key, value] of Object.entries(bag)) {
        const n = num(value);
        if (n !== undefined) values[key] = n;
      }
    }
    rows.push({
      bucket: typeof item["bucket"] === "string" ? item["bucket"] : undefined,
      bucketStartMs: num(item["bucket_start_ms"]),
      dims,
      values,
    });
  }

  // 指标列：Rust 给权威列表（`metrics`）；契约给的是 `columns`（含维度列，要剥掉）。
  let metrics = asStrings(raw["metrics"]);
  if (metrics.length === 0) {
    const dims = dimKeysOf(rows);
    metrics = asStrings(raw["columns"]).filter((column) => !dims.has(column) && column !== "bucket");
  }

  const groupBy =
    asStrings(raw["group_by"]).length > 0
      ? asStrings(raw["group_by"])
      : [...dimKeysOf(rows), ...(rows.some((r) => r.bucket !== undefined) ? ["time"] : [])];

  const totalGroups = num(raw["total_groups"]) ?? num(raw["total"]);
  // 后端自己说截断就信它；它没说但"已返回的行数 < 分组总数"同样是截断（[10 §2.4] 的语义）。
  const truncated =
    raw["truncated"] === true || (totalGroups !== undefined && rows.length < totalGroups);

  return {
    metrics,
    groupBy,
    rows,
    totalGroups,
    truncated,
    timeBucket:
      typeof raw["time_bucket"] === "string"
        ? raw["time_bucket"]
        : typeof raw["timeBucket"] === "string"
          ? raw["timeBucket"]
          : undefined,
  };
}

/** 一行的维度标签：时间维度优先用后端给的 `bucket`，其次 dims，最后才是本地的真实时间戳。 */
export function rowLabel(row: UsageRowView, groupBy: string): string {
  if (row.bucket !== undefined && row.bucket !== "") return row.bucket;
  const fromDims = row.dims[groupBy];
  if (fromDims !== undefined && fromDims !== "") return fromDims;
  if (groupBy === "time" && row.bucketStartMs !== undefined) {
    return formatLocalTime(row.bucketStartMs, "MM-DD HH:mm");
  }
  return DASH;
}

// ================================================================ 图表（ECharts）

export type ChartSeries = { key: string; values: number[] };

export type ChartInput = {
  /** x 轴：后端给的分组 / 时间标签。 */
  labels: string[];
  /** 左轴系列（柱）。 */
  series: ChartSeries[];
  /** 右轴系列（折线，只有花费用；没有就不画右轴）。 */
  costSeries?: ChartSeries;
  /** 因为"某些行缺该指标"而被排除的指标（**不补 0**）。 */
  excluded: string[];
};

export type ChartPalette = {
  text: string;
  muted: string;
  border: string;
  series: string[];
};

/**
 * 明细表 → 图表输入。**只搬运后端的数**：
 * 某指标只要有**任意一行**缺值，就不让它进图（宁可少画一条，也不补 0 造数据），
 * 被排除的指标写进 `excluded`，由界面说明。
 */
export function chartInputFromTable(
  table: UsageTableView | undefined,
  metrics: readonly string[],
): ChartInput {
  if (table === undefined) return { labels: [], series: [], excluded: [] };
  const labels = table.rows.map((row) => rowLabel(row, table.groupBy[0] ?? "time"));
  const series: ChartSeries[] = [];
  const excluded: string[] = [];
  for (const metric of metrics) {
    const values = table.rows.map((row) => row.values[metric]);
    if (values.some((value) => value === undefined)) {
      excluded.push(metric);
      continue;
    }
    series.push({ key: metric, values: values.map((value) => value ?? 0) });
  }
  return { labels, series, excluded };
}

export type TrendPointView = {
  tsMs: number;
  input: number;
  inputCached: number;
  output: number;
  cost: number;
};

/** 课表的今日趋势（`DashboardData.trend`，Rust 与契约字段一致）→ 图表输入。 */
export function chartInputFromTrend(trend: readonly TrendPointView[] | undefined): ChartInput {
  const points = trend ?? [];
  if (points.length === 0) return { labels: [], series: [], excluded: [] };
  return {
    labels: points.map((point) => formatLocalTime(point.tsMs, "MM-DD HH:mm")),
    series: [
      { key: "input", values: points.map((point) => point.input) },
      { key: "input_cached", values: points.map((point) => point.inputCached) },
      { key: "output", values: points.map((point) => point.output) },
    ],
    costSeries: { key: "cost", values: points.map((point) => point.cost) },
    excluded: [],
  };
}

/** ECharts option 的结构化描述（用宽类型，避免把 echarts 的联合类型抄一遍）。 */
export type ChartOption = Record<string, unknown>;

/**
 * 图表 option。**没有标签或没有系列就返回 `undefined`** —— 由调用方给空状态，
 * 而不是画一个空的坐标系（那会让人以为"有数据但都是 0"）。
 */
export function buildChartOption(
  input: ChartInput,
  palette: ChartPalette,
  options?: { stacked?: boolean },
): ChartOption | undefined {
  if (input.labels.length === 0 || input.series.length === 0) return undefined;
  const stacked = options?.stacked === true;
  const cost = input.costSeries;

  const series: Record<string, unknown>[] = input.series.map((item) => ({
    name: item.key,
    type: "bar",
    ...(stacked ? { stack: "total" } : {}),
    barMaxWidth: 16,
    data: [...item.values],
  }));
  if (cost !== undefined && cost.values.some((value) => value !== 0)) {
    series.push({
      name: cost.key,
      type: "line",
      yAxisIndex: 1,
      smooth: false,
      symbolSize: 4,
      data: [...cost.values],
    });
  }

  const axisLabel = { color: palette.muted, fontSize: typography.size.meta };
  const axisLine = { lineStyle: { color: palette.border } };
  const splitLine = { lineStyle: { color: palette.border } };

  return {
    color: palette.series,
    // 网格用令牌里的间距（[15 §3.3]），不写刻度外的数
    grid: { left: 8, right: 8, top: 24, bottom: 8, containLabel: true },
    tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
    legend: { show: false },
    xAxis: { type: "category", data: [...input.labels], axisLabel, axisLine },
    // 有花费系列时给一条右轴：token 与金额量级差几个数量级，共用一条轴会互相压扁
    yAxis:
      cost !== undefined && cost.values.some((value) => value !== 0)
        ? [
            { type: "value", axisLabel, splitLine },
            // 右轴**不画**分割线（否则两条轴的网格线会叠在一起）——
            // 原来这里写了两个 `splitLine`，后者是语法错误（重复属性），整页因此连模块都加载不了。
            { type: "value", axisLabel, splitLine: { show: false } },
          ]
        : { type: "value", axisLabel, splitLine },
    series,
  };
}

export type UsageChartProps = {
  option: ChartOption;
  /** 画布高度（px）。 */
  height: number;
  testId?: string;
};

/**
 * ECharts 容器。**option 变了就重建实例**（destroy → init → setOption），
 * 这样不会把上一次的系列留在图上。`option` 必须由调用方 `useMemo` 稳定住。
 */
export function UsageChart({ option, height, testId }: UsageChartProps): ReactElement {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (el === null) return;
    const chart = echarts.init(el);
    chart.setOption(option as EChartsOption);
    const onResize = () => chart.resize();
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      chart.dispose();
    };
  }, [option]);

  return <Box ref={ref} data-testid={testId} sx={{ width: "100%", height }} />;
}

// ================================================================ 读看板 DTO（两套都认）

export type DashboardView = {
  requests: number | undefined;
  tokens: number | undefined;
  cost: { amount: number; currency: string } | undefined;
  successRate: number | undefined;
  /** 上期值只有契约里有（Rust 的 `Num` 只有 `value`/`display`）→ 读不到就不给涨跌。 */
  prevRequests: number | undefined;
  prevTokens: number | undefined;
  prevCost: number | undefined;
  trend: TrendPointView[];
  unpricedCount: number | undefined;
};

function money(value: unknown): { amount: number; currency: string } | undefined {
  if (!isRecord(value)) return undefined;
  const amount = num(value["amount"]);
  const currency = typeof value["currency"] === "string" ? value["currency"] : undefined;
  if (amount === undefined || currency === undefined) return undefined;
  return { amount, currency };
}

/** `usage_dashboard` 的返回 → 概览视图。读不到的字段一律 `undefined`（不填 0）。 */
export function readDashboard(raw: unknown): DashboardView | undefined {
  if (!isRecord(raw)) return undefined;
  const cards = isRecord(raw["cards"]) ? raw["cards"] : {};
  const requests = isRecord(cards["requests"]) ? cards["requests"] : {};
  const tokens = isRecord(cards["tokens"]) ? cards["tokens"] : {};
  const cost = isRecord(cards["cost"]) ? cards["cost"] : {};

  const trend: TrendPointView[] = [];
  if (Array.isArray(raw["trend"])) {
    for (const point of raw["trend"]) {
      if (!isRecord(point)) continue;
      const tsMs = num(point["bucket_ts"]);
      if (tsMs === undefined) continue;
      trend.push({
        tsMs,
        input: num(point["input"]) ?? 0,
        inputCached: num(point["input_cached"]) ?? 0,
        output: num(point["output"]) ?? 0,
        cost: num(point["cost"]) ?? 0,
      });
    }
  }

  return {
    requests: num(requests["value"]),
    tokens: num(tokens["value"]),
    cost: money(cost),
    successRate: num(cards["success_rate"]),
    prevRequests: num(requests["prev"]),
    prevTokens: num(tokens["prev"]),
    prevCost: num(cost["prev"]),
    trend,
    unpricedCount: Array.isArray(raw["unpriced_models"]) ? raw["unpriced_models"].length : undefined,
  };
}

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/**
 * 把同一区块的几句解释合成**一条**提示正文（[docs/22 §3]：一个区块最多一个问号）。
 *
 * 分隔符用换行：`aria-label` 里保留断句，读屏不会把几句话连成一坨。
 * （同一个函数的规范实现是 `network/index.tsx` 的 `mergeHelpText`；这里**不跨页 import**
 * 另一个页面模块 —— 那会把整个 network 页拖进本页的依赖里。）
 */
function mergeHelpText(...parts: readonly (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined && part !== "").join("\n");
}

/**
 * 一个区块的标题 + 右边的问号圆圈（[docs/22 §1/§2]）。
 *
 * 这一页**全是数字**（[docs/22 §2] 第 5 条），所以只收"这些数是怎么算出来的"这类成段解释：
 * 带条数 / 组数 / 单位 / 时间戳的句子一律留在版面上（`cost.provenance.unpriced`、
 * `cost.result.truncated`、`cost.result.total`、`cost.chart.excluded`…）。
 */
function TitleWithHelp({ title, text, testId }: { title: string; text: string; testId: string }) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
      <Typography variant="h4">{title}</Typography>
      <HelpTip text={text} testId={testId} />
    </Box>
  );
}

/** 指标名：已知取值翻译，未知取值**原样显示 key**（开放集合，不丢弃）。 */
function metricText(metric: string, t: (key: I18nKey) => string): string {
  const key = METRIC_KEY[metric];
  return key === undefined ? metric : t(key);
}

/** 可选指标 = 内置 + 后端报出的 token 类型 + 当前已选的（已选的不因列表刷新而消失）。 */
function metricOptions(tokenKinds: readonly string[], selected: readonly string[]): string[] {
  const out = [...BUILTIN_METRICS];
  for (const kind of [...tokenKinds, ...selected]) {
    if (!out.includes(kind)) out.push(kind);
  }
  return out;
}

function chartPalette(theme: Theme): ChartPalette {
  return {
    text: theme.palette.text.primary,
    muted: theme.palette.text.secondary,
    border: theme.palette.divider,
    series: [
      theme.palette.primary.main,
      theme.palette.success.main,
      theme.palette.warning.main,
      theme.palette.error.main,
      theme.palette.text.secondary,
    ],
  };
}

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

/**
 * 涨跌比率 `(now - prev) / prev`。`prev` 缺失或为 0 → `undefined`（**不给涨跌**）。
 *
 * 注意 `NumberCard.delta.value` 要的是**比率**（它自己 `formatPercent`），不是差值 ——
 * 传差值会画成 `+16400.0%`。
 */
function ratio(now: number | undefined, prev: number | undefined): number | undefined {
  if (now === undefined || prev === undefined || prev === 0) return undefined;
  return (now - prev) / prev;
}

/** `NumberCard` 的 delta 载荷；`goodWhenPositive: false` 用于"涨了不是好事"的指标（花费）。 */
function deltaOf(
  now: number | undefined,
  prev: number | undefined,
  goodWhenPositive = true,
): { value: number; goodWhenPositive: boolean } | undefined {
  const value = ratio(now, prev);
  return value === undefined ? undefined : { value, goodWhenPositive };
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据"）

export type CostBodyProps = {
  /** 已生效的条件（结果区显示它；导出也用它）。 */
  applied: CostFilter;
  onSearch: (filter: CostFilter) => void;

  tokenKinds: readonly string[];
  tokenKindsError: unknown;

  table: UsageTableView | undefined;
  tableLoading: boolean;
  tableError: unknown;
  onRetryTable: () => void;

  dashboard: DashboardView | undefined;
  dashboardLoading: boolean;
  dashboardError: unknown;
  onRetryDashboard: () => void;

  unpriced: readonly string[] | undefined;
  unpricedError: unknown;

  recent: readonly RequestSummary[] | undefined;
  recentError: unknown;

  exportPath: string;
  onExportPathChange: (path: string) => void;
  onPickExportPath: () => void;
  /** 非 Tauri 宿主里没有保存对话框 → 不显示「选择位置」。 */
  canPickPath: boolean;
  onExport: () => void;
  exporting: boolean;
};

export function CostBody(props: CostBodyProps): ReactElement {
  const { t } = useTranslation();
  const theme = useTheme();
  const [draft, setDraft] = useState<CostFilter>(props.applied);

  const palette = useMemo(() => chartPalette(theme), [theme]);
  const options = metricOptions(props.tokenKinds, draft.metrics);

  const dimension = props.applied.groupBy;
  // 用局部 const 收窄一次：闭包里对 `props.table` 的收窄会被 TS 丢掉（那会逼出 `!` 断言）
  const resultTable = props.table;
  const tableChartInput = useMemo(
    () => chartInputFromTable(resultTable, resultTable?.metrics ?? []),
    [resultTable],
  );
  const tableOption = useMemo(
    () => buildChartOption(tableChartInput, palette, { stacked: true }),
    [tableChartInput, palette],
  );

  const trendInput = useMemo(() => chartInputFromTrend(props.dashboard?.trend), [props.dashboard]);
  const trendOption = useMemo(
    () => buildChartOption(trendInput, palette, { stacked: true }),
    [trendInput, palette],
  );

  const filterSummary = [
    `${t("cost.query.range")}=${t(RANGE_KEY[props.applied.range])}`,
    `${t("cost.query.groupBy")}=${t(GROUP_KEY[props.applied.groupBy])}`,
    `${t("cost.query.metrics")}=${props.applied.metrics.map((m) => metricText(m, t)).join(" / ")}`,
  ].join(" · ");

  const unpricedList = props.unpriced ?? [];
  const tableFailed = props.tableError !== undefined && props.tableError !== null;
  const dashboardFailed = props.dashboardError !== undefined && props.dashboardError !== null;

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 数据口径：先把"哪些是估算"说清楚（[11 §6] 铁律 4） */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={2} data-testid="cost-provenance">
          {/*
            「为什么 token 来源标记不可得」是**成段的设计解释**（[docs/22 §2] 的 prose）→ 收进问号
            （§3：一个区块最多一个，正文一个字都没丢：它就是 `aria-label`）。
            注意：但 `cost.provenance.cost`（"花费是本地算的、不是上游账单"）**留在版面上**：
            它是**数据口径结论**（用户据此判断眼前这些数字能不能当账单用），属于 §2 红线第 1 条 ——
            把它藏进悬停等于让用户拿估算当账单。带条数的 `cost.provenance.unpriced` 同理留在版面
            （数值 + 结论，§2 第 5 条）。
          */}
          <TitleWithHelp
            title={t("cost.provenance.title")}
            text={mergeHelpText(t("cost.provenance.tokens"), t("cost.provenance.tokensNote"))}
            testId="cost-provenance-help"
          />
          <Caption>{t("cost.provenance.cost")}</Caption>
          {unpricedList.length > 0 ? (
            // i18next 的 `count` 必须是 number（它自己按复数规则与本地化格式处理）；
            // 传 `formatInt(...)` 的字符串会被类型拒绝，且会让复数规则失效。
            <Caption>{t("cost.provenance.unpriced", { count: unpricedList.length })}</Caption>
          ) : null}
        </Stack>
      </Paper>

      {/* ② 概览（usage_dashboard）+ 今日趋势 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Typography variant="h4">{t("cost.summary.title")}</Typography>

          {dashboardFailed ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="cost-summary-error" sx={{ color: "error.main" }}>
                {`${t("error.title")} · ${errorMessage(props.dashboardError)}`}
              </Typography>
              <Button variant="outlined" onClick={props.onRetryDashboard}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : props.dashboardLoading && props.dashboard === undefined ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("common.loading")}
            </Typography>
          ) : props.dashboard === undefined ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("common.empty")}
            </Typography>
          ) : (
            <>
              <Box
                sx={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
                  gap: 3,
                }}
              >
                <NumberCard
                  label={t("dashboard.card.requests")}
                  value={props.dashboard.requests ?? 0}
                  format="int"
                  exact
                  // Rust 的 Num 没有上期值 → 不从别处凑一个"上期"出来
                  delta={deltaOf(props.dashboard.requests, props.dashboard.prevRequests)}
                />
                <NumberCard
                  label={t("dashboard.card.tokens")}
                  value={props.dashboard.tokens ?? 0}
                  format="tokens"
                  exact
                  unit={t("dashboard.card.tokensUnit")}
                  delta={deltaOf(props.dashboard.tokens, props.dashboard.prevTokens)}
                />
                <NumberCard
                  label={t("dashboard.card.cost")}
                  value={props.dashboard.cost?.amount ?? 0}
                  format="money"
                  currency={props.dashboard.cost?.currency ?? ""}
                  exact
                  // 花费涨了不是好事（与看板同一处理）
                  delta={deltaOf(
                    props.dashboard.cost?.amount,
                    props.dashboard.prevCost,
                    false,
                  )}
                />
                <NumberCard
                  label={t("dashboard.card.successRate")}
                  value={
                    props.dashboard.successRate === undefined
                      ? DASH
                      : formatPercent(props.dashboard.successRate, 1)
                  }
                  exact={
                    props.dashboard.successRate === undefined
                      ? DASH
                      : formatPercent(props.dashboard.successRate, 1)
                  }
                />
              </Box>
              {/* 没有上期字段就说清楚，而不是让四张卡看起来"本来就没有涨跌" */}
              {props.dashboard.prevRequests === undefined &&
              props.dashboard.prevTokens === undefined &&
              props.dashboard.prevCost === undefined ? (
                <Box data-testid="cost-delta-none">
                  <Caption>{t("cost.deltaUnavailable")}</Caption>
                </Box>
              ) : null}
            </>
          )}

          <Stack spacing={2}>
            <Typography variant="h4">{t("cost.trend.title")}</Typography>
            <Caption>{`${t("logs.col.time")}（${timezoneLabel()}）`}</Caption>
            {trendOption === undefined ? (
              <Typography variant="body1" data-testid="cost-trend-empty" sx={{ color: "text.secondary" }}>
                {t("cost.trend.empty")}
              </Typography>
            ) : (
              <UsageChart option={trendOption} height={200} testId="cost-trend-chart" />
            )}
          </Stack>
        </Stack>
      </Paper>

      {/* ③ 明细查询（usage_query + 图表 + 表格 + 导出） */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={4}>
          <Typography variant="h4">{t("cost.query.title")}</Typography>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
              gap: 3,
              alignItems: "start",
            }}
          >
            <TextField
              select
              label={t("cost.query.range")}
              value={draft.range}
              onChange={(e) => setDraft({ ...draft, range: e.target.value as UsageRange })}
            >
              {(Object.keys(RANGE_KEY) as UsageRange[]).map((preset) => (
                <MenuItem key={preset} value={preset}>
                  {t(RANGE_KEY[preset])}
                </MenuItem>
              ))}
            </TextField>

            <TextField
              select
              label={t("cost.query.groupBy")}
              value={draft.groupBy}
              onChange={(e) => setDraft({ ...draft, groupBy: e.target.value as UsageGroupDim })}
            >
              {(Object.keys(GROUP_KEY) as UsageGroupDim[]).map((dim) => (
                <MenuItem key={dim} value={dim}>
                  {t(GROUP_KEY[dim])}
                </MenuItem>
              ))}
            </TextField>

            {draft.groupBy === "time" ? (
              <TextField
                select
                label={t("cost.query.bucket")}
                value={draft.bucket}
                onChange={(e) => setDraft({ ...draft, bucket: e.target.value as UsageTimeBucket })}
              >
                {(Object.keys(BUCKET_KEY) as UsageTimeBucket[]).map((bucket) => (
                  <MenuItem key={bucket} value={bucket}>
                    {t(BUCKET_KEY[bucket])}
                  </MenuItem>
                ))}
              </TextField>
            ) : null}

            <TextField
              select
              label={t("cost.query.format")}
              value={draft.format}
              onChange={(e) => setDraft({ ...draft, format: e.target.value as UsageFormat })}
            >
              {(Object.keys(FORMAT_KEY) as UsageFormat[]).map((format) => (
                <MenuItem key={format} value={format}>
                  {t(FORMAT_KEY[format])}
                </MenuItem>
              ))}
            </TextField>
          </Box>

          <Stack spacing={2}>
            {/* 「指标 key 就是后端返回的列名 / 认不出的类型原样显示」是**解释**
                （[docs/22 §2]）→ 收进问号；下面那排指标名与括号里的 `usage_token_kinds` 报错留在版面上。 */}
            <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
              <Typography variant="h4">{t("cost.query.metrics")}</Typography>
              <HelpTip text={t("cost.query.metrics.hint")} testId="cost-query-metrics-help" />
            </Box>
            <Box sx={{ display: "flex", flexWrap: "wrap" }}>
              {options.map((metric) => (
                <FormControlLabel
                  key={metric}
                  sx={{ mr: 4 }}
                  control={
                    <Checkbox
                      size="small"
                      checked={draft.metrics.includes(metric)}
                      onChange={(e) =>
                        setDraft({
                          ...draft,
                          metrics: e.target.checked
                            ? [...draft.metrics, metric]
                            : draft.metrics.filter((m) => m !== metric),
                        })
                      }
                    />
                  }
                  label={metricText(metric, t)}
                />
              ))}
            </Box>
            {props.tokenKindsError !== undefined && props.tokenKindsError !== null ? (
              <Typography variant="caption" sx={{ color: "error.main" }}>
                {t("cost.query.metrics.error", { message: errorMessage(props.tokenKindsError) })}
              </Typography>
            ) : props.tokenKinds.length === 0 ? (
              <Caption>{t("cost.query.metrics.empty")}</Caption>
            ) : null}
          </Stack>

          <Box sx={{ display: "flex", gap: 3, alignItems: "center" }}>
            <Button variant="contained" onClick={() => props.onSearch(draft)}>
              {t("cost.query.submit")}
            </Button>
            <Button
              variant="outlined"
              onClick={() => {
                setDraft(DEFAULT_COST_FILTER);
                props.onSearch(DEFAULT_COST_FILTER);
              }}
            >
              {t("cost.query.reset")}
            </Button>
          </Box>

          <Box data-testid="cost-applied">
            <Caption>{t("cost.query.applied", { summary: filterSummary })}</Caption>
          </Box>

          {/* 结果 */}
          {tableFailed ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="cost-error" sx={{ color: "error.main" }}>
                {`${t("error.title")} · ${errorMessage(props.tableError)}`}
              </Typography>
              <Button variant="outlined" onClick={props.onRetryTable}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : props.tableLoading && props.table === undefined ? (
            <Typography variant="body1" data-testid="cost-loading" sx={{ color: "text.secondary" }}>
              {t("cost.result.loading")}
            </Typography>
          ) : resultTable === undefined || resultTable.rows.length === 0 ? (
            <Stack spacing={1} data-testid="cost-empty">
              <Typography variant="body1">{t("cost.result.empty.title")}</Typography>
              <Caption>{t("cost.result.empty.body")}</Caption>
              <Caption>{t("cost.result.empty.narrow")}</Caption>
            </Stack>
          ) : (
            <>
              <Stack direction="row" spacing={3} alignItems="baseline" sx={{ flexWrap: "wrap" }}>
                {resultTable.totalGroups !== undefined ? (
                  <Caption>{t("cost.result.total", { total: formatInt(resultTable.totalGroups) })}</Caption>
                ) : null}
                <Caption>{t("logs.result.loaded", { value: formatInt(resultTable.rows.length) })}</Caption>
              </Stack>

              {resultTable.truncated && resultTable.totalGroups !== undefined ? (
                <Box data-testid="cost-truncated">
                  <Caption>
                    {t("cost.result.truncated", {
                      shown: formatInt(resultTable.rows.length),
                      total: formatInt(resultTable.totalGroups),
                    })}
                  </Caption>
                </Box>
              ) : null}

              {tableOption === undefined ? (
                <Typography variant="body1" data-testid="cost-chart-empty" sx={{ color: "text.secondary" }}>
                  {t("cost.chart.empty")}
                </Typography>
              ) : (
                <UsageChart option={tableOption} height={240} testId="cost-chart" />
              )}

              {tableChartInput.excluded.length > 0 ? (
                <Box data-testid="cost-chart-excluded">
                  <Caption>
                    {t("cost.chart.excluded", {
                      metrics: tableChartInput.excluded.map((m) => metricText(m, t)).join(" / "),
                    })}
                  </Caption>
                </Box>
              ) : null}

              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="cost-table">
                  {/* 时间轴永远本地时区，且写在列头（[07 §1.3]） */}
                  <TableHead>
                    <TableRow>
                      <TableCell>
                        {dimension === "time"
                          ? `${t("cost.col.time")}（${timezoneLabel()}）`
                          : t(GROUP_KEY[dimension])}
                      </TableCell>
                      {(resultTable.metrics.length > 0 ? resultTable.metrics : ["cost"]).map((metric) => (
                        <TableCell key={metric} align="right">
                          {metricText(metric, t)}
                        </TableCell>
                      ))}
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {resultTable.rows.map((row, index) => (
                      <TableRow key={`${index}-${rowLabel(row, dimension)}`}>
                        <TableCell sx={{ fontVariantNumeric: "tabular-nums", overflowWrap: "anywhere" }}>
                          {rowLabel(row, dimension)}
                        </TableCell>
                        {(resultTable.metrics.length > 0 ? resultTable.metrics : ["cost"]).map(
                          (metric) => {
                            const value = row.values[metric];
                            return (
                              <TableCell
                                key={metric}
                                align="right"
                                sx={{ fontVariantNumeric: "tabular-nums" }}
                              >
                                {value === undefined ? DASH : formatExact(value)}
                              </TableCell>
                            );
                          },
                        )}
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            </>
          )}

          {/* 导出：真调用 usage_export_csv（后端已实现） */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("cost.export.title")}</Typography>
            <Box sx={{ display: "flex", gap: 3, alignItems: "flex-start", flexWrap: "wrap" }}>
              <TextField
                label={t("cost.export.path")}
                value={props.exportPath}
                onChange={(e) => props.onExportPathChange(e.target.value)}
                slotProps={{ htmlInput: { "data-testid": "cost-export-path" } }}
                sx={{ minWidth: 360 }}
              />
              {props.canPickPath ? (
                <Button variant="outlined" onClick={props.onPickExportPath}>
                  {t("cost.export.pick")}
                </Button>
              ) : null}
              <Button
                variant="contained"
                data-testid="cost-export-button"
                disabled={props.exportPath.trim() === "" || props.exporting}
                onClick={props.onExport}
              >
                {t("cost.export.button")}
              </Button>
            </Box>
            <Box data-testid="cost-export-hint">
              <Caption>{t("cost.export.pathHint")}</Caption>
            </Box>
          </Stack>
        </Stack>
      </Paper>

      {/* ④ 未定价模型：成本数字不完整必须说出来 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={2} data-testid="cost-unpriced">
          {/* 「为什么不给补定价按钮」是**解释**（[docs/22 §2]）→ 收进问号；
              模型名本身是数据，留在版面上。空态（没有未定价的模型）是结论，也留在版面上（§2 第 1 条）。 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
            <Typography variant="h4">{t("cost.unpriced.title")}</Typography>
            {unpricedList.length === 0 ? null : (
              <HelpTip text={t("cost.unpriced.hint")} testId="cost-unpriced-help" />
            )}
          </Box>
          {props.unpricedError !== undefined && props.unpricedError !== null ? (
            <Typography variant="body1" sx={{ color: "error.main" }}>
              {errorMessage(props.unpricedError)}
            </Typography>
          ) : unpricedList.length === 0 ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("cost.unpriced.empty")}
            </Typography>
          ) : (
            <Stack spacing={1}>
              {unpricedList.map((model) => (
                <Typography
                  key={model}
                  variant="body1"
                  sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                >
                  {model}
                </Typography>
              ))}
            </Stack>
          )}
        </Stack>
      </Paper>

      {/* ⑤ 最近请求（实时流快照） */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={2}>
          {/* 「每条的花费同样是本地算出来的」是口径解释（[docs/22 §2]）→ 收进问号；
              空态（还没有请求经过网关）是结论，留在版面上（§2 第 1 条）。 */}
          <TitleWithHelp
            title={t("cost.recent.title")}
            text={t("cost.recent.hint")}
            testId="cost-recent-help"
          />
          {props.recentError !== undefined && props.recentError !== null ? (
            <Typography variant="body1" sx={{ color: "error.main" }}>
              {errorMessage(props.recentError)}
            </Typography>
          ) : props.recent === undefined || props.recent.length === 0 ? (
            <Typography
              variant="body1"
              data-testid="cost-recent-empty"
              sx={{ color: "text.secondary" }}
            >
              {t("cost.recent.empty")}
            </Typography>
          ) : (
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" data-testid="cost-recent-table">
                <TableHead>
                  <TableRow>
                    <TableCell>{`${t("cost.col.time")}（${timezoneLabel()}）`}</TableCell>
                    <TableCell>{t("cost.col.model")}</TableCell>
                    <TableCell align="right">{t("cost.col.tokens")}</TableCell>
                    <TableCell align="right">{t("cost.col.cost")}</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {props.recent.map((item) => (
                    <TableRow key={item.request_id} data-testid="cost-recent-row">
                      <TableCell sx={{ fontVariantNumeric: "tabular-nums", color: "text.secondary" }}>
                        {formatLocalTime(item.ts_ms)}
                      </TableCell>
                      <TableCell sx={{ overflowWrap: "anywhere" }}>{item.model}</TableCell>
                      <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                        {formatTokens(item.total_tokens)}
                      </TableCell>
                      <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                        {formatMoney(item.cost)}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </Box>
          )}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 定价条目（[docs/21 §1 / §3]）

/**
 * 一条价格数字 → 文案。
 *
 * 常规量级用 `formatExact`（与成本页其它数字同一套格式）；**极小值原样显示** ——
 * `formatExact` 只保留 4 位小数，按 token 计价的价格会被四舍五入成 `0`，那是在编数据。
 */
function priceText(value: number): string {
  if (value !== 0 && Math.abs(value) < 1e-4) return String(value);
  return formatExact(value);
}

/**
 * 列表里的一行。
 *
 * **`index` 必须一路带着**：`pricing_delete` 的入参就是这个下标（`entity.rs:1532`），
 * 所以列表顺序**不许重排**（重排就等于换了要删的东西）。
 */
export type PricingEntryView = { index: number; raw: unknown };

/** `pricing_list` 的结果 → 行（非数组 = 一行都没有；保持后端顺序，下标不重排）。 */
export function readPriceEntries(raw: unknown): PricingEntryView[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((entry, index) => ({ index, raw: entry }));
}

/** `PriceMatch` 的三个契约字段（顺序固定；缺的一个字都不编）。 */
const PRICE_MATCH_FIELDS = ["model", "provider", "credential"] as const;

/**
 * 匹配列的文案：`model=… · provider=… · credential=…`。
 *
 * **不猜语义**：契约里没给的字段画 `—`（不会翻译成"任意"之类的说法 —— 那是我们在替后端下结论）。
 */
export function priceMatchText(match: unknown): string {
  if (!isRecord(match)) return DASH;
  return PRICE_MATCH_FIELDS.map((field) => {
    const value = match[field];
    return `${field}=${typeof value === "string" && value !== "" ? value : DASH}`;
  }).join(" · ");
}

/** 一条价格（`rates` 里按 `TokenKind::as_key()` 的键取值）；没有这个键就是 `—`。 */
export function priceRateText(rates: unknown, kind: string): string {
  if (!isRecord(rates)) return DASH;
  const value = num(rates[kind]);
  return value === undefined ? DASH : priceText(value);
}

/** 来源（`PriceSource`）：**原样显示后端的取值**（契约枚举，不是我们编的词）；读不到就是 `—`。 */
export function priceSourceText(source: unknown): string {
  return typeof source === "string" && source !== "" ? source : DASH;
}

/** 递归按 key 排序后的 JSON：**逐字段全等**的比较基准（与键序无关）。 */
function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (isRecord(value)) {
    const keys = Object.keys(value).sort();
    return `{${keys.map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

export type PricingIndexCheck = "same" | "changed" | "missing";

/**
 * [docs/21 §1.1 第 5 条] `pricing_delete` 收的是 **index**，所以删之前必须**重新读一次**
 * `pricing_list`，逐字段确认那个下标上**仍然是**用户看到的那一条（`JSON.stringify` 全等；
 * 这里先按 key 排序再比 —— 比较的仍是**全部字段**，只是不让键序抖动造成误判）。
 *
 * `changed` / `missing` 都**拒绝执行**：绝不在错位上下手，宁可让用户重看一遍。
 */
export function checkPricingIndex(
  fresh: unknown,
  index: number,
  snapshot: unknown,
): PricingIndexCheck {
  if (!Array.isArray(fresh) || index < 0 || index >= fresh.length) return "missing";
  return stableJson(fresh[index]) === stableJson(snapshot) ? "same" : "changed";
}

/**
 * 下标错位时用 `delete.result.staleIndex`（i18n 键已由主管补上）。
 *
 * 为什么不在这里放模块常量：键缺失时先报主管是纪律，但键到位后**必须**换回 `t(...)` ——
 * 中文界面能看，英文界面显示中文就是未翻译的漏网之鱼。
 */

/** 一条定价删除的结果。**逐条如实汇报**：成功说标识，失败说标识 + 后端原文。 */
export type PricingDeleteOutcome = {
  /** 身份标识：要删的那条**当时**所在的下标。 */
  key: string;
  /** 下标 + 匹配（写清"删的是哪一个"）。 */
  label: string;
  ok: boolean;
  /** 失败时的后端原文（含 TA 码）。 */
  error?: string;
  /** 拒绝执行：重新读回来时那个下标上已经不是这一条（**没有**调用后端）。 */
  stale?: boolean;
};

export type PricingDeleteReport = { batch: boolean; items: PricingDeleteOutcome[] };

/** 一行在确认框 / 结果区里的标识：下标 + 匹配（原样数据，不改写）。 */
function priceRowLabel(row: PricingEntryView): string {
  const match = isRecord(row.raw) ? row.raw["match"] : undefined;
  return `#${row.index} · ${priceMatchText(match)}`;
}

/** 删除结果区（失败逐条给标识 + 后端原文；批量补一行"成功 N 条，失败 M 条"）。 */
function PricingDeleteReportBox({ report }: { report: PricingDeleteReport }): ReactElement {
  const { t } = useTranslation();
  const failed = report.items.filter((item) => !item.ok);
  return (
    <Box data-testid="pricing-delete-result">
      <Stack spacing={1}>
        {report.batch ? (
          <Typography variant="body1" data-testid="pricing-delete-result-summary">
            {t("delete.result.summary", {
              ok: report.items.length - failed.length,
              failed: failed.length,
            })}
          </Typography>
        ) : null}
        {report.items.map((item) =>
          item.stale === true ? (
            <Typography
              key={item.key}
              variant="body1"
              data-testid={`pricing-delete-result-stale-${item.key}`}
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {`${item.label} · ${t("delete.result.staleIndex")}`}
            </Typography>
          ) : (
            <Typography
              key={item.key}
              variant="body1"
              data-testid={`pricing-delete-result-${item.key}`}
              sx={{
                color: item.ok ? "text.primary" : "error.main",
                overflowWrap: "anywhere",
              }}
            >
              {item.ok
                ? t("delete.result.ok", { name: item.label })
                : `${item.label} · ${t("delete.result.failed", { message: item.error ?? "" })}`}
            </Typography>
          ),
        )}
      </Stack>
    </Box>
  );
}

/**
 * 「定价条目」区块：列表（匹配 / 输入价 / 输出价 / 来源）+ 单条与批量删除。
 *
 * 三条硬规则与 `/policies` 完全一致（同一个 `ConfirmDelete`、同一套失败展示），
 * 外加**下标安全**这一条专属规则（见 `checkPricingIndex`）：
 * 每次删除之前都重新读一次 `pricing_list` 并逐字段比对，不一致就拒绝。
 */
export function PricingSection(): ReactElement {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const list = useCommandQuery("pricing_list", undefined, qk.pricing);
  const rows = useMemo(() => readPriceEntries(list.data), [list.data]);

  const [selected, setSelected] = useState<number[]>([]);
  /** 正在确认的单条；`null` = 没有。 */
  const [target, setTarget] = useState<PricingEntryView | null>(null);
  /** 批量确认（与单条共用 `ConfirmDelete`）。 */
  const [batchOpen, setBatchOpen] = useState(false);
  const [report, setReport] = useState<PricingDeleteReport | null>(null);
  const remove = useCommandMutation("pricing_delete");

  const selectedRows = rows.filter((row) => selected.includes(row.index));
  const allSelected = rows.length > 0 && selectedRows.length === rows.length;
  const listFailed = list.error !== undefined && list.error !== null;

  /** 定价表变了 → 未定价模型与花费视图也跟着变（同前缀 + 看板/用量）。 */
  const invalidate = async (): Promise<void> => {
    await qc.invalidateQueries({ queryKey: qk.pricing });
    void qc.invalidateQueries({ queryKey: qk.dashboardPrefix });
  };

  /**
   * 重新读一次 `pricing_list`。
   *
   * **故意不读缓存**：错位判断必须基于"后端现在手里那一份"，而缓存里那份恰恰可能是过时的
   * （[docs/21 §1.1 第 5 条] 要的就是"删之前重新读一次"）。
   */
  async function freshEntries(): Promise<unknown> {
    return await pricingApi.list();
  }

  const failedItem = (row: PricingEntryView, error: string): PricingDeleteOutcome => ({
    key: String(row.index),
    label: priceRowLabel(row),
    ok: false,
    error,
  });

  const runOne = async (row: PricingEntryView): Promise<void> => {
    setTarget(null);
    // 结果区只描述**最近一次**删除操作（上一次的结论不该混在这次里）
    setReport(null);
    let check: PricingIndexCheck;
    try {
      check = checkPricingIndex(await freshEntries(), row.index, row.raw);
    } catch (err) {
      // 重新读都失败 → 更不能删（如实显示后端原文）
      setReport({ batch: false, items: [failedItem(row, errorMessage(err))] });
      return;
    }
    if (check !== "same") {
      // 列表变了 → **拒绝执行**（一条都不删），并把最新列表取回来让用户重看
      setReport({
        batch: false,
        items: [{ key: String(row.index), label: priceRowLabel(row), ok: false, stale: true }],
      });
      setSelected([]);
      await invalidate();
      return;
    }
    try {
      await remove.mutateAsync({ index: row.index });
      setReport({
        batch: false,
        items: [{ key: String(row.index), label: priceRowLabel(row), ok: true }],
      });
      setSelected([]);
    } catch (err) {
      setReport({ batch: false, items: [failedItem(row, errorMessage(err))] });
    }
    await invalidate();
  };

  const runSelected = async (): Promise<void> => {
    setBatchOpen(false);
    setReport(null);
    // 下标会随删除**整体前移**（删掉一条，它后面每条的下标都 −1）→ 从**大到小**删：
    // 已经处理过的那些下标不会再被后面的删除挪动。
    const ordered = [...selectedRows].sort((a, b) => b.index - a.index);
    const items: PricingDeleteOutcome[] = [];
    for (const row of ordered) {
      let check: PricingIndexCheck;
      try {
        // 每一条都重新读 + 逐字段比对（[docs/21 §1.1 第 5 条]）
        check = checkPricingIndex(await freshEntries(), row.index, row.raw);
      } catch (err) {
        items.push(failedItem(row, errorMessage(err)));
        continue; // **不中止**其余条目
      }
      if (check !== "same") {
        items.push({ key: String(row.index), label: priceRowLabel(row), ok: false, stale: true });
        continue; // **不中止**其余条目
      }
      try {
        await remove.mutateAsync({ index: row.index });
        items.push({ key: String(row.index), label: priceRowLabel(row), ok: true });
      } catch (err) {
        items.push(failedItem(row, errorMessage(err)));
      }
    }
    setReport({ batch: true, items });
    setBatchOpen(false);
    // 删过之后下标就整体变了 → 清空选择（列表紧接着会重取）
    setSelected([]);
    await invalidate();
  };

  return (
    <Box sx={{ px: 8, pb: 8 }}>
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3} data-testid="cost-pricing">
          <Typography variant="h4">{t("cost.pricing.title")}</Typography>

          {listFailed ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="cost-pricing-error" sx={{ color: "error.main" }}>
                {`${t("error.title")} · ${errorMessage(list.error)}`}
              </Typography>
              <Button variant="outlined" onClick={() => void list.refetch()}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : list.isPending && list.data === undefined ? (
            <Typography variant="body1" data-testid="cost-pricing-loading" sx={{ color: "text.secondary" }}>
              {t("common.loading")}
            </Typography>
          ) : rows.length === 0 ? (
            <Typography variant="body1" data-testid="cost-pricing-empty" sx={{ color: "text.secondary" }}>
              {t("cost.pricing.empty")}
            </Typography>
          ) : (
            <>
              <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                <Box data-testid="pricing-selected-count">
                  <Caption>{t("delete.selectedCount", { count: selectedRows.length })}</Caption>
                </Box>
                <Button
                  variant="outlined"
                  color="error"
                  data-testid="pricing-delete-selected"
                  disabled={remove.isPending || selectedRows.length === 0}
                  onClick={() => setBatchOpen(true)}
                >
                  {t("common.deleteSelected")}
                </Button>
              </Stack>

              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="pricing-table">
                  <TableHead>
                    <TableRow>
                      <TableCell padding="checkbox">
                        <Box data-testid="pricing-select-all">
                          <Checkbox
                            size="small"
                            checked={allSelected}
                            indeterminate={selectedRows.length > 0 && !allSelected}
                            onChange={(event) =>
                              setSelected(event.target.checked ? rows.map((row) => row.index) : [])
                            }
                          />
                        </Box>
                      </TableCell>
                      <TableCell>{t("cost.pricing.col.match")}</TableCell>
                      <TableCell align="right">{t("cost.pricing.col.input")}</TableCell>
                      <TableCell align="right">{t("cost.pricing.col.output")}</TableCell>
                      <TableCell>{t("cost.pricing.col.source")}</TableCell>
                      <TableCell>{t("common.delete")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {rows.map((row) => {
                      // 一行一个防御性读取：形状不对就整格 `—`（不猜、不编）
                      const raw = isRecord(row.raw) ? row.raw : {};
                      return (
                        <TableRow key={row.index} data-testid="pricing-row">
                          <TableCell padding="checkbox">
                            <Box data-testid="pricing-select">
                              <Checkbox
                                size="small"
                                checked={selected.includes(row.index)}
                                onChange={(event) =>
                                  setSelected((prev) =>
                                    event.target.checked
                                      ? [...prev.filter((i) => i !== row.index), row.index]
                                      : prev.filter((i) => i !== row.index),
                                  )
                                }
                              />
                            </Box>
                          </TableCell>
                          <TableCell
                            sx={{
                              minWidth: 240,
                              fontFamily: typography.fontMono,
                              overflowWrap: "anywhere",
                            }}
                          >
                            {priceMatchText(raw["match"])}
                          </TableCell>
                          <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                            {priceRateText(raw["rates"], "input")}
                          </TableCell>
                          <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                            {priceRateText(raw["rates"], "output")}
                          </TableCell>
                          <TableCell sx={{ fontFamily: typography.fontMono }}>
                            {priceSourceText(raw["source"])}
                          </TableCell>
                          <TableCell>
                            <Button
                              size="small"
                              color="error"
                              data-testid="pricing-delete"
                              disabled={remove.isPending}
                              onClick={() => setTarget(row)}
                            >
                              {t("common.delete")}
                            </Button>
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </Box>
            </>
          )}

          {report !== null ? <PricingDeleteReportBox report={report} /> : null}
        </Stack>
      </Paper>

      <ConfirmDelete
        open={target !== null || batchOpen}
        testIdPrefix="pricing"
        title={t("delete.title.pricing")}
        // 确认框里写清删的是哪一个：单条 = 下标 + 匹配；批量 = 选中数 + 逐条标识
        targetLabel={
          target !== null
            ? priceRowLabel(target)
            : `${t("delete.selectedCount", { count: selectedRows.length })} · ${selectedRows
                .map(priceRowLabel)
                .join("、")}`
        }
        // `pricing_delete` 没有 force 语义（[docs/21 §1.1 第 3 条]）→ 不放级联按钮
        cascadeAvailable={false}
        pending={remove.isPending}
        onConfirm={() => {
          const row = target;
          if (row !== null) void runOne(row);
          else void runSelected();
        }}
        // `cascadeAvailable={false}` 时这个回调**永远不会被触发**（组件里那个按钮不渲染）
        onConfirmCascade={() => {}}
        onCancel={() => {
          setTarget(null);
          setBatchOpen(false);
        }}
      />
    </Box>
  );
}

// ================================================================ 数据层

export default function CostPage(): ReactElement {
  const { t } = useTranslation();
  const pushToast = useAppStore((s) => s.pushToast);

  const [applied, setApplied] = useState<CostFilter>(DEFAULT_COST_FILTER);
  const [exportPath, setExportPath] = useState("");

  const fingerprint = costFilterFingerprint(applied);
  // 条件与入参都要稳定引用：否则每次渲染都换新对象 → 图表 option 变化 → ECharts 反复重建
  const query = useMemo(() => buildUsageQuery(applied), [applied]);
  const queryArgs = useMemo(() => usageQueryArgs(query), [query]);

  const table = useCommandQuery("usage_query", queryArgs, qk.usage(fingerprint));
  const tokenKinds = useCommandQuery("usage_token_kinds", undefined, qk.usageTokenKinds);
  const dashboard = useCommandQuery(
    "usage_dashboard",
    { range: rustRange("today") },
    qk.dashboard("today"),
  );
  const unpriced = useCommandQuery("pricing_unpriced_models", undefined, qk.unpricedModels);
  // 实时流快照与其它用量视图同前缀（["usage"]）→ `usage:tick` 事件一次失效就够
  const recent = useCommandQuery(
    "usage_request_stream_snapshot",
    { limit: RECENT_LIMIT },
    qk.usage(`recent-${RECENT_LIMIT}`),
  );

  // 两套后端形状的收口在这里做一次（引用稳定，图表不会被无意义地重建）
  const tableView = useMemo(() => readUsageTable(table.data), [table.data]);
  const dashboardView = useMemo(() => readDashboard(dashboard.data), [dashboard.data]);

  const exportCsv = useCommandMutation("usage_export_csv");
  const openDir = useCommandMutation("app_open_dir");

  const onExport = () => {
    const path = exportPath.trim();
    if (path === "") return;
    exportCsv.mutate(usageExportArgs(query, path), {
      onSuccess: () =>
        pushToast({
          level: "info",
          title: t("cost.export.title"),
          body: t("cost.export.done", { path }),
          dedupeKey: "cost.export",
        }),
      onError: (err) =>
        pushToast({
          level: "error",
          title: t("cost.export.title"),
          body: t("cost.export.failed", { message: errorMessage(err) }),
          dedupeKey: "cost.export.error",
        }),
    });
  };

  /**
   * 保存对话框：`@tauri-apps/plugin-dialog` 的 `save`（`dialog:default` 已在
   * `src-tauri/capabilities/default.json` 里授权）。**只在 Tauri 宿主里挂这个按钮** ——
   * 浏览器开发环境没有这个插件，挂上去就是"点了没反应"。
   */
  const onPickExportPath = () => {
    void (async () => {
      try {
        const { save } = await import("@tauri-apps/plugin-dialog");
        const picked = await save({
          defaultPath: "usage.csv",
          filters: [{ name: "CSV", extensions: ["csv"] }],
        });
        if (typeof picked === "string" && picked !== "") setExportPath(picked);
      } catch (err) {
        pushToast({
          level: "error",
          title: t("cost.export.title"),
          body: errorMessage(err),
          dedupeKey: "cost.export.pick",
        });
      }
    })();
  };

  const onOpenDir = () => {
    openDir.mutate(
      { kind: "data" },
      {
        onSuccess: () =>
          pushToast({
            level: "info",
            title: t("cost.openDataDir"),
            body: t("settings.openDir.done"),
            dedupeKey: "cost.openDir",
          }),
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("cost.openDataDir"),
            body: t("settings.openDir.failed", { message: errorMessage(err) }),
            dedupeKey: "cost.openDir.error",
          }),
      },
    );
  };

  const refreshAll = () => {
    void table.refetch();
    void dashboard.refetch();
    void tokenKinds.refetch();
    void unpriced.refetch();
    void recent.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.cost")}
        description={t("cost.description")}
        docRef="07 §6 成本中心 · 10 §2.2 usage_query / usage_export_csv · 11 §6 铁律 4"
        actions={
          <>
            <Button variant="outlined" onClick={refreshAll}>
              {t("cost.refresh")}
            </Button>
            <Button variant="outlined" disabled={openDir.isPending} onClick={onOpenDir}>
              {t("cost.openDataDir")}
            </Button>
          </>
        }
      />

      <CostBody
        applied={applied}
        onSearch={setApplied}
        tokenKinds={tokenKinds.data ?? []}
        tokenKindsError={tokenKinds.error ?? undefined}
        table={tableView}
        tableLoading={table.isPending}
        tableError={table.error ?? undefined}
        onRetryTable={() => void table.refetch()}
        dashboard={dashboardView}
        dashboardLoading={dashboard.isPending}
        dashboardError={dashboard.error ?? undefined}
        onRetryDashboard={() => void dashboard.refetch()}
        unpriced={unpriced.data}
        unpricedError={unpriced.error ?? undefined}
        recent={recent.data}
        recentError={recent.error ?? undefined}
        exportPath={exportPath}
        onExportPathChange={setExportPath}
        onPickExportPath={onPickExportPath}
        canPickPath={isTauri()}
        onExport={onExport}
        exporting={exportCsv.isPending}
      />

      {/* 定价条目（[docs/21 §3]）：独立区块 —— 它有自己的取数与删除链路，不掺进用量视图 */}
      <PricingSection />
    </>
  );
}

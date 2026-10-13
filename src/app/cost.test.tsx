// @vitest-environment jsdom
/**
 * 成本中心 `/cost`（[07 §6]）的验收。三条硬要求各有一组断言：
 *
 * 1. **估算不许冒充上游上报**：`UsageSource`（upstream / upstream_partial / estimated）只写在
 *    写入路径的 `UsageRecord` 上（`crates/ta-core/src/types.rs:282`），而**查询与看板 DTO 都没带它**
 *    （`UsageRow` 只有 `dims`/`values`，`DashboardData` 没有 `estimated_requests` —— 见 [16 §8] 的推迟决定）。
 *    所以页面必须**明说"来源标记不可得"**，而不是把数字标成上游数据；花费更是**本地定价表算出来的**，
 *    一律按本地口径标注。
 * 2. **图表真用 ECharts 且只画后端给的数**：option 里的 x 轴标签就是后端给的 bucket 标签、
 *    系列值就是后端的数值，**不插点、不补 0、不造趋势**；没有数据时给空状态而不是空坐标系。
 * 3. **两套字段名都读得懂**：冻结契约（`bindings.ts` 的 `columns`/`metrics`/`total`）与
 *    Rust（`ta_store::UsageTable` 的 `metrics`/`values`/`total_groups`）字段名不一致，
 *    两种夹具都要能读出同一张表。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

// ECharts 要在 canvas 上量文字，jsdom 没有 canvas（`getContext("2d")` 是 null）。
// 这里只替换 `init`：**断言的重点是"我们把什么 option 交给了 ECharts"**，那才是页面自己的责任。
const echartsMock = vi.hoisted(() => {
  const setOption = vi.fn();
  const resize = vi.fn();
  const dispose = vi.fn();
  const init = vi.fn(() => ({ setOption, resize, dispose }));
  return { init, setOption, resize, dispose };
});

vi.mock("echarts", () => ({ init: echartsMock.init }));

import CostPage, {
  CostBody,
  buildChartOption,
  buildUsageQuery,
  chartInputFromTable,
  costFilterFingerprint,
  readDashboard,
  readPriceEntries,
  readUsageTable,
  priceMatchText,
  priceRateText,
  priceSourceText,
  usageExportArgs,
  usageQueryArgs,
  DEFAULT_COST_FILTER,
  type CostBodyProps,
} from "@/pages/cost";
// 价格配置已搬到模型页，所以定价的整形逻辑也搬到了 `@/shared/pricing`
// （它不再属于任何一页 —— 成本中心只显示、模型页只写）。
import {
  draftFromEntry,
  hasPrice,
  invalidPriceFields,
  parsePrice,
  priceEntryFor,
  priceRates,
  type PricingDraft,
} from "@/shared/pricing";
// `registerMock` 由 `@/shared/api`（门面）导出，不是 `@/shared/api/mock` ——
// 其它四个测试文件（channels/models/settings/providers）都是从门面导入的。
import { registerBaselineMocks, registerMock, TokenAgencyError } from "@/shared/api";
import type { PriceEntry, RequestSummary, UsageQuery } from "@/shared/bindings";
import { i18n, type I18nKey } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
  echartsMock.init.mockClear();
  echartsMock.setOption.mockClear();
});

// ================================================================ 夹具（两种后端形状）

/** 冻结契约 / 浏览器 mock 的形状（`bindings.ts:841-845`：`columns` + `rows[].metrics`）。 */
function contractTable() {
  return {
    columns: ["model", "requests", "input", "input_cached", "output", "cost"],
    rows: [
      {
        dims: { model: "deepseek-v4.1flash" },
        metrics: { requests: 1_284, input: 6_400_000, input_cached: 5_200_000, output: 800_000, cost: 16.9 },
      },
      {
        dims: { model: "claude-sonnet-4-5" },
        metrics: { requests: 42, input: 90_000, input_cached: 12_000, output: 30_000, cost: 1.42 },
      },
    ],
    total: 2,
  };
}

/** Rust `ta_store::UsageTable` 的形状（`usage_store.rs:252-265`：`metrics` + `rows[].values`）。 */
function rustTable() {
  return {
    format: "stacked_bar",
    time_bucket: "day",
    metrics: ["input", "input_cached", "output", "cost"],
    group_by: ["time"],
    rows: [
      {
        bucket_start_ms: 1_770_000_000_000,
        bucket: "2026-02-11",
        dims: {},
        values: { input: 320_000, input_cached: 180_000, output: 60_000, cost: 0.5 },
      },
      {
        bucket_start_ms: 1_770_086_400_000,
        bucket: "2026-02-12",
        dims: {},
        values: { input: 400_000, input_cached: 210_000, output: 68_000, cost: 0.62 },
      },
    ],
    total_groups: 5,
    truncated: true,
  };
}

function recent(overrides: Partial<RequestSummary> = {}): RequestSummary {
  return {
    request_id: "req_mock_0000",
    ts_ms: 1_770_000_000_000,
    model: "deepseek-v4.1flash",
    upstream_model: "deepseek-v4.1flash",
    channel_id: "ch-bytedance",
    client: "cursor",
    lane_id: "lane-direct",
    node: null,
    status: 200,
    outcome: "ok",
    latency_ms: 900,
    ttfb_ms: 320,
    total_tokens: 2_400,
    cost: { amount: 0.0032, currency: "CNY" },
    attempts: 1,
    error_type: null,
    ...overrides,
  };
}

function makeProps(overrides: Partial<CostBodyProps> = {}): CostBodyProps {
  return {
    applied: DEFAULT_COST_FILTER,
    onSearch: () => {},
    tokenKinds: ["input", "input_cached", "output"],
    tokenKindsError: undefined,
    table: undefined,
    tableLoading: false,
    tableError: undefined,
    onRetryTable: () => {},
    dashboard: undefined,
    dashboardLoading: false,
    dashboardError: undefined,
    onRetryDashboard: () => {},
    unpriced: [],
    unpricedError: undefined,
    recent: [],
    recentError: undefined,
    exportPath: "",
    onExportPathChange: () => {},
    onPickExportPath: () => {},
    canPickPath: false,
    onExport: () => {},
    exporting: false,
    ...overrides,
  };
}

function renderBody(overrides: Partial<CostBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <CostBody {...makeProps(overrides)} />
    </ThemeProvider>,
  );
}

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <CostPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

/** ECharts option 的窄化读法（测试里只读这三处）。 */
type NarrowOption = {
  xAxis: { data: string[] };
  series: { name: string; data: number[] }[];
};

// ================================================================ ① 读表：两套字段名

  it("筛选区的主操作固定在底部（长页面 1811 行）", () => {
    const { container } = render(<CostBody {...makeProps()} />);
    const bar = container.querySelector('[data-testid="cost-query-bar"]');
    expect(bar).toBeTruthy();
    // 主操作（查询）在 sticky 容器里；容器样式确实写了 position: sticky（emotion 会内联到 class）
    const style = window.getComputedStyle(bar as Element);
    const css = Array.from(document.querySelectorAll("style"))
      .map((s) => s.textContent ?? "")
      .join("");
    expect(css).toMatch(/position:\s*sticky/);
    expect(bar?.querySelector("button")).toBeTruthy();
    void style;
  });
describe("读 usage_query 的结果（契约名与 Rust 名两套都认）", () => {
  it("契约形状：columns 里去掉维度列就是指标，rows[].metrics 是数值", () => {
    const table = readUsageTable(contractTable());
    expect(table).toBeDefined();
    expect(table!.metrics).toEqual(["requests", "input", "input_cached", "output", "cost"]);
    expect(table!.rows).toHaveLength(2);
    expect(table!.rows[0]!.dims).toEqual({ model: "deepseek-v4.1flash" });
    expect(table!.rows[0]!.values["cost"]).toBe(16.9);
    expect(table!.totalGroups).toBe(2);
    expect(table!.truncated).toBe(false);
  });

  it("Rust 形状：metrics 是权威列表、values 是数值、bucket 是后端给的本地标签", () => {
    const table = readUsageTable(rustTable());
    expect(table).toBeDefined();
    expect(table!.metrics).toEqual(["input", "input_cached", "output", "cost"]);
    expect(table!.rows[0]!.bucket).toBe("2026-02-11");
    expect(table!.rows[0]!.values["input"]).toBe(320_000);
    expect(table!.timeBucket).toBe("day");
    expect(table!.groupBy).toEqual(["time"]);
    // total_groups=5 > rows=2 → 后端明确截断了（不拿行数冒充总数）
    expect(table!.totalGroups).toBe(5);
    expect(table!.truncated).toBe(true);
  });

  it("读不到就返回 undefined / 空，不编造表", () => {
    expect(readUsageTable(undefined)).toBeUndefined();
    expect(readUsageTable({ rows: "nope" })).toBeUndefined();
    expect(readUsageTable({ rows: [] })!.rows).toEqual([]);
  });

  it("缺值的行不补 0：读不到就是 undefined（0 是「确实是 0」）", () => {
    const table = readUsageTable({
      metrics: ["input", "cost"],
      group_by: ["model"],
      rows: [{ dims: { model: "m" }, values: { input: 10 } }],
      total_groups: 1,
      truncated: false,
    });
    expect(table!.rows[0]!.values["input"]).toBe(10);
    expect(table!.rows[0]!.values["cost"]).toBeUndefined();
  });
});

// ================================================================ ② 图表：只画后端的数

describe("图表的输入只来自后端（不插点、不补 0）", () => {
  it("x 轴标签用后端给的 bucket，系列值就是后端的数值", () => {
    const table = readUsageTable(rustTable())!;
    const input = chartInputFromTable(table, ["input", "output", "cost"]);
    expect(input.labels).toEqual(["2026-02-11", "2026-02-12"]);
    expect(input.series.map((s) => s.key)).toEqual(["input", "output", "cost"]);
    expect(input.series[0]!.values).toEqual([320_000, 400_000]);
    // 点数是后端给的，不多不少
    expect(input.series[0]!.values).toHaveLength(input.labels.length);
  });

  it("某行缺该指标时整条系列被排除，并在 excluded 里说明（不补 0 假装有数据）", () => {
    const table = readUsageTable({
      metrics: ["input", "cost"],
      group_by: ["model"],
      rows: [
        { dims: { model: "a" }, values: { input: 10, cost: 1 } },
        { dims: { model: "b" }, values: { input: 20 } },
      ],
      total_groups: 2,
      truncated: false,
    })!;
    const input = chartInputFromTable(table, ["input", "cost"]);
    expect(input.series.map((s) => s.key)).toEqual(["input"]);
    expect(input.excluded).toEqual(["cost"]);
  });

  it("没有数据 / 没有系列 → option 是 undefined（页面给空状态，不画空坐标系）", () => {
    const palette = {
      text: "#000",
      muted: "#000",
      border: "#000",
      series: ["#000"],
    };
    expect(buildChartOption({ labels: [], series: [], excluded: [] }, palette)).toBeUndefined();
    expect(
      buildChartOption({ labels: ["a"], series: [], excluded: [] }, palette),
    ).toBeUndefined();
  });

  it("有数据时 option 的 x 轴与系列和后端一一对应", () => {
    const palette = { text: "#000", muted: "#000", border: "#000", series: ["#111", "#222"] };
    const option = buildChartOption(
      {
        labels: ["2026-02-11", "2026-02-12"],
        series: [
          { key: "input", values: [320_000, 400_000] },
          { key: "cost", values: [0.5, 0.62] },
        ],
        excluded: [],
      },
      palette,
      { stacked: true },
    );
    expect(option).toBeDefined();
    const narrow = option as unknown as NarrowOption;
    expect(narrow.xAxis.data).toEqual(["2026-02-11", "2026-02-12"]);
    expect(narrow.series).toHaveLength(2);
    expect(narrow.series[0]!.data).toEqual([320_000, 400_000]);
    expect(narrow.series[1]!.data).toEqual([0.5, 0.62]);
  });

  it("页面把 usage_query 的结果真的交给了 ECharts", async () => {
    renderPage();
    await waitFor(() => expect(echartsMock.init).toHaveBeenCalled());
    // 页面上有两张图（今日趋势 + 明细），这里要的是"明细那张" ——
    // 按 x 轴标签认出它，而不是靠渲染顺序碰运气
    const options = echartsMock.setOption.mock.calls.map(
      (call) => call[0] as unknown as NarrowOption,
    );
    const tableChart = options.find(
      (option) => JSON.stringify(option.xAxis.data) === JSON.stringify(["deepseek-v4.1flash"]),
    );
    expect(tableChart, `没有把 usage_query 的分组值交给 ECharts：${JSON.stringify(options.map((o) => o.xAxis.data))}`).toBeDefined();
    expect(tableChart!.series.length).toBeGreaterThan(0);
  });
});

// ================================================================ ③ 查询形状（两套参数名）

describe("usage_query 的查询形状（契约与 Rust 不一致处，两种都发）", () => {
  it("时间范围发 Rust 的线上值（7d → last7d，裸字符串），不是旧契约里的 {preset:7d}", () => {
    const q = buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "7d" });
    expect(q.range).toBe("last7d");
    expect(buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "month" }).range).toBe("this_month");
    expect(buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "30d" }).range).toBe("last30d");
    expect(buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "today" }).range).toBe("today");
  });

  it("参数名两写：Rust 取 q、契约写 query（少一个就会在真机上失败）", () => {
    const args = usageQueryArgs(buildUsageQuery(DEFAULT_COST_FILTER)) as unknown as Record<
      string,
      unknown
    >;
    expect(args["q"]).toBeDefined();
    expect(args["query"]).toBeDefined();
    expect(args["q"]).toEqual(args["query"]);
  });

  it("导出也两写参数名，并带上目标路径", () => {
    const args = usageExportArgs(buildUsageQuery(DEFAULT_COST_FILTER), "C:/tmp/usage.csv") as unknown as Record<
      string,
      unknown
    >;
    expect(args["path"]).toBe("C:/tmp/usage.csv");
    expect(args["q"]).toEqual(args["query"]);
  });

  it("分组维度按选择变化，且时间维度带 time_bucket", () => {
    const byTime = buildUsageQuery({ ...DEFAULT_COST_FILTER, groupBy: "time", bucket: "hour" });
    expect(byTime.group_by).toEqual(["time"]);
    expect(byTime.time_bucket).toBe("hour");
    const byModel = buildUsageQuery({ ...DEFAULT_COST_FILTER, groupBy: "model" });
    expect(byModel.group_by).toEqual(["model"]);
    expect(byModel.time_bucket).toBeUndefined();
  });

  it("query key 指纹把条件都带上", () => {
    expect(costFilterFingerprint(DEFAULT_COST_FILTER)).not.toBe(
      costFilterFingerprint({ ...DEFAULT_COST_FILTER, range: "30d" }),
    );
    expect(costFilterFingerprint(DEFAULT_COST_FILTER)).not.toBe(
      costFilterFingerprint({ ...DEFAULT_COST_FILTER, groupBy: "client" }),
    );
  });
});

// ================================================================ ④ 估算 vs 上游上报

describe("估算 vs 上游上报：如实标注，不许把本地口径说成上游数据", () => {
  it("口径说明：token 细分的诚实说明收进问号；「花费是本地估算」留在版面上", () => {
    renderBody();
    // [极简 · 判据 1/2] 那 216 字的来源标记解释（枚举值 + 字段名 + 文档编号）整块**已删**；
    // 问号里只剩仍在生效的诚实说明（缺的类型不补 0）。
    const label = screen.getByTestId("cost-provenance-help").getAttribute("aria-label") ?? "";
    expect(label).toContain(zhCN["cost.provenance.tokensNote"]);
    expect(screen.queryByText(zhCN["cost.provenance.tokensNote"])).toBeNull();

    // 但 `cost.provenance.cost`（"花费是本地估算…不是上游账单"）是**数据口径结论**，
    // 用户据此判断眼前数字能不能当账单用 → 必须留在版面上。
    expect(zhCN["cost.provenance.cost"]).toContain("本地");
    const section = screen.getByTestId("cost-provenance");
    expect(within(section).getByText(zhCN["cost.provenance.cost"])).toBeTruthy();
    expect(label).not.toContain(zhCN["cost.provenance.cost"]);
  });

  it("枚举值与字段名一律不上界面（usage_source 那段解释整块已删）", () => {
    renderBody();
    const section = screen.getByTestId("cost-provenance");
    const help = screen.getByTestId("cost-provenance-help").getAttribute("aria-label") ?? "";
    // 记录里确实有这三个取值，但它们不该出现在版面上，也不该出现在提示里
    for (const token of ["usage_source", "upstream_partial", "estimated"]) {
      expect(section.textContent ?? "", token).not.toContain(token);
      expect(help, token).not.toContain(token);
    }
  });

  it("读表结果里没有来源字段可用 —— 页面不假装有（DTO 就没带）", () => {
    const table = readUsageTable(rustTable())!;
    expect(Object.keys(table)).not.toContain("usageSource");
  });

  it("未定价模型要显式说出来：花费数字不完整", () => {
    renderBody({ unpriced: ["command-code-preview", "gpt-4o-mini"] });
    const box = screen.getByTestId("cost-unpriced");
    // 模型名是**数据**：仍然一眼可见（§2 第 5 条的反面 —— 数据不许收）
    expect(box.textContent).toContain("command-code-preview");
    expect(box.textContent).toContain("gpt-4o-mini");
    // "为什么不给补定价按钮"这句解释收进问号；全文进 `aria-label`
    expect(
      within(box).getByTestId("cost-unpriced-help").getAttribute("aria-label"),
    ).toContain(zhCN["cost.unpriced.hint"]);
    expect(screen.queryByText(zhCN["cost.unpriced.hint"])).toBeNull();
  });

  it("没有上期字段就不给涨跌（不给假趋势）", () => {
    const dash = readDashboard({
      cards: {
        requests: { value: 1_284 },
        tokens: { value: 12_400_000 },
        cost: { amount: 18.62, currency: "CNY" },
        success_rate: 0.992,
      },
      trend: [],
      unpriced_models: [],
    });
    expect(dash!.prevRequests).toBeUndefined();
    renderBody({ dashboard: dash });
    /*
     * [极简 · 判据 1] 原来这里还有一行「对比上期：不可得（后端返回里没有上期字段…）」——
     * 那是在解释"我们为什么没画涨跌"。没有上期值就不画涨跌本来就是正确展示，
     * 所以那一行**已删**；这条用例改成直接钉住"一张带涨跌的卡都没有"（意图不变：
     * 不许编一个涨跌出来，也不许拿一句解释当借口）。
     */
    expect(screen.queryByTestId("cost-delta-none")).toBeNull();
    expect(screen.queryByText(/^[+-]\d/)).toBeNull();
  });

  it("后端给了上期值才画涨跌，且画的是比率不是差值", () => {
    const dash = readDashboard({
      cards: {
        requests: { value: 1_284, prev: 1_120 },
        tokens: { value: 12_400_000, prev: 11_050_000 },
        cost: { amount: 18.62, currency: "CNY", prev: 16.4 },
        success_rate: 0.992,
      },
      trend: [],
      unpriced_models: [],
    });
    expect(dash!.prevRequests).toBe(1_120);
    expect(dash!.prevCost).toBe(16.4);
    renderBody({ dashboard: dash });
    expect(screen.queryByTestId("cost-delta-none")).toBeNull();
    // (1284-1120)/1120 = 14.64% → "+14.6%"（NumberCard 的 delta.value 是比率，不是差值：
    // 传差值会画成 +16400.0%，这条断言就是钉住这一点）
    expect(screen.getByText("+14.6%")).toBeTruthy();
    // 花费涨了不是好事 → 用 error 色，但方向仍由 +/- 号表达（[15 §2.1]）
    expect(screen.getByText("+13.5%")).toBeTruthy();
  });
});

// ================================================================ ⑤ 空态 / 错误态

describe("空态与错误态都说实话", () => {
  it("明细为空：诚实空状态，不画空表格也不画空坐标系（只有 1 行）", () => {
    renderBody({
      table: readUsageTable({ metrics: ["cost"], group_by: ["model"], rows: [], total_groups: 0, truncated: false }),
    });
    expect(screen.queryByTestId("cost-table")).toBeNull();
    expect(screen.queryByTestId("cost-chart")).toBeNull();
    const empty = screen.getByTestId("cost-empty");
    expect(empty.textContent).toContain(zhCN["cost.result.empty.title"]);
    // [极简 · 判据 5] 空态只说"怎么开始"：一行结论，
    // 不再有"可能这段时间没有请求…"那两行解释（它们在替用户做推测）。
    expect(empty.querySelectorAll("p")).toHaveLength(1);
    // [第 2 轮 · 判据 A] 空态不再自带「重置」—— 筛选区里已经有同标签同动作的那一个
    expect(within(empty).queryByTestId("cost-empty-reset")).toBeNull();
    expect(empty.querySelectorAll("button")).toHaveLength(0);
  });

  it("有行但没有一条指标能画：给图表空状态，表格照常显示", () => {
    renderBody({
      table: readUsageTable({
        metrics: ["input", "cost"],
        group_by: ["model"],
        rows: [
          { dims: { model: "a" }, values: { input: 10 } },
          { dims: { model: "b" }, values: { cost: 1 } },
        ],
        total_groups: 2,
        truncated: false,
      }),
    });
    expect(screen.getByTestId("cost-table")).toBeTruthy();
    expect(screen.queryByTestId("cost-chart")).toBeNull();
    expect(screen.getByTestId("cost-chart-empty").textContent).toBe(zhCN["cost.chart.empty"]);
    // 两条指标都被排除，原因要写出来（不是静默少画）
    const excluded = screen.getByTestId("cost-chart-excluded").textContent ?? "";
    expect(excluded).toContain(zhCN["cost.token.input"]);
    expect(excluded).toContain(zhCN["cost.metric.cost"]);
  });

  it("加载中且没有数据：显示加载中，不显示空状态", () => {
    renderBody({ table: undefined, tableLoading: true });
    expect(screen.queryByTestId("cost-empty")).toBeNull();
    expect(screen.getByTestId("cost-loading").textContent).toBe(zhCN["cost.result.loading"]);
  });

  it("明细错误带 TA-xxxx 与重试入口", () => {
    renderBody({
      tableError: new TokenAgencyError({ code: "TA-4001", message: "用量查询失败" }),
    });
    const err = screen.getByTestId("cost-error");
    expect(err.textContent).toContain("TA-4001");
    expect(screen.getByRole("button", { name: zhCN["common.retry"] })).toBeTruthy();
  });

  it("汇总读不到时如实说，而不是显示 0", () => {
    renderBody({
      dashboardError: new TokenAgencyError({ code: "TA-9001", message: "看板读取失败" }),
    });
    const box = screen.getByTestId("cost-summary-error").textContent ?? "";
    expect(box).toContain("TA-9001");
    expect(screen.queryByText("¥0.00")).toBeNull();
  });

  it("后端截断了分组时显式说明（总组数 vs 已加载）", () => {
    renderBody({ table: readUsageTable(rustTable()) });
    const note = screen.getByTestId("cost-truncated").textContent ?? "";
    expect(note).toContain("5");
    expect(note).toContain("2");
  });
});

// ================================================================ ⑥ 明细表与最近请求

describe("明细表与最近请求", () => {
  it("明细表按后端指标列出列头，缺值画「—」而不是 0", () => {
    renderBody({
      table: readUsageTable({
        metrics: ["input", "cost"],
        group_by: ["model"],
        rows: [
          { dims: { model: "a" }, values: { input: 10 } },
          { dims: { model: "b" }, values: { input: 20, cost: 1.5 } },
        ],
        total_groups: 2,
        truncated: false,
      }),
    });
    const table = screen.getByTestId("cost-table");
    expect(table.textContent).toContain(zhCN["cost.token.input"]);
    expect(table.textContent).toContain(zhCN["cost.metric.cost"]);
    // a 行没有 cost → 一个「—」
    expect(screen.getAllByText("—").length).toBeGreaterThanOrEqual(1);
  });

  it("契约外的新 token 类型原样显示（开放集合，不丢弃也不猜）", () => {
    renderBody({
      table: readUsageTable({
        metrics: ["output_video_frames"],
        group_by: ["model"],
        rows: [{ dims: { model: "a" }, values: { output_video_frames: 7 } }],
        total_groups: 1,
        truncated: false,
      }),
    });
    expect(screen.getByTestId("cost-table").textContent).toContain("output_video_frames");
  });

  it("最近请求列出后端给的每条（时间 / 模型 / token / 花费），空则空态", () => {
    const withRows = renderBody({
      recent: [recent(), recent({ request_id: "req_x", total_tokens: 99 })],
    });
    expect(screen.getAllByTestId("cost-recent-row")).toHaveLength(2);
    withRows.unmount();

    renderBody({ recent: [] });
    expect(screen.getByTestId("cost-recent-empty").textContent).toBe(zhCN["cost.recent.empty"]);
  });
});

// ================================================================ ⑦ 导出（真做）

describe("导出 CSV 是真调用 usage_export_csv", () => {
  it("没填路径时导出按钮不可用，并说明为什么（不是禁用占位）", () => {
    renderBody({ exportPath: "" });
    const button = screen.getByTestId("cost-export-button") as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(screen.getByTestId("cost-export-hint").textContent).toContain(zhCN["cost.export.pathHint"]);
  });

  it("填了路径后点导出：真的发 usage_export_csv，参数是当前生效条件 + 该路径", async () => {
    const calls: unknown[] = [];
    registerMock("usage_export_csv", (args) => {
      calls.push(args);
      return undefined;
    });

    renderPage();

    const input = await screen.findByTestId("cost-export-path");
    fireEvent.change(input, { target: { value: "C:/tmp/usage.csv" } });
    fireEvent.click(screen.getByTestId("cost-export-button"));

    await waitFor(() => expect(calls).toHaveLength(1));
    const args = calls[0] as Record<string, unknown>;
    expect(args["path"]).toBe("C:/tmp/usage.csv");
    const query = args["query"] as UsageQuery;
    // 导出的就是结果区那张表的口径（默认按模型分组）
    expect(query.group_by).toEqual(["model"]);
    expect(query.metrics).toEqual(DEFAULT_COST_FILTER.metrics);
    expect(args["q"]).toEqual(query);
  });

  it("导出跟着「已生效」的条件走，而不是表单草稿", async () => {
    const calls: unknown[] = [];
    registerMock("usage_export_csv", (args) => {
      calls.push(args);
      return undefined;
    });

    renderPage();

    // 取消勾选「请求数」，但先不按「查询」→ 生效条件不变 → 导出也不该变
    fireEvent.click(await screen.findByRole("checkbox", { name: zhCN["cost.metric.requests"] }));
    fireEvent.change(await screen.findByTestId("cost-export-path"), {
      target: { value: "C:/tmp/usage2.csv" },
    });
    fireEvent.click(screen.getByTestId("cost-export-button"));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(((calls[0] as Record<string, unknown>)["query"] as UsageQuery).metrics).toContain(
      "requests",
    );

    // 等这一次导出结束（在途时按钮是禁用的），再改条件重导
    await waitFor(() =>
      expect((screen.getByTestId("cost-export-button") as HTMLButtonElement).disabled).toBe(false),
    );

    // 按下「查询」之后，生效条件变了 → 导出跟着变
    fireEvent.click(screen.getByRole("button", { name: zhCN["cost.query.submit"] }));
    fireEvent.click(screen.getByTestId("cost-export-button"));
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(((calls[1] as Record<string, unknown>)["query"] as UsageQuery).metrics).not.toContain(
      "requests",
    );
  });
});

// ================================================================ ⑧ 定价条目

/** 一条定价条目（`bindings.ts:525` 的 `PriceEntry`）。 */
function priceEntry(overrides: Partial<PriceEntry> = {}): PriceEntry {
  return {
    match: { model: "deepseek-v4.1flash", provider: null, credential: null },
    rates: { input: 1.5, output: 6 },
    multiplier: 1,
    currency: null,
    plan_fee: null,
    source: "manual",
    effective_from: null,
    ...overrides,
  };
}

/**
 * 这一节现在只验**只读展示**：成本中心不再有任何定价写入口
 * （新增 / 改价 / 清除全在模型页），所以下面没有一行"点删除"的断言 ——
 * 那些断言跟着入口一起搬到了 `models.test.tsx`。
 */
describe("[定价条目] 读列表与展示（取不到就 —，不猜）", () => {
  it("空列表：给诚实的空态，不画 0 行的空表格", async () => {
    registerMock("pricing_list", () => []);
    renderPage();
    const empty = await screen.findByTestId("cost-pricing-empty");
    expect(empty.textContent).toBe(zhCN["cost.pricing.empty"]);
    expect(screen.queryByTestId("pricing-table")).toBeNull();
  });

  it("列头是契约给的那四个键；字段取不到就画 —（不编默认值）", async () => {
    registerMock("pricing_list", () => [
      priceEntry({
        match: { model: "m-1", provider: null, credential: null },
        rates: { input: 1.5 },
        source: "builtin",
      }),
    ]);
    renderPage();

    const table = await screen.findByTestId("pricing-table");
    for (const key of [
      "cost.pricing.col.match",
      "cost.pricing.col.input",
      "cost.pricing.col.output",
      "cost.pricing.col.source",
    ] as const) {
      expect(table.textContent).toContain(zhCN[key]);
    }
    const row = screen.getByTestId("pricing-row").textContent ?? "";
    expect(row).toContain("model=m-1");
    // provider / credential 后端没给 → 逐格 —（不猜成"任意"）
    expect(row).toContain("provider=—");
    expect(row).toContain("credential=—");
    expect(row).toContain("1.5");
    // rates 里没有 output → —
    expect(row).toContain("—");
    // 来源是契约里的枚举值 → 原样显示后端的词
    expect(row).toContain("builtin");
  });

  it("读不出来（形状不对）时整格 —，不抛错也不编行", () => {
    expect(readPriceEntries(undefined)).toEqual([]);
    expect(readPriceEntries("nope")).toEqual([]);
    const rows = readPriceEntries([{ match: 7, rates: "x", source: 3 }]);
    expect(rows).toHaveLength(1);
    expect(priceMatchText((rows[0]!.raw as Record<string, unknown>)["match"])).toBe("—");
    expect(priceRateText("x", "input")).toBe("—");
    expect(priceSourceText(3)).toBe("—");
  });

  it("保持后端顺序、下标就是那一行在列表里的位置（不重排）", () => {
    const rows = readPriceEntries([priceEntry(), priceEntry(), priceEntry()]);
    expect(rows.map((row) => row.index)).toEqual([0, 1, 2]);
  });

  /**
   * **结构性**断言：成本中心不再有任何定价写入口。
   *
   * 与真机 e2e 的 step10 同一条判据（那边还多验一句"改在哪儿"的说明必须在）。
   * 单测这一条的价值在于它是**纯前端的**：只要有人把写对话框加回这一页，它立刻红，
   * 不需要等到真机跑一遍。
   */
  it("只做显示：这一页没有任何 pricing-add* / pricing-delete / pricing-edit 控件", async () => {
    registerMock("pricing_list", () => [priceEntry()]);
    renderPage();
    await screen.findByTestId("pricing-table");
    const ids = Array.from(document.querySelectorAll("[data-testid]")).map((el) =>
      el.getAttribute("data-testid"),
    );
    const writes = ids.filter(
      (id) => id !== null && (id.startsWith("pricing-add") || id === "pricing-delete" || id === "pricing-edit"),
    );
    expect(writes).toEqual([]);
    // 并且必须有一句**常驻**的「改在哪儿」（用户会回来这里找已经被搬走的那个按钮）
    expect(screen.getByTestId("cost-pricing-readonly").textContent).not.toBe("");
  });
});



// ================================================================ ⑨ 长简介收进「问号圆圈 + 悬停提示」
//
// 用户第 74 轮原话：「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个
// 问好加一个圆圈，鼠标移上去能看到提示那种」。按 钉：
//
// 1. **解释收走了**：`?` 在、`aria-label` 是全文、那段正文**不再内联**；
// 2. **红线一条都不收** —— 这一页尤其要紧：它**全是数字**（§2 第 5 条），
//    所以"几个模型没定价"（带条数）、"前 N 组 / 共 M 组"（带数值）、
//    "路径为空时导出按钮不可用"（带例子与禁用条件的校验说明）、空态、后端原文，
//    一律留在版面上。判不准就留在版面上（§2 末尾的默认）。

describe("⑨ [docs/22] 长简介收进问号圆圈，数值与单位一句都不收", () => {
  /** 文案取真源：断言直接用 zh-CN 里的全文，而不是把长句在测试里再抄一遍。 */
  function copy(key: I18nKey): string {
    return i18n.t(key);
  }

  /** 某个 `?` 的 `aria-label`（= 提示正文全文）。 */
  function helpLabel(testId: string): string {
    const label = screen.getByTestId(testId).getAttribute("aria-label");
    expect(label, `${testId} 的 aria-label 必须是提示全文`).toBeTruthy();
    return label ?? "";
  }

  it("数据口径：token 细分的诚实说明进问号（正文不再内联）；花费口径结论留在版面上", () => {
    renderBody();

    const label = helpLabel("cost-provenance-help");
    expect(label, "诚实说明应当进提示").toContain(copy("cost.provenance.tokensNote"));
    expect(screen.queryByText(copy("cost.provenance.tokensNote")), "不该再内联").toBeNull();
    // 注意：`cost.provenance.cost` 是**数据口径结论**（用户据此判断数字能不能当账单）→
    // 必须在版面上，也**不许**同时出现在提示里（两处都有就是两个真相源）。
    expect(screen.queryByText(copy("cost.provenance.cost"))).not.toBeNull();
    expect(label).not.toContain(copy("cost.provenance.cost"));
    // 一个区块只有一个 `?`（§3）
    expect(screen.getAllByTestId("cost-provenance-help")).toHaveLength(1);
  });

  it("查询口径与最近请求说明也收进问号（各自的区块只有一个 `?`）", () => {
    renderBody();

    expect(helpLabel("cost-query-metrics-help")).toContain(copy("cost.query.metrics.hint"));
    expect(screen.queryByText(copy("cost.query.metrics.hint"))).toBeNull();

    expect(helpLabel("cost-recent-help")).toContain(copy("cost.recent.hint"));
    expect(screen.queryByText(copy("cost.recent.hint"))).toBeNull();

    expect(screen.getAllByTestId(/-help$/).map((node) => node.getAttribute("data-testid"))).toEqual([
      "cost-provenance-help",
      "cost-query-metrics-help",
      "cost-recent-help",
    ]);
  });

  it("「几个模型没有定价」带条数 → 留在版面上，一个字都不收（§2 第 5 条）", () => {
    renderBody({ unpriced: ["command-code-preview", "gpt-4o-mini"] });

    // 这条带 `{{count}}`（条数就是数值）→ 必须在版面上
    const countLine = screen.getByText(/个模型没有定价/);
    expect(countLine.textContent).toContain("2");
    // 而且它**不在**任何提示里（收走它就等于把"花费数字不完整"这条结论藏起来）
    for (const help of screen.getAllByTestId(/-help$/)) {
      expect(help.getAttribute("aria-label") ?? "").not.toContain("个模型没有定价");
    }
  });

  it("红线仍内联：截断的数值 / 原文 / 带例子与禁用条件的导出说明 / 空态结论", () => {
    renderBody({
      table: readUsageTable(rustTable()),
      tokenKindsError: new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：usage_token_kinds" }),
    });
    // §2 第 5 条：截断说明里带着「前 N 组 / 共 M 组」两个数
    const truncated = screen.getByTestId("cost-truncated").textContent ?? "";
    expect(truncated).toContain("5");
    expect(truncated).toContain("2");
    // §2 第 2 条：指标清单读不到 → 原样显示 [TA-xxxx] + message
    expect(screen.getByText(/TA-9001/)).toBeTruthy();
    // §2 第 3 条：导出路径这一句有例子也有"路径为空时按钮不可用"
    expect(screen.getByTestId("cost-export-hint").textContent).toContain(copy("cost.export.pathHint"));
    cleanup();

    // [判据 5] 空态：一行结论（不再是那两行推测性解释）
    // [第 2 轮 · 判据 A] 也不再自带「重置」—— 筛选区里已经有了同标签同动作的那一个
    renderBody({ table: undefined });
    const empty = screen.getByTestId("cost-empty");
    expect(empty.textContent).toContain(copy("cost.result.empty.title"));
    expect(empty.querySelectorAll("p")).toHaveLength(1);
    expect(within(empty).queryByTestId("cost-empty-reset")).toBeNull();
    cleanup();

    // [判据 1] 没有上期字段就**没有涨跌**（不再用一行解释说明"为什么没有"）
    renderBody({
      dashboard: readDashboard({
        cards: {
          requests: { value: 1_284 },
          tokens: { value: 12_400_000 },
          cost: { amount: 18.62, currency: "CNY" },
          success_rate: 0.992,
        },
        trend: [],
        unpriced_models: [],
      }),
    });
    expect(screen.queryByTestId("cost-delta-none")).toBeNull();
    expect(screen.queryByText(/^[+-]\d/)).toBeNull();
  });
});

// ================================================================ 一级界面省略

describe("成本表分组名 / 最近请求模型 / 定价匹配式：省略后全文仍在 DOM", () => {
  it("表格里的两个长值整段留在 DOM 里；没溢出时一个提示都不挂", () => {
    const longModel = "claude-sonnet-4-5-2026-07-18-preview-long-upstream";
    renderBody({
      applied: { ...DEFAULT_COST_FILTER, groupBy: "model" },
      table: readUsageTable({
        columns: ["model", "cost"],
        rows: [{ dims: { model: longModel }, metrics: { cost: 1.42 } }],
        total: 1,
      }),
      recent: [recent({ model: longModel })],
    });

    expect(screen.getByTestId("cost-row-label-ellipsis-0").textContent).toBe(longModel);
    expect(screen.getByTestId("cost-recent-model-ellipsis").textContent).toBe(longModel);
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(screen.getByTestId("cost-row-label-ellipsis-0").getAttribute("tabindex")).toBeNull();
  });

  it("定价表的匹配式整段留在 DOM 里（`model=/provider=/credential=` 拼起来的长串）", async () => {
    const model = "claude-sonnet-4-5-2026-07-18-preview";
    registerMock("pricing_list", () => [
      priceEntry({
        match: { model, provider: "p-bytedance-tokyo-bgp", credential: "c-bytedance-monthly" },
      }),
    ]);
    renderPage();

    const element = await screen.findByTestId("pricing-match-ellipsis-0");
    expect(element.textContent).toBe(
      `model=${model} · provider=p-bytedance-tokyo-bgp · credential=c-bytedance-monthly`,
    );
    expect(element.getAttribute("tabindex")).toBeNull();
  });
});

// ================================================================ ⑨ 按对外模型定价

/**
 * **这一节的来历与它现在的口径**：
 *
 * * `pricing_upsert` 原先只由已删的「套餐与线路」页代发；
 * * 第 318 轮用户指出**口径错了**：*"我是希望用户为每一个对外模型定价，来算自己大概花了多少，
 *   而不是对服务商的线路计价"*，并要求*"成本中心页面只做显示，价格配置到模型页面去"*。
 *
 * 所以这组断言现在钉的是**按对外模型定价**：
 * `match` 里**只有 model**，不再发服务商/凭据 —— 那两样与"这个模型花了多少钱"无关。
 * 交互（对话框、按钮、清除定价）的断言在 `models.test.tsx` 里，因为入口在模型页。
 */
describe("[定价] 按对外模型定价的整形：只发填了的价，绝不把「没定价」写成「免费」", () => {
  /** 一份草稿；只覆盖关心的字段。 */
  function draft(overrides: Partial<PricingDraft> = {}): PricingDraft {
    return { model: "m-a", priceInput: "", priceOutput: "", priceCached: "", ...overrides };
  }

  it("价格：空 = 不计价（undefined）；非负数字才认；负号 / 非数字 / 半截小数都不认", () => {
    expect(parsePrice("")).toBeUndefined();
    expect(parsePrice("   ")).toBeUndefined();
    // `"0"` 是**明确的价**（0 美元），不是"没填"
    expect(parsePrice("0")).toBe(0);
    expect(parsePrice("1.5")).toBe(1.5);
    expect(parsePrice("-1")).toBeUndefined();
    expect(parsePrice("很贵")).toBeUndefined();
    expect(parsePrice("1.2.3")).toBeUndefined();
  });

  it("有任意一项非空 = 要写定价；三项全空 = 不写（`hasPrice` 是那个闸）", () => {
    expect(hasPrice(draft())).toBe(false);
    expect(hasPrice(draft({ priceInput: "0" }))).toBe(true);
    expect(hasPrice(draft({ priceCached: "1" }))).toBe(true);
  });

  it("rates 只放填了的键 —— 补 0 会把「没定价」写成「免费」", () => {
    expect(priceRates(draft({ priceInput: "1.5", priceCached: "0.25" }))).toEqual({
      input: 1.5,
      input_cached: 0.25,
    });
    expect(priceRates(draft())).toEqual({});
  });

  it("载荷：`match` **只有 model**（这是本轮修正的核心），source=manual，币种/生效时间显式 null", () => {
    const entry = priceEntryFor(draft({ model: " m-a ", priceOutput: "6" }));
    expect(entry.match).toEqual({ model: "m-a" });
    // 修正前这里发的是 `{provider, credential}` —— 那正是用户指出的错：
    // 定价挂在"某家服务商的某条线路"上，而用户想知道的是"**这个模型**花了多少钱"。
    expect((entry.match as Record<string, unknown>)["provider"]).toBeUndefined();
    expect((entry.match as Record<string, unknown>)["credential"]).toBeUndefined();
    expect(entry.rates).toEqual({ output: 6 });
    expect(entry.multiplier).toBe(1);
    expect(entry.source).toBe("manual");
    expect(entry.currency).toBeNull();
    expect(entry.effective_from).toBeNull();
  });

  it("本地预检：填了但不是合法非负数字的字段逐个点名", () => {
    expect(invalidPriceFields(draft())).toEqual([]);
    expect(invalidPriceFields(draft({ priceInput: "-1", priceOutput: "很贵" }))).toEqual([
      "priceInput",
      "priceOutput",
    ]);
  });

  it("回填：打开对话框时把当前价显示出来（没有那一项就留空，**不回填 0**）", () => {
    const filled = draftFromEntry("m-a", priceEntry({ rates: { input: 1.5, output: 6 } }));
    expect(filled).toEqual({
      model: "m-a",
      priceInput: "1.5",
      priceOutput: "6",
      // rates 里没有 input_cached ⇒ 留空（= 不计价）。回填 0 会把"没定价"写成"免费"。
      priceCached: "",
    });
    expect(draftFromEntry("m-b", undefined)).toEqual({
      model: "m-b",
      priceInput: "",
      priceOutput: "",
      priceCached: "",
    });
  });
});

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
  checkPricingIndex,
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
  it("口径说明：token 来源那句收进问号；「花费是本地算的、不是上游账单」留在版面上", () => {
    renderBody();
    // [docs/22 §2]：`tokens` / `tokensNote` 是"为什么这么算、为什么读不到来源标记"的**解释** →
    // 收进标题右边的问号圆圈；断言从 `textContent` 改成读那个 `?` 的 `aria-label`，全文一个字没丢，
    // 并且**额外**钉住"那段正文不再内联"。
    const label = screen.getByTestId("cost-provenance-help").getAttribute("aria-label") ?? "";
    expect(label).toContain(zhCN["cost.provenance.tokens"]);
    expect(label).toContain("usage_source");
    expect(screen.queryByText(zhCN["cost.provenance.tokens"])).toBeNull();

    // 注意：但 `cost.provenance.cost`（"花费是本地算的…不是上游账单"）是**数据口径结论**，
    // 按 §2 红线第 1 条**必须留在版面上**（用户据此判断眼前数字能不能当账单）——
    // 所以这条断言与上一版正好相反：它必须在 `[data-testid="cost-provenance"]` 里可见。
    expect(zhCN["cost.provenance.cost"]).toContain("本地");
    const section = screen.getByTestId("cost-provenance");
    expect(within(section).getByText(zhCN["cost.provenance.cost"])).toBeTruthy();
    expect(label).not.toContain(zhCN["cost.provenance.cost"]);
  });

  it("读表结果里没有来源字段可用 —— 页面不假装有（DTO 就没带）", () => {
    const table = readUsageTable(rustTable())!;
    expect(Object.keys(table)).not.toContain("usageSource");
    // 后端记录里确实有这三个取值，但查询 DTO 没带 → 页面只说明，不标
    expect(zhCN["cost.provenance.tokens"]).toContain("estimated");
    expect(zhCN["cost.provenance.tokens"]).toContain("upstream");
  });

  it("未定价模型要显式说出来：花费数字不完整", () => {
    renderBody({ unpriced: ["command-code-preview", "gpt-4o-mini"] });
    const box = screen.getByTestId("cost-unpriced");
    // 模型名是**数据**：仍然一眼可见（§2 第 5 条的反面 —— 数据不许收）
    expect(box.textContent).toContain("command-code-preview");
    expect(box.textContent).toContain("gpt-4o-mini");
    // "为什么不给补定价按钮"这句解释收进问号（[docs/22 §2]）；全文进 `aria-label`
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
    expect(screen.getByTestId("cost-delta-none").textContent).toBe(zhCN["cost.deltaUnavailable"]);
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
  it("明细为空：诚实空状态，不画空表格也不画空坐标系", () => {
    renderBody({
      table: readUsageTable({ metrics: ["cost"], group_by: ["model"], rows: [], total_groups: 0, truncated: false }),
    });
    expect(screen.queryByTestId("cost-table")).toBeNull();
    expect(screen.queryByTestId("cost-chart")).toBeNull();
    const empty = screen.getByTestId("cost-empty");
    expect(empty.textContent).toContain(zhCN["cost.result.empty.title"]);
    expect(empty.textContent).toContain(zhCN["cost.result.empty.body"]);
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

// ================================================================ ⑧ 定价条目（[docs/21 §1 / §3]）

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

/** 按 zh-CN 的真源拼预期文案（断言里不写死中文）。 */
function textOf(key: I18nKey, vars: Record<string, string | number> = {}): string {
  let out: string = zhCN[key];
  for (const [name, value] of Object.entries(vars)) {
    out = out.replace(`{{${name}}}`, String(value));
  }
  return out;
}

/** 行内「删除」按钮（第 index 行）。 */
async function rowDeleteButtons() {
  return await screen.findAllByTestId("pricing-delete");
}

function confirmPricingDelete() {
  fireEvent.click(screen.getByTestId("pricing-delete-confirm"));
}

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

  it("保持后端顺序、下标就是删除入参（不重排）", () => {
    const rows = readPriceEntries([priceEntry(), priceEntry(), priceEntry()]);
    expect(rows.map((row) => row.index)).toEqual([0, 1, 2]);
  });
});

describe("[定价条目] 删除：先确认、按 index 调用", () => {
  it("点行内「删除」只弹确认框（一条命令都不发），确认后才发 pricing_delete({index})", async () => {
    const calls: { index: number }[] = [];
    registerMock("pricing_list", () => [
      priceEntry({ match: { model: "m-1" } }),
      priceEntry({ match: { model: "m-2" } }),
    ]);
    registerMock("pricing_delete", (args) => {
      calls.push(args as unknown as { index: number });
      return undefined;
    });

    renderPage();
    const buttons = await rowDeleteButtons();
    expect(buttons).toHaveLength(2);
    fireEvent.click(buttons[1]!);

    expect(screen.getByTestId("pricing-delete-dialog")).toBeTruthy();
    // 确认框里写清删的是哪一个：下标 + 匹配
    const target = screen.getByTestId("pricing-delete-target").textContent ?? "";
    expect(target).toContain("#1");
    expect(target).toContain("model=m-2");
    // **二次确认之前绝不调用后端**
    expect(calls).toHaveLength(0);

    confirmPricingDelete();
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toEqual({ index: 1 });
    // 成功后在结果区如实写「已删除 <标识>」（标识 = 下标 + 匹配）
    const result = await screen.findByTestId("pricing-delete-result");
    expect(result.textContent).toContain(textOf("delete.result.ok", { name: "#1" }));
    // `pricing_delete` 没有 force 语义 → 不放级联按钮
    expect(screen.queryByTestId("pricing-delete-cascade")).toBeNull();
  });

  it("单条删除失败时：结果区原样显示后端原文（含 TA 码），不写「已删除」", async () => {
    registerMock("pricing_list", () => [priceEntry({ match: { model: "m-1" } })]);
    registerMock("pricing_delete", () => {
      throw new TokenAgencyError({ code: "TA-1004", message: "定价条目不存在（index 0）" });
    });

    renderPage();
    fireEvent.click(await screen.findByTestId("pricing-delete"));
    confirmPricingDelete();

    const failed = await screen.findByTestId("pricing-delete-result-0");
    expect(failed.textContent).toContain("TA-1004");
    expect(failed.textContent).toContain("定价条目不存在（index 0）");
    expect(screen.queryByText(textOf("delete.result.ok", { name: "#0" }))).toBeNull();
    // 这不是"下标错位"那条路（错位根本不会调后端）
    expect(screen.queryByTestId("pricing-delete-result-stale-0")).toBeNull();
  });

  it("**下标错位就拒绝**：重新读回来的那个下标已经是另一条 → 一条都不删，并让用户重看", async () => {
    let current: PriceEntry[] = [priceEntry({ match: { model: "m-1" } })];
    let reads = 0;
    registerMock("pricing_list", () => {
      reads += 1;
      return current;
    });
    const calls: { index: number }[] = [];
    registerMock("pricing_delete", (args) => {
      calls.push(args as unknown as { index: number });
      return undefined;
    });

    renderPage();
    fireEvent.click(await screen.findByTestId("pricing-delete"));
    const readsBeforeDelete = reads;

    // 用户看着列表的时候，列表变了：同一下标上已经是**另一条**
    current = [priceEntry({ match: { model: "m-2" } })];
    confirmPricingDelete();

    const stale = await screen.findByTestId("pricing-delete-result-stale-0");
    // ① 绝不执行删除
    expect(calls).toHaveLength(0);
    // ② 重新读了一次（错位判断建立在刚刚返回的那一份上）
    expect(reads).toBeGreaterThan(readsBeforeDelete);
    // ③ 把话说清楚
    expect(stale.textContent).toContain("重看");
  });

  it("批量时每一条都重新比对；列表真的一条条变短也不会错位（从大到小的理由）", async () => {
    // 这次让 mock **真的**随着删除变短：只有"从大到小 + 每条重新比对"才可能全成功
    let current: PriceEntry[] = [
      priceEntry({ match: { model: "m-1" } }),
      priceEntry({ match: { model: "m-2" } }),
    ];
    registerMock("pricing_list", () => current);
    const calls: { index: number }[] = [];
    registerMock("pricing_delete", (args) => {
      const call = args as unknown as { index: number };
      calls.push(call);
      current = current.filter((_, index) => index !== call.index);
      return undefined;
    });

    renderPage();
    fireEvent.click(
      within(await screen.findByTestId("pricing-select-all")).getByRole("checkbox"),
    );
    fireEvent.click(screen.getByTestId("pricing-delete-selected"));
    confirmPricingDelete();

    await waitFor(() => expect(calls).toHaveLength(2));
    // 两条都删掉了：先下标 1（列表变短后，原来下标 0 那条仍在 0），再下标 0
    expect(calls.map((call) => call.index)).toEqual([1, 0]);
    expect(current).toEqual([]);
    const result = await screen.findByTestId("pricing-delete-result");
    expect(result.textContent).toContain(textOf("delete.result.summary", { ok: 2, failed: 0 }));
  });

  it("批量：按下标**从大到小**逐条删（下标会前移），1 成功 1 失败逐条汇报，不中止其它条", async () => {
    const calls: { index: number }[] = [];
    registerMock("pricing_list", () => [
      priceEntry({ match: { model: "m-1" } }),
      priceEntry({ match: { model: "m-2" } }),
    ]);
    registerMock("pricing_delete", (args) => {
      const call = args as unknown as { index: number };
      calls.push(call);
      if (call.index === 0) {
        throw new TokenAgencyError({ code: "TA-2007", message: "定价条目非法：删不掉" });
      }
      return undefined;
    });

    renderPage();
    const toolbarButton = await screen.findByTestId("pricing-delete-selected");
    expect((toolbarButton as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(within(screen.getByTestId("pricing-select-all")).getByRole("checkbox"));
    expect(screen.getByTestId("pricing-selected-count").textContent).toContain(
      textOf("delete.selectedCount", { count: 2 }),
    );

    fireEvent.click(toolbarButton);
    // 一次确认
    expect(screen.getByTestId("pricing-delete-dialog")).toBeTruthy();
    expect(calls).toHaveLength(0);
    confirmPricingDelete();

    const result = await screen.findByTestId("pricing-delete-result");
    await waitFor(() => expect(calls).toHaveLength(2));
    // 从大到小：先删下标 1，再删下标 0（否则第一条删完，后面每条的下标都会前移）
    expect(calls.map((call) => call.index)).toEqual([1, 0]);
    // 逐条如实汇报，而不是一句"批量删除成功"
    expect(result.textContent).toContain(textOf("delete.result.summary", { ok: 1, failed: 1 }));
    const failed = screen.getByTestId("pricing-delete-result-0").textContent ?? "";
    expect(failed).toContain("TA-2007");
    expect(failed).toContain("定价条目非法：删不掉");
  });
});

describe("[定价条目] 下标比对（单测：删之前必须逐字段全等）", () => {
  const a = priceEntry({ match: { model: "m-1" } });
  const b = priceEntry({ match: { model: "m-2" } });

  it("同一下标仍是同一条 → same（键序不同不改结论：比的还是全部字段）", () => {
    expect(checkPricingIndex([a], 0, a)).toBe("same");
    expect(checkPricingIndex([JSON.parse(JSON.stringify(a))], 0, a)).toBe("same");
    const reordered = {
      source: a.source,
      effective_from: a.effective_from,
      rates: a.rates,
      match: a.match,
      plan_fee: a.plan_fee,
      currency: a.currency,
      multiplier: a.multiplier,
    };
    expect(checkPricingIndex([reordered], 0, a)).toBe("same");
  });

  it("下标处已经是另一条 → changed（拒绝执行）", () => {
    expect(checkPricingIndex([b], 0, a)).toBe("changed");
    // 少一个字段也算变了（逐字段全等，不是"看起来差不多"）
    expect(checkPricingIndex([{ ...a, rates: {} }], 0, a)).toBe("changed");
  });

  it("下标越界 / 不是数组 → missing（两边都拒绝）", () => {
    expect(checkPricingIndex([], 0, a)).toBe("missing");
    expect(checkPricingIndex([a], 5, a)).toBe("missing");
    expect(checkPricingIndex("nope", 0, a)).toBe("missing");
  });
});

// ================================================================ ⑨ [docs/22] 长简介收进「问号圆圈 + 悬停提示」
//
// 用户第 74 轮原话：「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个
// 问好加一个圆圈，鼠标移上去能看到提示那种」。按 [docs/22 §2/§3/§5] 钉：
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

  it("数据口径：token 来源两段合成一个问号（正文不再内联）；花费口径结论留在版面上", () => {
    renderBody();

    const label = helpLabel("cost-provenance-help");
    for (const key of ["cost.provenance.tokens", "cost.provenance.tokensNote"] as const) {
      expect(label, `${key} 应当进同一个提示`).toContain(copy(key));
      expect(screen.queryByText(copy(key)), `${key} 不该再内联`).toBeNull();
    }
    // 注意：`cost.provenance.cost` 是**数据口径结论**（§2 红线第 1 条）→ 必须在版面上，
    // 也**不许**同时出现在提示里（两处都有就是两个真相源）。
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

  it("红线仍内联：截断的数值 / 后端原文 / 带例子与禁用条件的导出说明 / 空态 / 不可得的结论", () => {
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

    // §2 第 1 条：空态（"可能这段时间没有请求…"是给用户下一步的说明，与空态绑在一起）
    renderBody({ table: undefined });
    const empty = screen.getByTestId("cost-empty").textContent ?? "";
    expect(empty).toContain(copy("cost.result.empty.title"));
    expect(empty).toContain(copy("cost.result.empty.body"));
    cleanup();

    // §2 第 1 条：没有上期字段就是没有涨跌 —— 这条结论一眼可见
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
    expect(screen.getByTestId("cost-delta-none").textContent).toBe(copy("cost.deltaUnavailable"));
  });
});

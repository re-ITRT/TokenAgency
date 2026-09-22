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
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  readUsageTable,
  usageExportArgs,
  usageQueryArgs,
  DEFAULT_COST_FILTER,
  type CostBodyProps,
} from "@/pages/cost";
// `registerMock` 由 `@/shared/api`（门面）导出，不是 `@/shared/api/mock` ——
// 其它四个测试文件（channels/models/settings/providers）都是从门面导入的。
import { registerBaselineMocks, registerMock, TokenAgencyError } from "@/shared/api";
import type { RequestSummary, UsageQuery } from "@/shared/bindings";
import { i18n } from "@/shared/i18n";
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
  it("时间范围发 Rust 的预设名（7d → last7d），不是契约里的 7d", () => {
    const q = buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "7d" }) as unknown as {
      range: { preset: string };
    };
    expect(q.range).toEqual({ preset: "last7d" });
    expect(
      (buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "month" }) as unknown as {
        range: { preset: string };
      }).range,
    ).toEqual({ preset: "this_month" });
    expect(
      (buildUsageQuery({ ...DEFAULT_COST_FILTER, range: "today" }) as unknown as {
        range: { preset: string };
      }).range,
    ).toEqual({ preset: "today" });
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
  it("口径说明点出 usage_source 与「来源标记不可得」，并说清花费是本地算的", () => {
    renderBody();
    const box = screen.getByTestId("cost-provenance").textContent ?? "";
    expect(box).toContain(zhCN["cost.provenance.tokens"]);
    expect(box).toContain("usage_source");
    expect(box).toContain(zhCN["cost.provenance.cost"]);
    // 花费的定性必须是「本地计算」，不是上游账单
    expect(zhCN["cost.provenance.cost"]).toContain("本地");
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
    const box = screen.getByTestId("cost-unpriced").textContent ?? "";
    expect(box).toContain("command-code-preview");
    expect(box).toContain("gpt-4o-mini");
    expect(box).toContain(zhCN["cost.unpriced.hint"]);
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

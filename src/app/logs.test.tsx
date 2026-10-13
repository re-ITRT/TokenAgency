// @vitest-environment jsdom
/**
 * 请求日志页（[07 §2] / [14 §12]）的验收。重点和看板那份一样是**诚实**，外加 C 组的三件事：
 *
 * 1. **有数据 / 无数据两种状态都要对**（本任务点名的要求）：
 *    有数据时按契约渲染行；无数据时给**诚实的空状态**（把 off 模式、没有流量、条件太窄三种
 *    可能都写出来），**不画 0 行的假表格**、不编数字。
 * 2. **两套字段名都要能读**：冻结契约（`bindings.ts`）与 Rust（`ta_store::trace_store`）
 *    的 `TraceRow` 字段名不一致 —— 这是"不许改 bindings.ts"下的现实。夹具里两种形状各来一行，
 *    断言两种都能渲染出来（这就是"页面在真机上不会整列显示 —"的证据）。
 * 3. **不许假筛选**：查询只发 `applied` 的条件；`log_query` 的入参**两个参数名都发**
 *    （Rust 取 `q`，契约写 `query`）；后端只接受一个检索串 → UI 互斥。
 *
 * 另外钉住 C4 的可视化：同一 request_id 的尝试链按顺序画出来、网关判定的 TA 码来自
 * 尝试链的 `reason`、header_diff 的三种状态（未知 / 无差异 / 有差异）各不相同。
 *
 * 4. **导出（log_export）不许自作多情**：路径没有默认值（没有任何后端命令上报目录，
 *    不许硬编 AppData），空着就禁用按钮；落盘路径以**后端回执**为准（目录会被后端解析成
 *    `tokenagency-diagnostic-<时间戳>.zip`）；没有回执就只说"已请求、不能确认落盘"。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import LogsPage, {
  LogsBody,
  OUTCOME_LEVELS,
  outcomeOf,
  type LogsBodyProps,
  type LogStatsView,
} from "@/pages/logs";
import {
  DEFAULT_FILTER,
  PAGE_SIZE,
  attemptsTotalMs,
  buildLogQuery,
  exportOutcome,
  filterFingerprint,
  isErrorCodeShape,
  logQueryArgs,
  mergeLogRows,
  newestRecordedMode,
  readHeaderDiff,
  readLogRow,
  readStorageStats,
  readTraceDetail,
  searchKind,
  searchTerm,
  type LogsFilter,
} from "@/pages/logs/model";
import { TracePanel } from "@/pages/logs/TracePanel";
import type { StorageStats, TraceDetail, TraceRow } from "@/shared/bindings";
import { formatBytes } from "@/shared/format";
import { TokenAgencyError, registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";
import { buildTheme } from "@/theme";

/** `log_export` 收到过的参数（证据：发的是当前生效条件 + 用户填的路径）。 */
let exportCalls: Array<Record<string, unknown>> = [];
/** `log_export` 的返回（回执对象 / null 两形态都要能显示）。 */
let exportResult: unknown = null;
let exportError: Error | null = null;

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

beforeEach(() => {
  exportCalls = [];
  exportResult = null;
  exportError = null;
  registerMock("log_export", (args) => {
    exportCalls.push({ ...(args as Record<string, unknown>) });
    if (exportError !== null) throw exportError;
    return exportResult;
  });
});

afterEach(() => {
  cleanup();
});

// ================================================================ 夹具（两种后端形状各一份）

/** 冻结契约 / 浏览器 mock 的形状（`bindings.ts:859-877` 那一套名字）。 */
function contractRow(overrides: Record<string, unknown> = {}): TraceRow {
  return {
    request_id: "req_contract_0001",
    ts_ms: 1_770_000_000_000,
    model: "deepseek-v4.1flash",
    upstream_model: "deepseek-v4.1flash-2026",
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
  } as unknown as TraceRow;
}

/** Rust `ta_store::TraceRow` 的形状（`trace_store.rs:130-158` 那一套名字）。 */
function rustRow(overrides: Record<string, unknown> = {}): TraceRow {
  return {
    request_id: "req_rust_0002",
    ts_ms: 1_770_000_060_000,
    client: "claude-code",
    model: "deepseek-v4.1flash",
    protocol_in: "openai",
    protocol_out: "anthropic",
    stream: true,
    policy_id: "pol-default",
    group_id: "grp-deepseek",
    chosen_channel: "ch-commandcode",
    attempts: 3,
    status: 502,
    outcome: "failed",
    total_ms: 30_000,
    ttfb_ms: null,
    tokens: 0,
    error: "全部通道失败",
    mode: "full",
    has_body: true,
    body_bytes: 12_345,
    ...overrides,
  } as unknown as TraceRow;
}

function makeProps(overrides: Partial<LogsBodyProps> = {}): LogsBodyProps {
  return {
    query: {
      draft: DEFAULT_FILTER,
      onDraftChange: () => {},
      onSearch: () => {},
      onReset: () => {},
      applied: DEFAULT_FILTER,
      rows: [],
      total: undefined,
      isLoading: false,
      error: undefined,
      onRetry: () => {},
      canLoadMore: false,
      isLoadingMore: false,
      onLoadMore: () => {},
    outcome: "all",
    onPickOutcome: () => {},
    onPickPreset: () => {},
    },
    log: {
      stats: undefined,
      statsLoading: false,
      statsError: undefined,
      mode: undefined,
      onModeChange: () => {},
      onSaveMode: () => {},
      savingMode: false,
      savedMode: undefined,
      retention: undefined,
      onRetentionChange: () => {},
      onSaveRetention: () => {},
      savingRetention: false,
      savedRetention: undefined,
      recordedMode: undefined,
      onClear: () => {},
      clearing: false,
    },
    export: {
      path: "",
      onPathChange: () => {},
      appliedSummary: "近 7 天 · 不限级别 · 不检索",
      onRun: () => {},
      running: false,
      outcome: undefined,
      error: undefined,
    },
    expanded: { requestId: null, onToggle: () => {}, renderTrace: () => null },
    ...overrides,
  };
}

function renderBody(overrides: Partial<LogsBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <LogsBody {...makeProps(overrides)} />
    </ThemeProvider>,
  );
}

/**
 * L2：顶部四张统计卡。
 *
 * 三条都要钉：① 数字来自 `log_stats`（不是前端自己数）；② **一行都没有**时平均延迟显示
 * 「读不到」（后端给 `null`，界面**不许**编 0ms）；③ 读取失败时整排卡显示「读不到」，
 * 不留旧数字 —— 这是本仓库"不编数"的硬规矩在界面上的落点。
 */
  it("导出：占位符只留一个示例（红线解释仍在版面）", () => {
    renderBody();
    // 那条 121 字的"没有默认路径"是**既有测试钉住的红线**，仍在版面上
    expect(screen.getByText(zhCN["logs.export.path.hint"])).toBeTruthy();
    // 占位符缩短（不许是一句话）
    const input = screen.getByLabelText(zhCN["logs.export.path"]) as HTMLInputElement;
    expect(input.placeholder).toBe(zhCN["logs.export.path.placeholder"]);
    expect(input.placeholder.length).toBeLessThan(20);
  });
describe("L2 统计卡", () => {
  /** 展示层是**纯 props**：卡片只认 `query.stats`，所以这里直接给它值（容器怎么映射另说）。 */
  const withStats = (stats: LogStatsView | null | undefined) =>
    renderBody({ query: { ...makeProps().query, stats } });

  it("四个数来自 log_stats：总数 / 成功率 / 失败 / 平均延迟", () => {
    withStats({ total: 40, ok: 30, failed: 10, avgTotalMs: 1200 });
    expect(screen.getByTestId("logs-stat-total-value").textContent).toContain("40");
    expect(screen.getByTestId("logs-stat-ok-value").textContent).toContain("75%");
    expect(screen.getByTestId("logs-stat-failed-value").textContent).toContain("10");
    expect(screen.getByTestId("logs-stat-avg-value").textContent).toContain("1,200ms");
  });

  it("没有行时平均延迟是「读不到」，不是 0ms", () => {
    withStats({ total: 0, ok: 0, failed: 0, avgTotalMs: null });
    expect(screen.getByTestId("logs-stat-avg-value").textContent).toContain("读不到");
  });

  it("统计拿不到（null）时整排显示「读不到」，不拿旧数字充数", () => {
    withStats(null);
    expect(screen.getByTestId("logs-stat-total-value").textContent).toContain("读不到");
    expect(screen.getByTestId("logs-stat-avg-value").textContent).toContain("读不到");
  });

  it("L4 列表花费列：契约给了就显示，没给就「—」（不拿 token 现算）", () => {
    renderBody({
      query: {
        ...makeProps().query,
        rows: [
          {
            requestId: "req_a",
            tsMs: 1,
            model: "m",
            status: 200,
            outcome: "ok",
            latencyMs: 10,
            tokens: 5,
            cost: { amount: 0.020401, currency: "USD" },
          },
          {
            requestId: "req_b",
            tsMs: 2,
            model: "m",
            status: 200,
            outcome: "ok",
            latencyMs: 10,
            tokens: 5,
          },
        ] as never,
      },
    });
    const costs = screen.getAllByTestId("logs-row-cost").map((el) => el.textContent);
    expect(costs[0]).toBe("USD 0.020401");
    // 没给 cost 就显示「—」：**绝不拿 token 乘一个单价编出来**
    expect(costs[1]).toBe("—");
  });
  it("L5 字段表：只显示这一行真有的值，读不到显示「—」（不猜）", () => {
    const row = {
      requestId: "req_fields",
      tsMs: 1,
      model: "deepseek-v4-flash",
      channel: "ch-a",
      status: 200,
      outcome: "ok",
      latencyMs: 1234,
      tokens: 315,
      // 刻意**不给** cost：契约没给就不许显示，也不许拿 token 现算
    };
    renderBody({
      query: { ...makeProps().query, rows: [row] as never },
      expanded: { requestId: "req_fields", onToggle: () => {}, renderTrace: () => null },
    });
    expect(screen.getByTestId("logs-field-model").textContent).toBe("deepseek-v4-flash");
    expect(screen.getByTestId("logs-field-latency").textContent).toBe("1,234ms");
    expect(screen.getByTestId("logs-field-tokens").textContent).toBe("315");
    expect(screen.getByTestId("logs-field-cost").textContent).toBe("—");
  });

  it("L5 字段表：契约给了 cost 就照它显示（不自己算金额）", () => {
    renderBody({
      query: {
        ...makeProps().query,
        rows: [
          {
            requestId: "req_cost",
            tsMs: 1,
            model: "m",
            status: 200,
            outcome: "ok",
            latencyMs: 10,
            tokens: 5,
            cost: { amount: 0.020401, currency: "USD" },
          },
        ] as never,
      },
      expanded: { requestId: "req_cost", onToggle: () => {}, renderTrace: () => null },
    });
    expect(screen.getByTestId("logs-field-cost").textContent).toBe("USD 0.020401");
  });
  it("L6 Token 明细：契约的 token_kinds 被读成 tokenKinds；没有就是 null（不编）", () => {
    const withKinds = readTraceDetail({
      request_id: "req_tk",
      row: { request_id: "req_tk", tokens: 999 },
      attempts: [],
      token_kinds: { input: 1200, output: 340 },
    } as never);
    expect(withKinds.tokenKinds).toEqual({ input: 1200, output: 340 });

    // 用量账本里没有这笔账 → `null`（界面据此说"没有这份记录"），**绝不**回落成 row 里的总数
    const withoutKinds = readTraceDetail({
      request_id: "req_tk2",
      row: { request_id: "req_tk2", tokens: 999 },
      attempts: [],
    } as never);
    expect(withoutKinds.tokenKinds).toBeNull();
  });
  it("L5 详情头部：显示后端给的请求 ID，复制按钮把它写进剪贴板", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    renderBody({
      query: {
        ...makeProps().query,
        rows: [
          {
            requestId: "req_copy_me",
            tsMs: 1,
            model: "m",
            status: 200,
            outcome: "ok",
            totalMs: 5,
            tokens: 1,
          },
        ] as never,
      },
      expanded: {
        requestId: "req_copy_me",
        onToggle: () => {},
        renderTrace: () => null,
      },
    });
    expect(screen.getByTestId("logs-detail-head").textContent).toContain("req_copy_me");
    fireEvent.click(screen.getByTestId("logs-detail-copy"));
    // 复制的是**后端给的那个 id**，不是前端拼的
    expect(writeText).toHaveBeenCalledWith("req_copy_me");
  });
  it("L3 双栏：选中一行 → 右栏显示它的详情；没选中 → 空态（不是一片空白）", () => {
    const rows = [
      { requestId: "req_a", tsMs: 1, model: "m", status: 200, outcome: "ok", totalMs: 10, tokens: 1 },
      { requestId: "req_b", tsMs: 2, model: "m", status: 502, outcome: "failed", totalMs: 20, tokens: 2 },
    ];
    const { rerender } = renderBody({
      query: { ...makeProps().query, rows: rows as never },
      expanded: { requestId: null, onToggle: () => {}, renderTrace: (id) => <div>trace:{id}</div> },
    });
    // 没选中：右栏是空态文案
    expect(screen.getByTestId("logs-detail-pane").textContent).toContain("从上表选一条");

    rerender(
      <ThemeProvider theme={buildTheme("dark")}>
        <LogsBody
          {...makeProps({
            query: { ...makeProps().query, rows: rows as never },
            expanded: {
              requestId: "req_b",
              onToggle: () => {},
              renderTrace: (id) => <div>trace:{id}</div>,
            },
          })}
        />
      </ThemeProvider>,
    );
    // 选中 req_b：右栏出现它的详情与 trace
    expect(screen.getByTestId("logs-detail-pane").textContent).toContain("trace:req_b");
  });
  it("三个计数片：数字与卡片同源，点「失败」= failed + retried（否则与统计对不上）", () => {
    const picked: string[] = [];
    renderBody({
      query: {
        ...makeProps().query,
        stats: { total: 40, ok: 30, failed: 10, avgTotalMs: null },
        outcome: "all",
        onPickOutcome: (next) => picked.push(next),
      },
    });
    expect(screen.getByTestId("logs-chip-all").textContent).toContain("40");
    expect(screen.getByTestId("logs-chip-ok").textContent).toContain("30");
    expect(screen.getByTestId("logs-chip-failed").textContent).toContain("10");

    fireEvent.click(screen.getByTestId("logs-chip-failed"));
    expect(picked).toEqual(["failed"]);
  });

  it("时间范围做成按钮：高亮跟 `applied.preset`，点一下把 preset 送出去（与计数片同一条生效路径）", () => {
    const picked: string[] = [];
    renderBody({
      query: {
        ...makeProps().query,
        applied: { ...DEFAULT_FILTER, preset: "30d" },
        onPickPreset: (next) => picked.push(next),
      },
    });
    // 四个预设都在版面上（不是下拉：截图里是分段按钮）
    expect(screen.getByTestId("logs-range-presets")).toBeTruthy();
    for (const preset of ["today", "7d", "30d", "month"]) {
      expect(screen.getByTestId(`logs-range-${preset}`)).toBeTruthy();
    }
    fireEvent.click(screen.getByTestId("logs-range-today"));
    expect(picked).toEqual(["today"]);
  });

  it("「失败」片对应的级别集合 = failed + retried（口径与后端 LogStats 一致）", () => {    expect(OUTCOME_LEVELS.all).toEqual([]);
    expect(OUTCOME_LEVELS.ok).toEqual(["ok"]);
    expect(OUTCOME_LEVELS.failed).toEqual(["failed", "retried"]);
    // 高亮状态从 applied.levels 推导：空 = 全部、只有 ok = 成功、含 failed/retried = 失败
    expect(outcomeOf([])).toBe("all");
    expect(outcomeOf(["ok"])).toBe("ok");
    expect(outcomeOf(["failed"])).toBe("failed");
    expect(outcomeOf(["retried"])).toBe("failed");
  });
  it("没传 stats（还没查完）时**不显示**卡片 —— 不用占位数字骗人", () => {
    withStats(undefined);
    expect(screen.queryByTestId("logs-stats")).toBeNull();
  });
});
function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <LogsPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

/**
 * L7：记录与留存那一节里的四块（存储 / 模式 / 保留 / 导出）**默认收起**。
 *
 * 收起只切 `display`（内容始终挂载），所以按"存在"查询的 `getByTestId` / `getByText`
 * 照旧能找到；但按"可见"查询的 `getByRole` 只认可访问性树 —— 隐藏的内容不在里面。
 * 那 7 条用例不是被"绕过"，而是如实复现用户路径：**先展开，再操作**。
 * （所以这里绝不允许把默认值改成"展开"来换绿。）
 */
function expandAdvanced(): void {
  fireEvent.click(screen.getByTestId("logs-advanced-toggle"));
}

// ================================================================ ① 纯逻辑：查询形状

describe("[C2] log_query 的查询形状（契约与 Rust 不一致处，两种都发）", () => {
  it("时间窗同时给 from_ms/to_ms 与契约的 range —— 只发 range 等于没有时间筛选", () => {
    const wire = buildLogQuery(
      { ...DEFAULT_FILTER, preset: "today" },
      { nowMs: 1_770_000_000_000 },
    );
    expect(typeof wire["from_ms"]).toBe("number");
    // Rust 侧字段（ta-store trace_store.rs:189-216）
    expect(wire["from_ms"]).toBeLessThan(wire["to_ms"] as number);
    expect(wire["to_ms"]).toBe(1_770_000_000_000);
    // 冻结契约的字段（bindings.ts:848）；线上值 = Rust `TimeRange` 的裸 snake_case 变体名
    expect(wire["range"]).toBe("today");
    expect(wire["limit"]).toBe(PAGE_SIZE);
  });

  it("参数名两写：Rust 取 q、契约写 query（少一个就会在真机上失败或编译不过）", () => {
    const args = logQueryArgs(DEFAULT_FILTER, { nowMs: 1 }) as unknown as Record<string, unknown>;
    expect(args["q"]).toBeDefined();
    expect(args["query"]).toBeDefined();
    expect(args["q"]).toEqual(args["query"]);
  });

  it("级别筛选进 status；空数组表示不限", () => {
    expect(buildLogQuery(DEFAULT_FILTER)["status"]).toEqual([]);
    expect(buildLogQuery({ ...DEFAULT_FILTER, levels: ["failed", "retried"] })["status"]).toEqual([
      "failed",
      "retried",
    ]);
  });

  it("只接受一个检索串：请求 ID 优先于错误码；两个都空时不发 search", () => {
    expect(searchTerm(DEFAULT_FILTER)).toBeUndefined();
    expect(buildLogQuery(DEFAULT_FILTER)["search"]).toBeUndefined();

    const both: LogsFilter = { ...DEFAULT_FILTER, requestId: " req_abc ", errorCode: "TA-4001" };
    expect(searchTerm(both)).toBe("req_abc");
    expect(searchKind(both)).toBe("request_id");
    expect(buildLogQuery(both)["search"]).toBe("req_abc");

    const codeOnly: LogsFilter = { ...DEFAULT_FILTER, errorCode: "TA-4001" };
    expect(searchTerm(codeOnly)).toBe("TA-4001");
    expect(searchKind(codeOnly)).toBe("error_code");
  });

  it("TA 码形状判定是严格四位（与 ta-core::diag::query 同口径）", () => {
    expect(isErrorCodeShape("TA-4001")).toBe(true);
    expect(isErrorCodeShape("ta-4001")).toBe(false);
    expect(isErrorCodeShape("TA-40011")).toBe(false);
    expect(isErrorCodeShape("TA-401")).toBe(false);
  });

  it("query key 指纹把条件都带上（条件变了必然重查）", () => {
    expect(filterFingerprint(DEFAULT_FILTER)).not.toBe(
      filterFingerprint({ ...DEFAULT_FILTER, preset: "30d" }),
    );
    expect(filterFingerprint(DEFAULT_FILTER)).not.toBe(
      filterFingerprint({ ...DEFAULT_FILTER, levels: ["failed"] }),
    );
  });
});

// ================================================================ ② 纯逻辑：读行

describe("读后端返回的行（契约名与 Rust 名两套都认）", () => {
  it("契约形状：channel_id / upstream_model / latency_ms / total_tokens / cost", () => {
    const view = readLogRow(contractRow());
    expect(view.requestId).toBe("req_contract_0001");
    expect(view.channel).toBe("ch-bytedance");
    expect(view.upstreamModel).toBe("deepseek-v4.1flash-2026");
    expect(view.latencyMs).toBe(900);
    expect(view.tokens).toBe(2_400);
    expect(view.cost).toEqual({ amount: 0.0032, currency: "CNY" });
    expect(view.outcome).toBe("ok");
    expect(view.mode).toBeUndefined();
  });

  it("Rust 形状：chosen_channel / total_ms / tokens / error / mode / 协议", () => {
    const view = readLogRow(rustRow());
    expect(view.channel).toBe("ch-commandcode");
    expect(view.latencyMs).toBe(30_000);
    expect(view.tokens).toBe(0);
    expect(view.error).toBe("全部通道失败");
    expect(view.mode).toBe("full");
    expect(view.protocolIn).toBe("openai");
    expect(view.protocolOut).toBe("anthropic");
    expect(view.stream).toBe(true);
    // 契约里没有的字段读不到就是 undefined —— 不猜、不填 0
    expect(view.credential).toBeUndefined();
    expect(view.cost).toBeUndefined();
  });

  it("翻页去重按 request_id；最近行记录的模式只取有证据的那个", () => {
    const a = contractRow();
    const b = rustRow();
    const merged = mergeLogRows([a], [a, b]).map(
      (r) => (r as unknown as { request_id: string }).request_id,
    );
    expect(merged).toEqual(["req_contract_0001", "req_rust_0002"]);
    expect(newestRecordedMode([readLogRow(rustRow())])).toBe("full");
    // 契约形状没有 mode 这一列 → 不推断当前模式
    expect(newestRecordedMode([readLogRow(contractRow())])).toBeUndefined();
  });

  it("占用：读 Rust 多出来的 body_bytes；整个对象缺失时返回 undefined", () => {
    const stats = {
      rows: 12_840,
      bytes: 386 * 1024 * 1024,
      oldest_ts: 1,
      newest_ts: 2,
    } as StorageStats;
    const view = readStorageStats(stats);
    expect(view?.rows).toBe(12_840);
    expect(view?.bodyBytes).toBeUndefined();
    expect(readStorageStats(undefined)).toBeUndefined();
  });

  it("字节格式化只在 format.ts 一处（1024 进制，别在页面里拼单位）", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(386 * 1024 * 1024)).toBe("386 MB");
    expect(formatBytes(-1)).toBe("—");
  });
});

// ================================================================ ③ 纯逻辑：轨迹

describe("[C4] 读轨迹：header_diff 三种状态、尝试链耗时合计", () => {
  it("header_diff：undefined=未知、空对象=无差异、有差异=计数", () => {
    expect(readHeaderDiff(undefined)).toBeUndefined();
    expect(readHeaderDiff({})?.count).toBe(0);

    // Rust 形状（ta-core/src/trace.rs:18-28）
    const rust = readHeaderDiff({
      added: ["x-extra"],
      removed: ["x-stainless-lang"],
      changed: [["user-agent", "cursor/1", "ta-gateway/0.1"]],
    });
    expect(rust?.count).toBe(3);
    expect(rust?.changed[0]).toEqual(["user-agent", "cursor/1", "ta-gateway/0.1"]);

    // 冻结契约形状（bindings.ts:898）
    const contract = readHeaderDiff({ "user-agent": { client: "cursor/1", upstream: "ta/0.1" } });
    expect(contract?.count).toBe(1);
  });

  it("尝试链：合计只算有记录的耗时，并如实标注部分缺失", () => {
    expect(attemptsTotalMs([{ ms: 310 }, { ms: 1_500 }])).toEqual({ total: 1_810, partial: false });
    expect(attemptsTotalMs([{ ms: 310 }, {}])).toEqual({ total: 310, partial: true });
    expect(attemptsTotalMs([])).toEqual({ total: undefined, partial: false });
  });

  it("Rust 形状：尝试链顺序、TA 码在 reason、body_shape 直接可读", () => {
    const detail = {
      row: rustRow(),
      attempts: [
        { channel: "ch-bytedance", lane: "lane-direct", status: 429, ms: 310, reason: "TA-3002" },
        { channel: "ch-commandcode", lane: "lane-jp", node: "东京-03", status: 200, ms: 1_500 },
      ],
      header_diff: { added: [], removed: [], changed: [] },
      body_shape: { messages: "array[3]" },
      // 契约里没有、后端多给的字段（例如 body_meta）必须被**点出来**，而不是默默丢掉
      body_meta: { truncated: false },
    } as unknown as TraceDetail;

    const view = readTraceDetail(detail);
    expect(view.attempts.map((a) => a.channel)).toEqual(["ch-bytedance", "ch-commandcode"]);
    expect(view.attempts[0]?.reason).toBe("TA-3002");
    expect(view.bodyShape).toEqual({ messages: "array[3]" });
    expect(view.extraKeys).toContain("body_meta");
  });

  it("契约形状：body_shape_in 也能当请求体结构读出来", () => {
    const detail = {
      row: contractRow(),
      attempts: [{ channel_id: "ch-a", lane_id: "lane-1", status: 500, ms: 10, reason: null }],
      header_diff: {},
      body_shape_in: { model: "string" },
      body_shape_out: { id: "string" },
    } as unknown as TraceDetail;

    const view = readTraceDetail(detail);
    expect(view.attempts[0]?.channel).toBe("ch-a");
    expect(view.bodyShape).toEqual({ model: "string" });
    expect(view.bodyShapeOut).toEqual({ id: "string" });
    expect(view.headerDiff?.count).toBe(0);
  });
});

// ================================================================ ④ 无数据（诚实空状态）

describe("无数据：给诚实的空状态，不给假表格 / 假数字", () => {
  it("没有行时：[判据 5] 空态只有 1 行说明，不画表格、也不重复表单里的「重置」", () => {
    renderBody();

    expect(screen.getByTestId("logs-empty")).toBeTruthy();
    expect(screen.getByText(zhCN["logs.empty.title"])).toBeTruthy();
    // 1 行结论（原来那三行"三种可能"的散文已删 —— 判据 5）
    const empty = screen.getByTestId("logs-empty");
    expect(empty.querySelectorAll("p")).toHaveLength(1);
    // [第 2 轮 · 判据 A] 空态不再自带「重置」（同一屏的筛选表单里已经有一个同标签同动作的）
    expect(screen.queryByTestId("logs-empty-reset")).toBeNull();
    expect(empty.querySelectorAll("button")).toHaveLength(0);
    // 空状态下不许有表格/行
    expect(screen.queryByTestId("logs-table")).toBeNull();
    expect(screen.queryAllByTestId("logs-row")).toHaveLength(0);
  });

  it("off 的后果进了查询条件的问号（判据 2）；检索上限这类**数值**留在版面上", () => {
    renderBody();
    // off 的后果是一整句解释 → 进 tooltip，全文一个字没丢（aria-label 就是全文）
    const help = screen.getByTestId("logs-filter-help").getAttribute("aria-label") ?? "";
    expect(help).toContain(zhCN["logs.filter.offHint"]);
    expect(screen.queryByText(zhCN["logs.filter.offHint"])).toBeNull();
    // 数值留在版面上（缩短后仍带同一个数）
    expect(screen.getByText(/最近 2,000 条候选/)).toBeTruthy();
  });

  it("加载中显示查询中，不显示空状态（不把还没查完说成没有数据）", () => {
    const props = makeProps();
    renderBody({ query: { ...props.query, isLoading: true } });
    expect(screen.getByText("查询中…")).toBeTruthy();
    expect(screen.queryByTestId("logs-empty")).toBeNull();
  });

  it("读占用失败：把原因显示出来，不吞", () => {
    renderBody({ log: { ...makeProps().log, statsError: new Error("boom") } });
    expect(screen.getByTestId("logs-stats-error").textContent).toContain("boom");
  });

  it("当前模式与保留期没有读取接口 → 显示未读取并说明原因，不猜一个值", () => {
    renderBody();
    expect(screen.getByText(/^当前模式：未读取/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：未读取/)).toBeTruthy();
    expect(screen.getByText(/最近日志行记录时的模式：未知（没有可读的日志行）/)).toBeTruthy();
    cleanup();
    // 本次会话写过之后，显示的必须是"写过的那个值"，而不是假装读到了当前值
    // （断言只钉"当前模式：<值>（本次会话已写入…）"这个形状，不把后半句何时清理抄一遍）
    renderBody({ log: { ...makeProps().log, savedMode: "full", savedRetention: "7d" } });
    expect(screen.getByText(/^当前模式：full（本次会话已写入/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：7 天（本次会话已写入/)).toBeTruthy();
  });

  it("模式与保留期都要先点选才能保存（不会把未选的当成默认值写过去）", () => {
    renderBody();
    // L7：这两块在默认收起的折叠区里 → 先展开（`getByRole` 只认可见的内容）
    expandAdvanced();
    expect((screen.getByRole("button", { name: "保存模式" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(
      (screen.getByRole("button", { name: "保存保留期" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    cleanup();

    const onModeChange = vi.fn();
    renderBody({ log: { ...makeProps().log, mode: "off", onModeChange } });
    // 第二次渲染又是一棵新树（折叠区回到默认收起）→ 再展开一次
    expandAdvanced();
    fireEvent.click(screen.getByRole("button", { name: "off" }));
    expect(onModeChange).toHaveBeenCalledWith("off");
    // 选中之后保存按钮才可用（选中态用强调色，不是靠猜）
    expect((screen.getByRole("button", { name: "保存模式" }) as HTMLButtonElement).disabled).toBe(
      false,
    );
  });
});

// ================================================================ ⑤ 有数据

describe("有数据：两种后端形状的行都渲染出来", () => {
  const rows = [readLogRow(contractRow()), readLogRow(rustRow({ request_id: "req_rust_0003" }))];

  it("契约形状与 Rust 形状的行都出现在表里（不是整列 —）", () => {
    renderBody({ query: { ...makeProps().query, rows, total: 2 } });

    expect(screen.getByTestId("logs-table")).toBeTruthy();
    expect(screen.getAllByTestId("logs-row")).toHaveLength(2);

    // 契约形状：耗时/token 取 latency_ms / total_tokens
    expect(screen.getByText("req_contract_0001")).toBeTruthy();
    expect(screen.getByText("900ms")).toBeTruthy();
    expect(screen.getByText("2,400")).toBeTruthy();

    // Rust 形状：耗时取 total_ms、错误取 error、通道取 chosen_channel
    expect(screen.getByText("req_rust_0003")).toBeTruthy();
    expect(screen.getByText("30s")).toBeTruthy();
    expect(screen.getByText("ch-commandcode")).toBeTruthy();
    expect(screen.getByText("全部通道失败")).toBeTruthy();
  });

  it("结果级别用圆点 + 文字，缺 outcome 时显示未知而不是正常", () => {
    const noOutcome = readLogRow(
      contractRow({ request_id: "req_no_outcome", outcome: undefined }),
    );
    renderBody({ query: { ...makeProps().query, rows: [...rows, noOutcome], total: 3 } });

    // 只看表格区（筛选表单里也有"成功"这个 checkbox 文案）
    const table = within(screen.getByTestId("logs-table"));
    expect(table.getByText("成功")).toBeTruthy();
    expect(table.getByText("失败")).toBeTruthy();
    expect(table.getByText("未知")).toBeTruthy();
  });

  it("结果区显示实际生效的条件（假筛选无处藏身）", () => {
    const applied: LogsFilter = {
      preset: "today",
      requestId: "",
      errorCode: "TA-4001",
      levels: ["failed"],
    };
    renderBody({ query: { ...makeProps().query, rows, applied, total: 2 } });

    const appliedText = screen.getByTestId("logs-applied").textContent ?? "";
    expect(appliedText).toContain("今天");
    expect(appliedText).toContain("失败");
    expect(appliedText).toContain("TA-4001");
  });

  it("有游标才给加载更多；点了会回调", () => {
    const onLoadMore = vi.fn();
    renderBody({ query: { ...makeProps().query, rows, canLoadMore: true, onLoadMore } });
    fireEvent.click(screen.getByRole("button", { name: "加载更多" }));
    expect(onLoadMore).toHaveBeenCalledTimes(1);
  });

  it("检索模式：说明后端不分页（否则用户会一直等加载更多）", () => {
    renderBody({
      query: {
        ...makeProps().query,
        rows,
        applied: { ...DEFAULT_FILTER, requestId: "req_contract_0001" },
      },
    });
    expect(screen.getByText(/检索模式：一次返回全部匹配/)).toBeTruthy();
  });

  it("展开某一行才注入轨迹面板（不为每行都打后端）", () => {
    const renderTraceSpy = vi.fn((id: string) => <div data-testid="fake-trace">{id}</div>);
    const props = makeProps();
    renderBody({
      query: { ...props.query, rows },
      expanded: { requestId: "req_rust_0003", onToggle: () => {}, renderTrace: renderTraceSpy },
    });

    expect(renderTraceSpy).toHaveBeenCalledWith("req_rust_0003");
    expect(screen.getByTestId("fake-trace").textContent).toBe("req_rust_0003");
    // 展开区里这一行的原始字段（未清洗）
    expect(screen.getByTestId("logs-row-raw").textContent).toContain("req_rust_0003");
    // 「原样显示、未清洗」是解释→ 收进问号，正文不再内联；下面的原文一个字没动
    expect(screen.getByTestId("logs-row-raw-help").getAttribute("aria-label")).toBe(
      zhCN["logs.trace.rawHint"],
    );
    expect(screen.queryByText(zhCN["logs.trace.rawHint"])).toBeNull();
    // 只有展开的那一行有面板
    expect(screen.getAllByTestId("fake-trace")).toHaveLength(1);
  });

  it("清空是破坏性操作：先弹确认，未确认前绝不回调", () => {
    const onClear = vi.fn();
    renderBody({ log: { ...makeProps().log, onClear } });

    // L7：清空按钮在折叠区里（确认框归 MUI Dialog，展开后才点得到）
    expandAdvanced();
    fireEvent.click(screen.getByRole("button", { name: "清空…" }));
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByText(/清空不可恢复/)).toBeTruthy();
    expect(onClear).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("radio", { name: /只清 body 与头/ }));
    fireEvent.click(screen.getByRole("button", { name: "确认清空" }));
    expect(onClear).toHaveBeenCalledWith("bodies_only");
  });

  it("查询失败：显示 TA 码 + 重试入口", () => {
    renderBody({
      query: {
        ...makeProps().query,
        error: new Error("[TA-9001] 非 Tauri 环境且未注册 mock：log_query"),
      },
    });
    const node = screen.getByTestId("logs-error");
    expect(node.textContent).toContain("TA-9001");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });
});

// ================================================================ ⑤′ 导出诊断包（log_export）

/**
 * 导出是真入口，所以这里钉三件最容易作假的事：
 *
 * 1. **路径没有默认值**：注册表里没有任何命令上报日志/数据目录，所以输入框必须是空的、
 *    空着时按钮禁用 —— 前端不许硬编一个 AppData 路径（那是在编造"后端会写到哪"）。
 * 2. **落盘路径以回执为准**：后端会把**目录**解析成 `tokenagency-diagnostic-<时间戳>.zip`，
 *    所以入参根本不是落点。有了回执就显示回执里的 `path`/`bytes`。
 * 3. **没有回执就只说"已请求"**：这条是**防御分支**（`log_export` 的契约已经是
 *    `LogExportReceipt { path, bytes }`，真机上正常都会回执）—— 但只要后端回了空 path
 *    或退回 void 形态，就绝不许把"我们发过请求"说成"已经导出成功"。
 */
describe("[task-23 B] 导出诊断包：路径不是前端编的，落盘以后端回执为准", () => {
  async function typeExportPath(path: string): Promise<void> {
    const box = await screen.findByTestId("logs-export-path");
    fireEvent.change(within(box).getByRole("textbox"), { target: { value: path } });
  }

  it("路径空着：输入框就是空的（没有编出来的默认值），按钮禁用，点了也不发请求", async () => {
    renderPage();
    // L7：导出块默认收起 —— 这里要看"看得见的输入框"，所以先展开
    expandAdvanced();

    const box = await screen.findByTestId("logs-export-path");
    const input = within(box).getByRole("textbox") as HTMLInputElement;
    expect(input.value).toBe("");

    const run = screen.getByTestId("logs-export-run") as HTMLButtonElement;
    expect(run.disabled).toBe(true);
    fireEvent.click(run);
    expect(exportCalls).toEqual([]);
    expect(screen.queryByTestId("logs-export-written")).toBeNull();
    expect(screen.queryByTestId("logs-export-requested")).toBeNull();
  });

  it("后端回执：显示回执里的落盘路径与字节数，而不是自己填的那个目录（真机上的常见路径）", async () => {
    exportResult = { path: "D:\\out\\tokenagency-diagnostic-1770000000000.zip", bytes: 4096 };
    renderPage();
    expandAdvanced(); // L7：导出块默认收起
    await typeExportPath("D:\\out");

    fireEvent.click(screen.getByTestId("logs-export-run"));
    await waitFor(() => expect(exportCalls.length).toBe(1));
    expect(exportCalls[0]).toEqual({ query: expect.anything(), path: "D:\\out" });

    const written = await screen.findByTestId("logs-export-written");
    expect(written.getAttribute("data-path")).toBe(
      "D:\\out\\tokenagency-diagnostic-1770000000000.zip",
    );
    expect(written.getAttribute("data-bytes")).toBe("4096");
    expect(written.textContent).toContain("tokenagency-diagnostic-1770000000000.zip");
    expect(written.textContent).toContain("4,096");
    expect(screen.queryByTestId("logs-export-requested")).toBeNull();
  });

  it("没有回执（防御分支：后端返回空 path，或退回老 void 形态）：只说「已请求」，并明说不能确认落盘", async () => {
    exportResult = null;
    renderPage();
    expandAdvanced(); // L7：导出块默认收起
    await typeExportPath("D:\\out");

    fireEvent.click(screen.getByTestId("logs-export-run"));

    const requested = await screen.findByTestId("logs-export-requested");
    expect(requested.getAttribute("data-path")).toBe("D:\\out");
    // 没有回执就只说"已请求"，并让用户自己去核对（不许说"已写出"）
    expect(requested.textContent).toMatch(/没有回执/);
    expect(screen.queryByTestId("logs-export-written")).toBeNull();
    // 整页不许出现"已写出"（那是只有回执才配说的话）
    expect(document.body.textContent).not.toMatch(/已写出/);
  });

  it("导出失败：原样显示错误码（诊断包命中敏感串时后端就是中止导出）", async () => {
    exportError = new TokenAgencyError({
      code: "TA-8001",
      message: "诊断包导出被拒绝（测试夹具）",
    });
    renderPage();
    expandAdvanced(); // L7：导出块默认收起
    await typeExportPath("D:\\out");

    fireEvent.click(screen.getByTestId("logs-export-run"));

    const err = await screen.findByTestId("logs-export-error");
    expect(err.textContent).toMatch(/\[TA-8001\]/);
    expect(err.textContent).toMatch(/诊断包导出被拒绝/);
    expect(screen.queryByTestId("logs-export-written")).toBeNull();
    expect(screen.queryByTestId("logs-export-requested")).toBeNull();
  });

  it("导出用的是**当前生效条件**：草稿里填了但没点查询 → 不进 query、也不进说明", async () => {
    renderPage();
    expandAdvanced(); // L7：导出块默认收起（下面的按钮要"点得到"）
    await typeExportPath("D:\\out");

    // 草稿：填了请求 ID，但**没有**点「查询」
    fireEvent.change(screen.getByLabelText("请求 ID"), {
      target: { value: "req_draft_should_not_be_used" },
    });

    fireEvent.click(screen.getByTestId("logs-export-run"));
    await waitFor(() => expect(exportCalls.length).toBe(1));

    const query = exportCalls[0]!["query"] as Record<string, unknown>;
    expect(query["search"]).toBeUndefined();
    expect(screen.getByTestId("logs-export-path").parentElement?.textContent ?? "").not.toContain(
      "req_draft_should_not_be_used",
    );
    expect(document.body.textContent).not.toContain("req_draft_should_not_be_used");
  });
});

// ================================================================ ⑥ 轨迹面板（C4）

type PanelProps = Parameters<typeof TracePanel>[0];

function renderPanel(detail: PanelProps["detail"], extra: Partial<PanelProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <TracePanel detail={detail} isLoading={false} error={undefined} onRetry={() => {}} {...extra} />
    </ThemeProvider>,
  );
}

describe("[C4] 轨迹面板：同一 request_id 的跨组件轨迹", () => {
  const view = readTraceDetail({
    row: rustRow(),
    attempts: [
      { channel: "ch-bytedance", lane: "lane-direct", status: 429, ms: 310, reason: "TA-3002" },
      { channel: "ch-commandcode", lane: "lane-jp", node: "东京-03", status: 200, ms: 1_500 },
    ],
    header_diff: { added: ["x-extra"], removed: [], changed: [] },
    body_shape: { messages: "array[3]" },
  } as unknown as TraceDetail);

  it("按尝试顺序画出尝试链，并显示网关判定的 TA 码", () => {
    renderPanel(view);

    const attempts = screen.getAllByTestId("logs-attempt");
    expect(attempts).toHaveLength(2);
    expect(attempts[0]?.textContent).toContain("尝试 1");
    expect(attempts[0]?.textContent).toContain("429");
    expect(attempts[0]?.textContent).toContain("310ms");
    expect(attempts[0]?.textContent).toContain("TA-3002");
    expect(attempts[1]?.textContent).toContain("尝试 2");
    expect(attempts[1]?.textContent).toContain("东京-03");
    expect(attempts[1]?.textContent).toContain("200");
  });

  it("header_diff 非空按网关有 bug 报警；空与缺要能区分", () => {
    renderPanel(view);
    expect(screen.getByTestId("logs-header-diff").textContent).toContain("1 处差异");
    expect(screen.getByText(/非空说明网关有 bug/)).toBeTruthy();
    cleanup();

    renderPanel(
      readTraceDetail({ row: rustRow(), attempts: [], header_diff: {} } as unknown as TraceDetail),
    );
    expect(screen.getByTestId("logs-header-diff").textContent).toContain("无差异");
    cleanup();

    renderPanel(readTraceDetail({ row: rustRow(), attempts: [] } as unknown as TraceDetail));
    expect(screen.getByTestId("logs-header-diff").textContent).toContain("未知");
  });

  it("没有尝试记录时说清楚；原始字段始终打印", () => {
    renderPanel(readTraceDetail({ row: rustRow(), attempts: [] } as unknown as TraceDetail));
    expect(screen.getByText("没有尝试记录。")).toBeTruthy();
    expect(screen.getByTestId("logs-trace-raw").textContent).toContain("req_rust_0002");
  });

  it("后端说没有这条时给诚实的缺失态，而不是空白或假数据", () => {
    renderPanel(null);
    expect(screen.getByTestId("logs-trace-missing")).toBeTruthy();
    expect(screen.getByText(/没有这条请求的轨迹/)).toBeTruthy();
  });

  it("读轨迹失败：带 TA 码 + 重试", () => {
    renderPanel(undefined, { error: new Error("[TA-9001] 未注册 mock：log_get_trace") });
    expect(screen.getByTestId("logs-trace-error").textContent).toContain("TA-9001");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });

  it("读取中不与没有这条混为一谈", () => {
    renderPanel(undefined, { isLoading: true });
    expect(screen.getByText("读取轨迹…")).toBeTruthy();
    expect(screen.queryByTestId("logs-trace-missing")).toBeNull();
  });
});

// ================================================================ ⑦ 整页（浏览器开发路径 mock）

describe("整页在 mock 基准场景下能用（不依赖 Rust/Tauri）", () => {
  it("渲染页头与 mock 的日志行；展开后取到轨迹并打印原始字段", async () => {
    renderPage();

    // 页面标题是 <h1>（B-7），页头已落地 → 路由不再走占位壳
    expect((await screen.findByText("请求日志")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();

    // mock 的请求日志（[03 §0] 基准场景）
    expect(await screen.findByText("req_mock_0000")).toBeTruthy();

    // 展开第一行 → `log_get_trace` 走同一个 mock，轨迹面板出现
    const expandButtons = await screen.findAllByRole("button", { name: "展开" });
    fireEvent.click(expandButtons[0]!);
    await waitFor(() => expect(screen.getByTestId("logs-trace")).toBeTruthy());
  });
});

// ================================================================ ⑧ 文案回归

describe("文案里不留未替换的占位符", () => {
  it("渲染出来的正文里不该出现双花括号占位符", () => {
    renderBody({ query: { ...makeProps().query, rows: [readLogRow(contractRow())], total: 1 } });
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑨ 日志导出：两种后端形态都要如实显示

/**
 * 这一段测的是**纯函数** `exportOutcome` 本身（UI 接线在上面 ⑤′ 那一段）。
 *
 * 契约现在是 `LogExportReceipt { path, bytes }`（c9264cb，void → 回执），所以真机上
 * 正常都会走 `written`；`requested` 是**防御分支**：后端退回 void 形态、或回执里没有
 * 非空 path 时，绝不能拿我们自己传进去的路径冒充"后端确认落盘"。
 */
describe("[task-23] log_export 的返回形态：回执路径与防御的 void 形态都要能如实显示", () => {
  it("后端回执了 { path, bytes } → written，用**后端给的**路径与字节数", () => {
    const out = exportOutcome(
      { path: "C:\\Users\\me\\AppData\\Local\\TokenAgency\\logs\\export-1.csv", bytes: 2048 },
      "C:\\填错的路径.csv",
    );
    expect(out.kind).toBe("written");
    // 必须是后端那个路径，而不是我们传进去的 —— 否则"真实落盘路径"就成了自述
    expect(out.path).toBe("C:\\Users\\me\\AppData\\Local\\TokenAgency\\logs\\export-1.csv");
    expect(out.kind === "written" ? out.bytes : undefined).toBe(2048);
  });

  it("后端退回 void 形态（防御分支）→ requested，只声称「已请求」，不声称已写入", () => {
    for (const result of [undefined, null, {}, []]) {
      const out = exportOutcome(result, "C:\\logs\\export.csv");
      expect(out.kind, `result=${JSON.stringify(result)}`).toBe("requested");
      expect(out.path).toBe("C:\\logs\\export.csv");
    }
  });

  it("回执对象但没带 path → 退化成 requested（不许拿我们自己的路径冒充后端确认）", () => {
    const out = exportOutcome({ bytes: 12 }, "C:\\logs\\export.csv");
    expect(out.kind).toBe("requested");
    expect(out.path).toBe("C:\\logs\\export.csv");
  });

  it("bytes 不是数字时不硬编一个 0（宁可只报路径）", () => {
    const out = exportOutcome({ path: "C:\\a.csv", bytes: "2048" }, "C:\\b.csv");
    expect(out.kind).toBe("written");
    expect(out.kind === "written" ? out.bytes : "x").toBeUndefined();
  });
});

// ================================================================ ⑩ 长篇说明收进「问号圆圈」

/**
 *：**解释**收进问号圆圈，**红线**留在版面上。
 *
 * 这一页的红线特别多（用户原话之外的硬约束）：空态三种可能、off 的后果、检索上限的**数值**、
 * "未读取"这种状态结论、以及"导出路径没有默认值"这种"按钮为什么禁用"的说明 ——
 * 它们一句都不许进 tooltip。
 */
describe("长篇说明收进问号圆圈，红线留在版面上", () => {
  it("筛选区：三条成段解释全部收进 logs-filter-help，正文一条都不内联", () => {
    renderBody();

    // 自证：确实是"一句解释"（而不是一个词）才收。
    // 门槛从 20 降到 12 是**文案被有意缩短**的结果：极简改造把
    // `logs.filter.searchExclusive` 从成句说明压到 16 字（"请求 ID 与错误码只能填一个。"），
    // 20 这个数字随即过期。留下的判据仍然成立且更贴近本仓既有约定：
    // 12 是"长文案"的既有门槛（见 duplicateCopy.guard），它挡住"把单个词塞进 tooltip"。
    for (const key of [
      "logs.filter.errorCodeHint",
      "logs.filter.searchExclusive",
      "logs.filter.levelHint",
    ] as const) {
      expect(zhCN[key].length).toBeGreaterThanOrEqual(12);
      expect(screen.queryByText(zhCN[key])).toBeNull();
    }

    const help = screen.getByTestId("logs-filter-help");
    const label = help.getAttribute("aria-label") ?? "";
    for (const key of [
      "logs.filter.errorCodeHint",
      "logs.filter.searchExclusive",
      "logs.filter.levelHint",
    ] as const) {
      expect(label).toContain(zhCN[key]);
    }
  });

  it("占用区：这组数字只算请求日志表 —— 收进 logs-storage-help", () => {
    renderBody();

    const text = zhCN["logs.storage.scope"];
    expect(text.length).toBeGreaterThan(12);

    const help = screen.getByTestId("logs-storage-help");
    expect(help.getAttribute("aria-label")).toBe(text);
    expect(screen.queryByText(text)).toBeNull();
  });

  it("轨迹面板：面板说明收进 logs-trace-help", () => {
    renderPanel(readTraceDetail({ row: rustRow(), attempts: [] } as unknown as TraceDetail));

    const text = zhCN["logs.trace.subtitle"];
    expect(text.length).toBeGreaterThan(12);

    const help = screen.getByTestId("logs-trace-help");
    expect(help.getAttribute("aria-label")).toBe(text);
    expect(screen.queryByText(text)).toBeNull();

    // 同一面板里的「原始字段」说明：同样收进问号，正文不再内联
    expect(screen.getByTestId("logs-trace-raw-help").getAttribute("aria-label")).toBe(
      zhCN["logs.trace.rawHint"],
    );
    expect(screen.queryByText(zhCN["logs.trace.rawHint"])).toBeNull();
  });

  it("红线仍然内联：空态（1 行）/ 检索上限的数值 / 未读取 / 导出路径没有默认值", () => {
    renderBody();

    // [极简 · 判据 5] 空态只说"怎么开始"：1 行（原来是 4 行散文）；
    // [第 2 轮 · 判据 A] 也不再自带「重置」—— 那个动作在筛选表单里已经有了
    expect(screen.getByText(zhCN["logs.empty.title"])).toBeTruthy();
    expect(screen.queryByTestId("logs-empty-reset")).toBeNull();
    // off 的后果是一整句解释（判据 2）→ 已并进查询条件的问号；正文一个字没丢（aria-label 里）
    expect(screen.getByTestId("logs-filter-help").getAttribute("aria-label")).toContain(
      zhCN["logs.filter.offHint"],
    );
    // 数值 + 单位：检索最多扫多少条候选 —— 数字留在版面上，但整句缩到 ≤36 字
    expect(screen.getByText(/最近 2,000 条候选/)).toBeTruthy();
    // 状态结论：没有读取接口 → "未读取"
    expect(screen.getByText(/^当前模式：未读取/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：未读取/)).toBeTruthy();
    // "导出路径没有默认值"（按钮为什么禁用）必须一眼看到
    expect(screen.getByText(zhCN["logs.export.path.hint"])).toBeTruthy();

    // 这一页正文里只有上面那两处被收走 —— 没有"一排问号"
    // （行详情里的 `logs-row-raw-help` 由 ③「展开某一行」那条用例单独钉住）
    expect(
      screen.queryAllByTestId(/-help$/).map((el) => el.getAttribute("data-testid")),
    ).toEqual(["logs-filter-help", "logs-storage-help"]);
  });
});

// ================================================================ L7：默认收起

/**
 * L7：记录与留存那一节里的四块（存储 / 模式 / 保留 / 导出）**默认收起**。
 *
 * 这条用例钉的是"不许把默认值改成展开来换绿"：
 * ① 默认 `aria-expanded=false`，里面的操作按钮**不在可访问性树里**（用户看不见就点不到）；
 * ② 但内容**一个字节都没被卸载**（`logs-export-path` 照旧在 DOM 里）—— 反例是 MUI
 *    `Accordion` 的 `unmountOnExit`，它会把真功能从 DOM 里拿走（第一次尝试就栽在这）；
 * ③ 点一下开关，同一批内容立刻可操作（`保存模式` 出现在可访问性树里）。
 */
describe("L7 记录与留存四块默认收起（可展开，内容不卸载）", () => {
  it("默认收起：内容挂载但不可见；点开关后同一批内容可操作", () => {
    renderBody();

    const toggle = screen.getByTestId("logs-advanced-toggle");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    // 收起时：按钮看不见（不在可访问性树里），但节点还在
    expect(screen.queryByRole("button", { name: "保存模式" })).toBeNull();
    expect(screen.getByTestId("logs-export-path")).toBeTruthy();

    expandAdvanced();
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: "保存模式" })).toBeTruthy();
    expect(screen.getByTestId("logs-export-path")).toBeTruthy();
  });
});

// ================================================================ 一级界面省略

describe("请求 id / 模型 / 通道：省略后全文仍在 DOM（错误原文不省略）", () => {
  it("三个长值都整段留在 DOM 里；没溢出时一个提示都不挂", () => {
    const requestId = "req_contract_0001-2026-07-18-preview-long";
    const model = "deepseek-v4.1flash-2026-07-18-preview";
    const channel = "ch-bytedance-2026-07-18-preview-a";
    renderBody({
      query: {
        ...makeProps().query,
        rows: [readLogRow(contractRow({ request_id: requestId, model, channel_id: channel }))],
      },
    });

    expect(screen.getByTestId("logs-request-id-ellipsis").textContent).toBe(requestId);
    expect(screen.getByTestId("logs-model-ellipsis").textContent).toBe(model);
    expect(screen.getByTestId("logs-channel-ellipsis").textContent).toBe(channel);
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(screen.getByTestId("logs-model-ellipsis").getAttribute("tabindex")).toBeNull();
  });
});

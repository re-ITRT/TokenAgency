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
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import LogsPage, { LogsBody, type LogsBodyProps } from "@/pages/logs";
import {
  DEFAULT_FILTER,
  PAGE_SIZE,
  attemptsTotalMs,
  buildLogQuery,
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
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
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
    // 冻结契约的字段（bindings.ts:848）
    expect(wire["range"]).toEqual({ preset: "today" });
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
  it("没有行时：说明三种可能（off / 没有流量 / 条件太窄），且不画表格", () => {
    renderBody();

    expect(screen.getByTestId("logs-empty")).toBeTruthy();
    expect(screen.getByText("没有可显示的日志行")).toBeTruthy();
    expect(screen.getByText(/日志模式可能是 off/)).toBeTruthy();
    expect(screen.getByText(/也可能只是这段时间没有请求经过网关/)).toBeTruthy();
    expect(screen.getByText(/或者条件太窄/)).toBeTruthy();
    // 空状态下不许有表格/行
    expect(screen.queryByTestId("logs-table")).toBeNull();
    expect(screen.queryAllByTestId("logs-row")).toHaveLength(0);
  });

  it("off 的后果写在筛选区（否则用户会以为页面坏了）", () => {
    renderBody();
    expect(screen.getByText(/off 模式不写任何请求日志/)).toBeTruthy();
    expect(screen.getByText(/检索最多在这段时间的最近 2,000 条候选里匹配/)).toBeTruthy();
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
    renderBody({ log: { ...makeProps().log, savedMode: "full", savedRetention: "7d" } });
    expect(screen.getByText(/^当前模式：full（本次会话已写入后端）/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：7 天（本次会话已写入后端/)).toBeTruthy();
  });

  it("模式与保留期都要先点选才能保存（不会把未选的当成默认值写过去）", () => {
    renderBody();
    expect((screen.getByRole("button", { name: "保存模式" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(
      (screen.getByRole("button", { name: "保存保留期" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    cleanup();

    const onModeChange = vi.fn();
    renderBody({ log: { ...makeProps().log, mode: "off", onModeChange } });
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
    // 只有展开的那一行有面板
    expect(screen.getAllByTestId("fake-trace")).toHaveLength(1);
  });

  it("清空是破坏性操作：先弹确认，未确认前绝不回调", () => {
    const onClear = vi.fn();
    renderBody({ log: { ...makeProps().log, onClear } });

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

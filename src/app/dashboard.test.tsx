// @vitest-environment jsdom
/**
 * 概览/看板的验收。**重点是"诚实"，不是"好看"**：
 *
 * 1. 契约里没有的判据（provider 是否加载成功）**必须显示"未知"** —— 即使内核 `state === "ready"`
 *    也不许点绿灯（B2 的核心，"不拿不确定冒充确定"）；
 * 2. 没有数据时给**空状态/骨架**，不许用 0 或别的数字填坑；
 * 3. 错误必须带 `TA-xxxx`，且真的用语义色画出来（写成不存在的调色板路径时这里会红）。
 *
 * 另外它顺带钉住"页面已落地"这件事：路由不再走占位壳（`routes.test.tsx` 负责整体一致性）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import DashboardPage, { DashboardBody, type DashboardBodyProps } from "@/pages/dashboard";
import { TokenAgencyError } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";
import type { AppStatus, DashboardData } from "@/shared/bindings";
import { buildTheme, tokens } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
});

function renderBody(props: Partial<DashboardBodyProps>) {
  const full: DashboardBodyProps = {
    status: undefined,
    data: undefined,
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    ...props,
  };
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <DashboardBody {...full} />
    </ThemeProvider>,
  );
}

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <DashboardPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

const KERNEL_READY_STATUS: AppStatus = {
  gateway: "running",
  kernel: { state: "ready", version: "1.19.31", pid: 1234, restarts: 0, last_error: null },
  version: "0.1.0",
  uptime_s: 90,
  active_policy_id: null,
  default_lane_id: null,
  current_node: null,
};

/** 最小可用看板数据（只为测渲染分支；数字是**测试夹具**，不是产品里的默认值）。 */
function makeData(overrides: Partial<DashboardData> = {}): DashboardData {
  return {
    status_bar: {
      gateway: "running",
      policy: { id: "pol-1", name: "默认策略" },
      lane: { id: "lane-1", name: "日本线", node: "东京-03", latency_ms: 82 },
    },
    cards: {
      requests: { value: 1_284, prev: 1_120 },
      tokens: { value: 12_400_000, prev: 11_050_000 },
      cost: { amount: 18.62, currency: "CNY", prev: 16.4 },
      success_rate: 0.992,
    },
    sparklines: { requests: [1, 2, 3], tokens: [1, 2, 3], cost: [1, 2, 3], success_rate: [1, 2, 3] },
    trend: [],
    lanes: [],
    live: [],
    top_models: [],
    cache: { hit_rate: 0.31, saved_amount: 4.2, currency: "CNY" },
    unpriced_models: [],
    ...overrides,
  } as DashboardData;
}

/** `#RRGGBB` → `rgb(r, g, b)`（`getComputedStyle` 的返回形态）。 */
function rgbOf(hex: string): string {
  const n = Number.parseInt(hex.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
}

describe("看板：诚实优先", () => {
  it("拿不到任何数据时：每一维都显示「未知」，不给编造的数字", () => {
    const { container } = renderBody({});

    // 可用性四行（网关/内核/provider 检查/出口）都得是"未知"
    expect(screen.getAllByText("未知").length).toBeGreaterThanOrEqual(3);
    // 数字卡不给 0 填坑，给空状态
    expect(screen.getAllByText("暂无数据").length).toBeGreaterThanOrEqual(1);
    expect(container.textContent).not.toContain("1,284");
  });

  it("内核 ready **也不**把 provider 检查点成绿灯：那一维显示「未知」", () => {
    renderBody({ status: KERNEL_READY_STATUS });

    // 内核状态来自契约，如实显示"就绪"
    expect(screen.getByText("就绪")).toBeTruthy();

    // 但 provider 加载检查没有契约判据 → 必须仍然是"未知"
    const label = screen.getByText("provider 加载检查");
    const row = label.parentElement;
    expect(row?.textContent).toContain("未知");
    // 整个可用性面板里不该出现"provider……正常"这种断言
    expect(row?.textContent).not.toContain("正常");
  });

  it("内核状态如实反映契约（starting → 启动中；没有状态 → 未知）", () => {
    renderBody({ status: { ...KERNEL_READY_STATUS, kernel: { ...KERNEL_READY_STATUS.kernel, state: "starting" } } });
    expect(screen.getByText("启动中")).toBeTruthy();
    cleanup();

    renderBody({ status: undefined });
    expect(screen.queryByText("就绪")).toBeNull();
  });

  it("错误必须带 TA 码，并真的用语义色画出来（调色板路径写错会红）", () => {
    const err = new TokenAgencyError({ code: "TA-5001", message: "配置校验失败" });
    renderBody({ error: err });

    const node = screen.getByTestId("dashboard-error");
    expect(node.textContent).toContain("TA-5001");
    expect(node.textContent).toContain("配置校验失败");
    // `danger.main` 这种不存在的路径会静默无色 —— 这里钉住真实的语义色
    // （主题开了 cssVariables，所以计算值是 var(...)；两种形态都接受，写错路径两者都不匹配）
    expect(["var(--mui-palette-error-main)", rgbOf(tokens.palette.dark.danger)]).toContain(
      getComputedStyle(node).color,
    );
    // 错误时给重试入口
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });

  it("有数据时：数字卡、出口、未定价提示都按契约渲染", () => {
    renderBody({
      status: KERNEL_READY_STATUS,
      data: makeData({ unpriced_models: ["gpt-5-preview", "claude-next"] }),
    });

    expect(screen.getByText("1,284")).toBeTruthy();
    expect(screen.getByText("12.4M")).toBeTruthy();
    expect(screen.getByText("¥18.62")).toBeTruthy();
    expect(screen.getByText("99.2%")).toBeTruthy();
    expect(screen.getByText(/日本线/)).toBeTruthy();
    // 未定价模型必须明说"成本不完整"（不许让用户以为是全量）
    expect(screen.getByText("2 个模型未定价：成本数字不完整")).toBeTruthy();
  });

  it("整页在 mock 基准场景下能渲染真实数字（浏览器开发路径，不依赖 Rust/Tauri）", async () => {
    renderPage();
    expect(await screen.findByText("1,284")).toBeTruthy();
    expect(await screen.findByText("12.4M")).toBeTruthy();
    // 页面标题是 <h1>（B-7），且页头已落地 → 路由不再走占位壳
    expect((await screen.findByText("概览")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();
  });

  /**
   * 注意：真机缺陷的回归（2026-09-22 release 包）：**没有策略、没有线路**时首页不许崩。
   *
   * Rust 的 `StatusBar.policy` / `.lane` 是 `Option` + `skip_serializing_if = "Option::is_none"`，
   * **没有就整个字段不出现**（新装用户的配置就是这样）。首页曾直接读
   * `data.status_bar.lane.name` → `Cannot read properties of undefined (reading 'name')`
   * → 被 ErrorBoundary 接住、整页显示「出错了」（真机 E2E 的 step1 就是它）。
   *
   * 这个夹具**故意只给 gateway**：本文件此前所有夹具都带着 policy/lane，
   * 所以这条路径一直没被测到 —— 缺陷就是这么溜过去的。
   */
  it("没有策略、没有线路时：出口与策略两行给兜底文案，不抛异常", () => {
    const data = makeData();
    const noPolicyNoLane = {
      ...data,
      status_bar: { gateway: "running" as const },
    } as unknown as DashboardData;

    renderBody({ status: KERNEL_READY_STATUS, data: noPolicyNoLane });

    // 兜底文案与 StatusBar 组件同一口径（既有 i18n 键），而不是空白或 undefined
    expect(screen.getByText("未选策略")).toBeTruthy();
    expect(screen.getByText("直连")).toBeTruthy();
    expect((document.body.textContent ?? "").split("undefined")).toHaveLength(1);
  });
});

// ================================================================ 长篇说明收进「问号圆圈」

describe("provider 加载检查的说明收进问号圆圈，状态结论留在版面上", () => {
  it("说明不再是行内文本：收进 dashboard-provider-check-help，aria-label 就是全文", () => {
    renderBody({ status: KERNEL_READY_STATUS });

    const text = zhCN["dashboard.providerCheck.hint"];
    // 自证：确实是"成段的解释"（≥ 20 字符）
    expect(text.length).toBeGreaterThan(20);

    const help = screen.getByTestId("dashboard-provider-check-help");
    expect(help.getAttribute("aria-label")).toBe(text);
    // 版面上不再有那段正文（正文一个字都没丢：它同时是 aria-label）
    expect(screen.queryByText(text)).toBeNull();

    // §2 第 1 条：状态结论（"未知"）仍然一眼可见，且这一行不许出现"正常"这种断言
    const row = screen.getByText("provider 加载检查").parentElement;
    expect(row?.textContent).toContain("未知");
    expect(row?.textContent).not.toContain("正常");
  });
});

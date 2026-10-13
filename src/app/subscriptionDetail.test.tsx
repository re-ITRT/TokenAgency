// @vitest-environment jsdom
/**
 * **一条订阅的详情页**（`/network/subscriptions/:id`）—— 用户第 321 轮。
 *
 * 用户原话：*"先到网络界面，再去点击一个订阅，里面就可以看到这个订阅的所有节点，
 * 可以打开或者关闭，就和服务商的线路是一样的"* + *"其中自动选择和直连在最上面"* +
 * *"直连是伪节点，但属于一直有的"*。
 *
 * 这一组钉六件事：
 * 1. **点订阅名能进来**（`/network` 的行上有真链接，不是纯文本）；
 * 2. **每个节点一个开关**，写的是 `subscription_nodes_enabled`，入参逐字；
 * 3. **顶部两行（自动选择 / 直连）恒在、在最上面、且开关禁用**；
 * 4. 「还没解析过」与「确实没有节点」是**两档**不同的话；
 * 5. 读失败给**后端原文**（含 TA 码），不画成"没有节点"；
 * 6. 还没解析过时**一个请求都不发**（后端那时返回的是错误，不是空数组）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import NetworkPage from "@/pages/network";
import SubscriptionDetailPage from "@/pages/network/subscription";
import { TokenAgencyError, registerMock } from "@/shared/api/client";
import { registerBaselineMocks } from "@/shared/api/mock";
import { formatDuration, formatLocalTime } from "@/shared/format";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

afterEach(() => {
  cleanup();
});

/**
 * `subscription_list` 的**契约形状**（`SubscriptionView`：实体在 `subscription` 里，
 * 解析元数据在**外层**）。
 *
 * 为什么不能用裸 `Subscription`：`readSubscription`（`model.ts:246-247`）明写
 * "解析结果只在**外层**（`SubscriptionView` 把它们放在实体旁边）" —— 用裸形状的话
 * `last_update_ms` 读不到，页面会正确地判成"还没解析过"。那是 fixture 的错，不是页面的错。
 */
function contractSubscription(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    subscription: {
      id: "sub-jp",
      name: "机场A",
      url: "https://example.com/api/v1/client/subscribe?token=REPLACE_ME",
      user_agent: "clash-verge/v2.0.0",
      format: "auto",
      interval_s: 21_600,
      keep_versions: 3,
      filter: { include: null, exclude: null },
      health_check: { enabled: true, url: null, interval_s: 60, tolerance_ms: 100 },
      enabled: true,
      provider_key: null,
      disabled_nodes: [],
    },
    provider_key: null,
    // 已解析：有时间戳 ⇒ 详情页才敢发 subscription_nodes
    last_update_ms: 1_770_000_000_000,
    parser: "clash-yaml@1",
    error: null,
    ...over,
  };
}

function renderAt(path: string, node: React.ReactElement) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[path]}>
        <ThemeProvider theme={buildTheme("dark")}>{node}</ThemeProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function renderPage(node: React.ReactElement) {
  return renderAt("/", node);
}

describe("/network/subscriptions/:id 一条订阅的节点开关", () => {
  const SUB_ID = "sub-jp";

  beforeEach(() => {
    registerBaselineMocks();
  });

  const NODES = [
    { name: "东京-03", type: "vmess", ok: true, latency_ms: 82 },
    { name: "首尔-02", type: "ss", ok: false, latency_ms: null },
  ];

  /** 与后端同一套语义的有状态 mock：只记"关掉的"，返回改完的完整清单。 */
  function store(over: { nodes?: Record<string, unknown>[] } = {}) {
    const disabled = new Set<string>();
    const calls: Record<string, unknown>[] = [];
    const list = (): Record<string, unknown>[] =>
      (over.nodes ?? NODES).map((node) => ({
        ...node,
        enabled: !disabled.has(String(node.name)),
      }));
    return {
      list,
      calls,
      disabled,
      setEnabled: (args: Record<string, unknown> | undefined): unknown => {
        calls.push(args ?? {});
        const a = (args ?? {}) as { nodes?: string[]; enabled?: boolean };
        for (const name of a.nodes ?? []) {
          if (a.enabled === true) disabled.delete(name);
          else disabled.add(name);
        }
        return list();
      },
    };
  }

  /** 进详情页：给 `:id` 填上订阅 id（用真实路由参数，不是把 id 当 prop 塞进去）。 */
  function renderDetail(): void {
    renderAt(
      `/network/subscriptions/${SUB_ID}`,
      <Routes>
        <Route path="/network/subscriptions/:id" element={<SubscriptionDetailPage />} />
      </Routes>,
    );
  }

  it("节点行：每个节点一个开关，默认全开；关掉一个 → 入参逐字只有那一条", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    const s = store();
    registerMock("subscription_nodes", s.list);
    registerMock("subscription_nodes_enabled", s.setEnabled);

    renderDetail();

    const rows = await screen.findAllByTestId("network-subscription-node-row");
    expect(rows).toHaveLength(2);
    // 节点名来自订阅：**原样显示**（含用户数据里的特殊字符也不改写）。
    // 这两个夹具名字是中文，属于用户数据，所以直接按原文断言。
    expect(rows[0]!.textContent).toContain("东京-03");
    expect(rows[1]!.textContent).toContain("首尔-02");

    const boxes = screen.getAllByTestId("network-subscription-node-enabled");
    expect(boxes).toHaveLength(2);
    const inputs = boxes.map((box) => box.querySelector("input") as HTMLInputElement);
    // 库里存的是"关掉的那些" ⇒ 默认全开
    expect(inputs[0]!.checked).toBe(true);
    expect(inputs[1]!.checked).toBe(true);

    // 关掉第一条：一次调用，nodes 逐字只有它
    fireEvent.click(inputs[0]!);
    await waitFor(() => expect(s.calls).toHaveLength(1));
    expect(s.calls[0]).toEqual({ id: SUB_ID, nodes: ["东京-03"], enabled: false });

    // 界面按**后端返回的清单**变（回填缓存），不是自己猜
    await waitFor(() => {
      const after = screen
        .getAllByTestId("network-subscription-node-enabled")
        .map((box) => (box.querySelector("input") as HTMLInputElement).checked);
      expect(after).toEqual([false, true]);
    });
  });

  it("顶部两行「自动选择 / 直连」**恒在最上面**，且开关是禁用的（不画一个点了没用的开关）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => NODES.map((n) => ({ ...n, enabled: true })));

    renderDetail();
    await screen.findAllByTestId("network-subscription-node-row");

    const auto = screen.getByTestId("network-subscription-builtin-auto");
    const direct = screen.getByTestId("network-subscription-builtin-direct");

    // 文案就是用户说的那两个词
    expect(auto.textContent).toContain(i18n.t("network.subscription.auto"));
    expect(direct.textContent).toContain(i18n.t("network.subscription.direct"));

    // 两行的开关都**禁用** —— 用户说的"无法关闭"就是这个（[15 §2.1]：我们自己不产生 emoji）
    for (const row of [auto, direct]) {
      const input = row.querySelector("input") as HTMLInputElement;
      expect(input).toBeTruthy();
      expect(input.disabled).toBe(true);
      expect(input.checked).toBe(true);
    }

    // 而且**在节点行的前面**（"在最上面"）：DOM 顺序 = 视觉顺序
    const allRows = Array.from(document.querySelectorAll("[data-testid]"))
      .map((el) => el.getAttribute("data-testid"))
      .filter(
        (id) =>
          id === "network-subscription-builtin-auto" ||
          id === "network-subscription-builtin-direct" ||
          id === "network-subscription-node-row",
      );
    expect(allRows[0]).toBe("network-subscription-builtin-auto");
    expect(allRows[1]).toBe("network-subscription-builtin-direct");
  });

  it("还没解析过 → **根本不发** subscription_nodes，给「还没解析」那档文案", async () => {
    // last_update_ms 与 error 都没有 = 没被解析采纳过
    registerMock("subscription_list", () => [
      { ...contractSubscription(), last_update_ms: null, error: null },
    ]);
    let nodesCalls = 0;
    registerMock("subscription_nodes", () => {
      nodesCalls += 1;
      return [];
    });

    renderDetail();

    // 文案是「还没解析过」那一档（不是"没有节点"）
    const empty = await screen.findByTestId("network-subscription-detail-empty");
    expect(empty.textContent).toBe(i18n.t("network.nodes.emptyUnparsed"));
    // 关键：一次都没发（后端在没有已采纳版本时返回的是**错误**，不是空数组）
    expect(nodesCalls).toBe(0);
  });

  it("解析过但确实没有节点 → 换第二档文案（不写「还没解析过」）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => []);

    renderDetail();

    // 先等订阅读回来（节点查询在它之前是 disabled 的）
    const empty = await screen.findByTestId("network-subscription-nodes-empty");
    expect(empty.textContent).toBe(i18n.t("network.nodes.emptyGroup"));
    expect(screen.queryByTestId("network-subscription-detail-empty")).toBeNull();
  });

  it("读节点失败 → 后端原文（含 TA 码）原样显示，不吞、不画成「没有节点」", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => {
      throw new TokenAgencyError({
        code: "TA-9001",
        message: "订阅 sub-jp 还没有已采纳的版本（先用 subscription_refresh 拉一次）",
      });
    });

    renderDetail();

    const err = await screen.findByTestId("network-subscription-detail-error");
    expect(err.textContent).toContain("TA-9001");
    expect(err.textContent).toContain("还没有已采纳的版本");
    expect(screen.queryByTestId("network-subscription-nodes-empty")).toBeNull();
  });

  it("刷新失败要**说出来**（按钮点下去什么都没发生 = 用户只会反复点）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => NODES.map((n) => ({ ...n, enabled: true })));
    registerMock("subscription_refresh", () => {
      throw new TokenAgencyError({ code: "TA-4001", message: "拉取失败：连接超时" });
    });

    renderDetail();
    await screen.findAllByTestId("network-subscription-node-row");

    fireEvent.click(await screen.findByTestId("network-subscription-detail-refresh"));

    const err = await screen.findByTestId("network-subscription-detail-write-error");
    expect(err.textContent).toContain("TA-4001");
    expect(err.textContent).toContain("连接超时");
  });

  it("/network 的订阅名是**真链接**，指向这条订阅的详情页（点得进去）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    renderPage(<NetworkPage />);

    const open = await screen.findByTestId("network-subscription-open");
    expect(open.getAttribute("href")).toBe(`/network/subscriptions/${SUB_ID}`);
  });
});

// ================================================================ 第 326 轮：详情抽屉 + 页头问号
//
// 概览那张表从 14 列砍到 6 列（规则 4），砍掉的格式 / 更新周期 / 保留版本 / 过滤 / 健康检查 /
// 流量 / 组数 / 解析器 / 错误原文**全部搬到这里**逐项列出 —— 信息一个都不许丢。
// 同时"顶部两行为什么不能关"那一整段从单元格里搬进页头那个问号（规则 2）。

describe("[326] 详情页：概览砍掉的参数在这儿；内置行只留一行状态", () => {
  const SUB_ID = "sub-jp";

  beforeEach(() => {
    registerBaselineMocks();
  });

  /** 进详情页（真实路由参数）。`subscription_nodes` 一律给空清单，这一组不测节点开关。 */
  function renderDetail(nodes: Record<string, unknown>[] = []): void {
    registerMock("subscription_nodes", () => nodes);
    registerMock("subscription_traffic", () => []);
    renderAt(
      `/network/subscriptions/${SUB_ID}`,
      <Routes>
        <Route path="/network/subscriptions/:id" element={<SubscriptionDetailPage />} />
      </Routes>,
    );
  }

  it("格式 / 更新周期 / 保留版本 / 过滤 / 健康检查 / 组数 / 解析器 / 精确更新时间都逐项列出", async () => {
    registerMock("subscription_list", () => [contractSubscription({ group_count: 4 })]);
    renderDetail();

    // 先等 `subscription_list` 落地：它回来之前参数表每一项都是「未读取」
    await screen.findByText(i18n.t("network.format.auto"));
    const params = within(await screen.findByTestId("network-subscription-detail-params"));
    // 格式：认识的取值翻成中文（不是把 `auto` 原样摆出来）
    expect(params.getByText(i18n.t("network.format.auto"))).toBeTruthy();
    expect(params.getByText(formatDuration(21_600))).toBeTruthy();
    expect(params.getByText("3")).toBeTruthy();
    expect(params.getByText("4")).toBeTruthy();
    expect(params.getByText("clash-yaml@1")).toBeTruthy();
    // 过滤两条正则都是 null → 「—」（缺失就是缺失，不写一句解释）
    expect(params.getByText("—")).toBeTruthy();
    // 精确时刻在这里（列表上只有 `N 天前` 那种短格式）
    expect(params.getByText(formatLocalTime(1_770_000_000_000))).toBeTruthy();
  });

  it("解析失败的**原文**在详情页逐字显示（列表上只给「解析失败」四个字）", async () => {
    registerMock("subscription_list", () => [
      contractSubscription({ last_update_ms: null, error: "订阅返回了 HTML，不是 YAML" }),
    ]);
    renderDetail();

    const error = await screen.findByTestId("network-subscription-detail-parse-error");
    expect(error.textContent).toContain("订阅返回了 HTML，不是 YAML");
  });

  it("顶部两行「为什么不能关」整段收进页头问号；行上只剩一行状态", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    renderDetail([
      { name: "东京-03", type: "vmess", ok: true, latency_ms: 82 },
      { name: "首尔-02", type: "ss", ok: false, latency_ms: null },
    ]);
    await screen.findAllByTestId("network-subscription-node-row");

    // 正文一个字没丢：整段都在页头那个问号里（提示气泡是懒渲染的，`aria-label` 才是全文）
    const help = screen.getByTestId("page-header-help").getAttribute("aria-label") ?? "";
    for (const key of [
      "network.subscription.description",
      "network.subscription.autoWhy",
      "network.subscription.directWhy",
      "network.subscription.toggleHint",
    ] as const) {
      expect(help, `${key} 应当进页头那个问号`).toContain(i18n.t(key));
    }

    // 行上只有四个字（否则两行各铺一大段，比节点本身还显眼）
    const auto = screen.getByTestId("network-subscription-builtin-auto");
    expect(auto.textContent).toContain(i18n.t("network.subscription.builtinLocked"));
    // 而且那两段解释**不再内联**
    expect(screen.queryByText(i18n.t("network.subscription.autoWhy"))).toBeNull();
    expect(screen.queryByText(i18n.t("network.subscription.directWhy"))).toBeNull();
  });
});

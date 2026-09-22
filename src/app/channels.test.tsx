// @vitest-environment jsdom
/**
 * 套餐与线路页（`/channels`，[07 §5] / [05 §2.1]）的验收。三条硬纪律：
 *
 * 1. **线路显示不许猜**：五级继承是 `channel -> credential -> provider -> default -> direct`，
 *    而本页只读得到前两级（`channel_list` / `credential_list`）。
 *    所以拿不到的那几级显示成「继承」并说明原因，**不替后端选一条线路**。
 * 2. **配额占用比只在口径真的对得上时才算**：`QuotaMetric` 与 `used_*` 字段必须匹配、
 *    `limit` 必须是有限正数 —— 否则一律「未设上限」，绝不拿错误的分子分母拼一个百分比。
 * 3. **矩阵未实现就如实说**：`channel_matrix` 还没注册 -> 这一块只说明现状，
 *    **不放批量按钮、也不放禁用占位**（本任务点名禁止）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import ChannelsPage, { ChannelsBody, type ChannelsBodyProps } from "@/pages/channels";
import {
  groupByProvider,
  quotaParts,
  readChannelRows,
  resolveLane,
  summarizeChannels,
  type ChannelRowContext,
} from "@/pages/channels/model";
// 纯逻辑单独取一份命名空间导入：写的当下 `channelIdFor` / `channelPayload` 还不存在，
// 这样红的时候是**一条条断言失败**（`xxx is not a function`），而不是整个文件加载不了。
import * as channelsModel from "@/pages/channels/model";
import type {
  ApplyResult,
  Channel,
  ChannelGroup,
  Credential,
  CredentialView,
  LaneView,
  PriceEntry,
  ProviderView,
  QuotaConfig,
} from "@/shared/bindings";
import { registerBaselineMocks, registerMock, qk, TokenAgencyError } from "@/shared/api";
import { i18n, type I18nKey } from "@/shared/i18n";
import { buildTheme } from "@/theme";

const MODEL_ID = "deepseek-v4.1flash";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
  // `channel_list` 与 `quota_limit_gaps` 在共享 mock（`src/shared/api/mock.ts`）里没有注册，
  // 而这一页要它们。mock.ts 不在本页写作用域内 -> 本页测试自己提供夹具（覆盖式注册）。
  registerMock("channel_list", () => channelsFixture());
  registerMock("quota_limit_gaps", () => []);
});

afterEach(() => {
  cleanup();
});

// ================================================================ 夹具

function quota(overrides: Partial<QuotaConfig> = {}): QuotaConfig {
  return {
    metric: "tokens",
    limit: 5_000_000,
    currency: "CNY",
    window: "month",
    reset_day: 1,
    soft_limit: 0.8,
    hard_limit: 0.98,
    reserve: 0.05,
    ...overrides,
  };
}

function credential(id = "c-bytedance", overrides: Partial<Credential> = {}): Credential {
  return {
    id,
    provider: id === "c-bytedance" ? "p-bytedance" : "p-commandcode",
    label: id === "c-bytedance" ? "字节 · 500万 token/月" : "command code · 按量",
    secret: `\${secret:${id}}`,
    lane: id === "c-bytedance" ? "lane-direct" : "lane-jp",
    quota: quota(id === "c-bytedance" ? {} : { metric: "cost", limit: 20 }),
    guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "wait" },
    tags: [],
    enabled: true,
    ...overrides,
  };
}

function view(c: Credential): CredentialView {
  return {
    credential: c,
    provider_name: c.id === "c-bytedance" ? "字节方舟" : "command code",
    quota:
      c.id === "c-bytedance"
        ? { credential_id: c.id, state: "soft", used_tokens: 4_100_000 }
        : { credential_id: c.id, state: "ok", used_cost: 4.2 },
    channel_count: 1,
  };
}

function channel(overrides: Partial<Channel> = {}): Channel {
  return {
    id: "ch-bytedance",
    model: MODEL_ID,
    provider: "p-bytedance",
    credential: "c-bytedance",
    lane: "lane-direct",
    upstream_model: MODEL_ID,
    transform: { headers_add: {}, headers_remove: [], headers_override: {}, body_patch: {} },
    priority: 10,
    weight: 100,
    tags: ["cn-direct"],
    enabled: true,
    ...overrides,
  };
}

function channelsFixture(): Channel[] {
  return [
    channel(),
    channel({
      id: "ch-commandcode",
      provider: "p-commandcode",
      credential: "c-commandcode",
      // 通道自己没有指定线路 -> 继承凭据的线路（这一级读得到）
      lane: null,
      upstream_model: `${MODEL_ID}-2026`,
      priority: 20,
      weight: 50,
      tags: ["backup"],
    }),
  ];
}

function providers(): ProviderView[] {
  return [
    {
      id: "p-bytedance",
      name: "字节方舟",
      protocol: "openai",
      base_url: "https://ark.example.com",
      enabled: true,
      channel_count: 1,
      status: "ok",
    },
    {
      id: "p-commandcode",
      name: "command code",
      protocol: "anthropic",
      base_url: "https://api.example.com",
      enabled: true,
      channel_count: 1,
      status: "warn",
    },
  ];
}

function lanes(): LaneView[] {
  return [
    {
      lane: {
        id: "lane-direct",
        name: "直连（本机网络）",
        kind: "direct",
        subscription: null,
        group: null,
        group_type: null,
        node: null,
        filter: null,
        exclude_filter: null,
        enabled: true,
      },
      type: "direct",
      node_count: 0,
      current_node: null,
      latency_ms: 18,
      health: "ok",
    },
    {
      lane: {
        id: "lane-jp",
        name: "日本线",
        kind: "subscription_group",
        subscription: "sub-jp",
        // 用户数据（订阅自带组名）：原样保留，不做任何清洗（[15 §2.2]）
        group: "日本",
        group_type: "url-test",
        node: null,
        filter: null,
        exclude_filter: null,
        enabled: true,
      },
      type: "subscription_group",
      node_count: 3,
      current_node: "东京-03",
      latency_ms: 82,
      health: "ok",
    },
  ];
}

function groups(): ChannelGroup[] {
  return [
    {
      id: "grp-deepseek",
      model: MODEL_ID,
      members: ["ch-bytedance", "ch-commandcode"],
      selector: "sticky_then_priority",
      sticky: { enabled: true, ttl_h: 12, key: ["header:x-session-id"] },
      release_on: ["quota_exhausted", "hard_error"],
    },
  ];
}

function context(overrides: Partial<ChannelRowContext> = {}): ChannelRowContext {
  return {
    providers: providers(),
    credentials: [view(credential()), view(credential("c-commandcode"))],
    lanes: lanes(),
    groups: groups(),
    limitGaps: [],
    ...overrides,
  };
}

function makeProps(overrides: Partial<ChannelsBodyProps> = {}): ChannelsBodyProps {
  return {
    rows: [],
    limitGaps: [],
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    // 线路下拉（[docs/21 §4.2]）：选项就是 lane_list，回调用空实现 —— 展示件这一层不拼载荷，
    // 「选直连必须发 lane_id: null」由整页用例（⑬）钉。
    lanes: lanes(),
    onAssignLane: () => {},
    selectedChannelIds: [],
    onToggleChannel: () => {},
    onToggleAllChannels: () => {},
    onDeleteChannel: () => {},
    // 通道组区块（[docs/21 §3]）：纯展示渲染，命令由整页用例（⑮）钉
    groups: [],
    selectedGroupIds: [],
    onToggleGroup: () => {},
    onToggleAllGroups: () => {},
    onDeleteGroup: () => {},
    ...overrides,
  };
}

function renderBody(overrides: Partial<ChannelsBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <ChannelsBody {...makeProps(overrides)} />
    </ThemeProvider>,
  );
}

function renderPage(): ReturnType<typeof render> & { client: QueryClient } {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const view = render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <ChannelsPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  // 把 client 一起交出去：⑬ 要机械地检查"改完线路之后到底失效了哪些 query"
  return { ...view, client: queryClient };
}

/**
 * 每个用例前把本页要的命令 mock 复位。
 *
 * 为什么不能只调 `registerBaselineMocks()`：`registerMock` 是**进程级**的注册表，
 * 上一个用例里为某个命令注册的替身会漏到下一个用例（例如 ⑬ 把 `channel_list` 换成了
 * 只有一条通道的桩，⑭ 就再也看不到两条通道了）。夹具必须每个用例重新注册一次。
 */
function resetPageMocks(): void {
  registerBaselineMocks();
  registerMock("channel_list", () => channelsFixture());
  registerMock("quota_limit_gaps", () => []);
}

// ================================================================ ① 纯逻辑：线路继承

describe("线路解析：五级继承里只认读得到的两级，读不到的明确说继承", () => {
  it("通道自己指定了线路 -> 用通道那一级，并带出线路健康与最近样本（两套字段名都认）", () => {
    const r = resolveLane(channel({ lane: "lane-jp" }), credential(), lanes());
    expect(r.source).toBe("channel");
    expect(r.laneId).toBe("lane-jp");
    expect(r.name).toBe("日本线");
    expect(r.health).toBe("ok");
    // 契约形状用 current_node / latency_ms；真机 lane_list 的健康证据在 latest 里
    expect(r.sample?.node).toBe("东京-03");
    expect(r.sample?.latencyMs).toBe(82);
    expect(r.nodeCount).toBe(3);
  });

  it("通道没指定但凭据指定了 -> 用凭据那一级，来源写 credential（不冒充通道级）", () => {
    const r = resolveLane(channel({ lane: null }), credential("c-commandcode"), lanes());
    expect(r.source).toBe("credential");
    expect(r.laneId).toBe("lane-jp");
    expect(r.name).toBe("日本线");
  });

  it("两级都没有 -> inherited，且不给任何具体线路（更上一级本页读不到，不猜）", () => {
    const r = resolveLane(channel({ lane: null }), credential("c-bytedance", { lane: null }), lanes());
    expect(r.source).toBe("inherited");
    expect(r.laneId).toBeUndefined();
    expect(r.name).toBeUndefined();
    expect(r.health).toBeUndefined();
  });

  it("线路 id 不在 lane_list 里 -> 保留 id 但名字未知（不编一个名字）", () => {
    const r = resolveLane(channel({ lane: "lane-ghost" }), undefined, lanes());
    expect(r.source).toBe("channel");
    expect(r.laneId).toBe("lane-ghost");
    expect(r.name).toBeUndefined();
  });
});

// ================================================================ ② 纯逻辑：额度口径

describe("额度：口径对得上才算占用比，对不上一律 null（= 未设上限）", () => {
  it("tokens 口径 + 有限正数上限 -> used / limit（比例由本页算，来源标 derived）", () => {
    const parts = quotaParts(quota(), { state: "soft", used_tokens: 2_500_000 });
    expect(parts?.used).toBe(2_500_000);
    expect(parts?.limit).toBe(5_000_000);
    expect(parts?.usedPct).toBe(0.5);
    expect(parts?.usedPctSource).toBe("derived");
    expect(parts?.metric).toBe("tokens");
    expect(parts?.window).toBe("month");
    expect(parts?.currency).toBe("CNY");
  });

  it("后端给了 used_pct 就一定用它（权威），并标记来源 backend", () => {
    const parts = quotaParts(quota(), { state: "soft", used_tokens: 2_500_000 }, 0.42);
    expect(parts?.usedPct).toBe(0.42);
    expect(parts?.usedPctSource).toBe("backend");
  });

  it("口径对不上（metric 与 used_* 不是同一维）-> 不算比", () => {
    const parts = quotaParts(quota({ metric: "cost", limit: 20 }), {
      state: "ok",
      used_tokens: 2_500_000,
    });
    expect(parts?.used).toBeUndefined();
    expect(parts?.usedPct).toBeNull();
    expect(parts?.usedPctSource).toBe("none");
  });

  it("metric 是 unknown -> 口径未声明，不算比", () => {
    const parts = quotaParts(quota({ metric: "unknown" }), { state: "unknown", used_tokens: 1 });
    expect(parts?.used).toBeUndefined();
    expect(parts?.usedPct).toBeNull();
  });

  it("上限缺失 / 0 / 负数 / 非有限 -> 不算比（0 不是「已用完」，是没配）", () => {
    const state = { state: "ok" as const, used_tokens: 10 };
    expect(quotaParts(quota({ limit: null }), state)?.usedPct).toBeNull();
    expect(quotaParts(quota({ limit: 0 }), state)?.usedPct).toBeNull();
    expect(quotaParts(quota({ limit: -5 }), state)?.usedPct).toBeNull();
    expect(quotaParts(quota({ limit: Number.NaN }), state)?.usedPct).toBeNull();
    expect(quotaParts(quota({ limit: Number.POSITIVE_INFINITY }), state)?.usedPct).toBeNull();
    // 上限读不出来，但用量仍然是事实 -> 保留下来（页面会显示"只有用量、没有上限"）
    expect(quotaParts(quota({ limit: null }), state)?.used).toBe(10);
  });

  it("没有额度配置 -> undefined；有配置但没用量 -> 给出上限并标记 usageMissing", () => {
    expect(quotaParts(undefined, undefined)).toBeUndefined();

    // 缺的是**用量**而不是上限：页面必须能区分（否则会画一个假的"未设上限"）
    const noUsage = quotaParts(quota(), undefined);
    expect(noUsage?.limit).toBe(5_000_000);
    expect(noUsage?.usedPct).toBeNull();
    expect(noUsage?.usedPctSource).toBe("none");
    expect(noUsage?.usageMissing).toBe(true);
  });

  it("cost 口径用 used_cost", () => {
    const parts = quotaParts(quota({ metric: "cost", limit: 20, currency: "CNY" }), {
      state: "ok",
      used_cost: 5,
    });
    expect(parts?.used).toBe(5);
    expect(parts?.usedPct).toBe(0.25);
    expect(parts?.currency).toBe("CNY");
  });
});

// ================================================================ ③ 纯逻辑：读通道行 / 分组 / 概览

describe("读通道行与分组", () => {
  it("把服务商 / 凭据 / 线路 / 通道组 / 额度都挂到行上", () => {
    const first = readChannelRows(channelsFixture(), context())[0]!;

    expect(first.id).toBe("ch-bytedance");
    expect(first.providerName).toBe("字节方舟");
    expect(first.providerStatus).toBe("ok");
    expect(first.credentialLabel).toBe("字节 · 500万 token/月");
    expect(first.credentialEnabled).toBe(true);
    expect(first.lane.source).toBe("channel");
    expect(first.groups.map((g) => g.id)).toEqual(["grp-deepseek"]);
    expect(first.groups[0]?.selector).toBe("sticky_then_priority");
    expect(first.quotaState).toBe("soft");
    expect(first.quotaParts?.usedPct).toBeCloseTo(0.82, 6);
  });

  it("清单里查不到的服务商 / 凭据 / 通道组不给名字，也不报错（只留 id）", () => {
    const first = readChannelRows(
      channelsFixture(),
      context({ providers: [], credentials: [], groups: [], lanes: [] }),
    )[0]!;

    expect(first.providerName).toBeUndefined();
    expect(first.credentialLabel).toBeUndefined();
    expect(first.groups).toEqual([]);
    expect(first.lane.source).toBe("channel");
    expect(first.lane.name).toBeUndefined();
    expect(first.quotaState).toBeUndefined();
    expect(first.quotaParts).toBeUndefined();
  });

  it("未配额度的凭据把后端原文挂到对应行上（没有的就没有）", () => {
    const rows = readChannelRows(
      channelsFixture(),
      context({
        limitGaps: [
          { credential_id: "c-commandcode", message: "未设上限：用完不会自动切换" },
          { credential_id: "c-unused", message: "另一条不该出现在任何行上" },
        ],
      }),
    );
    expect(rows[0]?.limitGapMessage).toBeUndefined();
    expect(rows[1]?.limitGapMessage).toBe("未设上限：用完不会自动切换");
  });

  it("按服务商分组，组名取 provider_list 的名字，排序稳定", () => {
    const grouped = groupByProvider(readChannelRows(channelsFixture(), context()));
    expect(grouped.map((g) => g.providerId)).toEqual(["p-bytedance", "p-commandcode"]);
    expect(grouped[0]?.providerName).toBe("字节方舟");
    expect(grouped[0]?.rows).toHaveLength(1);
    expect(grouped[1]?.rows[0]?.id).toBe("ch-commandcode");
  });

  it("概览：总数 / 启停 / 去重服务商 / 未配额度凭据数", () => {
    const rows = readChannelRows(
      channelsFixture(),
      context({ limitGaps: [{ credential_id: "c-commandcode", message: "未设上限" }] }),
    );
    const s = summarizeChannels([...rows, { ...rows[1]!, id: "ch-x", enabled: false }]);
    expect(s.total).toBe(3);
    expect(s.enabled).toBe(2);
    expect(s.disabled).toBe(1);
    expect(s.providers).toBe(2);
    expect(s.limitGapCredentials).toBe(1);
  });
});

// ================================================================ ④ 渲染：空 / 错误 / 加载

describe("空 / 错误 / 加载：都不许拿假数据填坑，也不放点了没反应的按钮", () => {
  it("没有通道：诚实空状态，且**不再说「新增入口还没有接线」**（写入口已在页头接上）", () => {
    renderBody();

    expect(screen.getByTestId("channels-empty")).toBeTruthy();
    expect(screen.getByText("配置里还没有通道")).toBeTruthy();
    expect(screen.getByText(/一条通道 = 一个逻辑模型/)).toBeTruthy();
    // 这句话在写下的那一刻是诚实的边界说明；写入口接上之后它就成了假话 —— 必须消失。
    expect(screen.queryByText(/还没有接线/)).toBeNull();
    expect(screen.queryByTestId("channels-row")).toBeNull();
    // 展示件（纯 props）里不放任何写按钮：写入口在页头的添加向导里
    expect(
      screen.queryByRole("button", { name: /新建|批量|启停|删除|编辑|保存|应用|导入/ }),
    ).toBeNull();
  });

  it("加载中不与没有通道混为一谈", () => {
    renderBody({ isLoading: true });
    expect(screen.getByText("读取通道…")).toBeTruthy();
    expect(screen.queryByTestId("channels-empty")).toBeNull();
  });

  it("读失败：显示 TA 码与重试入口", () => {
    const onRetry = vi.fn();
    renderBody({ error: new Error("[TA-4002] 通道清单读不到"), onRetry });

    expect(screen.getByTestId("channels-error").textContent).toContain("TA-4002");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("矩阵没有实现时如实说明，并明说没有批量入口（不放禁用占位）", () => {
    renderBody({ rows: readChannelRows(channelsFixture(), context()) });
    const note = screen.getByTestId("channels-matrix-absent");
    expect(note.textContent).toContain("channel_matrix");
    expect(note.textContent).toContain("点了没反应");
    expect(screen.queryByRole("button", { name: /批量|矩阵/ })).toBeNull();
  });
});

// ================================================================ ⑤ 渲染：有数据

describe("有数据：按服务商分组的列表 + 额度 + 线路 + 未配额度提醒", () => {
  const rows = readChannelRows(channelsFixture(), context());

  it("按服务商分组渲染，每组一个表；每行显示通道 / 模型 / 上游模型 / 凭据 / 优先级 / 权重 / 标签 / 通道组", () => {
    renderBody({ rows });

    expect(screen.getAllByTestId("channels-group")).toHaveLength(2);
    expect(screen.getAllByTestId("channels-group-header")).toHaveLength(2);
    expect(screen.getAllByTestId("channels-table")).toHaveLength(2);
    expect(screen.getAllByTestId("channels-row")).toHaveLength(2);

    const first = screen.getAllByTestId("channels-row")[0]!;
    expect(within(first).getByTestId("channels-id").textContent).toBe("ch-bytedance");
    expect(within(first).getByTestId("channels-model").textContent).toContain(MODEL_ID);
    expect(within(first).getByTestId("channels-upstream").textContent).toBe(MODEL_ID);
    expect(within(first).getByTestId("channels-credential").textContent).toContain(
      "字节 · 500万 token/月",
    );
    expect(within(first).getByTestId("channels-credential").textContent).toContain("字节方舟");
    expect(within(first).getByTestId("channels-priority").textContent).toBe("10");
    expect(within(first).getByTestId("channels-weight").textContent).toBe("100");
    expect(within(first).getByTestId("channels-tags").textContent).toBe("cn-direct");
    expect(within(first).getByTestId("channels-group-cell").textContent).toBe(
      "grp-deepseek（sticky_then_priority）",
    );
    expect(within(first).getByTestId("channels-status").textContent).toBe("已启用");

    const headers = screen.getAllByTestId("channels-group-header");
    expect(headers[0]?.textContent).toContain("字节方舟");
    expect(headers[0]?.textContent).toContain("1 条通道");
  });

  it("线路：通道级写线路名，凭据级写明「继承自凭据」，两级都没有写「继承」并解释", () => {
    renderBody({
      rows: readChannelRows(
        [
          // 通道级指定了线路
          channel({ id: "ch-a", lane: "lane-direct", credential: "c-bytedance" }),
          // 通道没指定 -> 凭据级（c-commandcode 的 lane 是 lane-jp）
          channel({ id: "ch-b", lane: null, credential: "c-commandcode" }),
          // 两级都没有 -> 只能显示"继承"（更上一级本页读不到）
          channel({ id: "ch-c", lane: null, credential: "c-nolane" }),
        ],
        context({
          credentials: [
            view(credential()),
            view(credential("c-commandcode")),
            view(credential("c-nolane", { lane: null, label: "没有线路的凭据" })),
          ],
        }),
      ),
    });

    const laneCells = screen.getAllByTestId("channels-lane").map((n) => n.textContent);
    expect(laneCells).toEqual([
      "直连（本机网络）",
      "日本线（继承自凭据）",
      "继承（凭据 / 服务商 / 默认线路）",
    ]);
    // [docs/22 §2] 之后这句解释收进了「按服务商分组」标题右边的问号：
    // 断言改成读那个 `?` 的 `aria-label`（**全文一个字都没丢**，而且额外钉住"版面上不再内联"）。
    expect(screen.getByTestId("channels-by-provider-help").getAttribute("aria-label")).toContain(
      i18n.t("channels.lane.hint"),
    );
    expect(screen.queryByText(/只读得到凭据那一级/)).toBeNull();
  });

  it("额度：算得出占用比时给进度条 + 原始数字（单位来自凭据的口径，不猜）", () => {
    renderBody({ rows });

    // c-bytedance: tokens 4.1M / 5M -> 82%（QuotaBar 统一给一位小数，避免同页两种精度）
    expect(screen.getByText("82.0%")).toBeTruthy();
    expect(screen.getByText("4.1M / 5M · token 数 · 月")).toBeTruthy();

    // c-commandcode: cost 4.2 / 20 -> 21%
    expect(screen.getByText("21.0%")).toBeTruthy();
    expect(screen.getByText("¥4.20 / ¥20.00 · 花费 · 月")).toBeTruthy();
  });

  it("凭据没配上限时明说，而不是给一个 0% 的进度条", () => {
    renderBody({
      rows: readChannelRows(
        [channel()],
        context({ credentials: [view(credential("c-bytedance", { quota: quota({ limit: null }) }))] }),
      ),
    });

    expect(screen.getAllByText("未设上限").length).toBeGreaterThan(0);
    expect(screen.getByText("没有上限就算不出占用比（用量仍然照常累计）。")).toBeTruthy();
    expect(screen.queryByText("0%")).toBeNull();
  });

  it("未配可用额度的凭据：用后端原文（LimitGapNotice），并带上凭据名", () => {
    const message = "未设上限：用完不会自动切换（后端原文）";
    renderBody({ rows, limitGaps: [{ credential_id: "c-commandcode", message }] });

    const section = screen.getByTestId("channels-limit-gaps");
    expect(within(section).getByText("command code · 按量")).toBeTruthy();
    expect(screen.getByTestId("limit-gap-notice").textContent).toBe(message);
    expect(section.textContent).toContain(message);
  });

  it("读不到未配额度提醒时如实说，而不是当作「没有这条提醒」", () => {
    renderBody({
      rows,
      limitGaps: [],
      limitGapsError: new Error("[TA-9001] 未注册 mock：quota_limit_gaps"),
    });
    expect(screen.getByTestId("channels-limit-gaps-error").textContent).toContain("TA-9001");
  });

  it("没有未配额度的凭据时不占位（不编一句「一切正常」）", () => {
    renderBody({ rows });
    expect(screen.queryByTestId("channels-limit-gaps")).toBeNull();
    expect(screen.queryByTestId("channels-limit-gaps-error")).toBeNull();
  });

  it("禁用的通道用圆点 + 文字，不用彩色块", () => {
    renderBody({
      rows: readChannelRows(
        [channel({ enabled: false }), channel({ id: "ch-b", enabled: true })],
        context(),
      ),
    });
    expect(screen.getAllByTestId("channels-status").map((n) => n.textContent)).toEqual([
      "已禁用",
      "已启用",
    ]);
  });
});

// ================================================================ ⑥ 真机形状（契约与实现不一致处）
//
// 依据 `src-tauri/src/commands/catalog.rs`：
//   · `provider_list` 直接返回 `Provider` 配置实体（**没有 status**）；
//   · `credential_list` 的用量在 `quota_state` / `used_pct` 里（不是契约的 `quota`）；
//   · `lane_list` 的健康证据在 `latest` 里（不是契约的 current_node/latency_ms/health）。
// 两套形状都要能读 —— 读不到就显示「未知」，**绝不退化成某个看起来正常的结论**。

describe("真机形状：两套字段名都认，缺列时不编结论", () => {
  /** 真机的 provider_list 元素（Provider 配置实体）。 */
  const rustProvider = {
    id: "p-bytedance",
    name: "字节方舟",
    protocol: "openai",
    base_url: "https://ark.example.com/api/v3",
    enabled: true,
  } as unknown as ProviderView;

  it("服务商没有 status 这一列：不点状态灯，但配置里的 enabled 仍然读得到", () => {
    const row = readChannelRows([channel()], context({ providers: [rustProvider] }))[0]!;
    expect(row.providerStatus).toBeUndefined();
    expect(row.providerEnabled).toBe(true);
    expect(row.providerName).toBe("字节方舟");
  });

  it("凭据用量在 quota_state 里、比例在 used_pct 里：都读得到，且后端比例优先", () => {
    const rustCredentialView = {
      credential: credential(),
      quota_state: {
        credential_id: "c-bytedance",
        window_start: 0,
        window_end: 0,
        used_tokens: 1_000_000,
        used_requests: 0,
        used_cost: 0,
        currency: "CNY",
        state: "soft",
      },
      used_pct: 0.2,
    } as unknown as CredentialView;

    const row = readChannelRows([channel()], context({ credentials: [rustCredentialView] }))[0]!;
    expect(row.quotaState).toBe("soft");
    expect(row.quotaParts?.used).toBe(1_000_000);
    expect(row.quotaParts?.usedPct).toBe(0.2);
    expect(row.quotaParts?.usedPctSource).toBe("backend");
  });

  it("契约没给 quota_state 时：上限来自配置，占用比算不出来（标记 usageMissing）", () => {
    const views = [{ credential: credential() } as unknown as CredentialView];
    const row = readChannelRows([channel()], context({ credentials: views }))[0]!;
    expect(row.quotaState).toBeUndefined();
    expect(row.quotaParts?.usageMissing).toBe(true);
    expect(row.quotaParts?.limit).toBe(5_000_000);
    expect(row.quotaParts?.usedPct).toBeNull();
  });

  it("lane_list 真机形状：健康证据在 latest 里，缺 health 时不编一个健康结论", () => {
    const rustLane = {
      lane: lanes()[1]!.lane,
      latest: { ts_ms: 1, lane_id: "lane-jp", node: "大阪-01", latency_ms: 96, ok: true },
      channel_count: 2,
    } as unknown as LaneView;

    const r = resolveLane(channel({ lane: "lane-jp" }), undefined, [rustLane]);
    expect(r.sample?.node).toBe("大阪-01");
    expect(r.sample?.latencyMs).toBe(96);
    expect(r.sample?.ok).toBe(true);
    expect(r.health).toBeUndefined();
  });

  it("渲染：没有用量记录时给上限 + 明说原因，**不画「未设上限」**；服务商那格给配置事实", () => {
    renderBody({
      rows: readChannelRows(
        [channel()],
        context({
          providers: [rustProvider],
          credentials: [{ credential: credential() } as unknown as CredentialView],
        }),
      ),
    });

    // 上限读得到 -> 明细里是「— / 5M」，并说明是"还没有用量记录"
    expect(screen.getByText("— / 5M · token 数 · 月")).toBeTruthy();
    expect(screen.getByText(/还没有用量记录/)).toBeTruthy();
    // 关键：不能说成"未设上限"（上限明明配了 5M）
    expect(screen.queryByText("未设上限")).toBeNull();
    // 服务商那格：配置事实 + 一句"为什么不点状态灯"
    expect(screen.getByText("配置为已启用")).toBeTruthy();
    // [docs/22 §2] 之后这句解释收进了「按服务商分组」标题右边的问号
    // （`aria-label` 是全文，版面上不再内联）
    expect(screen.getByTestId("channels-by-provider-help").getAttribute("aria-label")).toContain(
      i18n.t("channels.provider.statusAbsent"),
    );
    expect(screen.queryByText(/后端没有返回服务商健康状态这一列/)).toBeNull();
  });

  it("渲染：有 latest 时把最近样本（节点 + 延迟）显示出来，且不混进线路名里", () => {
    const rustLane = {
      lane: lanes()[1]!.lane,
      latest: { ts_ms: 1, lane_id: "lane-jp", node: "大阪-01", latency_ms: 96, ok: true },
      channel_count: 2,
    } as unknown as LaneView;

    renderBody({
      rows: readChannelRows([channel({ lane: "lane-jp" })], context({ lanes: [rustLane] })),
    });

    expect(screen.getByTestId("channels-lane").textContent).toBe("日本线");
    expect(screen.getByText("最近样本：大阪-01 · 96ms")).toBeTruthy();
  });
});

// ================================================================ ⑦ 整页（浏览器开发路径 mock）

describe("整页在 mock 基准场景下能用（路由不再走占位壳）", () => {
  it("渲染页头（导航标签是用户语言「套餐与线路」）与通道行", async () => {
    renderPage();

    expect((await screen.findByText("套餐与线路")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();

    // 两个服务商 -> 每组一张表
    expect(await screen.findAllByTestId("channels-table")).toHaveLength(2);
    expect(screen.getAllByTestId("channels-row").length).toBeGreaterThan(0);
    expect(screen.getByText("ch-bytedance")).toBeTruthy();
  });
});

// ================================================================ ⑧ 文案回归

describe("文案里不留未替换的占位符", () => {
  it("渲染出来的正文里不该出现双花括号占位符", () => {
    renderBody({
      rows: readChannelRows(channelsFixture(), context()),
      limitGaps: [{ credential_id: "c-commandcode", message: "未设上限" }],
    });
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑨ 线路添加向导（task-33）
//
// 用户的原话就是这条需求：
//   线路1: 服务商 Ollama        模型 deepseek-v4.1flash  网络 直连  计费信息
//   线路2: 服务商 CommandCode   模型 deepseek-v4.1flash  网络 直连  计费信息
// 本项目的「线路」= `Channel`（model x provider x credential x lane x upstream_model x 优先级/权重），
// 写命令是 `channel_upsert({ channel })`。
//
// 这一组钉住三件**只有机械检查才抓得住**的事（与 `network.test.tsx` ⑪/⑫ 同一套打法）：
//   1. 控件真的在页面上，而且 `data-testid` 就是真机 E2E 要用的那一串（用户点不到 = 坏在这里，
//      而只渲染展示件的单测完全看不出来）；
//   2. 真输入 / 真选择之后**真的调用了 `channel_upsert`**，入参 snake_case、且只带用户填了的字段
//      （用户没填的字段不替后端编默认值：那些字段 Rust 侧都有 `#[serde(default)]`）；
//   3. 成功区显示的是**后端返回的那个 Channel**，不是前端拼的一句「添加成功」——
//      所以下面的 mock 故意返回与输入完全不同的值，只有显示的是后端真值，这些断言才会过。
//
// 本轮以外的东西（能力块的控件与开关、计费的三个字段、`pricing_upsert` 的先后与入参）在 ⑪/⑫
// 里钉。这里保留一句反向断言：能力**永远是六个字段的完整对象**（不是只发用户碰过的那几个），
// 默认值逐字等于 Rust `ChannelCapabilities::default`。

/** MUI 的 Select 在 jsdom 里是个 div：先 `mouseDown` 打开菜单，再点角色为 option 的项。 */
async function chooseOption(testId: string, optionName: RegExp | string): Promise<void> {
  const box = screen.getByTestId(testId);
  fireEvent.mouseDown(within(box).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name: optionName }));
}

/** 往 testid 直接挂在输入框上的字段里打字。 */
function typeIntoField(testId: string, value: string): void {
  fireEvent.change(screen.getByTestId(testId), { target: { value } });
}

/** 记录入参的 mock：返回 `result`，同时把每次调用的入参留下来。 */
function recordingMock(result: unknown): {
  calls: Record<string, unknown>[];
  handler: (args: Record<string, unknown> | undefined) => unknown;
} {
  const calls: Record<string, unknown>[] = [];
  return {
    calls,
    handler: (args) => {
      calls.push(args ?? {});
      return result;
    },
  };
}

/** 本轮向导该有的控件（顺序 = 用户描述的顺序）。**不许改名**：真机 E2E 按它们驱动。 */
const WIZARD_CONTROLS = [
  "channels-add-provider",
  "channels-add-credential",
  "channels-add-model",
  "channels-add-upstreamModel",
  "channels-add-lane",
  "channels-add-priority",
  "channels-add-weight",
  "channels-add-submit",
] as const;

describe("⑨ /channels 线路添加向导：真控件 → 真的调 channel_upsert → 显示后端真值", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  /** 把必填的四项填好（服务商 / 凭据 / 模型 / 上游模型名）。 */
  async function fillRequired(): Promise<void> {
    await chooseOption("channels-add-provider", "command code");
    await chooseOption("channels-add-credential", /command code · 按量/);
    typeIntoField("channels-add-model", MODEL_ID);
    typeIntoField("channels-add-upstreamModel", `${MODEL_ID}-2026`);
  }

  it("页头有「添加」入口；打开后本轮该有的控件一个不少（testid 不许改名）", async () => {
    renderPage();

    fireEvent.click(await screen.findByTestId("channels-add-open"));
    for (const id of WIZARD_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }
    // 服务商 / 凭据 / 网络是下拉，模型是「下拉 + 可直接输入」—— 都要能交互
    expect(within(screen.getByTestId("channels-add-provider")).getByRole("combobox")).toBeTruthy();
    expect(within(screen.getByTestId("channels-add-credential")).getByRole("combobox")).toBeTruthy();
    expect(within(screen.getByTestId("channels-add-lane")).getByRole("combobox")).toBeTruthy();
    expect(screen.getByTestId("channels-add-model").tagName).toBe("INPUT");
  });

  it("服务商下拉来自 provider_list；凭据下拉**按 provider 过滤**（别的服务商的凭据不出现）", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));

    // 服务商选项就是 provider_list 里的两个
    const providerBox = screen.getByTestId("channels-add-provider");
    fireEvent.mouseDown(within(providerBox).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: "字节方舟" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "command code" })).toBeTruthy();
    // 关掉菜单再选一个（选完菜单自己会关）
    fireEvent.click(screen.getByRole("option", { name: "字节方舟" }));

    const credentialBox = screen.getByTestId("channels-add-credential");
    fireEvent.mouseDown(within(credentialBox).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: /字节 · 500万 token\/月/ })).toBeTruthy();
    // commandcode 的凭据属于另一个服务商 —— 不许出现在这里
    expect(screen.queryByRole("option", { name: /command code · 按量/ })).toBeNull();
  });

  it("网络下拉来自 lane_list，且**默认就是直连**（不是空、也不是第一条订阅线路）", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));

    const box = screen.getByTestId("channels-add-lane");
    expect(within(box).getByRole("combobox").textContent).toContain("直连");

    fireEvent.mouseDown(within(box).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: "直连（本机网络）" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "日本线" })).toBeTruthy();
  });

  it("填完真提交 → channel_upsert({channel})：入参 snake_case、只带填了的字段，成功后显示后端返回值", async () => {
    let listCalls = 0;
    registerMock("channel_list", () => {
      listCalls += 1;
      return channelsFixture();
    });
    // 后端返回的**与提交的完全不同**：只有显示的是后端真值，下面的断言才会过
    const upsert = recordingMock(
      channel({
        id: "ch-backend",
        model: "m-backend",
        provider: "p-backend",
        credential: "c-backend",
        lane: "lane-jp",
        upstream_model: "up-backend",
        priority: 3,
        weight: 7,
      }),
    );
    registerMock("channel_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));
    await fillRequired();
    typeIntoField("channels-add-priority", "7");
    typeIntoField("channels-add-weight", "3");
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const args = upsert.calls[0]!;
    // 命令入参只有一个 `channel`（契约：`channel_upsert: { args: { channel: Channel } }`）
    expect(Object.keys(args)).toEqual(["channel"]);
    const ch = args["channel"] as Record<string, unknown>;

    expect(ch["model"]).toBe(MODEL_ID);
    expect(ch["provider"]).toBe("p-commandcode");
    expect(ch["credential"]).toBe("c-commandcode");
    expect(ch["upstream_model"]).toBe(`${MODEL_ID}-2026`);
    // 网络没动过 -> 就是默认的直连那条 lane（来自 lane_list 的 id，不是前端编的字符串）
    expect(ch["lane"]).toBe("lane-direct");
    expect(ch["priority"]).toBe(7);
    expect(ch["weight"]).toBe(3);
    // 通道 id 是派生的（本轮没有 id 输入框）：由「模型 x 服务商 x 凭据」决定
    expect(typeof ch["id"]).toBe("string");
    expect(ch["id"]).toBe(channelsModel.channelIdFor({ ...channelsModel.EMPTY_CHANNEL_DRAFT, provider: "p-commandcode", credential: "c-commandcode", model: MODEL_ID }));

    // 全部键都是 snake_case（真机 `channel_arg_case_guard` 的口径）
    for (const key of Object.keys(ch)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);
    // 用户没填的字段**不替后端编**：这些在 Rust 侧都有 #[serde(default)]
    for (const absent of ["transform", "tags", "enabled"]) {
      expect(absent in ch, `${absent} 不该由前端硬编（Rust 有 serde(default)）`).toBe(false);
    }
    // 能力块是**唯一例外：永远发全六个字段**，默认值逐字等于 Rust `ChannelCapabilities::default`
    // （`[]` 与"没写"不是一回事，所以这里不能省字段 —— 详见 ⑪ 的第一条与 `model.ts` 的注释）
    expect(ch["capabilities"]).toEqual({
      vision: false,
      tools: false,
      reasoning: false,
      context_tokens: 0,
      max_output_tokens: null,
      modalities: ["text"],
    });

    // 回显的是**后端返回的那个 Channel**
    const saved = await screen.findByTestId("channels-add-result");
    expect(saved.textContent).toContain("ch-backend");
    expect(saved.textContent).toContain("m-backend");
    expect(saved.textContent).toContain("up-backend");
    expect(saved.textContent).toContain("lane-jp");
    expect(saved.textContent).toContain("3");
    expect(saved.textContent).toContain("7");

    // 成功后失效重取（列表真的刷新了，不是本地塞一行）
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("必填缺一个都不发命令，并在结果区逐个点名缺哪个", async () => {
    const upsert = recordingMock(channel());
    registerMock("channel_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    const issues = await screen.findByTestId("channels-add-issues");
    expect(issues.textContent).toContain("必填");
    expect(upsert.calls).toHaveLength(0);
  });

  it("后端拒绝时原样显示 TA 码与字段路径（不吞错、不改写成一句「保存失败」）", async () => {
    registerMock("channel_upsert", () => {
      throw new TokenAgencyError({
        code: "TA-5003",
        message: "channels[0].model 引用的模型不存在",
      });
    });

    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));
    await fillRequired();
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    const error = await screen.findByTestId("channels-add-error");
    expect(error.textContent).toContain("TA-5003");
    expect(error.textContent).toContain("channels[0].model");
    expect(screen.queryByTestId("channels-add-result")).toBeNull();
  });
});

// ================================================================ ⑩ 派生 id（本轮没有 id 输入框）

describe("线路 id 由「模型 x 服务商 x 凭据」派生：换上游名/网络/权重仍是同一条线（upsert 幂等）", () => {
  const identity = { provider: "p-ollama", credential: "c-ollama-personal", model: MODEL_ID };
  const draft: channelsModel.ChannelDraft = {
    ...channelsModel.EMPTY_CHANNEL_DRAFT,
    ...identity,
    upstreamModel: MODEL_ID,
  };

  it("同一条身份的线，换上游名 / 网络 / 优先级 / 权重 -> id 不变（覆盖而不是新增一条）", () => {
    const id = channelsModel.channelIdFor(identity);
    expect(id).toBe(channelsModel.channelIdFor({ ...identity }));
    // 这三样是**属性**：改了还是同一条线（`channel_upsert` 同 id 覆盖 = 幂等编辑）
    expect(channelsModel.channelPayload({ ...draft, upstreamModel: "别的上游名" }).upstream_model).toBe(
      "别的上游名",
    );
    expect(channelsModel.channelPayload({ ...draft, lane: "lane-jp" }).id).toBe(id);
    expect(channelsModel.channelPayload({ ...draft, priority: "1", weight: "99" }).id).toBe(id);
    // 换服务商 / 凭据 / 模型 -> 是另一条线
    expect(id).not.toBe(channelsModel.channelIdFor({ ...identity, provider: "p-commandcode" }));
    expect(id).not.toBe(channelsModel.channelIdFor({ ...identity, credential: "c-other" }));
    expect(id).not.toBe(channelsModel.channelIdFor({ ...identity, model: "别的模型" }));
  });

  it("id 只含小写字母/数字/短横线，且以 `ch-` 开头（模型名里的 `.` 也不会漏进去）", () => {
    const id = channelsModel.channelIdFor(identity);
    expect(id.startsWith("ch-")).toBe(true);
    expect(id).toMatch(/^[a-z0-9-]+$/);
    expect(id).toBe("ch-p-ollama-c-ollama-personal-deepseek-v4-1flash");
  });

  it("载荷只在真的填了的时候带 lane / priority / weight（空字符串 = 不发这个字段）", () => {
    const empty = channelsModel.channelPayload(draft);
    expect("lane" in empty).toBe(false);
    expect("priority" in empty).toBe(false);
    expect("weight" in empty).toBe(false);

    const filled = channelsModel.channelPayload({ ...draft, lane: "lane-direct", priority: "5", weight: "20" });
    expect(filled.lane).toBe("lane-direct");
    expect(filled.priority).toBe(5);
    expect(filled.weight).toBe(20);
  });
});

// ================================================================ ⑪ 能力块 + 计费块（docs/17 §3.3）
//
// 用户原话里的「线路1/线路2」除了身份（服务商 x 凭据 x 模型）与网络，还带**能力**与**计费**：
//   线路1: 服务商 Ollama        模型 deepseek-v4.1flash  网络 直连  计费信息
//   线路2: 服务商 CommandCode   模型 deepseek-v4.1flash  网络 直连  计费信息
// 这一组钉四件事（都是只有机械检查才抓得住的）：
//
//   1. **五个能力控件与三个计费控件真的在页面上**，testid 就是真机 E2E 要用的那一串；
//   2. 不碰能力块时，载荷里的 `capabilities` **逐字等于 Rust `ChannelCapabilities::default`**
//      （全 false / `context_tokens` 0 / `max_output_tokens` null / `modalities` `["text"]`）——
//      默认值本身就是契约的一部分（老配置不写也能用），前端**不许**发明另一套默认值；
//      `max_output_tokens` 本页不做输入，所以载荷里恒为 `null`（不放假控件）；
//   3. 计费三项全空 -> **一次都不调** `pricing_upsert`；有任意一项非空 -> `channel_upsert`
//      **成功之后**才调 `pricing_upsert({ entry })`，`match` = { model, provider }、
//      `rates` 只放填了的键（`input` / `output` / `input_cached` = ta-core `TokenKind::as_key()`）；
//   4. 成功区显示的是**两条命令各自的后端返回值**（mock 故意返回与提交完全不同的值），
//      计费失败时后端错误原样带 TA 码，且**不显示**"已保存"这句成功文案（不许谎报）。
//
// 「真的调了命令」为什么不用任务书里写的 `vi.spyOn(client, "invokeCommand")`：
// `src/shared/api/client.ts` 里**没有** `invokeCommand`（唯一入口是 `commands.ts` 的
// `invokeCommand` → `client.call`），ESM 的命名导出绑定又打不进 `vi.spyOn`
// （`models.test.tsx` ⑦ 已经踩过同一个坑，见那里的长注释）。
// 所以这里沿用 ⑨ 的办法：`registerMock(命令名, 记录入参的 handler)` —— handler 只有
// **命令名真的对得上**才会被调用，于是"拼错命令名"与"根本没发请求"都会立刻红。

/** MUI Switch：`data-testid` 落在根 `span` 上（与 `models-add-expose` 的 Checkbox 同一形状），
 *  里面那个真正的 `input[type=checkbox]` 才是可点的 —— 与真机 E2E「先找容器内的 input」同口径。 */
function switchInput(testId: string): HTMLInputElement {
  const host = screen.getByTestId(testId);
  const input = host.tagName === "INPUT" ? host : host.querySelector("input");
  if (input === null) throw new Error(`${testId} 里没有可交互的 input`);
  return input as HTMLInputElement;
}

/** 本轮新增的控件（能力块 + 计费块）。**不许改名**：真机 E2E 按它们驱动。 */
const CAPABILITY_AND_PRICE_CONTROLS = [
  "channels-add-vision",
  "channels-add-tools",
  "channels-add-reasoning",
  "channels-add-contextTokens",
  "channels-add-modalities",
  "channels-add-price-input",
  "channels-add-price-output",
  "channels-add-price-cached",
] as const;

/** Rust `ChannelCapabilities::default` 的**逐字**形状（[docs/17 §2] 明说它是契约的一部分）。 */
const RUST_DEFAULT_CAPABILITIES = {
  vision: false,
  tools: false,
  reasoning: false,
  context_tokens: 0,
  max_output_tokens: null,
  modalities: ["text"],
} as const;

describe("⑪ 线路向导的能力块与计费块：真控件 → 真载荷 → 计费按后端顺序单独落库", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  /** 把必填的四项填好（与 ⑨ 同一套）。 */
  async function fillRequired(): Promise<void> {
    await chooseOption("channels-add-provider", "command code");
    await chooseOption("channels-add-credential", /command code · 按量/);
    typeIntoField("channels-add-model", MODEL_ID);
    typeIntoField("channels-add-upstreamModel", `${MODEL_ID}-2026`);
  }

  /** 打开向导并等它可用。 */
  async function openWizard(): Promise<void> {
    renderPage();
    fireEvent.click(await screen.findByTestId("channels-add-open"));
  }

  it("能力的五个控件与计费的三个控件一个不少，且打开时就是 Rust 的默认值", async () => {
    await openWizard();

    for (const id of CAPABILITY_AND_PRICE_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }

    // 三个开关默认关（= Rust 默认 false）
    for (const id of ["channels-add-vision", "channels-add-tools", "channels-add-reasoning"]) {
      expect(switchInput(id).checked, `${id} 默认必须是关的`).toBe(false);
    }
    // 上下文窗口默认 0；模态默认 text（= Rust 默认 ["text"]）——不是空、也不是别的词
    expect((screen.getByTestId("channels-add-contextTokens") as HTMLInputElement).value).toBe("0");
    expect((screen.getByTestId("channels-add-modalities") as HTMLInputElement).value).toBe("text");
    // 三项计费默认全空 = 不计价（**不放 0**：0 是"免费"，不是"没填"）
    for (const id of ["channels-add-price-input", "channels-add-price-output", "channels-add-price-cached"]) {
      expect((screen.getByTestId(id) as HTMLInputElement).value).toBe("");
    }
  });

  it("不碰能力块 -> 载荷里的 capabilities 逐字等于 Rust 默认；计费全空 -> 一次都不调 pricing_upsert", async () => {
    const upsert = recordingMock(channel({ id: "ch-backend" }));
    const pricing = recordingMock({} as PriceEntry);
    registerMock("channel_upsert", upsert.handler);
    registerMock("pricing_upsert", pricing.handler);

    await openWizard();
    await fillRequired();
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const ch = upsert.calls[0]!["channel"] as Record<string, unknown>;
    expect(ch["capabilities"]).toEqual(RUST_DEFAULT_CAPABILITIES);
    // 三项全空 = 不计价：`pricing_upsert` 一次都不许发
    expect(pricing.calls).toHaveLength(0);
  });

  it("改过的能力真进载荷：开关 true、contextTokens 净化为非负整数、modalities 切片排序去重", async () => {
    const upsert = recordingMock(channel());
    registerMock("channel_upsert", upsert.handler);
    registerMock("pricing_upsert", () => ({}) as PriceEntry);

    await openWizard();
    await fillRequired();

    fireEvent.click(switchInput("channels-add-vision"));
    fireEvent.click(switchInput("channels-add-tools"));
    expect(switchInput("channels-add-vision").checked).toBe(true);
    expect(switchInput("channels-add-tools").checked).toBe(true);
    // 非数字字符进不来（与优先级/权重同一个 intInputFilter 手法）-> 载荷里不可能出现 NaN
    typeIntoField("channels-add-contextTokens", "12abc8");
    expect((screen.getByTestId("channels-add-contextTokens") as HTMLInputElement).value).toBe("128");
    // 逗号分隔：trim -> 切 -> 去空 -> 排序去重（image 出现两次只留一个）
    typeIntoField("channels-add-modalities", " image , text ,image, ");

    fireEvent.click(screen.getByTestId("channels-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const caps = (upsert.calls[0]!["channel"] as Record<string, unknown>)["capabilities"] as Record<
      string,
      unknown
    >;
    expect(caps["vision"]).toBe(true);
    expect(caps["tools"]).toBe(true);
    expect(caps["reasoning"]).toBe(false);
    expect(caps["context_tokens"]).toBe(128);
    expect(caps["max_output_tokens"]).toBeNull();
    expect(caps["modalities"]).toEqual(["image", "text"]);
  });

  it("填了计费 -> channel_upsert 成功之后才调 pricing_upsert({entry})，rates 只放填了的键", async () => {
    const order: string[] = [];
    const upsert = recordingMock(
      channel({ id: "ch-backend", model: "m-backend", upstream_model: "up-backend" }),
    );
    // 后端返回的**与提交的完全不同**：只有显示的是后端真值，回显断言才会过
    const savedEntry: PriceEntry = {
      match: { model: "backend-priced-model", provider: "p-backend" },
      rates: { input: 9.99 },
      multiplier: 3,
      currency: "USD",
      source: "imported",
      effective_from: "2026-01-01",
    };
    const pricing = recordingMock(savedEntry);
    registerMock("channel_upsert", (args) => {
      order.push("channel");
      return upsert.handler(args);
    });
    registerMock("pricing_upsert", (args) => {
      order.push("pricing");
      return pricing.handler(args);
    });

    await openWizard();
    await fillRequired();
    typeIntoField("channels-add-price-input", "1.5");
    // 输出留空 -> rates 里就没有 output 这个键
    typeIntoField("channels-add-price-cached", "0.25");
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    await waitFor(() => expect(pricing.calls).toHaveLength(1));
    // 顺序是契约的一部分：线路没落库就不许写价（否则价格指向一条不存在的线）
    expect(order).toEqual(["channel", "pricing"]);

    const call = pricing.calls[0]!;
    expect(Object.keys(call)).toEqual(["entry"]);
    const entry = call["entry"] as Record<string, unknown>;
    expect(entry["match"]).toEqual({ model: MODEL_ID, provider: "p-commandcode" });
    expect(entry["rates"]).toEqual({ input: 1.5, input_cached: 0.25 });
    expect(entry["multiplier"]).toBe(1);
    expect(entry["source"]).toBe("manual");
    expect(entry["currency"]).toBeNull();
    expect(entry["effective_from"]).toBeNull();

    // 成功区：**两条命令的后端返回值**都在（原样的后端值，不是我们提交的草稿）
    const saved = await screen.findByTestId("channels-add-result");
    expect(saved.textContent).toContain("ch-backend");
    expect(saved.textContent).toContain("m-backend");
    expect(saved.textContent).toContain("backend-priced-model");
    expect(saved.textContent).toContain("9.99");
    expect(saved.textContent).toContain("imported");
  });

  it("价格非法（负数 / 非数字）-> 本地预检点名，channel_upsert 与 pricing_upsert 一次都不发", async () => {
    const upsert = recordingMock(channel());
    const pricing = recordingMock({} as PriceEntry);
    registerMock("channel_upsert", upsert.handler);
    registerMock("pricing_upsert", pricing.handler);

    await openWizard();
    await fillRequired();
    typeIntoField("channels-add-price-input", "-1");
    typeIntoField("channels-add-price-output", "很贵");
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    const issues = await screen.findByTestId("channels-add-issues");
    expect(issues.textContent).toContain("输入：必须是数字（可留空）");
    expect(issues.textContent).toContain("输出：必须是数字（可留空）");
    expect(upsert.calls).toHaveLength(0);
    expect(pricing.calls).toHaveLength(0);
  });

  it("channel_upsert 失败 -> 计费一次都不发（价格不许指向没落库的线）", async () => {
    const pricing = recordingMock({} as PriceEntry);
    registerMock("channel_upsert", () => {
      throw new TokenAgencyError({ code: "TA-5003", message: "channels[0].model 引用的模型不存在" });
    });
    registerMock("pricing_upsert", pricing.handler);

    await openWizard();
    await fillRequired();
    typeIntoField("channels-add-price-input", "2");
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    const error = await screen.findByTestId("channels-add-error");
    expect(error.textContent).toContain("TA-5003");
    expect(pricing.calls).toHaveLength(0);
  });

  it("计费失败 -> 后端错误原样带 TA 码，且不许显示「已保存」成功文案", async () => {
    registerMock("channel_upsert", () => channel({ id: "ch-backend" }));
    registerMock("pricing_upsert", () => {
      throw new TokenAgencyError({ code: "TA-2007", message: "定价条目非法：rates.input 为负" });
    });

    await openWizard();
    await fillRequired();
    typeIntoField("channels-add-price-input", "3");
    fireEvent.click(screen.getByTestId("channels-add-submit"));

    const error = await screen.findByTestId("channels-add-error");
    expect(error.textContent).toContain("TA-2007");
    expect(error.textContent).toContain("rates.input");
    // 通道确实落库了（后端返回的 Channel 仍在），但**不许**声称整条线配好了
    expect(screen.getByTestId("channels-add-result").textContent).toContain("ch-backend");
    expect(screen.queryByText(/已保存/)).toBeNull();
  });
});

// ================================================================ ⑫ 能力 / 计费的纯逻辑
describe("⑫ 能力与计费的整形：默认值照抄 Rust、只发填了的价", () => {
  const base: channelsModel.ChannelDraft = {
    ...channelsModel.EMPTY_CHANNEL_DRAFT,
    provider: "p-ollama",
    credential: "c-ollama",
    model: MODEL_ID,
    upstreamModel: MODEL_ID,
  };

  it("modalities：trim -> 按逗号切 -> 去空 -> 排序去重；清空 = 显式声明「没有模态」（[] 不是 ['text']）", () => {
    expect(channelsModel.parseModalities(" image , text ,image, ")).toEqual(["image", "text"]);
    expect(channelsModel.parseModalities("text")).toEqual(["text"]);
    expect(channelsModel.parseModalities("")).toEqual([]);
  });

  it("contextTokens：空 = 0（Rust 默认）；非数字一律不当数字用（返回 undefined，绝不 NaN）", () => {
    expect(channelsModel.parseContextTokens("")).toBe(0);
    expect(channelsModel.parseContextTokens("128000")).toBe(128_000);
    expect(channelsModel.parseContextTokens("12.5")).toBeUndefined();
    expect(channelsModel.parseContextTokens("-1")).toBeUndefined();
    expect(Number.isNaN(channelsModel.channelPayload(base).capabilities?.context_tokens ?? 0)).toBe(
      false,
    );
  });

  it("价格：空 = 不计价（undefined）；非负数字才认；负号 / 非数字 / 半截小数都不认", () => {
    expect(channelsModel.parsePrice("")).toBeUndefined();
    expect(channelsModel.parsePrice("0")).toBe(0);
    expect(channelsModel.parsePrice("1.25")).toBe(1.25);
    expect(channelsModel.parsePrice("-1")).toBeUndefined();
    expect(channelsModel.parsePrice("1.2.3")).toBeUndefined();
    expect(channelsModel.parsePrice("abc")).toBeUndefined();
  });

  it("pricing 载荷：键名是 TokenKind::as_key() 的三个，只放填了的键；match 用所选模型与服务商", () => {
    const entry = channelsModel.priceEntryFor({
      ...base,
      priceInput: "1",
      priceOutput: "",
      priceCached: "0.5",
    });
    expect(entry.rates).toEqual({ input: 1, input_cached: 0.5 });
    expect(entry.match).toEqual({ model: MODEL_ID, provider: "p-ollama" });
    expect(entry.multiplier).toBe(1);
    expect(entry.source).toBe("manual");
    expect(entry.currency).toBeNull();
    expect(entry.effective_from).toBeNull();
    // 三项全空 -> 没有任何 rate（页面据此完全不调 pricing_upsert）
    expect(channelsModel.priceEntryFor({ ...base, priceInput: "", priceOutput: "", priceCached: "" }).rates).toEqual({});
    expect(channelsModel.hasPrice({ priceInput: "", priceOutput: "", priceCached: "" })).toBe(false);
    expect(channelsModel.hasPrice({ priceInput: "0", priceOutput: "", priceCached: "" })).toBe(true);
  });

  it("本地预检点名：必填缺项 + 价格非法（两类问题分开说，不混成一句「必填」）", () => {
    const problems = channelsModel.channelDraftIssues({
      ...channelsModel.EMPTY_CHANNEL_DRAFT,
      priceInput: "-2",
    });
    expect(problems).toEqual([
      { field: "provider", problem: "required" },
      { field: "credential", problem: "required" },
      { field: "model", problem: "required" },
      { field: "upstreamModel", problem: "required" },
      { field: "priceInput", problem: "number" },
    ]);
    expect(channelsModel.channelDraftIssues(base)).toEqual([]);
  });
});

// ================================================================ ⑬ 通道页每行的线路下拉（docs/21 §4.2）
//
// 用户原话：「线路设置不能点进服务商里面去修改」—— 所以线路的**唯一**编辑入口就是通道页这一列。
// 这一组钉三件事（都是只有机械检查才抓得住的）：
//   1. 每行真的有一个可交互的下拉（容器 `channels-lane-select`，里面是**真** `combobox`）；
//   2. 「直连（继承）」发的是 `lane_id: null`（= 清空，回到五级继承链），选某条线路发的是那个 id；
//   3. 成功后**真的重取**（矩阵 + 通道清单），行内展示跟着后端重取回来的值走；
//      失败原样显示后端原文（含 TA 码），**不**在本地偷偷改掉显示的值。

/** 在一个容器（一行）里操作 MUI Select：先 `mouseDown` 打开菜单，再点 `option`。 */
async function chooseInBox(box: HTMLElement, optionName: RegExp | string): Promise<void> {
  fireEvent.mouseDown(within(box).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name: optionName }));
}

/** MUI Checkbox：`data-testid` 在根 `span` 上，真可点的是里面那个 `input[type=checkbox]`。 */
function toggleBox(host: HTMLElement): void {
  const input = host.tagName === "INPUT" ? host : host.querySelector("input");
  if (input === null) throw new Error("容器里没有可点的 input");
  fireEvent.click(input);
}

/** `channel_bulk_assign_lane` 的成功回执（形状 = 契约的 `ApplyResult`）。 */
function applyOk(): ApplyResult {
  return { applied: true, restart_required: false, kernel_reload: false, channels_created: 0 };
}

describe("⑬ 通道页每行的线路下拉：直连=null、真实线路=id、失败原样显示", () => {
  beforeEach(resetPageMocks);

  it("每行一个 channels-lane-select（容器里有真 combobox），第一条选项永远是「直连（继承）」", async () => {
    renderPage();

    const boxes = await screen.findAllByTestId("channels-lane-select");
    expect(boxes).toHaveLength(2);
    expect(within(boxes[0]!).getByRole("combobox")).toBeTruthy();

    fireEvent.mouseDown(within(boxes[0]!).getByRole("combobox"));
    const options = (await screen.findAllByRole("option")).map((o) => o.textContent);
    // 第一条永远是「直连（继承）」；其余来自 lane_list（无名回落 id）
    expect(options[0]).toBe("直连（继承）");
    expect(options).toContain("直连（本机网络）");
    expect(options).toContain("日本线");
  });

  it("选「直连（继承）」-> channel_bulk_assign_lane({channel_ids:[那条], lane_id: null})", async () => {
    const assign = recordingMock(applyOk());
    registerMock("channel_bulk_assign_lane", assign.handler);

    renderPage();

    const first = (await screen.findAllByTestId("channels-row"))[0]!;
    await chooseInBox(within(first).getByTestId("channels-lane-select"), "直连（继承）");

    await waitFor(() => expect(assign.calls).toHaveLength(1));
    // 载荷就是契约那一对：一个通道 + lane_id: null（= 清空 = 回到继承链）
    expect(assign.calls[0]).toEqual({ channel_ids: ["ch-bytedance"], lane_id: null });
  });

  it("选某条真实线路 -> lane_id 是那个 id；成功后重取（矩阵 + 通道清单），行内显示新线路名", async () => {
    let listCalls = 0;
    registerMock("channel_list", () => {
      listCalls += 1;
      // 重取之后后端**真的变了**：这条通道自己钉上了 lane-jp
      return listCalls === 1
        ? [channel({ id: "ch-bytedance", lane: null })]
        : [channel({ id: "ch-bytedance", lane: "lane-jp" })];
    });
    // 本页现在没有 `channel_matrix` 的观察者（矩阵那一段如实写着"尚未接入"），
    // 所以"是否失效了矩阵"只能机械地看 invalidateQueries 的入参 —— 见下面的 spy。
    registerMock("channel_matrix", () => ({ models: [], credentials: [], cells: [] }));
    const assign = recordingMock(applyOk());
    registerMock("channel_bulk_assign_lane", assign.handler);

    const page = renderPage();
    const invalidate = vi.spyOn(page.client, "invalidateQueries");

    const first = (await screen.findAllByTestId("channels-row"))[0]!;
    // 通道自己没有 lane -> 下拉显示「直连（继承）」，而**当前生效**的是继承来的那条（两者不是一回事）
    expect(within(first).getByTestId("channels-lane").textContent).toBe("直连（本机网络）（继承自凭据）");

    await chooseInBox(within(first).getByTestId("channels-lane-select"), "日本线");

    await waitFor(() => expect(assign.calls).toHaveLength(1));
    expect(assign.calls[0]).toEqual({ channel_ids: ["ch-bytedance"], lane_id: "lane-jp" });

    // 成功后**真的重取**：矩阵与通道清单都被失效（不是本地乐观改一个）
    await waitFor(() =>
      expect(
        invalidate.mock.calls.some(
          ([arg]) => JSON.stringify(arg?.queryKey) === JSON.stringify(qk.channelMatrix),
        ),
      ).toBe(true),
    );
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));

    // 行内展示跟着**后端重取回来**的值走
    await waitFor(() =>
      expect(
        within(screen.getAllByTestId("channels-row")[0]!).getByTestId("channels-lane").textContent,
      ).toBe("日本线"),
    );
    // 并且如实说这一次改成了什么
    expect(screen.getByTestId("channels-lane-changed").textContent).toContain("日本线");
  });

  it("后端拒绝时原样显示后端原文（含 TA 码），且行内展示一个数都不许偷偷改", async () => {
    registerMock("channel_bulk_assign_lane", () => {
      throw new TokenAgencyError({ code: "TA-1004", message: "lanes 里没有 lane-ghost" });
    });

    renderPage();

    const first = (await screen.findAllByTestId("channels-row"))[0]!;
    const before = within(first).getByTestId("channels-lane").textContent;

    await chooseInBox(within(first).getByTestId("channels-lane-select"), "日本线");

    const error = await screen.findByTestId("channels-lane-error");
    expect(error.textContent).toContain("TA-1004");
    expect(error.textContent).toContain("lane-ghost");
    // 失败 = 没改：行内展示还是原来那条（不许本地乐观更新）
    expect(screen.getAllByTestId("channels-lane")[0]!.textContent).toBe(before);
    expect(screen.queryByTestId("channels-lane-changed")).toBeNull();
  });
});

// ================================================================ ⑭ 通道删除（docs/21 §1 / §3）
//
// 通道**没有** force 语义（`channel_delete` 的载荷只有 `{id}`），所以被引用时：
// 只把后端原文摆出来，**一个级联按钮都不许有**（那会是假按钮）。
// 批量是用户明确要的：一次确认 → 逐条调用 → 逐条汇报，一条失败不许中止其余条。

describe("⑭ 通道删除：先二次确认、失败原样显示、批量逐条汇报", () => {
  beforeEach(resetPageMocks);

  it("表头全选与选中计数：一条都没选时不摆点了没用的按钮", async () => {
    renderPage();

    const all = await screen.findAllByTestId("channels-select-all");
    // 每个服务商一张表，各有一个表头全选
    expect(all).toHaveLength(2);
    expect(screen.queryByTestId("channels-selected-count")).toBeNull();
    expect(screen.queryByTestId("channels-delete-selected")).toBeNull();

    toggleBox(all[0]!);
    expect(screen.getByTestId("channels-selected-count").textContent).toContain("1");
    expect(screen.getByTestId("channels-delete-selected")).toBeTruthy();

    // 再点一次 = 取消选择 -> 计数与按钮都收回去
    toggleBox(all[0]!);
    expect(screen.queryByTestId("channels-selected-count")).toBeNull();
  });

  it("行内删除 -> 确认框先于命令；确认后才 channel_delete({id})", async () => {
    const del = recordingMock(undefined);
    registerMock("channel_delete", del.handler);

    renderPage();

    const first = (await screen.findAllByTestId("channels-row"))[0]!;
    fireEvent.click(within(first).getByTestId("channels-delete"));

    // 二次确认：确认框里写清删的是哪一个，此时**一条命令都没发**
    expect(await screen.findByTestId("channels-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("channels-delete-target").textContent).toContain("ch-bytedance");
    expect(del.calls).toHaveLength(0);

    fireEvent.click(screen.getByTestId("channels-delete-confirm"));

    await waitFor(() => expect(del.calls).toHaveLength(1));
    // 载荷就是 `{id}` —— 没有 force 这个字段（这条命令没有级联语义）
    expect(del.calls[0]).toEqual({ id: "ch-bytedance" });
    // 成功后如实写「已删除 <名称 + id>」
    const result = await screen.findByTestId("channels-delete-result");
    expect(result.textContent).toContain("已删除");
    expect(result.textContent).toContain("ch-bytedance");
  });

  it("被引用时只显示后端原文（含 TA 码），不放任何级联按钮，也不谎报成功", async () => {
    registerMock("channel_delete", () => {
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "通道 ch-bytedance 被策略 pol-default 引用",
      });
    });

    renderPage();

    const first = (await screen.findAllByTestId("channels-row"))[0]!;
    fireEvent.click(within(first).getByTestId("channels-delete"));
    fireEvent.click(await screen.findByTestId("channels-delete-confirm"));

    const error = await screen.findByTestId("channels-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("pol-default");
    // 通道没有 force 语义 -> 级联按钮一个都不许有
    expect(screen.queryByTestId("channels-delete-cascade")).toBeNull();
    // 也没删成：确认框还开着，结果区不出现（不许假装删掉了）
    expect(screen.getByTestId("channels-delete-dialog")).toBeTruthy();
    expect(screen.queryByTestId("channels-delete-result")).toBeNull();
  });

  it("批量：一次确认、逐条调用、逐条汇报（1 成功 1 失败，一条失败不中止另一条）", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("channel_delete", (args) => {
      calls.push(args ?? {});
      if (args?.["id"] === "ch-commandcode") {
        throw new TokenAgencyError({
          code: "TA-5002",
          message: "通道 ch-commandcode 被策略 pol-default 引用",
        });
      }
      return undefined;
    });

    renderPage();

    const boxes = await screen.findAllByTestId("channels-select");
    expect(boxes).toHaveLength(2);
    toggleBox(boxes[0]!);
    toggleBox(boxes[1]!);
    expect(screen.getByTestId("channels-selected-count").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("channels-delete-selected"));
    expect(await screen.findByTestId("channels-delete-dialog")).toBeTruthy();
    // 一次确认之前一条命令都不许发
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByTestId("channels-delete-confirm"));

    // 两条都发了（失败的后面那条不许被中止）
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls.map((c) => c["id"]).sort()).toEqual(["ch-bytedance", "ch-commandcode"]);

    // 逐条汇报：成功 N 条 / 失败 M 条 + 失败项逐条给 id 与后端原文
    const result = await screen.findByTestId("channels-delete-result");
    expect(result.textContent).toContain("成功 1 条，失败 1 条");
    expect(result.textContent).toContain("ch-commandcode");
    expect(result.textContent).toContain("TA-5002");
  });
});

// ================================================================ ⑮ 通道组区块（docs/21 §3）
//
// 通道组（`group_list`）此前只在通道行里以 id 露过一面，删除入口一个都没有。
// 这一组钉：列表的列（模型 / 策略 / 通道数）、空态、以及删除载荷 `{id}`（同样没有 force）。

describe("⑮ 通道组区块：列表（模型 / 策略 / 通道数）+ 删除载荷 {id}", () => {
  beforeEach(resetPageMocks);

  it("列出 group_list 的每一条：模型 / 选择策略 / 通道数", async () => {
    registerMock("group_list", () => groups());

    renderPage();

    const rows = await screen.findAllByTestId("channels-group-row");
    expect(rows).toHaveLength(1);
    expect(within(rows[0]!).getByTestId("channels-group-id").textContent).toBe("grp-deepseek");
    expect(within(rows[0]!).getByTestId("channels-group-model").textContent).toBe(MODEL_ID);
    // 选择策略是本页**原样**显示后端的枚举值（不翻译成另一套说法）
    expect(within(rows[0]!).getByTestId("channels-group-strategy").textContent).toBe(
      "sticky_then_priority",
    );
    expect(within(rows[0]!).getByTestId("channels-group-count").textContent).toBe("2");
  });

  it("没有通道组时不编一行假数据，只说「配置里还没有通道组」", async () => {
    registerMock("group_list", () => []);

    renderPage();

    expect((await screen.findByTestId("channels-groups-empty")).textContent).toContain(
      "配置里还没有通道组",
    );
    expect(screen.queryByTestId("channels-group-row")).toBeNull();
  });

  it("行内删除 -> 先确认，再 group_delete({id})；没有级联按钮", async () => {
    registerMock("group_list", () => groups());
    const del = recordingMock(undefined);
    registerMock("group_delete", del.handler);

    renderPage();

    const row = (await screen.findAllByTestId("channels-group-row"))[0]!;
    fireEvent.click(within(row).getByTestId("groups-delete"));

    expect(await screen.findByTestId("groups-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("groups-delete-target").textContent).toContain("grp-deepseek");
    expect(del.calls).toHaveLength(0);

    fireEvent.click(screen.getByTestId("groups-delete-confirm"));

    await waitFor(() => expect(del.calls).toHaveLength(1));
    expect(del.calls[0]).toEqual({ id: "grp-deepseek" });
    expect(screen.queryByTestId("groups-delete-cascade")).toBeNull();
    expect((await screen.findByTestId("groups-delete-result")).textContent).toContain("已删除");
  });

  it("批量：groups-select-all 选中整表 -> groups-delete-selected 逐条调用并汇报", async () => {
    const first = groups()[0]!;
    registerMock("group_list", () => [first, { ...first, id: "grp-second", members: ["ch-commandcode"] }]);
    const calls: Record<string, unknown>[] = [];
    registerMock("group_delete", (args) => {
      calls.push(args ?? {});
      return undefined;
    });

    renderPage();

    toggleBox(await screen.findByTestId("groups-select-all"));
    expect(screen.getByTestId("groups-selected-count").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("groups-delete-selected"));
    expect(await screen.findByTestId("groups-delete-dialog")).toBeTruthy();
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByTestId("groups-delete-confirm"));

    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls.map((c) => c["id"]).sort()).toEqual(["grp-deepseek", "grp-second"]);
    expect((await screen.findByTestId("groups-delete-result")).textContent).toContain(
      "成功 2 条，失败 0 条",
    );
  });
});

// ================================================================ ⑯ [docs/22] 长简介收进「问号圆圈 + 悬停提示」
//
// 用户第 74 轮原话：「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个
// 问好加一个圆圈，鼠标移上去能看到提示那种」。这一组按 [docs/22 §2/§3/§5] 钉两件事：
//
// 1. **解释收走了**：`?` 在、`aria-label` 是全文（提示气泡是懒渲染的，读屏仍然拿得到全文）、
//    那段正文**不再内联**渲染；
// 2. **红线没被收走**：空态 / 现状结论 / 额度结论 / 数值与单位 / 后端原文 / 必填提示
//    —— 把状态或错误塞进 tooltip 是"藏问题"，不是"排版"。
//
// 判不准的一律留在版面上（§2 末尾的默认），所以下面这几处**故意不收**：
// `channels.empty.what`（空态）、`channels.matrix.absent`（现状结论）、
// `channels.quota.*` / `channels.tags.none`（每一格的结论）、
// `channels.add.price`（带单位「美元 / 百万 token」）。
//
// 同一区块**最多一个** `?`（§3）：线路继承与「为什么不点状态灯」两句合成一条提示。

describe("⑯ [docs/22] 长简介收进问号圆圈，红线一律留在版面上", () => {
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

  it("概览 / 未配额度 / 按服务商分组：区块说明收进问号，正文不再内联", () => {
    renderBody({
      rows: readChannelRows(
        [
          ...channelsFixture(),
          // 通道与凭据两级都没有线路 -> 这一行只能显示「继承」，它的解释跟着出现
          channel({ id: "ch-c", lane: null, credential: "c-nolane" }),
        ],
        context({
          credentials: [
            view(credential()),
            view(credential("c-commandcode")),
            view(credential("c-nolane", { lane: null, label: "没有线路的凭据" })),
          ],
        }),
      ),
      limitGaps: [{ credential_id: "c-commandcode", message: "未设上限：用完不会自动切换" }],
    });

    expect(helpLabel("channels-summary-help")).toContain(copy("channels.summary.hint"));
    expect(screen.queryByText(copy("channels.summary.hint"))).toBeNull();

    expect(helpLabel("channels-limit-gaps-help")).toContain(copy("channels.limitGaps.hint"));
    expect(screen.queryByText(copy("channels.limitGaps.hint"))).toBeNull();

    // 线路继承那一句（这一页最长的解释之一）也收走了
    expect(helpLabel("channels-by-provider-help")).toContain(copy("channels.lane.hint"));
    expect(screen.queryByText(copy("channels.lane.hint"))).toBeNull();
  });

  it("同一区块最多一个问号：线路继承与「为什么不点状态灯」合成一条提示", () => {
    // 真机形状：`provider_list` 直接返回 Provider 实体（**没有 status 这一列**）
    const rustProvider = {
      id: "p-bytedance",
      name: "字节方舟",
      protocol: "openai",
      base_url: "https://ark.example.com",
      enabled: true,
    } as unknown as ProviderView;

    renderBody({
      rows: readChannelRows(
        [
          channel(),
          // 两级都没有线路 -> 只能显示「继承」
          channel({ id: "ch-c", lane: null, credential: "c-nolane" }),
        ],
        context({
          providers: [rustProvider],
          credentials: [
            view(credential()),
            view(credential("c-nolane", { lane: null, label: "没有线路的凭据" })),
          ],
        }),
      ),
    });

    const label = helpLabel("channels-by-provider-help");
    expect(label).toContain(copy("channels.lane.hint"));
    expect(label).toContain(copy("channels.provider.statusAbsent"));
    expect(label).not.toBe(copy("channels.lane.hint"));
    expect(screen.queryByText(copy("channels.provider.statusAbsent"))).toBeNull();
    // 一个区块只有一个 `?`（一排问号比一排字更糟），整页正文里也只有这两个
    expect(screen.getAllByTestId("channels-by-provider-help")).toHaveLength(1);
    expect(screen.getAllByTestId(/-help$/).map((node) => node.getAttribute("data-testid"))).toEqual([
      "channels-summary-help",
      "channels-by-provider-help",
    ]);
  });

  it("红线仍内联：空态 / 现状结论 / 额度结论与数值 / 后端原文 / 未配额度提醒", () => {
    renderBody();
    // §2 第 1 条：空态里"一条通道是什么"的解释与空态本身绑在一起，留在版面上
    expect(screen.getByTestId("channels-empty").textContent).toContain(copy("channels.empty.what"));
    expect(screen.getByTestId("channels-empty").textContent).toContain(copy("channels.empty.title"));
    cleanup();

    // §2 第 1 条：矩阵的现状结论（含"点了没反应"这句边界）一眼可见
    renderBody({ rows: readChannelRows(channelsFixture(), context()) });
    expect(screen.getByTestId("channels-matrix-absent").textContent).toContain(
      copy("channels.matrix.absent"),
    );
    cleanup();

    // §2 第 5 条：数值与单位一眼可见
    renderBody({ rows: readChannelRows([channel()], context({ credentials: [view(credential())] })) });
    expect(screen.getByText("82.0%")).toBeTruthy();
    expect(screen.getByText("4.1M / 5M · token 数 · 月")).toBeTruthy();
    cleanup();

    // §2 第 1 条：这一格的结论（缺的是用量、不是上限）与上限绑在一起，不许收
    renderBody({
      rows: readChannelRows(
        [channel()],
        context({
          credentials: [{ credential: credential() } as unknown as CredentialView],
        }),
      ),
    });
    expect(screen.getByText("— / 5M · token 数 · 月")).toBeTruthy();
    expect(screen.getByText(/还没有用量记录/)).toBeTruthy();
    // 关键：不许把"缺用量"说成"未设上限"
    expect(screen.queryByText("未设上限")).toBeNull();
  });

  it("红线仍内联：未配额度提醒的后端原文（含 TA 码）", () => {
    renderBody({
      rows: readChannelRows(channelsFixture(), context()),
      limitGaps: [],
      limitGapsError: new Error("[TA-9001] 未注册 mock：quota_limit_gaps"),
    });
    const error = screen.getByTestId("channels-limit-gaps-error").textContent ?? "";
    expect(error).toContain("TA-9001");
    // 「这不等于没有这条提醒」是这个错误的一部分（把读失败说清楚），不许收
    expect(error).toContain("这不等于没有这条提醒");
  });

  it("添加向导：能力声明说明收进问号；带单位的计费说明与必填提示仍内联", async () => {
    resetPageMocks();
    renderPage();

    fireEvent.click(await screen.findByTestId("channels-add-open"));

    // §2 的成段解释 → 收进「上游能力声明」旁边的问号
    expect(helpLabel("channels-add-capabilities-help")).toContain(copy("capabilities.hint"));
    expect(screen.queryByText(copy("capabilities.hint"))).toBeNull();

    // §2 第 5 条：这句是**计费字段的单位**（美元 / 百万 token），一个字都不许收
    expect(screen.getByText(copy("channels.add.price"))).toBeTruthy();

    // §2 第 3 条：本地预检的必填提示留在版面上（点了才出现，说的就是这次输入）
    fireEvent.click(screen.getByTestId("channels-add-submit"));
    expect(screen.getByTestId("channels-add-issues").textContent).toContain(copy("wizard.required"));
  });
});

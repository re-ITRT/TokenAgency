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
  Channel,
  ChannelGroup,
  Credential,
  CredentialView,
  LaneView,
  ProviderView,
  QuotaConfig,
} from "@/shared/bindings";
import { registerBaselineMocks, registerMock, TokenAgencyError } from "@/shared/api";
import { i18n } from "@/shared/i18n";
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

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <ChannelsPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
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
    expect(screen.getByText(/只读得到凭据那一级/)).toBeTruthy();
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
    expect(screen.getByText(/后端没有返回服务商健康状态这一列/)).toBeTruthy();
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
// 本轮**不做**能力块（`channels-add-vision|tools|reasoning|contextTokens|modalities`）与计费块
// （`channels-add-price-*`）：能力等 `ChannelCapabilities` 进 `bindings.ts`（task-31），计费下轮一起。
// 所以这里也断言**没有**自己造一个 `capabilities` 字段发出去。

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
    // 本轮不做能力块 -> 更不许自己造一个 capabilities 字段
    expect("capabilities" in ch).toBe(false);

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

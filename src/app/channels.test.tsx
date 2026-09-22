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
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import ChannelsPage, { ChannelsBody, type ChannelsBodyProps } from "@/pages/channels";
import {
  groupByProvider,
  quotaParts,
  readChannelRows,
  resolveLane,
  summarizeChannels,
  type ChannelRowContext,
} from "@/pages/channels/model";
import type {
  Channel,
  ChannelGroup,
  Credential,
  CredentialView,
  LaneView,
  ProviderView,
  QuotaConfig,
} from "@/shared/bindings";
import { registerBaselineMocks, registerMock } from "@/shared/api";
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
  it("没有通道：诚实空状态 + 说明新增入口为什么不在 + 不动任何写后端的东西", () => {
    renderBody();

    expect(screen.getByTestId("channels-empty")).toBeTruthy();
    expect(screen.getByText("配置里还没有通道")).toBeTruthy();
    expect(screen.getByText(/一条通道 = 一个逻辑模型/)).toBeTruthy();
    expect(screen.getByText(/「新建」按钮/)).toBeTruthy();
    expect(screen.queryByTestId("channels-row")).toBeNull();
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

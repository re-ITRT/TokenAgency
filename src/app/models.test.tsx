// @vitest-environment jsdom
/**
 * 模型目录页（[07 §4]）的验收。重点和看板/日志两份一样是**诚实**，外加三件这一页特有的事：
 *
 * 1. **"被哪些通道引用"必须来自真实通道数据**：判定是 `channel.model === 模型 id`，
 *    名字（服务商 / 凭据）只在 `provider_list` / `credential_list` 里查得到时才显示，
 *    查不到就只留 id —— **不猜一个名字**。
 * 2. **定价有两个后端来源**：`model_list[].priced` 与 `pricing_unpriced_models`。
 *    两者不一致时本页**不替后端裁决**，把不一致本身显示出来（这才是"不猜"）。
 *    更重要的边界：**通道清单读不到时，"引用通道"这一列不可用，但绝不是"没有被引用"**。
 * 3. **空状态要诚实**：没有模型时不画空表，并明说"一键导入还没有接线"——
 *    而不是放一个点了没反应的按钮（本任务点名禁止禁用占位）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import ModelsPage, { ModelsBody, type ModelsBodyProps } from "@/pages/models";
import {
  EMPTY_MODEL_DRAFT,
  filterModels,
  hasDiscrepancy,
  pricingDiscrepancy,
  pricingVerdict,
  readModelRows,
  sortModels,
  summarizeModels,
  upstreamSummary,
  validateModelDraft,
  type ChannelsLookup,
  type ModelDraft,
  type ModelRowView,
} from "@/pages/models/model";
import type {
  Channel,
  Credential,
  CredentialView,
  Model,
  ModelView,
  ProviderView,
  QuotaConfig,
} from "@/shared/bindings";
import { registerBaselineMocks, registerMock, qk, TokenAgencyError } from "@/shared/api";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

const MODEL_ID = "deepseek-v4.1flash";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
  // `channel_list` 在共享 mock（`src/shared/api/mock.ts`）里**没有注册**，而这一页要它。
  // mock.ts 不在本页写作用域内 → 这一页的测试自己提供夹具（覆盖式注册）。
  registerMock("channel_list", () => channelsFixture());
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

function credential(overrides: Partial<Credential> = {}): Credential {
  return {
    id: "c-bytedance",
    provider: "p-bytedance",
    label: "字节 · 500万 token/月",
    secret: "${secret:bytedance}",
    lane: "lane-direct",
    quota: quota(),
    guard: { max_rpm: 60, max_concurrency: 8, max_input_tokens: null, on_exceed: "wait" },
    tags: ["cn-direct"],
    enabled: true,
    ...overrides,
  };
}

function credentialView(overrides: Partial<Credential> = {}): CredentialView {
  const c = credential(overrides);
  return {
    credential: c,
    provider_name: "字节方舟",
    quota: { credential_id: c.id, state: "soft", used_tokens: 4_100_000 },
    channel_count: 1,
  };
}

/** 一条通道 = 模型 x 服务商 x 凭据（+ 可选线路）。 */
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
      lane: "lane-jp",
      upstream_model: `${MODEL_ID}-2026`,
      priority: 20,
      weight: 50,
      tags: ["backup"],
    }),
    channel({ id: "ch-other-model", model: "gpt-4o-mini", upstream_model: "gpt-4o-mini" }),
  ];
}

function modelView(overrides: Partial<ModelView> = {}): ModelView {
  return {
    model: {
      id: MODEL_ID,
      aliases: ["flash"],
      display: { group: "字节", order: 10 },
      expose: true,
    },
    channel_count: 2,
    upstream_names: [MODEL_ID, `${MODEL_ID}-2026`],
    priced: false,
    cost_hint: null,
    ...overrides,
  };
}

function otherModel(id: string, overrides: Partial<ModelView> = {}): ModelView {
  return modelView({
    model: { id, aliases: [], display: { order: 100 }, expose: true },
    channel_count: 1,
    upstream_names: [id],
    priced: true,
    ...overrides,
  });
}

/**
 * 直接造一行 `ModelRowView`：用来测**纯排序逻辑**里"`display.order` 缺失"那一条分支
 * （契约里 `DisplayConfig.order` 是必填，所以那条分支只能在这一层造出来）。
 */
function rowOf(id: string, order: number | undefined): ModelRowView {
  return {
    id,
    aliases: [],
    group: undefined,
    order,
    expose: true,
    reportedChannels: 0,
    upstreamNames: [],
    priced: true,
    costHint: undefined,
    channels: [],
  };
}

function lookup(overrides: Partial<ChannelsLookup> = {}): ChannelsLookup {
  const providers: ProviderView[] = [
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
  return {
    channels: channelsFixture(),
    providers,
    credentials: [
      credentialView(),
      credentialView({
        id: "c-commandcode",
        label: "command code · 按量",
        provider: "p-commandcode",
      }),
    ],
    ...overrides,
  };
}

function makeProps(overrides: Partial<ModelsBodyProps> = {}): ModelsBodyProps {
  return {
    rows: [],
    unpricedIds: [],
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    ...overrides,
  };
}

function renderBody(overrides: Partial<ModelsBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <ModelsBody {...makeProps(overrides)} />
    </ThemeProvider>,
  );
}

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <ModelsPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

// ================================================================ ① 纯逻辑：读模型行

describe("读模型行：上游映射原样、引用通道来自真实通道数据", () => {
  it("引用通道只认 channel.model 等于这个模型 id 的那些（不按前缀、不模糊匹配）", () => {
    const row = readModelRows([modelView()], lookup())[0]!;

    expect(row.channels.map((c) => c.channelId)).toEqual(["ch-bytedance", "ch-commandcode"]);
    // 名字来自 provider_list / credential_list；查得到才填
    expect(row.channels[0]?.providerName).toBe("字节方舟");
    expect(row.channels[0]?.credentialLabel).toBe("字节 · 500万 token/月");
    expect(row.channels[0]?.upstreamModel).toBe(MODEL_ID);
    expect(row.channels[1]?.upstreamModel).toBe(`${MODEL_ID}-2026`);
    // 别的模型的通道不能被算进来
    expect(row.channels.some((c) => c.channelId === "ch-other-model")).toBe(false);
  });

  it("清单里查不到的服务商/凭据不给名字（不猜），只留 id 供核对", () => {
    const row = readModelRows([modelView()], lookup({ providers: [], credentials: [] }))[0]!;

    expect(row.channels).toHaveLength(2);
    expect(row.channels[0]?.providerName).toBeUndefined();
    expect(row.channels[0]?.credentialLabel).toBeUndefined();
    expect(row.channels[0]?.providerId).toBe("p-bytedance");
  });

  it("model_list 自报的通道数与实际匹配到的通道数是两个数，都留着（不一致要能看见）", () => {
    const row = readModelRows([modelView({ channel_count: 5 })], lookup())[0]!;
    expect(row.reportedChannels).toBe(5);
    expect(row.channels).toHaveLength(2);
  });

  it("别名 / 分组 / 排序 / 暴露 / 定价 / 成本提示原样透传，空数组保持空", () => {
    const row = readModelRows(
      [
        modelView({
          model: { id: MODEL_ID, aliases: [], display: { order: 7 }, expose: false },
          upstream_names: [],
          priced: true,
          cost_hint: "见内置价目表",
        }),
      ],
      lookup(),
    )[0]!;

    expect(row.aliases).toEqual([]);
    expect(row.group).toBeUndefined();
    expect(row.order).toBe(7);
    expect(row.expose).toBe(false);
    expect(row.priced).toBe(true);
    expect(row.costHint).toBe("见内置价目表");
  });

  it("上游映射去重后拼接；一个都没有时给破折号而不是空串", () => {
    expect(upstreamSummary(["a", "a", "b"])).toBe("a · b");
    expect(upstreamSummary([])).toBe("—");
  });

  it("真机形状（Rust ModelView）：upstream_models 也认；priced 缺失**不许当成 false**", () => {
    // 依据 src-tauri/src/commands/catalog.rs:33-37：真机的 ModelView 只有
    // { model, channel_count, upstream_models }，**没有 priced / cost_hint**。
    const rustView = {
      model: { id: MODEL_ID, aliases: ["flash"], display: { order: 1 }, expose: true },
      channel_count: 2,
      upstream_models: [MODEL_ID, `${MODEL_ID}-2026`],
    } as unknown as ModelView;

    const row = readModelRows([rustView], lookup())[0]!;
    expect(row.upstreamNames).toEqual([MODEL_ID, `${MODEL_ID}-2026`]);
    expect(row.reportedChannels).toBe(2);
    expect(row.priced).toBeUndefined();

    // 结论的优先级：未定价清单（定价引擎自己的）> 后端显式 priced > **未知**
    expect(pricingVerdict(row, [])).toBe("priced");
    expect(pricingVerdict(row, [MODEL_ID])).toBe("unpriced");
    expect(pricingVerdict(row, undefined)).toBe("unknown");

    // 后端没给 priced 时，两个来源不可能"不一致"（不许凭空造矛盾）
    expect(hasDiscrepancy(pricingDiscrepancy([row], [MODEL_ID]))).toBe(false);
  });
});

// ================================================================ ② 纯逻辑：概览 / 不一致 / 筛选 / 排序

describe("概览与两个定价来源的交叉核对", () => {
  const rows = readModelRows([modelView(), otherModel("gpt-4o-mini")], lookup());

  it("概览：引用通道按去重后的通道数算，未定价与未暴露分别计数", () => {
    const summary = summarizeModels(rows);
    expect(summary.models).toBe(2);
    // ch-bytedance / ch-commandcode / ch-other-model（共用通道只算一次）
    expect(summary.channels).toBe(3);
    expect(summary.unpriced).toBe(1);
    expect(summary.hidden).toBe(0);
  });

  it("两种不一致都点出来：说已定价却在未定价清单里、说未定价却不在清单里", () => {
    const d = pricingDiscrepancy(rows, ["gpt-4o-mini"]);
    // rows[0] 说未定价（priced:false）但它不在未定价清单里 -> 第二种
    expect(d.unpricedButNotListed).toEqual([MODEL_ID]);
    // rows[1] 说已定价，却在未定价清单里 -> 第一种
    expect(d.pricedButListed).toEqual(["gpt-4o-mini"]);
    expect(d.listedButUnknown).toEqual([]);
  });

  it("定价清单里有、模型目录里没有的 id 单独列出（不是悄悄丢掉）", () => {
    const d = pricingDiscrepancy(rows, ["gpt-4o-mini", "command-code-preview"]);
    expect(d.listedButUnknown).toEqual(["command-code-preview"]);
  });

  it("筛选覆盖 id / 别名 / 上游名 / 分组，大小写不敏感；空串返回全部", () => {
    expect(filterModels(rows, "")).toHaveLength(2);
    expect(filterModels(rows, "FLASH").map((r) => r.id)).toEqual([MODEL_ID]);
    expect(filterModels(rows, "2026").map((r) => r.id)).toEqual([MODEL_ID]);
    expect(filterModels(rows, "字节").map((r) => r.id)).toEqual([MODEL_ID]);
    expect(filterModels(rows, "gpt").map((r) => r.id)).toEqual(["gpt-4o-mini"]);
    expect(filterModels(rows, "  ")).toHaveLength(2);
  });

  it("排序按 display.order 升序，order 缺失排最后，同 order 按 id", () => {
    const sorted = sortModels([
      rowOf("model-10", 10),
      rowOf("model-zzz", 1),
      rowOf("model-aaa", undefined),
      rowOf("model-gpt", undefined),
    ]);
    expect(sorted.map((r) => r.id)).toEqual(["model-zzz", "model-10", "model-aaa", "model-gpt"]);
  });
});

// ================================================================ ③ 渲染：空 / 错误 / 加载

describe("空 / 错误 / 加载：都不许拿假数据填坑", () => {
  it("没有模型：给诚实空状态并说明导入入口为什么不在，且不画空表、不放假按钮", () => {
    renderBody();

    expect(screen.getByTestId("models-empty")).toBeTruthy();
    expect(screen.getByText("配置里还没有逻辑模型")).toBeTruthy();
    expect(screen.getByText(/模型目录来自配置的 models\[\]/)).toBeTruthy();
    expect(screen.getByText(/「一键导入」按钮/)).toBeTruthy();
    expect(screen.queryByTestId("models-table")).toBeNull();
    expect(screen.queryAllByTestId("models-row")).toHaveLength(0);
    // 除了刷新之外没有任何会写后端的按钮（硬约束：不放禁用占位）
    expect(screen.queryByRole("button", { name: /新建|导入|删除|编辑|启用|禁用/ })).toBeNull();
  });

  it("加载中显示读取中，不显示空状态（不把还没读完说成没有模型）", () => {
    renderBody({ isLoading: true });
    expect(screen.getByText("读取模型目录…")).toBeTruthy();
    expect(screen.queryByTestId("models-empty")).toBeNull();
  });

  it("读失败：显示 TA 码与重试入口", () => {
    const onRetry = vi.fn();
    renderBody({ error: new Error("[TA-4001] 模型清单读不到"), onRetry });

    expect(screen.getByTestId("models-error").textContent).toContain("TA-4001");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("通道清单读不到：引用通道列显示未知，并说清这不是「没有被引用」", () => {
    renderBody({
      rows: readModelRows([modelView()], lookup({ channels: [] })),
      unpricedIds: [],
      channelsError: new Error("[TA-9001] 未注册 mock：channel_list"),
    });

    const note = screen.getByTestId("models-channels-error");
    expect(note.textContent).toContain("TA-9001");
    expect(note.textContent).toContain("不是没有被引用");
    // 不许出现"没有被任何通道引用"这种在缺数据时是谎话的结论
    expect(screen.queryByTestId("models-channels-mismatch")).toBeNull();
  });

  it("未定价清单读不到：不宣称「全部已定价」", () => {
    renderBody({
      rows: readModelRows([modelView()], lookup()),
      unpricedIds: undefined,
      unpricedError: new Error("[TA-9001] 未注册 mock：pricing_unpriced_models"),
    });

    expect(screen.getByTestId("models-unpriced-error").textContent).toContain("TA-9001");
    expect(screen.queryByText("全部已定价")).toBeNull();
  });
});

// ================================================================ ④ 渲染：有数据

describe("有数据：清单、未定价、口径不一致", () => {
  const rows = readModelRows([modelView(), otherModel("gpt-4o-mini")], lookup());

  it("每行渲染模型 id / 别名 / 分组与排序 / 上游映射 / 引用通道", () => {
    renderBody({ rows, unpricedIds: [] });

    expect(screen.getByTestId("models-table")).toBeTruthy();
    expect(screen.getAllByTestId("models-row")).toHaveLength(2);
    // 行按 display.order 升序（10 在前，100 在后）
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toEqual([
      MODEL_ID,
      "gpt-4o-mini",
    ]);
    expect(screen.getByText("flash")).toBeTruthy();
    expect(screen.getByText("字节 · 10")).toBeTruthy();
    expect(screen.getByText(`${MODEL_ID} · ${MODEL_ID}-2026`)).toBeTruthy();

    // 引用通道：每条通道一行，带上游模型 / 服务商名 / 凭据名
    const chCell = screen.getByTestId("models-channels-ch-bytedance");
    expect(chCell.textContent).toContain("字节方舟");
    expect(chCell.textContent).toContain("字节 · 500万 token/月");

    const first = screen.getAllByTestId("models-row")[0]!;
    expect(within(first).getByText("对外暴露")).toBeTruthy();
    expect(within(first).getByText("未定价")).toBeTruthy();
  });

  it("未定价区块用后端 pricing_unpriced_models 的原文 id", () => {
    renderBody({ rows, unpricedIds: [MODEL_ID, "command-code-preview"] });

    const section = screen.getByTestId("models-unpriced");
    expect(section.textContent).toContain(MODEL_ID);
    expect(section.textContent).toContain("command-code-preview");
    // 定价清单里有、模型目录里没有的那条要点名（不是悄悄忽略）
    expect(screen.getByTestId("models-pricing-unknown").textContent).toContain(
      "command-code-preview",
    );
  });

  it("两个后端来源不一致：两句话都显示，且明说本页不裁决", () => {
    renderBody({ rows, unpricedIds: ["gpt-4o-mini"] });

    const mismatch = screen.getByTestId("models-mismatch");
    expect(mismatch.textContent).toContain(MODEL_ID);
    expect(mismatch.textContent).toContain("gpt-4o-mini");
    expect(mismatch.textContent).toContain("不替它们裁决");
  });

  it("一致时不给不一致区块（没有话就不占位）", () => {
    // 一致的口径：MODEL_ID 说未定价且确实在未定价清单里；gpt-4o-mini 说已定价且不在清单里
    renderBody({ rows, unpricedIds: [MODEL_ID] });
    expect(screen.queryByTestId("models-mismatch")).toBeNull();
    expect(screen.queryByTestId("models-pricing-unknown")).toBeNull();
  });

  it("本地筛选：只筛已加载的行，并明说这不是后端检索", () => {
    renderBody({ rows, unpricedIds: [] });
    expect(screen.getByText("只筛选已加载的 2 个模型（本地筛选，不是后端检索）。")).toBeTruthy();

    fireEvent.change(screen.getByLabelText("在当前列表中筛选"), { target: { value: "gpt" } });
    expect(screen.getAllByTestId("models-row")).toHaveLength(1);
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toEqual(["gpt-4o-mini"]);
    expect(screen.getByText("只筛选已加载的 1 个模型（本地筛选，不是后端检索）。")).toBeTruthy();
  });

  it("筛选没有命中：诚实说明，不显示 0 行的表格", () => {
    renderBody({ rows, unpricedIds: [] });
    fireEvent.change(screen.getByLabelText("在当前列表中筛选"), { target: { value: "zzz" } });
    expect(screen.getByTestId("models-filter-empty")).toBeTruthy();
    expect(screen.queryByTestId("models-table")).toBeNull();
  });

  it("模型自报的通道数在通道清单里一条都匹配不到时，如实点出不一致", () => {
    renderBody({
      rows: readModelRows([modelView({ channel_count: 2 })], lookup({ channels: [] })),
      unpricedIds: [],
    });
    expect(screen.getByTestId("models-channels-mismatch").textContent).toContain("2");
  });

  it("模型没有被任何通道引用时：说明它发不出去（这是真实结论，不是缺数据）", () => {
    renderBody({
      // 通道清单里没有以它为 model 的通道（夹具里只有 deepseek-v4.1flash 与 gpt-4o-mini 两条）
      rows: readModelRows([otherModel("claude-sonnet-4-5", { channel_count: 0 })], lookup()),
      unpricedIds: [],
    });
    expect(screen.getByTestId("models-channels-none").textContent).toContain("发不出去");
  });

  it("后端没给定价列、未定价清单也读不到：显示「未知」，**不点黄灯也不说未定价**", () => {
    const rustView = {
      model: { id: MODEL_ID, aliases: [], display: { order: 1 }, expose: true },
      channel_count: 2,
      upstream_models: [MODEL_ID],
    } as unknown as ModelView;

    renderBody({ rows: readModelRows([rustView], lookup()), unpricedIds: undefined });

    const row = screen.getAllByTestId("models-row")[0]!;
    expect(within(row).getByText("未知")).toBeTruthy();
    expect(within(row).queryByText("未定价")).toBeNull();
    expect(within(row).queryByText("已定价")).toBeNull();
    expect(within(row).getByText(/后端没有给出定价这一列/)).toBeTruthy();
    // 概览那一格也不能给一个看起来像结论的 0
    const stat = screen.getByTestId("models-summary-unpriced").textContent ?? "";
    expect(stat).toContain("—");
    expect(stat).not.toMatch(/\d/);
  });
});

// ================================================================ ⑤ 整页（浏览器开发路径 mock）

describe("整页在 mock 基准场景下能用（不依赖 Rust/Tauri，路由不再走占位壳）", () => {
  it("渲染页头与清单；本页自己注册的 channel_list 夹具也能读到", async () => {
    renderPage();

    expect((await screen.findByText("模型目录")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();

    expect(await screen.findByTestId("models-table")).toBeTruthy();
    expect(screen.getAllByTestId("models-row").length).toBeGreaterThan(0);
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toContain(MODEL_ID);
  });
});

// ================================================================ ⑥ 文案回归

describe("文案里不留未替换的占位符", () => {
  it("渲染出来的正文里不该出现双花括号占位符", () => {
    renderBody({
      rows: readModelRows([modelView()], lookup()),
      unpricedIds: [MODEL_ID],
    });
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑦ 添加模型向导（task-35）
//
// 契约是 [docs/17 §3.2]：
//   `models-add-open` → `models-add-id` / `-aliases`（逗号分隔）/ `-group` / `-order` / `-expose`
//   → `models-add-submit` → `model_upsert({ model })`，**成功后回显后端返回值**。
// 真机 E2E（`scripts/e2e/smoke.mjs` 的 step9）就是按这串 testid 驱动的，所以这一组钉四件事：
//
// 1. **入口与六个控件真的在页面上** —— 只渲染 `ModelsBody`（纯展示件）的单测看不见写入口，
//    写入口在 `ModelsPage` 的页头里；
// 2. 真输入后提交**真的调用了 `model_upsert`**，入参形状 = `{ model: { id, aliases,
//    display: { group, order }, expose } }`，键全在契约的 casing 里；
// 3. 成功区显示的是**后端返回的那个 Model**（mock 故意返回与提交完全不同的 id / expose，
//    所以「前端自己拼一句保存成功」过不了），并且 `qk.models` 真的失效（清单真的重取）；
// 4. 本地预检不过（id 重复 / id 形状非法 / order 负数）时**一次请求都不发**，并按字段给文案。
//
// 「真的调了命令」为什么不用 `vi.spyOn`：这条任务原本给的写法是
// `import * as client from "@/shared/api/client"` + `vi.spyOn(client, "invokeCommand")`，
// 但 `client.ts` 里**根本没有** `invokeCommand`（唯一入口是 `commands.ts` 的
// `invokeCommand` → `client.call`），而 ESM 的命名导出绑定是只读的，`vi.spyOn` 打不进去
// （`queries.ts` 内部也是直接引用模块内的那个绑定，不经命名空间对象）。
// 所以这里用**与 `channels.test.tsx` ⑨ 同一套、而且更硬**的办法：
// `registerMock("model_upsert", 记录入参的 handler)` —— 这个 handler **只有命令名真的对得上
// 才会被调用**（`client.ts` 的 `call` 只会去 `mocks` 里按命令名取），
// 于是「命令名拼错」和「根本没发请求」这两种情况都会立刻红。

/** 本轮向导该有的控件（顺序 = [docs/17 §3.2] 的行文顺序）。**不许改名**：真机 E2E 按它们驱动。 */
const WIZARD_CONTROLS = [
  "models-add-id",
  "models-add-aliases",
  "models-add-group",
  "models-add-order",
  "models-add-expose",
  "models-add-submit",
] as const;

/**
 * `data-testid` 挂在 MUI `TextField` 的**外层容器**上（MUI v7 把 `...other` 摊给 root，
 * 不是 input）。真机 E2E 的 `wdFillByTid` 就是按「先找里面的 input/textarea，取不到再用容器」
 * 这个口径写的，单测照抄同一口径 —— 这样单测驱动得到的东西 = 真机点得到的东西。
 */
function inputByTestId(testId: string): HTMLElement {
  const host = screen.getByTestId(testId);
  if (host.tagName === "INPUT" || host.tagName === "TEXTAREA") return host;
  return within(host).getByRole("textbox");
}

/** `-expose` 是 MUI `Checkbox`：`data-testid` 会落在 SwitchBase 的根 `span` 上，找里面真正的 checkbox。 */
function checkboxByTestId(testId: string): HTMLElement {
  const host = screen.getByTestId(testId);
  return host.getAttribute("type") === "checkbox" ? host : within(host).getByRole("checkbox");
}

/** 按 testid 填字段（= 真机 `wdFillByTid` 的单测版）。 */
function fillByTestId(testId: string, value: string): void {
  fireEvent.change(inputByTestId(testId), { target: { value } });
}

/** 记录入参的 mock：返回 `result`，同时把每次调用的入参留下来（与 `channels.test.tsx` 同一套）。 */
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

/** 后端返回的 `Model`（**故意与提交的完全不同**：只有显示的是后端真值，回显断言才会过）。 */
function savedModel(overrides: Partial<Model> = {}): Model {
  return {
    id: "backend-saved-id",
    aliases: ["backend-alias"],
    display: { group: "后端分组", order: 99 },
    expose: true,
    ...overrides,
  };
}

describe("⑦ /models 添加模型向导：真控件 → 真的调 model_upsert → 显示后端返回值", () => {
  beforeEach(() => {
    // 每个用例都从基准 mock 重来：用例里注册的录制 mock 不许漏给下一个用例
    registerBaselineMocks();
    // `channel_list` 在共享 mock 里没有（见本文件 `beforeAll` 的说明），基准注册会把它留在原位，
    // 这里再补一次是为了让「重来」这件事在这一个 describe 里自洽。
    registerMock("channel_list", () => channelsFixture());
  });

  it("页头有「添加模型」入口；点开后 [docs/17 §3.2] 的六个控件一个不少（testid 不许改名）", async () => {
    renderPage();

    fireEvent.click(await screen.findByTestId("models-add-open"));
    for (const id of WIZARD_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }
    // 四个字段背后都得是**真 input**（否则真机 E2E 的 fill 填不进去）、开关是真 checkbox、提交是真按钮
    for (const id of ["models-add-id", "models-add-aliases", "models-add-group", "models-add-order"]) {
      expect(inputByTestId(id).tagName, `${id} 背后不是 input`).toBe("INPUT");
    }
    expect(checkboxByTestId("models-add-expose").getAttribute("type")).toBe("checkbox");
    expect(screen.getByTestId("models-add-submit").tagName).toBe("BUTTON");
  });

  it("真输入后提交 → 真的调用了 model_upsert，入参形状正确；成功后回显后端返回值并失效 qk.models", async () => {
    const list = recordingMock([modelView()]);
    registerMock("model_list", list.handler);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);
    // `qk.models` 失效这件事直接钉在 QueryClient 上（页面该做的就只有这一件）
    const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    fillByTestId("models-add-id", "ta-e2e-m-add");
    // 逗号分隔 → 去空、去重、保序（空项与重复项都不许进 payload）
    fillByTestId("models-add-aliases", " 新模型 ,flash-alias,新模型, ");
    fillByTestId("models-add-group", "网络接入");
    fillByTestId("models-add-order", "42");
    // expose 默认是开的，点一下关掉 —— 开关真的连在草稿上（不是画着好看的）
    fireEvent.click(checkboxByTestId("models-add-expose"));
    fireEvent.click(screen.getByTestId("models-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const args = upsert.calls[0]!;
    // 入参只有一个 `model`（契约 `model_upsert: { args: { model: Model }; result: Model }`）
    expect(Object.keys(args)).toEqual(["model"]);
    const draft = args["model"] as Record<string, unknown>;

    expect(Object.keys(draft).sort()).toEqual(["aliases", "display", "expose", "id"]);
    expect(draft["id"]).toBe("ta-e2e-m-add");
    expect(draft["aliases"]).toEqual(["新模型", "flash-alias"]);
    expect(draft["expose"]).toBe(false);

    const display = draft["display"] as Record<string, unknown>;
    expect(Object.keys(display).sort()).toEqual(["group", "order"]);
    expect(display["group"]).toBe("网络接入");
    expect(display["order"]).toBe(42);

    // 每一层的键都在契约的 casing 里（真机 `channel_arg_case_guard` 的口径）
    for (const key of [...Object.keys(args), ...Object.keys(draft), ...Object.keys(display)]) {
      expect(key).toMatch(/^[a-z][a-z0-9_]*$/);
    }

    // 回显的是**后端返回的那个 Model**，不是我们提交的草稿
    const saved = await screen.findByTestId("models-add-result");
    expect(saved.textContent).toContain("backend-saved-id");
    expect(saved.textContent).toContain("已对外暴露");
    expect(saved.textContent).not.toContain("ta-e2e-m-add");

    // 成功后 `qk.models` 失效：QueryClient 上真的收到了 `["models"]`，清单也真的重取了
    await waitFor(() => expect(list.calls.length).toBeGreaterThan(1));
    const modelInvalidations = invalidate.mock.calls.filter(
      ([filters]) => JSON.stringify(filters?.queryKey) === JSON.stringify(qk.models),
    );
    expect(modelInvalidations.length, "没有对 qk.models 失效").toBeGreaterThan(0);
    invalidate.mockRestore();
  });

  it("本地预检不过：id 重复 / id 形状非法 / order 负数 —— 一次请求都不发，并按字段给对应文案", async () => {
    // 目录里已经有 `deepseek-v4.1flash`（模型夹具的 id）—— 重复判定要有真数据才成立
    registerMock("model_list", () => [modelView()]);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    // ① id 与现有目录重复 —— 文案取 `models.add.issue.id.duplicate`
    //    （「已存在同 id 的条目」在各向导里另有通用键 `wizard.duplicateId`，模型向导用自己的这两条）
    fillByTestId("models-add-id", MODEL_ID);
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect((await screen.findByTestId("models-add-issue-id")).textContent).toContain("已经存在");
    expect(upsert.calls).toHaveLength(0);

    // ② id 形状非法（[docs/17 §3.2]：字母/数字开头，只含字母数字 . _ -）—— `models.add.issue.id.shape`
    fillByTestId("models-add-id", "-bad-id");
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect(screen.getByTestId("models-add-issue-id").textContent).toContain("只能由字母/数字开头");
    expect(upsert.calls).toHaveLength(0);

    // ③ order 负数 —— `models.add.issue.order.negative`
    fillByTestId("models-add-id", "brand-new-model");
    fillByTestId("models-add-order", "-1");
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect(screen.getByTestId("models-add-issue-order").textContent).toContain("不能是负数");
    // 上一条的 id 错必须消失（不是把历次文案堆在一起）
    expect(screen.queryByTestId("models-add-issue-id")).toBeNull();
    expect(upsert.calls).toHaveLength(0);

    // ④ order 不是整数（空串也走这条：契约里 `display.order` 是 number，没有「留空」的语义）
    fillByTestId("models-add-order", "");
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect(screen.getByTestId("models-add-issue-order").textContent).toContain("非负整数");
    expect(upsert.calls).toHaveLength(0);
    // 全程一条都没发出去 —— 结果区自然也没有「后端已保存」
    expect(screen.queryByTestId("models-add-result")).toBeNull();
  });

  it("四个分支的机器可读原因码就是 UI 取文案用的那一串（UI 不解析文案本身）", () => {
    const draft = (overrides: Partial<ModelDraft>): ModelDraft => ({
      ...EMPTY_MODEL_DRAFT,
      ...overrides,
    });

    expect(validateModelDraft(draft({ id: MODEL_ID }), [MODEL_ID])).toEqual([
      { field: "id", reason: "duplicate" },
    ]);
    expect(validateModelDraft(draft({ id: "-bad-id" }), [])).toEqual([
      { field: "id", reason: "shape" },
    ]);
    expect(validateModelDraft(draft({ id: "ok-model", order: "-1" }), [])).toEqual([
      { field: "order", reason: "negative" },
    ]);
    expect(validateModelDraft(draft({ id: "ok-model", order: "" }), [])).toEqual([
      { field: "order", reason: "notInteger" },
    ]);
    // 都合法时一条 issue 都没有 —— 「有 issue 就一次都不发」那段逻辑才敢这么写
    expect(validateModelDraft(draft({ id: "ok-model" }), [])).toEqual([]);
  });

  it("后端拒绝：原样显示 TA 码与后端文案（本地预检不替代后端的权威判定）", async () => {
    registerMock("model_list", () => [modelView()]);
    registerMock("model_upsert", () => {
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "models[1].id 与既有模型冲突",
      });
    });

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));
    fillByTestId("models-add-id", "ta-e2e-m-add");
    fireEvent.click(screen.getByTestId("models-add-submit"));

    const error = await screen.findByTestId("models-add-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("models[1].id");
    expect(screen.queryByTestId("models-add-result")).toBeNull();
  });
});

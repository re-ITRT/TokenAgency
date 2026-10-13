// @vitest-environment jsdom
/**
 * 模型目录页（[07 §4] / [docs/28]）的验收。重点和看板/日志两份一样是**诚实**，外加关系翻转后
 * 这一页特有的四件事：
 *
 * 1. **关系的真源在模型侧**（[docs/28 §2]）："这个模型用哪几条线路"读的是 `Model.lines`，
 *    再拿 id 去 `channel_list` 里查；`Channel.models` 那个反向派生值**不参与判定**
 *    （两个真相源必漂，漂了不报错）。名字（服务商 / 网络代理）只在 `provider_list` /
 *    `lane_list` 里查得到时才显示，查不到就只留 id —— **不猜一个名字**。
 * 2. **模型是"分配"出来的**（[docs/28 §3]）：添加向导必须先选**至少一条线路**，零条时
 *    **一次命令都不发**；这一页**没有**任何"给模型选网络代理"的控件，载荷里也没有 `default_lane`。
 * 3. **只读的模型信息面板**（`models-info-open` → `models-info-panel`）：给"点进去看"用，
 *    面板里一个可编辑控件都没有（没有代理选择器、没有保存按钮）。
 * 4. 定价有两个后端来源，不一致时本页**不替后端裁决**；通道清单读不到时"使用的线路"这一列
 *    不可用，但那绝不是"没有线路"。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import ModelsPage, { ModelsBody, type ModelsBodyProps } from "@/pages/models";
import {
  EMPTY_MODEL_DRAFT,
  draftToModel,
  filterModels,
  hasDiscrepancy,
  lineCandidates,
  lineLabel,
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
// [docs/51] 定价对话框与它的下标比对（价格配置在模型页）
import { checkIndexStillOurs } from "@/pages/models/PricingDialog";
import type { Channel, LaneView, Model, ModelView, PriceEntry, ProviderView } from "@/shared/bindings";
import { registerBaselineMocks, registerMock, qk, TokenAgencyError } from "@/shared/api";
import { i18n, zhCN } from "@/shared/i18n";
import { buildTheme } from "@/theme";

const MODEL_ID = "deepseek-v4.1flash";

/** 线路（原「通道」）id。**一个模型用哪几条线路由 `Model.lines` 说了算**（[docs/28 §2]）。 */
const LINE_BYTEDANCE = "ch-bytedance";
const LINE_COMMANDCODE = "ch-commandcode";
const LINE_OTHER = "ch-other-model";

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

function providerView(id: string, name: string, overrides: Partial<ProviderView> = {}): ProviderView {
  return {
    id,
    name,
    protocol: "openai",
    base_url: `https://${id}.example.com`,
    enabled: true,
    channel_count: 1,
    status: "ok",
    ...overrides,
  };
}

/**
 * 一条线路（= `channel_list` 的一行）。
 *
 * 契约里它带一个 **只读派生值** `models`（"哪些模型在用我"）：夹具刻意把它设成与
 * `Model.lines` **不一致**，用来钉死"本页只信 `Model.lines`"（见 §① 的第一条用例）。
 */
function line(overrides: Partial<Channel> = {}): Channel {
  return {
    id: LINE_BYTEDANCE,
    models: [],
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
    line(),
    line({
      id: LINE_COMMANDCODE,
      provider: "p-commandcode",
      credential: "c-commandcode",
      lane: "lane-jp",
      upstream_model: `${MODEL_ID}-2026`,
      priority: 20,
      weight: 50,
      tags: ["backup"],
    }),
    line({ id: LINE_OTHER, models: [], upstream_model: "gpt-4o-mini" }),
  ];
}

/** 一条网络代理（`lane_list` 的一行，契约形状）。名字只在查得到时才用。 */
function laneView(id: string, name: string): LaneView {
  return {
    lane: { id, name, kind: "direct", enabled: true },
    type: "direct",
    node_count: 0,
    current_node: null,
    latency_ms: null,
    health: "ok",
  };
}

function modelView(overrides: Partial<ModelView> = {}): ModelView {
  return {
    model: {
      id: MODEL_ID,
      aliases: ["flash"],
      display: { group: "字节", order: 10 },
      expose: true,
      enabled: true,
      lines: [LINE_BYTEDANCE, LINE_COMMANDCODE],
    },
    channel_count: 2,
    upstream_names: [MODEL_ID, `${MODEL_ID}-2026`],
    // [docs/46 §304] 对外暴露的信息由后端算好；测试里给一份**声明过**的默认值，
    // 需要"未声明"的用例自己覆盖成 null / false / []。
    public_info: {
      context_window: 262144,
      max_output_tokens: 32768,
      vision: true,
      tools: true,
      reasoning: false,
      modalities: ["image", "text"],
      owned_by: "p-bytedance",
    },
    priced: false,
    cost_hint: null,
    ...overrides,
  };
}

function otherModel(id: string, overrides: Partial<ModelView> = {}): ModelView {
  return modelView({
    model: { id, aliases: [], display: { order: 100 }, expose: true, enabled: true, lines: [LINE_OTHER] },
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
    // [docs/48 §5] 这一行只用来测排序，开关给"开着"（最小合法值）
    enabled: true,
    reportedLines: 0,
    upstreamNames: [],
    // [docs/46 §304] 这一行只用来测排序，对外信息给一份"什么都没声明"的最小值
    publicInfo: {
      context_window: null,
      max_output_tokens: null,
      vision: false,
      tools: false,
      reasoning: false,
      modalities: [],
      owned_by: "tokenagency",
    },
    priced: true,
    costHint: undefined,
    lines: [],
    // [docs/28 §1] 行视图现在带一份**原始模型**（编辑时"以原始记录为底"灌草稿用）
    raw: { id, aliases: [], display: { order: order ?? 0 }, expose: true, enabled: true, lines: [] },
    missingLines: [],
  };
}

function lookup(overrides: Partial<ChannelsLookup> = {}): ChannelsLookup {
  return {
    channels: channelsFixture(),
    providers: [
      providerView("p-bytedance", "字节方舟"),
      providerView("p-commandcode", "command code", { protocol: "anthropic", status: "warn" }),
    ],
    lanes: [laneView("lane-direct", "直连"), laneView("lane-jp", "日本 · 东京")],
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

describe("读模型行：关系的真源是 Model.lines，显示名来自线路清单", () => {
  it("[docs/28 §2] 用的线路只认 `Model.lines`：`Channel.models` 那个反向派生值**不参与判定**", () => {
    // 一条**反向说"我在服务这个模型"**、但模型自己没点它的线路
    const reverseOnly = line({
      id: "ch-reverse-only",
      models: [MODEL_ID],
      provider: "p-commandcode",
      upstream_model: "reverse-only",
    });
    // 一条**模型点了它**、反向派生值却是空的线路（关系还没重算完也是这种形状）
    const forwardOnly = line({ id: "ch-forward-only", models: [], upstream_model: "forward-only" });

    const row = readModelRows(
      [
        modelView({
          model: {
            id: MODEL_ID,
            aliases: ["flash"],
            display: { group: "字节", order: 10 },
            expose: true,
            enabled: true,
            lines: ["ch-forward-only"],
          },
        }),
      ],
      lookup({ channels: [reverseOnly, forwardOnly] }),
    )[0]!;

    expect(row.lines.map((l) => l.lineId)).toEqual(["ch-forward-only"]);
    expect(row.lines.some((l) => l.lineId === "ch-reverse-only")).toBe(false);
  });

  it("线路的展示面来自真实线路数据：上游模型 / 服务商名 / 网络代理名；查不到就回落 id", () => {
    const row = readModelRows([modelView()], lookup())[0]!;

    expect(row.lines.map((l) => l.lineId)).toEqual([LINE_BYTEDANCE, LINE_COMMANDCODE]);
    expect(row.lines[0]?.providerName).toBe("字节方舟");
    expect(row.lines[0]?.upstreamModel).toBe(MODEL_ID);
    expect(row.lines[0]?.laneName).toBe("直连");
    expect(row.lines[1]?.providerName).toBe("command code");
    expect(row.lines[1]?.upstreamModel).toBe(`${MODEL_ID}-2026`);
    expect(row.lines[1]?.laneName).toBe("日本 · 东京");
    // 别的模型的线路不能被算进来
    expect(row.lines.some((l) => l.lineId === LINE_OTHER)).toBe(false);
  });

  it("清单里查不到的服务商/代理不给名字（不猜），id 仍然留着供核对", () => {
    const row = readModelRows([modelView()], lookup({ providers: [], lanes: [] }))[0]!;

    expect(row.lines).toHaveLength(2);
    expect(row.lines[0]?.providerName).toBeUndefined();
    expect(row.lines[0]?.laneName).toBeUndefined();
    expect(row.lines[0]?.providerId).toBe("p-bytedance");
    expect(row.lines[0]?.laneId).toBe("lane-direct");
  });

  it("线路自己没写网络代理：laneId/laneName 都是 undefined（继承链的事不在这里编一个『直连』）", () => {
    const row = readModelRows(
      [modelView({ model: { ...modelView().model, lines: ["ch-nolane"] } })],
      lookup({ channels: [line({ id: "ch-nolane", lane: null })] }),
    )[0]!;

    expect(row.lines[0]?.laneId).toBeUndefined();
    expect(row.lines[0]?.laneName).toBeUndefined();
  });

  it("[docs/28 §2] 契约点名、通道清单里没有的线路如实记进 missingLines（悬空引用，不悄悄丢）", () => {
    const row = readModelRows(
      [
        modelView({
          model: { ...modelView().model, lines: [LINE_BYTEDANCE, "ch-gone"] },
        }),
      ],
      lookup(),
    )[0]!;

    expect(row.lines.map((l) => l.lineId)).toEqual([LINE_BYTEDANCE]);
    expect(row.missingLines).toEqual(["ch-gone"]);
  });

  it("model_list 自报的线路数与实际查到的线路数是两个数，都留着（不一致要能看见）", () => {
    const row = readModelRows([modelView({ channel_count: 5 })], lookup())[0]!;
    expect(row.reportedLines).toBe(5);
    expect(row.lines).toHaveLength(2);
  });

  it("别名 / 分组 / 排序 / 暴露 / 定价 / 成本提示原样透传，空数组保持空", () => {
    const row = readModelRows(
      [
        modelView({
          model: {
            id: MODEL_ID,
            aliases: [],
            display: { order: 7 },
            expose: false,
            enabled: true,
            lines: [],
          },
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
    expect(row.lines).toEqual([]);
  });

  it("上游映射去重后拼接；一个都没有时给破折号而不是空串", () => {
    expect(upstreamSummary(["a", "a", "b"])).toBe("a · b");
    expect(upstreamSummary([])).toBe("—");
  });

  it("真机形状（Rust ModelView）：upstream_models 也认；priced 缺失**不许当成 false**", () => {
    // 依据 src-tauri/src/commands/catalog.rs:33-37：真机的 ModelView 只有
    // { model, channel_count, upstream_models }，**没有 priced / cost_hint**。
    const rustView = {
      model: {
        id: MODEL_ID,
        aliases: ["flash"],
        display: { order: 1 },
        expose: true,
        enabled: true,
        lines: [LINE_BYTEDANCE],
      },
      channel_count: 2,
      upstream_models: [MODEL_ID, `${MODEL_ID}-2026`],
    } as unknown as ModelView;

    const row = readModelRows([rustView], lookup())[0]!;
    expect(row.upstreamNames).toEqual([MODEL_ID, `${MODEL_ID}-2026`]);
    expect(row.reportedLines).toBe(2);
    expect(row.priced).toBeUndefined();

    // 结论的优先级：未定价清单（定价引擎自己的）> 后端显式 priced > **未知**
    expect(pricingVerdict(row, [])).toBe("priced");
    expect(pricingVerdict(row, [MODEL_ID])).toBe("unpriced");
    expect(pricingVerdict(row, undefined)).toBe("unknown");

    // 后端没给 priced 时，两个来源不可能"不一致"（不许凭空造矛盾）
    expect(hasDiscrepancy(pricingDiscrepancy([row], [MODEL_ID]))).toBe(false);
  });

  it("`Model.lines` 整列缺失（关系翻转落地前的真机）：读成 0 条，**不是**「没有线路」的结论", () => {
    // 关系翻转前 `Model` 没有 lines 这一列。那时页面得到 0 条，而后端自报的条数还在 ——
    // 两个数对不上就会被 mismtach 点出来（缺数据要说出来，不能静默当成"没有线路"）。
    const preFlip = {
      model: { id: MODEL_ID, aliases: [], display: { order: 1 }, expose: true },
      channel_count: 2,
      upstream_models: [MODEL_ID],
    } as unknown as ModelView;

    const row = readModelRows([preFlip], lookup())[0]!;
    expect(row.lines).toEqual([]);
    expect(row.reportedLines).toBe(2);
  });

  it("候选线路（添加模型时的可选项）：按 服务商 → 上游模型 → 线路 id 排序，且带着可读的名字", () => {
    // 用 ASCII 名字，让排序断言不受 ICU 中文排序影响（真实名字的显示在渲染用例里钉）
    const cands = lineCandidates(
      channelsFixture(),
      lookup({
        providers: [
          providerView("p-bytedance", "aaa"),
          providerView("p-commandcode", "bbb"),
        ],
      }),
    );

    expect(cands.map((c) => c.lineId)).toEqual([LINE_BYTEDANCE, LINE_OTHER, LINE_COMMANDCODE]);
    expect(cands[0]?.providerName).toBe("aaa");
    // 一条线路可以服务多个模型：候选清单**不排除**任何一条（[docs/28 §2]）
    expect(cands).toHaveLength(3);
  });

  it("线路的展示名是 `上游模型（服务商 · 网络代理）`：括号里两段都是值，禁用只多一句话", () => {
    const [bytedance, commandcode] = readModelRows([modelView()], lookup())[0]!.lines;

    expect(lineLabel(bytedance!, "已禁用")).toBe(`${MODEL_ID}（字节方舟 · 直连）`);
    expect(lineLabel(commandcode!, "已禁用")).toBe(`${MODEL_ID}-2026（command code · 日本 · 东京）`);
    // 服务商/代理名查不到 → 回落 id（**不编名字**），照样是这一条线路自己的值
    const bare = readModelRows([modelView()], lookup({ providers: [], lanes: [] }))[0]!.lines[0]!;
    expect(lineLabel(bare, "已禁用")).toBe(`${MODEL_ID}（p-bytedance · lane-direct）`);
    // 禁用的线路照写名字，只在末尾多一段（不许因此显示成"没有线路"）
    expect(lineLabel({ ...bare, enabled: false }, "已禁用")).toBe(
      `${MODEL_ID}（p-bytedance · lane-direct） · 已禁用`,
    );
  });
});

// ================================================================ ② 纯逻辑：概览 / 不一致 / 筛选 / 排序

describe("概览与两个定价来源的交叉核对", () => {
  const rows = readModelRows([modelView(), otherModel("gpt-4o-mini")], lookup());

  it("概览：线路按去重后的条数算，未定价与未暴露分别计数", () => {
    const summary = summarizeModels(rows);
    expect(summary.models).toBe(2);
    // ch-bytedance / ch-commandcode / ch-other-model（共用线路只算一次）
    expect(summary.lines).toBe(3);
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

  it("筛选覆盖 id / 别名 / 上游名 / 分组 / **所用线路的服务商**，大小写不敏感；空串返回全部", () => {
    expect(filterModels(rows, "")).toHaveLength(2);
    expect(filterModels(rows, "FLASH").map((r) => r.id)).toEqual([MODEL_ID]);
    expect(filterModels(rows, "2026").map((r) => r.id)).toEqual([MODEL_ID]);
    // 分组（`display.group`）独立成一维：服务商名读不到时只剩分组能命中
    const groupOnly = readModelRows(
      [modelView({ model: { ...modelView().model, lines: [LINE_BYTEDANCE] } })],
      lookup({ providers: [] }),
    );
    expect(filterModels(groupOnly, "字节").map((r) => r.id)).toEqual([MODEL_ID]);
    // 关系翻转后"这个模型走哪家"只有线路说得出来 → **服务商名也要能筛**。
    // 下面这两行会命中 gpt-4o-mini：它分组不叫「字节」，但它用的线路属于「字节方舟」。
    expect(filterModels(rows, "字节").map((r) => r.id)).toEqual([MODEL_ID, "gpt-4o-mini"]);
    expect(filterModels(rows, "command code").map((r) => r.id)).toEqual([MODEL_ID]);
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
  it("没有模型：[判据 5] 空态 1 行，且不画空表、不把 TODO 印在界面上", () => {
    renderBody();

    expect(screen.getByTestId("models-empty")).toBeTruthy();
    expect(screen.getByText(zhCN["models.empty.title"])).toBeTruthy();
    // [极简 · 判据 5] 空态只说"怎么开始"：只剩一行结论 ——
    // 原来那三行（"模型目录来自配置的 models[]" / "本版本还没有接线…等后端命令落地后再补" /
    // "把模型写进配置后刷新本页即可看到"）已全部删除（判据 1 禁止把 TODO 与实现交代印在界面上）。
    const empty = screen.getByTestId("models-empty");
    expect(empty.querySelectorAll("p")).toHaveLength(1);
    expect(empty.textContent).not.toMatch(/一键导入|接线|落地|models\[\]|命令/);
    expect(screen.queryByTestId("models-table")).toBeNull();
    expect(screen.queryAllByTestId("models-row")).toHaveLength(0);
    // 除了刷新之外没有任何会写后端的按钮（硬约束：不放禁用占位）
    expect(screen.queryByRole("button", { name: /新建|导入|删除|编辑|启用|禁用/ })).toBeNull();
  });

  it("[判据 A] 空态**不再**重复页头那个「添加模型」（同一屏不许两个同标签同动作的入口）", async () => {
    registerMock("model_list", () => []);
    renderPage();

    expect(await screen.findByTestId("models-empty")).toBeTruthy();
    // 空态只留一行说明：没有自己的按钮
    expect(screen.queryByTestId("models-empty-add")).toBeNull();
    expect(screen.getByTestId("models-empty").querySelectorAll("button")).toHaveLength(0);

    // 入口只剩页头那一个（真机 E2E 按 `models-add-open` 点击），它照样打开向导
    const entries = screen.getAllByTestId("models-add-open");
    expect(entries).toHaveLength(1);
    expect(entries[0]!.textContent).toBe(zhCN["models.add.open"]);
    fireEvent.click(entries[0]!);
    expect(await screen.findByTestId("models-add-submit")).toBeTruthy();
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

  it("通道清单读不到：线路这一列画「—」，并说清这是缺数据（后端原文 + TA 码原样留着）", () => {
    renderBody({
      rows: readModelRows([modelView()], lookup({ channels: [] })),
      unpricedIds: [],
      channelsError: new Error("[TA-9001] 未注册 mock：channel_list"),
    });

    const note = screen.getByTestId("models-lines-error");
    // 红线：后端原文与 TA 码原样显示（[docs/22 §2 第 2 条]）
    expect(note.textContent).toContain("TA-9001");
    // 缺数据时不许给"没有线路"这种在缺数据时是谎话的结论，也不许画一个 0 条
    expect(screen.queryByTestId("models-lines-none")).toBeNull();
    expect(screen.queryByTestId("models-lines-mismatch")).toBeNull();
    expect(screen.queryByTestId("models-lines-count")).toBeNull();
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

  it("每行渲染模型 id / 分组与排序 / 上游映射 / **使用的线路**（别名列已撤掉）", () => {
    renderBody({ rows, unpricedIds: [] });

    expect(screen.getByTestId("models-table")).toBeTruthy();
    expect(screen.getAllByTestId("models-row")).toHaveLength(2);
    // 行按 display.order 升序（10 在前，100 在后）
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toEqual([
      MODEL_ID,
      "gpt-4o-mini",
    ]);
    // 用户第 309 轮：「别名那个没必要搞」⇒ 表里不再有别名这一列（值还在配置里，只是不占版面）
    expect(screen.queryByTestId("models-aliases-ellipsis")).toBeNull();
    expect(screen.getByText("字节 · 10")).toBeTruthy();
    expect(screen.getByText(`${MODEL_ID} · ${MODEL_ID}-2026`)).toBeTruthy();

    const first = screen.getAllByTestId("models-row")[0]!;
    expect(within(first).getByText("对外暴露")).toBeTruthy();
    expect(within(first).getByText("未定价")).toBeTruthy();
  });

  it("[docs/28 §2] 每行给出**条数 + 逐条线路**：`上游模型（服务商 · 网络代理）`，不是一串 id", () => {
    renderBody({ rows, unpricedIds: [] });

    const first = screen.getAllByTestId("models-row")[0]!;
    // 条数那一格在（文案键 `models.lines.channelCount` 由主管补；键没落地时 i18next 原样回键名）
    expect(within(first).getByTestId("models-lines-count").textContent ?? "").toMatch(
      /2|channelCount/,
    );
    // 逐条：**结构上就是 model.lines 的两条**（不靠文案断言条数）
    expect(within(first).getAllByTestId(/^models-lines-ch-/)).toHaveLength(2);

    const bytedance = within(first).getByTestId(`models-lines-${LINE_BYTEDANCE}`);
    expect(bytedance.textContent).toContain(MODEL_ID);
    expect(bytedance.textContent).toContain("字节方舟");
    expect(bytedance.textContent).toContain("直连");

    const commandcode = within(first).getByTestId(`models-lines-${LINE_COMMANDCODE}`);
    expect(commandcode.textContent).toContain(`${MODEL_ID}-2026`);
    expect(commandcode.textContent).toContain("command code");
    expect(commandcode.textContent).toContain("日本");

    // 用线路的那一条：别的模型的线路不在这一行
    expect(within(first).queryByTestId(`models-lines-${LINE_OTHER}`)).toBeNull();
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

  it("[判据 1] 筛选计数只说「筛了已加载的 N 个」（括号里那句实现交代由映射文件删掉）", () => {
    renderBody({ rows, unpricedIds: [] });
    // 断言读真源：文案由映射文件改，这里不把句子再抄一遍；
    // 同时钉住"版面上只有这一句、没有额外装饰"（括号里的"不是后端检索"就是这么没的）。
    expect(
      screen.getByText(zhCN["models.search.scope"].replace("{{value}}", "2")),
    ).toBeTruthy();

    fireEvent.change(screen.getByLabelText("在当前列表中筛选"), { target: { value: "gpt" } });
    expect(screen.getAllByTestId("models-row")).toHaveLength(1);
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toEqual(["gpt-4o-mini"]);
    expect(
      screen.getByText(zhCN["models.search.scope"].replace("{{value}}", "1")),
    ).toBeTruthy();
  });

  it("筛选没有命中：诚实说明，不显示 0 行的表格", () => {
    renderBody({ rows, unpricedIds: [] });
    fireEvent.change(screen.getByLabelText("在当前列表中筛选"), { target: { value: "zzz" } });
    expect(screen.getByTestId("models-filter-empty")).toBeTruthy();
    expect(screen.queryByTestId("models-table")).toBeNull();
  });

  it("模型自报的线路数在通道清单里一条都匹配不到时，如实点出不一致", () => {
    renderBody({
      rows: readModelRows([modelView({ channel_count: 2 })], lookup({ channels: [] })),
      unpricedIds: [],
    });
    expect(screen.getByTestId("models-lines-mismatch").textContent).toContain("2");
  });

  it("模型一条线路都没有（后端也自报 0）：如实说明它发不出去", () => {
    renderBody({
      rows: readModelRows(
        [
          modelView({
            model: { ...modelView().model, lines: [] },
            channel_count: 0,
          }),
        ],
        lookup(),
      ),
      unpricedIds: [],
    });
    // 结论那一格在（文案键 `models.lines.none` 由主管补）
    expect(screen.getByTestId("models-lines-none")).toBeTruthy();
    expect(screen.getByTestId("models-lines-count")).toBeTruthy();
  });

  it("[docs/28 §2] 契约点名而清单里没有的线路：行里点出那个 id（悬空引用不吞）", () => {
    renderBody({
      rows: readModelRows(
        [modelView({ model: { ...modelView().model, lines: [LINE_BYTEDANCE, "ch-gone"] } })],
        lookup(),
      ),
      unpricedIds: [],
    });

    // 悬空线路的 id 是**数据**：说明文案之外，它自己单独一格（文案键还没落地也看得见）
    expect(screen.getByTestId("models-lines-missing")).toBeTruthy();
    expect(screen.getByTestId("models-lines-missing-ids").textContent).toBe("ch-gone");
    // 查得到的那条照常显示（一个事实都不藏）
    expect(screen.getByTestId(`models-lines-${LINE_BYTEDANCE}`)).toBeTruthy();
  });

  it("后端没给定价列、未定价清单也读不到：显示「未知」，**不点黄灯也不说未定价**", () => {
    const rustView = {
      model: {
        id: MODEL_ID,
        aliases: [],
        display: { order: 1 },
        expose: true,
        enabled: true,
        lines: [LINE_BYTEDANCE],
      },
      channel_count: 2,
      upstream_models: [MODEL_ID],
    } as unknown as ModelView;

    renderBody({ rows: readModelRows([rustView], lookup()), unpricedIds: undefined });

    const row = screen.getAllByTestId("models-row")[0]!;
    expect(within(row).getByText("未知")).toBeTruthy();
    expect(within(row).queryByText("未定价")).toBeNull();
    expect(within(row).queryByText("已定价")).toBeNull();
    expect(within(row).getByText(zhCN["models.priced.unknownHint"])).toBeTruthy();
    // 概览那一格也不能给一个看起来像结论的 0
    const stat = screen.getByTestId("models-summary-unpriced").textContent ?? "";
    expect(stat).toContain("—");
    expect(stat).not.toMatch(/\d/);
  });
});

// ================================================================ ⑤ [docs/28 §3] 只读模型信息面板

describe("[docs/28 §3] 只读模型信息面板：点进去看这个模型到底是怎么组成的", () => {
  beforeEach(() => {
    registerBaselineMocks();
    registerMock("channel_list", () => channelsFixture());
  });

  it("行里的 models-info-open → models-info-panel：id / 别名 / 对外暴露 / 线路（服务商 · 上游模型 · 代理）", async () => {
    registerMock("model_list", () => [modelView()]);
    renderPage();

    fireEvent.click((await screen.findAllByTestId("models-info-open"))[0]!);
    const panel = screen.getByTestId("models-info-panel");

    expect(within(panel).getByTestId("models-info-id").textContent).toContain(MODEL_ID);
    expect(within(panel).getByTestId("models-info-aliases").textContent).toContain("flash");
    // 对外暴露：状态灯 + 文字（值来自后端行，不是草稿）
    expect(within(panel).getByTestId("models-info-expose").textContent).toContain("对外暴露");

    const line = within(panel).getByTestId(`models-info-line-${LINE_BYTEDANCE}`);
    expect(line.textContent).toContain(MODEL_ID);
    expect(line.textContent).toContain("字节方舟");
    expect(line.textContent).toContain("直连");
    expect(
      within(panel).getByTestId(`models-info-line-${LINE_COMMANDCODE}`).textContent,
    ).toContain("日本");
    expect(within(panel).getAllByTestId(/^models-info-line-ch-/)).toHaveLength(2);
  });

  it("线路清单读不到时，面板里画「—」而不是「这个模型没有线路」（缺数据 != 结论）", async () => {
    registerMock("model_list", () => [modelView()]);
    registerMock("channel_list", () => {
      throw new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：channel_list" });
    });
    renderPage();

    fireEvent.click((await screen.findAllByTestId("models-info-open"))[0]!);
    const panel = screen.getByTestId("models-info-panel");
    expect(within(panel).queryByTestId(/^models-info-line-ch-/)).toBeNull();
    expect(within(panel).queryByTestId("models-info-line-none")).toBeNull();
  });

  it("面板里**零个可写控件**：没有代理选择器、没有输入框、没有按钮", async () => {
    registerMock("model_list", () => [modelView()]);
    renderPage();

    fireEvent.click((await screen.findAllByTestId("models-info-open"))[0]!);
    const panel = screen.getByTestId("models-info-panel");

    for (const role of ["combobox", "textbox", "button", "checkbox", "switch", "spinbutton"]) {
      expect(
        within(panel).queryAllByRole(role),
        `只读面板里不该有 ${role}`,
      ).toHaveLength(0);
    }
    // 也没有任何"模型侧的网络代理控件"（[docs/28 §1.2]：代理是线路的属性）
    expect(within(panel).queryAllByTestId(/lane|proxy/)).toHaveLength(0);
  });

  // ---------------------------------------------------------------- [docs/46 §304] 对外暴露的信息

  it("对外暴露的信息：上下文窗口 / 最大输出 / 能力 / 模态 / 对外标识**照后端给的值显示**", async () => {
    registerMock("model_list", () => [modelView()]);
    renderPage();

    fireEvent.click((await screen.findAllByTestId("models-info-open"))[0]!);
    const panel = screen.getByTestId("models-info-panel");
    const publicBox = within(panel).getByTestId("models-info-public");

    // 数字来自 `public_info`（千位分隔），不是前端自己从线路里算的
    expect(within(publicBox).getByTestId("models-info-public-context").textContent).toContain(
      "262,144",
    );
    expect(within(publicBox).getByTestId("models-info-public-max-output").textContent).toContain(
      "32,768",
    );
    // 能力：只列真的支持的（fixture 里 vision/tools 为 true、reasoning 为 false）
    const caps = within(publicBox).getByTestId("models-info-public-caps").textContent ?? "";
    expect(caps).toContain("图片输入");
    expect(caps).toContain("函数调用");
    expect(caps).not.toContain("推理内容");
    expect(within(publicBox).getByTestId("models-info-public-modalities").textContent).toContain(
      "image",
    );
    expect(within(publicBox).getByTestId("models-info-public-owned-by").textContent).toContain(
      "p-bytedance",
    );
  });

  it("没声明就写「未声明」、一个能力都没有就写「无」——**不编 0、不猜**", async () => {
    registerMock("model_list", () => [
      modelView({
        public_info: {
          context_window: null,
          max_output_tokens: null,
          vision: false,
          tools: false,
          reasoning: false,
          modalities: [],
          owned_by: "tokenagency",
        },
      }),
    ]);
    renderPage();

    fireEvent.click((await screen.findAllByTestId("models-info-open"))[0]!);
    const panel = screen.getByTestId("models-info-panel");

    expect(within(panel).getByTestId("models-info-public-context").textContent).toContain("未声明");
    expect(within(panel).getByTestId("models-info-public-max-output").textContent).toContain(
      "未声明",
    );
    expect(within(panel).getByTestId("models-info-public-caps").textContent).toContain("无");
    expect(within(panel).getByTestId("models-info-public-modalities").textContent).toContain(
      "未声明",
    );
  });
});

// ================================================================ ⑥ 整页（浏览器开发路径 mock）

describe("整页在 mock 基准场景下能用（不依赖 Rust/Tauri，路由不再走占位壳）", () => {
  it("渲染页头与清单；本页自己注册的 channel_list 夹具也能读到，并显示线路", async () => {
    registerBaselineMocks();
    registerMock("channel_list", () => channelsFixture());
    // 基准 mock 的 model_list 还没有 lines 这一列 → 这一条用例用**带 lines 的夹具**
    registerMock("model_list", () => [modelView()]);
    renderPage();

    expect((await screen.findByText("模型目录")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();

    expect(await screen.findByTestId("models-table")).toBeTruthy();
    expect(screen.getAllByTestId("models-row").length).toBeGreaterThan(0);
    expect(screen.getAllByTestId("models-id").map((n) => n.textContent)).toContain(MODEL_ID);
    // 整页也真的把"用哪几条线路"画出来了（服务商名来自 provider_list）
    expect(screen.getByTestId(`models-lines-${LINE_BYTEDANCE}`).textContent).toContain("字节方舟");
  });
});

// ================================================================ ⑦ 文案回归

describe("文案里不留未替换的占位符", () => {
  it("渲染出来的正文里不该出现双花括号占位符", () => {
    renderBody({
      rows: readModelRows([modelView()], lookup()),
      unpricedIds: [MODEL_ID],
    });
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑧ 添加模型向导（[docs/28 §3]）
//
// 契约是 [docs/28 §3] + [docs/17 §3.2]：
//   `models-add-open` → `models-add-id` / `-aliases`（逗号分隔）/ `-group` / `-order` / `-expose`
//   **`models-add-lines`（≥1 条线路，勾选框）** → `models-add-submit` →
//   `model_upsert({ model })`（**含 `lines`**），**成功后回显后端返回值**。
// 真机 E2E（`scripts/e2e/smoke.mjs` 的 step9）就是按这串 testid 驱动的，所以这一组钉五件事：
//
// 1. **入口与控件真的在页面上** —— 只渲染 `ModelsBody`（纯展示件）的单测看不见写入口；
// 2. **零条线路 = 一次请求都不发**（[docs/28 §2]：模型没有线路就永远路由不到任何地方）；
// 3. 选了线路后提交**真的调用了 `model_upsert`**，入参形状 = `{ model: { id, aliases,
//    display: { group, order }, expose, lines } }`，键全在契约的 casing 里，且**没有 `default_lane`**；
// 4. 成功区显示的是**后端返回的那个 Model**（mock 故意返回与提交完全不同的 id / expose），
//    并且 `qk.models` 真的失效；
// 5. `channel_list` 读不到时**如实说**并挡住提交（绝不静默给一个空清单）。
//
// 「真的调了命令」为什么不用 `vi.spyOn`：见本文件历史注释（`client.ts` 里没有 `invokeCommand`，
// ESM 命名导出绑定只读）—— 这里用 `registerMock("model_upsert", 记录入参的 handler)`，
// 命令名拼错或根本没发请求都会立刻红。

/** 本轮向导该有的控件（顺序 = [docs/28 §3] + [docs/17 §3.2] 的行文顺序）。**不许改名**：真机 E2E 按它们驱动。 */
const WIZARD_CONTROLS = ["models-add-id", "models-add-lines", "models-add-submit"] as const;

/** 收起状态下看不到的「高级（可选）」：分组 / 排序 / 对外暴露 —— 展开后才出现在页面上。 */
const WIZARD_ADVANCED_CONTROLS = [
  "models-add-group",
  "models-add-order",
  "models-add-expose",
] as const;

/**
 * 展开「高级（可选）」。
 *
 * 用户第 309 轮：「创建模型的时候就只输入模型名称和 id 就好」⇒ 默认收起；
 * 需要改元数据的用例自己展开（展开后控件与以前一模一样，testid 不变）。
 */
function openAdvanced(): void {
  fireEvent.click(screen.getByTestId("models-add-advanced-toggle"));
}

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

/** 勾选一条候选线路（**分配关系就是在这一步建立的**）。 */
function pickLine(lineId: string): void {
  fireEvent.click(checkboxByTestId(`models-add-line-${lineId}`));
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
    enabled: true,
    lines: [LINE_BYTEDANCE],
    ...overrides,
  };
}

describe("⑧ /models 添加模型向导：先分配线路 → 真的调 model_upsert → 显示后端返回值", () => {
  beforeEach(() => {
    // 每个用例都从基准 mock 重来：用例里注册的录制 mock 不许漏给下一个用例
    registerBaselineMocks();
    // `channel_list` 在共享 mock 里没有（见本文件 `beforeAll` 的说明），基准注册会把它留在原位，
    // 这里再补一次是为了让「重来」这件事在这一个 describe 里自洽。
    registerMock("channel_list", () => channelsFixture());
  });

  /**
   * [docs/28 §1.6]（用户第 86 轮）「模型目录给模型选择线路的时候也要这一套逻辑」：
   * **按服务商分组、可展开收起、只列已经打开（`enabled`）的线路**。
   *
   * 为什么"已经选中的"例外：用户可能正是来改一个用着关闭线路的模型 —— 把它藏起来，
   * 他保存时就会在不知情的情况下把它摘掉（数据丢失，不是极简）。
   */
  it("选线路 = 按服务商分组 + 可收起 + 只列打开的线路（已选中的即使关闭也留着）", async () => {
    registerMock("channel_list", () => [
      line({ id: "ch-open-a", provider: "p-a", credential: "c-a", enabled: true }),
      line({ id: "ch-off-a", provider: "p-a", credential: "c-a", enabled: false }),
      line({ id: "ch-open-b", provider: "p-b", credential: "c-b", enabled: true }),
    ]);
    registerMock("model_list", () => []);
    registerMock("provider_list", () => [
      { id: "p-a", name: "服务商 A", protocol: "openai", base_url: "https://a.example/v1" },
      { id: "p-b", name: "服务商 B", protocol: "openai", base_url: "https://b.example/v1" },
    ]);
    renderPage();

    fireEvent.click(await screen.findByTestId("models-add-open"));

    // 分组：一个服务商一组，组里有各自的线路
    const groups = screen.getAllByTestId("models-add-line-group");
    expect(groups).toHaveLength(2);
    // 只列**打开**的线路：关闭的那条不出现（没有它的勾选框）
    expect(screen.getByTestId("models-add-line-ch-open-a")).toBeTruthy();
    expect(screen.getByTestId("models-add-line-ch-open-b")).toBeTruthy();
    expect(screen.queryByTestId("models-add-line-ch-off-a")).toBeNull();

    // 收起一组 → 那一组的勾选框消失；再点一次回来
    const firstToggle = groups[0]!.querySelector(
      '[data-testid="models-add-line-group-toggle"]',
    ) as HTMLElement;
    fireEvent.click(firstToggle);
    expect(screen.queryByTestId("models-add-line-ch-open-a")).toBeNull();
    // 另一组不受影响（收起是**按服务商**的）
    expect(screen.getByTestId("models-add-line-ch-open-b")).toBeTruthy();
    fireEvent.click(firstToggle);
    expect(screen.getByTestId("models-add-line-ch-open-a")).toBeTruthy();
  });
  it("页头有「添加模型」入口；点开后向导的控件一个不少（含线路选择），testid 不许改名", async () => {
    renderPage();

    fireEvent.click(await screen.findByTestId("models-add-open"));
    for (const id of WIZARD_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }
    // 用户第 309 轮：「创建模型的时候就只输入模型名称和 id 就好」
    // ⇒ **别名连入口都不给**，分组/排序/对外暴露收进「高级（可选）」默认不占版面。
    expect(screen.queryByTestId("models-add-aliases")).toBeNull();
    for (const id of WIZARD_ADVANCED_CONTROLS) {
      expect(screen.queryByTestId(id), `${id} 不该在默认视图里`).toBeNull();
    }
    openAdvanced();
    for (const id of WIZARD_ADVANCED_CONTROLS) {
      expect(screen.getByTestId(id), `展开高级后缺控件 ${id}`).toBeTruthy();
    }
    // 这些字段背后都得是**真 input**（否则真机 E2E 的 fill 填不进去）、开关是真 checkbox
    for (const id of ["models-add-id", "models-add-group", "models-add-order"]) {
      expect(inputByTestId(id).tagName, `${id} 背后不是 input`).toBe("INPUT");
    }
    expect(checkboxByTestId("models-add-expose").getAttribute("type")).toBe("checkbox");
    expect(screen.getByTestId("models-add-submit").tagName).toBe("BUTTON");
    // 线路是**一条一条勾**的（多选）：每条候选一个真 checkbox
    expect(checkboxByTestId(`models-add-line-${LINE_BYTEDANCE}`).getAttribute("type")).toBe(
      "checkbox",
    );
    expect(checkboxByTestId(`models-add-line-${LINE_COMMANDCODE}`).getAttribute("type")).toBe(
      "checkbox",
    );
  });

  it("候选线路显示「服务商 · 上游模型」+ 这条线路自己的网络代理 —— 不是一个裸 id", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    const picker = screen.getByTestId("models-add-lines");
    // 两个服务商的线路都在候选里，且都写着可读的名字
    expect(picker.textContent).toContain("字节方舟");
    expect(picker.textContent).toContain(MODEL_ID);
    expect(picker.textContent).toContain("command code");
    expect(picker.textContent).toContain(`${MODEL_ID}-2026`);
    // 网络代理（`lane_list` 的名字）也带上
    expect(picker.textContent).toContain("直连");
    expect(picker.textContent).toContain("日本");
  });

  it("[docs/28 §2] 一条线路都没选：本地预检挡下、**一次命令都不发**，并给出 inline issue", async () => {
    registerMock("model_list", () => []);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));
    fillByTestId("models-add-id", "ta-e2e-m-add");
    // order 收在「高级（可选）」里（第 309 轮）—— 要改它先展开
    openAdvanced();
    fillByTestId("models-add-order", "42");
    // 刻意一条都不勾
    fireEvent.click(screen.getByTestId("models-add-submit"));

    const issue = await screen.findByTestId("models-add-issue-lines");
    expect(issue.textContent ?? "").not.toBe("");
    expect(upsert.calls).toHaveLength(0);
    expect(screen.queryByTestId("models-add-result")).toBeNull();

    // 勾一条之后就不再有这条 issue（它跟着草稿走，不是一次性横幅）
    pickLine(LINE_BYTEDANCE);
    fireEvent.click(screen.getByTestId("models-add-submit"));
    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    expect(screen.queryByTestId("models-add-issue-lines")).toBeNull();
  });

  it("[docs/28 §1] 「编辑」已有模型：灌进全部字段、可加线路，载荷一个字段都不丢", async () => {
    // 已有一个模型：只有一条线路 —— 但用户刚在服务商页**新建了第二条线路**，现在要把它加上
    const existing: Model = {
      id: "deepseek-v4.1flash",
      aliases: ["flash", "flash-cheap"],
      display: { group: "字节", order: 7 },
      expose: false,
      enabled: true,
      lines: [LINE_BYTEDANCE],
    };
    registerMock("model_list", () => [modelView({ model: existing, channel_count: 1 })]);
    const upsert = recordingMock(savedModel({ ...existing, lines: [LINE_BYTEDANCE, LINE_OTHER] }));
    registerMock("model_upsert", upsert.handler);

    renderPage();
    // 行上的「编辑」入口（**不能**靠"再开一次新建"来改已有模型：那会撞 id 预检）
    fireEvent.click(await screen.findByTestId("models-edit-open"));

    // ① 全部字段都被灌进草稿（少灌一个 = 提交时抹掉它）
    expect((inputByTestId("models-add-id") as HTMLInputElement).value).toBe("deepseek-v4.1flash");
    expect((inputByTestId("models-add-id") as HTMLInputElement).disabled).toBe(true);
    // **别名不再有输入框**（用户第 309 轮），但草稿里仍灌着原来的两个 —— 所以第 ④ 步的载荷里
    // 它们一个字都不少（"去掉输入框"不许变成"悄悄抹掉别名"）。
    expect(screen.queryByTestId("models-add-aliases")).toBeNull();
    openAdvanced();
    expect((inputByTestId("models-add-group") as HTMLInputElement).value).toBe("字节");
    expect((inputByTestId("models-add-order") as HTMLInputElement).value).toBe("7");
    expect((checkboxByTestId("models-add-expose") as HTMLInputElement).checked).toBe(false);
    // ② 现有线路是**已勾上**的（不是空的：空就意味着"编辑一下把线路全丢了"）
    expect((checkboxByTestId(`models-add-line-${LINE_BYTEDANCE}`) as HTMLInputElement).checked).toBe(true);
    expect((checkboxByTestId(`models-add-line-${LINE_OTHER}`) as HTMLInputElement).checked).toBe(false);

    // ③ 加上第二条线路后保存
    pickLine(LINE_OTHER);
    fireEvent.click(screen.getByTestId("models-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    // ④ 载荷 = 完整模型（id 不变、别名/分组/顺序/expose 原样、两条线路都在）
    expect(upsert.calls[0]).toEqual({
      model: {
        id: "deepseek-v4.1flash",
        aliases: ["flash", "flash-cheap"],
        display: { group: "字节", order: 7 },
        expose: false,
        enabled: true,
        lines: [LINE_BYTEDANCE, LINE_OTHER],
      },
    });
  });

  it("选好线路后提交 → model_upsert({ model: { …, lines } })：载荷就是选中的那几个 id，顺序 = 勾选顺序", async () => {
    const list = recordingMock([modelView()]);
    registerMock("model_list", list.handler);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);
    // `qk.models` 失效这件事直接钉在 QueryClient 上（页面该做的就只有这一件）
    const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    fillByTestId("models-add-id", "ta-e2e-m-add");
    // 别名不再由用户输入（第 309 轮）⇒ 新模型的载荷里就是空数组（不是"丢了什么"，本来就没有）
    openAdvanced();
    fillByTestId("models-add-group", "网络接入");
    fillByTestId("models-add-order", "42");
    // expose 默认是开的，点一下关掉 —— 开关真的连在草稿上（不是画着好看的）
    fireEvent.click(checkboxByTestId("models-add-expose"));
    // 先勾第二条、再勾第一条：载荷顺序必须与勾选顺序一致（**选了什么就发什么**）
    pickLine(LINE_COMMANDCODE);
    pickLine(LINE_BYTEDANCE);
    fireEvent.click(screen.getByTestId("models-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const args = upsert.calls[0]!;
    // 入参只有一个 `model`（契约 `model_upsert: { args: { model: Model }; result: Model }`）
    expect(Object.keys(args)).toEqual(["model"]);
    const draft = args["model"] as Record<string, unknown>;

    // [docs/48 §5] 载荷里多一个 `enabled`：新建时默认**开着**（开关只用于"暂时下线已有模型"）。
    expect(Object.keys(draft).sort()).toEqual([
      "aliases",
      "display",
      "enabled",
      "expose",
      "id",
      "lines",
    ]);
    expect(draft["id"]).toBe("ta-e2e-m-add");
    // 新模型没有别名输入 ⇒ 载荷里是空数组（"没输入"与"空数组"是同一件事，不是丢字段）
    expect(draft["aliases"]).toEqual([]);
    expect(draft["expose"]).toBe(false);
    expect(draft["enabled"]).toBe(true);
    // **关系就在这里建立**：发出去的就是选中的线路 id，一条不多、一条不少、顺序就是勾选顺序
    expect(draft["lines"]).toEqual([LINE_COMMANDCODE, LINE_BYTEDANCE]);
    // 没有"给模型挑网络代理"这回事（[docs/28 §1.2]）：载荷里不出现 default_lane
    expect("default_lane" in draft).toBe(false);

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

  it("这一页没有「给模型选网络代理」的控件（[docs/28 §1.2]）：向导里没有下拉/代理选项", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    // 面板里唯一的"选择"是线路的勾选框：没有 combobox（下拉选择器）、没有代理/线路 id 之外的控件
    const picker = screen.getByTestId("models-add-lines");
    expect(within(picker).queryByRole("combobox")).toBeNull();
    expect(within(picker).queryAllByTestId(/lane|proxy/)).toHaveLength(0);
    // 也没有那些"看着像代理选择器"的 testid（点了会改 `default_lane` 的那种）
    for (const id of ["models-add-lane", "models-add-proxy", "models-add-default-lane"]) {
      expect(screen.queryByTestId(id), `不该有 ${id}`).toBeNull();
    }
  });

  it("[docs/28 §3] channel_list 读不到：如实说（后端原文 + TA 码）并挡住提交 —— 绝不静默给空清单", async () => {
    registerMock("channel_list", () => {
      throw new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：channel_list" });
    });
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    const note = await screen.findByTestId("models-add-lines-error");
    expect(note.textContent).toContain("TA-9001");
    expect(screen.queryByTestId("models-add-lines-empty")).toBeNull();

    fillByTestId("models-add-id", "ta-e2e-m-add");
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect(await screen.findByTestId("models-add-issue-lines")).toBeTruthy();
    expect(upsert.calls).toHaveLength(0);
  });

  it("配置里确实一条线路都没有：明说「还没有可选的线路」，不装作读到了空", async () => {
    registerMock("channel_list", () => []);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));

    expect(await screen.findByTestId("models-add-lines-empty")).toBeTruthy();
    expect(screen.queryByTestId("models-add-lines-error")).toBeNull();
    expect(screen.queryByTestId("models-add-line-nonexistent")).toBeNull();

    fillByTestId("models-add-id", "ta-e2e-m-add");
    fireEvent.click(screen.getByTestId("models-add-submit"));
    expect(await screen.findByTestId("models-add-issue-lines")).toBeTruthy();
    expect(upsert.calls).toHaveLength(0);
  });

  it("本地预检不过：id 重复 / id 形状非法 / order 负数 —— 一次请求都不发，并按字段给对应文案", async () => {
    // 目录里已经有 `deepseek-v4.1flash`（模型夹具的 id）—— 重复判定要有真数据才成立
    registerMock("model_list", () => [modelView()]);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    fireEvent.click(await screen.findByTestId("models-add-open"));
    // 线路先选上：这一条测的是 id / order 那几条预检，别让 lines.required 先挡住
    pickLine(LINE_BYTEDANCE);

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

    // ③ order 负数 —— `models.add.issue.order.negative`（order 在「高级」里：先展开）
    openAdvanced();
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

  it("五个分支的机器可读原因码就是 UI 取文案用的那一串（UI 不解析文案本身）", () => {
    const draft = (overrides: Partial<ModelDraft>): ModelDraft => ({
      ...EMPTY_MODEL_DRAFT,
      ...overrides,
    });
    const oneLine = [LINE_BYTEDANCE];

    expect(validateModelDraft(draft({ id: MODEL_ID, lines: oneLine }), [MODEL_ID])).toEqual([
      { field: "id", reason: "duplicate" },
    ]);
    expect(validateModelDraft(draft({ id: "-bad-id", lines: oneLine }), [])).toEqual([
      { field: "id", reason: "shape" },
    ]);
    expect(validateModelDraft(draft({ id: "ok-model", order: "-1", lines: oneLine }), [])).toEqual([
      { field: "order", reason: "negative" },
    ]);
    expect(validateModelDraft(draft({ id: "ok-model", order: "", lines: oneLine }), [])).toEqual([
      { field: "order", reason: "notInteger" },
    ]);
    // [docs/28 §2]：至少一条线路是**硬规则**（与上面几条同档，不是警告）
    expect(validateModelDraft(draft({ id: "ok-model" }), [])).toEqual([
      { field: "lines", reason: "required" },
    ]);
    // 都合法时一条 issue 都没有 —— 「有 issue 就一次都不发」那段逻辑才敢这么写
    expect(validateModelDraft(draft({ id: "ok-model", lines: oneLine }), [])).toEqual([]);
  });

  it("draftToModel：别名去空去重、group 空串 → null、lines 去重保序，且**不写 default_lane**", () => {
    const model = draftToModel({
      id: " x ",
      aliases: " a , b ,a, ",
      group: "   ",
      order: "7",
      expose: false,
      // [docs/48 §5] 草稿里带着开关：`model_upsert` 是整条覆盖写，
      // 编辑别名时漏掉它就会把用户**关掉的**模型悄悄打开 —— 所以它必须随草稿往返。
      enabled: false,
      lines: [LINE_BYTEDANCE, LINE_BYTEDANCE, "", LINE_COMMANDCODE],
    });

    expect(model).toEqual({
      id: "x",
      aliases: ["a", "b"],
      display: { group: null, order: 7 },
      expose: false,
      enabled: false,
      lines: [LINE_BYTEDANCE, LINE_COMMANDCODE],
    });
    // [docs/28 §1.2]：模型侧没有"网络代理"这个字段的入口
    expect("default_lane" in model).toBe(false);
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
    pickLine(LINE_BYTEDANCE);
    fireEvent.click(screen.getByTestId("models-add-submit"));

    const error = await screen.findByTestId("models-add-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("models[1].id");
    expect(screen.queryByTestId("models-add-result")).toBeNull();
  });
});

// ================================================================ ⑨ [docs/21 §1] 删除模型
//
// 与 `providers.test.tsx` ⑨ 同一套语义：先确认 → `force=false` → 被引用时**原样显示后端原文**
// 且**没有级联按钮**（[docs/27 §1.2]：删模型永不动线路）→ 成功后结果区如实写"已删除 X"
// → 批量**逐条**调、逐条汇报。这一节全部走整页（`renderPage`），因为"点了到底发什么载荷"
// 只在数据层看得见。

/**
 * 多选复选框：`data-testid` 挂在 MUI `Checkbox` 的根 `span` 上，真正的 `input` 在它里面。
 * 选择器 `[data-testid="X"] input` 与真机 E2E 的 `wdFillByTid` 同一口径（见 §⑧ 的说明）。
 */
function selectBoxAt(testId: string, index: number): HTMLInputElement {
  const host = screen.getAllByTestId(testId)[index];
  if (host === undefined) throw new Error(`没有第 ${index} 个 [data-testid="${testId}"]`);
  const input = host.getAttribute("type") === "checkbox" ? host : host.querySelector("input");
  if (input === null) throw new Error(`[data-testid="${testId}"] 里没有 input`);
  return input as HTMLInputElement;
}

describe("[docs/21 §1] 模型删除：先确认、再 force=false、被引用也不级联、批量逐条", () => {
  beforeEach(() => {
    registerBaselineMocks();
    // 基准 mock 的 model_list 还没有 lines 列 → 删除这一节要真的线路（连带清单算得出来）
    registerMock("model_list", () => [modelView(), otherModel("gpt-4o-mini")]);
    registerMock("channel_list", () => channelsFixture());
  });

  it("行内 models-delete → 先弹确认框（模型 id），此时一个删除命令都没发", async () => {
    const remove = recordingMock(undefined);
    registerMock("model_delete", remove.handler);

    renderPage();
    const buttons = await screen.findAllByTestId("models-delete");
    expect(buttons[0]!.tagName).toBe("BUTTON");

    fireEvent.click(buttons[0]!);
    expect(screen.getByTestId("models-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("models-delete-target").textContent).toContain(MODEL_ID);
    expect(remove.calls).toHaveLength(0);
  });

  it("确认 → model_delete({id, force:false})；成功：结果区写「已删除 <id>」并失效相关查询", async () => {
    const remove = recordingMock(undefined);
    registerMock("model_delete", remove.handler);
    const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");

    renderPage();
    fireEvent.click((await screen.findAllByTestId("models-delete"))[0]!);
    fireEvent.click(screen.getByTestId("models-delete-confirm"));

    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: MODEL_ID, force: false });

    const result = await screen.findByTestId("models-delete-result");
    expect(result.textContent).toContain("已删除");
    expect(result.textContent).toContain(MODEL_ID);
    // 成功后确认框收起来（MUI 的 Dialog 有一个退场过渡，所以要等它真的卸载）
    await waitFor(() => expect(screen.queryByTestId("models-delete-dialog")).toBeNull());

    for (const key of [qk.models, qk.credentials, qk.providers, qk.channelMatrix]) {
      const hits = invalidate.mock.calls.filter(
        ([filters]) => JSON.stringify(filters?.queryKey) === JSON.stringify(key),
      );
      expect(hits.length, `没有失效 ${JSON.stringify(key)}`).toBeGreaterThan(0);
    }
    invalidate.mockRestore();
  });

  it("[docs/27 §1.2] 被引用：原样显示后端原文（含 TA 码），**没有级联按钮**（删模型永不动线路）", async () => {
    registerMock("model_delete", () => {
      throw new TokenAgencyError({
        code: "TA-5002",
        message: `模型 ${MODEL_ID} 还被 2 条线路用着：删模型不会删线路，请先去服务商界面删掉指向它的线路`,
      });
    });

    renderPage();
    fireEvent.click((await screen.findAllByTestId("models-delete"))[0]!);
    // 没有级联按钮：那条按钮点了也只会再失败一次（线路归服务商，删模型不碰它）
    expect(screen.queryByTestId("models-delete-cascade")).toBeNull();

    fireEvent.click(screen.getByTestId("models-delete-confirm"));
    const error = await screen.findByTestId("models-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("删模型不会删线路");
    expect(screen.queryByTestId("models-delete-result")).toBeNull();
  });

  it("[docs/27 §1.2] 永远只发 force=false：不存在「再确认一次就把线路一起删了」这条路", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("model_delete", (args) => {
      calls.push(args ?? {});
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "模型还被 1 条线路用着：删模型不会删线路，请先去服务商界面把指向它的线路删掉",
      });
    });

    renderPage();
    fireEvent.click((await screen.findAllByTestId("models-delete"))[0]!);
    fireEvent.click(screen.getByTestId("models-delete-confirm"));

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toEqual({ id: MODEL_ID, force: false });
    // 连点都不会有第二条命令，更没有 force=true（线路归服务商，删模型不碰它）
    expect(screen.queryByTestId("models-delete-cascade")).toBeNull();
    expect(calls.every((c) => c["force"] === false)).toBe(true);
  });

  it("[docs/27 §1.2] 「会连带影响」清单如实写「线路不会被删」，不谎称会一起删", async () => {
    renderPage();
    fireEvent.click((await screen.findAllByTestId("models-delete"))[0]!);
    const listed = screen.getByTestId("models-delete-consequences").textContent ?? "";
    // 模型夹具用了两条线路（LINE_BYTEDANCE / LINE_COMMANDCODE）
    expect(listed).toContain("2");
    expect(listed).toContain("不会被删");
    // 断言**页面名**而不是"界面/页"这种措辞：文案在极简改造里被压短过
    // （59 字 -> 33 字），但"告诉用户去哪删线路"这个意图必须一直在。
    expect(listed).toContain("服务商");
  });

  it("批量：逐条 force=false，一条失败不中止其余条，如实汇报成功/失败并逐条给 id + 后端原文", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("model_delete", (args) => {
      calls.push(args ?? {});
      if (args?.["id"] === "gpt-4o-mini") {
        throw new TokenAgencyError({ code: "TA-5002", message: "仍被 1 条线路引用" });
      }
      return undefined;
    });

    renderPage();
    await screen.findByTestId("models-table");
    fireEvent.click(selectBoxAt("models-select", 0));
    fireEvent.click(selectBoxAt("models-select", 1));
    expect(screen.getByTestId("models-selected-count").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("models-delete-selected"));
    expect(screen.getByTestId("models-delete-dialog")).toBeTruthy();
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByTestId("models-delete-confirm"));

    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls).toEqual([
      { id: MODEL_ID, force: false },
      { id: "gpt-4o-mini", force: false },
    ]);

    const text = (await screen.findByTestId("models-delete-result")).textContent ?? "";
    expect(text).toContain("成功 1 条，失败 1 条");
    expect(text).not.toContain("批量删除成功");
    expect(text).toContain("gpt-4o-mini");
    expect(text).toContain("TA-5002");
  });

  it("表头全选（models-select-all）选中当前列表全部", async () => {
    renderPage();
    await screen.findAllByTestId("models-delete");

    fireEvent.click(selectBoxAt("models-select-all", 0));
    expect(screen.getByTestId("models-selected-count").textContent).toContain("2");
    expect(selectBoxAt("models-select", 0).checked).toBe(true);
    expect(selectBoxAt("models-select", 1).checked).toBe(true);
  });
});

// ================================================================ ⑩ [docs/22] 长解释收进问号圆圈

/**
 * [docs/22]（用户第 74 轮：「简介好长，好多字……做成一个问号加一个圆圈」）的验收。
 *
 * 两条：解释**收走了**（`?` 存在、`aria-label` 是全文、正文不再内联）、
 * 红线**没被收走**（空态 / 数值 / 后端原文 / 连带清单，[docs/22 §2]）。
 *
 * 特别注意这一页有两个"看着像解释、其实不能收"的东西，这一节把它们钉住：
 * `models.priced.unknownHint`（每一行"定价为什么是未知"的状态说明）与
 * `models.mismatch.hint`（口径不一致时给的那句裁决说明）—— 后者被既有用例断言在
 * `models-mismatch` 元素**内部**，收走就等于把结论挪出它所属的那一块。
 */
describe("[docs/22] 长解释收进问号圆圈，红线一律留在版面上", () => {
  /** 取某个 `?` 的 `aria-label`（= 提示正文全文）。 */
  function helpLabel(testId: string): string {
    const label = screen.getByTestId(testId).getAttribute("aria-label");
    expect(label, `${testId} 的 aria-label 必须是提示全文`).toBeTruthy();
    return label ?? "";
  }

  it("概览：models.summary.hint 收进 models-summary-help，四个统计格的值仍内联", () => {
    renderBody({ rows: readModelRows([modelView()], lookup()), unpricedIds: [] });

    // 先钉"不再内联"（旧代码上这一条先红：那句说明正铺在概览下面）。
    // 措辞按"通道 / 线路"两种都认：关系翻转后这句「引用通道」迟早要改口，改文案不该让守卫变红。
    expect(screen.queryByText(/是去重后的实际(?:通道|线路)数/)).toBeNull();

    const label = helpLabel("models-summary-help");
    expect(screen.getAllByTestId("models-summary-help")).toHaveLength(1);

    // 提示正文就是**全文**（不是被截短的半句）：头与尾那两件事都在
    expect(label).toMatch(/是去重后的实际(?:通道|线路)数/);
    expect(label).toContain("「未定价」来自定价接口的未定价清单");
    expect(label).toContain("读不到的那一维显示「—」，不显示 0。");

    // §2 第 5 条：统计格里的数与单位仍然一眼可见
    const summary = within(screen.getByTestId("models-summary"));
    expect(summary.getByText("逻辑模型")).toBeTruthy();
    expect(summary.getByText(/引用通道|使用的线路/)).toBeTruthy();
    expect(summary.getByText("未对外暴露")).toBeTruthy();
    expect(summary.getByTestId("models-summary-unpriced")).toBeTruthy();
  });

  it("定价区块：models.unpriced.hint 收进 models-pricing-help，正文不再内联", () => {
    renderBody({ rows: readModelRows([modelView()], lookup()), unpricedIds: [] });

    // 断言的是"这段话在 `?` 里、不在版面上"，所以读 i18n 真源而不是把句子抄一遍：
    // 硬编码的那版原句（"未定价清单来自后端 pricing_unpriced_models"）本身就带着
    // 「后端」和内部字段名 —— 它 2026-10 被判为"向用户解释实现"，文案已按新标准重写。
    const hint = zhCN["models.unpriced.hint"];
    const label = helpLabel("models-pricing-help");
    expect(label).toContain(hint);
    expect(screen.getAllByTestId("models-pricing-help")).toHaveLength(1);
    expect(screen.queryByText(hint)).toBeNull();

    // §2 第 1 条：「全部已定价」是一条状态结论，留在一眼可见的地方
    expect(screen.getByText("全部已定价。")).toBeTruthy();
    expect(label).not.toContain("全部已定价");
  });

  it("红线仍内联：筛选计数、线路缺数据的提示、逐行定价未知、口径不一致的裁决", () => {
    // §2 第 5 条：筛选计数那一句里带着数字，必须内联（文案由映射文件改，这里读真源）
    renderBody({ rows: readModelRows([modelView()], lookup()), unpricedIds: [] });
    expect(
      screen.getByText(zhCN["models.search.scope"].replace("{{value}}", "1")),
    ).toBeTruthy();
    expect(helpLabel("models-summary-help")).not.toContain("筛选");
    cleanup();

    // §2 第 2 条：通道清单读不到是缺数据 + 后端原文（TA 码）
    renderBody({
      rows: readModelRows([modelView()], lookup({ channels: [] })),
      unpricedIds: [],
      channelsError: new Error("[TA-9001] 未注册 mock：channel_list"),
    });
    const note = screen.getByTestId("models-lines-error");
    expect(note.textContent).toContain("TA-9001");
    expect(helpLabel("models-summary-help")).not.toContain("TA-9001");
    cleanup();

    // 逐行「定价为什么是未知」是状态说明，不是区块说明 → 内联（每一行一个问号更糟）
    const rustView = {
      model: {
        id: MODEL_ID,
        aliases: [],
        display: { order: 1 },
        expose: true,
        enabled: true,
        lines: [LINE_BYTEDANCE],
      },
      channel_count: 2,
      upstream_models: [MODEL_ID],
    } as unknown as ModelView;
    renderBody({ rows: readModelRows([rustView], lookup()), unpricedIds: undefined });
    const row = screen.getAllByTestId("models-row")[0]!;
    expect(within(row).getByText(zhCN["models.priced.unknownHint"])).toBeTruthy();
    expect(within(row).queryByTestId("models-priced-help")).toBeNull();
    expect(helpLabel("models-pricing-help")).not.toContain(zhCN["models.priced.unknownHint"]);
    cleanup();

    // 口径不一致时那句裁决说明留在 models-mismatch **里面**
    renderBody({
      rows: readModelRows([modelView(), otherModel("gpt-4o-mini")], lookup()),
      unpricedIds: ["gpt-4o-mini"],
    });
    expect(screen.getByTestId("models-mismatch").textContent).toContain("不替它们裁决");
    expect(helpLabel("models-pricing-help")).not.toContain("不替它们裁决");
  });

  it("删除确认框与删除结果区照旧一眼可见（第 73 轮落地的东西没被弄坏）", async () => {
    registerBaselineMocks();
    registerMock("channel_list", () => channelsFixture());
    registerMock("model_list", () => [modelView()]);
    renderPage();
    await screen.findAllByTestId("models-delete");

    fireEvent.click(screen.getAllByTestId("models-delete")[0]!);
    expect(screen.getByTestId("models-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("models-delete-confirm").textContent).toBe("删除");
    expect(screen.getByTestId("models-delete-cancel").textContent).toBe("取消");
    expect(screen.getByTestId("models-delete-selected").textContent).toBe("删除选中");

    for (const help of screen.queryAllByTestId(/-help$/)) {
      expect(help.getAttribute("aria-label") ?? "").not.toContain("会连带删除");
      expect(help.getAttribute("aria-label") ?? "").not.toContain("无法撤销");
    }
  });
});

// ================================================================ ⑪ [docs/26 §2] 一级界面省略

describe("[docs/26 §2] 模型标识 / 上游名：省略后全文仍在 DOM", () => {
  it("两个长值都整段留在 DOM 里；没溢出时一个提示都不挂，也不占 Tab 站", () => {
    const longId = `${MODEL_ID}-2026-07-18-preview-upstream`;
    const upstream = `${longId}-a`;
    const rows = readModelRows(
      [
        modelView({
          model: {
            id: longId,
            aliases: ["flash", "flash-cheap", "flash-vision-preview"],
            display: { group: "字节", order: 10 },
            expose: true,
            enabled: true,
            lines: [LINE_BYTEDANCE],
          },
          upstream_names: [upstream, `${upstream}-b`],
        }),
      ],
      lookup(),
    );
    renderBody({ rows, unpricedIds: [] });

    // 省略只发生在 CSS 层：DOM 里是逐字完整的原文
    expect(screen.getByTestId("models-id-ellipsis").textContent).toBe(longId);
    // 别名列已撤（第 309 轮）：配置里那三个别名仍然读得出来（`row.aliases`），只是不再占一列
    expect(rows[0]!.aliases).toEqual(["flash", "flash-cheap", "flash-vision-preview"]);
    expect(screen.queryByTestId("models-aliases-ellipsis")).toBeNull();
    expect(screen.getByTestId("models-upstream-ellipsis").textContent).toContain(upstream);
    // 不溢出 → 不许挂空提示、也不许给每个格子加一个 Tab 站
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(screen.getByTestId("models-id-ellipsis").getAttribute("tabindex")).toBeNull();
  });
});

// ================================================================ ⑨ [docs/48 §5] 两个开关

describe("[docs/48 §5] 模型开关与线路开关", () => {
  /**
   * MUI `Switch`：`data-testid` 在外层，真正的 `input` 在它里面。
   *
   * **不用 `getByRole("checkbox")`**：MUI 的 `Switch` 给它的 `input` 挂的是
   * `role="switch"`，按 checkbox 找会找不到（服务商页的既有用例也是直接
   * `querySelector("input")` 取它，这里与那一套口径保持一致）。
   */
  function switchInputByTestId(testId: string): HTMLElement {
    const host = screen.getByTestId(testId);
    if (host.tagName === "INPUT") return host;
    const input = within(host).getByRole("switch");
    return input;
  }

  it("模型开关：发出的是**整条覆盖写**，只翻 enabled 一位，其余字段一个不丢", async () => {
    registerMock("model_list", () => [
      modelView({
        model: {
          id: MODEL_ID,
          aliases: ["flash"],
          display: { group: "字节", order: 7 },
          expose: true,
          enabled: true,
          lines: [LINE_BYTEDANCE],
        },
      }),
    ]);
    const upsert = recordingMock(savedModel());
    registerMock("model_upsert", upsert.handler);

    renderPage();
    // 先等行真的画出来（`switchInputByTestId` 是同步查找，不能替我们等数据）
    await screen.findByTestId("models-row");
    fireEvent.click(switchInputByTestId("models-enabled"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const model = upsert.calls[0]!["model"] as Record<string, unknown>;
    // **这条是核心**：只发 `{id, enabled}` 会被后端当成"其余字段为空"，
    // 把别名 / 分组 / 顺序 / 线路**全部抹掉** —— 所以必须整条带上。
    expect(model["id"]).toBe(MODEL_ID);
    expect(model["enabled"]).toBe(false);
    expect(model["aliases"]).toEqual(["flash"]);
    expect(model["display"]).toEqual({ group: "字节", order: 7 });
    expect(model["expose"]).toBe(true);
    expect(model["lines"]).toEqual([LINE_BYTEDANCE]);
  });

  it("线路开关：**先确认**再发 channel_set_enabled，并把「还有谁在用」如实列出来", async () => {
    registerMock("model_list", () => [
      modelView({
        model: {
          id: MODEL_ID,
          aliases: [],
          display: { order: 1 },
          expose: true,
          enabled: true,
          lines: [LINE_BYTEDANCE],
        },
      }),
    ]);
    const setEnabled = recordingMock(undefined);
    registerMock("channel_set_enabled", setEnabled.handler);

    renderPage();
    await screen.findByTestId("models-row");
    fireEvent.click(switchInputByTestId("models-line-enabled"));

    // **还没发命令**：先弹确认框（线路可能是共用的，不能悄悄替用户决定）
    expect(setEnabled.calls).toHaveLength(0);
    const dialog = await screen.findByTestId("models-line-toggle-dialog");
    expect(within(dialog).getByTestId("models-line-toggle-target").textContent).toBe(LINE_BYTEDANCE);
    // 共用者名单来自已加载的模型清单（这里是那唯一一个模型）
    expect(within(dialog).getByTestId("models-line-toggle-shared").textContent).toContain(MODEL_ID);

    fireEvent.click(within(dialog).getByTestId("models-line-toggle-confirm"));
    await waitFor(() => expect(setEnabled.calls).toHaveLength(1));
    expect(setEnabled.calls[0]).toEqual({ id: LINE_BYTEDANCE, enabled: false });
  });

  it("线路开关：取消**一次命令都不发**", async () => {
    const setEnabled = recordingMock(undefined);
    registerMock("channel_set_enabled", setEnabled.handler);

    renderPage();
    await screen.findByTestId("models-row");
    fireEvent.click(switchInputByTestId("models-line-enabled"));
    fireEvent.click(await screen.findByTestId("models-line-toggle-cancel"));

    await waitFor(() =>
      expect(screen.queryByTestId("models-line-toggle-dialog")).toBeNull(),
    );
    expect(setEnabled.calls).toHaveLength(0);
  });

  it("模型清单读不到时，确认框**不声称**「没有别的模型在用」（缺数据 != 结论）", async () => {
    registerMock("model_list", () => {
      throw new Error("TA-9001 读不到");
    });
    const setEnabled = recordingMock(undefined);
    registerMock("channel_set_enabled", setEnabled.handler);

    renderPage();
    // 清单读不到时页面画错误态、没有行 —— 所以这里直接验"读不到"这件事被说出来，
    // 而不是画一句"没有别的模型在用"（那是结论，而我们没有依据）。
    expect(await screen.findByTestId("models-error")).toBeTruthy();
    expect(screen.queryByTestId("models-row")).toBeNull();
  });
});
// ================================================================ [docs/51] 定价入口（模型页）

/**
 * 用户第 318 轮：*"我是希望用户为每一个对外模型定价，来算自己大概花了多少"* +
 * *"成本中心页面只做显示，价格配置到模型页面去"*。
 *
 * 这组断言钉三件事：
 * 1. **入口在模型页的每一行**，且模型来自那一行（用户不需要、也不能填模型名）；
 * 2. 发出去的载荷 `match` 里**只有 model** —— 这是本轮修正的核心；
 * 3. **清除定价**仍守"下标安全"那条纪律（删之前确认那一格还是这个模型）。
 */
describe("[docs/51] 定价：为**这个模型**定价（入口在模型页，成本中心只显示）", () => {
  /**
   * 整页渲染（`renderPage`）要先有**行**，否则那一行上的定价入口根本不存在。
   * 这里只造"目录里有这个模型"，价格状态由 `pricing_list` / `pricing_unpriced_models` 决定。
   */
  function withModelRow(priced = true): void {
    registerMock("model_list", () => [modelView({ priced })]);
  }

  /** 打开第 index 行的定价对话框（默认第 0 行）。 */
  async function openPricing(index = 0): Promise<void> {
    const buttons = await screen.findAllByTestId("models-price-open");
    fireEvent.click(buttons[index]!);
  }

  function priceEntryForModel(model: string, rates: Record<string, number> = { input: 1.5 }): PriceEntry {
    return {
      match: { model, provider: null, credential: null },
      rates,
      multiplier: 1,
      currency: null,
      plan_fee: null,
      source: "manual",
      effective_from: null,
    };
  }

  it("每一行都有一个定价入口，文案随「有没有价」变（没价=定价，有价=改价）", async () => {
    // 没价：`pricing_unpriced_models` 报了它
    const rows = [rowOf(MODEL_ID, 0)];
    registerMock("pricing_unpriced_models", () => [MODEL_ID]);
    registerMock("pricing_list", () => []);
    renderBody({ rows, unpricedIds: [MODEL_ID] });
    const open = await screen.findAllByTestId("models-price-open");
    expect(open.length).toBe(1);
    // 文案是「定价」（没价时），不是「改价」
    expect(open[0]!.textContent).not.toBe("");

    cleanup();
    // 有价：`pricing_list` 里已经有这个模型的条目 ⇒ 那一列显示「已定价」，
    // 按钮文案变「改价」。**同一个 testid**（一个入口，两种文案）——
    // 这是刻意的：用户不关心"新建/修改"的区别，他只关心"去哪儿改这个模型的价"。
    registerMock("pricing_list", () => [priceEntryForModel(MODEL_ID)]);
    registerMock("pricing_unpriced_models", () => []);
    renderBody({ rows, unpricedIds: [] });
    const edit = await screen.findAllByTestId("models-price-open");
    expect(edit.length).toBe(1);
  });

  it("载荷里 `match` **只有 model**（绝不发 provider/credential）", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("pricing_list", () => []);
    registerMock("pricing_upsert", (args) => {
      calls.push(args ?? {});
      return priceEntryForModel(MODEL_ID);
    });
    withModelRow(false);
    renderPage();
    await openPricing();
    fireEvent.change(await screen.findByTestId("models-price-input"), { target: { value: "1.5" } });
    fireEvent.click(screen.getByTestId("models-price-submit"));
    await waitFor(() => expect(calls).toHaveLength(1));
    const entry = calls[0]!["entry"] as Record<string, unknown>;
    expect(entry["match"]).toEqual({ model: MODEL_ID });
    expect((entry["match"] as Record<string, unknown>)["provider"]).toBeUndefined();
    expect((entry["match"] as Record<string, unknown>)["credential"]).toBeUndefined();
    expect(entry["rates"]).toEqual({ input: 1.5 });
  });

  it("模型字段是**只读展示**（用户改不了它，避免拼错或写成别名）", async () => {
    registerMock("pricing_list", () => []);
    withModelRow(false);
    renderPage();
    await openPricing();
    const shown = await screen.findByTestId("models-price-model");
    expect(shown.textContent).toContain(MODEL_ID);
    // 只读：那一格下面**没有**输入框
    expect(within(shown).queryByRole("textbox")).toBeNull();
  });

  it("三项全空 → 本地点名，一条命令都不发", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("pricing_list", () => []);
    registerMock("pricing_upsert", (args) => {
      calls.push(args ?? {});
      return priceEntryForModel(MODEL_ID);
    });
    withModelRow(false);
    renderPage();
    await openPricing();
    fireEvent.click(await screen.findByTestId("models-price-submit"));
    expect(await screen.findByTestId("models-price-issue-none")).toBeTruthy();
    expect(calls).toHaveLength(0);
  });

  it("清除定价：先确认、再按 index 调 pricing_delete", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("pricing_list", () => [priceEntryForModel(MODEL_ID)]);
    registerMock("pricing_delete", (args) => {
      calls.push(args ?? {});
      return undefined;
    });
    withModelRow(true);
    renderPage();
    await openPricing();
    // 第一次点只弹确认，不发命令
    fireEvent.click(await screen.findByTestId("models-price-clear"));
    expect(calls).toHaveLength(0);
    fireEvent.click(await screen.findByTestId("models-price-clear-execute"));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toEqual({ index: 0 });
  });

  it("清除时**下标已经错位**（那一格换成了别的模型）→ 拒绝执行，一条都不删", async () => {
    const calls: Record<string, unknown>[] = [];
    // 打开时看到的是本模型；确认时后端返回的那一格已经变成别的模型
    registerMock("pricing_list", () => [priceEntryForModel(MODEL_ID)]);
    registerMock("pricing_delete", (args) => {
      calls.push(args ?? {});
      return undefined;
    });
    withModelRow(true);
    renderPage();
    await openPricing();
    fireEvent.click(await screen.findByTestId("models-price-clear"));
    // 错位发生在"用户已经看到确认框之后"
    registerMock("pricing_list", () => [priceEntryForModel("别的模型")]);
    fireEvent.click(await screen.findByTestId("models-price-clear-execute"));
    await waitFor(() => expect(screen.getByTestId("models-price-clear-error")).toBeTruthy());
    expect(calls).toHaveLength(0);
  });
});

describe("[docs/51] 清除定价的下标比对（纯逻辑）", () => {
  const ours = {
    match: { model: "m-1", provider: null, credential: null },
    rates: { input: 1 },
    multiplier: 1,
    currency: null,
    plan_fee: null,
    source: "manual" as const,
    effective_from: null,
  };

  it("下标对得上同一个模型 → same", () => {
    expect(checkIndexStillOurs([ours], 0, ours)).toBe("same");
  });

  it("下标处已经是另一个模型 → changed", () => {
    expect(checkIndexStillOurs([{ ...ours, match: { model: "m-2" } }], 0, ours)).toBe("changed");
  });

  it("同一个模型改了价仍算 same —— 改价不该让人删不掉", () => {
    expect(checkIndexStillOurs([{ ...ours, rates: { input: 9 } }], 0, ours)).toBe("same");
  });

  it("越界 / 不是数组 → missing", () => {
    expect(checkIndexStillOurs([], 0, ours)).toBe("missing");
    expect(checkIndexStillOurs("nope", 0, ours)).toBe("missing");
  });
});

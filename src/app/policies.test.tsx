// @vitest-environment jsdom
/**
 * 路由策略页 `/policies`（[07 §5]）的验收。这一页的重点是**只读且诚实**：
 *
 * 1. **只调用已注册命令**：整页只发 `policy_list`。`policy_reorder` / `policy_set_enabled` /
 *    `policy_simulate` 后端尚未注册（`src-tauri/src/commands/mod.rs` 的 `collect_commands!`），
 *    所以这里**一个按钮都没有**、也没有禁用占位 —— 这条用"渲染出来的按钮数为 0"钉住。
 * 2. **顺序来自后端**：`policy_list` 自己按 `priority` 升序排（`commands/catalog.rs:128-135`），
 *    页面照它的顺序画，另外把**原始 priority 数值**单列出来，不让"第几行"冒充优先级。
 * 3. **不解析我们不认识的 id**：通道组 / 线路 / 通道只有 id（本页没有 `group_list` / `lane_list`
 *    的授权），就原样显示并说明，不猜名字。
 * 4. **空态与错误态如实**：空列表说明"没有规则会命中"，错误带 `TA-xxxx` 与重试入口。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import PoliciesPage, { PoliciesBody, readPolicyList, type PoliciesBodyProps } from "@/pages/policies";
import { registerBaselineMocks, registerMock, TokenAgencyError } from "@/shared/api";
import type { Policy } from "@/shared/bindings";
import { i18n, type I18nKey } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
});

// ================================================================ 夹具

/** 与 `ta_core::domain::Policy` / `bindings.ts` 的 `Policy` 一致（两处字段名本来就相同）。 */
function policy(overrides: Partial<Policy> = {}): Policy {
  return {
    id: "pol-default",
    name: "默认（先用字节直连，用完切日本线）",
    enabled: true,
    priority: 1000,
    match: {
      all: true,
      model_matches: [],
      has_image: null,
      header_matches: {},
      ua_matches: null,
      channel_tags_any: [],
      channel_tags_all: [],
      input_tokens_lt: null,
      input_tokens_gt: null,
      stream: null,
      quota_remaining_lt: null,
      time_window: null,
      provider_in: [],
    },
    action: {
      use_group: "grp-deepseek",
      use_channels: [],
      exclude_channels: [],
      require_tag: null,
      exclude_tag: null,
      selector: null,
      failover: {
        on: ["http429", "http5xx"],
        max_attempts: 3,
        cooldown_s: 120,
        backoff: "exponential",
      },
      on_no_candidate: "error",
      error_code: null,
      pin_sticky: null,
      use_lane: null,
    },
    ...overrides,
  };
}

function makeProps(overrides: Partial<PoliciesBodyProps> = {}): PoliciesBodyProps {
  return {
    policies: [],
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    ...overrides,
  };
}

function renderBody(overrides: Partial<PoliciesBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <PoliciesBody {...makeProps(overrides)} />
    </ThemeProvider>,
  );
}

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <PoliciesPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

// ================================================================ ① 读后端数据

describe("读 policy_list 的结果（不猜、不补默认值）", () => {
  it("照抄后端的字段：id / name / enabled / priority / match / action", () => {
    const views = readPolicyList([policy()]);
    expect(views).toHaveLength(1);
    const v = views[0]!;
    expect(v.id).toBe("pol-default");
    expect(v.priority).toBe(1000);
    expect(v.enabled).toBe(true);
    expect(v.action.group).toBe("grp-deepseek");
    expect(v.action.failover?.maxAttempts).toBe(3);
    expect(v.action.failover?.on).toEqual(["http429", "http5xx"]);
    // 未配置的就是 null —— 不填 0、不填默认选择器
    expect(v.action.selector).toBeNull();
    expect(v.action.lane).toBeNull();
  });

  it("非数组 / 非对象直接忽略（不抛错、不编行）", () => {
    expect(readPolicyList(undefined)).toEqual([]);
    expect(readPolicyList("nope")).toEqual([]);
    expect(readPolicyList([null, 42, policy()])).toHaveLength(1);
  });

  it("契约外的选择器取值原样保留（是后端说的话，不改写、不猜）", () => {
    const views = readPolicyList([
      policy({
        action: {
          ...policy().action,
          selector: "some_future_selector" as unknown as Policy["action"]["selector"],
        },
      }),
    ]);
    expect(views[0]!.action.selector).toBe("some_future_selector");
  });
});

// ================================================================ ② 只读：不许有按钮

describe("[只读] 未实现的命令不许出现在界面上（不按钮、不禁用占位）", () => {
  it("有数据时：正文里除了那个问号，一个操作按钮都没有", () => {
    renderBody({ policies: readPolicyList([policy()]) });
    // [docs/22] 之后正文里多了一个**没有文字**的按钮：收走解释的那个问号圆圈（`HelpTip` 是
    // `IconButton`）。这条断言钉的仍然是"正文里没有任何操作按钮"，并且**额外**钉住
    // "没有文字的按钮只能是它"（比原来只多不少）。
    const buttons = screen.queryAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual([""]);
    expect(buttons.map((b) => b.getAttribute("data-testid"))).toEqual([
      "policies-list-section-help",
    ]);
  });

  it("页头操作区只有「添加路由」与「刷新」：没有排序 / 启停 / 模拟入口", () => {
    renderPage();
    const buttons = screen.queryAllByRole("button");
    // 页头说明已经收进 `?` 悬停提示（[docs/22 §1]：`PageHeader` 的 `description` 不再内联）——
    // 那个按钮**没有文字**。这条断言钉的仍是"操作区里没有排序 / 启停 / 模拟这些文字入口"，
    // 并且**额外**钉住"没有文字的按钮只能是页头与策略列表那两处 `?`"（比原来只多不少，另一侧由
    // `page-header.test.tsx` 自己钉全文与 aria-label）。
    expect(
      buttons.filter((b) => (b.textContent ?? "") !== "").map((b) => b.textContent),
    ).toEqual([zhCN["policies.add.open"], zhCN["policies.refresh"]]);
    expect(
      buttons
        .filter((b) => (b.textContent ?? "") === "")
        .map((b) => b.getAttribute("data-testid")),
    ).toEqual(["page-header-help", "policies-list-section-help"]);
    // 未实现的命令名不许出现在界面上（既不是按钮，也不是文案里的操作）
    for (const name of ["policy_reorder", "policy_set_enabled", "policy_simulate"]) {
      expect(screen.queryAllByRole("button", { name: new RegExp(name) })).toHaveLength(0);
    }
  });

  it("只读说明写清楚「为什么没有这些操作」并点出未实现的命令名", () => {
    renderBody();
    const note = screen.getByTestId("policies-readonly").textContent ?? "";
    expect(note).toContain("policy_reorder");
    expect(note).toContain("policy_set_enabled");
    expect(note).toContain("policy_simulate");
  });
});

// ================================================================ ③ 列表与顺序

describe("列表按后端返回的顺序画，并单列原始优先级", () => {
  it("顺序 = 数组下标 + 1；优先级 = 后端给的数字", () => {
    renderBody({
      policies: readPolicyList([
        policy({ id: "pol-a", name: "紧急直连", priority: 10 }),
        policy({ id: "pol-b", name: "默认", priority: 1000 }),
      ]),
    });

    const rows = screen.getAllByTestId("policies-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("紧急直连");
    expect(rows[1]!.textContent).toContain("默认");
    // 顺序列与优先级列是两件事：第一个是 1、第二个是 2；优先级是 10、1000（千分位是 [15 §4.3] 的规则）
    expect(screen.getByTestId("policies-order-0").textContent).toBe("1");
    expect(screen.getByTestId("policies-order-1").textContent).toBe("2");
    expect(screen.getByTestId("policies-priority-0").textContent).toBe("10");
    expect(screen.getByTestId("policies-priority-1").textContent).toBe("1,000");
  });

  it("启用状态用状态点 + 文字，不用彩色 chip 也不用 emoji", () => {
    renderBody({
      policies: readPolicyList([
        policy({ id: "pol-on", name: "开着的", enabled: true }),
        policy({ id: "pol-off", name: "关掉的", enabled: false }),
      ]),
    });
    // 状态列的文字与列头同名（都是「启用」），所以用 getAllByText
    expect(screen.getAllByText(zhCN["common.enabled"]).length).toBeGreaterThan(0);
    expect(screen.getAllByText(zhCN["common.disabled"]).length).toBeGreaterThan(0);
  });

  it("命中条件逐条列出来（模型 glob / 流式 / token 阈值 / 时间窗 / 服务商）", () => {
    renderBody({
      policies: readPolicyList([
        policy({
          match: {
            ...policy().match,
            all: false,
            model_matches: ["gpt-4o*", "claude-*"],
            stream: true,
            input_tokens_lt: 4000,
            time_window: "09:00-19:00",
            provider_in: ["p-mytoken"],
          },
        }),
      ]),
    });
    const cell = screen.getByTestId("policies-match-0").textContent ?? "";
    expect(cell).toContain("gpt-4o*");
    expect(cell).toContain("claude-*");
    expect(cell).toContain("4,000");
    expect(cell).toContain("09:00-19:00");
    expect(cell).toContain("p-mytoken");
  });

  it("`all: true` 说明是恒真条件；`all: false` 且无条件说明不约束", () => {
    const allView = renderBody({
      policies: readPolicyList([policy({ id: "pol-all", match: policy().match })]),
    });
    expect(screen.getByTestId("policies-match-0").textContent).toContain(zhCN["policies.match.always"]);
    allView.unmount();

    renderBody({
      policies: readPolicyList([
        policy({ id: "pol-free", match: { ...policy().match, all: false } }),
      ]),
    });
    const cell = screen.getByTestId("policies-match-0").textContent ?? "";
    expect(cell).toContain(zhCN["policies.match.unconstrained"]);
    expect(cell).not.toContain(zhCN["policies.match.always"]);
  });

  it("未配置选择器时说「未覆盖（继承）」，不猜成某个具体选择器", () => {
    renderBody({ policies: readPolicyList([policy()]) });
    expect(screen.getByTestId("policies-selector-0").textContent).toContain(
      zhCN["policies.action.selectorInherit"],
    );
  });

  it("配置了选择器时按已知取值翻译；未知取值原样显示", () => {
    const known = renderBody({
      policies: readPolicyList([
        policy({ id: "pol-sel", action: { ...policy().action, selector: "cheapest" } }),
      ]),
    });
    expect(screen.getByTestId("policies-selector-0").textContent).toBe(
      zhCN["policies.selector.cheapest"],
    );
    known.unmount();

    renderBody({
      policies: readPolicyList([
        policy({
          id: "pol-sel2",
          action: {
            ...policy().action,
            selector: "brand_new" as unknown as Policy["action"]["selector"],
          },
        }),
      ]),
    });
    expect(screen.getByTestId("policies-selector-0").textContent).toBe("brand_new");
  });

  it("线路只有原始 id 就原样显示 id（不猜名称），并说明不解析的原因", () => {
    renderBody({
      policies: readPolicyList([policy({ action: { ...policy().action, use_lane: "lane-jp" } })]),
    });
    expect(screen.getByTestId("policies-lane-0").textContent).toBe("lane-jp");
    expect(screen.getByTestId("policies-raw-ids").textContent).toContain("group_list");
    expect(screen.getByTestId("policies-raw-ids").textContent).toContain("lane_list");
  });

  it("没有线路时说明「未指定」，不显示 —（那是「读不到」的意思，会误导）", () => {
    renderBody({ policies: readPolicyList([policy()]) });
    expect(screen.getByTestId("policies-lane-0").textContent).toBe(zhCN["policies.action.laneNone"]);
  });

  it("动作摘要包含故障转移参数与无候选行为", () => {
    renderBody({ policies: readPolicyList([policy()]) });
    const cell = screen.getByTestId("policies-action-0").textContent ?? "";
    expect(cell).toContain("3");
    expect(cell).toContain("120");
    expect(cell).toContain(zhCN["policies.onNoCandidate.error"]);
  });
});

// ================================================================ ④ 空态 / 错误态

describe("空态与错误态都说实话", () => {
  it("空列表：给出诚实的空状态，不画 0 行的空表格", () => {
    renderBody({ policies: [] });
    expect(screen.queryByTestId("policies-table")).toBeNull();
    const empty = screen.getByTestId("policies-empty");
    expect(empty.textContent).toContain(zhCN["policies.empty.title"]);
    expect(empty.textContent).toContain(zhCN["policies.empty.body"]);
  });

  it("加载中且还没有数据：显示加载中，不显示空状态", () => {
    renderBody({ policies: undefined, isLoading: true });
    expect(screen.queryByTestId("policies-empty")).toBeNull();
    expect(screen.getByText(zhCN["common.loading"])).toBeTruthy();
  });

  it("错误带 TA-xxxx 与重试入口", () => {
    renderBody({
      policies: undefined,
      error: new TokenAgencyError({ code: "TA-5001", message: "策略读取失败" }),
    });
    const err = screen.getByTestId("policies-error");
    expect(err.textContent).toContain("TA-5001");
    expect(screen.getByRole("button", { name: zhCN["common.retry"] })).toBeTruthy();
  });

  it("计数写清楚总数与启用数", () => {
    renderBody({
      policies: readPolicyList([
        policy({ id: "pol-a", enabled: true }),
        policy({ id: "pol-b", enabled: false }),
      ]),
    });
    expect(screen.getByTestId("policies-count").textContent).toContain("2");
    expect(screen.getByTestId("policies-count").textContent).toContain("1");
  });
});

// ================================================================ ⑤ 添加路由向导（[docs/17 §3.4]）

const MODEL = "deepseek-v4.1flash";
const OTHER_MODEL = "qwen3-max";

/** 契约默认能力（[docs/17 §2]：**省略整个键 = 这一组**）。 */
const DEFAULT_CAPS = {
  vision: false,
  tools: false,
  reasoning: false,
  context_tokens: 0,
  max_output_tokens: null,
  modalities: ["text"],
};

/** 一条线路（`channel_list` 的形状）。`capabilities` 是真契约字段（`Channel.capabilities?`）。 */
function channel(id: string, model: string, caps?: unknown): Record<string, unknown> {
  return {
    id,
    model,
    provider: "p-" + id,
    credential: "c-" + id,
    lane: "direct",
    upstream_model: id + "-upstream",
    priority: 100,
    weight: 10,
    tags: [],
    target_override: null,
    enabled: true,
    ...(caps === undefined ? {} : { capabilities: caps }),
  };
}

function capabilities(contextTokens: number, vision: boolean) {
  return {
    vision,
    tools: true,
    reasoning: false,
    context_tokens: contextTokens,
    max_output_tokens: 8192,
    modalities: ["image", "text"],
  };
}

type UpsertCall = { policy: Policy };

function registerWizardMocks(options: {
  channels: Record<string, unknown>[];
  upsert?: (args: UpsertCall) => Policy;
  calls?: UpsertCall[];
}): UpsertCall[] {
  const calls = options.calls ?? [];
  registerMock("model_list", () => [
    { model: { id: MODEL, aliases: [], expose: true, display: null }, channel_count: 2, upstream_names: [], priced: false },
    { model: { id: OTHER_MODEL, aliases: [], expose: true, display: null }, channel_count: 1, upstream_names: [], priced: false },
  ]);
  registerMock("channel_list", () => options.channels);
  registerMock("policy_list", () => []);
  registerMock("policy_upsert", (args) => {
    const call = args as unknown as UpsertCall;
    calls.push(call);
    if (options.upsert !== undefined) return options.upsert(call);
    return call.policy;
  });
  return calls;
}

async function openWizard() {
  renderPage();
  fireEvent.click(screen.getByTestId("policies-add-open"));
  await screen.findByTestId("policies-add-dialog");
}

/**
 * 打开 MUI 下拉并选一项 —— **与真机 E2E 的 `wdPickOption` 同一条路径**：
 * 点控件 → 等 `li[role="option"]` 弹层 → 点选项。
 * （原生 `<select>` 弹的是 OS 级下拉，`li[role=option]` 永远不会出现 —— 那条路走不通。）
 */
async function pickOption(testId: string, optionName: string | RegExp) {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name: optionName }));
}

/** 打开多选下拉，点若干项（MUI 多选点完不自动关，这里手动关掉，免得挡住后面的查询）。 */
async function pickOptions(testId: string, optionNames: (string | RegExp)[]) {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  for (const name of optionNames) {
    fireEvent.click(await screen.findByRole("option", { name }));
  }
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
}

/** 打开某个下拉，把弹层里的选项名读出来（用于断言"只列了该模型的线路"这类过滤）。 */
async function optionNamesOf(testId: string): Promise<string[]> {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  // **`findAllByRole`**：弹层里本来就有多个 `option`，用 `findByRole`（要求唯一）会直接抛错
  const names = (await screen.findAllByRole("option")).map((o) => o.textContent ?? "");
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  return names;
}

describe("[添加路由] 向导：真输入 → 真调用 → 入参形状正确", () => {
  it("点「添加路由」打开向导，控件都在（testid 按 docs/17 §3.4）", async () => {
    registerWizardMocks({ channels: [channel("ch-1", MODEL, capabilities(1_050_000, true))] });
    await openWizard();
    for (const tid of [
      "policies-add-dialog",
      "policies-add-model",
      "policies-add-channels",
      "policies-add-sticky",
      "policies-add-selector",
      "policies-add-failover",
      "policies-add-priority",
      "policies-add-submit",
    ]) {
      expect(screen.getByTestId(tid)).toBeTruthy();
    }
  });

  it("线路下拉**只列 `model` 等于所选模型的线路**（其它模型的线路一条都不出现）", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
        channel("ch-9", OTHER_MODEL, capabilities(1_050_000, true)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    const names = (await optionNamesOf("policies-add-channels")).join("|");
    expect(names).toContain("ch-1");
    expect(names).toContain("ch-2");
    expect(names).not.toContain("ch-9");
  });

  it("选定模型后**默认指向它的全部线路**，并派生出可改的 id / 名称（用户原话：暴露 X → 指向 1、2）", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    expect((screen.getByTestId("policies-add-id") as HTMLInputElement).value).toBe(
      "pol-deepseek-v4-1flash",
    );
    expect((screen.getByTestId("policies-add-name") as HTMLInputElement).value).toBe(MODEL);
    // 默认全选 → 两条线路已经在选中的集合里（多选控件显示它们的 id）
    expect(screen.getByTestId("policies-add-channels").textContent).toContain("ch-1");
    expect(screen.getByTestId("policies-add-channels").textContent).toContain("ch-2");
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(false);
  });

  it("钉住线路（pin_sticky）的下拉只列已选线路 —— 不发明契约外的开关", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
        channel("ch-9", OTHER_MODEL, capabilities(1_050_000, true)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    const names = (await optionNamesOf("policies-add-sticky")).join("|");
    expect(names).toContain("ch-1");
    expect(names).toContain("ch-2");
    expect(names).not.toContain("ch-9");
  });
});

describe("[添加路由] 能力一致性：不一致就禁用提交并写出逐字段差异", () => {
  it("能力不一致 → 提交按钮禁用 + 逐字段差异（字段名 + 各线路的值），并带因果反证", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(131_072, false)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);
    // 默认全选了两条 → 不一致在选完模型那一刻就可见（id/名称已派生 ⇒ 禁用只可能来自不一致）
    expect(screen.getByTestId("policies-add-capability-mismatch")).toBeTruthy();
    expect(screen.getByTestId("policies-add-capability-diff-vision").textContent).toContain(
      "ch-1=true / ch-2=false",
    );
    const contextDiff = screen.getByTestId("policies-add-capability-diff-context_tokens").textContent ?? "";
    expect(contextDiff).toContain("ch-1=1050000");
    expect(contextDiff).toContain("ch-2=131072");
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(true);

    // **因果反证**：取消一条（只剩一条，无需比对）→ 提交立刻可用
    await pickOptions("policies-add-channels", [/^ch-2/]);
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(false);
  });

  it("能力一致 → 说明一致、提交可用", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);
    expect(screen.queryByTestId("policies-add-capability-mismatch")).toBeNull();
    expect(screen.getByTestId("policies-add-capabilities").textContent).toContain("一致");
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(false);
  });

  it("**省略 `capabilities` = 契约默认能力**：与显式写默认值算**一致**（红先绿）", async () => {
    registerWizardMocks({
      channels: [channel("ch-1", MODEL, DEFAULT_CAPS), channel("ch-2", MODEL)],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    // 两条的语义是同一组能力（一条显式写、一条按契约省略）→ 不许报不一致
    expect(screen.queryByTestId("policies-add-capability-mismatch")).toBeNull();
    expect(screen.getByTestId("policies-add-capabilities").textContent).toContain("一致");
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(false);
  });

  it("形状不对（字段类型错）→ 如实说“读不出来”，不当作一致、也不装作默认", async () => {
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, { ...capabilities(1_050_000, true), vision: "yes" }),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    expect(screen.queryByTestId("policies-add-capability-mismatch")).toBeNull();
    const box = screen.getByTestId("policies-add-capabilities").textContent ?? "";
    expect(box).toContain("ch-2");
    expect(box).not.toContain("一致");
    // 读不出来 ≠ 编一个默认：不拦提交（由后端裁决）
    expect((screen.getByTestId("policies-add-submit") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("[添加路由] 提交：真实入参 + 回显后端返回值 + 原样显示后端错误", () => {
  it("入参是 policy_upsert({policy})：model_matches / use_channels / pin_sticky / selector / failover / priority", async () => {
    const calls = registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
      ],
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);

    await pickOption("policies-add-sticky", /ch-2/);
    await pickOption("policies-add-selector", /先粘性再优先级/);
    fireEvent.click(within(screen.getByTestId("policies-add-failover-http429")).getByRole("checkbox"));
    fireEvent.change(screen.getByTestId("policies-add-failover-max"), { target: { value: "5" } });
    fireEvent.change(screen.getByTestId("policies-add-failover-cooldown"), { target: { value: "30" } });
    await pickOption("policies-add-failover-backoff", /不退避/);
    fireEvent.change(screen.getByTestId("policies-add-priority"), { target: { value: "42" } });
    fireEvent.click(screen.getByTestId("policies-add-submit"));

    await screen.findByTestId("policies-add-result");
    expect(calls).toHaveLength(1);
    const sent = calls[0]!.policy;
    expect(sent.id).toBe("pol-deepseek-v4-1flash");
    expect(sent.name).toBe(MODEL);
    expect(sent.enabled).toBe(true);
    expect(sent.priority).toBe(42);
    expect(sent.match.model_matches).toEqual([MODEL]);
    expect(sent.match.all).toBe(false);
    expect(sent.action.use_channels).toEqual(["ch-1", "ch-2"]);
    expect(sent.action.pin_sticky).toBe("ch-2");
    expect(sent.action.selector).toBe("sticky_then_priority");
    expect(sent.action.failover).toEqual({
      on: ["http429"],
      max_attempts: 5,
      cooldown_s: 30,
      backoff: "none",
    });
    expect(sent.action.on_no_candidate).toBe("error");
  });

  it("成功后显示的是**后端返回的那一条**（不是本地输入）", async () => {
    registerWizardMocks({
      channels: [channel("ch-1", MODEL, capabilities(1_050_000, true))],
      upsert: (call) => ({ ...call.policy, id: "pol-backend", name: "后端存下来的名字" }),
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);
    fireEvent.click(screen.getByTestId("policies-add-submit"));

    const result = await screen.findByTestId("policies-add-result");
    expect(result.textContent).toContain("pol-backend");
    expect(result.textContent).toContain("后端存下来的名字");
  });

  it("后端拒绝时：原样显示 [TA-xxxx] + message + details（不翻译、不改写）", async () => {
    const details = {
      model: MODEL,
      channels: ["ch-1", "ch-2"],
      differences: [{ field: "vision", values: { "ch-1": true, "ch-2": false } }],
    };
    registerWizardMocks({
      channels: [
        channel("ch-1", MODEL, capabilities(1_050_000, true)),
        channel("ch-2", MODEL, capabilities(1_050_000, true)),
      ],
      upsert: () => {
        throw new TokenAgencyError({
          code: "TA-4001",
          message: "同一条路由下的线路能力不一致：vision（ch-1=true / ch-2=false）",
          details,
        });
      },
    });
    await openWizard();
    await pickOption("policies-add-model", MODEL);
    fireEvent.click(screen.getByTestId("policies-add-submit"));

    const text = await screen.findByTestId("policies-add-error-text");
    expect(text.textContent).toContain("TA-4001");
    expect(text.textContent).toContain("vision（ch-1=true / ch-2=false）");
    const detailsBox = screen.getByTestId("policies-add-error-details");
    expect(detailsBox.textContent).toContain('"field": "vision"');
  });
});

// ================================================================ ⑥ 删除（[docs/21 §1]）

/**
 * 删除的 mock：列表数据、`policy_delete` 的载荷记录、以及"某一条失败"的钩子。
 *
 * 载荷必须是 `{ id }`（`bindings.ts:1309`：`policy_delete: { args: { id: PolicyId } }`），
 * 而且**没有 force 参数**（[docs/21 §1.1 第 3 条]：只有 provider / credential / model 有级联语义）。
 */
function registerDeleteMocks(options: {
  policies: Policy[];
  remove?: (id: string) => void;
}): { id: string }[] {
  const calls: { id: string }[] = [];
  registerMock("policy_list", () => options.policies);
  registerMock("policy_delete", (args) => {
    const call = args as unknown as { id: string };
    calls.push(call);
    options.remove?.(call.id);
    return undefined;
  });
  return calls;
}

/** 按 zh-CN 的真源拼预期文案（断言里不写死中文，改文案时只改一处）。 */
function textOf(key: I18nKey, vars: Record<string, string | number> = {}): string {
  let out: string = zhCN[key];
  for (const [name, value] of Object.entries(vars)) {
    out = out.replace(`{{${name}}}`, String(value));
  }
  return out;
}

/** 点一次确认框里的「删除」。 */
function confirmDelete() {
  fireEvent.click(screen.getByTestId("policies-delete-confirm"));
}

describe("[删除] 单条：先二次确认，再按 id 调 policy_delete", () => {
  it("点行内「删除」只弹确认框（一条命令都不发），确认后才发 policy_delete({id})", async () => {
    const calls = registerDeleteMocks({
      policies: [policy({ id: "pol-a", name: "甲" }), policy({ id: "pol-b", name: "乙" })],
    });
    renderPage();

    const buttons = await screen.findAllByTestId("policies-delete");
    expect(buttons).toHaveLength(2);
    fireEvent.click(buttons[0]!);

    // ① 确认框先出现，且写清删的是哪一个（名称 + id）
    expect(screen.getByTestId("policies-delete-dialog")).toBeTruthy();
    const target = screen.getByTestId("policies-delete-target").textContent ?? "";
    expect(target).toContain("甲");
    expect(target).toContain("pol-a");
    // ② **二次确认之前绝不调用后端**
    expect(calls).toHaveLength(0);

    confirmDelete();
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toEqual({ id: "pol-a" });

    // ③ 成功后在结果区如实写「已删除 <名称>」
    const result = await screen.findByTestId("policies-delete-result");
    expect(result.textContent).toContain(textOf("delete.result.ok", { name: "甲" }));
    expect(result.textContent).toContain("pol-a");
  });

  it("被引用时：确认框里**原样**显示后端原文（含 TA 码），且不放级联按钮", async () => {
    registerDeleteMocks({
      policies: [policy({ id: "pol-a", name: "甲" })],
      remove: () => {
        throw new TokenAgencyError({
          code: "TA-5002",
          message: "策略 pol-a 正被引用，无法删除",
          details: { referencing: ["pol-b"] },
        });
      },
    });
    renderPage();

    fireEvent.click(await screen.findByTestId("policies-delete"));
    confirmDelete();

    const err = await screen.findByTestId("policies-delete-error");
    expect(err.textContent).toContain("TA-5002");
    expect(err.textContent).toContain("策略 pol-a 正被引用，无法删除");
    // `policy_delete` 没有 force 语义 → 没有"强行删除"按钮（那会是假按钮）
    expect(screen.queryByTestId("policies-delete-cascade")).toBeNull();
    // 确认框保持打开：用户看得见后端为什么拒绝
    expect(screen.getByTestId("policies-delete-dialog")).toBeTruthy();
    // 失败时**不许**在结果区写"已删除"
    expect(screen.queryByText(textOf("delete.result.ok", { name: "甲" }))).toBeNull();
  });
});

describe("[删除] 批量：一次确认、逐条调用、逐条如实汇报", () => {
  it("全选 → 一次确认 → 逐条 policy_delete；1 成功 1 失败都发出去，失败项给 id + 后端原文", async () => {
    const calls = registerDeleteMocks({
      policies: [policy({ id: "pol-a", name: "甲" }), policy({ id: "pol-b", name: "乙" })],
      remove: (id) => {
        if (id === "pol-b") {
          throw new TokenAgencyError({ code: "TA-5002", message: "策略 pol-b 正被引用" });
        }
      },
    });
    renderPage();

    // 没勾选时「删除选中」不可用（不是假按钮：勾上就能用）
    const toolbarButton = await screen.findByTestId("policies-delete-selected");
    expect((toolbarButton as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(
      within(screen.getByTestId("policies-select-all")).getByRole("checkbox"),
    );
    expect(screen.getByTestId("policies-selected-count").textContent).toContain(
      textOf("delete.selectedCount", { count: 2 }),
    );
    expect((screen.getByTestId("policies-delete-selected") as HTMLButtonElement).disabled).toBe(
      false,
    );

    fireEvent.click(toolbarButton);
    // 批量也只弹**一次**确认，确认之前一条命令都不发
    expect(screen.getByTestId("policies-delete-dialog")).toBeTruthy();
    expect(calls).toHaveLength(0);

    confirmDelete();

    // **一条失败不中止其它条**：两条都发出去了
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls.map((call) => call.id)).toEqual(["pol-a", "pol-b"]);

    const result = await screen.findByTestId("policies-delete-result");
    // 逐条如实汇报：成功 1 条，失败 1 条（不用一句"批量删除成功"盖住部分失败）
    expect(result.textContent).toContain(
      textOf("delete.result.summary", { ok: 1, failed: 1 }),
    );
    expect(result.textContent).toContain(textOf("delete.result.ok", { name: "甲" }));
    // 失败项：标识（id）+ 后端原文
    const failed = screen.getByTestId("policies-delete-result-pol-b").textContent ?? "";
    expect(failed).toContain("pol-b");
    expect(failed).toContain("TA-5002");
    expect(failed).toContain("策略 pol-b 正被引用");
  });
});

// ================================================================ ⑦ [docs/22] 长解释收进「问号圆圈 + 悬停提示」
//
// 用户第 74 轮原话：「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个
// 问好加一个圆圈，鼠标移上去能看到提示那种」。按 [docs/22 §2/§3/§5] 钉：
//
// 1. **解释收走了**：`?` 在、`aria-label` 是全文、那段正文**不再内联**；
// 2. **红线一条都不收**：这一页有两处"看着像解释、其实不能收"——
//    - `policies.readonly`（哪些命令还没接入 = 当前能力的**结论**，既有用例也锚着它）；
//    - 每行的命中条件判定（`policies.match.always` / `unconstrained`）与数值（顺序 / 优先级 / 次数）；
//    另外 `policies.empty.*`（空态）与后端原文（含 TA 码）本来就留在版面上。
//
// 同一区块**最多一个** `?`（§3）：顺序说明与命中条件的 AND/OR 说明合成一条提示，
// 而不是在每一行的命中条件下面各摆一个（"一排问号比一排字更糟"）。

describe("⑦ [docs/22] 长解释收进问号圆圈，红线一律留在版面上", () => {
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

  it("策略列表：顺序说明与「AND / OR」说明合成一个问号，正文不再内联", () => {
    renderBody({
      policies: readPolicyList([
        policy({ id: "pol-a", name: "甲" }),
        policy({ id: "pol-b", name: "乙" }),
      ]),
    });

    const label = helpLabel("policies-list-section-help");
    expect(label).toContain(copy("policies.orderNote"));
    expect(label).toContain(copy("policies.match.andHint"));
    expect(screen.queryByText(copy("policies.orderNote"))).toBeNull();
    expect(screen.queryByText(copy("policies.match.andHint"))).toBeNull();
    // 两行策略只有一个 `?`：命中条件那一列**没有**变成一排问号
    expect(screen.getAllByTestId(/-help$/).map((node) => node.getAttribute("data-testid"))).toEqual([
      "policies-list-section-help",
    ]);
  });

  it("红线仍内联：未接入命令的结论 / 空态 / 每行的判定与数值", () => {
    // §2 第 1 条：这一块写的是"现在能做什么、不能做什么"，是结论，不是解释
    renderBody();
    const note = screen.getByTestId("policies-readonly").textContent ?? "";
    expect(note).toContain("policy_reorder");
    expect(note).toContain("policy_set_enabled");
    expect(note).toContain("policy_simulate");
    const empty = screen.getByTestId("policies-empty").textContent ?? "";
    expect(empty).toContain(copy("policies.empty.title"));
    expect(empty).toContain(copy("policies.empty.body"));
    cleanup();

    // §2 第 1 / 5 条：每行的判定（恒真 / 未约束）与数值（顺序、优先级、故障转移参数）
    renderBody({
      policies: readPolicyList([policy({ id: "pol-a", name: "甲", priority: 1000 })]),
    });
    expect(screen.getByTestId("policies-match-0").textContent).toContain(
      copy("policies.match.always"),
    );
    expect(screen.getByTestId("policies-order-0").textContent).toBe("1");
    expect(screen.getByTestId("policies-priority-0").textContent).toBe("1,000");
    expect(screen.getByTestId("policies-action-0").textContent).toContain("3");
    cleanup();

    // §2 第 1 条：没有条件时的判定
    renderBody({
      policies: readPolicyList([policy({ id: "pol-b", match: { ...policy().match, all: false } })]),
    });
    expect(screen.getByTestId("policies-match-0").textContent).toContain(
      copy("policies.match.unconstrained"),
    );
    cleanup();

    // §2 第 2 条：后端原文（含 TA 码）
    renderBody({
      policies: undefined,
      error: new TokenAgencyError({ code: "TA-5001", message: "策略读取失败" }),
    });
    expect(screen.getByTestId("policies-error").textContent).toContain("TA-5001");
  });

  it("添加路由向导：字段级提示一律留在版面上（不是「成段解释」，一个问号都不加）", async () => {
    registerWizardMocks({ channels: [channel("ch-1", MODEL, capabilities(1_050_000, true))] });
    await openWizard();

    // `policies.add.stickyNote` 是**字段级**提示（它解释的正是下面那个控件怎么用），
    // 与 [docs/22 §2] 要收的"区块说明 / 成段 prose"不是一类 → 留在版面上。
    expect(screen.getByText(copy("policies.add.stickyNote"))).toBeTruthy();
    // §2 第 1 / 3 条：还没选模型时的引导留在版面上
    expect(screen.getByTestId("policies-add-channels").textContent).toContain(
      copy("policies.add.channelsNeedModel"),
    );
    // 这个对话框里一个 `?` 都没有
    expect(
      within(screen.getByTestId("policies-add-dialog")).queryAllByTestId(/-help$/),
    ).toHaveLength(0);
  });
});

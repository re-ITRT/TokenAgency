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
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import PoliciesPage, { PoliciesBody, readPolicyList, type PoliciesBodyProps } from "@/pages/policies";
import { registerBaselineMocks, registerMock, TokenAgencyError } from "@/shared/api";
import type { Policy } from "@/shared/bindings";
import { i18n } from "@/shared/i18n";
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
  it("有数据时：正文里一个按钮都没有", () => {
    renderBody({ policies: readPolicyList([policy()]) });
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  it("页头操作区只有「添加路由」与「刷新」：没有排序 / 启停 / 模拟入口", () => {
    renderPage();
    const buttons = screen.queryAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual([
      zhCN["policies.add.open"],
      zhCN["policies.refresh"],
    ]);
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

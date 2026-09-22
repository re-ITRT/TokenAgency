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
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import PoliciesPage, { PoliciesBody, readPolicyList, type PoliciesBodyProps } from "@/pages/policies";
import { TokenAgencyError } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
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

  it("页头操作区只有「刷新」：没有排序 / 启停 / 模拟入口", () => {
    renderPage();
    const buttons = screen.queryAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual([zhCN["policies.refresh"]]);
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

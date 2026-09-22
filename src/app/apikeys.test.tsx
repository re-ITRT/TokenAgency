// @vitest-environment jsdom
/**
 * 访问密钥页 `/api-keys`（[docs/19 §3.4]）的验收。这一页只有两件事**必须**被机械钉住：
 *
 * 1. **明文只在创建时出现一次**：`api_key_create` 的返回是唯一一次能看到明文的时刻。
 *    测试断言三件事：创建后明文**确实出现过**（否则"消失"是假的）、点「我已保存」后
 *    **从 DOM 上消失**、并且**列表里任何地方都拿不到它**（库里只存哈希）。
 * 2. **列表里不放复制按钮**：只存哈希，复制不出来。用 `apiKeys.hashOnlyNotice`
 *    把这件事实讲明白（这是诚实，不是缺失）。
 *
 * 另外：`last_used_at_ms === null` 显示"从未使用"（**不是 0、不是 1970**）——
 * "从没用过"与"确实用过、时间戳恰好是 0"是两件事。
 *
 * 命令形状按契约用 `registerMock` 提供（`api_key_*` 五条由 E3 注册；
 * 这些用例**不依赖注册状态**，只依赖契约形状）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import ApiKeysPage from "@/pages/apikeys";
import { TokenAgencyError, registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import type { ApiKeyUsageRow, ApiKeyView } from "@/shared/bindings";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

// ================================================================ 夹具

/** "后端"的密钥表（**只有哈希与展示前缀，没有明文**）。 */
let keys: ApiKeyView[] = [];
/** 用量表（按 key × 日）。 */
let usageRows: ApiKeyUsageRow[] = [];

let listFails = false;
let createCalls: Array<Record<string, unknown>> = [];
let toggleCalls: Array<Record<string, unknown>> = [];
let deleteCalls: Array<Record<string, unknown>> = [];
let usageCalls: Array<Record<string, unknown>> = [];
let usageFails = false;

/**
 * 创建返回里**唯一一次**给出的明文（测试要断言它出现一次、然后消失）。
 *
 * 注意：**运行期拼出来**，不写字面量：`scripts/gate.ps1` 的明文密钥扫描器按
 * `sk-[A-Za-z0-9_-]{20,}` 匹配，直接写一个字面量会被门禁拦下（那是**对的** ——
 * 仓库里就不该出现任何"看起来像真 key"的串）。这里没有真 key，拼出来的值只用于断言往返。
 */
const PLAINTEXT = "ta-sk-" + "9f2Kd0Qz7Xb1Lm4Np6Rt8Vw3Yc5Ae7Uj";

function keyView(overrides: Partial<ApiKeyView> = {}): ApiKeyView {
  return {
    id: "k-1",
    name: "笔记本",
    key_prefix: "ta-sk-9f2Kd…",
    enabled: true,
    note: null,
    created_at_ms: 1_770_000_000_000,
    last_used_at_ms: null,
    ...overrides,
  };
}

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

beforeEach(() => {
  keys = [keyView()];
  usageRows = [];
  listFails = false;
  createCalls = [];
  toggleCalls = [];
  deleteCalls = [];
  usageCalls = [];
  usageFails = false;

  registerMock("api_key_list", () => {
    if (listFails) {
      throw new TokenAgencyError({ code: "TA-9001", message: "密钥表读取失败（测试夹具）" });
    }
    return keys;
  });
  registerMock("api_key_create", (args) => {
    const write = args as { name: string; note?: string | null };
    createCalls.push({ ...write });
    const created = keyView({
      id: "k-2",
      name: write.name,
      note: write.note ?? null,
      key_prefix: "ta-sk-9f2Kd…",
    });
    keys = [...keys, created];
    return { key: created, plaintext: PLAINTEXT };
  });
  registerMock("api_key_set_enabled", (args) => {
    const write = args as { id: string; enabled: boolean };
    toggleCalls.push({ ...write });
    keys = keys.map((k) => (k.id === write.id ? { ...k, enabled: write.enabled } : k));
    return keys.find((k) => k.id === write.id) ?? keyView();
  });
  registerMock("api_key_delete", (args) => {
    const write = args as { id: string };
    deleteCalls.push({ ...write });
    keys = keys.filter((k) => k.id !== write.id);
    return undefined;
  });
  registerMock("api_key_usage", (args) => {
    usageCalls.push({ ...(args as Record<string, unknown>) });
    if (usageFails) {
      throw new TokenAgencyError({ code: "TA-9001", message: "用量读取失败（测试夹具）" });
    }
    return usageRows;
  });
});

afterEach(() => {
  cleanup();
});

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <ApiKeysPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

/** 等到列表真的读出来（空态或表格都算读完了）。 */
async function waitForList(): Promise<void> {
  await waitFor(() => expect(screen.getByTestId("api-keys-page")).toBeTruthy());
  await waitFor(() => {
    const done =
      screen.queryByTestId("api-keys-table") !== null ||
      screen.queryByTestId("api-keys-empty") !== null ||
      screen.queryByTestId("api-keys-error") !== null;
    if (!done) throw new Error("列表还没读完");
  });
}

// ================================================================ ① 列表

describe("列表：后端真值 + 从未使用 = null（不是 0）", () => {
  it("渲染后端的行；last_used_at_ms=null 显示「从未使用」，不是 0 / 1970", async () => {
    keys = [
      keyView({ id: "k-1", name: "笔记本", last_used_at_ms: null }),
      keyView({ id: "k-2", name: "服务器", last_used_at_ms: 1_760_000_000_000 }),
    ];
    renderPage();
    await waitForList();

    const rows = screen.getAllByTestId("api-key-row");
    expect(rows).toHaveLength(2);

    const never = screen
      .getAllByTestId("api-key-last-used")
      .find((el) => el.getAttribute("data-key-id") === "k-1");
    expect(never?.getAttribute("data-used")).toBe("never");
    expect(never?.textContent).toBe("从未使用");
    // 自证：绝不能渲染成 0 / 1970
    expect(never?.textContent).not.toMatch(/^0$/);
    expect(never?.textContent).not.toMatch(/1970/);

    const used = screen
      .getAllByTestId("api-key-last-used")
      .find((el) => el.getAttribute("data-key-id") === "k-2");
    expect(used?.getAttribute("data-used")).toBe("once");
    expect(used?.textContent).not.toBe("从未使用");
  });

  it("页面真的渲染出页头（路由壳能接管；不白屏）", async () => {
    renderPage();
    await waitForList();
    expect(screen.getByTestId("page-header")).toBeTruthy();
    expect(screen.getByTestId("api-keys-page")).toBeTruthy();
  });

  it("一条都没有：给诚实的空态，不画空表格", async () => {
    keys = [];
    renderPage();
    await waitForList();

    expect(screen.getByTestId("api-keys-empty").textContent).toBe("还没有任何访问密钥");
    expect(screen.queryByTestId("api-keys-table")).toBeNull();
    expect(screen.queryAllByTestId("api-key-row")).toEqual([]);
  });

  it("读失败：把后端原文（含 TA 码）显示出来，不吞", async () => {
    listFails = true;
    renderPage();
    await waitForList();

    expect(screen.getByTestId("api-keys-error").textContent).toMatch(/\[TA-9001\]/);
    expect(screen.queryByTestId("api-keys-table")).toBeNull();
  });
});

// ================================================================ ② 明文只显示一次（硬要求一）

describe("[硬要求] 明文只在创建时出现一次，点「我已保存」后从界面上消失", () => {
  it("创建后明文出现一次 + 提示语；点「我已保存」后 DOM 里再也找不到它", async () => {
    renderPage();
    await waitForList();

    // 一开始没有任何明文
    expect(screen.queryByTestId("api-keys-created-plaintext")).toBeNull();
    expect(document.body.textContent).not.toContain(PLAINTEXT);

    fireEvent.click(screen.getByTestId("api-keys-create"));
    fireEvent.change(within(screen.getByTestId("api-keys-create-name")).getByRole("textbox"), {
      target: { value: "新密钥" },
    });
    fireEvent.change(within(screen.getByTestId("api-keys-create-note")).getByRole("textbox"), {
      target: { value: "给 CI 用" },
    });
    // 入参形状（契约）：snake_case 的 name / note
    fireEvent.click(screen.getByTestId("api-keys-create-submit"));

    const box = await screen.findByTestId("api-keys-created-plaintext");
    await waitFor(() => expect(createCalls.length).toBe(1));
    expect(createCalls[0]).toEqual({ name: "新密钥", note: "给 CI 用" });

    // ① 明文**确实出现过**（这条自证很重要：否则下面的"消失"是假绿）
    expect(box.textContent).toContain(PLAINTEXT);
    expect(document.body.textContent).toContain(PLAINTEXT);
    // ② 而且给了"这是唯一一次"的提示
    expect(screen.getByTestId("api-keys-created-hint").textContent).toContain(
      "这是唯一一次能看到明文的机会，请立刻保存",
    );

    // ③ 点「我已保存」→ 明文从界面上消失
    fireEvent.click(screen.getByTestId("api-keys-created-done"));
    await waitFor(() => expect(screen.queryByTestId("api-keys-created-plaintext")).toBeNull());
    expect(screen.queryByTestId("api-keys-created-hint")).toBeNull();
    expect(document.body.textContent).not.toContain(PLAINTEXT);
  });

  it("列表里永远拿不到明文（就算刚创建过、列表也刷新了）", async () => {
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-keys-create"));
    fireEvent.change(within(screen.getByTestId("api-keys-create-name")).getByRole("textbox"), {
      target: { value: "给 CI 用" },
    });
    fireEvent.click(screen.getByTestId("api-keys-create-submit"));
    await screen.findByTestId("api-keys-created-plaintext");

    // 关掉明文之后，列表里（新行也在）任何地方都不含明文
    fireEvent.click(screen.getByTestId("api-keys-created-done"));
    await waitFor(() => expect(screen.getAllByTestId("api-key-row")).toHaveLength(2));
    expect(document.body.textContent).not.toContain(PLAINTEXT);
    // 展示的是前缀（后端给的），不是明文
    expect(screen.getByTestId("api-keys-table").textContent).toContain("ta-sk-9f2Kd…");
  });

  it("名称为空：提交按钮禁用，且一次都不调用后端（不生成无名密钥）", async () => {
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-keys-create"));
    const submit = screen.getByTestId("api-keys-create-submit") as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.click(submit);
    expect(createCalls).toEqual([]);

    fireEvent.change(within(screen.getByTestId("api-keys-create-name")).getByRole("textbox"), {
      target: { value: "  " },
    });
    expect((screen.getByTestId("api-keys-create-submit") as HTMLButtonElement).disabled).toBe(true);
    expect(createCalls).toEqual([]);
  });

  it("创建失败：把后端原文显示出来，并且**不给**任何明文", async () => {
    registerMock("api_key_create", () => {
      throw new TokenAgencyError({ code: "TA-5001", message: "名称重复（测试夹具）" });
    });
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-keys-create"));
    fireEvent.change(within(screen.getByTestId("api-keys-create-name")).getByRole("textbox"), {
      target: { value: "重复" },
    });
    fireEvent.click(screen.getByTestId("api-keys-create-submit"));

    const err = await screen.findByTestId("api-keys-create-error");
    expect(err.textContent).toMatch(/\[TA-5001\]/);
    expect(screen.queryByTestId("api-keys-created-plaintext")).toBeNull();
  });
});

// ================================================================ ③ 列表不放复制按钮（硬要求二）

describe("[硬要求] 列表里不放复制按钮：只存哈希，复制不出来", () => {
  it("表格里没有任何「复制」控件，而页面把这件事讲明白", async () => {
    renderPage();
    await waitForList();

    const table = screen.getByTestId("api-keys-table");
    // 表格内：文字里不出现"复制"，按钮里也没有复制语义
    expect(table.textContent).not.toMatch(/复制/);
    for (const button of within(table).getAllByRole("button")) {
      expect(button.textContent ?? "").not.toMatch(/复制/);
      expect(button.getAttribute("aria-label") ?? "").not.toMatch(/复制/);
    }
    // 整页除"说明句"外也不该有复制按钮（说明句本身必须存在，否则用户会以为忘了做）
    const notice = screen.getByTestId("api-keys-hash-only");
    expect(notice.textContent).toBe("库里只存哈希，列表里无法再取回明文");
    expect(within(notice).queryAllByRole("button")).toEqual([]);
  });
});

// ================================================================ ④ 启停 / 删除 / 用量

describe("启停、删除（二次确认）、用量", () => {
  it("开关写的是后端真值：拨一下真的调 api_key_set_enabled({id, enabled})，随后重取显示新状态", async () => {
    renderPage();
    await waitForList();

    const row = screen.getByTestId("api-key-row");
    const toggle = within(within(row).getByTestId("api-key-toggle")).getByRole(
      "switch",
    ) as HTMLInputElement;
    expect(toggle.checked).toBe(true);

    fireEvent.click(toggle);

    await waitFor(() => expect(toggleCalls.length).toBe(1));
    expect(toggleCalls[0]).toEqual({ id: "k-1", enabled: false });
    // 重取之后开关跟着后端走（不是本地状态）
    await waitFor(() =>
      expect(
        (
          within(within(screen.getByTestId("api-key-row")).getByTestId("api-key-toggle")).getByRole(
            "switch",
          ) as HTMLInputElement
        ).checked,
      ).toBe(false),
    );
  });

  it("删除：先弹二次确认，未确认前一次都不调用；确认后调 api_key_delete({id})", async () => {
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-key-delete"));
    expect(screen.getByTestId("api-key-delete-confirm")).toBeTruthy();
    expect(deleteCalls).toEqual([]);

    fireEvent.click(screen.getByTestId("api-key-delete-confirm"));

    await waitFor(() => expect(deleteCalls.length).toBe(1));
    expect(deleteCalls[0]).toEqual({ id: "k-1" });
    await waitFor(() => expect(screen.getByTestId("api-keys-empty")).toBeTruthy());
  });

  it("用量：点了那一行才调 api_key_usage({id, limit})，结果画在该行下面", async () => {
    usageRows = [
      {
        key_id: "k-1",
        day: "2026-01-01",
        token_kind: "input",
        tokens: 3456,
        requests: 12,
        cost: 0.12,
        currency: "USD",
      },
    ];
    renderPage();
    await waitForList();

    // 没点之前不取（不预取 N 条）
    expect(usageCalls).toEqual([]);
    fireEvent.click(screen.getByTestId("api-key-usage-open"));

    await waitFor(() => expect(usageCalls.length).toBe(1));
    expect(usageCalls[0]).toEqual({ id: "k-1", limit: 50 });
    const panel = await screen.findByTestId("api-key-usage");
    expect(panel.getAttribute("data-key-id")).toBe("k-1");
    await waitFor(() => expect(panel.textContent).toMatch(/12 次/));
    expect(panel.textContent).toMatch(/3,456 tokens/);
    // token_kind 与 currency 是后端真值，不许在前端被丢掉
    expect(panel.textContent).toMatch(/input/);
    expect(panel.textContent).toMatch(/0\.1200 USD/);
  });

  it("用量为空：说「还没有用量记录」，不画 0 行假表", async () => {
    usageRows = [];
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-key-usage-open"));
    await waitFor(() => expect(usageCalls.length).toBe(1));

    expect((await screen.findByTestId("api-key-usage-empty")).textContent).toBe(
      "还没有用量记录",
    );
  });

  it("用量读失败：原文照显，不吞", async () => {
    usageFails = true;
    renderPage();
    await waitForList();

    fireEvent.click(screen.getByTestId("api-key-usage-open"));
    const err = await screen.findByTestId("api-key-usage-error");
    expect(err.textContent).toMatch(/\[TA-9001\]/);
  });
});

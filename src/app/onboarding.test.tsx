// @vitest-environment jsdom
/**
 * 首启向导（[12 §6 F4] / [05 §2.4]）的验收。
 *
 * **这一页今天没有后端**：`onboarding_detect` / `onboarding_apply` / `onboarding_test_all`
 * 三条命令都还没在桌面端注册。所以这份测试要钉住的不是"功能好不好用"，而是**结构 + 诚实**：
 *
 * 1. **步骤骨架是真的**（[05 §2.4] 的四步）：每一步都有输入、有本地校验、有结果区。
 * 2. **没有任何"点了会成功"的假按钮**：验证步不许有「开始测试」，提交步不许有「生成配置」；
 *    两处都必须点名缺的是哪条命令，并且结果区必须明确写"从未调用过"（不是"全部失败"）。
 * 3. **校验与展开是真逻辑**：一行粘贴多个密钥会展开成多个凭据条目（提交契约每个条目只带一个
 *    密钥），校验按行给出行号级的错误。
 * 4. **接入信息是真读的**：来自 `app_clipboard_access_info`（已注册），不是拼出来的假地址。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import OnboardingPage from "@/pages/onboarding";
import {
  PROTOCOLS,
  buildOnboardingInput,
  credentialEntries,
  credentialLines,
  newProviderRow,
  newSubRow,
  payloadJson,
  testMatrix,
  validateProviders,
  validateSubscriptions,
} from "@/pages/onboarding/model";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
});

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <OnboardingPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

function textbox(testId: string): HTMLElement {
  return within(screen.getByTestId(testId)).getByRole("textbox");
}

function type(testId: string, value: string): void {
  fireEvent.change(textbox(testId), { target: { value } });
}

function isDisabled(el: HTMLElement): boolean {
  return (el as HTMLButtonElement).disabled;
}

/** 走完第 1 步：加一行并填成合法输入（含两个密钥）。 */
function fillValidProvider(): void {
  fireEvent.click(screen.getByTestId("onboarding-provider-add"));
  type("onboarding-provider-field-0-name", "字节方舟");
  type("onboarding-provider-field-0-baseUrl", "https://ark.example.com/v1");
  type("onboarding-provider-field-0-secret", "sk-first\nsk-second");
}

// ================================================================ ① 步骤骨架与本地校验

describe("[05 §2.4] 四步骨架：每步都有输入 / 校验 / 结果区", () => {
  it("第 1 步：加了一行但没填完 → 下一步禁用，并列出要改的地方；填完就放行", () => {
    renderPage();

    // 四步的进度与当前步都在页面上
    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("1");
    expect(screen.getByText(/第 1 \/ 4 步/)).toBeTruthy();
    // 没有任何服务商时不算"填错"：可以先去下一步，回头再配（说明里写了）
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(false);

    fireEvent.click(screen.getByTestId("onboarding-provider-add"));
    const issues = screen.getByTestId("onboarding-step1-issues");
    expect(issues.textContent).toMatch(/要改/);
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(true);

    // 接口地址不是 http(s) 时要给出行级错误
    type("onboarding-provider-field-0-name", "字节方舟");
    type("onboarding-provider-field-0-baseUrl", "ark.example.com");
    type("onboarding-provider-field-0-secret", "sk-first");
    expect(screen.getByTestId("onboarding-step1-issues").textContent).toMatch(/接口地址/);

    type("onboarding-provider-field-0-baseUrl", "https://ark.example.com/v1");
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(false);
  });

  it("第 1 步：标识重复会被拦下（标识是会写进配置的 id）", () => {
    renderPage();
    fireEvent.click(screen.getByTestId("onboarding-provider-add"));
    fireEvent.click(screen.getByTestId("onboarding-provider-add"));

    for (const i of [0, 1]) {
      type(`onboarding-provider-field-${i}-name`, `服务商 ${i}`);
      type(`onboarding-provider-field-${i}-baseUrl`, "https://a.example.com/v1");
      type(`onboarding-provider-field-${i}-secret`, "sk-1");
    }
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(false);

    // 把第二行的标识改成与第一行相同（标识是会写进配置的 id，必须唯一）
    fireEvent.change(textbox("onboarding-provider-field-1-id"), { target: { value: "p1" } });
    expect(screen.getByTestId("onboarding-step1-issues").textContent).toMatch(/标识/);
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(true);
  });

  it("第 2 步：「不用梯子」是明确选择，勾上后订阅列表为空且不再报错", () => {
    renderPage();
    fillValidProvider();
    fireEvent.click(screen.getByTestId("onboarding-next"));
    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("2");

    fireEvent.click(screen.getByTestId("onboarding-sub-add"));
    type("onboarding-sub-field-0-url", "not-a-url");
    expect(screen.getByTestId("onboarding-step2-issues").textContent).toMatch(/订阅链接/);
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(true);

    fireEvent.click(within(screen.getByTestId("onboarding-no-vpn")).getByRole("checkbox"));
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(false);
    expect(screen.getByTestId("onboarding-no-vpn-hint").textContent).toMatch(/不是/);
  });

  it("第 3 步：没有「开始测试」按钮，只给出将要测试的组合与未注册说明", async () => {
    renderPage();
    fillValidProvider();
    fireEvent.click(screen.getByTestId("onboarding-next"));
    fireEvent.click(within(screen.getByTestId("onboarding-no-vpn")).getByRole("checkbox"));
    fireEvent.click(screen.getByTestId("onboarding-next"));
    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("3");

    expect(screen.queryByTestId("onboarding-test-run")).toBeNull();
    expect(screen.getByTestId("onboarding-test-blocked").textContent).toMatch(/onboarding_test_all/);

    // 矩阵由输入推导（2 个凭据条目 × 线路候选，线路候选来自 config_get），不是测试结果
    const rows = await screen.findAllByTestId("onboarding-test-matrix-row");
    expect(rows.length).toBeGreaterThanOrEqual(2);
    expect(screen.getByTestId("onboarding-test-result").textContent).toMatch(
      /从来没有|从未调用过|没有测试结果/,
    );
  });

  it("第 4 步：没有「生成配置」按钮，只有 payload 预览与未注册说明", async () => {
    renderPage();
    fillValidProvider();
    fireEvent.click(screen.getByTestId("onboarding-next"));
    fireEvent.click(within(screen.getByTestId("onboarding-no-vpn")).getByRole("checkbox"));
    fireEvent.click(screen.getByTestId("onboarding-next"));
    fireEvent.click(screen.getByTestId("onboarding-next"));
    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("4");

    expect(screen.queryByTestId("onboarding-apply-submit")).toBeNull();
    expect(screen.getByTestId("onboarding-apply-blocked").textContent).toMatch(/onboarding_apply/);

    // payload 是真算出来的：一行两个密钥 → 两个凭据条目
    const payloadText = (await screen.findByTestId("onboarding-apply-payload")).textContent ?? "{}";
    const payload = JSON.parse(payloadText) as {
      providers: { id: string; name: string; secret: string }[];
      subscriptions: unknown[];
    };
    expect(payload.providers.length).toBe(2);
    expect(payload.providers[0]?.secret).toBe("sk-first");
    expect(payload.providers[1]?.secret).toBe("sk-second");
    expect(payload.subscriptions).toEqual([]);
    expect(screen.getByTestId("onboarding-apply-summary").textContent).toMatch(/2 个凭据条目/);
    expect(screen.getByTestId("onboarding-apply-result").textContent).toMatch(/不等于|没有提交结果/);
  });

  it("第 4 步：接入信息来自后端（app_clipboard_access_info），不是拼出来的", async () => {
    renderPage();
    expect(screen.getByTestId("onboarding-detect-note").textContent).toMatch(/onboarding_detect/);
    expect(screen.queryByTestId("onboarding-detect-run")).toBeNull();

    fireEvent.click(screen.getByTestId("onboarding-access-show"));
    const out = await screen.findByTestId("onboarding-access-output");
    expect(out.textContent).toContain("BASE_URL=http://127.0.0.1:8787/v1");
    expect(screen.getByTestId("onboarding-access-hint").textContent).toMatch(/原文/);
  });

  it("上一步能回去，且回到第 1 步时输入还在（不是重新开始）", () => {
    renderPage();
    fillValidProvider();
    fireEvent.click(screen.getByTestId("onboarding-next"));
    fireEvent.click(screen.getByTestId("onboarding-prev"));
    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("1");
    expect(textbox("onboarding-provider-field-0-name").getAttribute("value")).toBe("字节方舟");
  });
});

// ================================================================ ② 模型层（纯函数）

describe("向导模型层（纯函数）", () => {
  it("credentialLines：按行切、去空白、丢空行（一次粘贴多个密钥）", () => {
    expect(credentialLines("sk-a\n\n  sk-b  \r\n")).toEqual(["sk-a", "sk-b"]);
    expect(credentialLines("   ")).toEqual([]);
    expect(credentialLines("sk-only")).toEqual(["sk-only"]);
  });

  it("credentialEntries：一行多密钥展开成多个条目，每个条目只带一个密钥", () => {
    const row = { ...newProviderRow(1), name: "中转A", base_url: "https://a.example/v1", secret: "k1\nk2\nk3" };
    const entries = credentialEntries([row]);
    expect(entries.length).toBe(3);
    expect(new Set(entries.map((e) => e.id)).size).toBe(3);
    expect(entries.map((e) => e.secret)).toEqual(["k1", "k2", "k3"]);
    // 单密钥时标识保持原样（不与多密钥的编号形式混淆）
    const single = credentialEntries([{ ...row, secret: "k1" }]);
    expect(single.length).toBe(1);
    expect(single[0]?.id).toBe(row.id);
  });

  it("validateProviders：标识空/重复、名称空、地址非 http(s)、密钥空都能报出来", () => {
    const a = { ...newProviderRow(1), id: "", name: "", base_url: "nope", secret: "" };
    const b = { ...newProviderRow(2), id: "p1", name: "B", base_url: "https://b.example/v1", secret: "k" };
    const c = { ...newProviderRow(3), id: "p1", name: "C", base_url: "https://c.example/v1", secret: "k" };

    const issues = validateProviders([a, b, c]).map((i) => `${i.row}:${i.path}`);
    expect(issues).toContain("0:id");
    expect(issues).toContain("0:name");
    expect(issues).toContain("0:base_url");
    expect(issues).toContain("0:secret");
    expect(issues).toContain("2:id"); // 与第 1 行重复
    expect(validateProviders([b])).toEqual([]);
  });

  it("validateSubscriptions：链接必须是 http(s)；不用梯子时整段跳过", () => {
    const bad = { ...newSubRow(1), id: "s1", name: "机场", url: "ftp://x" };
    expect(validateSubscriptions([bad]).map((i) => i.path)).toEqual(["url"]);
    expect(validateSubscriptions([])).toEqual([]);
  });

  it("buildOnboardingInput：不用梯子 → 订阅为空；内核路径空白 → null", () => {
    const provider = { ...newProviderRow(1), name: "A", base_url: "https://a.example/v1", secret: "k" };
    const sub = { ...newSubRow(1), name: "机场", url: "https://sub.example/x" };

    const withVpn = buildOnboardingInput({
      kernelPath: "  ",
      providers: [provider],
      subscriptions: [sub],
      noVpn: false,
    });
    expect(withVpn.kernel_path).toBeNull();
    expect(withVpn.subscriptions.map((s) => s.url)).toEqual(["https://sub.example/x"]);
    expect(withVpn.providers.length).toBe(1);

    const noVpn = buildOnboardingInput({
      kernelPath: "C:/ta/mihomo.exe",
      providers: [provider],
      subscriptions: [sub],
      noVpn: true,
    });
    expect(noVpn.kernel_path).toBe("C:/ta/mihomo.exe");
    expect(noVpn.subscriptions).toEqual([]);

    // payloadJson 是稳定可读的原文（预览与断言用同一份）
    expect(JSON.parse(payloadJson(noVpn))).toEqual(noVpn);
  });

  it("testMatrix：凭据条目 × 线路候选；没有线路时为空（不编线路）", () => {
    const entries = credentialEntries([
      { ...newProviderRow(1), name: "A", base_url: "https://a.example/v1", secret: "k1\nk2" },
    ]);
    expect(testMatrix(entries, []).length).toBe(0);
    expect(testMatrix(entries, ["直连", "日本线"]).length).toBe(4);
    expect(testMatrix([], ["直连"]).length).toBe(0);
  });

  it("协议清单与冻结契约的 Protocol 取值一致", () => {
    expect([...PROTOCOLS]).toEqual(["openai", "openai_responses", "anthropic", "gemini", "custom"]);
  });
});

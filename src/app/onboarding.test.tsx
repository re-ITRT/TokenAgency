// @vitest-environment jsdom
/**
 * 首启向导（[12 §6 F4] / [05 §2.4]）的验收。
 *
 * **三条命令都已注册**（`settings_ext.rs` 的 `onboarding_detect` / `onboarding_apply` /
 * `onboarding_test_all`，见 `src-tauri/src/commands/mod.rs` 的 `collect_commands!`），
 * 所以这一版钉的是**真流程 + 诚实**：
 *
 * 1. **探测行必须是后端真值**：内核找到与否、是否已有配置、后端建议的服务商，全部来自
 *    `onboarding_detect` 的返回；探测失败就显示失败，不拿默认值糊过去。
 * 2. **应用结果按 `ApplyResult` 如实显示**：`applied` / `restart_required` / `kernel_reload`
 *    各说各的 —— **不许把"需要重启"翻译成"已完成"**。
 * 3. **测试结果每条都来自 `onboarding_test_all`**：通过/失败、延迟、后端说明（含 `ok` 口径）
 *    原样显示；"没有结果"与"全部失败"是两件事。
 * 4. **跳过就是跳过**：跳过只留下一句"没有写入任何配置"，不许出现"已配置"。
 * 5. 模型层仍是纯函数，单独测。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import OnboardingPage from "@/pages/onboarding";
import {
  PROTOCOLS,
  applyFacts,
  buildOnboardingInput,
  credentialEntries,
  credentialLines,
  detectFacts,
  newProviderRow,
  newSubRow,
  payloadJson,
  testMatrix,
  validateProviders,
  validateSubscriptions,
} from "@/pages/onboarding/model";
import { registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

/** 每个测试都从基线 mock 开始，需要时再覆盖具体命令。 */
beforeEach(() => {
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

/** 直接跳到第 n 步（填完第 1 步 + 勾「不用梯子」+ 逐次下一步）。 */
function gotoStep(n: number): void {
  fillValidProvider();
  fireEvent.click(screen.getByTestId("onboarding-next"));
  fireEvent.click(within(screen.getByTestId("onboarding-no-vpn")).getByRole("checkbox"));
  while (Number(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")) < n) {
    fireEvent.click(screen.getByTestId("onboarding-next"));
  }
}

// ================================================================ ① 探测：每行都是后端真值

describe("第 1 步：探测展示的是 onboarding_detect 的真值", () => {
  it("内核 / 已有配置 / 建议服务商三行都来自后端返回", async () => {
    registerMock("onboarding_detect", () => ({
      kernel_found: true,
      existing_config: false,
      suggested_providers: ["字节方舟", "command code"],
    }));
    renderPage();

    expect((await screen.findByTestId("onboarding-detect-kernel")).getAttribute("data-value")).toBe(
      "found",
    );
    expect((await screen.findByTestId("onboarding-detect-config")).getAttribute("data-value")).toBe(
      "missing",
    );
    const suggested = await screen.findByTestId("onboarding-detect-suggested");
    expect(suggested.textContent).toContain("字节方舟");
    expect(suggested.textContent).toContain("command code");
    // 有探测命令就有按钮（不是"未注册所以没有"）
    expect(screen.getByTestId("onboarding-detect-run")).toBeTruthy();
  });

  it("后端说没找到内核 / 已有配置时，页面照它说（不换成默认值）", async () => {
    registerMock("onboarding_detect", () => ({
      kernel_found: false,
      existing_config: true,
      suggested_providers: [],
    }));
    renderPage();

    expect((await screen.findByTestId("onboarding-detect-kernel")).getAttribute("data-value")).toBe(
      "missing",
    );
    expect((await screen.findByTestId("onboarding-detect-config")).getAttribute("data-value")).toBe(
      "existing",
    );
    // 空建议列表：如实说"后端没有建议"，不编一个
    expect((await screen.findByTestId("onboarding-detect-suggested")).textContent).toMatch(
      /没有|没有给出/,
    );
  });

  it("探测失败 → 显示失败原因，不显示任何「看起来正常」的默认值", async () => {
    registerMock("onboarding_detect", () => {
      throw new Error("探测炸了");
    });
    renderPage();

    const err = await screen.findByTestId("onboarding-detect-error");
    expect(err.textContent).toMatch(/探测失败/);
    expect(err.textContent).toMatch(/探测炸了/);
    // 失败时不许出现"已找到内核"这种没根据的行
    expect(screen.queryByTestId("onboarding-detect-kernel")).toBeNull();
  });
});

// ================================================================ ② 连通性测试：结果来自后端

describe("第 3 步：onboarding_test_all 的结果原样显示", () => {
  it("点「开始测试」真的调用命令，并逐条显示通过/失败与后端说明", async () => {
    registerMock("onboarding_test_all", () => [
      {
        credential_id: "c-a",
        lane_id: "direct",
        ok: true,
        latency_ms: 18,
        status: null,
        note: "口径：线路可达（内核真测延迟）+ 密钥引用可解析",
      },
      { credential_id: "c-b", lane_id: "jp", ok: false, note: "内核测速失败：超时" },
    ]);
    renderPage();
    gotoStep(3);

    expect(screen.getByTestId("onboarding-test-result").textContent).toMatch(/还没有|没有测试结果/);
    fireEvent.click(screen.getByTestId("onboarding-test-run"));

    const rows = await screen.findAllByTestId("onboarding-test-row");
    expect(rows.length).toBe(2);
    expect(rows[0]?.getAttribute("data-ok")).toBe("true");
    expect(rows[1]?.getAttribute("data-ok")).toBe("false");
    expect(rows[0]?.textContent).toContain("c-a");
    expect(rows[0]?.textContent).toContain("direct");
    expect(rows[0]?.textContent).toContain("18");
    // 后端说明（含 ok 口径）原文出现，且失败原因不丢
    expect(rows[0]?.textContent).toContain("口径：线路可达");
    expect(rows[1]?.textContent).toContain("内核测速失败：超时");
  });

  it("命令失败（例如没装配内核）→ 显示错误原文，不当成「全部失败」", async () => {
    registerMock("onboarding_test_all", () => {
      throw new Error("没有装配内核");
    });
    renderPage();
    gotoStep(3);
    fireEvent.click(screen.getByTestId("onboarding-test-run"));

    const err = await screen.findByTestId("onboarding-test-error");
    expect(err.textContent).toMatch(/没有装配内核/);
    expect(screen.queryAllByTestId("onboarding-test-row").length).toBe(0);
  });
});

// ================================================================ ③ 应用：按 ApplyResult 如实说

describe("第 4 步：onboarding_apply 的结果按 ApplyResult 如实显示", () => {
  it("restart_required=true → 显示「需要重启」，不许说「已完成」", async () => {
    registerMock("onboarding_apply", () => ({
      applied: true,
      restart_required: true,
      kernel_reload: false,
    }));
    renderPage();
    gotoStep(4);

    fireEvent.click(screen.getByTestId("onboarding-apply-submit"));
    expect((await screen.findByTestId("onboarding-apply-applied")).textContent).toMatch(/已写入/);
    expect((await screen.findByTestId("onboarding-apply-restart")).textContent).toMatch(/重启/);
    expect(screen.queryByTestId("onboarding-apply-kernel-reload")).toBeNull();

    const text = screen.getByTestId("onboarding-apply-result").textContent ?? "";
    expect(text).not.toMatch(/已完成|已生成|全部完成/);
  });

  it("kernel_reload=true → 显示「需要重载内核」，两条都为 false 才说无需重启", async () => {
    registerMock("onboarding_apply", () => ({
      applied: true,
      restart_required: false,
      kernel_reload: true,
    }));
    renderPage();
    gotoStep(4);
    fireEvent.click(screen.getByTestId("onboarding-apply-submit"));

    expect((await screen.findByTestId("onboarding-apply-kernel-reload")).textContent).toMatch(
      /重载/,
    );
    expect(screen.queryByTestId("onboarding-apply-restart")).toBeNull();
  });

  it("applied=false → 如实说后端没有写入，而不是「已生成」", async () => {
    registerMock("onboarding_apply", () => ({
      applied: false,
      restart_required: false,
      kernel_reload: false,
    }));
    renderPage();
    gotoStep(4);
    fireEvent.click(screen.getByTestId("onboarding-apply-submit"));

    const applied = await screen.findByTestId("onboarding-apply-applied");
    expect(applied.getAttribute("data-applied")).toBe("false");
    expect(applied.textContent).toMatch(/没有写入|未写入/);
    expect(screen.getByTestId("onboarding-apply-result").textContent).not.toMatch(/已生成/);
  });

  it("命令失败 → 显示错误原文（含 TA-xxxx 时原样带出）", async () => {
    registerMock("onboarding_apply", () => {
      throw new Error("配置校验失败：providers[0].id");
    });
    renderPage();
    gotoStep(4);
    fireEvent.click(screen.getByTestId("onboarding-apply-submit"));

    const err = await screen.findByTestId("onboarding-apply-error");
    expect(err.textContent).toMatch(/providers\[0\]\.id/);
  });
});

// ================================================================ ④ 跳过：跳过就是跳过

describe("跳过向导：没有写入任何配置", () => {
  it("跳过只留下「已跳过」，且不出现「已配置」", async () => {
    renderPage();
    fireEvent.click(screen.getByTestId("onboarding-skip"));

    const note = await screen.findByTestId("onboarding-skip-note");
    expect(note.textContent).toMatch(/已跳过/);
    expect(note.textContent).toMatch(/没有写入/);
    expect(document.body.textContent ?? "").not.toMatch(/已配置/);
    // 跳过之后仍然可以回来继续（不是死路）
    expect(screen.getByTestId("onboarding-skip-resume")).toBeTruthy();
  });

  it("跳过状态下不渲染「开始测试」/「应用这份配置」的提交入口", async () => {
    renderPage();
    fireEvent.click(screen.getByTestId("onboarding-skip"));
    gotoStep(3);
    expect(screen.queryByTestId("onboarding-test-run")).toBeNull();
    fireEvent.click(screen.getByTestId("onboarding-next"));
    expect(screen.queryByTestId("onboarding-apply-submit")).toBeNull();
  });
});

// ================================================================ ⑤ 步骤骨架与本地校验（原有）

describe("[05 §2.4] 四步骨架：每步都有输入 / 校验 / 结果区", () => {
  it("第 1 步：加了一行但没填完 → 下一步禁用，并列出要改的地方；填完就放行", () => {
    renderPage();

    expect(screen.getByTestId("onboarding-step-panel").getAttribute("data-step")).toBe("1");
    expect(screen.getByText(/第 1 \/ 4 步/)).toBeTruthy();
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(false);

    fireEvent.click(screen.getByTestId("onboarding-provider-add"));
    expect(screen.getByTestId("onboarding-step1-issues").textContent).toMatch(/要改/);
    expect(isDisabled(screen.getByTestId("onboarding-next"))).toBe(true);

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

  it("第 4 步：payload 是真算出来的（一行两个密钥 → 两个凭据条目）", async () => {
    renderPage();
    gotoStep(4);

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
  });

  it("接入信息来自后端（app_clipboard_access_info），不是拼出来的", async () => {
    renderPage();
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

// ================================================================ ⑥ 模型层（纯函数）

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
    expect(issues).toContain("2:id");
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

  it("detectFacts：把后端布尔值变成「有/没有」两档，不掺默认值", () => {
    expect(
      detectFacts({ kernel_found: true, existing_config: false, suggested_providers: ["A"] }),
    ).toEqual({ kernel: "found", config: "missing", suggested: ["A"] });
    expect(
      detectFacts({ kernel_found: false, existing_config: true, suggested_providers: [] }),
    ).toEqual({ kernel: "missing", config: "existing", suggested: [] });
  });

  it("applyFacts：逐项列出后端说的事实；两个 false 才给「无需重启/重载」", () => {
    expect(applyFacts({ applied: true, restart_required: true, kernel_reload: false })).toEqual([
      "applied",
      "restart",
    ]);
    expect(applyFacts({ applied: true, restart_required: false, kernel_reload: true })).toEqual([
      "applied",
      "kernelReload",
    ]);
    expect(applyFacts({ applied: true, restart_required: false, kernel_reload: false })).toEqual([
      "applied",
      "effective",
    ]);
    expect(applyFacts({ applied: false, restart_required: false, kernel_reload: false })).toEqual([
      "notApplied",
      "effective",
    ]);
  });
});

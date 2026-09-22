// @vitest-environment jsdom
/**
 * [docs/19 §1] `/providers` 的**两步向导**（流程 A）验收。
 *
 * 这一份测的是**用户点得到的那条路**，所以每一条都尽量按真机 E2E 的方式驱动：
 * 开的入口是 `providers-add-open` → 第 1 步 `provider-wizard-step1` → `pw-*` →
 * 第 2 步 `provider-wizard-step2`，`data-testid` 一个都不改名（真机按它们驱动）。
 *
 * 三条硬纪律在这里被钉住：
 *
 * 1. **红先绿后**：控件存在 → 真输入 → 真的调了该命令 → 入参形状正确 → 成功后显示**后端真值**。
 * 2. **落库时机说清楚**：`pw-next` 与 `pw-test`（服务商还没落库时）都会真的写配置，
 *    并且必须在界面上说出这件事（`pw-saved-notice`）—— 绝不静默产生持久化副作用。
 * 3. **明文只进密钥仓库**：`pw-key` 的明文只允许出现在 `credential_set_secret` 的入参里；
 *    这里用一个假金丝雀值证明它不在服务商/凭据载荷里，也不在页面上（真机上那是密钥泄漏）。
 *
 * 命令接线（[docs/19 §1.3]）：`provider_upsert` → `credential_upsert`（id = `<provider>-default`）
 * → 填了密钥才 `credential_set_secret`；扫描与添加走 `model_import_from_provider` /
 * `model_apply_import`；改网络代理走 `model_bulk_update({ ids, patch: { lane } })`。
 * 这三条本轮由另一个写者在 Rust 注册（`collect_commands!`），所以**测试用 `registerMock` 驱动**，
 * 页面本身通过 `@/shared/api` 的类型化门面调用（`api.provider.test` 等），
 * 不写裸命令字符串 —— `commandSurface.guard.test.ts` 因此保持绿。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import ProvidersPage from "@/pages/providers";
import { resetWizardSession } from "@/pages/providers/providerWizardModel";
import { TokenAgencyError, registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

beforeEach(() => {
  registerBaselineMocks();
  // 草稿活在模块级 store 里（[docs/19 §1.4] 要求"去 /network 再回来草稿还在"）：
  // 每条用例从干净状态开始，否则上一条的草稿会串到下一条。
  resetWizardSession();
});

afterEach(() => {
  cleanup();
});

// ================================================================ 工具

/** 明文金丝雀：它出现在这里，正是为了证明它**不会**出现在服务商/凭据载荷与页面上。 */
const KEY = "sk-plaintext-canary-0001"; // ta-allow-fake-secret —— 假值

function inputOf(testId: string): HTMLInputElement {
  const input = screen.getByTestId(testId).querySelector("input");
  if (input === null) throw new Error(`[data-testid="${testId}"] 里没有 input`);
  return input;
}

function typeInto(testId: string, value: string): void {
  fireEvent.change(inputOf(testId), { target: { value } });
}

async function chooseOption(testId: string, name: RegExp | string): Promise<void> {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name }));
}

/**
 * 下拉当前显示的文案。MUI 的 `Select` 把选中值渲染在 `[role="combobox"]` 里
 * （真机上 E2E 看的是同一处），所以断言它而不是隐藏 input 的 value。
 */
function selectTextOf(testId: string): string {
  return (within(screen.getByTestId(testId)).getByRole("combobox").textContent ?? "").trim();
}

function recordingMock(result: unknown): {
  calls: Record<string, unknown>[];
  handler: (args?: Record<string, unknown>) => unknown;
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

/** `/network` 的替身：只为了断言"去的是哪个 URL"与"回程还能回到 /providers"。 */
function NetworkStub() {
  const navigate = useNavigate();
  const location = useLocation();
  return (
    <div>
      <span data-testid="network-stub-search">{location.search}</span>
      <button type="button" data-testid="network-stub-back" onClick={() => navigate("/providers")}>
        back
      </button>
    </div>
  );
}

function renderApp() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <MemoryRouter initialEntries={["/providers"]}>
          <Routes>
            <Route path="/providers" element={<ProvidersPage />} />
            <Route path="/network" element={<NetworkStub />} />
          </Routes>
        </MemoryRouter>
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------- 后端夹具

/** `credential_list` 的一行（`CredentialView` 形状）：把凭据挂到某个服务商上。 */
function credentialView(id: string, provider: string): unknown {
  return {
    credential: {
      id,
      provider,
      label: "默认凭据",
      secret: `\${secret:${id}}`,
      lane: null,
      quota: {
        metric: "cost",
        limit: 20,
        currency: "CNY",
        window: "month",
        reset_day: 1,
        soft_limit: 0.8,
        hard_limit: 1,
        reserve: 0,
      },
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "wait" },
      tags: [],
      enabled: true,
    },
    provider_name: provider,
    quota: { credential_id: id, state: "ok" },
    channel_count: 1,
  };
}

/** `channel_matrix`：模型与凭据的挂载关系（"这个服务商下有哪些模型"的唯一真源）。 */
function matrixView(
  cells: Record<string, unknown>[],
  models: Record<string, unknown>[] = [],
): unknown {
  return {
    models,
    credentials: [],
    cells: cells.map((cell) => ({ tags: [], enabled: true, ...cell })),
  };
}

/** 落库三个命令的 recorder（`provider_upsert` / `credential_upsert` / `credential_set_secret`）。 */
function registerUpsertMocks() {
  const provider = recordingMock({
    id: "my-gateway",
    name: "My Gateway",
    protocol: "openai",
    base_url: "https://gw.example.com/v1",
  });
  const credential = recordingMock({
    id: "my-gateway-default",
    provider: "my-gateway",
    label: "My Gateway",
    secret: "${secret:my-gateway-default}",
  });
  const secret = recordingMock(undefined);
  registerMock("provider_upsert", provider.handler);
  registerMock("credential_upsert", credential.handler);
  registerMock("credential_set_secret", secret.handler);
  return { provider, credential, secret };
}

async function openWizard(): Promise<void> {
  renderApp();
  // 等目录读回来：名称去重必须对着**后端返回的**现有 name / id 做
  await screen.findByText("字节方舟");
  fireEvent.click(screen.getByTestId("providers-add-open"));
  await screen.findByTestId("provider-wizard-step1");
}

function fillStep1(name = "My Gateway", baseUrl = "https://gw.example.com/v1"): void {
  typeInto("pw-name", name);
  typeInto("pw-baseUrl", baseUrl);
  typeInto("pw-key", KEY);
}

async function gotoStep2(name = "My Gateway"): Promise<void> {
  await openWizard();
  fillStep1(name);
  fireEvent.click(screen.getByTestId("pw-next"));
  await screen.findByTestId("provider-wizard-step2");
}

// ================================================================ ① 第 1 步：真控件

describe("[docs/19 §1.1] 第 1 步：服务商信息", () => {
  it("从 providers-add-open 进的是两步向导；第 1 步的控件一个不少，密钥是 password", async () => {
    await openWizard();

    for (const id of [
      "pw-name",
      "pw-protocol",
      "pw-baseUrl",
      "pw-key",
      "pw-test",
      "pw-test-result",
      "pw-proxy",
      "pw-next",
      "pw-cancel",
      "pw-step1-issues",
    ]) {
      if (id === "pw-test-result" || id === "pw-step1-issues") continue; // 错与结果只在真有内容时出现
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }

    expect(inputOf("pw-name").tagName).toBe("INPUT");
    expect(inputOf("pw-baseUrl").tagName).toBe("INPUT");
    // 密钥：type=password（明文不进页面）
    expect(inputOf("pw-key").type).toBe("password");
    // 协议：下拉，选项来自 PROTOCOLS（不是前端另编一份）
    fireEvent.mouseDown(within(screen.getByTestId("pw-protocol")).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: "Anthropic" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "Gemini" })).toBeTruthy();

    // 它确实是"两步"：第 2 步的容器还不存在
    expect(screen.queryByTestId("provider-wizard-step2")).toBeNull();
    // 默认在第一页，能取消
    expect(screen.getByTestId("pw-cancel")).toBeTruthy();
  });

  it("高级添加入口仍然保留着原来的单步对话框（字段 tid 一个都没变）", async () => {
    renderApp();
    await screen.findByText("字节方舟");
    fireEvent.click(screen.getByTestId("providers-add-advanced-open"));

    for (const id of ["providers-add-id", "providers-add-name", "providers-add-baseUrl", "providers-add-submit"]) {
      expect(screen.getByTestId(id), `高级添加缺控件 ${id}`).toBeTruthy();
    }
    // 新向导没有被这个入口打开
    expect(screen.queryByTestId("provider-wizard-step1")).toBeNull();
  });

  it("pw-name 与现有 name / id 去重：本地立刻拦下，一次命令都不发；改名后放行", async () => {
    const upsert = recordingMock({});
    registerMock("provider_upsert", upsert.handler);

    await openWizard();
    fillStep1("字节方舟");

    // 本地即时提示（不用点下一步就该出现）
    expect((await screen.findByTestId("pw-step1-issues")).textContent).toContain(
      "这个名称已经被占用",
    );
    fireEvent.click(screen.getByTestId("pw-next"));
    expect(upsert.calls).toHaveLength(0);
    expect(screen.queryByTestId("provider-wizard-step2")).toBeNull();

    // 名字直接撞上现有 provider 的 id 也算重复
    typeInto("pw-name", "p-bytedance");
    fireEvent.click(screen.getByTestId("pw-next"));
    expect(screen.getByTestId("pw-step1-issues").textContent).toContain("这个名称已经被占用");
    expect(upsert.calls).toHaveLength(0);

    // 必填与 base_url 形状
    typeInto("pw-name", "My Gateway");
    typeInto("pw-baseUrl", "ftp://gw.example.com");
    fireEvent.click(screen.getByTestId("pw-next"));
    expect(screen.getByTestId("pw-step1-issues").textContent).toContain(
      "必须是 http:// 或 https:// 开头的完整地址",
    );
    expect(upsert.calls).toHaveLength(0);
  });

  it("pw-proxy 去 /network?from=provider-wizard；回程后第 1 步草稿还在（pw-name / pw-baseUrl）", async () => {
    await openWizard();
    typeInto("pw-name", "My Gateway");
    typeInto("pw-baseUrl", "https://gw.example.com/v1");
    typeInto("pw-key", KEY);

    fireEvent.click(screen.getByTestId("pw-proxy"));

    // 去的是哪个 URL
    expect((await screen.findByTestId("network-stub-search")).textContent).toBe(
      "?from=provider-wizard",
    );

    // 回程
    fireEvent.click(screen.getByTestId("network-stub-back"));

    // 向导还在（module store 记着"开着"），草稿一个字段都没丢
    await screen.findByTestId("provider-wizard-step1");
    expect(inputOf("pw-name").value).toBe("My Gateway");
    expect(inputOf("pw-baseUrl").value).toBe("https://gw.example.com/v1");
  });
});

// ================================================================ ② 测试连接（先落库再探测）

describe("[docs/19 §1.1 / §1.3] pw-test：先真落库（并说出来），再真调 provider_test", () => {
  it("还没落库时点 pw-test：provider_upsert → credential_upsert → credential_set_secret，并显示 pw-saved-notice", async () => {
    const order: string[] = [];
    const { provider, credential, secret } = registerUpsertMocks();
    // 重新包一层记顺序
    registerMock("provider_upsert", (args) => {
      order.push("provider_upsert");
      return provider.handler(args);
    });
    registerMock("credential_upsert", (args) => {
      order.push("credential_upsert");
      return credential.handler(args);
    });
    registerMock("credential_set_secret", (args) => {
      order.push("credential_set_secret");
      return secret.handler(args);
    });
    // 后端真值：延迟 137（不是任何前端常量）
    const test = recordingMock({ ok: true, status: 200, latency_ms: 137, probe: "models", note: null });
    registerMock("provider_test", test.handler);

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-test"));

    await waitFor(() => expect(test.calls).toHaveLength(1));

    // 顺序：先落库、再写密钥、最后才探测
    expect(order).toEqual(["provider_upsert", "credential_upsert", "credential_set_secret"]);

    // 入参形状（snake_case，与契约一致）
    expect(Object.keys(provider.calls[0]!)).toEqual(["provider"]);
    const p = provider.calls[0]!["provider"] as Record<string, unknown>;
    expect(p["id"]).toBe("my-gateway");
    expect(p["name"]).toBe("My Gateway");
    expect(p["protocol"]).toBe("openai");
    expect(p["base_url"]).toBe("https://gw.example.com/v1");
    expect(JSON.stringify(provider.calls[0])).not.toContain(KEY);

    expect(Object.keys(credential.calls[0]!)).toEqual(["credential"]);
    const c = credential.calls[0]!["credential"] as Record<string, unknown>;
    expect(c["id"]).toBe("my-gateway-default");
    expect(c["provider"]).toBe("my-gateway");
    expect(c["secret"]).toBe("${secret:my-gateway-default}");
    expect(JSON.stringify(credential.calls[0])).not.toContain(KEY);

    // 明文只进密钥仓库
    expect(secret.calls[0]).toEqual({ id: "my-gateway-default", plaintext: KEY });
    expect(document.body.textContent ?? "").not.toContain(KEY);

    // 探测的是真的落库 id
    expect(test.calls[0]!["id"]).toBe("my-gateway");

    // 成功显示后端给的延迟
    const result = await screen.findByTestId("pw-test-result");
    expect(result.textContent).toContain("137");

    // 绝不静默产生持久化副作用：必须说出来
    expect(screen.getByTestId("pw-saved-notice").textContent).toContain("关掉向导不会撤销");
  });

  it("status=404 && probe=models 显示「上游没有 /models 端点」，绝不渲染成连接失败", async () => {
    registerUpsertMocks();
    registerMock("provider_test", () => ({
      ok: false,
      status: 404,
      latency_ms: 21,
      probe: "models",
      note: null,
    }));

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-test"));

    const result = await screen.findByTestId("pw-test-result");
    expect(result.textContent).toBe("上游没有 /models 端点（这不等于连不通）");
    expect(result.textContent).not.toContain("失败");
  });

  it("其它失败原样显示后端原文（含 TA 码），不吞错、不改写成一句好听话", async () => {
    registerUpsertMocks();
    registerMock("provider_test", () => {
      throw new TokenAgencyError({ code: "TA-7001", message: "线路不可用：内核没装配" });
    });

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-test"));

    const result = await screen.findByTestId("pw-test-result");
    expect(result.textContent).toContain("TA-7001");
    expect(result.textContent).toContain("线路不可用：内核没装配");
  });

  it("测试中只有 pending 禁用态，没有假进度文案", async () => {
    registerUpsertMocks();
    let resolve!: (value: unknown) => void;
    registerMock(
      "provider_test",
      () => new Promise((r) => { resolve = r; }),
    );

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-test"));

    await waitFor(() => expect((screen.getByTestId("pw-test") as HTMLButtonElement).disabled).toBe(true));
    expect(screen.queryByTestId("pw-test-result")).toBeNull();

    await act(async () => {
      resolve({ ok: true, status: 200, latency_ms: 82, probe: "models", note: null });
    });
    expect((await screen.findByTestId("pw-test-result")).textContent).toContain("82");
    expect((screen.getByTestId("pw-test") as HTMLButtonElement).disabled).toBe(false);
  });
});

// ================================================================ ③ 第 2 步：结构与已添加模型

describe("[docs/19 §1.2] 第 2 步：添加模型", () => {
  it("pw-next 真的落库并进入第 2 步：滚动容器 / 已添加模型 / 保存说明 / 扫描与完成都在", async () => {
    const { provider, credential, secret } = registerUpsertMocks();

    await gotoStep2();

    // 第 1 步的落库真的发生了
    expect(provider.calls).toHaveLength(1);
    expect(credential.calls).toHaveLength(1);
    expect(secret.calls).toHaveLength(1);

    // 第 1 步的容器退场，第 2 步登场
    expect(screen.queryByTestId("provider-wizard-step1")).toBeNull();
    expect(screen.getByTestId("provider-wizard-step2")).toBeTruthy();

    // 已落库要说清楚（关掉向导不会撤销）
    expect(screen.getByTestId("pw-saved-notice").textContent).toContain("服务商与凭据已经写入配置");

    // 整页可滚动：上下两块都在**同一个**滚动容器里
    const scroll = screen.getByTestId("pw-step2-scroll");
    expect(within(scroll).getByTestId("pw-added-models")).toBeTruthy();
    expect(within(scroll).getByTestId("pw-scan")).toBeTruthy();
    expect(within(scroll).getByTestId("pw-finish")).toBeTruthy();

    // 这个服务商下还没有模型 → 诚实空态（不是 0 行的表，也不是假数据）
    expect(screen.getByText("这个服务商下还没有模型")).toBeTruthy();
  });

  it("已添加模型来自 channel_matrix：行里只有网络代理可改，信息面板是只读的", async () => {
    registerUpsertMocks();
    registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
    registerMock("channel_matrix", () =>
      matrixView(
        [
          {
            model_id: "m-added",
            credential_id: "my-gateway-default",
            channel_id: "ch-1",
            lane_id: "lane-jp",
            upstream_model: "upstream-added",
          },
        ],
        [{ model_id: "m-added", display_name: "m-added", unpriced: true }],
      ),
    );

    await gotoStep2();

    const row = (await screen.findAllByTestId("pw-model-row"))[0]!;
    // 行里显示的是后端给的值（模型 id 与上游名）
    expect(row.textContent).toContain("m-added");
    expect(row.textContent).toContain("upstream-added");

    // 「设置图标」按钮：hover 提示必须是「查看模型信息」
    const info = within(row).getByTestId("pw-model-info");
    expect(info.getAttribute("title")).toBe("查看模型信息");

    // 点开只读面板
    fireEvent.click(within(info).getByRole("button"));
    const panel = within(row).getByTestId("pw-model-info-panel");
    expect(panel.textContent).toContain("这些信息来自配置，只能看不能改");
    // 面板里不许有任何可写控件（唯一的例外是同一行的 pw-model-lane，它不在面板内）
    expect(panel.querySelectorAll("input, select, textarea, button")).toHaveLength(0);

    // 该行唯一可改的就是网络代理（默认显示后端给的 lane，而不是一个猜的值）
    expect(within(row).getByTestId("pw-model-lane")).toBeTruthy();
  });

  it("pw-model-lane 默认值：channel 有 lane 就用它；没有就继承第 1 步（/network 选的 net.default_lane）；再没有 = 直连", async () => {
    registerUpsertMocks();
    registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
    let cellLane: string | null = null;
    registerMock("channel_matrix", () =>
      matrixView(
        [
          {
            model_id: "m-added",
            credential_id: "my-gateway-default",
            channel_id: "ch-1",
            lane_id: cellLane,
            upstream_model: "upstream-added",
          },
        ],
        [{ model_id: "m-added", display_name: "m-added" }],
      ),
    );

    // ① 第 1 步没选（net.default_lane = null）且 channel 没有 lane → 直连
    registerMock("config_get", () => ({ net: { default_lane: null } }));
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    expect(selectTextOf("pw-model-lane")).toBe("直连");

    cleanup();
    resetWizardSession();

    // ② 第 1 步（在 /network 上）选了 lane-jp → 新模型默认继承它
    registerMock("config_get", () => ({ net: { default_lane: "lane-jp" } }));
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    await waitFor(() => expect(selectTextOf("pw-model-lane")).toContain("日本"));

    cleanup();
    resetWizardSession();

    // ③ channel 自己已经有 lane → 以后端为准
    cellLane = "lane-jp";
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    expect(selectTextOf("pw-model-lane")).toContain("日本");

    cleanup();
    resetWizardSession();

    // ④ 还没有 channel、但模型级默认 lane 已经存下来了（`Model.default_lane`）→ 显示它
    //    （否则用户改完 lane 在界面上看不到任何变化）
    cellLane = null;
    registerMock("config_get", () => ({ net: { default_lane: null } }));
    registerMock("model_list", () => [
      { model: { id: "m-added", aliases: [], display: { group: null, order: 0 }, expose: true, default_lane: "lane-jp" }, channel_count: 0, upstream_names: [], priced: false },
    ]);
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    expect(selectTextOf("pw-model-lane")).toContain("日本");
  });

  it("改 pw-model-lane 真的落库：model_bulk_update({ ids: [模型], patch: { lane } })", async () => {
    registerUpsertMocks();
    registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
    registerMock("channel_matrix", () =>
      matrixView(
        [
          {
            model_id: "m-added",
            credential_id: "my-gateway-default",
            channel_id: "ch-1",
            lane_id: "lane-jp",
            upstream_model: "upstream-added",
          },
        ],
        [{ model_id: "m-added", display_name: "m-added" }],
      ),
    );
    const bulk = recordingMock({ applied: true, restart_required: false, kernel_reload: false });
    registerMock("model_bulk_update", bulk.handler);

    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");

    // 换成直连 → lane 必须是 null（未选 = 直连）
    await chooseOption("pw-model-lane", "直连");
    await waitFor(() => expect(bulk.calls).toHaveLength(1));
    expect(bulk.calls[0]).toEqual({ ids: ["m-added"], patch: { lane: null } });
  });

  it("pw-cancel / pw-finish 都能关掉向导（cancel 一次命令都不发）", async () => {
    const { provider } = registerUpsertMocks();

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-cancel"));
    // MUI 的关闭过渡结束后内容才卸载，所以这里等一下（不是断言放宽）
    await waitFor(() => expect(screen.queryByTestId("provider-wizard-step1")).toBeNull());
    expect(provider.calls).toHaveLength(0);

    // 换一棵干净的树（同一个 `screen` 里两次 render 会撞出两个同名入口）
    cleanup();
    await gotoStep2();
    fireEvent.click(screen.getByTestId("pw-finish"));
    await waitFor(() => expect(screen.queryByTestId("provider-wizard-step2")).toBeNull());
  });
});

// ================================================================ ④ 扫描与添加

describe("[docs/19 §1.2] 一键扫描：结果只来自后端", () => {
  const PREVIEW = {
    preview_token: "pv-1",
    models: [
      { upstream_model: "deepseek-v4.1flash", suggested_id: "deepseek-v4.1flash", exists: false },
      { upstream_model: "gpt-4o-mini", suggested_id: "gpt-4o-mini", exists: true },
    ],
    skipped: [{ upstream_model: "bad model", reason: "名称不合法" }],
  };

  function registerScanMocks(preview: unknown = PREVIEW) {
    registerUpsertMocks();
    const scan = recordingMock(preview);
    const apply = recordingMock({ applied: true, restart_required: false, kernel_reload: false });
    registerMock("model_import_from_provider", scan.handler);
    registerMock("model_apply_import", apply.handler);
    return { scan, apply };
  }

  it("pw-scan 真调 model_import_from_provider；勾选后 pw-scan-add 真调 model_apply_import", async () => {
    const { scan, apply } = registerScanMocks();

    await gotoStep2();
    fireEvent.click(screen.getByTestId("pw-scan"));

    await waitFor(() => expect(scan.calls).toHaveLength(1));
    // 入参 snake_case + 真的是这个服务商
    expect(scan.calls[0]!["provider_id"]).toBe("my-gateway");

    const rows = await screen.findAllByTestId("pw-scan-row");
    expect(rows).toHaveLength(2);
    // exists=true 的行标出来，且默认不勾
    expect(rows[1]!.textContent).toContain("已存在");
    expect(within(rows[1]!).getByTestId("pw-scan-check").querySelector("input")?.checked).toBe(false);
    expect(within(rows[0]!).getByTestId("pw-scan-check").querySelector("input")?.checked).toBe(false);

    // skipped 如实显示条数
    expect(screen.getByTestId("pw-scan-result").textContent).toContain("跳过 1 条");

    // 全选：只勾 exists=false 的行
    fireEvent.click(screen.getByTestId("pw-scan-select-all"));
    expect(within(rows[0]!).getByTestId("pw-scan-check").querySelector("input")?.checked).toBe(true);
    expect(within(rows[1]!).getByTestId("pw-scan-check").querySelector("input")?.checked).toBe(false);

    fireEvent.click(screen.getByTestId("pw-scan-add"));
    await waitFor(() => expect(apply.calls).toHaveLength(1));
    expect(apply.calls[0]).toEqual({
      preview_token: "pv-1",
      selected: ["deepseek-v4.1flash"],
    });

    // 添加完说清楚加了几个（后端返回 applied=true）
    expect((await screen.findByTestId("pw-scan-result")).textContent).toContain("已添加 1 个模型");
  });

  it("空结果 → pw-scan-empty 说「上游没有返回任何模型」，不编数据", async () => {
    registerScanMocks({ preview_token: "pv-empty", models: [], skipped: [] });

    await gotoStep2();
    fireEvent.click(screen.getByTestId("pw-scan"));

    expect((await screen.findByTestId("pw-scan-empty")).textContent).toContain(
      "上游没有返回任何模型",
    );
    expect(screen.queryByTestId("pw-scan-row")).toBeNull();
  });

  it("扫描失败 → pw-scan-error 显示后端原文（含 TA 码）", async () => {
    registerUpsertMocks();
    registerMock("model_import_from_provider", () => {
      throw new TokenAgencyError({ code: "TA-1004", message: "预览已过期，请重新扫描" });
    });

    await gotoStep2();
    fireEvent.click(screen.getByTestId("pw-scan"));

    const error = await screen.findByTestId("pw-scan-error");
    expect(error.textContent).toContain("TA-1004");
    expect(error.textContent).toContain("预览已过期，请重新扫描");
  });

  it("只有真的 pending 才显示 pw-scanning 的文案；结果回来才出现 pw-scan-result", async () => {
    registerUpsertMocks();
    let resolve!: (value: unknown) => void;
    registerMock(
      "model_import_from_provider",
      () => new Promise((r) => { resolve = r; }),
    );

    await gotoStep2();
    expect(screen.queryByText("正在扫描上游模型")).toBeNull();

    fireEvent.click(screen.getByTestId("pw-scan"));
    expect(await screen.findByText("正在扫描上游模型")).toBeTruthy();
    expect(screen.queryByTestId("pw-scan-result")).toBeNull();

    await act(async () => {
      resolve(PREVIEW);
    });
    expect(await screen.findByTestId("pw-scan-result")).toBeTruthy();
    expect(screen.queryByText("正在扫描上游模型")).toBeNull();
  });
});

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
import { resetWizardSession, validateStep1 } from "@/pages/providers/providerWizardModel";
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

/**
 * 读「已添加的模型」行里那条线路的展示文案。
 *
 * [docs/21 §4.3] 之后它不是下拉而是**只读展示**（用户原话"线路设置不能点进服务商里面去修改"），
 * 所以断言的是文案而不是 combobox 的值。原来的 `selectTextOf` / `chooseOption` 两个下拉 helper
 * 随这次改动一起删掉了 —— 留着没人用的 helper 只会让下一个读代码的人以为还有下拉可点。
 */
function laneTextOf(row?: HTMLElement): string {
  const scope = row ?? screen.getAllByTestId("pw-model-row")[0]!;
  return (within(scope).getByTestId("pw-model-lane-name").textContent ?? "").trim();
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

// ================================================================ ⓪ 第 1 步的本地预检（纯函数）

/**
 * 预检必须认识"**本会话已经落库的那一份**"（[docs/19 §1.1 / §1.3]）。
 *
 * `pw-test` 是**先落库再探测**的，落库之后明文从草稿里被抹掉、`provider_list` 里出现了
 * 自己刚建的那一份。预检看不见这两件事时，「测试连接 → 下一步」这条明文路径会被自己
 * 刚建的东西挡住（"名称已经被占用" + "密钥必填"），永远进不了第 2 步 —— 真机上抓到过。
 */
describe("[docs/19 §1.1] 第 1 步预检要区分'本会话已落库的那一份'与'别人的东西'", () => {
  const draft = {
    name: "My Gateway",
    protocol: "openai",
    baseUrl: "https://gw.example.com/v1",
    key: "",
  } as const;
  const saved = { providerId: "my-gateway", name: "My Gateway", secretStored: true };

  it("还没落库：空 key 必填、撞上现有名字要拦下", () => {
    const issues = validateStep1({ ...draft }, { ids: ["my-gateway"], names: ["My Gateway"] });
    expect(issues.map((issue) => issue.field).sort()).toEqual(["key", "name"]);
  });

  it("已落库且密钥已存：同一个 id/名字不算重名，空 key = 沿用已存的那把", () => {
    const issues = validateStep1(
      { ...draft },
      { ids: ["my-gateway"], names: ["My Gateway"] },
      saved,
    );
    expect(issues).toEqual([]);
  });

  it("已落库，但改成了**别人**的名字：照样拦下（不能顺手把重名放过去）", () => {
    const issues = validateStep1(
      { ...draft, name: "字节方舟" },
      { ids: ["p-bytedance"], names: ["字节方舟"] },
      saved,
    );
    expect(issues.map((issue) => issue.field)).toEqual(["name"]);
  });
});

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

  it("点过 pw-test 之后 pw-next 仍然能进第 2 步：自己建的那份不算重名、已存的密钥不必重填", async () => {
    // 真 `provider_upsert` 之后清单里就会有它 —— 落库式探测的副作用，必须模拟出来
    const listed: Record<string, unknown>[] = [
      {
        id: "p-bytedance",
        name: "字节方舟",
        protocol: "openai",
        base_url: "https://ark.example/v3",
        enabled: true,
      },
    ];
    registerMock("provider_list", () => listed);
    const provider = recordingMock(undefined);
    registerMock("provider_upsert", (args) => {
      provider.handler(args);
      const payload = args!["provider"] as Record<string, unknown>;
      listed.push({ ...payload, enabled: true });
      return payload;
    });
    registerMock("credential_upsert", () => ({}));
    registerMock("credential_set_secret", () => undefined);
    registerMock("provider_test", () => ({
      ok: true,
      status: 200,
      latency_ms: 12,
      probe: "models",
      note: null,
    }));

    await openWizard();
    fillStep1();
    fireEvent.click(screen.getByTestId("pw-test"));
    await screen.findByTestId("pw-test-result");

    // 明文已经抹掉（纪律），而清单里已经有自己刚建的那一份
    expect(inputOf("pw-key").value).toBe("");
    expect(listed).toHaveLength(2);

    // 这一步以前是死的：预检会同时报「名称被占用」+「密钥必填」
    fireEvent.click(screen.getByTestId("pw-next"));
    await screen.findByTestId("provider-wizard-step2");
    expect(screen.queryByText(/已经被占用/)).toBeNull();

    // 两次落库用的是**同一个 id**：再保存一次不该造出第二个服务商
    const ids = provider.calls.map(
      (call) => (call["provider"] as Record<string, unknown>)["id"],
    );
    expect(ids).toEqual(["my-gateway", "my-gateway"]);
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

  it("[docs/21 §4.3] 第 2 步的线路列是**只读展示**：读出的值按 channel → Model.default_lane → net.default_lane → 直连", async () => {
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
    expect(laneTextOf()).toBe("直连");

    cleanup();
    resetWizardSession();

    // ② 第 1 步（在 /network 上）选了 lane-jp → 新模型默认继承它
    registerMock("config_get", () => ({ net: { default_lane: "lane-jp" } }));
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    await waitFor(() => expect(laneTextOf()).toContain("日本"));

    cleanup();
    resetWizardSession();

    // ③ channel 自己已经有 lane → 以后端为准
    cellLane = "lane-jp";
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    expect(laneTextOf()).toContain("日本");

    cleanup();
    resetWizardSession();

    // ④ 还没有 channel、但模型级默认 lane 已经存下来了（`Model.default_lane`）→ 显示它
    cellLane = null;
    registerMock("config_get", () => ({ net: { default_lane: null } }));
    registerMock("model_list", () => [
      { model: { id: "m-added", aliases: [], display: { group: null, order: 0 }, expose: true, default_lane: "lane-jp" }, channel_count: 0, upstream_names: [], priced: false },
    ]);
    await gotoStep2();
    await screen.findAllByTestId("pw-model-row");
    expect(laneTextOf()).toContain("日本");
  });

  it("[docs/21 §4.3] 向导里**改不了**线路：没有下拉、一次 model_bulk_update 都不发，并指路到通道页", async () => {
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
    const row = (await screen.findAllByTestId("pw-model-row"))[0]!;

    // 用户原话：「线路设置不能点进服务商里面去修改」——这一列必须只剩只读展示
    const laneBox = within(row).getByTestId("pw-model-lane");
    expect(within(laneBox).queryByRole("combobox")).toBeNull();
    expect(within(laneBox).queryByRole("textbox")).toBeNull();
    expect(within(row).getByTestId("pw-model-lane-name").textContent).toContain("日本");
    expect(within(row).getByTestId("pw-model-lane-readonly").textContent).not.toBe("");

    // 点它、敲它都不该产生任何落库调用（旧代码里这是个可改的下拉）
    fireEvent.click(laneBox);
    fireEvent.keyDown(laneBox, { key: "ArrowDown" });
    expect(bulk.calls).toHaveLength(0);
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
    const apply = recordingMock({
      applied: true,
      restart_required: false,
      kernel_reload: false,
      channels_created: 1,
    });
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
    // [docs/20 §3]：默认 lane 也要一起发出去（基准 mock 的 net.default_lane = lane-direct）
    expect(apply.calls[0]).toEqual({
      preview_token: "pv-1",
      selected: ["deepseek-v4.1flash"],
      lane: "lane-direct",
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

// ================================================================ ⑤ [docs/20 §3] 添加之后立刻能看、每行能改线路

/** 一份只有一条待添加模型的扫描结果（tid 与真机一致）。 */
const LANE_PREVIEW = {
  preview_token: "pv-lane",
  models: [{ upstream_model: "m-new", suggested_id: "m-new", exists: false }],
  skipped: [],
};

/** 一条已被这个服务商挂上的 cell（`channel_matrix` 的形状）。 */
function matrixCell(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    model_id: "m-x",
    credential_id: "my-gateway-default",
    channel_id: "ch-x",
    lane_id: null,
    upstream_model: "up-x",
    ...overrides,
  };
}

/**
 * 「后端真的建了通道」的替身。
 *
 * 为什么必须由替身建：第 2 步上面那块是**从 `channel_matrix` 派生的**（[docs/20 §0]），
 * 前端不许自己往清单里塞行；所以"添加之后看得到"这件事，只能靠替身在 apply 之后
 * 真的往矩阵里加一条 —— 否则测的就是前端编的假数据。
 *
 * 改线路也一样：`model_bulk_update` 的替身把 lane 写回矩阵，界面上的值只有**重取**之后
 * 才会变（这正是"值要真的变回来"那条要求）。
 */
function registerMatrixBackend() {
  registerUpsertMocks();
  registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
  const cells: Record<string, unknown>[] = [];
  registerMock("channel_matrix", () => matrixView(cells, []));
  const bulkCalls: Record<string, unknown>[] = [];
  registerMock("model_bulk_update", (args) => {
    bulkCalls.push(args ?? {});
    const ids = (args?.["ids"] ?? []) as string[];
    const patch = (args?.["patch"] ?? {}) as Record<string, unknown>;
    for (const cell of cells) {
      if (ids.includes(cell["model_id"] as string)) cell["lane_id"] = patch["lane"] ?? null;
    }
    return { applied: true, restart_required: false, kernel_reload: false, channels_created: 0 };
  });
  return { cells, bulkCalls };
}

describe("[docs/20 §3] 添加之后上面立刻看得到，且每一行都能改线路", () => {
  it("model_apply_import 带上默认 lane；后端真建了通道 → 行立刻出现；channels_created 如实显示", async () => {
    const { cells } = registerMatrixBackend();
    registerMock("model_import_from_provider", () => LANE_PREVIEW);
    const applyCalls: Record<string, unknown>[] = [];
    registerMock("model_apply_import", (args) => {
      applyCalls.push(args ?? {});
      // 后端建通道（[docs/20 §1.1]）：这条之后矩阵里才有它
      cells.push(
        matrixCell({
          model_id: "m-new",
          channel_id: "ch-my-gateway-my-gateway-default-m-new",
          lane_id: (args?.["lane"] as string | null) ?? null,
        }),
      );
      return { applied: true, restart_required: false, kernel_reload: false, channels_created: 1 };
    });

    await gotoStep2();
    fireEvent.click(screen.getByTestId("pw-scan"));
    fireEvent.click(await screen.findByTestId("pw-scan-select-all"));
    fireEvent.click(screen.getByTestId("pw-scan-add"));

    // ① 入参带上当前默认 lane
    await waitFor(() => expect(applyCalls).toHaveLength(1));
    expect(applyCalls[0]).toEqual({
      preview_token: "pv-lane",
      selected: ["m-new"],
      lane: "lane-direct",
    });

    // ② 上面那块立刻有东西（不是点完添加、上面永远空着）
    const row = await screen.findByTestId("pw-model-row");
    expect(row.textContent).toContain("m-new");

    // ③ 后端说新建了几条线路就显示几条（不编数字、不写死）
    expect(screen.getByTestId("pw-channels-created").textContent).toContain("1");
  });

  it("[docs/21 §4.3] 只读展示也永不为空：没配过就是「直连」，并在旁边指路到通道页", async () => {
    registerUpsertMocks();
    registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
    // 第 1 步也没选过默认代理：链路末端就是直连
    registerMock("config_get", () => ({ net: { default_lane: null } }));
    registerMock("channel_matrix", () => matrixView([matrixCell()]));

    await gotoStep2();
    const row = (await screen.findAllByTestId("pw-model-row"))[0]!;

    // 旧代码里这里是个空下拉（用户看到的"未配置"）；现在必须是可读的名字 + 指路文案
    expect(within(row).getByTestId("pw-model-lane-name").textContent).toBe("直连");
    expect(within(row).getByTestId("pw-model-lane-readonly").textContent).not.toBe("");
    expect(within(row).queryByRole("combobox")).toBeNull();
  });

  it("[docs/21 §4.3] 线路的编辑入口不在向导里：旧代码那条 model_bulk_update 路径必须消失", async () => {
    const { cells, bulkCalls } = registerMatrixBackend();
    cells.push(matrixCell());
    registerMock("config_get", () => ({ net: { default_lane: null } }));

    await gotoStep2();
    const row = (await screen.findAllByTestId("pw-model-row"))[0]!;

    // 行内只剩只读展示：点它、敲它都不会产生任何落库调用（旧代码这里是可改的下拉）
    const laneBox = within(row).getByTestId("pw-model-lane");
    fireEvent.click(laneBox);
    fireEvent.keyDown(laneBox, { key: "ArrowDown" });
    expect(bulkCalls).toHaveLength(0);
    expect(laneTextOf(row)).toBe("直连");
  });
});

// ================================================================ ⑤ [docs/22] 长解释收进问号圆圈

/**
 * [docs/22]（用户第 74 轮：「简介好长，好多字……做成一个问号加一个圆圈」）的验收。
 *
 * 向导这一块的边界特别要紧：**第 2 步线路那一行已经明确"只读 + 指路到通道页"**
 * （[docs/21 §4.3]），那句 `pw-model-lane-readonly` 是用户必须一眼看到的约束，
 * 不是可藏起来的解释 —— 收紧它等于把"这里改不了"变成"要悬停才知道这里改不了"。
 */
describe("[docs/22] 长解释收进问号圆圈，红线一律留在版面上", () => {
  it("第 2 步：modelLaneDefaultHint 收进 pw-added-models-help，正文不再内联", async () => {
    await gotoStep2();

    // 先钉"不再内联"（旧代码上这一条先红：那句灰字正挂在区块标题下面）
    expect(screen.queryByText(/没选过就是直连/)).toBeNull();

    const help = screen.getByTestId("pw-added-models-help");
    // 正文一个字都没丢：`aria-label` 就是全文
    expect(help.getAttribute("aria-label")).toBe("没选过就是直连（继承链末端是直连）");
    expect(screen.getAllByTestId("pw-added-models-help")).toHaveLength(1);

    // 区块标题与**空态**都还在版面上（空态是结论，不是解释：[docs/22 §2 第 1 条]）
    expect(screen.getByText("已添加的模型")).toBeTruthy();
    expect(screen.getByTestId("pw-added-models-empty").textContent).toBe(
      "这个服务商下还没有模型",
    );
    expect(help.getAttribute("aria-label") ?? "").not.toContain("这个服务商下还没有模型");
  });

  it("第 2 步：线路的只读指路文案 pw-model-lane-readonly 仍然内联（不许收进问号）", async () => {
    registerUpsertMocks();
    registerMock("credential_list", () => [credentialView("my-gateway-default", "my-gateway")]);
    registerMock("channel_matrix", () =>
      matrixView([
        {
          model_id: "m-added",
          credential_id: "my-gateway-default",
          channel_id: "ch-1",
          lane_id: "lane-jp",
          upstream_model: "upstream-added",
        },
      ]),
    );

    await gotoStep2();
    const row = (await screen.findAllByTestId("pw-model-row"))[0]!;

    const readonly = within(row).getByTestId("pw-model-lane-readonly");
    expect(readonly.textContent).toContain("线路只在通道页改");
    // 这一行只有那一个区块级的问号，行内没有第二个
    expect(within(row).queryByTestId("pw-model-lane-help")).toBeNull();
    expect(screen.getAllByTestId("pw-added-models-help")).toHaveLength(1);
  });

  it("第 1 步红线仍内联：非必填说明、密钥字段语义、按钮文案，且第 1 步没有问号", async () => {
    await openWizard();
    const step1 = within(screen.getByTestId("provider-wizard-step1"));

    // §2 第 3 条：必填 / 可选这类提示留在版面上
    expect(step1.getByText("测试连接不是必须的，可以先跳过")).toBeTruthy();
    // 密钥字段的语义（只写密钥仓库、永不回显）是字段级提示，留在字段下面
    expect(step1.getByText("密钥（只写入密钥仓库，界面上永不回显）")).toBeTruthy();
    // 第 1 步里**一个问号都没有**：它里面的长句全是红线（校验 / 后果 / 按钮）
    expect(step1.queryAllByTestId(/-help$/)).toHaveLength(0);
  });

  it("第 2 步的落库后果、扫描结果里的数与后端原文仍然一眼可见", async () => {
    await gotoStep2();

    // §2 第 4 条：关掉向导不会撤销已经写入的配置 —— 破坏性/持久化后果
    expect(screen.getByTestId("pw-saved-notice").textContent).toContain(
      "服务商与凭据已经写入配置；关掉向导不会撤销它们",
    );
    // §2 第 6 条：按钮文案永远留在按钮上
    expect(screen.getByTestId("pw-scan").textContent).toBe("一键扫描");
    expect(screen.getByTestId("pw-finish").textContent).toBe("完成");
    expect(screen.getByTestId("pw-cancel").textContent).toBe("取消");
    // 页面上任何一个问号的正文都不是这几句红线
    for (const help of screen.queryAllByTestId(/-help$/)) {
      const label = help.getAttribute("aria-label") ?? "";
      expect(label).not.toContain("关掉向导不会撤销");
      expect(label).not.toContain("一键扫描");
    }
  });
});

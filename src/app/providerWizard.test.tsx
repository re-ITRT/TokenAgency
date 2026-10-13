// @vitest-environment jsdom
/**
 * `/providers` 的**两步向导**（流程 A）验收。
 *
 * 这一份测的是**用户点得到的那条路**，所以每一条都尽量按真机 E2E 的方式驱动：
 * 开的入口是 `providers-add-open` → 第 1 步 `provider-wizard-step1` → `pw-*` →
 * 第 2 步 `provider-wizard-step2`。
 *
 * 三条硬纪律在这里被钉住：
 *
 * 1. **红先绿后**：控件存在 → 真输入 → 真的调了该命令 → 入参形状正确 → 成功后显示**后端真值**。
 * 2. **落库时机说清楚**：`pw-next` 与 `pw-test`（服务商还没落库时）都会真的写配置，
 *    并且必须在界面上说出这件事（`pw-saved-notice`）—— 绝不静默产生持久化副作用。
 * 3. **明文只进密钥仓库**：`pw-key` 的明文只允许出现在 `credential_set_secret` 的入参里；
 *    这里用一个假金丝雀值证明它不在服务商/凭据载荷里，也不在页面上（真机上那是密钥泄漏）。
 *
 * 命令接线：`provider_upsert` → `credential_upsert`（id = `<provider>-default`）
 * → 填了密钥才 `credential_set_secret`；扫描 `model_import_from_provider`；
 * **创建线路** `model_apply_import`。
 *
 * ** 第 2 步建的是「线路」，不是「模型」**（owner 原话："创建完服务商和线路之后，
 * 模型是不自动创建的，是等待用户去分配的"）。这一份测试因此多钉三件事：
 *
 * * 第 2 步的标题 / 说明 / 按钮 / 结果区**不再说"模型会被创建"**（`pw-step2-why`、
 *   `pw-scan-add`、`pw-channels-created`），并且给出 `pw-goto-models`（去模型页分配线路）；
 * * 上面那块是**本服务商已有的线路**（`channel_matrix` 派生，每行 = 上游模型名 + 线路 id +
 *   **只读**的网络代理），testid 用线路语义 `pw-line-lane*`；
 * * **走完整个第 2 步，`model_upsert` 的调用次数必须为 0**（见文末第 ⑥ 节）。
 *
 * 这三条命令由另一个写者在 Rust 注册（`collect_commands!`），所以**测试用 `registerMock` 驱动**，
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
import { i18n, type I18nKey } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

beforeEach(() => {
  registerBaselineMocks();
  // 草稿活在模块级 store 里（要求"去 /network 再回来草稿还在"）：
  // 每条用例从干净状态开始，否则上一条的草稿会串到下一条。
  resetWizardSession();
});

afterEach(() => {
  cleanup();
});

// ================================================================ 工具

/** 明文金丝雀：它出现在这里，正是为了证明它**不会**出现在服务商/凭据载荷与页面上。 */
const KEY = "sk-plaintext-canary-0001"; // ta-allow-fake-secret —— 假值

/**
 * 第 2 步新用的文案键 → 文本。
 *
 * 这 10 个键由主管补进 `src/shared/i18n/**`（本任务的写作用域**不含 i18n**）。键还没落地时
 * `t(key)` 原样返回键名 —— 所以这里**不写死中文**，而是问 i18n 要同一份资源：
 * 组件与断言取的是同一个值（键补齐后这几条断言自动变成"用户真的看到那句中文"，一个字都不用改）。
 *
 * 顺带好处：这份测试不会把"键名"和"文案"绑成两个真相源 —— 改文案不许让测试变红。
 */
function copy(key: string): string {
  return i18n.t(key as I18nKey);
}

/** 带 `{{count}}` 插值的键（`linesCreated`）。 */
function copyCount(key: string, count: number): string {
  return i18n.t(key as I18nKey, { count });
}

function inputOf(testId: string): HTMLInputElement {
  const input = screen.getByTestId(testId).querySelector("input");
  if (input === null) throw new Error(`[data-testid="${testId}"] 里没有 input`);
  return input;
}

function typeInto(testId: string, value: string): void {
  fireEvent.change(inputOf(testId), { target: { value } });
}

/**
 * 读「本服务商已有的线路」行里那条代理的展示文案。
 *
 * 之后它不是下拉而是**只读展示**（代理是线路的属性，编辑口在服务商详情页的
 * 「编辑线路」里；向导里**不许**出现可写的代理下拉），所以断言的是文案而不是 combobox 的值。
 * 旧的 `selectTextOf` / `chooseOption` 两个下拉 helper 随那次改动一起删掉了 ——
 * 留着没人用的 helper 只会让下一个读代码的人以为还有下拉可点。
 */
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

/**
 * `/models` 的替身：只为了断言"`pw-goto-models` 真的把人送到模型页"
 * （：线路建完之后，模型要用户自己去**分配**线路）。
 */
function ModelsStub() {
  const location = useLocation();
  return <span data-testid="models-stub-path">{location.pathname}</span>;
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
            <Route path="/models" element={<ModelsStub />} />
          </Routes>
        </MemoryRouter>
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

// ---------------------------------------------------------------- 后端夹具

/** `credential_list` 的一行（`CredentialView` 形状）：把凭据挂到某个服务商上。 */
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
 * 预检必须认识"**本会话已经落库的那一份**"。
 *
 * `pw-test` 是**先落库再探测**的，落库之后明文从草稿里被抹掉、`provider_list` 里出现了
 * 自己刚建的那一份。预检看不见这两件事时，「测试连接 → 下一步」这条明文路径会被自己
 * 刚建的东西挡住（"名称已经被占用" + "密钥必填"），永远进不了第 2 步 —— 真机上抓到过。
 */
describe(" 第 1 步预检要区分'本会话已落库的那一份'与'别人的东西'", () => {
  const draft = {
    name: "My Gateway",
    protocol: "openai",
    baseUrl: "https://gw.example.com/v1",
    key: "",
    lane: "direct:",
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

describe(" 第 1 步：服务商信息", () => {
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

describe(" pw-test：先真落库（并说出来），再真调 provider_test", () => {
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

// ================================================================ ③ 第 2 步：结构与「本服务商已有的线路」

// ================================================================ ③ 第 2 步：一个列表

/**
 * 用户第 82 轮的重构原话：
 * 「添加服务商默认添加所有线路，然后通过滑块来'启用'和'关闭'。因此，可以直接写在在一个列表里面，
 *  没有添加的线路也可以设置'网络代理'，只是有个滑块控制是否可见」
 *
 * 所以第 2 步 = **详情页那一节**（同一个 `LinesSection`）：扫描（记录目录 + 把清单里的每条线路
 * 都建出来，**默认关闭**）+ 每行（上游原文 / 网络代理 / 启用滑块）+ 全选。
 * 这里钉住"一个列表"这件事本身，以及**旧的"勾一下才创建"控件一个都不许再出现**。
 */
describe(" 第 2 步：一个列表（扫描即全部建出、滑块控制启停、每行都能设代理）", () => {
  /** 一条线路（`channel_list` 的一行）：字段给全，用来钉"整条覆盖写不丢字段"。 */
  function channelLine(overrides: Record<string, unknown> = {}): Record<string, unknown> {
    return {
      id: "ch-my-gateway-my-gateway-default-ta-e2e-node-hk",
      model: "",
      provider: "my-gateway",
      credential: "my-gateway-default",
      lane: null,
      upstream_model: "ta-e2e-node-hk",
      transform: {
        path: null,
        headers_add: { "x-keep": "1" },
        headers_remove: [],
        headers_override: {},
        body_patch: {},
      },
      priority: 100,
      weight: 10,
      tags: ["keep-me"],
      enabled: false,
      capabilities: {
        vision: true,
        tools: false,
        reasoning: false,
        context_tokens: 128000,
        modalities: ["text"],
      },
      ...overrides,
    };
  }

  /** 目录里的一行（`provider_model_catalog`）：上游原文从这里来。 */
  function catalogEntry(overrides: Record<string, unknown> = {}): Record<string, unknown> {
    return {
      provider_id: "my-gateway",
      upstream_model: "ta-e2e-node-hk",
      suggested_id: "ta-e2e-node-hk",
      seen_in_last_scan: true,
      first_seen_ms: 1,
      last_seen_ms: 2,
      added: true,
      channel_ids: ["ch-my-gateway-my-gateway-default-ta-e2e-node-hk"],
      enabled: false,
      raw: { id: "ta-e2e-node-hk", object: "model", owned_by: "vendor-x", created: 1712345678 },
      ...overrides,
    };
  }

  /** 扫描/建线路/列表的最常见一套替身；返回几个 recorder 供断言。 */
  function registerStep2Backend(
    lineOverrides: Record<string, unknown>[] = [
      {},
      { id: "ch-2", upstream_model: "m-2", enabled: true },
    ],
  ) {
    registerUpsertMocks();
    const scan = recordingMock({
      preview_token: "pv-1",
      models: [
        { upstream_model: "ta-e2e-node-hk", suggested_id: "ta-e2e-node-hk", exists: false },
        { upstream_model: "m-2", suggested_id: "m-2", exists: false },
      ],
      skipped: [],
    });
    registerMock("model_import_from_provider", scan.handler);
    const createAll = recordingMock({ created: 2, total: 2 });
    registerMock("provider_lines_create_all", createAll.handler);
    const lines = lineOverrides.map((over) => channelLine(over));
    registerMock("channel_list", () => lines);
    registerMock("provider_model_catalog", () => [
      catalogEntry(),
      catalogEntry({ upstream_model: "m-2", suggested_id: "m-2", channel_ids: ["ch-2"], raw: {} }),
    ]);
    registerMock("config_get", () => ({ net: { default_lane: null } }));
    return { scan, createAll, lines };
  }

  it("pw-next 真的落库并进入第 2 步：保存说明 / 语义说明 / 扫描 / 一个列表 / 指路 / 完成", async () => {
    registerStep2Backend();

    await gotoStep2();

    // 已落库这件事必须说出来（关掉向导不会撤销）
    expect(screen.getByTestId("pw-saved-notice").textContent).toContain(
      copy("providers.wizard.savedNotice"),
    );
    // 这一节建的是**线路**，模型要用户自己去分配
    expect(screen.getByTestId("pw-step2-why").textContent).toBe(copy("providers.wizard.step2.why"));
    expect(screen.getByTestId("pw-step2-scroll")).toBeTruthy();
    // 扫描按钮（历史 testid `pw-scan`）+ 完成 / 指路
    expect(screen.getByTestId("pw-scan").textContent).toBe(copy("providers.wizard.scan"));
    // **主操作不参与滚动**：长列表时「完成」必须还在（所以它放在对话框的
    // 固定按钮区，不在 `pw-step2-scroll` 那个滚动容器里）。
    expect(screen.getByTestId("pw-finish").textContent).toBe(copy("providers.wizard.finish"));
    expect(within(screen.getByTestId("pw-step2-scroll")).queryByTestId("pw-finish")).toBeNull();
    expect(screen.getByTestId("pw-cancel").textContent).toBe(copy("common.cancel"));
    expect(screen.getByTestId("pw-goto-models")).toBeTruthy();
    expect(screen.getByTestId("pw-scan-goto-detail")).toBeTruthy();
    // **一个列表**：行就是这家服务商的线路（`channel_list`）
    expect((await screen.findAllByTestId("providers-line-row")).length).toBe(2);

    // 旧的"勾一下才创建"三步法控件**一个都不许再出现**
    expect(screen.queryByTestId("pw-scan-check")).toBeNull();
    expect(screen.queryByTestId("pw-scan-add")).toBeNull();
    expect(screen.queryByTestId("pw-scan-select-all")).toBeNull();
    expect(screen.queryByTestId("pw-scan-row")).toBeNull();
  });

  it(" 扫描 = 记录目录 + 把清单里的每条线路都建出来（两条命令，入参逐字）", async () => {
    const { scan, createAll } = registerStep2Backend();

    await gotoStep2();

    // 进第 2 步就自动扫了一次
    await waitFor(() => expect(scan.calls).toHaveLength(1));
    expect(scan.calls[0]).toEqual({ provider_id: "my-gateway", prefix_filter: undefined });
    await waitFor(() => expect(createAll.calls).toHaveLength(1));
    // **不带 lane**：代理只属于服务商（同一服务商一个 URL），向导不写线路级代理
    expect(createAll.calls[0]).toEqual({ provider_id: "my-gateway", lane: null });

    // 「重新扫描」再走一轮（真命令，不是本地拼）
    fireEvent.click(screen.getByTestId("pw-scan"));
    await waitFor(() => expect(scan.calls).toHaveLength(2));
    await waitFor(() => expect(createAll.calls).toHaveLength(2));

    // 数字如实说：扫到 2 条、本次新建 2 条、目录里共 2 条
    const notice = await screen.findByTestId("providers-model-notice");
    expect(notice.textContent).toContain("2");
  });

  it(" 每行 = 上游原文 + 网络代理 + 启用滑块；**未启用**的行也能设代理", async () => {
    registerStep2Backend([{ enabled: false }, { id: "ch-2", upstream_model: "m-2", enabled: true }]);

    await gotoStep2();

    const rows = await screen.findAllByTestId("providers-line-row");
    expect(rows).toHaveLength(2);
    const offRow = rows.find((row) => (row.textContent ?? "").includes("ta-e2e-node-hk"))!;
    const onRow = rows.find((row) => (row.textContent ?? "").includes("m-2"))!;

    // 滑块反映库里的真实状态（默认关闭 = 用户要的"默认关闭"）
    expect(
      (within(offRow).getByTestId("providers-line-enabled").querySelector("input") as HTMLInputElement)
        .checked,
    ).toBe(false);
    expect(
      (within(onRow).getByTestId("providers-line-enabled").querySelector("input") as HTMLInputElement)
        .checked,
    ).toBe(true);
    // **没有添加的线路（未启用）也能设网络代理** —— 用户原话
    // **线路不再有自己的代理**（出口只由服务商决定）→ 行里没有代理控件
    expect(within(offRow).queryByTestId("providers-line-lane")).toBeNull();
    // **列表是清单，不是每条一张卡片**（用户原话："一条就是左边名字，
    // 右边选择滑块就好了"）：行里**不许**显示上游原文 / 线路 id / 优先级权重；它们都在
    // 点开的详情里。先证明"列表干净"，再点开证明"详情看得到"。
    expect(within(offRow).queryByTestId("providers-line-raw-json")).toBeNull();
    expect(within(offRow).queryByTestId("providers-line-id")).toBeNull();
    expect(within(offRow).queryByTestId("providers-line-priority")).toBeNull();

    fireEvent.click(within(offRow).getByTestId("providers-line-info"));
    const panel = await within(offRow).findByTestId("providers-line-info-panel");
    const rawJson = within(panel).getByTestId("providers-line-raw-json").textContent ?? "";
    expect(JSON.parse(rawJson)).toEqual({
      id: "ta-e2e-node-hk",
      object: "model",
      owned_by: "vendor-x",
      created: 1712345678,
    });
    expect(within(panel).getByTestId("providers-line-id")).toBeTruthy();
    // 再点一次收起（同时证明它是个开关，不是只进不出的抽屉）
    fireEvent.click(within(offRow).getByTestId("providers-line-info"));
    await waitFor(() =>
      expect(within(offRow).queryByTestId("providers-line-info-panel")).toBeNull(),
    );

    // 目录里记的是 `{}` → 详情里如实说"上游没给对象"，不画空 JSON 框
    fireEvent.click(within(onRow).getByTestId("providers-line-info"));
    const onPanel = await within(onRow).findByTestId("providers-line-info-panel");
    expect(within(onPanel).queryByTestId("providers-line-raw-json")).toBeNull();
    expect(within(onPanel).getByTestId("providers-line-raw-empty").textContent).toBeTruthy();
  });


  it(" 向导里没有任何路径会创建模型；也没有删除线路的入口", async () => {
    const { scan, createAll } = registerStep2Backend();
    const upsertModel = recordingMock({});
    registerMock("model_upsert", upsertModel.handler);
    // 另一条会建 `Model` 的命令（"接入"）：向导里也不许出现
    const attach = recordingMock({});
    registerMock("provider_model_attach", attach.handler);

    await gotoStep2();
    await waitFor(() => expect(scan.calls).toHaveLength(1));
    await waitFor(() => expect(createAll.calls).toHaveLength(1));
    fireEvent.click(screen.getByTestId("pw-scan"));
    await waitFor(() => expect(scan.calls).toHaveLength(2));

    expect(upsertModel.calls).toHaveLength(0);
    expect(attach.calls).toHaveLength(0);
    // 向导是**新增**流程：不在这里删东西（删除走详情页 / 通道页）
    expect(screen.queryByTestId("providers-line-remove")).toBeNull();
  });

  it("空结果 → pw-scan-empty 说「上游没有返回任何模型」，不编数据", async () => {
    registerUpsertMocks();
    registerMock("model_import_from_provider", () => ({
      preview_token: "pv-empty",
      models: [],
      skipped: [],
    }));
    registerMock("provider_lines_create_all", () => ({ created: 0, total: 0 }));
    registerMock("channel_list", () => []);
    registerMock("provider_model_catalog", () => []);
    registerMock("config_get", () => ({ net: { default_lane: null } }));

    await gotoStep2();

    expect((await screen.findByTestId("pw-scan-empty")).textContent).toBe(
      copy("providers.wizard.scanEmpty"),
    );
  });

  it("扫描失败 → pw-scan-error 显示后端原文（含 TA 码）", async () => {
    registerUpsertMocks();
    registerMock("model_import_from_provider", () => {
      throw new TokenAgencyError({ code: "TA-2001", message: "上游连不上" });
    });
    registerMock("channel_list", () => []);
    registerMock("provider_model_catalog", () => []);
    registerMock("config_get", () => ({ net: { default_lane: null } }));

    await gotoStep2();

    const err = await screen.findByTestId("pw-scan-error");
    expect(err.textContent).toContain("TA-2001");
    expect(err.textContent).toContain("上游连不上");
  });

  it(" 进第 2 步就自动扫描：扫描期间只有真实的 pending 文案", async () => {
    registerUpsertMocks();
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    registerMock("model_import_from_provider", async () => {
      await gate;
      return { preview_token: "pv", models: [], skipped: [] };
    });
    registerMock("provider_lines_create_all", () => ({ created: 0, total: 0 }));
    registerMock("channel_list", () => []);
    registerMock("provider_model_catalog", () => []);
    registerMock("config_get", () => ({ net: { default_lane: null } }));

    await gotoStep2();

    // 真的在等后端才说"正在扫描"（无假进度）
    expect(screen.getByTestId("providers-model-scanning").textContent).toBe(
      copy("providers.catalog.rescanning"),
    );
    release?.();
    await screen.findByTestId("pw-scan-empty");
    expect(screen.queryByTestId("providers-model-scanning")).toBeNull();
  });

  it("pw-cancel / pw-finish 都能关掉向导（cancel 一次命令都不发）", async () => {
    const { scan, createAll } = registerStep2Backend();

    await gotoStep2();
    await waitFor(() => expect(scan.calls).toHaveLength(1));
    const afterScan = scan.calls.length;
    const afterCreate = createAll.calls.length;

    fireEvent.click(screen.getByTestId("pw-cancel"));
    // MUI 的 Dialog 退场过渡结束后才真正卸载
    await waitFor(() => expect(screen.queryByTestId("provider-wizard-step2")).toBeNull());
    expect(scan.calls).toHaveLength(afterScan);
    expect(createAll.calls).toHaveLength(afterCreate);
  });
});

// ================================================================ ⑥ 长解释收进问号圆圈

describe(" 长解释收进问号圆圈，红线一律留在版面上", () => {
  it("第 1 步红线仍内联：非必填说明、密钥字段语义、按钮文案；唯一问号是密钥那条", async () => {
    await openWizard();
    const step1 = within(screen.getByTestId("provider-wizard-step1"));

    // 第 1 步不再有"新建线路默认代理"（代理只属于服务商）→ 问号只剩密钥那一条。
    // 其余红线**内联**。
    expect(
      step1.getAllByTestId(/-help$/).map((el) => el.getAttribute("data-testid")),
    ).toEqual(["pw-key-multiple-help"]);
    const help = step1.getByTestId("pw-key-multiple-help").getAttribute("aria-label") ?? "";
    expect(help).toBe(copy("providers.key.multipleHint"));
    for (const el of step1.queryAllByTestId(/-help$/)) {
      const label = el.getAttribute("aria-label") ?? "";
      expect(label).not.toContain("测试连接不是必须的");
      expect(label).not.toContain("只写入密钥仓库");
    }
  });

  it("第 2 步的落库后果、扫描结果里的数与上游原文仍然一眼可见", async () => {
    registerUpsertMocks();
    registerMock("model_import_from_provider", () => ({
      preview_token: "pv-1",
      models: [{ upstream_model: "m-1", suggested_id: "m-1", exists: false }],
      skipped: [{ upstream_model: "bad model", reason: "名称不合法" }],
    }));
    registerMock("provider_lines_create_all", () => ({ created: 1, total: 1 }));
    registerMock("channel_list", () => [
      {
        id: "ch-1",
        model: "",
        provider: "my-gateway",
        credential: "my-gateway-default",
        lane: null,
        upstream_model: "m-1",
        enabled: false,
        priority: 1,
        weight: 1,
        tags: [],
        transform: { path: null, headers_add: {}, headers_remove: [], headers_override: {}, body_patch: {} },
        capabilities: { vision: false, tools: false, reasoning: false, context_tokens: 0, modalities: [] },
      },
    ]);
    registerMock("provider_model_catalog", () => [
      {
        provider_id: "my-gateway",
        upstream_model: "m-1",
        suggested_id: "m-1",
        seen_in_last_scan: true,
        first_seen_ms: 1,
        last_seen_ms: 2,
        added: true,
        channel_ids: ["ch-1"],
        enabled: false,
        raw: { id: "m-1", object: "model" },
      },
    ]);
    registerMock("config_get", () => ({ net: { default_lane: null } }));

    await gotoStep2();

    // 落库后果（关掉向导不回滚）
    expect(screen.getByTestId("pw-saved-notice").textContent).toContain(
      copy("providers.wizard.savedNotice"),
    );
    // 按钮文案永远留在按钮上
    expect(screen.getByTestId("pw-scan").textContent).toBe(copy("providers.wizard.scan"));
    // **主操作不参与滚动**：长列表时「完成」必须还在（所以它放在对话框的
    // 固定按钮区，不在 `pw-step2-scroll` 那个滚动容器里）。
    expect(screen.getByTestId("pw-finish").textContent).toBe(copy("providers.wizard.finish"));
    expect(within(screen.getByTestId("pw-step2-scroll")).queryByTestId("pw-finish")).toBeNull();
    expect(screen.getByTestId("pw-cancel").textContent).toBe(copy("common.cancel"));
    // 扫到几条 + 跳过了几条都在版面上；上游原文在**点开的详情**里
    expect((await screen.findByTestId("providers-model-notice")).textContent).toContain("1");
    const skipped = screen.getByText(copyCount("providers.wizard.scanSkipped", 1));
    expect(skipped).toBeTruthy();
    const row = (await screen.findAllByTestId("providers-line-row"))[0]!;
    fireEvent.click(within(row).getByTestId("providers-line-info"));
    const panel = await within(row).findByTestId("providers-line-info-panel");
    expect(JSON.parse(within(panel).getByTestId("providers-line-raw-json").textContent ?? "{}")).toEqual({
      id: "m-1",
      object: "model",
    });
  });
});

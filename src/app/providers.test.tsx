// @vitest-environment jsdom
/**
 * 服务商页（[07 §3.1 列表 / §3.2 详情]）的验收。重点和看板、日志两份一样是**诚实**：
 *
 * 1. **两套后端形状都要读得出来**。冻结契约（`bindings.ts`）说 `provider_list` 返回
 *    `ProviderView[]`，而 Rust 实现（`src-tauri/src/commands/catalog.rs:53`）返回的是
 *    **完整的 `Provider[]`**（详情 `provider_get` 同理，且返回 `Option`）。两边字段名不同、
 *    各有对方没有的列 —— 页面必须按并集读（`src/pages/providers/model.ts`），
 *    **缺哪一列就显示"未知/—"，绝不填 0、也绝不拿别的列推断**。
 * 2. **没有数据给诚实空状态**：目录为空时说明"配置里就是没有"，不画一张 0 行的表；
 *    连接配置字段没返回时明说"后端没返回这一列"，不排一排空占位。
 * 3. **错误带 `TA-xxxx` 且不吞**：读服务商失败、读凭据失败都要各自显示出来 ——
 *    凭据读失败只该让"凭据数"变未知，不该把整张表打成错误。
 * 4. **没接线的能力不放按钮**：`provider_test` / `provider_echo` 的本轮状态以
 *    `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 为准（`commandSurface.guard.test.ts`
 *    机械检查调用方引用），所以这一份只断言**列表/详情页**上没有测试/编辑/删除按钮
 *    —— 两步向导里那个「连通性测试」按钮的接线由 `src/app/providerWizard.test.tsx` 负责。
 * 5. **添加向导（+）是真入口**：`provider_upsert` / `credential_upsert` /
 *    `credential_set_secret` 都已在注册表里，所以「添加服务商」（本轮已是**两步向导**，
 *    见 `providerWizard.test.tsx`）、「高级添加」（**原来的单步对话框**，字段 tid 不变）
 *    与「添加密钥」必须是能真填、真发命令、成功后回显**后端返回值**的向导
 *    —— 不是禁用占位、也不是本地假保存。
 *    断言在 ⑥⑦ 两节：控件存在 → 真输入 → 真的调了命令 → 入参 snake_case 且形状正确 →
 *    成功后显示后端真值；校验分支断言**一次请求都不发**。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import ProvidersPage, { ProvidersBody, type ProvidersBodyProps } from "@/pages/providers";
import { resetWizardSession } from "@/pages/providers/providerWizardModel";
import ProviderDetailPage, {
  ProviderDetailBody,
  linesOfProvider,
  rawPreview,
  readLine,
  type ProviderDetailBodyProps,
  type ProviderLinesProps,
} from "@/pages/providers/detail";
import { mountOf, readCredential, readProvider } from "@/pages/providers/model";
import { knownPages, resolvePage } from "@/app/pageRegistry";
import { routeByKey } from "@/app/routes";
import { TokenAgencyError, qk, registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n, type I18nKey } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

/**
 * **默认 5s 对这个文件结构性不够**（不是页面缺陷）——与 `routes.test.tsx` 同一条理由。
 *
 * 这一份里有多条用例要**跑两轮完整渲染**（例如「附加头：真能加行 / 改值 / 删行」先加行保存、
 * 再重挂一次详情页删行保存），单条独立跑就要 ~1.7s；`npm test` 会**并行**跑 34 个文件，
 * 机器一忙就顶穿 5s。
 *
 * 这不是猜的：第 323 轮 `build.ps1` 里它**真的**超时红过一次，而
 * `npx vitest run src/app/providers.test.tsx` 单独跑 69/69 全过。
 * 给它显式预算，**判据一条都没放宽** —— 只是不再让"机器忙"伪装成"测试挂"。
 *
 * 必须在**模块作用域**设置：放进 `beforeAll` 太晚，收集测试时就已经定好超时了。
 */
vi.setConfig({ testTimeout: 20_000, hookTimeout: 20_000 });

afterEach(() => {
  cleanup();
});

/**
 * 文案值 → 文本：**问 i18n 要同一份资源**，不在断言里写死中文。
 *
 * 为什么（与 `providerWizard.test.tsx` 的 `copy` 同一条理由）：本轮的文案改动走
 * `.ta-tmp/ui-providers/i18n.json` 映射文件（i18n 由主管独占），改文案不许让测试变红。
 * 断言"这句话不再内联 / 出现在 `?` 里"的**意图**不变，变的只是措辞的取值来源。
 */
function copy(key: string): string {
  return i18n.t(key as I18nKey);
}

/** 带插值的键（挂载数那种 `凭据 {{credentials}} · 通道 {{channels}}`）。 */
function copyVars(key: string, vars: Record<string, unknown>): string {
  return i18n.t(key as I18nKey, vars);
}

// ================================================================ 夹具（两种后端形状各一份）

/** Rust `ta_core::domain::Provider`（`catalog.rs:53` 的 `provider_list` 返回的就是它）。 */
function rustProvider(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: "p-rust",
    name: "自建网关",
    protocol: "openai",
    base_url: "https://llm.example.com/v1",
    auth: { scheme: "header", header: "x-api-key", prefix: "Bearer " },
    lane: "lane-jp",
    headers: { "x-org": "team-a" },
    on_header_conflict: "keep_client",
    passthrough: { header_mode: "pass", body: "preserve" },
    timeout: { connect_s: 10, read_s: 120, total_s: null },
    tls: { insecure: false, extra_ca_pem: "-----BEGIN CERTIFICATE-----", min_version: "1.2" },
    model_prefix: "openai/",
    enabled: true,
    ...overrides,
  };
}

/** 冻结契约 / 浏览器 mock 的 `ProviderView` 形状（`bindings.ts:589-601`）。 */
function viewProvider(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: "p-view",
    name: "command code",
    protocol: "anthropic",
    base_url: "https://api.commandcode.ai/v1",
    enabled: true,
    success_rate_24h: 0.998,
    avg_latency_ms: 82,
    channel_count: 3,
    status: "warn",
    ...overrides,
  };
}

function rustCredential(overrides: Record<string, unknown> = {}): unknown {
  return {
    credential: {
      id: "c-rust",
      provider: "p-rust",
      label: "自建 · 500万 token/月",
      secret: "${secret:rust}",
      lane: "lane-jp",
      guard: { max_rpm: 60, max_concurrency: 8, max_input_tokens: null, on_exceed: "wait" },
      tags: ["cn-direct"],
      enabled: true,
    },
    ...overrides,
  };
}

/**
 * 同一家下的**第二份**凭据（只有旧数据才会出现）。
 *
 * 用来钉住"页面不会因为多了份凭据就退回凭据表/多选"，而是**如实说出**这件事
 * （硬规则 3：一个服务商一把密钥；要多个账号 = 加多个服务商）。
 */
function secondRustCredential(): unknown {
  const base = rustCredential() as { credential: Record<string, unknown> };
  return { ...(rustCredential() as Record<string, unknown>), credential: { ...base.credential, id: "c-rust-2" } };
}

function viewCredential(overrides: Record<string, unknown> = {}): unknown {
  return {
    credential: {
      id: "c-view",
      provider: "p-view",
      label: "command code · 按量",
      secret: "${secret:commandcode}",
      lane: "lane-jp",
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "next_candidate" },
      tags: ["backup"],
      enabled: true,
    },
    provider_name: "command code",
    channel_count: 1,
    ...overrides,
  };
}

/**
 * `channel_list` 的一行（真机 `ChannelView` = `ChannelConfig` 逐字段 + 现算的 `models`）。
 *
 * 为什么带这么全：`channel_upsert` 是**整条覆盖写**，"改个网络代理"的载荷必须**以这条原始记录
 * 为底**。少给一个键，测试就永远发现不了"少发一个键会抹掉配置"这个缺陷。
 */
function rustChannel(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: "ch-bytedance",
    model: "deepseek-v4.1flash",
    provider: "p-bytedance",
    credential: "c-bytedance",
    lane: "lane-jp",
    upstream_model: "deepseek-v4.1flash",
    transform: {
      path: null,
      headers_add: { "x-keep": "1" },
      headers_remove: [],
      headers_override: {},
      body_patch: {},
    },
    priority: 100,
    weight: 10,
    tags: ["cn-direct"],
    target_override: "https://upstream.example/v1",
    enabled: true,
    capabilities: {
      vision: true,
      tools: true,
      reasoning: false,
      context_tokens: 128_000,
      max_output_tokens: null,
      modalities: ["text"],
    },
    models: ["deepseek-v4.1flash"],
    ...overrides,
  };
}

// ================================================================ ① 纯逻辑：读服务商

describe("读服务商：契约形状与 Rust 形状都认，缺的列不猜", () => {
  it("Rust 形状：完整配置字段读得出来；视图专有列一律 undefined（不是 0）", () => {
    const row = readProvider(rustProvider());

    expect(row.id).toBe("p-rust");
    expect(row.name).toBe("自建网关");
    expect(row.protocol).toBe("openai");
    expect(row.baseUrl).toBe("https://llm.example.com/v1");
    expect(row.enabled).toBe(true);
    expect(row.lane).toBe("lane-jp");
    expect(row.authScheme).toBe("header");
    expect(row.authHeader).toBe("x-api-key");
    expect(row.authPrefix).toBe("Bearer ");
    expect(row.modelPrefix).toBe("openai/");
    expect(row.timeoutConnectS).toBe(10);
    expect(row.timeoutReadS).toBe(120);
    expect(row.tlsInsecure).toBe(false);
    expect(row.tlsExtraCa).toBe(true);
    expect(row.tlsMinVersion).toBe("1.2");
    expect(row.onHeaderConflict).toBe("keep_client");
    expect(row.passthroughHeaderMode).toBe("pass");
    expect(row.passthroughBody).toBe("preserve");
    expect(row.headers).toEqual([{ name: "x-org", value: "team-a" }]);
    expect(row.hasConnectionConfig).toBe(true);

    // 契约视图才有、Rust 没有的列：读不到就是未知，不拿"启用"当"正常"
    expect(row.status).toBeUndefined();
    expect(row.successRate24h).toBeUndefined();
    expect(row.avgLatencyMs).toBeUndefined();
    expect(row.channelCount).toBeUndefined();
  });

  it("契约形状：视图专有列读得出来；连接配置字段缺失时明说不可用", () => {
    const row = readProvider(viewProvider());

    expect(row.successRate24h).toBe(0.998);
    expect(row.avgLatencyMs).toBe(82);
    expect(row.channelCount).toBe(3);
    expect(row.status).toBe("warn");

    expect(row.hasConnectionConfig).toBe(false);
    expect(row.authScheme).toBeUndefined();
    expect(row.timeoutReadS).toBeUndefined();
    expect(row.headers).toEqual([]);
    expect(row.lane).toBeUndefined();
  });

  it("附加头只收字符串值，按名字排序（用户数据原样显示）", () => {
    const row = readProvider(
      rustProvider({ headers: { "x-b": "2", "x-a": "1", "x-bad": 7, "x-c": null } }),
    );
    expect(row.headers).toEqual([
      { name: "x-a", value: "1" },
      { name: "x-b", value: "2" },
    ]);
  });

  it("非对象输入不炸：按空行处理（后端返回 undefined 时不猜）", () => {
    expect(readProvider(undefined).id).toBe("");
    expect(readProvider(null).hasConnectionConfig).toBe(false);
    expect(readProvider("nope").enabled).toBeUndefined();
  });
});

// ================================================================ ② 纯逻辑：读凭据与挂载

describe("读凭据与挂载统计", () => {
  it("Rust 形状：id / provider / 标签 / 开关都读得到", () => {
    const row = readCredential(rustCredential());
    expect(row.id).toBe("c-rust");
    expect(row.provider).toBe("p-rust");
    expect(row.enabled).toBe(true);
    expect(row.tags).toEqual(["cn-direct"]);
  });

  it("契约/mock 形状：provider_name 与 channel_count 都能读出来", () => {
    const row = readCredential(viewCredential());
    expect(row.id).toBe("c-view");
    expect(row.provider).toBe("p-view");
    expect(row.label).toBe("command code · 按量");
  });

  it("挂载统计：凭据列表读不到时是「未知」，不是 0", () => {
    const rust = readProvider(rustProvider());
    const view = readProvider(viewProvider());

    // 凭据读不到（undefined）= 未知
    expect(mountOf(rust, undefined)).toEqual({ credentials: undefined, channels: undefined });
    // 凭据列表为空 = 确实是 0；通道数来自服务商视图
    expect(mountOf(view, [])).toEqual({ credentials: 0, channels: 3 });
    // Rust 形状没有通道数那一列 → 只有凭据数是已知的
    expect(mountOf(rust, [readCredential(rustCredential())])).toEqual({
      credentials: 1,
      channels: undefined,
    });
    // 只数属于这个服务商的凭据
    const rows = [readCredential(rustCredential()), readCredential(viewCredential())];
    expect(mountOf(rust, rows).credentials).toBe(1);
  });
});

// ================================================================ ②b 纯逻辑：这一家的线路

describe("读线路：只认读得到的列；写载荷**以原始记录为底**", () => {
  it("`channel_list` 的行按 provider 过滤；读不到的列是 undefined（不是默认值）", () => {
    const rows = linesOfProvider(
      [rustChannel(), rustChannel({ id: "ch-other", provider: "p-other" })],
      "p-bytedance",
    );
    expect(rows).toHaveLength(1);
    const only = rows![0]!;
    expect(only.id).toBe("ch-bytedance");
    expect(only.upstreamModel).toBe("deepseek-v4.1flash");
    expect(only.laneId).toBe("lane-jp");
    expect(only.priority).toBe(100);
    expect(only.weight).toBe(10);
    expect(only.enabled).toBe(true);

    // 缺列 = 读不到（**不拿 `false` 冒充"已停用"**；`lane` 缺 = 不指定 → `null`）
    const bare = readLine({ id: "ch-x", provider: "p-rust" });
    expect(bare.enabled).toBeUndefined();
    expect(bare.priority).toBeUndefined();
    expect(bare.laneId).toBeNull();

    // `channel_list` 读不到 ≠ 这一家没有线路（两件事不能混）
    expect(linesOfProvider(undefined, "p-bytedance")).toBeUndefined();
    expect(linesOfProvider([], "p-bytedance")).toEqual([]);
  });

});

// ================================================================ ③ 展示层

function makeListProps(overrides: Partial<ProvidersBodyProps> = {}): ProvidersBodyProps {
  return {
    rows: [],
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    credentials: undefined,
    credentialsError: undefined,
    onRetryCredentials: () => {},
    onOpen: () => {},
    ...overrides,
  };
}

function renderList(overrides: Partial<ProvidersBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <ProvidersBody {...makeListProps(overrides)} />
    </ThemeProvider>,
  );
}

/**
 * 线路区的默认 props：**空线路 + 空目录**是一种真实的起点
 * （数据层永远把这一份注入；"读不到"要显式传 `lines: undefined`）。
 */
function makeLinesProps(overrides: Partial<ProviderLinesProps> = {}): ProviderLinesProps {
  return {
    lines: [],
    catalog: [],
    scanPending: false,
    onScan: () => {},
    busy: false,
    onToggleEnabled: () => {},
    onBulkEnabled: () => {},
    onRemove: () => {},
    ...overrides,
  };
}

function makeDetailProps(overrides: Partial<ProviderDetailBodyProps> = {}): ProviderDetailBodyProps {
  return {
    id: "p-rust",
    row: undefined,
    isLoading: false,
    error: undefined,
    onRetry: () => {},
    credentials: undefined,
    credentialsError: undefined,
    onRetryCredentials: () => {},
    laneChoiceGroups: [],
    autoSubscriptions: [],
    laneName: () => undefined,
    laneError: undefined,
    onBack: () => {},
    lines: makeLinesProps(),
    ...overrides,
  };
}

function renderDetail(overrides: Partial<ProviderDetailBodyProps> = {}) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <ProviderDetailBody {...makeDetailProps(overrides)} />
    </ThemeProvider>,
  );
}

function renderAt(path: string) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route path="/providers" element={<ProvidersPage />} />
            <Route path="/providers/:id" element={<ProviderDetailPage />} />
          </Routes>
        </MemoryRouter>
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

describe("[07 §3.1] 列表页：空 / 加载 / 错误 / 有数据", () => {
  /**
   * 这一节里有**整页**用例会临时改写 `provider_list`（"空态按钮接到向导"那条）。
   * `registerMock` 是全局注册 → 跑完必须把基准 mock 放回去，否则后面的整页用例会看到 0 条。
   */
  afterEach(() => {
    registerBaselineMocks();
  });

  it("目录为空时给诚实空状态：**只有一句话**，不画 0 行的表、也不重复页头那个按钮", () => {
    renderList();

    const empty = screen.getByTestId("providers-empty");
    expect(empty.textContent).toBe("配置里还没有服务商");
    expect(screen.queryByTestId("providers-table")).toBeNull();
    // 第 2 轮补丁判据 A：页头已经有同标签同动作的「添加服务商」→ 空态里不再放第二个 primary
    expect(within(empty).queryByRole("button")).toBeNull();
    expect(screen.queryByTestId("providers-empty-add")).toBeNull();
  });

  it("整页空态：空态里没有按钮，添加入口就是页头那一个（不是本地假入口）", async () => {
    registerBaselineMocks();
    registerMock("provider_list", () => []);

    renderAt("/providers");
    const empty = await screen.findByTestId("providers-empty");
    expect(within(empty).queryByRole("button")).toBeNull();
    // 同一屏唯一的添加入口仍是真的：点了进两步向导
    fireEvent.click(screen.getByTestId("providers-add-open"));
    expect(await screen.findByTestId("provider-wizard-step1")).toBeTruthy();
  });

  it("只有 1 个服务商时**没有**多选（规则 3：没有多选场景就不出现勾选框）", () => {
    renderList({
      rows: [readProvider(viewProvider())],
      onDelete: () => {},
      selectedIds: [],
      onToggleSelect: () => {},
      onToggleSelectAll: () => {},
      onDeleteSelected: () => {},
    });

    // 没有多选场景 → 勾选框、计数与「删除选中」一个都不出现
    expect(screen.queryByTestId("providers-select")).toBeNull();
    expect(screen.queryByTestId("providers-select-all")).toBeNull();
    expect(screen.queryByTestId("providers-selected-count")).toBeNull();
    expect(screen.queryByTestId("providers-delete-selected")).toBeNull();
    // 唯一的删除需求仍有出口：行内那个真按钮
    expect(screen.getByTestId("providers-delete")).toBeTruthy();
  });

  it("列收敛：「启用」并入「状态」，内容列一共 6 个（规则 4）", () => {
    renderList({ rows: [readProvider(viewProvider())] });

    const labels = screen
      .getAllByRole("columnheader")
      .map((cell) => (cell.textContent ?? "").trim())
      .filter((text) => text !== "");
    expect(labels).toEqual([
      copy("providers.col.name"),
      copy("providers.col.protocol"),
      copy("providers.col.status"),
      copy("providers.col.successRate"),
      copy("providers.col.latency"),
      copy("providers.col.mount"),
    ]);
    // 「启用」不再单独占一列：关闭的服务商在「状态」列里如实说「已禁用」
    expect(labels).not.toContain("启用");
  });

  it("关闭的服务商在「状态」列里显示「已禁用」（合并不等于丢信息）", () => {
    renderList({ rows: [readProvider(viewProvider({ enabled: false }))], credentials: [] });

    const row = screen.getByTestId("providers-row");
    expect(within(row).getByText("已禁用")).toBeTruthy();
  });

  it("加载中不与「没有服务商」混为一谈", () => {
    renderList({ isLoading: true });
    expect(screen.getByText("加载中…")).toBeTruthy();
    expect(screen.queryByTestId("providers-empty")).toBeNull();
  });

  it("读服务商失败：显示 TA 码 + 重试入口", () => {
    const onRetry = vi.fn();
    renderList({
      error: new TokenAgencyError({ code: "TA-5001", message: "配置校验失败" }),
      onRetry,
    });

    const node = screen.getByTestId("providers-error");
    expect(node.textContent).toContain("TA-5001");
    expect(node.textContent).toContain("配置校验失败");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("两种形状的行都渲染；缺的列显示「—」而不是 0", () => {
    const rows = [readProvider(rustProvider()), readProvider(viewProvider())];
    renderList({ rows, credentials: [readCredential(rustCredential())] });

    const table = within(screen.getByTestId("providers-table"));
    expect(table.getAllByTestId("providers-row")).toHaveLength(2);

    // 视图形状：成功率 / 延迟 / 通道数 / 状态灯都有
    expect(table.getByText("command code")).toBeTruthy();
    expect(table.getByText("99.8%")).toBeTruthy();
    expect(table.getByText("82ms")).toBeTruthy();
    expect(table.getByText("警告")).toBeTruthy();

    // Rust 形状：这些列读不到 → "—"（不是 0），状态显示"未知"
    const rustRow = screen.getAllByTestId("providers-row")[0]!;
    expect(rustRow.textContent).toContain("—");
    expect(within(rustRow).getByText("未知")).toBeTruthy();
    // 协议名走界面语言，不把 openai / anthropic 直接漏给用户
    expect(rustRow.textContent).toContain("OpenAI 兼容");
  });

  it("凭据数：凭据列表读不到就是「未知」；读失败只降级这一列，不毁整张表", () => {
    const rows = [readProvider(viewProvider())];

    renderList({ rows, credentials: [readCredential(viewCredential())] });
    expect(
      screen.getByText(copyVars("providers.mount.both", { credentials: 1, channels: 3 })),
    ).toBeTruthy();
    cleanup();

    renderList({ rows, credentials: undefined });
    expect(screen.getByText(copyVars("providers.mount.channelsOnly", { channels: 3 }))).toBeTruthy();
    expect(screen.getByTestId("providers-table")).toBeTruthy();
    cleanup();

    renderList({
      rows,
      credentials: undefined,
      credentialsError: new TokenAgencyError({ code: "TA-9001", message: "未注册 mock" }),
    });
    expect(screen.getByTestId("providers-credentials-error").textContent).toContain("TA-9001");
    expect(screen.getByTestId("providers-table")).toBeTruthy();
  });

  it("点名称进入详情；行上只有「删除」这一个写入口（测试/编辑/复制/停用仍然没有）", () => {
    const onOpen = vi.fn();
    const onDelete = vi.fn();
    renderList({ rows: [readProvider(viewProvider())], onOpen, onDelete });

    fireEvent.click(screen.getByRole("button", { name: "command code" }));
    expect(onOpen).toHaveBeenCalledWith("p-view");

    for (const label of ["测试连通性", "编辑", "复制", "停用"]) {
      expect(screen.queryByRole("button", { name: label })).toBeNull();
    }
    // 删除是本轮新接的写入口：是真按钮、点得到（不是禁用占位）
    const remove = screen.getByTestId("providers-delete");
    expect(remove.tagName).toBe("BUTTON");
    fireEvent.click(remove);
    expect(onDelete).toHaveBeenCalledTimes(1);
  });
});

describe("[07 §3.2] 详情页：目录里没有 / 连接配置不可用 / 有完整配置", () => {
  it("目录里没有这个 id：说清楚这是「没有」，不是「读失败」", () => {
    renderDetail({ id: "p-gone", row: undefined });

    expect(screen.getByTestId("providers-detail-notfound")).toBeTruthy();
    expect(screen.getByText(/目录里没有这个服务商/)).toBeTruthy();
    expect(screen.getByText(/p-gone/)).toBeTruthy();
    expect(screen.queryByTestId("providers-connection")).toBeNull();
  });

  it("加载中与读失败各有各的显示（都不与「没有」混）", () => {
    renderDetail({ isLoading: true });
    expect(screen.getByText("加载中…")).toBeTruthy();
    expect(screen.queryByTestId("providers-detail-notfound")).toBeNull();
    cleanup();

    renderDetail({
      error: new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：provider_list" }),
    });
    expect(screen.getByTestId("providers-detail-error").textContent).toContain("TA-9001");
  });

  it("后端只给了视图（没有连接配置字段）：整页只读并说清原因，不排一排空占位", () => {
    renderDetail({ id: "p-view", row: readProvider(viewProvider()) });

    // 只读的原因写在最上面（纪律 2）：覆盖写会用默认值抹掉线上配置
    expect(screen.getByTestId("providers-view-only")).toBeTruthy();
    expect(screen.getByText(copy("providers.edit.viewOnly"))).toBeTruthy();
    // 「连接配置没读到」这句结论仍然一眼可见（红线第 1 条）
    expect(screen.getByTestId("providers-connection-unavailable")).toBeTruthy();
    expect(screen.getByText(copy("providers.connection.unavailable"))).toBeTruthy();
    // 视图里有的字段照常显示（值在只读控件里），但没有"假保存按钮"
    expect(inputOf("providers-edit-name").value).toBe("command code");
    expect(inputOf("providers-edit-name").disabled).toBe(true);
    expect(inputOf("providers-edit-baseUrl").value).toBe("https://api.commandcode.ai/v1");
    expect(screen.getByText("Anthropic")).toBeTruthy();
    expect(screen.queryByTestId("providers-save-bar")).toBeNull();
    expect(screen.queryByTestId("providers-detail-save")).toBeNull();
  });

  it("完整配置：鉴权 / 超时 / TLS / 附加头 / 透传 / 默认线路都在可视化控件里，值就是后端那些字", () => {
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      laneChoiceGroups: [
        {
          group: "直连（本机网络）",
          items: [{ value: "lane-jp", label: "日本线", kind: "lane", lane_id: "lane-jp" }],
        },
      ],
      laneName: (lane) => (lane === "lane-jp" ? "日本线" : undefined),
      onSave: async () => {},
    });

    // 基本信息：值直接可改（不是一排只读文字）
    expect(inputOf("providers-edit-name").value).toBe("自建网关");
    expect(inputOf("providers-edit-baseUrl").value).toBe("https://llm.example.com/v1");
    expect(inputOf("providers-edit-modelPrefix").value).toBe("openai/");
    expect(selectTextOf("providers-edit-protocol")).toContain("OpenAI");
    // 服务商级默认线路：下拉里是线路名，不是 id
    expect(selectTextOf("providers-edit-lane")).toContain("日本线");
    // id 是主键：只读显示，不给输入框
    expect(screen.queryByTestId("providers-edit-id")).toBeNull();
    expect(screen.getByText("p-rust")).toBeTruthy();

    // 鉴权与连接
    expect(selectTextOf("providers-edit-authScheme")).toContain("自定义头");
    expect(inputOf("providers-edit-authHeader").value).toBe("x-api-key");
    // 值前缀的尾随空格有意义（`Bearer ` ≠ `Bearer`）→ 原样带进控件
    expect(inputOf("providers-edit-authPrefix").value).toBe("Bearer ");
    expect(inputOf("providers-edit-timeoutConnect").value).toBe("10");
    expect(inputOf("providers-edit-timeoutRead").value).toBe("120");
    expect(textareaOf("providers-edit-extraCa").value).toContain("BEGIN CERTIFICATE");

    // 附加头是一张可增删的行表（不是一段 JSON 文本）
    const headerNames = screen.getAllByTestId("providers-header-name");
    const headerValues = screen.getAllByTestId("providers-header-value");
    expect(headerNames).toHaveLength(1);
    expect(headerNames[0]!.querySelector("input")!.value).toBe("x-org");
    expect(headerValues[0]!.querySelector("input")!.value).toBe("team-a");
    expect(screen.getByTestId("providers-header-add")).toBeTruthy();
    expect(selectTextOf("providers-edit-headerConflict")).toContain("保留客户端的（只补缺）");
    expect(selectTextOf("providers-edit-passthroughHeader")).toContain("pass");
    expect(selectTextOf("providers-edit-passthroughBody")).toContain("preserve");
  });

  it("密钥区只显示这一家的那条凭据（别家的一个字都不出现）", () => {
    const rows = [readCredential(rustCredential()), readCredential(viewCredential())];
    renderDetail({ id: "p-rust", row: readProvider(rustProvider()), credentials: rows });

    // 一个服务商一把密钥（硬规则 3）：页面上只有**一块**密钥区，没有凭据表
    expect(screen.getByTestId("providers-key")).toBeTruthy();
    expect(screen.queryByTestId("providers-credentials-table")).toBeNull();
    expect(screen.getByTestId("providers-key-label").textContent).toBe("自建 · 500万 token/月");
    // 密钥以**引用**形式显示
    expect(screen.getByTestId("providers-key-secret").textContent).toBe("${secret:rust}");
    // 别家的凭据一个字都不出现（按 `credential.provider` 过滤）
    expect(screen.queryByText("command code · 按量")).toBeNull();
  });

  it("这一家还没有凭据：**只留一句结论**，且不放点了没用的按钮", () => {
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      credentials: [],
    });

    const empty = screen.getByTestId("providers-key-empty");
    expect(empty).toBeTruthy();
    expect(empty.textContent).toBe(copy("providers.credentials.empty"));
    // 原来那半句"实现解释"（凭据的 provider 字段 / 通道候选）不再铺在版面上
    expect(screen.queryByText(/provider 字段/)).toBeNull();
    expect(screen.queryByTestId("providers-credentials-table")).toBeNull();
  });

  it("旧数据里挂了多份凭据：如实说出来（而不是悄悄只显示第一份）", () => {
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      credentials: [readCredential(rustCredential()), readCredential(secondRustCredential())],
    });

    const multiple = screen.getByTestId("providers-key-multiple");
    // 文案键由主管补进 i18n（见任务汇报）→ 这里钉"这句话**出现**了"，不钉它的措辞
    expect(multiple).toBeTruthy();
    // 页面上仍然没有凭据表 / 多选（一个服务商一把密钥；要多个账号就多个服务商）
    expect(screen.queryByTestId("providers-credentials-table")).toBeNull();
    expect(screen.queryByTestId("providers-credential-select")).toBeNull();
  });

  it("原始字段原样打印（后端多给什么看得见）", () => {
    renderDetail({ id: "p-rust", row: readProvider(rustProvider({ custom_thing: "keep-me" })) });
    expect(screen.getByTestId("providers-raw").textContent).toContain("keep-me");
  });
});

// ================================================================ ④ 整页（浏览器 mock 路径）

describe("整页在 mock 基准场景下能用（不依赖 Rust/Tauri）", () => {
  beforeEach(() => {
    resetWizardSession();
  });

  it("/providers 渲染真实目录，且不再是占位壳", async () => {
    renderAt("/providers");

    expect((await screen.findByText("服务商")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();
    expect(await screen.findByText("字节方舟")).toBeTruthy();
    expect(await screen.findByText("共 2 个服务商")).toBeTruthy();
  });

  it("/providers/:id 渲染详情（含挂载的凭据），不再是占位壳", async () => {
    renderAt("/providers/p-bytedance");

    expect(await screen.findByTestId("providers-detail")).toBeTruthy();
    expect(screen.queryByTestId("page-placeholder")).toBeNull();
    await waitFor(() => expect(screen.getByText("字节 · 500万 token/月")).toBeTruthy());
    // mock 的 provider_list 给的是视图形状 → 连接配置如实显示"未返回"
    expect(screen.getByTestId("providers-connection-unavailable")).toBeTruthy();
  });

  it("文案里不留未替换的占位符", async () => {
    renderAt("/providers");
    await screen.findByText("字节方舟");
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑤ 接线（两条路由都解析到真页面）

describe("两条路由都解析到真页面，不再走占位壳", () => {
  /**
   * 为什么这里用 `resolvePage` 而不是"文件名启发式"：
   * `routes.ts` 给 `provider-detail` 的候选路径是 `providers/detail`（不是 `providers/detail/index`），
   * 而 `noPlaceholder.guard.test.ts` 的 `pageEntryKeys()` 只认"根级 x.tsx 或 x/index.tsx"，
   * 于是它会把**已经落地的** `src/pages/providers/detail.tsx` 判成"没有页面"。
   * 这一条按 `pageRegistry` 的真实解析规则钉住接线，假阳性就不会被当成真缺陷。
   */
  it("候选路径都在 knownPages 里，且 resolvePage 能解析出组件", () => {
    const known = knownPages();
    expect(known).toContain("providers/index");
    expect(known).toContain("providers/detail");

    for (const key of ["providers", "provider-detail"] as const) {
      const route = routeByKey(key);
      expect(route, `路由表里应有 ${key}`).toBeDefined();
      expect(resolvePage(route!.pages), `${route!.path} 应解析到真页面`).toBeDefined();
    }
  });
});

// ================================================================ ⑥⑦ 两个添加向导

/**
 * 记录每次调用入参的 mock：返回 `result`，同时把入参留下来
 * （与 `channels.test.tsx` ⑨ 同一手法）。
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

/**
 * 输入框取值：MUI 的 `TextField` / `Select` **不吃 `data-testid`**（TS2353），
 * 所以 testid 挂在外层元素上 —— 真机 E2E 用 `[data-testid="X"] input` 命中，
 * 这里用**同一个选择器**，测试与 E2E 不会各认一套。
 */
function inputOf(testId: string): HTMLInputElement {
  const input = screen.getByTestId(testId).querySelector("input");
  if (input === null) throw new Error(`[data-testid="${testId}"] 里没有 input`);
  return input;
}

/** 打真字（`fireEvent.change` 走 React 的 onChange，不是塞 state）。 */
function typeInto(testId: string, value: string): void {
  fireEvent.change(inputOf(testId), { target: { value } });
}

/** 多行控件的取法：MUI 的 `multiline` 渲染成 `<textarea>`，`querySelector("input")` 找不到它。 */
function textareaOf(testId: string): HTMLTextAreaElement {
  const area = screen.getByTestId(testId).querySelector("textarea");
  if (area === null) throw new Error(`[data-testid="${testId}"] 里没有 textarea`);
  return area;
}

/** 打开一个 MUI 下拉并点中某一项。 */
async function chooseOption(testId: string, name: RegExp | string): Promise<void> {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name }));
}

/**
 * 行内的下拉：线路列表里每一条线路都有自己的网络代理下拉，作用域必须收窄到**那一行**，


/** 本轮向导该有的控件。**不许改名**：真机 E2E 按它们驱动。 */
const PROVIDER_WIZARD_CONTROLS = [
  "providers-add-id",
  "providers-add-name",
  "providers-add-protocol",
  "providers-add-baseUrl",
  "providers-add-authScheme",
  "providers-add-authHeader",
  "providers-add-authPrefix",
  "providers-add-submit",
] as const;

const CREDENTIAL_WIZARD_CONTROLS = [
  "credentials-add-id",
  "credentials-add-label",
  "credentials-add-provider",
  "credentials-add-secret",
  "credentials-add-submit",
] as const;

describe(" 高级添加（单步对话框）：真控件 → 真的调 provider_upsert → 显示后端真值", () => {
  beforeEach(() => {
    resetWizardSession();
  });

  /*
   * 为什么这里的入口是 `providers-add-advanced-open`：本轮`providers-add-open`
   * 改成**两步向导**（`src/app/providerWizard.test.tsx` 验它），而原来的单步对话框**原样保留**
   * 并改挂到「高级添加」入口 —— 字段 tid（`providers-add-*`）一个都没变，
   * 所以下面这些断言一字不改，只换了"点哪个入口"。
   */
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("页头有「高级添加」；打开后控件一个不少，且都是能真交互的控件", async () => {
    renderAt("/providers");

    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));
    for (const id of PROVIDER_WIZARD_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }
    expect(inputOf("providers-add-id").tagName).toBe("INPUT");
    expect(inputOf("providers-add-name").tagName).toBe("INPUT");
    expect(inputOf("providers-add-baseUrl").tagName).toBe("INPUT");
    // 协议与鉴权方式是下拉（协议清单来自 PROTOCOLS，不是前端另编一份）
    expect(within(screen.getByTestId("providers-add-protocol")).getByRole("combobox")).toBeTruthy();
    expect(within(screen.getByTestId("providers-add-authScheme")).getByRole("combobox")).toBeTruthy();
    fireEvent.mouseDown(within(screen.getByTestId("providers-add-protocol")).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: "Anthropic" })).toBeTruthy();
    expect(screen.getByRole("option", { name: "Gemini" })).toBeTruthy();
  });

  it("填完真提交 → provider_upsert({provider})：snake_case、只带用户真给的字段；回显后端返回值", async () => {
    let listCalls = 0;
    registerMock("provider_list", () => {
      listCalls += 1;
      return [viewProvider()];
    });
    // 后端返回的**与提交的完全不同**：只有"显示的是后端真值"，下面的断言才会过
    const upsert = recordingMock({
      id: "p-backend",
      name: "后端改名了",
      protocol: "gemini",
      base_url: "https://backend.example/v1",
      auth: { scheme: "bearer", header: null, prefix: null },
      headers: {},
      on_header_conflict: "keep_client",
      passthrough: { header_mode: null, body: null },
      timeout: { connect_s: 10, read_s: 120, total_s: null },
      tls: { insecure: false, extra_ca_pem: null, min_version: null },
      lane: null,
      model_prefix: null,
      enabled: true,
    });
    registerMock("provider_upsert", upsert.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));
    typeInto("providers-add-id", "my-gw");
    typeInto("providers-add-name", "我的网关");
    await chooseOption("providers-add-protocol", "Anthropic");
    typeInto("providers-add-baseUrl", "https://gw.example.com/v1");
    await chooseOption("providers-add-authScheme", "自定义头");
    typeInto("providers-add-authHeader", "x-api-key");
    typeInto("providers-add-authPrefix", "Bearer ");
    fireEvent.click(screen.getByTestId("providers-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const args = upsert.calls[0]!;
    // 命令入参只有一个 `provider`（契约：`provider_upsert: { args: { provider: ProviderInput } }`）
    expect(Object.keys(args)).toEqual(["provider"]);
    const p = args["provider"] as Record<string, unknown>;
    expect(p["id"]).toBe("my-gw");
    expect(p["name"]).toBe("我的网关");
    expect(p["protocol"]).toBe("anthropic");
    expect(p["base_url"]).toBe("https://gw.example.com/v1");
    expect(p["auth"]).toEqual({ scheme: "header", header: "x-api-key", prefix: "Bearer " });

    // 全部键都是 snake_case（与真机命令层守卫同一口径）
    for (const key of Object.keys(p)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);
    // 用户没填的字段**不替后端编**：这些在 Rust 侧都有 #[serde(default)]
    for (const absent of [
      "headers",
      "on_header_conflict",
      "passthrough",
      "timeout",
      "tls",
      "enabled",
      "lane",
      "model_prefix",
    ]) {
      expect(absent in p, `${absent} 有 Rust serde(default)，前端不许替后端硬编`).toBe(false);
    }
    // 没有 API Key 输入框：载荷里也不许出现任何明文密钥字段
    expect("api_key" in p).toBe(false);

    // 回显的是**后端返回的那个 Provider**（名字/协议/地址都不是我们提交的）
    const saved = await screen.findByTestId("providers-add-result");
    expect(saved.textContent).toContain("p-backend");
    expect(saved.textContent).toContain("后端改名了");
    expect(saved.textContent).toContain("https://backend.example/v1");

    // 成功后失效重取（列表真的刷新了，不是本地塞一行）
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("鉴权细节一个都没填 → 载荷里连 `auth` 都不出现（交给 Rust 的 serde(default)）", async () => {
    const upsert = recordingMock({ id: "p-x", name: "x", protocol: "openai", base_url: "https://x.example" });
    registerMock("provider_upsert", upsert.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));
    typeInto("providers-add-id", "my-gw");
    typeInto("providers-add-name", "我的网关");
    typeInto("providers-add-baseUrl", "https://gw.example.com/v1");
    fireEvent.click(screen.getByTestId("providers-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const p = upsert.calls[0]!["provider"] as Record<string, unknown>;
    expect("auth" in p).toBe(false);
  });

  it("本地校验不过就**一次都不发**：必填 / id 形状 / 与现有 id 重复 / base_url", async () => {
    const upsert = recordingMock({});
    registerMock("provider_upsert", upsert.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));

    // ① 全空 → 必填
    fireEvent.click(screen.getByTestId("providers-add-submit"));
    expect((await screen.findByTestId("providers-add-issues")).textContent).toContain("必填");
    expect(upsert.calls).toHaveLength(0);

    // ② id 形状（只许小写字母/数字/短横线）
    typeInto("providers-add-id", "Bad_Id");
    typeInto("providers-add-name", "网关");
    typeInto("providers-add-baseUrl", "https://gw.example.com/v1");
    fireEvent.click(screen.getByTestId("providers-add-submit"));
    expect(screen.getByTestId("providers-add-issues").textContent).toContain(
      "id 只能用小写字母、数字与短横线",
    );
    expect(upsert.calls).toHaveLength(0);

    // ③ 与 provider_list 里已有的 id 重复
    typeInto("providers-add-id", "p-bytedance");
    fireEvent.click(screen.getByTestId("providers-add-submit"));
    expect(screen.getByTestId("providers-add-issues").textContent).toContain("已存在同 id 的条目");
    expect(upsert.calls).toHaveLength(0);

    // ④ base_url 不是 http(s)://
    typeInto("providers-add-id", "my-gw");
    typeInto("providers-add-baseUrl", "ftp://gw.example.com");
    fireEvent.click(screen.getByTestId("providers-add-submit"));
    expect(screen.getByTestId("providers-add-issues").textContent).toContain(
      "必须是 http:// 或 https:// 开头的完整地址",
    );
    expect(upsert.calls).toHaveLength(0);
  });

  it("后端拒绝时原样显示 TA 码与字段路径（不吞错、不改写成一句「保存失败」）", async () => {
    registerMock("provider_upsert", () => {
      throw new TokenAgencyError({
        code: "TA-5003",
        message: "providers[0].base_url 不是合法 URL",
      });
    });

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));
    typeInto("providers-add-id", "my-gw");
    typeInto("providers-add-name", "我的网关");
    typeInto("providers-add-baseUrl", "https://gw.example.com/v1");
    fireEvent.click(screen.getByTestId("providers-add-submit"));

    const error = await screen.findByTestId("providers-add-error");
    expect(error.textContent).toContain("TA-5003");
    expect(error.textContent).toContain("providers[0].base_url");
    expect(screen.queryByTestId("providers-add-result")).toBeNull();
  });
});

// ================================================================ ⑦ 一个服务商一把密钥
//
// 第 73 轮这里验的是「添加密钥」对话框（一次建一份凭据：自己的 id / 标签 / 服务商 / 明文 /
// 额度上限 / 币种 / 窗口）。**第 80 轮那个对话框整条下线了**：owner 原话「一个服务商肯定就一个
// 密钥，怎么还做多个删除的……我期望的操作是直接添加多个同一个服务商，取不同名字」。
//
// 所以这一节不再验"密钥对话框能填能发"，而是钉住**它真的没了**、以及替代它的两条规则：
//
// 1. **列表页没有「添加密钥」入口**（`credentials-add-open` / `credentials-add-*` 一个都不在）；
// 2. **列表页一个 `credential_upsert` 调用点都没有**（用 recorder 数调用次数 = 0）——
//    这是机械判据：只要有人把那个对话框接回来，计数立刻不为 0；
// 3. **同一家的第二个账号 = 第二条服务商**：名称重复被本地预检挡住（一次命令都不发），
//    换一个名字就能提交（`provider_upsert` 的载荷里也**没有任何密钥字段**）。
//
// 密钥本身的写入口（两步向导第 1 步 `pw-key`、详情页「密钥」块）分别由
// `providerWizard.test.tsx` 与本节 ⑧ 覆盖。

describe(" 一个服务商一把密钥：列表页没有第二个凭据入口", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("页头没有「添加密钥」按钮，也没有任何 credentials-add-* 控件", async () => {
    renderAt("/providers");
    // 等列表真的渲染出来（否则"找不到"可能只是还没渲染）
    await screen.findByTestId("providers-table");

    expect(screen.queryByTestId("credentials-add-open")).toBeNull();
    expect(screen.queryByText("添加密钥")).toBeNull();
    for (const id of CREDENTIAL_WIZARD_CONTROLS) {
      expect(screen.queryByTestId(id), `${id} 是「添加密钥」的控件，应当随对话框一起下线`).toBeNull();
    }
    // 页头剩下的两个入口仍在（这一步是"删对了"的自证，不是"整页被删空"）
    expect(screen.getByTestId("providers-add-open")).toBeTruthy();
    expect(screen.getByTestId("providers-add-advanced-open")).toBeTruthy();
  });

  it("列表页把凭据挂到服务商上的调用次数 = 0（没有 credential_upsert 调用点）", async () => {
    const upsert = recordingMock({});
    const setSecret = recordingMock(undefined);
    registerMock("credential_upsert", upsert.handler);
    registerMock("credential_set_secret", setSecret.handler);

    renderAt("/providers");
    await screen.findByTestId("providers-table");

    // 页头两个入口都点开再关掉：走一遍所有可达的 UI，计数仍然是 0
    fireEvent.click(screen.getByTestId("providers-add-advanced-open"));
    expect(screen.getByTestId("providers-add-submit")).toBeTruthy();
    fireEvent.click(screen.getByText("取消"));
    fireEvent.click(screen.getByTestId("providers-add-open"));
    await screen.findByTestId("provider-wizard-step1");

    expect(upsert.calls, "列表页不该有任何 credential_upsert 调用点").toHaveLength(0);
    expect(setSecret.calls).toHaveLength(0);
  });

  it("同一家的第二个账号：名称重复本地就挡住（一次都不发），换名字就能提交", async () => {
    const upsert = recordingMock({ id: "p-2", name: "字节二号", protocol: "openai", base_url: "https://gw.example.com/v1" });
    registerMock("provider_upsert", upsert.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("providers-add-advanced-open"));

    // 名字与现有服务商（基准 mock 里的「字节方舟」）重复 → 本地预检点名名称，**不发命令**
    typeInto("providers-add-id", "bytedance-2");
    typeInto("providers-add-name", "字节方舟");
    typeInto("providers-add-baseUrl", "https://gw.example.com/v1");
    fireEvent.click(screen.getByTestId("providers-add-submit"));

    const issues = await screen.findByTestId("providers-add-issues");
    expect(issues.textContent).toContain("名称");
    expect(issues.textContent).toContain("这个名称已经被占用，换一个");
    expect(upsert.calls).toHaveLength(0);

    // 取个**不同**的名字 → 这是 owner 说的"添加多个同一个服务商、取不同名字"，放行
    typeInto("providers-add-name", "字节二号");
    fireEvent.click(screen.getByTestId("providers-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const payload = upsert.calls[0]!["provider"] as Record<string, unknown>;
    expect(payload["id"]).toBe("bytedance-2");
    expect(payload["name"]).toBe("字节二号");
    expect(payload["base_url"]).toBe("https://gw.example.com/v1");
    // 服务商载荷里**没有任何密钥字段**：密钥只走 credential_set_secret（明文永不进载荷）
    for (const key of Object.keys(payload)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);
    for (const absent of ["secret", "key", "credential", "api_key"]) {
      expect(absent in payload, `${absent} 不属于服务商载荷`).toBe(false);
    }
  });
});


// ================================================================ ⑧ 详情页 = 查看 + 编辑一体
//
// 用户第 75 轮原话：「服务商编辑页面不走下一步下一步，直接把所有信息在点开服务商之后提供
// 可视化编辑界面，相当于编辑和查看一体了，当然不是纯文本编辑」。
//
// 这一节全部走**整页**（`renderAt`）：编辑的语义一半在控件上、一半在"点了到底发了什么载荷"上
// ——而载荷正是第 72 轮那个数据丢失缺陷的现场（编辑向导只发 4 个字段，覆盖写抹掉其余配置）。

/** 一条挂在某个服务商下的凭据：模型区按凭据归属过滤。 */
function boundCredential(id: string, provider: string): unknown {
  return {
    credential: {
      id,
      provider,
      label: "默认凭据",
      secret: `\${secret:${id}}`,
      lane: null,
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "wait" },
      tags: [],
      enabled: true,
    },
    provider_name: provider,
    channel_count: 1,
  };
}

/** 下拉当前显示的文案（MUI 把选中的值渲染在 `[role="combobox"]` 里）。 */
function selectTextOf(testId: string): string {
  return (within(screen.getByTestId(testId)).getByRole("combobox").textContent ?? "").trim();
}

/** 基准 mock 的模型 id（`shared/api/mock.ts` 的场景常量）。 */
const BASELINE_MODEL = "deepseek-v4.1flash";

/** 一份**完整**的 Rust `Provider`：编辑页要有真值可改、也要有真值可回写。 */
function rustBytedance(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: "p-bytedance",
    name: "字节方舟",
    protocol: "openai",
    base_url: "https://ark.cn-beijing.volces.com/api/v3",
    auth: { scheme: "bearer", header: null, prefix: null },
    lane: "lane-direct",
    headers: { "x-org": "team-a" },
    on_header_conflict: "keep_client",
    passthrough: { header_mode: "pass", body: "preserve" },
    timeout: { connect_s: 10, read_s: 600, total_s: null },
    tls: { insecure: false, extra_ca_pem: null, min_version: null },
    model_prefix: null,
    enabled: true,
    ...overrides,
  };
}

/** 打开一个服务商的详情页（这一页本身就是编辑器，没有"点编辑"这一步）。 */
async function openDetail(id = "p-bytedance"): Promise<void> {
  renderAt(`/providers/${id}`);
  await screen.findByTestId("providers-detail");
  // `providers-detail` 这个容器在"加载中"时就存在 → 必须再等这一行真的到齐
  await screen.findByTestId("providers-edit-name");
}

function saveButton(): HTMLElement {
  return screen.getByTestId("providers-detail-save");
}

/**
 * 上游原文的**单行预览**：只截断、不改写。
 *
 * 原文是"上游真的给了什么"的证据 —— 预览压掉换行只是为了让一行放得下，
 * **不许**挑字段、不许重排、不许加自己的话（那样它就不是证据了）。
 */
describe("rawPreview：单行预览只截断，不改写原文", () => {
  it("短的逐字给出（只压空白）；长的只加省略号", () => {
    expect(rawPreview({ id: "m", object: "model" })).toBe('{"id":"m","object":"model"}');
    // 值里的连续空白压成一个（只为放进一行；键与值的字符本身没动）
    expect(rawPreview({ a: "x  y" })).toBe('{"a":"x y"}');
    // JSON 里的换行本来就是转义的 `\n`（两个字符）→ 它天然是一行，不该被改掉
    expect(rawPreview({ a: "x\ny" })).toBe('{"a":"x\\ny"}');
    const long = rawPreview({ id: "m", note: "z".repeat(300) }, 40);
    expect(long.endsWith("…")).toBe(true);
    expect(long.length).toBe(41);
    // 截断只是后缀省略号：保留的前缀与原文逐字一致
    expect(long.slice(0, 40)).toBe(`{"id":"m","note":"${"z".repeat(22)}`);
  });
});
describe(" 详情页 = 查看 + 编辑一体：点开就能改，不再走下一步", () => {
  beforeEach(() => {
    registerBaselineMocks();
    registerMock("provider_list", () => [rustBytedance()]);
  });

  it("点开服务商直接就是编辑器：没有「编辑」按钮也没有下一步，控件里是后端真值", async () => {
    await openDetail();

    // 不再有"编辑 → 下一步 → 下一步"
    expect(screen.queryByTestId("providers-detail-edit")).toBeNull();
    expect(screen.queryByTestId("providers-next")).toBeNull();
    expect(screen.queryByTestId("pw-next")).toBeNull();
    expect(screen.queryByTestId("provider-wizard-step1")).toBeNull();

    // 控件就在页面上，值是后端那一条（不是空表）
    expect(inputOf("providers-edit-name").value).toBe("字节方舟");
    expect(inputOf("providers-edit-baseUrl").value).toBe("https://ark.cn-beijing.volces.com/api/v3");
    expect(inputOf("providers-edit-timeoutRead").value).toBe("600");
    expect(selectTextOf("providers-edit-protocol")).toContain("OpenAI");

    // 没有改动 → 保存不可点，并如实说"没有改动"（不是点了没反应的按钮）
    expect(saveButton().hasAttribute("disabled")).toBe(true);
    expect(screen.getByText("没有改动")).toBeTruthy();
  });

  it("改一个字段再保存：载荷是**整条记录**（没碰过的连接配置一个字节都不丢）", async () => {
    const upsert = recordingMock(rustBytedance({ name: "字节方舟 v2", status: "ok" }));
    registerMock("provider_upsert", upsert.handler);

    await openDetail();
    typeInto("providers-edit-name", "字节方舟 v2");
    expect(saveButton().hasAttribute("disabled")).toBe(false);
    fireEvent.click(saveButton());

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const payload = upsert.calls[0]!["provider"] as Record<string, unknown>;
    // 改动的字段 + id 复用（改名不等于新建一个服务商）
    expect(payload["name"]).toBe("字节方舟 v2");
    expect(payload["id"]).toBe("p-bytedance");
    // 回归：整条覆盖写**不许**抹掉没碰过的配置（第 72 轮编辑向导就是在这里丢数据的）
    expect(payload["auth"]).toMatchObject({ scheme: "bearer", header: null, prefix: null });
    expect(payload["headers"]).toEqual({ "x-org": "team-a" });
    expect(payload["on_header_conflict"]).toBe("keep_client");
    expect(payload["passthrough"]).toMatchObject({ header_mode: "pass", body: "preserve" });
    expect(payload["timeout"]).toMatchObject({ connect_s: 10, read_s: 600, total_s: null });
    expect(payload["tls"]).toMatchObject({ insecure: false, extra_ca_pem: null, min_version: null });
    expect(payload["lane"]).toBe("lane-direct");
    expect(payload["enabled"]).toBe(true);
    // 只读视图专有的列不是配置：不许被当成用户自定义字段写进 `extra`
    expect(payload["status"]).toBeUndefined();
    expect(payload["success_rate_24h"]).toBeUndefined();

    // 成功的汇报 + 保存按钮回到"没有改动"
    // （汇报**那句话的措辞**归 i18n 所有：这里钉"后端写入成功后界面真的说了这件事"）
    expect((await screen.findByTestId("providers-detail-save-result")).textContent).not.toBe("");
    await waitFor(() => expect(saveButton().hasAttribute("disabled")).toBe(true));
  });

  it("清空一个可选字段：载荷里是**显式 null**（不发这个键 = 默认值，那不是清空）", async () => {
    const upsert = recordingMock(rustBytedance());
    registerMock("provider_upsert", upsert.handler);
    registerMock("provider_list", () => [rustBytedance({ model_prefix: "openai/" })]);

    await openDetail();
    expect(inputOf("providers-edit-modelPrefix").value).toBe("openai/");
    typeInto("providers-edit-modelPrefix", "");
    fireEvent.click(saveButton());

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const payload = upsert.calls[0]!["provider"] as Record<string, unknown>;
    expect(Object.prototype.hasOwnProperty.call(payload, "model_prefix")).toBe(true);
    expect(payload["model_prefix"]).toBeNull();
  });

  it("本地预检不过就一次都不发：必填 / base_url 形状 / 超时不是正整数", async () => {
    const upsert = recordingMock(rustBytedance());
    registerMock("provider_upsert", upsert.handler);

    await openDetail();
    typeInto("providers-edit-name", "");
    typeInto("providers-edit-baseUrl", "ftp://nope");
    typeInto("providers-edit-timeoutRead", "abc");
    fireEvent.click(saveButton());

    expect(upsert.calls).toHaveLength(0);
    const issues = await screen.findByTestId("providers-detail-issues");
    expect(issues.textContent).toContain("必填");
    expect(issues.textContent).toContain("必须是 http:// 或 https:// 开头的完整地址");
    expect(issues.textContent).toContain("必须是数字（可留空）");
  });

  it("后端拒绝时原样显示 TA 码与原文（不吞错、不改写成一句「保存失败」）", async () => {
    registerMock("provider_upsert", () => {
      throw new TokenAgencyError({ code: "TA-5001", message: "providers[0].base_url 非法" });
    });

    await openDetail();
    typeInto("providers-edit-name", "改一下");
    fireEvent.click(saveButton());

    const error = await screen.findByTestId("providers-detail-save-error");
    expect(error.textContent).toContain("TA-5001");
    expect(error.textContent).toContain("providers[0].base_url 非法");
  });

  it("附加头：真能加行 / 改值 / 删行，载荷里就是那张表（不是一段 JSON 文本）", async () => {
    const upsert = recordingMock(rustBytedance());
    registerMock("provider_upsert", upsert.handler);

    await openDetail();
    // 加一行 → 填上 → 把原来那行的值改掉
    fireEvent.click(screen.getByTestId("providers-header-add"));
    const names = screen.getAllByTestId("providers-header-name");
    const values = screen.getAllByTestId("providers-header-value");
    expect(names).toHaveLength(2);
    fireEvent.change(names[1]!.querySelector("input")!, { target: { value: "x-extra" } });
    fireEvent.change(values[1]!.querySelector("input")!, { target: { value: "1" } });
    fireEvent.change(values[0]!.querySelector("input")!, { target: { value: "team-b" } });

    fireEvent.click(saveButton());
    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    expect((upsert.calls[0]!["provider"] as Record<string, unknown>)["headers"]).toEqual({
      "x-org": "team-b",
      "x-extra": "1",
    });
    cleanup();

    // 删行：只在点过保存之后才重新渲染太绕，再来一遍
    registerBaselineMocks();
    registerMock("provider_list", () => [rustBytedance()]);
    const second = recordingMock(rustBytedance());
    registerMock("provider_upsert", second.handler);
    await openDetail();
    fireEvent.click(screen.getByTestId("providers-header-remove"));
    expect(screen.queryAllByTestId("providers-header-row")).toHaveLength(0);
    expect(screen.getByTestId("providers-headers-empty")).toBeTruthy();
    fireEvent.click(saveButton());
    await waitFor(() => expect(second.calls).toHaveLength(1));
    expect((second.calls[0]!["provider"] as Record<string, unknown>)["headers"]).toEqual({});
  });

  it("密钥留空 = 不改：只 provider_upsert，一个字节都不碰密钥仓库", async () => {
    const upsert = recordingMock(rustBytedance());
    const secret = recordingMock(undefined);
    registerMock("provider_upsert", upsert.handler);
    registerMock("credential_set_secret", secret.handler);

    await openDetail();
    typeInto("providers-edit-name", "字节方舟 v2");
    fireEvent.click(saveButton());

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    expect(secret.calls).toHaveLength(0);
  });

  it("填了密钥才写：写的是这家**已有的那把**凭据，且明文不进载荷、不上屏、保存后清空", async () => {
    const upsert = recordingMock(rustBytedance());
    const credential = recordingMock({ id: "c-bytedance" });
    const secret = recordingMock(undefined);
    registerMock("provider_upsert", upsert.handler);
    registerMock("credential_upsert", credential.handler);
    registerMock("credential_set_secret", secret.handler);
    registerMock("credential_list", () => [boundCredential("c-bytedance", "p-bytedance")]);

    const KEY = "sk-detail-canary-0007"; // ta-allow-fake-secret —— 假值
    await openDetail();
    // 极简：**已经存在的凭据不再重复报一遍**（它就在下面那张凭据表里）。
    // 那句话只在"这次会新建一份凭据"时才有信息量；这里已有凭据 → 不许再出现。
    expect(screen.queryByText(/写入的凭据/)).toBeNull();
    // 而"写的是哪一把"这个事实仍在页面上（凭据表里有它的 id）

    typeInto("providers-edit-secret", KEY);
    fireEvent.click(saveButton());
    await waitFor(() => expect(secret.calls).toHaveLength(1));
    // 不新造 `<provider>-default`：写的是已有的那把
    expect(credential.calls).toHaveLength(0);
    expect(secret.calls[0]).toEqual({ id: "c-bytedance", plaintext: KEY });
    // 明文不进载荷、页面上任何地方都没有它
    expect(JSON.stringify(upsert.calls[0]!["provider"])).not.toContain(KEY);
    expect(document.body.textContent ?? "").not.toContain(KEY);
    // 写成功之后输入框清空（明文只活在内存里的那一次）
    await waitFor(() => expect(inputOf("providers-edit-secret").value).toBe(""));
  });

  it("这把服务商还没有凭据：填了密钥就先建一份默认凭据再写", async () => {
    const upsert = recordingMock(rustBytedance());
    const credential = recordingMock({ id: "p-bytedance-default" });
    const secret = recordingMock(undefined);
    registerMock("provider_upsert", upsert.handler);
    registerMock("credential_upsert", credential.handler);
    registerMock("credential_set_secret", secret.handler);
    registerMock("credential_list", () => []);

    const KEY = "sk-detail-canary-0008"; // ta-allow-fake-secret —— 假值
    await openDetail();
    // 界面上说清"这一填会新建哪一份凭据"（钉 id 本身，不钉那句话的措辞 —— 文案归 i18n）
    expect(screen.getByText(/p-bytedance-default/)).toBeTruthy();
    typeInto("providers-edit-secret", KEY);
    fireEvent.click(saveButton());

    await waitFor(() => expect(secret.calls).toHaveLength(1));
    expect(credential.calls[0]!["credential"]).toMatchObject({
      id: "p-bytedance-default",
      provider: "p-bytedance",
      secret: "${secret:p-bytedance-default}",
    });
    expect(secret.calls[0]).toEqual({ id: "p-bytedance-default", plaintext: KEY });
  });

  /**
   * 用户第 82 轮的重构：
   * 「添加服务商默认添加所有线路，然后通过滑块来'启用'和'关闭'……直接写在在一个列表里面，
   *  没有添加的线路也可以设置'网络代理'，只是有个滑块控制是否可见」
   *
   * 所以这一版：**扫描 = 扫上游 + 把目录里每条线路都建出来（默认关闭）**；
   * 列表里每行 = 上游原文 + 网络代理 + 启用/关闭的滑块；列表头上有**全选**
   * （一条批量命令，不是前端循环）。不再有"勾一下才创建"。
   */
  it(" 扫描把清单里的每条线路都建出来（默认关闭）；一个列表里未启用的行也能设代理；全选走批量命令", async () => {
    const scan = recordingMock({
      preview_token: "tok-1",
      models: [{ suggested_id: "m-new", upstream_model: "brand-new-model", exists: false }],
      skipped: [],
    });
    registerMock("model_import_from_provider", scan.handler);
    const createAll = recordingMock({ created: 1, total: 2 });
    registerMock("provider_lines_create_all", createAll.handler);
    const bulk = recordingMock({
      applied: true,
      restart_required: false,
      kernel_reload: false,
      channels_created: 0,
    });
    registerMock("channel_bulk_set_enabled", bulk.handler);
    // 目录里记着**上游原文**（键序都原样）；刻意给一个我们"不认识"的字段，
    // 证明页面是把 `raw` 原样画出来的，而不是只挑自己认识的键。
    const UPSTREAM_RAW = {
      id: "brand-new-model",
      object: "model",
      owned_by: "some-vendor-we-never-modeled",
      created: 1712345678,
    };
    registerMock("provider_model_catalog", () => [
      {
        provider_id: "p-bytedance",
        upstream_model: "brand-new-model",
        suggested_id: "m-new",
        seen_in_last_scan: true,
        first_seen_ms: 1,
        last_seen_ms: 2,
        added: true,
        channel_ids: ["ch-new"],
        enabled: false,
        raw: UPSTREAM_RAW,
      },
      {
        provider_id: "p-bytedance",
        upstream_model: BASELINE_MODEL,
        suggested_id: BASELINE_MODEL,
        seen_in_last_scan: true,
        first_seen_ms: 1,
        last_seen_ms: 2,
        added: true,
        channel_ids: ["ch-bytedance"],
        enabled: true,
        // 上游那一条不是对象 / 没记到 → `{}`：页面要**如实说**，不许编一个对象出来
        raw: {},
      },
    ]);
    registerMock("channel_list", () => [
      rustChannel(),
      rustChannel({ id: "ch-new", upstream_model: "brand-new-model", enabled: false, lane: null }),
    ]);

    await openDetail();

    // ① 扫描：先记录目录，再把缺的线路补建出来（两步各自是一件事实）
    fireEvent.click(screen.getByTestId("providers-model-scan"));
    await waitFor(() => expect(scan.calls).toHaveLength(1));
    expect(scan.calls[0]).toEqual({ provider_id: "p-bytedance" });
    await waitFor(() => expect(createAll.calls).toHaveLength(1));
    // 新线路的默认代理 = 这家服务商自己那条 `lane`（fixture 里是 `lane-direct`）；
    // 服务商没配就是 `null`（继承），**不编一个 id**
    expect(createAll.calls[0]).toEqual({ provider_id: "p-bytedance", lane: "lane-direct" });
    // 扫描结果如实说：扫到几个、这次新建几条、目录里共几条
    expect((await screen.findByTestId("providers-model-notice")).textContent).toContain("1");

    // ② 一个列表：两条线路都在，滑块反映真实状态
    const rows = await screen.findAllByTestId("providers-line-row");
    expect(rows).toHaveLength(2);
    const offRow = rows.find((r) => (r.textContent ?? "").includes("brand-new-model"))!;
    const onRow = rows.find((r) => (r.textContent ?? "").includes(BASELINE_MODEL))!;
    expect(
      (within(offRow).getByTestId("providers-line-enabled").querySelector("input") as HTMLInputElement)
        .checked,
    ).toBe(false);
    expect(
      (within(onRow).getByTestId("providers-line-enabled").querySelector("input") as HTMLInputElement)
        .checked,
    ).toBe(true);

    // **没有添加的线路（未启用）也能设网络代理** —— 用户原话
    // **线路不再有自己的代理**：出口只由服务商决定（同一服务商一个 URL，
    // 不同出口 IP 会被上游当成"同账号多地并发"）→ 行里**没有**代理下拉。
    expect(within(offRow).queryByTestId("providers-line-lane")).toBeNull();
    // **列头里也不许再有「网络代理」那一列** —— 第 88 轮只删了行内控件，
    // 列头漏了一处（替换静默失败，`tsc` 不报），用户第 117 轮看出来的。这条断言就是防它再回来。
    const header = screen.getByTestId("providers-lines-header");
    expect(header.textContent).toContain("名称");
    expect(header.textContent).not.toContain("网络代理");
    // 行里**不许**显示上游原文 / 线路 id / 优先级权重 —— 它们在点开的「详情」里。
    expect(within(offRow).queryByTestId("providers-line-raw-json")).toBeNull();
    expect(within(offRow).queryByTestId("providers-line-id")).toBeNull();
    expect(within(offRow).queryByTestId("providers-line-priority")).toBeNull();
    // 但"点一下详情看原文"这条路还在（上一轮用户认可的那条路径）
    fireEvent.click(within(offRow).getByTestId("providers-line-info"));
    const panel = await within(offRow).findByTestId("providers-line-info-panel");
    const rawJson = within(panel).getByTestId("providers-line-raw-json").textContent ?? "";
    expect(JSON.parse(rawJson)).toEqual(UPSTREAM_RAW);
    expect(rawJson.indexOf('"id"')).toBeLessThan(rawJson.indexOf('"owned_by"'));
    expect(within(panel).getByTestId("providers-line-id")).toBeTruthy();
    expect(within(panel).getByTestId("providers-line-priority").textContent).toBe("100 / 10");
    // 目录里记的是 `{}` → 详情里如实说"上游没给对象"，不画空 JSON 框
    fireEvent.click(within(onRow).getByTestId("providers-line-info"));
    const onPanel = await within(onRow).findByTestId("providers-line-info-panel");
    expect(within(onPanel).queryByTestId("providers-line-raw-json")).toBeNull();
    expect(within(onPanel).getByTestId("providers-line-raw-empty").textContent).toBeTruthy();

    // ③ 全选：计数如实 + 一次动作 → 一条批量命令（不是前端循环）
    expect(screen.getByTestId("providers-lines-all-count").textContent).toContain("1/2");
    fireEvent.click(
      within(screen.getByTestId("providers-lines-all")).getByRole("switch"),
    );
    await waitFor(() => expect(bulk.calls).toHaveLength(1));
    expect(bulk.calls[0]).toEqual({ channel_ids: ["ch-bytedance", "ch-new"], enabled: true });
  });

  it(" 目录里没记到上游原文（`{}`）→ 行里如实说「上游没给对象」，不编一个出来", async () => {
    registerMock("provider_model_catalog", () => [
      {
        provider_id: "p-bytedance",
        upstream_model: BASELINE_MODEL,
        suggested_id: BASELINE_MODEL,
        seen_in_last_scan: true,
        first_seen_ms: 1,
        last_seen_ms: 2,
        added: true,
        channel_ids: ["ch-bytedance"],
        enabled: true,
        raw: {},
      },
    ]);
    registerMock("channel_list", () => [rustChannel()]);

    await openDetail();

    const row = (await screen.findAllByTestId("providers-line-row"))[0]!;
    // 列表行里没有原文块；点开详情后：没有任何 JSON 框（不画 `{}` 冒充"上游说了个空对象"），
    // 只有那句实话
    expect(within(row).queryByTestId("providers-line-raw-json")).toBeNull();
    fireEvent.click(within(row).getByTestId("providers-line-info"));
    const panel = await within(row).findByTestId("providers-line-info-panel");
    expect(within(panel).queryByTestId("providers-line-raw-json")).toBeNull();
    expect(within(panel).getByTestId("providers-line-raw-empty").textContent).toBeTruthy();
  });

  it(" 线路列表：一行一条（上游模型名 / 网络代理 / 优先级权重 / 启用），启停真调 channel_set_enabled", async () => {
    const setEnabled = recordingMock(undefined);
    registerMock("channel_set_enabled", setEnabled.handler);
    registerMock("provider_model_catalog", () => [
      {
        provider_id: "p-bytedance",
        upstream_model: BASELINE_MODEL,
        suggested_id: BASELINE_MODEL,
        seen_in_last_scan: false,
        first_seen_ms: 1,
        last_seen_ms: 2,
        added: true,
        channel_ids: ["ch-bytedance"],
        enabled: true,
      },
    ]);
    registerMock("channel_list", () => [rustChannel()]);

    await openDetail();

    const row = (await screen.findAllByTestId("providers-line-row"))[0]!;
    // 列表行 = 名字 + 网络代理 + 滑块（用户原话："一条就是左边名字，
    // 右边选择滑块就好了"）：**id / 优先级权重不在行里**
    expect(within(row).getByTestId("providers-line-upstream").textContent).toBe(BASELINE_MODEL);
    expect(within(row).queryByTestId("providers-line-id")).toBeNull();
    expect(within(row).queryByTestId("providers-line-priority")).toBeNull();
    // 网络代理：这一行**自己的** lane，下拉里显示的是线路名（不是 id）
    // 线路行里不再有代理（list 只放名字 + 滑块）
    expect(within(row).queryByTestId("providers-line-lane")).toBeNull();
    expect(within(row).getByTestId("providers-line-stale")).toBeTruthy();
    // 启用状态来自配置本身
    expect(
      (within(row).getByTestId("providers-line-enabled").querySelector("input") as HTMLInputElement)
        .checked,
    ).toBe(true);
    // 点开「详情」：线路 id 与优先级权重在这里（证据一个不少，只是不占列表版面）
    fireEvent.click(within(row).getByTestId("providers-line-info"));
    const panel = await within(row).findByTestId("providers-line-info-panel");
    expect(within(panel).getByTestId("providers-line-id").textContent).toBe("ch-bytedance");
    expect(within(panel).getByTestId("providers-line-priority").textContent).toBe("100 / 10");

    // 停用 → `channel_set_enabled({id, enabled:false})`（可逆，绝不删）
    fireEvent.click(within(row).getByTestId("providers-line-enabled").querySelector("input")!);
    await waitFor(() => expect(setEnabled.calls).toHaveLength(1));
    expect(setEnabled.calls[0]).toEqual({ id: "ch-bytedance", enabled: false });
  });




  it(" 移除线路：先确认、写清删的是哪条、channel_delete 没有 force 所以也没有级联按钮", async () => {
    const remove = recordingMock(undefined);
    registerMock("channel_delete", remove.handler);
    registerMock("channel_list", () => [rustChannel()]);

    await openDetail();

    const row = (await screen.findAllByTestId("providers-line-row"))[0]!;
    // 「移除线路」在**点开的详情**里（：列表行只放名字 + 代理 + 滑块）
    fireEvent.click(within(row).getByTestId("providers-line-info"));
    fireEvent.click(within(row).getByTestId("providers-line-remove"));

    expect(screen.getByTestId("providers-line-delete-dialog")).toBeTruthy();
    const target = screen.getByTestId("providers-line-delete-target").textContent ?? "";
    expect(target).toContain(BASELINE_MODEL);
    expect(target).toContain("ch-bytedance");
    // 确认之前一条命令都不发
    expect(remove.calls).toHaveLength(0);

    fireEvent.click(screen.getByTestId("providers-line-delete-confirm"));
    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "ch-bytedance" });
    // 成功的事实写在结果区（只讲刚发生的事）
    expect((await screen.findByTestId("providers-line-delete-result")).textContent).toContain("已删除");
    // `channel_delete` 没有 `force` → 级联按钮永远不出现（那是假按钮）
    expect(screen.queryByTestId("providers-line-delete-cascade")).toBeNull();
  });

  it(" 移除被策略点名引用：原样显示后端原文，且仍然没有级联按钮", async () => {
    registerMock("channel_delete", () => {
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "channel ch-bytedance 仍被 1 条策略引用（pol-default）",
      });
    });
    registerMock("channel_list", () => [rustChannel()]);

    await openDetail();
    const removeRow = (await screen.findAllByTestId("providers-line-row"))[0]!;
    fireEvent.click(within(removeRow).getByTestId("providers-line-info"));
    fireEvent.click(within(removeRow).getByTestId("providers-line-remove"));
    fireEvent.click(screen.getByTestId("providers-line-delete-confirm"));

    const error = await screen.findByTestId("providers-line-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("仍被 1 条策略引用（pol-default）");
    // 失败不许写成功
    expect(screen.queryByTestId("providers-line-delete-result")).toBeNull();
    expect(screen.queryByTestId("providers-line-delete-cascade")).toBeNull();
  });

  it(" 重新扫描真调 model_import_from_provider，并把「已全部记录」如实说出来", async () => {
    const scan = recordingMock({
      preview_token: "tok-1",
      models: [{ suggested_id: "m-new", upstream_model: "brand-new-model", exists: false }],
      skipped: [{ upstream_model: "bad model", reason: "名称不合法" }],
    });
    registerMock("model_import_from_provider", scan.handler);

    await openDetail();
    fireEvent.click(await screen.findByTestId("providers-model-scan"));

    await waitFor(() => expect(scan.calls).toHaveLength(1));
    expect(scan.calls[0]).toEqual({ provider_id: "p-bytedance" });
    // 1 个模型 + 1 条派生不出 id 的，都记下来了 —— 数字如实说
    const notice = await screen.findByTestId("providers-model-notice");
    expect(notice.textContent).toContain("2");
  });

  it(" 空态与读不到是两件事：目录空 / 没有线路 / 线路清单读不到，各有各的话", async () => {
    // ① 目录还没扫过 + 这一家一条线路都没有
    // 目录读到了但**是空的** —— 与"读不到"是两件事，页面各说各的话（见下面的 ②）。
    // 不注册这条 mock 的话 `catalog` 是 `undefined`（读不到），落到的是另一条分支。
    registerMock("provider_model_catalog", () => []);
    registerMock("channel_list", () => []);
    await openDetail();
    expect(await screen.findByTestId("providers-model-empty")).toBeTruthy();
    expect(screen.queryAllByTestId("providers-model-row")).toHaveLength(0);
    // 没有线路 ≠ 画一行破折号：如实说"到上面扫描入口去创建"
    expect(screen.getByTestId("providers-lines-empty")).toBeTruthy();
    expect(screen.queryAllByTestId("providers-line-row")).toHaveLength(0);
    cleanup();

    // ② `channel_list` 读失败：后端原文照旧摆出来 + 说"读不到线路清单"（**不是**"没有线路"）
    registerBaselineMocks();
    registerMock("provider_list", () => [rustBytedance()]);
    registerMock("channel_list", () => {
      throw new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：channel_list" });
    });
    await openDetail();
    expect((await screen.findByTestId("providers-lines-error")).textContent).toContain("TA-9001");
    expect(screen.getByTestId("providers-lines-unreadable")).toBeTruthy();
    expect(screen.queryByTestId("providers-lines-empty")).toBeNull();
    expect(screen.queryAllByTestId("providers-line-row")).toHaveLength(0);
  });
});

// ================================================================ ⑩ 删除服务商 / 凭据
// 为什么这一节全部走**整页**（`renderAt`）而不是只看 `ProvidersBody`：
// 「删除」的语义一半在展示层（行内按钮 / 多选 / 确认框），一半在数据层
// （载荷到底是 `{id, force:false}` 还是 `force:true`、成功后失效哪些查询、后端原文怎么摆出来）。
// 只测展示层的话，"点了到底发了什么"完全不设防 —— 而那恰好是 的核心。

/**
 * 多选复选框：`data-testid` 挂在 MUI `Checkbox` 的根 `span` 上，真正的 `input` 在它里面。
 *
 * 用 `[data-testid="X"] input` 这个选择器而不是 `getByRole("checkbox")`：真机 E2E 的
 * `wdFillByTid` 就是**同一个**口径（见本文件 §⑥ 的说明），单测照抄它，两边不会各认一套。
 */
function selectBoxAt(testId: string, index: number): HTMLInputElement {
  const host = screen.getAllByTestId(testId)[index];
  if (host === undefined) throw new Error(`没有第 ${index} 个 [data-testid="${testId}"]`);
  const input = host.getAttribute("type") === "checkbox" ? host : host.querySelector("input");
  if (input === null) throw new Error(`[data-testid="${testId}"] 里没有 input`);
  return input as HTMLInputElement;
}

describe(" 服务商删除：先确认、再 force=false、被引用才给级联", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("列表每行有 providers-delete；点了先弹确认框（写清名称 + id），此时一个删除命令都没发", async () => {
    const remove = recordingMock(undefined);
    registerMock("provider_delete", remove.handler);

    renderAt("/providers");
    const buttons = await screen.findAllByTestId("providers-delete");
    expect(buttons).toHaveLength(2);
    expect(buttons[0]!.tagName).toBe("BUTTON");

    fireEvent.click(buttons[0]!);

    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    const target = screen.getByTestId("providers-delete-target");
    expect(target.textContent).toContain("字节方舟");
    expect(target.textContent).toContain("p-bytedance");
    // **确认框必须在命令之前**：此刻一条都没发
    expect(remove.calls).toHaveLength(0);
  });

  it("确认 → provider_delete({id, force:false})；成功后结果区写「已删除 <名称>」并失效相关查询", async () => {
    const remove = recordingMock(undefined);
    registerMock("provider_delete", remove.handler);
    const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");

    renderAt("/providers");
    fireEvent.click((await screen.findAllByTestId("providers-delete"))[0]!);
    fireEvent.click(screen.getByTestId("providers-delete-confirm"));

    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "p-bytedance", force: false });

    const result = await screen.findByTestId("providers-delete-result");
    expect(result.textContent).toContain("已删除 字节方舟");
    // 成功后确认框收起来（MUI 的 Dialog 有一个退场过渡，所以要等它真的卸载）
    await waitFor(() => expect(screen.queryByTestId("providers-delete-dialog")).toBeNull());

    // 受影响的查询真的失效
    for (const key of [qk.providers, qk.credentials, qk.models, qk.channelMatrix]) {
      const hits = invalidate.mock.calls.filter(
        ([filters]) => JSON.stringify(filters?.queryKey) === JSON.stringify(key),
      );
      expect(hits.length, `没有失效 ${JSON.stringify(key)}`).toBeGreaterThan(0);
    }
    invalidate.mockRestore();
  });

  it("被引用：原样显示后端原文（含 TA 码），此时才出现级联按钮，且不假装成功", async () => {
    registerMock("provider_delete", () => {
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "providers[0] p-bytedance 仍被 1 条策略引用（pol-default）",
      });
    });

    renderAt("/providers");
    fireEvent.click((await screen.findAllByTestId("providers-delete"))[0]!);
    // 还没拿到"被引用"这句话之前，级联按钮**不许**出现
    expect(screen.queryByTestId("providers-delete-cascade")).toBeNull();

    fireEvent.click(screen.getByTestId("providers-delete-confirm"));

    const error = await screen.findByTestId("providers-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("仍被 1 条策略引用（pol-default）");
    // 确认框仍然开着（用户要能选择级联），并且这时才有级联按钮
    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("providers-delete-cascade")).toBeTruthy();
    // 失败不许写成功
    expect(screen.queryByTestId("providers-delete-result")).toBeNull();
  });

  it("级联按钮走 force=true（与第一次的载荷不是同一个）", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("provider_delete", (args) => {
      calls.push(args ?? {});
      if (args?.["force"] !== true) {
        throw new TokenAgencyError({ code: "TA-5002", message: "仍被引用" });
      }
      return undefined;
    });

    renderAt("/providers");
    fireEvent.click((await screen.findAllByTestId("providers-delete"))[0]!);
    fireEvent.click(screen.getByTestId("providers-delete-confirm"));
    await screen.findByTestId("providers-delete-cascade");
    fireEvent.click(screen.getByTestId("providers-delete-cascade"));

    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[0]).toEqual({ id: "p-bytedance", force: false });
    expect(calls[1]).toEqual({ id: "p-bytedance", force: true });
    expect((await screen.findByTestId("providers-delete-result")).textContent).toContain("已删除");
  });

  it("「会连带删除」清单只用前端算得出来的事实（该服务商下的凭据数 / 通道数）", async () => {
    renderAt("/providers");
    fireEvent.click((await screen.findAllByTestId("providers-delete"))[0]!);

    const listed = screen.getByTestId("providers-delete-consequences").textContent ?? "";
    // 基准 mock：c-bytedance 挂在 p-bytedance 下（凭据数来自 credential_list）
    expect(listed).toContain("1");
    expect(listed).toContain("凭据");
    // 线路数来自 provider_list 的 channel_count 这一列。
    // 名词（「线路」/「通道」）归 i18n 所有，这里钉的是"这一维被说出来了" ——
    // 把它收进问号、或干脆不提，才是这条断言要拦的缺陷。
    expect(listed).toMatch(/线路|通道/);
    // 本页读不到策略清单 → **不许编**"被 N 条策略引用"
    expect(listed).not.toContain("策略");
  });

  it("批量：先确认，再逐条 force=false；一条失败不中止其余条；如实汇报成功/失败并逐条给 id + 后端原文", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("provider_delete", (args) => {
      calls.push(args ?? {});
      if (args?.["id"] === "p-commandcode") {
        throw new TokenAgencyError({ code: "TA-5002", message: "仍被 1 条通道引用" });
      }
      return undefined;
    });

    renderAt("/providers");
    await screen.findAllByTestId("providers-delete");
    fireEvent.click(selectBoxAt("providers-select", 0));
    fireEvent.click(selectBoxAt("providers-select", 1));
    expect(screen.getByTestId("providers-selected-count").textContent).toContain("2");

    fireEvent.click(screen.getByTestId("providers-delete-selected"));
    // 批量也要先确认
    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    expect(calls).toHaveLength(0);
    // 批量框里写清删的是哪几条（名称 + id）
    const target = screen.getByTestId("providers-delete-target").textContent ?? "";
    expect(target).toContain("p-bytedance");
    expect(target).toContain("p-commandcode");

    fireEvent.click(screen.getByTestId("providers-delete-confirm"));

    await waitFor(() => expect(calls).toHaveLength(2));
    // 逐条调，一条失败也不中止后面那条；**两条都是 force=false**
    expect(calls).toEqual([
      { id: "p-bytedance", force: false },
      { id: "p-commandcode", force: false },
    ]);

    const result = await screen.findByTestId("providers-delete-result");
    const text = result.textContent ?? "";
    // 不许写一句"批量删除成功"
    expect(text).toContain("成功 1 条，失败 1 条");
    expect(text).not.toContain("批量删除成功");
    // 失败项逐条给 id + 后端原文
    expect(text).toContain("p-commandcode");
    expect(text).toContain("TA-5002");
    expect(text).toContain("仍被 1 条通道引用");
  });

  it("表头全选（providers-select-all）把当前列表全选上；一行都没选时「删除选中」不发命令", async () => {
    const remove = recordingMock(undefined);
    registerMock("provider_delete", remove.handler);

    renderAt("/providers");
    await screen.findAllByTestId("providers-delete");

    // ① 一行都没选：点「删除选中」不该弹框、更不该发命令
    fireEvent.click(screen.getByTestId("providers-delete-selected"));
    expect(screen.queryByTestId("providers-delete-dialog")).toBeNull();
    expect(remove.calls).toHaveLength(0);

    // ② 表头全选
    fireEvent.click(selectBoxAt("providers-select-all", 0));
    expect(screen.getByTestId("providers-selected-count").textContent).toContain("2");
    expect(selectBoxAt("providers-select", 0).checked).toBe(true);
    expect(selectBoxAt("providers-select", 1).checked).toBe(true);
  });
});

describe(" 详情页删除：页头删服务商（凭据表随「一把密钥」下线）", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("页头有 providers-detail-delete；确认后 provider_delete({id, force:false})", async () => {
    const remove = recordingMock(undefined);
    registerMock("provider_delete", remove.handler);

    renderAt("/providers/p-bytedance");
    fireEvent.click(await screen.findByTestId("providers-detail-delete"));

    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    expect(screen.getByTestId("providers-delete-target").textContent).toContain("p-bytedance");
    expect(remove.calls).toHaveLength(0);

    fireEvent.click(screen.getByTestId("providers-delete-confirm"));
    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "p-bytedance", force: false });
  });

  it("页头删服务商被引用时：原文原样显示，级联按钮是 §3 点名的 providers-delete-cascade（force=true）", async () => {
    const calls: Record<string, unknown>[] = [];
    registerMock("provider_delete", (args) => {
      calls.push(args ?? {});
      if (args?.["force"] !== true) {
        throw new TokenAgencyError({ code: "TA-5002", message: "仍被 1 条策略引用" });
      }
      return undefined;
    });

    renderAt("/providers/p-bytedance");
    fireEvent.click(await screen.findByTestId("providers-detail-delete"));
    // 拿到"被引用"之前没有级联按钮
    expect(screen.queryByTestId("providers-delete-cascade")).toBeNull();

    fireEvent.click(screen.getByTestId("providers-delete-confirm"));
    const error = await screen.findByTestId("providers-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("仍被 1 条策略引用");

    fireEvent.click(await screen.findByTestId("providers-delete-cascade"));
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[0]).toEqual({ id: "p-bytedance", force: false });
    expect(calls[1]).toEqual({ id: "p-bytedance", force: true });
  });

  it(" 凭据表下线：这一页没有任何凭据多选 / 批量删除 / credential_delete 调用点", async () => {
    const remove = recordingMock(undefined);
    registerMock("credential_delete", remove.handler);

    renderAt("/providers/p-bytedance");
    await screen.findByTestId("providers-key");

    // 凭据表、行、多选、批量删除 —— 一个都不许留下（一个服务商就一把密钥）
    for (const gone of [
      "providers-credentials-table",
      "providers-credential-row",
      "providers-credential-select",
      "providers-credential-select-all",
      "providers-credential-selected-count",
      "providers-credential-delete",
      "providers-credential-delete-selected",
      "providers-credential-delete-dialog",
    ]) {
      expect(screen.queryByTestId(gone), `${gone} 应当随凭据表下线`).toBeNull();
    }
    // 页面只剩两条删除入口：删服务商（页头）+ 移除线路（每行）
    expect(screen.getByTestId("providers-detail-delete")).toBeTruthy();
    // 而这两条都还没有被点过：一条删除命令都没发
    expect(remove.calls).toHaveLength(0);

    // 要撤掉这把密钥 = 删掉这个服务商；页面**不**提供"删掉唯一一把密钥"的按钮
    fireEvent.click(screen.getByTestId("providers-detail-delete"));
    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    expect(remove.calls).toHaveLength(0);
  });

  it(" 旧数据里挂了两份凭据：页面如实说，但仍然不退回表格 / 批量删除", async () => {
    registerMock("credential_list", () => [
      boundCredential("c-a", "p-bytedance"),
      boundCredential("c-b", "p-bytedance"),
    ]);

    renderAt("/providers/p-bytedance");
    const multiple = await screen.findByTestId("providers-key-multiple");
    // 文案键还没落地 → 只钉这句话**出现**了（不该悄悄只显示第一份）
    expect(multiple).toBeTruthy();

    expect(screen.queryByTestId("providers-credentials-table")).toBeNull();
    expect(screen.queryByTestId("providers-credential-delete-selected")).toBeNull();
    expect(screen.queryByTestId("providers-credential-select-all")).toBeNull();
  });
});

// ================================================================ ⑦ 长解释收进问号圆圈

/**
 *（用户第 74 轮：「简介好长，好多字……做成一个问号加一个圆圈」）的验收。
 *
 * 这一节只钉**两件事**：
 * 1. 解释**收走了**：`?` 存在、`aria-label` 是全文（正文一个字都没丢）、那句正文**不再内联**；
 * 2. 红线**没被收走**（六条）：空态 / 后端原文（TA 码）/ 破坏性后果 / 数值 / 按钮文案
 *    仍然一眼可见 —— 把错误塞进 tooltip 是"藏问题"，不是排版。
 *
 * 既有用例（含第 73 轮的删除那一批）一条都没动：这里只加断言，不放宽任何东西。
 */
describe(" 长解释收进问号圆圈，红线一律留在版面上", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  /** 取某个 `?` 的 `aria-label`（= 提示正文全文，提示气泡本身是懒渲染的）。 */
  function helpLabel(testId: string): string {
    const label = screen.getByTestId(testId).getAttribute("aria-label");
    expect(label, `${testId} 的 aria-label 必须是提示全文`).toBeTruthy();
    return label ?? "";
  }

  it("列表页：四句诚实说明合成**一个** providers-table-help（换行分隔），正文不再内联", () => {
    renderList({
      rows: [readProvider(viewProvider())],
      credentials: [readCredential(viewCredential())],
    });

    // 先钉"不再内联"（旧代码上这一条先红：四行灰字正铺在表的下面）
    expect(screen.queryByText(copy("providers.honesty.status"))).toBeNull();

    const label = helpLabel("providers-table-help");
    expect(label).toContain(copy("providers.honesty.status"));
    expect(label).toContain(copy("providers.honesty.fields"));
    expect(label).toContain(copy("providers.honesty.mounted"));
    expect(label).toContain("点名称进入详情");
    // 多句合成一条、一个区块只有一个问号（：一排问号比一排字更糟）
    expect(label).toContain("\n");
    expect(screen.getAllByTestId("providers-table-help")).toHaveLength(1);

    // 正文不再内联（原来的四行灰字一行都不剩）
    expect(screen.queryByText(copy("providers.honesty.fields"))).toBeNull();
    expect(screen.queryByText(copy("providers.honesty.mounted"))).toBeNull();
    expect(screen.queryByText(/点名称进入详情/)).toBeNull();
  });

  it("列表页红线仍内联：计数与单位、空态、TA 码都没被收进去", () => {
    renderList({
      rows: [readProvider(viewProvider())],
      credentials: [readCredential(viewCredential())],
    });
    // §2 第 5 条：任何数值与单位一眼可见
    expect(screen.getByText("共 1 个服务商")).toBeTruthy();
    expect(
      screen.getByText(copyVars("providers.mount.both", { credentials: 1, channels: 3 })),
    ).toBeTruthy();
    const withData = helpLabel("providers-table-help");
    expect(withData).not.toContain("个服务商");
    // 数值与单位留在版面上：提示里不许出现**这一格渲染出来的那句话**
    expect(withData).not.toContain(
      copyVars("providers.mount.both", { credentials: 1, channels: 3 }),
    );
    cleanup();

    // §2 第 1 条 + 第 2 轮判据 A：空态是**一句结论**，不是解释、也不再重复页头的动作
    renderList();
    const empty = screen.getByTestId("providers-empty");
    expect(empty.textContent).toBe("配置里还没有服务商");
    expect(within(empty).queryByRole("button")).toBeNull();
    // 旧版那句"服务商来自配置文件的 providers 段……尚未接线"是**解释**，已整条下线
    expect(screen.queryByText(/providers 段/)).toBeNull();
    expect(helpLabel("providers-table-help")).not.toContain("配置里还没有服务商");
    cleanup();

    // §2 第 2 条：后端原文（含 TA 码）原样可见
    renderList({ error: new TokenAgencyError({ code: "TA-5001", message: "配置校验失败" }) });
    expect(screen.getByTestId("providers-error").textContent).toContain("TA-5001");
    expect(helpLabel("providers-table-help")).not.toContain("TA-5001");
  });

  it("详情页：六个区块各一个问号（含线路与密钥），长解释不再内联", () => {
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      credentials: [readCredential(rustCredential())],
      laneName: (lane) => (lane === "lane-jp" ? "日本线" : undefined),
    });

    // 先钉"不再内联"（旧代码上这一条先红：概览下面正铺着那句灰字）
    expect(screen.queryByText(copy("providers.honesty.detailFields"))).toBeNull();

    expect(helpLabel("providers-overview-help")).toContain(copy("providers.honesty.detailFields"));
    const connection = helpLabel("providers-connection-help");
    // 第 2 轮：`connection.partial` 这一条里同时放了「—」的含义与读取超时的结论；
    // 「不会用默认值覆盖线上值」那条**破坏性后果**留在版面上（见下面 `providers-connection-protect`）。
    expect(connection).toContain(copy("providers.connection.partial"));
    expect(connection).toContain(copy("providers.tls.extraCaHint"));
    expect(connection).not.toContain(copy("providers.connection.hint"));
    expect(connection).toContain("\n");

    expect(helpLabel("providers-headers-help")).toContain(copy("providers.headers.hint"));
    expect(helpLabel("providers-key-help")).toContain(copy("providers.credentials.secretHint"));
    // 线路区是这一页的主体：它**有**自己的那一个问号
    expect(helpLabel("providers-lines-help")).toBeTruthy();
    expect(helpLabel("providers-raw-section-help")).toContain("原样显示（未做任何清洗与删减）");

    // 每个区块**只有**一个问号
    for (const testId of [
      "providers-overview-help",
      "providers-connection-help",
      "providers-headers-help",
      "providers-key-help",
      "providers-lines-help",
      "providers-raw-section-help",
    ]) {
      expect(screen.getAllByTestId(testId), `${testId} 只能有一个`).toHaveLength(1);
    }

    // 正文不再内联
    expect(screen.queryByText(copy("providers.honesty.detailFields"))).toBeNull();
    expect(screen.queryByText(copy("providers.connection.partial"))).toBeNull();
    expect(screen.queryByText(copy("providers.tls.extraCaHint"))).toBeNull();
    expect(screen.queryByText(copy("providers.headers.hint"))).toBeNull();
    expect(screen.queryByText(copy("providers.credentials.secretHint"))).toBeNull();
    expect(screen.queryByText(/原样显示（未做任何清洗与删减）/)).toBeNull();
  });

  it("详情页红线仍内联：不可用结论、破坏性后果与数值都没被收进去", () => {
    // §2 第 1 条：「连接配置没读到」是一句**结论**，必须一眼可见
    renderDetail({ id: "p-view", row: readProvider(viewProvider()) });
    expect(screen.getByTestId("providers-connection-unavailable")).toBeTruthy();
    expect(screen.getByText(copy("providers.connection.unavailable"))).toBeTruthy();
    // 结论**不许**被收进 tooltip（收了就是"藏问题"）
    expect(helpLabel("providers-connection-help")).not.toContain(copy("providers.connection.unavailable"));
    // 只读时**不重复**那句承诺：上面 `providers-view-only` 已经说过了（同一屏同一事实说一遍）
    expect(screen.queryByTestId("providers-connection-protect")).toBeNull();
    // 可编辑的那一行必须看得见这条承诺（第 2 轮补丁：破坏性后果留在版面，原因才进 `?`）
    cleanup();
    renderDetail({ id: "p-rust", row: readProvider(rustProvider()), onSave: async () => {} });
    expect(screen.getByTestId("providers-connection-protect").textContent).toBe(
      copy("providers.connection.hint"),
    );
    expect(helpLabel("providers-connection-help")).not.toContain(copy("providers.connection.hint"));
    expect(screen.queryByTestId("providers-view-only")).toBeNull();
    cleanup();

    // §2 第 5 条：数值与单位（超时秒数在控件里也是**一眼可见**的值，不是藏在提示里的）
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      credentials: [readCredential(rustCredential())],
      laneName: () => undefined,
      onSave: async () => {},
    });
    expect(inputOf("providers-edit-timeoutConnect").value).toBe("10");
    expect(inputOf("providers-edit-timeoutRead").value).toBe("120");
    expect(helpLabel("providers-connection-help")).not.toContain("120");
    // 密钥**引用**（`${secret:rust}`）原样留在版面上 —— 它是这一条凭据的状态，不是解释
    expect(screen.getByTestId("providers-key-secret").textContent).toBe("${secret:rust}");
  });

  it("详情页：连接配置没返回时，概览与附加头区块不画问号也不留空占位", () => {
    renderDetail({ id: "p-view", row: readProvider(viewProvider()) });
    // 视图形状没有附加头 → 那个区块整个不出现（不是"一个问号 + 空白"）
    expect(screen.queryByTestId("providers-headers")).toBeNull();
    expect(screen.queryByTestId("providers-headers-help")).toBeNull();
  });

  it("删除确认框的破坏性后果与按钮文案仍然一眼可见（§2 第 4、6 条）", async () => {
    const remove = recordingMock(undefined);
    registerMock("provider_delete", remove.handler);

    renderAt("/providers");
    fireEvent.click((await screen.findAllByTestId("providers-delete"))[0]!);

    // 第 73 轮刚落地的"会连带删除 / 影响"清单：一个字都不许收进 tooltip
    expect(screen.getByTestId("providers-delete-dialog")).toBeTruthy();
    expect(screen.getByText("删除后无法撤销。确认删除以下对象？")).toBeTruthy();
    expect(screen.getByText("会连带删除 / 影响：")).toBeTruthy();
    const consequences = screen.getByTestId("providers-delete-consequences").textContent ?? "";
    expect(consequences).toContain("凭据");
    expect(consequences).toMatch(/线路|通道/);

    // 按钮文案永远留在按钮上
    expect(screen.getByTestId("providers-delete-confirm").textContent).toBe("删除");
    expect(screen.getByTestId("providers-delete-cancel").textContent).toBe("取消");
    expect(screen.getByTestId("providers-delete-selected").textContent).toBe("删除选中");

    // 页面上任何一个问号的正文都不是这段警告
    for (const help of screen.queryAllByTestId(/-help$/)) {
      expect(help.getAttribute("aria-label") ?? "").not.toContain("会连带删除");
      expect(help.getAttribute("aria-label") ?? "").not.toContain("无法撤销");
    }
    expect(remove.calls).toHaveLength(0);
  });
});

// ================================================================ 一级界面省略

describe(" 服务商名：省略后两条恢复路径都在（进详情 + 悬停/聚焦提示）", () => {
  it("长名字整段留在 DOM 里；没溢出时既不挂提示，也不是 Tab 站", () => {
    const name = "自建网关（东京 B 机 · 团队 A 专用 · 仅供白天批处理任务）";
    renderList({ rows: [readProvider(rustProvider({ id: "p-long", name }))] });

    const element = screen.getByTestId("providers-name-ellipsis");
    // 名字还可以照旧点进详情页；DOM 里也是完整原文（省略只发生在 CSS 层）
    expect(element.textContent).toBe(name);
    expect(screen.getByRole("button", { name })).toBeTruthy();
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(element.getAttribute("tabindex")).toBeNull();
  });
});

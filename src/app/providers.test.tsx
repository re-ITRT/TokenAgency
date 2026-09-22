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
 * 5. **添加向导（[docs/17 §3.1] + [docs/19 §1]）是真入口**：`provider_upsert` / `credential_upsert` /
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
import ProviderDetailPage, {
  ProviderDetailBody,
  type ProviderDetailBodyProps,
} from "@/pages/providers/detail";
import { mountOf, readCredential, readProvider } from "@/pages/providers/model";
import { resetWizardSession } from "@/pages/providers/providerWizardModel";
import { knownPages, resolvePage } from "@/app/pageRegistry";
import { routeByKey } from "@/app/routes";
import { TokenAgencyError, registerMock } from "@/shared/api";
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
      quota: {
        metric: "tokens",
        limit: 5_000_000,
        currency: "CNY",
        window: "month",
        reset_day: 1,
        soft_limit: 0.8,
        hard_limit: 0.98,
        reserve: 0.05,
      },
      guard: { max_rpm: 60, max_concurrency: 8, max_input_tokens: null, on_exceed: "wait" },
      tags: ["cn-direct"],
      enabled: true,
    },
    quota_state: { credential_id: "c-rust", state: "soft", used_tokens: 4_100_000 },
    used_pct: 0.82,
    ...overrides,
  };
}

function viewCredential(overrides: Record<string, unknown> = {}): unknown {
  return {
    credential: {
      id: "c-view",
      provider: "p-view",
      label: "command code · 按量",
      secret: "${secret:commandcode}",
      lane: "lane-jp",
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
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "next_candidate" },
      tags: ["backup"],
      enabled: true,
    },
    provider_name: "command code",
    quota: { credential_id: "c-view", state: "ok", used_cost: 4.2 },
    channel_count: 1,
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
  it("Rust 形状：used_pct 与 quota_state 都读得到", () => {
    const row = readCredential(rustCredential());
    expect(row.id).toBe("c-rust");
    expect(row.provider).toBe("p-rust");
    expect(row.usedPct).toBe(0.82);
    expect(row.quotaState).toBe("soft");
    expect(row.limit).toBe(5_000_000);
    expect(row.metric).toBe("tokens");
    expect(row.window).toBe("month");
    expect(row.enabled).toBe(true);
    expect(row.tags).toEqual(["cn-direct"]);
  });

  it("契约/mock 形状：只有 quota.state，没有 used_pct → usedPct 留 undefined（不推断比例）", () => {
    const row = readCredential(viewCredential());
    expect(row.usedPct).toBeUndefined();
    expect(row.quotaState).toBe("ok");
    expect(row.limit).toBe(20);
    expect(row.metric).toBe("cost");
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
    laneName: () => undefined,
    laneError: undefined,
    onBack: () => {},
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
  it("目录为空时给诚实空状态，不画 0 行的表", () => {
    renderList();

    expect(screen.getByTestId("providers-empty")).toBeTruthy();
    expect(screen.getByText("配置里还没有服务商")).toBeTruthy();
    expect(screen.queryByTestId("providers-table")).toBeNull();
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
    expect(screen.getByText("1 个凭据 · 3 条通道")).toBeTruthy();
    cleanup();

    renderList({ rows, credentials: undefined });
    expect(screen.getByText("凭据数未知 · 3 条通道")).toBeTruthy();
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

  it("点名称进入详情；列表行上没有测试/编辑/删除按钮（本轮只接「添加」入口）", () => {
    const onOpen = vi.fn();
    renderList({ rows: [readProvider(viewProvider())], onOpen });

    fireEvent.click(screen.getByRole("button", { name: "command code" }));
    expect(onOpen).toHaveBeenCalledWith("p-view");

    for (const label of ["测试连通性", "编辑", "删除", "复制", "停用"]) {
      expect(screen.queryByRole("button", { name: label })).toBeNull();
    }
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

  it("后端只给了视图（没有连接配置字段）：明说不可用，不排一排空占位", () => {
    renderDetail({ id: "p-view", row: readProvider(viewProvider()) });

    expect(screen.getByTestId("providers-connection-unavailable")).toBeTruthy();
    expect(screen.getByText(/后端返回的是服务商视图/)).toBeTruthy();
    // 概览里的视图列照常显示
    expect(screen.getByText("command code")).toBeTruthy();
    expect(screen.getByText("https://api.commandcode.ai/v1")).toBeTruthy();
  });

  it("完整配置：鉴权 / 超时 / TLS / 附加头 / 透传 / 默认线路名都渲染", () => {
    renderDetail({
      id: "p-rust",
      row: readProvider(rustProvider()),
      laneName: (lane) => (lane === "lane-jp" ? "日本线" : undefined),
    });

    const connection = within(screen.getByTestId("providers-connection"));
    expect(connection.getByText("自定义头")).toBeTruthy();
    expect(connection.getByText("x-api-key")).toBeTruthy();
    expect(connection.getByText("Bearer")).toBeTruthy();
    expect(connection.getByText("openai/")).toBeTruthy();
    expect(connection.getByText("10s")).toBeTruthy();
    expect(connection.getByText("120s")).toBeTruthy();
    // 额外 CA 在字段网格里只显示"配没配"：PEM 内容不进这一块（它只在下方「原始字段」里原样可见）
    expect(connection.getByText("已配置")).toBeTruthy();
    expect(connection.queryByText(/BEGIN CERTIFICATE/)).toBeNull();

    // 默认线路名 + id 都给（名字读不到时至少还有 id）
    expect(document.body.textContent).toContain("日本线");
    expect(document.body.textContent).toContain("lane-jp");

    const headers = within(screen.getByTestId("providers-headers"));
    expect(headers.getByText("x-org")).toBeTruthy();
    expect(headers.getByText("team-a")).toBeTruthy();
    expect(headers.getByText("保留客户端的（只补缺）")).toBeTruthy();
  });

  it("凭据按 provider 过滤；没有 used_pct 时不画假比例，改成上限 + 后端状态", () => {
    const rows = [readCredential(rustCredential()), readCredential(viewCredential())];
    renderDetail({ id: "p-rust", row: readProvider(rustProvider()), credentials: rows });

    const table = within(screen.getByTestId("providers-credentials-table"));
    expect(table.getAllByTestId("providers-credential-row")).toHaveLength(1);
    expect(table.getByText("自建 · 500万 token/月")).toBeTruthy();
    expect(table.queryByText("command code · 按量")).toBeNull();
    // [docs/15 §4.3] 百分比固定一位小数 —— 断言的是**文档要求的**形态，不是组件当前凑巧的形态。
    // （原先这条断言被写成 "82%"，等于把 `QuotaBar` 少传 digits 的缺陷抄进了测试；
    //   根因已在 QuotaBar.tsx 修掉：显式 `formatPercent(v, 1)`。）
    expect(table.getByText("82.0%")).toBeTruthy();
    cleanup();

    // mock 形状：只有 quota.state + 上限 → 显示上限与状态，不推导百分比
    renderDetail({
      id: "p-view",
      row: readProvider(viewProvider()),
      credentials: [readCredential(viewCredential())],
    });
    expect(screen.getByText(/上限 ¥20/)).toBeTruthy();
    expect(screen.getByText("正常")).toBeTruthy();
  });

  it("凭据列表为空时说清楚「挂载关系写在凭据的 provider 字段上」", () => {
    renderDetail({ id: "p-rust", row: readProvider(rustProvider()), credentials: [] });
    expect(screen.getByTestId("providers-credentials-empty")).toBeTruthy();
    expect(screen.queryByTestId("providers-credentials-table")).toBeNull();
  });

  it("原始字段原样打印（后端多给什么看得见）", () => {
    renderDetail({ id: "p-rust", row: readProvider(rustProvider({ custom_thing: "keep-me" })) });
    expect(screen.getByTestId("providers-raw").textContent).toContain("keep-me");
  });
});

// ================================================================ ④ 整页（浏览器 mock 路径）

describe("整页在 mock 基准场景下能用（不依赖 Rust/Tauri）", () => {
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
    // 额度：mock 没给 used_pct，所以显示上限而不是一个编出来的百分比
    expect(screen.getByText(/上限 5M/)).toBeTruthy();
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

// ================================================================ ⑥⑦ [docs/17 §3.1] 两个添加向导

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

/** 打开一个 MUI 下拉并点中某一项。 */
async function chooseOption(testId: string, name: RegExp | string): Promise<void> {
  fireEvent.mouseDown(within(screen.getByTestId(testId)).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name }));
}

/** 本轮向导该有的控件（[docs/17 §3.1]）。**不许改名**：真机 E2E 按它们驱动。 */
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
  "credentials-add-quotaLimit",
  "credentials-add-currency",
  "credentials-add-window",
  "credentials-add-submit",
] as const;

describe("[docs/17 §3.1] 高级添加（单步对话框）：真控件 → 真的调 provider_upsert → 显示后端真值", () => {
  /*
   * 为什么这里的入口是 `providers-add-advanced-open`：本轮（[docs/19 §1]）`providers-add-open`
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

describe("[docs/17 §3.1] 添加密钥：明文只进密钥仓库，界面永不回显", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("页头有「添加密钥」；打开后控件一个不少，secret 是 password", async () => {
    renderAt("/providers");

    fireEvent.click(await screen.findByTestId("credentials-add-open"));
    for (const id of CREDENTIAL_WIZARD_CONTROLS) {
      expect(screen.getByTestId(id), `缺控件 ${id}`).toBeTruthy();
    }
    expect(inputOf("credentials-add-id").tagName).toBe("INPUT");
    expect(inputOf("credentials-add-secret").type).toBe("password");
    // 服务商是下拉，选项来自 provider_list
    fireEvent.mouseDown(within(screen.getByTestId("credentials-add-provider")).getByRole("combobox"));
    expect(await screen.findByRole("option", { name: "字节方舟" })).toBeTruthy();
  });

  it("填完真提交 → 先 credential_upsert（secret 只是引用、载荷里没有明文）→ 再 credential_set_secret", async () => {
    const SECRET = "sk-live-plaintext-canary-123456"; // ta-allow-fake-secret —— 假的金丝雀值：它出现在这里，正是为了证明它不会出现在载荷 / 错误 / 页面上
    let listCalls = 0;
    registerMock("credential_list", () => {
      listCalls += 1;
      return [];
    });
    const upsert = recordingMock({
      id: "c-backend",
      provider: "p-bytedance",
      label: "后端标签",
      secret: "${secret:c-backend}",
      quota: {
        metric: "cost",
        limit: 12,
        currency: "CNY",
        window: "month",
        reset_day: 1,
        soft_limit: 0.8,
        hard_limit: 0.98,
        reserve: 0,
      },
      guard: { max_rpm: null, max_concurrency: null, max_input_tokens: null, on_exceed: "wait" },
      tags: [],
      enabled: true,
    });
    const setSecret = recordingMock(undefined);
    /** 两个命令的**真实调用顺序**（"先 upsert 再写密钥"是规格里的硬要求）。 */
    const order: string[] = [];
    registerMock("credential_upsert", (args) => {
      order.push("upsert");
      return upsert.handler(args);
    });
    registerMock("credential_set_secret", (args) => {
      order.push("set_secret");
      return setSecret.handler(args);
    });

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("credentials-add-open"));
    typeInto("credentials-add-id", "my-key");
    typeInto("credentials-add-label", "我的套餐");
    await chooseOption("credentials-add-provider", "字节方舟");
    typeInto("credentials-add-secret", SECRET);
    typeInto("credentials-add-quotaLimit", "12");
    typeInto("credentials-add-currency", "CNY");
    fireEvent.click(screen.getByTestId("credentials-add-submit"));

    await waitFor(() => expect(setSecret.calls).toHaveLength(1));

    // ① credential_upsert 的载荷：secret 是 `${secret:<条目名>}` 引用，明文一个字节都不在里面
    const upArgs = upsert.calls[0]!;
    expect(Object.keys(upArgs)).toEqual(["credential"]);
    expect(JSON.stringify(upArgs)).not.toContain(SECRET);
    const cred = upArgs["credential"] as Record<string, unknown>;
    expect(cred["id"]).toBe("my-key");
    expect(cred["provider"]).toBe("p-bytedance");
    expect(cred["label"]).toBe("我的套餐");
    expect(cred["secret"]).toBe("${secret:my-key}");
    expect(cred["quota"]).toEqual({ limit: 12, currency: "CNY", window: "month" });
    for (const key of Object.keys(cred)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);
    // guard / tags / enabled / lane 在 Rust 侧都有 serde(default) —— 前端不替后端编
    for (const absent of ["guard", "tags", "enabled", "lane"]) {
      expect(absent in cred, `${absent} 有 Rust serde(default)`).toBe(false);
    }

    // ② 顺序：先建凭据（拿到后端 id），再把明文写进密钥仓库
    expect(order).toEqual(["upsert", "set_secret"]);
    expect(setSecret.calls[0]).toEqual({ id: "c-backend", plaintext: SECRET });

    // ③ 成功后就地清空输入框：明文不留在一个还开着的对话框里
    expect(inputOf("credentials-add-secret").value).toBe("");
    expect(inputOf("credentials-add-id").value).toBe("");

    // ④ 回显后端返回值 + 重取列表
    const saved = await screen.findByTestId("credentials-add-result");
    expect(saved.textContent).toContain("c-backend");
    expect(saved.textContent).toContain("后端标签");
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("没填 secret：只 credential_upsert，**不碰密钥仓库**", async () => {
    const upsert = recordingMock({ id: "c-backend", provider: "p-bytedance", label: "x", secret: "${secret:c-backend}" });
    const setSecret = recordingMock(undefined);
    registerMock("credential_upsert", upsert.handler);
    registerMock("credential_set_secret", setSecret.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("credentials-add-open"));
    typeInto("credentials-add-id", "my-key");
    typeInto("credentials-add-label", "我的套餐");
    await chooseOption("credentials-add-provider", "字节方舟");
    fireEvent.click(screen.getByTestId("credentials-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    expect(setSecret.calls).toHaveLength(0);
    // 空 secret 也不许把明文（空串以外的东西）塞进载荷
    const cred = upsert.calls[0]!["credential"] as Record<string, unknown>;
    expect(cred["secret"]).toBe("${secret:my-key}");
  });

  it("本地校验不过一次都不发：必填 / id 形状 / 与现有凭据 id 重复", async () => {
    const upsert = recordingMock({});
    const setSecret = recordingMock(undefined);
    registerMock("credential_upsert", upsert.handler);
    registerMock("credential_set_secret", setSecret.handler);

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("credentials-add-open"));

    fireEvent.click(screen.getByTestId("credentials-add-submit"));
    expect((await screen.findByTestId("credentials-add-issues")).textContent).toContain("必填");
    expect(upsert.calls).toHaveLength(0);

    typeInto("credentials-add-id", "Bad_Id");
    typeInto("credentials-add-label", "套餐");
    await chooseOption("credentials-add-provider", "字节方舟");
    fireEvent.click(screen.getByTestId("credentials-add-submit"));
    expect(screen.getByTestId("credentials-add-issues").textContent).toContain(
      "id 只能用小写字母、数字与短横线",
    );
    expect(upsert.calls).toHaveLength(0);

    // 与 credential_list 里已有的 id 重复
    typeInto("credentials-add-id", "c-bytedance");
    fireEvent.click(screen.getByTestId("credentials-add-submit"));
    expect(screen.getByTestId("credentials-add-issues").textContent).toContain("已存在同 id 的条目");
    expect(upsert.calls).toHaveLength(0);
    expect(setSecret.calls).toHaveLength(0);
  });

  it("写密钥失败：原样显示 TA 码，且页面上**任何地方**都没有明文", async () => {
    const SECRET = "sk-leak-canary-987654321"; // ta-allow-fake-secret —— 同上：假值，用来断言"页面上任何地方都没有明文"
    registerMock("credential_upsert", () => ({
      id: "c-backend",
      provider: "p-bytedance",
      label: "我的套餐",
      secret: "${secret:c-backend}",
    }));
    registerMock("credential_set_secret", () => {
      throw new TokenAgencyError({ code: "TA-6001", message: "密钥仓库不可用（钥匙串被锁）" });
    });

    renderAt("/providers");
    fireEvent.click(await screen.findByTestId("credentials-add-open"));
    typeInto("credentials-add-id", "my-key");
    typeInto("credentials-add-label", "我的套餐");
    await chooseOption("credentials-add-provider", "字节方舟");
    typeInto("credentials-add-secret", SECRET);
    fireEvent.click(screen.getByTestId("credentials-add-submit"));

    const error = await screen.findByTestId("credentials-add-error");
    expect(error.textContent).toContain("TA-6001");
    expect(error.textContent).toContain("钥匙串被锁");
    // 明文不进日志、不进错误消息 —— 页面上任何渲染出来的文本都不含它
    expect(document.body.textContent ?? "").not.toContain(SECRET);
    // 失败时**不清空**输入框：用户还要重试
    expect(inputOf("credentials-add-secret").value).toBe(SECRET);
  });
});

// ================================================================ ⑧ [docs/20 §4] 详情页的图形化编辑

/** 一条挂在某个服务商下的凭据：第 2 步"已添加的模型"按凭据归属过滤。 */
function boundCredential(id: string, provider: string): unknown {
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

/** 下拉当前显示的文案（MUI 把选中的值渲染在 `[role="combobox"]` 里）。 */
function selectTextOf(testId: string): string {
  return (within(screen.getByTestId(testId)).getByRole("combobox").textContent ?? "").trim();
}

/** 基准 mock 的模型 id（`shared/api/mock.ts` 的场景常量）。 */
const BASELINE_MODEL = "deepseek-v4.1flash";

/** 从列表点进详情，再点 `providers-detail-edit` 打开编辑向导的第 1 步。 */
async function openEditWizard(id = "p-bytedance"): Promise<void> {
  renderAt(`/providers/${id}`);
  await screen.findByTestId("providers-detail");
  fireEvent.click(await screen.findByTestId("providers-detail-edit"));
  await screen.findByTestId("provider-wizard-step1");
}

describe("[docs/20 §4] 详情页的图形化编辑：与新增同一个两步向导", () => {
  beforeEach(() => {
    registerBaselineMocks();
    // 草稿活在模块级 store 里：每条用例从干净状态开始
    resetWizardSession();
  });

  it("详情页有 providers-detail-edit；点开是向导第 1 步且预填与现有服务商一致", async () => {
    await openEditWizard();

    // 预填的是**现有服务商**的名字 / 地址 / 协议（不是空表单）
    expect(inputOf("pw-name").value).toBe("字节方舟");
    expect(inputOf("pw-baseUrl").value).toBe("https://ark.cn-beijing.volces.com/api/v3");
    expect(selectTextOf("pw-protocol")).toContain("OpenAI");
    // 标题与密钥说明是编辑模式那一套（留空 = 不改）
    expect(screen.getByText("编辑服务商")).toBeTruthy();
    expect(screen.getByText("留空表示不改动已保存的密钥")).toBeTruthy();
  });

  it("编辑模式留空密钥：只 provider_upsert（用现有 id 覆盖写），不发 credential_set_secret", async () => {
    const provider = recordingMock({
      id: "p-bytedance",
      name: "字节方舟 v2",
      protocol: "openai",
      base_url: "https://ark.cn-beijing.volces.com/api/v3",
    });
    const secret = recordingMock(undefined);
    registerMock("provider_upsert", provider.handler);
    registerMock("credential_set_secret", secret.handler);
    registerMock("credential_list", () => [boundCredential("c-bytedance", "p-bytedance")]);

    await openEditWizard();
    typeInto("pw-name", "字节方舟 v2");
    fireEvent.click(screen.getByTestId("pw-next"));
    await screen.findByTestId("provider-wizard-step2");

    // 密钥框留空 -> 一个字节都不碰密钥仓库
    expect(secret.calls).toHaveLength(0);
    // 但服务商是**覆盖写**：id 必须还是现有那一个（不许派生新 id）
    expect(provider.calls).toHaveLength(1);
    expect(provider.calls[0]!["provider"]).toMatchObject({
      id: "p-bytedance",
      name: "字节方舟 v2",
      protocol: "openai",
      base_url: "https://ark.cn-beijing.volces.com/api/v3",
    });
    // 自己不算重名：改回自己的名字也不该被拦（第 1 步能过就说明去重排除了自己）
    expect(screen.queryByText(/已经被占用/)).toBeNull();
  });

  it("填了密钥才写：先 provider_upsert（同 id）→ credential_set_secret", async () => {
    const provider = recordingMock({ id: "p-bytedance", name: "字节方舟" });
    const credential = recordingMock({ id: "c-bytedance" });
    const secret = recordingMock(undefined);
    registerMock("provider_upsert", provider.handler);
    registerMock("credential_upsert", credential.handler);
    registerMock("credential_set_secret", secret.handler);
    registerMock("credential_list", () => [boundCredential("c-bytedance", "p-bytedance")]);

    const KEY = "sk-edit-canary-0002"; // ta-allow-fake-secret —— 假值
    await openEditWizard();
    typeInto("pw-key", KEY);
    fireEvent.click(screen.getByTestId("pw-next"));
    await screen.findByTestId("provider-wizard-step2");

    // 写的是**这个服务商已有的那把凭据**，不是新造一个 `<id>-default`
    expect(credential.calls[0]!["credential"]).toMatchObject({ id: "c-bytedance" });
    expect(secret.calls[0]).toEqual({ id: "c-bytedance", plaintext: KEY });
    expect(document.body.textContent ?? "").not.toContain(KEY);
  });

  it("编辑模式第 2 步：列出该服务商已有模型，并能就地改线路", async () => {
    registerMock("provider_upsert", () => ({ id: "p-bytedance", name: "字节方舟" }));
    registerMock("credential_list", () => [boundCredential("c-bytedance", "p-bytedance")]);
    const bulk = recordingMock({
      applied: true,
      restart_required: false,
      kernel_reload: false,
      channels_created: 0,
    });
    registerMock("model_bulk_update", bulk.handler);

    await openEditWizard();
    fireEvent.click(screen.getByTestId("pw-next"));
    await screen.findByTestId("provider-wizard-step2");

    // 基准 mock 的 channel_matrix 里，c-bytedance 上挂着这个模型
    const row = await screen.findByTestId("pw-model-row");
    expect(row.textContent).toContain(BASELINE_MODEL);
    // 「已添加的模型」不是空态
    expect(screen.queryByTestId("pw-added-models-empty")).toBeNull();

    // 就地改线路：走的是同一个 pw-model-lane
    await chooseOption("pw-model-lane", /日本/);
    await waitFor(() => expect(bulk.calls).toHaveLength(1));
    expect(bulk.calls[0]).toEqual({ ids: [BASELINE_MODEL], patch: { lane: "lane-jp" } });
  });
});

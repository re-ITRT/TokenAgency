// @vitest-environment jsdom
/**
 * 外壳的机械化验收（F1 的 DoD 2 + 路由契约一致性）。
 *
 * 为什么不靠"我看了一眼"：
 * - **每个路由都必须能渲染**（页面缺失 → 占位壳，不白屏、不抛错）——这里逐条跑；
 * - **`/` 必须重定向到看板**；
 * - **未知路径必须给 404 页面**，不能是空白；
 * - **路由表自身要自洽**：key/path 不重复、`titleKey` 在 zh-CN 里真的存在、
 *   每个 nav 项都有图标 —— 这些都是契约，写错了就该红。
 */

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { api, isTauri } from "@/shared/api";
import { registerBaselineMocks, registerDevMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";

import { AppShell } from "./AppShell";
import { primaryCandidate } from "./pageRegistry";
import { HOME_PATH, NAV_ROUTES, ROUTES, routeByKey } from "./routes";

beforeAll(async () => {
  // 语言固定成中文并**等它生效**：CI 机器的 navigator.language 不可控，
  // 断言不能依赖运行环境。（也顺带验证了 setLocale → changeLanguage 这条链路。）
  await i18n.changeLanguage("zh-CN");
  // 浏览器开发用的基准场景 mock：让状态条有真实数据可渲染（[03 §0]）
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
});

function renderAt(path: string) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[path]}>
        <AppShell />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/** 占位壳里显示的"该放哪个文件"——用它判断这一页确实渲染了。 */
function placeholderMarker(path: string): string {
  return `src/pages/${path}.tsx`;
}

describe("路由外壳", () => {
  it("每个路由都能渲染（页面缺失时给占位壳，不白屏）", () => {
    for (const route of ROUTES) {
      const url = route.path.replace(":id", "p-bytedance");
      const view = renderAt(url);
      const expected = placeholderMarker(primaryCandidate(route.pages));
      expect(
        screen.queryByText(expected),
        `${url} 应当渲染占位壳（找 ${expected}）`,
      ).toBeTruthy();
      view.unmount();
    }
  });

  it("`/` 重定向到看板", () => {
    renderAt("/");
    const dashboard = routeByKey("dashboard");
    expect(dashboard).toBeDefined();
    expect(screen.queryByText(placeholderMarker(primaryCandidate(dashboard!.pages)))).toBeTruthy();
  });

  it("未知路径渲染 404，而不是空白", () => {
    renderAt("/this-does-not-exist");
    // 用 testid 判定（与语言无关），文本用于人眼确认
    expect(screen.queryByTestId("page-not-found")).toBeTruthy();
    expect(screen.queryByText(zhCN["error.notFound"])).toBeTruthy();
  });

  it("路由表自洽（契约一致性）", () => {
    const keys = ROUTES.map((r) => r.key);
    const paths = ROUTES.map((r) => r.path);
    expect(new Set(keys).size).toBe(keys.length);
    expect(new Set(paths).size).toBe(paths.length);

    for (const route of ROUTES) {
      expect(route.path.startsWith("/"), `${route.key} 的 path 必须以 / 开头`).toBe(true);
      // titleKey 必须真的存在于 zh-CN（漏了就没法显示，且 en 也过不了 tsc）
      expect(zhCN[route.titleKey], `${route.key} 的 titleKey 不存在`).toBeTruthy();
      expect(route.icon, `${route.key} 缺图标`).toBeTruthy();
      expect(route.pages.length, `${route.key} 必须有页面候选路径`).toBeGreaterThan(0);
      expect(route.docRef.length, `${route.key} 必须标注契约出处`).toBeGreaterThan(0);
    }
  });

  it("侧边栏项由路由表生成，且覆盖用户主视角（概览在首位）", () => {
    expect(NAV_ROUTES.length).toBeGreaterThanOrEqual(8);
    expect(NAV_ROUTES[0]?.path).toBe(HOME_PATH);
    // 导航标签不得出现内部概念（channel / credential / lane）——[12 §6 F1] 的措辞要求
    for (const route of NAV_ROUTES) {
      const label = zhCN[route.titleKey];
      expect(/channel|credential|lane/i.test(label), `导航标签不该用内部概念：${label}`).toBe(
        false,
      );
    }
  });

  it("浏览器开发路径可用：非 Tauri 下 mock 生效，不需要 Rust/Tauri（DoD 3）", async () => {
    // 本测试跑在 jsdom 里、没有 Tauri 宿主，等价于 `npm run dev` 的浏览器环境
    expect(isTauri()).toBe(false);
    expect(() => registerDevMocks()).not.toThrow();
    // 走完整链路：api.app.status() → call() → mock（基准场景：网关运行中）
    await expect(api.app.status()).resolves.toMatchObject({ gateway: "running" });
    await expect(api.usage.dashboard({ preset: "today" })).resolves.toMatchObject({
      cards: { requests: { value: 1_284 } },
    });
  });
});

// @vitest-environment jsdom
/**
 * 外壳的机械化验收（F1 的 DoD 2 + 路由契约一致性）。
 *
 * 为什么不靠"我看了一眼"：
 * - **每个路由都必须能渲染**：有页面文件就渲染页面（必须有页头、不许再走占位壳），
 *   没有页面文件就给占位壳（不白屏、不抛错）——这里逐条跑；
 * - **`/` 必须重定向到看板**；
 * - **未知路径必须给 404 页面**，不能是空白；
 * - **路由表自身要自洽**：key/path 不重复、`titleKey` 在 zh-CN 里真的存在、
 *   每个 nav 项都有图标 —— 这些都是契约，写错了就该红。
 */

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { api, isTauri } from "@/shared/api";
import { registerBaselineMocks, registerDevMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { zhCN } from "@/shared/i18n/zh-CN";
import { useUiStore } from "@/stores/ui";

import { AppShell } from "./AppShell";
import { resolvePage } from "./pageRegistry";
import { HOME_PATH, NAV_ROUTES, navSections, ROUTES, routeByKey } from "./routes";

// 这条文件里"每个路由都能渲染"要**逐个真实挂载 13 条路由**（含懒加载分包）。
// 页面全落地之前它 1.3s 就跑完；13 个页面都在之后，光"非 network 的 11 条"实测就要 ~4.2s，
// 再加每条路由的 waitFor 等待，默认 5s 已经**结构性不够**（不是页面缺陷）。
// 注意：必须在**模块作用域**设置 —— 放进 beforeAll 太晚，收集测试时就已经定好超时了。
vi.setConfig({ testTimeout: 30_000, hookTimeout: 30_000 });

beforeAll(async () => {
  // 语言固定成中文并**等它生效**：CI 机器的 navigator.language 不可控，
  // 断言不能依赖运行环境。（也顺带验证了 setLocale → changeLanguage 这条链路。）
  await i18n.changeLanguage("zh-CN");
  // 浏览器开发用的基准场景 mock：让状态条有真实数据可渲染（[03 §0]）
  registerBaselineMocks();
  // **再加上开发用的完整 mock 集**：`registerBaselineMocks` 只覆盖基准场景那几条命令，
  // 而 13 条路由各自的页面（成本中心要 usage_*、设置页要 config_*、网络页要 subscription_list …）
  // 需要更多命令。少了它们，页面会抛错被 ErrorBoundary 接管，于是"每个路由都能渲染"这条
  // 会在**页面明明已经实现**的情况下超时失败 —— 这是测试自身的环境缺口，不是页面缺陷。
  registerDevMocks();
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

describe("路由外壳", () => {
  it("每个路由都能渲染：有页面就渲染页面，没页面就给 404（都不白屏）", async () => {
    for (const route of ROUTES) {
      const url = route.path.replace(":id", "p-bytedance");
      const page = resolvePage(route.pages);
      const view = renderAt(url);

      // 页面组件是懒加载的：先出 Suspense 骨架，再出内容
      await waitFor(() => {
        expect(document.body.textContent?.trim().length ?? 0, `${url} 白屏了`).toBeGreaterThan(0);
      });

      if (page === undefined) {
        // [极简] 占位壳已整块删除：缺页面时给**用户能看懂的 404**（不再显示"该放哪个文件"）
        expect(
          screen.queryByTestId("page-not-found"),
          `${url} 没有页面文件时应当渲染 404 页`,
        ).toBeTruthy();
      } else {
        // 页面已就位：必须渲染出自己的页头（而不是任何回退壳）
        expect(screen.queryByTestId("page-not-found"), `${url} 不该走 404`).toBeNull();
        // 逐条串行挂载 + 懒加载分包 + 每页自带查询，默认 1s 的 waitFor 会偶发超时；
        // 10s 只是等待上限，断言本身没有放宽。
        await waitFor(
          () => expect(screen.queryByTestId("page-header"), `${url} 没能渲染出页头`).toBeTruthy(),
          { timeout: 10_000 },
        );
      }
      view.unmount();
    }
    // 这条要**逐个真实挂载 13 条路由**（含懒加载分包）：页面全落地后整轮 5–7s，
    // 默认 5s 是**结构性不够**（不是页面缺陷；页面未落地时它 1.3s 就跑完）。
    // 断言一条没动，只是给它足够的预算。
  }, 60_000);

  it("`/` 重定向到看板（看板已有页面）", async () => {
    renderAt("/");
    const dashboard = routeByKey("dashboard");
    expect(dashboard).toBeDefined();
    expect(resolvePage(dashboard!.pages)).toBeDefined();
    await waitFor(() => expect(screen.queryByTestId("page-header")).toBeTruthy());
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

  // ================================================================ 信息架构

  /**
   * 侧边栏的**顺序与分组**是设计决定，不是"谁先被写进数组"的副产品。
   *
   * 用户第 323 轮原话：「侧边栏几个页面重新排序一下，现在这样有点不合理，
   * 从用户体验工程和直觉上来说」。改之前是
   * `概览 | 服务商 模型 网络 | 成本 日志 | 设置 | 访问密钥` ——
   * 语义是**配置(3) → 观察(2) → 系统(1) → 又回到配置(1)**：
   * 配置链的**终点**（访问密钥）被丢在「设置」之后，读起来像设置的子项。
   *
   * 这一组把它钉死：**顺序错了、组断了、组名没登记，都当场红**。
   */
  describe("侧边栏顺序与分组", () => {
    it("顺序 = 概览 → 配置四项 → 记录两项 → 设置（按任务流，不按频率）", () => {
      expect(NAV_ROUTES.map((r) => r.key)).toEqual([
        "dashboard",
        // 配置：接谁 → 提供什么 → 怎么出网 → **谁可以调用**（链的终点）
        "providers",
        "models",
        "network",
        "api-keys",
        // 记录：明细 → 聚合
        "logs",
        "cost",
        // 系统：永远最后
        "settings",
      ]);
    });

    it("分组后：首页不归组、三组连续、组名都在 zh-CN 里", () => {
      const sections = navSections();
      expect(sections, "分组算不出来（同组不连续 / 组名没登记）").not.toBeNull();
      const list = sections!;

      // 首页自成一段且在最前：不塞进任何组（组名与项名都是"概览"会显得重复）
      expect(list[0]?.groupKey).toBeUndefined();
      expect(list[0]?.routes.map((r) => r.key)).toEqual(["dashboard"]);

      expect(list.slice(1).map((s) => s.groupKey)).toEqual([
        "navGroup.setup",
        "navGroup.records",
        "navGroup.system",
      ]);
      for (const section of list.slice(1)) {
        expect(zhCN[section.groupKey!], `${section.groupKey} 不存在`).toBeTruthy();
      }

      // 组名与它下面的项名不许撞（"配置 / 配置"那种读起来像 bug）
      for (const section of list.slice(1)) {
        const groupLabel = zhCN[section.groupKey!];
        for (const route of section.routes) {
          expect(zhCN[route.titleKey]).not.toBe(groupLabel);
        }
      }
    });

    it("**每一组内的项在 NAV_ROUTES 里是连续的一段**（这是 `navSections` 的硬前提）", () => {
      // 把 `navSections` 的连续性判定用**独立的方式**再算一遍：直接看分组序列有没有回摆。
      // 两边都算 = 万一哪天 `navSections` 的判定写松了，这条仍然会红。
      const groups = NAV_ROUTES.map((r) => r.navGroup ?? "__ungrouped__");
      const seen = new Set<string>();
      let previous: string | undefined;
      for (const group of groups) {
        if (group !== previous) {
          expect(seen.has(group), `分组 ${group} 被切成了两段（第 ${groups.indexOf(group) + 1} 项又回来了）`).toBe(
            false,
          );
          seen.add(group);
          previous = group;
        }
      }
    });

    /**
     * **画出来的和算出来的一致** —— 上面几条测的是 `navSections()` 的返回值，
     * 这一条测**侧边栏真的渲染了那几个组名**（算对了但没画 = 用户还是看不到结构）。
     *
     * 同时也钉住"顺序"这件事的**用户可见形式**：DOM 文本流里组名与项名的先后。
     */
    it("侧边栏真的把三个组名画出来，且顺序是 概览 → 配置 → 记录 → 系统", async () => {
      renderAt("/dashboard");
      await waitFor(() => expect(screen.queryByTestId("page-header")).toBeTruthy());

      const nav = document.querySelector("nav");
      expect(nav, "侧边栏没渲染").not.toBeNull();

      const headings = Array.from(nav!.querySelectorAll('[data-testid="sidebar-group-heading"]'))
        .map((el) => el.textContent)
        .filter(Boolean);
      expect(headings).toEqual([
        zhCN["navGroup.setup"],
        zhCN["navGroup.records"],
        zhCN["navGroup.system"],
      ]);

      // 组名与它下面的项名在 DOM 里的先后：用文本流位置判（不依赖 CSS 布局）
      const text = nav!.textContent ?? "";
      const at = (s: string) => {
        const i = text.indexOf(s);
        expect(i, `侧边栏里找不到「${s}」`).toBeGreaterThanOrEqual(0);
        return i;
      };
      expect(at(zhCN["nav.dashboard"])).toBeLessThan(at(zhCN["navGroup.setup"]!));
      expect(at(zhCN["nav.providers"])).toBeGreaterThan(at(zhCN["navGroup.setup"]!));
      // 配置链的**终点**（访问密钥）在配置组内、且早于记录组 —— 这正是本轮修的那一处
      expect(at(zhCN["nav.apiKeys"])).toBeGreaterThan(at(zhCN["navGroup.setup"]!));
      expect(at(zhCN["nav.apiKeys"])).toBeLessThan(at(zhCN["navGroup.records"]!));
      // 记录组：明细（日志）在聚合（成本）之前
      expect(at(zhCN["nav.logs"])).toBeGreaterThan(at(zhCN["navGroup.records"]!));
      expect(at(zhCN["nav.logs"])).toBeLessThan(at(zhCN["nav.cost"]));
      // 系统最后一项
      expect(at(zhCN["nav.settings"])).toBeGreaterThan(at(zhCN["navGroup.system"]!));
    });

    /**
     * **折叠态不画组名** —— 这条是`Sidebar.tsx` 里那句注释的可执行版本。
     *
     * 为什么必须不画：折叠后整条栏只有 56px，一个组名（哪怕 11px）也放不下，
     * 硬塞会把图标挤歪。折叠态的识别靠 `Tooltip`（原本就有），不受影响。
     *
     * 注释里写了断言却没测 = 下一个人改坏了不会有人发现，所以这里真的点一次折叠。
     */
    it("折叠后不画组名（56px 放不下），但导航项一个都不少", async () => {
      useUiStore.getState().setSidebarCollapsed(false);
      renderAt("/dashboard");
      await waitFor(() => expect(screen.queryByTestId("page-header")).toBeTruthy());
      expect(document.querySelectorAll('[data-testid="sidebar-group-heading"]').length).toBe(3);

      // 真的点那个折叠按钮（不是直接改 store）——顺便验证按钮接线
      fireEvent.click(screen.getByTestId("sidebar-collapse-toggle"));
      await waitFor(() =>
        expect(document.querySelectorAll('[data-testid="sidebar-group-heading"]').length).toBe(0),
      );

      // 组名没了，但**每一项都还在**（图标态可点、hover 有 Tooltip）
      const nav = document.querySelector("nav")!;
      expect(nav.querySelectorAll('[role="button"], .MuiListItemButton-root').length).toBe(
        NAV_ROUTES.length,
      );

      // 复位：别把状态漏给后面的用例（store 是全局单例）
      useUiStore.getState().setSidebarCollapsed(false);
    });
  });

  it("浏览器开发路径可用：非 Tauri 下 mock 生效，不需要 Rust/Tauri（DoD 3）", async () => {
    // 本测试跑在 jsdom 里、没有 Tauri 宿主，等价于 `npm run dev` 的浏览器环境
    expect(isTauri()).toBe(false);
    expect(() => registerDevMocks()).not.toThrow();
    // 走完整链路：api.app.status() → call() → mock（基准场景：网关运行中）
    await expect(api.app.status()).resolves.toMatchObject({ gateway: "running" });
    await expect(api.usage.dashboard("today")).resolves.toMatchObject({
      cards: { requests: { value: 1_284 } },
    });
  });
});

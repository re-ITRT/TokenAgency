/**
 * `@/app` —— 前端外壳（F1）。
 *
 * 对外只有两个入口：
 * - `<AppShell />`：布局 + 路由 + 事件桥（`App.tsx` 用）；
 * - `routes.ts` 的 `ROUTES` / `NAV_ROUTES`：**路由契约**，四个包对照它放页面。
 */

export { AppShell } from "./AppShell";
export { AppRouter } from "./AppRouter";
export { PagePlaceholder, type PagePlaceholderProps } from "./PagePlaceholder";
export { ErrorBoundary, type ErrorBoundaryProps } from "./ErrorBoundary";
export {
  HOME_PATH,
  matchRoute,
  NAV_ROUTES,
  routeByKey,
  ROUTES,
  type AppRoute,
  type RouteOwner,
} from "./routes";
export { knownPages, primaryCandidate, resolvePage } from "./pageRegistry";
export { SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH } from "./Sidebar";

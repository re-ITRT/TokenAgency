/**
 * 应用根组件（F1 交付：取代 W0 骨架页）。
 *
 * 职责边界：
 * - `main.tsx`（W0 提供）负责 QueryClientProvider；
 * - 这里负责主题（跟随 `useUiStore.themeMode`）、i18n 初始化、路由与外壳；
 * - **浏览器里能跑**：非 Tauri 环境下 `registerDevMocks()` 会注册 [03 §0] 基准场景的
 *   mock 数据，所以 `npm run dev` 不依赖 Rust/Tauri 就能看页面。
 *
 * 路由用 `HashRouter`：Tauri 生产环境走自定义协议、没有服务端回退，
 * hash 路由刷新/深链不会 404（桌面应用里 URL 美观度不重要）。
 */

import { CssBaseline, ThemeProvider } from "@mui/material";
import { useEffect, useState } from "react";
import { HashRouter } from "react-router-dom";

// 注意：用显式路径 `@/app/AppShell` 而不是 `@/app`：Windows 文件系统大小写不敏感，
// `@/app` 会先命中 `src/App.tsx`（那个骨架页）而不是 `src/app/` 目录，
// 导致 TS 报"only in casing"错误。四个包同理：请 `import { ROUTES } from "@/app/routes"`。
import { AppShell } from "@/app/AppShell";
import { registerDevMocks } from "@/shared/api";
import { useUiStore } from "@/stores/ui";
import { resolveThemeMode, systemPrefersDark } from "@/stores/ui";

import { buildTheme } from "./theme";

// i18n：导入即初始化（资源内联 + 关 Suspense，首帧可渲染）
import "@/shared/i18n";

// 浏览器开发环境自动注册基准场景 mock（幂等；Tauri 里是空操作）
registerDevMocks();

export default function App() {
  const themeMode = useUiStore((s) => s.themeMode);
  /**
   * [用户第 147 轮] **主题默认跟随系统**：`system` 不是一种配色，它必须解析成 dark/light。
   * 这里同时订阅系统的变化 —— 用户在系统里切深浅色时，界面**当场**跟着变，
   * 而不是等下一次刷新（`matchMedia` 的 change 事件）。
   */
  // 惰性初值 + 容错：`matchMedia` 缺失或被 CSP 拦掉时**退回浅色**，绝不因此让整个界面白屏
  const [prefersDark, setPrefersDark] = useState(() => {
    try {
      return systemPrefersDark();
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    } catch {
      return;
    }
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (event: MediaQueryListEvent) => setPrefersDark(event.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  const resolvedTheme = resolveThemeMode(themeMode, prefersDark);

  return (
    <ThemeProvider theme={buildTheme(resolvedTheme)}>
      <CssBaseline />
      <HashRouter>
        <AppShell />
      </HashRouter>
    </ThemeProvider>
  );
}

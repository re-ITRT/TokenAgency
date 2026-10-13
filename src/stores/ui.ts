/**
 * 界面偏好（Zustand + 持久化）。这些是**纯显示层**状态，与后端配置无关：
 * 主题深浅、侧边栏折叠、密度。语言在 `shared/i18n` 里自己持久化（它要早点初始化）。
 */

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type ThemeMode = "dark" | "light" | "system";

/** 真正落到调色板上的两种模式（`system` 是"跟随系统"，必须解析成其中之一）。 */
export type ResolvedThemeMode = "dark" | "light";

/**
 * `system` → 当前系统偏好（用户第 147 轮：**主题默认跟随系统**）。
 *
 * 纯函数，只有两个输入：用户的选择与"系统现在是不是深色"。
 * 把它抽出来是因为"默认"这件事很容易被写成三处 `if`（App / Sidebar / 持久化各一份），
 * 而三份判断迟早不一致 —— 这里一份，两侧都调它。
 */
export function resolveThemeMode(
  mode: ThemeMode,
  systemPrefersDark: boolean,
): ResolvedThemeMode {
  if (mode === "system") return systemPrefersDark ? "dark" : "light";
  return mode;
}

/** 系统是否偏好深色（SSR / 测试环境没有 `matchMedia` 时按**浅色**处理，不猜深色）。 */
export function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

type UiState = {
  /** 深色优先：默认 dark。 */
  themeMode: ThemeMode;
  sidebarCollapsed: boolean;
  setThemeMode: (mode: ThemeMode) => void;
  toggleTheme: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  toggleSidebar: () => void;
};

export const useUiStore = create<UiState>()(
  persist(
    (set, get) => ({
      // 默认**跟随系统**（用户第 147 轮）；用户点了切换才会变成显式的 light/dark
  themeMode: "system",
      sidebarCollapsed: false,
      setThemeMode: (themeMode) => set({ themeMode }),
  // 切换 = 从**当前生效**的那一种跳到另一种（显式写下来，从此不再跟随系统）
  toggleTheme: () =>
    set({
      themeMode: resolveThemeMode(get().themeMode, systemPrefersDark()) === "dark" ? "light" : "dark",
    }),
    setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
    toggleSidebar: () => set({ sidebarCollapsed: !get().sidebarCollapsed }),
  }),
  { name: "ta.ui" },
  ),
);

/**
 * 界面偏好（Zustand + 持久化）。这些是**纯显示层**状态，与后端配置无关：
 * 主题深浅、侧边栏折叠、密度。语言在 `shared/i18n` 里自己持久化（它要早点初始化）。
 */

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type ThemeMode = "dark" | "light";

type UiState = {
  /** 深色优先（[07 §1.3]）：默认 dark。 */
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
      themeMode: "dark",
      sidebarCollapsed: false,
      setThemeMode: (themeMode) => set({ themeMode }),
      toggleTheme: () => set({ themeMode: get().themeMode === "dark" ? "light" : "dark" }),
      setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
      toggleSidebar: () => set({ sidebarCollapsed: !get().sidebarCollapsed }),
    }),
    { name: "ta.ui" },
  ),
);

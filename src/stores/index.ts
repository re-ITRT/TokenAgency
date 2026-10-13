/**
 * 全局 store 出口。
 *
 * 约束（[12 §6 F1]）：`src/stores/**` 只放**跨页面**状态。
 * 页面内状态用 `useState`；数据实体用 TanStack Query（`@/shared/api`）。
 */

export { useAppStore, TOAST_DEDUPE_MS, type ToastItem } from "./app";
export {
  resolveThemeMode,
  systemPrefersDark,
  useUiStore,
  type ResolvedThemeMode,
  type ThemeMode,
} from "./ui";

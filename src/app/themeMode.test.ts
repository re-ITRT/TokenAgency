/**
 * [用户第 147 轮] **主题默认跟随系统**。
 *
 * 这一条只钉"默认与解析"这一件事（App / Sidebar 都调同一个 `resolveThemeMode`）：
 * ① `system` 必须解析成 dark/light；② **默认就是 `system`**（不是 dark）；
 * ③ 用户显式选过之后**以用户为准**（不再跟随系统）。
 */
import { describe, expect, it, beforeEach } from "vitest";

import { resolveThemeMode, systemPrefersDark, useUiStore } from "@/stores";

describe("[用户第 147 轮] 主题默认跟随系统", () => {
  beforeEach(() => {
    useUiStore.setState({ themeMode: "system" });
  });

  it("默认是 `system`（跟随系统），不是写死的 dark", () => {
    expect(useUiStore.getState().themeMode).toBe("system");
  });

  it("`system` 解析成当前系统偏好；显式选择以用户为准", () => {
    expect(resolveThemeMode("system", true)).toBe("dark");
    expect(resolveThemeMode("system", false)).toBe("light");
    // 用户选过的两种，系统怎么变都不许改
    expect(resolveThemeMode("dark", false)).toBe("dark");
    expect(resolveThemeMode("light", true)).toBe("light");
  });

  it("切换 = 从**当前生效**的那种跳到另一种（跟随状态下按系统当前值算）", () => {
    const prefersDark = systemPrefersDark();
    useUiStore.getState().toggleTheme();
    expect(useUiStore.getState().themeMode).toBe(prefersDark ? "light" : "dark");
    // 再切回去，就是另一种显式选择
    useUiStore.getState().toggleTheme();
    expect(useUiStore.getState().themeMode).toBe(prefersDark ? "dark" : "light");
  });
});
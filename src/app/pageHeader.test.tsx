// @vitest-environment jsdom
/**
 * `<PageHeader>` 的验收：**四件事的字号层级必须与 [15] 一致**，四个包不能各写一版。
 *
 * 断言三类东西：
 * - 标题必须是 `text.page`（20/500）—— `MuiTypography-h2` 这一档就是 [15 §4.2] 的 `text.page`；
 *   **不能**用 `h1`（那是 `text.hero`，留给看板主数字，一页最多一个）；
 * - `docRef` 必须渲染出来（契约出处是给人看的，不能悄悄丢），缺省时不许留空行；
 * - 间距与配色走令牌：这里用"没有硬编码颜色、没有刻度外间距"的守卫兜底（design.guard）。
 */

import { Button, ThemeProvider } from "@mui/material";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { PageHeader, StatusDot } from "@/shared/components";
import { i18n } from "@/shared/i18n";
import { buildTheme, typography } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

afterEach(() => {
  cleanup();
});

function renderHeader(props: Parameters<typeof PageHeader>[0]) {
  return render(
    <ThemeProvider theme={buildTheme("dark")}>
      <PageHeader {...props} />
    </ThemeProvider>,
  );
}

describe("[15 §4.2] PageHeader：页面标题只有一档", () => {
  it("标题用 text.page（20px / 500），不是 hero", () => {
    renderHeader({ title: "服务商" });
    const title = screen.getByText("服务商");

    // 一页最多一个 hero（[15 §4.2]）—— 页头不许抢看板主数字的地位
    expect(title.className).toContain("MuiTypography-h2");
    expect(title.className).not.toContain("MuiTypography-h1");
    expect(getComputedStyle(title).fontSize).toBe(`${typography.size.page}px`);
    expect(getComputedStyle(title).fontWeight).toBe(`${typography.weight.medium}`);
  });

  it("标题下方只能是正文/元信息层级：说明用 body1（13px），出处用 caption（12px）", () => {
    renderHeader({ title: "请求日志", description: "每一次转发都留痕", docRef: "07 §2 请求日志" });

    const desc = screen.getByText("每一次转发都留痕");
    expect(desc.className).toContain("MuiTypography-body1");
    expect(getComputedStyle(desc).fontSize).toBe(`${typography.size.body}px`);

    const doc = screen.getByText(/07 §2 请求日志/);
    expect(doc.className).toContain("MuiTypography-caption");
    // 出处是开发向信息：必须带前缀（"契约出处："），否则用户看不懂这行是什么
    expect(doc.textContent).toContain("契约出处");
  });

  it("缺省项不占位：没有 description/docRef/actions 时页头只有标题", () => {
    renderHeader({ title: "成本中心" });
    const header = screen.getByTestId("page-header");
    expect(header.textContent).toBe("成本中心");
  });

  it("badge 只读展示（如实验模式圆点），actions 渲染在右侧", () => {
    renderHeader({
      title: "路由策略",
      badge: <StatusDot status="warn" label="实验模式" />,
      actions: <Button>保存</Button>,
    });
    expect(screen.getByText("实验模式")).toBeTruthy();
    expect(screen.getByRole("button", { name: "保存" })).toBeTruthy();
  });
});

// @vitest-environment jsdom
/**
 * `<PageHeader>` 的验收：**四件事的字号层级必须与 [15] 一致**，四个包不能各写一版。
 *
 * 断言三类东西：
 * - 标题必须是 `text.page`（20/500）—— `MuiTypography-h2` 这一档就是 [15 §4.2] 的 `text.page`；
 *   **不能**用 `h1`（那是 `text.hero`，留给看板主数字，一页最多一个）；
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

    // 一页最多一个 hero（[15 §4.2]）—— 字号只到 `text.page`，页头不许抢看板主数字的地位
    // （不再断言内部类名：那是把实现抄一遍；断言可观察到的字号与 DOM 语义）
    expect(getComputedStyle(title).fontSize).toBe(`${typography.size.page}px`);
    expect(getComputedStyle(title).fontWeight).toBe(`${typography.weight.medium}`);
  });

  it("[B-7] 页面标题是文档里的第一个标题：渲染成 `<h1>`（字号仍是 text.page）", () => {
    renderHeader({ title: "服务商" });
    const title = screen.getByText("服务商");

    // 全站此前没有任何 <h1>：读屏按标题层级导航时，页面主标题只能撞到 <h2>（B-7）
    expect(title.tagName).toBe("H1");
    expect(getComputedStyle(title).fontSize).toBe(`${typography.size.page}px`);
  });

  it("[docs/22] 说明不再铺在版面上：收进标题右边的问号圆圈，悬停/聚焦才展开", () => {
    renderHeader({ title: "请求日志", description: "每一次转发都留痕" });

    // 正文一个字都没丢：它是问号圆圈的 `aria-label`（读屏与"只看无障碍树"的调用方仍拿得到全文）
    const help = screen.getByTestId("page-header-help");
    expect(help.getAttribute("aria-label")).toBe("每一次转发都留痕");

    // 但页面上**不再**有那段正文（用户原话："简介好长，好多字"）
    expect(screen.queryByText("每一次转发都留痕")).toBeNull();

    // [极简] **契约出处已经彻底删除**：它是给四个包对照文档用的开发向信息，
    // 不是给用户看的 —— 页头里不该再出现文档编号。
    const header = screen.getByTestId("page-header");
    expect(header.textContent).not.toContain("§");
    expect(header.textContent).not.toContain("契约");
  });

  it("缺省项不占位：没有 description/actions 时页头只有标题", () => {
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

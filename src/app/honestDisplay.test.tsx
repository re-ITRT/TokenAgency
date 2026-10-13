// @vitest-environment jsdom
/**
 * E2 交办的 D2/B2 显示层契约（/）：
 * **判定在后端，显示层只负责"不撒谎、不各写一份文案"**。
 *
 * 三条不变量：
 * 1. 后端的 `message`/`reason` **原样**呈现（一个字节都不改，含用户数据里的 emoji ——）；
 * 2. 禁用态必须能解释原因（且"禁用控件不派发鼠标事件"这个坑要被统一掉），
 *    **并且鼠标之外还要键盘可达、读屏可达**（B-2）—— 放进真 `<Menu>` 里也不许破坏结构（B-3）；
 * 3. 能力不支持时，outcome 显示"未知"，**不是"正常"**。
 */

import { Button, Menu, ThemeProvider } from "@mui/material";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { DisabledMenuItem, DisabledReason, LimitGapNotice, outcomeDisplay } from "@/shared/components";
import { buildTheme } from "@/theme";

afterEach(() => {
  cleanup();
});

function wrap(ui: ReactElement) {
  return render(<ThemeProvider theme={buildTheme("dark")}>{ui}</ThemeProvider>);
}

describe("LimitGapNotice：三处同一句 = 同一个组件", () => {
  it("后端原文原样呈现，组件不加一个字", () => {
    const message = "「字节方舟」未设额度上限：不填额度就无法「用完自动切」";
    const { container } = wrap(<LimitGapNotice message={message} />);
    expect(screen.getByTestId("limit-gap-notice").textContent).toBe(message);
    // 圆点是纯装饰（无文本），不会污染这句话
    expect(container.textContent).toBe(message);
  });

  it("用户数据里的 emoji 原样透传（我们自己永不产生 emoji，但用户的名字不动）", () => {
    // 用码点构造，避免测试文件自己带上 emoji
    const userData = `${String.fromCodePoint(0x1f680)} 节点选择 · 未设额度`;
    wrap(<LimitGapNotice message={userData} />);
    expect(screen.getByTestId("limit-gap-notice").textContent).toBe(userData);
  });

  it("空串 = 没有要说的：不占位，也不编一句「正常」", () => {
    const { container } = wrap(<LimitGapNotice message="" />);
    expect(container.textContent).toBe("");
    expect(screen.queryByTestId("limit-gap-notice")).toBeNull();
  });
});

describe("DisabledReason：禁用必须能说明原因（鼠标 + 键盘 + 读屏）", () => {
  const reason = "请求日志已关闭（log.mode=off），没有尝试链可看，无法按「重试」筛选";

  it("禁用时：鼠标悬停能看到原因原文（原因来自后端，不是这里写的）", async () => {
    wrap(
      <DisabledReason disabled reason={reason}>
        <Button>重试</Button>
      </DisabledReason>,
    );

    // 关键：被禁用控件外面必须有承接 hover 的容器，否则 MUI 的 tooltip 永远不出现
    const wrapper = screen.getByTestId("disabled-reason");
    fireEvent.mouseOver(wrapper);
    await waitFor(() => expect(screen.getByRole("tooltip").textContent).toContain(reason));
  });

  it("[B-2] 禁用时键盘可达：Tab 能聚焦到容器，聚焦后原因可见（原来只有鼠标能到）", async () => {
    wrap(
      <DisabledReason disabled reason={reason}>
        <Button>重试</Button>
      </DisabledReason>,
    );
    const wrapper = screen.getByTestId("disabled-reason");

    // 容器必须自己可聚焦 —— 被禁用的控件本身不可聚焦，键盘用户否则永远看不到原因
    expect(wrapper.getAttribute("tabindex")).toBe("0");
    act(() => {
      wrapper.focus();
    });
    expect(document.activeElement).toBe(wrapper);

    // 真实键盘操作（Tab）在浏览器里派发的是 `focusin`（React 的 onFocus 也走它）
    fireEvent(wrapper, new FocusEvent("focusin", { bubbles: true }));
    await waitFor(() => expect(screen.getByRole("tooltip").textContent).toContain(reason));

    // 即使提示因为任何原因没渲染，键盘/读屏用户仍能从 aria-describedby 拿到原因
    const describedBy = wrapper.getAttribute("aria-describedby");
    expect(document.getElementById(describedBy!)?.textContent).toBe(reason);
  });

  it("[B-2] 禁用时读屏可达：原因常驻无障碍树 + aria-describedby 指过去（不依赖 hover）", () => {
    wrap(
      <DisabledReason disabled reason={reason}>
        <Button>重试</Button>
      </DisabledReason>,
    );
    const wrapper = screen.getByTestId("disabled-reason");
    const describedBy = wrapper.getAttribute("aria-describedby");
    expect(describedBy).toBeTruthy();

    // 没 hover、没 focus 的情况下，原因已经在 DOM 里且被关联上
    const hidden = document.getElementById(describedBy!);
    expect(hidden?.textContent).toBe(reason);
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("可用时：容器仍在，但不挂 tabIndex/aria/提示（不给正常控件添无障碍噪音）", () => {
    wrap(
      <DisabledReason disabled={false} reason={reason}>
        <Button>重试</Button>
      </DisabledReason>,
    );
    const wrapper = screen.getByTestId("disabled-reason");
    expect(wrapper.getAttribute("tabindex")).toBeNull();
    expect(wrapper.getAttribute("aria-describedby")).toBeNull();
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });

  it("禁用但没有原因 / `reason === null`：不硬编假理由，也不挂 aria", () => {
    for (const r of ["", null]) {
      wrap(
        <DisabledReason disabled reason={r}>
          <Button>重试</Button>
        </DisabledReason>,
      );
      const wrapper = screen.getByTestId("disabled-reason");
      expect(wrapper.getAttribute("tabindex"), `reason=${String(r)}`).toBeNull();
      expect(wrapper.getAttribute("aria-describedby"), `reason=${String(r)}`).toBeNull();
      expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
      cleanup();
    }
  });
});

describe("DisabledMenuItem：放进真 <Menu> 里也不破坏结构/键盘", () => {
  const reason = "请求日志已关闭（log.mode=off），没有尝试链可看，无法按「重试」筛选";

  function renderMenu(disabled: boolean) {
    return render(
      <ThemeProvider theme={buildTheme("dark")}>
        <Menu open anchorEl={document.body}>
          <DisabledMenuItem disabled={disabled} reason={reason} value="retried">
            重试
          </DisabledMenuItem>
        </Menu>
      </ThemeProvider>,
    );
  }

  it("[B-3] `ul > span > li` 非法嵌套已消除：菜单直接子是 `li[role=menuitem]`，不是 span", () => {
    renderMenu(true);

    // Menu 渲染在 portal 里，所以查 document.body
    expect(document.body.querySelector("ul > span")).toBeNull();
    const items = document.body.querySelectorAll('ul > li[role="menuitem"]');
    expect(items.length).toBe(1);
    // 不能是 menuitem 套 menuitem（外包一层就会出现这种）
    expect(document.body.querySelectorAll('[role="menuitem"] [role="menuitem"]').length).toBe(0);
  });

  it("[B-2/B-3] 原因不需要 hover 或聚焦就看得见（键盘走不到禁用项也能读到）", () => {
    renderMenu(true);
    // 常驻次要文字：没有 mouseOver / focus 也已经渲染
    expect(screen.getByText(reason)).toBeTruthy();
    expect(screen.getByText("重试")).toBeTruthy();

    const item = document.body.querySelector('ul > li[role="menuitem"]');
    const describedBy = item?.getAttribute("aria-describedby");
    expect(describedBy).toBeTruthy();
    expect(document.getElementById(describedBy!)?.textContent).toBe(reason);
  });

  it("可用时：不显示原因、不挂 aria（菜单项可正常选择）", () => {
    renderMenu(false);
    expect(screen.queryByText(reason)).toBeNull();
    const item = document.body.querySelector('ul > li[role="menuitem"]');
    expect(item?.getAttribute("aria-disabled")).toBeNull();
    expect(item?.hasAttribute("disabled")).toBe(false);
  });
});

describe("outcomeDisplay：不支持就说未知，不说正常", () => {
  it("能力支持 → 原样显示后端给的文案", () => {
    expect(
      outcomeDisplay({ supportEnabled: true, label: "成功", unknownLabel: "未知" }),
    ).toEqual({ text: "成功", known: true });
  });

  it("能力不支持 → 显示「未知」并且标记为不可信（哪怕后端给的是 ok）", () => {
    expect(
      outcomeDisplay({ supportEnabled: false, label: "正常", unknownLabel: "未知" }),
    ).toEqual({ text: "未知", known: false });
  });
});

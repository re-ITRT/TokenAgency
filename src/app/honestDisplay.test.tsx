// @vitest-environment jsdom
/**
 * E2 交办的 D2/B2 显示层契约（[06 §10.2 D2] / [06 §10.1 B2]）：
 * **判定在后端，显示层只负责"不撒谎、不各写一份文案"**。
 *
 * 三条不变量：
 * 1. 后端的 `message`/`reason` **原样**呈现（一个字节都不改，含用户数据里的 emoji —— [15 §2.2]）；
 * 2. 禁用态必须能解释原因（且 MUI 的"禁用控件不派发鼠标事件"坑要被统一掉）；
 * 3. 能力不支持时，outcome 显示"未知"，**不是"正常"**。
 */

import { Button, MenuItem, ThemeProvider } from "@mui/material";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { DisabledReason, LimitGapNotice, outcomeDisplay } from "@/shared/components";
import { buildTheme } from "@/theme";

afterEach(() => {
  cleanup();
});

function wrap(ui: ReactElement) {
  return render(<ThemeProvider theme={buildTheme("dark")}>{ui}</ThemeProvider>);
}

describe("[06 §10.1 B2] LimitGapNotice：三处同一句 = 同一个组件", () => {
  it("后端原文原样呈现，组件不加一个字", () => {
    const message = "「字节方舟」未设额度上限：不填额度就无法「用完自动切」";
    const { container } = wrap(<LimitGapNotice message={message} />);
    expect(screen.getByTestId("limit-gap-notice").textContent).toBe(message);
    // 圆点是纯装饰（无文本），不会污染这句话
    expect(container.textContent).toBe(message);
  });

  it("用户数据里的 emoji 原样透传（我们自己永不产生 emoji，但用户的名字不动）", () => {
    // 用码点构造，避免测试文件自己带上 emoji（[15 §2.1]）
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

describe("[06 §10.2 D2] DisabledReason：禁用必须能说明原因", () => {
  const reason = "请求日志已关闭（log.mode=off），没有尝试链可看，无法按「重试」筛选";

  it("禁用时：原因原文出现在提示里（原因来自后端，不是这里写的）", async () => {
    wrap(
      <DisabledReason disabled reason={reason}>
        <MenuItem value="retried">重试</MenuItem>
      </DisabledReason>,
    );

    // 关键：被禁用控件外面必须有承接 hover 的容器，否则 MUI 的 tooltip 永远不出现
    const wrapper = screen.getByTestId("disabled-reason");
    fireEvent.mouseOver(wrapper);
    await waitFor(() => expect(screen.getByText(reason)).toBeTruthy());
  });

  it("可用时：不加包装、不挂提示（不要给正常的控件套一个没用的 tooltip）", () => {
    wrap(
      <DisabledReason disabled={false} reason={reason}>
        <Button>重试</Button>
      </DisabledReason>,
    );
    expect(screen.queryByTestId("disabled-reason")).toBeNull();
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });

  it("禁用但没有原因时：不硬编一句假的理由，退化成原样包装", () => {
    wrap(
      <DisabledReason disabled reason="">
        <Button>重试</Button>
      </DisabledReason>,
    );
    expect(screen.queryByTestId("disabled-reason")).toBeNull();
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });

  it("E2 的契约：`reason === null`（enabled 时后端就是 null）也不包装、不编假理由", () => {
    // D2 契约：Support { enabled, reason: Option<&str> } —— enabled=true 时 reason 为 null。
    // 所以调用点直接透传 `caps.retried.reason` 即可，不该各自 `?? ""` 出三种口径。
    wrap(
      <DisabledReason disabled reason={null}>
        <Button>重试</Button>
      </DisabledReason>,
    );
    expect(screen.queryByTestId("disabled-reason")).toBeNull();
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });
});

describe("[06 §10.2 D2] outcomeDisplay：不支持就说未知，不说正常", () => {
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

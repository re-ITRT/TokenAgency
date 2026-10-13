// @vitest-environment jsdom
/**
 * 「宽度不够就省略」的验收（[docs/26 §2]）—— 四件事必须被机械钉住：
 *
 * 1. **没溢出就不挂 tooltip**：既不该有 `role="tooltip"`，也不该有 `tabIndex`
 *    （否则满屏单元格各挂一层空提示 + 各占一个 Tab 站）；
 * 2. **真溢出才挂 tooltip，且全文在里面**：`scrollWidth > clientWidth` 之后，悬停**和**聚焦
 *    都能看到完整原文（键盘用户不许被落下）；
 * 3. **全文永远留在 DOM 里**：省略是 CSS 的事，任何 JS 截断都会让"复制/在页面里搜索/读屏"
 *    同时失效 —— 这一条用一段很长的原文来钉；
 * 4. **测量之前一律当作没溢出**：首帧不许闪 tooltip（宁可晚一帧，也不闪）。
 *
 * 另外附一条**回归守卫**：四个详情页文件里**不许出现** `AutoEllipsis`
 * （用户原话"我点开想看详细信息的那种肯定不加"）。守卫直接读源文件，不靠自觉。
 */

import { ThemeProvider } from "@mui/material/styles";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { ReactElement } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { AutoEllipsis, useIsOverflowing } from "@/shared/components";
import { buildTheme } from "@/theme";

// ================================================================ 测量环境（jsdom 不做布局）

/**
 * jsdom **不做布局**：元素的 `clientWidth` / `scrollWidth` 恒为 0，`ResizeObserver` 也不存在。
 * 所以这一份自己造一个可控环境：
 * - `stubBox(c, s)` 把**所有** HTML 元素的这两个读数钉死（等价于"这个窗口太窄"）——
 *   钉在原型上而不是某一个节点上：组件在"要挂 tooltip"那一刻会重挂载一次测量元素
 *   （`Tooltip` 与裸盒子在同一个位置是两个类型），只钉实例会漏掉新节点；
 * - `FakeResizeObserver` 记录回调与目标，测试手动 `trigger()`（等价于浏览器里宽度变化）。
 *
 * 下面两个 getter 只在模块加载时装一次（装上去就不摘，靠 `fakeBox` 开关）——
 * 这样不需要 `delete` 原型属性，jsdom 的原始 getter 也还在（`REAL` 里存着）。
 */
/** 当前伪造的布局读数；`null` = 用 jsdom 真实的 0。 */
let fakeBox: { clientWidth: number; scrollWidth: number } | null = null;

const REAL: Record<"clientWidth" | "scrollWidth", (() => number) | undefined> = {
  clientWidth: Object.getOwnPropertyDescriptor(Element.prototype, "clientWidth")?.get,
  scrollWidth: Object.getOwnPropertyDescriptor(Element.prototype, "scrollWidth")?.get,
};

function readBox(prop: "clientWidth" | "scrollWidth", element: HTMLElement): number {
  if (fakeBox !== null) return fakeBox[prop];
  return REAL[prop]?.call(element) ?? 0;
}

for (const prop of ["clientWidth", "scrollWidth"] as const) {
  Object.defineProperty(HTMLElement.prototype, prop, {
    configurable: true,
    get(this: HTMLElement) {
      return readBox(prop, this);
    },
  });
}

/** 把**所有** HTML 元素的读数钉死（等价于"这个窗口太窄 / 刚好够宽"）。 */
function stubBox(clientWidth: number, scrollWidth: number): void {
  fakeBox = { clientWidth, scrollWidth };
}

function restoreBox(): void {
  fakeBox = null;
}

class FakeResizeObserver {
  static instances: FakeResizeObserver[] = [];

  readonly targets = new Set<Element>();

  constructor(readonly callback: () => void) {
    FakeResizeObserver.instances.push(this);
  }

  observe(target: Element): void {
    this.targets.add(target);
  }

  unobserve(target: Element): void {
    this.targets.delete(target);
  }

  disconnect(): void {
    this.targets.clear();
  }

  /** 模拟"被观察的元素尺寸变了"：手动跑一遍回调（组件读元素本身，不吃 entries）。 */
  trigger(): void {
    this.callback();
  }
}

/** 触发所有观察者 = "窗口宽度变了"（一个 AutoEllipsis 建一个观察者）。 */
function resizeAll(): void {
  act(() => {
    for (const observer of FakeResizeObserver.instances) observer.trigger();
  });
}

/** 有没有真的在测量（而不是"忘了 observe"）。 */
function observingSomething(): boolean {
  return FakeResizeObserver.instances.some((observer) => observer.targets.size > 0);
}

beforeEach(() => {
  FakeResizeObserver.instances = [];
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = FakeResizeObserver;
});

afterEach(() => {
  cleanup();
  restoreBox();
  delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
});

function wrap(ui: ReactElement) {
  return render(<ThemeProvider theme={buildTheme("dark")}>{ui}</ThemeProvider>);
}

// ================================================================ 1. 没溢出

describe("[docs/26 §2] AutoEllipsis：没溢出 = 没有要恢复的东西", () => {
  it("没溢出：不渲染 tooltip，也不给 tabIndex（不给表格加一屏 Tab 站）", () => {
    stubBox(120, 120);
    wrap(<AutoEllipsis testId="ae">短文本</AutoEllipsis>);
    const element = screen.getByTestId("ae");

    resizeAll(); // 宽度通知来了，但这一格并不溢出

    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(element.getAttribute("tabindex")).toBeNull();

    // 悬停一个不该有提示的元素：什么都不该出现
    fireEvent.mouseOver(element);
    expect(screen.queryByRole("tooltip")).toBeNull();
    // 但组件确实在测量它（不是"干脆没测"才没提示）
    expect(observingSomething()).toBe(true);
  });

  it("测量之前恒为 false（首帧不闪 tooltip）", () => {
    // 首帧元素就已经"溢出"：如果实现里初值不是 false，第一笔记录就会是 true
    stubBox(40, 400);
    const firstRenderValues: boolean[] = [];

    function Probe() {
      const { ref, overflowing } = useIsOverflowing<HTMLDivElement>();
      firstRenderValues.push(overflowing);
      return <div ref={ref} data-testid="probe" />;
    }

    wrap(<Probe />);

    expect(firstRenderValues[0]).toBe(false);
  });
});

// ================================================================ 2. 溢出

describe("[docs/26 §2] AutoEllipsis：真溢出才给 tooltip，且键盘也能看到", () => {
  it("溢出：ResizeObserver 回调之后才有 tooltip，触发元素 tabIndex=0，悬停看得到全文", async () => {
    const text = "openai/gpt-4o-mini-2024-07-18-preview-upstream";
    wrap(<AutoEllipsis testId="ae-ov">{text}</AutoEllipsis>);

    // 一开始不溢出（jsdom 默认 0/0）→ 没有提示、也不是 Tab 站
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(screen.getByTestId("ae-ov").getAttribute("tabindex")).toBeNull();

    // 窗口变窄 → 溢出：只由 ResizeObserver 回调驱动（不靠 hover/focus）
    stubBox(60, 400);
    resizeAll();

    const element = screen.getByTestId("ae-ov");
    expect(element.textContent).toBe(text);
    expect(element.getAttribute("tabindex")).toBe("0");

    fireEvent.mouseOver(element);
    await waitFor(() => expect(screen.getByRole("tooltip").textContent).toBe(text));
  });

  it("溢出时键盘可达：聚焦也能看到全文（不是只有鼠标能到）", async () => {
    const text = "自建网关-东京-bu-1 凭据标签很长很长很长很长很长很长";
    wrap(<AutoEllipsis testId="ae-kb">{text}</AutoEllipsis>);

    stubBox(48, 300);
    resizeAll();

    const element = screen.getByTestId("ae-kb");
    expect(element.getAttribute("tabindex")).toBe("0");

    act(() => {
      element.focus();
    });
    expect(document.activeElement).toBe(element);
    // 真实键盘 Tab 在浏览器里派发的是 focusin（React 的 onFocus 也走它）
    fireEvent(element, new FocusEvent("focusin", { bubbles: true }));
    await waitFor(() => expect(screen.getByRole("tooltip").textContent).toBe(text));
  });
});

// ================================================================ 3. 全文仍在 DOM

describe("[docs/26 §2] 全文必须始终在 DOM 里（省略是 CSS，不是 JS 截断）", () => {
  it("溢出状态下文本一个字符都不少（读屏/复制/页内搜索都还能用）", async () => {
    const text = "provider-upstream-model-name-that-is-far-too-long-for-any-narrow-window";
    wrap(<AutoEllipsis testId="ae-full">{text}</AutoEllipsis>);

    stubBox(60, 900);
    resizeAll();

    const element = screen.getByTestId("ae-full");
    // 省略发生在 CSS 层：DOM 里还是整段原文
    expect(element.textContent).toBe(text);
    expect(element.textContent?.length).toBe(text.length);

    fireEvent.mouseOver(element);
    await waitFor(() => expect(screen.getByRole("tooltip").textContent).toBe(text));
    // 提示里的全文 = DOM 里的全文 = 传进来那一份（三处逐字相等）
    expect(screen.getByTestId("ae-full").textContent).toBe(screen.getByRole("tooltip").textContent);
  });

  it("用户数据里的 emoji 原样透传（我们只做裁剪，不做清洗）", () => {
    const userData = `${String.fromCodePoint(0x1f680)} 节点选择 · 香港 01`;
    wrap(<AutoEllipsis testId="ae-emoji">{userData}</AutoEllipsis>);
    expect(screen.getByTestId("ae-emoji").textContent).toBe(userData);
  });
});

// ================================================================ 4. 回归守卫：详情页零用法

describe("[docs/26 §2] 回归守卫：详情页里不许出现 AutoEllipsis", () => {
  /**
   * 用户原话：「只限于一级界面……我点开想看详细信息的那种肯定不加」。
   * 这三个文件就是"点进去看详细"的那几个，守卫直接读源码文本 ——
   * 谁把它们也包上省略，这一条立刻红（而不是等到有人抱怨"详情页也看不全了"）。
   *
   * [docs/55]（第 322 轮）`pages/network/assignments.tsx` 已从那四个里去掉：
   * 「线路分配」整页删除（用户："线路级别的分配逻辑我们已经放弃了"）。
   */
  const DETAIL_ONLY_FILES = [
    "pages/providers/detail.tsx",
    "pages/network/nodes.tsx",
    "pages/logs/TracePanel.tsx",
  ];

  it.each(DETAIL_ONLY_FILES)("%s 里零 AutoEllipsis 用法", (relative) => {
    // 路径基准 = 本文件所在目录（ESM 里没有 __dirname）
    const here = path.dirname(fileURLToPath(import.meta.url));
    const source = readFileSync(path.resolve(here, "..", relative), "utf8");

    // 文件必须真的读到了（防止路径写错让守卫空跑成绿色）
    expect(source.length).toBeGreaterThan(0);
    expect(source).not.toContain("AutoEllipsis");
  });
});

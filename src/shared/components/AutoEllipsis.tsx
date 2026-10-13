/**
 * 「宽度不够就省略」（[docs/26 §2]）—— **只在一级界面用**。
 *
 * 用户第 78 轮原话：长信息类的东西在窗口不够宽时收起来，**只限侧边栏能直接到的页面**；
 * 并且「给任意地方加省略逻辑之前，先确认这个被省略隐藏的信息可以重新看到」。
 * 所以这个组件只解决**一半**问题（怎么省略），另一半（"怎么再看一遍"）由**使用点**负责，
 * 每个调用处都必须在注释里写出它的恢复路径 —— 两条路径都没有的值，**不许省略**。
 *
 * 四条硬规则（全部会被 `src/app/autoEllipsis.test.tsx` 钉住）：
 *
 * 1. **CSS 省略，绝不 JS 截断**：`overflow:hidden + text-overflow:ellipsis + white-space:nowrap`。
 *    字符串原文**始终整段留在 DOM 里** —— 读屏要读全、复制要能复制、"在页面里搜索"要搜得到。
 *    （任何 `text.slice(0, n) + "…"` 都同时违反这三件事，而且会让"搜不到"变成不可诊断的 bug。）
 * 2. **只在真溢出时才挂 tooltip**：`ResizeObserver` 比 `scrollWidth > clientWidth`。
 *    没溢出就**不渲染 Tooltip**（不是渲染一个标题为空的）—— 否则满屏每个单元格都挂一层
 *    MUI Tooltip，焦点/悬停噪音与开销都是白付的。
 * 3. **溢出时触发元素键盘可达**：`tabIndex={0}`，键盘用户聚焦即可看到全文（MUI Tooltip 自己
 *    监听 focus/blur）。**没溢出就一个 tabIndex 都不加** —— 不给表格加一屏 Tab 站。
 * 4. **测量之前一律当作"没溢出"**：初值 `false`，`ResizeObserver`（或挂载后那一次测量）
 *    报出真溢出才切过去。宁可晚一帧出 tooltip，也不要在首帧闪一个空提示。
 *
 * 为什么不设 `aria-label`（[docs/26 §2] 第 3 条只说"全文必须仍在 DOM 里"）：
 * 读屏是按 DOM 文本流的顺序念的，**被省略的只是视觉**，全文就在元素里，本来就会完整念出来。
 * 再加一个 `aria-label={children}` 等于把同一句话让读屏念两遍（或让无障碍树里出现"标签 ≠ 内容"
 * 的重复节点），而它一个字的信息量都没增加。真正需要补的那一半是无障碍树里**没有**的东西 ——
 * 这里没有这种东西，所以**故意不写** `aria-label`。
 *
 * 约束：颜色/间距只能走令牌与刻度（`design.guard.test.ts` 机械检查），本组件不写颜色、
 * 不写刻度外间距、不出现 emoji。
 */

import { Box, Tooltip } from "@mui/material";
import type { SxProps, Theme } from "@mui/material/styles";
import { useCallback, useEffect, useState } from "react";

/**
 * 省略样式（**唯一来源**）。`display: block` 是必须的：`text-overflow` 对 inline 盒子不生效，
 * 而这一层常常落进 `<td>` 里；`maxWidth: 100%` 保证它不会反过来把父格撑宽（宽度约束由使用点给）。
 */
const ELLIPSIS_SX = {
  display: "block",
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
  maxWidth: "100%",
} as const;

/** `SxProps` 里"一维数组"的那一支（MUI 的 `sx` **不吃嵌套数组**，所以要自己摊平）。 */
type SxArray = Extract<SxProps<Theme>, readonly unknown[]>;
type SxItem = SxArray extends readonly (infer Item)[] ? Item : never;

function isSxArray(sx: SxProps<Theme>): sx is SxArray {
  return Array.isArray(sx);
}

/** 使用点的 `sx` 归一成一维数组：省略样式**永远在前**，使用点的样式在后（后者覆盖前者）。 */
function mergeSx(sx: SxProps<Theme> | undefined): SxProps<Theme> {
  if (sx === undefined) return ELLIPSIS_SX;
  const extra: readonly SxItem[] = isSxArray(sx) ? sx : [sx];
  return [ELLIPSIS_SX, ...extra];
}

export type UseIsOverflowingResult<E extends HTMLElement = HTMLElement> = {
  /** 挂到被测量的元素上（回调 ref：节点挂载/卸载时自动重新测量/断开观察）。 */
  ref: (node: E | null) => void;
  /** 是否**真的**溢出。测量完成前恒为 `false`（见上面第 4 条）。 */
  overflowing: boolean;
};

/**
 * 「这个元素现在溢出吗」：`scrollWidth > clientWidth` + `ResizeObserver`。
 *
 * 纯浏览器测量，不碰数据层（没有 IPC、没有查询）；`ResizeObserver` 不存在时（SSR / jsdom）
 * 退化成"挂载后测一次"，不会抛错。
 */
export function useIsOverflowing<E extends HTMLElement = HTMLElement>(): UseIsOverflowingResult<E> {
  /** 用 state 存节点（而不是 ref）：节点一确定，下面的 effect 才会跑一次并建观察者。 */
  const [node, setNode] = useState<E | null>(null);
  const [overflowing, setOverflowing] = useState(false);
  const ref = useCallback((next: E | null) => setNode(next), []);

  useEffect(() => {
    if (node === null) return;
    const measure = (): void => setOverflowing(node.scrollWidth > node.clientWidth);
    // 挂载后先量一次：不等第一次 RO 回调，避免"明明溢出却一直没有 tooltip"
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [node]);

  return { ref, overflowing };
}

export type AutoEllipsisProps = {
  /** 要显示的**字符串原文**（不是节点）：全文既要进 DOM，也要当 tooltip 正文。 */
  children: string;
  /**
   * 内容元素的 testid（可选）。**不要**拿它替代 `<td>` 上的 testid ——
   * 这一层是格子里面的那一层。
   */
  testId?: string;
  /** 额外样式（宽度约束由使用点给：`maxWidth` 之类）。省略样式本身不可覆盖。 */
  sx?: SxProps<Theme>;
};

/**
 * 一级界面里"宽度不够就收成…、悬停/聚焦能看全"的那一层（[docs/26 §2]）。
 *
 * 用法与责任（**每个调用点都要在注释里写清恢复路径**，例如"本页没有详情页 → tooltip 就是恢复路径"）：
 * ```tsx
 * <TableCell data-testid="channels-upstream">
 *   <AutoEllipsis testId="channels-upstream-ellipsis" sx={{ maxWidth: 280 }}>
 *     {row.upstreamModel}
 *   </AutoEllipsis>
 * </TableCell>
 * ```
 * **宽度约束放在 `sx.maxWidth` 上**（不是只放在 `<td>` 上）：省略要生效，这一层必须有确定宽度；
 * 而 `maxWidth` 同时会**钳住这一格对列的 min/max-content 贡献**，所以列也不会被 nowrap 的长文本撑开。
 *
 * 已知的一处实现细节（不是 bug）：溢出状态翻转时，这一层外面会多/少一层 `Tooltip`，
 * 于是 React 会重建这个 `<span>`（同一位置换了类型）。两种分支的标签/样式/文本完全一样，
 * 浏览器里尺寸不变、不会闪；组件测试用"钉在原型上的读数"覆盖了重建后的新节点。
 */
export function AutoEllipsis({ children, testId, sx }: AutoEllipsisProps) {
  const { ref, overflowing } = useIsOverflowing<HTMLSpanElement>();

  const content = (
    <Box
      component="span"
      ref={ref}
      data-testid={testId}
      // 只有真的藏了东西时才是 Tab 站（见文件头第 3 条）
      tabIndex={overflowing ? 0 : undefined}
      sx={mergeSx(sx)}
    >
      {children}
    </Box>
  );

  // 没溢出 = 没有要恢复的东西 → 不挂 tooltip（文件头第 2 条）
  if (!overflowing) return content;

  return (
    <Tooltip title={children} placement="top">
      {content}
    </Tooltip>
  );
}

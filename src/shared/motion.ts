/**
 * 动效的**机械约定** —— [docs/26 §1] 把 [15 §7] 的规则变成可执行的东西。
 *
 * 为什么单独一个模块而不是散在 `theme.ts` / 守卫里各写一份：
 * 这些字符串同时被**实现**（`theme.ts` 的 `prefers-reduced-motion` 收口）与**守卫**
 * （`src/app/motion.test.ts`）使用。写在两处必然漂 —— 而"守卫和实现各说各话"比没有守卫更糟，
 * 因为它给人"已经守住了"的错觉。
 *
 * 边界：这里**只有字符串与解析器**，一个毫秒数都不复述；
 * 时长/缓动的取值仍然只在 `theme.ts` 的 `motion` 令牌里（[15 §7] 的唯一来源）。
 */

/**
 * 系统说"不要动画"时的媒体查询（[15 §7]）。
 *
 * **必须用 CSS 媒体查询实现，不用 JS 判断**（[26 §1.3]）：JS 判断会有"首帧已经动了才发现"的闪烁，
 * 而这正是我们要防的那种跳变。
 */
export const REDUCED_MOTION_QUERY = "@media (prefers-reduced-motion: reduce)";

/**
 * 停用循环动画时用的时长（不是"更快的时长"，是"停"）。
 *
 * 刻意**不写 `animation: none`**：MUI 的 `TouchRipple` 靠 `animationend` 收尾，
 * 没有事件它会把水波纹留在 DOM 里（那是个真缺陷，不是一个视觉取舍）。
 */
export const MOTION_OFF_DURATION = "0.01ms";

/** 循环动画在 reduced-motion 下的迭代次数：1（`infinite` 死在这里）。 */
export const MOTION_OFF_ITERATIONS = "1";

/**
 * reduced-motion 下**保留**的过渡属性（全是颜色类）。
 *
 * [26 §1.3]：颜色变化不引发前庭反应，可以留；位移/缩放/宽度/stroke 必须瞬变。
 * 用 `transition-property: <这份清单> !important` 而不是把时长压到 0.01ms ——
 * 后者会把颜色反馈一起杀掉（改动"啪"地跳一下，反而更刺眼）。
 */
export const COLOR_TRANSITION_PROPERTIES = [
  "background-color",
  "border-color",
  "color",
  "fill",
  "outline-color",
  "stroke",
] as const;

/**
 * 允许被过渡的属性（[26 §1.5] 不许引发布局抖动）。
 *
 * `width` 只许出现在进度条上（它已经在动，且不改变其它元素的几何）；
 * `height` / `margin` / `padding` / `top` / `left` 一律不许 —— 那会在每帧触发重排。
 */
export const ANIMATABLE_PROPERTIES = ["opacity", "stroke-dashoffset", "transform", "width"] as const;

/**
 * `"180ms"` → `180`。
 *
 * MUI 的 `transitions.duration.*` 要数字，而我们的令牌是 CSS 字符串（组件里要用）。
 * 只接受 **ms** 形式：`Number.parseInt` 会把 `"cubic-bezier(.2,0,0,1)"` 静默解析成 `NaN`，
 * 那种错误会在运行时变成"过渡时长无效"而不报错 —— 所以这里直接抛。
 */
export function toMs(token: string): number {
  const match = /^(\d+(?:\.\d+)?)ms$/.exec(token.trim());
  if (match?.[1] === undefined) {
    throw new Error(`动效令牌必须是 ms 字符串（收到 ${JSON.stringify(token)}）`);
  }
  return Number(match[1]);
}

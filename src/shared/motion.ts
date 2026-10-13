/**
 * 动效的**机械约定**：把"尊重 reduced-motion"这条规则变成可执行、可被守卫核对的东西。
 *
 * 为什么单独一个模块而不是散在 `theme.ts` / 守卫里各写一份：
 * 这些字符串同时被**实现**（`theme.ts`）与**守卫**（`motion.test.ts`）使用。写在两处必然漂 ——
 * 而"守卫和实现各说各话"比没有守卫更糟，因为它给人"已经守住了"的错觉。
 *
 * 边界：这里只有字符串与解析器，一个毫秒数都不复述；时长/缓动的取值只在 `theme.ts` 的令牌里。
 */

/**
 * 系统说"不要动画"时的媒体查询。
 *
 * **必须用 CSS 媒体查询实现，不用 JS 判断**：JS 判断会有"首帧已经动了才发现"的闪烁，
 * 而这正是我们要防的那种跳变。
 */
export const REDUCED_MOTION_QUERY = "@media (prefers-reduced-motion: reduce)";

/**
 * 停用循环动画时用的时长（是"停"，不是"更快"）。
 *
 * 刻意**不写 `animation: none`**：MUI 的 `TouchRipple` 靠 `animationend` 收尾，
 * 没有事件它会把水波纹留在 DOM 里 —— 那是真缺陷，不是视觉取舍。
 */
export const MOTION_OFF_DURATION = "0.01ms";

/** 循环动画在 reduced-motion 下的迭代次数：1（`infinite` 死在这里）。 */
export const MOTION_OFF_ITERATIONS = "1";

/**
 * reduced-motion 下**保留**的过渡属性（全是颜色类）。
 *
 * 颜色变化不引发前庭反应，可以留；位移/缩放/宽度/stroke 必须瞬变。
 * 用 `transition-property: <这份清单> !important`，而不是把时长压到 0.01ms ——
 * 后者会把颜色反馈一起杀掉，改动"啪"地跳一下反而更刺眼。
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
 * 允许被过渡的属性（不许引发布局抖动）。
 *
 * `width` 只许用在进度条上（它已经在动）；`height` / `margin` / `padding` / `top` / `left`
 * 一律不许 —— 那会在每帧触发重排。
 */
export const ANIMATABLE_PROPERTIES = ["opacity", "stroke-dashoffset", "transform", "width"] as const;

/**
 * `"180ms"` → `180`；不是 ms 形式就**抛**。
 *
 * MUI 的 `transitions.duration.*` 要数字，而令牌是 CSS 字符串。不静默兜底的原因：
 * `Number.parseInt` 会把 `"cubic-bezier(.2,0,0,1)"` 解析成 `NaN`，运行时只表现为
 * "过渡时长无效"而不报错。
 */
export function toMs(token: string): number {
  const match = /^(\d+(?:\.\d+)?)ms$/.exec(token.trim());
  if (match?.[1] === undefined) {
    throw new Error(`动效令牌必须是 ms 字符串（收到 ${JSON.stringify(token)}）`);
  }
  return Number(match[1]);
}

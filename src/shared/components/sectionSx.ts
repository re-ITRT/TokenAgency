import { radius } from "@/theme";

/**
 * 页面里"一个区块"的外框内边距 + 圆角（`<Paper variant="outlined" sx={SECTION_SX}>`）。
 *
 * **为什么值得单独成常量**：这一行在 **7 个页面**逐字重复
 * （[docs/50] 的重复清单）。它不是"随手写的样式"而是**视觉规则**
 * （[15 §3.3] 的间距刻度 + 圆角令牌），所以必须只有一处真源 ——
 * 否则调圆角时会改到其中几个页面，界面出现"差不多的两种区块"。
 *
 * `as const` 保留：MUI 的 `sx` 接受只读对象，而 `as const` 能防止有人在这里
 * 顺手改成运行时可变的值。
 *
 * 注意：**不要**把它写进 `theme.tokens` 守卫之外的地方 —— 组件里不许出现颜色/刻度字面量
 * （`design.guard.test.ts` 会机械检查），所以这里只引用 `radius` 令牌，不写数字。
 */
export const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

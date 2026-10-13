/**
 * F1–F4 共用组件（[12 §6] 明写的最小集）。
 *
 * 视觉规则一律来自 **[15] 视觉设计系统**（该文是界面观感的唯一裁决处），不要在各页面自创：
 * - 页头：`<PageHeader>`（标题 `text.page` / 说明 / 契约出处 / 右侧操作）
 * - 表格：`useDataGridPreset()` + `dataGridSx`（[15 §5] 的行高/表头/选中/悬停都在这里）
 * - 数字排版：`<NumberCard>`（`text.hero` 等宽数字、缩写、悬停精确值）
 * - 状态灯：`<StatusDot>`（绿=正常 黄=警告 红=失败 强调=进行中 灰=未启用；6px 圆点 + 文字）
 * - 趋势：`<Sparkline>`（纯 SVG，零依赖）
 * - 宽度不够就省略：`<AutoEllipsis>` + `useIsOverflowing`（[docs/26 §2]：**只在一级界面用**，
 *   CSS 省略 + 溢出才挂 tooltip；每个调用点必须写明"被藏起来的信息怎么再看一遍"）
 * - 诚实显示层（[06 §10.1 B2] / [06 §10.2 D2]）：`<LimitGapNotice>`、`<DisabledReason>`（非列表控件）、
 *   `<DisabledMenuItem>`（菜单选项）、`outcomeDisplay()`
 *   —— 判定在后端，**前端只消费原文**，不许自己重判、也不许自己写第二份文案。
 *
 * 约束：**组件里不许写颜色字面量、不许写刻度外间距、不许出现 emoji**
 * —— `src/app/design.guard.test.ts` 会机械检查（[15 §8]）。
 */

export { PageHeader, type PageHeaderProps } from "./PageHeader";
export {
  AutoEllipsis,
  useIsOverflowing,
  type AutoEllipsisProps,
  type UseIsOverflowingResult,
} from "./AutoEllipsis";
export {
  dataGridDefaults,
  dataGridLocaleText,
  dataGridSx,
  useDataGridPreset,
} from "./dataGrid";
export {
  DisabledMenuItem,
  DisabledReason,
  type DisabledMenuItemProps,
  type DisabledReasonProps,
} from "./DisabledReason";
export { LimitGapNotice, type LimitGapNoticeProps } from "./LimitGapNotice";
export { HelpTip, type HelpTipProps } from "./HelpTip";
export { ConfirmDelete, type ConfirmDeleteProps } from "./confirmDelete";
export { outcomeDisplay, type OutcomeDisplay, type OutcomeDisplayArgs } from "./honestDisplay";

export { NumberCard, type NumberCardProps, type ValueFormat } from "./NumberCard";
export { Caption } from "./Caption";
export { SECTION_SX } from "./sectionSx";
export { StatItem, type StatItemProps } from "./StatItem";
export { SwitchControl } from "./SwitchControl";

export { Sparkline, type SparklineProps } from "./Sparkline";
export { StatusDot, type SemStatus, type StatusDotProps } from "./StatusDot";
export {
  KernelStateDot,
  KERNEL_LABEL_KEY,
  KERNEL_SEM_STATUS,
  type KernelStateDotProps,
} from "./KernelStateDot";

/**
 * F1–F4 共用组件（[12 §6] 明写的最小集）。
 *
 * 视觉规则一律来自 [07 §1.3]，不要在各页面自创：
 * - 数字排版：`<NumberCard>`（等宽数字、缩写、悬停精确值）
 * - 进度/额度：`<QuotaBar>`（<70% 绿 / 70–90% 黄 / >90% 红并脉动；未设上限必须显式）
 * - 状态灯：`<StatusDot>`（绿=正常 黄=警告 红=失败 蓝=进行中 灰=未启用）
 * - 趋势：`<Sparkline>`（纯 SVG，零依赖）
 */

export { NumberCard, type NumberCardProps, type ValueFormat } from "./NumberCard";
export { QuotaBar, quotaLevel, type QuotaBarProps, type QuotaLevel } from "./QuotaBar";
export { Sparkline, type SparklineProps } from "./Sparkline";
export { StatusDot, type SemStatus, type StatusDotProps } from "./StatusDot";

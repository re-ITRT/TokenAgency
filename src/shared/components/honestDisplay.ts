/**
 * 「未知不许说成正常」—— 的显示层铁律，做成一个纯函数而不是一条口头约定。
 *
 * 背景：日志模式 `mode = off` 时 `filters.status = retried` **判不了**。
 * 落库层保留"未知即 ok"的实现（不猜不估算的边界），但**显示层必须显示"未知"** ——
 * 把不确定说成"正常"是拿不确定冒充确定，用户会据此做错误判断。
 *
 * 判定来源是后端的 `outcome_display_support(mode)`（`{ enabled, reason }`），
 * 前端只消费：**能力说不行 → 显示未知**。前端不许自己推断 mode。
 */

/** 显示结果：`known = false` 时调用方应把它当"未知"画（弱化，不给语义色）。 */
export type OutcomeDisplay = {
  /** 已经过诚实性检查的文案（由调用方传入 i18n 渲染好的文案）。 */
  text: string;
  /** `false` = 后端能力不支持，此值不可信。 */
  known: boolean;
};

export type OutcomeDisplayArgs = {
  /** `outcome_display_support(mode).enabled`。 */
  supportEnabled: boolean;
  /** 后端返回的 outcome 经 i18n 渲染后的文案（如"成功"/"重试"）。 */
  label: string;
  /** "未知"文案（`t("common.unknown")`）。 */
  unknownLabel: string;
};

/**
 * 唯一的诚实性入口：支持 → 原样显示；不支持 → 显示未知。
 *
 * 注意 `supportEnabled === true` 时也要允许 `label` 是"未知"（后端自己给了 unknown），
 * 本函数不做二次猜测。
 */
export function outcomeDisplay({
  supportEnabled,
  label,
  unknownLabel,
}: OutcomeDisplayArgs): OutcomeDisplay {
  if (!supportEnabled) return { text: unknownLabel, known: false };
  return { text: label, known: true };
}

/**
 * 内核状态 → 「圆点 + 文字」的**唯一落点**（状态条与看板共用）。
 *
 * 为什么抽出来：`KernelState` 到语义色/文案的映射如果各页各写一份，
 * 同一次「restarting」就会在两处显示成不同颜色或不同措辞 —— 那正是我们一直在防的"两个真相源"。
 *
 * 语义色沿用 [15 §3.2]（绿=成功 · 黄=需注意 · 红=失败 · 强调=进行中 · 灰=未知/停用）。
 *
 * **诚实边界**：这里只表达"内核进程状态"。**"内核在跑但 provider 没加载"是另一件事**
 * （[06 §2.1] B2）—— 当前 IPC 契约（`KernelState`）里没有这个判据，
 * 所以调用方**不许**把它显示成绿色；看板把那一维如实标成"未知"。
 */

import type { KernelState } from "@/shared/bindings";
import { useTranslation } from "@/shared/i18n";

import { StatusDot, type SemStatus } from "./StatusDot";

/** `KernelState` → 全站语义色。 */
export const KERNEL_SEM_STATUS: Record<KernelState, SemStatus> = {
  stopped: "off",
  starting: "info",
  ready: "ok",
  restarting: "warn",
  failed: "error",
};

/** `KernelState` → i18n key（`zh-CN.ts` 是真源）。 */
export const KERNEL_LABEL_KEY = {
  stopped: "status.kernel.stopped",
  starting: "status.kernel.starting",
  ready: "status.kernel.ready",
  restarting: "status.kernel.restarting",
  failed: "status.kernel.failed",
} as const satisfies Record<KernelState, string>;

export type KernelStateDotProps = {
  /** `undefined` = 还拿不到状态 → 显示"未知"，**不要**默认成 `stopped`（那是编事实）。 */
  state: KernelState | undefined;
  /** 是否带 `内核` 前缀（状态条上是"内核 就绪"）。 */
  withLabelPrefix?: boolean;
};

export function KernelStateDot({ state, withLabelPrefix = false }: KernelStateDotProps) {
  const { t } = useTranslation();

  if (state === undefined) {
    return <StatusDot status="off" label={t("common.unknown")} />;
  }

  const label = t(KERNEL_LABEL_KEY[state]);
  return (
    <StatusDot
      status={KERNEL_SEM_STATUS[state]}
      label={withLabelPrefix ? `${t("status.kernel.label")} ${label}` : label}
    />
  );
}

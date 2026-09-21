/**
 * 「为什么这个控件是灰的」—— 把**后端给的原因原文**挂在被禁用的控件上（[06 §10.2 D2]）。
 *
 * 为什么需要它，而不是各页自己写 `disabled` + `Tooltip`：
 * 1. **MUI 的经典坑**：禁用的控件不派发鼠标事件 → 直接套 `Tooltip` 时**提示永远不出现**，
 *    必须包一层容器元素。这里把那层容器统一掉。
 * 2. **文案不能各写一份**：[06 §10.2 D2] 要求"禁用该筛选项并说明原因"，
 *    原因原文来自后端（如 `ta_core::diag::query::retried_filter_support(mode).reason`）。
 *    前端自己写文案 → 设置页/日志页/首启三处就会漂。
 *
 * 用法（日志页的"重试"筛选项）：
 * ```tsx
 * // E2 的契约：enabled=true 时 reason 为 null，所以这里直接透传，不要在调用点 ?? ""
 * <DisabledReason disabled={!caps.retried.enabled} reason={caps.retried.reason}>
 *   <MenuItem value="retried">重试</MenuItem>
 * </DisabledReason>
 * ```
 */

import { Box, Tooltip } from "@mui/material";
import type { ReactElement } from "react";

export type DisabledReasonProps = {
  /** 是否禁用。**由后端能力决定**（前端不重判）。 */
  disabled: boolean;
  /**
   * 禁用原因的**原文**。契约（E2 的 `Support`）：`enabled=true` → `null`；
   * `null`/空串 = 没原因可说 → 不包装、不编一句假理由。
   */
  reason: string | null;
  /** 被包装的控件（`MenuItem` / `Button` / `Checkbox`…）。 */
  children: ReactElement;
};

export function DisabledReason({ disabled, reason, children }: DisabledReasonProps) {
  if (!disabled || reason === null || reason === "") return children;

  return (
    <Tooltip title={reason}>
      {/* 禁用的控件不派发鼠标事件，必须用容器承接 hover —— 否则提示永远不出现 */}
      <Box
        component="span"
        data-testid="disabled-reason"
        sx={{ display: "inline-flex", alignItems: "center", cursor: "not-allowed", minWidth: 0 }}
      >
        {children}
      </Box>
    </Tooltip>
  );
}

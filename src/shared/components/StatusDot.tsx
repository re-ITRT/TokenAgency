/**
 * 状态灯。**全站颜色语义唯一**（沿用）：
 * 绿 = 成功/正常 · 黄 = 警告/重试 · 红 = 失败 · 强调色 = 选中/进行中 · 灰 = 未知/未启用。
 *
 *：状态**不用彩色 chip**，用一个 **6px 圆点 + 文字**。
 * 不许在页面里自己写颜色点 —— 出现第五种含义就是 bug。
 *
 *：**唯一允许的循环动效是"额度 >90% 时的轻微脉动"**（在 `QuotaBar` 里），
 * 所以状态点**不做任何动画** —— "到处闪"正是廉价感的来源。
 */

import { Box, Tooltip } from "@mui/material";

import { typography } from "@/theme";
import { useTranslation } from "@/shared/i18n";

/** 全站统一的状态语义（与 `bindings.ts` 里的视图状态字段对齐）。 */
export type SemStatus = "ok" | "warn" | "error" | "info" | "off";

const COLOR: Record<SemStatus, string> = {
  ok: "success.main",
  warn: "warning.main",
  error: "error.main",
  info: "primary.main",
  off: "text.disabled",
};

export type StatusDotProps = {
  status: SemStatus;
  /** 有标签时点在前、文字在后（状态条/表格状态列用法） */
  label?: string;
  /** 默认 6px；工具栏等紧凑处可传 4。 */
  size?: number;
};

export function StatusDot({ status, label, size = 6 }: StatusDotProps) {
  const { t } = useTranslation();
  const text = label ?? t(`component.statusDot.${status}`);

  const dot = (
    <Box
      component="span"
      sx={{
        display: "inline-block",
        width: size,
        height: size,
        borderRadius: "50%",
        bgcolor: COLOR[status],
        flex: "0 0 auto",
      }}
    />
  );

  if (label === undefined) {
    return (
      <Tooltip title={text}>
        <Box component="span" sx={{ display: "inline-flex", alignItems: "center" }}>
          {dot}
        </Box>
      </Tooltip>
    );
  }

  return (
    <Box component="span" sx={{ display: "inline-flex", alignItems: "center", gap: 1, minWidth: 0 }}>
      {dot}
      <Box
        component="span"
        sx={{
          fontSize: typography.size.body,
          lineHeight: typography.lineHeight.table,
          color: "text.primary",
          whiteSpace: "nowrap",
        }}
      >
        {label}
      </Box>
    </Box>
  );
}

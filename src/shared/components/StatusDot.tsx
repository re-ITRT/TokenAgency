/**
 * 状态灯。**全站颜色语义唯一**（[07 §1.3]）：
 * 绿 = 成功/正常 · 黄 = 警告/重试 · 红 = 失败 · 蓝 = 主色/进行中 · 灰 = 未启用。
 *
 * 不许在页面里自己写颜色点 —— 出现第五种含义就是 bug。
 */

import { Box, Tooltip } from "@mui/material";
import type { Theme } from "@mui/material/styles";

import { useTranslation } from "@/shared/i18n";

/** 全站统一的状态语义（与 `bindings.ts` 里的视图状态字段对齐）。 */
export type SemStatus = "ok" | "warn" | "error" | "info" | "off";

const COLOR: Record<SemStatus, (t: Theme) => string> = {
  ok: (t) => t.palette.success.main,
  warn: (t) => t.palette.warning.main,
  error: (t) => t.palette.error.main,
  info: (t) => t.palette.primary.main,
  off: (t) => t.palette.text.disabled,
};

export type StatusDotProps = {
  status: SemStatus;
  /** 有标签时点在后、文字在前（状态条用法） */
  label?: string;
  /** 强制脉动；默认只有 `error` 会动（[07 §1.3]：只在红时用动效，别到处闪） */
  pulse?: boolean;
  size?: number;
};

export function StatusDot({ status, label, pulse, size = 8 }: StatusDotProps) {
  const { t } = useTranslation();
  const animated = pulse ?? status === "error";
  const text = label ?? t(`component.statusDot.${status}`);

  const dot = (
    <Box
      component="span"
      sx={(theme) => ({
        display: "inline-block",
        width: size,
        height: size,
        borderRadius: "50%",
        bgcolor: COLOR[status](theme),
        flex: "0 0 auto",
        ...(animated
          ? {
              animation: "ta-pulse 1.6s ease-in-out infinite",
              "@keyframes ta-pulse": {
                "0%, 100%": { opacity: 1 },
                "50%": { opacity: 0.35 },
              },
            }
          : {}),
      })}
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
    <Box
      component="span"
      sx={{ display: "inline-flex", alignItems: "center", gap: 0.75, minWidth: 0 }}
    >
      {dot}
      <Box component="span" sx={{ fontSize: 12, lineHeight: 1.2, whiteSpace: "nowrap" }}>
        {label}
      </Box>
    </Box>
  );
}

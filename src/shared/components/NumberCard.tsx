/**
 * 数字卡（看板 4 张卡：今日请求 / 今日 Token / 今日花费 / 成功率）。
 *
 * 按 [07 §1.3] 的排版规则：
 * - 大号**等宽数字**（`tabular-nums`）+ 小号单位，千分位；
 * - Token 用 `12.4M` 缩写，**悬停显示精确值**；
 * - 与上期比较的涨跌用同一套颜色语义（绿好 / 红坏，`goodWhenPositive` 可翻转）。
 */

import { Box, Paper, Skeleton, Tooltip, Typography } from "@mui/material";
import type { ReactNode } from "react";

import { formatExact, formatInt, formatMoney, formatPercent, formatTokens } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";

import { Sparkline } from "./Sparkline";
import type { SemStatus } from "./StatusDot";

/** 数值格式化方式（避免页面各自 `toFixed`）。 */
export type ValueFormat = "int" | "tokens" | "money" | "percent";

export type NumberCardProps = {
  label: string;
  value: number | string;
  format?: ValueFormat;
  /** `format: "money"` 时必填。 */
  currency?: string;
  /** 单位后缀（如 `tok` / `/天`）。 */
  unit?: string;
  /** 悬停展示的精确值；传 `true` 表示用 `value` 自动生成。 */
  exact?: string | true;
  /** 迷你趋势线数据（按时间升序）。 */
  sparkline?: readonly number[];
  /** 与上期比较：正数=上升。 */
  delta?: { value: number; goodWhenPositive?: boolean };
  icon?: ReactNode;
  /** 强调色（用 <70% 绿 / 警告黄 / 失败红那套语义）。 */
  tone?: Extract<SemStatus, "ok" | "warn" | "error" | "info">;
  loading?: boolean;
};

/** 语义色 → MUI 调色板键（全站统一：绿=成功、黄=警告、红=失败、蓝=主色）。 */
const TONE_KEY: Record<"ok" | "warn" | "error" | "info", string> = {
  ok: "success",
  warn: "warning",
  error: "error",
  info: "primary",
};

function renderValue(props: NumberCardProps): string {
  const { value, format, currency } = props;
  if (typeof value === "string") return value;
  switch (format) {
    case "int":
      return formatInt(value);
    case "tokens":
      return formatTokens(value);
    case "money":
      return formatMoney({ amount: value, currency: currency ?? "USD" });
    case "percent":
      return formatPercent(value);
    default:
      return formatExact(value);
  }
}

function exactText(props: NumberCardProps): string | undefined {
  const { value, exact, currency, format } = props;
  if (exact === undefined) return undefined;
  if (typeof exact === "string") return exact;
  if (typeof value !== "number") return value;
  if (format === "money") {
    return formatMoney({ amount: value, currency: currency ?? "USD" });
  }
  return formatExact(value);
}

export function NumberCard(props: NumberCardProps) {
  const { label, unit, sparkline, delta, icon, tone, loading } = props;
  const { t } = useTranslation();

  if (loading === true) {
    return (
      <Paper variant="outlined" sx={{ p: 1.5, minWidth: 0 }}>
        <Skeleton width={72} height={14} />
        <Skeleton width={120} height={34} />
      </Paper>
    );
  }

  const shown = renderValue(props);
  const exact = exactText(props);
  const tonePath = tone === undefined ? undefined : `${TONE_KEY[tone]}.main`;

  return (
    <Paper variant="outlined" sx={{ p: 1.5, minWidth: 0, height: "100%" }}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 0.75, color: "text.secondary" }}>
        {icon}
        <Typography variant="caption" noWrap>
          {label}
        </Typography>
      </Box>

      <Tooltip title={exact === undefined ? "" : t("component.numberCard.hintExact", { value: exact })}>
        <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.5, mt: 0.25 }}>
          <Typography
            component="span"
            sx={{
              fontSize: 26,
              fontWeight: 600,
              lineHeight: 1.15,
              fontVariantNumeric: "tabular-nums",
              color: tonePath,
            }}
          >
            {shown}
          </Typography>
          {unit !== undefined ? (
            <Typography component="span" variant="caption" sx={{ color: "text.secondary" }}>
              {unit}
            </Typography>
          ) : null}
          {delta !== undefined ? (
            <Typography
              component="span"
              variant="caption"
              sx={(theme) => ({
                ml: 0.5,
                fontVariantNumeric: "tabular-nums",
                color:
                  (delta.value >= 0) === (delta.goodWhenPositive ?? true)
                    ? theme.palette.success.main
                    : theme.palette.error.main,
              })}
            >
              {delta.value >= 0 ? "▲" : "▼"} {formatPercent(Math.abs(delta.value), 1)}
            </Typography>
          ) : null}
        </Box>
      </Tooltip>

      {sparkline !== undefined && sparkline.length > 0 ? (
        <Box sx={{ mt: 0.5, color: tonePath ?? "text.secondary" }}>
          <Sparkline data={sparkline} label={t("component.sparkline.label")} />
        </Box>
      ) : null}
    </Paper>
  );
}

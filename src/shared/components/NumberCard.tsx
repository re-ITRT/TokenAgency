/**
 * 数字卡（看板 4 张卡：今日请求 / 今日 Token / 今日花费 / 成功率）。
 *
 * 按 [15] 的排版规则：
 * - 主数字 `text.hero`（28px / 500，**一页最多一个 hero** —— 卡片行里最多一张卡用它）；
 * - 大号**等宽数字**（`tabular-nums`）+ 小号单位（`text.meta` / `text.secondary`），千分位；
 * - Token 用 `12.4M` 缩写，**悬停显示精确值**；
 * - 涨跌用数字符号（`+3.2%` / `-1.8%`），颜色只作辅助；
 * - **主数字不着语义色**： 明令不许用 success 当"好的数字"的颜色去美化 KPI。
 */

import { Box, Paper, Skeleton, Tooltip, Typography } from "@mui/material";
import type { ReactNode } from "react";

import { formatExact, formatInt, formatMoney, formatPercent, formatTokens } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import { Sparkline } from "./Sparkline";

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
  loading?: boolean;
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
  const { label, unit, sparkline, delta, icon, loading } = props;
  const { t } = useTranslation();

  if (loading === true) {
    return (
      <Paper variant="outlined" sx={{ p: 4, minWidth: 0, borderRadius: `${radius.surface}px` }}>
        <Skeleton width={72} height={14} />
        <Skeleton width={120} height={34} />
      </Paper>
    );
  }

  const shown = renderValue(props);
  const exact = exactText(props);

  const body = (
    <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.5, mt: 0.5 }}>
      <Typography
        component="span"
        sx={{
          fontSize: typography.size.hero,
          fontWeight: typography.weight.medium,
          lineHeight: typography.lineHeight.heading,
          letterSpacing: typography.letterSpacing.large,
          fontVariantNumeric: "tabular-nums",
          color: "text.primary",
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
          sx={{
            ml: 0.5,
            fontVariantNumeric: "tabular-nums",
            // 颜色只作辅助；方向由 +/- 号表达
            color:
              (delta.value >= 0) === (delta.goodWhenPositive ?? true)
                ? "success.main"
                : "error.main",
          }}
        >
          {delta.value >= 0 ? "+" : "-"}
          {formatPercent(Math.abs(delta.value), 1)}
        </Typography>
      ) : null}
    </Box>
  );

  return (
    <Paper
      variant="outlined"
      sx={{ p: 4, minWidth: 0, height: "100%", borderRadius: `${radius.surface}px` }}
    >
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, color: "text.secondary" }}>
        {icon !== undefined ? (
          <Box component="span" sx={{ display: "inline-flex", "& svg": { fontSize: 14 } }}>
            {icon}
          </Box>
        ) : null}
        <Typography variant="caption" noWrap>
          {label}
        </Typography>
      </Box>

      {exact === undefined ? (
        body
      ) : (
        <Tooltip title={t("component.numberCard.hintExact", { value: exact })}>{body}</Tooltip>
      )}

      {sparkline !== undefined && sparkline.length > 0 ? (
        // 趋势线是数据不是装饰：不给语义色，跟随次要文字色
        <Box sx={{ mt: 1, color: "text.secondary" }}>
          <Sparkline data={sparkline} label={t("component.sparkline.label")} />
        </Box>
      ) : null}
    </Paper>
  );
}

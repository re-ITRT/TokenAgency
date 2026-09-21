/**
 * 额度条。实现 [07 §1.3] 的两条硬规则 + [05 §2.1] 的一条产品规则：
 *
 * - **颜色分级**：<70% 绿 · 70–90% 黄 · >90% 红并轻微脉动（只有红才动效）；
 * - **未设上限必须显式**：`usedPct === null` → 灰色「未设上限」+ ⚠️。
 *   不能静默 —— 不填额度就无法"用完自动切"（[03 §0] 的前提），这是最容易让人困惑的地方。
 */

import { Box, Tooltip, Typography } from "@mui/material";
import type { Theme } from "@mui/material/styles";

import type { QuotaStateValue } from "@/shared/bindings";
import { useTranslation } from "@/shared/i18n";

/** 额度条的颜色档位（与后端 `QuotaStateValue` 不是一回事：这里是显示档位）。 */
export type QuotaLevel = "ok" | "soft" | "hard" | "unknown";

export function quotaLevel(usedPct: number | null): QuotaLevel {
  if (usedPct === null || !Number.isFinite(usedPct)) return "unknown";
  if (usedPct > 0.9) return "hard";
  if (usedPct >= 0.7) return "soft";
  return "ok";
}

function levelColor(theme: Theme, level: QuotaLevel): string {
  switch (level) {
    case "ok":
      return theme.palette.success.main;
    case "soft":
      return theme.palette.warning.main;
    case "hard":
      return theme.palette.error.main;
    case "unknown":
      return theme.palette.text.disabled;
  }
}

export type QuotaBarProps = {
  /** 已用比率 0~1。**`null` = 未设上限**（会渲染成灰色 + 提示）。 */
  usedPct: number | null;
  /** 后端额度状态（`ok|soft|exhausted|unknown`），用于文案与 tooltip。 */
  state?: QuotaStateValue;
  /** 左侧标题（如套餐名）。 */
  label?: string;
  /** 右侧明细（如 `418 / 500万 · 额度/月`）。 */
  detail?: string;
  width?: number;
  /** 紧凑模式：不给 label/百分号，只给一条进度（列表里用）。 */
  compact?: boolean;
};

export function QuotaBar({
  usedPct,
  state,
  label,
  detail,
  width = 160,
  compact = false,
}: QuotaBarProps) {
  const { t } = useTranslation();
  const level = quotaLevel(usedPct);
  const pctText = usedPct === null ? t("component.quotaBar.unlimited") : `${Math.round(usedPct * 100)}%`;
  const stateText = t(`component.quotaBar.state.${state ?? (usedPct === null ? "unknown" : "ok")}`);

  const tooltip = usedPct === null ? t("component.quotaBar.unlimitedHint") : `${stateText} · ${pctText}`;
  const fillPct = usedPct === null ? 0 : Math.min(100, Math.max(0, usedPct * 100));

  return (
    <Tooltip title={tooltip}>
      <Box sx={{ minWidth: 0, width: compact ? width : undefined }}>
        {label !== undefined || !compact ? (
          <Box
            sx={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: 1 }}
          >
            {label !== undefined ? (
              <Typography variant="caption" noWrap sx={{ color: "text.secondary" }}>
                {label}
              </Typography>
            ) : (
              <span />
            )}
            <Typography
              variant="caption"
              sx={{ fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap" }}
            >
              {pctText}
              {usedPct === null ? " ⚠️" : ""}
            </Typography>
          </Box>
        ) : null}

        <Box
          sx={(theme) => ({
            mt: compact ? 0 : 0.5,
            height: 6,
            width,
            borderRadius: 3,
            bgcolor: theme.palette.action.hover,
            overflow: "hidden",
          })}
        >
          <Box
            sx={(theme) => ({
              height: "100%",
              width: `${fillPct}%`,
              bgcolor: levelColor(theme, level),
              transition: "width 300ms ease", // 数值缓动，避免整版跳变
              ...(level === "hard"
                ? {
                    animation: "ta-quota-pulse 1.8s ease-in-out infinite",
                    "@keyframes ta-quota-pulse": {
                      "0%, 100%": { opacity: 1 },
                      "50%": { opacity: 0.55 },
                    },
                  }
                : {}),
            })}
          />
        </Box>

        {detail !== undefined ? (
          <Typography
            variant="caption"
            sx={{ color: "text.secondary", fontVariantNumeric: "tabular-nums" }}
          >
            {detail}
          </Typography>
        ) : null}
      </Box>
    </Tooltip>
  );
}

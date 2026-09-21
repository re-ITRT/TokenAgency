/**
 * 额度条。实现 [07 §1.3] 的分级规则 + [05 §2.1] 的产品规则 + [15] 的视觉规则：
 *
 * - **颜色分级**：<70% 成功 · 70–90% 警告 · >90% 失败并轻微脉动（[15 §7] 唯一允许的循环动效）；
 * - **未设上限必须显式**：`usedPct === null` → 「未设上限」+ 警告色 6px 圆点。
 *   [15 §2.1] 明令**不许用 emoji 做状态表示**（这里原来用的是警告 emoji），改用文字 + 圆点。
 *   不能静默 —— 不填额度就无法"用完自动切"（[03 §0] 的前提），这是最容易让人困惑的地方。
 */

import { Box, Tooltip, Typography } from "@mui/material";

import { formatPercent } from "@/shared/format";
import type { QuotaStateValue } from "@/shared/bindings";
import { useTranslation } from "@/shared/i18n";
import { motion, radius, typography } from "@/theme";

import { StatusDot } from "./StatusDot";

/** 额度条的颜色档位（与后端 `QuotaStateValue` 不是一回事：这里是显示档位）。 */
export type QuotaLevel = "ok" | "soft" | "hard" | "unknown";

export function quotaLevel(usedPct: number | null): QuotaLevel {
  if (usedPct === null || !Number.isFinite(usedPct)) return "unknown";
  if (usedPct > 0.9) return "hard";
  if (usedPct >= 0.7) return "soft";
  return "ok";
}

/** 档位 → 颜色（[15 §3.2]：绿=成功 · 黄=需注意 · 红=失败 · 灰=未知）。 */
const LEVEL_COLOR: Record<QuotaLevel, string> = {
  ok: "success.main",
  soft: "warning.main",
  hard: "error.main",
  unknown: "text.disabled",
};

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
  const unlimited = usedPct === null;
  const pctText = formatPercent(usedPct ?? 0);
  const stateText = t(`component.quotaBar.state.${state ?? (unlimited ? "unknown" : "ok")}`);

  const tooltip = unlimited ? t("component.quotaBar.unlimitedHint") : `${stateText} · ${pctText}`;
  const fillPct = usedPct === null ? 0 : Math.min(100, Math.max(0, usedPct * 100));

  return (
    <Tooltip title={tooltip}>
      <Box sx={{ minWidth: 0, width: compact ? width : undefined }}>
        {label !== undefined || !compact ? (
          <Box
            sx={{
              display: "flex",
              alignItems: "baseline",
              justifyContent: "space-between",
              gap: 2,
            }}
          >
            {label !== undefined ? (
              <Typography variant="caption" noWrap sx={{ color: "text.secondary" }}>
                {label}
              </Typography>
            ) : (
              <span />
            )}
            {unlimited ? (
              // [15 §2.1] 警告的替代手段：文字 + 6px 警告色圆点（不是 emoji，也不是色块）
              <StatusDot status="warn" label={t("component.quotaBar.unlimited")} size={6} />
            ) : (
              <Typography
                variant="caption"
                sx={{
                  fontVariantNumeric: "tabular-nums",
                  whiteSpace: "nowrap",
                  color: LEVEL_COLOR[level],
                }}
              >
                {pctText}
              </Typography>
            )}
          </Box>
        ) : null}

        <Box
          sx={{
            mt: compact ? 0 : 1,
            height: 6,
            width,
            borderRadius: `${radius.control}px`,
            bgcolor: "action.hover",
            overflow: "hidden",
          }}
        >
          <Box
            sx={{
              height: "100%",
              width: `${fillPct}%`,
              bgcolor: LEVEL_COLOR[level],
              // 数值缓动，避免整版跳变（[15 §7]：≤250ms，快出缓入）
              transition: `width ${motion.enter} ${motion.easing}`,
              ...(level === "hard"
                ? {
                    animation: "ta-quota-pulse 1.8s ease-in-out infinite",
                    "@keyframes ta-quota-pulse": {
                      "0%, 100%": { opacity: 1 },
                      "50%": { opacity: 0.55 },
                    },
                  }
                : {}),
            }}
          />
        </Box>

        {detail !== undefined ? (
          <Typography
            variant="caption"
            sx={{
              color: "text.secondary",
              fontFamily: typography.fontMono,
              fontVariantNumeric: "tabular-nums",
            }}
          >
            {detail}
          </Typography>
        ) : null}
      </Box>
    </Tooltip>
  );
}

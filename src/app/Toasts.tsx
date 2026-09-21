/**
 * Toast 容器：读全局 store 里的队列（由 `useEventBridge` 从 `toast` 事件灌入）。
 * 去重规则在同 10 分钟内同 `dedupeKey` 只弹一次（[05 §3]），在 store 里实现。
 *
 * 视觉（[15 §2.1]）：失败 = 文字 + `danger` 色**圆点**与文字，**不要红底白字的大色块**
 * —— 所以这里不用 `Alert variant="filled"`，改用浮层表面（`bg.raised` + 唯一的浮层阴影，§3.5）。
 */

import { Box, IconButton, Paper, Slide, Tooltip } from "@mui/material";
import CloseIcon from "@mui/icons-material/CloseOutlined";

import { useTranslation } from "@/shared/i18n";
import { useAppStore } from "@/stores";
import { motion, paletteOf, radius, typography } from "@/theme";

import { StatusDot, type SemStatus } from "@/shared/components";

const LEVEL_STATUS: Record<"info" | "warn" | "error", SemStatus> = {
  info: "info",
  warn: "warn",
  error: "error",
};

export function Toasts() {
  const { t } = useTranslation();
  const toasts = useAppStore((s) => s.toasts);
  const dismissToast = useAppStore((s) => s.dismissToast);

  if (toasts.length === 0) return null;

  return (
    <Box
      sx={{
        position: "fixed",
        right: 16,
        bottom: 16,
        display: "flex",
        flexDirection: "column",
        gap: 2,
        maxWidth: 360,
        zIndex: (theme) => theme.zIndex.snackbar,
      }}
    >
      {toasts.map((toast) => (
        <Slide in direction="left" key={toast.id} timeout={Number.parseInt(motion.enter, 10)}>
          <Paper
            variant="outlined"
            sx={(theme) => {
              const p = paletteOf(theme.palette.mode);
              return {
                p: 4,
                display: "flex",
                alignItems: "flex-start",
                gap: 2,
                bgcolor: p.bg.raised,
                borderColor: p.border.subtle,
                borderRadius: `${radius.overlay}px`,
                boxShadow: p.shadowOverlay,
              };
            }}
          >
            <Box sx={{ mt: 0.5, display: "inline-flex" }}>
              <StatusDot status={LEVEL_STATUS[toast.level]} />
            </Box>
            <Box sx={{ minWidth: 0, flex: 1 }}>
              <Box
                sx={{
                  fontSize: typography.size.body,
                  fontWeight: typography.weight.medium,
                  lineHeight: typography.lineHeight.heading,
                  color: "text.primary",
                }}
              >
                {toast.title}
              </Box>
              {toast.body !== "" ? (
                <Box
                  sx={{
                    mt: 0.5,
                    fontSize: typography.size.meta,
                    lineHeight: typography.lineHeight.body,
                    color: "text.secondary",
                    wordBreak: "break-word",
                  }}
                >
                  {toast.body}
                </Box>
              ) : null}
            </Box>
            <Tooltip title={t("common.close")}>
              <IconButton
                onClick={() => dismissToast(toast.id)}
                sx={{ mt: -0.5, mr: -0.5, "& svg": { fontSize: 14 } }}
              >
                <CloseIcon />
              </IconButton>
            </Tooltip>
          </Paper>
        </Slide>
      ))}
    </Box>
  );
}

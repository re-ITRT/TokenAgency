/**
 * Toast 容器：读全局 store 里的队列（由 `useEventBridge` 从 `toast` 事件灌入）。
 * 去重规则在同 10 分钟内同 `dedupeKey` 只弹一次（[05 §3]），在 store 里实现。
 */

import { Alert, Box, Slide } from "@mui/material";

import { useAppStore } from "@/stores";

export function Toasts() {
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
        gap: 1,
        maxWidth: 360,
        zIndex: (theme) => theme.zIndex.snackbar,
      }}
    >
      {toasts.map((toast) => (
        <Slide in direction="left" key={toast.id}>
          <Alert
            // MUI 用 `warning`，事件载荷用 `warn`（Rust 侧取值），这里做一次映射
            severity={toast.level === "warn" ? "warning" : toast.level}
            variant="filled"
            onClose={() => dismissToast(toast.id)}
            sx={{ alignItems: "flex-start" }}
          >
            <Box sx={{ fontWeight: 600, fontSize: 13 }}>{toast.title}</Box>
            {toast.body !== "" ? (
              <Box sx={{ fontSize: 12, opacity: 0.9 }}>{toast.body}</Box>
            ) : null}
          </Alert>
        </Slide>
      ))}
    </Box>
  );
}

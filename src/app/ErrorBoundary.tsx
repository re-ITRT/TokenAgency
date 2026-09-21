/**
 * 错误边界：**一个页面崩了不能带走整个应用**（DoD：每个路由都能渲染，不白屏）。
 *
 * 面包屑：React 要求错误边界是 class 组件，所以这里 class 只做捕获，
 * 渲染交给函数组件 `ErrorFallback`（这样才能用 i18n / hooks）。
 *
 * 附带信息（错误码 + 消息）可以一键复制 —— 用户贴给我们就够了。
 */

import { Alert, Box, Button, Paper, Stack, Typography } from "@mui/material";
import { Component, type ErrorInfo, type ReactNode } from "react";

import { errorCode, errorMessage } from "@/shared/api";
import { useTranslation } from "@/shared/i18n";

export type ErrorBoundaryProps = {
  children: ReactNode;
  /** 变化时自动重置错误状态（通常传当前路由 path，切页后自动恢复）。 */
  resetKey?: string;
};

type ErrorBoundaryState = { error: unknown };

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  override state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: unknown): ErrorBoundaryState {
    return { error };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    // 前端没有远端日志：控制台 + 用户可复制是当前阶段的排查手段
    console.error("[ta] 页面渲染失败：", error, info.componentStack);
  }

  override componentDidUpdate(prev: ErrorBoundaryProps): void {
    if (prev.resetKey !== this.props.resetKey && this.state.error !== null) {
      this.setState({ error: null });
    }
  }

  private readonly handleReset = (): void => {
    this.setState({ error: null });
  };

  override render(): ReactNode {
    if (this.state.error !== null) {
      return <ErrorFallback error={this.state.error} onReset={this.handleReset} />;
    }
    return this.props.children;
  }
}

function ErrorFallback({ error, onReset }: { error: unknown; onReset: () => void }) {
  const { t } = useTranslation();
  const code = errorCode(error);
  const detail = `${code ?? "TA-9001"} ${errorMessage(error)}`;

  return (
    <Box sx={{ p: 2, maxWidth: 820 }}>
      <Paper variant="outlined" sx={{ p: 3 }}>
        <Stack spacing={1.5}>
          <Typography variant="h6">{t("error.title")}</Typography>
          <Typography variant="body2" color="text.secondary">
            {t("error.boundary.body")}
          </Typography>

          <Alert severity={code?.startsWith("TA-5") === true ? "warning" : "error"}>
            {code !== undefined ? (
              <Typography variant="caption" sx={{ display: "block", mb: 0.5 }}>
                {t("error.code", { code })}
              </Typography>
            ) : null}
            <Typography variant="body2" sx={{ fontFamily: "monospace", wordBreak: "break-all" }}>
              {errorMessage(error)}
            </Typography>
            {code?.startsWith("TA-5") === true ? (
              <Typography variant="caption">{t("error.fixable")}</Typography>
            ) : null}
          </Alert>

          <Stack direction="row" spacing={1}>
            <Button
              variant="outlined"
              onClick={() => {
                void navigator.clipboard?.writeText(detail);
              }}
            >
              {t("error.copyDetails")}
            </Button>
            <Button variant="contained" onClick={onReset}>
              {t("common.retry")}
            </Button>
            <Button variant="text" onClick={() => window.location.reload()}>
              {t("error.reload")}
            </Button>
          </Stack>
        </Stack>
      </Paper>
    </Box>
  );
}

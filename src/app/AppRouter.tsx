/**
 * 路由渲染：把 `routes.ts` 的表变成 `<Route>`，页面组件按约定路径自动解析。
 *
 * 三条 DoD 保证：
 * - **页面缺失时渲染占位壳**（不白屏、不报错）；
 * - **页面崩了只崩这一块**：页面区外套 `<ErrorBoundary>`，切走再切回来自动恢复；
 * - **懒加载**：页面按需分包，`Suspense` 期间显示加载态（不是白屏）。
 */

import { Box, CircularProgress, Typography } from "@mui/material";
import { Suspense } from "react";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";

import { useTranslation } from "@/shared/i18n";

import { ErrorBoundary } from "./ErrorBoundary";
import { PagePlaceholder } from "./PagePlaceholder";
import { resolvePage } from "./pageRegistry";
import { HOME_PATH, ROUTES } from "./routes";

function PageLoading() {
  const { t } = useTranslation();
  return (
    <Box
      sx={{
        p: 8,
        display: "flex",
        alignItems: "center",
        gap: 3,
        color: "text.secondary",
      }}
    >
      <CircularProgress size={14} />
      <Typography variant="body1">{t("common.loading")}</Typography>
    </Box>
  );
}

function NotFound() {
  const { t } = useTranslation();
  return (
    <Box data-testid="page-not-found">
      <PagePlaceholder
        title={t("error.notFound")}
        owner="F1"
        pages={[]}
        docRef="10 §2.2 命令清单"
      />
    </Box>
  );
}

export function AppRouter() {
  const location = useLocation();
  const { t } = useTranslation();

  return (
    <ErrorBoundary key={location.pathname} resetKey={location.pathname}>
      <Suspense fallback={<PageLoading />}>
        <Routes>
          <Route path="/" element={<Navigate to={HOME_PATH} replace />} />
          {ROUTES.map((route) => {
            const Page = resolvePage(route.pages);
            return (
              <Route
                key={route.key}
                path={route.path}
                element={
                  Page === undefined ? (
                    <PagePlaceholder
                      title={t(route.titleKey)}
                      owner={route.owner}
                      pages={route.pages}
                      docRef={route.docRef}
                    />
                  ) : (
                    <Page />
                  )
                }
              />
            );
          })}
          <Route path="*" element={<NotFound />} />
        </Routes>
      </Suspense>
    </ErrorBoundary>
  );
}

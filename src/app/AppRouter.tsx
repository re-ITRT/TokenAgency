/**
 * 路由渲染：把 `routes.ts` 的表变成 `<Route>`，页面组件按约定路径自动解析。
 *
 * 三条 DoD 保证：
 * - **页面缺失时给一个用户能看懂的 404**（不白屏、不报错、不显示"该放哪个文件"这种开发向信息）；
 * - **页面崩了只崩这一块**：页面区外套 `<ErrorBoundary>`，切走再切回来自动恢复；
 * - **懒加载**：页面按需分包，`Suspense` 期间显示加载态（不是白屏）。
 *
 * [极简] 原来的 `<PagePlaceholder>`（显示"归属 F2/F3/F4 + 该放哪个文件路径 + 契约出处"）
 * 已经**整块删掉**：那些是给开发看的，而用户在页面上只该看到"这个地址没有页面"和一条回去的路。
 */

import { Box, Button, CircularProgress, Typography } from "@mui/material";
import { Suspense } from "react";
import { Navigate, Route, Routes, useLocation, useNavigate } from "react-router-dom";

import { useTranslation } from "@/shared/i18n";

import { ErrorBoundary } from "./ErrorBoundary";
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

/** 用户能看懂的 404：一句状态 + 一条回去的路。**不放**归属、文件路径、文档编号。 */
function NotFound() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  return (
    <Box
      data-testid="page-not-found"
      sx={{ p: 8, display: "flex", flexDirection: "column", alignItems: "flex-start", gap: 4 }}
    >
      <Typography variant="h2" component="h1">
        {t("error.notFound")}
      </Typography>
      <Typography variant="body1" sx={{ color: "text.secondary" }}>
        {t("error.notFound.body")}
      </Typography>
      <Button variant="contained" onClick={() => navigate(HOME_PATH)}>
        {t("error.backHome")}
      </Button>
    </Box>
  );
}

export function AppRouter() {
  const location = useLocation();

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
                  Page === undefined ? <NotFound /> : <Page />
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

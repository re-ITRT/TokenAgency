/**
 * 侧边栏：**完全由 `routes.ts` 生成**（四个包加页面时不用碰这里）。
 *
 * 标签一律来自 i18n（用户语言），不出现 channel / credential / lane 这类内部概念。
 * 折叠状态存在 `useUiStore`（跨页面 + 持久化）。
 */

import {
  Box,
  Divider,
  IconButton,
  List,
  ListItemButton,
  ListItemIcon,
  ListItemText,
  Tooltip,
  Typography,
} from "@mui/material";
import ChevronLeftIcon from "@mui/icons-material/ChevronLeft";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import DarkModeIcon from "@mui/icons-material/DarkModeOutlined";
import LightModeIcon from "@mui/icons-material/LightModeOutlined";
import TranslateIcon from "@mui/icons-material/TranslateOutlined";
import { useLocation, useNavigate } from "react-router-dom";

import { currentLocale, otherLocale, setLocale, useTranslation } from "@/shared/i18n";
import { useUiStore } from "@/stores";

import { matchRoute, NAV_ROUTES } from "./routes";

export const SIDEBAR_WIDTH = 216;
export const SIDEBAR_COLLAPSED_WIDTH = 56;

export function Sidebar() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const collapsed = useUiStore((s) => s.sidebarCollapsed);
  const toggleSidebar = useUiStore((s) => s.toggleSidebar);
  const themeMode = useUiStore((s) => s.themeMode);
  const toggleTheme = useUiStore((s) => s.toggleTheme);

  const width = collapsed ? SIDEBAR_COLLAPSED_WIDTH : SIDEBAR_WIDTH;
  const activeKey = matchRoute(location.pathname)?.key;

  return (
    <Box
      component="nav"
      sx={{
        width,
        flex: `0 0 ${width}px`,
        borderRight: 1,
        borderColor: "divider",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        transition: "width 150ms ease",
      }}
    >
      <Box sx={{ px: 1.5, py: 1.25, display: "flex", alignItems: "center", gap: 1, minHeight: 44 }}>
        <Box
          sx={{
            width: 22,
            height: 22,
            borderRadius: 1,
            bgcolor: "primary.main",
            color: "primary.contrastText",
            display: "grid",
            placeItems: "center",
            fontSize: 12,
            fontWeight: 700,
            flex: "0 0 auto",
          }}
        >
          TA
        </Box>
        {!collapsed ? (
          <Typography variant="subtitle2" noWrap>
            {t("app.name")}
          </Typography>
        ) : null}
      </Box>

      <Divider />

      <List dense sx={{ flex: 1, minHeight: 0, overflowY: "auto", py: 0.5 }}>
        {NAV_ROUTES.map((route) => {
          const Icon = route.icon;
          const active = route.key === activeKey;
          const label = t(route.titleKey);
          return (
            <ListItemButton
              key={route.key}
              selected={active}
              onClick={() => navigate(route.path)}
              sx={{
                mx: 0.5,
                borderRadius: 1,
                minHeight: 34,
                justifyContent: collapsed ? "center" : "flex-start",
                px: collapsed ? 1 : 1.25,
              }}
            >
              <Tooltip title={collapsed ? label : ""} placement="right">
                <ListItemIcon
                  sx={{ minWidth: collapsed ? 0 : 28, justifyContent: "center", color: "inherit" }}
                >
                  <Icon fontSize="small" />
                </ListItemIcon>
              </Tooltip>
              {!collapsed ? (
                <ListItemText
                  primary={label}
                  slotProps={{ primary: { fontSize: 13, noWrap: true } }}
                />
              ) : null}
            </ListItemButton>
          );
        })}
      </List>

      <Divider />

      <Box
        sx={{
          px: 0.5,
          py: 0.5,
          display: "flex",
          alignItems: "center",
          justifyContent: collapsed ? "center" : "space-between",
          gap: 0.5,
        }}
      >
        {!collapsed ? (
          <>
            <Tooltip title={t("app.theme.toggle")}>
              <IconButton size="small" onClick={toggleTheme}>
                {themeMode === "dark" ? (
                  <LightModeIcon fontSize="small" />
                ) : (
                  <DarkModeIcon fontSize="small" />
                )}
              </IconButton>
            </Tooltip>
            <Tooltip title={t("app.locale.toggle")}>
              <IconButton
                size="small"
                onClick={() => setLocale(otherLocale())}
                aria-label={`locale:${currentLocale()}`}
              >
                <TranslateIcon fontSize="small" />
              </IconButton>
            </Tooltip>
          </>
        ) : null}
        <Tooltip title={collapsed ? t("app.sidebar.expand") : t("app.sidebar.collapse")}>
          <IconButton size="small" onClick={toggleSidebar}>
            {collapsed ? (
              <ChevronRightIcon fontSize="small" />
            ) : (
              <ChevronLeftIcon fontSize="small" />
            )}
          </IconButton>
        </Tooltip>
      </Box>
    </Box>
  );
}

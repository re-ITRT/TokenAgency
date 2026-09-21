/**
 * 侧边栏：**完全由 `routes.ts` 生成**（四个包加页面时不用碰这里）。
 *
 * 标签一律来自 i18n（用户语言），不出现 channel / credential / lane 这类内部概念。
 * 折叠状态存在 `useUiStore`（跨页面 + 持久化）。
 *
 * 视觉（[15]）：品牌区只有文字层级、没有色块（§1 克制）；图标一律 outlined 18px（§6）；
 * 选中态 = `accent.subtle` 底 + 左侧 2px 强调色条（§5，由主题的 `MuiListItemButton` 提供）。
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
import { motion, typography } from "@/theme";

import { matchRoute, NAV_ROUTES } from "./routes";

export const SIDEBAR_WIDTH = 216;
export const SIDEBAR_COLLAPSED_WIDTH = 56;

/** 品牌区与状态条同高，视觉上一条线。 */
export const CHROME_HEIGHT = 48;

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
        borderRight: `1px solid`,
        borderColor: "divider",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        transition: `width ${motion.quick} ${motion.easing}`,
      }}
    >
      <Box
        sx={{
          px: 3,
          display: "flex",
          flexDirection: "column",
          justifyContent: "center",
          minHeight: CHROME_HEIGHT,
          gap: 0.5,
        }}
      >
        <Typography
          noWrap
          sx={{
            fontSize: typography.size.section,
            fontWeight: typography.weight.medium,
            lineHeight: typography.lineHeight.heading,
            letterSpacing: typography.letterSpacing.large,
            color: "text.primary",
          }}
        >
          {collapsed ? "TA" : t("app.name")}
        </Typography>
        {!collapsed ? (
          <Typography
            noWrap
            sx={{
              fontSize: typography.size.label,
              color: "text.secondary",
              lineHeight: typography.lineHeight.table,
            }}
          >
            {t("app.tagline")}
          </Typography>
        ) : null}
      </Box>

      <Divider />

      <List dense sx={{ flex: 1, minHeight: 0, overflowY: "auto", py: 1 }}>
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
                mx: 1,
                justifyContent: collapsed ? "center" : "flex-start",
                px: 1,
              }}
            >
              <Tooltip title={collapsed ? label : ""} placement="right">
                {/* [15 §6] 侧边栏图标 18px；与文字间距 10px（用 minWidth 表达，不是随手 margin） */}
                <ListItemIcon
                  sx={{
                    minWidth: collapsed ? 0 : 28,
                    justifyContent: "center",
                    color: "inherit",
                    "& svg": { fontSize: 18 },
                  }}
                >
                  <Icon />
                </ListItemIcon>
              </Tooltip>
              {!collapsed ? (
                <ListItemText
                  primary={label}
                  slotProps={{
                    primary: { noWrap: true, sx: { fontSize: typography.size.body } },
                  }}
                />
              ) : null}
            </ListItemButton>
          );
        })}
      </List>

      <Divider />

      <Box
        sx={{
          px: 1,
          py: 1,
          display: "flex",
          alignItems: "center",
          justifyContent: collapsed ? "center" : "space-between",
          gap: 1,
          "& svg": { fontSize: 16 },
        }}
      >
        {!collapsed ? (
          <>
            <Tooltip title={t("app.theme.toggle")}>
              <IconButton onClick={toggleTheme}>
                {themeMode === "dark" ? <LightModeIcon /> : <DarkModeIcon />}
              </IconButton>
            </Tooltip>
            <Tooltip title={t("app.locale.toggle")}>
              <IconButton
                onClick={() => setLocale(otherLocale())}
                aria-label={`locale:${currentLocale()}`}
              >
                <TranslateIcon />
              </IconButton>
            </Tooltip>
          </>
        ) : null}
        <Tooltip title={collapsed ? t("app.sidebar.expand") : t("app.sidebar.collapse")}>
          <IconButton onClick={toggleSidebar}>
            {collapsed ? <ChevronRightIcon /> : <ChevronLeftIcon />}
          </IconButton>
        </Tooltip>
      </Box>
    </Box>
  );
}

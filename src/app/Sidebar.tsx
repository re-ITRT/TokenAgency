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
import { Fragment } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import { currentLocale, otherLocale, setLocale, useTranslation } from "@/shared/i18n";
import { useUiStore } from "@/stores/ui";
import { resolveThemeMode, systemPrefersDark } from "@/stores/ui";
import { typography } from "@/theme";

import { matchRoute, navSections, NAV_ROUTES } from "./routes";

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

  /**
   * [docs/56] 侧边栏按**分组**画。
   *
   * `navSections()` 在"同组的项不连续 / 组名没登记顺序"时返回 `null` —— 那是**契约错误**，
   * `routes.test.tsx` 会当场红。这里兜底成"不分组的一份清单"：宁可少画几条线，
   * 也不能因为分组算不出来就**整条侧边栏空白**（那是把内部错误变成用户看不见导航）。
   */
  const sections = navSections() ?? [{ routes: NAV_ROUTES }];

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
        // [docs/26 §1.4] **不给宽度加过渡**：宽度是布局属性，折叠时整壳每一帧都要重排，
        // 而允许动画的只有 opacity / transform / stroke-dashoffset / 进度条 width。
        // 折叠本来就是一次瞬时状态切换，加过渡只换来掉帧。
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
        {sections.map((section, index) => {
          // [docs/56] 组名只在**展开**时画：折叠态里 40px 宽的栏放不下一个字，
          // 硬塞会把图标挤歪（那比"少一个组名"糟得多）。折叠态靠 `Tooltip` 不受影响。
          const heading =
            section.groupKey !== undefined && !collapsed ? t(section.groupKey) : undefined;
          return (
            <Fragment key={section.groupKey ?? "__ungrouped__"}>
              {/*
                [docs/56] 分组之间加一条**细线**，不是大间距 ——
                [15 §3.3] 的间距刻度里，`mt` 最多只能给 24px，而"同类相邻、异类分开"靠
                一条 1px 线比靠留白更明确（同屏也给不出更大的间距）。
                第一条前面不加线（与上面的品牌区分隔线重复会显得像两段）。
              */}
              {index > 0 ? <Divider sx={{ my: 1, mx: 2 }} /> : null}
              {heading === undefined ? null : (
                <Typography
                  noWrap
                  data-testid="sidebar-group-heading"
                  sx={{
                    px: 2,
                    pt: index === 0 ? 0 : 1,
                    pb: 0.5,
                    fontSize: typography.size.label,
                    fontWeight: typography.weight.medium,
                    letterSpacing: typography.letterSpacing.label,
                    color: "text.secondary",
                    textTransform: "uppercase",
                  }}
                >
                  {heading}
                </Typography>
              )}
              {section.routes.map((route) => {
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
            </Fragment>
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
                {resolveThemeMode(themeMode, systemPrefersDark()) === "dark" ? <LightModeIcon /> : <DarkModeIcon />}
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
          <IconButton
            data-testid="sidebar-collapse-toggle"
            aria-label={collapsed ? t("app.sidebar.expand") : t("app.sidebar.collapse")}
            onClick={toggleSidebar}
          >
            {collapsed ? <ChevronRightIcon /> : <ChevronLeftIcon />}
          </IconButton>
        </Tooltip>
      </Box>
    </Box>
  );
}

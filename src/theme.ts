import { createTheme, type Theme } from "@mui/material/styles";

/**
 * 主题：深色优先（开发者多数晚上用），浅色同构。
 * 桌面密度优先：4px 间距基准 + 小尺寸控件，表格行高锁 32px。
 */
export function buildTheme(mode: "dark" | "light"): Theme {
  return createTheme({
    // 开启 CSS 主题变量：减少 Emotion 运行时样式重算（实时请求流高频更新时的关键）
    cssVariables: true,
    palette: {
      mode,
      primary: { main: "#4c8dff" },
      success: { main: "#3fb950" },
      warning: { main: "#d29922" },
      error: { main: "#f85149" },
      background:
        mode === "dark"
          ? { default: "#0d1117", paper: "#161b22" }
          : { default: "#f6f8fa", paper: "#ffffff" },
    },
    spacing: 4,
    shape: { borderRadius: 6 },
    typography: {
      fontFamily:
        '-apple-system, "Segoe UI", "Microsoft YaHei", Roboto, "Helvetica Neue", sans-serif',
      fontSize: 13,
      // 数字用等宽 + 表格数字对齐
      button: { textTransform: "none" },
    },
    components: {
      MuiButton: { defaultProps: { size: "small", disableElevation: true } },
      MuiTextField: { defaultProps: { size: "small" } },
      MuiSelect: { defaultProps: { size: "small" } },
      MuiChip: { defaultProps: { size: "small" } },
      MuiTableCell: { styleOverrides: { root: { padding: "2px 8px", height: 32 } } },
      MuiTooltip: { defaultProps: { arrow: true } },
    },
  });
}

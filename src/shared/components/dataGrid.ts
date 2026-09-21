/**
 * MUI X DataGrid 的令牌预设 —— **本项目 70% 的界面是表格**（[15 §5]），
 * 所以这一份必须由 F1 统一出，而不是 F2/F3/F4 各写一遍 `sx`。
 *
 * [15 §5] 的硬性约定，逐条落在下面：
 * - 行高 **32px**；单元格内边距纵向 **2px**（`2` 只用于表格内的紧凑对齐）、横向 **8px**；
 * - **表头**：`text.label`(11px/500) + `text.secondary`，**下边框一条 `border.subtle`**、**无背景色**；
 * - **行分隔**只用 `border.subtle` 的一条 1px 下边线；**不用斑马纹**；**不用竖线**；
 * - **选中**：`accent.subtle` 背景 + 左侧 2px `accent` 色条（不整行填彩色）；
 * - **悬停**：`bg.subtle`，不做位移、不加阴影；
 * - **数字列右对齐**（列上写 `type: "number"` 或 `align: "right"`）并**等宽**（`tabular-nums`）；
 * - 状态列用 6px 圆点 + 文字（见 `<StatusDot>`），**不用彩色 chip**。
 *
 * 怎么用（F2–F4 照抄这一行就够）：
 * ```tsx
 * const preset = useDataGridPreset();
 * <DataGrid rows={rows} columns={columns} {...preset} />
 * ```
 * 需要额外 `sx` 时用 `sx={[dataGridSx, { minHeight: 0 }]}` 合并，**不要**再手写行高/表头样式。
 */

import type { SxProps, Theme } from "@mui/material/styles";
import type { DataGridProps } from "@mui/x-data-grid";
import { enUS, zhCN } from "@mui/x-data-grid/locales";

import { currentLocale, type Locale, useTranslation } from "@/shared/i18n";
import { paletteOf, radius, typography } from "@/theme";

/**
 * 表格的视觉令牌。给 MUI X **自己的 CSS 变量**赋值优先（官方调节点，比覆盖生成类名稳），
 * 它没暴露成变量的细节再用类选择器补 —— 两条都从 `tokens` / `theme.palette` 取。
 */
export const dataGridSx: SxProps<Theme> = (theme) => {
  const p = paletteOf(theme.palette.mode);

  return {
    // ── 官方 CSS 变量（名字必须与 MUI X 一致，否则是死代码。已按包内实现核对：
    //      `--unstable_DataGrid-headWeight` 由 GridRootStyles.js:96 定义、GridColumnHeaderTitle.js:39 消费）
    // 表格容器圆角用 `radius.surface`（§3.4：卡片、面板、**表格容器**）
    "--unstable_DataGrid-radius": `${radius.surface}px`,
    "--DataGrid-rowBorderColor": p.border.subtle,
    "--unstable_DataGrid-headWeight": typography.weight.medium,

    // ── 容器本身：靠背景台阶分层，不加阴影（§3.5）
    borderColor: p.border.subtle,
    backgroundColor: p.bg.surface,
    color: p.text.primary,
    fontSize: typography.size.body,

    // ── 单元格内边距（§3.3：2 只用于表格单元格内的紧凑对齐）与焦点框
    "& .MuiDataGrid-columnHeader, & .MuiDataGrid-cell": {
      padding: "2px 8px",
      // 焦点框保持可见但收细：a11y 不能拿"没有焦点框"换好看
      "&:focus, &:focus-within": { outline: `1px solid ${p.accent}`, outlineOffset: -1 },
    },

    // ── 表头：无背景色、一条下边线、小标签字
    "& .MuiDataGrid-columnHeaders": {
      backgroundColor: "transparent",
      color: p.text.secondary,
      borderBottom: `1px solid ${p.border.subtle}`,
    },
    "& .MuiDataGrid-columnHeaderTitle": {
      fontSize: typography.size.label,
      letterSpacing: typography.letterSpacing.label,
      // 字重**只**由官方变量 `--unstable_DataGrid-headWeight` 给：
      // 同一属性两处写 = 两个真相源，漂了也不会红（这正是评审抓到的那个错）
    },
    // ── 不用竖线
    "& .MuiDataGrid-columnSeparator": { display: "none" },

    // ── 单元格：行分隔只靠 `border.subtle` 的下边线；数字列等宽
    "& .MuiDataGrid-cell": {
      lineHeight: typography.lineHeight.table,
      borderBottom: `1px solid ${p.border.subtle}`,
    },
    "& .MuiDataGrid-cell--textRight": { fontVariantNumeric: "tabular-nums" },

    // ── 行：不用斑马纹；悬停 bg.subtle；选中 accent.subtle + 左 2px 强调色条
    "& .MuiDataGrid-row": { backgroundColor: "transparent" },
    "& .MuiDataGrid-row:hover": { backgroundColor: p.bg.subtle },
    "& .MuiDataGrid-row.Mui-selected": {
      backgroundColor: p.accentSubtle,
      boxShadow: `inset 2px 0 0 0 ${p.accent}`,
      "&:hover": { backgroundColor: p.accentSubtle },
    },

    // ── 底栏与空状态（空状态给诚实的"暂无数据"，不给假行）
    "& .MuiDataGrid-footerContainer": { borderTop: `1px solid ${p.border.subtle}` },
    "& .MuiDataGrid-overlay": { color: p.text.secondary },
  };
};

/** 几何与预设默认值。`sx` 一起给上，页面 `{...preset}` 就是 [15] 的表格。 */
export const dataGridDefaults = {
  rowHeight: 32,
  columnHeaderHeight: 32,
  density: "compact",
  sx: dataGridSx,
} satisfies Partial<DataGridProps>;

/**
 * DataGrid 自带的分页/筛选/空状态文案**不走我们的 i18n**（它是 MUI X 的 localeText），
 * 不接就会在中文界面里冒出 "Rows per page" —— 所以这里做一次桥接。
 */
export function dataGridLocaleText(locale: Locale = currentLocale()) {
  return (locale === "en" ? enUS : zhCN).components.MuiDataGrid.defaultProps.localeText;
}

/** 页面用法：`<DataGrid rows columns {...useDataGridPreset()} />`（语言切换会自动跟随）。 */
export function useDataGridPreset(): Partial<DataGridProps> {
  // 只为"语言变化时重渲染"而订阅；语言判定只走 `currentLocale()` 一处（不在这里抄第二份映射）
  useTranslation();
  return { ...dataGridDefaults, localeText: dataGridLocaleText(currentLocale()) };
}

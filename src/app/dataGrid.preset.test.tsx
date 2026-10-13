// @vitest-environment jsdom
/**
 * DataGrid 令牌预设的验收 —— 表格是本项目 70% 的界面，
 * 这一份守卫保证：**预设里的值就是文档里的值**，而且**在装的 MUI X v8 上真的能用**。
 *
 * 两类断言：
 * 1. 预设对象逐条等于（行高 32、表头 11/500 + 下边线 + 无底色、无竖线、
 *    选中 accent.subtle + 左 2px 强调色条、悬停 bg.subtle、数字列等宽）；
 * 2. 真渲染一个 DataGrid（jsdom 里给 ResizeObserver 打个最小桩）——
 *    证明这套 `sx`/props 与当前安装的 MUI X v8 的 API 对得上（文档会漂，包不会）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { DataGrid } from "@mui/x-data-grid";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { dataGridDefaults, dataGridLocaleText, dataGridSx } from "@/shared/components";
import { buildTheme, radius, tokens, typography } from "@/theme";

afterEach(() => {
  cleanup();
});

/** MUI X 内部用 ResizeObserver；jsdom 没有，给个最小桩（只为让组件能挂载）。 */
class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver ??= ResizeObserverStub;

type SxBag = Record<string, Record<string, unknown>>;

/** `sx` 允许是函数（拿 theme）；取深色那一份来断言。 */
function darkSx(): SxBag {
  const sx = dataGridSx as unknown as (theme: ReturnType<typeof buildTheme>) => SxBag;
  return sx(buildTheme("dark"));
}

describe("DataGrid 预设：值等于文档", () => {
  it("几何：行高 32、表头 32、compact", () => {
    expect(dataGridDefaults.rowHeight).toBe(32);
    expect(dataGridDefaults.columnHeaderHeight).toBe(32);
    expect(dataGridDefaults.density).toBe("compact");
  });

  it("表格容器圆角 = radius.surface，行/单元格边线 = border.subtle（官方 CSS 变量）", () => {
    const sx = darkSx();
    expect(sx["--unstable_DataGrid-radius"]).toBe(`${radius.surface}px`);
    expect(sx["--DataGrid-rowBorderColor"]).toBe(tokens.palette.dark.border.subtle);
  });

  it("表头字重走官方变量，且**同一属性只有一个来源**（评审抓到的错名不许回来）", () => {
    const sx = darkSx();

    // 变量名以包内实现为准，不以我的实现为准：
    //   node_modules/@mui/x-data-grid/components/containers/GridRootStyles.js:96 定义
    //   node_modules/@mui/x-data-grid/components/columnHeaders/GridColumnHeaderTitle.js:39 消费
    expect(sx["--unstable_DataGrid-headWeight"]).toBe(typography.weight.medium);
    // 曾经写错的死名字（`--DataGrid-headWeight`）：包内 grep 无此名，出现即无效代码
    expect(sx["--DataGrid-headWeight"]).toBeUndefined();

    // "别两边都写"：类选择器只管字号与字距，字重只能来自那个变量
    const title = sx["& .MuiDataGrid-columnHeaderTitle"] ?? {};
    expect(title["fontSize"]).toBe(typography.size.label);
    expect(title["letterSpacing"]).toBe(typography.letterSpacing.label);
    expect(title["fontWeight"]).toBeUndefined();
  });

  it("表头：text.label + text.secondary、无底色、一条下边线；没有竖线", () => {
    const sx = darkSx();
    expect(sx["& .MuiDataGrid-columnHeaders"]).toMatchObject({
      backgroundColor: "transparent",
      color: tokens.palette.dark.text.secondary,
      borderBottom: `1px solid ${tokens.palette.dark.border.subtle}`,
    });
    expect(sx["& .MuiDataGrid-columnHeaderTitle"]).toMatchObject({
      fontSize: typography.size.label,
      letterSpacing: typography.letterSpacing.label,
      // 字重不在这里（见上一条：只由官方变量给）
    });
    expect(sx["& .MuiDataGrid-columnSeparator"]).toMatchObject({ display: "none" });
  });

  it("单元格：内边距 2px 8px、下边线、数字列等宽", () => {
    const sx = darkSx();
    expect(sx["& .MuiDataGrid-columnHeader, & .MuiDataGrid-cell"]).toMatchObject({
      padding: "2px 8px",
    });
    expect(sx["& .MuiDataGrid-cell"]).toMatchObject({
      borderBottom: `1px solid ${tokens.palette.dark.border.subtle}`,
      lineHeight: typography.lineHeight.table,
    });
    expect(sx["& .MuiDataGrid-cell--textRight"]).toMatchObject({
      fontVariantNumeric: "tabular-nums",
    });
  });

  it("行：不用斑马纹；悬停 bg.subtle；选中 accent.subtle + 左侧 2px 强调色条", () => {
    const sx = darkSx();
    expect(sx["& .MuiDataGrid-row"]).toMatchObject({ backgroundColor: "transparent" });
    expect(sx["& .MuiDataGrid-row:hover"]).toMatchObject({
      backgroundColor: tokens.palette.dark.bg.subtle,
    });
    const selected = sx["& .MuiDataGrid-row.Mui-selected"] ?? {};
    expect(selected["backgroundColor"]).toBe(tokens.palette.dark.accentSubtle);
    expect(String(selected["boxShadow"])).toContain(tokens.palette.dark.accent);
    expect(String(selected["boxShadow"])).toContain("inset 2px");
  });

  it("localeText 桥接：中文界面不会冒出 MUI X 的英文表格文案", () => {
    const zh = dataGridLocaleText("zh-CN");
    const en = dataGridLocaleText("en");
    // 空状态是最常见的一屏（[15] 要求诚实的空状态，而不是英文 "No rows"）
    expect(zh.noRowsLabel).toBe("没有数据。");
    expect(en.noRowsLabel).toBe("No rows");
    expect(zh.paginationRowsPerPage).toBeTruthy();
    expect(zh.paginationRowsPerPage).not.toBe(en.paginationRowsPerPage);
  });

  it("真渲染：预设能被当前安装的 MUI X v8 消费（API 对得上）", () => {
    render(
      <ThemeProvider theme={buildTheme("dark")}>
        <div style={{ height: 240, width: 480 }}>
          <DataGrid
            rows={[{ id: 1, name: "字节方舟" }]}
            columns={[{ field: "name", headerName: "名称", flex: 1 }]}
            {...dataGridDefaults}
            localeText={dataGridLocaleText("zh-CN")}
          />
        </div>
      </ThemeProvider>,
    );

    // 表头与单元格都渲染出来了（不是空表/白屏）
    expect(screen.getByText("名称")).toBeTruthy();
    expect(screen.getByText("字节方舟")).toBeTruthy();
  });
});

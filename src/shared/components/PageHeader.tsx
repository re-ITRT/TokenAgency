/**
 * 页面头：**F2–F4 每个页面的第一块**，统一"标题 — 说明 — 契约出处 — 右侧操作"的排布。
 *
 * 为什么要做成共享组件而不是各页自己写：这四件事的**字号层级**在 [15] 里是定死的
 * （标题 `text.page` 20/500、说明正文），四个包各写一遍必然写出四种大小。
 *
 * 规则（[15]）：
 * - 标题只有一档 `text.page`（**hero 留给看板主数字，一页最多一个**）；
 * - 位置/间距只从刻度取：页面边距 32（§3.3），区块之间 24；
 * - 没有一个装饰性元素：没有背景色块、没有边框、没有图标装饰（§1 克制）。
 *
 * I2 边界：`badge` 只允许放**只读展示**（如实验模式的圆点）——
 * 任何"顺手改系统设置"的开关都不属于这里，而属于设置页里明确的用户操作。
 */

import { Box, Typography } from "@mui/material";
import type { ReactNode } from "react";


import { HelpTip } from "./HelpTip";

export type PageHeaderProps = {
  /** 页面标题（已翻译）。 */
  title: string;
  /** 一句话说明这页是干什么的；没有就省略，不要写"这里什么都没有"。 */
  description?: string;
  /** 标题右侧的只读角标（如 `<StatusDot>`）。 */
  badge?: ReactNode;
  /** 右侧操作区（按钮）。不传就不占位。 */
  actions?: ReactNode;
};

export function PageHeader({ title, description, badge, actions }: PageHeaderProps) {
  return (
    <Box
      component="header"
      data-testid="page-header"
      sx={{
        px: 8,
        pt: 8,
        pb: 6,
        display: "flex",
        alignItems: "flex-start",
        gap: 4,
      }}
    >
      <Box sx={{ minWidth: 0, flex: 1, display: "flex", flexDirection: "column", gap: 1 }}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
          {/* 页面标题是文档里的第一个标题 → 必须是 `<h1>`（[B-7]）；
              字号仍由 `variant="h2"`（= `text.page` 20/500）决定，两者互不冲突 */}
          <Typography variant="h2" component="h1" noWrap>
            {title}
          </Typography>
          {badge}
          {/*
            页头说明**不再铺在版面上**（[docs/22]）：用户嫌"简介好长、好多字"，
            所以它变成标题右边的一个问号圆圈，悬停/聚焦才展开。
            正文一个字都没丢：`aria-label` 就是全文，测试与读屏都拿得到。
          */}
          {description !== undefined && description !== "" ? (
            /* [docs/31 §35] 问号不能被挤到右侧操作区下面（`/providers` 三个按钮把左列挤窄时，
               20px 的圆圈会落进 `actions` 的命中区 → 悬停/聚焦都不出气泡，真机 step22 就是这样红的）。 */
            <Box sx={{ display: "flex", flexShrink: 0 }}>
              <HelpTip text={description} testId="page-header-help" />
            </Box>
          ) : null}
          </Box>


      </Box>

      {actions !== undefined ? (
        <Box sx={{ display: "flex", alignItems: "center", gap: 2, flex: "0 0 auto" }}>
          {actions}
        </Box>
      ) : null}
    </Box>
  );
}

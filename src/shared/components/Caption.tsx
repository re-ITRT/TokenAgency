import { Typography } from "@mui/material";
import type { ReactElement, ReactNode } from "react";

/**
 * 次级说明文字（`caption` + `text.secondary`）。
 *
 * **为什么值得单独成组件**：这个形状在删页之前散在 **10 个页面文件**里各写一遍
 * （[docs/50] 的重复清单：8 份逐字相同，2 份各多一个属性）。
 * 10 份拷贝的问题不是行数，而是**它们会各自漂移** —— 改配色/字号时只改到其中几处，
 * 界面就会出现"差不多的两行小字"。收敛成一个之后，`text.secondary` 只有一处真源。
 *
 * 两个可选属性来自那 2 份"多了一点"的拷贝，保留它们是为了**不改变现有渲染**：
 * - `testId`：`/providers/:id` 的额度窗口那一段要按 testid 定位（真机 e2e 也在用）；
 * - `block`： `/models` 的那一份需要 `display: block`（它后面紧跟另一个块级元素）。
 */
export function Caption({
  children,
  testId,
  block = false,
}: {
  children: ReactNode;
  testId?: string;
  block?: boolean;
}): ReactElement {
  return (
    <Typography
      variant="caption"
      data-testid={testId}
      sx={block ? { color: "text.secondary", display: "block" } : { color: "text.secondary" }}
    >
      {children}
    </Typography>
  );
}

import { Box, Typography } from "@mui/material";
import type { ReactElement, ReactNode } from "react";

/**
 * 一个统计格：标签（`overline`，次要色）+ 值（等宽数字、可换行）。
 *
 * # 为什么收敛
 *
 * 第 319 轮机械测量发现这个组件在 **4 个页面各写了一遍**
 * （`settings` / `logs` / `models` / `network` 的 `StatItem`），
 * 前三个逐字相同，第四个多了一个 `testId` 参数 —— 正是 说的
 * "同一份东西写 N 遍，然后开始各自演化"。
 *
 * 取值上取**并集**：`testId` 可选（`models` 需要它给 e2e 定位，别的页不需要）。
 *
 * # 为什么值用等宽数字
 *
 * 这一格几乎总是数字（`tabular-nums`）：等宽之后一列统计格的数字
 * 才能上下对齐，扫一眼就能比较。**这是版面规则，不是风格偏好**，
 * 所以它必须由共享件保证，不能靠每个调用点自己记得写。
 */
export type StatItemProps = {
  label: string;
  value: ReactNode;
  /** 给测试/真机定位用（不传就不挂 `data-testid`，不产生空属性）。 */
  testId?: string;
};

export function StatItem({ label, value, testId }: StatItemProps): ReactElement {
  return (
    <Box sx={{ minWidth: 0 }} {...(testId === undefined ? {} : { "data-testid": testId })}>
      <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
        {label}
      </Typography>
      <Typography
        variant="body1"
        sx={{ fontVariantNumeric: "tabular-nums", overflowWrap: "anywhere" }}
      >
        {value}
      </Typography>
    </Box>
  );
}

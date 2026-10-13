/**
 * 未配额度提醒 —— **套餐列表 / 通道矩阵 / 首启向导三处必须同一句**。
 *
 * 为什么做成组件而不是各页各写一遍文案：三个页面各写一句，句子就一定会漂；
 * 而这句话的用户后果很实： 的"用完自动切"前提是**配了额度**，
 * 不填 `limit` 时本地累计推不出耗尽，只有上游响应头报耗尽才可能自动切 ——
 * 静默会让用户以为"用完自动切坏了"。
 *
 * **判定不在这里**： 的结论是判定必须走 E2 的纯函数
 * （`ta_core::quota::limit_gap` 覆盖了 `limit = 0` / 负数 / `NaN` / `inf`，
 * 前端自己判 `limit == null` 一定会漏）。所以本组件**只接收后端给的原文**：
 *
 * ```tsx
 * const gap = useCommandQuery("quota_limit_gaps", ...);   // E3 的命令面
 * gap.data?.map((g) => <LimitGapNotice key={g.credential_id} message={g.message} />)
 * ```
 *
 *：警告的表示是「文字 + warning 色 6px 圆点」，不是 emoji、也不是色块。
 *：`message` 里的用户数据（套餐名/节点名）**原样透传**，不做任何清洗。
 */

import { Box, Typography } from "@mui/material";

import { StatusDot } from "./StatusDot";

export type LimitGapNoticeProps = {
  /**
   * 后端 `LimitGap::message()` 的**原文**。传空串 = 没有要说的（组件不占位，也不编一句"正常"）。
   */
  message: string;
  /** 紧凑模式：表格单元格里用（单行、纵向居中）。 */
  compact?: boolean;
};

export function LimitGapNotice({ message, compact = false }: LimitGapNoticeProps) {
  if (message === "") return null;

  return (
    <Box
      data-testid="limit-gap-notice"
      sx={{
        display: "flex",
        alignItems: compact ? "center" : "flex-start",
        gap: 1,
        minWidth: 0,
      }}
    >
      <Box sx={{ mt: compact ? 0 : 0.5, display: "inline-flex" }}>
        <StatusDot status="warn" />
      </Box>
      <Typography
        variant="body1"
        sx={{ minWidth: 0, wordBreak: "break-word", color: "text.primary" }}
      >
        {message}
      </Typography>
    </Box>
  );
}

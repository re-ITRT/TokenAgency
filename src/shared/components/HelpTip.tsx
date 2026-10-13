/**
 * 「问号圆圈 + 悬停提示」——**长篇介绍的唯一去处**。
 *
 * 为什么要有它：页面上原本有一大段一大段的说明（页头一句话、区块说明、各种 hint），
 * 用户的原话是「里面的简介好长，好多字……那些比较介绍做成一个问号加一个圆圈，鼠标移上去能看到提示」。
 * 所以规则不是"删掉解释"，而是**把解释从版面上收进一个 16px 的圆里**：需要的人悬停（或键盘 Tab 聚焦）
 * 才看得到，不需要的人不用被一屏字挡住。
 *
 * 三条硬规则：
 * 1. **只收"解释"**：状态结论、错误原文、必填/校验提示、破坏性操作的后果 —— 一律留在页面上。
 * 2. **无障碍不许退步**：`aria-label` 就是提示正文，所以读屏和"只看无障碍树"的调用方仍然拿得到全文
 *    （提示气泡本身是懒渲染的，只在悬停/聚焦时进 DOM）。
 * 3. **不能自己发明视觉**：只有主题令牌（`text.secondary`），没有颜色字面量、没有 emoji（design.guard 会查）。
 */

import HelpOutlineIcon from "@mui/icons-material/HelpOutline";
import { IconButton, Tooltip } from "@mui/material";
import type { TooltipProps } from "@mui/material";

export type HelpTipProps = {
  /** 提示正文（已翻译全文）。同时作为触发按钮的 `aria-label`。 */
  text: string;
  /** testid：约定 `<页头或区块的 testid>-help`；页头统一是 `page-header-help`。 */
  testId?: string;
  placement?: TooltipProps["placement"];
};

export function HelpTip({ text, testId, placement = "top" }: HelpTipProps) {
  return (
    <Tooltip
      title={text}
      arrow
      placement={placement}
      enterTouchDelay={0}
      leaveTouchDelay={6000}
      // 同一区块的几句解释会合成一条提示（`mergeHelpText` 用 `\n` 连），所以气泡里要**保留换行**，
      // 否则 5 句挤成一行，比原来铺在版面上更难读。
      slotProps={{ tooltip: { sx: { whiteSpace: "pre-line", maxWidth: 420 } } }}
    >
      <IconButton
        size="small"
        data-testid={testId}
        aria-label={text}
        /* 间距只从刻度取（[15 §3.3] 只允许 2/4/8/12/16/24/32/48/64；`p: 0.5` = 2px） */
        sx={{ ml: 0.5, p: 0.5, color: "text.secondary", verticalAlign: "middle" }}
      >
        <HelpOutlineIcon sx={{ fontSize: 16 }} />
      </IconButton>
    </Tooltip>
  );
}

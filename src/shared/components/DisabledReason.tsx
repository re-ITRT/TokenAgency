/**
 * 「为什么这个控件是灰的」—— 把**后端给的原因原文**挂到被禁用的控件上（[06 §10.2 D2]）。
 *
 * 为什么需要它，而不是各页自己写 `disabled` + `Tooltip`：
 * 1. **MUI 的经典坑**：禁用的控件不派发鼠标事件 → 直接套 `Tooltip` 时提示永不出现，必须包一层容器。
 * 2. **文案不能各写一份**：[06 §10.2 D2] 要求"禁用该筛选项并说明原因"，
 *    原因原文来自后端（`Support { enabled, reason: Option<&str> }`）。前端自己写文案 → 三处就会漂。
 *
 * 两个组件分工（**别用错**，用错会出无障碍问题）：
 *
 * | 场景 | 用哪个 | 为什么 |
 * |---|---|---|
 * | 表单/工具栏里的 `Button` / `Checkbox` / `Select` 等**非列表**控件 | `<DisabledReason>` | 需要一层容器承接 hover，`<span>` 合法 |
 * | `<Menu>` / `<Select>` 的**选项**（`MenuItem`） | `<DisabledMenuItem>` | `MenuItem` 自己就是 `<li>`，外包一层会得到 `ul > span > li`（React 会告警），而且 `MenuList` 会克隆直接子节点、接管键盘导航 —— 外包会打断它 |
 *
 * 无障碍（原实现的两个真缺陷，已修）：
 * - **键盘可达**：容器 `tabIndex={0}`（MUI Tooltip 同时监听 focus/blur，键盘用户也能看到原因）；
 * - **读屏可达**：原因**常驻在无障碍树里**（视觉隐藏的 `<span>` + `aria-describedby`），
 *   不依赖 hover 才进入 DOM。`DisabledMenuItem` 更进一步：原因直接显示成次要文字，
 *   连"能不能聚焦到"都不影响用户看到它。
 */

import { Box, MenuItem, Tooltip, type MenuItemProps } from "@mui/material";
import { useId, type ReactElement } from "react";

import { typography } from "@/theme";

/** 只有"真的要说原因"时才显示（否则不占位、不编一句假理由）。 */
function hasReason(disabled: boolean, reason: string | null): boolean {
  return disabled && reason !== null && reason !== "";
}

/** 视觉隐藏但**留在无障碍树里**（裁剪而不是 `display:none`）。 */
const VISUALLY_HIDDEN = {
  position: "absolute",
  width: 1,
  height: 1,
  overflow: "hidden",
  clipPath: "inset(50%)",
  whiteSpace: "nowrap",
} as const;

export type DisabledReasonProps = {
  /** 是否禁用。**由后端能力决定**（前端不重判）。 */
  disabled: boolean;
  /**
   * 禁用原因的**原文**。契约（E2 的 `Support`）：`enabled=true` → `null`；
   * `null`/空串 = 没原因可说 → 不挂提示、不编假理由。
   */
  reason: string | null;
  /** 被包装的控件（`Button` / `Checkbox` / 非菜单场景的控件）。 */
  children: ReactElement;
};

export function DisabledReason({ disabled, reason, children }: DisabledReasonProps) {
  const id = useId();
  const show = hasReason(disabled, reason);

  // 无论禁用与否都保持同一棵结构：`disabled` 翻转时不会重挂载子树（菜单/表单里尤其重要）
  return (
    <Tooltip title={show ? (reason ?? "") : ""}>
      <Box
        component="span"
        data-testid="disabled-reason"
        {...(show ? { tabIndex: 0, "aria-describedby": id } : {})}
        sx={{
          display: "inline-flex",
          alignItems: "center",
          minWidth: 0,
          ...(show ? { cursor: "not-allowed" } : {}),
        }}
      >
        {children}
        {show ? (
          <Box component="span" id={id} sx={VISUALLY_HIDDEN}>
            {reason}
          </Box>
        ) : null}
      </Box>
    </Tooltip>
  );
}

export type DisabledMenuItemProps = Omit<MenuItemProps, "disabled" | "children"> & {
  /** 是否禁用。**由后端能力决定**（前端不重判）。 */
  disabled: boolean;
  /** 禁用原因的**原文**（`null`/空串 = 没原因可说）。 */
  reason: string | null;
  children: ReactElement | string;
};

/**
 * 菜单/下拉里的**禁用选项**：原因以**常驻次要文字**呈现，不依赖 hover 或聚焦。
 *
 * 为什么不做成 `<span>` 包 `MenuItem`：那会得到 `ul > span > li`（非法嵌套），
 * 且 `MenuList` 克隆的是直接子节点 —— 外包一层会接管键盘导航，选项就选不动了。
 * 这里直接把 `MenuItem` 当直接子节点，`...rest` 透传让 `MenuList` 的注入（ref/tabIndex/onKeyDown）落到它身上。
 */
export function DisabledMenuItem({ disabled, reason, children, ...rest }: DisabledMenuItemProps) {
  const id = useId();
  const show = hasReason(disabled, reason);

  return (
    <MenuItem
      {...rest}
      disabled={disabled}
      {...(show ? { "aria-describedby": id } : {})}
      // 禁用的菜单项默认 `pointer-events:none` → 连"为什么"都点不出来；这里放行指针事件（不放行选择）
      sx={{ ...(show ? { pointerEvents: "auto" } : {}), ...rest.sx }}
    >
      {children}
      {show ? (
        <Box
          component="span"
          id={id}
          sx={{ ml: 2, color: "text.secondary", fontSize: typography.size.meta, whiteSpace: "normal" }}
        >
          {reason}
        </Box>
      ) : null}
    </MenuItem>
  );
}

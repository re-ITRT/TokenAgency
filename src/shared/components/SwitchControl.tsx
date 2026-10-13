import { FormControlLabel, Stack, Switch } from "@mui/material";
import type { ReactElement } from "react";

/**
 * 一个**符号化**的开关（MUI `Switch` + 可达名称）。
 *
 * **为什么值得共享**：这个形状原先在 `/models` 与 `/providers/:id` 各写了一份
 * （[docs/50] 的重复清单）。两处不一致的地方恰恰是最容易出无障碍缺陷的地方 ——
 * 开关在视觉上不写"启用/关闭"两个字（[15 §2] 的符号化），于是**读屏的名称只能来自
 * `aria-label`**；两份实现里只要有一份忘了给，那一屏的开关对读屏用户就是"无名称的复选框"。
 *
 * 共享之后 `label` 是**必填**参数，没有"忘了给"的可能。
 *
 * `hideLabel`（默认 `false`）：
 * - `true` = 版面上不画文字，`label` 变成 `aria-label`（列表里每行一个开关时用这个）；
 * - `false` = `label` 作为可见文字画出来（MUI 会自动把它关联成可达名称，不必再给 `aria-label`）。
 *
 * `data-testid` 挂在**外层 `Stack`** 上、`role="switch"` 在内层 `input` 上 —— 这是
 * 真机 e2e 的驱动口径（`wdSetSwitch` 先按 testid 找 host，再在里面找 `input[type=checkbox]`），
 * **不许改**。
 */
export function SwitchControl({
  testId,
  label,
  checked,
  onChange,
  disabled = false,
  hideLabel = false,
}: {
  testId: string;
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  hideLabel?: boolean;
}): ReactElement {
  return (
    <Stack data-testid={testId}>
      <FormControlLabel
        control={
          <Switch
            size="small"
            checked={checked}
            disabled={disabled}
            onChange={(_, next) => onChange(next)}
            // 符号化时视觉上不写那两个字，但读屏仍要读得到（无障碍名称一个字不少）
            inputProps={hideLabel ? { "aria-label": label } : undefined}
          />
        }
        label={hideLabel ? undefined : label}
      />
    </Stack>
  );
}

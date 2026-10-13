/**
 * 删除确认对话框——**两个页面族共用的同一份**。
 *
 * 为什么必须是共享组件而不是各页各写一个：删除是**不可逆**操作，它在四个页面上必须长得一样、
 * 说的话一样、失败时展示的也是同一件事（后端原文）。四个包各写一遍，必然出现
 * "有的页面点了直接删、有的页面二次确认、有的页面把被引用说成失败"三种口径。
 *
 * 三条硬规则：
 * 1. **永远先确认**：确认框里写清删的是哪一个（名称 + id）；有连带影响就把清单列出来。
 * 2. **被引用时不许假装**：后端说"被引用"就把**原文**摆出来；只有 `provider` / `credential` / `model`
 *    有 `force` 语义，才给"确认级联删除"按钮 —— 其余实体给这个按钮就是假按钮。
 * 3. **承诺要说实话**：`consequences` 只能是前端算得出来的事实（例如"该服务商下 3 个凭据"）；
 *    算不出来就留空，**不编**。
 *
 * 视觉规则来自 [15]：只用主题令牌（`error.main` / `text.secondary`），没有颜色字面量、没有 emoji。
 */

import {
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  List,
  ListItem,
  ListItemText,
  Stack,
  Typography,
} from "@mui/material";

import { useTranslation } from "@/shared/i18n";

export type ConfirmDeleteProps = {
  open: boolean;
  /** testid 前缀：拼出 `<prefix>-delete-dialog` / `-delete-confirm` / `-delete-cascade` / `-delete-cancel` / `-delete-error`。 */
  testIdPrefix: string;
  /** 标题，例如「删除服务商」（已翻译）。 */
  title: string;
  /** 删的是哪一个：名称 + id（**原样**，不改写用户数据）。 */
  targetLabel: string;
  /** 会连带删除/影响什么（只放算得出来的事实；没有就留空，不编）。 */
  consequences?: readonly string[];
  /** 只有 provider / credential / model 为 true：被引用时给"确认级联删除"。 */
  cascadeAvailable: boolean;
  /** 正在提交：确认按钮进入禁用态（**不允许**把它写成"删除了"的假进度）。 */
  pending?: boolean;
  /** 后端原文（被引用时的错误）。有它才展示，也只有它才能把级联按钮显出来。 */
  errorText?: string;
  onConfirm: () => void;
  /**
   * 级联确认（`force=true`）。
   *
   * **可以省略**：有些删除路径压根没有 `force` 语义（`channel_delete` 就是 —— 被策略点名引用时
   * 它只会拒绝，绝不会"确认一下就连带删掉"）。那种情况下传了也没用，所以不必传；
   * 配合 `cascadeAvailable={false}`，那个按钮根本不会出现（不会变成假按钮）。
   */
  onConfirmCascade?: () => void;
  onCancel: () => void;
};

export function ConfirmDelete({
  open,
  testIdPrefix,
  title,
  targetLabel,
  consequences,
  cascadeAvailable,
  pending = false,
  errorText,
  onConfirm,
  onConfirmCascade,
  onCancel,
}: ConfirmDeleteProps) {
  const { t } = useTranslation();
  const listed = consequences ?? [];
  const blocked = errorText !== undefined && errorText !== "";

  return (
    <Dialog
      open={open}
      onClose={onCancel}
      fullWidth
      maxWidth="sm"
      data-testid={`${testIdPrefix}-delete-dialog`}
    >
      <DialogTitle>{title}</DialogTitle>
      <DialogContent>
        <Stack spacing={2}>
          <Typography variant="body1">{t("delete.body")}</Typography>
          <Typography variant="body1" data-testid={`${testIdPrefix}-delete-target`}>
            {targetLabel}
          </Typography>
          {listed.length > 0 ? (
            <Stack spacing={1}>
              {/*
                表头必须**分两种情况**，这是第 320 轮用户真机报上来的那个 bug：
                * `cascadeAvailable`（provider / credential / model 有 force）→ 列出的东西**会被一并删掉**，
                  说"会连带删除 / 影响"是对的；
                * 没有 force（`subscription` / `lane`）→ 列出的引用**恰恰会拦住这次删除**：
                  后端直接拒（TA-5001），一个字都不会删。原来这里也写"会连带删除"，
                  于是对话框承诺了一件不会发生的事 —— 用户确认后只拿到一个 TA 码。
              */}
              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                {cascadeAvailable ? t("delete.consequences") : t("delete.blockers")}
              </Typography>
              <List dense disablePadding data-testid={`${testIdPrefix}-delete-consequences`}>
                {listed.map((line) => (
                  <ListItem key={line} disableGutters>
                    <ListItemText primary={line} />
                  </ListItem>
                ))}
              </List>
            </Stack>
          ) : null}
          {blocked ? (
            // 后端原文（含 TA 码）：不改写、不截断、不吞
            <Typography
              variant="body1"
              data-testid={`${testIdPrefix}-delete-error`}
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {errorText}
            </Typography>
          ) : null}
          {cascadeAvailable && blocked ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("delete.cascadeHint")}
            </Typography>
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button variant="text" data-testid={`${testIdPrefix}-delete-cancel`} onClick={onCancel}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="contained"
          color="error"
          data-testid={`${testIdPrefix}-delete-confirm`}
          disabled={pending}
          onClick={onConfirm}
        >
          {t("common.delete")}
        </Button>
        {cascadeAvailable && blocked ? (
          <Button
            variant="contained"
            color="error"
            data-testid={`${testIdPrefix}-delete-cascade`}
            disabled={pending}
            onClick={onConfirmCascade}
          >
            {t("delete.cascade")}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
}

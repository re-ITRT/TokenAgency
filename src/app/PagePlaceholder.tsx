/**
 * 占位壳：**页面还没实现时也必须能渲染**（DoD：每个路由都不白屏、不报错）。
 *
 * 它把三件对开发有用的信息摆在明面上：
 * - 归属（F2/F3/F4）—— 谁该来填这个坑；
 * - 该放哪个文件（约定路径，见 `pageRegistry.tsx`）—— 放进去就自动接线；
 * - 契约出处（文档章节）。
 *
 * 视觉（[15]）：虚线边框本身承担信息（"这块还没实现"），所以留着；
 * 其余靠文字层级（`text.page` → `text.body` → `text.meta`），不用色块与 chip（§1 克制）。
 */

import { Box, Paper, Stack, Typography } from "@mui/material";

import { useTranslation } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import { primaryCandidate } from "./pageRegistry";

export type PagePlaceholderProps = {
  /** 已翻译的页面标题。 */
  title: string;
  /** 归属工作包（F2/F3/F4…）。 */
  owner: string;
  /** 页面组件候选路径（相对 `src/pages/`）。 */
  pages: readonly string[];
  /** 契约出处，如 `07 §1 首页看板`。 */
  docRef: string;
  /** 为什么落到占位壳（排查用）。 */
  note?: string;
};

export function PagePlaceholder({ title, owner, pages, docRef, note }: PagePlaceholderProps) {
  const { t } = useTranslation();
  const path = primaryCandidate(pages);

  return (
    <Box
      sx={{ p: 8, maxWidth: 820 }}
      data-testid="page-placeholder"
      data-page-path={path}
    >
      <Paper
        variant="outlined"
        sx={{
          p: 6,
          borderStyle: "dashed",
          borderRadius: `${radius.surface}px`,
          bgcolor: "background.paper",
        }}
      >
        <Stack spacing={3}>
          <Stack spacing={1}>
            <Typography variant="h2">{title}</Typography>
            <Typography variant="caption" sx={{ color: "text.secondary" }}>
              {`${t("common.owner")} ${owner} · ${t("common.notImplemented")}`}
            </Typography>
          </Stack>

          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {t("placeholder.body", { owner })}
          </Typography>

          {path !== "" ? (
            <Stack spacing={1}>
              <Typography
                sx={{
                  fontFamily: typography.fontMono,
                  fontSize: typography.size.body,
                  lineHeight: typography.lineHeight.table,
                  color: "text.primary",
                }}
              >
                src/pages/{path}.tsx
              </Typography>
              <Typography variant="caption" sx={{ color: "text.secondary" }}>
                {t("placeholder.pagePath", { path: `${path}.tsx` })}
              </Typography>
            </Stack>
          ) : null}

          <Typography variant="caption" sx={{ color: "text.secondary" }}>
            {t("placeholder.docRef", { doc: docRef })}
          </Typography>

          {note !== undefined ? (
            <Typography variant="caption" sx={{ color: "warning.main" }}>
              {note}
            </Typography>
          ) : null}
        </Stack>
      </Paper>
    </Box>
  );
}

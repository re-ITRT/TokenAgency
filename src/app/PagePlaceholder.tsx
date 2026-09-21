/**
 * 占位壳：**页面还没实现时也必须能渲染**（DoD：每个路由都不白屏、不报错）。
 *
 * 它把三件对开发有用的信息摆在明面上：
 * - 归属（F2/F3/F4）—— 谁该来填这个坑；
 * - 该放哪个文件（约定路径，见 `pageRegistry.tsx`）—— 放进去就自动接线；
 * - 契约出处（文档章节）。
 */

import { Box, Chip, Paper, Stack, Typography } from "@mui/material";

import { useTranslation } from "@/shared/i18n";

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
    <Box sx={{ p: 2 }} data-testid="page-placeholder" data-page-path={path}>
      <Paper
        variant="outlined"
        sx={{ p: 3, borderStyle: "dashed", bgcolor: "transparent", maxWidth: 720 }}
      >
        <Stack spacing={1.25}>
          <Stack direction="row" spacing={1} alignItems="center">
            <Typography variant="h6">{title}</Typography>
            <Chip size="small" label={`${t("common.owner")}: ${owner}`} />
            <Chip size="small" variant="outlined" label={t("common.notImplemented")} />
          </Stack>

          <Typography variant="body2" color="text.secondary">
            {t("placeholder.body", { owner })}
          </Typography>

          {path !== "" ? (
            <>
              <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
                src/pages/{path}.tsx
              </Typography>
              <Typography variant="caption" color="text.secondary">
                {t("placeholder.pagePath", { path: `${path}.tsx` })}
              </Typography>
            </>
          ) : null}

          <Typography variant="caption" color="text.secondary">
            {t("placeholder.docRef", { doc: docRef })}
          </Typography>

          {note !== undefined ? (
            <Typography variant="caption" color="warning.main">
              {note}
            </Typography>
          ) : null}
        </Stack>
      </Paper>
    </Box>
  );
}

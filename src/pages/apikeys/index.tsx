/**
 * 访问密钥页 `/api-keys`（[docs/19 §3.4]）—— **我们网关对外的**访问密钥。
 * 客户端拿它调 `127.0.0.1:8787`（与配置里的 `gateway.local_key` 是两条并行的鉴权路径）。
 *
 * 两条**诚实**硬要求（这一页最容易违反，所以写在最上面）：
 *
 * 1. **明文只在创建时出现一次**：`api_key_create` 的返回是唯一一次能看到明文的时刻
 *    （库里只存 sha256）。所以创建成功后把明文显示出来 + 一句"这是唯一一次能看到明文的
 *    机会，请立刻保存"（`apiKeys.createdHint`），用户点「我已保存」后**从界面上消失** ——
 *    状态里也清掉，不留副本；列表**从来**拿不到明文，所以没有任何"再看一眼"的路径。
 * 2. **列表里不放复制按钮**：只存哈希，复制不出来。用 `apiKeys.hashOnlyNotice`
 *    把这件事实说清楚 —— 这是诚实，不是功能缺失。
 *
 * 另外：`last_used_at_ms` 为 `null` = **从未使用**（显示"从未使用"，**不显示 0/1970**）。
 * `0` 是"确实用过、时间戳是 0"，那是另一件事，不许混。
 *
 * 数据流：
 * - `useCommandQuery("api_key_list", undefined, qk.apiKeys)` → 列表（不含明文、不含 hash）
 * - `useCommandMutation("api_key_create")` → 创建，返回 `{ key, plaintext }`
 * - `useCommandMutation("api_key_set_enabled" | "api_key_delete")` → 启停 / 删除（都立刻影响鉴权）
 * - `useCommandMutation("api_key_usage")` → 某一行的用量（点了才取，不预取 N 条）
 */

import {
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Paper,
  Stack,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { ApiKeyCreated, ApiKeyUsageRow, ApiKeyView } from "@/shared/bindings";
import { PageHeader } from "@/shared/components";
import { formatInt, formatLocalTime } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

/** 一次取多少条用量（后端按 key × 日聚合）。 */
const USAGE_LIMIT = 50;

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

function Panel({
  titleKey,
  testId,
  children,
}: {
  titleKey: I18nKey;
  testId: string;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  return (
    <Paper variant="outlined" sx={SECTION_SX} data-testid={testId}>
      <Stack spacing={3}>
        <Typography variant="h4">{t(titleKey)}</Typography>
        {children}
      </Stack>
    </Paper>
  );
}

/**
 * 「上次使用」的可读形态。
 *
 * `null`/`undefined` = **从未使用**（后端就是这么说的），**绝不**渲染成 `0` 或 1970 年 ——
 * 那会把"不知道/没用过"和"确实在某个时刻用过"混成同一件事。
 */
function lastUsedText(value: number | null | undefined, never: string): string {
  return value === null || value === undefined ? never : formatLocalTime(value);
}

/**
 * 一行用量（后端按 key × 日 × **token 种类** 聚合 —— 形状与 `ta-store` 的类型逐字一致）。
 *
 * **不在前端丢掉 `token_kind` / `currency`**：那是后端的真值，丢掉就等于造了第二份"汇总口径"。
 */
function usageLabel(row: ApiKeyUsageRow): string {
  return [
    row.day,
    row.token_kind,
    `${formatInt(row.requests)} 次`,
    `${formatInt(row.tokens)} tokens`,
    `${row.cost.toFixed(4)} ${row.currency}`,
  ].join(" · ");
}

export default function ApiKeysPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const list = useCommandQuery("api_key_list", undefined, qk.apiKeys);
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.apiKeys });

  const create = useCommandMutation("api_key_create", { onSuccess: invalidate });
  const setEnabled = useCommandMutation("api_key_set_enabled", { onSuccess: invalidate });
  const remove = useCommandMutation("api_key_delete", { onSuccess: invalidate });
  const usage = useCommandMutation("api_key_usage");

  const [createOpen, setCreateOpen] = useState(false);
  const [name, setName] = useState("");
  const [note, setNote] = useState("");
  /**
   * **唯一一次**的明文。点「我已保存」后置回 `undefined` —— 它从界面上消失，
   * 也不再有任何地方能取回（列表里本来就没有）。
   */
  const [created, setCreated] = useState<ApiKeyCreated | undefined>(undefined);
  /** 待确认删除的那一行（二次确认之前**绝不**调用后端）。 */
  const [pendingDelete, setPendingDelete] = useState<ApiKeyView | undefined>(undefined);
  /** 正在看用量的那一行。 */
  const [usageFor, setUsageFor] = useState<string | undefined>(undefined);

  const rows = list.data ?? [];

  const onCreate = () => {
    const trimmed = name.trim();
    if (trimmed === "") return;
    create.mutate(
      { name: trimmed, ...(note.trim() === "" ? {} : { note: note.trim() }) },
      {
        onSuccess: (result) => {
          setCreated(result);
          setCreateOpen(false);
          setName("");
          setNote("");
        },
      },
    );
  };

  const onToggle = (row: ApiKeyView, enabled: boolean) => {
    setEnabled.mutate({ id: row.id, enabled });
  };

  const onDeleteConfirmed = () => {
    const target = pendingDelete;
    setPendingDelete(undefined);
    if (target === undefined) return;
    remove.mutate({ id: target.id });
  };

  const onUsage = (row: ApiKeyView) => {
    if (usageFor === row.id) {
      setUsageFor(undefined);
      return;
    }
    setUsageFor(row.id);
    usage.mutate({ id: row.id, limit: USAGE_LIMIT });
  };

  return (
    <>
      <PageHeader
        title={t("nav.apiKeys")}
        description={t("apiKeys.description")}
        docRef="19 §3.4 访问密钥（明文只显示一次；列表不放复制按钮）"
        actions={
          <Button variant="outlined" onClick={() => void list.refetch()}>
            {t("common.refresh")}
          </Button>
        }
      />

      <Box
        data-testid="api-keys-page"
        sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}
      >
        {/* ① 新建 */}
        <Panel titleKey="apiKeys.create" testId="api-keys-create-panel">
          <Box>
            <Button
              data-testid="api-keys-create"
              variant="outlined"
              onClick={() => setCreateOpen((prev) => !prev)}
            >
              {t("apiKeys.create")}
            </Button>
          </Box>
          {createOpen ? (
            <Stack spacing={3}>
              <Box
                sx={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
                  gap: 3,
                }}
              >
                <Box data-testid="api-keys-create-name">
                  <TextField
                    fullWidth
                    label={t("apiKeys.name")}
                    value={name}
                    onChange={(event) => setName(event.target.value)}
                  />
                </Box>
                <Box data-testid="api-keys-create-note">
                  <TextField
                    fullWidth
                    label={t("apiKeys.note")}
                    value={note}
                    onChange={(event) => setNote(event.target.value)}
                  />
                </Box>
              </Box>
              <Box>
                <Button
                  data-testid="api-keys-create-submit"
                  variant="contained"
                  disabled={name.trim() === "" || create.isPending}
                  onClick={onCreate}
                >
                  {t("apiKeys.create")}
                </Button>
              </Box>
              {create.error !== null && create.error !== undefined ? (
                <Typography
                  variant="body1"
                  data-testid="api-keys-create-error"
                  sx={{ color: "error.main" }}
                >
                  {errorMessage(create.error)}
                </Typography>
              ) : null}
            </Stack>
          ) : null}
        </Panel>

        {/* ② 明文：**只有这里、只有这一次** */}
        {created !== undefined ? (
          <Panel titleKey="apiKeys.createdTitle" testId="api-keys-created">
            <Stack spacing={3}>
              <Box
                data-testid="api-keys-created-plaintext"
                sx={{
                  p: 3,
                  border: 1,
                  borderColor: "divider",
                  borderRadius: `${radius.control}px`,
                  overflowWrap: "anywhere",
                }}
              >
                <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                  {created.plaintext}
                </Typography>
              </Box>
              <Typography variant="body1" data-testid="api-keys-created-hint">
                {t("apiKeys.createdHint")}
              </Typography>
              {/* 点它 → 明文从界面上消失（这条是硬要求，测试会断言消失） */}
              <Box>
                <Button
                  data-testid="api-keys-created-done"
                  variant="contained"
                  onClick={() => setCreated(undefined)}
                >
                  {t("apiKeys.createdDone")}
                </Button>
              </Box>
            </Stack>
          </Panel>
        ) : null}

        {/* ③ 列表 */}
        <Panel titleKey="apiKeys.title" testId="api-keys-list-panel">
          {list.error !== null && list.error !== undefined ? (
            <Typography variant="body1" data-testid="api-keys-error" sx={{ color: "error.main" }}>
              {errorMessage(list.error)}
            </Typography>
          ) : list.isPending ? (
            <Caption>{t("common.loading")}</Caption>
          ) : rows.length === 0 ? (
            <Typography variant="body1" data-testid="api-keys-empty">
              {t("apiKeys.empty")}
            </Typography>
          ) : (
            <Table data-testid="api-keys-table">
              <TableHead>
                <TableRow>
                  <TableCell>{t("apiKeys.name")}</TableCell>
                  <TableCell>{t("apiKeys.prefix")}</TableCell>
                  <TableCell>{t("apiKeys.enabled")}</TableCell>
                  <TableCell>{t("apiKeys.lastUsed")}</TableCell>
                  <TableCell />
                </TableRow>
              </TableHead>
              <TableBody>
                {rows.map((row) => (
                  <TableRow key={row.id} data-testid="api-key-row" data-key-id={row.id}>
                    <TableCell>{row.name}</TableCell>
                    <TableCell sx={{ fontFamily: typography.fontMono }}>{row.key_prefix}</TableCell>
                    <TableCell>
                      <Box data-testid="api-key-toggle" data-key-id={row.id}>
                        <Switch
                          checked={row.enabled}
                          disabled={setEnabled.isPending}
                          onChange={(event) => onToggle(row, event.target.checked)}
                          slotProps={{ input: { "aria-label": row.name } }}
                        />
                      </Box>
                    </TableCell>
                    <TableCell
                      data-testid="api-key-last-used"
                      data-key-id={row.id}
                      data-used={row.last_used_at_ms === null || row.last_used_at_ms === undefined ? "never" : "once"}
                    >
                      {lastUsedText(row.last_used_at_ms, t("apiKeys.neverUsed"))}
                    </TableCell>
                    <TableCell>
                      <Stack direction="row" spacing={2} sx={{ alignItems: "center" }}>
                        <Button
                          data-testid="api-key-usage-open"
                          variant="outlined"
                          onClick={() => onUsage(row)}
                        >
                          {t("apiKeys.usage")}
                        </Button>
                        <Button
                          data-testid="api-key-delete"
                          variant="outlined"
                          color="error"
                          onClick={() => setPendingDelete(row)}
                        >
                          {t("apiKeys.delete")}
                        </Button>
                      </Stack>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}

          {/* 用量：点了某一行才取；结果只画在那一行下面 */}
          {usageFor !== undefined ? (
            <Box data-testid="api-key-usage" data-key-id={usageFor}>
              {usage.isPending ? (
                <Caption>{t("common.loading")}</Caption>
              ) : usage.error !== null && usage.error !== undefined ? (
                <Typography variant="body1" data-testid="api-key-usage-error" sx={{ color: "error.main" }}>
                  {errorMessage(usage.error)}
                </Typography>
              ) : (usage.data ?? []).length === 0 ? (
                <Typography variant="body1" data-testid="api-key-usage-empty">
                  {t("apiKeys.usageEmpty")}
                </Typography>
              ) : (
                <Stack spacing={1}>
                  {(usage.data ?? []).map((row) => (
                    <Typography
                      key={`${row.key_id}-${row.day}-${row.token_kind}`}
                      variant="body2"
                      sx={{ fontFamily: typography.fontMono }}
                    >
                      {usageLabel(row)}
                    </Typography>
                  ))}
                </Stack>
              )}
            </Box>
          ) : null}

          {/* 为什么列表里没有"复制"：只存哈希 —— 这是诚实，不是缺失 */}
          <Typography variant="body1" data-testid="api-keys-hash-only" sx={{ color: "text.secondary" }}>
            {t("apiKeys.hashOnlyNotice")}
          </Typography>
        </Panel>

        <Dialog open={pendingDelete !== undefined} onClose={() => setPendingDelete(undefined)}>
          <DialogTitle>{t("apiKeys.delete")}</DialogTitle>
          <DialogContent>
            <Typography variant="body1">{t("apiKeys.deleteHint")}</Typography>
          </DialogContent>
          <DialogActions>
            <Button onClick={() => setPendingDelete(undefined)}>{t("common.cancel")}</Button>
            <Button
              data-testid="api-key-delete-confirm"
              variant="contained"
              color="error"
              disabled={remove.isPending}
              onClick={onDeleteConfirmed}
            >
              {t("apiKeys.deleteConfirm")}
            </Button>
          </DialogActions>
        </Dialog>
      </Box>
    </>
  );
}

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
 * 第三条（[docs/21 §1]）：**删除先二次确认、失败不静默**。单条与批量共用共享的 `ConfirmDelete`；
 * 被拒绝时后端原文（含 TA 码）留在确认框里，`api_key_delete` 没有 force 语义 → 不放"强行删除"按钮。
 * 批量是**一次确认 → 逐条调用 → 逐条汇报**（一条失败不中止其余条）。
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
  Checkbox,
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
import { useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { ApiKeyCreated, ApiKeyUsageRow, ApiKeyView } from "@/shared/bindings";
import { SECTION_SX, Caption, AutoEllipsis, ConfirmDelete, PageHeader } from "@/shared/components";
import { formatInt, formatLocalTime } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";


/** 一次取多少条用量（后端按 key × 日聚合）。 */
const USAGE_LIMIT = 50;

/**
 * 一条删除的结果。**逐条如实汇报**：成功说名称，失败说标识 + 后端原文（[docs/21 §1.2]）。
 */
type DeleteOutcome = {
  /** 该条的身份（key id）—— 失败时的"标识"就是它。 */
  id: string;
  /** 名称 + id（原样，不改写用户数据）。 */
  label: string;
  ok: boolean;
  /** 失败时的后端原文（含 TA 码）。 */
  error?: string;
};

/** 一次删除操作的汇报：`batch` = 批量（要额外给"成功 N 条 / 失败 M 条"的一行）。 */
type DeleteReport = { batch: boolean; items: DeleteOutcome[] };

/** 名称 + id：确认框与结果区都用它写清"删的是哪一个"（[docs/21 §1.1 第 2 条]）。 */
function keyLabel(row: ApiKeyView): string {
  return row.name === "" ? row.id : `${row.name}（${row.id}）`;
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
  /**
   * 删除**不**挂 `onSuccess: invalidate`：批量是逐条调用，挂在这里就会每删一条重取一次列表
   * （N 条 = N 次重取）。两条删除路径都在**真的删成功之后**显式 `await invalidateAfterDelete()`
   * （失败路径不失效 —— 配置没变，重取只会白跑）。
   */
  const remove = useCommandMutation("api_key_delete");
  const usage = useCommandMutation("api_key_usage");

  const [createOpen, setCreateOpen] = useState(false);
  const [name, setName] = useState("");
  const [note, setNote] = useState("");
  /**
   * **唯一一次**的明文。点「我已保存」后置回 `undefined` —— 它从界面上消失，
   * 也不再有任何地方能取回（列表里本来就没有）。
   */
  const [created, setCreated] = useState<ApiKeyCreated | undefined>(undefined);
  /**
   * 删除状态（[docs/21 §1]）：单条与批量**共用同一个确认组件、同一套失败展示**。
   * `target` = 正在确认的单条（`null` = 没有）；`batchOpen` = 批量确认框。
   * **确认之前一条命令都不发** —— 旧实现在点「确认」的瞬间就把框关掉了，于是失败时用户
   * 什么也看不到（后端原文被丢掉）；现在失败会把原文留在框里。
   */
  const [target, setTarget] = useState<ApiKeyView | null>(null);
  const [batchOpen, setBatchOpen] = useState(false);
  /** 后端原文（删除失败时）：有它才展示，且**保持对话框打开**让用户看得见。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  const [report, setReport] = useState<DeleteReport | null>(null);
  /** 多选（行 `api-key-select` / 表头 `api-key-select-all` / 计数 `api-key-selected-count`）。 */
  const [selected, setSelected] = useState<string[]>([]);
  /** 批量进行中（逐条 `mutateAsync` 之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);
  /** 正在看用量的那一行。 */
  const [usageFor, setUsageFor] = useState<string | undefined>(undefined);

  const rows = list.data ?? [];
  const busy = remove.isPending || batchRunning;
  /**
   * 勾选集合。
   *
   * [docs/52] 性能：这一页原来在 `rows.every(...)` / `rows.some(...)` 和**每一行**的
   * `checked={selected.includes(row.id)}` 里直接扫 `selected` 数组 ——
   * 那是 **O(行数 × 勾选数)**，而且每次渲染都重算。
   * 看这里没有意义地重算正是本仓 `models` / `providers` 两页已经用 `Set` 解决过的问题
   * （它们先写了 `selectedSet = useMemo(() => new Set(selected))`），只有这一页漏了。
   * 行数一大（这一页允许一次列很多把密钥），复选框就是可感知的卡顿。
   */
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  /** 真正要删的行：勾选里还在列表上的那些。 */
  const targets = rows.filter((row) => selectedSet.has(row.id));

  /** 删除成功后**等**列表重取完再汇报（否则结果区会先说"已删除"、列表还停在旧行上）。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    await queryClient.invalidateQueries({ queryKey: qk.apiKeys });
  };

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

  /**
   * 单条删除：**只有确认之后才发命令**。成功 → 结果区写"已删除 <名称>"；
   * 失败 → 后端原文（含 TA 码）留在确认框里、框**不关**（旧实现把框先关了，失败就彻底静默）。
   */
  const onDeleteOne = async (row: ApiKeyView): Promise<void> => {
    setReport(null);
    try {
      await remove.mutateAsync({ id: row.id });
      setReport({ batch: false, items: [{ id: row.id, label: keyLabel(row), ok: true }] });
      setSelected((prev) => prev.filter((id) => id !== row.id));
      setTarget(null);
      setDeleteError(undefined);
      await invalidateAfterDelete();
    } catch (err) {
      setDeleteError(errorMessage(err));
    }
  };

  const onToggleSelect = (id: string, checked: boolean): void => {
    setSelected((prev) => (checked ? [...prev, id] : prev.filter((selectedId) => selectedId !== id)));
  };

  /**
   * 「删除选中」（[docs/21 §1.2]）：**逐条**调用，一条失败**不中止**其余条，
   * 最后如实汇报成功/失败条数 + 失败项的 id 与后端原文 —— 绝不用一句"批量删除成功"盖住部分失败。
   */
  const onDeleteSelected = async (): Promise<void> => {
    setBatchRunning(true);
    setReport(null);
    const items: DeleteOutcome[] = [];
    for (const row of targets) {
      try {
        await remove.mutateAsync({ id: row.id });
        items.push({ id: row.id, label: keyLabel(row), ok: true });
      } catch (err) {
        items.push({ id: row.id, label: keyLabel(row), ok: false, error: errorMessage(err) });
      }
    }
    setBatchRunning(false);
    setReport({ batch: true, items });
    setBatchOpen(false);
    setDeleteError(undefined);
    // 成功的已经不在列表里了 → 只让**失败的那些**保持选中（它们还在，可以直接重试）
    setSelected(items.filter((item) => !item.ok).map((item) => item.id));
    await invalidateAfterDelete();
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
        actions={
          <>
            <Button variant="outlined" onClick={() => void list.refetch()}>
              {t("common.refresh")}
            </Button>
            {/* [极简 · 判据 3「一屏一个主操作」] 新建密钥是这一页**唯一的主动作**：
                它现在就在页头。原来它被放进一张卡片里，而卡片的区块标题也叫「新建密钥」
                —— 同一个东西在同一屏里说两遍（标题 + 按钮），且那整张卡只为放这一个按钮。
                testid `api-keys-create` 原地保留（真机 E2E step18 按它点击）。 */}
            <Button
              data-testid="api-keys-create"
              variant="contained"
              onClick={() => setCreateOpen((prev) => !prev)}
            >
              {t("apiKeys.create")}
            </Button>
          </>
        }
      />

      <Box
        data-testid="api-keys-page"
        sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}
      >
        {/* ① 新建：**只有展开时**才有这一块表单（不再有"空卡片 + 同名按钮"） */}
        {createOpen ? (
          <Paper variant="outlined" sx={SECTION_SX} data-testid="api-keys-create-panel">
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
          </Paper>
        ) : null}

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
          {/* 批量删除（[docs/21 §1.2]）：有行才出现 —— 空列表上摆一排点了没用的控件就是假界面。
              选中数与按钮同排；一条都没选中时按钮进禁用态（功能是**做了的**，禁用不是占位）。 */}
          {rows.length > 0 ? (
            <Stack direction="row" spacing={2} sx={{ alignItems: "center", flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="api-key-selected-count">
                {t("delete.selectedCount", { count: selected.length })}
              </Typography>
              <Button
                variant="outlined"
                color="error"
                data-testid="api-key-delete-selected"
                disabled={selected.length === 0 || busy}
                onClick={() => {
                  setDeleteError(undefined);
                  setBatchOpen(true);
                }}
              >
                {t("common.deleteSelected")}
              </Button>
            </Stack>
          ) : null}

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
                  {/* 表头全选（`api-key-select-all`）：选中当前列表的全部行 */}
                  <TableCell padding="checkbox">
                    <Box data-testid="api-key-select-all">
                      <Checkbox
                        size="small"
                        checked={rows.length > 0 && rows.every((row) => selectedSet.has(row.id))}
                        indeterminate={
                          rows.some((row) => selectedSet.has(row.id)) &&
                          !rows.every((row) => selectedSet.has(row.id))
                        }
                        onChange={(event) => setSelected(event.target.checked ? rows.map((row) => row.id) : [])}
                      />
                    </Box>
                  </TableCell>
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
                    <TableCell padding="checkbox">
                      <Box data-testid="api-key-select">
                        <Checkbox
                          size="small"
                          checked={selectedSet.has(row.id)}
                          onChange={(event) => onToggleSelect(row.id, event.target.checked)}
                        />
                      </Box>
                    </TableCell>
                    <TableCell>
                      {/* [docs/26 §2] 密钥名是用户自己起的，可能很长。
                          恢复路径：密钥没有详情页 → 悬停/聚焦的 tooltip 就是恢复路径
                          （「前缀」那一列是另一个值，不受影响）。 */}
                      <AutoEllipsis testId="api-key-name-ellipsis" sx={{ maxWidth: 240 }}>
                        {row.name}
                      </AutoEllipsis>
                    </TableCell>
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
                          onClick={() => {
                            // **只开确认框**：命令在确认之后才发（[docs/21 §1.1 第 2 条]）
                            setDeleteError(undefined);
                            setTarget(row);
                          }}
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

        {/* 删除结果（[docs/21 §1.2] 的 `api-key-delete-result`）：成功写"已删除 <名称>"；
            批量先给成功/失败条数，失败项**逐条**带 id + 后端原文 —— 部分失败绝不盖成"成功"。 */}
        {report === null ? null : (
          <Paper variant="outlined" sx={SECTION_SX} data-testid="api-key-delete-result">
            <Stack spacing={1}>
              {report.batch ? (
                <Typography variant="body1" data-testid="api-key-delete-result-summary">
                  {t("delete.result.summary", {
                    ok: formatInt(report.items.filter((item) => item.ok).length),
                    failed: formatInt(report.items.filter((item) => !item.ok).length),
                  })}
                </Typography>
              ) : null}
              {report.items.map((item) => (
                <Typography
                  key={item.id}
                  variant="body1"
                  data-testid={`api-key-delete-result-${item.id}`}
                  sx={{ color: item.ok ? "text.primary" : "error.main", overflowWrap: "anywhere" }}
                >
                  {item.ok
                    ? t("delete.result.ok", { name: item.label })
                    : t("delete.result.failed", { message: `${item.id} · ${item.error ?? ""}` })}
                </Typography>
              ))}
            </Stack>
          </Paper>
        )}

        {/*
          删除确认（[docs/21 §1 / §2]）：单条与批量共用**同一个**共享组件。
          - **先确认再发命令**，确认框里写清删的是哪一个（名称 + id）；
          - 失败时后端原文（含 TA 码）留在框里、框**不关**（旧实现先关框 -> 失败彻底静默）；
          - `api_key_delete` 没有 force 语义 → `cascadeAvailable={false}`，不放任何"强行删除"按钮。
          - `testIdPrefix="api-key"` 拼出的 `api-key-delete-confirm` 是**真机 E2E**
            （`scripts/e2e/smoke.mjs` step18）依赖的 testid，连同行内的 `api-key-delete` 一起**不许改名**。
        */}
        <ConfirmDelete
          open={target !== null || batchOpen}
          testIdPrefix="api-key"
          title={t("delete.title.apiKey")}
          // 单条 = 名称 + id；批量 = 选中数 + 逐条标识
          targetLabel={
            target !== null
              ? keyLabel(target)
              : `${t("delete.selectedCount", { count: targets.length })} · ${targets.map(keyLabel).join("、")}`
          }
          // 能算出来的后果只有一条（它就是这条命令的语义）：删掉之后带着这个 key 的请求立刻 401
          consequences={[t("apiKeys.deleteHint")]}
          cascadeAvailable={false}
          pending={busy}
          errorText={deleteError}
          onConfirm={() => {
            if (target !== null) void onDeleteOne(target);
            else void onDeleteSelected();
          }}
          onConfirmCascade={() => {}}
          onCancel={() => {
            setTarget(null);
            setBatchOpen(false);
            setDeleteError(undefined);
          }}
        />
      </Box>
    </>
  );
}

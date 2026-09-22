/**
 * 模型目录页 `/models`（[07 §4]）。候选路径见 `src/app/routes.ts`：
 * `pages: ["models", "models/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * 这一页**读**用 `model_list` / `pricing_unpriced_models` / `channel_list` /
 * `provider_list` / `credential_list`；**添加模型向导**（[docs/17 §3.2]）调 `model_upsert`，
 * 成功后用**后端返回值**回显并 `invalidateQueries(qk.models)`。
 * 其余写命令（`model_delete` / `model_import_from_provider` / `model_apply_import` /
 * `model_bulk_update`）**已在后端登记**（以 `src-tauri/src/commands/mod.rs` 的 `collect_commands!`
 * 为准，`src/app/commandSurface.guard.test.ts` 机械检查），**入口尚未接线** ——
 * 在那之前空状态里如实说明，**不放假按钮、也不放点了没反应的禁用占位**。
 *
 * 三条诚实纪律：
 * 1. **"被哪些通道引用"缺数据时不许说成"没有被引用"**：`channel_list` 读失败时，
 *    这一列显示"—"并在概览里点明"这是缺数据，不是没有被引用"。
 * 2. **两个定价来源（`model_list[].priced` 与 `pricing_unpriced_models`）不一致时不裁决**，
 *    把不一致本身显示出来。
 * 3. **本地筛选要自报家门**：筛选只作用在已加载的行上，界面上必须写明"不是后端检索"。
 */

import {
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  Paper,
  Stack,
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
import type { Model } from "@/shared/bindings";
import { ConfirmDelete, HelpTip, PageHeader, StatusDot } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  EMPTY_MODEL_DRAFT,
  draftToModel,
  filterModels,
  hasDiscrepancy,
  pricingDiscrepancy,
  pricingVerdict,
  readModelRows,
  sortModels,
  summarizeModels,
  upstreamSummary,
  validateModelDraft,
  type ChannelsLookup,
  type ModelChannelRef,
  type ModelDraft,
  type ModelDraftIssue,
  type ModelRowView,
  type PricingVerdict,
} from "./model";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

// ================================================================ 删除（[docs/21 §1]）

/**
 * 「会连带删除 / 影响」清单的行文案：用 i18n 键 `delete.consequence.modelChannels`
 * （第 73 轮主管补上；在这之前这里是带注释的字面量，英文界面会露中文）。
 */

/** 表头全选复选框的无障碍标签：用 `common.selectAll`（见 `providers/index.tsx` 同一常量说明）。 */
const SELECT_ALL_LABEL_KEY: I18nKey = "common.selectAll";

/** 一次删除的目标（单条与批量共用同一个形状）。 */
type DeleteTarget = {
  readonly ids: readonly string[];
  readonly label: string;
  /** 批量**没有**级联按钮（[docs/21 §1.2]：批量一律 `force=false`）。 */
  readonly batch: boolean;
  readonly consequences: readonly string[];
};

/** 执行完的**事实**（不带文案：文案由页面用 `t(...)` 拼）。 */
type DeleteOutcome = {
  deletedIds: string[];
  failures: { id: string; message: string }[];
};

/**
 * 逐条执行删除：**一条失败不中止其余条**、**批量一律 `force=false`**（[docs/21 §1]）。
 * 与 `pages/providers/index.tsx` 的同名函数是同一套语义（同一份契约、同一套 testid 约定）。
 */
async function runDelete(
  target: DeleteTarget,
  force: boolean,
  remove: (id: string, force: boolean) => Promise<unknown>,
): Promise<DeleteOutcome> {
  const effectiveForce = target.batch ? false : force;
  const deletedIds: string[] = [];
  const failures: { id: string; message: string }[] = [];

  for (const id of target.ids) {
    try {
      await remove(id, effectiveForce);
      deletedIds.push(id);
    } catch (err) {
      failures.push({ id, message: errorMessage(err) });
    }
  }

  return { deletedIds, failures };
}

/**
 * 定价结论 → 文案。**「未知」是一等公民**：真机的 `model_list` 没有 `priced` 这一列，
 * 那时不能点一个"未定价"的黄灯 —— 那是拿不确定冒充确定。
 */
const PRICING_LABEL: Record<PricingVerdict, I18nKey> = {
  priced: "models.priced.yes",
  unpriced: "models.priced.no",
  unknown: "models.priced.unknown",
};

/** 定价结论 → 状态灯语义（灰 = 未知，与「未启用」共用同一色的弱化档）。 */
const PRICING_STATUS: Record<PricingVerdict, "ok" | "warn" | "off"> = {
  priced: "ok",
  unpriced: "warn",
  unknown: "off",
};

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary", display: "block" }}>
      {children}
    </Typography>
  );
}

/** 一个统计格：标签（text.label）+ 值（等宽数字）。 */
function StatItem({
  label,
  value,
  testId,
}: {
  label: string;
  value: ReactNode;
  testId?: string;
}) {
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

/** 分组 + 排序合成一格（两者都缺时给破折号）。 */
function groupOrderText(row: ModelRowView): string {
  const parts = [row.group, row.order === undefined ? undefined : formatInt(row.order)].filter(
    (v): v is string => v !== undefined && v !== "",
  );
  return parts.length === 0 ? DASH : parts.join(" · ");
}

/**
 * 上游映射一格。
 *
 * 只有一个上游名、而且它就等于逻辑模型名时，**不把同一个字符串再写一遍**：
 * 一页里出现两个文本完全相同的元素，用户读不出"这条通道到底有没有改名"，
 * 机械验收也无法区分"逻辑模型列"与"上游映射列"（`getByText(id)` 会命中两个）。
 * 这种情况给一句标记，原文仍留在逻辑模型列。
 */
function upstreamCellText(row: ModelRowView, sameLabel: string): string {
  const unique: string[] = [];
  for (const name of row.upstreamNames) {
    if (name !== "" && !unique.includes(name)) unique.push(name);
  }
  if (unique.length === 1 && unique[0] === row.id) return sameLabel;
  return upstreamSummary(row.upstreamNames);
}

/** 引用通道一行：`上游模型（服务商 · 凭据）`；禁用的通道加一个文字标记。 */
function channelRefLine(ref: ModelChannelRef, disabledLabel: string): string {
  const provider = ref.providerName ?? ref.providerId;
  const credential = ref.credentialLabel ?? ref.credentialId;
  return `${ref.upstreamModel}（${provider} · ${credential}）${ref.enabled ? "" : ` · ${disabledLabel}`}`;
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据/缺数据"）

export type ModelsBodyProps = {
  /** 模型清单（整形后的行）。 */
  rows: readonly ModelRowView[];
  /** 后端 `pricing_unpriced_models` 的原文 id；**`undefined` = 这一路读不到**（不宣称"全部已定价"）。 */
  unpricedIds: readonly string[] | undefined;
  /** 未定价清单读取失败的原因。 */
  unpricedError?: unknown;
  /** 通道清单读取失败的原因（此时"引用通道"这一列不可用）。 */
  channelsError?: unknown;
  /** `model_list` 自身是否还在加载（空清单时用来区分"还没读完"与"确实没有"）。 */
  isLoading: boolean;
  /** `model_list` 读取失败的原因（读不到清单时不许给"配置里还没有模型"这种结论）。 */
  error: unknown;
  onRetry: () => void;
  /**
   * 行内「删除」（[docs/21 §1]）。**省略 = 这一行没有删除入口** ——
   * 纯展示件不自己造写入口，页面（数据层）注入真处理器。
   */
  onDelete?: (row: ModelRowView) => void;
  /** 多选（行 `models-select` / 表头 `models-select-all` / 计数 `models-selected-count`）。 */
  selectedIds?: readonly string[];
  onToggleSelect?: (id: string) => void;
  /** 表头全选：把**当前筛选后看得见的那批 id** 交回数据层。 */
  onToggleSelectAll?: (visibleIds: readonly string[]) => void;
  /** 「删除选中」（`models-delete-selected`）：一次确认后**逐条**删。 */
  onDeleteSelected?: () => void;
};

export function ModelsBody({
  rows,
  unpricedIds,
  unpricedError,
  channelsError,
  isLoading,
  error,
  onRetry,
  onDelete,
  selectedIds,
  onToggleSelect,
  onToggleSelectAll,
  onDeleteSelected,
}: ModelsBodyProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");

  const ordered = useMemo(() => sortModels(rows), [rows]);
  const filtered = useMemo(() => filterModels(ordered, query), [ordered, query]);
  const summary = summarizeModels(ordered, unpricedIds);
  const discrepancy = useMemo(
    () => pricingDiscrepancy(ordered, unpricedIds),
    [ordered, unpricedIds],
  );

  const selected = selectedIds ?? [];
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  /** 多选只在数据层把两个处理器都注入时才画（否则是一排点了没用的复选框）。 */
  const selectable = onToggleSelect !== undefined && onToggleSelectAll !== undefined;
  /** 全选的"可见集合" = **本地筛选之后**的行 —— 筛选框里的东西才是用户眼里"当前这批"。 */
  const visibleIds = filtered.map((row) => row.id);
  const allSelected = visibleIds.length > 0 && visibleIds.every((id) => selectedSet.has(id));
  const someSelected = !allSelected && visibleIds.some((id) => selectedSet.has(id));

  const listBroken = error !== undefined && error !== null;
  const channelsMissing = channelsError !== undefined && channelsError !== null;

  /**
   * 「未定价」这一格：只在**有结论**时才给数。
   * 真机的 `model_list` 没有 `priced` 这一列，所以当未定价清单也读不到时，
   * 结论就是"未知"——那时画破折号，而不是画一个看起来像"没有未定价模型"的 0。
   */
  const unpricedStat =
    listBroken || (unpricedIds === undefined && summary.pricingUnknown > 0)
      ? DASH
      : summary.pricingUnknown > 0
        ? t("models.summary.unpricedUnknown", {
            value: formatInt(summary.unpriced),
            unknown: formatInt(summary.pricingUnknown),
          })
        : formatInt(summary.unpriced);

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* 1 概览：读不到的那一维一律画破折号，不画 0 */}
      <Paper variant="outlined" sx={SECTION_SX} data-testid="models-summary">
        <Stack spacing={3}>
          {/* 「引用通道 / 未定价这两个数从哪来」是**解释** → 收进标题右边的问号（[docs/22 §2]）；
              四个统计格里的**数值**照旧留在一眼可见的地方（§2 第 5 条）。 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
            <Typography variant="h4">{t("models.summary.title")}</Typography>
            <HelpTip text={t("models.summary.hint")} testId="models-summary-help" />
          </Box>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
              gap: 3,
            }}
          >
            <StatItem
              label={t("models.summary.models")}
              value={listBroken ? DASH : formatInt(summary.models)}
            />
            <StatItem
              label={t("models.summary.channels")}
              value={channelsMissing || listBroken ? DASH : formatInt(summary.channels)}
            />
            <StatItem
              label={t("models.summary.unpriced")}
              value={unpricedStat}
              testId="models-summary-unpriced"
            />
            <StatItem
              label={t("models.summary.hidden")}
              value={listBroken ? DASH : formatInt(summary.hidden)}
            />
          </Box>

          {/* 缺数据必须说出来：这一列不可用 != 这个模型没有被引用
              （后端原文 + TA 码是红线，[docs/22 §2 第 2 条] → 永远内联） */}
          {channelsMissing ? (
            <Typography
              variant="body1"
              data-testid="models-channels-error"
              sx={{ color: "warning.main" }}
            >
              {t("models.channels.error", { message: errorMessage(channelsError) })}
            </Typography>
          ) : null}
        </Stack>
      </Paper>

      {/* 2 定价：两个后端来源 + 交叉核对（只在真的有模型时才说定价的事） */}
      {ordered.length > 0 ? (
        <Paper variant="outlined" sx={SECTION_SX} data-testid="models-pricing">
          <Stack spacing={3}>
            {/* 「未定价清单来自哪条命令」是解释 → 收进标题右边的问号（[docs/22 §2]）。
                而下面那句「全部已定价。」是**空态结论**、「本页不替它们裁决」是**这一块自己的
                判定说明** → 都留在版面上（§2 第 1 条；且既有用例断言后者在 `models-mismatch` 里）。 */}
            <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
              <Typography variant="h4">{t("models.unpriced.title")}</Typography>
              <HelpTip text={t("models.unpriced.hint")} testId="models-pricing-help" />
            </Box>

            {unpricedError !== undefined && unpricedError !== null ? (
              <Typography
                variant="body1"
                data-testid="models-unpriced-error"
                sx={{ color: "error.main" }}
              >
                {t("models.unpriced.error", { message: errorMessage(unpricedError) })}
              </Typography>
            ) : unpricedIds === undefined ? (
              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                {t("common.loading")}
              </Typography>
            ) : (
              <>
                {unpricedIds.length === 0 ? (
                  <Typography variant="body1" data-testid="models-unpriced-none">
                    {t("models.unpriced.none")}
                  </Typography>
                ) : (
                  <Box
                    data-testid="models-unpriced"
                    sx={{ display: "flex", flexWrap: "wrap", gap: 2 }}
                  >
                    {unpricedIds.map((id) => (
                      <Typography
                        key={id}
                        variant="body1"
                        sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                      >
                        {id}
                      </Typography>
                    ))}
                  </Box>
                )}

                {discrepancy !== undefined && discrepancy.listedButUnknown.length > 0 ? (
                  <Typography
                    variant="body1"
                    data-testid="models-pricing-unknown"
                    sx={{ color: "text.secondary" }}
                  >
                    {t("models.unpriced.unknownIds", {
                      ids: discrepancy.listedButUnknown.join(" · "),
                    })}
                  </Typography>
                ) : null}

                {discrepancy !== undefined && hasDiscrepancy(discrepancy) ? (
                  <Box
                    data-testid="models-mismatch"
                    sx={{ display: "flex", flexDirection: "column", gap: 1 }}
                  >
                    <Typography variant="body1">{t("models.mismatch.title")}</Typography>
                    {discrepancy.pricedButListed.length > 0 ? (
                      <Caption>
                        {t("models.mismatch.pricedButListed", {
                          ids: discrepancy.pricedButListed.join(" · "),
                        })}
                      </Caption>
                    ) : null}
                    {discrepancy.unpricedButNotListed.length > 0 ? (
                      <Caption>
                        {t("models.mismatch.unpricedButNotListed", {
                          ids: discrepancy.unpricedButNotListed.join(" · "),
                        })}
                      </Caption>
                    ) : null}
                    <Caption>{t("models.mismatch.hint")}</Caption>
                  </Box>
                ) : null}
              </>
            )}
          </Stack>
        </Paper>
      ) : null}

      {/* 3 清单 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 3, flexWrap: "wrap" }}>
            <Typography variant="h4">{t("models.list.title")}</Typography>
            <Caption>{t("models.search.scope", { value: formatInt(filtered.length) })}</Caption>
          </Box>

          <TextField
            label={t("models.search")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            sx={{ maxWidth: 320 }}
          />

          {listBroken ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {/* `errorMessage()` 自带 `[TA-xxxx]`（唯一格式化出口），不吞错误码 */}
              <Typography variant="body1" data-testid="models-error" sx={{ color: "error.main" }}>
                {errorMessage(error)}
              </Typography>
              <Button variant="outlined" onClick={onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : isLoading && ordered.length === 0 ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("models.loading")}
            </Typography>
          ) : ordered.length === 0 ? (
            /* 空状态：给出原因与"为什么没有导入按钮"，不给一个假表格 */
            <Stack spacing={1} data-testid="models-empty">
              <Typography variant="body1">{t("models.empty.title")}</Typography>
              <Caption>{t("models.empty.source")}</Caption>
              <Caption>{t("models.empty.import")}</Caption>
              <Caption>{t("models.empty.howto")}</Caption>
            </Stack>
          ) : filtered.length === 0 ? (
            <Typography
              variant="body1"
              data-testid="models-filter-empty"
              sx={{ color: "text.secondary" }}
            >
              {t("models.search.empty", { value: formatInt(ordered.length) })}
            </Typography>
          ) : (
            <>
              {/* 删除选中：选中数 + 「删除选中」（[docs/21 §1.2]） */}
              {onDeleteSelected === undefined ? null : (
                <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                  <Typography variant="caption" data-testid="models-selected-count">
                    {t("delete.selectedCount", { count: selected.length })}
                  </Typography>
                  <Button
                    variant="outlined"
                    color="error"
                    data-testid="models-delete-selected"
                    disabled={selected.length === 0}
                    onClick={onDeleteSelected}
                  >
                    {t("common.deleteSelected")}
                  </Button>
                </Stack>
              )}

              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="models-table">
                  <TableHead>
                    <TableRow>
                      {selectable ? (
                        <TableCell padding="checkbox">
                          <Checkbox
                            data-testid="models-select-all"
                            checked={allSelected}
                            indeterminate={someSelected}
                            onChange={() => onToggleSelectAll?.(visibleIds)}
                            slotProps={{ input: { "aria-label": t(SELECT_ALL_LABEL_KEY) } }}
                          />
                        </TableCell>
                      ) : null}
                      <TableCell>{t("models.col.expose")}</TableCell>
                      <TableCell>{t("models.col.model")}</TableCell>
                      <TableCell>{t("models.col.aliases")}</TableCell>
                      <TableCell>{t("models.col.group")}</TableCell>
                      <TableCell>{t("models.col.upstream")}</TableCell>
                      <TableCell>{t("models.col.channels")}</TableCell>
                      <TableCell>{t("models.col.priced")}</TableCell>
                      {/* 操作列表头留空（同服务商页）：不为一个表头去造 i18n 键 */}
                      {onDelete === undefined ? null : <TableCell align="right" />}
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {filtered.map((row) => {
                      // 定价结论：**未定价清单（定价引擎自己的）> 后端显式 priced > 未知**
                      const verdict = pricingVerdict(row, unpricedIds);
                      return (
                        <TableRow key={row.id} hover data-testid="models-row">
                        {selectable ? (
                          <TableCell padding="checkbox">
                            <Checkbox
                              data-testid="models-select"
                              checked={selectedSet.has(row.id)}
                              onChange={() => onToggleSelect?.(row.id)}
                              slotProps={{
                                // 无障碍标签用这一行自己的模型 id（用户数据，不需要新键）
                                input: { "aria-label": row.id },
                              }}
                            />
                          </TableCell>
                        ) : null}
                        <TableCell>
                        <StatusDot
                          status={row.expose ? "ok" : "off"}
                          label={row.expose ? t("models.expose.on") : t("models.expose.off")}
                        />
                      </TableCell>
                      <TableCell
                        data-testid="models-id"
                        sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                      >
                        {row.id}
                      </TableCell>
                      <TableCell sx={{ color: "text.secondary" }}>
                        {row.aliases.length === 0 ? DASH : row.aliases.join(" · ")}
                      </TableCell>
                      <TableCell sx={{ color: "text.secondary" }}>{groupOrderText(row)}</TableCell>
                      <TableCell
                        sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                      >
                        {upstreamCellText(row, t("models.upstream.same"))}
                      </TableCell>
                      <TableCell sx={{ minWidth: 240 }}>
                        {channelsMissing ? (
                          <Typography variant="body1" sx={{ color: "text.secondary" }}>
                            {DASH}
                          </Typography>
                        ) : (
                          <Stack spacing={0.5}>
                            {/* 后端自报 0 条通道 = 这个模型发不出去（这是结论，不是缺数据）。
                                即便通道清单里还匹配得到条目，两者的不一致由下一行点出，两个事实都不藏。 */}
                            {row.reportedChannels === 0 ? (
                              <Typography
                                variant="caption"
                                data-testid="models-channels-none"
                                sx={{ color: "text.secondary" }}
                              >
                                {t("models.channels.none")}
                              </Typography>
                            ) : null}
                            {/* 两个数对不上：如实点出，不挑一个信 */}
                            {row.channels.length === 0 && row.reportedChannels > 0 ? (
                              <Typography
                                variant="caption"
                                data-testid="models-channels-mismatch"
                                sx={{ color: "warning.main" }}
                              >
                                {t("models.channels.mismatch", { value: formatInt(row.reportedChannels) })}
                              </Typography>
                            ) : null}
                            {row.channels.map((ref) => (
                              <Typography
                                key={ref.channelId}
                                data-testid={`models-channels-${ref.channelId}`}
                                variant="caption"
                                sx={{
                                  fontFamily: typography.fontMono,
                                  lineHeight: typography.lineHeight.table,
                                  color: ref.enabled ? "text.primary" : "text.secondary",
                                  overflowWrap: "anywhere",
                                }}
                              >
                                {channelRefLine(ref, t("models.channels.disabled"))}
                              </Typography>
                            ))}
                          </Stack>
                        )}
                      </TableCell>
                      <TableCell>
                        <StatusDot
                          status={PRICING_STATUS[verdict]}
                          label={t(PRICING_LABEL[verdict])}
                        />
                        {verdict === "unknown" ? (
                          <Caption>{t("models.priced.unknownHint")}</Caption>
                        ) : null}
                        {row.costHint !== undefined ? (
                          <Caption>{t("models.priced.costHint", { hint: row.costHint })}</Caption>
                        ) : null}
                      </TableCell>
                      {onDelete === undefined ? null : (
                        <TableCell align="right">
                          <Button
                            size="small"
                            variant="outlined"
                            color="error"
                            data-testid="models-delete"
                            onClick={() => onDelete(row)}
                          >
                            {t("common.delete")}
                          </Button>
                        </TableCell>
                      )}
                    </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </Box>
            </>
          )}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function ModelsPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const models = useCommandQuery("model_list", undefined, qk.models);
  const channels = useCommandQuery("channel_list", undefined, qk.channels);
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const unpriced = useCommandQuery("pricing_unpriced_models", undefined, qk.unpricedModels);

  const upsert = useCommandMutation("model_upsert");
  const [addOpen, setAddOpen] = useState(false);
  const [draft, setDraft] = useState<ModelDraft>(EMPTY_MODEL_DRAFT);
  const [issues, setIssues] = useState<readonly ModelDraftIssue[]>([]);
  /** 成功后回显的是**后端返回的那个 Model**，不是我们提交的草稿。 */
  const [added, setAdded] = useState<Model | undefined>(undefined);

  const lookup = useMemo<ChannelsLookup>(
    () => ({
      channels: channels.data ?? [],
      providers: providers.data ?? [],
      credentials: credentials.data ?? [],
    }),
    [channels.data, providers.data, credentials.data],
  );

  const rows = useMemo(() => readModelRows(models.data ?? [], lookup), [models.data, lookup]);

  const existingIds = useMemo(() => (models.data ?? []).map((m) => m.model.id), [models.data]);

  const refresh = () => {
    void models.refetch();
    void channels.refetch();
    void providers.refetch();
    void credentials.refetch();
    void unpriced.refetch();
  };

  // ---------------------------------------------------------------- 删除（[docs/21 §1]）

  const removeModel = useCommandMutation("model_delete");

  /** 多选（按 id 存，顺序 = 勾选顺序）。 */
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  /** 确认框里那一条（`undefined` = 框没开）。 */
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | undefined>(undefined);
  /** 后端原文（被引用时的错误）：有它才在对话框里显示、也才有级联按钮。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  /** 结果区：每一次事实一行。 */
  const [deleteResult, setDeleteResult] = useState<readonly string[]>([]);
  /** 批量进行中（`mutateAsync` 逐条之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);

  /** 删除成功后要失效的查询（[docs/21 §1.1 第 4 条]）：模型级联会动到通道与通道组。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    for (const key of [qk.models, qk.credentials, qk.providers, qk.channelMatrix]) {
      await qc.invalidateQueries({ queryKey: key });
    }
  };

  /**
   * 「会连带删除 / 影响」清单：模型级联删的是挂在它下面的通道。
   * 通道清单**读失败时一个字都不写** —— 那一列本来就已经是"未知"（[docs/21 §1.1 第 3 条]：
   * 算不出来不编）；就算读到了，0 条也不写成一句多余的话。
   */
  const consequencesOf = (row: ModelRowView): string[] => {
    if (channels.error !== null && channels.error !== undefined) return [];
    return row.channels.length === 0
      ? []
      : [t("delete.consequence.modelChannels", { value: formatInt(row.channels.length) })];
  };

  const openDelete = (target: DeleteTarget): void => {    setDeleteTarget(target);
    setDeleteError(undefined);
    // 新一次操作开始 → 清掉上一次的汇报（结果区永远只讲刚刚发生的事）
    setDeleteResult([]);
  };

  const confirmDelete = async (force: boolean): Promise<void> => {
    const target = deleteTarget;
    if (target === undefined) return;
    if (target.batch) setBatchRunning(true);
    const outcome = await runDelete(target, force, (id, effectiveForce) =>
      removeModel.mutateAsync({ id, force: effectiveForce }),
    );
    setBatchRunning(false);
    const singleFailed = !target.batch && outcome.failures.length > 0;
    setDeleteResult(
      target.batch
        ? [
            t("delete.result.summary", {
              ok: formatInt(outcome.deletedIds.length),
              failed: formatInt(outcome.failures.length),
            }),
            ...outcome.deletedIds.map((id) => t("delete.result.ok", { name: id })),
            ...outcome.failures.map((failure) =>
              t("delete.result.failed", { message: `${failure.id} · ${failure.message}` }),
            ),
          ]
        : singleFailed
          ? []
          : outcome.deletedIds.map((id) => t("delete.result.ok", { name: id })),
    );
    setDeleteError(singleFailed ? outcome.failures[0]!.message : undefined);
    // 只有真的删掉的才从选中里摘掉；失败的留着
    setSelectedIds((prev) => prev.filter((id) => !outcome.deletedIds.includes(id)));
    // 单条失败时留着框（后端原文与级联按钮就在那里）；其余情况收框
    if (target.batch || !singleFailed) setDeleteTarget(undefined);
    await invalidateAfterDelete();
  };

  const toggleSelect = (id: string): void => {
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((selectedId) => selectedId !== id) : [...prev, id],
    );
  };

  /** 表头全选：只对**本地筛选后看得见**的那些行生效（可见集合由展示层交回来）。 */
  const toggleSelectAll = (visible: readonly string[]): void => {
    setSelectedIds((prev) =>
      visible.length > 0 && visible.every((id) => prev.includes(id))
        ? prev.filter((id) => !visible.includes(id))
        : [...new Set([...prev, ...visible])],
    );
  };

  const submitAdd = () => {
    const found = validateModelDraft(draft, existingIds);
    setIssues(found);
    // 本地预检不过 → **一次都不发**（与设置页 config_validate 的纪律一致）
    if (found.length > 0) return;
    upsert.mutate(
      { model: draftToModel(draft) },
      {
        onSuccess: (saved) => {
          setAdded(saved);
          setDraft(EMPTY_MODEL_DRAFT);
          setIssues([]);
          void qc.invalidateQueries({ queryKey: qk.models });
        },
      },
    );
  };

  return (
    <>
      <PageHeader
        title={t("nav.models")}
        description={t("models.description")}
        docRef="07 §4 模型目录"
        actions={
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            <Button variant="outlined" onClick={refresh}>
              {t("common.refresh")}
            </Button>
            <Button
              variant="contained"
              data-testid="models-add-open"
              onClick={() => {
                setAdded(undefined);
                setIssues([]);
                setAddOpen(true);
              }}
            >
              {t("models.add.open")}
            </Button>
          </Box>
        }
      />
      <ModelsBody
        rows={rows}
        unpricedIds={unpriced.data}
        unpricedError={unpriced.error ?? undefined}
        channelsError={channels.error ?? undefined}
        isLoading={models.isPending}
        error={models.error ?? undefined}
        onRetry={refresh}
        onDelete={(row) =>
          openDelete({ ids: [row.id], label: row.id, batch: false, consequences: consequencesOf(row) })
        }
        selectedIds={selectedIds}
        onToggleSelect={toggleSelect}
        onToggleSelectAll={toggleSelectAll}
        onDeleteSelected={() => {
          const chosen = rows.filter((row) => selectedIds.includes(row.id));
          if (chosen.length === 0) return;
          openDelete({
            ids: chosen.map((row) => row.id),
            // 确认框里写清删的是哪几条（模型的名字就是 id）
            label: chosen.map((row) => row.id).join("、"),
            batch: true,
            // 批量的连带清单 = 每一条各自的清单（算不出来就是空）
            consequences: chosen.flatMap((row) =>
              consequencesOf(row).map((line) => `${row.id}：${line}`),
            ),
          });
        }}
      />

      {/* 删除结果区（[docs/21 §3] 的 `models-delete-result`）：只写真的发生过的事 */}
      {deleteResult.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="models-delete-result">
              {deleteResult.map((line, index) => (
                <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}

      {/*
        删除确认（[docs/21 §1 / §2]）：**先确认再发命令**；被引用时后端原文原样显示、
        此时才出现级联按钮；批量一律 `force=false`，所以批量框里没有级联按钮。
      */}
      <ConfirmDelete
        open={deleteTarget !== undefined}
        testIdPrefix="models"
        title={t("delete.title.model")}
        targetLabel={deleteTarget?.label ?? ""}
        consequences={deleteTarget?.consequences}
        cascadeAvailable={deleteTarget !== undefined && !deleteTarget.batch}
        pending={removeModel.isPending || batchRunning}
        errorText={deleteError}
        onConfirm={() => void confirmDelete(false)}
        onConfirmCascade={() => void confirmDelete(true)}
        onCancel={() => {
          setDeleteTarget(undefined);
          setDeleteError(undefined);
        }}
      />

      <Dialog open={addOpen} onClose={() => setAddOpen(false)} fullWidth maxWidth="sm">
        <DialogTitle>{t("models.add.title")}</DialogTitle>
        <DialogContent>
          <Stack spacing={2} sx={{ pt: 1 }}>
            <TextField
              label={t("models.add.id")}
              value={draft.id}
              onChange={(e) => setDraft({ ...draft, id: e.target.value })}
              {...{ "data-testid": "models-add-id" }}
              helperText={t("models.add.id.hint")}
              fullWidth
            />
            <TextField
              label={t("models.add.aliases")}
              value={draft.aliases}
              onChange={(e) => setDraft({ ...draft, aliases: e.target.value })}
              {...{ "data-testid": "models-add-aliases" }}
              helperText={t("models.add.aliases.hint")}
              fullWidth
            />
            <TextField
              label={t("models.add.group")}
              value={draft.group}
              onChange={(e) => setDraft({ ...draft, group: e.target.value })}
              {...{ "data-testid": "models-add-group" }}
              helperText={t("models.add.group.hint")}
              fullWidth
            />
            <TextField
              label={t("models.add.order")}
              value={draft.order}
              onChange={(e) => setDraft({ ...draft, order: e.target.value })}
              {...{ "data-testid": "models-add-order", inputMode: "numeric" }}
              helperText={t("models.add.order.hint")}
              fullWidth
            />
            <FormControlLabel
              control={
                <Checkbox
                  checked={draft.expose}
                  onChange={(e) => setDraft({ ...draft, expose: e.target.checked })}
                  {...{ "data-testid": "models-add-expose" }}
                />
              }
              label={t("models.add.expose")}
            />
            <Typography variant="caption" sx={{ color: "text.secondary" }}>
              {t("models.add.expose.hint")}
            </Typography>

            {/* 本地预检的错，按字段逐条显示（不解析文案，按机器可读的 reason 取 key） */}
            {issues.map((issue) => (
              <Typography
                key={`${issue.field}-${issue.reason}`}
                variant="body1"
                data-testid={`models-add-issue-${issue.field}`}
                sx={{ color: "error.main" }}
              >
                {t(`models.add.issue.${issue.field}.${issue.reason}` as I18nKey)}
              </Typography>
            ))}

            {/* 后端的错原样显示（TA 码 + 文案），本地预检不替代它 */}
            {upsert.error !== null ? (
              <Typography variant="body1" data-testid="models-add-error" sx={{ color: "error.main" }}>
                {errorMessage(upsert.error)}
              </Typography>
            ) : null}

            {/* 成功：显示**后端返回值**（不是草稿） */}
            {added !== undefined ? (
              <Typography variant="body1" data-testid="models-add-result">
                {t("models.add.result", {
                  id: added.id,
                  expose: added.expose ? t("models.add.result.expose") : t("models.add.result.hidden"),
                })}
              </Typography>
            ) : null}
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setAddOpen(false)}>{t("models.add.close")}</Button>
          <Button
            variant="contained"
            data-testid="models-add-submit"
            disabled={upsert.isPending}
            onClick={submitAdd}
          >
            {t("models.add.submit")}
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}

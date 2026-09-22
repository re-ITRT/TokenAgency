/**
 * 模型目录页 `/models`（[07 §4]）。候选路径见 `src/app/routes.ts`：
 * `pages: ["models", "models/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * 这一页只做**读**：
 * - 命令面只用已注册的五条：`model_list` / `pricing_unpriced_models` / `channel_list` /
 *   `provider_list` / `credential_list`（`src-tauri/src/commands/mod.rs` 的 `collect_commands!`）。
 * - `model_upsert` / `model_delete` / `model_import_from_provider` / `model_apply_import` /
 *   `model_bulk_update` **都还没注册** -> **不放按钮、也不放禁用占位**：空状态里直接说明
 *   "导入入口还没有接线"，而不是给一个点了没反应的按钮。
 *
 * 三条诚实纪律：
 * 1. **"被哪些通道引用"缺数据时不许说成"没有被引用"**：`channel_list` 读失败时，
 *    这一列显示"—"并在概览里点明"这是缺数据，不是没有被引用"。
 * 2. **两个定价来源（`model_list[].priced` 与 `pricing_unpriced_models`）不一致时不裁决**，
 *    把不一致本身显示出来。
 * 3. **本地筛选要自报家门**：筛选只作用在已加载的行上，界面上必须写明"不是后端检索"。
 */

import { Box, Button, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, TextField, Typography } from "@mui/material";
import { useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader, StatusDot } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  filterModels,
  hasDiscrepancy,
  pricingDiscrepancy,
  pricingVerdict,
  readModelRows,
  sortModels,
  summarizeModels,
  upstreamSummary,
  type ChannelsLookup,
  type ModelChannelRef,
  type ModelRowView,
  type PricingVerdict,
} from "./model";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

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
};

export function ModelsBody({
  rows,
  unpricedIds,
  unpricedError,
  channelsError,
  isLoading,
  error,
  onRetry,
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
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Typography variant="h4">{t("models.summary.title")}</Typography>

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

          <Caption>{t("models.summary.hint")}</Caption>

          {/* 缺数据必须说出来：这一列不可用 != 这个模型没有被引用 */}
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
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3}>
            <Typography variant="h4">{t("models.unpriced.title")}</Typography>

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

                <Caption>{t("models.unpriced.hint")}</Caption>

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
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" data-testid="models-table">
                <TableHead>
                  <TableRow>
                    <TableCell>{t("models.col.expose")}</TableCell>
                    <TableCell>{t("models.col.model")}</TableCell>
                    <TableCell>{t("models.col.aliases")}</TableCell>
                    <TableCell>{t("models.col.group")}</TableCell>
                    <TableCell>{t("models.col.upstream")}</TableCell>
                    <TableCell>{t("models.col.channels")}</TableCell>
                    <TableCell>{t("models.col.priced")}</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {filtered.map((row) => {
                    // 定价结论：**未定价清单（定价引擎自己的）> 后端显式 priced > 未知**
                    const verdict = pricingVerdict(row, unpricedIds);
                    return (
                      <TableRow key={row.id} hover data-testid="models-row">
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
                    </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </Box>
          )}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function ModelsPage() {
  const { t } = useTranslation();

  const models = useCommandQuery("model_list", undefined, qk.models);
  const channels = useCommandQuery("channel_list", undefined, qk.channels);
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const unpriced = useCommandQuery("pricing_unpriced_models", undefined, qk.unpricedModels);

  const lookup = useMemo<ChannelsLookup>(
    () => ({
      channels: channels.data ?? [],
      providers: providers.data ?? [],
      credentials: credentials.data ?? [],
    }),
    [channels.data, providers.data, credentials.data],
  );

  const rows = useMemo(() => readModelRows(models.data ?? [], lookup), [models.data, lookup]);

  const refresh = () => {
    void models.refetch();
    void channels.refetch();
    void providers.refetch();
    void credentials.refetch();
    void unpriced.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.models")}
        description={t("models.description")}
        docRef="07 §4 模型目录"
        actions={
          <Button variant="outlined" onClick={refresh}>
            {t("common.refresh")}
          </Button>
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
      />
    </>
  );
}

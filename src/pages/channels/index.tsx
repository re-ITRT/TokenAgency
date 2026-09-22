/**
 * 套餐与线路页 `/channels`（[07 §5] / [05 §2.1]）。候选路径见 `src/app/routes.ts`：
 * `pages: ["channels", "channels/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * 这一页本轮**只做读**（列表 + 按服务商分组）：
 * - 命令面只用已注册的六条：`channel_list` / `provider_list` / `credential_list` / `lane_list` /
 *   `group_list` / `quota_limit_gaps`（`src-tauri/src/commands/mod.rs` 的 `collect_commands!`）。
 * - **矩阵视图（批量配置）依赖 `channel_matrix`，它还没注册** -> 这一块只说明现状：
 *   没有批量改线路 / 批量启停的入口，**也不放禁用占位**。同理
 *   `channel_upsert` / `channel_set_enabled` / `channel_bulk_assign_lane` / `channel_set_tags`
 *   / `channel_delete` 都没注册 -> 空状态里直接说明"新建入口还没有接线"。
 *
 * 三条诚实纪律（与 `./model.ts` 的文件头一致）：
 * 1. **线路继承只认读得到的层级**：通道级 > 凭据级，再往上显示「继承」并说明原因；
 * 2. **占用比只在口径对得上时才算**，否则显示「未设上限」而不是 0%；
 * 3. **读不到的维度画破折号**，不画 0、也不编一句"一切正常"。
 */

import { Box, Button, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { useMemo, type ReactNode } from "react";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import type { QuotaMetric, QuotaStateValue, QuotaWindow } from "@/shared/bindings";
import { LimitGapNotice, PageHeader, QuotaBar, StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatMoney, formatTokens } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  credentialLabelFor,
  groupByProvider,
  readChannelRows,
  summarizeChannels,
  type ChannelRowContext,
  type ChannelRowView,
  type LaneResolution,
  type QuotaLimitGap,
  type QuotaParts,
} from "./model";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

/**
 * 未配额度提醒的 query key。
 *
 * **为什么不是 `qk.xxx`**：`shared/api/keys.ts` 里还没有 `quota_limit_gaps` 这一条
 * （那个文件属于 F1 的写作用域，本页不改）。这里**不能**复用 `qk.credentials` ——
 * 同一个 key 配两个不同 queryFn 会互相串缓存（TanStack Query 按 key 去重），
 * 那是"看起来有数据、其实是另一个命令的返回"这类最难查的错。
 * 用 `["channels", …]` 前缀：`qk.channels` 的事件失效（`channel:state` / `config:changed`）
 * 会连带失效它。
 */
const LIMIT_GAPS_KEY = ["channels", "limit-gaps"] as const;

/** 额度口径与窗口的显示名（枚举取值来自后端，这里只是翻译，不改写）。 */
const METRIC_LABEL: Record<QuotaMetric, I18nKey> = {
  tokens: "channels.metric.tokens",
  requests: "channels.metric.requests",
  cost: "channels.metric.cost",
  unknown: "channels.metric.unknown",
};

const WINDOW_LABEL: Record<QuotaWindow, I18nKey> = {
  hour: "channels.window.hour",
  day: "channels.window.day",
  month: "channels.window.month",
  none: "channels.window.none",
};

const PROVIDER_STATUS: Record<string, SemStatus> = {
  ok: "ok",
  warn: "warn",
  error: "error",
  off: "off",
};

/** 状态灯的文案（复用共享组件那套键，不另写一份）。 */
const STATUS_LABEL: Record<SemStatus, I18nKey> = {
  ok: "component.statusDot.ok",
  warn: "component.statusDot.warn",
  error: "component.statusDot.error",
  info: "component.statusDot.info",
  off: "component.statusDot.off",
};

// ================================================================ 小组件与格式化

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary", display: "block" }}>
      {children}
    </Typography>
  );
}

function StatItem({ label, value }: { label: string; value: ReactNode }) {
  return (
    <Box sx={{ minWidth: 0 }}>
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

/** 线路一格的名字：名字读不到就退回线路 id，再读不到就是破折号。 */
function laneName(lane: LaneResolution): string {
  return lane.name ?? lane.laneId ?? DASH;
}

/**
 * 额度明细：数字按口径选格式化函数（金额 vs 计数）。
 * 口径与窗口的**文案由调用方翻译好传进来** —— 翻译函数的重载类型不适合当参数传。
 */
function quotaDetail(parts: QuotaParts, metricLabel: string, windowLabel: string): string {
  const amount = (value: number): string =>
    parts.metric === "cost"
      ? formatMoney({ amount: value, currency: parts.currency })
      : formatTokens(value);

  const numbers =
    parts.used !== undefined
      ? parts.limit !== undefined
        ? `${amount(parts.used)} / ${amount(parts.limit)}`
        : amount(parts.used)
      : parts.limit !== undefined
        ? `${DASH} / ${amount(parts.limit)}`
        : DASH;

  return `${numbers} · ${metricLabel} · ${windowLabel}`;
}

/**
 * 额度一格。
 *
 * 三种情况必须分开画，**因为它们说的是三件不同的事**：
 * 1. 有占用比（后端给的，或本页按口径算的）→ 进度条 + 明细；
 * 2. **没有用量记录**（真机 `quota_state` 缺失）→ 只展示上限 + 明说"用量没有记录"，
 *    **绝不画"未设上限"**（缺的是用量，不是上限）；
 * 3. 上限本身没配 / 口径对不上 → 明确说出来。
 */
function QuotaCell({
  parts,
  state,
}: {
  parts: QuotaParts | undefined;
  state: QuotaStateValue | undefined;
}) {
  const { t } = useTranslation();

  if (parts === undefined) {
    return (
      <Typography variant="body1" sx={{ color: "text.secondary" }}>
        {DASH}
      </Typography>
    );
  }

  const detail = quotaDetail(parts, t(METRIC_LABEL[parts.metric]), t(WINDOW_LABEL[parts.window]));

  if (parts.usedPct === null && parts.usageMissing) {
    return (
      <>
        <Typography
          variant="body1"
          sx={{ fontFamily: typography.fontMono, fontVariantNumeric: "tabular-nums" }}
        >
          {detail}
        </Typography>
        <Caption>{t("channels.quota.usageMissing")}</Caption>
      </>
    );
  }

  return (
    <>
      <QuotaBar usedPct={parts.usedPct} state={state} detail={detail} width={160} />
      {parts.usedPct === null ? (
        <Caption>
          {parts.limit === undefined ? t("channels.quota.noLimit") : t("channels.quota.metricUnknown")}
        </Caption>
      ) : parts.usedPctSource === "derived" ? (
        /* 数字是哪来的要说清楚：后端这次没给 used_pct，是本页算的 */
        <Caption>{t("channels.quota.derived")}</Caption>
      ) : null}
    </>
  );
}

/** 通道组一格：属于哪些组 + 选择器（没有组就没有话）。 */
function groupText(row: ChannelRowView, noneLabel: string): string {
  if (row.groups.length === 0) return noneLabel;
  return row.groups.map((g) => `${g.id}（${g.selector}）`).join(" · ");
}

// ================================================================ 展示层（纯 props）

export type ChannelsBodyProps = {
  rows: readonly ChannelRowView[];
  /** 后端 `quota_limit_gaps` 的原文提醒（判定与文案都来自后端，本页一个字不改）。 */
  limitGaps: readonly QuotaLimitGap[];
  /** 未配额度提醒读取失败的原因（读不到就如实说，不当作"没有这条提醒"）。 */
  limitGapsError?: unknown;
  /** `channel_list` 自身是否还在加载。 */
  isLoading: boolean;
  /** `channel_list` 读取失败的原因。 */
  error: unknown;
  onRetry: () => void;
};

export function ChannelsBody({
  rows,
  limitGaps,
  limitGapsError,
  isLoading,
  error,
  onRetry,
}: ChannelsBodyProps) {
  const { t } = useTranslation();

  const summary = useMemo(() => summarizeChannels(rows), [rows]);
  const grouped = useMemo(() => groupByProvider(rows), [rows]);
  const anyInherited = rows.some((r) => r.lane.source === "inherited");

  const listBroken = error !== undefined && error !== null;
  const gapsBroken = limitGapsError !== undefined && limitGapsError !== null;
  /** 有服务商没有健康状态这一列（真机现状）→ 说清楚"为什么不点状态灯"。 */
  const anyStatusMissing = grouped.some((g) => g.status === undefined);

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* 1 概览：读不到的那一维画破折号，不画 0 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Typography variant="h4">{t("channels.summary.title")}</Typography>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
              gap: 3,
            }}
          >
            <StatItem
              label={t("channels.summary.channels")}
              value={listBroken ? DASH : formatInt(summary.total)}
            />
            <StatItem
              label={t("channels.summary.enabled")}
              value={listBroken ? DASH : formatInt(summary.enabled)}
            />
            <StatItem
              label={t("channels.summary.disabled")}
              value={listBroken ? DASH : formatInt(summary.disabled)}
            />
            <StatItem
              label={t("channels.summary.providers")}
              value={listBroken ? DASH : formatInt(summary.providers)}
            />
            <StatItem
              label={t("channels.summary.limitGaps")}
              value={gapsBroken ? DASH : formatInt(summary.limitGapCredentials)}
            />
          </Box>

          <Caption>{t("channels.summary.hint")}</Caption>
        </Stack>
      </Paper>

      {/* 2 矩阵：没实现就说明现状，不放任何点了没反应的按钮 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={2}>
          <Typography variant="h4">{t("channels.matrix.title")}</Typography>
          <Typography variant="body1" data-testid="channels-matrix-absent" sx={{ color: "text.secondary" }}>
            {t("channels.matrix.absent")}
          </Typography>
        </Stack>
      </Paper>

      {/* 3 未配可用额度的凭据：复用共享的 LimitGapNotice（三处同一句话，不各写一份） */}
      {limitGaps.length > 0 || gapsBroken ? (
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3}>
            <Typography variant="h4">{t("channels.limitGaps.title")}</Typography>
            <Caption>{t("channels.limitGaps.hint")}</Caption>

            {gapsBroken ? (
              <Typography
                variant="body1"
                data-testid="channels-limit-gaps-error"
                sx={{ color: "error.main" }}
              >
                {t("channels.limitGaps.error", { message: errorMessage(limitGapsError) })}
              </Typography>
            ) : (
              <Stack spacing={3} data-testid="channels-limit-gaps">
                {limitGaps.map((gap) => (
                  <Box
                    key={gap.credential_id}
                    sx={{ display: "flex", flexDirection: "column", gap: 1 }}
                  >
                    <Typography variant="body1" sx={{ overflowWrap: "anywhere" }}>
                      {credentialLabelFor(gap.credential_id, rows)}
                    </Typography>
                    <LimitGapNotice message={gap.message} />
                  </Box>
                ))}
              </Stack>
            )}
          </Stack>
        </Paper>
      ) : null}

      {/* 4 按服务商分组的列表 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={4}>
          <Typography variant="h4">{t("channels.groups.title")}</Typography>

          {anyInherited ? <Caption>{t("channels.lane.hint")}</Caption> : null}
          {anyStatusMissing ? <Caption>{t("channels.provider.statusAbsent")}</Caption> : null}

          {listBroken ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="channels-error" sx={{ color: "error.main" }}>
                {errorMessage(error)}
              </Typography>
              <Button variant="outlined" onClick={onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : isLoading && rows.length === 0 ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("channels.loading")}
            </Typography>
          ) : rows.length === 0 ? (
            /* 空状态：说明通道是什么、新增入口为什么不在，不给假表格 */
            <Stack spacing={1} data-testid="channels-empty">
              <Typography variant="body1">{t("channels.empty.title")}</Typography>
              <Caption>{t("channels.empty.what")}</Caption>
              <Caption>{t("channels.empty.create")}</Caption>
            </Stack>
          ) : (
            <Stack spacing={6}>
              {grouped.map((group) => (
                <Box key={group.providerId} data-testid="channels-group">
                  <Box
                    sx={{ display: "flex", alignItems: "center", gap: 3, mb: 2, minWidth: 0 }}
                    data-testid="channels-group-header"
                  >
                    <Typography variant="body1" noWrap sx={{ minWidth: 0 }}>
                      {group.providerName ?? group.providerId}
                    </Typography>
                    {/* 契约的 ProviderView 有 status；真机的 provider_list 直接返回 Provider 配置实体，
                        **没有这一列** —— 那时只陈述配置事实，不点状态灯（[15 §3.2] 颜色只表达状态）。 */}
                    {group.status !== undefined ? (
                      <StatusDot
                        status={PROVIDER_STATUS[group.status] ?? "off"}
                        label={t(STATUS_LABEL[PROVIDER_STATUS[group.status] ?? "off"])}
                      />
                    ) : (
                      <Caption>
                        {group.enabled === false
                          ? t("channels.provider.disabled")
                          : group.enabled === true
                            ? t("channels.provider.enabled")
                            : t("common.unknown")}
                      </Caption>
                    )}
                    <Caption>
                      {t("channels.group.count", { value: formatInt(group.rows.length) })}
                    </Caption>
                  </Box>

                  <Box sx={{ overflowX: "auto" }}>
                    <Table size="small" data-testid="channels-table">
                      <TableHead>
                        <TableRow>
                          <TableCell>{t("channels.col.status")}</TableCell>
                          <TableCell>{t("channels.col.channel")}</TableCell>
                          <TableCell>{t("channels.col.model")}</TableCell>
                          <TableCell>{t("channels.col.upstream")}</TableCell>
                          <TableCell>{t("channels.col.credential")}</TableCell>
                          <TableCell>{t("channels.col.lane")}</TableCell>
                          <TableCell>{t("channels.col.quota")}</TableCell>
                          <TableCell align="right">{t("channels.col.priorityWeight")}</TableCell>
                          <TableCell>{t("channels.col.tags")}</TableCell>
                          <TableCell>{t("channels.col.group")}</TableCell>
                        </TableRow>
                      </TableHead>
                      <TableBody>
                        {group.rows.map((row) => (
                          <TableRow key={row.id} hover data-testid="channels-row">
                            <TableCell data-testid="channels-status">
                              <StatusDot
                                status={row.enabled ? "ok" : "off"}
                                label={row.enabled ? t("channels.status.enabled") : t("channels.status.disabled")}
                              />
                            </TableCell>
                            <TableCell
                              data-testid="channels-id"
                              sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                            >
                              {row.id}
                            </TableCell>
                            <TableCell
                              data-testid="channels-model"
                              sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                            >
                              {row.model}
                            </TableCell>
                            <TableCell
                              data-testid="channels-upstream"
                              sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                            >
                              {row.upstreamModel}
                            </TableCell>
                            <TableCell data-testid="channels-credential" sx={{ minWidth: 160 }}>
                              {row.credentialLabel ?? (
                                <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                                  {row.credentialId}
                                </Typography>
                              )}
                              <Caption>
                                {t("channels.credential.provider", {
                                  name: row.providerName ?? row.providerId,
                                })}
                              </Caption>
                              {row.credentialLabel === undefined ? (
                                <Caption>
                                  {t("channels.credential.missing", { id: row.credentialId })}
                                </Caption>
                              ) : null}
                            </TableCell>
                            <TableCell sx={{ minWidth: 160 }}>
                              {/* testid 挂在文字这一层：单元格里还会追加"最近样本"，两者别混在一个文本里 */}
                              <Box data-testid="channels-lane">
                                {row.lane.source === "inherited"
                                  ? t("channels.lane.inherited")
                                  : row.lane.source === "credential"
                                    ? t("channels.lane.fromCredential", { name: laneName(row.lane) })
                                    : laneName(row.lane)}
                              </Box>
                              {/* 健康证据：契约形状的 current_node/latency_ms，或真机 lane_list 的 latest */}
                              {row.lane.sample !== undefined ? (
                                <Caption>
                                  {t("channels.lane.sample", {
                                    node: row.lane.sample.node ?? t("common.unknown"),
                                    latency: formatLatency(row.lane.sample.latencyMs),
                                  })}
                                </Caption>
                              ) : null}
                            </TableCell>
                            <TableCell data-testid="channels-quota" sx={{ minWidth: 160 }}>
                              <QuotaCell parts={row.quotaParts} state={row.quotaState} />
                            </TableCell>
                            <TableCell align="right">
                              <Box
                                data-testid="channels-priority"
                                sx={{ fontVariantNumeric: "tabular-nums" }}
                              >
                                {formatInt(row.priority)}
                              </Box>
                              <Box
                                data-testid="channels-weight"
                                sx={{ fontVariantNumeric: "tabular-nums", color: "text.secondary" }}
                              >
                                {formatInt(row.weight)}
                              </Box>
                            </TableCell>
                            <TableCell
                              data-testid="channels-tags"
                              sx={{ color: "text.secondary", overflowWrap: "anywhere" }}
                            >
                              {row.tags.length === 0 ? t("channels.tags.none") : row.tags.join(" · ")}
                            </TableCell>
                            <TableCell
                              data-testid="channels-group-cell"
                              sx={{ color: "text.secondary", overflowWrap: "anywhere" }}
                            >
                              {groupText(row, t("channels.group.none"))}
                            </TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </Box>
                </Box>
              ))}
            </Stack>
          )}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function ChannelsPage() {
  const { t } = useTranslation();

  const channels = useCommandQuery("channel_list", undefined, qk.channels);
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  const groups = useCommandQuery("group_list", undefined, qk.groups);
  const limitGaps = useCommandQuery("quota_limit_gaps", undefined, LIMIT_GAPS_KEY);

  const context = useMemo<ChannelRowContext>(
    () => ({
      providers: providers.data ?? [],
      credentials: credentials.data ?? [],
      lanes: lanes.data ?? [],
      groups: groups.data ?? [],
      limitGaps: limitGaps.data ?? [],
    }),
    [providers.data, credentials.data, lanes.data, groups.data, limitGaps.data],
  );

  const rows = useMemo(
    () => readChannelRows(channels.data ?? [], context),
    [channels.data, context],
  );

  const refresh = () => {
    void channels.refetch();
    void providers.refetch();
    void credentials.refetch();
    void lanes.refetch();
    void groups.refetch();
    void limitGaps.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.plansAndRoutes")}
        description={t("channels.description")}
        docRef="07 §5 / 05 §2.1 套餐链（列表视图；矩阵视图后端未实现）"
        actions={
          <Button variant="outlined" onClick={refresh}>
            {t("common.refresh")}
          </Button>
        }
      />
      <ChannelsBody
        rows={rows}
        limitGaps={limitGaps.data ?? []}
        limitGapsError={limitGaps.error ?? undefined}
        isLoading={channels.isPending}
        error={channels.error ?? undefined}
        onRetry={refresh}
      />
    </>
  );
}

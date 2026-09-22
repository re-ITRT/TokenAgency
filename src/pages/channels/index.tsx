/**
 * 套餐与线路页 `/channels`（[07 §5] / [05 §2.1]）。候选路径见 `src/app/routes.ts`：
 * `pages: ["channels", "channels/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * 两条线：
 * - **读**：列表 + 按服务商分组，取数用 `channel_list` / `provider_list` / `credential_list` /
 *   `lane_list` / `group_list` / `quota_limit_gaps`；
 * - **写**（本轮 task-33）：页头的「添加」打开线路向导 ——
 *   服务商 / 凭据（按 provider 过滤）/ 模型（下拉 + 可直接输入）/ 上游模型名 / 网络 / 优先级 /
 *   权重 → `channel_upsert({ channel })`，成功后用**后端返回值**回显并失效重取。
 *   固定 testid 见 [docs/17 §3.3]（`channels-add-*`），真机 E2E 按它们驱动，**不许改名**。
 *
 * **本轮不做**：能力块（`channels-add-vision|tools|reasoning|contextTokens|modalities`）与
 * 计费块（`channels-add-price-*`）—— 能力要等 `ChannelCapabilities` 进契约（task-31），计费下轮一起。
 * 所以这里既不放禁用占位，也不自己造一个 `capabilities` 字段发出去（契约先行）。
 * 其余写命令（`channel_set_enabled` / `channel_bulk_assign_lane` / `channel_set_tags` /
 * `channel_delete`）仍**没有**入口，空状态里也不再声称"入口还没接线"（那已经变成假话）。
 *
 * 三条诚实纪律（与 `./model.ts` 的文件头一致）：
 * 1. **线路继承只认读得到的层级**：通道级 > 凭据级，再往上显示「继承」并说明原因；
 * 2. **占用比只在口径对得上时才算**，否则显示「未设上限」而不是 0%；
 * 3. **读不到的维度画破折号**，不画 0、也不编一句"一切正常"。
 */

import {
  Autocomplete,
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  MenuItem,
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
import { useEffect, useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type {
  CredentialView,
  LaneView,
  ModelView,
  ProviderView,
  QuotaMetric,
  QuotaStateValue,
  QuotaWindow,
} from "@/shared/bindings";
import { LimitGapNotice, PageHeader, QuotaBar, StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatMoney, formatTokens } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  EMPTY_CHANNEL_DRAFT,
  channelDraftIssues,
  channelPayload,
  credentialLabelFor,
  credentialOptionsFor,
  defaultLaneId,
  groupByProvider,
  laneOptionsOf,
  readChannelRows,
  summarizeChannels,
  type ChannelDraft,
  type ChannelDraftField,
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
            /* 空状态：说明通道是什么。**不再**写"新建入口还没有接线" —— 那一句已经变成假话
               （页头的「添加」就是入口），i18n 里的旧键留给主管改写/删除，本页不再引用。 */
            <Stack spacing={1} data-testid="channels-empty">
              <Typography variant="body1">{t("channels.empty.title")}</Typography>
              <Caption>{t("channels.empty.what")}</Caption>
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

// ================================================================ 添加线路向导（task-33）

/** 本地预检点名的字段 → 既有 i18n 键（**不新造键**：缺键由主管统一补，见 [docs/17 §3.0]）。 */
const DRAFT_FIELD_LABEL: Record<ChannelDraftField, I18nKey> = {
  provider: "channels.add.provider",
  credential: "channels.add.credential",
  model: "channels.add.model",
  upstreamModel: "channels.add.upstreamModel",
};

/**
 * 一个下拉字段。
 *
 * `data-testid` 挂在**容器**上（与 `network/index.tsx` 的 `SelectField` 同一形状）：真机 E2E
 * 先找容器内的 `input` / `[role=combobox]`，找不到才退回容器本身 —— 所以容器里必须真的有控件。
 *
 * 值为空时用 `renderValue` 显示 `emptyLabel`：**空就是真的空**（显示「必填」），
 * 不放一个"请选择…"的假选项，也不禁用控件。
 */
function SelectField({
  testId,
  label,
  value,
  onChange,
  options,
  emptyLabel,
  hint,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
  hint?: string;
}) {
  const labelOf = (id: string): string => options.find((o) => o.value === id)?.label ?? id;
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        label={label}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        slotProps={{
          select: {
            displayEmpty: true,
            renderValue: (selected: unknown) =>
              (selected as string) === "" ? emptyLabel : labelOf(selected as string),
          },
        }}
      >
        {options.map((option) => (
          <MenuItem key={option.value} value={option.value}>
            {option.label}
          </MenuItem>
        ))}
      </TextField>
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

/** 只放数字（优先级另许负号）进输入框：非整数字符串进不来，本地就不需要"必须是整数"的新文案。 */
function intInputFilter(raw: string, allowNegative: boolean): string {
  return raw.replace(allowNegative ? /[^0-9-]/g : /[^0-9]/g, "");
}

export type ChannelAddDialogProps = {
  open: boolean;
  onClose: () => void;
  providers: readonly ProviderView[];
  credentials: readonly CredentialView[];
  models: readonly ModelView[];
  lanes: readonly LaneView[];
};

/**
 * 「添加线路」向导（[docs/17 §3.3]）。
 *
 * 顺序就是用户描述的顺序：服务商 → 凭据（按 provider 过滤）→ 模型（下拉 + 可直接输入新名字）
 * → 上游模型名 → 网络（默认直连）→ 优先级 / 权重 → 保存。
 *
 * 保存只做一件事：`channel_upsert({ channel })`；成功区显示的是**后端返回的那个 Channel**
 * （不是草稿），然后失效 `qk.channels` / `qk.models` 重取。
 * 能力块与计费块本轮不做（见文件头），所以这里**没有**它们的控件，也没有禁用占位。
 */
export function ChannelAddDialog({
  open,
  onClose,
  providers,
  credentials,
  models,
  lanes,
}: ChannelAddDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const upsert = useCommandMutation("channel_upsert");

  const [draft, setDraft] = useState<ChannelDraft>(EMPTY_CHANNEL_DRAFT);
  const [issues, setIssues] = useState<readonly ChannelDraftField[]>([]);

  const set = <K extends keyof ChannelDraft>(key: K, value: ChannelDraft[K]): void =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const credentialOptions = useMemo(
    () => credentialOptionsFor(credentials, draft.provider),
    [credentials, draft.provider],
  );
  const laneOptions = useMemo(() => laneOptionsOf(lanes), [lanes]);
  const modelIds = useMemo(() => models.map((m) => m.model.id), [models]);
  const directLane = useMemo(() => defaultLaneId(lanes), [lanes]);

  /**
   * 默认网络 = 配置里那条启用的直连线路（用户原话里的「网络 直连」）。
   * 只在**用户还没选**的时候补上（`prev.lane === ""`）：lane_list 是异步读来的，
   * 无条件覆盖会把用户刚选的线路冲掉。配置里没有直连线路时保持空 → 按继承链走。
   */
  useEffect(() => {
    if (!open || directLane === "") return;
    setDraft((previous) => (previous.lane === "" ? { ...previous, lane: directLane } : previous));
  }, [open, directLane]);

  /** 关掉对话框 = 回到干净草稿（下次打开不会带着上一次的残留，也不会带着旧的回显）。 */
  const close = (): void => {
    setDraft(EMPTY_CHANNEL_DRAFT);
    setIssues([]);
    upsert.reset();
    onClose();
  };

  /** 换服务商 → 凭据清空：凭据下拉是按 provider 过滤的，留着上一个服务商的 id 会变成无效引用。 */
  const changeProvider = (provider: string): void => {
    setDraft((previous) => ({ ...previous, provider, credential: "" }));
  };

  const submit = (): void => {
    const found = channelDraftIssues(draft);
    setIssues(found);
    // 本地预检不过 → **一次都不发**（与设置页 config_validate 的纪律一致）
    if (found.length > 0) return;
    upsert.reset();
    upsert.mutate(
      { channel: channelPayload(draft) },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.channels });
          // 通道数会变（`model_list` 的 channel_count），模型页也一起刷新
          void queryClient.invalidateQueries({ queryKey: qk.models });
        },
      },
    );
  };

  const saved = upsert.data;

  return (
    <Dialog open={open} onClose={close} fullWidth maxWidth="sm">
      <DialogTitle>{t("channels.add.title")}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          <SelectField
            testId="channels-add-provider"
            label={t("channels.add.provider")}
            value={draft.provider}
            onChange={changeProvider}
            emptyLabel={t("wizard.required")}
            options={providers.map((p) => ({ value: p.id, label: p.name ?? p.id }))}
          />

          <SelectField
            testId="channels-add-credential"
            label={t("channels.add.credential")}
            value={draft.credential}
            onChange={(value) => set("credential", value)}
            emptyLabel={t("wizard.required")}
            options={credentialOptions.map((c) => ({ value: c.id, label: c.label }))}
            /* 选中的服务商一份凭据都没有：如实说"暂无数据"，不列别人的凭据、也不禁用控件 */
            hint={
              draft.provider !== "" && credentialOptions.length === 0
                ? t("common.empty")
                : undefined
            }
          />

          {/* 模型：已有 model_list 的下拉 + **允许直接输入新名字**（freeSolo） */}
          <Autocomplete
            freeSolo
            options={modelIds}
            inputValue={draft.model}
            onInputChange={(_event, value) => set("model", value)}
            onChange={(_event, value) => set("model", typeof value === "string" ? value : (value ?? ""))}
            renderInput={(params) => (
              <TextField
                {...params}
                label={t("channels.add.model")}
                size="small"
                fullWidth
                slotProps={{
                  htmlInput: { ...params.inputProps, "data-testid": "channels-add-model" },
                }}
              />
            )}
          />

          <TextField
            label={t("channels.add.upstreamModel")}
            value={draft.upstreamModel}
            onChange={(event) => set("upstreamModel", event.target.value)}
            size="small"
            fullWidth
            slotProps={{ htmlInput: { "data-testid": "channels-add-upstreamModel" } }}
          />

          <SelectField
            testId="channels-add-lane"
            label={t("channels.add.lane")}
            value={draft.lane}
            onChange={(value) => set("lane", value)}
            /* 配置里没有直连线路时才是空的：那时显示继承（后端语义：最终仍是直连） */
            emptyLabel={t("channels.lane.inherited")}
            options={laneOptions.map((l) => ({ value: l.id, label: l.name }))}
          />

          <Box sx={{ display: "flex", gap: 2 }}>
            <TextField
              label={t("channels.add.priority")}
              value={draft.priority}
              onChange={(event) => set("priority", intInputFilter(event.target.value, true))}
              size="small"
              fullWidth
              slotProps={{ htmlInput: { "data-testid": "channels-add-priority", inputMode: "numeric" } }}
            />
            <TextField
              label={t("channels.add.weight")}
              value={draft.weight}
              onChange={(event) => set("weight", intInputFilter(event.target.value, false))}
              size="small"
              fullWidth
              slotProps={{ htmlInput: { "data-testid": "channels-add-weight", inputMode: "numeric" } }}
            />
          </Box>

          {/* 本地预检的错：逐个点名缺哪个（文案用既有键，不新造） */}
          {issues.length > 0 ? (
            <Stack spacing={1} data-testid="channels-add-issues">
              {issues.map((field) => (
                <Typography key={field} variant="body1" sx={{ color: "error.main" }}>
                  {`${t(DRAFT_FIELD_LABEL[field])}：${t("wizard.required")}`}
                </Typography>
              ))}
            </Stack>
          ) : null}

          {/* 后端的错原样显示（TA 码 + 字段路径），本地预检不替代它 */}
          {upsert.error !== null ? (
            <Typography variant="body1" data-testid="channels-add-error" sx={{ color: "error.main" }}>
              {errorMessage(upsert.error)}
            </Typography>
          ) : null}

          {/* 成功：显示**后端返回的那个 Channel**（不是我们提交的草稿） */}
          {saved !== undefined ? (
            <Stack spacing={1} data-testid="channels-add-result">
              <Typography variant="body1">{t("wizard.saved", { id: saved.id })}</Typography>
              <Typography
                variant="body1"
                sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
              >
                {[
                  saved.model,
                  saved.provider,
                  saved.credential,
                  saved.upstream_model,
                  saved.lane ?? DASH,
                  formatInt(saved.priority),
                  formatInt(saved.weight),
                ].join(" · ")}
              </Typography>
            </Stack>
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={close}>{t("common.cancel")}</Button>
        <Button
          variant="contained"
          data-testid="channels-add-submit"
          disabled={upsert.isPending}
          onClick={submit}
        >
          {t("common.save")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

// ================================================================ 数据层

export default function ChannelsPage() {
  const { t } = useTranslation();
  const [addOpen, setAddOpen] = useState(false);

  const channels = useCommandQuery("channel_list", undefined, qk.channels);
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  const groups = useCommandQuery("group_list", undefined, qk.groups);
  const limitGaps = useCommandQuery("quota_limit_gaps", undefined, LIMIT_GAPS_KEY);
  // 只有「添加线路」用得到（模型下拉），列表视图不需要 —— 但它是同一个 key，别处已经在读，不会多打一次
  const models = useCommandQuery("model_list", undefined, qk.models);

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
    void models.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.plansAndRoutes")}
        description={t("channels.description")}
        docRef="07 §5 / 05 §2.1 套餐链（列表视图；矩阵视图后端未实现）"
        actions={
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            <Button variant="outlined" onClick={refresh}>
              {t("common.refresh")}
            </Button>
            <Button
              variant="contained"
              data-testid="channels-add-open"
              onClick={() => setAddOpen(true)}
            >
              {t("channels.add.open")}
            </Button>
          </Box>
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
      <ChannelAddDialog
        open={addOpen}
        onClose={() => setAddOpen(false)}
        providers={providers.data ?? []}
        credentials={credentials.data ?? []}
        models={models.data ?? []}
        lanes={lanes.data ?? []}
      />
    </>
  );
}

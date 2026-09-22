/**
 * 套餐与线路页 `/channels`（[07 §5] / [05 §2.1]）。候选路径见 `src/app/routes.ts`：
 * `pages: ["channels", "channels/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * 两条线：
 * - **读**：列表 + 按服务商分组，取数用 `channel_list` / `provider_list` / `credential_list` /
 *   `lane_list` / `group_list` / `quota_limit_gaps`；
 * - **写**（本轮 task-33）：页头的「添加」打开线路向导 ——
 *   服务商 / 凭据（按 provider 过滤）/ 模型（下拉 + 可直接输入）/ 上游模型名 / 网络 /
 *   优先级 / 权重 / **上游能力声明** / **计费** →
 *   `channel_upsert({ channel })`，成功后用**后端返回值**回显并失效重取；
 *   计费三项里有任意一项非空时，**通道落库之后**再调 `pricing_upsert({ entry })`，
 *   同样回显后端返回值；计费失败时错误原样显示，且**不显示"已保存"**（不许谎报整条线配好了）。
 *   固定 testid 见 [docs/17 §3.3]（`channels-add-*`），真机 E2E 按它们驱动，**不许改名**。
 *
 * 能力块的默认值与 Rust `ChannelCapabilities::default` **逐字一致**（全 false / `context_tokens` 0 /
 * `max_output_tokens` null / `modalities ["text"]`）。`max_output_tokens` 本页**不做输入**
 * （载荷里恒为 `null` = 未声明），所以这里既没有它的控件、也没有禁用占位。
 *
 * 本轮（第 73 轮，[docs/21]）又接上三件事：
 * - **§4.2 每行一个线路下拉**（`channels-lane-select`）：线路的**唯一**编辑入口就在这里
 *   （用户原话「线路设置不能点进服务商里面去修改」）。选「直连（继承）」发 `lane_id: null`
 *   （= 清空 = 回到五级继承链），选某条线路发那个 id；成功后** await 重取**（矩阵 + 通道清单），
 *   行内展示于是跟着**后端重取回来的值**走，失败则原样显示后端原文（含 TA 码）。
 *   行内 `channels-lane` 的"当前生效线路"展示**保留**：下拉编的是通道级那一档，
 *   两者不是一回事（继承时行内会说明继承到了哪条）。
 * - **§1/§3 通道删除 + 批量**（`channels-delete` / `channels-select` / `-select-all` /
 *   `-selected-count` / `-delete-selected`）：二次确认（共享 `ConfirmDelete`）→ `channel_delete({ id })`。
 *   这条命令**没有 force 语义**，所以 `cascadeAvailable={false}`：被引用时只显示后端原文，
 *   不放任何"强行删除"的按钮（那会是假按钮）。批量逐条调用、逐条汇报，一条失败不中止其余条。
 * - **§3 通道组区块**（`groups-delete` / `groups-select` / `-select-all` / `-delete-selected`）：
 *   数据来自 `group_list`，同样没有 force。
 *
 * 仍未接线的写命令：`channel_set_enabled` / `channel_set_tags`（本页不放点了没反应的按钮）。
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
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  MenuItem,
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
import { useEffect, useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type {
  ChannelGroup,
  ChannelGroupId,
  ChannelId,
  CredentialView,
  LaneView,
  ModelView,
  ProviderView,
  QuotaMetric,
  QuotaStateValue,
  QuotaWindow,
} from "@/shared/bindings";
import {
  ConfirmDelete,
  HelpTip,
  LimitGapNotice,
  PageHeader,
  QuotaBar,
  StatusDot,
  type SemStatus,
} from "@/shared/components";
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
  hasPrice,
  laneOptionsOf,
  priceEntryFor,
  readChannelRows,
  summarizeChannels,
  type ChannelDraft,
  type ChannelDraftField,
  type ChannelDraftIssue,
  type ChannelDraftProblem,
  type ChannelNumberField,
  type ChannelRowContext,
  type ChannelRowView,
  type LaneResolution,
  type QuotaLimitGap,
  type QuotaParts,
} from "./model";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

/**
 * 把同一区块的几句解释合成**一条**提示正文（[docs/22 §3]：一个区块最多一个问号）。
 *
 * 两件事逼着要合并：提示气泡只吃一个字符串；而"一排问号"比一排字更糟。
 * 分隔符用换行 —— `aria-label` 里保留断句，读屏不会把两句话连成一坨。
 * （同一个函数的规范实现是 `network/index.tsx` 的 `mergeHelpText`；这里**不跨页 import**
 * 另一个页面模块 —— 那会把整个 network 页拖进本页的依赖里。）
 */
function mergeHelpText(...parts: readonly (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined && part !== "").join("\n");
}

/**
 * 区块的说明行 / 标题里那个问号（[docs/22 §2]）。
 *
 * 只收**解释**：状态结论、空态、错误原文、必填提示、破坏性后果、数值与单位、
 * 按钮文案一律留在版面上（§2 的红线）；判不准时留在版面上（§2 末尾的默认）。
 */
function TitleWithHelp({
  title,
  text,
  testId,
}: {
  title: string;
  text: string;
  testId: string;
}) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
      <Typography variant="h4">{title}</Typography>
      <HelpTip text={text} testId={testId} />
    </Box>
  );
}

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

/**
 * 线路下拉的结果提示（挂在**那一行**上）：
 * `ok === true` 时 `text` 是"已改为 …"，`ok === false` 时 `text` 是**后端原文**（含 TA 码）。
 * 两者都不是从别处推断出来的 —— 失败时行内展示一个数都不改（不做本地乐观更新）。
 */
export type LaneNotice = { channelId: string; ok: boolean; text: string };

/** 删除的逐条结果：成功项与失败项（失败带 id 与后端原文）分开记，汇报时一句都不许盖。 */
export type DeleteOutcome = {
  ok: readonly { id: string; label: string }[];
  failed: readonly { id: string; label: string; message: string }[];
};

/** 一行多选框：`data-testid` 放在容器上，真可点的是里面那个 `input`（与真机 E2E 同口径）。 */
function RowCheckbox({
  testId,
  checked,
  indeterminate = false,
  onChange,
}: {
  testId: string;
  checked: boolean;
  indeterminate?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <Checkbox
      size="small"
      data-testid={testId}
      checked={checked}
      indeterminate={indeterminate}
      onChange={(event) => onChange(event.target.checked)}
    />
  );
}

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
  /** `lane_list`：线路下拉的选项（无名回落 id；第一条永远是「直连（继承）」）。 */
  lanes: readonly LaneView[];
  /** 把**某一行**的通道钉到一条线路上（`null` = 清空 = 回到继承链）。 */
  onAssignLane: (channelId: string, laneId: string | null) => void;
  /** 正在提交的那一行（下拉进禁用态）。 */
  laneBusyId?: string | undefined;
  /** 线路改动的结果（成功文案或后端原文），只显示在对应的那一行上。 */
  laneNotice?: LaneNotice | undefined;
  selectedChannelIds: readonly string[];
  onToggleChannel: (id: string, selected: boolean) => void;
  onToggleAllChannels: (ids: readonly string[], selected: boolean) => void;
  /** 请求删除这些通道（单条 = 一个 id）；确认与命令由数据层做。 */
  onDeleteChannel: (ids: readonly string[]) => void;
  /** `group_list`：通道组区块的数据。 */
  groups: readonly ChannelGroup[];
  selectedGroupIds: readonly string[];
  onToggleGroup: (id: string, selected: boolean) => void;
  onToggleAllGroups: (ids: readonly string[], selected: boolean) => void;
  onDeleteGroup: (ids: readonly string[]) => void;
};

export function ChannelsBody({
  rows,
  limitGaps,
  limitGapsError,
  isLoading,
  error,
  onRetry,
  lanes,
  onAssignLane,
  laneBusyId,
  laneNotice,
  selectedChannelIds,
  onToggleChannel,
  onToggleAllChannels,
  onDeleteChannel,
  groups,
  selectedGroupIds,
  onToggleGroup,
  onToggleAllGroups,
  onDeleteGroup,
}: ChannelsBodyProps) {
  const { t } = useTranslation();

  const summary = useMemo(() => summarizeChannels(rows), [rows]);
  const grouped = useMemo(() => groupByProvider(rows), [rows]);
  const anyInherited = rows.some((r) => r.lane.source === "inherited");

  /**
   * 线路下拉的选项：**第一条永远是「直连（继承）」**（value `""` → 载荷 `lane_id: null`），
   * 其余来自 `lane_list`（label 用 `lane.name`，没有名字就回落 id —— 不编一个名字）。
   */
  const laneChoices = useMemo(
    () => laneOptionsOf(lanes).map((l) => ({ value: l.id, label: l.name === "" ? l.id : l.name })),
    [lanes],
  );

  const listBroken = error !== undefined && error !== null;
  const gapsBroken = limitGapsError !== undefined && limitGapsError !== null;
  /** 有服务商没有健康状态这一列（真机现状）→ 说清楚"为什么不点状态灯"。 */
  const anyStatusMissing = grouped.some((g) => g.status === undefined);

  /**
   * 「按服务商分组」这一块的全部解释，合成**一条**提示正文（[docs/22 §3]：一个区块最多一个问号）。
   *
   * 两句都是"怎么理解这里显示的线路 / 为什么这里没有状态灯"的解释 → 收进问号圆圈（§2）。
   * 两句都各自带条件：只有真的出现「继承」或真的缺状态列时才进提示 —— 没有说明就不渲染。
   */
  const groupsHelp = mergeHelpText(
    anyInherited ? t("channels.lane.hint") : undefined,
    anyStatusMissing ? t("channels.provider.statusAbsent") : undefined,
  );

  /* 选中计数只对**当前真的在列表里**的 id 算：删掉/换掉的通道不会留在计数里 */
  const selectedRows = rows.filter((r) => selectedChannelIds.includes(r.id));
  const selectedGroups = groups.filter((g) => selectedGroupIds.includes(g.id));

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* 1 概览：读不到的那一维画破折号，不画 0 */}
      <Paper variant="outlined" sx={SECTION_SX} data-testid="channels-summary">
        <Stack spacing={3}>
          {/* 「读不到的那一维显示「—」，不显示 0」是一句**解释**（[docs/22 §2] 的区块说明行，
              超过 12 个字符）→ 收进标题右边的问号圆圈；下面那五个数与单位一个字都不动（§2 第 5 条）。 */}
          <TitleWithHelp
            title={t("channels.summary.title")}
            text={t("channels.summary.hint")}
            testId="channels-summary-help"
          />

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
            {/* 「判定与文案都来自后端，本页一个字都不改写」是**解释**（[docs/22 §2]）→ 收进问号；
                读失败时的那句（带 TA 码 + "这不等于没有这条提醒"）留在版面上（§2 第 2 条）。 */}
            <TitleWithHelp
              title={t("channels.limitGaps.title")}
              text={t("channels.limitGaps.hint")}
              testId="channels-limit-gaps-help"
            />

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
          {/* 这一块的两句解释（线路怎么继承 / 为什么不点状态灯）合成**一个**问号（[docs/22 §2/§3]）。
              它们只在真的出现「继承」或真的缺状态列时才存在：没有说明就不占位、不摆一个空问号。 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
            <Typography variant="h4">{t("channels.groups.title")}</Typography>
            {groupsHelp === "" ? null : (
              <HelpTip text={groupsHelp} testId="channels-by-provider-help" />
            )}
          </Box>

          {/* 批量删除：**选中了才出现**（一条都没选时摆一个点了没用的按钮就是假界面） */}
          {selectedRows.length > 0 ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="channels-selected-count">
                {t("delete.selectedCount", { count: selectedRows.length })}
              </Typography>
              <Button
                variant="outlined"
                color="error"
                data-testid="channels-delete-selected"
                onClick={() => onDeleteChannel(selectedRows.map((r) => r.id))}
              >
                {t("common.deleteSelected")}
              </Button>
            </Stack>
          ) : null}

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
                          {/* 表头全选：选中的是**这一张表**（这个服务商）的行 */}
                          <TableCell padding="checkbox">
                            <RowCheckbox
                              testId="channels-select-all"
                              checked={
                                group.rows.length > 0 &&
                                group.rows.every((r) => selectedChannelIds.includes(r.id))
                              }
                              indeterminate={
                                group.rows.some((r) => selectedChannelIds.includes(r.id)) &&
                                !group.rows.every((r) => selectedChannelIds.includes(r.id))
                              }
                              onChange={(checked) =>
                                onToggleAllChannels(
                                  group.rows.map((r) => r.id),
                                  checked,
                                )
                              }
                            />
                          </TableCell>
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
                          <TableCell align="right" />
                        </TableRow>
                      </TableHead>
                      <TableBody>
                        {group.rows.map((row) => (
                          <TableRow key={row.id} hover data-testid="channels-row">
                            <TableCell padding="checkbox">
                              <RowCheckbox
                                testId="channels-select"
                                checked={selectedChannelIds.includes(row.id)}
                                onChange={(checked) => onToggleChannel(row.id, checked)}
                              />
                            </TableCell>
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
                            <TableCell sx={{ minWidth: 200 }}>
                              {/* 当前**生效**的线路（读得到哪一级就说哪一级）——
                                  它与下面下拉里选的那一档不是一回事：继承时它来自凭据级。 */}
                              <Box data-testid="channels-lane">
                                {row.lane.source === "inherited"
                                  ? t("channels.lane.inherited")
                                  : row.lane.source === "credential"
                                    ? t("channels.lane.fromCredential", { name: laneName(row.lane) })
                                    : laneName(row.lane)}
                              </Box>
                              {/* 线路设置的**唯一**入口（[docs/21 §4.2]）。
                                  值为 `""`（= 通道自己没有钉线路）时显示「直连（继承）」：
                                  它是**清空**（`lane_id: null`），不是"选了一条叫直连的线路"。 */}
                              <Box sx={{ mt: 1 }}>
                                <SelectField
                                  testId="channels-lane-select"
                                  label={t("channels.lane.select")}
                                  value={row.lane.source === "channel" ? (row.lane.laneId ?? "") : ""}
                                  onChange={(value) => onAssignLane(row.id, value === "" ? null : value)}
                                  emptyLabel={t("channels.lane.inherit")}
                                  emptyOptionLabel={t("channels.lane.inherit")}
                                  options={laneChoices}
                                  disabled={laneBusyId === row.id}
                                />
                              </Box>
                              {/* 这一次改动的结果：成功如实说是哪条，失败原样显示后端原文（含 TA 码） */}
                              {laneNotice !== undefined && laneNotice.channelId === row.id ? (
                                laneNotice.ok ? (
                                  <Typography
                                    variant="caption"
                                    data-testid="channels-lane-changed"
                                    sx={{ color: "text.secondary", display: "block" }}
                                  >
                                    {laneNotice.text}
                                  </Typography>
                                ) : (
                                  <Typography
                                    variant="caption"
                                    data-testid="channels-lane-error"
                                    sx={{ color: "error.main", display: "block", overflowWrap: "anywhere" }}
                                  >
                                    {laneNotice.text}
                                  </Typography>
                                )
                              ) : null}
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
                            <TableCell align="right">
                              {/* 行内删除：只是**请求**删除，真正的命令在确认之后（数据层） */}
                              <Button
                                size="small"
                                color="error"
                                data-testid="channels-delete"
                                onClick={() => onDeleteChannel([row.id])}
                              >
                                {t("common.delete")}
                              </Button>
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

      {/* 5 通道组（[docs/21 §3]）：数据来自 `group_list`；删除同样没有 force 语义。
          标题 / 空态 / 列名的键是 `channels.channelGroups.*`（主管为避免与上面
          "按服务商分组" 的 `channels.groups.title` 撞名而改的口径，[docs/21 §5] 的名单以此为准）。 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Typography variant="h4">{t("channels.channelGroups.title")}</Typography>

          {/* 批量删除：与通道那一段同一套语义（选中了才出现） */}
          {selectedGroups.length > 0 ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography variant="body1" data-testid="groups-selected-count">
                {t("delete.selectedCount", { count: selectedGroups.length })}
              </Typography>
              <Button
                variant="outlined"
                color="error"
                data-testid="groups-delete-selected"
                onClick={() => onDeleteGroup(selectedGroups.map((g) => g.id))}
              >
                {t("common.deleteSelected")}
              </Button>
            </Stack>
          ) : null}

          {groups.length === 0 ? (
            /* 空态：如实说"配置里还没有通道组"，**不编**一行占位数据 */
            <Typography variant="body1" data-testid="channels-groups-empty" sx={{ color: "text.secondary" }}>
              {t("channels.channelGroups.empty")}
            </Typography>
          ) : (
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" data-testid="channels-groups-table">
                <TableHead>
                  <TableRow>
                    <TableCell padding="checkbox">
                      <RowCheckbox
                        testId="groups-select-all"
                        checked={
                          groups.length > 0 && groups.every((g) => selectedGroupIds.includes(g.id))
                        }
                        indeterminate={
                          groups.some((g) => selectedGroupIds.includes(g.id)) &&
                          !groups.every((g) => selectedGroupIds.includes(g.id))
                        }
                        onChange={(checked) =>
                          onToggleAllGroups(
                            groups.map((g) => g.id),
                            checked,
                          )
                        }
                      />
                    </TableCell>
                    <TableCell>{t("channels.channelGroups.col.model")}</TableCell>
                    <TableCell>{t("channels.channelGroups.col.strategy")}</TableCell>
                    <TableCell align="right">{t("channels.channelGroups.col.channels")}</TableCell>
                    <TableCell align="right">{t("channels.channelGroups.col.actions")}</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {groups.map((group) => (
                    <TableRow key={group.id} hover data-testid="channels-group-row">
                      <TableCell padding="checkbox">
                        <RowCheckbox
                          testId="groups-select"
                          checked={selectedGroupIds.includes(group.id)}
                          onChange={(checked) => onToggleGroup(group.id, checked)}
                        />
                      </TableCell>
                      <TableCell>
                        <Box
                          data-testid="channels-group-id"
                          sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                        >
                          {group.id}
                        </Box>
                        <Box
                          data-testid="channels-group-model"
                          sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                        >
                          {group.model}
                        </Box>
                      </TableCell>
                      {/* 选择策略**原样**显示后端的枚举值（翻译成另一套说法就等于解释后端了） */}
                      <TableCell
                        data-testid="channels-group-strategy"
                        sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                      >
                        {group.selector}
                      </TableCell>
                      <TableCell
                        align="right"
                        data-testid="channels-group-count"
                        sx={{ fontVariantNumeric: "tabular-nums" }}
                      >
                        {formatInt(group.members.length)}
                      </TableCell>
                      <TableCell align="right">
                        <Button
                          size="small"
                          color="error"
                          data-testid="groups-delete"
                          onClick={() => onDeleteGroup([group.id])}
                        >
                          {t("common.delete")}
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </Box>
          )}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 添加线路向导（task-33）

/** 本地预检点名的字段 → 既有 i18n 键（**不新造键**：缺键由主管统一补，见 [docs/17 §3.0]）。 */
const DRAFT_FIELD_LABEL: Record<ChannelDraftField | ChannelNumberField, I18nKey> = {
  provider: "channels.add.provider",
  credential: "channels.add.credential",
  model: "channels.add.model",
  upstreamModel: "channels.add.upstreamModel",
  contextTokens: "capabilities.contextTokens",
  priceInput: "channels.add.priceInput",
  priceOutput: "channels.add.priceOutput",
  priceCached: "channels.add.priceCached",
};

/** 本地预检的两类问题各自的文案（**必填**与**不是数字**是两件事，不许混成一句话）。 */
const DRAFT_PROBLEM_LABEL: Record<ChannelDraftProblem, I18nKey> = {
  required: "wizard.required",
  number: "wizard.invalidNumber",
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
  emptyOptionLabel,
  hint,
  disabled = false,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
  /**
   * 给定时，列表里**真的多一条** `value: ""` 的选项（文案就是它）。
   *
   * 为什么需要它：`emptyLabel` 只影响"当前值是空"时控件上显示什么，列表里并没有这一项 ——
   * 而通道页的线路列必须能**选回**空（= 清空 channel 级线路，回到继承链），
   * 少了这一项用户就再也回不到继承了。向导的 `channels-add-lane` 不传它：那里空值只是
   * "配置里没有直连线路"的兜底，不是一条用户该主动选的选项。
   */
  emptyOptionLabel?: string;
  hint?: string;
  /** 正在提交这一条：控件进禁用态（**临时**状态，不是"点了没反应"的占位）。 */
  disabled?: boolean;
}) {
  const labelOf = (id: string): string => options.find((o) => o.value === id)?.label ?? id;
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        disabled={disabled}
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
        {emptyOptionLabel === undefined ? null : (
          <MenuItem value="">{emptyOptionLabel}</MenuItem>
        )}
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
 * → 上游模型名 → 网络（默认直连）→ 优先级 / 权重 → 上游能力声明 → 计费 → 保存。
 *
 * 保存是**两条命令、有先后**：
 * 1. `channel_upsert({ channel })` —— 载荷里**永远**带 `capabilities`（默认值 = Rust 的默认值）；
 * 2. 计费三项里有任意一项非空时，**通道成功之后**才 `pricing_upsert({ entry })` ——
 *    价格不许指向一条没落库的线。
 *
 * 成功区显示的两块都是**后端返回值**（不是草稿）。计费失败时：通道确实已经落库（后端返回的
 * Channel 照旧显示），但**不显示"已保存"这句成功文案**，后端错误（含 TA 码）原样显示 ——
 * 那才是这一刻的真实状态。
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
  const pricing = useCommandMutation("pricing_upsert");

  const [draft, setDraft] = useState<ChannelDraft>(EMPTY_CHANNEL_DRAFT);
  const [issues, setIssues] = useState<readonly ChannelDraftIssue[]>([]);

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
    pricing.reset();
    onClose();
  };

  /** 换服务商 → 凭据清空：凭据下拉是按 provider 过滤的，留着上一个服务商的 id 会变成无效引用。 */
  const changeProvider = (provider: string): void => {
    setDraft((previous) => ({ ...previous, provider, credential: "" }));
  };

  const submit = (): void => {
    const found = channelDraftIssues(draft);
    setIssues(found);
    // 本地预检不过 → **一次都不发**（与设置页 config_validate 的纪律一致）。
    // 价格与上下文窗口的非法输入在这一步就被挡住，所以两条命令都不会被调用。
    if (found.length > 0) return;
    upsert.reset();
    pricing.reset();
    upsert.mutate(
      { channel: channelPayload(draft) },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.channels });
          // 通道数会变（`model_list` 的 channel_count），模型页也一起刷新
          void queryClient.invalidateQueries({ queryKey: qk.models });

          // 计费就在**这里**发：它必须排在 `channel_upsert` 成功之后（价格不许指向
          // 一条没落库的线）。三项全空 = 不计价 → 这一整条命令都不发。
          if (!hasPrice(draft)) return;
          pricing.mutate(
            { entry: priceEntryFor(draft) },
            {
              onSuccess: () => {
                void queryClient.invalidateQueries({ queryKey: qk.pricing });
              },
            },
          );
        },
      },
    );
  };

  const saved = upsert.data;
  const priced = pricing.data;
  /** 计费失败：通道确实落库了，但**不许**把这一刻说成"已保存"（错误原文会显示在下面）。 */
  const priceFailed = pricing.error !== null && pricing.error !== undefined;
  const failure = upsert.error ?? pricing.error;

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

          {/* 上游能力声明（[docs/17 §3.3] / §2）：默认值与 Rust `ChannelCapabilities::default`
              逐字一致。`max_output_tokens` 本页不做输入 -> 载荷里恒为 null，
              **不放禁用占位控件**（放一个点不动的框就是假界面）。 */}
          <Stack spacing={2}>
            {/* 「同一模型的多条线路能力必须一致」是这一小块的**说明行**（[docs/22 §2]，超过 12 个字符）
                → 收进问号圆圈。真正的不一致提示（`capabilities.mismatch`，带逐字段差异）留在版面上（§2 第 1/3 条）。 */}
            <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
              <Typography variant="body1">{t("capabilities.title")}</Typography>
              <HelpTip text={t("capabilities.hint")} testId="channels-add-capabilities-help" />
            </Box>

            <Box sx={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
              <FormControlLabel
                control={
                  <Switch
                    size="small"
                    checked={draft.vision}
                    onChange={(event) => set("vision", event.target.checked)}
                    data-testid="channels-add-vision"
                  />
                }
                label={t("capabilities.vision")}
              />
              <FormControlLabel
                control={
                  <Switch
                    size="small"
                    checked={draft.tools}
                    onChange={(event) => set("tools", event.target.checked)}
                    data-testid="channels-add-tools"
                  />
                }
                label={t("capabilities.tools")}
              />
              <FormControlLabel
                control={
                  <Switch
                    size="small"
                    checked={draft.reasoning}
                    onChange={(event) => set("reasoning", event.target.checked)}
                    data-testid="channels-add-reasoning"
                  />
                }
                label={t("capabilities.reasoning")}
              />
            </Box>
            {/* 上下文窗口只放数字进来（与优先级/权重同一个 intInputFilter）：本地就不会有 NaN */}
            <TextField
              label={t("capabilities.contextTokens")}
              value={draft.contextTokens}
              onChange={(event) => set("contextTokens", intInputFilter(event.target.value, false))}
              size="small"
              fullWidth
              slotProps={{
                htmlInput: { "data-testid": "channels-add-contextTokens", inputMode: "numeric" },
              }}
            />
            <TextField
              label={t("capabilities.modalities")}
              value={draft.modalities}
              onChange={(event) => set("modalities", event.target.value)}
              size="small"
              fullWidth
              slotProps={{ htmlInput: { "data-testid": "channels-add-modalities" } }}
            />
          </Stack>

          {/* 计费（[docs/17 §3.3]）：单位是"美元 / 百万 token"（`PricingConfig.unit` 的默认
              `per_1m`），**可留空 = 不计价**。三项全空时一条定价都不写 —— 不写 0
              （0 是"免费"，是另一个意思）。这里**不净化输入**：非法价格要点名，不许被悄悄吞掉。 */}
          <Stack spacing={2}>
            <Caption>{t("channels.add.price")}</Caption>
            <Box sx={{ display: "flex", gap: 2 }}>
              <TextField
                label={t("channels.add.priceInput")}
                value={draft.priceInput}
                onChange={(event) => set("priceInput", event.target.value)}
                size="small"
                fullWidth
                slotProps={{ htmlInput: { "data-testid": "channels-add-price-input", inputMode: "decimal" } }}
              />
              <TextField
                label={t("channels.add.priceOutput")}
                value={draft.priceOutput}
                onChange={(event) => set("priceOutput", event.target.value)}
                size="small"
                fullWidth
                slotProps={{ htmlInput: { "data-testid": "channels-add-price-output", inputMode: "decimal" } }}
              />
              <TextField
                label={t("channels.add.priceCached")}
                value={draft.priceCached}
                onChange={(event) => set("priceCached", event.target.value)}
                size="small"
                fullWidth
                slotProps={{ htmlInput: { "data-testid": "channels-add-price-cached", inputMode: "decimal" } }}
              />
            </Box>
          </Stack>

          {/* 本地预检的错：逐个点名是哪个字段的哪一类问题（文案用既有键，不新造）。
              「必填」与「必须是数字」是两件事，所以分开渲染，不混成一句。 */}
          {issues.length > 0 ? (
            <Stack spacing={1} data-testid="channels-add-issues">
              {issues.map((issue) => (
                <Typography
                  key={`${issue.field}:${issue.problem}`}
                  variant="body1"
                  sx={{ color: "error.main" }}
                >
                  {`${t(DRAFT_FIELD_LABEL[issue.field])}：${t(DRAFT_PROBLEM_LABEL[issue.problem])}`}
                </Typography>
              ))}
            </Stack>
          ) : null}

          {/* 后端的错原样显示（TA 码 + 字段路径），本地预检不替代它；
              计费的错也走这里 —— 两条命令各自的失败原因都不能被吞掉 */}
          {failure !== null && failure !== undefined ? (
            <Typography variant="body1" data-testid="channels-add-error" sx={{ color: "error.main" }}>
              {errorMessage(failure)}
            </Typography>
          ) : null}

          {/* 成功：显示**后端返回的那个 Channel**（不是我们提交的草稿）；
              调了计费就再显示**后端返回的 PriceEntry 原样**（JSON 原样，不自己拼一句概述）。
              计费失败时**不显示"已保存"** —— 那一刻整条线还没配好；但通道的身份仍然如实列出来
              （id 在第一位），用户才知道库里多了/改了哪一条。 */}
          {saved !== undefined ? (
            <Stack spacing={1} data-testid="channels-add-result">
              {priceFailed ? null : (
                <Typography variant="body1">{t("wizard.saved", { id: saved.id })}</Typography>
              )}
              <Typography
                variant="body1"
                sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
              >
                {[
                  saved.id,
                  saved.model,
                  saved.provider,
                  saved.credential,
                  saved.upstream_model,
                  saved.lane ?? DASH,
                  formatInt(saved.priority),
                  formatInt(saved.weight),
                ].join(" · ")}
              </Typography>
              {priced !== undefined ? (
                <Typography
                  variant="body1"
                  sx={{
                    fontFamily: typography.fontMono,
                    overflowWrap: "anywhere",
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {JSON.stringify(priced, null, 2)}
                </Typography>
              ) : null}
            </Stack>
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={close}>{t("common.cancel")}</Button>
        <Button
          variant="contained"
          data-testid="channels-add-submit"
          disabled={upsert.isPending || pricing.isPending}
          onClick={submit}
        >
          {t("common.save")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

// ================================================================ 数据层

/**
 * 删除结果的汇报区（[docs/21 §1]）：**逐条**写清楚。
 *
 * 为什么不是一句"批量删除完成"：部分失败被一句话盖住，用户就再也分不清哪条还在
 * （§1.2 明说禁止）。所以成功条数 / 失败条数各算各的，失败项**逐条**给 id 与后端原文。
 */
function DeleteResult({ testId, outcome }: { testId: string; outcome: DeleteOutcome | undefined }) {
  const { t } = useTranslation();
  if (outcome === undefined) return null;

  return (
    <Stack spacing={1} data-testid={testId}>
      <Typography variant="body1">
        {t("delete.result.summary", { ok: outcome.ok.length, failed: outcome.failed.length })}
      </Typography>
      {outcome.ok.map((item) => (
        <Typography key={`ok:${item.id}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
          {t("delete.result.ok", { name: item.label })}
        </Typography>
      ))}
      {outcome.failed.map((item) => (
        <Typography
          key={`failed:${item.id}`}
          variant="body1"
          sx={{ color: "error.main", overflowWrap: "anywhere" }}
        >
          {/* id 与后端原文都要在：只说"失败了"用户不知道该重试哪一条 */}
          {`${item.id}：${t("delete.result.failed", { message: item.message })}`}
        </Typography>
      ))}
    </Stack>
  );
}

/** 待确认的删除（单条与批量走**同一套**语义：同一个确认组件、同一种载荷形状）。 */
type PendingDelete = {
  kind: "channel" | "group";
  /** id + 显示名（名称 + id）：确认框与成功文案都用它，**原样**不改写用户数据。 */
  targets: readonly { id: string; label: string }[];
};

export default function ChannelsPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
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

  // ---------------------------------------------------------------- 线路（[docs/21 §4.2]）

  const assignLane = useCommandMutation("channel_bulk_assign_lane");
  const [laneNotice, setLaneNotice] = useState<LaneNotice | undefined>(undefined);

  /** 一条被选中的线路的显示名（`null` 就是「直连（继承）」那一档，不是一个叫"直连"的线路）。 */
  const laneLabelOf = (laneId: string | null): string => {
    if (laneId === null) return t("channels.lane.inherit");
    return (lanes.data ?? []).find((l) => l.lane.id === laneId)?.lane.name ?? laneId;
  };

  const changeLane = (channelId: string, laneId: string | null): void => {
    setLaneNotice(undefined);
    assignLane.mutate(
      { channel_ids: [channelId], lane_id: laneId },
      {
        onSuccess: async () => {
          /**
           * **await 重取**：值必须真的从后端回来。
           * 本页不做本地乐观更新 —— 那会让"后端其实没改"也显示成改好了，
           * 而且行内展示的"当前生效线路"是继承链算出来的，只有重取才说得准。
           *
           * `qk.channelMatrix` 也一起失效：本页此刻没有它的观察者（矩阵那一段如实写着"尚未接入"），
           * 所以这一次失效现在不会产生请求；矩阵视图接上的那一天，它就已经是对的。
           */
          await queryClient.invalidateQueries({ queryKey: qk.channelMatrix });
          await queryClient.invalidateQueries({ queryKey: qk.channels });
          await queryClient.invalidateQueries({ queryKey: qk.lanes });
          setLaneNotice({
            channelId,
            ok: true,
            text: t("channels.lane.changed", { name: laneLabelOf(laneId) }),
          });
        },
        onError: (error) => {
          // 后端原文（含 TA 码）原样写到那一行：不改写、不吞、也不偷偷把显示值改掉
          setLaneNotice({ channelId, ok: false, text: errorMessage(error) });
        },
      },
    );
  };

  // ---------------------------------------------------------------- 删除（[docs/21 §1 / §3]）

  const channelDelete = useCommandMutation("channel_delete");
  const groupDelete = useCommandMutation("group_delete");

  const [selectedChannels, setSelectedChannels] = useState<readonly string[]>([]);
  const [selectedGroups, setSelectedGroups] = useState<readonly string[]>([]);
  const [pending, setPending] = useState<PendingDelete | undefined>(undefined);
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  const [channelOutcome, setChannelOutcome] = useState<DeleteOutcome | undefined>(undefined);
  const [groupOutcome, setGroupOutcome] = useState<DeleteOutcome | undefined>(undefined);

  const toggleIn = (
    set: (updater: (previous: readonly string[]) => readonly string[]) => void,
    id: string,
    selected: boolean,
  ): void => {
    set((previous) =>
      selected ? [...new Set([...previous, id])] : previous.filter((existing) => existing !== id),
    );
  };

  const toggleAllIn = (
    set: (updater: (previous: readonly string[]) => readonly string[]) => void,
    ids: readonly string[],
    selected: boolean,
  ): void => {
    set((previous) =>
      selected
        ? [...new Set([...previous, ...ids])]
        : previous.filter((existing) => !ids.includes(existing)),
    );
  };

  const askDeleteChannels = (ids: readonly string[]): void => {
    setDeleteError(undefined);
    const targets = rows
      .filter((row) => ids.includes(row.id))
      .map((row) => ({ id: row.id, label: `${row.model} · ${row.id}` }));
    // 一条都对不上（列表已经变了）→ **不开**一个空确认框：那是一个点了不知道会删什么的框
    if (targets.length === 0) return;
    setPending({ kind: "channel", targets });
  };

  const askDeleteGroups = (ids: readonly string[]): void => {
    setDeleteError(undefined);
    const targets = (groups.data ?? [])
      .filter((group) => ids.includes(group.id))
      .map((group) => ({ id: group.id, label: `${group.model} · ${group.id}` }));
    if (targets.length === 0) return;
    setPending({ kind: "group", targets });
  };

  const cancelDelete = (): void => {
    setPending(undefined);
    setDeleteError(undefined);
  };

  /** 删除命令的入参就是 `{ id }`（通道与通道组**都没有** force / 级联语义）。 */
  const runDelete = (kind: "channel" | "group", id: string): Promise<void> =>
    kind === "channel"
      ? channelDelete.mutateAsync({ id: id as ChannelId })
      : groupDelete.mutateAsync({ id: id as ChannelGroupId });

  /** 逐条删除（**串行**）：一条失败不中止其余条，逐条记账。 */
  const deleteEach = async (
    kind: "channel" | "group",
    targets: PendingDelete["targets"],
  ): Promise<DeleteOutcome> => {
    const ok: { id: string; label: string }[] = [];
    const failed: { id: string; label: string; message: string }[] = [];
    for (const target of targets) {
      try {
        await runDelete(kind, target.id);
        ok.push({ id: target.id, label: target.label });
      } catch (error) {
        failed.push({ id: target.id, label: target.label, message: errorMessage(error) });
      }
    }
    return { ok, failed };
  };

  /** 删除之后失效的查询：列表 + 矩阵 + 通道组（members 里可能还写着被删的通道）+ 模型（通道数）。 */
  const invalidateAfterDelete = (): void => {
    for (const key of [qk.channels, qk.channelMatrix, qk.groups, qk.models, qk.policies]) {
      void queryClient.invalidateQueries({ queryKey: key });
    }
  };

  /** 成功了的那几条要退出选中集；失败的**留着**（用户可以马上再点一次）。 */
  const dropSucceeded = (
    set: (updater: (previous: readonly string[]) => readonly string[]) => void,
    succeeded: readonly string[],
  ): void => {
    set((previous) => previous.filter((id) => !succeeded.includes(id)));
  };

  const confirmDelete = (): void => {
    const target = pending;
    if (target === undefined) return;
    setDeleteError(undefined);

    // 单条：失败就把**后端原文**留在确认框里（通道/通道组没有 force -> 一个级联按钮都不放）
    if (target.targets.length === 1) {
      const only = target.targets[0]!;
      void runDelete(target.kind, only.id)
        .then(() => {
          const outcome: DeleteOutcome = { ok: [{ id: only.id, label: only.label }], failed: [] };
          if (target.kind === "channel") {
            setChannelOutcome(outcome);
            dropSucceeded(setSelectedChannels, [only.id]);
          } else {
            setGroupOutcome(outcome);
            dropSucceeded(setSelectedGroups, [only.id]);
          }
          setPending(undefined);
          invalidateAfterDelete();
        })
        .catch((error: unknown) => {
          setDeleteError(errorMessage(error));
        });
      return;
    }

    // 批量：一次确认 -> 逐条 -> 逐条汇报（确认框关掉，结果区说清楚哪条成了、哪条没成）
    void deleteEach(target.kind, target.targets).then((outcome) => {
      const succeeded = outcome.ok.map((item) => item.id);
      if (target.kind === "channel") {
        setChannelOutcome(outcome);
        dropSucceeded(setSelectedChannels, succeeded);
      } else {
        setGroupOutcome(outcome);
        dropSucceeded(setSelectedGroups, succeeded);
      }
      setPending(undefined);
      invalidateAfterDelete();
    });
  };

  const pendingLabel =
    pending === undefined
      ? ""
      : pending.targets.length === 1
        ? pending.targets[0]!.label
        : `${t("delete.selectedCount", { count: pending.targets.length })}：${pending.targets
            .map((item) => item.id)
            .join(" · ")}`;

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

      {/* 删除结果：有才出现（没删过就不摆一个空区域） */}
      {channelOutcome !== undefined || groupOutcome !== undefined ? (
        <Box sx={{ px: 8, pt: 4, display: "flex", flexDirection: "column", gap: 2 }}>
          <DeleteResult testId="channels-delete-result" outcome={channelOutcome} />
          <DeleteResult testId="groups-delete-result" outcome={groupOutcome} />
        </Box>
      ) : null}

      <ChannelsBody
        rows={rows}
        limitGaps={limitGaps.data ?? []}
        limitGapsError={limitGaps.error ?? undefined}
        isLoading={channels.isPending}
        error={channels.error ?? undefined}
        onRetry={refresh}
        lanes={lanes.data ?? []}
        onAssignLane={changeLane}
        laneBusyId={assignLane.isPending ? assignLane.variables?.channel_ids[0] : undefined}
        laneNotice={laneNotice}
        selectedChannelIds={selectedChannels}
        onToggleChannel={(id, selected) => toggleIn(setSelectedChannels, id, selected)}
        onToggleAllChannels={(ids, selected) => toggleAllIn(setSelectedChannels, ids, selected)}
        onDeleteChannel={askDeleteChannels}
        groups={groups.data ?? []}
        selectedGroupIds={selectedGroups}
        onToggleGroup={(id, selected) => toggleIn(setSelectedGroups, id, selected)}
        onToggleAllGroups={(ids, selected) => toggleAllIn(setSelectedGroups, ids, selected)}
        onDeleteGroup={askDeleteGroups}
      />

      {/* 通道删除确认（[docs/21 §2]）：`consequences` 留空 —— 前端算得出"这条通道属于哪些通道组"，
          但 i18n 里没有这句话的键（不许自己插键），所以宁可不编，也不在这里硬写中文。 */}
      <ConfirmDelete
        open={pending?.kind === "channel"}
        testIdPrefix="channels"
        title={t("delete.title.channel")}
        targetLabel={pendingLabel}
        cascadeAvailable={false}
        pending={channelDelete.isPending}
        errorText={deleteError}
        onConfirm={confirmDelete}
        onConfirmCascade={() => {}}
        onCancel={cancelDelete}
      />

      {/* 通道组删除确认：同样没有 force 语义（`cascadeAvailable={false}` -> 级联按钮不会出现） */}
      <ConfirmDelete
        open={pending?.kind === "group"}
        testIdPrefix="groups"
        title={t("delete.title.group")}
        targetLabel={pendingLabel}
        cascadeAvailable={false}
        pending={groupDelete.isPending}
        errorText={deleteError}
        onConfirm={confirmDelete}
        onConfirmCascade={() => {}}
        onCancel={cancelDelete}
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

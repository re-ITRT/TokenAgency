/**
 * 请求日志页 `/logs`（[07 §2]、[14 §12]）—— M1 验收 C 组（C1/C2/C4）的用户可见面。
 *
 * 三条硬纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **只用已注册的命令**：`log_query` / `log_get_trace` / `log_set_mode` / `log_set_retention`
 *    / `log_storage_stats` / `log_clear` / `app_open_dir` / `log_export`。
 *    导出是**真入口**（不是禁用占位）：路径由用户填 —— 后端**没有**任何命令上报日志/数据目录，
 *    所以界面**不硬编** AppData、也不显示一个编出来的默认路径；落盘路径以**后端回执**为准。
 * 2. **不许假筛选**：只有按下「查询」后进入 `applied` 的条件才会进 query key，
 *    结果区常驻显示"当前条件"，用户看得见实际生效的是什么。
 *    后端只接受**一个**检索串，所以「请求 ID」与「错误码」在 UI 上互斥（与后端同一规则）。
 * 3. **没有读取接口的就不猜**：当前日志模式与保留期**都没有读取命令**
 *    （`log_set_mode` / `log_set_retention` 只写不读），所以选择框默认显示「未读取」，
 *    另外单独一行陈述**有证据的事实**：最近日志行**记录时**的模式（来自行内字段）。
 *    off 模式的后果（列表为空、筛选无结果）如实写在筛选项旁边。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("log_query", logQueryArgs(applied), qk.logs(fp))`  → 列表第一页（C2 的检索/时间/级别筛选）
 * - `useCommandMutation("log_query")`                                  → 「加载更多」（游标分页）
 * - `useCommandQuery("log_get_trace", {request_id}, qk.trace(id))`      → 展开某一行时的跨组件轨迹（C4）
 * - `useCommandQuery("log_storage_stats", undefined, qk.logStats)`      → 占用（C1）
 * - `useCommandMutation("app_open_dir")`                                → 一键打开日志目录（C1）
 * - `useCommandMutation("log_set_mode" | "log_set_retention" | "log_clear")`
 *
 * 契约与 Rust 实现的字段名/参数名偏差见 `model.ts` 的文件头（那里是唯一的收口处）。
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
  IconButton,
  MenuItem,
  Paper,
  Radio,
  RadioGroup,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import ExpandLessOutlinedIcon from "@mui/icons-material/ExpandLessOutlined";
import ExpandMoreOutlinedIcon from "@mui/icons-material/ExpandMoreOutlined";
import { Fragment, useMemo, useState, type ReactNode } from "react";

import {
  errorMessage,
  qk,
  useCommandMutation,
  useCommandQuery,
} from "@/shared/api";
import type {
  ClearMode,
  CommandArgs,
  LogMode,
  Retention,
  TraceDetail,
  TraceRow,
} from "@/shared/bindings";
import {
  HelpTip,
  outcomeDisplay,
  PageHeader,
  StatusDot,
  type SemStatus,
} from "@/shared/components";
import {
  formatBytes,
  formatInt,
  formatLatency,
  formatLocalTime,
  timezoneLabel,
} from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { useAppStore } from "@/stores/app";
import { radius, typography } from "@/theme";

import { TracePanel } from "./TracePanel";
import {
  DEFAULT_FILTER,
  LEVELS,
  LOG_MODES,
  RANGE_PRESETS,
  RETENTIONS,
  SEARCH_SCAN_ROWS,
  buildLogQuery,
  exportOutcome,
  filterFingerprint,
  filterSummary,
  logQueryArgs,
  mergeLogRows,
  newestRecordedMode,
  outcomeKnown,
  readLogRow,
  readStorageStats,
  readTraceDetail,
  searchTerm,
  type ExportOutcome,
  type Level,
  type LogsFilter,
  type LogRowView,
  type RangePreset,
  type StorageView,
} from "./model";

// ================================================================ 文案映射（显式表，不用模板拼 key）

const RANGE_LABEL: Record<RangePreset, I18nKey> = {
  today: "logs.filter.range.today",
  "7d": "logs.filter.range.7d",
  "30d": "logs.filter.range.30d",
  month: "logs.filter.range.month",
};

const LEVEL_LABEL: Record<Level, I18nKey> = {
  ok: "logs.filter.level.ok",
  retried: "logs.filter.level.retried",
  failed: "logs.filter.level.failed",
};

/** 结果语义：[15 §3.2] 绿=成功 · 黄=重试 · 红=失败。取值来自后端的 `outcome` 字段。 */
const OUTCOME_STATUS: Record<string, SemStatus> = { ok: "ok", retried: "warn", failed: "error" };
const OUTCOME_LABEL: Record<string, I18nKey> = {
  ok: "logs.filter.level.ok",
  retried: "logs.filter.level.retried",
  failed: "logs.filter.level.failed",
};

const MODE_DESC: Record<LogMode, I18nKey> = {
  off: "logs.mode.off.desc",
  meta: "logs.mode.meta.desc",
  shape: "logs.mode.shape.desc",
  full: "logs.mode.full.desc",
};

const RETENTION_LABEL: Record<Retention, I18nKey> = {
  "7d": "logs.retention.7d",
  "30d": "logs.retention.30d",
  "90d": "logs.retention.90d",
  forever: "logs.retention.forever",
};

/** 空数组常量：保持引用稳定，避免 `useMemo` 每帧重算。 */
const NO_ROWS: readonly TraceRow[] = [];

/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/** 一个统计格：标签（text.label）+ 值（等宽数字）。 */
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

/** 单条日志行展开后的内容：**轨迹（C4）+ 这一行的原始字段**。 */
export type LogsRowDetailProps = {
  view: LogRowView;
  /** 轨迹面板由页面注入（它要发 `log_get_trace`，钩子在数据层）。 */
  trace: ReactNode;
};

export function LogsRowDetail({ view, trace }: LogsRowDetailProps) {
  const { t } = useTranslation();

  return (
    <Box
      sx={{
        p: 4,
        bgcolor: "background.default",
        display: "flex",
        flexDirection: "column",
        gap: 4,
      }}
    >
      {trace}

      {/* 原始字段：即使轨迹读不到（保留期外/已清理），这一行的原始字段也必须看得到 */}
      <Stack spacing={1}>
        <Typography variant="h4">{t("logs.trace.raw")}</Typography>
        <Caption>{t("logs.trace.rawHint")}</Caption>
        <Box
          component="pre"
          data-testid="logs-row-raw"
          sx={{
            m: 0,
            p: 3,
            bgcolor: "background.surface",
            borderRadius: `${radius.control}px`,
            border: "1px solid",
            borderColor: "divider",
            fontFamily: typography.fontMono,
            fontSize: typography.size.meta,
            lineHeight: typography.lineHeight.table,
            color: "text.secondary",
            whiteSpace: "pre-wrap",
            overflowWrap: "anywhere",
            maxHeight: 320,
            overflow: "auto",
          }}
        >
          {JSON.stringify(view.raw, null, 2)}
        </Box>
      </Stack>
    </Box>
  );
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据"）

export type LogsQueryProps = {
  /** 表单草稿（**不影响查询**，按下「查询」才进 `applied`）。 */
  draft: LogsFilter;
  onDraftChange: (next: LogsFilter) => void;
  onSearch: () => void;
  onReset: () => void;
  /** 真正生效的条件（结果区显示它）。 */
  applied: LogsFilter;
  rows: LogRowView[];
  total: number | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  canLoadMore: boolean;
  isLoadingMore: boolean;
  onLoadMore: () => void;
};

export type LogsLogProps = {
  stats: StorageView | undefined;
  statsLoading: boolean;
  statsError: unknown;
  /** 用户本次点选的模式；`undefined` = 还没选 → 保存按钮不可用。 */
  mode: LogMode | undefined;
  onModeChange: (mode: LogMode) => void;
  onSaveMode: () => void;
  savingMode: boolean;
  /** **本次会话**写进后端的模式（`undefined` = 本次没写过 → 显示"未读取"，不猜）。 */
  savedMode: LogMode | undefined;
  retention: Retention | undefined;
  onRetentionChange: (retention: Retention) => void;
  onSaveRetention: () => void;
  savingRetention: boolean;
  savedRetention: Retention | undefined;
  /** 最近日志行**记录时**的模式（有证据的事实；没有行时为 `undefined`）。 */
  recordedMode: { mode: string; tsMs: number | undefined } | undefined;
  onClear: (mode: ClearMode) => void;
  clearing: boolean;
};

/**
 * 导出诊断包（`log_export`）。
 *
 * **路径为什么必须由用户填**：注册表里**没有**任何命令上报日志/数据目录
 * （`log_storage_stats` 只有条数与字节，`app_open_dir` 只打开窗口不返回路径），
 * 所以这里没有"默认值"可拿 —— 前端**不许**硬编 `%APPDATA%` 那类路径（那是在编造"后端会写到哪"）。
 * 空路径时按钮禁用，用户至少要知道路径是自己给的。
 *
 * **落盘路径以回执为准**：后端会把**目录**解析成 `tokenagency-diagnostic-<时间戳>.zip`
 * （`src-tauri/src/commands/settings_ext.rs` 的 `resolve_export_target`），
 * 所以"入参"根本不是"落点"；显示走 `exportOutcome(result, requestedPath)` 两条分支。
 */
export type LogsExportProps = {
  /** 用户填的导出路径（空 = 还没填）。 */
  path: string;
  onPathChange: (next: string) => void;
  /** 导出用的是**当前生效条件**（`applied`）的摘要 —— 由页面渲染好，纯函数不懂 i18n。 */
  appliedSummary: string;
  onRun: () => void;
  running: boolean;
  /** 归一化后的结果（`exportOutcome`）；`undefined` = 还没点过/正在重试。 */
  outcome: ExportOutcome | undefined;
  error: Error | undefined;
};

export type LogsBodyProps = {
  query: LogsQueryProps;
  log: LogsLogProps;
  export: LogsExportProps;
  expanded: {
    requestId: string | null;
    onToggle: (requestId: string) => void;
    renderTrace: (requestId: string) => ReactNode;
  };
};

export function LogsBody({ query, log, export: exportProps, expanded }: LogsBodyProps) {
  const { t } = useTranslation();
  const [clearOpen, setClearOpen] = useState(false);
  const [clearMode, setClearMode] = useState<ClearMode>("all");

  const requestIdFilled = query.draft.requestId.trim() !== "";
  const errorCodeFilled = query.draft.errorCode.trim() !== "";

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 查询条件（[07 §2.4]：时间范围 / 检索 / 状态） */}
      <Paper variant="outlined" sx={SECTION_SX} data-testid="logs-filter">
        <Stack spacing={3}>
          {/* 区块说明收进问号圆圈（[docs/22 §2]）：下面那几行说明里，
              "检索上限的条数"是数值（§2 第 5 条）、"off 的后果"是状态结论（第 1 条），
              它们照旧留在版面上 —— 这一块只收这一条"检索是怎么工作的"解释 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
            <Typography variant="h4">{t("logs.filter.title")}</Typography>
            <HelpTip text={t("logs.filter.errorCodeHint")} testId="logs-filter-help" />
          </Box>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
              gap: 3,
              alignItems: "start",
            }}
          >
            <TextField
              select
              label={t("logs.filter.range")}
              value={query.draft.preset}
              onChange={(e) =>
                query.onDraftChange({ ...query.draft, preset: e.target.value as RangePreset })
              }
            >
              {RANGE_PRESETS.map((preset) => (
                <MenuItem key={preset} value={preset}>
                  {t(RANGE_LABEL[preset])}
                </MenuItem>
              ))}
            </TextField>

            <TextField
              label={t("logs.filter.requestId")}
              placeholder={t("logs.filter.requestId.placeholder")}
              value={query.draft.requestId}
              disabled={errorCodeFilled}
              onChange={(e) => query.onDraftChange({ ...query.draft, requestId: e.target.value })}
            />

            <TextField
              label={t("logs.filter.errorCode")}
              placeholder={t("logs.filter.errorCode.placeholder")}
              value={query.draft.errorCode}
              disabled={requestIdFilled}
              onChange={(e) => query.onDraftChange({ ...query.draft, errorCode: e.target.value })}
            />

            <Box>
              <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
                {t("logs.filter.level")}
              </Typography>
              <Box sx={{ display: "flex", alignItems: "center", flexWrap: "wrap" }}>
                {LEVELS.map((level) => (
                  <FormControlLabel
                    key={level}
                    sx={{ mr: 4 }}
                    control={
                      <Checkbox
                        size="small"
                        checked={query.draft.levels.includes(level)}
                        onChange={(e) =>
                          query.onDraftChange({
                            ...query.draft,
                            levels: e.target.checked
                              ? [...query.draft.levels, level]
                              : query.draft.levels.filter((x) => x !== level),
                          })
                        }
                      />
                    }
                    label={t(LEVEL_LABEL[level])}
                  />
                ))}
              </Box>
            </Box>
          </Box>

          {/* 诚实说明：哪些限制是后端的事实（不是我们懒得做） */}
          <Stack spacing={1}>
            <Caption>{t("logs.filter.searchExclusive")}</Caption>
            <Caption>{t("logs.filter.searchScanLimit", { limit: formatInt(SEARCH_SCAN_ROWS) })}</Caption>
            <Caption>{t("logs.filter.levelHint")}</Caption>
            {/* off 的真后果：不落任何行 → 筛选必然空。写在这里，用户才不会以为是页面坏了 */}
            <Caption>{t("logs.filter.offHint")}</Caption>
          </Stack>

          <Box sx={{ display: "flex", gap: 3, alignItems: "center" }}>
            <Button variant="contained" onClick={query.onSearch}>
              {t("logs.filter.submit")}
            </Button>
            <Button variant="outlined" onClick={query.onReset}>
              {t("logs.filter.reset")}
            </Button>
          </Box>
        </Stack>
      </Paper>

      {/* ② 结果 */}
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 3, flexWrap: "wrap" }}>
            <Typography variant="h4">{t("logs.result.title")}</Typography>
            {query.total !== undefined ? (
              <Caption>{t("logs.result.total", { value: formatInt(query.total) })}</Caption>
            ) : null}
            <Caption>{t("logs.result.loaded", { value: formatInt(query.rows.length) })}</Caption>
            <Typography
              variant="caption"
              data-testid="logs-applied"
              sx={{ color: "text.secondary" }}
            >
              {t("logs.result.applied", {
                summary: filterSummary(query.applied, {
                  preset: t(RANGE_LABEL[query.applied.preset]),
                  levelLabel: t("logs.filter.level"),
                  levelValue: query.applied.levels.map((level) => t(LEVEL_LABEL[level])).join(" / "),
                  requestId: t("logs.filter.requestId"),
                  errorCode: t("logs.filter.errorCode"),
                  none: t("logs.result.appliedNone"),
                }),
              })}
            </Typography>
          </Box>

          {query.error !== undefined && query.error !== null ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {/* `errorMessage()` 自带 `[TA-xxxx]`（唯一格式化出口），不吞错误码 */}
              <Typography variant="body1" data-testid="logs-error" sx={{ color: "error.main" }}>
                {errorMessage(query.error)}
              </Typography>
              <Button variant="outlined" onClick={query.onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : query.isLoading && query.rows.length === 0 ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("logs.result.loading")}
            </Typography>
          ) : query.rows.length === 0 ? (
            /* 空状态：给出全部可能的原因，不给一个假的 0 行表格 */
            <Stack spacing={1} data-testid="logs-empty">
              <Typography variant="body1">{t("logs.empty.title")}</Typography>
              <Caption>{t("logs.empty.off")}</Caption>
              <Caption>{t("logs.empty.noTraffic")}</Caption>
              <Caption>{t("logs.empty.narrow")}</Caption>
            </Stack>
          ) : (
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" data-testid="logs-table">
                <TableHead>
                  <TableRow>
                    <TableCell padding="none" />
                    {/* 时间轴永远本地时区，且写在列头（[07 §1.3]） */}
                    <TableCell>{`${t("logs.col.time")}（${timezoneLabel()}）`}</TableCell>
                    <TableCell>{t("logs.col.requestId")}</TableCell>
                    <TableCell>{t("logs.col.model")}</TableCell>
                    <TableCell>{t("logs.col.channel")}</TableCell>
                    <TableCell>{t("logs.col.status")}</TableCell>
                    <TableCell align="right">{t("logs.col.latency")}</TableCell>
                    <TableCell align="right">{t("logs.col.tokens")}</TableCell>
                    <TableCell>{t("logs.col.error")}</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {query.rows.map((view) => {
                    const isOpen = expanded.requestId === view.requestId;
                    const raw = view.outcome;
                    const label =
                      raw === undefined
                        ? t("common.unknown")
                        : OUTCOME_LABEL[raw] !== undefined
                          ? t(OUTCOME_LABEL[raw]!)
                          : // 契约外的取值原样显示（是后端说的话，不改写、不猜）
                            raw;
                    const outcome = outcomeDisplay({
                      supportEnabled: outcomeKnown(view),
                      label,
                      unknownLabel: t("common.unknown"),
                    });

                    return (
                      <Fragment key={view.requestId}>
                        <TableRow hover data-testid="logs-row">
                          <TableCell padding="none">
                            <Tooltip title={isOpen ? t("logs.col.collapse") : t("logs.col.expand")}>
                              <IconButton
                                size="small"
                                aria-label={isOpen ? t("logs.col.collapse") : t("logs.col.expand")}
                                onClick={() => expanded.onToggle(view.requestId)}
                              >
                                {isOpen ? (
                                  <ExpandLessOutlinedIcon fontSize="small" />
                                ) : (
                                  <ExpandMoreOutlinedIcon fontSize="small" />
                                )}
                              </IconButton>
                            </Tooltip>
                          </TableCell>
                          <TableCell sx={{ fontVariantNumeric: "tabular-nums", color: "text.secondary" }}>
                            {view.tsMs === undefined ? DASH : formatLocalTime(view.tsMs)}
                          </TableCell>
                          <TableCell
                            sx={{
                              fontFamily: typography.fontMono,
                              overflowWrap: "anywhere",
                            }}
                          >
                            {view.requestId === "" ? DASH : view.requestId}
                          </TableCell>
                          <TableCell>{view.model ?? DASH}</TableCell>
                          <TableCell
                            sx={{ fontFamily: typography.fontMono, color: "text.secondary" }}
                          >
                            {view.channel ?? DASH}
                          </TableCell>
                          <TableCell>
                            <StatusDot status={OUTCOME_STATUS[raw ?? ""] ?? "off"} label={outcome.text} />
                          </TableCell>
                          <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                            {view.latencyMs === undefined ? DASH : formatLatency(view.latencyMs)}
                          </TableCell>
                          <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                            {view.tokens === undefined ? DASH : formatInt(view.tokens)}
                          </TableCell>
                          <TableCell sx={{ maxWidth: 280 }}>
                            {view.error === undefined ? (
                              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                                {DASH}
                              </Typography>
                            ) : (
                              <Tooltip title={view.error}>
                                <Typography
                                  variant="body1"
                                  noWrap
                                  sx={{ color: "error.main", overflowWrap: "anywhere" }}
                                >
                                  {view.error}
                                </Typography>
                              </Tooltip>
                            )}
                          </TableCell>
                        </TableRow>

                        {isOpen ? (
                          <TableRow>
                            <TableCell colSpan={9} sx={{ p: 0 }}>
                              <LogsRowDetail
                                view={view}
                                trace={expanded.renderTrace(view.requestId)}
                              />
                            </TableCell>
                          </TableRow>
                        ) : null}
                      </Fragment>
                    );
                  })}
                </TableBody>
              </Table>
            </Box>
          )}

          {query.rows.length > 0 ? (
            <Stack direction="row" spacing={3} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {query.canLoadMore ? (
                <Button variant="outlined" disabled={query.isLoadingMore} onClick={query.onLoadMore}>
                  {t("logs.result.loadMore")}
                </Button>
              ) : null}
              {/* 检索模式下后端不做分页（trace_store.rs:491-513）—— 如实说，别让用户干等"加载更多" */}
              {searchTerm(query.applied) !== undefined ? (
                <Caption>{t("logs.result.searchNoPaging")}</Caption>
              ) : null}
            </Stack>
          ) : null}
        </Stack>
      </Paper>

      {/* ③ 记录与留存（C1：占用 + 一键打开目录在页头；模式/保留期/清空在这里） */}
      <Paper variant="outlined" sx={SECTION_SX} data-testid="logs-storage">
        <Stack spacing={4}>
          {/* "这组数字算的是什么"收进问号圆圈（[docs/22 §2]）；脱敏说明与导出说明留在版面上 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
            <Typography variant="h4">{t("logs.storage.title")}</Typography>
            <HelpTip text={t("logs.storage.scope")} testId="logs-storage-help" />
          </Box>

          {log.statsError !== undefined && log.statsError !== null ? (
            <Typography variant="body1" data-testid="logs-stats-error" sx={{ color: "error.main" }}>
              {t("logs.storage.failed", { message: errorMessage(log.statsError) })}
            </Typography>
          ) : (
            <Box
              sx={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
                gap: 3,
              }}
            >
              <StatItem
                label={t("logs.storage.rows")}
                value={
                  log.stats?.rows !== undefined
                    ? formatInt(log.stats.rows)
                    : log.statsLoading
                      ? t("common.loading")
                      : DASH
                }
              />
              <StatItem
                label={t("logs.storage.bytes")}
                value={
                  log.stats?.bytes !== undefined
                    ? formatBytes(log.stats.bytes)
                    : log.statsLoading
                      ? t("common.loading")
                      : DASH
                }
              />
              <StatItem
                label={t("logs.storage.bodyBytes")}
                value={
                  log.stats?.bodyBytes !== undefined
                    ? formatBytes(log.stats.bodyBytes)
                    : log.statsLoading
                      ? t("common.loading")
                      : DASH
                }
              />
              <StatItem
                label={t("logs.storage.oldest")}
                value={
                  log.stats?.oldestTs !== undefined ? formatLocalTime(log.stats.oldestTs) : DASH
                }
              />
              <StatItem
                label={t("logs.storage.newest")}
                value={
                  log.stats?.newestTs !== undefined ? formatLocalTime(log.stats.newestTs) : DASH
                }
              />
            </Box>
          )}

          <Stack spacing={1}>
            <Caption>{t("logs.storage.sanitized")}</Caption>
          </Stack>

          {/* 记录模式：off / meta / shape / full。当前值**没有读取接口** → 只显示"未读取"
              与**本次会话写过的值**，另外单列一行"最近日志行记录时的模式"（有证据的事实） */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("logs.mode.title")}</Typography>
            <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
              {LOG_MODES.map((mode) => (
                <Button
                  key={mode}
                  variant={log.mode === mode ? "contained" : "outlined"}
                  onClick={() => log.onModeChange(mode)}
                  sx={{ fontFamily: typography.fontMono }}
                >
                  {mode}
                </Button>
              ))}
              <Button
                variant="outlined"
                disabled={log.mode === undefined || log.savingMode}
                onClick={log.onSaveMode}
              >
                {t("logs.mode.save")}
              </Button>
            </Box>
            <Stack spacing={1}>
              <Caption>
                {log.savedMode === undefined
                  ? t("logs.mode.currentUnknown")
                  : t("logs.mode.currentSet", { mode: log.savedMode })}
              </Caption>
              <Caption>
                {log.recordedMode === undefined
                  ? t("logs.mode.observedNone")
                  : t("logs.mode.observed", {
                      mode: log.recordedMode.mode,
                      time:
                        log.recordedMode.tsMs === undefined
                          ? t("common.unknown")
                          : formatLocalTime(log.recordedMode.tsMs),
                    })}
              </Caption>
              <Caption>{t("logs.mode.writeTime")}</Caption>
            </Stack>
            <Stack spacing={1}>
              {LOG_MODES.map((mode) => (
                <Box key={mode} sx={{ display: "flex", gap: 2, minWidth: 0 }}>
                  <Typography
                    variant="caption"
                    sx={{ fontFamily: typography.fontMono, minWidth: 48, color: "text.primary" }}
                  >
                    {mode}
                  </Typography>
                  <Caption>{t(MODE_DESC[mode])}</Caption>
                </Box>
              ))}
            </Stack>
          </Stack>

          {/* 保留期：同样没有读取接口 */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("logs.retention.title")}</Typography>
            <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
              {RETENTIONS.map((value) => (
                <Button
                  key={value}
                  variant={log.retention === value ? "contained" : "outlined"}
                  onClick={() => log.onRetentionChange(value)}
                >
                  {t(RETENTION_LABEL[value])}
                </Button>
              ))}
              <Button
                variant="outlined"
                disabled={log.retention === undefined || log.savingRetention}
                onClick={log.onSaveRetention}
              >
                {t("logs.retention.save")}
              </Button>
            </Box>
            <Caption>
              {log.savedRetention === undefined
                ? t("logs.retention.currentUnknown")
                : t("logs.retention.currentSet", {
                    value: t(RETENTION_LABEL[log.savedRetention]),
                  })}
            </Caption>
          </Stack>

          <Box>
            <Button variant="outlined" color="error" onClick={() => setClearOpen(true)}>
              {t("logs.clear.button")}
            </Button>
          </Box>

          {/* 导出诊断包（log_export）：真入口。路径由用户填（后端没有任何目录可上报），
              落盘路径以**后端回执**为准 —— 没回执就只说"已请求"，不说"已导出成功"。 */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("logs.export.title")}</Typography>
            <Caption>{t("logs.export.query", { summary: exportProps.appliedSummary })}</Caption>
            <Box data-testid="logs-export-path">
              <TextField
                fullWidth
                label={t("logs.export.path")}
                placeholder={t("logs.export.path.placeholder")}
                value={exportProps.path}
                onChange={(event) => exportProps.onPathChange(event.target.value)}
              />
            </Box>
            <Caption>{t("logs.export.path.hint")}</Caption>
            <Box>
              <Button
                data-testid="logs-export-run"
                variant="outlined"
                disabled={exportProps.path.trim() === "" || exportProps.running}
                onClick={exportProps.onRun}
              >
                {exportProps.running ? t("logs.export.running") : t("logs.export.run")}
              </Button>
            </Box>
            {exportProps.error !== undefined ? (
              <Typography
                variant="body1"
                data-testid="logs-export-error"
                sx={{ color: "error.main" }}
              >
                {t("logs.export.error", { message: errorMessage(exportProps.error) })}
              </Typography>
            ) : exportProps.outcome === undefined ? null : exportProps.outcome.kind === "written" ? (
              <Typography
                variant="body1"
                data-testid="logs-export-written"
                data-path={exportProps.outcome.path}
                data-bytes={
                  exportProps.outcome.bytes === undefined ? "" : String(exportProps.outcome.bytes)
                }
              >
                {exportProps.outcome.bytes === undefined
                  ? t("logs.export.writtenNoBytes", { path: exportProps.outcome.path })
                  : t("logs.export.written", {
                      path: exportProps.outcome.path,
                      bytes: formatInt(exportProps.outcome.bytes),
                    })}
              </Typography>
            ) : (
              <Typography
                variant="body1"
                data-testid="logs-export-requested"
                data-path={exportProps.outcome.path}
                sx={{ color: "warning.main" }}
              >
                {t("logs.export.requested", { path: exportProps.outcome.path })}
              </Typography>
            )}
          </Stack>
        </Stack>

        <Dialog open={clearOpen} onClose={() => setClearOpen(false)}>
          <DialogTitle>{t("logs.clear.confirmTitle")}</DialogTitle>
          <DialogContent>
            <Typography variant="body1" sx={{ mb: 2 }}>
              {t("logs.clear.confirmBody")}
            </Typography>
            <RadioGroup
              value={clearMode}
              onChange={(e) => setClearMode(e.target.value as ClearMode)}
            >
              <FormControlLabel
                value="all"
                control={<Radio size="small" />}
                label={t("logs.clear.mode.all")}
              />
              <FormControlLabel
                value="bodies_only"
                control={<Radio size="small" />}
                label={t("logs.clear.mode.bodies_only")}
              />
            </RadioGroup>
          </DialogContent>
          <DialogActions>
            <Button onClick={() => setClearOpen(false)}>{t("logs.clear.cancel")}</Button>
            <Button
              variant="contained"
              color="error"
              disabled={log.clearing}
              onClick={() => {
                setClearOpen(false);
                log.onClear(clearMode);
              }}
            >
              {t("logs.clear.confirm")}
            </Button>
          </DialogActions>
        </Dialog>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

/**
 * 展开某一行的轨迹（C4）。
 *
 * 只在展开时挂载 → 只在展开时发一次 `log_get_trace`（不会为列表每行都打一次后端）。
 * 真机上返回的是 `Option<TraceDetail>`（`src-tauri/src/commands/log.rs:26-31`），
 * 即**可能是 `null`**（契约里没写这一层）—— 这里如实区分"没有这条"与"读失败"。
 */
export function TraceSection({ requestId }: { requestId: string }) {
  const trace = useCommandQuery("log_get_trace", { request_id: requestId }, qk.trace(requestId));
  const detail = (trace.data ?? null) as TraceDetail | null;

  return (
    <TracePanel
      detail={detail === null ? null : readTraceDetail(detail)}
      isLoading={trace.isPending}
      error={trace.error ?? undefined}
      onRetry={() => void trace.refetch()}
    />
  );
}

export default function LogsPage() {
  const { t } = useTranslation();
  const pushToast = useAppStore((s) => s.pushToast);

  const [draft, setDraft] = useState<LogsFilter>(DEFAULT_FILTER);
  const [applied, setApplied] = useState<LogsFilter>(DEFAULT_FILTER);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [mode, setMode] = useState<LogMode | undefined>(undefined);
  const [savedMode, setSavedMode] = useState<LogMode | undefined>(undefined);
  const [retention, setRetention] = useState<Retention | undefined>(undefined);
  const [savedRetention, setSavedRetention] = useState<Retention | undefined>(undefined);
  /** 导出路径：**没有默认值**（后端不上报目录），空着时按钮禁用。 */
  const [exportPath, setExportPath] = useState("");
  /** 归一化后的导出结果；`undefined` = 还没点过。 */
  const [exportResult, setExportResult] = useState<ExportOutcome | undefined>(undefined);

  const fingerprint = filterFingerprint(applied);

  // 第一页：跟随生效条件；事件（request:new / log:append）到达时由事件桥失效后自动重取
  const page = useCommandQuery("log_query", logQueryArgs(applied), qk.logs(fingerprint));
  const stats = useCommandQuery("log_storage_stats", undefined, qk.logStats);

  // 「加载更多」的增量页：条件一变就作废（比较 fingerprint，不用 effect，避免竞态）
  const [extra, setExtra] = useState<{ fingerprint: string; rows: TraceRow[]; cursor: string | null }>(
    { fingerprint: "", rows: [], cursor: null },
  );
  const extraRows = extra.fingerprint === fingerprint ? extra.rows : NO_ROWS;
  const rows = useMemo(
    () => mergeLogRows(page.data?.rows ?? NO_ROWS, extraRows),
    [page.data, extraRows],
  );
  const views = useMemo(() => rows.map(readLogRow), [rows]);
  const nextCursor =
    extraRows.length > 0 ? extra.cursor : (page.data?.cursor ?? null);
  const canLoadMore = typeof nextCursor === "string" && nextCursor !== "";

  const loadMore = useCommandMutation("log_query");
  const saveMode = useCommandMutation("log_set_mode");
  const saveRetention = useCommandMutation("log_set_retention");
  const clearLogs = useCommandMutation("log_clear");
  const openDir = useCommandMutation("app_open_dir");
  const runExport = useCommandMutation("log_export");

  const refreshAll = () => {
    void page.refetch();
    void stats.refetch();
  };

  const onLoadMore = () => {
    if (!canLoadMore || nextCursor === null) return;
    loadMore.mutate(logQueryArgs(applied, { cursor: nextCursor }), {
      onSuccess: (result) =>
        setExtra((prev) =>
          prev.fingerprint === fingerprint
            ? {
                fingerprint,
                rows: [...prev.rows, ...result.rows],
                cursor: result.cursor ?? null,
              }
            : { fingerprint, rows: result.rows, cursor: result.cursor ?? null },
        ),
      onError: (err) =>
        pushToast({
          level: "error",
          title: t("logs.result.loadMore"),
          body: errorMessage(err),
          dedupeKey: "logs.loadMore",
        }),
    });
  };

  const onSaveMode = () => {
    if (mode === undefined) return;
    saveMode.mutate(
      { mode },
      {
        onSuccess: () => {
          // 成功返回 = 后端接受了这次写入 → 记下"本次会话写过的值"（这是事实，不是推断）
          setSavedMode(mode);
          setMode(undefined);
          pushToast({
            level: "info",
            title: t("logs.mode.title"),
            body: t("logs.mode.applied", { mode }),
            dedupeKey: "logs.mode",
          });
          refreshAll();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.mode.title"),
            body: errorMessage(err),
            dedupeKey: "logs.mode.error",
          }),
      },
    );
  };

  const onSaveRetention = () => {
    if (retention === undefined) return;
    saveRetention.mutate(
      { retention },
      {
        onSuccess: () => {
          setSavedRetention(retention);
          setRetention(undefined);
          pushToast({
            level: "info",
            title: t("logs.retention.title"),
            body: t("logs.retention.applied", { value: t(RETENTION_LABEL[retention]) }),
            dedupeKey: "logs.retention",
          });
          refreshAll();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.retention.title"),
            body: errorMessage(err),
            dedupeKey: "logs.retention.error",
          }),
      },
    );
  };

  const onClear = (clearMode: ClearMode) => {
    clearLogs.mutate(
      { mode: clearMode },
      {
        onSuccess: (result) => {
          // 契约写的是 `void`，Rust 实际返回删除条数（`commands/log.rs:59`）→ 两种都如实处理
          const deleted = result as unknown;
          const count = typeof deleted === "number" ? deleted : undefined;
          pushToast({
            level: "info",
            title: t("logs.clear.title"),
            body:
              count === undefined
                ? t("logs.clear.doneUnknown")
                : t("logs.clear.done", { value: formatInt(count) }),
            dedupeKey: "logs.clear",
          });
          setExpandedId(null);
          refreshAll();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.clear.title"),
            body: errorMessage(err),
            dedupeKey: "logs.clear.error",
          }),
      },
    );
  };

  const onOpenDir = () => {
    openDir.mutate(
      { kind: "logs" },
      {
        onSuccess: () =>
          pushToast({
            level: "info",
            title: t("logs.storage.openDir"),
            body: t("logs.storage.openDir.done"),
            dedupeKey: "logs.openDir",
          }),
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.storage.openDir"),
            body: t("logs.storage.openDir.failed", { message: errorMessage(err) }),
            dedupeKey: "logs.openDir.error",
          }),
      },
    );
  };

  const recorded = newestRecordedMode(views);

  /**
   * 导出：用的是**当前生效条件**（`applied`）—— 与列表看到的是同一份筛选，不另算一个。
   * 落盘路径以**后端回执**为准（`exportOutcome` 两形态兼容）：后端把目录解析成
   * `tokenagency-diagnostic-<时间戳>.zip`，所以入参不是落点。
   */
  const onExport = () => {
    const path = exportPath.trim();
    if (path === "") return;
    setExportResult(undefined);
    const query = buildLogQuery(applied) as unknown as CommandArgs<"log_export">["query"];
    runExport.mutate(
      { query, path },
      { onSuccess: (result) => setExportResult(exportOutcome(result, path)) },
    );
  };

  return (
    <>
      <PageHeader
        title={t("nav.logs")}
        description={t("logs.description")}
        docRef="07 §2 请求日志 · 14 §12（C1 目录/占用 · C2 按码检索 · C4 轨迹）"
        actions={
          <>
            <Button variant="outlined" onClick={refreshAll}>
              {t("logs.refresh")}
            </Button>
            <Button variant="outlined" disabled={openDir.isPending} onClick={onOpenDir}>
              {t("logs.storage.openDir")}
            </Button>
          </>
        }
      />

      <LogsBody
        query={{
          draft,
          onDraftChange: setDraft,
          onSearch: () => {
            setApplied(draft);
            setExpandedId(null);
          },
          onReset: () => {
            setDraft(DEFAULT_FILTER);
            setApplied(DEFAULT_FILTER);
            setExpandedId(null);
          },
          applied,
          rows: views,
          total: page.data?.total,
          isLoading: page.isPending,
          error: page.error ?? undefined,
          onRetry: refreshAll,
          canLoadMore,
          isLoadingMore: loadMore.isPending,
          onLoadMore,
        }}
        log={{
          stats: readStorageStats(stats.data),
          statsLoading: stats.isPending,
          statsError: stats.error ?? undefined,
          mode,
          onModeChange: setMode,
          onSaveMode,
          savingMode: saveMode.isPending,
          savedMode,
          retention,
          onRetentionChange: setRetention,
          onSaveRetention,
          savingRetention: saveRetention.isPending,
          savedRetention,
          recordedMode:
            recorded === undefined
              ? undefined
              : { mode: recorded, tsMs: views.find((v) => v.mode !== undefined)?.tsMs },
          onClear,
          clearing: clearLogs.isPending,
        }}
        export={{
          path: exportPath,
          onPathChange: setExportPath,
          appliedSummary: filterSummary(applied, {
            preset: t(RANGE_LABEL[applied.preset]),
            levelLabel: t("logs.filter.level"),
            levelValue: applied.levels.map((level) => t(LEVEL_LABEL[level])).join(", "),
            requestId: t("logs.filter.requestId"),
            errorCode: t("logs.filter.errorCode"),
            none: t("logs.result.appliedNone"),
          }),
          onRun: onExport,
          running: runExport.isPending,
          outcome: exportResult,
          error: runExport.error ?? undefined,
        }}
        expanded={{
          requestId: expandedId,
          onToggle: (requestId) =>
            setExpandedId((prev) => (prev === requestId ? null : requestId)),
          renderTrace: (requestId) => <TraceSection requestId={requestId} />,
        }}
      />
    </>
  );
}

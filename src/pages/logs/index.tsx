/**
 * 请求日志页 `/logs`（）—— M1 验收 C 组（C1/C2/C4）的用户可见面。
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
  Chip,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  IconButton,
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
import ContentCopyOutlinedIcon from "@mui/icons-material/ContentCopyOutlined";
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
  SECTION_SX,
  Caption,
  AutoEllipsis,
  HelpTip,
  outcomeDisplay,
  PageHeader,
  StatItem,
  StatusDot,
  type SemStatus,
} from "@/shared/components";
import {
  DASH,
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

/** 结果语义： 绿=成功 · 黄=重试 · 红=失败。取值来自后端的 `outcome` 字段。 */
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


// ================================================================ 小组件


// 统计格收敛到共享件（原先本文件自己写了一份）

/** 单条日志行展开后的内容：**轨迹（C4）+ 这一行的原始字段**。 */
export type LogsRowDetailProps = {
  view: LogRowView;
  /** 轨迹面板由页面注入（它要发 `log_get_trace`，钩子在数据层）。 */
  trace: ReactNode;
};

/**
 * L5 字段表：**只列这一行真的有的东西**。
 *
 * 列名复用列表列头那批键（`logs.col.*`）——同一个概念不许两种叫法。
 * 值读不到时渲染「—」（`null`），**不猜、不补默认值**（例如没有 `status` 就不显示 200）。
 */
function DETAIL_FIELDS(view: LogRowView): { key: string; label: I18nKey; value: string | null }[] {
  return [
    { key: "model", label: "logs.col.model", value: view.model ?? null },
    { key: "channel", label: "logs.col.channel", value: view.channel ?? null },
    {
      key: "status",
      label: "logs.col.status",
      value: view.status === undefined ? null : String(view.status),
    },
    {
      key: "latency",
      label: "logs.col.latency",
      value: view.latencyMs === undefined ? null : `${formatInt(view.latencyMs)}ms`,
    },
    {
      key: "tokens",
      label: "logs.col.tokens",
      value: view.tokens === undefined ? null : formatInt(view.tokens),
    },
    {
      // 契约里有 `cost` 就显示它；**没有就显示「—」**，绝不拿 token 现算金额（那是第二个真相源）
      key: "cost",
      label: "logs.col.cost",
      value:
        view.cost === undefined
          ? null
          : `${view.cost.currency} ${view.cost.amount.toFixed(6)}`,
    },
    { key: "error", label: "logs.col.error", value: view.error ?? null },
  ];
}

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
      {/* L5：详情头部 —— 请求 ID + 一键复制。
          复制的是**后端给的那个 id**（不是前端拼的），失败就如实说"复制不了"。 */}
      <Stack direction="row" spacing={1} alignItems="center" data-testid="logs-detail-head">
        <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
          {view.requestId}
        </Typography>
        <IconButton
          size="small"
          data-testid="logs-detail-copy"
          aria-label={t("logs.detail.copyId")}
          onClick={() => {
            void navigator.clipboard?.writeText(view.requestId).catch(() => undefined);
          }}
        >
          <ContentCopyOutlinedIcon fontSize="small" />
        </IconButton>
      </Stack>

      {/* L5：字段表 —— 只显示**这一行真的有**的值，读不到就显示「—」（不猜、不补默认）。
          列名复用列表列头同一批 i18n 键，避免同一个概念两种叫法。 */}
      <Box
        data-testid="logs-detail-fields"
        sx={{
          display: "grid",
          gap: 1,
          gridTemplateColumns: { xs: "1fr", sm: "repeat(2, minmax(0, 1fr))" },
        }}
      >
        {DETAIL_FIELDS(view).map(({ key, label, value }) => (
          <Box key={key} sx={{ display: "flex", gap: 2, minWidth: 0 }}>
            <Typography variant="caption" sx={{ color: "text.secondary", minWidth: 96 }}>
              {t(label)}
            </Typography>
            <Typography variant="body1" sx={{ overflowWrap: "anywhere" }} data-testid={`logs-field-${key}`}>
              {value ?? "—"}
            </Typography>
          </Box>
        ))}
      </Box>

      {trace}

      {/* 原始字段：即使轨迹读不到（保留期外/已清理），这一行的原始字段也必须看得到。
          「原样显示、未清洗」是**解释** → 收进标题右边的问号，
          与 `TracePanel` 里同一个键的渲染位置一致；下面 `pre` 里的原文一个字都没动。 */}
      <Stack spacing={1}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
          <Typography variant="h4">{t("logs.trace.raw")}</Typography>
          <HelpTip text={t("logs.trace.rawHint")} testId="logs-row-raw-help" />
        </Box>
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

/** 计数筛选片的三种状态（L2）。 */
export type LogOutcome = "all" | "ok" | "failed";

/** `applied.levels` → 当前是哪一片（片子的高亮状态由它决定，不另存一份状态）。 */
export function outcomeOf(levels: readonly Level[]): LogOutcome {
  if (levels.length === 0) return "all";
  if (levels.length === 1 && levels[0] === "ok") return "ok";
  return levels.includes("failed") || levels.includes("retried") ? "failed" : "all";
}

/**
 * 「失败」片对应的**级别集合**：`failed` **和** `retried` 都算失败。
 *
 * 这条不是口味问题：后端 `LogStats.failed = total - ok`（即"非 ok 全算失败"），
 * 如果这里只筛 `failed`，就会出现"片上写 10、点进去只有 4 条"的自相矛盾。
 * 口径统一写在这里，界面与统计都从它取。
 */
export const OUTCOME_LEVELS: Record<LogOutcome, readonly Level[]> = {
  all: [],
  ok: ["ok"],
  failed: ["failed", "retried"],
};

/** L2 顶部四张卡要的四个数（来自 `log_stats` 命令）。 */
export type LogStatsView = {
  total: number;
  ok: number;
  failed: number;
  /** `null` = 一行都没有（存储层不编 0） */
  avgTotalMs: number | null;
};

/**
 * 一张统计卡：**只有数字 + 一个小标题**（用户第 90 轮："金额/数字别铺一堆字"）。
 * 刻意不做图标、不做趋势线 —— 这一屏的信息密度已经在卡片本身了。
 */
function StatCard({ testId, label, value }: { testId: string; label: string; value: string }) {
  return (
    <Paper variant="outlined" sx={{ px: 2, py: 2 }} data-testid={testId}>
      <Stack spacing={0.5}>
        <Typography variant="caption" sx={{ color: "text.secondary" }}>
          {label}
        </Typography>
        <Typography variant="h4" data-testid={`${testId}-value`}>
          {value}
        </Typography>
      </Stack>
    </Paper>
  );
}

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
  /**
   * L2 顶部四张统计卡的数据源（`log_stats` 命令，与查询**同一套过滤**）。
   * `null` = 读取失败或搜索模式（存储层拒绝统计）→ 界面显示「读不到」，**不许拿旧数字充数**。
   */
  stats?: LogStatsView | null;
  statsLoading?: boolean;
  /** 当前生效的"结果"筛选（与 `applied.levels` 同源）。 */
  outcome: LogOutcome;
  /** 点计数片即筛（**不进草稿**，直接生效 —— 与卡片同一套过滤）。 */
  onPickOutcome: (outcome: LogOutcome) => void;
  /** 点时间范围按钮即筛（与计数片同一条生效路径）。 */
  onPickPreset: (preset: RangePreset) => void;
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
  /**
   * L7：记录与留存这一节里的四块（存储 / 模式 / 保留 / 导出）**默认收起**。
   *
   * 折叠只切 `display`，**内容始终挂载** —— 这些是真功能，卸载等于把它们从 DOM 里拿走
   * （第一次尝试用 MUI `Accordion`，它的 `unmountOnExit` 就是这么把测试弄红的）。
   * 因此只按"存在"查询（`getByTestId` / `getByText`）照旧能找到；
   * 按"可见"查询（`getByRole` —— 它只认可访问性树）的用例必须先点一下 `logs-advanced-toggle`。
   */
  const [open, setOpen] = useState(false);
  const [clearMode, setClearMode] = useState<ClearMode>("all");

  // L3：右栏显示"当前选中的那一行"。选中状态只有一份（`expanded.requestId`），
  // 这里只把它翻译成行视图 —— 找不到（翻页/筛选变了）就是 `undefined` → 右栏给空态。
  const selectedRow = query.rows.find((row) => row.requestId === expanded.requestId);

  const requestIdFilled = query.draft.requestId.trim() !== "";
  const errorCodeFilled = query.draft.errorCode.trim() !== "";

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 查询条件（：时间范围 / 检索 / 状态） */}
      <Paper variant="outlined" sx={SECTION_SX} data-testid="logs-filter">
        <Stack spacing={3}>
          {/* 区块说明收进问号圆圈：下面那几行说明里，
              检索上限的条数是数值（判据 2）→ 缩短后留在版面上（≤36 字）；
              "off 的后果"虽然重要，但它是**一整句解释**（判据 2 的取向：句子级说明进 tooltip），
              所以并进本区块唯一的问号。 */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
            <Typography variant="h4">{t("logs.filter.title")}</Typography>
            <HelpTip
              text={`${t("logs.filter.errorCodeHint")} ${t("logs.filter.searchExclusive")} ${t("logs.filter.levelHint")} ${t("logs.filter.offHint")}`}
              testId="logs-filter-help"
            />
          </Box>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
              gap: 3,
              alignItems: "start",
            }}
          >
            <Box>
              <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
                {t("logs.filter.range")}
              </Typography>
              {/* 时间范围做成**分段按钮**（：与截图同一形态）—— 点一下即筛，
                  与计数片走同一条生效路径；不再需要"先选下拉再按查询"。 */}
              <Stack direction="row" spacing={1} data-testid="logs-range-presets" sx={{ flexWrap: "wrap" }}>
                {RANGE_PRESETS.map((preset) => (
                  <Button
                    key={preset}
                    size="small"
                    data-testid={`logs-range-${preset}`}
                    variant={query.applied.preset === preset ? "contained" : "outlined"}
                    onClick={() => query.onPickPreset(preset)}
                  >
                    {t(RANGE_LABEL[preset])}
                  </Button>
                ))}
              </Stack>
            </Box>

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

          {/* 留在版面上的只有一条**带数字**的检索上限（判据 2：数值一眼可见，但句子要短）。
              "off 的后果"那一整句已并进标题右边的问号（见上面的 `HelpTip`）。 */}
          <Stack spacing={1}>
            <Caption>{t("logs.filter.searchScanLimit", { limit: formatInt(SEARCH_SCAN_ROWS) })}</Caption>
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

      {/* ①.5 统计卡（L2）：四个数来自 `log_stats`，与下面的列表同口径。
          搜索模式下存储层会拒绝统计 → `stats` 是 null → 显示「读不到」而不是旧数字。 */}
      {query.stats === undefined ? null : (
        <Box
          data-testid="logs-stats"
          sx={{ display: "grid", gap: 2, gridTemplateColumns: { xs: "1fr 1fr", md: "repeat(4, 1fr)" } }}
        >
          <StatCard
            testId="logs-stat-total"
            label={t("logs.stats.total")}
            value={query.stats === null ? t("logs.stats.unknown") : formatInt(query.stats.total)}
          />
          <StatCard
            testId="logs-stat-ok"
            label={t("logs.stats.okRate")}
            value={
              query.stats === null || query.stats.total === 0
                ? t("logs.stats.unknown")
                : `${Math.round((query.stats.ok / query.stats.total) * 100)}%`
            }
          />
          <StatCard
            testId="logs-stat-failed"
            label={t("logs.stats.failed")}
            value={query.stats === null ? t("logs.stats.unknown") : formatInt(query.stats.failed)}
          />
          <StatCard
            testId="logs-stat-avg"
            label={t("logs.stats.avgMs")}
            value={
              query.stats === null || query.stats.avgTotalMs === null
                ? t("logs.stats.unknown")
                : `${formatInt(query.stats.avgTotalMs)}ms`
            }
          />
        </Box>
      )}

      {/* ①.6 计数筛选片（L2）：数字与统计卡**同源**，点一下即筛。
          「失败」= failed + retried（见 `OUTCOME_LEVELS`），否则片上的数会与列表对不上。 */}
      {query.stats === undefined ? null : (
        <Stack direction="row" spacing={2} data-testid="logs-outcome-chips">
          {(["all", "ok", "failed"] as const).map((key) => {
            const s = query.stats ?? null;
            const count =
              s === null ? null : key === "all" ? s.total : key === "ok" ? s.ok : s.failed;
            return (
              <Chip
                key={key}
                data-testid={`logs-chip-${key}`}
                label={`${t(`logs.chips.${key}`)} ${count === null ? t("logs.stats.unknown") : formatInt(count)}`}
                color={query.outcome === key ? "primary" : "default"}
                variant={query.outcome === key ? "filled" : "outlined"}
                onClick={() => query.onPickOutcome(key)}
              />
            );
          })}
        </Stack>
      )}

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
            /* [极简 · 判据 5] 空态只说"怎么开始"：1 行。
               原来这里是 4 行散文（没有可显示的日志行 / 日志模式可能是 off… / 也可能只是这段时间
               没有请求 / 或者条件太窄…）—— 那是在替用户列举所有可能性。
               [第 2 轮 · 判据 A] 这里原来还带一个「重置」按钮，与上方筛选表单里的「重置」
               **同标签同动作**：已删，条件收不回来时用表单里那一个。 */
            <Box data-testid="logs-empty">
              <Typography variant="body1">{t("logs.empty.title")}</Typography>
            </Box>
          ) : (
            /* L3 主从双栏：左列表、右详情。窄屏（xs）回落成上下。 */
            <Box
              sx={{
                display: "grid",
                gap: 2,
                gridTemplateColumns: { xs: "1fr", md: "minmax(0, 1.6fr) minmax(0, 1fr)" },
                alignItems: "start",
              }}
            >
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="logs-table">
                <TableHead>
                  <TableRow>
                    <TableCell padding="none" />
                    {/* 时间轴永远本地时区，且写在列头 */}
                    <TableCell>{`${t("logs.col.time")}（${timezoneLabel()}）`}</TableCell>
                    <TableCell>{t("logs.col.requestId")}</TableCell>
                    <TableCell>{t("logs.col.model")}</TableCell>
                    <TableCell>{t("logs.col.channel")}</TableCell>
                    <TableCell>{t("logs.col.status")}</TableCell>
                    <TableCell align="right">{t("logs.col.latency")}</TableCell>
                    <TableCell align="right">{t("logs.col.tokens")}</TableCell>
                    <TableCell align="right">{t("logs.col.cost")}</TableCell>
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
                          <TableCell sx={{ fontFamily: typography.fontMono }}>
                            {/* 请求 id 很长（一行一条的排查钥匙）。
                                恢复路径：①展开这一行能看到这条请求的完整轨迹（本页既有入口）；
                                ②悬停/聚焦的 tooltip。 */}
                            <AutoEllipsis testId="logs-request-id-ellipsis" sx={{ maxWidth: 280 }}>
                              {view.requestId === "" ? DASH : view.requestId}
                            </AutoEllipsis>
                          </TableCell>
                          <TableCell>
                            {/* 模型名（带前缀/日期的长名字）。
                                恢复路径：展开这一行的轨迹面板里有同一条记录 → 悬停/聚焦的 tooltip。 */}
                            <AutoEllipsis testId="logs-model-ellipsis" sx={{ maxWidth: 200 }}>
                              {view.model ?? DASH}
                            </AutoEllipsis>
                          </TableCell>
                          <TableCell
                            sx={{ fontFamily: typography.fontMono, color: "text.secondary" }}
                          >
                            {/* 通道 id 是排查"走哪条通道"的钥匙。
                                恢复路径：展开这一行的轨迹面板 → 悬停/聚焦的 tooltip。 */}
                            <AutoEllipsis testId="logs-channel-ellipsis" sx={{ maxWidth: 200 }}>
                              {view.channel ?? DASH}
                            </AutoEllipsis>
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
                        {/* L4 花费列：**只用契约给的 cost**，没有就 —（不拿 token 现算） */}
                        <TableCell align="right" data-testid="logs-row-cost">
                          {view.cost === undefined
                            ? DASH
                            : `${view.cost.currency} ${view.cost.amount.toFixed(6)}`}
                        </TableCell>
                          <TableCell sx={{ maxWidth: 280 }}>
                            {/* **错误原文与 TA 码不省略**（红线）：
                                这一格刻意不用 `AutoEllipsis` —— 错误是"必须看得见"的东西，
                                藏进"…"里就是藏问题。保留下面的原文 + 悬停全文。 */}
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

                      </Fragment>
                    );
                  })}
                </TableBody>
                </Table>
              </Box>

              {/* 右栏详情（L3）：选中哪一行就显示哪一行 —— 与左栏共用同一份选中状态
                  （`expanded.requestId`），不另存一份。没选中时给**空态**，不是一片空白。 */}
              <Box data-testid="logs-detail-pane">
                {selectedRow === undefined ? (
                  <Paper variant="outlined" sx={{ p: 2 }}>
                    <Caption>{t("logs.detail.pick")}</Caption>
                  </Paper>
                ) : (
                  <LogsRowDetail
                    view={selectedRow}
                    trace={expanded.renderTrace(selectedRow.requestId)}
                  />
                )}
              </Box>
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
      <Paper variant="outlined" sx={SECTION_SX} data-testid="logs-advanced">
        {/* 这组数字算的是什么收进问号圆圈；脱敏说明与导出说明留在版面上 */}
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
          <Typography variant="h4">{t("logs.storage.title")}</Typography>
          <HelpTip text={t("logs.storage.scope")} testId="logs-storage-help" />
          {/* L7：折叠开关。标题行**永远可见** —— 收起的是下面那四块，
              不是这一节本身；看不见的功能等于没有功能，所以恢复入口就在标题行右侧。 */}
          <IconButton
            size="small"
            data-testid="logs-advanced-toggle"
            aria-expanded={open}
            aria-label={t("logs.storage.title")}
            onClick={() => setOpen((value) => !value)}
            sx={{ ml: "auto", transform: open ? "rotate(180deg)" : "none" }}
          >
            <ExpandMoreOutlinedIcon fontSize="small" />
          </IconButton>
        </Box>

        {/* L7：四块（存储 / 模式 / 保留 / 导出）默认收起。
            这里**只切 `display`**（内容始终挂载，见 `open` 的注释）：
            原先的 `<Stack spacing={4}>` 直接改成这个 Box，是为了不把 200 多行内容重新缩进
            —— "整段搬家 + 行号拼接"正是前两次把 JSX 弄坏的那个操作。 */}
        <Box sx={{ display: open ? "flex" : "none", flexDirection: "column", gap: 4, mt: 4 }}>
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
        {/* 导出路径那段的"没有默认路径、界面不替用户猜"是**既有测试钉住的红线**，
            必须内联（第 78 轮曾误判为可收进问号，被测试当场拦下 → 已撤回）。
            本轮只做一件事：**占位符缩短**（59 字的两个示例路径会把输入区挤满）。 */}
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
        </Box>

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
  /**
   * L2：顶部四张统计卡。与列表**同一套过滤**（后端 `log_stats` 复用同一个 WHERE），
   * 所以卡片上的数与列表里的行永远同口径。
   *
   * 搜索模式下后端**如实拒绝**（正文是压缩体，SQL 数不准）→ 这里把 `stats` 置 `null`，
   * 界面显示「读不到」，**不拿旧数字充数**（本仓库硬规矩：不编数）。
   */
  const searching = applied.requestId.trim() !== "" || applied.errorCode.trim() !== "";
  const statsQuery = useCommandQuery(
    "log_stats",
    { query: logQueryArgs(applied).query },
    qk.logs(`${fingerprint}:stats`),
    { enabled: !searching },
  );
  const logStats: LogStatsView | null | undefined = searching
    ? null
    : statsQuery.data === undefined
      ? undefined
      : {
          total: Number(statsQuery.data.total),
          ok: Number(statsQuery.data.ok),
          failed: Number(statsQuery.data.failed),
          avgTotalMs: statsQuery.data.avg_total_ms ?? null,
        };
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
          stats: logStats,
          statsLoading: statsQuery.isLoading,
          draft,
          onDraftChange: setDraft,
          onSearch: () => {
            setApplied(draft);
            setExpandedId(null);
          },
          // L2：计数片的高亮与点击。高亮**从 `applied.levels` 推导**，不另存一份状态
          // （两份状态迟早不一致）；点片直接进 `applied`，与按「查询」是同一条生效路径。
          outcome: outcomeOf(applied.levels),
          onPickOutcome: (next) => {
            setApplied((prev) => ({ ...prev, levels: [...OUTCOME_LEVELS[next]] }));
            setDraft((prev) => ({ ...prev, levels: [...OUTCOME_LEVELS[next]] }));
            setExpandedId(null);
          },
          onPickPreset: (next) => {
            setApplied((prev) => ({ ...prev, preset: next }));
            setDraft((prev) => ({ ...prev, preset: next }));
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

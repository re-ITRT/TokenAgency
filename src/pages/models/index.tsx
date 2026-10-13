/**
 * 模型目录页 `/models`（/）。候选路径见 `src/app/routes.ts`：
 * `pages: ["models", "models/index"]` —— 本文件是**文件夹形式**（与 `logs/index.tsx`、
 * `providers/index.tsx` 一致）。纯逻辑与整形在 `./model.ts`，那一层是单测的主要对象。
 *
 * ## 这一页的职责
 *
 * **模型是在这里"分配"出来的**：服务商与线路在服务商页创建，**模型不自动创建** ——
 * 用户在这一页把若干条**已经存在**的线路指定给一个模型（≥1 条）。
 * 所以这一页：
 *
 * 1. **读** `model_list` / `pricing_unpriced_models` / `channel_list` / `provider_list` / `lane_list`；
 * 2. **写**只走 `model_upsert`（新增/覆盖，`lines` 是必备项）与 `model_delete`；
 * 3. **没有**任何"给模型挑网络代理"的控件（：代理是**线路**的属性，只在服务商页改）；
 *    本页也从不读写 `Model.default_lane`。
 *
 * 其余写命令（`model_import_from_provider` / `model_apply_import` / `model_bulk_update`）**已在后端登记**
 * （以 `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 为准，`src/app/commandSurface.guard.test.ts`
 * 机械检查），**入口尚未接线** —— 在那之前空状态里如实说明，**不放假按钮、也不放点了没反应的禁用占位**。
 *
 * 三条诚实纪律：
 * 1. **"用了哪几条线路"缺数据时不许说成"没有线路"**：`channel_list` 读失败时，
 *    这一列显示"—"并在概览里点明"这是缺数据，不是没有线路"。
 * 2. **两个定价来源（`model_list[].priced` 与 `pricing_unpriced_models`）不一致时不裁决**，
 *    把不一致本身显示出来。
 * 3. **本地筛选要自报家门**：筛选只作用在已加载的行上，界面上必须写明"不是后端检索"。
 */

import {
  Box,
  Button,
  Checkbox,
  Collapse,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControl,
  FormControlLabel,
  FormLabel,
  IconButton,
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
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { Model } from "@/shared/bindings";
import { SwitchControl, SECTION_SX, Caption, AutoEllipsis, ConfirmDelete, HelpTip, PageHeader, StatItem, StatusDot } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { typography } from "@/theme";

import { PricingDialog } from "./PricingDialog";

import {
  DASH,
  EMPTY_MODEL_DRAFT,
  draftToModel,
  filterModels,
  hasDiscrepancy,
  lineCandidates,
  lineLabel,
  modelToDraft,
  pricingDiscrepancy,
  pricingVerdict,
  readModelRows,
  sortModels,
  summarizeModels,
  upstreamSummary,
  validateModelDraft,
  type ChannelsLookup,
  type ModelDraft,
  type ModelDraftIssue,
  type ModelLineRef,
  type ModelRowView,
  type PricingVerdict,
} from "./model";

import { i18nKey } from "@/shared/i18n";

/** 本地预检的 issue → 文案键。**不解析文案本身**（按机器可读的 field/reason 取键）。 */
const ISSUE_KEYS: Record<string, I18nKey> = {
  "id.required": "models.add.issue.id.required",
  "id.shape": "models.add.issue.id.shape",
  "id.duplicate": "models.add.issue.id.duplicate",
  "order.notInteger": "models.add.issue.order.notInteger",
  "order.negative": "models.add.issue.order.negative",
  // 线路是这一页新加的一条硬规则，文案在 `models.lines.*` 域下
  "lines.required": i18nKey("models.lines.required"),
};

/** 这条 issue 的文案键（取不到就**原样抛出键名**，不静默吞掉一个本该显示的错）。 */
function issueKey(issue: ModelDraftIssue): I18nKey {
  return ISSUE_KEYS[`${issue.field}.${issue.reason}`] ?? i18nKey(`models.add.issue.${issue.field}`);
}


// ================================================================ 删除

/** 表头全选复选框的无障碍标签：用 `common.selectAll`（见 `providers/index.tsx` 同一常量说明）。 */
const SELECT_ALL_LABEL_KEY: I18nKey = "common.selectAll";

/** 一次删除的目标（单条与批量共用同一个形状）。 */
type DeleteTarget = {
  readonly ids: readonly string[];
  readonly label: string;
  /** 批量**没有**级联按钮（：批量一律 `force=false`）。 */
  readonly batch: boolean;
  readonly consequences: readonly string[];
};

/** 执行完的**事实**（不带文案：文案由页面用 `t(...)` 拼）。 */
type DeleteOutcome = {
  deletedIds: string[];
  failures: { id: string; message: string }[];
};

/**
 * 逐条执行删除：**一条失败不中止其余条**、**批量一律 `force=false`**。
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


/** 一个统计格：标签（text.label）+ 值（等宽数字）。 */
// 统计格收敛到共享件（原先本文件自己写了一份）

/** 只读信息面板里的一行：上标签、下取值（**没有输入控件** —— 这一块只给人看）。 */
function InfoRow({
  label,
  testId,
  children,
}: {
  label: string;
  testId: string;
  children: ReactNode;
}) {
  return (
    <Box
      data-testid={testId}
      sx={{ display: "flex", flexDirection: "column", gap: 1, minWidth: 0 }}
    >
      <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
        {label}
      </Typography>
      {children}
    </Box>
  );
}

/**
 * 对外暴露信息里的**一行**：左标签 + 右取值。
 *
 * 与 [`InfoRow`] 的区别：这里的标签是**同一个区块内的次级字段**（不是一整块），所以标签用
 * `caption` 而不是 `overline`，并且固定一个最小宽度让五行能对齐。
 */
function InfoLine({
  label,
  value,
  testId,
}: {
  label: string;
  value: string;
  testId: string;
}) {
  return (
    <Box sx={{ display: "flex", gap: 2, minWidth: 0 }}>
      <Typography variant="caption" sx={{ color: "text.secondary", minWidth: 112 }}>
        {label}
      </Typography>
      <Typography
        variant="body1"
        data-testid={testId}
        sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
      >
        {value}
      </Typography>
    </Box>
  );
}

/**
 * 上下文窗口 / 最大输出：**没声明就写「未声明」**（`null` 与 0 是两件事，见）。
 */
function tokenCount(value: number | null, t: (key: I18nKey) => string): string {
  return value === null ? t("models.info.public.undeclared") : formatInt(value);
}

/**
 * 能力一格：只把**真的支持**的列出来；一个都不支持时写「无」（不是空着让人以为没加载）。
 */
function capabilityText(
  info: ModelRowView["publicInfo"],
  t: (key: I18nKey) => string,
): string {
  const parts: string[] = [];
  if (info.vision) parts.push(t("capabilities.vision"));
  if (info.tools) parts.push(t("capabilities.tools"));
  if (info.reasoning) parts.push(t("capabilities.reasoning"));
  return parts.length === 0 ? t("models.info.public.none") : parts.join(" · ");
}

/** 分组 + 排序合成一格（两者都缺时给破折号）。 */function groupOrderText(row: ModelRowView): string {
  const parts = [row.group, row.order === undefined ? undefined : formatInt(row.order)].filter(
    (v): v is string => v !== undefined && v !== "",
  );
  return parts.length === 0 ? DASH : parts.join(" · ");
}

/**
 * 上游映射一格。
 *
 * 只有一个上游名、而且它就等于逻辑模型名时，**不把同一个字符串再写一遍**：
 * 一页里出现两个文本完全相同的元素，用户读不出"这条线路到底有没有改名"，
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

/**
 * 一个开关。
 *
 * 把这个形状收敛成了共享件 [`SwitchControl`]（`@/shared/components`）。
 * 本文件原先自己写了一份，注释里写的理由是"另一份是 `providers/detail.tsx` 的私有组件，
 * 提成公共件是跨页重构、风险大于收益" —— 那个判断在**当时**是对的，
 * 但现在两处已经**各自漂移**（本文件恒隐藏文字、那一处有 `hideLabel`），
 * 而漂移点恰好是**无障碍名称**：开关视觉上不写"启用/关闭"，读屏的名称只能来自 `aria-label`，
 * 谁漏给一次，那一屏的开关对读屏用户就是"无名复选框"。
 * 共享件把 `label` 变成**必填参数**，从根上消掉这个漏法。见 的实测记录。
 */
function ModelSwitch(props: {
  testId: string;
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  // 本页的开关**恒不画文字**（版面上只有符号），名称走 `aria-label`。
  return <SwitchControl {...props} hideLabel />;
}

/**
 * 这个模型用了哪几条线路（**行里与只读信息面板共用同一份渲染**，避免两处说法不一致）。
 *
 * 逐条给出 `上游模型（服务商 · 网络代理）`；条数单独一行（用户第一眼要的是"几条"）。
 * 悬空引用（契约点名了但通道清单里没有）与自报条数对不上，都**如实点出来**，一个都不藏。
 */
function ModelLines({
  row,
  countTestId,
  testIdPrefix,
  onToggleLine,
  togglingLine,
}: {
  row: ModelRowView;
  countTestId: string;
  /** 逐条线路的 testid 前缀：`<prefix>-<线路 id>`，另加 `<prefix>-none/-mismatch/-missing`。 */
  testIdPrefix: string;
  /**
   * 打开/关闭这一条线路。**省略 = 只读** —— 只读信息面板复用这个组件，
   * 而那一页刻意一个可写控件都没有（有专门用例钉着）。
   */
  onToggleLine?: (lineId: string, enabled: boolean) => void;
  /** 正在提交的那条线路 id（按钮转圈 + 防重复点）。 */
  togglingLine?: string;
}) {
  const { t } = useTranslation();
  const disabled = t("models.channels.disabled");

  return (
    <Stack spacing={0.5}>
      <Typography variant="caption" data-testid={countTestId} sx={{ color: "text.secondary" }}>
        {t(i18nKey("models.lines.channelCount"), { count: row.lines.length })}
      </Typography>

      {/* 后端自报 0 条 = 这个模型发不出去（这是结论，不是缺数据） */}
      {row.lines.length === 0 && row.reportedLines === 0 ? (
        <Typography
          variant="caption"
          data-testid={`${testIdPrefix}-none`}
          sx={{ color: "text.secondary" }}
        >
          {t(i18nKey("models.lines.none"))}
        </Typography>
      ) : null}

      {/* 两个数对不上：如实点出，不挑一个信（文案沿用 `models.channels.mismatch`：
          它说的就是"模型清单自报 {{value}} 条、线路清单里一条都没匹配到"这件事） */}
      {row.lines.length === 0 && row.reportedLines > 0 ? (
        <Typography
          variant="caption"
          data-testid={`${testIdPrefix}-mismatch`}
          sx={{ color: "warning.main" }}
        >
          {t("models.channels.mismatch", { value: formatInt(row.reportedLines) })}
        </Typography>
      ) : null}

      {/* 契约点名、通道清单里却没有的线路 id：悬空引用，后端校验器也会报，这里不吞。
          说明一句（文案键），id 自己是**数据**、单独一格 —— 键还没落地时也照样看得见它。 */}
      {row.missingLines.length > 0 ? (
        <Stack spacing={0.5} data-testid={`${testIdPrefix}-missing`}>
          <Typography variant="caption" sx={{ color: "warning.main" }}>
            {t(i18nKey("models.lines.missing"))}
          </Typography>
          <Typography
            variant="caption"
            data-testid={`${testIdPrefix}-missing-ids`}
            sx={{
              fontFamily: typography.fontMono,
              color: "warning.main",
              overflowWrap: "anywhere",
            }}
          >
            {row.missingLines.join(" · ")}
          </Typography>
        </Stack>
      ) : null}

      {row.lines.map((ref: ModelLineRef) => (
        <Stack key={ref.lineId} direction="row" spacing={0.5} alignItems="center">
          <Typography
            data-testid={`${testIdPrefix}-${ref.lineId}`}
            variant="caption"
            sx={{
              fontFamily: typography.fontMono,
              lineHeight: typography.lineHeight.table,
              color: ref.enabled ? "text.primary" : "text.secondary",
              overflowWrap: "anywhere",
            }}
          >
            {lineLabel(ref, disabled)}
          </Typography>
          {onToggleLine === undefined ? null : (
            <ModelSwitch
              testId="models-line-enabled"
              label={ref.enabled ? t("common.enabled") : t("common.disabled")}
              checked={ref.enabled}
              disabled={togglingLine === ref.lineId}
              onChange={(next) => onToggleLine(ref.lineId, next)}
            />
          )}
        </Stack>
      ))}
    </Stack>
  );
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据/缺数据"）

export type ModelsBodyProps = {
  /** 模型清单（整形后的行）。 */
  rows: readonly ModelRowView[];
  /** 后端 `pricing_unpriced_models` 的原文 id；**`undefined` = 这一路读不到**（不宣称"全部已定价"）。 */
  unpricedIds: readonly string[] | undefined;
  /** 未定价清单读取失败的原因。 */
  unpricedError?: unknown;
  /** 通道清单读取失败的原因（此时"使用的线路"这一列不可用）。 */
  channelsError?: unknown;
  /** `model_list` 自身是否还在加载（空清单时用来区分"还没读完"与"确实没有"）。 */
  isLoading: boolean;
  /** `model_list` 读取失败的原因（读不到清单时不许给"配置里还没有模型"这种结论）。 */
  error: unknown;
  onRetry: () => void;
  /**
   * 行内「删除」。**省略 = 这一行没有删除入口** ——
   * 纯展示件不自己造写入口，页面（数据层）注入真处理器。
   */
  onDelete?: (row: ModelRowView) => void;
  /** 打开「编辑」（给已有模型加/减线路）：与删除一样由页面传下来，表格自己不发命令。 */
  onEdit?: (row: ModelRowView) => void;
  /**
   * 模型级总开关。**省略 = 只读**（信息面板那条路径刻意没有可写控件）。
   *
   * 契约 `Model` 里 `enabled` 是必填，所以在数据层发的是**整条覆盖写**
   * （`model_upsert`）：这条回调只负责"翻一下这一位"，其余字段由页面从 `row.raw` 带齐。
   */
  onToggleModel?: (row: ModelRowView, enabled: boolean) => void;
  /** 正在提交开关的模型 id（防重复点）。 */
  togglingModel?: string;
  /**
   * 线路开关：走 `channel_set_enabled`。
   *
   * **与模型开关是两回事**：线路可能被**多个模型共用**，关掉它会影响别的模型 ——
   * 页面负责把这件事说清楚（在确认框里），这里只负责把意图传上去。
   */
  onToggleLine?: (lineId: string, enabled: boolean) => void;
  /** 正在提交开关的线路 id。 */
  togglingLine?: string;
  /** 多选（行 `models-select` / 表头 `models-select-all` / 计数 `models-selected-count`）。 */
  selectedIds?: readonly string[];
  onToggleSelect?: (id: string) => void;
  /** 表头全选：把**当前筛选后看得见的那批 id** 交回数据层。 */
  onToggleSelectAll?: (visibleIds: readonly string[]) => void;
  /** 「删除选中」（`models-delete-selected`）：一次确认后**逐条**删。 */
  onDeleteSelected?: () => void;
  /**
   * 行内「定价 / 改价」。**省略 = 这一行没有定价入口**。
   *
   * 与删除/编辑同一条纪律：表格是纯展示件，不自己开对话框、也不发写命令 ——
   * 对话框挂在页面（数据层）上，因为它要 `useCommandQuery("pricing_list")`
   * 与 `pricing_upsert` / `pricing_delete` 两条写命令。
   */
  onPrice?: (row: ModelRowView) => void;
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
  onEdit,
  onToggleModel,
  togglingModel,
  onToggleLine,
  togglingLine,
  selectedIds,
  onToggleSelect,
  onToggleSelectAll,
  onDeleteSelected,
  onPrice,
}: ModelsBodyProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  /**
   * 「查看模型信息」：**只读**面板，从行里点进去看。
   *
   * 它没有写入口 —— 改关系要走 `model_upsert`（本页只做新增/覆盖的分配），
   * 代理更是线路自己的事，所以面板里连一个可编辑控件都没有。
   */
  const [infoRow, setInfoRow] = useState<ModelRowView | undefined>(undefined);

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
          {/* 「引用通道 / 未定价这两个数从哪来」是**解释** → 收进标题右边的问号；
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
              value={channelsMissing || listBroken ? DASH : formatInt(summary.lines)}
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

          {/* 缺数据必须说出来：这一列不可用 != 这个模型没有线路
              （后端原文 + TA 码是红线， → 永远内联） */}
          {channelsMissing ? (
            <Typography
              variant="body1"
              data-testid="models-lines-error"
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
            {/* 「未定价清单来自哪条命令」是解释 → 收进标题右边的问号。
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
                      <Caption block>
                        {t("models.mismatch.pricedButListed", {
                          ids: discrepancy.pricedButListed.join(" · "),
                        })}
                      </Caption>
                    ) : null}
                    {discrepancy.unpricedButNotListed.length > 0 ? (
                      <Caption block>
                        {t("models.mismatch.unpricedButNotListed", {
                          ids: discrepancy.unpricedButNotListed.join(" · "),
                        })}
                      </Caption>
                    ) : null}
                    <Caption block>{t("models.mismatch.hint")}</Caption>
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
            <Caption block>{t("models.search.scope", { value: formatInt(filtered.length) })}</Caption>
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
            /* [极简 · 判据 5] 空态只说"怎么开始"：1 行。
               原来这里是 4 行（含一句把 TODO 印出来的"本版本还没有接线…等后端命令落地后再补"，
               以及"模型目录来自配置的 models[]"这种实现交代）—— 全部删掉。
               [第 2 轮 · 判据 A] 这里原来还有一个实心「添加模型」按钮，与页头 `models-add-open`
               **同标签同动作** —— 同一屏两个 primary。已删：入口只留页头那一个
               （真机 E2E 与 `models.test.tsx` 都按页头那个点击）。 */
            <Box data-testid="models-empty">
              <Typography variant="body1">{t("models.empty.title")}</Typography>
            </Box>
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
              {/* 删除选中：选中数 + 「删除选中」 */}
              {onDeleteSelected === undefined ? null : (
                <Stack
                  direction="row"
                  spacing={2}
                  alignItems="center"
                  data-testid="models-delete-bar"
                  sx={{
                    // 长列表：批量操作**钉在顶上**，滚到第 100 行也点得到
                    //（它在表格上方，所以钉顶而不是钉底 —— 用户原话允许"在上面或者下面"）。
                    position: "sticky",
                    top: 0,
                    zIndex: 1,
                    flexWrap: "wrap",
                    py: 1,
                    bgcolor: "background.surface",
                    borderBottom: "1px solid",
                    borderColor: "divider",
                  }}
                >
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
                      {/* **总开关**单开一列，与"对外暴露"分开：
                          一个是"列不列进 /v1/models"，一个是"能不能用"。混在一列里，
                          用户就分不清"我只是藏起来"和"我把它关了"。 */}
                      <TableCell>{t(i18nKey("models.col.enabled"))}</TableCell>
                      <TableCell>{t("models.col.model")}</TableCell>
                      {/* 别名列**已撤掉**（用户第 309 轮：「别名那个没必要搞」）：
                          模型只讲对外 id，别名这种"客户端可能用的另一个名字"不再占一列。
                          历史别名仍在配置里，只读信息面板里还看得到（数据不动，只是不再要人管）。 */}
                      <TableCell>{t("models.col.group")}</TableCell>
                      <TableCell>{t("models.col.upstream")}</TableCell>
                      {/* 关系翻转后这一列是**模型侧的线路分配**，不再是"被谁引用" */}
                      <TableCell>{t(i18nKey("models.lines.title"))}</TableCell>
                      <TableCell>{t("models.col.priced")}</TableCell>
                      {/* 操作列表头留空（同服务商页）：不为一个表头去造 i18n 键 */}
                      <TableCell align="right" />
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
                      {/* 模型级总开关：关掉 = 网关不再列出它、调用它直接 404。
                          关掉**不删除**任何东西 —— 这正是这个开关存在的理由。 */}
                      <TableCell>
                        {onToggleModel === undefined ? (
                          <StatusDot
                            status={row.enabled ? "ok" : "off"}
                            label={
                              row.enabled ? t(i18nKey("models.enabled.on")) : t(i18nKey("models.enabled.off"))
                            }
                          />
                        ) : (
                          <ModelSwitch
                            testId="models-enabled"
                            label={
                              row.enabled ? t(i18nKey("models.enabled.on")) : t(i18nKey("models.enabled.off"))
                            }
                            checked={row.enabled}
                            disabled={togglingModel === row.id}
                            onChange={(next) => onToggleModel(row, next)}
                          />
                        )}
                      </TableCell>
                      <TableCell data-testid="models-id" sx={{ fontFamily: typography.fontMono }}>
                        {/* 模型 id 是最长的标识（`提供商/型号-日期`）。
                            恢复路径：行内的「查看模型信息」面板给全文（不靠 tooltip 猜）。 */}
                        <AutoEllipsis testId="models-id-ellipsis" sx={{ maxWidth: 280 }}>
                          {row.id}
                        </AutoEllipsis>
                      </TableCell>
                      <TableCell sx={{ color: "text.secondary" }}>{groupOrderText(row)}</TableCell>
                      <TableCell sx={{ fontFamily: typography.fontMono }}>
                        {/* 上游模型名（`upstream`）常常带前缀与日期。
                            恢复路径：行内的「查看模型信息」面板给全文。 */}
                        <AutoEllipsis testId="models-upstream-ellipsis" sx={{ maxWidth: 280 }}>
                          {upstreamCellText(row, t("models.upstream.same"))}
                        </AutoEllipsis>
                      </TableCell>
                      <TableCell sx={{ minWidth: 240 }}>
                        {channelsMissing ? (
                          <Typography variant="body1" sx={{ color: "text.secondary" }}>
                            {DASH}
                          </Typography>
                        ) : (
                          <ModelLines
                            row={row}
                            countTestId="models-lines-count"
                            testIdPrefix="models-lines"
                            onToggleLine={onToggleLine}
                            togglingLine={togglingLine}
                          />
                        )}
                      </TableCell>
                      <TableCell>
                        <StatusDot
                          status={PRICING_STATUS[verdict]}
                          label={t(PRICING_LABEL[verdict])}
                        />
                        {verdict === "unknown" ? (
                          <Caption block>{t("models.priced.unknownHint")}</Caption>
                        ) : null}
                        {row.costHint !== undefined ? (
                          <Caption block>{t("models.priced.costHint", { hint: row.costHint })}</Caption>
                        ) : null}
                      </TableCell>
                      <TableCell align="right">
                        <Stack direction="row" spacing={1} sx={{ justifyContent: "flex-end" }}>
                          {/* 定价：价格配置的唯一入口就在这里 —— 成本中心只做显示。
                              「改价」与「定价」是同一个入口：对话框打开时会回填当前价，
                              所以文案按"有没有价"分两种，用户一眼知道点下去是新建还是改。 */}
                          <Button
                            size="small"
                            variant="text"
                            data-testid="models-price-open"
                            onClick={() => onPrice?.(row)}
                          >
                            {verdict === "priced"
                              ? t(i18nKey("models.price.openEdit"))
                              : t(i18nKey("models.price.open"))}
                          </Button>
                          {/* 只读信息面板：点进去看这个模型到底用了哪几条线路 */}
                          <Button
                            size="small"
                            variant="text"
                            data-testid="models-info-open"
                            onClick={() => setInfoRow(row)}
                          >
                            {t(i18nKey("models.info.open"))}
                          </Button>
                          {/*
                            「编辑」：给**已有**模型加/减线路。
                            没有它，用户想用上一条新建的线路就只能删掉模型重建 —— 那会连带丢掉
                            别名/分组/顺序/expose。它与「查看」的区别就是**能改**，所以文案必须是
                            动词，不能也叫"查看模型信息"。
                          */}
                          <Button
                            size="small"
                            variant="text"
                            data-testid="models-edit-open"
                            onClick={() => onEdit?.(row)}
                          >
                            {t(i18nKey("models.edit.open"))}
                          </Button>
                          {onDelete === undefined ? null : (
                            <Button
                              size="small"
                              variant="outlined"
                              color="error"
                              data-testid="models-delete"
                              onClick={() => onDelete(row)}
                            >
                              {t("common.delete")}
                            </Button>
                          )}
                        </Stack>
                      </TableCell>
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

      {/*
        只读模型信息面板：模型 id / 别名 / 对外暴露 / 它用的线路。
        **一个可编辑控件都没有** —— 没有代理选择器（代理是线路的属性）、没有保存按钮：
        这个面板只回答"这个模型现在到底是怎么组成的"。收起来靠 Esc 或点面板外（对话框自带）。
      */}
      <Dialog
        open={infoRow !== undefined}
        onClose={() => setInfoRow(undefined)}
        fullWidth
        maxWidth="sm"
        data-testid="models-info-dialog"
      >
        <DialogTitle>{t(i18nKey("models.info.open"))}</DialogTitle>
        <DialogContent>
          {infoRow === undefined ? null : (
            <Stack data-testid="models-info-panel" spacing={2} sx={{ pt: 1 }}>
              <Caption block>{t(i18nKey("models.info.readonly"))}</Caption>

              <InfoRow label={t("models.col.model")} testId="models-info-id">
                <Typography
                  variant="body1"
                  sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                >
                  {infoRow.id}
                </Typography>
              </InfoRow>

              <InfoRow label={t("models.col.aliases")} testId="models-info-aliases">
                <Typography variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {infoRow.aliases.length === 0 ? DASH : infoRow.aliases.join(" · ")}
                </Typography>
              </InfoRow>

              <InfoRow label={t("models.col.expose")} testId="models-info-expose">
                <StatusDot
                  status={infoRow.expose ? "ok" : "off"}
                  label={infoRow.expose ? t("models.expose.on") : t("models.expose.off")}
                />
              </InfoRow>

              {/* **对外暴露的信息**：客户端在 `/v1/models` 里看到的就是这一份
                  （后端只有一个聚合口径，网关与这里用的是同一个函数）。
                  没声明的写「未声明」——**不编 0、不猜**。 */}
              <InfoRow label={t("models.info.public")} testId="models-info-public">
                <Stack spacing={1} sx={{ minWidth: 0 }}>
                  <InfoLine
                    label={t("models.info.public.context")}
                    value={tokenCount(infoRow.publicInfo.context_window, t)}
                    testId="models-info-public-context"
                  />
                  <InfoLine
                    label={t("models.info.public.maxOutput")}
                    value={tokenCount(infoRow.publicInfo.max_output_tokens, t)}
                    testId="models-info-public-max-output"
                  />
                  <InfoLine
                    label={t("models.info.public.caps")}
                    value={capabilityText(infoRow.publicInfo, t)}
                    testId="models-info-public-caps"
                  />
                  <InfoLine
                    label={t("models.info.public.modalities")}
                    value={
                      infoRow.publicInfo.modalities.length === 0
                        ? t("models.info.public.undeclared")
                        : infoRow.publicInfo.modalities.join(" · ")
                    }
                    testId="models-info-public-modalities"
                  />
                  <InfoLine
                    label={t("models.info.public.ownedBy")}
                    value={infoRow.publicInfo.owned_by}
                    testId="models-info-public-owned-by"
                  />
                </Stack>
              </InfoRow>

              <InfoRow
                label={t(i18nKey("models.lines.title"))}
                testId="models-info-lines"
              >
                {channelsMissing ? (
                  <Typography variant="body1" sx={{ color: "text.secondary" }}>
                    {DASH}
                  </Typography>
                ) : (
                  <ModelLines
                    row={infoRow}
                    countTestId="models-info-lines-count"
                    testIdPrefix="models-info-line"
                  />
                )}
              </InfoRow>
            </Stack>
          )}
        </DialogContent>
      </Dialog>
    </Box>
  );
}

// ================================================================ 数据层

export default function ModelsPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  /**
   * 正在定价的模型（`undefined` = 对话框关着）。
   *
   * **为什么定价在模型页**（用户第 318 轮：*"成本中心页面只做显示，价格配置到模型页面去"*）：
   * 定价的对象就是**这一行的模型**，id 现成，所以对话框不必再问"给哪个模型定价"。
   * 旧的实现让用户从服务商/凭据下拉里挑 —— 挑出来的东西与"这个模型花了多少钱"无关，
   * 这正是用户指出的那个错。
   */
  const [priceRow, setPriceRow] = useState<ModelRowView | undefined>(undefined);

  const models = useCommandQuery("model_list", undefined, qk.models);
  const channels = useCommandQuery("channel_list", undefined, qk.channels);
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  // `lane_list` 只用来把线路的 `lane` 翻成**网络代理的名字**（查不到就回落 id）。
  // 它是只读的，本页也不会拿它当"给模型选代理"的选项表。
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  const unpriced = useCommandQuery("pricing_unpriced_models", undefined, qk.unpricedModels);

  const upsert = useCommandMutation("model_upsert");
  /** 线路启停：与服务商页用的是**同一条**命令，不是新造一条。 */
  const setLineEnabled = useCommandMutation("channel_set_enabled");
  /** 正在提交开关的模型 / 线路 id（防重复点；空 = 没有在提交）。 */
  const [togglingModel, setTogglingModel] = useState<string | undefined>(undefined);
  const [togglingLine, setTogglingLine] = useState<string | undefined>(undefined);
  /** 开关失败的原因（共用一格：同一个操作序列里不会同时失败两件事）。 */
  const [toggleError, setToggleError] = useState<string | undefined>(undefined);
  /**
   * 待确认的线路启停（`undefined` = 没在问）。
   *
   * 为什么线路开关要**问一句**：线路可能被多个模型共用，关掉它会影响别的模型 ——
   * 那是用户必须知道的事，不能因为他点了一个模型页上的开关就悄悄改掉别的模型的可用性。
   */
  const [lineToggle, setLineToggle] = useState<
    { lineId: string; enabled: boolean } | undefined
  >(undefined);
  const [addOpen, setAddOpen] = useState(false);
  const [draft, setDraft] = useState<ModelDraft>(EMPTY_MODEL_DRAFT);
  /**
   * 创建/编辑对话框里的「高级（可选）」：分组 / 排序 / 对外暴露。
   *
   * 默认收起 —— 用户第 309 轮的要求是"创建模型的时候只输入模型名称和 id 就好"。
   * 这三个字段是元数据，要的人自己展开；**不删**是因为模型表与其它页面在读它们。
   * 别名更彻底：**连入口都不给**（见对话框里的注释）。
   */
  const [advanced, setAdvanced] = useState(false);
  const [issues, setIssues] = useState<readonly ModelDraftIssue[]>([]);
  /** 成功后回显的是**后端返回的那个 Model**，不是我们提交的草稿。 */
  const [added, setAdded] = useState<Model | undefined>(undefined);
  /**
   * 正在**编辑**的那个模型 id（`undefined` = 新建）。
   *
   *「模型……是等待用户去分配的」：线路总是后加的，所以"给已有模型加/减线路"
   * 必须有入口 —— 否则用户只能删掉模型重建，连带丢掉别名/分组/顺序/expose。
   * 编辑与新建**共用同一个对话框**（同一份校验、同一份载荷纪律），只有 id 只读。
   */
  const [editingId, setEditingId] = useState<string | undefined>(undefined);

  const lookup = useMemo<ChannelsLookup>(
    () => ({
      channels: channels.data ?? [],
      providers: providers.data ?? [],
      lanes: lanes.data ?? [],
    }),
    [channels.data, providers.data, lanes.data],
  );

  const rows = useMemo(() => readModelRows(models.data ?? [], lookup), [models.data, lookup]);

  const existingIds = useMemo(() => (models.data ?? []).map((m) => m.model.id), [models.data]);

  /** 候选线路：`channel_list` 全量（一条线路可以服务多个模型，所以不排除任何一条）。 */
  const candidates = useMemo(
    () => lineCandidates(lookup.channels, lookup),
    [lookup],
  );

  /**
   *（用户第 86 轮）**与「套餐与线路」同一套**：按服务商分组、可折叠、只留下
   * 已经打开的线路。
   *
   * 为什么"已选中的"也要留下：用户可能正是要**改一个已经用着关闭线路的模型** ——
   * 把那条藏起来，他点保存时就会在毫不知情的情况下把它摘掉（那是数据丢失，不是极简）。
   * 所以可见 = `enabled` **或** 已被当前草稿选中。
   */
  const visibleGroups = useMemo(() => {
    const wanted = candidates.filter((ref) => ref.enabled || draft.lines.includes(ref.lineId));
    const map = new Map<string, { providerId: string; providerName: string | undefined; refs: typeof wanted }>();
    for (const ref of wanted) {
      const found = map.get(ref.providerId);
      if (found === undefined) {
        map.set(ref.providerId, {
          providerId: ref.providerId,
          providerName: ref.providerName,
          refs: [ref],
        });
      } else {
        found.refs.push(ref);
      }
    }
    return [...map.values()];
  }, [candidates, draft.lines]);

  /** 收起了哪些服务商（默认全展开；只有用户主动收起才折起来 —— 与"默认不显示"是两回事）。 */
  const [collapsedProviders, setCollapsedProviders] = useState<readonly string[]>([]);
  const toggleProvider = (providerId: string): void => {
    setCollapsedProviders((prev) =>
      prev.includes(providerId) ? prev.filter((id) => id !== providerId) : [...prev, providerId],
    );
  };

  /**
   * `channel_list` 读不到时**如实说出来**：没有候选可选、提交会被本地预检挡住，
   * 用户必须知道这是"读不到"而不是"配置里一条线路都没有"——两者对用户是两件事。
   */
  const channelsBroken = channels.error !== null && channels.error !== undefined;

  const refresh = () => {
    void models.refetch();
    void channels.refetch();
    void providers.refetch();
    void lanes.refetch();
    void unpriced.refetch();
  };

  // ---------------------------------------------------------------- 开关

  /**
   * 模型级总开关。`model_upsert` 是**整条覆盖写**，所以要把 `row.raw` 原样带上、
   * 只翻 `enabled` 一位 —— 只发 `{id, enabled}` 会被后端当成"其余字段为空"，
   * 把别名/分组/线路**全部抹掉**。
   */
  const toggleModelEnabled = async (row: ModelRowView, enabled: boolean): Promise<void> => {
    setTogglingModel(row.id);
    setToggleError(undefined);
    try {
      await upsert.mutateAsync({ model: { ...row.raw, enabled } });
      // 清单上的"启用/停用"要立刻反映出来（模型开关影响的不只是这一行）
      for (const key of [qk.models, qk.providers]) {
        await qc.invalidateQueries({ queryKey: key });
      }
    } catch (err) {
      setToggleError(errorMessage(err));
    } finally {
      setTogglingModel(undefined);
    }
  };

  /**
   * 线路开关：走 `channel_set_enabled`（服务商页的线路启停用的同一条命令）。
   *
   * **先问一句**：线路是**共用**的（一条火山线路可能同时服务好几个模型），
   * 关掉它不只是"这个模型少一条"，别的模型也一起少 —— 这是用户必须知道的事，
   * 不能悄悄替他决定。所以先弹确认框，把"还有哪些模型在用"如实列出来。
   */
  const toggleLineEnabled = (lineId: string, enabled: boolean): void => {
    setLineToggle({ lineId, enabled });
  };

  const confirmLineToggle = async (): Promise<void> => {
    const target = lineToggle;
    if (target === undefined) return;
    setTogglingLine(target.lineId);
    setToggleError(undefined);
    try {
      await setLineEnabled.mutateAsync({ id: target.lineId, enabled: target.enabled });
      // 线路启停会同时影响：通道清单本身、各模型的线路状态、以及矩阵视图
      for (const key of [qk.channels, qk.models, qk.channelMatrix]) {
        await qc.invalidateQueries({ queryKey: key });
      }
      setLineToggle(undefined);
    } catch (err) {
      setToggleError(errorMessage(err));
    } finally {
      setTogglingLine(undefined);
    }
  };

  // ---------------------------------------------------------------- 删除

  const removeModel = useCommandMutation("model_delete");

  /** 多选（按 id 存，顺序 = 勾选顺序）。 */
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  /** 确认框里那一条（`undefined` = 框没开）。 */
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | undefined>(undefined);
  /** 后端原文（被引用时的错误）：有它才在对话框里显示。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  /** 结果区：每一次事实一行。 */
  const [deleteResult, setDeleteResult] = useState<readonly string[]>([]);
  /** 批量进行中（`mutateAsync` 逐条之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);

  /** 删除成功后要失效的查询。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    for (const key of [qk.models, qk.credentials, qk.providers, qk.channelMatrix]) {
      await qc.invalidateQueries({ queryKey: key });
    }
  };

  /**
   * 「会连带删除 / 影响」清单：**删模型不会删线路**。
   *
   * 线路是服务商那边定义的，模型只负责**用**它们 —— 所以这里如实写"线路不会被删"
   * （用户要能在点删除之前就知道这件事），并指路"要删线路去服务商界面"。
   * 通道清单读失败时一个字都不写（：算不出来不编）。
   */
  const consequencesOf = (row: ModelRowView): string[] => {
    if (channels.error !== null && channels.error !== undefined) return [];
    return row.lines.length === 0
      ? []
      : [t("delete.consequence.modelLinesUntouched", { value: formatInt(row.lines.length) })];
  };

  const openDelete = (target: DeleteTarget): void => {
    setDeleteTarget(target);
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
    // 单条失败时留着框（后端原文就在那里）；其余情况收框
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

  // ---------------------------------------------------------------- 添加（分配线路）

  /** 勾选/取消一条线路（顺序 = 勾选顺序，提交载荷与它逐字一致）。 */
  const toggleDraftLine = (lineId: string): void => {
    setDraft((prev) => ({
      ...prev,
      lines: prev.lines.includes(lineId)
        ? prev.lines.filter((id) => id !== lineId)
        : [...prev.lines, lineId],
    }));
  };

  const submitAdd = () => {
    // 编辑态：id 是本模型自己的 id（只读），不该被"重复 id"预检挡住
    const known = editingId === undefined ? existingIds : existingIds.filter((id) => id !== editingId);
    const found = validateModelDraft(draft, known);
    setIssues(found);
    // 本地预检不过（含"一条线路都没选"）→ **一次都不发**（与设置页 config_validate 的纪律一致）
    if (found.length > 0) return;
    upsert.mutate(
      { model: draftToModel(draft) },
      {
        onSuccess: (saved) => {
          setAdded(saved);
          setDraft(EMPTY_MODEL_DRAFT);
          setEditingId(undefined);
          setIssues([]);
          void qc.invalidateQueries({ queryKey: qk.models });
        },
      },
    );
  };

  /** 打开「编辑」：把该模型的**全部字段**灌进同一份草稿（少灌一个 = 提交时会抹掉它）。 */
  const openEdit = (row: ModelRowView) => {
    setDraft(modelToDraft(row.raw));
    setAdded(undefined);
    setIssues([]);
    setEditingId(row.id);
    setAddOpen(true);
  };

  /**
   * 模型清单**读不到**（与 `channelsBroken` 同一套判据）：用来区分"没有别的模型在用"
   * 与"我们根本不知道谁在用"。两者对用户是两件事，不能合并成同一句话。
   */
  const modelsBroken = models.error !== null && models.error !== undefined;

  /**
   * 待确认的线路开关要用的"还有谁在用"：从**已加载的模型清单**里算，不另发命令。
   *
   * 读不到模型清单时这里会是空数组 —— 那时对话框按"读不到"的文案显示，
   * **不写"没有别的模型在用"**：那是结论，而我们没有依据。
   */
  const sharedWith = useMemo(() => {
    if (lineToggle === undefined) return [] as string[];
    return rows
      .filter((row) => row.lines.some((line) => line.lineId === lineToggle.lineId))
      .map((row) => row.id);
  }, [rows, lineToggle]);

  return (
    <>
      <PageHeader
        title={t("nav.models")}
        description={t("models.description")}
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
        onEdit={openEdit}
        onToggleModel={(row, enabled) => void toggleModelEnabled(row, enabled)}
        togglingModel={togglingModel}
        onToggleLine={toggleLineEnabled}
        togglingLine={togglingLine}
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
        onPrice={(row) => setPriceRow(row)}
      />

      {/*
        定价对话框：模型来自被点的那一行，**用户改不了它**。
        挂在数据层（而不是 `ModelsBody`）的理由与删除/编辑一样：它要自己的查询
        （`pricing_list`，用来回填当前价 + 定位删除下标）与两条写命令。
      */}
      <PricingDialog
        open={priceRow !== undefined}
        model={priceRow?.id ?? ""}
        onClose={() => setPriceRow(undefined)}
      />

      {/* 删除结果区（`models-delete-result`）：只写真的发生过的事 */}
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
        删除确认：**先确认再发命令**；被引用时后端原文原样显示。
：**这里没有级联按钮** —— 删模型永远不会删线路（线路归服务商），
        所以"确认级联"只会再一次失败，那是个假按钮。真被线路引用时后端会给出原文与指路。
      */}
      <ConfirmDelete
        open={deleteTarget !== undefined}
        testIdPrefix="models"
        title={t("delete.title.model")}
        targetLabel={deleteTarget?.label ?? ""}
        consequences={deleteTarget?.consequences}
        cascadeAvailable={false}
        pending={removeModel.isPending || batchRunning}
        errorText={deleteError}
        onConfirm={() => void confirmDelete(false)}
        onCancel={() => {
          setDeleteTarget(undefined);
          setDeleteError(undefined);
        }}
      />

      {/*
        线路启停的确认框。
        为什么它必须存在：线路是**共用**的，关掉它不只影响当前这个模型 ——
        别的模型也会一起少一条。用户点的是"这个模型上的一个开关"，
        所以我们有义务在改之前告诉他"还有谁在用"。共用者名单来自**已加载的模型清单**
        （读不到就一个字都不写，不编）。
      */}
      <Dialog
        open={lineToggle !== undefined}
        onClose={() => setLineToggle(undefined)}
        data-testid="models-line-toggle-dialog"
      >
        <DialogTitle>
          {t(i18nKey(
            lineToggle?.enabled === false ? "models.lineToggle.offTitle" : "models.lineToggle.onTitle",
          ))}
        </DialogTitle>
        <DialogContent>
          <Stack spacing={1}>
            <Typography data-testid="models-line-toggle-target" sx={{ fontFamily: typography.fontMono }}>
              {lineToggle?.lineId ?? ""}
            </Typography>
            <Typography data-testid="models-line-toggle-shared" variant="body2">
              {/* 模型清单读不到时**不写"没有别的模型在用"** —— 那是结论，而我们没有依据。
                  与 `models.lines.missing` 同一套纪律：缺数据要说出来，不冒充结论。 */}
              {modelsBroken
                ? t(i18nKey("models.lineToggle.sharedUnknown"))
                : sharedWith.length === 0
                  ? t(i18nKey("models.lineToggle.sharedNone"))
                  : t(i18nKey("models.lineToggle.shared"), { value: sharedWith.join(" · ") })}
            </Typography>
            {toggleError === undefined ? null : (
              <Typography data-testid="models-line-toggle-error" variant="body2" sx={{ color: "error.main" }}>
                {toggleError}
              </Typography>
            )}
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button data-testid="models-line-toggle-cancel" onClick={() => setLineToggle(undefined)}>
            {t(i18nKey("models.lineToggle.cancel"))}
          </Button>
          <Button
            data-testid="models-line-toggle-confirm"
            variant="contained"
            disabled={togglingLine !== undefined}
            onClick={() => void confirmLineToggle()}
          >
            {t(i18nKey("models.lineToggle.confirm"))}
          </Button>
        </DialogActions>
      </Dialog>

      <Dialog
        open={addOpen}
        onClose={() => {
          setAddOpen(false);
          setEditingId(undefined);
        }}
        fullWidth
        maxWidth="sm"
      >
        <DialogTitle>
          {editingId === undefined ? t("models.add.title") : t(i18nKey("models.edit.title"))}
        </DialogTitle>
        <DialogContent>
          <Stack spacing={2} sx={{ pt: 1 }}>
            <TextField
              label={t("models.add.id")}
              value={draft.id}
              onChange={(e) => setDraft({ ...draft, id: e.target.value })}
              {...{ "data-testid": "models-add-id" }}
              /* 编辑态 id 只读：`model_upsert` 按 id 匹配，允许改 id 等于"建一个新模型 + 留一个孤儿" */
              disabled={editingId !== undefined}
              helperText={
                editingId === undefined ? t("models.add.id.hint") : t(i18nKey("models.edit.idFixed"))
              }
              fullWidth
            />
            {/*
              **别名不再要用户输入**（用户第 309 轮原话：「别名那个没必要搞，创建模型的时候就只输入
              模型名称和 id 就好」）。语义上别名只是"客户端可能用的另一个名字"，
              对绝大多数人是从不使用的概念 —— 摆一个空输入框只会让人犹豫该填什么。
              已有模型的别名**一个字都不丢**：编辑时草稿仍从那一行灌进来（`draftFromRow`），
              提交时原样带回，所以"去掉输入框"不会变成"悄悄抹掉别名"。
            */}
            <Box>
              <Button
                size="small"
                onClick={() => setAdvanced((v) => !v)}
                {...{ "data-testid": "models-add-advanced-toggle" }}
              >
                {advanced ? t("models.add.advanced.hide") : t("models.add.advanced.show")}
              </Button>
              <Collapse in={advanced} unmountOnExit>
                <Stack spacing={2} sx={{ pt: 2 }}>
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
                </Stack>
              </Collapse>
            </Box>
            <Typography variant="caption" sx={{ color: "text.secondary" }}>
              {t("models.add.expose.hint")}
            </Typography>

            {/*
              **线路分配**：模型由线路构成，至少一条。
              这是**选线路**的控件，不是选网络代理 —— 代理是线路自己的属性，
              这一页没有、也不加"给模型挑代理"的入口。
            */}
            <FormControl component="fieldset" fullWidth data-testid="models-add-lines">
              <FormLabel component="legend">{t(i18nKey("models.lines.pick"))}</FormLabel>
              {channelsBroken ? (
                /* 读不到就**如实说**（绝不静默给一个空清单：那会让人以为"没有线路可选"） */
                <Typography
                  variant="body1"
                  data-testid="models-add-lines-error"
                  sx={{ color: "error.main", mt: 1 }}
                >
                  {t("models.channels.error", { message: errorMessage(channels.error) })}
                </Typography>
              ) : candidates.length === 0 ? (
                <Typography
                  variant="body1"
                  data-testid="models-add-lines-empty"
                  sx={{ color: "text.secondary", mt: 1 }}
                >
                  {t(i18nKey("models.lines.noCandidates"))}
                </Typography>
              ) : (
                /**
                 *（用户第 86 轮）：**与「套餐与线路」同一套逻辑** ——
                 * 按服务商分组、可展开收起、**只列已经打开（`enabled`）的线路**。
                 *
                 * 两个边界不能踩：
                 * * 已经选中的线路**必须照旧显示**（哪怕它现在是关闭的）—— 藏起来等于让用户
                 *   在不知情的情况下把它从模型里摘掉（保存即数据丢失）；
                 * * `enabled` 是布尔（`channel_list` 一定有这一列），所以这里不存在"读不到"。
                 */
                <Stack spacing={2} data-testid="models-add-line-groups" sx={{ mt: 1 }}>
                  {visibleGroups.map((group) => {
                    const expanded = !collapsedProviders.includes(group.providerId);
                    return (
                      <Box key={group.providerId} data-testid="models-add-line-group">
                        <Stack direction="row" spacing={2} alignItems="center" sx={{ minWidth: 0 }}>
                          <IconButton
                            size="small"
                            data-testid="models-add-line-group-toggle"
                            aria-label={t("channels.group.toggle")}
                            aria-expanded={expanded}
                            onClick={() => toggleProvider(group.providerId)}
                          >
                            {expanded ? (
                              <ExpandMoreIcon fontSize="small" />
                            ) : (
                              <ChevronRightIcon fontSize="small" />
                            )}
                          </IconButton>
                          <Typography variant="body1" noWrap sx={{ minWidth: 0 }}>
                            {group.providerName ?? group.providerId}
                          </Typography>
                          <Typography variant="caption" sx={{ color: "text.secondary" }}>
                            {formatInt(group.refs.length)}
                          </Typography>
                        </Stack>
                        {expanded ? (
                          <Stack spacing={1} sx={{ mt: 1, maxHeight: 240, overflowY: "auto" }}>
                            {group.refs.map((ref) => (
                              <FormControlLabel
                                key={ref.lineId}
                                control={
                                  <Checkbox
                                    checked={draft.lines.includes(ref.lineId)}
                                    onChange={() => toggleDraftLine(ref.lineId)}
                                    {...{ "data-testid": `models-add-line-${ref.lineId}` }}
                                  />
                                }
                                label={
                                  /* 组头已经说了服务商，行里只留"上游模型 · 网络代理"
                                     （：同一个信息不在同一屏里说两遍） */
                                  <Typography
                                    variant="body1"
                                    sx={{
                                      fontFamily: typography.fontMono,
                                      overflowWrap: "anywhere",
                                      color: ref.enabled ? "text.primary" : "text.secondary",
                                    }}
                                  >
                                    {`${ref.upstreamModel} · ${
                                      ref.laneName ?? ref.laneId ?? t("channels.lane.inherited")
                                    }`}
                                  </Typography>
                                }
                              />
                            ))}
                          </Stack>
                        ) : null}
                      </Box>
                    );
                  })}
                </Stack>
              )}
            </FormControl>

            {/* 本地预检的错，按字段逐条显示（不解析文案，按机器可读的 reason 取 key） */}
            {issues.map((issue) => (
              <Typography
                key={`${issue.field}-${issue.reason}`}
                variant="body1"
                data-testid={`models-add-issue-${issue.field}`}
                sx={{ color: "error.main" }}
              >
                {t(issueKey(issue))}
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
          <Button
            onClick={() => {
              setAddOpen(false);
              setEditingId(undefined);
            }}
          >
            {t("models.add.close")}
          </Button>
          <Button
            variant="contained"
            data-testid="models-add-submit"
            disabled={upsert.isPending}
            onClick={submitAdd}
          >
            {editingId === undefined ? t("models.add.submit") : t(i18nKey("models.edit.submit"))}
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}

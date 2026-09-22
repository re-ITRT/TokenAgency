/**
 * 网络页（[04 §5–§8]、[12 §6 F4]）—— `/network` 概览，**以及三个网络视图共用的展示件**。
 *
 * ## 两条硬纪律
 *
 * 1. **I2：系统网络零副作用。** 这一页（含节点列表与线路分配）**不碰系统网络**：
 *    没有"改系统代理 / DNS / 路由 / 网卡 / 开机自启"的路径或按钮，也没有启停网关或内核的按钮。
 *    页面调用的命令是一条可机械检查的白名单（见 `src/app/network.test.tsx` 的命令面守卫）：
 *    只读的 `app_status` / `subscription_list` / `lane_list` / `config_get`，
 *    外加**只管订阅与线路这两个配置实体**的写命令（`subscription_*` / `lane_upsert` /
 *    `lane_delete` / `lane_set_node`）—— 后者改的是 TokenAgency 自己的配置与内核运行态，
 *    系统网络配置一个字段都不动。守卫对"系统/内核/托盘写命令"仍然零容忍。
 * 2. **不猜。** 读不到的字段显示「未读取」，配置里确实是空的显示空状态，两者**不是一回事**；
 *    写操作**只显示后端返回的对象**（不拼一句"保存成功"），失败原样显示 TA 码。
 *    需要但尚未注册的后端接口如实点名，而不是编一份看起来完整的节点清单
 *    （见 `model.ts` 文件头「一」的三态区分）。
 *
 * ## 写界面与读界面为什么在同一页
 *
 * 订阅与线路是"看一眼、改一下、再看一眼"的闭环：列表里的每一行都带刷新 / 删除，
 * 表单就在列表下面。**只读的五级链路解析**（后端算的最终生效线路）放在 `/network/assignments`，
 * 因为"删 lane 之前先看谁在引用它"是另一个操作流。
 * 写操作走 `useCommandMutation` + `invalidateQueries`：**不用本地状态伪造一行结果**。
 *
 * ## 为什么这个文件还导出别的视图要用的组件
 *
 * `pageRegistry` 把 `src/pages/**` 下**每一个 `.tsx`** 都当成一个页面候选，而
 * `noPlaceholder.guard.test.ts` 会断言"每个页面文件都在路由表里有归属（不许有孤儿页面）"。
 * 路由表（`routes.ts`）给 network 的候选只有 `network` / `network/index` /
 * `network/nodes` / `network/assignments` 四条。所以共用展示件**不能**单独放一个
 * `sections.tsx` —— 那会被判成孤儿页面并把守卫变红。三个视图共用的这几个小组件
 * 因此挂在本文件（`/network` 的页面模块）上，由 `./nodes` 与 `./assignments` import。
 */

import {
  Box,
  Button,
  Checkbox,
  Divider,
  FormControlLabel,
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
import { useState, type ReactNode } from "react";
import { Link } from "react-router-dom";

import { errorMessage, qk, useAppStatus, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { AppStatus } from "@/shared/bindings";
import { KernelStateDot, PageHeader, StatusDot } from "@/shared/components";
import { formatBytes, formatDuration, formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  GROUP_TYPE_LABEL,
  HEALTH_LABEL,
  HEALTH_STATUS,
  LANE_GROUP_TYPES,
  LANE_KINDS,
  SUBSCRIPTION_FORMATS,
  filterSummary,
  isKnownHealth,
  laneKindLabel,
  laneMissingField,
  laneNameOf,
  lanePayload,
  parseOptionalCount,
  readLane,
  readSubscription,
  subscriptionDraftComplete,
  subscriptionFormatLabel,
  subscriptionPayload,
  type LaneDraft,
  type LaneRow,
  type SubscriptionDraft,
  type SubscriptionRow,
} from "./model";

// ================================================================ 共用展示件（见文件头末节）

/** 契约出处标注、说明文字：一律 `caption` + 次要色，不抢正文。 */
export function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/** 一个只读区块：标题 + 一句话说明 + 内容。**不是一个装饰性卡片**（无阴影、无强调色）。 */
export function SectionPaper({
  title,
  description,
  hint,
  hintTestId,
  testId,
  children,
}: {
  title: string;
  description?: string;
  hint?: ReactNode;
  hintTestId?: string;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }} data-testid={testId}>
      <Stack spacing={3}>
        <Box>
          <Typography variant="h4">{title}</Typography>
          {description === undefined ? null : <Caption>{description}</Caption>}
        </Box>
        {hint === undefined ? null : <Box data-testid={hintTestId}>{hint}</Box>}
        {children}
      </Stack>
    </Paper>
  );
}

/** 一个统计格：标签（`overline`）+ 值（等宽数字）。 */
export function StatItem({ label, value }: { label: string; value: ReactNode }) {
  return (
    <Box sx={{ minWidth: 0 }}>
      <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
        {label}
      </Typography>
      <Typography variant="body1" sx={{ fontVariantNumeric: "tabular-nums", overflowWrap: "anywhere" }}>
        {value}
      </Typography>
    </Box>
  );
}

/**
 * I2 的**用户可见面**：把"这一页不碰系统网络"写在页面上，而不是只写在文档里。
 *
 * 为什么值得占一块版面：网络类工具的用户最怕的就是"我点开一个页面，它顺手把我系统代理改了"。
 * 这一条是承诺，也是这一页唯一"解释自己"的文字。
 */
export function ZeroSideEffectNotice() {
  const { t } = useTranslation();
  return (
    <Paper
      variant="outlined"
      data-testid="network-zero-side-effect"
      sx={{ p: 4, borderRadius: `${radius.surface}px` }}
    >
      <Stack spacing={1}>
        <Typography variant="h4">{t("network.zeroSideEffect.title")}</Typography>
        <Caption>{t("network.zeroSideEffect.body")}</Caption>
      </Stack>
    </Paper>
  );
}

/** 线路健康：6px 圆点 + 文字（[15 §5] 不许用彩色 chip）。 */
export function HealthCell({ health, raw }: { health: LaneRow["health"]; raw?: string }) {
  const { t } = useTranslation();
  // 契约外的取值原样显示（是后端说的话，不改写）；我们认识的词表才翻成中文
  const label = raw !== undefined && !isKnownHealth(raw) ? raw : t(HEALTH_LABEL[health]);
  return <StatusDot status={HEALTH_STATUS[health]} label={label} />;
}

/**
 * 「未读取」的统一写法（**不是**空值、**不是** 0）。
 *
 * 固定 `component="span"`：它会出现在表格单元格里，也会出现在别的 `<Typography>`（即 `<p>`）
 * 内部 —— 用默认的 `<p>` 会得到非法的 `<p><p>` 嵌套（React 会告警，浏览器会重排）。
 */
export function Unreadable({ testId }: { testId?: string }) {
  const { t } = useTranslation();
  return (
    <Typography
      variant="body1"
      component="span"
      data-testid={testId}
      sx={{ color: "text.secondary" }}
    >
      {t("network.unreadable")}
    </Typography>
  );
}

/**
 * 「后端这个接口还没注册」的统一说明。
 *
 * 文案（含命令名）**只从 i18n 取**，页面源码里不出现命令名字面量 —— 这不是洁癖：
 * `src/app/network.test.tsx` 的命令面守卫会扫页面源码里的命令名，而"我在文案里提一句
 * 某个还没注册的命令"和"我真的去调它"在源码上是同一种字面量。把命令名收进 i18n
 * 之后，"页面调用的命令集合 == 只读白名单"这条断言才是有意义的。
 */
export function MissingCommandNote({ messageKey }: { messageKey: I18nKey }) {
  const { t } = useTranslation();
  return <Caption>{t(messageKey)}</Caption>;
}

const TABLE_WRAP_SX = { overflowX: "auto" } as const;

// ================================================================ ① 当前出口

function ExitRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
      <Typography variant="caption" sx={{ color: "text.secondary", minWidth: 96, flex: "0 0 auto" }}>
        {label}
      </Typography>
      <Box sx={{ display: "inline-flex", alignItems: "center", gap: 2, minWidth: 0 }}>{children}</Box>
    </Box>
  );
}

function CurrentExit({ status, lanes }: { status: AppStatus | undefined; lanes: readonly LaneRow[] }) {
  const { t } = useTranslation();
  const gateway = status?.gateway;
  // 只有后端给了 id 才显示出口；能给名字就给名字，给不了解析就原样显示 id（不猜一个线路名）
  const laneId = status?.default_lane_id ?? undefined;
  const laneName = laneId === undefined ? undefined : laneNameOf(laneId, lanes);

  return (
    <SectionPaper
      title={t("network.currentExit.title")}
      description={t("network.currentExit.hint")}
      testId="network-current-exit"
    >
      <Stack spacing={2}>
        <ExitRow label={t("network.currentExit.gateway")}>
          {gateway === undefined ? (
            <Unreadable />
          ) : (
            <StatusDot
              status={gateway === "running" ? "ok" : "warn"}
              label={gateway === "running" ? t("status.gateway.running") : t("status.gateway.paused")}
            />
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.kernel")}>
          <KernelStateDot state={status?.kernel.state} />
        </ExitRow>

        <ExitRow label={t("network.currentExit.defaultLane")}>
          {laneId === undefined ? (
            <Unreadable />
          ) : (
            <Typography variant="body1">{laneName ?? laneId}</Typography>
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.node")}>
          {status?.current_node === undefined || status.current_node === null ? (
            <Unreadable />
          ) : (
            <Typography variant="body1">{status.current_node}</Typography>
          )}
        </ExitRow>
      </Stack>
    </SectionPaper>
  );
}

// ================================================================ ② 订阅（读 + 写）

/** 表单里的一行：标签 + 控件。窄屏时换行（`flexWrap`），不做"响应式隐藏"。 */
const FIELD_ROW_SX = { flexWrap: "wrap", alignItems: "flex-start" } as const;

/** 一个输入字段：`testId` 挂在容器上（测试用 `within(...).getByRole("textbox")` 取输入框）。 */
function Field({
  testId,
  label,
  value,
  onChange,
  hint,
  minWidth = 200,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  hint?: string;
  minWidth?: number;
}) {
  return (
    <Stack spacing={1} data-testid={testId} sx={{ minWidth: 0 }}>
      <TextField
        size="small"
        label={label}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        // 数字字段用 `inputMode` 而不是 `type="number"`：留一个真正可编辑的文本框，
        // 校验（是非负整数吗）由我们自己说清楚，而不是靠浏览器静默丢掉非法输入。
        slotProps={{ htmlInput: { sx: { minWidth } } }}
      />
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

/** 写操作的错误区：一律 `errorMessage`（带 TA 码），不吞错、不改写成一句"保存失败"。 */
function WriteError({ testId, error }: { testId: string; error: unknown }) {
  if (error === undefined || error === null) return null;
  return (
    <Typography
      variant="body1"
      data-testid={testId}
      sx={{ color: "error.main", overflowWrap: "anywhere" }}
    >
      {errorMessage(error)}
    </Typography>
  );
}

/** 表单里的本地校验消息（必填 / 数字格式）：**说清缺什么**，而不是把空值发给后端。 */
function FormNotice({ testId, message }: { testId: string; message: string | undefined }) {
  if (message === undefined) return null;
  return (
    <Typography
      variant="body1"
      data-testid={testId}
      sx={{ color: "error.main", overflowWrap: "anywhere" }}
    >
      {message}
    </Typography>
  );
}

const EMPTY_SUBSCRIPTION_DRAFT: SubscriptionDraft = {
  id: "",
  name: "",
  url: "",
  format: "auto",
  // 表单里显示的值就是发出去的值；后端默认值也是这两个（21_600s / 3 个版本）
  intervalS: "21600",
  keepVersions: "3",
  include: "",
  exclude: "",
  enabled: true,
};

/**
 * 新建 / 覆盖订阅的表单。
 *
 * 成功区显示的**是 `subscription_upsert` 的返回值**（经 `readSubscription` 归一并遮蔽地址），
 * 不是前端拼的一行"添加成功" —— 后者在"后端其实没存进去"时也会显示成功。
 */
function SubscriptionWriteForm() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<SubscriptionDraft>(EMPTY_SUBSCRIPTION_DRAFT);
  const [formError, setFormError] = useState<string | undefined>(undefined);
  const upsert = useCommandMutation("subscription_upsert");
  const preview = useCommandMutation("subscription_preview");

  const set = <K extends keyof SubscriptionDraft>(key: K, value: SubscriptionDraft[K]) =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const saved = upsert.data === undefined ? undefined : readSubscription(upsert.data);
  const savedFormat = saved === undefined ? undefined : subscriptionFormatLabel(saved.format);

  const onPreview = () => {
    setFormError(undefined);
    upsert.reset();
    preview.reset();
    if (draft.url.trim() === "") {
      setFormError(t("network.subscriptions.write.required"));
      return;
    }
    preview.mutate({ url: draft.url.trim(), format: draft.format });
  };

  const onSubmit = () => {
    setFormError(undefined);
    upsert.reset();
    preview.reset();
    if (!subscriptionDraftComplete(draft)) {
      setFormError(t("network.subscriptions.write.required"));
      return;
    }
    if (
      parseOptionalCount(draft.intervalS) === null ||
      parseOptionalCount(draft.keepVersions) === null
    ) {
      setFormError(t("network.subscriptions.write.badNumber"));
      return;
    }
    upsert.mutate(
      { subscription: subscriptionPayload(draft) },
      {
        // 后端真值回来之后重新拉列表：**不用本地状态拼一行"添加成功"**
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  const previewData = preview.data;

  return (
    <Stack spacing={3} data-testid="network-subscriptions-write">
      <Box>
        <Typography variant="h5">{t("network.subscriptions.write.title")}</Typography>
        <Caption>{t("network.subscriptions.write.hint")}</Caption>
      </Box>

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Field
          testId="network-subscriptions-add-id"
          label={t("network.subscriptions.write.id")}
          value={draft.id}
          onChange={(value) => set("id", value)}
          hint={t("network.subscriptions.write.idHint")}
        />
        <Field
          testId="network-subscriptions-add-name"
          label={t("network.subscriptions.write.name")}
          value={draft.name}
          onChange={(value) => set("name", value)}
        />
      </Stack>

      <Field
        testId="network-subscriptions-add-url"
        label={t("network.subscriptions.write.url")}
        value={draft.url}
        onChange={(value) => set("url", value)}
        hint={t("network.subscriptions.write.urlHint")}
        minWidth={360}
      />

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Stack spacing={1} data-testid="network-subscriptions-add-format" sx={{ minWidth: 200 }}>
          <TextField
            select
            size="small"
            label={t("network.subscriptions.write.format")}
            value={draft.format}
            onChange={(event) => set("format", event.target.value as SubscriptionDraft["format"])}
          >
            {SUBSCRIPTION_FORMATS.map((format) => {
              const labelKey = subscriptionFormatLabel(format);
              return (
                <MenuItem key={format} value={format}>
                  {labelKey === undefined ? format : t(labelKey)}
                </MenuItem>
              );
            })}
          </TextField>
        </Stack>
        <Field
          testId="network-subscriptions-add-interval"
          label={t("network.subscriptions.write.interval")}
          value={draft.intervalS}
          onChange={(value) => set("intervalS", value)}
        />
        <Field
          testId="network-subscriptions-add-keep-versions"
          label={t("network.subscriptions.write.keepVersions")}
          value={draft.keepVersions}
          onChange={(value) => set("keepVersions", value)}
        />
      </Stack>

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Field
          testId="network-subscriptions-add-include"
          label={t("network.subscriptions.write.include")}
          value={draft.include}
          onChange={(value) => set("include", value)}
        />
        <Field
          testId="network-subscriptions-add-exclude"
          label={t("network.subscriptions.write.exclude")}
          value={draft.exclude}
          onChange={(value) => set("exclude", value)}
        />
      </Stack>

      <FormControlLabel
        data-testid="network-subscriptions-add-enabled"
        control={
          <Checkbox
            size="small"
            checked={draft.enabled}
            onChange={(event) => set("enabled", event.target.checked)}
          />
        }
        label={t("network.subscriptions.write.enabled")}
      />

      <Stack direction="row" spacing={2} sx={{ flexWrap: "wrap" }}>
        <Button variant="outlined" data-testid="network-subscriptions-preview" onClick={onPreview}>
          {t("network.subscriptions.write.preview")}
        </Button>
        <Button variant="contained" data-testid="network-subscriptions-add-submit" onClick={onSubmit}>
          {t("network.subscriptions.write.submit")}
        </Button>
      </Stack>

      <FormNotice testId="network-subscriptions-write-error" message={formError} />
      <WriteError testId="network-subscriptions-write-error" error={upsert.error ?? preview.error} />

      {saved === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-write-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.saved")}：${saved.name ?? saved.id} · ${saved.id}`}
          {savedFormat === undefined ? "" : ` · ${saved.format === undefined ? "" : t(savedFormat)}`}
          {saved.intervalS === undefined ? "" : ` · ${formatDuration(saved.intervalS)}`}
          {saved.keepVersions === undefined
            ? ""
            : ` · ${t("network.subscriptions.col.keepVersions")} ${saved.keepVersions}`}
          {` · ${saved.enabled ? t("common.enabled") : t("common.disabled")}`}
          {saved.url === "" ? "" : ` · ${saved.url}`}
        </Typography>
      )}

      {previewData === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-preview-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.previewTitle")}：`}
          {t("network.subscriptions.write.previewCounts", {
            groups: previewData.groups.length,
            nodes: previewData.nodes.length,
          })}
          {previewData.sample.length === 0
            ? ""
            : ` · ${t("network.subscriptions.write.previewSample", {
                sample: previewData.sample.map((node) => node.name).join(" · "),
              })}`}
        </Typography>
      )}
    </Stack>
  );
}

function SubscriptionsSection({
  rows,
  isLoading,
  error,
  onRetry,
}: {
  rows: readonly SubscriptionRow[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 行级写操作：刷新（强制重下）与删除。都**不碰系统网络**，只改 TokenAgency 自己的配置 /
  // 订阅版本存储；删完 / 刷完重新拉列表，显示的是后端下一次给的事实。
  const refreshOne = useCommandMutation("subscription_refresh");
  const removeOne = useCommandMutation("subscription_delete");
  const busy = refreshOne.isPending || removeOne.isPending;

  const onRefresh = (id: string) => {
    refreshOne.reset();
    removeOne.reset();
    refreshOne.mutate(
      { id, force: true },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  const onDelete = (id: string) => {
    refreshOne.reset();
    removeOne.reset();
    removeOne.mutate(
      { id },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  const deletedId = removeOne.isSuccess ? removeOne.variables?.id : undefined;
  const refreshResult = refreshOne.data;

  return (
    <SectionPaper
      title={t("network.subscriptions.title")}
      description={t("network.subscriptions.description")}
      hintTestId="network-subscriptions-hint"
      hint={
        <Stack spacing={1}>
          <Caption>{t("network.subscriptions.countHint")}</Caption>
          <Caption>{t("network.subscriptions.urlHint")}</Caption>
          <Caption>{t("network.subscriptions.healthHint")}</Caption>
          <Caption>{t("network.subscriptions.row.refreshHint")}</Caption>
        </Stack>
      }
      testId="network-subscriptions"
    >
      {error !== undefined && error !== null ? (
        <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Typography variant="body1" data-testid="network-subscriptions-error" sx={{ color: "error.main" }}>
            {errorMessage(error)}
          </Typography>
          <Button variant="outlined" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </Stack>
      ) : rows === undefined ? (
        <Typography variant="body1" sx={{ color: "text.secondary" }}>
          {isLoading ? t("common.loading") : DASH}
        </Typography>
      ) : rows.length === 0 ? (
        <Typography variant="body1" data-testid="network-subscriptions-empty">
          {t("network.subscriptions.empty")}
        </Typography>
      ) : (
        <Box sx={TABLE_WRAP_SX}>
          <Table size="small" data-testid="network-subscriptions-table">
            <TableHead>
              <TableRow>
                <TableCell>{t("network.subscriptions.col.name")}</TableCell>
                <TableCell>{t("network.subscriptions.col.url")}</TableCell>
                <TableCell>{t("network.subscriptions.col.enabled")}</TableCell>
                <TableCell>{t("network.subscriptions.col.format")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.interval")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.keepVersions")}</TableCell>
                <TableCell>{t("network.subscriptions.col.filter")}</TableCell>
                <TableCell>{t("network.subscriptions.col.health")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.traffic")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.nodes")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.groups")}</TableCell>
                <TableCell>{t("network.subscriptions.col.lastUpdate")}</TableCell>
                <TableCell>{t("network.subscriptions.col.parser")}</TableCell>
                <TableCell>{t("network.subscriptions.col.error")}</TableCell>
                <TableCell align="right">{t("network.subscriptions.col.actions")}</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => {
                const formatLabel = subscriptionFormatLabel(row.format);
                return (
                  <TableRow key={row.id} hover data-testid="network-subscription-row">
                    <TableCell>{row.name === undefined ? DASH : row.name}</TableCell>
                    <TableCell
                      sx={{ fontFamily: typography.fontMono, maxWidth: 280, overflowWrap: "anywhere" }}
                    >
                      {row.url === "" ? DASH : row.url}
                    </TableCell>
                    <TableCell>{row.enabled ? t("common.enabled") : t("common.disabled")}</TableCell>
                    <TableCell>
                      {row.format === undefined
                        ? DASH
                        : formatLabel === undefined
                          ? row.format
                          : t(formatLabel)}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.intervalS === undefined ? DASH : formatDuration(row.intervalS)}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.keepVersions === undefined ? DASH : row.keepVersions}
                    </TableCell>
                    {/* 过滤正则是配置里声明的**用户数据**：原样显示，不做任何改写 */}
                    <TableCell sx={{ fontFamily: typography.fontMono, maxWidth: 280, overflowWrap: "anywhere" }}>
                      {filterSummary(row.include, row.exclude, DASH)}
                    </TableCell>
                    <TableCell>
                      {row.healthEnabled === undefined ? (
                        <Unreadable />
                      ) : row.healthEnabled ? (
                        row.healthIntervalS === undefined ? (
                          t("common.enabled")
                        ) : (
                          formatDuration(row.healthIntervalS)
                        )
                      ) : (
                        t("common.disabled")
                      )}
                    </TableCell>
                    {/* 解析结果：契约的 SubscriptionView 有，Rust 的裸 Subscription 没有 → 未读取 */}
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.total === undefined ? (
                        <Unreadable />
                      ) : (
                        `${row.download === undefined ? DASH : formatBytes(row.download)} / ${formatBytes(row.total)}${
                          row.expireMs === undefined ? "" : ` · ${formatLocalTime(row.expireMs, "YYYY-MM-DD")}`
                        }`
                      )}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.nodeCount === undefined ? <Unreadable /> : row.nodeCount}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.groupCount === undefined ? <Unreadable /> : row.groupCount}
                    </TableCell>
                    <TableCell sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.lastUpdateMs === undefined ? <Unreadable /> : formatLocalTime(row.lastUpdateMs)}
                    </TableCell>
                    <TableCell sx={{ fontFamily: typography.fontMono }}>
                      {row.parser === undefined ? <Unreadable /> : row.parser}
                    </TableCell>
                    <TableCell sx={{ maxWidth: 280 }}>
                      {row.error === undefined ? (
                        DASH
                      ) : (
                        <Typography variant="body1" sx={{ color: "error.main", overflowWrap: "anywhere" }}>
                          {row.error}
                        </Typography>
                      )}
                    </TableCell>
                    <TableCell align="right" sx={{ whiteSpace: "nowrap" }}>
                      <Stack direction="row" spacing={2} justifyContent="flex-end">
                        <Button
                          size="small"
                          variant="outlined"
                          data-testid="network-subscriptions-refresh"
                          disabled={busy}
                          onClick={() => onRefresh(row.id)}
                        >
                          {t("network.subscriptions.row.refresh")}
                        </Button>
                        <Button
                          size="small"
                          variant="outlined"
                          data-testid="network-subscriptions-delete"
                          disabled={busy}
                          onClick={() => onDelete(row.id)}
                        >
                          {t("network.subscriptions.row.delete")}
                        </Button>
                      </Stack>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}

      {/* 行级写操作的结果：显示后端返回的事实，不是前端拼的一句话 */}
      {refreshResult === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-refresh-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.refreshTitle")}：${
            refreshResult.ok
              ? t("network.subscriptions.write.refreshOk")
              : t("network.subscriptions.write.refreshFailed")
          }`}
          {` · ${t("network.subscriptions.col.nodes")} ${refreshResult.node_count}`}
          {` · ${t("network.subscriptions.col.groups")} ${refreshResult.group_count}`}
          {refreshResult.error === undefined || refreshResult.error === null
            ? ""
            : ` · ${refreshResult.error}`}
        </Typography>
      )}

      {deletedId === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-delete-result">
          {t("network.subscriptions.write.deleteDone", { id: deletedId })}
        </Typography>
      )}

      <WriteError testId="network-subscriptions-row-error" error={refreshOne.error ?? removeOne.error} />

      <Divider />

      <SubscriptionWriteForm />
    </SectionPaper>
  );
}

// ================================================================ ③ 线路（读 + 写）

const EMPTY_LANE_DRAFT: LaneDraft = {
  id: "",
  name: "",
  kind: "direct",
  subscription: "",
  group: "",
  groupType: "",
  node: "",
  filter: "",
  excludeFilter: "",
  enabled: true,
};

/** 一个下拉项：`""` 表示"没选"（值本身就是契约里的语义，不是占位符）。 */
function SelectField({
  testId,
  label,
  value,
  onChange,
  children,
  hint,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  children: ReactNode;
  hint?: string;
}) {
  return (
    <Stack spacing={1} data-testid={testId} sx={{ minWidth: 200 }}>
      <TextField select size="small" label={label} value={value} onChange={(event) => onChange(event.target.value)}>
        {children}
      </TextField>
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

/**
 * 新建 / 覆盖线路的表单。
 *
 * 两条与后端对齐的规则（都不是前端发明的）：
 * 1. 必填项按 `Lane::missing_kind_field()`（`crates/ta-core/src/domain/behavior.rs`）提前挡一次；
 * 2. 组名 / 节点名必须**逐字**一致，所以把 `subscription_groups` / `subscription_nodes` 的解析结果
 *    读出来给人照抄 —— 读不到就如实说读不到，不猜、不替用户改字（[15 §2.2]）。
 */
function LaneWriteForm() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<LaneDraft>(EMPTY_LANE_DRAFT);
  const [formError, setFormError] = useState<string | undefined>(undefined);
  const upsert = useCommandMutation("lane_upsert");

  // 订阅 id + 名字：只能从配置里已有的订阅里选（`subscription_list` 是同一份缓存，页面里共用）
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const subscriptionOptions =
    subscriptions.data?.map((item) => {
      const row = readSubscription(item);
      return { id: row.id, name: row.name ?? row.id };
    }) ?? [];

  const needsGroup = draft.kind === "subscription_group";
  const needsNode = draft.kind === "subscription_node";
  const hasSubscription = draft.subscription.trim() !== "";
  // 只在真的需要且真的选了订阅时才去读解析结果（否则会拿到一条必然失败的请求）
  const groups = useCommandQuery(
    "subscription_groups",
    { id: draft.subscription },
    [...qk.subscriptions, "groups", draft.subscription],
    { enabled: needsGroup && hasSubscription },
  );
  const nodes = useCommandQuery(
    "subscription_nodes",
    { id: draft.subscription },
    [...qk.subscriptions, "nodes", draft.subscription],
    { enabled: needsNode && hasSubscription },
  );

  const set = <K extends keyof LaneDraft>(key: K, value: LaneDraft[K]) =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const saved = upsert.data === undefined ? undefined : readLane(upsert.data);
  const savedKind = saved === undefined ? undefined : laneKindLabel(saved.kind);

  const onSubmit = () => {
    setFormError(undefined);
    upsert.reset();
    const missing = laneMissingField(draft);
    if (missing !== undefined) {
      setFormError(t("network.lanes.write.missing", { field: missing }));
      return;
    }
    upsert.mutate(
      { lane: lanePayload(draft) },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.lanes });
        },
      },
    );
  };

  return (
    <Stack spacing={3} data-testid="network-lanes-write">
      <Box>
        <Typography variant="h5">{t("network.lanes.write.title")}</Typography>
        <Caption>{t("network.lanes.write.hint")}</Caption>
      </Box>

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Field
          testId="network-lanes-add-id"
          label={t("network.lanes.write.id")}
          value={draft.id}
          onChange={(value) => set("id", value)}
          hint={t("network.lanes.write.idHint")}
        />
        <Field
          testId="network-lanes-add-name"
          label={t("network.lanes.write.name")}
          value={draft.name}
          onChange={(value) => set("name", value)}
        />
        <SelectField
          testId="network-lanes-add-kind"
          label={t("network.lanes.write.kind")}
          value={draft.kind}
          onChange={(value) => set("kind", value as LaneDraft["kind"])}
          hint={t("network.lanes.write.kindHint")}
        >
          {LANE_KINDS.map((kind) => (
            <MenuItem key={kind} value={kind}>
              {t(laneKindLabel(kind) ?? "network.lanes.col.kind")}
            </MenuItem>
          ))}
        </SelectField>
      </Stack>

      {draft.kind === "direct" ? null : (
        <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
          <SelectField
            testId="network-lanes-add-subscription"
            label={t("network.lanes.write.subscription")}
            value={draft.subscription}
            onChange={(value) => set("subscription", value)}
            hint={t("network.lanes.write.subscriptionHint")}
          >
            <MenuItem value="">{t("network.lanes.write.subscriptionNone")}</MenuItem>
            {subscriptionOptions.map((option) => (
              <MenuItem key={option.id} value={option.id}>
                {option.name}
              </MenuItem>
            ))}
          </SelectField>
          {needsGroup ? (
            <Field
              testId="network-lanes-add-group"
              label={t("network.lanes.write.group")}
              value={draft.group}
              onChange={(value) => set("group", value)}
              hint={t("network.lanes.write.groupHint")}
            />
          ) : null}
          {needsNode ? (
            <Field
              testId="network-lanes-add-node"
              label={t("network.lanes.write.node")}
              value={draft.node}
              onChange={(value) => set("node", value)}
              hint={t("network.lanes.write.nodeHint")}
            />
          ) : null}
          {needsGroup ? (
            <SelectField
              testId="network-lanes-add-group-type"
              label={t("network.lanes.write.groupType")}
              value={draft.groupType}
              onChange={(value) => set("groupType", value)}
            >
              <MenuItem value="">{t("network.lanes.write.groupTypeNone")}</MenuItem>
              {LANE_GROUP_TYPES.map((groupType) => {
                const labelKey = GROUP_TYPE_LABEL[groupType];
                return (
                  <MenuItem key={groupType} value={groupType}>
                    {labelKey === undefined ? groupType : t(labelKey)}
                  </MenuItem>
                );
              })}
            </SelectField>
          ) : null}
        </Stack>
      )}

      {draft.kind === "direct" ? null : (
        <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
          <Field
            testId="network-lanes-add-filter"
            label={t("network.lanes.write.filter")}
            value={draft.filter}
            onChange={(value) => set("filter", value)}
          />
          <Field
            testId="network-lanes-add-exclude-filter"
            label={t("network.lanes.write.excludeFilter")}
            value={draft.excludeFilter}
            onChange={(value) => set("excludeFilter", value)}
          />
        </Stack>
      )}

      {/* 订阅解析出来的组 / 节点：给人照抄的证据，不是可选项（不替用户改字、不猜） */}
      {needsGroup && hasSubscription ? (
        <Typography
          variant="caption"
          data-testid="network-lanes-known-groups"
          sx={{ color: "text.secondary", overflowWrap: "anywhere" }}
        >
          {groups.isError
            ? errorMessage(groups.error)
            : groups.data === undefined
              ? t("common.loading")
              : groups.data.length === 0
                ? t("network.lanes.write.knownGroupsEmpty")
                : t("network.lanes.write.knownGroups", {
                    list: groups.data.map((group) => group.name).join(" · "),
                  })}
        </Typography>
      ) : null}

      {needsNode && hasSubscription ? (
        <Typography
          variant="caption"
          data-testid="network-lanes-known-nodes"
          sx={{ color: "text.secondary", overflowWrap: "anywhere" }}
        >
          {nodes.isError
            ? errorMessage(nodes.error)
            : nodes.data === undefined
              ? t("common.loading")
              : nodes.data.length === 0
                ? t("network.lanes.write.knownNodesEmpty")
                : t("network.lanes.write.knownNodes", {
                    list: nodes.data.map((node) => node.name).join(" · "),
                  })}
        </Typography>
      ) : null}

      <FormControlLabel
        data-testid="network-lanes-add-enabled"
        control={
          <Checkbox
            size="small"
            checked={draft.enabled}
            onChange={(event) => set("enabled", event.target.checked)}
          />
        }
        label={t("network.lanes.write.enabled")}
      />

      <Stack direction="row" spacing={2} sx={{ flexWrap: "wrap" }}>
        <Button variant="contained" data-testid="network-lanes-add-submit" onClick={onSubmit}>
          {t("network.lanes.write.submit")}
        </Button>
      </Stack>

      <FormNotice testId="network-lanes-write-error" message={formError} />
      <WriteError testId="network-lanes-write-error" error={upsert.error} />

      {saved === undefined ? null : (
        <Typography variant="body1" data-testid="network-lanes-write-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.lanes.write.saved")}：${saved.name ?? saved.id} · ${saved.id}`}
          {savedKind === undefined ? "" : ` · ${saved.kind === undefined ? "" : t(savedKind)}`}
          {` · ${saved.enabled ? t("common.enabled") : t("common.disabled")}`}
          {saved.group === undefined ? "" : ` · ${t("network.lanes.write.groupValue", { name: saved.group })}`}
          {saved.node === undefined ? "" : ` · ${t("network.lanes.write.nodeValue", { name: saved.node })}`}
        </Typography>
      )}
    </Stack>
  );
}

function LanesSection({
  rows,
  isLoading,
  error,
  onRetry,
}: {
  rows: readonly LaneRow[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 行级写操作：删除（配置实体）与钉住节点（**只改内核运行态**，不改配置）。
  const removeOne = useCommandMutation("lane_delete");
  const setNode = useCommandMutation("lane_set_node");
  const [nodeDrafts, setNodeDrafts] = useState<Record<string, string>>({});
  const [rowError, setRowError] = useState<string | undefined>(undefined);
  const busy = removeOne.isPending || setNode.isPending;

  const onDelete = (id: string) => {
    setRowError(undefined);
    removeOne.reset();
    removeOne.mutate(
      { id },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.lanes });
        },
      },
    );
  };

  const onSetNode = (id: string) => {
    setRowError(undefined);
    setNode.reset();
    const node = (nodeDrafts[id] ?? "").trim();
    if (node === "") {
      setRowError(t("network.lanes.row.setNodeEmpty"));
      return;
    }
    setNode.mutate(
      { id, node },
      {
        onSuccess: () => {
          // 钉住是运行态：列表里的健康样本要重新拉才是新的
          void queryClient.invalidateQueries({ queryKey: qk.lanes });
        },
      },
    );
  };

  const deletedId = removeOne.isSuccess ? removeOne.variables?.id : undefined;
  const pinned = setNode.isSuccess ? setNode.variables : undefined;

  return (
    <SectionPaper
      title={t("network.lanes.title")}
      description={t("network.lanes.description")}
      hintTestId="network-lanes-hint"
      hint={
        <Stack spacing={1}>
          <Caption>{t("network.lanes.kindHint")}</Caption>
          <Caption>{t("network.lanes.channelCountHint")}</Caption>
          <Caption>{t("network.lanes.sampleHint")}</Caption>
          <Caption>{t("network.lanes.row.setNodeHint")}</Caption>
          <MissingCommandNote messageKey="network.missingCommands.lanes" />
        </Stack>
      }
      testId="network-lanes"
    >
      {error !== undefined && error !== null ? (
        <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Typography variant="body1" data-testid="network-lanes-error" sx={{ color: "error.main" }}>
            {errorMessage(error)}
          </Typography>
          <Button variant="outlined" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </Stack>
      ) : rows === undefined ? (
        <Typography variant="body1" sx={{ color: "text.secondary" }}>
          {isLoading ? t("common.loading") : DASH}
        </Typography>
      ) : rows.length === 0 ? (
        <Typography variant="body1" data-testid="network-lanes-empty">
          {t("network.lanes.empty")}
        </Typography>
      ) : (
        <Box sx={TABLE_WRAP_SX}>
          <Table size="small" data-testid="network-lanes-table">
            <TableHead>
              <TableRow>
                <TableCell>{t("network.lanes.col.name")}</TableCell>
                <TableCell>{t("network.lanes.col.kind")}</TableCell>
                <TableCell>{t("network.lanes.col.group")}</TableCell>
                <TableCell>{t("network.lanes.col.node")}</TableCell>
                <TableCell>{t("network.lanes.col.filter")}</TableCell>
                <TableCell>{t("network.lanes.col.enabled")}</TableCell>
                <TableCell>{t("network.lanes.col.health")}</TableCell>
                <TableCell align="right">{t("network.lanes.col.latency")}</TableCell>
                <TableCell align="right">{t("network.lanes.col.nodes")}</TableCell>
                <TableCell align="right">{t("network.lanes.col.channels")}</TableCell>
                <TableCell align="right">{t("network.lanes.col.actions")}</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => {
                const kindLabel = laneKindLabel(row.kind);
                return (
                  <TableRow key={row.id} hover data-testid="network-lane-row">
                    <TableCell>{row.name === undefined ? row.id : row.name}</TableCell>
                    <TableCell>
                      {row.kind === undefined
                        ? DASH
                        : kindLabel === undefined
                          ? // 契约外的类型原样显示（后端说的话，不改写）
                            row.kind
                          : t(kindLabel)}
                    </TableCell>
                    {/* 组名来自订阅，可能含 emoji：原样显示、不过滤、不清洗（[15 §2.2]） */}
                    <TableCell sx={{ overflowWrap: "anywhere" }}>{row.group ?? DASH}</TableCell>
                    <TableCell sx={{ overflowWrap: "anywhere" }}>{row.node ?? DASH}</TableCell>
                    <TableCell sx={{ fontFamily: typography.fontMono, maxWidth: 200, overflowWrap: "anywhere" }}>
                      {/* 二次收窄的两个正则合成一句；都空就是没有收窄 */}
                      {filterSummary(row.filter, row.excludeFilter, DASH)}
                    </TableCell>
                    <TableCell>{row.enabled ? t("common.enabled") : t("common.disabled")}</TableCell>
                    <TableCell>
                      <HealthCell health={row.health} raw={row.healthRaw} />
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.latencyMs === undefined ? DASH : formatLatency(row.latencyMs)}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.nodeCount === undefined ? <Unreadable /> : row.nodeCount}
                    </TableCell>
                    <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                      {row.channelCount === undefined ? <Unreadable /> : row.channelCount}
                    </TableCell>
                    <TableCell align="right" sx={{ whiteSpace: "nowrap" }}>
                      <Stack spacing={2} alignItems="flex-end">
                        {row.kind === "direct" ? (
                          <Caption>{t("network.lanes.row.directNoGroup")}</Caption>
                        ) : (
                          <Stack direction="row" spacing={2} alignItems="center">
                            <Box data-testid="network-lanes-set-node" sx={{ width: 160 }}>
                              <TextField
                                size="small"
                                label={t("network.lanes.row.setNode")}
                                value={nodeDrafts[row.id] ?? ""}
                                onChange={(event) =>
                                  setNodeDrafts((previous) => ({
                                    ...previous,
                                    [row.id]: event.target.value,
                                  }))
                                }
                              />
                            </Box>
                            <Button
                              size="small"
                              variant="outlined"
                              data-testid="network-lanes-set-node-submit"
                              disabled={busy}
                              onClick={() => onSetNode(row.id)}
                            >
                              {t("network.lanes.row.setNodeSubmit")}
                            </Button>
                          </Stack>
                        )}
                        <Button
                          size="small"
                          variant="outlined"
                          data-testid="network-lanes-delete"
                          disabled={busy}
                          onClick={() => onDelete(row.id)}
                        >
                          {t("network.lanes.row.delete")}
                        </Button>
                      </Stack>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}

      {/* 行级写操作的结果：钉住成功 = 后端接受了这个名字（失败会带 TA 码原样显示） */}
      {pinned === undefined ? null : (
        <Typography variant="body1" data-testid="network-lanes-set-node-result" sx={{ overflowWrap: "anywhere" }}>
          {t("network.lanes.row.setNodeResult", { lane: pinned.id, node: pinned.node })}
        </Typography>
      )}

      {deletedId === undefined ? null : (
        <Typography variant="body1" data-testid="network-lanes-delete-result">
          {t("network.lanes.row.deleteDone", { id: deletedId })}
        </Typography>
      )}

      <FormNotice testId="network-lanes-row-error" message={rowError} />
      <WriteError testId="network-lanes-row-error" error={setNode.error ?? removeOne.error} />

      <Divider />

      <LaneWriteForm />
    </SectionPaper>
  );
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据/读不到"）

export type NetworkBodyProps = {
  status: AppStatus | undefined;
  subscriptions: SubscriptionRow[] | undefined;
  subscriptionsLoading: boolean;
  subscriptionsError: unknown;
  lanes: LaneRow[] | undefined;
  lanesLoading: boolean;
  lanesError: unknown;
  onRetry: () => void;
};

export function NetworkBody({
  status,
  subscriptions,
  subscriptionsLoading,
  subscriptionsError,
  lanes,
  lanesLoading,
  lanesError,
  onRetry,
}: NetworkBodyProps) {
  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      <ZeroSideEffectNotice />
      <CurrentExit status={status} lanes={lanes ?? []} />
      <SubscriptionsSection
        rows={subscriptions}
        isLoading={subscriptionsLoading}
        error={subscriptionsError}
        onRetry={onRetry}
      />
      <LanesSection rows={lanes} isLoading={lanesLoading} error={lanesError} onRetry={onRetry} />
    </Box>
  );
}

// ================================================================ 数据层

export default function NetworkPage() {
  const { t } = useTranslation();
  // `app_status` 的唯一取数入口（与状态条共用同一份缓存）
  const status = useAppStatus({ refetchInterval: 30_000 });
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);

  const refresh = () => {
    void status.refetch();
    void subscriptions.refetch();
    void lanes.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.network")}
        description={t("network.description")}
        docRef="04 §5–§8 订阅与线路 · 12 §6 F4"
        actions={
          <>
            <Button variant="outlined" onClick={refresh}>
              {t("common.refresh")}
            </Button>
            <Button variant="outlined" component={Link} to="/network/nodes">
              {t("network.gotoNodes")}
            </Button>
            <Button variant="outlined" component={Link} to="/network/assignments">
              {t("network.gotoAssignments")}
            </Button>
          </>
        }
      />
      <NetworkBody
        status={status.data}
        subscriptions={
          subscriptions.data === undefined
            ? undefined
            : subscriptions.data.map((item) => readSubscription(item))
        }
        subscriptionsLoading={subscriptions.isPending}
        subscriptionsError={subscriptions.error ?? undefined}
        lanes={lanes.data === undefined ? undefined : lanes.data.map((item) => readLane(item))}
        lanesLoading={lanes.isPending}
        lanesError={lanes.error ?? undefined}
        onRetry={refresh}
      />
    </>
  );
}

/**
 * 网络页（[04 §5–§8]、[12 §6 F4]、[19 §2]）—— `/network` 概览，**以及三个网络视图共用的展示件**。
 *
 * ## 三条硬纪律
 *
 * 1. **I2：系统网络零副作用。** 这一页（含节点列表与线路分配）**不碰系统网络**：
 *    没有"改系统代理 / DNS / 路由 / 网卡 / 开机自启"的路径或按钮，也没有启停网关或内核的按钮。
 *    页面调用的命令是一条可机械检查的白名单（见 `src/app/network.test.tsx` 的命令面守卫）：
 *    只读的 `app_status` / `subscription_list` / `lane_list` / `config_get`，
 *    外加**只管订阅与线路这两个配置实体**的写命令（`subscription_*` / `lane_upsert` /
 *    `lane_delete` / `lane_set_node`）与**不改配置**的逐节点测速 `lane_test`
 *    —— 后者改的是 TokenAgency 自己的配置与内核运行态，系统网络配置一个字段都不动。
 *    守卫对"系统/内核/托盘写命令"仍然零容忍。
 * 2. **不猜。** 读不到的字段显示「未读取」，配置里确实是空的显示空状态，两者**不是一回事**；
 *    写操作**只显示后端返回的对象**（不拼一句"保存成功"），失败原样显示 TA 码。
 *    需要但尚未注册的后端接口如实点名，而不是编一份看起来完整的节点清单
 *    （见 `model.ts` 文件头「一」的三态区分）。
 * 3. **常显（[19 §2.2]，取代 [18 §2.2] 的"选装入口"）。** 网络代理**默认开启、默认直连**：
 *    这一页永远渲染出口 / 订阅 / 线路，没配置时给诚实的空态 —— 页面上**没有"启用网络代理"这个动作**。
 *    未配内核路径时出口显示「出口 · 直连」（`network.directLabel`），不是错误态、不点红点。
 *
 * ## 写界面与读界面为什么在同一页
 *
 * 订阅与线路是"看一眼、改一下、再看一眼"的闭环：列表里的每一行都带刷新 / 删除 / 测速，
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
import { Link, useSearchParams } from "react-router-dom";

import { errorMessage, qk, useAppStatus, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { AppStatus } from "@/shared/bindings";
import { KernelStateDot, PageHeader, StatusDot } from "@/shared/components";
import { formatBytes, formatDuration, formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
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
  readKernelPath,
  readLane,
  readSubscription,
  readTrafficMap,
  subscriptionDraftComplete,
  subscriptionFormatLabel,
  subscriptionPayload,
  summarizeTraffic,
  type LaneDraft,
  type LaneRow,
  type SubscriptionDraft,
  type SubscriptionRow,
  type TrafficRow,
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

// 上一版这里有一个 `MissingCommandNote`（"后端这个接口还没注册"的统一说明）与一个
// `OptionalNetworkBlock`（未配置时唯一的"启用网络代理"入口）。两者都随 [19 §2] 作废：
// - 节点清单 / 订阅解析 / 逐节点测速三条命令**都已接入**，那些"本页尚未接入"的说明是**过期说明**；
// - 网络代理默认开启（默认直连），页面上**没有"启用网络代理"这个动作**。
// 于是这一页不再有任何"这条命令还没注册"的占位文案 —— 真缺接口时再按需加回来。

// ================================================================ ⓪ 从服务商向导返回（[19 §1.4]）
//
// 服务商两步向导的「让我先设置网络代理」把用户送到 `#/network?from=provider-wizard`，
// 于是这一页顶部要给一条**回得去**的路：文案说清"正在为服务商配置网络代理"，
// 按钮回 `/providers`（向导的草稿存在页面级 store 里，回去还在）。
// 触发条件只看 URL 参数：不是"任何从别处进来的链接都弹一条返回条"。

export function ProviderReturnBanner() {
  const { t } = useTranslation();
  return (
    <Paper
      variant="outlined"
      data-testid="network-return-banner"
      sx={{ p: 4, borderRadius: `${radius.surface}px` }}
    >
      <Stack direction="row" spacing={3} alignItems="center" sx={{ flexWrap: "wrap" }}>
        <Typography variant="body1">{t("network.returnBanner")}</Typography>
        {/* MUI 控件不吃 `data-testid`（要取到真正的按钮得再往下一层）→ 外层普通元素包住它 */}
        <Box data-testid="network-return-btn">
          <Button variant="outlined" component={Link} to="/providers">
            {t("network.returnBtn")}
          </Button>
        </Box>
      </Stack>
    </Paper>
  );
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

function CurrentExit({
  status,
  lanes,
  kernelPath,
}: {
  status: AppStatus | undefined;
  lanes: readonly LaneRow[];
  /** `net.kernel.path`：`undefined` = 还没读到；`""` = 读到了、用户没配（[19 §2.2]）。 */
  kernelPath: string | undefined;
}) {
  const { t } = useTranslation();
  const gateway = status?.gateway;
  // 只有后端给了 id 才显示出口；能给名字就给名字，给不了解析就原样显示 id（不猜一个线路名）
  const laneId = status?.default_lane_id ?? undefined;
  const laneName = laneId === undefined ? undefined : laneNameOf(laneId, lanes);

  // 「未配内核路径」= **读到了**配置、且 `net.kernel.path` 是空的（[19 §2.2]）。
  // 这时出口就是**直连** —— 不是错误态、也不是"未启用"（网络代理默认开启，默认直连）。
  // `undefined`（还没读到）不算：那是"不知道"，不能冒充"直连"。
  const kernelNotConfigured = kernelPath !== undefined && kernelPath.trim() === "";
  const kernelPathValue = kernelPath === undefined ? undefined : kernelPath.trim();

  return (
    <SectionPaper
      title={t("network.currentExit.title")}
      description={t("network.currentExit.hint")}
      testId="network-current-exit"
    >
      <Stack spacing={2}>
        {kernelNotConfigured ? (
          // 「出口 · 直连」：用户看得懂的一句话，不是错误提示
          <Typography variant="body1" data-testid="network-direct-label">
            {`${t("status.exit")} · ${t("network.directLabel")}`}
          </Typography>
        ) : null}

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
          {kernelNotConfigured ? (
            // **不点红点**：没有内核路径只说明"走直连"，不是故障
            <Typography variant="body1" data-testid="network-kernel-not-configured" sx={{ color: "text.secondary" }}>
              {t("network.kernel.notConfigured")}
            </Typography>
          ) : (
            <>
              <Box component="span" data-testid="network-kernel-state-dot">
                <KernelStateDot state={status?.kernel.state} />
              </Box>
              {kernelPathValue === undefined || kernelPathValue === "" ? null : (
                <Typography
                  variant="caption"
                  component="span"
                  data-testid="network-kernel-path-value"
                  sx={{ color: "text.secondary", fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                >
                  {kernelPathValue}
                </Typography>
              )}
            </>
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.defaultLane")}>
          {kernelNotConfigured ? (
            <Typography variant="body1">{t("network.directLabel")}</Typography>
          ) : laneId === undefined ? (
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

/**
 * 一行上的**流量**（[18 §1.3]，等价于 Clash 面板那一行）：更新时间 + 已用/总量 + 百分比 + 到期。
 *
 * **三条各自独立，缺哪样就不显示那一样**（不显示 0、不显示占位符）：
 * 数字全部来自后端（`subscription_traffic` 的持久化观测值），前端只做格式化 ——
 * 不自己算 `total`、不把缺失当 0（`used = upload + download` 要两者都有才算）。
 *
 * 三态不混：`traffic === undefined`（还没读到）→「未读取」；`null`（从没成功刷新过）→「尚未获取」。
 */
function TrafficCell({ traffic }: { traffic: TrafficRow | null | undefined }) {
  const { t } = useTranslation();
  if (traffic === undefined) return <Unreadable />;
  const summary = summarizeTraffic(traffic);
  if (summary.kind === "none") {
    return (
      <Typography variant="body1" component="span" sx={{ color: "text.secondary" }}>
        {t("network.sub.traffic.none")}
      </Typography>
    );
  }
  const parts: string[] = [];
  if (summary.fetchedAtMs !== undefined) {
    // 本地时区 `YYYY-MM-DD HH:mm`（Clash 面板同款）
    parts.push(t("network.sub.traffic.updated", { at: formatLocalTime(summary.fetchedAtMs, "YYYY-MM-DD HH:mm") }));
  }
  if (summary.usedBytes !== undefined) {
    parts.push(
      summary.totalBytes === undefined
        ? t("network.sub.traffic.usedOnly", { used: formatBytes(summary.usedBytes) })
        : t("network.sub.traffic.used", {
            used: formatBytes(summary.usedBytes),
            total: formatBytes(summary.totalBytes),
          }),
    );
  }
  if (summary.usedPct !== undefined) {
    parts.push(t("network.sub.traffic.percent", { pct: summary.usedPct }));
  }
  if (summary.expireAtS !== undefined) {
    // 契约是 **Unix 秒**
    parts.push(t("network.sub.traffic.expire", { date: formatLocalTime(summary.expireAtS * 1000, "YYYY-MM-DD") }));
  }
  if (parts.length === 0) return <Unreadable />;
  return (
    <Typography variant="body1" component="span">
      {parts.join(" · ")}
    </Typography>
  );
}

function SubscriptionsSection({
  rows,
  isLoading,
  error,
  trafficError,
  onRetry,
}: {
  rows: readonly SubscriptionRow[] | undefined;
  isLoading: boolean;
  error: unknown;
  /** `subscription_traffic` 自己的读取错误（与 `subscription_list` 的错误分开说，不吞错）。 */
  trafficError: unknown;
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
          {/* 「节点数 / 组数从哪来」那一句（`network.subscriptions.countHint`）已作废：
              它写着"本页尚未接入 subscription_nodes / subscription_groups"，而这两条命令
              这一页真的在调（见 `LaneWriteForm`）；/network/nodes 上更是直接列出了节点清单（[19 §2.3]）。 */}
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
                    {/* 流量：**上次已知**的观测值（`subscription_traffic`）。缺哪样不显示哪样 */}
                    <TableCell
                      align="right"
                      data-testid="network-subscription-traffic"
                      sx={{ fontVariantNumeric: "tabular-nums" }}
                    >
                      <TrafficCell traffic={row.traffic} />
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

      {/* 流量（`subscription_traffic`）读失败：**如实说**，不把失败画成"尚未获取" */}
      <WriteError testId="network-subscriptions-traffic-error" error={trafficError} />

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
  // 行级写操作：删除（配置实体）、钉住节点（**只改内核运行态**，不改配置）、逐节点测速（[19 §2.4]）。
  const removeOne = useCommandMutation("lane_delete");
  const setNode = useCommandMutation("lane_set_node");
  const testLane = useCommandMutation("lane_test");
  const [nodeDrafts, setNodeDrafts] = useState<Record<string, string>>({});
  const [rowError, setRowError] = useState<string | undefined>(undefined);
  const busy = removeOne.isPending || setNode.isPending || testLane.isPending;

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
  // 测速结果**原样显示后端的话**：成功就逐节点列延迟，失败就显示后端原文（含 TA 码）。
  const tested = testLane.variables?.id;
  const testFailed = testLane.error !== null && testLane.error !== undefined;

  const onTest = (id: string) => {
    setRowError(undefined);
    testLane.reset();
    testLane.mutate({ id });
  };

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
          {/* 测速的口径（[19 §2.4]）：**没有装配内核时后端会如实报错**，这里也如实显示 */}
          <Caption>{t("network.lanes.testHint")}</Caption>
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
                          data-testid="network-lanes-test"
                          disabled={busy}
                          onClick={() => onTest(row.id)}
                        >
                          {t("network.lanes.test")}
                        </Button>
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

      {/* 逐节点测速：成功逐行列延迟；失败（例如没装配内核）**原样显示后端原文** */}
      {tested === undefined ? null : (
        <Typography
          variant="body1"
          data-testid="network-lanes-test-result"
          sx={{ overflowWrap: "anywhere", color: testFailed ? "error.main" : "text.primary" }}
        >
          {testFailed
            ? `${t("network.lanes.test")} ${tested}：${errorMessage(testLane.error)}`
            : `${t("network.lanes.test")} ${tested}：${
                testLane.data === undefined || testLane.data.length === 0
                  ? DASH
                  : testLane.data
                      .map((row) =>
                        row.latency_ms === null || row.latency_ms === undefined
                          ? row.node
                          : `${row.node} ${formatLatency(row.latency_ms)}`,
                      )
                      .join(" · ")
              }`}
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
  /**
   * `net.kernel.path`（来自 `config_get`）。
   * `undefined` = **还没读到** —— 与"读到了、但为空（用户没配内核路径）"不是一回事（[18 §2.1]）。
   * 空值时出口显示「出口 · 直连」，**不是**错误态（[19 §2.2]）。
   */
  kernelPath: string | undefined;
  subscriptions: SubscriptionRow[] | undefined;
  subscriptionsLoading: boolean;
  subscriptionsError: unknown;
  /** 流量（`subscription_traffic`）的读取错误：与订阅列表的错误**分开**，不吞错也不混为一谈。 */
  trafficError?: unknown;
  lanes: LaneRow[] | undefined;
  lanesLoading: boolean;
  lanesError: unknown;
  onRetry: () => void;
  /**
   * 是否显示「从服务商向导返回」的返回条（[19 §1.4]）。
   *
   * 由**数据层**从真实 URL 的 `?from=provider-wizard` 读出来传进来（`NetworkBody` 是纯 props 展示层，
   * 不自己解 URL —— 那样展示层的单测就都得包一层 Router）。
   */
  showProviderReturn?: boolean;
};

/**
 * 网络概览的展示层（纯 props，便于测"有数据 / 空 / 读不到"三种状态）。
 *
 * **[19 §2.2] 之后这里没有任何"门控"**：网络代理默认开启（默认直连），所以这一页
 * **永远**渲染出口 / 订阅 / 线路三块 —— 没配置时每块给**诚实的空态**（不是空表、不是白纸），
 * 但页面上**没有"启用网络代理"这个动作**（上一版的 `network-optional` 已作废）。
 */
export function NetworkBody({
  status,
  kernelPath,
  subscriptions,
  subscriptionsLoading,
  subscriptionsError,
  trafficError,
  lanes,
  lanesLoading,
  lanesError,
  onRetry,
  showProviderReturn = false,
}: NetworkBodyProps) {
  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {showProviderReturn ? <ProviderReturnBanner /> : null}
      <ZeroSideEffectNotice />
      <CurrentExit status={status} lanes={lanes ?? []} kernelPath={kernelPath} />
      <SubscriptionsSection
        rows={subscriptions}
        isLoading={subscriptionsLoading}
        error={subscriptionsError}
        trafficError={trafficError}
        onRetry={onRetry}
      />
      <LanesSection rows={lanes} isLoading={lanesLoading} error={lanesError} onRetry={onRetry} />
    </Box>
  );
}

// ================================================================ 数据层

export default function NetworkPage() {
  const { t } = useTranslation();
  // 返回条的触发条件是**真实 URL**（[19 §1.4]）：服务商向导里的「让我先设置网络代理」
  // 跳到 `#/network?from=provider-wizard`，这里据此给一条回得去的路。
  const [searchParams] = useSearchParams();
  const showProviderReturn = searchParams.get("from") === "provider-wizard";
  // `app_status` 的唯一取数入口（与状态条共用同一份缓存）
  const status = useAppStatus({ refetchInterval: 30_000 });
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  // 出口的「直连 / 线路名」判定要用 `net.kernel.path`（[19 §2.2]）：它只在 `config_get` 里，
  // 而 `config_get` 是**原子快照**（与分配页共用同一份缓存，`config:changed` 事件会失效它）。
  const config = useCommandQuery("config_get", undefined, qk.config);
  // 订阅流量是**运行时观测值**（配置实体里没有它，[18 §1.2]）：单独一条只读命令读"上次已知"。
  // query key 挂在 `qk.subscriptions` 前缀下 —— 于是 `subscription:updated` 事件、
  // 以及行上的刷新/删除（都 invalidate `qk.subscriptions`）会连带把流量一起失效重拉。
  const traffic = useCommandQuery("subscription_traffic", undefined, [
    ...qk.subscriptions,
    "traffic",
  ]);

  const refresh = () => {
    void status.refetch();
    void subscriptions.refetch();
    void lanes.refetch();
    void config.refetch();
    void traffic.refetch();
  };

  // 列表（配置实体）× 流量（观测值）在这里合流：`Map` 里没有这个 id = 还没读到（→ 未读取）
  const trafficById = traffic.data === undefined ? undefined : readTrafficMap(traffic.data);

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
        kernelPath={config.data === undefined ? undefined : readKernelPath(config.data)}
        subscriptions={
          subscriptions.data === undefined
            ? undefined
            : subscriptions.data.map((item) => {
                const row = readSubscription(item);
                // `undefined` = 流量还没读到（与 `null` = 从没成功刷新过**不是一回事**）
                return { ...row, traffic: trafficById?.get(row.id) };
              })
        }
        subscriptionsLoading={subscriptions.isPending}
        subscriptionsError={subscriptions.error ?? undefined}
        trafficError={traffic.error ?? undefined}
        lanes={lanes.data === undefined ? undefined : lanes.data.map((item) => readLane(item))}
        lanesLoading={lanes.isPending}
        lanesError={lanes.error ?? undefined}
        onRetry={refresh}
        showProviderReturn={showProviderReturn}
      />
    </>
  );
}

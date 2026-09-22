/**
 * 网络页（[04 §5–§8]、[12 §6 F4]）—— `/network` 概览，**以及三个网络视图共用的展示件**。
 *
 * ## 两条硬纪律
 *
 * 1. **I2：系统网络零副作用。** 这一页（含节点列表与线路分配）**只展示**：
 *    没有任何"顺带改系统代理 / DNS / 路由 / 网卡 / 开机自启"的路径或按钮。
 *    页面调用的命令是一条可机械检查的白名单（见 `src/app/network.test.tsx` 的命令面守卫）：
 *    `app_status` / `subscription_list` / `lane_list` / `config_get` —— **全部只读**。
 * 2. **不猜。** 读不到的字段显示「未读取」，配置里确实是空的显示空状态，两者**不是一回事**；
 *    需要但尚未注册的后端接口如实点名，而不是编一份看起来完整的节点清单
 *    （见 `model.ts` 文件头「一」的三态区分）。
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

import { Box, Button, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { errorMessage, qk, useAppStatus, useCommandQuery } from "@/shared/api";
import type { AppStatus } from "@/shared/bindings";
import { KernelStateDot, PageHeader, StatusDot } from "@/shared/components";
import { formatBytes, formatDuration, formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  HEALTH_LABEL,
  HEALTH_STATUS,
  filterSummary,
  isKnownHealth,
  laneKindLabel,
  laneNameOf,
  readLane,
  readSubscription,
  subscriptionFormatLabel,
  type LaneRow,
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

// ================================================================ ② 订阅

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
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}
    </SectionPaper>
  );
}

// ================================================================ ③ 线路

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
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}
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

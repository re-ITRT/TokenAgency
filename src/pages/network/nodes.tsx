/**
 * 节点列表 `/network/nodes`（[04 §7 线路类型、选择与健康检查]、[12 §6 F4]、[19 §2.3]）。
 *
 * ## 这一页现在真的画出**完整节点清单**
 *
 * 用户原话：「我添加了 clash 订阅，刷新了，进去也是为什么这里没有完整的节点列表」+「没有节点啊」。
 * 上一版页面里那句"节点清单需要订阅解析命令 `subscription_nodes`…本页尚未接入"是**过期说明**：
 * 那条命令后端**已注册已实现**（`src-tauri/src/commands/net.rs:780`）。现在这一页：
 *
 * 1. `subscription_list` → 每份订阅的解析状态（`last_update_ms` / `error`）；
 * 2. **先判硬前置**（[19 §2.3]）：`last_update_ms == null && error == null` = 这份订阅还没被解析采纳过
 *    → **这一轮根本不发** `subscription_nodes`。理由不是省一次调用：后端的 `latest_outcome`
 *    在没有已采纳版本时返回的是**错误**（"订阅 {id} 还没有已采纳的版本（先用 subscription_refresh 拉一次）"），
 *    **不是空数组** —— 直接发命令会把一次必然失败画成"这份订阅没有节点"；
 * 3. 前置满足才调 `subscription_nodes({ id })`（**不带 `group`**：后端带 `group` 会**直接报错**，
 *    见 `net.rs:765-773`），把后端给的 `NodeView` 逐行画出来；
 * 4. 空态分**两档**且文案不同（还没解析过 / 这条订阅下确实没有节点），错误**原样显示**（含 TA 码）。
 *
 * **不自己解析订阅内容、不伪造节点**；也**不放** `nodes-group-filter`：本轮不做组筛选 ——
 * 后端按组取成员需要先给 `ta-subscription` 的 `GroupMeta` 加 `members`（跨范围改动），
 * 宁可不放，也不放一个点了会报错的筛选器。
 *
 * ## 另外两块仍然只画**拿得到的**证据
 *
 * 1. **每条线路声明的节点来源**：`lane.group` / `lane.node` / `lane.filter` —— 配置里的字面值，
 *    **原样显示**（用户数据里的 emoji 与特殊字符不做任何替换或清洗，[15 §2.2]）；
 * 2. **后端最近一次健康样本**：`lane_list` 的 `latest`（`ta_store::HealthSample`）——
 *    节点、延迟、结果、上游状态码、采样时间。没有样本就显示未知。
 *
 * ## I2
 *
 * 测速由内核负责（`lane_test` 挂在 `/network` 的线路表上，[19 §2.4]）：这一页**不发起任何探测**，
 * 更不碰系统网络。页面调用的命令是一条可机械检查的白名单（命令面守卫见 `src/app/network.test.tsx`）。
 */

import {
  Box,
  Button,
  MenuItem,
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
import { useState } from "react";
import { Link } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import { PageHeader, StatusDot } from "@/shared/components";
import { formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import { Caption, HealthCell, SectionPaper, ZeroSideEffectNotice } from "./index";
import {
  DASH,
  HEALTH_LABEL,
  HEALTH_STATUS,
  filterSummary,
  laneKindLabel,
  readLane,
  readSubscription,
  type LaneHealth,
  type LaneRow,
  type SubscriptionRow,
} from "./model";

/**
 * 这份订阅**有没有已采纳的解析结果**（[19 §2.3] 的硬前置，判定只在这一处）。
 *
 * `readSubscription` 把后端的 `null` 收成 `undefined`，所以"两个都没有"= 还没解析过。
 * 反过来（任一有值）也算"有解析结果"：刷新失败会留下 `error`，那说明后端**确实处理过**
 * 这份订阅 —— 这时该发 `subscription_nodes` 让后端自己说结论，而不是前端替它下判断。
 */
function hasParsedOutcome(row: SubscriptionRow | undefined): boolean {
  return row !== undefined && (row.lastUpdateMs !== undefined || row.error !== undefined);
}

/** `NodeView.ok` → 全站统一的健康语义（`ok` 缺失 = 未知，**不默认成功**）。 */
function nodeHealth(ok: boolean | null | undefined): LaneHealth {
  if (ok === null || ok === undefined) return "unknown";
  return ok ? "ok" : "down";
}

// ================================================================ ① 节点清单（真命令）

/**
 * 节点清单：`subscription_list` → 前置判定 → `subscription_nodes` → 逐行显示。
 *
 * 为什么这一块自己取数（而不是像 samples / declared 那样收 props）：它要**按选中的订阅**
 * 取数，而"选了哪一份"是这一块的局部状态；把它提升到页面级只会让 props 多两个没人用的字段。
 */
function NodeListSection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const rows = subscriptions.data?.map((item) => readSubscription(item));

  // 默认用后端清单里的第一份；用户选了就以他选的为准（**只有一个订阅时选择器也显示**，[19 §2.3]）
  const [pickedId, setPickedId] = useState<string | undefined>(undefined);
  const selectedId = pickedId ?? rows?.[0]?.id;
  const selected = rows?.find((row) => row.id === selectedId);
  // 前置没满足（还没解析过）→ 这一轮不发 `subscription_nodes`（见文件头「2」）
  const unparsed = selected !== undefined && !hasParsedOutcome(selected);

  const nodes = useCommandQuery(
    "subscription_nodes",
    { id: selectedId ?? "" },
    [...qk.subscriptions, "nodes", selectedId],
    { enabled: selectedId !== undefined && !unparsed },
  );

  const refresh = useCommandMutation("subscription_refresh");
  const onRefresh = () => {
    if (selectedId === undefined) return;
    refresh.reset();
    refresh.mutate(
      { id: selectedId, force: true },
      {
        // 顺序是**先刷新、再取清单**：`qk.subscriptions` 是节点清单 query key 的前缀，
        // 一次失效把 `subscription_list`（解析状态）与 `subscription_nodes` 一起重拉。
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  const refreshError = refresh.error;

  return (
    <SectionPaper
      title={t("nav.networkNodes")}
      description={t("network.nodes.description")}
      testId="network-nodes-command-section"
    >
      <Stack spacing={3}>
        {subscriptions.error === null || subscriptions.error === undefined ? null : (
          <Typography
            variant="body1"
            data-testid="nodes-error"
            sx={{ color: "error.main", overflowWrap: "anywhere" }}
          >
            {errorMessage(subscriptions.error)}
          </Typography>
        )}

        {rows === undefined ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {subscriptions.isPending ? t("common.loading") : DASH}
          </Typography>
        ) : rows.length === 0 ? (
          // 一份订阅都没有：没有可选的清单，也没有可刷新的对象 —— 如实说，别画一个空表
          <Typography variant="body1" data-testid="nodes-empty">
            {t("network.subscriptions.empty")}
          </Typography>
        ) : (
          <>
            <Stack direction="row" spacing={2} alignItems="flex-start" sx={{ flexWrap: "wrap" }}>
              {/* MUI 的 Select 不吃 `data-testid` 在输入层：testid 挂在容器上（与既有页面同法） */}
              <Stack spacing={1} data-testid="nodes-subscription" sx={{ minWidth: 240 }}>
                <TextField
                  select
                  size="small"
                  label={t("network.nodes.subscription")}
                  value={selectedId ?? ""}
                  onChange={(event) => setPickedId(event.target.value)}
                >
                  {rows.map((row) => (
                    <MenuItem key={row.id} value={row.id}>
                      {row.name ?? row.id}
                    </MenuItem>
                  ))}
                </TextField>
              </Stack>
              <Button
                variant="outlined"
                data-testid="nodes-refresh"
                disabled={refresh.isPending}
                onClick={onRefresh}
              >
                {t("common.refresh")}
              </Button>
            </Stack>

            {unparsed ? (
              // 第一档空态：**还没解析过**（提示点一次刷新）—— 命令没发，所以这不是"没有节点"
              <Typography variant="body1" data-testid="nodes-empty">
                {t("network.nodes.emptyUnparsed")}
              </Typography>
            ) : nodes.error === null || nodes.error === undefined ? (
              nodes.data === undefined ? (
                <Typography variant="body1" sx={{ color: "text.secondary" }}>
                  {nodes.isPending ? t("common.loading") : DASH}
                </Typography>
              ) : nodes.data.length === 0 ? (
                // 第二档空态：**确实没有节点**
                <Typography variant="body1" data-testid="nodes-empty">
                  {t("network.nodes.emptyGroup")}
                </Typography>
              ) : (
                <Box sx={{ overflowX: "auto" }}>
                  <Table size="small" data-testid="nodes-list">
                    <TableHead>
                      <TableRow>
                        <TableCell>{t("network.nodes.samples.col.node")}</TableCell>
                        <TableCell>{t("network.nodes.samples.col.kind")}</TableCell>
                        <TableCell>{t("network.nodes.group")}</TableCell>
                        <TableCell align="right">
                          {t("network.nodes.samples.col.latency")}
                        </TableCell>
                        <TableCell>{t("network.nodes.samples.col.ok")}</TableCell>
                      </TableRow>
                    </TableHead>
                    <TableBody>
                      {nodes.data.map((node, index) => {
                        const health = nodeHealth(node.ok);
                        return (
                          // 节点名在实际订阅里**可能重名**（机场很常见），所以 key 不能只用名字
                          <TableRow key={`${node.name}-${index}`} hover data-testid="nodes-row">
                            {/* 节点名 / 组名来自订阅：**原样显示**，一个字符都不改（[15 §2.2]） */}
                            <TableCell sx={{ overflowWrap: "anywhere" }}>{node.name}</TableCell>
                            <TableCell sx={{ fontFamily: typography.fontMono }}>{node.type}</TableCell>
                            <TableCell sx={{ overflowWrap: "anywhere" }}>
                              {node.group === null || node.group === undefined || node.group === ""
                                ? DASH
                                : node.group}
                            </TableCell>
                            <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                              {node.latency_ms === null || node.latency_ms === undefined
                                ? DASH
                                : formatLatency(node.latency_ms)}
                            </TableCell>
                            <TableCell>
                              <StatusDot status={HEALTH_STATUS[health]} label={t(HEALTH_LABEL[health])} />
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                </Box>
              )
            ) : (
              // 失败：**后端原文**（含 TA 码），不吞、不改写成"没有节点"
              <Typography
                variant="body1"
                data-testid="nodes-error"
                sx={{ color: "error.main", overflowWrap: "anywhere" }}
              >
                {errorMessage(nodes.error)}
              </Typography>
            )}
          </>
        )}

        {/* 刷新结果 = 后端返回的事实（ok / 节点数 / 组数 / 后端原文），不是前端拼的一句"刷新成功" */}
        {refresh.data === undefined ? null : (
          <Typography variant="body1" data-testid="nodes-refresh-result" sx={{ overflowWrap: "anywhere" }}>
            {`${t("network.subscriptions.write.refreshTitle")}：${
              refresh.data.ok
                ? t("network.subscriptions.write.refreshOk")
                : t("network.subscriptions.write.refreshFailed")
            }`}
            {` · ${t("network.subscriptions.col.nodes")} ${refresh.data.node_count}`}
            {` · ${t("network.subscriptions.col.groups")} ${refresh.data.group_count}`}
            {refresh.data.error === undefined || refresh.data.error === null
              ? ""
              : ` · ${refresh.data.error}`}
          </Typography>
        )}

        {refreshError === null || refreshError === undefined ? null : (
          <Typography
            variant="body1"
            data-testid="nodes-refresh-error"
            sx={{ color: "error.main", overflowWrap: "anywhere" }}
          >
            {errorMessage(refreshError)}
          </Typography>
        )}
      </Stack>
    </SectionPaper>
  );
}

// ================================================================ ② 健康样本 / 声明来源（纯 props）

export type NodesBodyProps = {
  lanes: LaneRow[] | undefined;
  lanesLoading: boolean;
  lanesError: unknown;
  onRetry: () => void;
};

export function NodesBody({ lanes, lanesLoading, lanesError, onRetry }: NodesBodyProps) {
  const { t } = useTranslation();

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      <ZeroSideEffectNotice />

      {/* ① 完整节点清单：真调 `subscription_nodes`（[19 §2.3]） */}
      <NodeListSection />

      {/* ② 最近健康样本：来自 lane_list 的 latest */}
      <SectionPaper
        title={t("network.nodes.samples.title")}
        description={t("network.nodes.samples.description")}
        testId="network-nodes-samples-section"
      >
        {lanesError !== undefined && lanesError !== null ? (
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            <Typography variant="body1" data-testid="network-nodes-error" sx={{ color: "error.main" }}>
              {errorMessage(lanesError)}
            </Typography>
            <Button variant="outlined" onClick={onRetry}>
              {t("common.retry")}
            </Button>
          </Stack>
        ) : lanes === undefined ? (
          <Typography variant="body1" data-testid="network-nodes-loading" sx={{ color: "text.secondary" }}>
            {lanesLoading ? t("common.loading") : DASH}
          </Typography>
        ) : lanes.length === 0 ? (
          <Typography variant="body1" data-testid="network-nodes-empty">
            {t("network.nodes.samples.empty")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small" data-testid="network-nodes-samples">
              <TableHead>
                <TableRow>
                  <TableCell>{t("network.nodes.samples.col.lane")}</TableCell>
                  <TableCell>{t("network.nodes.samples.col.kind")}</TableCell>
                  <TableCell>{t("network.nodes.samples.col.node")}</TableCell>
                  <TableCell align="right">{t("network.nodes.samples.col.latency")}</TableCell>
                  <TableCell>{t("network.nodes.samples.col.ok")}</TableCell>
                  <TableCell align="right">{t("network.nodes.samples.col.status")}</TableCell>
                  <TableCell>{t("network.nodes.samples.col.sampledAt")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {lanes.map((row) => {
                  const kindLabel = laneKindLabel(row.kind);
                  return (
                    <TableRow key={row.id} hover data-testid="network-node-sample-row">
                      <TableCell>{row.name ?? row.id}</TableCell>
                      <TableCell>
                        {/* 与概览页同一套文案：认识的类型翻成中文，契约外的原样显示 */}
                        {row.kind === undefined
                          ? DASH
                          : kindLabel === undefined
                            ? row.kind
                            : t(kindLabel)}
                      </TableCell>
                      {/* 节点名来自订阅，可能含 emoji：原样显示 */}
                      <TableCell sx={{ overflowWrap: "anywhere" }}>{row.currentNode ?? DASH}</TableCell>
                      <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                        {row.latencyMs === undefined ? DASH : formatLatency(row.latencyMs)}
                      </TableCell>
                      <TableCell>
                        <HealthCell health={row.health} raw={row.healthRaw} />
                      </TableCell>
                      <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                        {row.sampleStatus === undefined ? DASH : row.sampleStatus}
                      </TableCell>
                      <TableCell sx={{ fontVariantNumeric: "tabular-nums" }}>
                        {row.sampledAtMs === undefined ? DASH : formatLocalTime(row.sampledAtMs)}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </Box>
        )}
      </SectionPaper>

      {/* ③ 每条线路声明的节点来源（配置里的字面值，原样） */}
      <SectionPaper
        title={t("network.nodes.declared.title")}
        description={t("network.nodes.declared.description")}
        hintTestId="network-nodes-declared-hint"
        hint={<Caption>{t("network.nodes.declared.hint")}</Caption>}
        testId="network-nodes-declared-section"
      >
        {lanes === undefined || lanes.length === 0 ? (
          <Typography variant="body1" data-testid="network-nodes-declared-empty">
            {t("network.nodes.declared.empty")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small" data-testid="network-nodes-declared">
              <TableHead>
                <TableRow>
                  <TableCell>{t("network.nodes.declared.col.lane")}</TableCell>
                  <TableCell>{t("network.nodes.declared.col.group")}</TableCell>
                  <TableCell>{t("network.nodes.declared.col.groupType")}</TableCell>
                  <TableCell>{t("network.nodes.declared.col.node")}</TableCell>
                  <TableCell>{t("network.nodes.declared.col.filter")}</TableCell>
                  <TableCell>{t("network.nodes.declared.col.subscription")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {lanes.map((row) => (
                  <TableRow key={row.id} hover data-testid="network-declared-row">
                    <TableCell>{row.name ?? row.id}</TableCell>
                    {/* 这三格的取值全部来自用户数据（订阅组名 / 节点名 / 自己的正则）：
                        **一个字符都不改**（[15 §2.2]） */}
                    <TableCell sx={{ overflowWrap: "anywhere" }}>
                      {row.group ?? t("network.nodes.declared.none")}
                    </TableCell>
                    {/* 组策略 = 内核的组类型（选择节点的方式），不是我们自己的策略实现 */}
                    <TableCell sx={{ fontFamily: typography.fontMono }}>
                      {row.groupType ?? t("network.nodes.declared.none")}
                    </TableCell>
                    <TableCell sx={{ overflowWrap: "anywhere" }}>
                      {row.node ?? t("network.nodes.declared.none")}
                    </TableCell>
                    <TableCell sx={{ fontFamily: typography.fontMono, maxWidth: 240, overflowWrap: "anywhere" }}>
                      {filterSummary(row.filter, row.excludeFilter, t("network.nodes.declared.none"))}
                    </TableCell>
                    <TableCell sx={{ fontFamily: typography.fontMono }}>
                      {row.subscriptionId ?? DASH}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Box>
        )}
      </SectionPaper>
    </Box>
  );
}

export default function NodesPage() {
  const { t } = useTranslation();
  // 与 `/network` 概览共用同一个 query key：一次取数、两页看
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);

  return (
    <>
      <PageHeader
        title={t("nav.networkNodes")}
        description={t("network.nodes.description")}
        docRef="04 §7 线路类型、选择与健康检查"
        actions={
          <>
            <Button variant="outlined" component={Link} to="/network">
              {t("network.backToOverview")}
            </Button>
            <Button variant="outlined" component={Link} to="/network/assignments">
              {t("network.gotoAssignments")}
            </Button>
          </>
        }
      />
      <NodesBody
        lanes={lanes.data === undefined ? undefined : lanes.data.map((item) => readLane(item))}
        lanesLoading={lanes.isPending}
        lanesError={lanes.error ?? undefined}
        onRetry={() => void lanes.refetch()}
      />
    </>
  );
}

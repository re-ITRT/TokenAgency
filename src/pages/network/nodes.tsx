/**
 * 节点列表 `/network/nodes`（[04 §7 线路类型、选择与健康检查]、[12 §6 F4]）。
 *
 * ## 这一页只画**拿得到的**东西
 *
 * 完整的节点清单需要订阅解析接口 **`subscription_nodes`**（可按组筛选）—— 它**尚未注册**。
 * 所以这一页不自己解析订阅内容、不伪造节点、也不放一个禁用的"刷新节点"按钮
 * （那会把"后端没有"伪装成"前端懒得做"）。它只展示两类**有出处**的事实：
 *
 * 1. **每条线路声明的节点来源**：`lane.group` / `lane.node` / `lane.filter` —— 配置里的字面值，
 *    **原样显示**（用户数据里的 emoji 与特殊字符不做任何替换或清洗，[15 §2.2]）；
 * 2. **后端最近一次健康样本**：`lane_list` 的 `latest`（`ta_store::HealthSample`）——
 *    节点、延迟、结果、上游状态码、采样时间。没有样本就显示未知。
 *
 * ## I2
 *
 * 测速由内核负责（`lane_test` 尚未注册）：这一页**不发起任何探测**，更不碰系统网络。
 * 页面调用的命令是 `lane_list` 这一条只读命令（命令面守卫见 `src/app/network.test.tsx`）。
 */

import { Box, Button, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { Link } from "react-router-dom";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader } from "@/shared/components";
import { formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import {
  Caption,
  HealthCell,
  MissingCommandNote,
  SectionPaper,
  ZeroSideEffectNotice,
} from "./index";
import { DASH, filterSummary, laneKindLabel, readLane, type LaneRow } from "./model";

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

      {/* ① 最近健康样本：这一页**唯一**的节点级证据（来自 lane_list 的 latest） */}
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
        <Caption>{t("network.nodes.samples.noProbe")}</Caption>
      </SectionPaper>

      {/* ② 每条线路声明的节点来源（配置里的字面值，原样） */}
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

      {/* ③ 缺席的东西要说清楚：为什么没有完整节点列表 */}
      <SectionPaper title={t("network.nodes.missing.title")} testId="network-nodes-missing">
        <Stack spacing={1}>
          <Caption>{t("network.nodes.missing.body")}</Caption>
          <Caption>{t("network.nodes.missing.testHint")}</Caption>
          <MissingCommandNote messageKey="network.missingCommands.nodes" />
        </Stack>
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

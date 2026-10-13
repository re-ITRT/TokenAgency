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
  Checkbox,
  FormControlLabel,
  MenuItem,
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
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import { PageHeader, HelpTip, StatusDot } from "@/shared/components";
import { formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import { HealthCell, SectionPaper, mergeHelpText } from "./index";
import {
  DASH,
  HEALTH_LABEL,
  HEALTH_STATUS,
  filterSummary,
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

/**
 * **单元格一律不换行**（规则 4，与 `/network` 那张表同一条口径）。
 *
 * **这一页刻意不做视觉裁剪**（回归守卫管着这个文件：节点清单属于
 * "点进去看详细"的那一类，**只展示不裁剪**）。所以长值靠 `white-space: nowrap` 保持一行、
 * 由外层 `overflowX: auto` 横向滚动 —— 全文始终在版面上，一个字符都不省。
 */
const CELL_NOWRAP_SX = { whiteSpace: "nowrap" } as const;

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

  // query key 提到外面：写命令成功后要**按同一个 key 回填**（见下面的 `writeEnabled`）
  const nodesKey = useMemo(() => [...qk.subscriptions, "nodes", selectedId], [selectedId]);
  const nodes = useCommandQuery("subscription_nodes", { id: selectedId ?? "" }, nodesKey, {
    enabled: selectedId !== undefined && !unparsed,
  });

  // ---------------------------------------------------------------- 节点级启用 / 关闭（用户第 202 轮）
  //
  // 用户原话：「订阅里面的节点，也将服务商的『模型』一样设置一个启用或关闭的功能，对了，这个也要
  // 那种有全选功能，默认全部选择」。
  //
  // 后端语义（`bindings.ts` 已冻结的契约）：**默认全部启用** —— 库里存的是"**关掉的那些**"
  // （`Subscription.disabled_nodes`），所以机场新增的节点默认就是开着的。
  //
  // 为什么批量是**一条命令**（`subscription_nodes_enabled`）而不是前端循环：一次用户动作 = 一次
  // 写库 + 一次内核重载；循环发 N 条既慢，中途失败还会留下"关了前半截"的中间态。而且它返回
  // **改完之后**的完整清单 —— 页面直接拿它回填缓存（`setQueryData`），既不用再发一次
  // `subscription_nodes`，也不会出现"界面上的开关和后端存的不一致"。
  const setEnabled = useCommandMutation("subscription_nodes_enabled");
  const [writeError, setWriteError] = useState<unknown>(undefined);
  /** 批量写成功后的**极简回执**（"已关闭 3 条"）；单行开关不写（开关自己就是回执）。 */
  const [writeNotice, setWriteNotice] = useState<string | undefined>(undefined);

  const nodeNames = useMemo(() => nodes.data?.map((node) => node.name) ?? [], [nodes.data]);
  /** 重名节点（机场很常见）在**选择**这一层只算一条：命令收的是名字，不是行号。 */
  const uniqueNames = useMemo(() => [...new Set(nodeNames)], [nodeNames]);
  /**
   * **默认全选**（用户原话「默认全部选择」）：用户没动过选择时 = 当前清单的全部节点。
   *
   * 只在**名字清单**变了的时候回到全选（换订阅、刷新后多/少了节点）—— 单纯启用状态的变化
   * （写完回填的那份清单）名字一个字没变，所以不会把用户的选择洗掉。
   */
  const listKey = `${selectedId ?? ""}\u0000${uniqueNames.join("\u0000")}`;
  const [pickedNames, setPickedNames] = useState<{ key: string; names: Set<string> } | undefined>(
    undefined,
  );
  /** 选中集合**按当前清单算**：清单里没有的名字一律不算选中（换订阅后不会把旧名字带过去）。 */
  const selectedNames = useMemo(() => {
    const set =
      pickedNames !== undefined && pickedNames.key === listKey
        ? pickedNames.names
        : new Set(uniqueNames);
    return uniqueNames.filter((name) => set.has(name));
  }, [pickedNames, listKey, uniqueNames]);
  const allSelected = uniqueNames.length > 0 && selectedNames.length === uniqueNames.length;
  const someSelected = selectedNames.length > 0 && !allSelected;
  /** 写命令在飞：开关与批量按钮一律禁用（避免第二次点击覆盖掉第一次的目标状态）。 */
  const busy = setEnabled.isPending;

  const select = (names: Set<string>): void => setPickedNames({ key: listKey, names });

  /**
   * 写一批节点的启用状态。`targets` 是**已经按当前清单算好**的节点名；空清单**不发命令**
   * （后端对空清单是**明确报错**：空清单不是「全部」，那会静默改掉整份订阅）。
   */
  const writeEnabled = (targets: readonly string[], enabled: boolean, receipt: boolean): void => {
    if (selectedId === undefined || targets.length === 0) return;
    setWriteError(undefined);
    setWriteNotice(undefined);
    setEnabled.mutate(
      { id: selectedId, nodes: [...targets], enabled },
      {
        onSuccess: (list) => {
          // 后端返回的就是"改完之后"：直接回填，界面与后端一个字都不会漂
          queryClient.setQueryData(nodesKey, list);
          if (receipt) {
            setWriteNotice(
              t(enabled ? "network.nodes.bulk.enabled" : "network.nodes.bulk.disabled", {
                count: targets.length,
              }),
            );
          }
        },
        // 失败：后端原文（含 TA 码）原样显示，不吞、不改写、不假装成功
        onError: (err) => setWriteError(err),
      },
    );
  };

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
      // 标题不再复用页头标题（`nav.networkNodes`）：同一屏出现两个「节点列表」（H1 + 区块标题）
      // 是同一个事实说两遍。这一块的内容是"订阅解析出来的节点"，就照这个说。
      title={t("network.nodes.list.title")}
      // 说明也不再重复页头那句：`network.nodes.description` 在页头已经有一份（`page-header-help`
      // 就是全文），同一句话在一屏里摆两遍不增加任何信息。
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
                <Stack spacing={2}>
                  {/* 批量动作条：计数**只留数字**（极简：同一句话不在这里再说一遍），
                      两个动作各说清改哪一半；一条都没选中时按钮进禁用态（功能是做了的，禁用不是占位）。 */}
                  <Stack
                    direction="row"
                    spacing={2}
                    alignItems="center"
                    data-testid="network-nodes-bulk-bar"
                    sx={{ flexWrap: "wrap" }}
                  >
                    <Typography variant="body1" data-testid="network-nodes-selected-count">
                      {t("network.nodes.selectedCount", { count: selectedNames.length })}
                    </Typography>
                    <Button
                      variant="outlined"
                      data-testid="network-nodes-enable-selected"
                      disabled={selectedNames.length === 0 || busy}
                      onClick={() => writeEnabled(selectedNames, true, true)}
                    >
                      {t("network.nodes.enableSelected")}
                    </Button>
                    <Button
                      variant="outlined"
                      data-testid="network-nodes-disable-selected"
                      disabled={selectedNames.length === 0 || busy}
                      onClick={() => writeEnabled(selectedNames, false, true)}
                    >
                      {t("network.nodes.disableSelected")}
                    </Button>
                    {writeError === null || writeError === undefined ? null : (
                      <Typography
                        variant="body1"
                        data-testid="network-nodes-bulk-result"
                        sx={{ color: "error.main", overflowWrap: "anywhere" }}
                      >
                        {errorMessage(writeError)}
                      </Typography>
                    )}
                    {writeError === null || writeError === undefined ? (
                      writeNotice === undefined ? null : (
                        <Typography variant="body1" data-testid="network-nodes-bulk-result">
                          {writeNotice}
                        </Typography>
                      )
                    ) : null}
                  </Stack>

                  <Box sx={{ overflowX: "auto" }}>
                    {/*
                      * **列数 ≤6 且不换行**（规则 4）：勾选框 + 节点 / 类型 / 分组 / 延迟（含结果）/ 启用。
                      * 「结果」与「延迟」本来是两列、说的是同一次采样（成不成、多快），合成一格之后
                      * 既少了横向挤压，也没有丢信息（圆点的文字标签还在）。
                      */}
                    <Table size="small" data-testid="nodes-list">
                      <TableHead>
                        <TableRow>
                          {/* 表头「全选」：三态（全选 / 全不选 / 部分），选中的按**当前清单**算 */}
                          <TableCell padding="checkbox">
                            <Box data-testid="network-nodes-select-all">
                              <Checkbox
                                size="small"
                                checked={allSelected}
                                indeterminate={someSelected}
                                disabled={busy}
                                onChange={(event) =>
                                  select(event.target.checked ? new Set(uniqueNames) : new Set())
                                }
                                slotProps={{ input: { "aria-label": t("network.nodes.selectAll") } }}
                              />
                            </Box>
                          </TableCell>
                          <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.node")}</TableCell>
                          <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.kind")}</TableCell>
                          <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.group")}</TableCell>
                          <TableCell sx={CELL_NOWRAP_SX}>
                            {t("network.nodes.samples.col.latency")}
                          </TableCell>
                          {/* 列名只在表头出现：行里只有开关本身（[15 §2] 符号化） */}
                          <TableCell sx={CELL_NOWRAP_SX}>{t("common.enabled")}</TableCell>
                        </TableRow>
                      </TableHead>
                      <TableBody>
                        {nodes.data.map((node, index) => {
                          const health = nodeHealth(node.ok);
                          return (
                            // 节点名在实际订阅里**可能重名**（机场很常见），所以 key 不能只用名字
                            <TableRow key={`${node.name}-${index}`} hover data-testid="nodes-row">
                              <TableCell padding="checkbox">
                                <Box data-testid="network-nodes-select">
                                  <Checkbox
                                    size="small"
                                    checked={selectedNames.includes(node.name)}
                                    disabled={busy}
                                    onChange={(event) => {
                                      const next = new Set(selectedNames);
                                      if (event.target.checked) next.add(node.name);
                                      else next.delete(node.name);
                                      select(next);
                                    }}
                                    // 无障碍名称用这一行自己的节点名（用户数据，不需要新键）
                                    slotProps={{ input: { "aria-label": node.name } }}
                                  />
                                </Box>
                              </TableCell>
                              {/* 节点名 / 组名来自订阅：**原样显示**，一个字符都不改（[15 §2.2]）。
                                  这一页不裁剪（回归守卫）：长名字靠 nowrap 保持一行。 */}
                              <TableCell sx={CELL_NOWRAP_SX}>{node.name}</TableCell>
                              <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                                {node.type}
                              </TableCell>
                              <TableCell sx={CELL_NOWRAP_SX}>
                                {node.group === null || node.group === undefined || node.group === ""
                                  ? DASH
                                  : node.group}
                              </TableCell>
                              {/* 结果（圆点 + 文字）与延迟合成一格 */}
                              <TableCell sx={CELL_NOWRAP_SX}>
                                <Stack direction="row" spacing={2} alignItems="center">
                                  <StatusDot
                                    status={HEALTH_STATUS[health]}
                                    label={t(HEALTH_LABEL[health])}
                                  />
                                  <Typography
                                    variant="caption"
                                    component="span"
                                    sx={{ color: "text.secondary", fontVariantNumeric: "tabular-nums" }}
                                  >
                                    {node.latency_ms === null || node.latency_ms === undefined
                                      ? DASH
                                      : formatLatency(node.latency_ms)}
                                  </Typography>
                                </Stack>
                              </TableCell>
                              <TableCell sx={CELL_NOWRAP_SX}>
                                {/* 启用 / 关闭的**符号**：开关本身就是那两个字，行里不再写一遍；
                                    `aria-label` 是这一行的节点名（读屏一个字不少，[15 §2]）。
                                    缺字段 = 没被关掉 = 启用（与库里存"关掉的那些"同语义）。 */}
                                <Stack data-testid="network-nodes-enabled">
                                  <FormControlLabel
                                    // 不画标签（符号化）；无障碍名称在 `inputProps` 的 aria-label 上
                                    label={undefined}
                                    control={
                                      <Switch
                                        size="small"
                                        checked={node.enabled !== false}
                                        disabled={busy}
                                        onChange={(_, next) => writeEnabled([node.name], next, false)}
                                        inputProps={{ "aria-label": node.name }}
                                      />
                                    }
                                  />
                                </Stack>
                              </TableCell>
                            </TableRow>
                          );
                        })}
                      </TableBody>
                    </Table>
                  </Box>
                </Stack>
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

/** 空值的统一占位（同一张表里三列各用一个，所以收敛成一个常量，改文案不会漏）。 */
const NONE_KEY = "network.nodes.declared.none";

export function NodesBody({ lanes, lanesLoading, lanesError, onRetry }: NodesBodyProps) {
  const { t } = useTranslation();

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* 零副作用承诺不再单独占一张卡片：它随页头那个问号走（见 `NodesPage` 的 description） */}

      {/* ① 完整节点清单：真调 `subscription_nodes`（[19 §2.3]） */}
      <NodeListSection />

      {/* ② 最近健康样本：来自 lane_list 的 latest */}
      <SectionPaper
        title={t("network.nodes.samples.title")}
        // 说明 + "本页不自己测速"那句合成**同一个**问号（规则 2：解释收进 `?`）
        description={mergeHelpText(
          t("network.nodes.samples.description"),
          t("network.nodes.samples.emptyHint"),
        )}
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
          // 空态：**1 行**（规则 5）。"本页不自己探测"那句在标题旁的问号里。
          <Typography variant="body1" data-testid="network-nodes-empty">
            {t("network.nodes.samples.empty")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            {/* **6 列且不换行**（规则 4）：这一张表要回答的是"最近一次采样成不成、多快、上游答了什么"，
                所以只留这五样证据 + 线路名；线路类型在 /network 的线路表里（同一份 `lane_list`），
                不在这里再说一遍。 */}
            <Table size="small" data-testid="network-nodes-samples">
              <TableHead>
                <TableRow>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.lane")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.node")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.latency")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.ok")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.status")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.sampledAt")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {lanes.map((row) => (
                  <TableRow key={row.id} hover data-testid="network-node-sample-row">
                    <TableCell sx={CELL_NOWRAP_SX}>{row.name ?? row.id}</TableCell>
                    {/* 节点名来自订阅，可能含 emoji：原样显示（这一页不裁剪） */}
                    <TableCell sx={CELL_NOWRAP_SX}>{row.currentNode ?? DASH}</TableCell>
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}>
                      {row.latencyMs === undefined ? DASH : formatLatency(row.latencyMs)}
                    </TableCell>
                    <TableCell sx={CELL_NOWRAP_SX}>
                      <HealthCell health={row.health} raw={row.healthRaw} />
                    </TableCell>
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}>
                      {row.sampleStatus === undefined ? DASH : row.sampleStatus}
                    </TableCell>
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}>
                      {row.sampledAtMs === undefined ? DASH : formatLocalTime(row.sampledAtMs)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Box>
        )}
      </SectionPaper>

      {/* ③ 每条线路声明的节点来源（配置里的字面值，原样） */}
      <SectionPaper
        title={t("network.nodes.declared.title")}
        // description 与 hint 说的是同一件事（"原样显示 + 只读"）→ 合成**一个**问号。
        // 为什么自己构造 `<HelpTip>` 而不是传 `description="…"`（渲染结果一样）：
        // 极简守卫只把**字面量就写在 `<HelpTip … />` 里**的键算成按需展开的说明，
        // 多键合并出来的长句会被它保守地误算成版面文案（见 `SectionPaper` 的 `help` prop 注释）。
        help={
          <HelpTip
            text={mergeHelpText(
              t("network.nodes.declared.description"),
              t("network.nodes.declared.hint"),
            )}
            testId="network-nodes-declared-section-help"
          />
        }
        testId="network-nodes-declared-section"
      >
        {lanes === undefined || lanes.length === 0 ? (
          <Typography variant="body1" data-testid="network-nodes-declared-empty">
            {t("network.nodes.declared.empty")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            {/* 这一张正好 6 列（规则 4 的上限）；单元格不换行 —— 长值不裁剪，
                由外层横向滚动兜住（用户数据一个字符都不许改，也不许因为长就看不见）。 */}
            <Table size="small" data-testid="network-nodes-declared">
              <TableHead>
                <TableRow>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.lane")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.group")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.groupType")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.node")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.filter")}</TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.declared.col.subscription")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {lanes.map((row) => (
                  <TableRow key={row.id} hover data-testid="network-declared-row">
                    <TableCell sx={CELL_NOWRAP_SX}>{row.name ?? row.id}</TableCell>
                    {/* 这三格的取值全部来自用户数据（订阅组名 / 节点名 / 自己的正则）：
                        **一个字符都不改**（[15 §2.2]）—— 这一页也不做视觉裁剪。 */}
                    <TableCell sx={CELL_NOWRAP_SX}>{row.group ?? t(NONE_KEY)}</TableCell>
                    {/* 组策略 = 内核的组类型（选择节点的方式），不是我们自己的策略实现 */}
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                      {row.groupType ?? t(NONE_KEY)}
                    </TableCell>
                    <TableCell sx={CELL_NOWRAP_SX}>{row.node ?? t(NONE_KEY)}</TableCell>
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                      {filterSummary(row.filter, row.excludeFilter, t(NONE_KEY))}
                    </TableCell>
                    <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
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
        // 页头那句说明 + I2 的零副作用承诺合进**同一个**问号（承诺原来占一整张卡片）
        description={mergeHelpText(
          t("network.nodes.description"),
          t("network.zeroSideEffect.title"),
          t("network.zeroSideEffect.body"),
        )}
        actions={
          // 「线路分配」那一页整页删除了→ 那个按钮一并去掉（点了会落到不存在的路由）
          <Button variant="outlined" component={Link} to="/network">
            {t("network.backToOverview")}
          </Button>
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

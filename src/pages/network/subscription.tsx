/**
 * **一条订阅的详情**（`/network/subscriptions/:id`）—— 用户第 321 轮。
 *
 * 用户原话：*"先到网络界面，再去点击一个订阅，里面就可以看到这个订阅的所有节点，
 * 可以打开或者关闭，就和服务商的线路是一样的，可以打开和关闭"*。
 *
 * # 这一页只做一件事
 *
 * 列出这份订阅的**全部节点**，每个节点一个开关。没有分组、没有筛选器、没有测速按钮 ——
 * 用户要的就是"点进来就能开关"。
 *
 * # 顶部两行是**固定的**，而且**不可关闭**
 *
 * 用户原话：*"其中自动选择和直连在最上面"* + *"直连是伪节点，但属于一直有的"*。
 * 所以它们画成两行、恒在，但**没有开关**（灰掉 + 写明为什么）。
 *
 * | 行 | 是什么 | 为什么不能关 |
 * |---|---|---|
 * | **自动选择** | 这条订阅里**所有启用节点**各自 ping 该服务商 URL，取延迟最低，然后按地址粘性 | 它是"这整份订阅"的一种用法，不是某个节点 —— 关掉它等于关掉整份订阅（那件事由「订阅启用」负责） |
 * | **直连** | 内核内置的 `DIRECT`（不经过任何代理） | 它是**伪节点**：内核自带、不由订阅提供，关掉它没有意义 |
 *
 * 两行的开关都**禁用**，并且给出**可执行的出口**：要停用某条线路请去「网络」页把那条线路关掉。
 * 只画一个灰开关而不说为什么，用户只会反复点它。
 *
 * # 开关写的是后端已有的命令
 *
 * `subscription_nodes_enabled({ id, nodes, enabled })` 是**批量**的（那轮已有的命令），
 * 一次用户动作 = 一次写库 + 一次内核重载；它返回**改完之后**的完整清单，页面直接回填缓存，
 * 所以界面与后端一个字都不会漂。
 *
 * 后端语义：库里存的是"**关掉的那些**"（`Subscription.disabled_nodes`），
 * 所以机场新增的节点默认就是开着的。
 */

import { Box, Button, Stack, Switch, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState, type ReactNode } from "react";
import { Link, useParams } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import { PageHeader, HelpTip, StatusDot } from "@/shared/components";
import { DASH, formatDuration, formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import { SectionPaper, TrafficCell, Unreadable, mergeHelpText } from "./index";
import {
  HEALTH_LABEL,
  HEALTH_STATUS,
  filterSummary,
  readSubscription,
  readTrafficMap,
  subscriptionFormatLabel,
  type LaneHealth,
} from "./model";

/** `NodeView.ok` → 全站统一的健康语义（缺失 = 未知，**不默认成功**）。 */
function nodeHealth(ok: boolean | null | undefined): LaneHealth {
  if (ok === null || ok === undefined) return "unknown";
  return ok ? "ok" : "down";
}

/** 单元格不换行（规则 4）。**这一页不裁剪**（：详情页只展示、不做省略）——
 *  长节点名靠 `white-space: nowrap` 保持一行，由外层横向滚动兜住，全文始终在版面上。 */
const CELL_NOWRAP_SX = { whiteSpace: "nowrap" } as const;

/**
 * 详情页的一行"参数"：标签 + 值（标签用订阅列表那套列名 —— 这些键本来就是"这一项叫什么"）。
 *
 * 值放在 `Box` 里而不是再套一层 `Typography`：值可能是 `TrafficCell` / `StatusDot` 这类
 * 自带行内元素的组件，套 `<p>` 会得到非法的 `<p><p>` 嵌套（同 `Unreadable` 的注释）。
 */
function ParamRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Box sx={{ display: "flex", alignItems: "baseline", gap: 2, minWidth: 0 }}>
      <Typography
        variant="caption"
        sx={{ color: "text.secondary", minWidth: 96, flex: "0 0 auto" }}
      >
        {label}
      </Typography>
      <Box sx={{ minWidth: 0, overflowWrap: "anywhere" }}>{children}</Box>
    </Box>
  );
}

/** 一格"参数"的纯文本值（缺失一律「未读取」—— 与 0 / 空串不是一回事）。 */
function ParamValue({ text }: { text: string | undefined }) {
  if (text === undefined) return <Unreadable />;
  return (
    <Typography variant="body1" component="span">
      {text}
    </Typography>
  );
}

/**
 * 顶部两行的**内置行**（不是订阅里的节点）。
 *
 * 单独建模而不是"在表格里塞两行假数据"：它们的 `name` 是**界面语言**（要翻译的），
 * 而节点名的用户数据**绝不翻译**。混在一起迟早会有人把用户节点名也丢进 `t()`。
 */
type BuiltinRow = {
  /** 稳定标识（testid 后缀 + React key）。 */
  key: string;
  /** i18n 文案键（**不是**用户数据，必须翻译）。 */
  labelKey: "network.subscription.auto" | "network.subscription.direct";
};

const BUILTIN_ROWS: readonly BuiltinRow[] = [
  { key: "auto", labelKey: "network.subscription.auto" },
  { key: "direct", labelKey: "network.subscription.direct" },
];

export default function SubscriptionDetailPage() {
  const { t } = useTranslation();
  const { id } = useParams<{ id: string }>();
  const subscriptionId = id ?? "";
  const queryClient = useQueryClient();

  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const rows = subscriptions.data?.map((item) => readSubscription(item));
  const current = rows?.find((row) => row.id === subscriptionId);

  /**
   * 这份订阅**有没有已采纳的解析结果**（硬前置）。
   *
   * `last_update_ms == null && error == null` = 还没被解析采纳过 ⇒ **这一轮根本不发**
   * `subscription_nodes`：后端在没有已采纳版本时返回的是**错误**，不是空数组，
   * 发了就会把一次必然失败画成"这份订阅没有节点"。
   */
  const unparsed =
    current !== undefined && current.lastUpdateMs === undefined && current.error === undefined;

  const nodesKey = useMemo(() => [...qk.subscriptions, "nodes", subscriptionId], [subscriptionId]);
  const nodes = useCommandQuery("subscription_nodes", { id: subscriptionId }, nodesKey, {
    enabled: subscriptionId !== "" && current !== undefined && !unparsed,
  });

  /**
   * 流量是**运行时观测值**（配置实体里没有它）：本条订阅上一次已知的用量。
   *
   * 第 326 轮起它从概览页那张表**搬到这里**（列表砍到 6 列）：key 与概览页那份完全相同
   * （`qk.subscriptions` 前缀 + `traffic`），所以两页共享同一份缓存，不会多打一次命令。
   */
  const traffic = useCommandQuery("subscription_traffic", undefined, [
    ...qk.subscriptions,
    "traffic",
  ]);
  const trafficById = traffic.data === undefined ? undefined : readTrafficMap(traffic.data);
  const trafficRow = trafficById?.get(subscriptionId);
  /** 订阅的解析格式 → 文案 key（认不出来的格式名**原样显示**，不改写）。 */
  const formatLabel =
    current?.format === undefined ? undefined : subscriptionFormatLabel(current.format);

  const setEnabled = useCommandMutation("subscription_nodes_enabled");
  const [writeError, setWriteError] = useState<unknown>(undefined);
  const busy = setEnabled.isPending;

  /**
   * 开 / 关**一个**节点。
   *
   * 单条也走批量命令（后端只有这一条）—— 一次用户动作 = 一次写库 + 一次内核重载。
   * 成功后用后端返回的清单**回填缓存**：界面与后端不会漂。
   */
  const toggle = (name: string, next: boolean): void => {
    setWriteError(undefined);
    setEnabled.mutate(
      { id: subscriptionId, nodes: [name], enabled: next },
      {
        onSuccess: (list) => queryClient.setQueryData(nodesKey, list),
        onError: (err) => setWriteError(err),
      },
    );
  };

  const refresh = useCommandMutation("subscription_refresh");

  return (
    <>
      <PageHeader
        title={t("nav.networkSubscription")}
        /*
         * 页头说明 + 三条"为什么这两行不能关"的解释合成**同一个**问号（规则 2）：
         * 那些理由原来**逐行**铺在表格单元格里（`colSpan={3}` 一整段），把节点表的第一行撑得
         * 比节点本身还显眼。正文一个字没丢，只是从"每次都要看"变成"想知道再看"。
         *
         * 为什么走 `badge` 而不是 `description`（两者渲染在**同一行、同一个位置**）：
         * `description` 只吃一个字符串，`t("network.subscription.autoWhy")` 这些键在源码里就落在
         * `<HelpTip … />` 之外 —— 极简守卫（`uiMinimalism.guard.test.ts` 规则 2）只把字面量
         * **写在 `<HelpTip>` 里**的键算成按需展开的说明，多键合并出来的长句会被它保守地误算成
         * 版面文案。这里把 `<HelpTip>` 直接写出来，守卫看到的就是事实：它们真的只在问号里。
         * 外面那层 `Box`（`flexShrink: 0`）与 `PageHeader` 包 `description` 问号用的是同一套样式
         * （：问号不能被右侧操作区挤掉），所以视觉与改动前一致。
         */
        badge={
          <Box sx={{ display: "flex", flexShrink: 0 }}>
            <HelpTip
              text={mergeHelpText(
                t("network.subscription.description"),
                t("network.subscription.autoWhy"),
                t("network.subscription.directWhy"),
                t("network.subscription.toggleHint"),
              )}
              testId="page-header-help"
            />
          </Box>
        }
        actions={
          <Button variant="outlined" component={Link} to="/network">
            {t("network.backToOverview")}
          </Button>
        }
      />

      <Box sx={{ px: 8, pb: 8 }}>
        <Stack spacing={3} data-testid="network-subscription-detail">
          {/* 是哪一份订阅：名字 + id（原样，不改写用户数据） */}
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            <Typography variant="h5" data-testid="network-subscription-detail-name">
              {current === undefined ? subscriptionId : (current.name ?? current.id)}
            </Typography>
            <Typography
              variant="caption"
              data-testid="network-subscription-detail-id"
              sx={{ fontFamily: typography.fontMono, color: "text.secondary" }}
            >
              {subscriptionId}
            </Typography>
            <Button
              variant="outlined"
              size="small"
              data-testid="network-subscription-detail-refresh"
              disabled={subscriptionId === "" || refresh.isPending}
              onClick={() => {
                setWriteError(undefined);
                refresh.mutate(
                  { id: subscriptionId, force: true },
                  {
                    onSuccess: () => {
                      // 刷新会换掉这份订阅的节点清单 → **两个查询都要失效**：
                      // 只失效 `subscriptions` 会让节点表继续显示上一版（用户刚刷新完却看不到新节点）。
                      void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
                      void queryClient.invalidateQueries({ queryKey: nodesKey });
                    },
                    // 刷新失败要**说出来**：不然按钮点下去什么都不发生，用户只会反复点
                    onError: (err) => setWriteError(err),
                  },
                );
              }}
            >
              {t("common.refresh")}
            </Button>
          </Stack>

          {/* 读不到这份订阅：如实说（可能刚被删掉），不给一个空白页 */}
          {rows !== undefined && current === undefined ? (
            <Typography
              variant="body1"
              data-testid="network-subscription-detail-notfound"
              sx={{ color: "error.main" }}
            >
              {t("network.subscription.notFound", { id: subscriptionId })}
            </Typography>
          ) : null}

          {unparsed ? (
            <Typography variant="body1" data-testid="network-subscription-detail-empty">
              {t("network.nodes.emptyUnparsed")}
            </Typography>
          ) : nodes.error === null || nodes.error === undefined ? (
            nodes.data === undefined ? (
              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                {nodes.isPending ? t("common.loading") : DASH}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                {/* 5 列（节点 / 类型 / 延迟 / 结果 / 启用）：这一页的主要动作就是"逐节点开关"，
                    所以列只留给"这个节点是什么、现在通不通"。 */}
                <Table size="small" data-testid="network-subscription-nodes">
                  <TableHead>
                    <TableRow>
                      <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.node")}</TableCell>
                      <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.kind")}</TableCell>
                      <TableCell align="right" sx={CELL_NOWRAP_SX}>
                        {t("network.nodes.samples.col.latency")}
                      </TableCell>
                      <TableCell sx={CELL_NOWRAP_SX}>{t("network.nodes.samples.col.ok")}</TableCell>
                      <TableCell sx={CELL_NOWRAP_SX}>{t("common.enabled")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {/*
                      顶部两行：**固定、恒在、不可关闭**（用户第 321 轮："自动选择和直连在最上面"、
                      "直连是伪节点，但属于一直有的"）。它们**不是**订阅里的节点，所以画在节点行之前、
                      用独立 testid，并且开关**禁用**。
                      "为什么不能关"那一整段不再铺在这一行里（它把节点表第一行撑得比节点还显眼）：
                      整段正文进了**页头那个问号**，行上只留四个字的状态。
                    */}
                    {BUILTIN_ROWS.map((row) => (
                      <TableRow
                        key={row.key}
                        data-testid={`network-subscription-builtin-${row.key}`}
                      >
                        <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                          {t(row.labelKey)}
                        </TableCell>
                        <TableCell colSpan={3} sx={{ ...CELL_NOWRAP_SX, color: "text.secondary" }}>
                          {t("network.subscription.builtinLocked")}
                        </TableCell>
                        <TableCell sx={CELL_NOWRAP_SX}>
                          <Switch
                            size="small"
                            checked
                            disabled
                            slotProps={{
                              input: { "aria-label": t(row.labelKey) },
                            }}
                          />
                        </TableCell>
                      </TableRow>
                    ))}

                    {nodes.data.length === 0 ? (
                      // 「确实没有节点」与「还没解析过」是**两档**，文案不同（不混为一谈）
                      <TableRow data-testid="network-subscription-nodes-empty">
                        <TableCell colSpan={5} sx={{ color: "text.secondary" }}>
                          {t("network.nodes.emptyGroup")}
                        </TableCell>
                      </TableRow>
                    ) : (
                      nodes.data.map((node, index) => {
                        const health = nodeHealth(node.ok);
                        return (
                          // 节点名可能重名（机场很常见）→ key 不能只用名字
                          <TableRow
                            key={`${node.name}-${index}`}
                            hover
                            data-testid="network-subscription-node-row"
                          >
                            {/* 节点名来自订阅：**原样显示**，一个字符都不改；
                                这一页不裁剪（详情页看的就是完整的名字）。 */}
                            <TableCell sx={CELL_NOWRAP_SX}>{node.name}</TableCell>
                            <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                              {node.type}
                            </TableCell>
                            <TableCell
                              align="right"
                              sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}
                            >
                              {node.latency_ms === null || node.latency_ms === undefined
                                ? DASH
                                : formatLatency(node.latency_ms)}
                            </TableCell>
                            <TableCell sx={CELL_NOWRAP_SX}>
                              <StatusDot
                                status={HEALTH_STATUS[health]}
                                label={t(HEALTH_LABEL[health])}
                              />
                            </TableCell>
                            <TableCell sx={CELL_NOWRAP_SX}>
                              {/*
                                缺字段 = 没被关掉 = 启用（与库里"存关掉的那些"同语义）。
                                这也是这一页的**主要动作**，所以它必须是个真开关、而且写下去要真的生效
                                （`subscription_nodes_enabled` → `commit_config` → `maybe_apply_kernel`）。
                              */}
                              <Switch
                                size="small"
                                data-testid="network-subscription-node-enabled"
                                checked={node.enabled !== false}
                                disabled={busy}
                                onChange={(_, next) => toggle(node.name, next)}
                                slotProps={{ input: { "aria-label": node.name } }}
                              />
                            </TableCell>
                          </TableRow>
                        );
                      })
                    )}
                  </TableBody>
                </Table>
              </Box>
            )
          ) : (
            // 失败：**后端原文**（含 TA 码），不吞、不改写成"没有节点"
            <Typography
              variant="body1"
              data-testid="network-subscription-detail-error"
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {errorMessage(nodes.error)}
            </Typography>
          )}

          {writeError === null || writeError === undefined ? null : (
            <Typography
              variant="body1"
              data-testid="network-subscription-detail-write-error"
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {errorMessage(writeError)}
            </Typography>
          )}

          {/*
            * **这份订阅的参数**（第 326 轮新增的"详情抽屉"）。
            *
            * 概览页那张表从 14 列砍到 6 列之后，格式 / 更新周期 / 保留版本 / 过滤 / 健康检查 /
            * 流量 / 组数 / 解析器 / 错误原文都搬到这里 —— 信息一个都没丢，只是从"每次都要扫一屏"
            * 变成"点进来再看"。精确的最近更新时间也在这里（列表上只有 `2 分钟前` 那种短格式）。
            *
            * 三态照旧不混：缺失显示「未读取」，与 0 / 空串不是一回事。
            */}
          <SectionPaper title={t("network.subscription.detail.title")} testId="network-subscription-detail-params">
            <Stack spacing={2}>
              <ParamRow label={t("network.subscriptions.col.format")}>
                {current === undefined || current.format === undefined ? (
                  <Unreadable />
                ) : (
                  <ParamValue
                    text={
                      // 契约外的格式名原样显示（是后端说的话，不改写）
                      formatLabel === undefined ? current.format : t(formatLabel)
                    }
                  />
                )}
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.interval")}>
                <ParamValue
                  text={current?.intervalS === undefined ? undefined : formatDuration(current.intervalS)}
                />
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.keepVersions")}>
                <ParamValue
                  text={current?.keepVersions === undefined ? undefined : String(current.keepVersions)}
                />
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.filter")}>
                <ParamValue
                  text={
                    current === undefined
                      ? undefined
                      : filterSummary(current.include, current.exclude, DASH)
                  }
                />
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.health")}>
                {current?.healthEnabled === undefined ? (
                  <Unreadable />
                ) : (
                  <ParamValue
                    text={
                      current.healthEnabled
                        ? current.healthIntervalS === undefined
                          ? t("common.enabled")
                          : formatDuration(current.healthIntervalS)
                        : t("common.disabled")
                    }
                  />
                )}
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.traffic")}>
                {/* 流量三态各自独立（缺哪样不显示哪样），全在 `TrafficCell` 里 */}
                <Box data-testid="network-subscription-traffic">
                  <TrafficCell traffic={trafficRow} />
                </Box>
              </ParamRow>
              {/* 流量读失败：**如实说**，不把失败画成「尚未获取」（列表上那条照旧） */}
              {traffic.error === null || traffic.error === undefined ? null : (
                <Typography
                  variant="body1"
                  data-testid="network-subscriptions-traffic-error"
                  sx={{ color: "error.main" }}
                >
                  {errorMessage(traffic.error)}
                </Typography>
              )}
              <ParamRow label={t("network.subscriptions.col.groups")}>
                <ParamValue
                  text={current?.groupCount === undefined ? undefined : String(current.groupCount)}
                />
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.parser")}>
                <ParamValue text={current?.parser} />
              </ParamRow>
              <ParamRow label={t("network.subscriptions.col.lastUpdate")}>
                <ParamValue
                  text={
                    current?.lastUpdateMs === undefined
                      ? undefined
                      : // 列表上只给 `2 分钟前`，精确时刻在这里（恢复路径就是这一行）
                        formatLocalTime(current.lastUpdateMs)
                  }
                />
              </ParamRow>
              {/* 解析失败的后端原文：列表上那一行只显示「解析失败」四个字，原文在此**逐字**显示 */}
              {current?.error === undefined ? null : (
                <ParamRow label={t("network.subscriptions.col.error")}>
                  <Typography
                    variant="body1"
                    component="span"
                    data-testid="network-subscription-detail-parse-error"
                    sx={{ color: "error.main" }}
                  >
                    {current.error}
                  </Typography>
                </ParamRow>
              )}
            </Stack>
          </SectionPaper>
        </Stack>
      </Box>
    </>
  );
}

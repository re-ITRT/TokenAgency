/**
 * 线路分配 `/network/assignments`（[04 §8 线路分配]、[12 §6 F4]）。
 *
 * ## 这一页做什么、**不**做什么
 *
 * [04 §8.1] 的线路继承是五级：`policy.use_lane` → `channel.lane` → `credential.lane`
 * → `provider.lane` → `net.default_lane` → 兜底 `direct`。**最终生效线路的解析归后端**
 * （`lane_assignments`）—— 这一页把它的返回原样摆出来（`LaneChainSections`）。
 *
 * 这一页自己**不**算"最终生效线路"：那是把后端的判定在前端复刻一遍，
 * 必然产生两个真相源，而且漂了不会报错。所以分工是：
 *
 * 1. `AssignmentsBody`（从 `config_get`）列出**每一级显式写了什么**（`null` = 未指定 /
 *    继承上一级；键不存在 = 未读取），以及显式引用者的倒排；
 * 2. `LaneChainSections` 显示**后端算的结论**：`lane_assignments`（最终走哪条线 + 来自哪一级）、
 *    `lane_pick_options`（候选清单）、`lane_reverse_lookup`（"删 lane 之前先看谁在引用它"）。
 *
 * 第 1 与第 2 互补：一个说"配置里写了什么"，一个说"后端据此算出了什么"。
 *
 * ## I2
 *
 * 这一页**一个写入口都没有**：调用的是只读命令（`config_get` / `lane_list` /
 * `lane_assignments` / `lane_pick_options` / `lane_reverse_lookup`，命令面守卫见
 * `src/app/network.test.tsx`），更不碰系统代理 / DNS / 路由 / 网卡。
 * 要改分配请去 `/network`（订阅与线路的写界面在那里）。
 */

import {
  Box,
  Button,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import { Link } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { ConfigDoc } from "@/shared/bindings";
import { HelpTip, PageHeader } from "@/shared/components";
import { formatLatency } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import { Caption, SectionPaper, Unreadable, ZeroSideEffectNotice, mergeHelpText } from "./index";
import {
  DASH,
  laneKindLabel,
  laneSourceLabel,
  readAssignments,
  readLane,
  type AssignmentView,
  type LaneRef,
} from "./model";

/** 一级里的 lane 单元格：显式引用 → 名字（解析不出就用 id）/ 未指定 / 未读取。三态不混。 */
function LaneCell({
  lane,
  readable,
  notSet,
}: {
  lane: LaneRef | undefined;
  readable: boolean;
  notSet: string;
}) {
  if (lane !== undefined) return <>{lane.laneName ?? lane.laneId}</>;
  if (!readable) return <Unreadable />;
  return (
    <Typography variant="body1" sx={{ color: "text.secondary" }}>
      {notSet}
    </Typography>
  );
}

/** 「读不到」与「空」是两件事：这个组件专门画前者，并**点名**缺的是哪个字段。 */
function UnreadSection({ field, testId }: { field: string; testId: string }) {
  const { t } = useTranslation();
  return (
    <Stack spacing={1}>
      <Typography variant="body1" data-testid={testId}>
        {t("network.assignments.unread.title", { field })}
      </Typography>
      <Caption>{t("network.assignments.unread.body")}</Caption>
    </Stack>
  );
}

// ================================================================ 展示层

export type AssignmentsBodyProps = {
  doc: ConfigDoc | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
};

export function AssignmentsBody({ doc, isLoading, error, onRetry }: AssignmentsBodyProps) {
  const { t } = useTranslation();
  const view: AssignmentView | undefined = doc === undefined ? undefined : readAssignments(doc);
  const notSet = t("network.assignments.notSet");

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      <ZeroSideEffectNotice />

      {/* ① 这一页的边界：为什么没有"最终生效线路" */}
      <SectionPaper
        title={t("network.assignments.noResolve.title")}
        // 五级优先顺序 + "为什么不在前端再算一遍" → 合成**一个**问号（[docs/22 §2/§3]）。
        // 结论句本身是标题（"本页不自己算「最终生效线路」"），留在版面上。
        description={mergeHelpText(
          t("network.assignments.precedence"),
          t("network.assignments.noResolve.body"),
        )}
        testId="network-assignments-no-resolve"
      />

      {error !== undefined && error !== null ? (
        <SectionPaper title={t("network.assignments.title")} testId="network-assignments-error-section">
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            <Typography variant="body1" data-testid="network-assignments-error" sx={{ color: "error.main" }}>
              {errorMessage(error)}
            </Typography>
            <Button variant="outlined" onClick={onRetry}>
              {t("common.retry")}
            </Button>
          </Stack>
        </SectionPaper>
      ) : view === undefined ? (
        <SectionPaper title={t("network.assignments.title")} testId="network-assignments-loading-section">
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {isLoading ? t("common.loading") : DASH}
          </Typography>
        </SectionPaper>
      ) : (
        <>
          {/* ② 策略级（优先级最高） */}
          <SectionPaper
            title={t("network.assignments.policies.title")}
            // 区块说明（hint）就是解释 → 收进标题右边的问号圆圈，一个区块只有一个问号（[docs/22 §3]）
            description={t("network.assignments.policies.hint")}
            testId="network-assignments-policies"
          >
            {view.policies.length === 0 ? (
              <Typography variant="body1" data-testid="network-assignments-policies-empty">
                {t("network.assignments.policies.empty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("network.assignments.policies.col.name")}</TableCell>
                      <TableCell align="right">
                        {t("network.assignments.policies.col.priority")}
                      </TableCell>
                      <TableCell>{t("network.assignments.policies.col.enabled")}</TableCell>
                      <TableCell>{t("network.assignments.policies.col.lane")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {view.policies.map((row) => (
                      <TableRow key={row.id} hover>
                        <TableCell>{row.name}</TableCell>
                        <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                          {row.priority === undefined ? DASH : row.priority}
                        </TableCell>
                        <TableCell>
                          {row.enabled ? t("common.enabled") : t("common.disabled")}
                        </TableCell>
                        <TableCell>
                          <LaneCell lane={row.lane} readable notSet={notSet} />
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            )}
          </SectionPaper>

          {/* ③ 通道级（最常用的手动指定点） */}
          <SectionPaper
            title={t("network.assignments.channels.title")}
            description={t("network.assignments.channels.hint")}
            testId="network-assignments-channels"
          >
            {!view.has.channels ? (
              <UnreadSection field="channels" testId="network-assignments-channels-unread" />
            ) : view.channels.length === 0 ? (
              <Typography variant="body1" data-testid="network-assignments-channels-empty">
                {t("network.assignments.channels.empty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("network.assignments.channels.col.model")}</TableCell>
                      <TableCell>{t("network.assignments.channels.col.provider")}</TableCell>
                      <TableCell>{t("network.assignments.channels.col.credential")}</TableCell>
                      <TableCell>{t("network.assignments.channels.col.lane")}</TableCell>
                      <TableCell>{t("network.assignments.channels.col.enabled")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {view.channels.map((row) => (
                      <TableRow key={row.id} hover>
                        <TableCell>{row.model ?? DASH}</TableCell>
                        <TableCell>{row.providerName ?? row.providerId ?? DASH}</TableCell>
                        <TableCell>{row.credentialLabel ?? row.credentialId ?? DASH}</TableCell>
                        <TableCell>
                          <LaneCell lane={row.lane} readable={row.laneReadable} notSet={notSet} />
                        </TableCell>
                        <TableCell>
                          {row.enabled ? t("common.enabled") : t("common.disabled")}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            )}
          </SectionPaper>

          {/* ④ 套餐级 */}
          <SectionPaper title={t("network.assignments.credentials.title")} testId="network-assignments-credentials">
            {!view.has.credentials ? (
              <UnreadSection field="credentials" testId="network-assignments-credentials-unread" />
            ) : view.credentials.length === 0 ? (
              <Typography variant="body1" data-testid="network-assignments-credentials-empty">
                {t("network.assignments.credentials.empty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("network.assignments.credentials.col.label")}</TableCell>
                      <TableCell>{t("network.assignments.credentials.col.provider")}</TableCell>
                      <TableCell>{t("network.assignments.credentials.col.lane")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {view.credentials.map((row) => (
                      <TableRow key={row.id} hover>
                        <TableCell>{row.label}</TableCell>
                        <TableCell>{row.providerName ?? row.providerId ?? DASH}</TableCell>
                        <TableCell>
                          <LaneCell lane={row.lane} readable={row.laneReadable} notSet={notSet} />
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            )}
          </SectionPaper>

          {/* ⑤ 服务商级 */}
          <SectionPaper
            title={t("network.assignments.providers.title")}
            description={t("network.assignments.providers.hint")}
            testId="network-assignments-providers"
          >
            {!view.has.providers ? (
              <UnreadSection field="providers" testId="network-assignments-providers-unread" />
            ) : view.providers.length === 0 ? (
              <Typography variant="body1" data-testid="network-assignments-providers-empty">
                {t("network.assignments.providers.empty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("network.assignments.providers.col.name")}</TableCell>
                      <TableCell>{t("network.assignments.providers.col.lane")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {view.providers.map((row) => (
                      <TableRow key={row.id} hover>
                        <TableCell>{row.name}</TableCell>
                        <TableCell>
                          <LaneCell lane={row.lane} readable={row.laneReadable} notSet={notSet} />
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            )}
          </SectionPaper>

          {/* ⑥ 全局默认（继承链的最后一站） */}
          <SectionPaper title={t("network.assignments.default.title")} testId="network-assignments-default">
            <Stack direction="row" spacing={3} alignItems="baseline">
              <Typography variant="caption" sx={{ fontFamily: typography.fontMono, color: "text.secondary" }}>
                {t("network.assignments.default.field")}
              </Typography>
              <Typography variant="body1">
                {!view.has.net ? (
                  <Unreadable />
                ) : view.defaultLane === undefined ? (
                  <Caption>{t("network.assignments.default.none")}</Caption>
                ) : (
                  (view.defaultLane.laneName ?? view.defaultLane.laneId)
                )}
              </Typography>
            </Stack>
          </SectionPaper>

          {/* ⑦ 倒排：谁**显式**引用了这条线（不等于"实际走了这条线"） */}
          <SectionPaper
            title={t("network.assignments.reverse.title")}
            testId="network-assignments-reverse"
            hintTestId="network-assignments-reverse-hint"
            hint={
              // 第一句留着：它是这个区块的口径说明，而且**点名了命令**
              // （测试与用户都靠这一句知道倒排是谁算的）。第二句是"为什么没人引用的线路也要列出来"
              // 的附注 → 收进句尾的问号圆圈（[docs/22 §2/§3]，这一块只有一个 -help）。
              <Caption>
                {t("network.assignments.reverse.hint")}
                <HelpTip
                  text={t("network.assignments.reverse.noneHint")}
                  testId="network-assignments-reverse-hint-help"
                />
              </Caption>
            }
          >
            {view.reverse.length === 0 ? (
              <Typography variant="body1" data-testid="network-assignments-reverse-empty">
                {t("network.assignments.reverse.empty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("network.assignments.reverse.col.lane")}</TableCell>
                      <TableCell>{t("network.assignments.reverse.col.channels")}</TableCell>
                      <TableCell>{t("network.assignments.reverse.col.credentials")}</TableCell>
                      <TableCell>{t("network.assignments.reverse.col.providers")}</TableCell>
                      <TableCell>{t("network.assignments.reverse.col.policies")}</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {view.reverse.map((row) => {
                      const none = t("network.assignments.reverse.none");
                      const list = (items: readonly string[]) =>
                        items.length === 0 ? (
                          <Typography variant="body1" sx={{ color: "text.secondary" }}>
                            {none}
                          </Typography>
                        ) : (
                          <Box component="span" sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}>
                            {items.join(" · ")}
                          </Box>
                        );
                      return (
                        <TableRow key={row.laneId} hover>
                          <TableCell>{row.laneName ?? row.laneId}</TableCell>
                          <TableCell>{list(row.channels)}</TableCell>
                          <TableCell>{list(row.credentials)}</TableCell>
                          <TableCell>{list(row.providers)}</TableCell>
                          <TableCell>{list(row.policies)}</TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </Box>
            )}
          </SectionPaper>
        </>
      )}
    </Box>
  );
}

// ================================================================ 后端算的链路（`lane_assignments` 等三条只读命令）

/**
 * 三节**后端结论**：最终生效线路、候选清单、按 lane 查引用。
 *
 * 为什么单独一个组件、由页面挂在 `AssignmentsBody` 之后：`AssignmentsBody` 是**纯 props**
 * 的展示件（测试直接喂夹具就能测"有数据 / 空 / 读不到"三态）。把三条 query 塞进去会让
 * 那些测试变成"必须先注册 mock"，反而看不清它在展示什么。**读与写都不在这里**：
 * 这里只有只读 query + 一个只读 mutation（`lane_reverse_lookup` 是查询命令，不是写）。
 */
export function LaneChainSections() {
  const { t } = useTranslation();
  const resolved = useCommandQuery("lane_assignments", undefined, qk.laneAssignments);
  const pickOptions = useCommandQuery("lane_pick_options", undefined, qk.lanePickOptions);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  // `lane_reverse_lookup(lane_id)` 是后端按 lane 现算的倒排；`useCommandMutation` 只是
  // "点了才发"的那条路径（命令本身只读，命令面守卫把它列在只读白名单里）。
  const lookup = useCommandMutation("lane_reverse_lookup");

  const laneRows = (lanes.data ?? []).map((item) => readLane(item));

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 后端算的最终生效线路（与上面那节"配置里显式写了什么"互补） */}
      <SectionPaper
        title={t("network.assignments.chain.title")}
        description={t("network.assignments.chain.hint")}
        testId="network-assignments-resolved"
      >
        {resolved.error !== undefined && resolved.error !== null ? (
          <Typography variant="body1" data-testid="network-assignments-resolved-error" sx={{ color: "error.main" }}>
            {errorMessage(resolved.error)}
          </Typography>
        ) : resolved.data === undefined ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {resolved.isPending ? t("common.loading") : DASH}
          </Typography>
        ) : resolved.data.length === 0 ? (
          <Typography variant="body1" data-testid="network-assignments-resolved-empty">
            {t("network.assignments.chain.empty")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell>{t("network.assignments.chain.col.model")}</TableCell>
                  <TableCell>{t("network.assignments.chain.col.channel")}</TableCell>
                  <TableCell>{t("network.assignments.chain.col.lane")}</TableCell>
                  <TableCell>{t("network.assignments.chain.col.source")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {resolved.data.map((row) => {
                  const sourceLabel = laneSourceLabel(row.source);
                  return (
                    <TableRow key={`${row.channel_id}:${row.lane_id}`} hover>
                      <TableCell sx={{ fontFamily: typography.fontMono }}>{row.model_id}</TableCell>
                      <TableCell sx={{ fontFamily: typography.fontMono }}>{row.channel_id}</TableCell>
                      <TableCell sx={{ fontFamily: typography.fontMono }}>{row.lane_id}</TableCell>
                      <TableCell>
                        {/* 词表外的 source 原样显示（后端说的话，不改写） */}
                        {sourceLabel === undefined ? row.source : t(sourceLabel)}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </Box>
        )}
      </SectionPaper>

      {/* ② 候选清单（`lane_pick_options`） */}
      <SectionPaper
        title={t("network.assignments.pick.title")}
        description={t("network.assignments.pick.hint")}
        testId="network-assignments-pick-options"
      >
        {pickOptions.error !== undefined && pickOptions.error !== null ? (
          <Typography variant="body1" data-testid="network-assignments-pick-error" sx={{ color: "error.main" }}>
            {errorMessage(pickOptions.error)}
          </Typography>
        ) : pickOptions.data === undefined ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {pickOptions.isPending ? t("common.loading") : DASH}
          </Typography>
        ) : pickOptions.data.length === 0 ? (
          <Typography variant="body1" data-testid="network-assignments-pick-empty">
            {t("network.assignments.pick.empty")}
          </Typography>
        ) : (
          <Stack spacing={4}>
            {pickOptions.data.map((group) => (
              <Stack key={group.group} spacing={2}>
                <Typography variant="h5">{group.group}</Typography>
                <Stack spacing={1}>
                  {group.options.map((option) => {
                    const kindLabel = laneKindLabel(option.kind);
                    return (
                      <Stack
                        key={option.lane_id}
                        direction="row"
                        spacing={3}
                        sx={{ flexWrap: "wrap", alignItems: "baseline" }}
                      >
                        <Typography variant="body1">{option.name}</Typography>
                        <Caption>{kindLabel === undefined ? option.kind : t(kindLabel)}</Caption>
                        {option.node_count === undefined ? null : (
                          <Caption>
                            {t("network.assignments.pick.nodeCount", { n: option.node_count })}
                          </Caption>
                        )}
                        {option.latency_ms === undefined || option.latency_ms === null ? null : (
                          <Caption>{formatLatency(option.latency_ms)}</Caption>
                        )}
                      </Stack>
                    );
                  })}
                </Stack>
              </Stack>
            ))}
          </Stack>
        )}
      </SectionPaper>

      {/* ③ 按 lane 查引用（`lane_reverse_lookup`）：删 lane 之前先看这一眼 */}
      <SectionPaper
        title={t("network.assignments.reverseLookup.title")}
        description={t("network.assignments.reverseLookup.hint")}
        testId="network-assignments-reverse-lookup-section"
      >
        {lanes.error !== undefined && lanes.error !== null ? (
          <Typography variant="body1" data-testid="network-assignments-reverse-lookup-lanes-error" sx={{ color: "error.main" }}>
            {errorMessage(lanes.error)}
          </Typography>
        ) : lanes.data === undefined ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {lanes.isPending ? t("common.loading") : DASH}
          </Typography>
        ) : laneRows.length === 0 ? (
          <Typography variant="body1" data-testid="network-assignments-reverse-lookup-no-lanes">
            {t("network.assignments.reverseLookup.noLanes")}
          </Typography>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell>{t("network.assignments.reverseLookup.col.lane")}</TableCell>
                  <TableCell align="right">
                    {t("network.assignments.reverseLookup.button")}
                  </TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {laneRows.map((row) => (
                  <TableRow key={row.id} hover>
                    <TableCell>{row.name === undefined ? row.id : row.name}</TableCell>
                    <TableCell align="right">
                      <Button
                        size="small"
                        variant="outlined"
                        data-testid="network-assignments-reverse-lookup"
                        disabled={lookup.isPending}
                        onClick={() => {
                          lookup.reset();
                          lookup.mutate({ lane_id: row.id });
                        }}
                      >
                        {t("network.assignments.reverseLookup.button")}
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Box>
        )}

        {lookup.error === undefined || lookup.error === null ? null : (
          <Typography
            variant="body1"
            data-testid="network-assignments-reverse-lookup-error"
            sx={{ color: "error.main", overflowWrap: "anywhere" }}
          >
            {errorMessage(lookup.error)}
          </Typography>
        )}

        {lookup.data === undefined ? null : (
          <Typography
            variant="body1"
            data-testid="network-assignments-reverse-lookup-result"
            sx={{ overflowWrap: "anywhere" }}
          >
            {`${t("network.assignments.reverseLookup.resultFor", {
              lane: lookup.variables?.lane_id ?? "",
            })}：`}
            {lookup.data.channel_ids.length === 0 && lookup.data.model_ids.length === 0
              ? t("network.assignments.reverseLookup.none")
              : `${t("network.assignments.reverseLookup.channels")} ${
                  lookup.data.channel_ids.length === 0
                    ? t("network.assignments.reverseLookup.none")
                    : lookup.data.channel_ids.join(" · ")
                } · ${t("network.assignments.reverseLookup.models")} ${
                  lookup.data.model_ids.length === 0
                    ? t("network.assignments.reverseLookup.none")
                    : lookup.data.model_ids.join(" · ")
                }`}
          </Typography>
        )}
      </SectionPaper>
    </Box>
  );
}

// ================================================================ 数据层

export default function AssignmentsPage() {
  const { t } = useTranslation();
  // `config_get` 一条命令就给出五级需要的全部字段，而且它是**原子快照**：
  // 分四条命令去取会出现"通道来自新配置、套餐来自旧配置"的撕裂读。
  const config = useCommandQuery("config_get", undefined, qk.config);

  return (
    <>
      <PageHeader
        title={t("nav.networkAssignments")}
        description={t("network.assignments.description")}
        docRef="04 §8 线路分配（五级继承 / 反向视图）"
        actions={
          <>
            <Button variant="outlined" component={Link} to="/network">
              {t("network.backToOverview")}
            </Button>
            <Button variant="outlined" component={Link} to="/network/nodes">
              {t("network.gotoNodes")}
            </Button>
          </>
        }
      />
      <AssignmentsBody
        doc={config.data}
        isLoading={config.isPending}
        error={config.error ?? undefined}
        onRetry={() => void config.refetch()}
      />
      {/* 后端算的结论（lane_assignments / lane_pick_options / lane_reverse_lookup） */}
      <LaneChainSections />
    </>
  );
}

/**
 * 线路分配 `/network/assignments`（[04 §8 线路分配]、[12 §6 F4]）。
 *
 * ## 这一页做什么、**不**做什么
 *
 * [04 §8.1] 的线路继承是五级：`policy.use_lane` → `channel.lane` → `credential.lane`
 * → `provider.lane` → `net.default_lane` → 兜底 `direct`。**最终生效线路的解析归后端**
 * （`lane_assignments` / `lane_reverse_lookup`，两者都后端已可用、本页尚未接入）。
 *
 * 所以这一页**不自己算一个"最终生效线路"**：那是把后端的判定在前端复刻一遍，
 * 必然产生两个真相源，而且漂了不会报错。它只做两件**不需要解析**的事：
 *
 * 1. 把**每一级显式写了什么**列出来（`null` = 未指定 / 继承上一级；键不存在 = 未读取）；
 * 2. 收集**谁显式引用了这条线**的倒排（[04 §8.4] 反向视图的显式部分）。
 *
 * 第 2 条刻意与"实际走了这条线"划清界限：继承下来的使用者不在倒排里，
 * 需要后端的 `lane_reverse_lookup` 才算得准 —— 页面上把这句话原样写出来。
 *
 * ## I2
 *
 * 这一页显示的每一个值都是"改分配"的入口所在处，所以更要紧的是**一个写入口都没有**：
 * 调用的是 `config_get` 一条只读命令（命令面守卫见 `src/app/network.test.tsx`），
 * 更不碰系统代理 / DNS / 路由 / 网卡。
 */

import { Box, Button, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { Link } from "react-router-dom";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import type { ConfigDoc } from "@/shared/bindings";
import { PageHeader } from "@/shared/components";
import { useTranslation } from "@/shared/i18n";
import { typography } from "@/theme";

import { Caption, SectionPaper, Unreadable, ZeroSideEffectNotice } from "./index";
import {
  DASH,
  readAssignments,
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
        testId="network-assignments-no-resolve"
      >
        <Stack spacing={1}>
          <Caption>{t("network.assignments.precedence")}</Caption>
          <Caption>{t("network.assignments.noResolve.body")}</Caption>
        </Stack>
      </SectionPaper>

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
            hintTestId="network-assignments-policies-hint"
            hint={<Caption>{t("network.assignments.policies.hint")}</Caption>}
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
            hintTestId="network-assignments-channels-hint"
            hint={<Caption>{t("network.assignments.channels.hint")}</Caption>}
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
            hintTestId="network-assignments-providers-hint"
            hint={<Caption>{t("network.assignments.providers.hint")}</Caption>}
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
              <Stack spacing={1}>
                <Caption>{t("network.assignments.reverse.hint")}</Caption>
                <Caption>{t("network.assignments.reverse.noneHint")}</Caption>
              </Stack>
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
    </>
  );
}

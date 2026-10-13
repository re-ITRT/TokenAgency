/**
 * 概览 / 看板（[07 §1]）—— **它是"现在能不能用"的观测面**，也是 M1 验收（A1/B2）唯一能用眼睛看的地方。
 *
 * 三条诚实纪律（这个页面最容易违反，所以写在最上面）：
 * 1. **契约里没有的判据一律显示「未知」**：`KernelState` 只说明内核进程状态，
 *    **不说明 provider 是否真的加载成功**（[06 §2.1] B2 的核心）。所以这一维显示「未知」+ 一句说明，
 *    **绝不**因为 `state === "ready"` 就点绿灯 —— 那正是"拿不确定冒充确定"。
 * 2. **没有数据就给诚实的空状态/骨架**，不用 0 或别的数字填坑；错误必须带 `TA-xxxx` 出处与重试入口。
 * 3. 排版/颜色/间距全部走 [15] 令牌（守卫会机械检查）。
 */

import { Box, Button, Paper, Stack, Typography } from "@mui/material";
import { useTranslation } from "@/shared/i18n";
import { useMemo, useState, type ReactNode } from "react";
import {
  errorCode,
  errorMessage,
  qk,
  useAppStatus,
  useCommandQuery,
} from "@/shared/api";
import type { AppStatus, CommandArgs, DashboardData, TimeRange } from "@/shared/bindings";
import { formatMoney, formatPercent } from "@/shared/format";
import {
  HelpTip,
  KernelStateDot,
  NumberCard,
  PageHeader,
  StatusDot,
} from "@/shared/components";
import { radius } from "@/theme";

/** 数字卡的比率（`(now - prev) / prev`）；`prev` 缺失或为 0 时不给涨跌（不给假趋势）。 */
function ratio(now: number, prev: number | undefined): number | undefined {
  if (prev === undefined || prev === 0) return undefined;
  return (now - prev) / prev;
}

/**
 * 看板用的时间范围。目前固定"今天"，但形状必须是 `TimeRange` 的线上值
 * （**裸字符串**）—— 曾经写成 `{preset:"today"}`，真机上 `usage_dashboard`
 * 会被 serde 判成未知变体直接失败（见 `src/app/argsShape.guard.test.ts`）。
 */
export const DASHBOARD_RANGE = "today";

/** `usage_dashboard` 的入参（抽成纯函数，好让形状守卫直接调用真货而不是复述）。 */
export function dashboardRangeArgs(
  range: TimeRange = DASHBOARD_RANGE,
): CommandArgs<"usage_dashboard"> {
  return { range };
}

/** 「可用性」一行：只陈述事实，事实拿不到就写"未知"。 */
function ReadinessRow({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
      <Typography
        variant="caption"
        sx={{ color: "text.secondary", minWidth: 96, flex: "0 0 auto" }}
      >
        {label}
      </Typography>
      <Box sx={{ display: "inline-flex", alignItems: "center", gap: 2, minWidth: 0 }}>
        {children}
      </Box>
    </Box>
  );
}

export type DashboardBodyProps = {
  /** `app_status`（网关/内核事实）。`undefined` = 还拿不到 → 显示"未知"。 */
  status: AppStatus | undefined;
  /** `usage_dashboard` 的结果。`undefined` 且不在加载中 = 没有数据 → 空状态。 */
  data: DashboardData | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
};

export function DashboardBody({ status, data, isLoading, error, onRetry }: DashboardBodyProps) {
  const { t } = useTranslation();

  const gatewayRunning = status?.gateway === "running";
  const code = errorCode(error);

  // 注意：出口一行必须能在**没有线路**时渲染。Rust 的 `StatusBar.lane` / `.policy` 是
  // `Option` + `skip_serializing_if = "Option::is_none"` —— 没有就**整个字段不出现**；
  // 而这里曾直接读 `data.status_bar.lane.name`，于是"策略/线路为空"的配置（新装就是）
  // 会让首页抛 `Cannot read properties of undefined (reading 'name')`，
  // 被 ErrorBoundary 接住、整页显示"出错了"（真机 release 包上实测抓到过）。
  // 同一应用里的 StatusBar 组件早就写对了（`?? t("status.policy.none")`），首页漏了。
  const lane = data?.status_bar.lane ?? null;
  const laneLine =
    lane === null
      ? t("status.node.none")
      : [
          lane.name,
          lane.node !== undefined && lane.node !== null
            ? t("status.exit.node", { node: lane.node })
            : null,
          lane.latency_ms !== undefined && lane.latency_ms !== null
            ? `${lane.latency_ms}ms`
            : null,
        ]
          .filter((x) => x !== null)
          .join(" · ");

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 可用性：现在能不能用（[07 §1.2] ①）。缺数据一律"未知"。 */}
      <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }}>
        <Stack spacing={3}>
          <Typography variant="h4">{t("dashboard.readiness")}</Typography>

          <ReadinessRow label={t("dashboard.gateway")}>
            {status === undefined ? (
              <StatusDot status="off" label={t("common.unknown")} />
            ) : (
              <StatusDot
                status={gatewayRunning ? "ok" : "warn"}
                label={gatewayRunning ? t("status.gateway.running") : t("status.gateway.paused")}
              />
            )}
          </ReadinessRow>

          <ReadinessRow label={t("status.kernel.label")}>
            <KernelStateDot state={status?.kernel.state} />
            {status?.kernel.restarts !== undefined && status.kernel.restarts > 0 ? (
              <Typography variant="caption" sx={{ color: "text.secondary" }}>
                {t("dashboard.kernel.restarts", { count: status.kernel.restarts })}
              </Typography>
            ) : null}
          </ReadinessRow>

          {/* 关键：契约里没有"provider 是否加载成功"的判据 → 如实显示未知，不点绿灯。
              为什么是未知这段解释收进问号圆圈（[docs/22 §2]：这是怎么理解这一维的说明）——
              它本来就在 tooltip 里，收进统一件之后多了个看得见的问号与 `aria-label`（读屏也拿得到）。 */}
          <ReadinessRow label={t("dashboard.providerCheck")}>
            <StatusDot status="off" label={t("common.unknown")} />
            <HelpTip text={t("dashboard.providerCheck.hint")} testId="dashboard-provider-check-help" />
          </ReadinessRow>

          <ReadinessRow label={t("status.exit")}>
            {data === undefined ? (
              <StatusDot status="off" label={t("common.unknown")} />
            ) : (
              <Typography variant="body1" sx={{ color: "text.primary" }}>
                {laneLine}
              </Typography>
            )}
          </ReadinessRow>

          <ReadinessRow label={t("status.policy")}>
            {data === undefined ? (
              <StatusDot status="off" label={t("common.unknown")} />
            ) : (
              <Typography variant="body1" sx={{ color: "text.primary" }}>
                {data.status_bar.policy?.name ?? t("status.policy.none")}
              </Typography>
            )}
          </ReadinessRow>

          {status !== undefined ? (
            <ReadinessRow label={t("dashboard.uptime")}>
              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                {t("app.uptime", { duration: `${status.uptime_s}s` })}
              </Typography>
            </ReadinessRow>
          ) : null}

          {error !== undefined ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {/* `errorMessage()` 本身就带 `[TA-xxxx]`（唯一格式化出口），这里不再重复拼一次码 */}
              <Typography variant="body1" data-testid="dashboard-error" sx={{ color: "error.main" }}>
                {code === undefined ? `${t("error.title")} · ${errorMessage(error)}` : errorMessage(error)}
              </Typography>
              <Button variant="outlined" onClick={onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : null}
        </Stack>
      </Paper>

      {/* ② 四张数字卡（[07 §1.2]） */}
      {isLoading ? (
        <Box sx={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: 3 }}>
          {[0, 1, 2, 3].map((i) => (
            <NumberCard key={i} label="" value={0} loading />
          ))}
        </Box>
      ) : data === undefined ? (
        <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }}>
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {error === undefined ? t("common.empty") : t("error.title")}
          </Typography>
        </Paper>
      ) : (
        <Box sx={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: 3 }}>
          <NumberCard
            label={t("dashboard.card.requests")}
            value={data.cards.requests.value}
            format="int"
            exact
            sparkline={data.sparklines.requests}
            delta={(() => {
              const r = ratio(data.cards.requests.value, data.cards.requests.prev);
              return r === undefined ? undefined : { value: r };
            })()}
          />
          <NumberCard
            label={t("dashboard.card.tokens")}
            value={data.cards.tokens.value}
            format="tokens"
            exact
            unit={t("dashboard.card.tokensUnit")}
            sparkline={data.sparklines.tokens}
            delta={(() => {
              const r = ratio(data.cards.tokens.value, data.cards.tokens.prev);
              return r === undefined ? undefined : { value: r };
            })()}
          />
          <NumberCard
            label={t("dashboard.card.cost")}
            value={data.cards.cost.amount}
            format="money"
            currency={data.cards.cost.currency}
            exact
            sparkline={data.sparklines.cost}
            // 花费涨了不是好事：颜色语义要反过来（[15 §3.2] 语义色只表达状态）
            delta={(() => {
              const r = ratio(data.cards.cost.amount, data.cards.cost.prev);
              return r === undefined ? undefined : { value: r, goodWhenPositive: false };
            })()}
          />
          <NumberCard
            label={t("dashboard.card.successRate")}
            // [15 §4.3]：百分比固定一位小数（99.2%，不是 99% 也不是 99.2000001%）
            value={formatPercent(data.cards.success_rate, 1)}
            sparkline={data.sparklines.success_rate}
            exact={formatPercent(data.cards.success_rate, 1)}
          />
        </Box>
      )}

      {/* ③ 未定价模型：成本数字不完整必须说出来（[07 §1.3] 诚实） */}
      {data !== undefined && data.unpriced_models.length > 0 ? (
        <Stack direction="row" spacing={2} alignItems="center">
          <StatusDot status="warn" />
          <Typography variant="body1">
            {t("dashboard.unpriced", { count: data.unpriced_models.length })}
          </Typography>
        </Stack>
      ) : null}

      {data !== undefined && data.cache.hit_rate > 0 ? (
        <Typography variant="caption" sx={{ color: "text.secondary" }}>
          {t("dashboard.cache", {
            rate: formatPercent(data.cache.hit_rate, 1),
            saved: formatMoney({ amount: data.cache.saved_amount, currency: data.cache.currency }),
          })}
        </Typography>
      ) : null}
    </Box>
  );
}

export default function DashboardPage() {
  const { t } = useTranslation();
  // 显式 `"today"`（不给 `useState` 推断的余地）：`useState("today")` 会被推断成 `string`，
  // 那样 `range` 就不再是 `TimeRange` 了 —— 形状守卫要的正是"类型上就发不出错的形状"。
  const [range] = useState<"today">(DASHBOARD_RANGE);
  const status = useAppStatus({ refetchInterval: 5_000 });
  // 入参引用要稳定，否则每次渲染都换新对象 → react-query 反复重查
  const args = useMemo(() => dashboardRangeArgs(range), [range]);
  const dashboard = useCommandQuery("usage_dashboard", args, qk.dashboard(range));

  return (
    <>
      <PageHeader title={t("nav.dashboard")} />
      <DashboardBody
        status={status.data}
        data={dashboard.data}
        isLoading={dashboard.isPending}
        error={dashboard.error ?? undefined}
        onRetry={() => void dashboard.refetch()}
      />
    </>
  );
}

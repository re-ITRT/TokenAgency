/**
 * 顶部状态条 —— 版面里的 ①（[07 §1.2]：「现在能不能用？」永远排最上，且不能要滚动才看到）。
 *
 * 它是**外壳级**的（所有页面共享），数据来源：
 * - `app_status`（轮询 + 事件失效）→ 网关/内核/当前策略/默认出口；
 * - `policy_list` / `lane_list`（缓存）→ 把 id 翻成用户看得懂的名字；
 * - `config_get` → 判断是否处于实验模式（TUN / 系统代理），给状态条加角标。
 *
 * 视觉（[15]）：状态一律「6px 圆点 + 文字」（§5），**不用彩色 chip**（§2.1 也不许 emoji）；
 * 数字用 `tabular-nums`（§4.3）；层级靠字号与深浅，不用 HTML 粗体标签（浏览器默认 700 字重，§4.2 禁止）。
 *
 * I2 边界：这里对 TUN / 系统代理**只做展示与说明**，没有任何"顺手改系统设置"的入口。
 */

import { Box, Button, Divider, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopyOutlined";
import PauseCircleOutlineIcon from "@mui/icons-material/PauseCircleOutlineOutlined";
import PlayCircleOutlineIcon from "@mui/icons-material/PlayCircleOutlineOutlined";
import RefreshIcon from "@mui/icons-material/RefreshOutlined";
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

import {
  api,
  ALL_RESOURCE_PREFIXES,
  errorMessage,
  qk,
  useAppStatus,
  useCommandMutation,
  useCommandQuery,
} from "@/shared/api";
import { formatDuration, formatLatency } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { useAppStore } from "@/stores";
import { typography } from "@/theme";

import { KernelStateDot, StatusDot } from "@/shared/components";

/** 与侧边栏品牌区同高。 */
const CHROME_HEIGHT = 48;

export function StatusBar() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const { data: status } = useAppStatus({ refetchInterval: 5_000 });
  const kernel = useAppStore((s) => s.kernel);
  const pushToast = useAppStore((s) => s.pushToast);
  const setExperimental = useAppStore((s) => s.setExperimental);
  const experimental = useAppStore((s) => s.experimental);

  const policies = useCommandQuery("policy_list", undefined, qk.policies);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  const config = useCommandQuery("config_get", undefined, qk.config);

  const pause = useCommandMutation("app_pause_gateway", {
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.appStatus }),
  });
  const resume = useCommandMutation("app_resume_gateway", {
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.appStatus }),
  });

  // 实验模式角标：用户必须知道自己处于"会改系统网络"的模式（[09 §1.2]）
  useEffect(() => {
    if (config.data === undefined) return;
    setExperimental(config.data.net.tun || config.data.net.system_proxy);
  }, [config.data, setExperimental]);

  const gatewayRunning = (status ?? undefined)?.gateway !== "paused";
  // 状态源顺序：实时事件（store）→ app_status → **未知**（不默认成 "stopped"，那是编事实）
  const kernelState = kernel?.state ?? status?.kernel.state;
  const policyName =
    policies.data?.find((p) => p.id === status?.active_policy_id)?.name ?? t("status.policy.none");
  const laneId = status?.default_lane_id ?? null;
  const laneName =
    lanes.data?.find((l) => l.lane.id === laneId)?.lane.name ?? t("status.node.none");
  const latency = lanes.data?.find((l) => l.lane.id === laneId)?.latency_ms ?? null;

  const busy = pause.isPending || resume.isPending;

  return (
    <Box
      component="header"
      sx={{
        px: 3,
        py: 1,
        borderBottom: `1px solid`,
        borderColor: "divider",
        display: "flex",
        alignItems: "center",
        gap: 3,
        minHeight: CHROME_HEIGHT,
        flex: "0 0 auto",
      }}
    >
      <StatusDot
        status={gatewayRunning ? "ok" : "warn"}
        label={gatewayRunning ? t("status.gateway.running") : t("status.gateway.paused")}
      />

      <KernelStateDot state={kernelState} withLabelPrefix />

      {experimental ? (
        // 只展示与解释，不提供开关（I2）
        <Tooltip title={t("app.badge.experimental.hint")}>
          <Box component="span" sx={{ display: "inline-flex" }}>
            <StatusDot status="warn" label={t("app.badge.experimental")} />
          </Box>
        </Tooltip>
      ) : null}

      <Divider orientation="vertical" flexItem />

      <Typography variant="caption" noWrap sx={{ color: "text.secondary" }}>
        {t("status.policy")}
        {" · "}
        <Box component="span" sx={{ color: "text.primary" }}>
          {policyName}
        </Box>
      </Typography>

      <Typography variant="caption" noWrap sx={{ color: "text.secondary", minWidth: 0 }}>
        {t("status.exit")}
        {" · "}
        <Box component="span" sx={{ color: "text.primary" }}>
          {laneName}
        </Box>
        {status?.current_node !== null && status?.current_node !== undefined
          ? ` · ${t("status.exit.node", { node: status.current_node })}`
          : ""}
      </Typography>

      {latency !== null ? (
        <Typography
          variant="caption"
          sx={{ fontVariantNumeric: "tabular-nums", color: "text.secondary" }}
        >
          {t("status.latency", { latency: formatLatency(latency) })}
        </Typography>
      ) : null}

      <Box sx={{ flex: 1 }} />

      {status !== undefined ? (
        <Typography
          variant="caption"
          noWrap
          sx={{ color: "text.secondary", fontVariantNumeric: "tabular-nums" }}
        >
          {t("app.uptime", { duration: formatDuration(status.uptime_s) })}
        </Typography>
      ) : null}

      <Tooltip title={gatewayRunning ? t("status.gateway.pause") : t("status.gateway.resume")}>
        <span>
          <IconButton
            disabled={busy}
            onClick={() => {
              if (gatewayRunning) pause.mutate(undefined);
              else resume.mutate(undefined);
            }}
            sx={{ "& svg": { fontSize: 16 } }}
          >
            {gatewayRunning ? <PauseCircleOutlineIcon /> : <PlayCircleOutlineIcon />}
          </IconButton>
        </span>
      </Tooltip>

      <Tooltip title={t("status.clipboard")}>
        <IconButton
          sx={{ "& svg": { fontSize: 16 } }}
          onClick={() => {
            void api.app
              .clipboardAccessInfo()
              .then((text) => navigator.clipboard?.writeText(text))
              .then(() =>
                pushToast({
                  level: "info",
                  title: t("status.clipboard.done"),
                  body: "",
                  dedupeKey: "clipboard",
                }),
              )
              .catch((e: unknown) =>
                pushToast({
                  level: "error",
                  title: t("error.title"),
                  body: errorMessage(e),
                  dedupeKey: "clipboard-err",
                }),
              );
          }}
        >
          <ContentCopyIcon />
        </IconButton>
      </Tooltip>

      <Tooltip title={t("common.refresh")}>
        <Button
          variant="text"
          startIcon={<RefreshIcon sx={{ fontSize: 14 }} />}
          sx={{ fontSize: typography.size.meta, color: "text.secondary" }}
          onClick={() => {
            for (const key of ALL_RESOURCE_PREFIXES) {
              void queryClient.invalidateQueries({ queryKey: key });
            }
          }}
        >
          {t("common.refresh")}
        </Button>
      </Tooltip>
    </Box>
  );
}

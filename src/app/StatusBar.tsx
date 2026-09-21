/**
 * 顶部状态条 —— 版面里的 ①（[07 §1.2]：「现在能不能用？」永远排最上，且不能要滚动才看到）。
 *
 * 它是**外壳级**的（所有页面共享），数据来源：
 * - `app_status`（轮询 + 事件失效）→ 网关/内核/当前策略/默认出口；
 * - `policy_list` / `lane_list`（缓存）→ 把 id 翻成用户看得懂的名字；
 * - `config_get` → 判断是否处于实验模式（TUN / 系统代理），给状态条加角标。
 */

import {
  Box,
  Button,
  Chip,
  Divider,
  IconButton,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
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
import type { KernelState } from "@/shared/bindings";
import { formatDuration, formatLatency } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { useAppStore } from "@/stores";

import { StatusDot, type SemStatus } from "@/shared/components";

/** 内核状态 → 语义色（与 StatusDot 的全局语义一致）。 */
const KERNEL_STATUS: Record<KernelState, SemStatus> = {
  stopped: "off",
  starting: "info",
  ready: "ok",
  restarting: "warn",
  failed: "error",
};

const KERNEL_LABEL_KEY = {
  stopped: "status.kernel.stopped",
  starting: "status.kernel.starting",
  ready: "status.kernel.ready",
  restarting: "status.kernel.restarting",
  failed: "status.kernel.failed",
} as const;

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
  const kernelState = kernel?.state ?? status?.kernel.state ?? "stopped";
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
        px: 1.5,
        py: 0.75,
        borderBottom: 1,
        borderColor: "divider",
        display: "flex",
        alignItems: "center",
        gap: 1.5,
        minHeight: 44,
        flex: "0 0 auto",
      }}
    >
      <StatusDot
        status={gatewayRunning ? "ok" : "warn"}
        label={gatewayRunning ? t("status.gateway.running") : t("status.gateway.paused")}
      />

      <Chip
        size="small"
        variant="outlined"
        color={KERNEL_STATUS[kernelState] === "ok" ? "success" : "default"}
        label={`${t("status.kernel.label")}: ${t(KERNEL_LABEL_KEY[kernelState])}`}
      />

      {experimental ? (
        <Tooltip title="TUN / 系统代理已开启：会修改系统网络状态（实验模式）">
          <Chip size="small" color="warning" label={t("app.badge.experimental")} />
        </Tooltip>
      ) : null}

      <Divider orientation="vertical" flexItem />

      <Typography variant="caption" color="text.secondary" noWrap>
        {t("status.policy")}: <strong>{policyName}</strong>
      </Typography>

      <Stack direction="row" spacing={0.5} alignItems="center" sx={{ minWidth: 0 }}>
        <Typography variant="caption" color="text.secondary" noWrap>
          {t("status.exit")}: <strong>{laneName}</strong>
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
      </Stack>

      <Box sx={{ flex: 1 }} />

      {status !== undefined ? (
        <Typography variant="caption" color="text.secondary" noWrap>
          {t("app.uptime", { duration: formatDuration(status.uptime_s) })}
        </Typography>
      ) : null}

      <Tooltip title={gatewayRunning ? t("status.gateway.pause") : t("status.gateway.resume")}>
        <span>
          <IconButton
            size="small"
            disabled={busy}
            onClick={() => {
              if (gatewayRunning) pause.mutate(undefined);
              else resume.mutate(undefined);
            }}
          >
            {gatewayRunning ? (
              <PauseCircleOutlineIcon fontSize="small" />
            ) : (
              <PlayCircleOutlineIcon fontSize="small" />
            )}
          </IconButton>
        </span>
      </Tooltip>

      <Tooltip title={t("status.clipboard")}>
        <IconButton
          size="small"
          onClick={() => {
            void api.app
              .clipboardAccessInfo()
              .then((text) => navigator.clipboard?.writeText(text))
              .then(() => pushToast({ level: "info", title: t("status.clipboard.done"), body: "", dedupeKey: "clipboard" }))
              .catch((e: unknown) => pushToast({ level: "error", title: t("error.title"), body: errorMessage(e), dedupeKey: "clipboard-err" }));
          }}
        >
          <ContentCopyIcon fontSize="small" />
        </IconButton>
      </Tooltip>

      <Tooltip title={t("common.refresh")}>
        <Button
          size="small"
          variant="text"
          startIcon={<RefreshIcon fontSize="small" />}
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

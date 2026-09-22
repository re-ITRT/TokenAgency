/**
 * 服务商列表 `/providers`（[07 §3.1]）—— M1 里"配了哪几家、还活着吗"的入口。
 *
 * 三条硬纪律（写在最上面，这一页最容易违反）：
 *
 * 1. **只调用已注册的命令**：`provider_list` / `credential_list`。
 *    `provider_test` / `provider_upsert` / `provider_delete` **尚未注册**（见
 *    `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 与 `commandSurface.guard.test.ts`），
 *    所以这里**没有测试/编辑/删除按钮，也没有禁用占位**。
 * 2. **没有读取接口的就不猜**：状态灯、成功率、延迟、通道数都**只**取后端返回的那一列；
 *    后端没给（Rust 的 `provider_list` 返回的是完整 `Provider`，没有这几列）就显示「未知/—」。
 *    内核在跑**不等于**服务商可用（[06 §2.1] B2），所以绝不点一个假绿灯。
 * 3. **凭据数读不到就是未知**：`credential_list` 失败只让"凭据数"这一列变未知 + 一行错误，
 *    **不把整张表打成错误**，也绝不填 0。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("provider_list", undefined, qk.providers)`   → 目录（详情页共用同一个 key）
 * - `useCommandQuery("credential_list", undefined, qk.credentials)`→ 凭据（用来数"挂载了几个凭据"）
 *
 * 契约与 Rust 的字段偏差、以及"为什么详情页也从 `provider_list` 取行"见 `model.ts` 的文件头。
 */

import { Box, Button, Link, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { useMemo, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader, StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatPercent } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import { mountOf, readCredential, readProvider, type CredentialRow, type ProviderRow } from "./model";

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 协议 → 界面语言。**不把 `openai_responses` 这类线上值漏给用户。** */
const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

/** 后端 `status` 的取值 → 全站统一的状态语义（[15 §3.2]）。契约外的取值一律按"未知"画。 */
const STATUS_SEM: Record<string, SemStatus> = { ok: "ok", warn: "warn", error: "error", off: "off" };
const STATUS_LABEL: Record<string, I18nKey> = {
  ok: "component.statusDot.ok",
  warn: "component.statusDot.warn",
  error: "component.statusDot.error",
  off: "component.statusDot.off",
};

/** 数值缺失一律画「—」（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

// ================================================================ 小组件

/** 弱化的一行说明（诚实性说明专用）。 */
function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据"）

export type ProvidersBodyProps = {
  rows: ProviderRow[];
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /**
   * 凭据列表。**`undefined` = 还没读回来或读失败** → 凭据数显示"未知"（不是 0）。
   * 空数组才是"确实一个凭据都没有"。
   */
  credentials: CredentialRow[] | undefined;
  credentialsError: unknown;
  onRetryCredentials: () => void;
  /** 打开某一个服务商（页面注入路由跳转，展示层不认识 router）。 */
  onOpen: (id: string) => void;
};

export function ProvidersBody({
  rows,
  isLoading,
  error,
  onRetry,
  credentials,
  credentialsError,
  onRetryCredentials,
  onOpen,
}: ProvidersBodyProps) {
  const { t } = useTranslation();

  /** 挂载文案：**两半各自独立**，哪半读不到就说哪半未知（不写 `0 / —` 这种含糊写法）。 */
  const mountText = (row: ProviderRow): string => {
    const mount = mountOf(row, credentials);
    if (mount.credentials !== undefined && mount.channels !== undefined) {
      return t("providers.mount.both", {
        credentials: formatInt(mount.credentials),
        channels: formatInt(mount.channels),
      });
    }
    if (mount.credentials !== undefined) {
      return t("providers.mount.credentialsOnly", { credentials: formatInt(mount.credentials) });
    }
    if (mount.channels !== undefined) {
      return t("providers.mount.channelsOnly", { channels: formatInt(mount.channels) });
    }
    return t("providers.mount.unknown");
  };

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 3, flexWrap: "wrap" }}>
            <Typography variant="h4">{t("providers.tableTitle")}</Typography>
            {!isLoading && error === undefined ? (
              <Caption>{t("providers.count", { value: formatInt(rows.length) })}</Caption>
            ) : null}
          </Box>

          {/* 凭据读失败只降级"凭据数"这一列：说清原因（带 TA 码）+ 单独重试，不毁整张表 */}
          {credentialsError !== undefined && credentialsError !== null ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography
                variant="caption"
                data-testid="providers-credentials-error"
                sx={{ color: "error.main" }}
              >
                {t("providers.credentialsError", { message: errorMessage(credentialsError) })}
              </Typography>
              <Button variant="outlined" onClick={onRetryCredentials}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : null}

          {error !== undefined && error !== null ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {/* `errorMessage()` 自带 `[TA-xxxx]`（唯一格式化出口），不吞错误码 */}
              <Typography variant="body1" data-testid="providers-error" sx={{ color: "error.main" }}>
                {t("providers.error", { message: errorMessage(error) })}
              </Typography>
              <Button variant="outlined" onClick={onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : isLoading ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("common.loading")}
            </Typography>
          ) : rows.length === 0 ? (
            /* 空状态：说清"配置里就是没有"，不画一张 0 行的表、不给假数据 */
            <Stack spacing={1} data-testid="providers-empty">
              <Typography variant="body1">{t("providers.empty.title")}</Typography>
              <Caption>{t("providers.empty.body")}</Caption>
            </Stack>
          ) : (
            <Box sx={{ overflowX: "auto" }}>
              <Table size="small" data-testid="providers-table">
                <TableHead>
                  <TableRow>
                    <TableCell>{t("providers.col.name")}</TableCell>
                    <TableCell>{t("providers.col.protocol")}</TableCell>
                    <TableCell>{t("providers.col.status")}</TableCell>
                    <TableCell align="right">{t("providers.col.successRate")}</TableCell>
                    <TableCell align="right">{t("providers.col.latency")}</TableCell>
                    <TableCell>{t("providers.col.mount")}</TableCell>
                    <TableCell>{t("providers.col.enabled")}</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {rows.map((row) => {
                    const status = row.status;
                    const sem = status === undefined ? "off" : (STATUS_SEM[status] ?? "off");
                    const statusLabel =
                      status === undefined
                        ? t("common.unknown")
                        : STATUS_LABEL[status] !== undefined
                          ? t(STATUS_LABEL[status]!)
                          : // 契约外的取值原样显示（是后端说的话，不改写、不猜）
                            status;
                    const protocol = row.protocol;

                    return (
                      <TableRow key={row.id} hover data-testid="providers-row">
                        <TableCell>
                          {/* 名称是进入详情的入口。**它只是个跳转**，不是数据写入 */}
                          <Link
                            component="button"
                            type="button"
                            onClick={() => onOpen(row.id)}
                            sx={{ fontSize: typography.size.body, textAlign: "left" }}
                          >
                            {row.name === "" ? row.id : row.name}
                          </Link>
                        </TableCell>
                        <TableCell sx={{ color: "text.secondary" }}>
                          {protocol === undefined
                            ? DASH
                            : PROTOCOL_LABEL[protocol] !== undefined
                              ? t(PROTOCOL_LABEL[protocol]!)
                              : protocol}
                        </TableCell>
                        <TableCell>
                          <StatusDot status={sem} label={statusLabel} />
                        </TableCell>
                        <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                          {row.successRate24h === undefined
                            ? DASH
                            : formatPercent(row.successRate24h, 1)}
                        </TableCell>
                        <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                          {row.avgLatencyMs === undefined ? DASH : formatLatency(row.avgLatencyMs)}
                        </TableCell>
                        <TableCell sx={{ color: "text.secondary" }}>{mountText(row)}</TableCell>
                        <TableCell sx={{ color: "text.secondary" }}>
                          {row.enabled === undefined
                            ? t("common.unknown")
                            : row.enabled
                              ? t("common.enabled")
                              : t("common.disabled")}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </Box>
          )}

          {/* 诚实说明：哪些列是后端真给的、哪些只是这一版没有 */ }
          <Stack spacing={1}>
            <Caption>{t("providers.honesty.status")}</Caption>
            <Caption>{t("providers.honesty.fields")}</Caption>
            <Caption>{t("providers.honesty.mounted")}</Caption>
            <Caption>{t("providers.honesty.click")}</Caption>
          </Stack>
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function ProvidersPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  // 详情页读同一个 key：一次取数、两页共用缓存（事件到达时由事件桥失效）
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);

  const rows = useMemo(() => (providers.data ?? []).map((p) => readProvider(p)), [providers.data]);
  /** `undefined`（读不到）与 `[]`（确实是空）在这里被严格区分开。 */
  const credentialRows = useMemo(
    () => (credentials.data === undefined ? undefined : credentials.data.map((c) => readCredential(c))),
    [credentials.data],
  );

  const refresh = () => {
    void providers.refetch();
    void credentials.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.providers")}
        description={t("providers.description")}
        docRef="07 §3.1 服务商列表"
        actions={
          <Button variant="outlined" onClick={refresh}>
            {t("providers.refresh")}
          </Button>
        }
      />

      <ProvidersBody
        rows={rows}
        isLoading={providers.isPending}
        error={providers.error ?? undefined}
        onRetry={refresh}
        credentials={credentialRows}
        credentialsError={credentials.error ?? undefined}
        onRetryCredentials={() => void credentials.refetch()}
        onOpen={(id) => navigate(`/providers/${encodeURIComponent(id)}`)}
      />
    </>
  );
}

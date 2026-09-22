/**
 * 服务商列表 `/providers`（[07 §3.1]）—— M1 里"配了哪几家、还活着吗"的入口。
 *
 * 三条硬纪律（写在最上面，这一页最容易违反）：
 *
 * 1. **只调用已注册的命令**：本页取数只用 `provider_list` / `credential_list`；
 *    写入口是页头的三个向导（见下），它们调 `provider_upsert` / `credential_upsert` /
 *    `credential_set_secret` / `model_bulk_update` —— 都在 `src-tauri/src/commands/mod.rs` 的
 *    `collect_commands!` 里（`src/app/commandSurface.guard.test.ts` 机械检查）。
 *    **本页已有真入口**：`providers-add-open`（两步向导：[docs/19 §1] 服务商信息 → 添加模型，
 *    `./providerWizard.tsx`）、`providers-add-advanced-open`（**原样的单步对话框**，
 *    `./wizards.tsx`，一次填完 id / 鉴权方式等完整字段）与 `credentials-add-open`（添加密钥，
 *    明文只进密钥仓库）。编辑/删除/连通性测试（详情页那一档）**仍没有**入口；
 *    在那之前不放假控件、也不放禁用占位。
 * 2. **没有读取接口的就不猜**：状态灯、成功率、延迟、通道数都**只**取后端返回的那一列；
 *    后端没给（Rust 的 `provider_list` 返回的是完整 `Provider`，没有这几列）就显示「未知/—」。
 *    内核在跑**不等于**服务商可用（[06 §2.1] B2），所以绝不点一个假绿灯。
 * 3. **凭据数读不到就是未知**：`credential_list` 失败只让"凭据数"这一列变未知 + 一行错误，
 *    **不把整张表打成错误**，也绝不填 0。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("provider_list", undefined, qk.providers)`   → 目录（详情页共用同一个 key）
 * - `useCommandQuery("credential_list", undefined, qk.credentials)`→ 凭据（用来数"挂载了几个凭据"）
 * - 两个添加向导（`./wizards.tsx`）在成功后失效 `qk.providers` / `qk.credentials`
 *
 * 契约与 Rust 的字段偏差、以及"为什么详情页也从 `provider_list` 取行"见 `model.ts` 的文件头。
 */

import { Box, Button, Link, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { useMemo, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader, StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatPercent } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import { mountOf, readCredential, readProvider, type CredentialRow, type ProviderRow } from "./model";
import { ProviderTwoStepWizard } from "./providerWizard";
import { readWizardSession, resetWizardSession, writeWizardSession } from "./providerWizardModel";
import { CredentialAddDialog, ProviderAddDialog } from "./wizards";

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

/**
 * 「高级添加」入口的按钮文案（点开的是**原样的单步对话框**，字段 tid 全不变）。
 *
 * 为什么这里是字面量而不是 `t(...)`：本轮的写范围**明确禁止改 `src/shared/i18n/*.ts`**
 * （键由主管独占），而现有键集合里没有这一条（`providers.add.*` 只有 title / open）。
 * 缺的键已报给主管：`providers.add.advanced` 一落地，这里就换成 `t("providers.add.advanced")`。
 */
const ADVANCED_ADD_LABEL = "高级添加";

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

  /**
   * 三个向导的开合。**列表页只读**，写全部落在 `./providerWizard.tsx`（两步向导）
   * 与 `./wizards.tsx`（两个单步对话框）里。
   *
   * 两步向导的"开着/第几步/草稿"活在 `./providerWizardModel.ts` 的**模块级 store** 里：
   * 用户点 `pw-proxy` 去 `/network` 会把这一页整个卸载，草稿必须比组件活得久
   * （[docs/19 §1.4]）。所以初值从 store 读 —— 从 `/network` 回来时向导自己还是开着的。
   */
  const [providerAddOpen, setProviderAddOpen] = useState(() => readWizardSession().open);
  const [advancedAddOpen, setAdvancedAddOpen] = useState(false);
  const [credentialAddOpen, setCredentialAddOpen] = useState(false);

  const openProviderWizard = (): void => {
    writeWizardSession({ open: true });
    setProviderAddOpen(true);
  };
  const closeProviderWizard = (): void => {
    resetWizardSession();
    setProviderAddOpen(false);
  };

  const rows = useMemo(() => (providers.data ?? []).map((p) => readProvider(p)), [providers.data]);
  /** `undefined`（读不到）与 `[]`（确实是空）在这里被严格区分开。 */
  const credentialRows = useMemo(
    () => (credentials.data === undefined ? undefined : credentials.data.map((c) => readCredential(c))),
    [credentials.data],
  );

  /** 本地去重用的现有 id：契约的视图与 Rust 的完整实体都有 `id`，两种形状都读得到。 */
  const providerIds = useMemo(() => (providers.data ?? []).map((p) => p.id), [providers.data]);
  /** 两步向导的显示名称**去重**要对着后端返回的现有名字做（[docs/19 §1.1]）。 */
  const providerNames = useMemo(
    () => (providers.data ?? []).map((p) => p.name).filter((name): name is string => typeof name === "string"),
    [providers.data],
  );
  const credentialIds = useMemo(() => (credentials.data ?? []).map((c) => c.credential.id), [credentials.data]);

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
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            <Button variant="outlined" onClick={refresh}>
              {t("providers.refresh")}
            </Button>
            {/*
              三个入口的按钮文案：两步向导用它的 title 键（「添加服务商」）、密钥入口用
              `credentials.add.title`（「添加密钥」）—— 并排两个「添加」用户分不出谁是谁。
              高级添加的文案见下面的常量说明。
            */}
            <Button
              variant="outlined"
              data-testid="credentials-add-open"
              onClick={() => setCredentialAddOpen(true)}
            >
              {t("credentials.add.title")}
            </Button>
            <Button
              variant="outlined"
              data-testid="providers-add-advanced-open"
              onClick={() => setAdvancedAddOpen(true)}
            >
              {ADVANCED_ADD_LABEL}
            </Button>
            <Button
              variant="contained"
              data-testid="providers-add-open"
              onClick={openProviderWizard}
            >
              {t("providers.add.title")}
            </Button>
          </Box>
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

      {/*
        两步向导（[docs/19 §1]）：`providers-add-open` 进的就是它 ——
        第 1 步服务商信息（含落库）、第 2 步添加模型。
      */}
      <ProviderTwoStepWizard
        open={providerAddOpen}
        existing={{ ids: providerIds, names: providerNames }}
        onClose={closeProviderWizard}
      />
      {/*
        原来的**单步**对话框**原样保留**（字段 tid 一个没变），改挂到「高级添加」入口：
        它是真控件、真能提交 —— 老手要的"一次填完 id / 鉴权方式 / 头名 / 值前缀"仍然可达。
      */}
      <ProviderAddDialog
        open={advancedAddOpen}
        existingIds={providerIds}
        onClose={() => setAdvancedAddOpen(false)}
      />
      <CredentialAddDialog
        open={credentialAddOpen}
        providers={providers.data ?? []}
        existingIds={credentialIds}
        onClose={() => setCredentialAddOpen(false)}
      />
    </>
  );
}

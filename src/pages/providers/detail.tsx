/**
 * 服务商详情 `/providers/:id`（[07 §3.2]）—— 一个服务商的**配置真相**：
 * 连接（base_url / 协议 / 鉴权 / 超时 / TLS / 默认线路）、附加头与透传、挂载的凭据。
 *
 * 三条硬纪律：
 *
 * 1. **只调用已注册的命令**：`provider_list`（取这一行，理由见 `model.ts` 文件头）、
 *    `credential_list`（挂载的凭据）、`lane_list`（把 `lane` id 换成线路名）。
 *    [07 §3.2] 的第三个 Tab「测试」要用 `provider_test` / `provider_echo`：
 *    **本页当前不画那个 Tab**（不放禁用占位），它们的注册状态以 `collect_commands!` 为准
 *    —— `src/app/commandSurface.guard.test.ts` 会机械检查这一条。
 * 2. **后端没返回的字段不填坑**：契约说 `provider_list` 返回 `ProviderView`，
 *    Rust 实际返回完整的 `Provider` —— 两边字段不同。拿到视图形状（没有 `auth`/`timeout`/
 *    `tls`/`headers`）时**明说"连接配置没返回"**，而不是排一排空的占位值。
 * 3. **密钥只以引用形式出现**：凭据的 `secret` 是 `${secret:name}`，明文不在这里显示；
 *    列出的字段网格里额外 CA 只显示"配没配"（完整内容在「原始字段」里原样可见，不藏数据）。
 *
 * 数据流见 `index.tsx`（两页共用 `qk.providers`），字段偏差见 `model.ts`。
 *
 * ## 图形化编辑（[docs/20 §4]）
 *
 * 页头有 `providers-detail-edit`：点开的是**与"添加服务商"同一个两步向导**
 * （`./providerWizard.tsx`），只是以 `mode: "edit"` 打开 —— 第 1 步预填现有名称/协议/地址、
 * 密钥框留空 = 不改，第 2 步与新增完全一致（扫描、勾选添加、逐行改线路）。
 * 打开之前先把草稿种进那个模块级会话（`startEditSession`），所以从 `/network` 绕一圈回来
 * 它照样在（与新增路径同一条数据路径）。
 */

import {
  Box,
  Button,
  Checkbox,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { useNavigate, useParams } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { Protocol, QuotaStateValue } from "@/shared/bindings";
import { ConfirmDelete, HelpTip, PageHeader, QuotaBar, StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatMoney, formatPercent, formatTokens } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  credentialsOf,
  laneNameOf,
  readCredential,
  readLane,
  readProvider,
  type CredentialRow,
  type ProviderRow,
} from "./model";
import { ProviderTwoStepWizard } from "./providerWizard";
import { resetWizardSession, startEditSession } from "./providerWizardModel";

// ================================================================ 文案映射（显式表，不用模板拼 key）

const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

const AUTH_LABEL: Record<string, I18nKey> = {
  bearer: "providers.authScheme.bearer",
  header: "providers.authScheme.header",
  query: "providers.authScheme.query",
  none: "providers.authScheme.none",
};

const CONFLICT_LABEL: Record<string, I18nKey> = {
  keep_client: "providers.headerConflict.keep_client",
  override: "providers.headerConflict.override",
};

const PASSTHROUGH_HEADER_LABEL: Record<string, I18nKey> = {
  pass: "providers.passthrough.header.pass",
  allowlist: "providers.passthrough.header.allowlist",
};

const PASSTHROUGH_BODY_LABEL: Record<string, I18nKey> = {
  preserve: "providers.passthrough.body.preserve",
  strip: "providers.passthrough.body.strip",
};

const METRIC_LABEL: Record<string, I18nKey> = {
  tokens: "providers.metric.tokens",
  requests: "providers.metric.requests",
  cost: "providers.metric.cost",
  unknown: "providers.metric.unknown",
};

const WINDOW_LABEL: Record<string, I18nKey> = {
  hour: "providers.quota.window.hour",
  day: "providers.quota.window.day",
  month: "providers.quota.window.month",
  none: "providers.quota.window.none",
};

/** 服务商健康状态（契约视图的 `status`）→ 全站状态语义（[15 §3.2]）。 */
const STATUS_SEM: Record<string, SemStatus> = { ok: "ok", warn: "warn", error: "error", off: "off" };
const STATUS_LABEL: Record<string, I18nKey> = {
  ok: "component.statusDot.ok",
  warn: "component.statusDot.warn",
  error: "component.statusDot.error",
  off: "component.statusDot.off",
};

/** 后端额度状态 → 语义色 / 文案。 */
const QUOTA_STATE_SEM: Record<string, SemStatus> = {
  ok: "ok",
  soft: "warn",
  exhausted: "error",
  unknown: "off",
};
const QUOTA_STATE_LABEL: Record<string, I18nKey> = {
  ok: "component.quotaBar.state.ok",
  soft: "component.quotaBar.state.soft",
  exhausted: "component.quotaBar.state.exhausted",
  unknown: "component.quotaBar.state.unknown",
};

/** 数值缺失一律画「—」（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

const GRID_SX = {
  display: "grid",
  gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
  gap: 4,
} as const;

// ================================================================ 删除（[docs/21 §1]）

/**
 * 「会连带删除 / 影响」清单的行文案：用 i18n 键 `delete.consequence.credentials` / `.channels`
 * （第 73 轮主管补上；在这之前这里是带注释的字面量，英文界面会露中文）。
 *
 * **只放前端真算得出来的事实**：本页读得到 `provider_list` / `credential_list` / `lane_list`，
 * **读不到通道清单** —— 所以"这份凭据被几条通道引用"这类话这里**一个字都不写**（算不出来不编）。
 */

/** 表头全选复选框的无障碍标签：用 `common.selectAll`（见 `providers/index.tsx` 同一常量说明）。 */
const SELECT_ALL_LABEL_KEY: I18nKey = "common.selectAll";

/** 一次删除的目标（单条与批量共用同一个形状）。 */
type DeleteTarget = {
  readonly ids: readonly string[];
  readonly label: string;
  /** 批量**没有**级联按钮（[docs/21 §1.2]：批量一律 `force=false`）。 */
  readonly batch: boolean;
  readonly consequences: readonly string[];
};

/** 执行完的**事实**（不带文案：文案由页面用 `t(...)` 拼）。 */
type DeleteOutcome = {
  deletedIds: string[];
  failures: { id: string; message: string }[];
};

/**
 * 逐条执行删除：**一条失败不中止其余条**；批量一律 `force=false`（机械地钉在这里）。
 * 本页两条流程（服务商 / 凭据）共用这一份，语义与 `providers/index.tsx` 完全一致。
 */
async function runDelete(
  target: DeleteTarget,
  force: boolean,
  remove: (id: string, force: boolean) => Promise<unknown>,
): Promise<DeleteOutcome> {
  const effectiveForce = target.batch ? false : force;
  const deletedIds: string[] = [];
  const failures: { id: string; message: string }[] = [];

  for (const id of target.ids) {
    try {
      await remove(id, effectiveForce);
      deletedIds.push(id);
    } catch (err) {
      failures.push({ id, message: errorMessage(err) });
    }
  }

  return { deletedIds, failures };
}

// ================================================================ 小组件与纯函数

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/** 一个字段格：标签（overline）+ 值。层级只靠字号与颜色，没有装饰（[15 §1]）。 */
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Box sx={{ minWidth: 0 }}>
      <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
        {label}
      </Typography>
      <Box sx={{ minWidth: 0 }}>{children}</Box>
    </Box>
  );
}

/** 普通值（界面语言）。 */
function Value({ children }: { children: ReactNode }) {
  return (
    <Typography variant="body1" sx={{ overflowWrap: "anywhere" }}>
      {children}
    </Typography>
  );
}

/** 等宽值（id / URL / 头名 / 密钥引用这类"机器字符串"）。 */
function MonoValue({ children }: { children: ReactNode }) {
  return (
    <Typography variant="body1" sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}>
      {children}
    </Typography>
  );
}

/** 秒 → `10s`；缺失画「—」。 */
function seconds(value: number | undefined): string {
  return value === undefined ? DASH : `${value}s`;
}

/**
 * 编辑模式预填用的协议值。
 *
 * 后端给什么就用什么：能对上 `PROTOCOLS` 就是它，**对不上也原样带进草稿**
 * （契约外的取值不改写 —— 与这一页"原样显示"同一条纪律，向导的协议下拉会为它补一行原值）。
 * 只有后端**根本没给**协议时才落到契约里的 `custom`（那种配置本身是不完整的，用户在
 * 下拉里看得见、也改得掉）。
 */
function editProtocolOf(raw: string | undefined): Protocol {
  return raw === undefined || raw === "" ? "custom" : (raw as Protocol);
}

/** 把后端的线上值翻成界面语言；契约外的取值**原样显示**（是后端说的话，不改写、不猜）。 */
function labelOf(
  map: Record<string, I18nKey>,
  t: (key: I18nKey) => string,
  raw: string | undefined,
): string | undefined {
  if (raw === undefined) return undefined;
  const key = map[raw];
  return key === undefined ? raw : t(key);
}

/**
 * 额度**上限**的显示值：按 `metric` 选单位（token 缩写 / 金额带币种 / 其余千分位）。
 * 只做格式化，不做换算。
 */
function limitValue(credential: CredentialRow): string | undefined {
  const limit = credential.limit;
  if (limit === undefined) return undefined;
  if (credential.metric === "cost" && credential.currency !== undefined) {
    return formatMoney({ amount: limit, currency: credential.currency });
  }
  if (credential.metric === "tokens") return formatTokens(limit);
  return formatInt(limit);
}

/**
 * 额度**已用**的显示值 —— **只用后端已经给的数字**。
 * `metric` 与数字对不上时返回 `undefined`（不拿"唯一那个数"硬套单位）。
 */
function usedValue(credential: CredentialRow): string | undefined {
  if (credential.metric === "cost" && credential.usedCost !== undefined) {
    return credential.currency === undefined
      ? formatInt(credential.usedCost)
      : formatMoney({ amount: credential.usedCost, currency: credential.currency });
  }
  if (credential.metric === "tokens" && credential.usedTokens !== undefined) {
    return formatTokens(credential.usedTokens);
  }
  if (credential.metric === "requests" && credential.usedRequests !== undefined) {
    return formatInt(credential.usedRequests);
  }
  return undefined;
}

// ================================================================ 展示层（纯 props，便于测各分支）

export type ProviderDetailBodyProps = {
  /** 路由参数里的 id（"目录里没有"这句话要把它写出来）。 */
  id: string;
  /** 目录里这一行。**`undefined` = 目录里没有这个 id**（不是"读失败"，读失败走 `error`）。 */
  row: ProviderRow | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /** 挂载的凭据：`undefined` = 读不到，空数组 = 确实没有。 */
  credentials: CredentialRow[] | undefined;
  credentialsError: unknown;
  onRetryCredentials: () => void;
  /** `lane` id → 线路名；`undefined` = 名字读不到（退回显示 id，不编名字）。 */
  laneName: (laneId: string) => string | undefined;
  laneError: unknown;
  /** 返回列表（展示层不认识 router）。 */
  onBack: () => void;
  /**
   * 凭据表上的删除入口（[docs/21 §3]）。**省略 = 这一页没有凭据删除入口** ——
   * 纯展示件不自己造写入口，数据层（页面）注入真处理器。
   */
  onDeleteCredential?: (credential: CredentialRow) => void;
  /** 凭据多选：行 `providers-credential-select` / 表头 `-select-all` / 计数 `-selected-count`。 */
  credentialSelectedIds?: readonly string[];
  onToggleCredentialSelect?: (id: string) => void;
  onToggleCredentialSelectAll?: () => void;
  /** 「删除选中」凭据（`providers-credential-delete-selected`）：一次确认后**逐条**删。 */
  onDeleteCredentialsSelected?: () => void;
};

export function ProviderDetailBody({
  id,
  row,
  isLoading,
  error,
  onRetry,
  credentials,
  credentialsError,
  onRetryCredentials,
  laneName,
  laneError,
  onBack,
  onDeleteCredential,
  credentialSelectedIds,
  onToggleCredentialSelect,
  onToggleCredentialSelectAll,
  onDeleteCredentialsSelected,
}: ProviderDetailBodyProps) {
  const { t } = useTranslation();

  const mounted = credentials === undefined ? undefined : credentialsOf(credentials, id);
  const credentialSelected = credentialSelectedIds ?? [];
  const credentialSelectedSet = useMemo(
    () => new Set(credentialSelected),
    [credentialSelected],
  );
  /** 多选只在数据层把两个处理器都注入时才画（否则是一排点了没用的复选框）。 */
  const credentialSelectable =
    onToggleCredentialSelect !== undefined && onToggleCredentialSelectAll !== undefined;
  const mountedIds = (mounted ?? []).map((credential) => credential.id);
  const allCredentialsSelected =
    mountedIds.length > 0 && mountedIds.every((credentialId) => credentialSelectedSet.has(credentialId));
  const someCredentialsSelected =
    !allCredentialsSelected && mountedIds.some((credentialId) => credentialSelectedSet.has(credentialId));

  return (
    <Box
      data-testid="providers-detail"
      sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}
    >
      {error !== undefined && error !== null ? (
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            <Typography
              variant="body1"
              data-testid="providers-detail-error"
              sx={{ color: "error.main" }}
            >
              {t("providers.detail.error", { message: errorMessage(error) })}
            </Typography>
            <Button variant="outlined" onClick={onRetry}>
              {t("common.retry")}
            </Button>
          </Stack>
        </Paper>
      ) : isLoading ? (
        <Paper variant="outlined" sx={SECTION_SX}>
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {t("common.loading")}
          </Typography>
        </Paper>
      ) : row === undefined ? (
        /* 目录是完整的：不在里面就是"没有这条"，不是"读失败"（失败走上面的 error 分支） */
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3} data-testid="providers-detail-notfound">
            <Typography variant="h4">{t("providers.detail.notFound.title")}</Typography>
            <Typography variant="body1">{t("providers.detail.notFound.body", { id })}</Typography>
            <Box>
              <Button variant="outlined" onClick={onBack}>
                {t("providers.detail.back")}
              </Button>
            </Box>
          </Stack>
        </Paper>
      ) : (
        <>
          {/* ① 概览：契约视图给的健康列 + 完整实体给的默认线路 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-overview">
            <Stack spacing={4}>
              {/* 区块说明（那句"这四列只有后端视图才提供"）是**解释** → 收进标题右边的问号
                  （[docs/22 §2]）；它不再铺在字段网格下面。**数值与「—」照旧留在网格里。** */}
              <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                <Typography variant="h4">{t("providers.section.overview")}</Typography>
                <HelpTip
                  text={t("providers.honesty.detailFields")}
                  testId="providers-overview-help"
                />
              </Box>

              <Box sx={GRID_SX}>
                <Field label={t("providers.field.id")}>
                  <MonoValue>{row.id === "" ? DASH : row.id}</MonoValue>
                </Field>
                <Field label={t("providers.field.name")}>
                  <Value>{row.name === "" ? DASH : row.name}</Value>
                </Field>
                <Field label={t("providers.field.protocol")}>
                  <Value>{labelOf(PROTOCOL_LABEL, t, row.protocol) ?? DASH}</Value>
                </Field>
                <Field label={t("providers.field.baseUrl")}>
                  <MonoValue>{row.baseUrl ?? DASH}</MonoValue>
                </Field>
                <Field label={t("providers.field.enabled")}>
                  <Value>
                    {row.enabled === undefined
                      ? t("common.unknown")
                      : row.enabled
                        ? t("common.enabled")
                        : t("common.disabled")}
                  </Value>
                </Field>
                <Field label={t("providers.field.lane")}>
                  {row.lane === undefined ? (
                    <Value>{t("providers.field.laneNone")}</Value>
                  ) : laneName(row.lane) === undefined ? (
                    <MonoValue>{t("providers.field.laneUnresolved", { id: row.lane })}</MonoValue>
                  ) : (
                    <Value>{`${laneName(row.lane)} · ${row.lane}`}</Value>
                  )}
                </Field>
                <Field label={t("providers.field.status")}>
                  <StatusDot
                    status={row.status === undefined ? "off" : (STATUS_SEM[row.status] ?? "off")}
                    label={
                      row.status === undefined
                        ? t("common.unknown")
                        : (labelOf(STATUS_LABEL, t, row.status) ?? row.status)
                    }
                  />
                </Field>
                <Field label={t("providers.field.successRate")}>
                  <Value>
                    {row.successRate24h === undefined
                      ? DASH
                      : formatPercent(row.successRate24h, 1)}
                  </Value>
                </Field>
                <Field label={t("providers.field.latency")}>
                  <Value>
                    {row.avgLatencyMs === undefined ? DASH : formatLatency(row.avgLatencyMs)}
                  </Value>
                </Field>
                <Field label={t("providers.field.channelCount")}>
                  <Value>{row.channelCount === undefined ? DASH : formatInt(row.channelCount)}</Value>
                </Field>
              </Box>

              {/* 线路名读不到是**后端原文 + TA 码**（[docs/22 §2 第 2 条]）→ 留在版面上 */}
              {laneError !== undefined && laneError !== null ? (
                <Typography
                  variant="caption"
                  data-testid="providers-lane-error"
                  sx={{ color: "error.main" }}
                >
                  {t("providers.lane.error", { message: errorMessage(laneError) })}
                </Typography>
              ) : null}
            </Stack>
          </Paper>

          {/* ② 连接：只有完整 `Provider` 才有这些字段 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-connection">
            <Stack spacing={4}>
              {/* 三句区块说明（「—」是什么意思 / 读超时为什么给流式留余量 / 额外 CA 在这里只显示配没配）
                  合成**一条**提示（[docs/22 §2/§3]：一个区块最多一个问号）。 */}
              <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                <Typography variant="h4">{t("providers.section.connection")}</Typography>
                <HelpTip
                  text={[
                    t("providers.connection.partial"),
                    t("providers.connection.hint"),
                    t("providers.tls.extraCaHint"),
                  ].join("\n")}
                  testId="providers-connection-help"
                />
              </Box>

              {!row.hasConnectionConfig ? (
                /* 后端只给了视图 → 明说不可用，不排空占位（"没返回"与"空值"是两件事） */
                <Typography variant="body1" data-testid="providers-connection-unavailable">
                  {t("providers.connection.unavailable")}
                </Typography>
              ) : (
                <>
                  <Box sx={GRID_SX}>
                    <Field label={t("providers.field.authScheme")}>
                      <Value>{labelOf(AUTH_LABEL, t, row.authScheme) ?? DASH}</Value>
                    </Field>
                    <Field label={t("providers.field.authHeader")}>
                      <MonoValue>{row.authHeader ?? DASH}</MonoValue>
                    </Field>
                    <Field label={t("providers.field.authPrefix")}>
                      <MonoValue>{row.authPrefix ?? DASH}</MonoValue>
                    </Field>
                    <Field label={t("providers.field.modelPrefix")}>
                      <MonoValue>{row.modelPrefix ?? DASH}</MonoValue>
                    </Field>
                    <Field label={t("providers.field.timeoutConnect")}>
                      <Value>{seconds(row.timeoutConnectS)}</Value>
                    </Field>
                    <Field label={t("providers.field.timeoutRead")}>
                      <Value>{seconds(row.timeoutReadS)}</Value>
                    </Field>
                    <Field label={t("providers.field.timeoutTotal")}>
                      <Value>{seconds(row.timeoutTotalS)}</Value>
                    </Field>
                    <Field label={t("providers.field.tlsInsecure")}>
                      <Value>
                        {row.tlsInsecure === undefined
                          ? DASH
                          : row.tlsInsecure
                            ? t("common.yes")
                            : t("common.no")}
                      </Value>
                    </Field>
                    <Field label={t("providers.field.tlsExtraCa")}>
                      <Value>
                        {row.tlsExtraCa
                          ? t("providers.tls.extraCaConfigured")
                          : t("providers.tls.extraCaNone")}
                      </Value>
                    </Field>
                    <Field label={t("providers.field.tlsMinVersion")}>
                      <MonoValue>{row.tlsMinVersion ?? DASH}</MonoValue>
                    </Field>
                  </Box>
                </>
              )}
            </Stack>
          </Paper>

          {/* ③ 附加头与透传：同属完整 `Provider` */}
          {row.hasConnectionConfig ? (
            <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-headers">
              <Stack spacing={4}>
                <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                  <Typography variant="h4">{t("providers.section.headers")}</Typography>
                  {/* 「附加头默认只补缺，不覆盖同名头」是解释 → 收进问号（[docs/22 §2]）；
                      同名头冲突的**取值**仍在下面那个字段格里原样可见。 */}
                  <HelpTip text={t("providers.headers.hint")} testId="providers-headers-help" />
                </Box>

                {row.headers.length === 0 ? (
                  <Typography variant="body1" sx={{ color: "text.secondary" }}>
                    {t("providers.headers.none")}
                  </Typography>
                ) : (
                  <Box sx={{ overflowX: "auto" }}>
                    <Table size="small" data-testid="providers-headers-table">
                      <TableHead>
                        <TableRow>
                          <TableCell>{t("providers.headers.col.name")}</TableCell>
                          <TableCell>{t("providers.headers.col.value")}</TableCell>
                        </TableRow>
                      </TableHead>
                      <TableBody>
                        {row.headers.map((header) => (
                          <TableRow key={header.name}>
                            <TableCell sx={{ fontFamily: typography.fontMono }}>
                              {header.name}
                            </TableCell>
                            <TableCell
                              sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                            >
                              {header.value}
                            </TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </Box>
                )}

                <Box sx={GRID_SX}>
                  <Field label={t("providers.field.headerConflict")}>
                    <Value>{labelOf(CONFLICT_LABEL, t, row.onHeaderConflict) ?? DASH}</Value>
                  </Field>
                  <Field label={t("providers.field.passthroughHeader")}>
                    <Value>
                      {labelOf(PASSTHROUGH_HEADER_LABEL, t, row.passthroughHeaderMode) ?? DASH}
                    </Value>
                  </Field>
                  <Field label={t("providers.field.passthroughBody")}>
                    <Value>{labelOf(PASSTHROUGH_BODY_LABEL, t, row.passthroughBody) ?? DASH}</Value>
                  </Field>
                </Box>
              </Stack>
            </Paper>
          ) : null}

          {/* ④ 凭据：挂载关系写在凭据的 provider 字段上 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-credentials">
            <Stack spacing={4}>
              <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                <Typography variant="h4">{t("providers.section.credentials")}</Typography>
                {/* 「密钥只以引用形式出现 / 已用比例只用后端给的 used_pct」是解释 → 收进问号
                    （[docs/22 §2]）。**空态与「这份凭据存的是明文密钥」那条状态留在版面上**：
                    前者是结论（§2 第 1 条），后者是这一行的告警。 */}
                <HelpTip
                  text={t("providers.credentials.secretHint")}
                  testId="providers-credentials-help"
                />
              </Box>

              {credentialsError !== undefined && credentialsError !== null ? (
                <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                  <Typography
                    variant="body1"
                    data-testid="providers-credentials-error"
                    sx={{ color: "error.main" }}
                  >
                    {t("providers.credentials.error", { message: errorMessage(credentialsError) })}
                  </Typography>
                  <Button variant="outlined" onClick={onRetryCredentials}>
                    {t("common.retry")}
                  </Button>
                </Stack>
              ) : mounted === undefined ? (
                <Typography variant="body1" sx={{ color: "text.secondary" }}>
                  {t("common.loading")}
                </Typography>
              ) : mounted.length === 0 ? (
                <Stack spacing={1} data-testid="providers-credentials-empty">
                  <Typography variant="body1">{t("providers.credentials.empty")}</Typography>
                  <Caption>{t("providers.credentials.emptyHint")}</Caption>
                </Stack>
              ) : (
                <>
                  {/* 删除选中：选中数 + 「删除选中」（[docs/21 §1.2]） */}
                  {onDeleteCredentialsSelected === undefined ? null : (
                    <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                      <Typography variant="caption" data-testid="providers-credential-selected-count">
                        {t("delete.selectedCount", { count: credentialSelected.length })}
                      </Typography>
                      <Button
                        variant="outlined"
                        color="error"
                        data-testid="providers-credential-delete-selected"
                        disabled={credentialSelected.length === 0}
                        onClick={onDeleteCredentialsSelected}
                      >
                        {t("common.deleteSelected")}
                      </Button>
                    </Stack>
                  )}

                  <Box sx={{ overflowX: "auto" }}>
                    <Table size="small" data-testid="providers-credentials-table">
                      <TableHead>
                        <TableRow>
                          {credentialSelectable ? (
                            <TableCell padding="checkbox">
                              <Checkbox
                                data-testid="providers-credential-select-all"
                                checked={allCredentialsSelected}
                                indeterminate={someCredentialsSelected}
                                onChange={onToggleCredentialSelectAll}
                                slotProps={{ input: { "aria-label": t(SELECT_ALL_LABEL_KEY) } }}
                              />
                            </TableCell>
                          ) : null}
                          <TableCell>{t("providers.credentials.col.label")}</TableCell>
                          <TableCell>{t("providers.credentials.col.enabled")}</TableCell>
                          <TableCell>{t("providers.credentials.col.lane")}</TableCell>
                          <TableCell>{t("providers.credentials.col.quota")}</TableCell>
                          <TableCell>{t("providers.credentials.col.tags")}</TableCell>
                          {/* 操作列表头留空（同列表页）：不为一个表头去造 i18n 键 */}
                          {onDeleteCredential === undefined ? null : <TableCell align="right" />}
                        </TableRow>
                      </TableHead>
                      <TableBody>
                        {mounted.map((credential) => {
                        const stateValue =
                          credential.quotaState !== undefined &&
                          QUOTA_STATE_SEM[credential.quotaState] !== undefined
                            ? (credential.quotaState as QuotaStateValue)
                            : undefined;
                        const stateSem =
                          credential.quotaState === undefined
                            ? "off"
                            : (QUOTA_STATE_SEM[credential.quotaState] ?? "off");
                        const stateLabel =
                          credential.quotaState === undefined
                            ? t("common.unknown")
                            : (labelOf(QUOTA_STATE_LABEL, t, credential.quotaState) ??
                              credential.quotaState);

                        /** 额度明细：**只用后端给的数字**（`used_pct` 不由前端计算）。 */
                        const limitText = limitValue(credential);
                        const usedText = usedValue(credential);
                        const parts: string[] = [];
                        if (limitText !== undefined) {
                          parts.push(
                            usedText === undefined
                              ? t("providers.quota.limit", { limit: limitText })
                              : t("providers.quota.usedOf", { used: usedText, limit: limitText }),
                          );
                        }
                        const metricText = labelOf(METRIC_LABEL, t, credential.metric);
                        if (metricText !== undefined) parts.push(metricText);
                        const windowText = labelOf(WINDOW_LABEL, t, credential.window);
                        if (windowText !== undefined) parts.push(windowText);
                        const detail = parts.join(" · ");

                        return (
                          <TableRow key={credential.id} data-testid="providers-credential-row">
                            {credentialSelectable ? (
                              <TableCell padding="checkbox">
                                <Checkbox
                                  data-testid="providers-credential-select"
                                  checked={credentialSelectedSet.has(credential.id)}
                                  onChange={() => onToggleCredentialSelect?.(credential.id)}
                                  slotProps={{
                                    // 无障碍标签用这一行自己的标签 / id（用户数据，不需要新键）
                                    input: {
                                      "aria-label":
                                        credential.label === "" ? credential.id : credential.label,
                                    },
                                  }}
                                />
                              </TableCell>
                            ) : null}
                            <TableCell>
                              <Value>{credential.label === "" ? DASH : credential.label}</Value>
                              {credential.secret === undefined ? null : (
                                <Typography
                                  variant="caption"
                                  sx={{
                                    color: "text.secondary",
                                    fontFamily: typography.fontMono,
                                    display: "block",
                                    overflowWrap: "anywhere",
                                  }}
                                >
                                  {credential.secret}
                                </Typography>
                              )}
                              {credential.plaintextSecret === true ? (
                                <StatusDot status="warn" label={t("providers.credentials.plaintext")} />
                              ) : null}
                            </TableCell>
                            <TableCell sx={{ color: "text.secondary" }}>
                              {credential.enabled === undefined
                                ? t("common.unknown")
                                : credential.enabled
                                  ? t("common.enabled")
                                  : t("common.disabled")}
                            </TableCell>
                            <TableCell sx={{ color: "text.secondary" }}>
                              {credential.lane === undefined
                                ? DASH
                                : (laneName(credential.lane) ?? credential.lane)}
                            </TableCell>
                            <TableCell>
                              {credential.limit === undefined ? (
                                /* 未设上限：显式说出来（[05 §2.1]），不是画一个 0% */
                                <QuotaBar usedPct={null} state={stateValue} width={160} />
                              ) : credential.usedPct !== undefined ? (
                                <QuotaBar
                                  usedPct={credential.usedPct}
                                  state={stateValue}
                                  detail={detail}
                                  width={160}
                                />
                              ) : (
                                /* 后端没给已用比例 → 只陈述上限与后端给的状态，不推导百分比 */
                                <Stack spacing={1}>
                                  <Typography
                                    variant="caption"
                                    sx={{
                                      color: "text.secondary",
                                      fontFamily: typography.fontMono,
                                      fontVariantNumeric: "tabular-nums",
                                    }}
                                  >
                                    {detail}
                                  </Typography>
                                  <StatusDot status={stateSem} label={stateLabel} />
                                </Stack>
                              )}
                            </TableCell>
                            <TableCell sx={{ color: "text.secondary", maxWidth: 280 }}>
                              {credential.tags.length === 0 ? DASH : credential.tags.join(" · ")}
                            </TableCell>
                            {onDeleteCredential === undefined ? null : (
                              <TableCell align="right">
                                <Button
                                  size="small"
                                  variant="outlined"
                                  color="error"
                                  data-testid="providers-credential-delete"
                                  onClick={() => onDeleteCredential(credential)}
                                >
                                  {t("common.delete")}
                                </Button>
                              </TableCell>
                            )}
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                </Box>
                </>
              )}
            </Stack>
          </Paper>

          {/* ⑤ 原始字段：后端返回的整条记录，原样显示（未做任何清洗） */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-raw-section">
            <Stack spacing={2}>
              <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                <Typography variant="h4">{t("providers.section.raw")}</Typography>
                {/* 「原样显示、未做任何清洗」是解释 → 收进问号（[docs/22 §2]）；
                    下面的 `pre` 里那份原文一个字都没动。 */}
                <HelpTip
                  text={t("providers.section.rawHint")}
                  testId="providers-raw-section-help"
                />
              </Box>
              <Box
                component="pre"
                data-testid="providers-raw"
                sx={{
                  m: 0,
                  p: 3,
                  bgcolor: "background.surface",
                  borderRadius: `${radius.control}px`,
                  border: "1px solid",
                  borderColor: "divider",
                  fontFamily: typography.fontMono,
                  fontSize: typography.size.meta,
                  lineHeight: typography.lineHeight.table,
                  color: "text.secondary",
                  whiteSpace: "pre-wrap",
                  overflowWrap: "anywhere",
                  maxHeight: 320,
                  overflow: "auto",
                }}
              >
                {JSON.stringify(row.raw, null, 2)}
              </Box>
            </Stack>
          </Paper>
        </>
      )}
    </Box>
  );
}

// ================================================================ 数据层

export default function ProviderDetailPage() {
  const { t } = useTranslation();
  const params = useParams<{ id: string }>();
  const id = params.id ?? "";
  const navigate = useNavigate();

  // 与列表页同一个 key：从列表点进来时是缓存命中，不需要再打一次后端
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);

  const row = useMemo(
    () =>
      (providers.data ?? [])
        .map((p) => readProvider(p))
        .find((candidate) => candidate.id === id),
    [providers.data, id],
  );
  const credentialRows = useMemo(
    () =>
      credentials.data === undefined ? undefined : credentials.data.map((c) => readCredential(c)),
    [credentials.data],
  );
  const laneOptions = useMemo(() => (lanes.data ?? []).map((l) => readLane(l)), [lanes.data]);

  /**
   * 编辑向导（[docs/20 §4]）。
   *
   * 打开顺序很重要：**先把草稿种进会话，再让它 open** —— 组件在 `open` 变真时从会话读一次
   * （与从 `/network` 回来走同一条路径）。这样预填、"留空密钥 = 不改"、
   * "名称去重排除自己"三件事都由会话里的那一份负责，组件里不需要第二套状态。
   */
  const [editOpen, setEditOpen] = useState(false);
  const openEdit = (): void => {
    if (row === undefined) return;
    startEditSession({
      providerId: row.id,
      name: row.name,
      protocol: editProtocolOf(row.protocol),
      baseUrl: row.baseUrl ?? "",
    });
    setEditOpen(true);
  };
  const closeEdit = (): void => {
    resetWizardSession();
    setEditOpen(false);
  };

  /** 名称去重对着后端给的整份目录做（编辑模式会把"自己"从里面摘掉）。 */
  const providerIds = useMemo(() => (providers.data ?? []).map((p) => p.id), [providers.data]);
  const providerNames = useMemo(
    () =>
      (providers.data ?? [])
        .map((p) => p.name)
        .filter((name): name is string => typeof name === "string"),
    [providers.data],
  );

  // ---------------------------------------------------------------- 删除（[docs/21 §1]）
  //
  // 本页有**两条**删除流程（页头删服务商、凭据表删凭据），语义完全一样：
  // 先确认 → `force=false` → 被引用时后端原文原样显示且此时才给级联按钮 → 成功写结果区。
  // 两条各自持有一份状态（mutate 的目标不同），执行体都是模块级的 `runDelete`。

  const qc = useQueryClient();
  const removeProvider = useCommandMutation("provider_delete");
  const removeCredential = useCommandMutation("credential_delete");

  /** 挂在当前服务商下的凭据（空数组 = 确实没有；凭据清单读不到时它也是空的）。 */
  const mountedCredentials = useMemo(
    () => (credentialRows === undefined ? [] : credentialsOf(credentialRows, id)),
    [credentialRows, id],
  );

  /** 一条删除流程的状态（本页两条：服务商 / 凭据）。 */
  type DeleteFlowState = {
    target: DeleteTarget | undefined;
    /** 后端原文（被引用时的错误）；有它对话框才显示、也才给级联按钮。 */
    error: string | undefined;
    /** 结果区：每一次事实一行。 */
    result: readonly string[];
  };
  const EMPTY_FLOW: DeleteFlowState = { target: undefined, error: undefined, result: [] };
  /** `useState` 的 setter 类型（`openFlow` / `cancelFlow` / `runFlow` 三个执行体共用）。 */
  type SetFlow = Dispatch<SetStateAction<DeleteFlowState>>;
  const [providerFlow, setProviderFlow] = useState<DeleteFlowState>(EMPTY_FLOW);
  const [credentialFlow, setCredentialFlow] = useState<DeleteFlowState>(EMPTY_FLOW);
  /** 凭据多选（按 id 存，顺序 = 勾选顺序）。 */
  const [credentialSelectedIds, setCredentialSelectedIds] = useState<readonly string[]>([]);
  /** 批量进行中（`mutateAsync` 逐条之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);

  const openFlow = (setFlow: SetFlow, target: DeleteTarget): void => {
    // 新一次操作开始 → 清掉上一次的汇报与错误（结果区永远只讲刚刚发生的事）
    setFlow({ target, error: undefined, result: [] });
  };

  const cancelFlow = (setFlow: SetFlow): void => {
    setFlow((prev) => ({ ...prev, target: undefined, error: undefined }));
  };

  /** 删除成功后要失效的查询（[docs/21 §1.1 第 4 条]）：服务商级联会动到凭据 / 模型 / 通道矩阵。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    for (const key of [qk.providers, qk.credentials, qk.models, qk.channelMatrix]) {
      await qc.invalidateQueries({ queryKey: key });
    }
  };

  /** 「会连带删除 / 影响」清单：只用本页读得到的查询算（凭据数 = 这一页的凭据表）。 */
  const providerConsequences = (): string[] => {
    if (credentialRows === undefined) return [];
    const lines = [
      t("delete.consequence.credentials", {
        value: formatInt(credentialsOf(credentialRows, id).length),
      }),
    ];
    // 通道数只有后端给了 `channel_count` 才算得出来（Rust 的 provider_list 没有这一列）
    if (row?.channelCount !== undefined) {
      lines.push(t("delete.consequence.channels", { value: formatInt(row.channelCount) }));
    }
    return lines;
  };

  /** 一次删除的公共执行体：跑完把**事实**写回对应的那份状态。 */
  const runFlow = async (
    setFlow: SetFlow,
    target: DeleteTarget,
    force: boolean,
    remove: (targetId: string, effectiveForce: boolean) => Promise<unknown>,
    nameOf: (targetId: string) => string,
  ): Promise<void> => {
    if (target.batch) setBatchRunning(true);
    const outcome = await runDelete(target, force, remove);
    setBatchRunning(false);
    const singleFailed = !target.batch && outcome.failures.length > 0;
    const result = target.batch
      ? [
          t("delete.result.summary", {
            ok: formatInt(outcome.deletedIds.length),
            failed: formatInt(outcome.failures.length),
          }),
          ...outcome.deletedIds.map((deletedId) => t("delete.result.ok", { name: nameOf(deletedId) })),
          ...outcome.failures.map((failure) =>
            t("delete.result.failed", { message: `${failure.id} · ${failure.message}` }),
          ),
        ]
      : singleFailed
        ? []
        : outcome.deletedIds.map((deletedId) => t("delete.result.ok", { name: nameOf(deletedId) }));
    setFlow((prev) => ({
      ...prev,
      error: singleFailed ? outcome.failures[0]!.message : undefined,
      result,
      // 单条失败时留着框（后端原文与级联按钮就在那里）；其余情况收框
      target: target.batch || !singleFailed ? undefined : target,
    }));
    // 只有真的删掉的才从选中里摘掉；失败的留着（用户可以直接再来一次）
    setCredentialSelectedIds((prev) => prev.filter((selectedId) => !outcome.deletedIds.includes(selectedId)));
    await invalidateAfterDelete();
  };

  const detailLabel = (name: string, entityId: string): string =>
    name === "" ? entityId : `${name}（${entityId}）`;

  const credentialNameOf = (credentialId: string): string => {
    const found = mountedCredentials.find((credential) => credential.id === credentialId);
    if (found === undefined) return credentialId;
    return found.label === "" ? found.id : found.label;
  };

  return (
    <>
      <PageHeader
        title={row === undefined ? t("nav.providerDetail") : row.name === "" ? row.id : row.name}
        description={t("providers.detail.description")}
        docRef="07 §3.2 服务商详情（连接 / 附加头与透传）"
        actions={
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            {/*
              编辑入口只在这一行真的存在时出现（目录里没有这个 id 时不放一个点了会错的按钮）。
              文案走 `providers.edit`。
            */}
            {row === undefined ? null : (
              <Button
                variant="contained"
                data-testid="providers-detail-edit"
                onClick={openEdit}
              >
                {t("providers.edit")}
              </Button>
            )}
            {/*
              「删除服务商」（[docs/21 §1 / §3]）：只在这一行真的存在时出现 ——
              目录里没有这个 id 时不放一个点了会错的按钮。点击**先弹确认框**，不发命令。
            */}
            {row === undefined ? null : (
              <Button
                variant="outlined"
                color="error"
                data-testid="providers-detail-delete"
                onClick={() =>
                  openFlow(setProviderFlow, {
                    ids: [row.id],
                    label: detailLabel(row.name, row.id),
                    batch: false,
                    consequences: providerConsequences(),
                  })
                }
              >
                {t("delete.title.provider")}
              </Button>
            )}
            <Button
              variant="outlined"
              onClick={() => {
                void providers.refetch();
                void credentials.refetch();
                void lanes.refetch();
              }}
            >
              {t("providers.refresh")}
            </Button>
          </Box>
        }
      />

      <ProviderDetailBody
        id={id}
        row={row}
        isLoading={providers.isPending}
        error={providers.error ?? undefined}
        onRetry={() => void providers.refetch()}
        credentials={credentialRows}
        credentialsError={credentials.error ?? undefined}
        onRetryCredentials={() => void credentials.refetch()}
        laneName={(laneId) => laneNameOf(laneOptions, laneId)}
        laneError={lanes.error ?? undefined}
        onBack={() => navigate("/providers")}
        onDeleteCredential={(credential) =>
          openFlow(setCredentialFlow, {
            ids: [credential.id],
            label: detailLabel(credential.label, credential.id),
            batch: false,
            // 本页读不到通道清单 → 这份凭据被哪些通道引用**算不出来**，就不写（不编）
            consequences: [],
          })
        }
        credentialSelectedIds={credentialSelectedIds}
        onToggleCredentialSelect={(credentialId) =>
          setCredentialSelectedIds((prev) =>
            prev.includes(credentialId)
              ? prev.filter((selectedId) => selectedId !== credentialId)
              : [...prev, credentialId],
          )
        }
        onToggleCredentialSelectAll={() => {
          const visible = mountedCredentials.map((credential) => credential.id);
          setCredentialSelectedIds((prev) =>
            visible.length > 0 && visible.every((credentialId) => prev.includes(credentialId))
              ? prev.filter((credentialId) => !visible.includes(credentialId))
              : [...new Set([...prev, ...visible])],
          );
        }}
        onDeleteCredentialsSelected={() => {
          const chosen = mountedCredentials.filter((credential) =>
            credentialSelectedIds.includes(credential.id),
          );
          if (chosen.length === 0) return;
          openFlow(setCredentialFlow, {
            ids: chosen.map((credential) => credential.id),
            label: chosen
              .map((credential) => detailLabel(credential.label, credential.id))
              .join("、"),
            batch: true,
            // 每一份凭据都算不出连带清单（本页没有通道清单）→ 保持空，不编
            consequences: [],
          });
        }}
      />

      {/* 删除结果区（两份流程各一块）：只写真的发生过的事 */}
      {providerFlow.result.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="providers-delete-result">
              {providerFlow.result.map((line, index) => (
                <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}
      {credentialFlow.result.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="providers-credential-delete-result">
              {credentialFlow.result.map((line, index) => (
                <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}

      {/*
        删除服务商（[docs/21 §1 / §2]）：testid 前缀用 `providers`（与列表页同一个实体同一个前缀，
        §3 表里点名的级联按钮就是 `providers-delete-cascade`）。先确认 → `force=false` →
        被引用时后端原文原样显示、此时才出现级联按钮。
      */}
      <ConfirmDelete
        open={providerFlow.target !== undefined}
        testIdPrefix="providers"
        title={t("delete.title.provider")}
        targetLabel={providerFlow.target?.label ?? ""}
        consequences={providerFlow.target?.consequences}
        cascadeAvailable={providerFlow.target !== undefined && !providerFlow.target.batch}
        pending={removeProvider.isPending || batchRunning}
        errorText={providerFlow.error}
        onConfirm={() => {
          if (providerFlow.target !== undefined) {
            void runFlow(
              setProviderFlow,
              providerFlow.target,
              false,
              (targetId, effectiveForce) => removeProvider.mutateAsync({ id: targetId, force: effectiveForce }),
              (targetId) => (row === undefined ? targetId : row.name === "" ? row.id : row.name),
            );
          }
        }}
        onConfirmCascade={() => {
          if (providerFlow.target !== undefined) {
            void runFlow(
              setProviderFlow,
              providerFlow.target,
              true,
              (targetId, effectiveForce) => removeProvider.mutateAsync({ id: targetId, force: effectiveForce }),
              (targetId) => (row === undefined ? targetId : row.name === "" ? row.id : row.name),
            );
          }
        }}
        onCancel={() => cancelFlow(setProviderFlow)}
      />

      {/* 删除凭据（[docs/21 §3]）：单条与批量同一个组件、同一套语义 */}
      <ConfirmDelete
        open={credentialFlow.target !== undefined}
        testIdPrefix="providers-credential"
        title={t("delete.title.credential")}
        targetLabel={credentialFlow.target?.label ?? ""}
        consequences={credentialFlow.target?.consequences}
        cascadeAvailable={credentialFlow.target !== undefined && !credentialFlow.target.batch}
        pending={removeCredential.isPending || batchRunning}
        errorText={credentialFlow.error}
        onConfirm={() => {
          if (credentialFlow.target !== undefined) {
            void runFlow(
              setCredentialFlow,
              credentialFlow.target,
              false,
              (targetId, effectiveForce) => removeCredential.mutateAsync({ id: targetId, force: effectiveForce }),
              credentialNameOf,
            );
          }
        }}
        onConfirmCascade={() => {
          if (credentialFlow.target !== undefined) {
            void runFlow(
              setCredentialFlow,
              credentialFlow.target,
              true,
              (targetId, effectiveForce) => removeCredential.mutateAsync({ id: targetId, force: effectiveForce }),
              credentialNameOf,
            );
          }
        }}
        onCancel={() => cancelFlow(setCredentialFlow)}
      />

      {/* 与「添加服务商」同一个两步向导：这里只是以编辑模式打开（功能一模一样） */}
      <ProviderTwoStepWizard
        open={editOpen}
        existing={{ ids: providerIds, names: providerNames }}
        onClose={closeEdit}
      />
    </>
  );
}

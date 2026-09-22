/**
 * 路由策略页 `/policies`（[07 §5]、[02 §2.8]）—— **只读的策略列表**。
 *
 * 三条纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **只用已注册的命令**：整页取数只发 `policy_list`。
 *    写命令（`policy_upsert` / `policy_delete` / `policy_reorder` / `policy_set_enabled` /
 *    `policy_simulate`）**已在后端登记**（以 `src-tauri/src/commands/mod.rs` 的 `collect_commands!`
 *    为准，`src/app/commandSurface.guard.test.ts` 机械检查），**但本页的写入入口尚未接入** ——
 *    后续一批接上增删改与模拟器；在那之前**不放按钮，也不放点了没反应的禁用占位**，
 *    只在页头下方一句话说清现状。
 * 2. **顺序是后端的，不是我们排的**：`policy_list` 自己按 `priority` 升序排
 *    （`src-tauri/src/commands/catalog.rs:128-135`，注释明写"UI 的拖拽顺序就是求值顺序"）。
 *    页面照它返回的顺序画，**另外单列原始 `priority` 数值** —— 不让"第几行"冒充优先级。
 * 3. **不认识的东西不猜**：通道组 / 线路 / 通道只有配置里的 id（本页没有 `group_list` /
 *    `lane_list` 的授权），就原样显示 id 并说明；选择器、故障转移触发器等枚举值
 *    只翻译已知取值，**契约外的取值原样显示**（那是后端说的话）。
 *
 * 数据流：`useCommandQuery("policy_list", undefined, qk.policies)` → `readPolicyList()` → `<PoliciesBody>`
 */

import {
  Box,
  Button,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Tooltip,
  Typography,
} from "@mui/material";
import type { ReactElement, ReactNode } from "react";

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader, StatusDot } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 已知选择器 → i18n；表里没有的取值**原样显示**（开放集合，不丢弃也不猜）。 */
const SELECTOR_KEY: Record<string, I18nKey> = {
  priority: "policies.selector.priority",
  sticky_then_priority: "policies.selector.sticky_then_priority",
  quota_first: "policies.selector.quota_first",
  cheapest: "policies.selector.cheapest",
  lowest_latency: "policies.selector.lowest_latency",
  weighted_random: "policies.selector.weighted_random",
  round_robin: "policies.selector.round_robin",
};

const NO_CANDIDATE_KEY: Record<string, I18nKey> = {
  error: "policies.onNoCandidate.error",
  use_any: "policies.onNoCandidate.use_any",
};

const BACKOFF_KEY: Record<string, I18nKey> = {
  none: "policies.backoff.none",
  exponential: "policies.backoff.exponential",
};

const TRIGGER_KEY: Record<string, I18nKey> = {
  http429: "policies.trigger.http429",
  http402: "policies.trigger.http402",
  http5xx: "policies.trigger.http5xx",
  timeout: "policies.trigger.timeout",
  connect_error: "policies.trigger.connect_error",
  quota_exhausted: "policies.trigger.quota_exhausted",
  channel_tagged_out: "policies.trigger.channel_tagged_out",
  capability_mismatch: "policies.trigger.capability_mismatch",
};

// ================================================================ 读后端数据（不猜、不补默认值）

/** 一条命中条件。渲染时才翻译，读出阶段不做任何文案拼接。 */
export type PolicyConditionView =
  | { kind: "models"; values: string[] }
  | { kind: "hasImage"; value: boolean }
  | { kind: "header"; name: string; value: string }
  | { kind: "ua"; value: string }
  | { kind: "tagsAny"; values: string[] }
  | { kind: "tagsAll"; values: string[] }
  | { kind: "inputTokensLt"; value: number }
  | { kind: "inputTokensGt"; value: number }
  | { kind: "stream"; value: boolean }
  | { kind: "quotaRemainingLt"; value: number }
  | { kind: "timeWindow"; value: string }
  | { kind: "providersIn"; values: string[] };

export type PolicyFailoverView = {
  on: string[];
  maxAttempts: number | undefined;
  cooldownS: number | undefined;
  backoff: string | undefined;
};

export type PolicyActionView = {
  /** 通道组 id（原样；本页不解析名称）。 */
  group: string | null;
  /** 一键全切线路的 lane id（原样）。 */
  lane: string | null;
  channels: string[];
  excludeChannels: string[];
  requireTag: string | null;
  excludeTag: string | null;
  /** 覆盖的选择器；`null` = 本策略没覆盖（继承）。 */
  selector: string | null;
  pinSticky: string | null;
  onNoCandidate: string | null;
  errorCode: number | null;
  failover: PolicyFailoverView | null;
};

export type PolicyView = {
  id: string;
  name: string;
  enabled: boolean;
  priority: number;
  /** `all: true` = 恒真兜底规则（`routing/engine.rs:380` 的注释）。 */
  alwaysMatch: boolean;
  conditions: PolicyConditionView[];
  action: PolicyActionView;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

function str(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function bool(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

/** 字符串列表：只保留真的字符串，**不把 undefined 变成 `""`**（那会显示成一个空条件）。 */
function strList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

/** 把 `PolicyMatch` 摊成一组"条件"；读不到的字段直接不产生条件（不猜、不补默认）。 */
export function readConditions(match: Record<string, unknown>): PolicyConditionView[] {
  const out: PolicyConditionView[] = [];

  const models = strList(match["model_matches"]);
  if (models.length > 0) out.push({ kind: "models", values: models });

  const hasImage = bool(match["has_image"]);
  if (hasImage !== null) out.push({ kind: "hasImage", value: hasImage });

  const headers = match["header_matches"];
  if (isRecord(headers)) {
    for (const [name, value] of Object.entries(headers)) {
      if (typeof value === "string") out.push({ kind: "header", name, value });
    }
  }

  const ua = str(match["ua_matches"]);
  if (ua !== null) out.push({ kind: "ua", value: ua });

  const tagsAny = strList(match["channel_tags_any"]);
  if (tagsAny.length > 0) out.push({ kind: "tagsAny", values: tagsAny });

  const tagsAll = strList(match["channel_tags_all"]);
  if (tagsAll.length > 0) out.push({ kind: "tagsAll", values: tagsAll });

  const lt = num(match["input_tokens_lt"]);
  if (lt !== null) out.push({ kind: "inputTokensLt", value: lt });

  const gt = num(match["input_tokens_gt"]);
  if (gt !== null) out.push({ kind: "inputTokensGt", value: gt });

  const stream = bool(match["stream"]);
  if (stream !== null) out.push({ kind: "stream", value: stream });

  const quotaLt = num(match["quota_remaining_lt"]);
  if (quotaLt !== null) out.push({ kind: "quotaRemainingLt", value: quotaLt });

  const window = str(match["time_window"]);
  if (window !== null) out.push({ kind: "timeWindow", value: window });

  const providers = strList(match["provider_in"]);
  if (providers.length > 0) out.push({ kind: "providersIn", values: providers });

  return out;
}

function readAction(action: Record<string, unknown>): PolicyActionView {
  const failover = isRecord(action["failover"]) ? action["failover"] : undefined;
  return {
    group: str(action["use_group"]),
    lane: str(action["use_lane"]),
    channels: strList(action["use_channels"]),
    excludeChannels: strList(action["exclude_channels"]),
    requireTag: str(action["require_tag"]),
    excludeTag: str(action["exclude_tag"]),
    selector: str(action["selector"]),
    pinSticky: str(action["pin_sticky"]),
    onNoCandidate: str(action["on_no_candidate"]),
    errorCode: num(action["error_code"]),
    failover:
      failover === undefined
        ? null
        : {
            on: strList(failover["on"]),
            maxAttempts: num(failover["max_attempts"]) ?? undefined,
            cooldownS: num(failover["cooldown_s"]) ?? undefined,
            backoff: str(failover["backoff"]) ?? undefined,
          },
  };
}

/**
 * `policy_list` → 视图模型。**非数组、非对象一律忽略**（不抛错、也不编出空行）。
 * 缺字段留空（`null` / `[]`），不填"看起来合理"的默认值。
 */
export function readPolicyList(raw: unknown): PolicyView[] {
  if (!Array.isArray(raw)) return [];
  const out: PolicyView[] = [];
  for (const item of raw) {
    if (!isRecord(item)) continue;
    const match = isRecord(item["match"]) ? item["match"] : {};
    const action = isRecord(item["action"]) ? item["action"] : {};
    out.push({
      id: str(item["id"]) ?? "",
      name: str(item["name"]) ?? "",
      enabled: item["enabled"] !== false,
      priority: num(item["priority"]) ?? 0,
      alwaysMatch: match["all"] === true,
      conditions: readConditions(match),
      action: readAction(action),
    });
  }
  return out;
}

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/** 一个枚举值：已知取值翻译，未知取值**原样显示**。 */
function enumText(
  raw: string | null,
  table: Record<string, I18nKey>,
  t: (key: I18nKey) => string,
): string | null {
  if (raw === null) return null;
  const key = table[raw];
  return key === undefined ? raw : t(key);
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据"）

export type PoliciesBodyProps = {
  /** `undefined` = 还没有数据（加载中或出错）。 */
  policies: PolicyView[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
};

export function PoliciesBody({ policies, isLoading, error, onRetry }: PoliciesBodyProps) {
  const { t } = useTranslation();

  const rows = policies ?? [];
  const enabledCount = rows.filter((row) => row.enabled).length;
  const failed = error !== undefined && error !== null;

  /** 一条条件的文案。`values` 是后端给的原样值，只做连接、不做改写。 */
  const conditionText = (condition: PolicyConditionView): string => {
    switch (condition.kind) {
      case "models":
        return t("policies.match.models", { values: condition.values.join(" · ") });
      case "hasImage":
        return t(condition.value ? "policies.match.hasImage.yes" : "policies.match.hasImage.no");
      case "header":
        return t("policies.match.header", { name: condition.name, value: condition.value });
      case "ua":
        return t("policies.match.ua", { value: condition.value });
      case "tagsAny":
        return t("policies.match.tagsAny", { values: condition.values.join(" · ") });
      case "tagsAll":
        return t("policies.match.tagsAll", { values: condition.values.join(" · ") });
      case "inputTokensLt":
        return t("policies.match.inputTokensLt", { value: formatInt(condition.value) });
      case "inputTokensGt":
        return t("policies.match.inputTokensGt", { value: formatInt(condition.value) });
      case "stream":
        return t(condition.value ? "policies.match.stream.yes" : "policies.match.stream.no");
      case "quotaRemainingLt":
        return t("policies.match.quotaRemainingLt", { value: String(condition.value) });
      case "timeWindow":
        return t("policies.match.timeWindow", { value: condition.value });
      case "providersIn":
        return t("policies.match.providersIn", { values: condition.values.join(" · ") });
    }
  };

  /** 动作摘要：每行一件事，缺的不编。 */
  const actionLines = (action: PolicyActionView): string[] => {
    const lines: string[] = [];
    if (action.channels.length > 0) {
      lines.push(t("policies.action.channels", { count: action.channels.length }));
    }
    if (action.excludeChannels.length > 0) {
      lines.push(
        t("policies.action.excludeChannels", { count: action.excludeChannels.length }),
      );
    }
    if (action.requireTag !== null) {
      lines.push(t("policies.action.requireTag", { value: action.requireTag }));
    }
    if (action.excludeTag !== null) {
      lines.push(t("policies.action.excludeTag", { value: action.excludeTag }));
    }
    if (action.pinSticky !== null) {
      lines.push(t("policies.action.pinSticky", { value: action.pinSticky }));
    }
    if (action.errorCode !== null) {
      lines.push(t("policies.action.errorCode", { value: String(action.errorCode) }));
    }
    const onNoCandidate = enumText(action.onNoCandidate, NO_CANDIDATE_KEY, t);
    if (onNoCandidate !== null) {
      lines.push(t("policies.action.onNoCandidate", { value: onNoCandidate }));
    }
    if (action.failover === null) {
      lines.push(t("policies.action.failoverNone"));
    } else {
      const on = action.failover.on
        .map((trigger) => enumText(trigger, TRIGGER_KEY, t) ?? trigger)
        .join(" · ");
      lines.push(
        t("policies.action.failover", {
          on: on === "" ? DASH : on,
          attempts: formatInt(action.failover.maxAttempts ?? 0),
          cooldown: formatInt(action.failover.cooldownS ?? 0),
          backoff: enumText(action.failover.backoff ?? null, BACKOFF_KEY, t) ?? DASH,
        }),
      );
    }
    if (lines.length === 0) lines.push(t("policies.action.none"));
    return lines;
  };

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {/* ① 这一页是什么、为什么是只读的 */}
      <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }}>
        <Stack spacing={2}>
          <Typography variant="h4">{t("policies.section.list")}</Typography>
          <Caption>{t("policies.orderNote")}</Caption>
          {/* 说明为什么本页没有排序 / 启停 / 模拟入口：写命令已在后端登记，但界面接入在后续一批 */}
          <Box data-testid="policies-readonly">
            <Caption>{t("policies.readonly")}</Caption>
          </Box>
          <Box data-testid="policies-count">
            <Caption>
              {t("policies.count", {
                total: formatInt(rows.length),
                enabled: formatInt(enabledCount),
              })}
            </Caption>
          </Box>
        </Stack>
      </Paper>

      {/* ② 列表 */}
      <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }}>
        {failed ? (
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            {/* `errorMessage()` 自带 `[TA-xxxx]`（唯一格式化出口），不吞错误码 */}
            <Typography variant="body1" data-testid="policies-error" sx={{ color: "error.main" }}>
              {`${t("error.title")} · ${errorMessage(error)}`}
            </Typography>
            <Button variant="outlined" onClick={onRetry}>
              {t("common.retry")}
            </Button>
          </Stack>
        ) : isLoading && rows.length === 0 ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {t("common.loading")}
          </Typography>
        ) : rows.length === 0 ? (
          /* 空态：给出"没有策略意味着什么"，而不是一张 0 行的表 */
          <Stack spacing={1} data-testid="policies-empty">
            <Typography variant="body1">{t("policies.empty.title")}</Typography>
            <Caption>{t("policies.empty.body")}</Caption>
            <Caption>{t("policies.empty.hint")}</Caption>
          </Stack>
        ) : (
          <Box sx={{ overflowX: "auto" }}>
            <Table size="small" data-testid="policies-table">
              <TableHead>
                <TableRow>
                  <TableCell align="right">{t("policies.col.order")}</TableCell>
                  <TableCell>{t("policies.col.name")}</TableCell>
                  <TableCell>{t("policies.col.enabled")}</TableCell>
                  <TableCell align="right">
                    <Tooltip title={t("policies.priority.hint")}>
                      <Box component="span">{t("policies.col.priority")}</Box>
                    </Tooltip>
                  </TableCell>
                  <TableCell>{t("policies.col.match")}</TableCell>
                  <TableCell>{t("policies.col.action")}</TableCell>
                  <TableCell>{t("policies.col.selector")}</TableCell>
                  <TableCell>{t("policies.col.lane")}</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {rows.map((policy, index) => {
                  const selector = enumText(policy.action.selector, SELECTOR_KEY, t);
                  const lines = actionLines(policy.action);
                  return (
                    <TableRow key={policy.id === "" ? `${policy.name}-${index}` : policy.id} data-testid="policies-row">
                      {/* 顺序 = 后端返回的次序；优先级是配置里的原始数值（两者分开显示） */}
                      <TableCell
                        align="right"
                        data-testid={`policies-order-${index}`}
                        sx={{ fontVariantNumeric: "tabular-nums" }}
                      >
                        {formatInt(index + 1)}
                      </TableCell>
                      <TableCell sx={{ minWidth: 200 }}>
                        <Typography variant="body1">{policy.name === "" ? DASH : policy.name}</Typography>
                        {policy.id !== "" ? (
                          <Typography
                            variant="caption"
                            sx={{ color: "text.secondary", fontFamily: typography.fontMono }}
                          >
                            {policy.id}
                          </Typography>
                        ) : null}
                      </TableCell>
                      <TableCell>
                        <StatusDot
                          status={policy.enabled ? "ok" : "off"}
                          label={policy.enabled ? t("common.enabled") : t("common.disabled")}
                        />
                      </TableCell>
                      <TableCell
                        align="right"
                        data-testid={`policies-priority-${index}`}
                        sx={{ fontVariantNumeric: "tabular-nums" }}
                      >
                        {formatInt(policy.priority)}
                      </TableCell>
                      <TableCell sx={{ minWidth: 240 }}>
                        <Stack spacing={1} data-testid={`policies-match-${index}`}>
                          {policy.conditions.length === 0 ? (
                            <Typography variant="body1">
                              {t("policies.match.unconstrained")}
                            </Typography>
                          ) : (
                            policy.conditions.map((condition, i) => (
                              <Typography
                                key={`${condition.kind}-${i}`}
                                variant="body1"
                                sx={{ overflowWrap: "anywhere" }}
                              >
                                {conditionText(condition)}
                              </Typography>
                            ))
                          )}
                          {/* `all` 是恒真项：它不约束任何东西（`routing/engine.rs:380` 的注释），
                              所以它单独一行，**不覆盖**上面真正生效的条件。 */}
                          {policy.alwaysMatch ? (
                            <Typography variant="body1">{t("policies.match.always")}</Typography>
                          ) : null}
                          <Caption>{t("policies.match.andHint")}</Caption>
                        </Stack>
                      </TableCell>
                      <TableCell sx={{ minWidth: 260 }}>
                        <Stack spacing={1} data-testid={`policies-action-${index}`}>
                          <Typography
                            variant="body1"
                            sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                          >
                            {t("policies.action.group", {
                              value: policy.action.group ?? t("common.none"),
                            })}
                          </Typography>
                          {lines.map((line) => (
                            <Typography
                              key={line}
                              variant="caption"
                              sx={{ color: "text.secondary", overflowWrap: "anywhere" }}
                            >
                              {line}
                            </Typography>
                          ))}
                        </Stack>
                      </TableCell>
                      <TableCell
                        data-testid={`policies-selector-${index}`}
                        sx={{ minWidth: 160, overflowWrap: "anywhere" }}
                      >
                        {selector === null ? t("policies.action.selectorInherit") : selector}
                      </TableCell>
                      <TableCell
                        data-testid={`policies-lane-${index}`}
                        sx={{
                          minWidth: 120,
                          fontFamily: typography.fontMono,
                          overflowWrap: "anywhere",
                        }}
                      >
                        {policy.action.lane === null
                          ? t("policies.action.laneNone")
                          : policy.action.lane}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
            {/* id 不解析成名称的原因写在表下：诚实，也免得用户以为是 bug */}
            <Box sx={{ mt: 3 }} data-testid="policies-raw-ids">
              <Caption>{t("policies.rawIds")}</Caption>
            </Box>
          </Box>
        )}
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function PoliciesPage(): ReactElement {
  const { t } = useTranslation();
  const policies = useCommandQuery("policy_list", undefined, qk.policies);

  return (
    <>
      <PageHeader
        title={t("nav.policies")}
        description={t("policies.description")}
        docRef="07 §5 路由策略 · 02 §2.8 policy.match / policy.action"
        actions={
          <Button variant="outlined" onClick={() => void policies.refetch()}>
            {t("policies.refresh")}
          </Button>
        }
      />
      <PoliciesBody
        policies={policies.data === undefined ? undefined : readPolicyList(policies.data)}
        isLoading={policies.isPending}
        error={policies.error ?? undefined}
        onRetry={() => void policies.refetch()}
      />
    </>
  );
}

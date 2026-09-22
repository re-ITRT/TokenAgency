/**
 * 路由策略页 `/policies`（[07 §5]、[02 §2.8]）—— **只读的策略列表**。
 *
 * 三条纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **只用已注册的命令**：整页取数只发 `policy_list`；`policies-add-open` 打开的添加向导
 *    另发 `model_list` / `channel_list`（只在向导打开时才查），提交走 `policy_upsert`。
 *    其余写命令（`policy_reorder` / `policy_set_enabled` / `policy_simulate`）**后端已登记但本页没有入口** ——
 *    **不放按钮，也不放点了没反应的禁用占位**（[docs/17 §3.4] 只要求"添加路由"这一条链路）。
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
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Divider,
  FormControlLabel,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState, type ReactElement, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery, TokenAgencyError } from "@/shared/api";
import type {
  Backoff,
  Channel,
  ChannelId,
  FailoverTrigger,
  Policy,
  Selector,
} from "@/shared/bindings";
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

// ================================================================ 能力一致性（[docs/17 §2]）

/**
 * 上游模型的能力声明（[docs/17 §2]）。
 *
 * **为什么这里自己写一份**：真正的契约字段（`Channel.capabilities`）由 task-31 落地。
 * 在它出现之前，比对逻辑按 [docs/17 §2] 的形状写成**纯函数**并配测试；契约到了只换类型来源，
 * 判断与文案不动。读取口 [`readCapabilities`] 对"没有 / 形状不对"一律返回 `null`
 * —— **绝不编默认值**（"没声明"与"声明为 false"是两件事）。
 */
export type ChannelCapabilities = {
  vision: boolean;
  tools: boolean;
  reasoning: boolean;
  context_tokens: number;
  max_output_tokens: number | null;
  modalities: string[];
};

const CAPABILITY_FIELDS: (keyof ChannelCapabilities)[] = [
  "vision",
  "tools",
  "reasoning",
  "context_tokens",
  "max_output_tokens",
  "modalities",
];

/** 一个字段上的不一致：这个字段在各条线路上的值（原样，不翻译成"好/坏"）。 */
export type CapabilityFieldDiff = {
  field: keyof ChannelCapabilities;
  values: { channelId: string; text: string }[];
};

export type CapabilityInput = { channelId: string; capabilities: ChannelCapabilities | null };

export type CapabilityComparison = {
  /** 至少两条线路且都声明了能力 → 才是真的比过了。 */
  compared: boolean;
  /** 读不到能力声明的线路 id（**不能**当成"一致"）。 */
  unknown: string[];
  /** 逐字段差异；空 = 一致。 */
  diffs: CapabilityFieldDiff[];
};

function readBoolStrict(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function readIntStrict(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) ? value : null;
}

/** 严格读取能力声明：任一必需字段缺失或类型不对 → `null`。 */
export function readCapabilities(raw: unknown): ChannelCapabilities | null {
  if (!isRecord(raw)) return null;
  const vision = readBoolStrict(raw["vision"]);
  const tools = readBoolStrict(raw["tools"]);
  const reasoning = readBoolStrict(raw["reasoning"]);
  const context = readIntStrict(raw["context_tokens"]);
  const modalitiesRaw = raw["modalities"];
  const modalities =
    Array.isArray(modalitiesRaw) && modalitiesRaw.every((m) => typeof m === "string")
      ? (modalitiesRaw as string[]).slice()
      : null;
  const maxRaw = raw["max_output_tokens"];
  const maxOut = maxRaw === null || maxRaw === undefined ? null : readIntStrict(maxRaw);
  if (
    vision === null ||
    tools === null ||
    reasoning === null ||
    context === null ||
    modalities === null ||
    (maxRaw !== null && maxRaw !== undefined && maxOut === null)
  ) {
    return null;
  }
  return { vision, tools, reasoning, context_tokens: context, max_output_tokens: maxOut, modalities };
}

/** `modalities` 比较前先排序去重（[docs/17 §2] 规则 2）。 */
export function normalizeModalities(list: string[]): string[] {
  return Array.from(new Set(list.map((m) => m.trim()).filter((m) => m !== ""))).sort();
}

function capabilityText(field: keyof ChannelCapabilities, caps: ChannelCapabilities): string {
  const value = caps[field];
  if (field === "modalities") return normalizeModalities(value as string[]).join(",");
  return value === null ? "null" : String(value);
}

/**
 * 逐字段比对（[docs/17 §2] 规则 2：一致 = 每个字段逐一相等，`modalities` 排序去重后比，
 * `null` 只与 `null` 相等）。**不足两条 / 有线路没声明能力时 `compared: false`** ——
 * 这两种情况都不许当成"一致"（那就成了猜）。
 */
export function compareCapabilities(entries: CapabilityInput[]): CapabilityComparison {
  if (entries.length < 2) return { compared: false, unknown: [], diffs: [] };
  const unknown = entries.filter((e) => e.capabilities === null).map((e) => e.channelId);
  if (unknown.length > 0) return { compared: false, unknown, diffs: [] };
  const diffs: CapabilityFieldDiff[] = [];
  for (const field of CAPABILITY_FIELDS) {
    const texts = entries.map((e) => capabilityText(field, e.capabilities as ChannelCapabilities));
    if (new Set(texts).size > 1) {
      diffs.push({
        field,
        values: entries.map((e, i) => ({ channelId: e.channelId, text: texts[i] ?? "" })),
      });
    }
  }
  return { compared: true, unknown: [], diffs };
}

// ================================================================ 添加路由向导（[docs/17 §3.4]）

const SELECTOR_VALUES: Selector[] = [
  "priority",
  "sticky_then_priority",
  "quota_first",
  "cheapest",
  "lowest_latency",
  "weighted_random",
  "round_robin",
];

const TRIGGER_VALUES: FailoverTrigger[] = [
  "http429",
  "http402",
  "http5xx",
  "timeout",
  "connect_error",
  "quota_exhausted",
  "channel_tagged_out",
  "capability_mismatch",
];

const BACKOFF_VALUES: Backoff[] = ["none", "exponential"];

/** 非负整数：空串 → `null`（**不当成 0**）。 */
function parseNonNegativeInt(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "" || !/^\d+$/.test(trimmed)) return null;
  return Number(trimmed);
}

function errorDetailsText(err: unknown): string | null {
  if (!(err instanceof TokenAgencyError)) return null;
  const details = err.details;
  if (details === undefined || details === null) return null;
  return typeof details === "string" ? details : JSON.stringify(details, null, 2);
}

export type PoliciesAddDialogProps = {
  open: boolean;
  onClose: () => void;
};

/**
 * 「添加路由」向导：暴露哪个模型 → 选哪些线路 → 粘性 → 失败切换 → 优先级 → 保存。
 *
 * 两个"只暴露能表达的"决定（[docs/17 §3.4] + 主管纪律"不许发明契约里没有的开关"）：
 * 1. **粘性**：策略里能表达的只有 `action.pin_sticky`（钉住优先线路）与 `action.selector`
 *    （`sticky_then_priority` 等真实取值）。**真正的粘性窗口与派生键（`ttl_h` / `key`）挂在
 *    `ChannelGroup.sticky` 上，`Policy` 没有对应字段** —— 所以这里不造那两个输入框，
 *    只在控件下如实说明（缺的写进回报）。
 * 2. **失败切换**：只写 `FailoverPolicy` 真实存在的四个字段（`on` / `max_attempts` /
 *    `cooldown_s` / `backoff`）；不选任何触发条件 = 不写 `failover`（继承全局默认，
 *    不是"填了一堆我们编的数字"）。
 */
export function PoliciesAddDialog({ open, onClose }: PoliciesAddDialogProps): ReactElement {
  const { t } = useTranslation();
  const qc = useQueryClient();

  // 只在向导打开时查：页面本身仍然只发 policy_list（[docs/17 §3.4]）
  const models = useCommandQuery("model_list", undefined, qk.models, { enabled: open });
  const channels = useCommandQuery("channel_list", undefined, qk.channels, { enabled: open });
  const upsert = useCommandMutation("policy_upsert");

  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [model, setModel] = useState("");
  const [selected, setSelected] = useState<ChannelId[]>([]);
  const [pinSticky, setPinSticky] = useState("");
  const [selector, setSelector] = useState("");
  const [failoverOn, setFailoverOn] = useState<FailoverTrigger[]>([]);
  const [maxAttempts, setMaxAttempts] = useState("3");
  const [cooldownS, setCooldownS] = useState("120");
  const [backoff, setBackoff] = useState<Backoff>("exponential");
  const [priority, setPriority] = useState("1000");
  const [saved, setSaved] = useState<Policy | null>(null);

  const modelOptions = useMemo(
    () => (models.data ?? []).map((view) => view.model.id),
    [models.data],
  );
  const allChannels = useMemo<Channel[]>(() => channels.data ?? [], [channels.data]);
  /** **只列 `model` 等于所选模型的线路**（[docs/17 §3.4]）。 */
  const candidates = useMemo(
    () => allChannels.filter((channel) => channel.model === model),
    [allChannels, model],
  );

  const comparison = useMemo(
    () =>
      compareCapabilities(
        selected.map((channelId) => {
          const channel = allChannels.find((c) => c.id === channelId);
          // `Channel.capabilities` 由 task-31 落契约；在它出现前从原始对象上读，读不到就是 null
          const raw = channel as unknown as Record<string, unknown> | undefined;
          return { channelId, capabilities: readCapabilities(raw?.["capabilities"]) };
        }),
      ),
    [selected, allChannels],
  );

  const hasMismatch = comparison.compared && comparison.diffs.length > 0;
  const priorityValue = parseNonNegativeInt(priority);
  const attemptsValue = parseNonNegativeInt(maxAttempts);
  const cooldownValue = parseNonNegativeInt(cooldownS);
  const failoverOnInvalid =
    failoverOn.length > 0 && (attemptsValue === null || cooldownValue === null);
  const canSubmit =
    id.trim() !== "" &&
    name.trim() !== "" &&
    model !== "" &&
    selected.length > 0 &&
    priorityValue !== null &&
    !failoverOnInvalid &&
    !hasMismatch &&
    !upsert.isPending;

  function toggleChannel(channelId: ChannelId, checked: boolean) {
    setSelected((prev) =>
      checked ? [...prev.filter((c) => c !== channelId), channelId] : prev.filter((c) => c !== channelId),
    );
  }

  function toggleTrigger(trigger: FailoverTrigger, checked: boolean) {
    setFailoverOn((prev) =>
      checked ? [...prev.filter((x) => x !== trigger), trigger] : prev.filter((x) => x !== trigger),
    );
  }

  function submit() {
    if (!canSubmit || priorityValue === null) return;
    const policy: Policy = {
      id: id.trim(),
      name: name.trim(),
      enabled: true,
      priority: priorityValue,
      match: {
        all: false,
        model_matches: [model],
        has_image: null,
        header_matches: {},
        ua_matches: null,
        channel_tags_any: [],
        channel_tags_all: [],
        input_tokens_lt: null,
        input_tokens_gt: null,
        stream: null,
        quota_remaining_lt: null,
        time_window: null,
        provider_in: [],
      },
      action: {
        use_group: null,
        use_channels: selected,
        exclude_channels: [],
        require_tag: null,
        exclude_tag: null,
        selector: selector === "" ? null : (selector as Selector),
        failover:
          failoverOn.length === 0
            ? null
            : {
                on: failoverOn,
                max_attempts: attemptsValue ?? 0,
                cooldown_s: cooldownValue ?? 0,
                backoff,
              },
        on_no_candidate: "error",
        error_code: null,
        pin_sticky: pinSticky === "" ? null : pinSticky,
        use_lane: null,
      },
    };
    upsert.mutate(
      { policy },
      {
        onSuccess: (stored) => {
          // 回显**后端返回的那一条**（不是我们本地拼的入参）
          setSaved(stored);
          void qc.invalidateQueries({ queryKey: qk.policies });
        },
      },
    );
  }

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="sm">
      {/* `data-testid` 不能直接挂在 MUI 组件上（TS 会报未知属性）→ 包一层普通元素 */}
      <Box data-testid="policies-add-dialog">
      <DialogTitle>{t("policies.add.title")}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField
            size="small"
            label={t("policies.add.id")}
            value={id}
            onChange={(e) => setId(e.target.value)}
            inputProps={{ "data-testid": "policies-add-id" }}
          />
          <TextField
            size="small"
            label={t("policies.add.name")}
            value={name}
            onChange={(e) => setName(e.target.value)}
            inputProps={{ "data-testid": "policies-add-name" }}
          />

          <TextField
            select
            size="small"
            SelectProps={{ native: true }}
            label={t("policies.add.model")}
            value={model}
            onChange={(e) => {
              setModel(e.target.value);
              // 换模型 → 之前选的线路不属于这个模型了，必须清掉（否则会提交错的路由）
              setSelected([]);
            }}
            inputProps={{ "data-testid": "policies-add-model" }}
          >
            <option value="">{t("policies.add.modelPlaceholder")}</option>
            {modelOptions.map((modelId) => (
              <option key={modelId} value={modelId}>
                {modelId}
              </option>
            ))}
          </TextField>

          <Box data-testid="policies-add-channels">
            <Typography variant="subtitle2">{t("policies.add.channels")}</Typography>
            {model === "" ? (
              <Caption>{t("policies.add.channelsNeedModel")}</Caption>
            ) : candidates.length === 0 ? (
              <Caption>{t("policies.add.channelsEmpty")}</Caption>
            ) : (
              candidates.map((channel) => (
                <Box key={channel.id} data-testid={`policies-add-channel-${channel.id}`}>
                  <FormControlLabel
                    control={
                      <Checkbox
                        size="small"
                        checked={selected.includes(channel.id)}
                        onChange={(e) => toggleChannel(channel.id, e.target.checked)}
                      />
                    }
                    label={`${channel.id} · ${channel.upstream_model}`}
                  />
                </Box>
              ))
            )}
          </Box>

          {/* 能力一致性：即时比对（[docs/17 §2] 规则 5） */}
          <Box data-testid="policies-add-capabilities">
            {comparison.compared && comparison.diffs.length === 0 && (
              <Caption>{t("policies.add.capabilities.consistent")}</Caption>
            )}
            {hasMismatch && (
              <Stack spacing={0.5} data-testid="policies-add-capability-mismatch">
                <Typography variant="body2" sx={{ color: "warning.main" }}>
                  {t("policies.add.capabilities.mismatch")}
                </Typography>
                {comparison.diffs.map((diff) => (
                  <Typography
                    key={diff.field}
                    variant="caption"
                    data-testid={`policies-add-capability-diff-${diff.field}`}
                  >
                    {/* 字段名就用契约里的名字（vision / tools / …）：这是接口字段，不翻译 */}
                    {`${diff.field}：${diff.values
                      .map((v) => `${v.channelId}=${v.text}`)
                      .join(" / ")}`}
                  </Typography>
                ))}
              </Stack>
            )}
            {!comparison.compared && comparison.unknown.length > 0 && (
              <Caption>
                {`${t("policies.add.capabilities.unknown")}：${comparison.unknown.join(", ")}`}
              </Caption>
            )}
          </Box>

          <Divider />
          <TextField
            select
            size="small"
            SelectProps={{ native: true }}
            label={t("policies.add.sticky")}
            value={pinSticky}
            onChange={(e) => setPinSticky(e.target.value)}
            inputProps={{ "data-testid": "policies-add-sticky" }}
          >
            <option value="">{t("policies.add.stickyNone")}</option>
            {selected.map((channelId) => (
              <option key={channelId} value={channelId}>
                {channelId}
              </option>
            ))}
          </TextField>
          <TextField
            select
            size="small"
            SelectProps={{ native: true }}
            label={t("policies.add.selector")}
            value={selector}
            onChange={(e) => setSelector(e.target.value)}
            inputProps={{ "data-testid": "policies-add-selector" }}
          >
            <option value="">{t("policies.add.selectorInherit")}</option>
            {SELECTOR_VALUES.map((value) => (
              <option key={value} value={value}>
                {t(SELECTOR_KEY[value] as I18nKey)}
              </option>
            ))}
          </TextField>
          <Caption>{t("policies.add.stickyNote")}</Caption>

          <Divider />
          <Box data-testid="policies-add-failover">
            <Typography variant="subtitle2">{t("policies.add.failover")}</Typography>
            <Caption>{t("policies.add.failoverNone")}</Caption>
            <Box>
              {TRIGGER_VALUES.map((trigger) => (
                <Box key={trigger} data-testid={`policies-add-failover-${trigger}`}>
                  <FormControlLabel
                    control={
                      <Checkbox
                        size="small"
                        checked={failoverOn.includes(trigger)}
                        onChange={(e) => toggleTrigger(trigger, e.target.checked)}
                      />
                    }
                    label={t(TRIGGER_KEY[trigger] as I18nKey)}
                  />
                </Box>
              ))}
            </Box>
            <Stack direction="row" spacing={2} sx={{ mt: 1 }}>
              <TextField
                size="small"
                type="number"
                label={t("policies.add.failoverMax")}
                value={maxAttempts}
                onChange={(e) => setMaxAttempts(e.target.value)}
                inputProps={{ "data-testid": "policies-add-failover-max" }}
              />
              <TextField
                size="small"
                type="number"
                label={t("policies.add.failoverCooldown")}
                value={cooldownS}
                onChange={(e) => setCooldownS(e.target.value)}
                inputProps={{ "data-testid": "policies-add-failover-cooldown" }}
              />
              <TextField
                select
                size="small"
                SelectProps={{ native: true }}
                label={t("policies.add.failoverBackoff")}
                value={backoff}
                onChange={(e) => setBackoff(e.target.value as Backoff)}
                inputProps={{ "data-testid": "policies-add-failover-backoff" }}
              >
                {BACKOFF_VALUES.map((value) => (
                  <option key={value} value={value}>
                    {t(BACKOFF_KEY[value] as I18nKey)}
                  </option>
                ))}
              </TextField>
            </Stack>
          </Box>

          <Divider />
          <TextField
            size="small"
            type="number"
            label={t("policies.add.priority")}
            value={priority}
            onChange={(e) => setPriority(e.target.value)}
            inputProps={{ "data-testid": "policies-add-priority" }}
          />

          {upsert.error !== null && (
            <Box data-testid="policies-add-error">
              {/* 后端说的话**原样**显示：[TA-xxxx] + message（不翻译、不改写） */}
              <Typography
                variant="body2"
                sx={{ color: "error.main" }}
                data-testid="policies-add-error-text"
              >
                {errorMessage(upsert.error)}
              </Typography>
              {errorDetailsText(upsert.error) !== null && (
                <Box
                  component="pre"
                  data-testid="policies-add-error-details"
                  sx={{ fontFamily: typography.fontMono, m: 0, whiteSpace: "pre-wrap" }}
                >
                  {errorDetailsText(upsert.error)}
                </Box>
              )}
            </Box>
          )}

          {saved !== null && (
            <Box data-testid="policies-add-result">
              <Typography variant="body2">
                {`${t("policies.add.saved", { id: saved.id })} · ${saved.name}`}
              </Typography>
            </Box>
          )}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} data-testid="policies-add-cancel">
          {t("policies.add.cancel")}
        </Button>
        <Button
          variant="contained"
          onClick={submit}
          disabled={!canSubmit}
          data-testid="policies-add-submit"
        >
          {t("policies.add.submit")}
        </Button>
      </DialogActions>
      </Box>
    </Dialog>
  );
}

// ================================================================ 数据层

export default function PoliciesPage(): ReactElement {
  const { t } = useTranslation();
  const policies = useCommandQuery("policy_list", undefined, qk.policies);
  const [addOpen, setAddOpen] = useState(false);

  return (
    <>
      <PageHeader
        title={t("nav.policies")}
        description={t("policies.description")}
        docRef="07 §5 路由策略 · 02 §2.8 policy.match / policy.action"
        actions={
          <Stack direction="row" spacing={1}>
            <Button
              variant="contained"
              onClick={() => setAddOpen(true)}
              data-testid="policies-add-open"
            >
              {t("policies.add.open")}
            </Button>
            <Button variant="outlined" onClick={() => void policies.refetch()}>
              {t("policies.refresh")}
            </Button>
          </Stack>
        }
      />
      <PoliciesBody
        policies={policies.data === undefined ? undefined : readPolicyList(policies.data)}
        isLoading={policies.isPending}
        error={policies.error ?? undefined}
        onRetry={() => void policies.refetch()}
      />
      <PoliciesAddDialog open={addOpen} onClose={() => setAddOpen(false)} />
    </>
  );
}

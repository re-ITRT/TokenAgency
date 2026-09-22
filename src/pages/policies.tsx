/**
 * 路由策略页 `/policies`（[07 §5]、[02 §2.8]）—— **只读的策略列表 + 删除（[docs/21 §1]）**。
 *
 * 三条纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **只用已注册的命令**：整页取数只发 `policy_list`；`policies-add-open` 打开的添加向导
 *    另发 `model_list` / `channel_list`（只在向导打开时才查），提交走 `policy_upsert`；
 *    删除走 `policy_delete`。其余写命令（`policy_reorder` / `policy_set_enabled` / `policy_simulate`）
 *    **后端已登记但本页没有入口** —— 不放按钮，也不放点了没反应的禁用占位（[docs/17 §3.4]）。
 * 2. **顺序是后端的，不是我们排的**：`policy_list` 自己按 `priority` 升序排
 *    （`src-tauri/src/commands/catalog.rs:128-135`，注释明写"UI 的拖拽顺序就是求值顺序"）。
 *    页面照它返回的顺序画，**另外单列原始 `priority` 数值** —— 不让"第几行"冒充优先级。
 * 3. **不认识的东西不猜**：通道组 / 线路 / 通道只有配置里的 id（本页没有 `group_list` /
 *    `lane_list` 的授权），就原样显示 id 并说明；选择器、故障转移触发器等枚举值
 *    只翻译已知取值，**契约外的取值原样显示**（那是后端说的话）。
 *
 * 删除的三条硬规则（[docs/21 §1]，单条与批量**走同一套语义**）：
 * - **先二次确认**（共享组件 `ConfirmDelete`），确认框里写清删的是哪一个（名称 + id）；
 * - **被引用时不许假装成功**：`policy_delete` **没有 force 参数** → `cascadeAvailable={false}`，
 *   只把后端原文（含 TA 码）摆出来，**不放**"强行删除"按钮（那会是假按钮）；
 * - **批量逐条汇报**：一条失败不中止其它条，逐条给标识 + 后端原文，绝不用一句
 *   "批量删除成功"盖住部分失败。
 *
 * 删除入口是**页面授权**才渲染（`PoliciesBody` 的 `deletion` 可选 prop）：`PoliciesBody` 本身
 * 仍是纯展示层，没有被授权时一行按钮都不画（正文里除了 [docs/22] 那个收走解释的问号圆圈，
 * 一个操作按钮都没有 —— 既有断言按这条口径钉住）。
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
  MenuItem,
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
import { useEffect, useMemo, useState, type ReactElement, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery, TokenAgencyError } from "@/shared/api";
import type {
  Backoff,
  Channel,
  ChannelCapabilities,
  ChannelId,
  FailoverTrigger,
  Policy,
  Selector,
} from "@/shared/bindings";
import { PageHeader, StatusDot, ConfirmDelete, HelpTip } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

/**
 * 把同一区块的几句解释合成**一条**提示正文（[docs/22 §3]：一个区块最多一个问号）。
 *
 * 分隔符用换行：`aria-label` 里保留断句，读屏不会把两句话连成一坨。
 * （同一个函数的规范实现是 `network/index.tsx` 的 `mergeHelpText`；这里**不跨页 import**
 * 另一个页面模块 —— 那会把整个 network 页拖进本页的依赖里。）
 */
function mergeHelpText(...parts: readonly (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined && part !== "").join("\n");
}

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

/**
 * 删除授权（[docs/21 §1]）：由页面传进来。
 *
 * **为什么是可选 prop 而不是写死在列表里**：`PoliciesBody` 是纯展示层，它的"只读"是被一条
 * 既有断言钉住的（有数据时正文里除了 [docs/22] 那个问号圆圈，一个操作按钮都没有）。
 * 删除是页面级授权，没被授权时这一层就一个删除控件都不画 —— 授权与展示因此不会互相污染。
 */
export type PoliciesDeletionProps = {
  /** 已勾选的策略 id。 */
  selected: readonly string[];
  /** 有删除在途：所有删除控件进入禁用态（**不显示假进度**）。 */
  pending: boolean;
  onToggle: (id: string, next: boolean) => void;
  onToggleAll: (next: boolean) => void;
  /** 点行内「删除」→ 由页面打开确认框（**这里不发命令**）。 */
  onDeleteOne: (policy: PolicyView) => void;
  /** 点「删除选中」→ 由页面打开确认框。 */
  onDeleteSelected: () => void;
};

export type PoliciesBodyProps = {
  /** `undefined` = 还没有数据（加载中或出错）。 */
  policies: PolicyView[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /** `undefined` = 没有删除授权 → 表格保持只读（多一列都不加）。 */
  deletion?: PoliciesDeletionProps;
};

export function PoliciesBody({ policies, isLoading, error, onRetry, deletion }: PoliciesBodyProps) {
  const { t } = useTranslation();

  const rows = policies ?? [];
  const enabledCount = rows.filter((row) => row.enabled).length;
  const failed = error !== undefined && error !== null;

  /** 能删的行：`policy_delete` 收 id，读不到 id 的行就不给可点的入口（不给假控件）。 */
  const selectableRows = rows.filter((row) => row.id !== "");
  const selectedIds = new Set(deletion?.selected ?? []);
  const selectedCount = selectableRows.filter((row) => selectedIds.has(row.id)).length;
  const allSelected = selectableRows.length > 0 && selectedCount === selectableRows.length;

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
      <Paper
        variant="outlined"
        sx={{ p: 4, borderRadius: `${radius.surface}px` }}
        data-testid="policies-list-section"
      >
        <Stack spacing={2}>
          {/*
            「顺序 = 后端返回的顺序」与「多个条件 AND、一个条件里的多个值 OR」都是**解释**
            （[docs/22 §2] 的区块说明 / 成段 prose）→ 合成**一个**问号放在标题右边（§3：
            一个区块最多一个）。命中条件那一列因此不再每一行都挂一句灰字 —— 一排问号比一排字更糟。
            留在这里的 `policies-readonly` 是**结论**（哪些命令还没接入），一个字都不收（§2 第 1 条）。
          */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
            <Typography variant="h4">{t("policies.section.list")}</Typography>
            <HelpTip
              text={mergeHelpText(t("policies.orderNote"), t("policies.match.andHint"))}
              testId="policies-list-section-help"
            />
          </Box>
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
            {/* 多选与「删除选中」：只在有删除授权时出现；一条都没勾选时按钮不可用
                （勾上就能用 —— 这不是禁用占位） */}
            {deletion !== undefined ? (
              <Stack
                direction="row"
                spacing={2}
                alignItems="center"
                sx={{ mb: 3, flexWrap: "wrap" }}
              >
                <Box data-testid="policies-selected-count">
                  <Caption>{t("delete.selectedCount", { count: selectedCount })}</Caption>
                </Box>
                <Button
                  variant="outlined"
                  color="error"
                  data-testid="policies-delete-selected"
                  disabled={deletion.pending || selectedCount === 0}
                  onClick={deletion.onDeleteSelected}
                >
                  {t("common.deleteSelected")}
                </Button>
              </Stack>
            ) : null}
            <Table size="small" data-testid="policies-table">
              <TableHead>
                <TableRow>
                  {deletion !== undefined ? (
                    <TableCell padding="checkbox">
                      <Box data-testid="policies-select-all">
                        <Checkbox
                          size="small"
                          checked={allSelected}
                          indeterminate={selectedCount > 0 && !allSelected}
                          onChange={(event) => deletion.onToggleAll(event.target.checked)}
                        />
                      </Box>
                    </TableCell>
                  ) : null}
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
                  {deletion !== undefined ? <TableCell>{t("common.delete")}</TableCell> : null}
                </TableRow>
              </TableHead>
              <TableBody>
                {rows.map((policy, index) => {
                  const selector = enumText(policy.action.selector, SELECTOR_KEY, t);
                  const lines = actionLines(policy.action);
                  return (
                    <TableRow key={policy.id === "" ? `${policy.name}-${index}` : policy.id} data-testid="policies-row">
                      {deletion !== undefined ? (
                        <TableCell padding="checkbox">
                          {policy.id === "" ? (
                            /* 读不到 id 就没法指名要删哪一条（`policy_delete` 收 id）→ 画 —，不画假入口 */
                            <Typography variant="body2" sx={{ color: "text.secondary" }}>
                              {DASH}
                            </Typography>
                          ) : (
                            <Box data-testid="policies-select">
                              <Checkbox
                                size="small"
                                checked={selectedIds.has(policy.id)}
                                onChange={(event) =>
                                  deletion.onToggle(policy.id, event.target.checked)
                                }
                              />
                            </Box>
                          )}
                        </TableCell>
                      ) : null}
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
                        {/* 这一格里放的全是**这一行的事实**（列出来的条件 / 恒真 / 不约束）——
                            它们是状态与数值，留在版面上（[docs/22 §2] 第 1/5 条）；
                            「多个条件 AND、一个条件里的多个值 OR」那句**规则说明**收进了这一块标题右边的问号。 */}
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
                      {deletion !== undefined ? (
                        <TableCell>
                          {policy.id === "" ? (
                            <Typography variant="body2" sx={{ color: "text.secondary" }}>
                              {DASH}
                            </Typography>
                          ) : (
                            <Button
                              size="small"
                              color="error"
                              data-testid="policies-delete"
                              disabled={deletion.pending}
                              onClick={() => deletion.onDeleteOne(policy)}
                            >
                              {t("common.delete")}
                            </Button>
                          )}
                        </TableCell>
                      ) : null}
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
 * 上游模型的能力声明 —— **用真契约类型**（`src/shared/bindings.ts` 的 `ChannelCapabilities`，
 * 对应 Rust `ta_core::domain::ChannelCapabilities`）。本文件不再自己写一份形状。
 *
 * 契约（[docs/17 §2] / `Channel.capabilities?`）：
 * - 读出来的对象**永远带全 6 个字段**；
 * - **整个键可省略 = 默认能力**（全 `false` / `context_tokens: 0` / `max_output_tokens: null` /
 *   `modalities: ["text"]`）—— 所以"没写"不是"不知道"，按契约默认值参与比对（见 [`readCapabilities`]）。
 */
const DEFAULT_CAPABILITIES: ChannelCapabilities = {
  vision: false,
  tools: false,
  reasoning: false,
  context_tokens: 0,
  max_output_tokens: null,
  modalities: ["text"],
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

/**
 * 读一条线路的能力声明：
 * - **键不在**（`undefined` / `null`）→ 契约默认能力（[`DEFAULT_CAPABILITIES`]）。
 *   契约明写"省略 = 默认能力"，所以这不是"编默认值"，而是**按契约读**；这也让
 *   "省略"与"显式写默认值"被视为**一致**（它们本来就是同一组能力）。
 * - **形状不对**（字段类型错）→ `null`：那是"读不出来"，不能冒充默认，也不能当成"一致"。
 */
export function readCapabilities(raw: unknown): ChannelCapabilities | null {
  if (raw === undefined || raw === null) return { ...DEFAULT_CAPABILITIES, modalities: ["text"] };
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

/** `deepseek-v4.1flash` → `deepseek-v4-1flash`：只留 `[a-z0-9-]`，用于派生的路由 id。 */
function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

export type PoliciesAddDialogProps = {
  open: boolean;
  onClose: () => void;
};

/**
 * 下拉字段：`data-testid` 挂在**容器**上（与线路页 `SelectField` 同形状 —— 真机 E2E 先找容器内的
 * `[role=combobox]`，所以容器里必须真的有控件）。
 *
 * **必须用 MUI 的 `select`（不是原生 `<select>`）**：`wdPickOption` 的路径是
 * "点控件 → 等 `li[role="option"]` → 点选项"；原生 select 弹的是 OS 级下拉，
 * `li[role=option]` 永远不会出现 → 真机 E2E 选不上、后续全崩（这是实测踩到的）。
 *
 * 值为空时用 `renderValue` 显示 `emptyLabel`（**空就是真的空**，不放"请选择…"的假选项）。
 */
function WizardSelect({
  testId,
  label,
  value,
  onChange,
  options,
  emptyLabel,
  hint,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
  hint?: string;
}) {
  const labelOf = (id: string): string => options.find((o) => o.value === id)?.label ?? id;
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        label={label}
        value={value}
        onChange={(event) => onChange(String(event.target.value))}
        slotProps={{
          select: {
            displayEmpty: true,
            renderValue: (selected: unknown) =>
              String(selected) === "" ? emptyLabel : labelOf(String(selected)),
          },
        }}
      >
        {options.map((option) => (
          <MenuItem key={option.value} value={option.value}>
            {option.label}
          </MenuItem>
        ))}
      </TextField>
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

/** 多选下拉（线路多选）：同样是 MUI `select`，点开也是 `li[role="option"]` 弹层。 */
function WizardMultiSelect({
  testId,
  label,
  values,
  onChange,
  options,
  emptyLabel,
  hint,
}: {
  testId: string;
  label: string;
  values: string[];
  onChange: (values: string[]) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
  hint?: string;
}) {
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        label={label}
        value={values}
        onChange={(event) => {
          const next = event.target.value;
          onChange(Array.isArray(next) ? (next as string[]) : [String(next)]);
        }}
        slotProps={{
          select: {
            multiple: true,
            displayEmpty: true,
            renderValue: (selected: unknown) => {
              const picked = selected as string[];
              return picked.length === 0 ? emptyLabel : picked.join(", ");
            },
          },
        }}
      >
        {options.map((option) => (
          <MenuItem key={option.value} value={option.value}>
            {option.label}
          </MenuItem>
        ))}
      </TextField>
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

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
  /** 用户是否手改过 id / 名称：改过就不再被"选模型"覆盖（不然会悄悄吃掉用户的输入）。 */
  const [idTouched, setIdTouched] = useState(false);
  const [nameTouched, setNameTouched] = useState(false);
  const [model, setModel] = useState("");
  const [selected, setSelected] = useState<ChannelId[]>([]);
  /** 用户是否手改过线路多选：改过就不再被"默认全选该模型的线路"覆盖。 */
  const [selectedTouched, setSelectedTouched] = useState(false);
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

  /**
   * 选定模型后**默认指向它的全部线路**（用户原话就是"暴露 deepseek → 指向 1、2"），
   * 于是能力一致性在选完模型那一刻就可见；用户手动改过选择就不再覆盖。
   *
   * 用 effect 而不是写在 onChange 里：`channel_list` 可能比选模型晚到，写在 onChange 里
   * 会在"数据还没到"时全选成空集（表现为"选完模型一条线路都没选上"）。
   */
  useEffect(() => {
    if (selectedTouched) return;
    setSelected(candidates.map((channel) => channel.id));
  }, [candidates, selectedTouched]);

  const comparison = useMemo(
    () =>
      compareCapabilities(
        selected.map((channelId) => {
          const channel = allChannels.find((c) => c.id === channelId);
          // 真契约字段：`Channel.capabilities?`（省略 = 契约默认能力，见 `readCapabilities`）
          return { channelId, capabilities: readCapabilities(channel?.capabilities) };
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

  /**
   * 选定对外模型。三件事一起做，缺一个都会留下"看起来对、其实错"的状态：
   * 1. 默认指向该模型的全部线路（由上面的 effect 落地，用户可再取消）；
   * 2. id / 名称按模型**派生**（[docs/17 §3.4] 没给这两个字段，而 `Policy.id`/`name` 必填）——
   *    派生值**显示在界面上、可改**，所以不是"编一段用户看不见的数据"；用户改过就不再覆盖；
   * 3. 清掉"钉住线路"：换模型后原来那条可能不在候选里了。
   */
  function chooseModel(next: string) {
    setModel(next);
    setSelectedTouched(false);
    setPinSticky("");
    if (!idTouched) setId(next === "" ? "" : `pol-${slug(next)}`);
    if (!nameTouched) setName(next);
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
            onChange={(e) => {
              setIdTouched(true);
              setId(e.target.value);
            }}
            inputProps={{ "data-testid": "policies-add-id" }}
          />
          <TextField
            size="small"
            label={t("policies.add.name")}
            value={name}
            onChange={(e) => {
              setNameTouched(true);
              setName(e.target.value);
            }}
            inputProps={{ "data-testid": "policies-add-name" }}
          />

          <WizardSelect
            testId="policies-add-model"
            label={t("policies.add.model")}
            value={model}
            onChange={chooseModel}
            emptyLabel={t("policies.add.modelPlaceholder")}
            options={modelOptions.map((modelId) => ({ value: modelId, label: modelId }))}
          />

          {/* 多选线路：MUI 多选下拉（点开是 `li[role="option"]`，真机 E2E 的 `wdPickOption` 按它驱动）。
              没选模型 / 该模型没有线路时如实说明，不放空下拉、也不放禁用占位。 */}
          {model === "" || candidates.length === 0 ? (
            <Stack spacing={1} data-testid="policies-add-channels">
              <Typography variant="subtitle2">{t("policies.add.channels")}</Typography>
              <Caption>
                {model === "" ? t("policies.add.channelsNeedModel") : t("policies.add.channelsEmpty")}
              </Caption>
            </Stack>
          ) : (
            <WizardMultiSelect
              testId="policies-add-channels"
              label={t("policies.add.channels")}
              values={selected}
              onChange={(next) => {
                setSelectedTouched(true);
                setSelected(next);
              }}
              emptyLabel={t("policies.add.channelsNeedModel")}
              options={candidates.map((channel) => ({
                value: channel.id,
                label: `${channel.id} · ${channel.upstream_model}`,
              }))}
            />
          )}

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
          <WizardSelect
            testId="policies-add-sticky"
            label={t("policies.add.sticky")}
            value={pinSticky}
            onChange={setPinSticky}
            emptyLabel={t("policies.add.stickyNone")}
            options={selected.map((channelId) => ({ value: channelId, label: channelId }))}
          />
          <WizardSelect
            testId="policies-add-selector"
            label={t("policies.add.selector")}
            value={selector}
            onChange={setSelector}
            emptyLabel={t("policies.add.selectorInherit")}
            options={SELECTOR_VALUES.map((value) => ({
              value,
              label: t(SELECTOR_KEY[value] as I18nKey),
            }))}
          />
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
              <WizardSelect
                testId="policies-add-failover-backoff"
                label={t("policies.add.failoverBackoff")}
                value={backoff}
                onChange={(next) => setBackoff(next as Backoff)}
                emptyLabel={backoff}
                options={BACKOFF_VALUES.map((value) => ({
                  value,
                  label: t(BACKOFF_KEY[value] as I18nKey),
                }))}
              />
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

// ================================================================ 删除（[docs/21 §1]）

/** 一条删除的结果。**逐条如实汇报**：成功说名称，失败说标识 + 后端原文。 */
export type DeleteOutcome = {
  /** 该条的身份（策略 id）—— 失败时的"标识"就是它。 */
  key: string;
  /** 名称 + id（原样，不改写用户数据）。 */
  label: string;
  ok: boolean;
  /** 失败时的后端原文（含 TA 码）。 */
  error?: string;
};

/** 一次删除操作的汇报。`batch` = 批量（要额外给"成功 N 条 / 失败 M 条"的一行）。 */
export type DeleteReport = { batch: boolean; items: DeleteOutcome[] };

/** 名称 + id：确认框与结果区都用它写清"删的是哪一个"。 */
function policyLabel(policy: PolicyView): string {
  return policy.name === "" ? policy.id : `${policy.name}（${policy.id}）`;
}

/**
 * 删除结果区。**失败逐条给标识 + 后端原文**；批量再补一行"成功 N 条，失败 M 条" ——
 * 绝不用一句"批量删除成功"盖住部分失败。
 */
function DeleteReportBox({ report }: { report: DeleteReport }): ReactElement {
  const { t } = useTranslation();
  const failed = report.items.filter((item) => !item.ok);
  return (
    <Box sx={{ px: 8, pb: 8 }} data-testid="policies-delete-result">
      <Stack spacing={1}>
        {report.batch ? (
          <Typography variant="body1" data-testid="policies-delete-result-summary">
            {t("delete.result.summary", { ok: report.items.length - failed.length, failed: failed.length })}
          </Typography>
        ) : null}
        {report.items.map((item) => (
          <Typography
            key={item.key}
            variant="body1"
            data-testid={`policies-delete-result-${item.key}`}
            sx={{ color: item.ok ? "text.primary" : "error.main", overflowWrap: "anywhere" }}
          >
            {item.ok
              ? t("delete.result.ok", { name: item.label })
              : `${item.label} · ${t("delete.result.failed", { message: item.error ?? "" })}`}
          </Typography>
        ))}
      </Stack>
    </Box>
  );
}

// ================================================================ 数据层

export default function PoliciesPage(): ReactElement {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const policies = useCommandQuery("policy_list", undefined, qk.policies);
  const [addOpen, setAddOpen] = useState(false);

  const rows = useMemo(
    () => (policies.data === undefined ? undefined : readPolicyList(policies.data)),
    [policies.data],
  );

  // ---- 删除状态：单条与批量共用同一个确认组件、同一套失败展示
  const [selected, setSelected] = useState<string[]>([]);
  /** 正在确认的单条；`null` = 没有。 */
  const [target, setTarget] = useState<PolicyView | null>(null);
  /** 批量确认（与单条共用 `ConfirmDelete`：一次确认 → 逐条调用）。 */
  const [batchOpen, setBatchOpen] = useState(false);
  /** 后端原文（被引用时）。有它就展示；`policy_delete` 没有 force → 不给级联按钮。 */
  const [errorText, setErrorText] = useState<string | undefined>(undefined);
  const [report, setReport] = useState<DeleteReport | null>(null);
  const remove = useCommandMutation("policy_delete");

  const allRows = rows ?? [];
  /** 真正要删的行：读不到 id 的行不进删除流程。 */
  const targets = allRows.filter((row) => row.id !== "" && selected.includes(row.id));

  const onDeleteOne = async (policy: PolicyView): Promise<void> => {
    // 结果区只描述**最近一次**删除操作（上一次的结论不该混在这次里）
    setReport(null);
    try {
      await remove.mutateAsync({ id: policy.id });
      setReport({
        batch: false,
        items: [{ key: policy.id, label: policyLabel(policy), ok: true }],
      });
      setSelected((prev) => prev.filter((id) => id !== policy.id));
      setTarget(null);
      setErrorText(undefined);
      await qc.invalidateQueries({ queryKey: qk.policies });
    } catch (err) {
      // **不假装成功**：后端原文（含 TA 码）留在确认框里，对话框保持打开让用户看见
      setErrorText(errorMessage(err));
    }
  };

  const onDeleteSelected = async (): Promise<void> => {
    setReport(null);
    const items: DeleteOutcome[] = [];
    // 逐条调用：**一条失败不中止其它条**（[docs/21 §1.2]）
    for (const policy of targets) {
      try {
        await remove.mutateAsync({ id: policy.id });
        items.push({ key: policy.id, label: policyLabel(policy), ok: true });
      } catch (err) {
        items.push({
          key: policy.id,
          label: policyLabel(policy),
          ok: false,
          error: errorMessage(err),
        });
      }
    }
    setReport({ batch: true, items });
    setBatchOpen(false);
    setErrorText(undefined);
    // 成功的已经不在列表里了 → 只让**失败的那些**保持选中（它们还在，可以直接重试）
    setSelected(items.filter((item) => !item.ok).map((item) => item.key));
    await qc.invalidateQueries({ queryKey: qk.policies });
  };

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
        policies={rows}
        isLoading={policies.isPending}
        error={policies.error ?? undefined}
        onRetry={() => void policies.refetch()}
        deletion={{
          selected,
          pending: remove.isPending,
          onToggle: (id, next) =>
            setSelected((prev) => (next ? [...prev.filter((x) => x !== id), id] : prev.filter((x) => x !== id))),
          onToggleAll: (next) =>
            setSelected(next ? allRows.filter((row) => row.id !== "").map((row) => row.id) : []),
          // 单条与批量都**只打开确认框**：命令在确认之后才发
          onDeleteOne: (policy) => {
            setErrorText(undefined);
            setTarget(policy);
          },
          onDeleteSelected: () => {
            setErrorText(undefined);
            setBatchOpen(true);
          },
        }}
      />
      {report !== null ? <DeleteReportBox report={report} /> : null}
      <PoliciesAddDialog open={addOpen} onClose={() => setAddOpen(false)} />
      <ConfirmDelete
        open={target !== null || batchOpen}
        testIdPrefix="policies"
        title={t("delete.title.policy")}
        // 确认框里写清删的是哪一个：单条 = 名称 + id；批量 = 选中数 + 逐条标识
        targetLabel={
          target !== null
            ? policyLabel(target)
            : `${t("delete.selectedCount", { count: targets.length })} · ${targets.map(policyLabel).join("、")}`
        }
        cascadeAvailable={false}
        pending={remove.isPending}
        errorText={errorText}
        onConfirm={() => {
          if (target !== null) void onDeleteOne(target);
          else void onDeleteSelected();
        }}
        // `policy_delete` 没有 force 语义 → `cascadeAvailable={false}` 时这个回调**永远不会被触发**
        onConfirmCascade={() => {}}
        onCancel={() => {
          setTarget(null);
          setBatchOpen(false);
          setErrorText(undefined);
        }}
      />
    </>
  );
}

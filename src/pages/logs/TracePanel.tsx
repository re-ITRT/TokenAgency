/**
 * 请求轨迹面板（C4 的可视化）—— **纯展示**：入参是已经读好的 `TraceView`，
 * 它自己不发命令（取数在 `index.tsx` 的 `TraceSection`，这样面板能在测试里单独渲染）。
 *
 * 一条 request_id 的跨组件轨迹，按后端的呈现顺序画成三块：
 *
 * 1. **请求级字段**（模型/协议/通道/策略/耗时/TTFB/记录模式）—— 一次请求一行的那条记录；
 * 2. **尝试链**（`attempts`）—— 依次经过的通道与判定。这是 C4 的核心：
 *    "这次失败到底走了哪条 lane、第几次尝试才失败"（[14 §1.3]）。
 * 3. **原始字段** —— 后端返回的整条记录**原样**打印，保证"页面上看不到的字段也能看到"
 *    （诚实优先：[14] 的排查场景不能让 UI 替用户过滤掉证据）。
 *
 * 两条必须守住的诚实边界：
 * - **尝试链没有时间戳**。后端的 `TraceAttempt` 只有 `ms`（`ta-store trace_store.rs:46-58`），
 *   所以这里**不给**"每次尝试发生在几点"或"瀑布图"—— 那需要编造时间；
 *   只给每次尝试自己的耗时与一个如实标注"只算有记录的"合计。
 * - **契约与实现字段名不一致**（见 `model.ts` 文件头）。这里只画 `TraceView` 里
 *   已经归一化/并存的名字，缺了就不画，绝不显示成 0 或"正常"。
 */

import { Box, Button, Stack, Typography } from "@mui/material";
import type { ReactNode } from "react";

import { errorMessage } from "@/shared/api";
import { StatusDot, type SemStatus } from "@/shared/components";
import { formatInt, formatLatency, formatLocalTime } from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import type { AttemptView, TraceView } from "./model";

/** HTTP 状态码 → 语义色。**这是定义映射，不是推断**：2xx 成功、4xx/5xx 失败，其余未知。 */
export function statusSemantic(status: number | undefined): SemStatus {
  if (status === undefined) return "off";
  if (status >= 200 && status < 300) return "ok";
  if (status >= 400) return "error";
  return "off";
}

/** 一行「标签 — 值」。值缺失时整行不画（不写"—"占位，也就没有"看起来像 0"的风险）。 */
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <Typography variant="overline" sx={{ color: "text.secondary" }}>
        {label}
      </Typography>
      <Box sx={{ minWidth: 0 }}>{children}</Box>
    </>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return (
    <Typography
      variant="body1"
      component="span"
      sx={{
        fontFamily: typography.fontMono,
        fontVariantNumeric: "tabular-nums",
        overflowWrap: "anywhere",
      }}
    >
      {children}
    </Typography>
  );
}

function Text({ children }: { children: ReactNode }) {
  return (
    <Typography variant="body1" component="span" sx={{ overflowWrap: "anywhere" }}>
      {children}
    </Typography>
  );
}

/** 任意 JSON 的只读呈现：键值对逐行（对象/数组用 JSON 原文，不猜结构）。 */
function JsonValue({ value }: { value: unknown }) {
  const { t } = useTranslation();

  if (value === null || value === undefined) {
    return (
      <Typography variant="body1" sx={{ color: "text.secondary" }}>
        {t("common.none")}
      </Typography>
    );
  }
  if (typeof value === "object") {
    const entries = Array.isArray(value)
      ? value.map((v, i) => [String(i), v] as const)
      : Object.entries(value as Record<string, unknown>);
    if (entries.length === 0) {
      return (
        <Typography variant="body1" sx={{ color: "text.secondary" }}>
          {t("common.none")}
        </Typography>
      );
    }
    return (
      <Stack spacing={1}>
        {entries.map(([key, v]) => (
          <Box key={key} sx={{ display: "flex", gap: 2, minWidth: 0 }}>
            <Typography
              variant="caption"
              sx={{ color: "text.secondary", fontFamily: typography.fontMono, flex: "0 0 auto" }}
            >
              {key}
            </Typography>
            <Typography
              variant="caption"
              sx={{
                fontFamily: typography.fontMono,
                color: "text.primary",
                overflowWrap: "anywhere",
                whiteSpace: "pre-wrap",
              }}
            >
              {typeof v === "object" ? JSON.stringify(v) : String(v)}
            </Typography>
          </Box>
        ))}
      </Stack>
    );
  }
  return (
    <Typography
      variant="caption"
      sx={{ fontFamily: typography.fontMono, color: "text.primary", whiteSpace: "pre-wrap" }}
    >
      {String(value)}
    </Typography>
  );
}

/** 尝试链里的一次尝试。 */
function AttemptRow({ index, attempt }: { index: number; attempt: AttemptView }) {
  const { t } = useTranslation();
  const parts: ReactNode[] = [];

  if (attempt.channel !== undefined) {
    parts.push(`${t("logs.trace.channel")} ${attempt.channel}`);
  }
  if (attempt.lane !== undefined) parts.push(`${t("logs.trace.lane")} ${attempt.lane}`);
  if (attempt.node !== undefined) parts.push(`${t("logs.trace.node")} ${attempt.node}`);

  return (
    <Box
      data-testid="logs-attempt"
      sx={{
        borderLeft: "2px solid",
        borderColor: "divider",
        pl: 3,
        display: "flex",
        flexDirection: "column",
        gap: 1,
      }}
    >
      <Box sx={{ display: "flex", alignItems: "center", gap: 3, flexWrap: "wrap" }}>
        <Typography variant="body1" sx={{ color: "text.secondary", fontVariantNumeric: "tabular-nums" }}>
          {t("logs.trace.attempt", { n: index + 1 })}
        </Typography>
        {/* 状态码用原始数字 + 语义点（定义映射）；没有状态码就是灰点 + "未知" */}
        <StatusDot
          status={statusSemantic(attempt.status)}
          label={attempt.status === undefined ? t("common.unknown") : String(attempt.status)}
        />
        <Typography variant="body1" sx={{ fontVariantNumeric: "tabular-nums", color: "text.primary" }}>
          {formatLatency(attempt.ms)}
        </Typography>
        {attempt.reason !== undefined ? (
          // Rust 把**网关自己判定的 TA 码**放在 reason 里（ta-gateway/src/trace.rs:99-101）
          <Typography variant="caption" sx={{ fontFamily: typography.fontMono, color: "text.secondary" }}>
            {t("logs.trace.reason")} {attempt.reason}
          </Typography>
        ) : null}
      </Box>
      {parts.length > 0 ? (
        <Typography
          variant="caption"
          sx={{ color: "text.secondary", fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
        >
          {parts.join(" · ")}
        </Typography>
      ) : null}
    </Box>
  );
}

export type TracePanelProps = {
  /** 后端明确说"没有这条"（`log_get_trace` 返回 `Option`，真机上是 `null`）。 */
  detail: TraceView | null | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
};

export function TracePanel({ detail, isLoading, error, onRetry }: TracePanelProps) {
  const { t } = useTranslation();

  if (error !== undefined && error !== null) {
    return (
      <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
        <Typography variant="body1" data-testid="logs-trace-error" sx={{ color: "error.main" }}>
          {errorMessage(error)}
        </Typography>
        <Button variant="outlined" onClick={onRetry}>
          {t("common.retry")}
        </Button>
      </Stack>
    );
  }

  if (isLoading) {
    return (
      <Typography variant="body1" sx={{ color: "text.secondary" }}>
        {t("logs.trace.loading")}
      </Typography>
    );
  }

  if (detail === null || detail === undefined) {
    return (
      <Typography variant="body1" data-testid="logs-trace-missing" sx={{ color: "text.secondary" }}>
        {t("logs.trace.missing")}
      </Typography>
    );
  }

  const { row } = detail;

  return (
    <Stack spacing={4} data-testid="logs-trace">
      <Box>
        <Typography variant="h4">{t("logs.trace.title")}</Typography>
        <Typography variant="caption" sx={{ color: "text.secondary" }}>
          {t("logs.trace.subtitle")}
        </Typography>
      </Box>

      {/* ① 请求级字段 */}
      <Box
        sx={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
          gap: 3,
          alignItems: "baseline",
        }}
      >
        <Field label={t("logs.col.requestId")}>
          <Mono>{detail.requestId !== "" ? detail.requestId : row.requestId}</Mono>
        </Field>
        {row.tsMs !== undefined ? (
          <Field label={t("logs.col.time")}>
            <Mono>{formatLocalTime(row.tsMs)}</Mono>
          </Field>
        ) : null}
        {row.client !== undefined ? (
          <Field label={t("logs.field.client")}>
            <Text>{row.client}</Text>
          </Field>
        ) : null}
        {row.model !== undefined ? (
          <Field label={t("logs.col.model")}>
            <Text>
              {row.upstreamModel !== undefined && row.upstreamModel !== row.model
                ? `${row.model} → ${row.upstreamModel}`
                : row.model}
            </Text>
          </Field>
        ) : null}
        {row.protocolIn !== undefined || row.protocolOut !== undefined ? (
          <Field label={t("logs.field.protocol")}>
            <Mono>{`${row.protocolIn ?? "?"} → ${row.protocolOut ?? "?"}`}</Mono>
          </Field>
        ) : null}
        {row.stream !== undefined ? (
          <Field label={t("logs.field.stream")}>
            <Text>{row.stream ? t("common.yes") : t("common.no")}</Text>
          </Field>
        ) : null}
        {row.status !== undefined ? (
          <Field label={t("logs.trace.status")}>
            <StatusDot status={statusSemantic(row.status)} label={String(row.status)} />
          </Field>
        ) : null}
        {row.latencyMs !== undefined ? (
          <Field label={t("logs.col.latency")}>
            <Mono>{formatLatency(row.latencyMs)}</Mono>
          </Field>
        ) : null}
        {row.ttfbMs !== undefined ? (
          <Field label="TTFB">
            <Mono>{formatLatency(row.ttfbMs)}</Mono>
          </Field>
        ) : null}
        {row.channel !== undefined ? (
          <Field label={t("logs.col.channel")}>
            <Mono>{row.channel}</Mono>
          </Field>
        ) : null}
        {row.credential !== undefined ? (
          <Field label={t("logs.field.credential")}>
            <Mono>{row.credential}</Mono>
          </Field>
        ) : null}
        {row.policyId !== undefined ? (
          <Field label={t("logs.field.policy")}>
            <Mono>{row.policyId}</Mono>
          </Field>
        ) : null}
        {row.groupId !== undefined ? (
          <Field label={t("logs.field.group")}>
            <Mono>{row.groupId}</Mono>
          </Field>
        ) : null}
        {row.mode !== undefined ? (
          <Field label={t("logs.field.mode")}>
            <Mono>{row.mode}</Mono>
          </Field>
        ) : null}
        {row.tokens !== undefined ? (
          <Field label={t("logs.col.tokens")}>
            <Mono>{formatInt(row.tokens)}</Mono>
          </Field>
        ) : null}
        {row.error !== undefined ? (
          <Field label={t("logs.col.error")}>
            <Text>{row.error}</Text>
          </Field>
        ) : null}
      </Box>

      {/* ② 尝试链（C4 的核心） */}
      <Stack spacing={2}>
        <Typography variant="h4">{t("logs.trace.attempts", { value: detail.attempts.length })}</Typography>
        {detail.attempts.length === 0 ? (
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {t("logs.trace.attemptsNone")}
          </Typography>
        ) : (
          <Stack spacing={3}>
            {detail.attempts.map((attempt, index) => (
              <AttemptRow key={`${attempt.channel ?? "?"}-${index}`} index={index} attempt={attempt} />
            ))}
          </Stack>
        )}
      </Stack>

      {/* ③ 头差异：自检字段。非空 = 网关吞了/加了头（[07 §1.3] 的"谁吞了字段"） */}
      <Stack spacing={1}>
        <Typography variant="h4">{t("logs.trace.headerDiff")}</Typography>
        {detail.headerDiff === undefined ? (
          <Typography variant="body1" data-testid="logs-header-diff" sx={{ color: "text.secondary" }}>
            {t("logs.trace.headerDiff.unknown")}
          </Typography>
        ) : detail.headerDiff.count === 0 ? (
          <Typography variant="body1" data-testid="logs-header-diff" sx={{ color: "text.secondary" }}>
            {t("logs.trace.headerDiff.empty")}
          </Typography>
        ) : (
          <Stack spacing={1} data-testid="logs-header-diff">
            <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
              <StatusDot status="warn" />
              <Typography variant="body1" sx={{ color: "warning.main" }}>
                {t("logs.trace.headerDiff.found", { value: detail.headerDiff.count })}
              </Typography>
            </Box>
            {detail.headerDiff.added.map((name) => (
              <Mono key={`added-${name}`}>{`+ ${name}`}</Mono>
            ))}
            {detail.headerDiff.removed.map((name) => (
              <Mono key={`removed-${name}`}>{`- ${name}`}</Mono>
            ))}
            {detail.headerDiff.changed.map(([name, client, upstream]) => (
              <Mono key={`changed-${name}`}>{`~ ${name}: ${client} → ${upstream}`}</Mono>
            ))}
          </Stack>
        )}
      </Stack>

      {/* ④ body 结构 / 头 / body（后面几项只在 full 模式才有） */}
      {detail.bodyShape !== undefined || detail.bodyShapeOut !== undefined ? (
        <Stack spacing={1}>
          <Typography variant="h4">{t("logs.trace.bodyShape")}</Typography>
          {detail.bodyShape !== undefined ? <JsonValue value={detail.bodyShape} /> : null}
          {detail.bodyShapeOut !== undefined ? <JsonValue value={detail.bodyShapeOut} /> : null}
        </Stack>
      ) : null}
      {detail.requestHeaders !== undefined ? (
        <Stack spacing={1}>
          <Typography variant="h4">{t("logs.trace.requestHeaders")}</Typography>
          <JsonValue value={detail.requestHeaders} />
        </Stack>
      ) : null}
      {detail.responseHeaders !== undefined ? (
        <Stack spacing={1}>
          <Typography variant="h4">{t("logs.trace.responseHeaders")}</Typography>
          <JsonValue value={detail.responseHeaders} />
        </Stack>
      ) : null}
      {detail.requestBody !== undefined ? (
        <Stack spacing={1}>
          <Typography variant="h4">{t("logs.trace.requestBody")}</Typography>
          <JsonValue value={detail.requestBody} />
        </Stack>
      ) : null}
      {detail.responseBody !== undefined ? (
        <Stack spacing={1}>
          <Typography variant="h4">{t("logs.trace.responseBody")}</Typography>
          <JsonValue value={detail.responseBody} />
        </Stack>
      ) : null}

      {/* ⑤ 原始字段：后端返回的整条记录，原样打印（UI 不替用户过滤证据） */}
      <Stack spacing={1}>
        <Typography variant="h4">{t("logs.trace.raw")}</Typography>
        <Typography variant="caption" sx={{ color: "text.secondary" }}>
          {t("logs.trace.rawHint")}
        </Typography>
        {detail.extraKeys.length > 0 ? (
          <Typography variant="caption" sx={{ color: "text.secondary" }}>
            {t("logs.trace.extra", { keys: detail.extraKeys.join(", ") })}
          </Typography>
        ) : null}
        <Box
          component="pre"
          data-testid="logs-trace-raw"
          sx={{
            m: 0,
            p: 3,
            bgcolor: "background.default",
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
          {JSON.stringify(detail.raw, null, 2)}
        </Box>
      </Stack>
    </Stack>
  );
}

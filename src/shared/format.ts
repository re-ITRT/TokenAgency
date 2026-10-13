/**
 * 数字 / 金额 / 时间 的显示规则 —— 实现「怎么做得好看（具体规则，不是感觉）」。
 *
 * 全站**只允许**用这里的函数格式化，不要在页面里手写 `toFixed` / 拼字符串：
 *
 * - 大号数字用 `tabular-nums` 等宽对齐（由 `<NumberCard>` 负责），千分位；
 * - Token 用 `12.4M` / `842K` 缩写，**悬停显示精确值**（`formatExact`）；
 * - 时间轴永远本地时区，列头用 `timezoneLabel()` 标出（如"今天（GMT+8）"）。
 */

import dayjs from "dayjs";

/** 千分位整数。`1284` → `1,284`。 */
/**
 * 缺值占位符（U+2014 EM DASH）。
 *
 * **为什么放在这里**：这个字符原先在 **7 个文件**各写了一遍 `const DASH = "—";`
 * （重复清单）。它出现在"后端没给这个字段"的每一个格子里 ——
 * 一旦有人写成 `-`/`--`/`N/A`，界面就会同时出现几种缺值写法。
 * 收敛成一处之后，全站只有一个缺值符号。
 *
 * 注意它**不是 0**：`—` 表示"读不到"，`0` 表示"确实是 0"（红线）。
 */
export const DASH = "\u2014";
export function formatInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** 精确值（给 tooltip 用），保留最多 4 位小数且去掉尾随 0。 */
export function formatExact(n: number): string {
  if (Number.isInteger(n)) return formatInt(n);
  return n.toLocaleString("en-US", { maximumFractionDigits: 4 });
}

/** Token / 请求量缩写：`12.4M` / `842K` / `1,284`。**悬停请用 `formatExact`。** */
export function formatTokens(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1_000_000_000) return `${trim1(n / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${trim1(n / 1_000_000)}M`;
  if (abs >= 1_000) return `${trim1(n / 1_000)}K`;
  return formatInt(n);
}

function trim1(v: number): string {
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

const CURRENCY_SYMBOL: Record<string, string> = {
  USD: "$",
  CNY: "¥",
  EUR: "€",
  JPY: "¥",
  GBP: "£",
};

/** 币种符号（未知币种退回代码，避免显示错误符号）。 */
export function currencySymbol(currency: string): string {
  return CURRENCY_SYMBOL[currency.toUpperCase()] ?? `${currency.toUpperCase()} `;
}

/**
 * 金额。小额自动多给有效位（单价常是 0.0003 这种）。
 * `{ amount: 18.62, currency: "CNY" }` → `¥18.62`
 */
export function formatMoney(money: { amount: number; currency: string }): string {
  const abs = Math.abs(money.amount);
  const digits = abs === 0 ? 2 : abs < 0.01 ? 6 : abs < 1 ? 4 : 2;
  return `${currencySymbol(money.currency)}${money.amount.toLocaleString("en-US", {
    minimumFractionDigits: Math.min(digits, 2),
    maximumFractionDigits: digits,
  })}`;
}

/** 比率 → 百分比。**入参是 0~1 的比率**（后端一律给比率，不给 0~100）。 */
export function formatPercent(ratio: number, digits?: number): string {
  const pct = ratio * 100;
  const d = digits ?? (Math.abs(pct) < 10 ? 1 : 0);
  return `${pct.toFixed(d)}%`;
}

/**
 * 字节数：`386 MB` / `1.2 GB`（**1024 进制**，与库里 `max_size_mb` / `StorageStats.bytes` 同口径）。
 * 日志/库文件大小的唯一格式化出口（别在页面里手写除法拼单位）。
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  const units = ["KB", "MB", "GB", "TB"] as const;
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  // 大数不再给小数位（386.0 MB 是噪声）；小数位只用于 100 以下
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[i]}`;
}

/** 延迟：`82ms` / `1.2s`。 */
export function formatLatency(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${trim1(ms / 1000)}s`;
}

/** 本地时间轴**列头**：`今天（GMT+8）`。跨时区误读是真实事故，故必须标。 */
export function timezoneLabel(): string {
  const offsetMin = -new Date().getTimezoneOffset();
  const sign = offsetMin >= 0 ? "+" : "-";
  const abs = Math.abs(offsetMin);
  const h = Math.floor(abs / 60);
  const m = abs % 60;
  return `GMT${sign}${h}${m === 0 ? "" : `:${String(m).padStart(2, "0")}`}`;
}

/** 本地时间（默认 `MM-DD HH:mm:ss`，请求流用得到秒）。 */
export function formatLocalTime(tsMs: number, fmt = "MM-DD HH:mm:ss"): string {
  return dayjs(tsMs).format(fmt);
}

/** 本地日期（列头/分组用）。 */
export function formatLocalDate(tsMs: number): string {
  return dayjs(tsMs).format("YYYY-MM-DD");
}

/** 相对时间：`3 分钟前`。仅用于"最后更新"这类弱信息。 */
export function formatAgo(tsMs: number, nowMs: number = Date.now()): string {
  const diff = Math.max(0, nowMs - tsMs);
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s} 秒前`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分钟前`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} 小时前`;
  return `${Math.floor(h / 24)} 天前`;
}

/** 时长（秒）→ `1h 20m` / `3m 12s`。 */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

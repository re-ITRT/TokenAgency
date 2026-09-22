/**
 * 设置页（[07 §7]）的数据模型 —— **这一页最容易做错的三处都收口在这个文件里**：
 *
 * 1. **I2 边界（只展示，不改系统设置）**：不是靠"写页面的时候注意一下"，而是靠
 *    `EDITABLE_PATHS` 白名单 + `setPath()` 对白名单外**直接抛错**。
 *    也就是说 `net.tun` / `net.system_proxy` / `system_integration.enable_tun|enable_system_proxy|autostart`
 *    这几条**在类型之外也走不通**：想"顺手"给它们加个开关，测试会红。
 * 2. **后端返回值的宽容读取**：`ok` / `applied` 只有**显式为 true** 才算成功。
 *    读不出来（形状变了、返回 undefined）一律按"未通过 / 未应用"处理 ——
 *    **不认识的返回绝不能被当成成功**（那正是"假成功"的来源）。
 * 3. **本地预检 vs 权威校验**：`validateDraft()` 只挡明显非法的输入（端口形状、取值区间），
 *    它**不替代**后端的 `config_validate`：命令层接受什么、拒绝什么都由后端说了算。
 *
 * 关于开机自启动：读取系统实际状态的命令（app_get_autostart / app_set_autostart /
 * app_repair_autostart）**尚未在桌面端注册**，所以这里**没有**任何对应的类型、白名单或开关 ——
 * 配置里的 `autostart` 只作为"配置期望"被读取展示，UI 不许把它说成"当前系统状态"。
 */

import type { ConfigDoc, LogQuery } from "@/shared/bindings";
import type { I18nKey } from "@/shared/i18n";

/** 反序列化后的未知对象（后端可能给出契约之外的形状）。 */
export type Row = Record<string, unknown>;

function asRow(v: unknown): Row {
  return typeof v === "object" && v !== null ? (v as Row) : {};
}

function pickStr(row: Row, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const v = row[key];
    if (typeof v === "string" && v !== "") return v;
  }
  return undefined;
}

function pickNum(row: Row, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const v = row[key];
    if (typeof v === "number" && Number.isFinite(v)) return v;
  }
  return undefined;
}

// ================================================================ 可改字段白名单（I2）

/**
 * 设置页**允许**写进配置的字段（路径 = 从 `ConfigDoc` 根开始）。
 *
 * 加一条之前请先想清楚两件事：
 * - 它是不是**系统状态**？（会写 OS 的：TUN / 系统代理 / 开机自启动 —— 一律不许进这里，见 I2）
 * - 有没有**专属命令**在管它？（有专属命令而命令未注册的，等命令落地，别用通用应用绕过去）
 */
export const EDITABLE_PATHS: readonly (readonly string[])[] = [
  ["gateway", "listen"],
  ["gateway", "admin_listen"],
  ["gateway", "max_body_mb"],
  ["gateway", "debug_headers"],
  ["gateway", "expose_models_with_capabilities"],
  ["request_log", "max_rows"],
  ["request_log", "max_size_mb"],
  ["request_log", "sample_rate"],
  ["request_log", "image_handling"],
  ["system_integration", "tray_show_latency"],
  ["system_integration", "autostart_mode"],
  ["system_integration", "autostart_delay_s"],
];

/**
 * 设置页**明确禁止**写入的字段（[09 §1.2] E1 / I2）。
 *
 * 这些不是"暂时没做"，而是**设计上就不该由这一页来改**：
 * 改它们会动操作系统的网络或启动状态，必须走各自明确的入口与确认流程。
 */
export const FORBIDDEN_PATHS: readonly (readonly string[])[] = [
  ["net", "tun"],
  ["net", "system_proxy"],
  ["net", "default_lane"],
  ["system_integration", "enable_tun"],
  ["system_integration", "enable_system_proxy"],
  ["system_integration", "autostart"],
  ["system_integration", "file_watch_mode"],
];

/** 路径 → 稳定字符串（`["gateway","listen"]` → `gateway.listen`）。 */
export function pathKey(path: readonly string[]): string {
  return path.join(".");
}

/** 是否在白名单里。 */
export function isEditable(path: readonly string[]): boolean {
  const key = pathKey(path);
  return EDITABLE_PATHS.some((p) => pathKey(p) === key);
}

/** 是否被明确禁止（给测试与评审用；`setPath` 用的是白名单，不是黑名单）。 */
export function isForbidden(path: readonly string[]): boolean {
  const key = pathKey(path);
  return FORBIDDEN_PATHS.some((p) => pathKey(p) === key);
}

// ================================================================ 不可变读写

/** 读一个嵌套路径（缺任何一段都返回 `undefined`，不抛）。 */
export function getPath(doc: unknown, path: readonly string[]): unknown {
  let node: unknown = doc;
  for (const key of path) {
    const row = asRow(node);
    if (!(key in row)) return undefined;
    node = row[key];
  }
  return node;
}

/**
 * 写一个嵌套路径，**返回新对象**（不改原对象）。
 *
 * **只允许白名单里的路径**：路径不在 `EDITABLE_PATHS` 里直接抛错 ——
 * 这是 I2 的机械保证（"改系统设置"的字段连写都写不进去），
 * 而不是靠调用方自觉。未被触碰的分支保持引用共享（不整份深拷贝）。
 */
export function setPath<T>(doc: T, path: readonly string[], value: unknown): T {
  if (path.length === 0) throw new Error("空路径");
  if (!isEditable(path)) {
    throw new Error(`不是设置页的可改字段：${pathKey(path)}（I2：这一页不写系统设置）`);
  }
  return writeIn(doc, path, value) as T;
}

function writeIn(node: unknown, path: readonly string[], value: unknown): unknown {
  const [head, ...rest] = path;
  if (head === undefined) return value;
  const current = asRow(node);
  return { ...current, [head]: writeIn(current[head], rest, value) };
}

function isPlainObject(v: unknown): v is Row {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((item, i) => deepEqual(item, b[i]));
  }
  if (isPlainObject(a) && isPlainObject(b)) {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const key of keys) {
      if (!deepEqual(a[key], b[key])) return false;
    }
    return true;
  }
  return false;
}

/** 两棵配置树的差异（叶子路径，字典序）。相同则空数组。 */
export function diffPaths(a: unknown, b: unknown, prefix = ""): string[] {
  if (isPlainObject(a) && isPlainObject(b)) {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    const out: string[] = [];
    for (const key of [...keys].sort()) {
      out.push(...diffPaths(a[key], b[key], prefix === "" ? key : `${prefix}.${key}`));
    }
    return out;
  }
  return deepEqual(a, b) ? [] : [prefix];
}

/** 深拷贝一份配置（草稿要能独立编辑，不能原地改到 query 缓存上的对象）。 */
export function cloneDoc(doc: ConfigDoc): ConfigDoc {
  return JSON.parse(JSON.stringify(doc)) as ConfigDoc;
}

// ================================================================ 本地预检（不替代后端校验）

/** 本地预检的一条问题：路径 + i18n 键（文案在 i18n 里，模型层不拼中文）。 */
export type LocalIssue = { path: string; key: I18nKey };

function inRange(v: unknown, min: number, max: number): boolean {
  return typeof v === "number" && Number.isFinite(v) && v >= min && v <= max;
}

function isPositiveInt(v: unknown): boolean {
  return typeof v === "number" && Number.isInteger(v) && v > 0;
}

/** `主机:端口`（端口 1–65535）。IPv6 允许写成 `[::1]:8787`。 */
export function isValidListen(addr: string): boolean {
  const m = /^\[?([0-9a-zA-Z.\-:]+?)\]?:(\d{1,5})$/.exec(addr.trim());
  if (m === null) return false;
  const host = m[1] ?? "";
  const port = Number(m[2]);
  return host !== "" && port >= 1 && port <= 65535;
}

/** `http(s)://` 开头的绝对地址。 */
export function isValidHttpUrl(url: string): boolean {
  try {
    const parsed = new URL(url.trim());
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * 明显非法的输入先挡在本地（**权威判定仍然是后端的 `config_validate`**）。
 * 只做"形状与区间"这一层：不做引用完整性（那需要整份配置的语义，属于后端）。
 */
export function validateDraft(doc: ConfigDoc): LocalIssue[] {
  const issues: LocalIssue[] = [];
  if (!isValidListen(doc.gateway.listen)) {
    issues.push({ path: "gateway.listen", key: "settings.issue.listen" });
  }
  if (!isValidListen(doc.gateway.admin_listen)) {
    issues.push({ path: "gateway.admin_listen", key: "settings.issue.listen" });
  }
  if (!inRange(doc.gateway.max_body_mb, 1, 4096)) {
    issues.push({ path: "gateway.max_body_mb", key: "settings.issue.maxBody" });
  }
  if (!isPositiveInt(doc.request_log.max_rows)) {
    issues.push({ path: "request_log.max_rows", key: "settings.issue.maxRows" });
  }
  if (!isPositiveInt(doc.request_log.max_size_mb)) {
    issues.push({ path: "request_log.max_size_mb", key: "settings.issue.maxSize" });
  }
  if (!inRange(doc.request_log.sample_rate, 0, 1)) {
    issues.push({ path: "request_log.sample_rate", key: "settings.issue.sampleRate" });
  }
  if (!inRange(doc.system_integration.autostart_delay_s, 0, 300)) {
    issues.push({ path: "system_integration.autostart_delay_s", key: "settings.issue.delay" });
  }
  return issues;
}

// ================================================================ 后端返回值的宽容读取

/** 一条字段级问题（契约 `FieldError`）。缺字段给空串，不编内容。 */
export type FieldIssueView = { path: string; code: string; message: string };

export type ReportView = {
  /** **只有显式 `ok: true` 才算通过**。 */
  ok: boolean;
  errors: FieldIssueView[];
  warnings: FieldIssueView[];
  /** 后端返回的形状能不能认出来（认不出来就不能当成功）。 */
  recognizable: boolean;
};

function readFieldIssues(raw: unknown): FieldIssueView[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((item) => {
    const row = asRow(item);
    return {
      path: pickStr(row, "path") ?? "",
      code: pickStr(row, "code") ?? "",
      message: pickStr(row, "message") ?? "",
    };
  });
}

export function readValidationReport(raw: unknown): ReportView {
  const row = asRow(raw);
  return {
    ok: row["ok"] === true,
    errors: readFieldIssues(row["errors"]),
    warnings: readFieldIssues(row["warnings"]),
    recognizable: typeof row["ok"] === "boolean",
  };
}

export type ApplyView = {
  /** **只有显式 `applied: true` 才算写进去了**。 */
  applied: boolean;
  restartRequired: boolean;
  kernelReload: boolean;
  recognizable: boolean;
};

export function readApplyResult(raw: unknown): ApplyView {
  const row = asRow(raw);
  return {
    applied: row["applied"] === true,
    restartRequired: row["restart_required"] === true,
    kernelReload: row["kernel_reload"] === true,
    recognizable: typeof row["applied"] === "boolean",
  };
}

export type SnapshotView = {
  id: string;
  label: string;
  createdAtMs?: number;
  sizeBytes?: number;
};

/** 快照列表（`config_history_list`）；读不出来给空列表，不给假条目。 */
export function readSnapshots(raw: unknown): SnapshotView[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .map((item) => {
      const row = asRow(item);
      const id = pickStr(row, "id");
      if (id === undefined) return undefined;
      const view: SnapshotView = { id, label: pickStr(row, "label") ?? "" };
      const createdAtMs = pickNum(row, "created_at_ms");
      if (createdAtMs !== undefined) view.createdAtMs = createdAtMs;
      const sizeBytes = pickNum(row, "size_bytes");
      if (sizeBytes !== undefined) view.sizeBytes = sizeBytes;
      return view;
    })
    .filter((v): v is SnapshotView => v !== undefined);
}

/** 日志占用（`log_storage_stats`）。缺的字段留 `undefined` → UI 画"—"，不画 0。 */
export type StorageView = {
  rows?: number;
  bytes?: number;
  bodyBytes?: number;
  oldestTs?: number;
  newestTs?: number;
};

export function readStorageStats(raw: unknown): StorageView | undefined {
  if (raw === undefined || raw === null) return undefined;
  const row = asRow(raw);
  return {
    rows: pickNum(row, "rows"),
    bytes: pickNum(row, "bytes"),
    bodyBytes: pickNum(row, "body_bytes"),
    oldestTs: pickNum(row, "oldest_ts"),
    newestTs: pickNum(row, "newest_ts"),
  };
}

/**
 * 最近一条日志行**记录时**的模式。
 *
 * 为什么读它：命令面里**没有"读取当前模式"的接口**（`log_set_mode` 只写不读），
 * 所以"当前模式"只能显示「未读取」；而"最近一行是以什么模式落库的"是**有证据的事实**。
 */
export function readNewestRow(raw: unknown): { mode?: string; tsMs?: number } {
  const rows = asRow(raw)["rows"];
  if (!Array.isArray(rows) || rows.length === 0) return {};
  const first = asRow(rows[0]);
  const out: { mode?: string; tsMs?: number } = {};
  const mode = pickStr(first, "mode");
  if (mode !== undefined) out.mode = mode;
  const tsMs = pickNum(first, "ts_ms", "ts");
  if (tsMs !== undefined) out.tsMs = tsMs;
  return out;
}

// ================================================================ 密钥状态

export type SecretState = "unset" | "reference" | "inline";

/** 本地访问密钥的三种状态。**值本身不在这里返回**（展示与否由页面决定）。 */
export function secretState(value: string | null | undefined): SecretState {
  if (value === null || value === undefined || value.trim() === "") return "unset";
  return /^\$\{secret:[^}]+\}$/.test(value.trim()) ? "reference" : "inline";
}

/** 密钥状态的 i18n 键。 */
export const SECRET_STATE_KEY: Record<SecretState, I18nKey> = {
  unset: "settings.secret.unset",
  reference: "settings.secret.reference",
  inline: "settings.secret.inline",
};

// ================================================================ 日志查询入参（两套名字都发）

const DAY_MS = 24 * 60 * 60 * 1000;

export type LogQueryArgs = { query: LogQuery; q: Record<string, unknown> };

/**
 * 取最近一条日志行用的 `log_query` 入参。
 *
 * **两个参数名都发**：Rust 侧签名是 `q`（`src-tauri/src/commands/log.rs`），
 * 冻结契约写的是 `query` —— 只发一个必然有一边拿不到（与日志页同一处理）。
 */
export function recentLogQueryArgs(limit: number, nowMs: number = Date.now()): LogQueryArgs {
  const fromMs = nowMs - 7 * DAY_MS;
  const query = {
    // `range` 是契约字段（`LogQuery.range: TimeRange`），线上值必须是
    // `crates/ta-core/src/clock.rs` 的裸 snake_case 变体名 —— 不是 `{preset:…}`。
    // 真机生效靠下面的 `q.from_ms` / `q.to_ms`（Rust 的 `LogQuery` 没有 `range` 字段）。
    range: "last7d" as const,
    group_by: [],
    metrics: [],
    format: "table" as const,
    limit,
  } as unknown as LogQuery;
  return { query, q: { from_ms: fromMs, to_ms: nowMs, limit } };
}

// @vitest-environment jsdom
/**
 * 守卫：**前端发给后端的 `range` 形状必须与 Rust 实现一致**。
 *
 * ## 为什么需要它（实测，不是推断）
 *
 * `usage_dashboard` / `usage_query` / `usage_export_csv` 的 `range` 参数在 Rust 里是
 * `ta_core::clock::TimeRange`（`crates/ta-core/src/clock.rs:77-88`）：
 * 一个 `#[serde(rename_all = "snake_case")]` 的**外部标记枚举**，所以合法的 JSON 只有两种：
 *
 * - 预设：**裸字符串** `"last7d"` / `"today"` / `"last30d"` / `"this_month"` / `"all"`
 * - 显式区间：`{"explicit":{"from_ms":…,"to_ms":…}}`
 *
 * 而前端曾按旧契约发 `{"range":{"preset":"last7d"}}`。用真的 `ta_store::UsageQuery`
 * 反序列化该载荷（一次性探针，`serde_json::from_value`）实测输出：
 *
 * ```text
 * FAIL UsageQuery <- {"range":{"preset":"last7d"},"group_by":["model"],…}
 *   :: unknown variant `preset`, expected one of `explicit`, `today`, `last7d`,
 *      `last30d`, `this_month`, `all`
 * OK   UsageQuery <- {"range":"last7d","group_by":["model"],…}
 * ```
 *
 * 也就是说：真机（Tauri IPC）上 `range` 一进去就被 serde 拒掉，**页面取不到任何数据**；
 * 而浏览器里 `mock.ts` 忽略参数，所以看起来一切正常 —— 这正是"看起来能用"的典型。
 *
 * ## 本守卫四件事（每一件都能独立抓到上面那类漂移）
 *
 * 1. **契约 = 实现**：从 `clock.rs` 解析 `TimeRange` 的变体名，与 `bindings.ts` 的
 *    `TimeRange` 联合逐一对齐。Rust 改了名字而 TS 没跟 → 这里红。**这是防复发的主力**：
 *    它不依赖下面任何一条调用点，也不需要有人记得来改守卫。
 * 2. **真实构造器产出的形状**：调用页面真的在用的构造函数（不是复述），断言产出的
 *    `range` 是线上形状。
 * 3. **源码扫描**：页面里不许再把 `{preset:…}` 当 `range` 发出去 —— 抓内联在组件里的
 *    调用点（这类没有纯函数可测，dashboard 那一处原来就是内联的）。
 * 4. **守卫自证有牙**：合成一段"Rust 改了名字"的源码与一个 `{preset:…}` 载荷，
 *    必须被判定为漂移/非法（否则本文件就是"对着空集断言然后绿"）。
 *
 * 已知边界（诚实写出来）：
 * - 第 1 条只能证明**变体名集合**一致，证不了"每个变体的负载字段"（如 `explicit.from_ms`）。
 *   负载字段靠第 2 条 + `bindings.ts` 里的形状断言覆盖。
 * - 本守卫只管 `range` / `TimeRange`。同一批命令里还有**别的**形状漂移（见文件末的
 *   `KNOWN_OTHER_DRIFT`），那些不在这里修、也不在这里断言。
 */

import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

// `@/pages/cost` 会 import echarts；本守卫不渲染，只需要它别在 import 期炸掉。
vi.mock("echarts", () => ({ init: vi.fn() }));

import {
  buildUsageQuery,
  usageExportArgs,
  usageQueryArgs,
  DEFAULT_COST_FILTER,
  type UsageRange,
} from "@/pages/cost";
import { DASHBOARD_RANGE, dashboardRangeArgs } from "@/pages/dashboard";
import { buildLogQuery, DEFAULT_FILTER, RANGE_PRESETS } from "@/pages/logs/model";
import { recentLogQueryArgs } from "@/pages/settings/model";

const SRC_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const REPO_ROOT = path.resolve(SRC_ROOT, "..");
const CLOCK_RS = path.join(REPO_ROOT, "crates/ta-core/src/clock.rs");
const BINDINGS_TS = path.join(SRC_ROOT, "shared/bindings.ts");

const read = (f: string): string => readFileSync(f, "utf8");

// ================================================================ ① 解析两个真相源

export type RustVariant = {
  /** Rust 里的变体名，如 `Last7d`。 */
  name: string;
  /** 线上名字（`rename_all = "snake_case"` 之后），如 `last7d`。 */
  wire: string;
  /** 变体是否带负载（`Explicit { … }` 带，`Today` 不带）。 */
  hasFields: boolean;
};

/**
 * `clock.rs` 里 `pub enum TimeRange { … }` 的**顶层**变体。
 *
 * 只认 4 空格缩进的 `Name,` / `Name {` —— 8 空格的是 `Explicit` 的内部字段（`from_ms` 等）。
 * 锚点找不到时返回空数组，让调用方去喊（而不是对着空集断言然后绿）。
 */
export function timeRangeVariants(rustSource: string): RustVariant[] {
  const lines = rustSource.split(/\r?\n/);
  const start = lines.findIndex((l) => /^pub enum TimeRange\b/.test(l));
  if (start < 0) return [];
  const out: RustVariant[] = [];
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("}")) break;
    const m = line.match(/^ {4}([A-Z][A-Za-z0-9_]*)\s*([,{])/);
    const name = m?.[1];
    if (name === undefined) continue;
    out.push({ name, wire: toSnakeCase(name), hasFields: m?.[2] === "{" });
  }
  return out;
}

/** serde `rename_all = "snake_case"` 对**变体名**的实际效果（`Last7d` → `last7d`、`ThisMonth` → `this_month`）。 */
export function toSnakeCase(variant: string): string {
  return variant.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}

/** `bindings.ts` 的 `export type TimeRange = …` 整段声明（到下一个顶层 `export` 为止）。 */
export function timeRangeDeclaration(bindingsSource: string): string {
  const start = bindingsSource.indexOf("export type TimeRange =");
  if (start < 0) return "";
  const next = bindingsSource.indexOf("\nexport ", start + 1);
  return bindingsSource.slice(start, next < 0 ? undefined : next);
}

/** 该声明里的**字符串字面量**（= 只有负载为空的变体才该出现在这里）。 */
export function bindingTimeRangeLiterals(declaration: string): string[] {
  return [...declaration.matchAll(/"([a-z0-9_]+)"/g)].map((m) => m[1] ?? "");
}

/**
 * 契约与实现的漂移清单（空数组 = 一致）。**这就是本守卫的核心判据。**
 */
export function timeRangeDrift(rustSource: string, bindingsSource: string): string[] {
  const variants = timeRangeVariants(rustSource);
  if (variants.length === 0) return ["`clock.rs` 里没解析出 `TimeRange` 的变体（锚点失效？）"];

  const declaration = timeRangeDeclaration(bindingsSource);
  if (declaration === "") return ["`bindings.ts` 里没找到 `export type TimeRange`（锚点失效？）"];

  const drift: string[] = [];
  const unitWire = variants.filter((v) => !v.hasFields).map((v) => v.wire);
  const fieldWire = variants.filter((v) => v.hasFields).map((v) => v.wire);
  const literals = bindingTimeRangeLiterals(declaration);

  const missing = unitWire.filter((w) => !literals.includes(w));
  const extra = literals.filter((w) => !unitWire.includes(w));
  if (missing.length > 0) {
    drift.push(`实现有、契约缺的预设：${missing.join(", ")}（Rust 改了名字而 TS 没跟？）`);
  }
  if (extra.length > 0) {
    drift.push(`契约有、实现没有的预设：${extra.join(", ")}（发出去就是 \`unknown variant\`）`);
  }

  for (const w of fieldWire) {
    if (!new RegExp(`\\b${w}\\s*:\\s*\\{`).test(declaration)) {
      drift.push(`实现里的带负载变体 \`${w}\` 在契约里没有对应的对象成员`);
    }
  }
  // 旧契约的两种错形状：`{preset:…}`（枚举里没有这个变体）与 `{from,to}`（键名也错）
  if (/\bpreset\b/.test(declaration)) {
    drift.push("契约里出现了 `preset` 键 —— `TimeRange` 里没有这个变体");
  }
  if (/\bfrom\s*:\s*number/.test(declaration)) {
    drift.push("契约里出现了 `{from,to}` 形状 —— 线上键名是 `explicit.{from_ms,to_ms}`");
  }
  return drift;
}

// ================================================================ ② 运行时形状判定

/** `explicit` 负载的形状（Rust 是 `Explicit { from_ms: i64, to_ms: i64 }`）。 */
function hasExplicitPayload(v: Record<string, unknown>): boolean {
  const inner = v["explicit"];
  if (typeof inner !== "object" || inner === null || Array.isArray(inner)) return false;
  const keys = Object.keys(inner as Record<string, unknown>).sort();
  const row = inner as Record<string, unknown>;
  return (
    keys.length === 2 &&
    keys[0] === "from_ms" &&
    keys[1] === "to_ms" &&
    typeof row["from_ms"] === "number" &&
    typeof row["to_ms"] === "number"
  );
}

/**
 * 一个值是不是 Rust `TimeRange` 认的形状。
 *
 * `presets` / `fieldKeys` **必须是从 `clock.rs` 解析出来的**（不是手抄的常量）——
 * 否则 Rust 改了名字这个判定还会说"合法"。
 */
export function isRustTimeRangeShape(
  v: unknown,
  presets: readonly string[],
  fieldKeys: readonly string[],
): boolean {
  if (typeof v === "string") return presets.includes(v);
  if (typeof v !== "object" || v === null || Array.isArray(v)) return false;
  const row = v as Record<string, unknown>;
  const keys = Object.keys(row);
  if (keys.length !== 1) return false;
  const key = keys[0] ?? "";
  if (!fieldKeys.includes(key)) return false;
  // 现在 `TimeRange` 只有一个带负载变体；它的负载形状单独钉住。
  return key === "explicit" ? hasExplicitPayload(row) : true;
}

// ================================================================ ③ 源码扫描

/** 涂白注释（**保留行号**）：注释里"提到"形状不算发出去。 */
export function blankComments(text: string): string {
  const noBlocks = text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  return noBlocks
    .split(/\r?\n/)
    .map((l) => l.replace(/\/\/.*$/, ""))
    .join("\n");
}

/**
 * 源码里把 `{preset:…}` 当 `range` 发出去的地方（这次漂移的**确切**形状）。
 *
 * 为什么规则长这样：`preset` 这个词在页面上是**合法的界面词汇**（`draft.preset`、
 * `RangePreset`、`RUST_PRESET` 映射表都用它）。所以不能简单禁词，只禁
 * "`range` 后面紧跟一个以 `preset` 开头的对象字面量"这一种线上形状。
 */
export function presetRangeLiterals(source: string): string[] {
  return [...blankComments(source).matchAll(/range\s*:\s*\{\s*preset\s*:/g)].map((m) => m[0]);
}

/** `src/` 下的生产文件（排除测试与夹具）。 */
function productionFiles(dir: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) productionFiles(p, out);
    else if (/\.tsx?$/.test(e.name) && !/\.test\.tsx?$/.test(e.name)) out.push(p);
  }
  return out;
}

// ================================================================ 测试

const VARIANTS = timeRangeVariants(read(CLOCK_RS));
const UNIT_PRESETS = VARIANTS.filter((v) => !v.hasFields).map((v) => v.wire);
const FIELD_KEYS = VARIANTS.filter((v) => v.hasFields).map((v) => v.wire);

/** `UsageRange` 加了取值而这里没跟 → tsc 直接报错，守卫保持穷尽。 */
const EVERY_USAGE_RANGE: Record<UsageRange, true> = {
  today: true,
  "7d": true,
  "30d": true,
  month: true,
};
const ALL_USAGE_RANGES = Object.keys(EVERY_USAGE_RANGE) as UsageRange[];

describe("[range 形状守卫] 前端发的 TimeRange 必须与 Rust 实现一致", () => {
  it("两个真相源都真的读到了（路径/锚点写错时不许对着空集断言然后绿）", () => {
    expect(VARIANTS.length, `${CLOCK_RS} 没解析出 TimeRange 变体`).toBeGreaterThan(0);
    expect(UNIT_PRESETS).toContain("today");
    expect(UNIT_PRESETS).toContain("last7d");
    expect(FIELD_KEYS).toContain("explicit");
    expect(ALL_USAGE_RANGES.length).toBe(4);
  });

  it("① 契约的 TimeRange 联合 == clock.rs 的变体（Rust 改名字而 TS 没跟 → 红）", () => {
    const drift = timeRangeDrift(read(CLOCK_RS), read(BINDINGS_TS));
    expect(
      drift,
      drift.length === 0
        ? ""
        : `src/shared/bindings.ts 的 TimeRange 与 crates/ta-core/src/clock.rs 不一致：\n` +
            drift.map((d) => `  - ${d}`).join("\n") +
            `\n\n修法：以 clock.rs 为准改 bindings.ts（那是 Rust 实现，是权威）；` +
            `两边都改不许改错一边。`,
    ).toEqual([]);
  });

  it("①b 契约里的 explicit 就是 {explicit:{from_ms,to_ms}}，且不再有 {preset} / {from,to}", () => {
    const declaration = timeRangeDeclaration(read(BINDINGS_TS));
    expect(declaration).toMatch(/\{\s*explicit\s*:\s*\{\s*from_ms\s*:\s*number\s*;\s*to_ms\s*:\s*number\s*\}\s*\}/);
    expect(declaration).not.toMatch(/\bpreset\b/);
    expect(declaration).not.toMatch(/\bfrom\s*:\s*number/);
  });

  it("② 页面真的在用的构造器，产出的 range 都是 Rust 认的形状", () => {
    const bad: string[] = [];
    const check = (label: string, value: unknown): void => {
      if (!isRustTimeRangeShape(value, UNIT_PRESETS, FIELD_KEYS)) {
        bad.push(`${label} → ${JSON.stringify(value)}`);
      }
    };

    // /cost：usage_query 与 usage_export_csv（`q` 与 `query` 两个参数名都发）
    for (const range of ALL_USAGE_RANGES) {
      const q = buildUsageQuery({ ...DEFAULT_COST_FILTER, range });
      check(`buildUsageQuery(range=${range}).range`, q.range);
      const qa = usageQueryArgs(q) as unknown as Record<string, { range: unknown }>;
      check(`usageQueryArgs(range=${range}).q.range`, qa["q"]?.range);
      check(`usageQueryArgs(range=${range}).query.range`, qa["query"]?.range);
      const ea = usageExportArgs(q, "C:/tmp/usage.csv") as unknown as Record<
        string,
        { range: unknown }
      >;
      check(`usageExportArgs(range=${range}).q.range`, ea["q"]?.range);
      check(`usageExportArgs(range=${range}).query.range`, ea["query"]?.range);
    }

    // /dashboard：usage_dashboard
    check("DASHBOARD_RANGE", DASHBOARD_RANGE);
    check("dashboardRangeArgs().range", dashboardRangeArgs().range);

    // /logs：log_query（`range` 是契约字段；真机生效靠 from_ms/to_ms）
    for (const preset of RANGE_PRESETS) {
      check(
        `buildLogQuery(preset=${preset}).range`,
        buildLogQuery({ ...DEFAULT_FILTER, preset })["range"],
      );
    }

    // /settings：最近一条日志的 log_query 入参
    const recent = recentLogQueryArgs(1) as unknown as { query: { range: unknown } };
    check("recentLogQueryArgs(1).query.range", recent.query.range);

    expect(bad, bad.length === 0 ? "" : `以下参数在真机上会被 serde 拒掉：\n  ${bad.join("\n  ")}`).toEqual(
      [],
    );
  });

  it("③ 页面源码里没有把 {preset:…} 当 range 发出去的内联调用点", () => {
    const offenders = productionFiles(SRC_ROOT)
      .flatMap((f) =>
        presetRangeLiterals(read(f)).map(
          (s) => `  ${path.relative(REPO_ROOT, f)}: \`${s}\``,
        ),
      );
    expect(
      offenders.length,
      offenders.length === 0
        ? ""
        : `以下调用点发的是 \`{preset:…}\`，而 Rust 的 TimeRange 没有 \`preset\` 这个变体` +
            `（真机上报 \`unknown variant \\\`preset\\\`\`）：\n${offenders.join("\n")}\n\n` +
            `修法：发裸字符串（"today" / "last7d" / "last30d" / "this_month" / "all"）` +
            `或 {explicit:{from_ms,to_ms}}。`,
    ).toBe(0);
  });

  it("④ 守卫自证有牙：改了名字 / 发了旧形状都必须被抓出来", () => {
    // 合成"Rust 把 Last7d 改名成 Last7Days、TS 没跟"→ 必须判定为漂移
    const originalRust = read(CLOCK_RS);
    const renamedRust = originalRust.replace("    Last7d,", "    Last7Days,");
    expect(renamedRust, "合成用的替换没生效，这条自证就没意义了").not.toBe(originalRust);
    expect(timeRangeDrift(renamedRust, read(BINDINGS_TS)).length).toBeGreaterThan(0);
    expect(isRustTimeRangeShape("last7d", ["last7days"], ["explicit"])).toBe(false);

    // 合成"契约被改回旧形状"→ 必须判定为漂移
    const oldBindings = read(BINDINGS_TS).replace(
      timeRangeDeclaration(read(BINDINGS_TS)),
      'export type TimeRange = { from: number; to: number } | { preset: "today" | "7d" };\n',
    );
    const oldDrift = timeRangeDrift(read(CLOCK_RS), oldBindings);
    expect(oldDrift.length).toBeGreaterThan(0);
    expect(oldDrift.join(" ")).toMatch(/preset|from/);

    // 这次漂移的确切载荷：{preset:…} 与旧契约的 {from,to} 都不是合法形状
    expect(isRustTimeRangeShape({ preset: "last7d" }, UNIT_PRESETS, FIELD_KEYS)).toBe(false);
    expect(isRustTimeRangeShape({ preset: "today" }, UNIT_PRESETS, FIELD_KEYS)).toBe(false);
    expect(isRustTimeRangeShape({ from: 1, to: 2 }, UNIT_PRESETS, FIELD_KEYS)).toBe(false);
    // 合法的两种形状要放行（否则上面那些 false 可能是"一律 false"换来的）
    expect(isRustTimeRangeShape("last7d", UNIT_PRESETS, FIELD_KEYS)).toBe(true);
    expect(
      isRustTimeRangeShape({ explicit: { from_ms: 1, to_ms: 2 } }, UNIT_PRESETS, FIELD_KEYS),
    ).toBe(true);
    // 缺字段 / 字段名错的 explicit 不许放行
    expect(isRustTimeRangeShape({ explicit: { from: 1, to: 2 } }, UNIT_PRESETS, FIELD_KEYS)).toBe(
      false,
    );

    // 源码扫描器本身有牙：合成一行内联的旧写法必须被抓到
    expect(presetRangeLiterals('useCommandQuery("usage_dashboard", { range: { preset: "today" } });'))
      .toHaveLength(1);
    expect(presetRangeLiterals('const x = { range: "today" };\nconst p = { preset: "today" };'))
      .toHaveLength(0);
  });
});

/**
 * 已知的**其它**形状漂移（不在本守卫的断言范围内，写在这里是为了不丢线索）。
 *
 * `usage_query` / `usage_export_csv` 的 `metrics` 字段同样对不上：
 * `ta_store::Metric`（`crates/ta-store/src/usage_store.rs:110-117`）**没有** `rename_all`，
 * 线上值是 `"Requests"` / `"Cost"` / `{"Tokens":"input_cached"}`，而前端发的是
 * `["requests","cost"]`（实测 `unknown variant \`requests\``）。也就是说即使 `range` 修好了，
 * `/cost` 页的 `usage_query` 在真机上**仍然取不到数**。`usage_dashboard` 不吃 `metrics`，
 * 所以它只受 `range` 影响。
 */
export const KNOWN_OTHER_DRIFT = [
  "usage_query/usage_export_csv 的 metrics：Rust `Metric` 是 PascalCase 外部标记（``Requests``/``Cost``/``{Tokens:…}``），前端发 snake_case（``requests``/``cost``）",
] as const;

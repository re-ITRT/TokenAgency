// @vitest-environment node
/**
 * 设计系统守卫 —— **机械化执行 [15 §8] 的四条**（不靠自觉）。
 *
 * 为什么要做成守卫而不是评审时看一眼：这四条都是**静默劣化** ——
 * 多一个 `13px`、多一处 `#333`、多一个 emoji、多一个 `font-weight:700`，
 * 都不会报错、不影响功能，只会让界面一个月后变得不统一。
 * **而"不一致"正是"不高级"的定义**（[15 §8] 原话）。
 *
 * 四条：
 * 1. **我们自己产出的文本里没有 emoji**（[15 §2.1]）；
 * 2. **没有硬编码颜色**（必须走 `theme.palette` / `tokens`，[15 §3]）；
 * 3. **没有刻度外间距**（只能取 `2 4 8 12 16 24 32 48 64`，[15 §3.3]）；
 * 4. **没有 `fontWeight >= 600`**（字重只用 400 / 500，[15 §4.2]）。
 *
 * 边界（[15 §2.2]，别搞反）：**我们永不自己产生 emoji；用户数据里的 emoji 原样透传。**
 * 所以 `mock.ts`（模拟用户数据的机场组名 / 节点名，真实样本就是 `U+1F680 节点选择` 那种）
 * 是**例外**，而且只在这里豁免 —— **检测器绝不进入运行时数据路径**
 * （谁在组件里过滤用户名字，就是比 emoji 更严重的 bug）。
 *
 * 例外只有下面 `EXEMPT_FILES` 里这两条，每条都写了原因；加白名单前先想清楚是不是在掩盖劣化。
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { SPACING_SCALE } from "@/theme";

const SRC_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * 豁免文件（相对 `src/`）。**每加一条都要在评论里写清"为什么它不是我们的文本"。**
 *
 * 现在只剩一条 —— 而且这是好事：**豁免越少，守卫越硬**。
 * （`shared/bindings.ts` 曾经也在列表里，因为它是 gitignored 的生成物位置；
 * 第 11 轮它被入库后就不再是"生成物"，我把它注释里那 4 个自造标记 emoji 换成了文字，
 * 只给唯一一处**用户数据示例**打了 `ta-allow-emoji` 行内标记。）
 */
const EXEMPT_FILES = new Set([
  // 模拟"用户数据"：订阅自带的机场组名/节点名（真实样本含 emoji，[15 §2.2] 要求原样呈现）
  "shared/api/mock.ts",
]);

/**
 * emoji 检测范围 = 明确的 emoji 区块 + 变体选择符 + 键帽。
 *
 * **故意不用 `\p{Extended_Pictographic}`**：它把 `↔` `↕` 之类的箭头也算进去，
 * 而箭头是文档与注释里的排版符号（[15] 自己就大量用 `↔`）。守卫误伤一次，
 * 下一次就会有人加白名单把它整体绕过 —— 那才是真的失去守卫。
 */
const EMOJI_PATTERN =
  /[\u{1F000}-\u{1FAFF}\u{1F1E6}-\u{1F1FF}\u{2600}-\u{27BF}\u{2B00}-\u{2BFF}\u{FE0F}\u{20E3}]/u;

/** `#rgb` / `#rrggbb(aa)` / `rgb(` / `hsl(` —— 颜色必须来自 `tokens` / `theme.palette`。 */
const COLOR_PATTERN = /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(/;

/** 刻度内间距（px）与 MUI `sx` 倍数（px ÷ 4）。`0` = 不设间距，单独放行。 */
const PX_VALUES = new Set<number>([0, ...SPACING_SCALE]);
const MULTIPLIER_VALUES = new Set<number>([0, ...SPACING_SCALE.map((v) => v / 4)]);

/** `fontWeight: 600+`（以及 JSX 的 `<strong>` / `<b>` —— 浏览器默认就是 700）。 */
const BOLD_PATTERN = /fontWeight\s*[:=]\s*["']?([6-9]\d\d)/;
const BOLD_TAG_PATTERN = /<(?:strong|b)[\s/>]/;

/** `padding|margin|gap` 系列属性（`sx` 对象里的写法）。 */
const SPACING_PROP_PATTERN =
  /(?:^|[\s{,(])(padding|paddingX|paddingY|paddingTop|paddingRight|paddingBottom|paddingLeft|margin|marginX|marginY|marginTop|marginRight|marginBottom|marginLeft|gap|rowGap|columnGap|p|px|py|pt|pr|pb|pl|m|mx|my|mt|mr|mb|ml)\s*:\s*([^,;}\n]+)/g;

/** MUI 的 `spacing={n}`（等于 `n × 4px`）。 */
const SPACING_JSX_PATTERN = /spacing=\{([^}]+)\}/g;

type Finding = { file: string; line: number; text: string };

function listSources(dir: string = SRC_ROOT, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      listSources(full, out);
      continue;
    }
    if (/\.tsx?$/.test(entry)) out.push(full);
  }
  return out;
}

function relOf(file: string): string {
  return path.relative(SRC_ROOT, file).split(path.sep).join("/");
}

/** 逐行扫描 `src/**`（可只扫某类后缀），返回命中行。 */
function scan(
  predicate: (line: string, rel: string) => boolean,
  options: { only?: RegExp } = {},
): Finding[] {
  const findings: Finding[] = [];
  for (const file of listSources()) {
    const rel = relOf(file);
    if (EXEMPT_FILES.has(rel)) continue;
    if (options.only !== undefined && !options.only.test(rel)) continue;
    const lines = readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((text, index) => {
      if (predicate(text, rel)) findings.push({ file: rel, line: index + 1, text: text.trim() });
    });
  }
  return findings;
}

function format(findings: Finding[]): string {
  return findings.map((f) => `  src/${f.file}:${f.line}  ${f.text}`).join("\n");
}

/** 从一行里抽出所有 `padding/margin/gap` 的数值，判断是否在刻度内。 */
function offScaleSpacing(line: string): string | null {
  for (const match of line.matchAll(SPACING_PROP_PATTERN)) {
    const raw = (match[2] ?? "").trim();
    // 令牌表达式（`${space.lg}px`）静态解不出来，跳过 —— 它的值本身就来自刻度
    if (raw.includes("${")) continue;
    if (raw.includes("px")) {
      const nums = raw.match(/\d+(?:\.\d+)?/g) ?? [];
      const bad = nums.filter((n) => !PX_VALUES.has(Number(n)));
      if (bad.length > 0) return `${match[1]}: ${raw}`;
      continue;
    }
    // 裸值（含 `compact ? 0 : 1` 这类三元）：每个数字都必须是合法倍数
    const nums = raw.match(/\d+(?:\.\d+)?/g) ?? [];
    const bad = nums.filter((n) => !MULTIPLIER_VALUES.has(Math.abs(Number(n))));
    if (bad.length > 0) return `${match[1]}: ${raw}`;
  }
  for (const match of line.matchAll(SPACING_JSX_PATTERN)) {
    const raw = (match[1] ?? "").trim();
    if (/^-?\d+(?:\.\d+)?$/.test(raw) && !MULTIPLIER_VALUES.has(Math.abs(Number(raw)))) {
      return `spacing={${raw}}`;
    }
  }
  return null;
}

describe("[15 §8] 设计系统守卫（机械检查，违反即红）", () => {
  it("1. 我们自己产出的文本里没有 emoji（用户数据除外）", () => {
    const findings = scan(
      (line) => EMOJI_PATTERN.test(line) && !line.includes("ta-allow-emoji"),
    );
    expect(
      findings,
      `[15 §2.1] 不许自己产生 emoji（状态用圆点/文字，重点用字重）。\n${format(findings)}`,
    ).toEqual([]);
  });

  it("1b. 检测器本身有效（自证：能识出 emoji，且不误伤排版符号）", () => {
    // 用码点构造，避免测试文件自己带 emoji
    const rocket = String.fromCodePoint(0x1f680);
    const warn = `${String.fromCodePoint(0x26a0)}${String.fromCodePoint(0xfe0f)}`;
    const star = String.fromCodePoint(0x2605);
    expect(EMOJI_PATTERN.test(`${rocket} 节点选择`)).toBe(true);
    expect(EMOJI_PATTERN.test(warn)).toBe(true);
    expect(EMOJI_PATTERN.test(star)).toBe(true);
    // 这些是排版符号，不是 emoji —— 误伤会让守卫变成噪音，最后被绕过
    for (const ok of ["规则 -> 页面", "A ↔ B", "今天 — 明天", "12.4M", "≥ 90%", "A · B"]) {
      expect(EMOJI_PATTERN.test(ok), `不该被判成 emoji：${ok}`).toBe(false);
    }
  });

  it("2. 组件里没有硬编码颜色（必须走 tokens / theme.palette）", () => {
    // 扫 **ts 与 tsx**：样式层不止在 .tsx 里（`shared/components/dataGrid.ts` 就是一整层 sx），
    // 只扫 tsx 会让新代码悄悄漏出覆盖面（评审 B-4）。
    // 两处豁免，各有明确的理由：
    //   · `src/theme.ts` —— 它**就是**调色板本身，颜色字面量的唯一合法出口；
    //   · `*.test.*` —— 用字面量**断言文档里的值**（这正是守卫的执法方式），不在运行时渲染路径上。
    const findings = scan(
      (line) =>
        COLOR_PATTERN.test(line) &&
        !line.includes("ta-allow-color") &&
        !line.trimStart().startsWith("*") &&
        !line.trimStart().startsWith("//"),
      { only: /(?<!\.test)\.(?:ts|tsx)$/ },
    ).filter((f) => f.file !== "theme.ts");
    expect(
      findings,
      `[15 §3] 颜色只能从 tokens / theme.palette 取。\n${format(findings)}`,
    ).toEqual([]);
  });

  it("3. 没有刻度外间距（只允许 2/4/8/12/16/24/32/48/64）", () => {
    // 同样扫 ts + tsx（`theme.ts` 一起扫：它里面的 px 也必须在刻度内）
    const findings: Finding[] = [];
    for (const file of listSources()) {
      const rel = relOf(file);
      if (EXEMPT_FILES.has(rel) || !/\.tsx?$/.test(rel)) continue;
      const lines = readFileSync(file, "utf8").split(/\r?\n/);
      lines.forEach((text, index) => {
        if (text.includes("ta-allow-spacing")) return;
        const bad = offScaleSpacing(text);
        if (bad !== null) findings.push({ file: rel, line: index + 1, text: bad });
      });
    }
    expect(
      findings,
      `[15 §3.3] 间距刻度外的值：\n${format(findings)}`,
    ).toEqual([]);
  });

  it("4. 没有 fontWeight >= 600、也没有 <strong>/<b>（字重只用 400/500）", () => {
    const findings = scan(
      (line) => BOLD_PATTERN.test(line) || BOLD_TAG_PATTERN.test(line),
      { only: /(?<!\.test)\.(?:ts|tsx)$/ },
    );
    expect(
      findings,
      `[15 §4.2] 粗体在深色底上会糊；强调用 500。\n${format(findings)}`,
    ).toEqual([]);
  });
});

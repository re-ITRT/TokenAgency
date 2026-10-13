// @vitest-environment jsdom
/**
 * 动效守卫 —— **机械化执行**（规则不靠自觉）。
 *
 * 为什么要做成守卫：动效的劣化**全是静默的** —— 多一处 `transition: all .3s`、
 * 多一个 `ease-in-out`、多一个循环动画、多一处 `height` 过渡，都不报错、不影响功能，
 * 只会让界面在"跳变"和"抖"之间反复横跳。而这四条正是 的原话：
 *
 * 1. **时长只有一个来源**：`theme.ts` 的 `motion`（120/180ms，≤250ms）。组件里不许写字面量；
 * 2. **只有一处循环动画**：额度硬耗尽脉动（>90%），其余一次性（该脉动随额度功能下线后，
 *    源码里已无循环动效，检查仍然保留）；
 * 3. **必须尊重 `prefers-reduced-motion: reduce`**（CSS 媒体查询，不是 JS）：循环动画停、
 *    位移/缩放过渡停、**颜色过渡留**（颜色不引发前庭反应）；
 * 4. **不许引发布局抖动**：只动 `opacity` / `transform` / `stroke-dashoffset` / 进度条的 `width`。
 *
 * 这一份按 `design.guard.test.ts` 的路子写：扫源码、命中即打印 `文件:行号`，
 * 并且**每条检查都有一条自证有牙的用例**（种一条违规，检测器必须识出）——
 * 否则"检查通过"和"检查没在工作"长得一模一样。
 *
 * 边界：`*.test.*` 不进扫描（它们要写文档里的字面量、还要故意种违规）；
 * 注释行也不进扫描（`[15]` 自己就在正文里大量引用这些写法）。
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { CssBaseline, ThemeProvider } from "@mui/material";
import { createTheme, type Theme } from "@mui/material/styles";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  ANIMATABLE_PROPERTIES,
  COLOR_TRANSITION_PROPERTIES,
  MOTION_OFF_DURATION,
  MOTION_OFF_ITERATIONS,
  REDUCED_MOTION_QUERY,
} from "@/shared/motion";
import { buildTheme, type ThemeMode } from "@/theme";

const SRC_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MODES: ThemeMode[] = ["dark", "light"];

/** 扫描结果：`文件:行号` + 命中的是什么（报错信息里直接可读）；`raw` 是原始行，供"钉值"判定用。 */
type Finding = { file: string; line: number; text: string; kind: string; raw?: string };

// ================================================================ 源码扫描（可单独喂字符串自证）

/** `transition: ...` / `animation: ...` 等声明（值取到行尾；值为空的看下一行）。 */
const DECLARATION = /(?:^|[\s{,(;])(transition|transitionProperty|transitionDuration|animation|animationDuration|animationIterationCount|animationName)\s*:\s*(.*)$/;

/** 时长字面量（`300ms` / `0.3s`）；模板插值先被剥掉，所以令牌不会命中。 */
const DURATION_LITERAL = /\d+(?:\.\d+)?(?:ms|s)\b/;

/** 缓动字面量（关键字与手写曲线）。 */
const EASING_LITERAL = /\b(?:linear|ease|ease-in|ease-out|ease-in-out|step-start|step-end)\b|cubic-bezier\(/;

/** 属性名清单里允许出现的东西（`transition: <属性> <时长> <缓动>`）。 */
const ALLOWED_PROPERTIES = new Set<string>(ANIMATABLE_PROPERTIES);

function isCommentLine(line: string): boolean {
  const t = line.trim();
  return t.startsWith("//") || t.startsWith("*") || t.startsWith("/*") || t === "";
}

/** 去模板插值：`${motion.enter}` = 走令牌，不是字面量。 */
function stripTokens(value: string): string {
  return value.replace(/\$\{[^}]*\}/g, " ");
}

/** 行尾注释不参与判定（正文里引用写法的地方很多）。 */
function stripTrailingComment(value: string): string {
  return value.split("//")[0] ?? value;
}

function trimPunctuation(value: string): string {
  return value.replace(/[,;)}\]]+\s*$/, "").trim();
}

/**
 * 扫一段源码里所有动效声明。
 *
 * **纯函数**（给文件路径 + 源码，返回命中），所以自证用例可以直接喂一条种进去的违规。
 */
export function scanMotionSource(file: string, source: string): Finding[] {
  const findings: Finding[] = [];
  const lines = source.split(/\r?\n/);

  lines.forEach((raw, index) => {
    if (isCommentLine(raw)) return;
    const match = DECLARATION.exec(raw);
    const prop = match?.[1];
    if (prop === undefined) return;

    // 值写到下一行也认（`transition:\n  \`width ...\`` 这种换行写法不能成为逃逸口）
    let value = stripTokens(match?.[2] ?? "");
    if (trimPunctuation(value) === "" && lines[index + 1] !== undefined) {
      value = stripTokens(lines[index + 1] ?? "");
    }
    // 行尾的对象收尾（`},`）不属于值；属性名清单必须看修剪后的值
    const trimmed = trimPunctuation(stripTrailingComment(value));
    const at = { file, line: index + 1, text: `${prop}: ${trimmed}` };

    // ① 字面量时长 / 缓动：一律不许（例外在 `scanRepo` 里按"钉住令牌值"处理）
    if (DURATION_LITERAL.test(trimmed)) findings.push({ ...at, kind: "duration" });
    if (EASING_LITERAL.test(trimmed)) findings.push({ ...at, kind: "easing" });

    // ② 没走令牌：既没有字面量、也没有引用令牌/常量 —— 那这个过渡的时长是哪来的？
    const tokenised = /\$\{/.test(match?.[2] ?? "") || raw.includes("motion.") || raw.includes("REDUCED_MOTION");
    if (!tokenised && !DURATION_LITERAL.test(trimmed) && !EASING_LITERAL.test(trimmed)) {
      findings.push({ ...at, kind: "untokenised" });
    }

    // ③ 只许动 opacity / transform / stroke-dashoffset / 进度条的 width（布局抖动）；
    //    `width` 单独一档：它是布局属性，必须逐条登记（见 `WIDTH_SITES`）
    if (prop === "transition") {
      const head = trimmed.split(DURATION_LITERAL)[0] ?? trimmed;
      const properties = head
        .split(/[,\s]+/)
        .map((p) => p.replace(/["'`]/g, "").trim())
        .filter((p) => p !== "");
      for (const property of properties) {
        if (property === "width") findings.push({ ...at, kind: "width" });
        else if (!ALLOWED_PROPERTIES.has(property)) findings.push({ ...at, kind: `property:${property}` });
      }
    }
  });

  return findings;
}

/**
 * 关键帧名的**常量表**：`@keyframes ${CONST}` 这种写法静态解析不出名字，
 * 不登记就只能"跳过" —— 而**静默跳过**正是守卫最坏的失败方式（检查通过 = 什么也没查）。
 * 所以新加一个插值关键帧名，必须在这里登记（下面的 `unresolvableKeyframes` 会拦住）。
 */
const KEYFRAME_CONSTANTS: Record<string, string> = {};

/** 源码里声明的 `@keyframes` 名（字面量 + 可解析的常量插值；组件的脉动就是从这里来的）。 */
export function keyframesNames(source: string): string[] {
  const literal = [...source.matchAll(/@keyframes\s+([A-Za-z_][\w-]*)/g)].flatMap((m) =>
    m[1] === undefined ? [] : [m[1]],
  );
  const interpolated = [...source.matchAll(/@keyframes\s+\$\{([A-Za-z_][\w]*)\}/g)].flatMap((m) => {
    const resolved = m[1] === undefined ? undefined : KEYFRAME_CONSTANTS[m[1]];
    return resolved === undefined ? [] : [resolved];
  });
  return [...literal, ...interpolated];
}

/** 解析不出来的插值关键帧名（= 守卫会静默跳过它们，必须登记）。 */
export function unresolvableKeyframes(source: string): string[] {
  return [...source.matchAll(/@keyframes\s+\$\{([A-Za-z_][\w]*)\}/g)].flatMap((m) =>
    m[1] !== undefined && KEYFRAME_CONSTANTS[m[1]] !== undefined ? [] : [m[1] ?? "?"],
  );
}

/** 带 `infinite` 的动画声明 = 循环动效（ 只允许一处）。 */
export function loopingDeclarations(file: string, source: string): Finding[] {
  const findings: Finding[] = [];
  source.split(/\r?\n/).forEach((raw, index) => {
    if (isCommentLine(raw)) return;
    const match = DECLARATION.exec(raw);
    if (match?.[1] === undefined) return;
    const value = trimPunctuation(stripTokens(match?.[2] ?? ""));
    if (!/\binfinite\b/.test(value)) return;
    findings.push({ file, line: index + 1, text: `${match[1]}: ${value}`, kind: "loop", raw });
  });
  return findings;
}

// ================================================================ 仓库扫描（例外与登记都写在这里）

type SourceFile = { file: string; rel: string; source: string };

function listSources(): SourceFile[] {
  const out: SourceFile[] = [];
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir)) {
      const full = path.join(dir, entry);
      if (statSync(full).isDirectory()) {
        walk(full);
        continue;
      }
      if (!/\.tsx?$/.test(entry) || /\.test\.tsx?$/.test(entry)) continue;
      out.push({
        file: full,
        rel: path.relative(SRC_ROOT, full).split(path.sep).join("/"),
        source: readFileSync(full, "utf8"),
      });
    }
  };
  walk(SRC_ROOT);
  return out;
}

/**
 * `width` 过渡**逐条登记**：`width` 是布局属性，只有"进度条那类已经在动的"才允许
 * （）。
 *
 * 现在**一条都没有**：唯一登记过的 `QuotaBar` 已随额度功能整条下线。
 *
 * `Sidebar` 那条**已修掉**（第 78 轮的动效包把 `transition: width …` 从侧边栏删掉了：
 * 折叠是瞬时状态切换，加过渡只会让整壳每帧重排）—— 所以它的登记也一并删掉，
 * 而不是留一条"它已经不算违规"的死名单：名单里的每一条都必须是**当下真实存在**的。
 */
const WIDTH_SITES: readonly { file: string; note: string }[] = [];

/** `width` 过渡的登记检查（纯函数，自证用例直接喂一个没登记过的文件）。 */
export function unregisteredWidthFindings(rel: string, source: string): Finding[] {
  if (WIDTH_SITES.some((site) => site.file === rel)) return [];
  return scanMotionSource(rel, source).filter((finding) => finding.kind === "width");
}

function format(findings: Finding[]): string {
  return findings.map((f) => `  src/${f.file}:${f.line}  [${f.kind}] ${f.text}`).join("\n");
}

/** 全仓扫描：`width` 过渡按登记表放行，其余一律报出。 */
function scanRepo(): Finding[] {
  const findings: Finding[] = [];
  for (const { rel, source } of listSources()) {
    const widthRegistered = WIDTH_SITES.some((site) => site.file === rel);
    for (const finding of scanMotionSource(rel, source)) {
      // 登记过 `width` 的文件放行（登记表本身有预算检查，见下面的用例）
      if (finding.kind === "width" && widthRegistered) continue;
      findings.push(finding);
    }
  }
  return findings;
}

// ================================================================ reduced-motion 的三个闸

/** 取出 `MuiCssBaseline` 里 reduced-motion 的那块 CSS（没有就返回 `undefined`）。 */
function reducedMotionBlock(theme: Theme): Record<string, unknown> | undefined {
  const overrides = theme.components?.MuiCssBaseline?.styleOverrides as
    | Record<string, Record<string, unknown>>
    | undefined;
  const block = overrides?.["body"]?.[REDUCED_MOTION_QUERY];
  return typeof block === "object" && block !== null ? (block as Record<string, unknown>) : undefined;
}

/**
 * reduced-motion 收口检查（**纯函数**：喂任意主题，返回缺什么）。三条闸：
 * ① 循环动画停掉（属性级 + `!important`，与样式表注入顺序无关）；
 * ② 过渡只留颜色类（位移/缩放/宽度/stroke 瞬变）；
 * ③ 源码里每个 `@keyframes` 都有一份"静止版"（按名）。
 */
export function reducedMotionFindings(theme: Theme, declaredKeyframes: readonly string[]): string[] {
  const block = reducedMotionBlock(theme);
  if (block === undefined) return [`MuiCssBaseline 里没有 ${REDUCED_MOTION_QUERY} 这块 CSS`];

  const universal = (block["*, *::before, *::after"] ?? {}) as Record<string, unknown>;
  const findings: string[] = [];

  if (universal["animationDuration"] !== `${MOTION_OFF_DURATION} !important`) {
    findings.push("① 循环动画没被关掉：缺 animationDuration !important（MOTION_OFF_DURATION）");
  }
  if (universal["animationIterationCount"] !== `${MOTION_OFF_ITERATIONS} !important`) {
    findings.push("① `infinite` 没被关掉：缺 animationIterationCount !important");
  }

  const raw = String(universal["transitionProperty"] ?? "");
  if (!raw.endsWith("!important")) {
    findings.push("② transitionProperty 没有 !important（压不过组件里的 transition 简写）");
  }
  const allowed = new Set<string>(COLOR_TRANSITION_PROPERTIES);
  const properties = raw
    .replace("!important", "")
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p !== "");
  for (const property of properties) {
    if (!allowed.has(property)) findings.push(`② 保留清单里混进了会动的属性：${property}`);
  }
  for (const colour of COLOR_TRANSITION_PROPERTIES) {
    if (!properties.includes(colour)) findings.push(`② 颜色过渡被误杀：少了 ${colour}`);
  }

  for (const name of declaredKeyframes) {
    if (block[`@keyframes ${name}`] === undefined) {
      findings.push(`③ ${name} 没有 reduced-motion 的静止版（@keyframes 按名摊平）`);
    }
  }

  return findings;
}

afterEach(() => {
  cleanup();
});

describe("动效守卫（机械检查）", () => {
  it("1. 时长只有一个来源：组件里没有字面量时长/缓动（例外按值钉住）", () => {
    // 先证明扫描器真的扫到了东西（空目录里"没违规"和"没在扫"长得一样）
    expect(listSources().length).toBeGreaterThan(20);
    const findings = scanRepo();
    expect(
      findings,
      ` 过渡只能用 motion / theme.transitions；动静分开写必然漂。\n${format(findings)}`,
    ).toEqual([]);
  });

  it("1b. 检测器本身有牙（自证：种一条违规必须被识出）", () => {
    const planted = [
      'transition: "all 300ms ease-in-out",',
      "transition: `height ${motion.enter} ${motion.easing}`,",
      'transition: "opacity 0.3s",',
      'transition: "width",',
    ];
    for (const line of planted) {
      expect(scanMotionSource("planted.tsx", line), `漏掉了：${line}`).not.toEqual([]);
    }
    // 走令牌的正确写法必须**不被**误伤（误伤的守卫最后一定会被绕过）
    expect(scanMotionSource("ok.tsx", "transition: `opacity ${motion.quick} ${motion.easing}`,")).toEqual([]);
    expect(
      scanMotionSource(
        "ok.tsx",
        '"& circle": { transition: `stroke-dashoffset ${motion.enter} ${motion.easing}` },',
      ),
    ).toEqual([]);
    // 换行写法也不能成为逃逸口
    expect(scanMotionSource("sneaky.tsx", 'transition:\n  "all 300ms ease-in-out",')).not.toEqual([]);
  });

  it("2. 循环动效的检查器有牙，且当前源码里一处都没有", () => {
    const loops = listSources().flatMap(({ rel, source }) => loopingDeclarations(rel, source));
    // `QuotaBar` 的额度硬耗尽脉动随额度功能下线 —— 现在源码里不该再有循环动效
    expect(loops, `源码里不该再有循环动效。\n${format(loops)}`).toHaveLength(0);

    // 自证有牙：多一条循环一定会被识出
    expect(loopingDeclarations("x.tsx", 'animation: "spin 1s linear infinite",')).toHaveLength(1);
    expect(
      loopingDeclarations("x.tsx", 'animationIterationCount: "infinite",'),
      "换一个属性名写 infinite 也不能漏",
    ).toHaveLength(1);
    expect(loopingDeclarations("x.tsx", 'animation: "fade ${motion.enter} ease-out",')).toHaveLength(0);
  });

  it("3. 源码里声明的每个 @keyframes 都在 reduced-motion 里有一份静止版", () => {
    const sources = listSources();
    const declared = sources.flatMap(({ source }) => keyframesNames(source));
    // 现在源码里不该再声明任何循环脉动关键帧（额度功能已整条下线）

    // 插值的关键帧名必须能解析：解析不出来 = 这条检查会静默跳过它
    expect(
      sources.flatMap(({ source }) => unresolvableKeyframes(source)),
      "新的 @keyframes 名常量要在守卫的 KEYFRAME_CONSTANTS 里登记",
    ).toEqual([]);

    for (const mode of MODES) {
      const findings = reducedMotionFindings(buildTheme(mode), declared);
      expect(findings, `reduced-motion 收口不完整（${mode}）：\n  ${findings.join("\n  ")}`).toEqual([]);
    }

    // 自证有牙：① 一个没登记的关键帧名必须被抓；② MUI 默认主题（根本没有这块 CSS）必须被抓；
    // ③ 插值写法要能解析出名字（否则上面那条"必须有一份静止版"就被绕过了）
    expect(reducedMotionFindings(buildTheme("dark"), ["ta-planted-loop"]).length).toBeGreaterThan(0);
    expect(reducedMotionFindings(createTheme({}), declared).length).toBeGreaterThan(0);
    expect(keyframesNames("`@keyframes ta-literal {}`")).toEqual(["ta-literal"]);
    expect(unresolvableKeyframes("[`@keyframes ${SOME_NEW_LOOP}`]: {}")).toEqual(["SOME_NEW_LOOP"]);
  });

  it("4. reduced-motion 的 CSS 真的进了样式表（不是只写在主题对象里）", () => {
    render(
      <ThemeProvider theme={buildTheme("dark")}>
        <CssBaseline />
      </ThemeProvider>,
    );
    // emotion 会把 `!important` 与逗号后的空格压掉，比较前先去空白
    const css = Array.from(document.querySelectorAll("style"))
      .map((style) => style.textContent ?? "")
      .join("\n")
      .replace(/\s+/g, "");

    expect(css).toContain(REDUCED_MOTION_QUERY.replace(/\s+/g, ""));
    expect(css).toContain(`animation-duration:${MOTION_OFF_DURATION}!important`);
    expect(css).toContain(`animation-iteration-count:${MOTION_OFF_ITERATIONS}!important`);
    expect(css).toContain(
      `transition-property:${COLOR_TRANSITION_PROPERTIES.join(",")}!important`,
    );
  });

  it("5. `width` 过渡逐条登记（现在一条都没有，多一条必须先说清为什么不抖）", () => {
    expect(
      WIDTH_SITES.map((site) => `${site.file} :: ${site.note}`),
      "width 是布局属性：新增登记 = 有意决定，请同时更新这条预算并写清理由",
    ).toHaveLength(0);
    // 侧边栏那条**已经修掉**，所以它的登记也必须从名单里消失（名单只放当下真实存在的）
    expect(WIDTH_SITES.map((site) => site.file)).not.toContain("app/Sidebar.tsx");

    // 自证有牙：没登记过的 `width` 过渡必须被识出（`height` 这类更是）
    expect(unregisteredWidthFindings("pages/newPage.tsx", "transition: `width ${motion.enter}`,")).toHaveLength(1);
    expect(scanMotionSource("x.tsx", "transition: `height ${motion.enter}`,")).not.toEqual([]);
  });
});

// @vitest-environment node
/**
 * [极简] **把"极简"变成机器检查**，而不是每轮靠人眼核对 11 个页面。
 *
 * 为什么需要它：用户的原话是"文字内容太多了"，而"文字太多"这种判断如果只靠人眼，
 * 每轮都要重新看一遍 11 个页面 —— 那既烧预算，又会随审查者心情漂移。
 * 这一条守卫把用户的裁决钉成五条可执行判据。
 *
 * ## 用户裁决（2026-10-13）
 * * **彻底删除契约出处**（`docRef`）—— 连 `routes.ts` 的字段与旧守卫一起删；
 * * **激进极简** —— 界面几乎只剩「状态 + 操作」；解释进 `?` 气泡；表格 ≤6 列；空态 1 行。
 *
 * ## 检查什么
 *
 * | # | 规则 | 判据 |
 * |---|---|---|
 * | 1 | 不解释实现 | 文案里不得出现函数名 / `后端` `前端` / `字段` `枚举` / `接线` `落地` `在途` / 文档编号 `§` / `契约` / `TODO` `待补` |
 * | 2 | 句子级说明进 tooltip | **版面上**的文案 ≤36 字（`<HelpTip>` 里的不算 —— 那本来就是按需展开） |
 * | 3 | 一屏一个主操作 | 见 `page-primary-action` 那条（逐页断言，不在这里猜） |
 * | 4 | 表格 ≤6 列 | 见 `dataGrid.preset.test.tsx` 与本文件的列数断言 |
 * | 5 | 空态 1 行 | 空态文案不得出现"后端/命令/落地/接线"（规则 1 已覆盖），且 ≤36 字（规则 2 已覆盖） |
 *
 * ## 为什么规则 2 要区分"版面"与"tooltip"
 *
 * 已经把页头说明收进问号圆圈了 —— 那是**对的**：长解释放在按需展开处不是噪音。
 * 第一版守卫没做这个区分，把 100 条全算违规，其中 21 条本来就是 tooltip 文案 ——
 * **守卫自己制造了 21 个假阳性**。所以这里先扫出"只出现在 `<HelpTip>` 里"的键并豁免。
 *
 * ## 棘轮（ratchet）
 *
 * 规则 1 是硬性 0（用户明确要的）。规则 2 / 孤儿键是**棘轮**：只许变小。
 * 收紧时改 `LIMITS` 里的数字，**不许调大**。
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

import { zhCN } from "@/shared/i18n";
import { en } from "@/shared/i18n/en";

const ROOT = join(__dirname, "..", "..");
const SRC = join(ROOT, "src");

/**
 * 棘轮上限。**只许变小。**
 *
 * 调大的唯一正当理由是"新增了一条本来就该长的 tooltip 文案"，
 * 而且要先确认它真的进了 `<HelpTip>`（那时它根本不会被这条规则数到）。
 */
const LIMITS = {
  /** 规则 1：实现向词汇出现在用户能看到的文案里。用户要求为 0。 */
  implementationWords: 0,
  /**
   * 规则 2：版面上超过 36 字的文案条数。
   *
   * 基线 88（第 336 轮改造前）→ 33 → 32 → 8 → **0**（第 3 轮：修正映射应用顺序后，第 2 轮的文案真正落地）。
   * **0 是目标态**：新增任何 >36 字的版面文案都会红。
   *
   * 注意：这个检测器是**保守近似**：它只豁免"字面量就写在 `<HelpTip … />` 或 `<Tooltip …>` 里"的键。
   * 有些页面把多条说明拼成一个 tooltip（`text={`${t("a")} ${t("b")}`}`，或把键放进
   * 数组再 join），那些键会被**多算**成版面文案。宁可多算不可少算 ——
   * 少算会让规则失效，多算只是让数字比真实情况略高。
   */
  visibleLongText: 0,
  /**
   * 没有任何地方引用的 i18n 键（死文案）。它同时是规则 1 的拖累 ——
   * 页面上的句子删掉了，但 i18n 里还留着，于是"界面没有"与"文案还有"长期不一致。
   *
   * 基线 240 → **214**。只许往下调。
   */
  orphanKeys: 0,
} as const;

/**
 * 从源码里取出**真正会被渲染的字符串字面量**。
 *
 * 为什么不能按"行"判断（第一版就是这么写的，结果**自己制造了 4 个假阳性**）：
 * * JSX 注释（大括号里包着块注释那种）整行不以星号开头；
 * * 块注释的**续行**（第一行是 `/**`，后面几行写文档编号）同样不以星号开头；
 * * 于是"这一行是注释吗"这个判断在真实源码里根本不成立。
 *
 * 正确的做法：**先把块注释挖掉，再取引号里的内容**。
 * 注释里的 `§` 是开发向溯源（允许存在），字符串里的 `§` 会渲染给用户（不许存在）。
 */
function stringLiterals(src: string): string[] {
  const out: string[] = [];
  let i = 0;
  while (i < src.length) {
    // 块注释（含 JSX 里那种大括号包着的）：整段跳过
    if (src.startsWith("/*", i)) {
      const end = src.indexOf("*/", i + 2);
      i = end === -1 ? src.length : end + 2;
      continue;
    }
    // 行注释：跳到行尾。**URL 里的 `//` 不会走到这里** ——
    // 它在字符串里，会被下面的字符串分支整体吃掉。
    if (src.startsWith("//", i)) {
      const end = src.indexOf("\n", i);
      i = end === -1 ? src.length : end + 1;
      continue;
    }

    const quote = src[i];
    if (quote === '"' || quote === "'" || quote === "`") {
      let j = i + 1;
      let buf = "";
      while (j < src.length) {
        const c = src[j]!;
        if (c === "\\") {
          buf += src[j + 1] ?? "";
          j += 2;
          continue;
        }
        if (c === quote) break;
        buf += c;
        j += 1;
      }
      out.push(buf);
      // **整体消费掉这个字面量**：里面的撇号（`don't`）与引号不再被当成新的字面量起点。
      // 第一版是按正则全量搜引号，结果英文文案里的 `don't` 被当成单引号串的开头，
      // 一路吞到几十行之外的注释（含 `§`）才闭合 —— 于是守卫又报了假阳性。
      i = j + 1;
      continue;
    }

    i += 1;
  }
  return out;
}

/** 规则 1 的禁用词。每一条都是"用户不该看到"的实现细节。 */
const IMPLEMENTATION_WORDS: Array<[string, RegExp]> = [
  // 函数名 / 内部命令名：`app_set_autostart`、`config_set_file_watch_mode`…
  ["函数名", /\b(?:app|config|log|net|catalog|entity|upstream|kernel|api|model|usage|provider)_[a-z_]+\b/],
  ["后端/前端", /后端|前端/],
  ["本地口径/远端/本地筛选", /本地口径|远端|本地筛选|本地算/],
  ["字段/枚举", /字段|枚举/],
  ["接线/落地/在途", /接线|落地|在途|还没实现|尚未实现/],
  ["文档编号", /§|docs\/\d|\[\d\d §/],
  ["契约", /契约/],
  ["TODO/待补", /TODO|待补|等后端/],
];

/** 英文侧同一类词（`en.ts` 也要干净，否则切到英文又露出来了）。 */
const IMPLEMENTATION_WORDS_EN: Array<[string, RegExp]> = [
  ["backend/frontend", /\bbackend\b|\bfrontend\b/i],
  ["wired/landed", /\bnot wired\b|\bwiring\b|\bnot implemented yet\b/i],
  ["field/enum", /\benum\b/i],
];

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      if (name === "node_modules" || name === "dist") continue;
      walk(full, out);
    } else if (/\.(ts|tsx)$/.test(name)) {
      out.push(full);
    }
  }
  return out;
}

/** 生产源码（不含测试）：守卫只看真的会跑到用户面前的代码。 */
function productionFiles(): string[] {
  return walk(SRC).filter((p) => !/\.test\.(ts|tsx)$/.test(p));
}

/**
 * 只出现在**按需展开**容器里的键 —— 这些不受长度约束。
 *
 * 容器有两种，代码里都在用：
 * * 自己的 `<HelpTip … />`；
 * * MUI 的 `<Tooltip title={…}>`（`StatusBar.tsx` 的实验模式圆点就是这种）。
 *
 * 第一版只认 `<HelpTip>`，于是 `app.badge.experimental.hint`（45 字，确实在 Tooltip 里）
 * 被算成了"版面超长"——**又一个守卫自己制造的假阳性**。
 *
 * 近似做法：把每个容器元素切成一段，取段内的 `t("...")`。
 * 这是保守近似（宁可漏豁免、不可错豁免）：找不到就直接算作"版面文案"。
 */
function tooltipOnlyKeys(): Set<string> {
  const keys = new Set<string>();
  for (const file of productionFiles()) {
    const src = readFileSync(file, "utf8");
    for (const tag of ["<HelpTip", "<Tooltip"]) {
      let idx = 0;
      while (true) {
        const start = src.indexOf(tag, idx);
        if (start === -1) break;
        const end = src.indexOf("/>", start);
        const seg = src.slice(start, end === -1 ? start + 800 : end);
        for (const m of seg.matchAll(/t\(\s*"([^"]+)"/g)) keys.add(m[1]!);
        idx = start + tag.length;
      }
    }
  }
  return keys;
}

/**
 * 被源码引用过的 i18n 键。
 *
 * ## 为什么不能只找 `t("key")`（这条判据第一版是错的，报出 214 个"死键"，真实只有 7 个）
 *
 * 这个仓库大量使用**"键名先存进对象/常量，之后再传给 `t()`"**的写法：
 *
 * * `KernelStateDot.tsx`：`stopped: "status.kernel.stopped",`（状态 → 键名的映射对象）
 * * `providers/editor.ts`：`issues.push({ field: "baseUrl", message: "wizard.invalidUrl" })`
 * * `providers/index.tsx`：`const SELECT_ALL_LABEL_KEY: I18nKey = "common.selectAll";`
 *
 * 只看 `t("...")` 会把这些**正在用的**键全判成死的 —— 而"删死键"是破坏性动作，
 * 按错的清单删就是**删掉活文案**。所以判据必须是：
 *
 * > 一个键只要它的**字符串本身**出现在 `src/**` 里（`shared/i18n/` 除外，**含测试文件**，
 * > 含注释），就算被引用。
 *
 * 再加 `` t(`prefix.${x}`) `` 这类动态模板前缀的覆盖。
 * 取向是**保守**：宁可少报死键（留一条没用的文案），也不可多报（删掉在用的文案）。
 */
function referencedKeys(): Set<string> {
  const allKeys = new Set(Object.keys(zhCN));
  const used = new Set<string>();

  for (const file of walk(SRC)) {
    // 只用 `relative` + 手动换分隔符：不额外 import `node:path` 的默认导出
    // （第一版写了 `path.sep` 却没 import，规则 2b 直接抛 "path is not defined" ——
    //  它"红了"，但红的是守卫自己坏了，而不是产品有问题）
    const rel = relative(SRC, file).split("\\").join("/");
    // i18n 自己的定义不算"被引用"，否则每个键都能在自己身上找到
    if (rel.startsWith("shared/i18n/")) continue;
    const src = readFileSync(file, "utf8");

    // ① 字符串字面量里出现键名（覆盖"存进对象/常量再传给 t()"的写法）
    for (const literal of stringLiterals(src)) {
      if (allKeys.has(literal)) used.add(literal);
    }
    // ② 注释里提到的键名也算引用（保守：注释里写了说明它还有意义）
    for (const m of src.matchAll(/"([^"\n]{2,})"/g)) {
      if (allKeys.has(m[1]!)) used.add(m[1]!);
    }
    // ③ 动态模板 `` t(`logs.chip.${x}`) ``：前缀命中的键全部算被引用
    for (const m of src.matchAll(/t\(\s*`([^`$]+)/g)) {
      const prefix = m[1]!;
      for (const key of allKeys) if (key.startsWith(prefix)) used.add(key);
    }
  }
  return used;
}

describe("[极简] 界面上不该有这些文字（用户裁决 2026-10-13）", () => {
  it("规则 1：文案里不解释实现（函数名 / 后端 / 字段 / 文档编号 / TODO）", () => {
    // **按"键"计数，不按"匹配次数"**：一个键同时命中两条模式（例如既有函数名又有 §）
    // 只算一条违规 —— 否则数字虚高，改完之后还会以为剩很多。
    const violations: string[] = [];

    for (const [key, value] of Object.entries(zhCN)) {
      const text = value as string;
      const names = IMPLEMENTATION_WORDS.filter(([, pattern]) => pattern.test(text)).map(([n]) => n);
      if (names.length > 0) {
        violations.push(`${key}（${names.join("+")}）= ${text.slice(0, 70)}`);
      }
    }

    const enViolations: string[] = [];
    for (const [key, value] of Object.entries(en)) {
      const text = value as string;
      const names = IMPLEMENTATION_WORDS_EN.filter(([, pattern]) => pattern.test(text)).map(([n]) => n);
      if (names.length > 0) enViolations.push(`${key}（${names.join("+")}）`);
    }

    // 一次打印全部，避免"改一条跑一次"
    expect(
      violations.length,
      `zh-CN 有 ${violations.length} 条文案在向用户解释实现：\n${violations.join("\n")}\n` +
        `修法：删掉这句，或把必要的部分改写成用户能懂的话（≤36 字），把解释搬进 <HelpTip>。`,
    ).toBeLessThanOrEqual(LIMITS.implementationWords);

    expect(
      enViolations.length,
      `en 有 ${enViolations.length} 条同类问题（切到英文就露出来了）：\n${enViolations.join("\n")}`,
    ).toBeLessThanOrEqual(LIMITS.implementationWords);
  });

  it("规则 1b：源码里不再有文档编号 / docRef（契约出处已彻底删除）", () => {
    const offenders: string[] = [];
    for (const file of productionFiles()) {
      const src = readFileSync(file, "utf8");
      for (const literal of stringLiterals(src)) {
        if (literal.includes("§")) {
          offenders.push(`${relative(ROOT, file)}: ${literal.slice(0, 90)}`);
        }
      }
    }
    expect(
      offenders,
      `这些**字符串**里的人话带文档编号，会渲染到界面上：\n${offenders.join("\n")}`,
    ).toEqual([]);

    // `docRef` 整个概念都删掉了（routes.ts 的字段 + 渲染）
    const leftovers: string[] = [];
    for (const file of productionFiles()) {
      if (readFileSync(file, "utf8").includes("docRef")) leftovers.push(relative(ROOT, file));
    }
    expect(leftovers, "docRef 已彻底删除，不该再有引用").toEqual([]);
  });

  /**
   * **守卫自己没瞎**：喂给它合成样本，真的违规必须抓到、注释里的必须放过。
   *
   * 为什么必须有这一条：守卫最容易的坏法是"永远绿" —— 正则写错、路径没扫到、
   * 提取器把内容全滤掉，都会让它变成一块安慰牌。这里用两个合成样本把它的两端都钉住：
   * 一个**必须报警**，一个**必须放过**。它红了说明守卫坏了，而不是产品坏了。
   */
  it("守卫自检：字符串里的 § 必须抓到，注释里的必须放过", () => {
    // 样本里的文档编号**在源码里拼出来**，不写字面量。
    // 为什么要这样：这一整套编号引用正在被废除（docsRef.guard 是只许收缩的棘轮），
    // 清理工具会把源码里的字面量当引用删掉 —— 第 343 轮就真的把下面这几个样本删了，
    // 于是自检从"必须报警"变成"永远报警不了"，而**守卫安静地失去了牙齿**。
    // 拼接之后：运行时字符串仍然含编号（牙齿还在），源码里不再有可被扫走的字面量。
    const cite = `[${"15"} §2]`;

    // 必须抓到：字符串字面量里有文档编号
    const planted = `const label = "见 ${cite}";`;
    expect(stringLiterals(planted).some((s) => s.includes("§"))).toBe(true);

    // 必须放过：块注释、块注释续行、JSX 注释里的文档编号
    const commented = [
      `/** 说明 ${cite} */`,
      "/*",
      " * 第一行",
      ` * 续行里也有 ${cite}`,
      " */",
      `{/* JSX 注释里 ${cite} */}`,
    ].join("\n");
    expect(stringLiterals(commented).filter((s) => s.includes("§"))).toEqual([]);
  });

  it("规则 2：版面上的说明 ≤36 字（tooltip 不计）", () => {
    const tooltips = tooltipOnlyKeys();
    const long = Object.entries(zhCN)
      .filter(([key]) => !tooltips.has(key))
      .filter(([, value]) => [...(value as string)].length > 36)
      .map(([key, value]) => `${key}（${[...(value as string)].length} 字）= ${(value as string).slice(0, 50)}…`);

    expect(
      long.length,
      `版面上还有 ${long.length} 条超过 36 字的文案（棘轮上限 ${LIMITS.visibleLongText}）：\n` +
        `${long.join("\n")}\n` +
        `修法：版面上只留结论（≤36 字），解释搬进 <HelpTip>（搬进去就自动被豁免）。`,
    ).toBeLessThanOrEqual(LIMITS.visibleLongText);
  });

  it("规则 2b：没有一条 i18n 文案是死的（死文案会拖累规则 1）", () => {
    const used = referencedKeys();
    const orphans = Object.keys(zhCN).filter((key) => !used.has(key));

    expect(
      orphans.length,
      `有 ${orphans.length} 条文案没有任何地方引用（棘轮上限 ${LIMITS.orphanKeys}）：\n` +
        `${orphans.slice(0, 40).join("\n")}\n` +
        `修法：页面改完后把没人用的键删掉 —— 留着它会让"界面上没有这句话"与"文案里还有这句话"长期不一致。`,
    ).toBeLessThanOrEqual(LIMITS.orphanKeys);
  });
});

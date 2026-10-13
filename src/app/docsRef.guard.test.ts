// @vitest-environment node
/**
 * 守卫：`docs/NN` 引用**只许减少，不许增加**（棘轮）。
 *
 * # 为什么要废除这套引用制度
 *
 * 它已经腐烂了，而且是**静默地**烂：
 *   * 代码里引用 `docs/24`，但 `docs/24-*.md` **根本不存在**；
 *   * 两处文档都声称"MIT 全文已登记在 `THIRD_PARTY_NOTICES.md`"，
 *     而那个文件里**从来没有过那段全文** —— 义务被指向了，却从没落下来。
 *
 * 一个悬空的引用比没有引用更糟：它给人"细节有处可查"的错觉，点进去却是 404。
 *
 * # 为什么是棘轮而不是"禁止"
 *
 * 全仓约 2500 处引用、235 个文件，一次改完不现实（且已三次证明批量正则会改坏东西）。
 * 棘轮把**当前数量钉住**：每改掉一批就把基线调低，**一处增加就红**。
 * 这样清理可以慢慢做，但**不可能反弹**。
 *
 * 注意 `[NN §x]` 这种**短式**（不带 `docs/`）也算 —— 第一版工具漏了它，
 * 于是把 `[15 §7]` 当成了普通文本。
 */
import { readFileSync, readdirSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

/** 仓库根（本文件在 `src/app/` 下）。与 `repoMap.guard.test.ts` 同一写法。 */
const ROOT = resolve(__dirname, "..", "..");

const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "dist",
  ".ta-tmp",
  "target",
  "target-e3",
  "target-e5",
]);
/**
 * 扫描范围：**不含 `docs/`**。
 *
 * 口径要和验收标准对齐：目标第 2 条管的是"`docs/` 之外不再出现 `docs/NN` 形式的引用"；
 * 保留下来的文档之间**互相引用编号是允许的**（`docs/README.md` 那份索引本来就要写编号）。
 * `docs/` 自身的规模由下面第二条棘轮管。
 */
const SCAN_DIRS = ["src", "crates", "src-tauri", "scripts"];
const EXT = new Set([".rs", ".ts", ".tsx", ".mjs", ".js", ".ps1", ".sh", ".yaml", ".yml", ".json", ".md"]);

/**
 * 棘轮基线（第 342 轮实测，2026-10-14）。
 *
 * **只许往下调。** 改掉一批引用后，把这两个数字改小；守卫会逼着你确认
 * "确实只减不增"。
 */
const BASELINE = { files: 224, lines: 1647 };

/**
 * `docs/` 篇数的棘轮基线（第 342 轮实测 52 篇）。
 *
 * 目标的另一半：文档编号本身就是接口，**篇数不缩，引用就清不干净**。
 * 同样只许往下调；删一篇就把这里调低一格，目标 16。
 */
const DOCS_BASELINE = { files: 16 };

/** 命中一处引用即算一行。两个形态都要认。 */
export function countsAsRef(line: string): boolean {
  return /docs\/\d{2}|\[\d{2} §/.test(line);
}

/** 统计一份文本里的引用行数（导出是为了自检可以直接喂样本）。 */
export function countRefLines(text: string): number {
  return text.split("\n").filter(countsAsRef).length;
}

function walk(dir: string, out: string[] = []): string[] {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    const p = join(dir, e.name);
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) walk(p, out);
    } else if (EXT.has(extname(e.name))) {
      out.push(p);
    }
  }
  return out;
}

/** 扫全仓，返回 { files, lines, worst }。 */
function scanRepo(): { files: number; lines: number; worst: Array<[number, string]> } {
  let files = 0;
  let lines = 0;
  const worst: Array<[number, string]> = [];
  for (const dir of SCAN_DIRS) {
    for (const p of walk(join(ROOT, dir))) {
      // `docs/` 自身的**文件名**带编号，但文档互相引用才是要清的；这里连 docs/ 一起扫，
      // 因为 docs/README.md 的索引最终也要重写（验收标准第 3 条）。
      const rel = relative(ROOT, p).replace(/\\/g, "/");
      let text: string;
      try {
        text = readFileSync(p, "utf8");
      } catch {
        continue;
      }
      const n = countRefLines(text);
      if (n > 0) {
        files += 1;
        lines += n;
        worst.push([n, rel]);
      }
    }
  }
  worst.sort((a, b) => b[0] - a[0]);
  return { files, lines, worst };
}

describe("[docsRef] docs/NN 引用制度的棘轮", () => {
  it("计数函数本身有牙（自检：正负样本都要认对）", () => {
    // 正面：两种形态都要被数到
    expect(countsAsRef("// 见 docs/28 §1.2 说明了关系翻转")).toBe(true);
    expect(countsAsRef("it(\"[docs/30 §3.18] 筛选区的主操作固定在底部\", () => {})")).toBe(true);
    expect(countsAsRef("`docs/11-internal-contracts.md` 是契约")).toBe(true);
    // 关键：**短式**也必须认（不带 docs/），否则这一整类会漏掉
    expect(countsAsRef("* 模型目录页（[07 §4] / [docs/28]）的纯逻辑层")).toBe(true);
    // 反面：这些不该被误判
    expect(countsAsRef("// 端口 39100 到 39299 是 lane 区间")).toBe(false);
    expect(countsAsRef("const REVISION = 46;")).toBe(false);
    expect(countsAsRef("// §5 的白名单在别处")).toBe(false);
    // 一份合成文本：3 处命中
    const synthetic = [
      "// 见 docs/28",
      "let x = 1;",
      "// [15 §7] 的规则",
      "// 普通注释",
      "it(\"[docs/32] 契约给了就显示\", () => {});",
    ].join("\n");
    expect(countRefLines(synthetic)).toBe(3);
  });

  it("引用总数不得超过棘轮基线（只许减少）", () => {
    const { files, lines, worst } = scanRepo();
    const top = worst
      .slice(0, 8)
      .map(([n, f]) => `  ${String(n).padStart(4)}  ${f}`)
      .join("\n");
    const why =
      `引用制度在反弹。当前 ${files} 个文件 / ${lines} 行，基线是 ` +
      `${BASELINE.files} 个文件 / ${BASELINE.lines} 行。\n` +
      `要么把新加的引用清掉，要么（只在你确实改掉了一批时）把 BASELINE 调低。\n` +
      `当前最密的文件：\n${top}`;
    // 注意：这里**不能**写成 expect({files, lines}).toEqual({files, lines}) —— 那是恒真断言，
    // 等于没断言。必须拿实测值去比基线。
    expect(files, why).toBeLessThanOrEqual(BASELINE.files);
    expect(lines, why).toBeLessThanOrEqual(BASELINE.lines);
  });

  it("基线不是空转（扫到的规模必须远大于 0，否则说明扫描路径写错了）", () => {
    const { files, lines } = scanRepo();
    // 防"对着空集断言而恒绿"：路径写错时 files/lines 会变成 0，上一条反而更容易通过。
    expect(lines, "只扫到 0 处引用 —— 扫描路径或排除规则多半写错了").toBeGreaterThan(1000);
    expect(files).toBeGreaterThan(100);
  });

  it("docs/ 的文件数不得超过棘轮基线（目标 ≤16）", () => {
    // 另一半：引用制度的源头是"文档编号本身就是接口"。编号从 51 个文件缩到 16 个，
    // 引用才会真正变少。这个数字同样**只许往下调**。
    const docs = readdirSync(join(ROOT, "docs")).filter((f) => f.endsWith(".md"));
    expect(
      docs.length,
      `docs/ 有 ${docs.length} 篇，基线 ${DOCS_BASELINE.files}，目标 16。` +
        `删掉一篇就把 DOCS_BASELINE.files 调低一格；**增加即红**。`,
    ).toBeLessThanOrEqual(DOCS_BASELINE.files);
    expect(docs.length, "扫到 0 篇说明路径写错了").toBeGreaterThan(3);
  });
});

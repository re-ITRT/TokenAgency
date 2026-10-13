// @vitest-environment node
/**
 * 守卫：`docs/NN` 引用**只许减少，不许增加**（棘轮）。
 *
 * # 为什么要废除这套引用制度
 *
 * 它已经腐烂了，而且是**静默地**烂：
 *   * 代码里引用 ，但  **根本不存在**；
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
 * 于是把 `` 当成了普通文本。
 */
import { readFileSync, readdirSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

/** 仓库根（本文件在 `src/app/` 下）。与 `repoMap.guard.test.ts` 同一写法。 */
const ROOT = resolve(__dirname, "..", "..");

const SKIP_DIRS = new Set([
  // `docs/` 自己要排除：验收标准第 2 条管的是"`docs/` 之外"，
  // 保留下来的文档之间互相引用编号是允许的（索引就是靠编号导航）。
  "docs",
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
// **扫全仓**，不是"我恰好想到的那几个目录"。
// 曾经只列了四个子目录，于是根目录的 `Cargo.toml` 从未被计入（实测又漏了几行）。
const SCAN_DIRS = ["."];
// **只排除二进制与生成物，不列“文本白名单”。**
// 白名单已经漏过三次（.toml/.sql、根目录文件、.py/.nsh）——
// “全仓”的定义不该取决于我恰好想到哪些扩展名。
const BINARY_EXT = new Set([
  ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".svg",
  ".exe", ".dll", ".so", ".dylib", ".pdb", ".lib", ".obj",
  ".zip", ".gz", ".tar", ".7z", ".mmdb", ".db", ".sqlite", ".sqlite3",
  ".woff", ".woff2", ".ttf", ".otf", ".pdf", ".lock", ".map",
  ".bak", ".tmp", ".log",
]);

/**
 * 棘轮基线（第 342 轮实测，2026-10-14）。
 *
 * **只许往下调。** 改掉一批引用后，把这两个数字改小；守卫会逼着你确认
 * "确实只减不增"。
 */
const BASELINE = { files: 0, lines: 0 };

/**
 * `docs/` 篇数的棘轮基线（第 342 轮实测 52 篇）。
 *
 * 目标的另一半：文档编号本身就是接口，**篇数不缩，引用就清不干净**。
 * 同样只许往下调；删一篇就把这里调低一格，目标 16。
 */
const DOCS_BASELINE = { files: 16 };

/**
 * 功能性路径实参的**极小例外清单**。
 *
 * 这些不是"引用"，而是**函数实参**：守卫靠它去读它核对的那篇契约。
 * 删掉它没有语法错误，`cargo check` 与 `tsc` 也全过 —— **只有 `cargo test` 会红**
 * （第 343 轮实测）。所以它们必须留，但要留得**有账可查**：
 * 下面有一条测试断言"全仓命中这个模式的行数不超过 3"，清单一旦变大就红。
 */
const FUNCTIONAL_PATH = /(?:read_repo_file|repo_doc)\(\s*"docs\/\d{2}/;

/**
 * 夹具里的文档路径 —— **在源码里拼出来**，不写字面量。
 *
 * 本文件也在棘轮的扫描范围里；写死的字面量会让棘轮**永远到不了 0**
 * （实测：本文件自己贡献了 3 行）。理由与 `uiMinimalism.guard` 的自检完全一样。
 */
const DOC_OF = (stem: string) => ["docs", stem].join("/");

/** 命中一处引用即算一行。两个形态都要认；**功能性路径实参不算**。 */
export function countsAsRef(line: string): boolean {
  if (FUNCTIONAL_PATH.test(line)) {
    return false;
  }
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
    } else if (!BINARY_EXT.has(extname(e.name.toLowerCase()))) {
      out.push(p);
    }
  }
  return out;
}

/** 扫全仓，返回 { files, lines, worst }。 */
function scanRepo(): {
  files: number;
  lines: number;
  scanned: number;
  worst: Array<[number, string]>;
} {
  let files = 0;
  let lines = 0;
  let scanned = 0;
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
      scanned += 1;
      const n = countRefLines(text);
      if (n > 0) {
        files += 1;
        lines += n;
        worst.push([n, rel]);
      }
    }
  }
  worst.sort((a, b) => b[0] - a[0]);
  return { files, lines, scanned, worst };
}

describe("[docsRef] docs/NN 引用制度的棘轮", () => {
  it("计数函数本身有牙（自检：正负样本都要认对）", () => {
    // 夹具里的文档编号**在源码里拼出来**，不写字面量。
    //
    // 为什么：这套编号引用正在被废除，清理工具会把源码里的字面量当引用删掉。
    // 第 343 轮就真的把下面这几个样本删了 —— 自检于是从"必须命中"变成"永远不命中"，
    // **守卫安静地失去了牙齿**，而它自己还绿着（因为夹具残缺后断言的是"有没有命中"）。
    // 拼接之后：运行时样本仍然含编号（牙齿还在），源码里不再有可被扫走的字面量，
    // 本文件自身也不再被棘轮计数。
    const ref = (n: string, sec?: string) => `docs/${n}${sec ? ` §${sec}` : ""}`;
    const short = (n: string, sec: string) => `[${n} §${sec}]`;

    // 正面：两种形态都要被数到
    expect(countsAsRef(`// 见 ${ref("28", "1.2")} 说明了关系翻转`)).toBe(true);
    expect(countsAsRef(`it("${short("30", "3.18")} 筛选区的主操作固定在底部", () => {})`)).toBe(true);
    expect(countsAsRef("`" + DOC_OF("11-internal-contracts.md") + "` 是契约")).toBe(true);
    // 关键：**短式**也必须认（不带 docs/），否则这一整类会漏掉
    expect(countsAsRef(`* 模型目录页（${short("07", "4")} / ${ref("28")}）的纯逻辑层`)).toBe(true);
    // 反面：这些不该被误判
    expect(countsAsRef("// 端口 39100 到 39299 是 lane 区间")).toBe(false);
    expect(countsAsRef("const REVISION = 46;")).toBe(false);
    expect(countsAsRef("// §5 的白名单在别处")).toBe(false);
    // 一份合成文本：3 处命中
    const synthetic = [
      `// 见 ${ref("28")}`,
      "let x = 1;",
      `// ${short("15", "7")} 的规则`,
      "// 普通注释",
      `it("${ref("32")} 契约给了就显示", () => {});`,
    ].join("\n");
    expect(countRefLines(synthetic)).toBe(3);

    // 功能性路径实参**不算引用**（它是函数实参）；但**同类写法不带调用时必须算**，
    // 否则这条豁免会被滥用成新的"引用制度"。
    // 夹具同样在源码里拼出来 —— 否则它自己会变成第 4 条"功能性路径"（实测被上面那条断言抓到过）。
    const fixture = `let doc = common::read_repo_file("${DOC_OF("14-observability.md")}")`;
    expect(countsAsRef(fixture)).toBe(false);
    expect(countsAsRef("// 见 " + DOC_OF("14-observability.md") + " 的字段白名单")).toBe(true);
  });

  it("功能性路径实参的例外清单不许变大（当前 3 行）", () => {
    let n = 0;
    for (const dir of SCAN_DIRS) {
      for (const p of walk(join(ROOT, dir))) {
        const text = readFileSync(p, "utf8");
        n += text.split("\n").filter((line) => FUNCTIONAL_PATH.test(line)).length;
      }
    }
    expect(
      n,
      `功能性路径实参从 3 行变成了 ${n} 行。这是**故意留的例外** —— 守卫必须读它核对的那篇契约，` +
        `删了它只有 cargo test 会红。但它只许有那几处；新增之前先问自己：这是"读文档"，` +
        `还是又长出了新的引用制度？`,
    ).toBeLessThanOrEqual(3);
    expect(n, "一处都没有说明扫描写错了").toBeGreaterThan(0);
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

  it("扫描确实覆盖了仓库（防空转）", () => {
    const { scanned, lines } = scanRepo();
    // 防空转**不能**用"引用数 > 某个阈值"：本目标就是把引用降到 0，
    // 那种断言会在接近 0 时自己红（实测：降到 500 以下就误报过一次）。
    // 该防的是"扫描路径或排除规则写错，导致一个文件都没扫到"。
    expect(scanned, "一个文件都没扫到 —— 扫描路径或排除规则写错了").toBeGreaterThan(200);
    expect(lines, "引用数不可能是负数").toBeGreaterThanOrEqual(0);
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

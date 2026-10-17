import { readdirSync, readFileSync } from "node:fs";
import { extname, join, relative } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * 守卫：**注释里不许留下"删引用"造成的残句**。
 *
 * # 为什么需要它
 *
 * 有一次批量清理把注释里的文档引用删掉了，句子却留在原地：
 * 模块头变成「契约 + 句号」、「分工（依据 + 两个顿号 + 冒号）」这种读不通的样子。
 * 编译、clippy、其它守卫**全绿** —— 因为它们只看代码，不看句子读不读得通。
 * 于是这些残句一路进了模块头，成了读者看到的第一行。
 *
 * # 判据（只扫注释行，且要求"见"出现在**子句开头**）
 *
 * 残句的形状是确定的：被删的引用要么紧跟在冒号后面，要么紧跟「见 / 按」，
 * 于是留下「冒号挨句号」「见挨句号」「括号里只剩一个见」这类**标点挨着标点**的空洞。
 * 而正常的"常见/可见/看得见"里，「见」前面是汉字，不是标点 —— 所以不会误伤。
 *
 * # 纪律
 *
 * 加新规则前先确认它**不误伤**：在 `自检` 里同时给出"该红的"和"该绿的"样例。
 * 这条守卫上一版就死在"规则只对空文件成立"上。
 */
const ROOT = join(__dirname, "..", "..");
const SOURCE_EXT = new Set([".rs", ".ts", ".tsx", ".mjs", ".ps1", ".nsh", ".sql"]);
const SKIP_DIRS = new Set(["node_modules", "dist", ".git", "target", ".ta-tmp"]);

/** 残句的形状。见：/（见）/：。/（ 的 /（）、 等。 */
const RESIDUE = [
  { name: "冒号后接标点（契约：。 / 分工：、）", re: /[：:]\s*[。，；、）]/ },
  // 「见」必须出现在**子句开头**（行首或标点/括号之后）才算残句。
  { name: "见 后接标点（见。/（见）/，见））", re: /(?:^|[\s，。；：（(、—–-])\s*见\s*[。，）)]/ },
  { name: "双顿号（依据、、）", re: /[、]\s*[、]/ },
  { name: "全角空括号（（））", re: /（\s*）|（\s*的/ },
  { name: "空格悬空（在 的 / 按 的）", re: /(?:在|按|据|于)\s+的/ },
];

/** 生成物：会被 `scripts/gen-bindings.ps1` 整份覆盖，改它没意义（要改生成器或源头注释）。 */
const GENERATED = new Set(["src/shared/bindings.ts"]);

/** 只认注释行：代码里的字符串不受影响。 */
function isComment(line: string): boolean {
  const t = line.trim();
  if (t.startsWith("#!")) return false;
  return t.startsWith("//") || t.startsWith("*") || t.startsWith("--") || t.startsWith("#");
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
    } else if (SOURCE_EXT.has(extname(e.name).toLowerCase())) {
      out.push(p);
    }
  }
  return out;
}

type Hit = { file: string; line: number; rule: string; text: string };

/** 扫全仓的注释行。`frozenMigrations` 排除：那些文件是**不可变字节**，改一个字节就炸所有老用户的库。 */
function scan(): { hits: Hit[]; scannedLines: number } {
  const hits: Hit[] = [];
  let scannedLines = 0;
  for (const dir of ["crates", "src-tauri", "src", "scripts"]) {
    for (const p of walk(join(ROOT, dir))) {
      const rel = relative(ROOT, p).replace(/\\/g, "/");
      if (rel.startsWith("crates/ta-store/migrations/")) continue;
      if (GENERATED.has(rel)) continue;
      let text: string;
      try {
        text = readFileSync(p, "utf8");
      } catch {
        continue;
      }
      text.split("\n").forEach((line, i) => {
        if (!isComment(line)) return;
        scannedLines += 1;
        for (const { name, re } of RESIDUE) {
          if (re.test(line)) {
            hits.push({ file: rel, line: i + 1, rule: name, text: line.trim().slice(0, 100) });
            break;
          }
        }
      });
    }
  }
  return { hits, scannedLines };
}

describe("注释完整性", () => {
  it("注释里没有'删引用'留下的残句", () => {
    const { hits, scannedLines } = scan();
    // 防空转：规则写错导致一行都没扫到时，下面会"全绿"。
    expect(scannedLines, "一条注释都没扫到 —— 目录或扩展名写错了").toBeGreaterThan(2000);
    expect(
      hits,
      hits.length
        ? `发现 ${hits.length} 处残句（删掉引用后没补句子）：\n` +
            hits
              .slice(0, 20)
              .map((h) => `  ${h.file}:${h.line}  [${h.rule}]  ${h.text}`)
              .join("\n") +
            "\n修法：把引用换成语义自足的句子（注释必须自足到不需要翻文档）。"
        : "",
    ).toEqual([]);
  });

  it("规则本身有效（该红的红、该绿的不误伤）", () => {
    // 用拼接造样例：守卫自己的源码里不许出现残句（否则它会扫到自己）。
    const bad = ["契约" + "：。", "见" + "。", "（" + "见" + "）", "依据" + "、" + "、", "（" + "）", "在" + " 的"];
    for (const sample of bad) {
      expect(
        RESIDUE.some((r) => r.re.test(sample)),
        `样例应当被判为残句：${sample}`,
      ).toBe(true);
    }
    // 正常中文里的「见」前面是汉字，不该被误判。
    const good = ["很常见。", "对外可见。", "点名字就看得见。", "契约：只存事实，不存策略。", "排序依据"];
    for (const sample of good) {
      expect(
        RESIDUE.some((r) => r.re.test(sample)),
        `正常句子被误判成残句：${sample}`,
      ).toBe(false);
    }
  });
});

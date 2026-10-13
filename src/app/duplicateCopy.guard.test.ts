/**
 * [docs/30 §1 判据 1] **同一屏里同一个事实不许说两遍** —— 机械守卫。
 *
 * 为什么需要它：这条判据最容易"看着像做到了、实际没做到"（第 117 轮列头漏删就是例子），
 * 而人工核对 11 个页面等于每轮烧掉半轮预算。这里把它变成机器检查。
 *
 * ## 检查什么
 *
 * 在**同一个页面文件**里，找**同一段长文案字面量**（≥ 12 个中文字符）出现**两次以上**，
 * 且两次相距 **< 200 行** —— 这是"很可能同屏"的保守近似。
 *
 * ## 为什么不查 i18n 键的重复使用
 *
 * 第 155 轮试过：`cost.export.title` 在一页里用了 4 次，**全都正当**
 * （1 个区块标题 + 3 个不同动作的 toast 标题，不同屏同时出现）。
 * 键的重复 ≠ 同屏重复；**字面量**的近距离重复才几乎一定是。所以只查字面量。
 *
 * ## 允许清单
 *
 * 确有正当重复（例如同一句在两个互斥分支里）时，把**文件相对路径**加进 `ALLOW`，
 * 并写清理由 —— 允许清单只许变短。
 */
import { readFileSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

const ROOT = join(__dirname, "..", "..");
const PAGES = join(ROOT, "src", "pages");

/** 页面文件清单（不递归 index.tsx 之外的目录以外的东西，够用）。 */
const FILES = [
  "apikeys/index.tsx",
  "cost.tsx",
  "dashboard/index.tsx",
  "logs/index.tsx",
  "models/index.tsx",
  "network/index.tsx",
  "network/nodes.tsx",
  "providers/index.tsx",
  "providers/detail.tsx",
  "settings/index.tsx",
];

/** 见文件头：只允许"同一屏不可能同时看到"的正当重复。**只许变短**。 */
const ALLOW: readonly string[] = [];

/** 一段够长、够具体的中文文案（短词如「时间」「模型」不算）。 */
const LITERAL = /"([^"\\\n]*[\u4e00-\u9fa5][^"\\\n]*)"/g;
const MIN_CHARS = 12;
const MAX_DISTANCE = 200;

type Hit = { text: string; lines: number[] };

function longLiterals(file: string): Hit[] {
  const source = readFileSync(join(PAGES, file), "utf8");
  const byText = new Map<string, number[]>();
  source.split("\n").forEach((line, index) => {
    // 注释里出现的不算（注释不是界面）
    const stripped = line.replace(/\/\/.*$/, "").replace(/\/\*.*?\*\//g, "");
    for (const match of stripped.matchAll(LITERAL)) {
      const text = match[1]!;
      if (text.length < MIN_CHARS) continue;
      const lines = byText.get(text) ?? [];
      lines.push(index + 1);
      byText.set(text, lines);
    }
  });
  return [...byText.entries()]
    .filter(([, lines]) => lines.length > 1 && lines[1]! - lines[0]! < MAX_DISTANCE)
    .map(([text, lines]) => ({ text, lines }));
}

describe("[docs/30 §1 判据 1] 同一屏不许把同一句话说两遍（机械检查）", () => {
  it("页面里没有近距离重复的长文案", () => {
    const offenders = FILES.filter((file) => !ALLOW.includes(file))
      .map((file) => ({ file, hits: longLiterals(file) }))
      .filter((entry) => entry.hits.length > 0)
      .map(
        (entry) =>
          `${relative(ROOT, join(PAGES, entry.file))}:\n` +
          entry.hits
            .map((hit) => `  行 ${hit.lines.join(" / ")} 重复：${hit.text}`)
            .join("\n"),
      );
    expect(offenders, `下面这些页面把同一句话说了两遍：\n${offenders.join("\n")}`).toEqual([]);
  });
});

/**
 * 一次性审计：**同一屏里重复出现的标签**（用户第 89 轮："删掉重复标签"）。
 *
 * 判据：同一个 `t("key")` 在**同一个页面文件**里出现 ≥2 次，且这些出现**不在**
 * 问号 / 无障碍名称 / 错误原文 / 占位符里（那些是"另一个语境"，不算重复）。
 *
 * 为什么这是极简的抓手：一屏里同一个标签说 N 遍，是"看起来乱"的第一来源；
 * 而它又最难靠肉眼在 review 里发现（每处在各自的小组件里都"合理"）。
 */
import fs from "node:fs";
import path from "node:path";

const files = [];
(function walk(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p);
    else if (e.name.endsWith(".tsx") && !e.name.includes(".test.")) files.push(p);
  }
})("src/pages");

const rows = [];
for (const file of files) {
  const text = fs.readFileSync(file, "utf8");
  const counts = new Map();
  text.split(/\r?\n/).forEach((line, index) => {
    if (/help|Help|aria|title|description|placeholder|notice|hint/i.test(line)) return;
    for (const m of line.matchAll(/t\("([^"]+)"/g)) {
      const list = counts.get(m[1]) ?? [];
      list.push(index + 1);
      counts.set(m[1], list);
    }
  });
  const dupes = [...counts.entries()]
    .filter(([, lines]) => lines.length >= 2)
    .map(([key, lines]) => ({ key, times: lines.length, lines }))
    .sort((a, b) => b.times - a.times);
  if (dupes.length > 0) {
    rows.push({ file: file.replace(/\\/g, "/"), dupes });
  }
}

rows.sort(
  (a, b) =>
    b.dupes.reduce((n, d) => n + d.times, 0) - a.dupes.reduce((n, d) => n + d.times, 0),
);

let total = 0;
for (const row of rows) {
  const occurrences = row.dupes.reduce((n, d) => n + d.times, 0);
  total += occurrences;
  console.log(`${String(occurrences).padStart(3)}  ${row.file}`);
  for (const d of row.dupes.slice(0, 5)) {
    console.log(`       x${d.times}  ${d.key}  @${d.lines.join(",")}`);
  }
}
console.log(`\n合计 ${total} 处重复渲染，涉及 ${rows.length} 个页面。`);

/**
 * 一次性审计：找出**内联铺在版面上**的长文案（≥40 字），按页面统计。
 *
 * 为什么要有这一步：用户第 89 轮要求「优化每一个界面，走极简风格」。凭感觉删不行 ——
 * 先把"哪些字是真的铺在版面上"量出来，再按量排序逐屏清。
 *
 * 判据：某个 `t("key")` 的 zh-CN 值 ≥40 字，且这一行**不在**问号 / 无障碍 / 错误原文 /
 * 占位符 / 提示（help|aria|title|description|error|placeholder|notice|hint）里 → 算内联。
 */
import fs from "node:fs";
import path from "node:path";

const zh = fs.readFileSync("src/shared/i18n/zh-CN.ts", "utf8");
const vals = new Map();
for (const line of zh.split(/\r?\n/)) {
  const m = line.match(/^\s*"([^"]+)":\s*"(.*)",?\s*$/);
  if (m) vals.set(m[1], m[2]);
}

const files = [];
(function walk(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p);
    else if (e.name.endsWith(".tsx") && !e.name.includes(".test.")) files.push(p);
  }
})("src/pages");

const offenders = [];
for (const file of files) {
  const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
  lines.forEach((line, index) => {
    for (const m of line.matchAll(/t\("([^"]+)"/g)) {
      const value = vals.get(m[1]);
      if (value === undefined || value.length < 40) continue;
      if (/help|Help|aria|title|description|Description|error|Error|placeholder|notice|hint/i.test(line)) {
        continue;
      }
      offenders.push({
        file: file.replace(/\\/g, "/"),
        line: index + 1,
        len: value.length,
        key: m[1],
        text: value.slice(0, 44),
      });
    }
  });
}

const perFile = {};
for (const o of offenders) perFile[o.file] = (perFile[o.file] ?? 0) + 1;

console.log(`内联长文案（zh 值 >= 40 字）共 ${offenders.length} 处，分布在 ${Object.keys(perFile).length} 个页面：`);
for (const [file, count] of Object.entries(perFile).sort((a, b) => b[1] - a[1])) {
  console.log(`  ${String(count).padStart(3)}  ${file}`);
}
console.log("\n最长的 14 条（长度 / 位置 / 键）：");
for (const o of offenders.sort((a, b) => b.len - a.len).slice(0, 14)) {
  console.log(`  ${o.len}  ${o.file}:${o.line}  ${o.key}\n        ${o.text}`);
}

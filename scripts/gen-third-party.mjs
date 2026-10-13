#!/usr/bin/env node
/**
 * 生成完整的第三方依赖清单 → `THIRD-PARTY-DEPS.md`。
 *
 * 为什么要有它：`THIRD-PARTY-NOTICES.md` 是**人工判断**的那份（哪些许可要求额外做什么），
 * 而这份是**机械汇总**（谁、什么版本、什么许可）。两者分工不能混：
 * 机器能列出许可，但**不能**替我们判断"这个许可要不要公开源码 / 要不要署名"。
 *
 * 为什么不用 `cargo-about` / `license-checker`：它们要联网装工具（`cargo install` 是分钟级
 * 编译），而这里需要的信息在 `cargo metadata` 与各包的 package.json 里都有。
 * 少一个工具就少一个"干净 clone 跑不动"的理由。
 *
 * 用法：node scripts/gen-third-party.mjs
 */
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, "THIRD-PARTY-DEPS.md");

/** 这些是宽松许可：列出来就行，不需要额外动作。 */
const PERMISSIVE = /^(MIT|Apache-2\.0|BSD|ISC|Unlicense|Zlib|CC0|0BSD|MIT-0|Unicode-3\.0|CDLA-Permissive)/i;
/** 这些不是：生成器只做标记，判断留给人。 */
const NEEDS_REVIEW = /(MPL|LGPL|GPL|AGPL|CDDL|EPL|EUPL|SSPL|BSL|CC-BY|Commons Clause)/i;

function rustDeps() {
  // maxBuffer 要给足：workspace 的 metadata 有 3.5MB 左右。
  const r = spawnSync("cargo", ["metadata", "--format-version", "1"], {
    cwd: ROOT,
    maxBuffer: 512 * 1024 * 1024,
    encoding: "utf8",
  });
  if (r.status !== 0) {
    throw new Error(`cargo metadata 失败（status=${r.status}）：${(r.stderr || "").slice(0, 400)}`);
  }
  return JSON.parse(r.stdout).packages.map((p) => ({
    name: p.name,
    version: p.version,
    license: p.license ?? "(未声明)",
    repo: (p.repository ?? "").replace(/^git\+/, ""),
  }));
}

function npmDeps() {
  const root = join(ROOT, "node_modules");
  if (!existsSync(root)) return [];
  const out = [];
  const dirs = [];
  for (const e of readdirSync(root, { withFileTypes: true })) {
    if (!e.isDirectory()) continue;
    if (e.name.startsWith("@")) {
      for (const s of readdirSync(join(root, e.name), { withFileTypes: true })) {
        if (s.isDirectory()) dirs.push(join(root, e.name, s.name));
      }
    } else {
      dirs.push(join(root, e.name));
    }
  }
  for (const d of dirs) {
    const pj = join(d, "package.json");
    if (!existsSync(pj)) continue;
    try {
      const j = JSON.parse(readFileSync(pj, "utf8"));
      out.push({
        name: j.name ?? "(未命名)",
        version: j.version ?? "?",
        license: j.license ?? (j.licenses ? JSON.stringify(j.licenses) : "(未声明)"),
        repo: "",
      });
    } catch {
      /* 坏 package.json 跳过：它不影响清单的可用性 */
    }
  }
  return out;
}

function section(title, rows) {
  const lines = [`### ${title}（${rows.length} 个）`, "", "| 组件 | 版本 | 许可 |", "| --- | --- | --- |"];
  for (const r of rows.sort((a, b) => a.name.localeCompare(b.name))) {
    lines.push(`| \`${r.name}\` | ${r.version} | ${r.license} |`);
  }
  lines.push("");
  return lines.join("\n");
}

const rust = rustDeps();
const npm = npmDeps();

const rustReview = rust.filter((r) => NEEDS_REVIEW.test(r.license) || !PERMISSIVE.test(r.license));
const npmReview = npm.filter((r) => NEEDS_REVIEW.test(r.license) || !PERMISSIVE.test(r.license));

const doc = [
  "# 第三方依赖清单（机器生成）",
  "",
  "> ⚠️ **本文件由 `node scripts/gen-third-party.mjs` 生成，不要手改。**",
  "> 需要人工判断的许可义务在 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)；",
  "> 这里只是机械汇总：机器能列出许可，但**不能**替我们判断它要求我们做什么。",
  "",
  `生成时间：${new Date().toISOString()}`,
  "",
  `统计：Rust ${rust.length} 个，npm ${npm.length} 个。`,
  "",
  "## ⚠️ 需要人工过一遍的",
  "",
  rustReview.length === 0 && npmReview.length === 0
    ? "（无）"
    : [
        "以下条目的许可**不是**常见的宽松许可，或未声明。新增的都必须在 THIRD-PARTY-NOTICES.md 里交代清楚：",
        "",
        "| 生态 | 组件 | 版本 | 许可 |",
        "| --- | --- | --- | --- |",
        ...rustReview.map((r) => `| Rust | \`${r.name}\` | ${r.version} | ${r.license} |`),
        ...npmReview.map((r) => `| npm | \`${r.name}\` | ${r.version} | ${r.license} |`),
      ].join("\n"),
  "",
  "## Rust",
  "",
  section("全部 Rust 依赖", rust),
  "## npm",
  "",
  section("全部 npm 依赖", npm),
].join("\n");

writeFileSync(OUT, doc, "utf8");
console.log(`  ✅ ${OUT}`);
console.log(`     Rust ${rust.length} 个 / npm ${npm.length} 个`);
console.log(`     需要人工判断：Rust ${rustReview.length} 个，npm ${npmReview.length} 个`);

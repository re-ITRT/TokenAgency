// @vitest-environment node
/**
 * 守卫：**源码里说的"某命令还没注册"必须是真的没注册**。
 *
 * ## 为什么需要它（这不是文风问题，是诚实问题）
 *
 * 本项目的页面/i18n 里到处写着"某某命令尚未注册，所以这里不放按钮" —— 这些话
 * 在写下的那一刻是**诚实的边界说明**（比假按钮好），但它们是**对注册表的状态断言**。
 * 注册表一旦前进（本轮后端从 38 条涨到 104 条），这些话就**同时变成假话**，而且没有任何
 * 机械检查会发现 —— 页面会继续对用户说"这个功能后端还没有"，而它其实早就有了。
 * 真机端到端验证就抓到过这一类（设置页写着 `app_get_autostart` 尚未注册，而 exe 里有这个符号）。
 *
 * 规则一句话：**出现"未注册/尚未注册/还没注册"这类断言的同一行里，凡是被点名的命令，
 * 必须真的不在 `collect_commands!` 里。**
 *
 * 与另外两条守卫的分工：
 * - `commandSurface.guard.test.ts`：调用方引用的命令**必须已注册**（"不能调没有的"）；
 * - `command_arg_case_guard.rs`：命令的入参 key 大小写（"参数名对不对"）；
 * - 本文件：文字声称的注册状态**必须与注册表一致**（"不能说假话"）。
 *
 * 已知边界（诚实写出来）：断言是**行级**的 —— 一句话跨两行、或命令名与断言词不在同一行时抓不到；
 * 它也不判断"这句话整体读起来是否合理"，只判断"点名的那条命令是否真的未注册"。
 */

import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

const BINDINGS = "src/shared/bindings.ts";
const REGISTRY = "src-tauri/src/commands/mod.rs";

/** 断言"某命令不可用/没有"的词。**只收"注册"口径**：`尚未接线` 讲的是界面，不算。 */
const CLAIM = /(尚未注册|还没注册|都没注册|没有注册|未注册|not registered|isn'?t registered|is not registered|hasn'?t been registered|not yet registered|yet to be registered)/i;

/** 从 `collect_commands![ … ]` 块里取注册的命令名。 */
export function registeredCommands(text: string): Set<string> {
  const lines = text.split(/\r?\n/);
  const out = new Set<string>();
  let inside = false;
  for (const line of lines) {
    if (!inside) {
      if (line.includes("collect_commands![")) inside = true;
      continue;
    }
    const t = line.trim();
    if (t.startsWith("])")) break;
    const entry = t.replace(/,$/, "");
    const at = entry.lastIndexOf("::");
    if (at < 0) continue;
    const name = entry.slice(at + 2).trim();
    if (/^[a-z][a-z0-9_]*$/.test(name)) out.add(name);
  }
  return out;
}

/** `bindings.ts` 的 `CommandMap` 直接成员名（契约全集）。 */
export function contractCommands(text: string): Set<string> {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith("export type CommandMap"));
  const out = new Set<string>();
  if (start < 0) return out;
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("};")) break;
    if (!line.startsWith("  ") || line.startsWith("   ")) continue;
    const m = line.slice(2).match(/^([a-z][a-z0-9_]*):\s*\{/);
    if (m?.[1] !== undefined) out.add(m[1]);
  }
  return out;
}

/** 一行文字里点名的、且**其实已经注册**的命令（= 这就是假话）。 */
export function falseClaims(
  line: string,
  registered: ReadonlySet<string>,
  contract: ReadonlySet<string>,
): string[] {
  if (!CLAIM.test(line)) return [];
  const named = line.match(/[a-z][a-z0-9_]{3,}/g) ?? [];
  return [...new Set(named)].filter((n) => contract.has(n) && registered.has(n));
}

type Finding = { file: string; line: number; names: string[]; text: string };

export function scanSources(
  files: readonly string[],
  registered: ReadonlySet<string>,
  contract: ReadonlySet<string>,
  read: (f: string) => string,
): Finding[] {
  const out: Finding[] = [];
  for (const f of files) {
    const lines = read(f).split(/\r?\n/);
    lines.forEach((l, i) => {
      const names = falseClaims(l, registered, contract);
      if (names.length > 0) out.push({ file: f, line: i + 1, names, text: l.trim() });
    });
  }
  return out;
}

function productionFiles(): string[] {
  const tracked = execSync("git ls-files src", { encoding: "utf8" }).trim().split(/\r?\n/);
  return tracked
    .filter((f) => /\.(ts|tsx)$/.test(f))
    .filter((f) => !/\.test\.(ts|tsx)$/.test(f))
    // 模拟后端与契约门面镜像整份 CommandMap，"注册"一词在这里指的是 mock 注册，不是 Rust 注册表
    .filter((f) => f !== "src/shared/api/mock.ts" && f !== "src/shared/api/commands.ts")
    .sort();
}

const read = (f: string): string => readFileSync(path.resolve(f), "utf8");

describe("源码声称的注册状态必须与 collect_commands! 一致", () => {
  it("解析器有牙（自证）：对已注册命令说'尚未注册'必须被抓，对真未注册的必须放行", () => {
    const contract = new Set(["pricing_upsert", "provider_test"]);
    const registered = new Set(["pricing_upsert"]);

    // 假话：pricing_upsert 已注册，却说它尚未注册 → 必须抓
    expect(falseClaims("定价写命令 pricing_upsert 尚未注册 → 不放按钮", registered, contract)).toEqual([
      "pricing_upsert",
    ]);
    // 真话：provider_test 确实未注册 → 必须放行
    expect(falseClaims("provider_test 尚未注册，所以不放按钮", registered, contract)).toEqual([]);
    // 不含"注册"字样的句子不算断言（例如"尚未接线"讲的是界面）
    expect(falseClaims("本页新增入口尚未接线", registered, contract)).toEqual([]);
    // 源码级自证：合成一份文件，只有那条假话被抓
    const found = scanSources(
      ["fake.ts"],
      registered,
      contract,
      () => "// pricing_upsert 尚未注册\n// provider_test 尚未注册\n",
    );
    expect(found.map((f) => f.line)).toEqual([1]);
  });

  it("生产源码里没有任何'已注册却说未注册'的断言", () => {
    const registry = registeredCommands(read(REGISTRY));
    const contract = contractCommands(read(BINDINGS));

    // 两个来源都必须真的读到东西（路径写错时不能对着空集放绿）
    expect(registry.size, `${REGISTRY} 没解析出命令`).toBeGreaterThan(60);
    expect(contract.size, `${BINDINGS} 没解析出命令`).toBeGreaterThan(60);

    const findings = scanSources(productionFiles(), registry, contract, read);
    const report = findings
      .map((f) => `  ${f.file}:${f.line}  点名 ${f.names.join(", ")}（其实已注册）\n      ${f.text}`)
      .join("\n");

    expect(
      findings.length,
      findings.length === 0
        ? ""
        : `以下位置声称某命令"未注册"，但那条命令**已经在 collect_commands! 里** ——
对用户说"这个功能后端还没有"而它其实有，是本项目最不该留的一类假话：
${report}

修法：要么把这句改成说清现状（命令已可用/界面接入在下一批），要么真的把界面接上。`,
    ).toBe(0);
  });
});

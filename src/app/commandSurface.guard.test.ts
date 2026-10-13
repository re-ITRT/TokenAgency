// @vitest-environment node
/**
 * 守卫：**页面/外壳引用的命令，必须是 Rust 真的注册了的**（主管 task-14 裁决②）。
 *
 * 为什么需要它：`src/shared/bindings.ts` 的 `CommandMap` 是**手写契约**（105 条），而
 * `src-tauri` 的 `collect_commands!` 只注册了其中一部分 —— 于是前端手里有一张
 * "看起来什么都能调"的地图，真 Tauri 构建下**点了会失败**（浏览器里因为 `mock.ts` 假实现，
 * 反而看起来正常）。这类"看起来能用"只能靠机械检查发现。
 *
 * 规则（两句话）：
 * 1. **注册表**没注册的命令，**调用方**（页面/外壳）不许引用；
 * 2. 这条守卫**只红在"调用方引用了未注册命令"**上 —— 注册表自身与契约的差异是 E2 的
 *    `command_surface_guard.rs`（task-15）的职责，两边各管一段、不重复。
 *
 * 排除项与理由（都写在 `EXCLUDED` 里）：
 * - `src/shared/api/commands.ts`：**契约的带类型门面**，它有意镜像整份 `CommandMap`；
 *   它的职责是"类型齐全"，不是"调用方"。守卫的主体是调用方。
 * - `src/shared/api/mock.ts`：模拟后端（浏览器开发路径），不是调用方。
 * - `*.test.*`：测试与夹具。
 *
 * 已知边界（诚实写出来）：本守卫比对的是**契约里出现过的命令名**这个全集，
 * 所以"拼错成契约里没有的名字"它抓不到 —— 那类由 tsc 抓（`invokeCommand` 的入参是
 * `CommandName` 联合类型，拼错即编译错）。
 *
 * **它现在应当是绿的**：`StatusBar.tsx` 引用的 `app_pause_gateway` / `app_resume_gateway`
 * 已在 `collect_commands!` 里注册（task-13 已落地），`net::` 那一整组
 * （`subscription_*` / `lane_*` / `kernel_*`）也已登记。
 * 若它变红，先看报出来的 file:line 是不是自己新引用了未注册命令 —— 不要靠 mock 让它"看起来能用"。
 */

import { existsSync, readFileSync } from "node:fs";
import { execSync } from "node:child_process";
import path from "node:path";

import { describe, expect, it } from "vitest";

const BINDINGS = "src/shared/bindings.ts";
const REGISTRY = "src-tauri/src/commands/mod.rs";

/** 只在"`collect_commands!` 块内、`module::name`"这种形态下认命令名。 */
const REGISTRY_BLOCK_START = "collect_commands![";
const REGISTRY_BLOCK_END = "])";

const EXCLUDED = [
  // 契约的带类型门面：有意镜像整份 CommandMap（职责是类型齐全，不是调用方）
  "src/shared/api/commands.ts",
  // 模拟后端：浏览器开发路径，不是调用方
  "src/shared/api/mock.ts",
];

function isCommandName(s: string): boolean {
  return /^[a-z][a-z0-9_]*$/.test(s);
}

/**
 * 涂白注释（**保留行号**）：引用扫描只认**代码里的字符串**，
 * 不认注释/文档里"提到"的名字 —— 否则注释里举例一条命令也会被当作调用点（实测踩到过：
 * `src/shared/api/index.ts` 与 `queries.ts` 的说明性注释各命中一次）。
 */
export function blankComments(text: string): string {
  const noBlocks = text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  return noBlocks
    .split(/\r?\n/)
    .map((l) => l.replace(/\/\/.*$/, ""))
    .join("\n");
}

/** `bindings.ts` 里 `export type CommandMap = { … }` 的直接成员名（恰好两空格缩进 + `name: {`）。 */
export function contractCommands(text: string): string[] {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith("export type CommandMap"));
  if (start < 0) return [];
  const out: string[] = [];
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("};")) break;
    // 恰好两空格 = CommandMap 的直接成员；更深缩进是值类型内部字段
    if (!line.startsWith("  ") || line.startsWith("   ")) continue;
    const m = line.slice(2).match(/^([a-z][a-z0-9_]*):\s*\{/);
    if (m?.[1] !== undefined) out.push(m[1]);
  }
  return out;
}

/** `src-tauri/src/commands/mod.rs` 的 `collect_commands![ … ]` 里注册的命令名。 */
export function registeredCommands(text: string): string[] {
  const lines = text.split(/\r?\n/);
  let inside = false;
  const out: string[] = [];
  for (const line of lines) {
    if (!inside) {
      if (line.includes(REGISTRY_BLOCK_START)) inside = true;
      continue;
    }
    const t = line.trim();
    if (t.startsWith(REGISTRY_BLOCK_END)) break;
    const entry = t.replace(/,$/, "");
    const at = entry.lastIndexOf("::");
    if (at < 0) continue;
    const name = entry.slice(at + 2).trim();
    if (isCommandName(name)) out.push(name);
  }
  return out;
}

type Reference = { file: string; line: number; name: string };

/** 扫描"调用方"文件里对**契约命令名**的字符串引用。 */
export function commandReferences(
  files: readonly string[],
  contract: readonly string[],
  read: (f: string) => string,
): Reference[] {
  const universe = new Set(contract);
  const found: Reference[] = [];
  for (const file of files) {
    const lines = blankComments(read(file)).split(/\r?\n/);
    lines.forEach((line, i) => {
      for (const m of line.matchAll(/"([a-z][a-z0-9_]*)"/g)) {
        const name = m[1];
        if (name !== undefined && universe.has(name)) {
          found.push({ file, line: i + 1, name });
        }
      }
    });
  }
  return found;
}

function productionFiles(): string[] {
  const tracked = execSync("git ls-files src", { encoding: "utf8" }).trim().split(/\r?\n/);
  return tracked
    .filter((f) => /\.(ts|tsx)$/.test(f))
    .filter((f) => !/\.test\.(ts|tsx)$/.test(f))
    .filter((f) => !EXCLUDED.includes(f))
    // `git ls-files` 报的是**已跟踪**的文件：刚删掉、删除还没提交的文件仍在名单里。
    // 那种文件没有调用点（它根本不存在），`readFileSync` 会以 ENOENT 把守卫整条打红 ——
    // 那是扫描器的噪音，不是"引用了未注册命令"。所以只扫**磁盘上真的在**的文件。
    .filter((f) => existsSync(f))
    .sort();
}

const read = (f: string): string => readFileSync(path.resolve(f), "utf8");

describe("[task-14] 调用方只许引用 Rust 已注册的命令", () => {
  it("解析器本身有牙（自证：两个来源都读到了锚点，且能认出未注册引用）", () => {
    const contract = contractCommands(read(BINDINGS));
    const registered = registeredCommands(read(REGISTRY));

    // 两个来源都必须真的读到东西（路径写错时不许"对着空集断言然后绿"）
    expect(contract.length, `${BINDINGS} 没解析出命令`).toBeGreaterThan(30);
    expect(registered.length, `${REGISTRY} 没解析出命令`).toBeGreaterThan(10);
    expect(contract).toContain("app_status");
    expect(contract).toContain("log_query");
    expect(registered).toContain("app_status");

    // "能认出未注册引用"：合成一段调用方源码（含一条未注册命令）必须被抓出来
    const synthetic = ['invokeCommand("app_status", undefined);', 'invokeCommand("not_registered_yet", {});'];
    const refs = commandReferences(["fake.ts"], [...contract, "not_registered_yet"], () =>
      synthetic.join("\n"),
    );
    expect(refs.map((r) => r.name)).toContain("not_registered_yet");
  });

  it("生产调用方引用的每条命令都在注册表里（今天红在 StatusBar 的两条上）", () => {
    const contract = contractCommands(read(BINDINGS));
    const registered = new Set(registeredCommands(read(REGISTRY)));

    const offenders = commandReferences(productionFiles(), contract, read).filter(
      (r) => !registered.has(r.name),
    );

    const report = offenders
      .map((r) => `  ${r.file}:${r.line}  "${r.name}"（未在 ${REGISTRY} 注册）`)
      .join("\n");

    expect(
      offenders.length,
      offenders.length === 0
        ? ""
        : `以下调用点在真 Tauri 构建下会失败（浏览器里被 mock 掩盖）：\n${report}\n\n` +
            `修法：等 src-tauri 注册该命令（task-13），或把调用点撤掉 —— 不要靠 mock 让它"看起来能用"。`,
    ).toBe(0);
  });
});

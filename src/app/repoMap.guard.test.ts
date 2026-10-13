// @vitest-environment node
/**
 * **地图守卫**：`AGENTS.md` 与各模块 README 说的每一件事，都要能在代码里对上。
 *
 * ## 为什么需要它
 *
 * 用户的要求是"AI 可读的项目地图"。地图最容易的坏法不是"写得不好"，而是**烂掉**：
 * 路由从 12 改成 11、命令加了一组、页面删了一个 —— 地图还写着旧数字，
 * 而**没有任何东西会红**。于是 AI 读着过期地图去改代码，比没有地图更糟。
 *
 * 所以地图必须像其它契约一样被机械核对。这条守卫核对四件事：
 *
 * | # | 检查 | 为什么 |
 * |---|---|---|
 * | 1 | `AGENTS.md` 里出现的**仓库路径都真实存在** | 防止"指向不存在的文件"（AI 会照着去读） |
 * | 2 | 三个**数字**与代码一致（路由 / 命令 / 事件） | 数字是最容易过期的东西 |
 * | 3 | 命令文件清单与 `src-tauri/src/commands/` 实际文件一致 | 加了命令组忘了写进地图 ⇒ 红 |
 * | 4 | 各模块 README 存在，且**它们提到的路径也真实存在** | 第 2 层地图同样不许烂 |
 *
 * ## 一个真实的教训（本文件第一次跑之前）
 *
 * 我在 `AGENTS.md` 初稿里写"193 个命令 / 11 个命令文件 / 12 条路由"，
 * 实测是 **116 / 10 / 11** —— 因为当时我用 `^pub async fn` 去数命令，
 * 把**不是** Tauri 命令的 pub async fn 也算进去了。数字错了整整 77。
 * 这正是"必须能核对"的理由：**手写的数字一定有错，除非有东西替你核。**
 */

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { ROUTES } from "./routes";

const ROOT = resolve(__dirname, "..", "..");
const AGENTS = join(ROOT, "AGENTS.md");

/** 第 9 节声明的模块 README（少一个就红 —— 地图不许指向不存在的文件）。 */
const MODULE_READMES = [
  "crates/ta-core/README.md",
  "crates/ta-store/README.md",
  "crates/ta-kernel/README.md",
  "crates/ta-subscription/README.md",
  "crates/ta-secrets/README.md",
  "crates/ta-gateway/README.md",
  "src-tauri/README.md",
  "src/README.md",
  "scripts/README.md",
] as const;

/**
 * 从 Markdown 里取出"看起来是仓库路径"的反引号片段。
 *
 * 保守过滤：带空格 / 通配符 / 省略号 / 尖括号的一律跳过（那些是命令、glob 或占位符）；
 * **以 `/` 开头的也跳过** —— 那是 HTTP 路由（`/v1/chat/completions`），不是文件路径。
 */
function repoPaths(md: string): string[] {
  const out = new Set<string>();
  for (const m of md.matchAll(/`([^`\n]+)`/g)) {
    const token = m[1]!.trim();
    if (token === "" || token.startsWith("/") || token.startsWith("http")) continue;
    if (/[\s*<>{}…]/.test(token)) continue;
    const looksLikePath =
      token.includes("/") || /\.(md|rs|ts|tsx|mjs|js|ps1|yaml|yml|toml|json|txt)$/.test(token);
    if (!looksLikePath) continue;
    out.add(token);
  }
  return [...out];
}

/** 路径是否存在于"相对 README 所在目录"或"相对仓库根"（两种写法都允许）。 */
function pathExistsNear(fromDir: string, path: string): boolean {
  // `crates/ta-gateway/src/probe.rs::some_guard` 是**地图里的自然记法**（路径::符号），
  // 核对时只看 `::` 之前的那一段 —— 符号的正确性由"人和测试"保证，
  // 路径的正确性由这条守卫保证。
  //
  // `crates/ta-kernel/src/lib.rs:306` 同理：**行号要剥掉**。
  // 这是 spec 明确要求的格式（`文件:行` 是代码地图最有用的记法），
  // 守卫不剥就等于"照规范写必红" —— 而两个写者已经踩中了，
  // 所以错的是守卫，不是他们。
  const filePart = path.split("::")[0]!.replace(/:\d+(-\d+)?$/, "");
  return existsSync(resolve(fromDir, filePart)) || existsSync(resolve(ROOT, filePart));
}

/** 数 `src-tauri` 里真正的 Tauri 命令（`#[tauri::command` 的出现次数）。 */
function countTauriCommands(): number {
  let n = 0;
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.name.endsWith(".rs")) {
        n += (readFileSync(full, "utf8").match(/#\[tauri::command/g) ?? []).length;
      }
    }
  };
  walk(join(ROOT, "src-tauri", "src"));
  return n;
}

/** 数 `TaEvent::name()` 的分支（后端 → 前端的事件名）。 */
function countEvents(): number {
  const src = readFileSync(join(ROOT, "crates", "ta-gateway", "src", "events.rs"), "utf8");
  const start = src.indexOf("pub fn name");
  const body = src.slice(start, src.indexOf("\n    }", start));
  return (body.match(/=>\s*"/g) ?? []).length;
}

/**
 * 从 `AGENTS.md` 里取出地图声明的数字。
 *
 * 传**完整正则**（含一个捕获组）而不是"锚点 + 单位"：第一版用
 * `事件[\s\S]*?(\d+)\s*个` 去抓事件数，而"事件"二字在地图里先出现在别处，
 * 于是它一路匹配到了**命令行**的 `116 个` —— 断言变成"地图写 116 个事件"，
 * 而真实是 10。教训：表格里的数字必须用**足够唯一的锚点**去抓。
 */
function claimedNumber(md: string, pattern: RegExp): number | null {
  const m = pattern.exec(md);
  return m === null ? null : Number(m[1]);
}

describe("[地图] AGENTS.md 与模块 README 必须与代码一致", () => {
  const agents = readFileSync(AGENTS, "utf8");

  it("1. AGENTS.md 里提到的仓库路径都真实存在", () => {
    const missing = repoPaths(agents).filter((p) => !pathExistsNear(ROOT, p));
    expect(
      missing,
      `AGENTS.md 指向了不存在的路径（AI 会照着去读，所以这是硬错误）：\n${missing.join("\n")}`,
    ).toEqual([]);
  });

  it("2. 三个数字与代码一致（路由 / 命令 / 事件）", () => {
    // 锚点必须**足够唯一**：`前端路由` 只在那一行出现；命令用 `Tauri 命令`；
    // 事件用带反引号的 `emit` 那一格（"事件"二字在别处也出现过，见 claimedNumber 的注释）。
    const routes = claimedNumber(agents, /前端路由[\s\S]*?(\d+)\s*条/);
    const commands = claimedNumber(agents, /Tauri 命令[\s\S]*?(\d+)\s*个/);
    const events = claimedNumber(agents, /前端 `emit`\s*\|\s*(\d+)\s*个事件名/);

    expect(routes, "AGENTS.md 没写路由数（或格式变了）").not.toBeNull();
    expect(routes, `地图写 ${routes} 条路由，代码里是 ${ROUTES.length} 条`).toBe(ROUTES.length);

    const realCommands = countTauriCommands();
    expect(
      commands,
      `地图写 ${commands} 个命令，代码里是 ${realCommands} 个（\`#[tauri::command]\` 计数）`,
    ).toBe(realCommands);

    const realEvents = countEvents();
    expect(events, `地图写 ${events} 个事件，代码里是 ${realEvents} 个`).toBe(realEvents);
  });

  it("3. 命令文件清单与 src-tauri/src/commands/ 实际文件一致", () => {
    const dir = join(ROOT, "src-tauri", "src", "commands");
    const actual = readdirSync(dir)
      .filter((f) => f.endsWith(".rs") && f !== "mod.rs")
      .map((f) => f.replace(/\.rs$/, ""))
      .sort();

    // 地图用 `{a,b,c}.rs` 的花括号写法列清单
    const braces = /\{([a-z_,]+)\}\.rs/.exec(agents);
    expect(braces, "AGENTS.md 里的命令文件清单不见了（或格式变了）").not.toBeNull();
    const claimed = braces![1]!.split(",").map((s) => s.trim()).sort();

    expect(
      claimed,
      `地图的命令文件清单与目录不一致。\n目录里有：${actual.join(", ")}\n地图写了：${claimed.join(", ")}`,
    ).toEqual(actual);
  });

  it("4. 各模块 README 存在，且它们提到的路径也真实存在", () => {
    const missing: string[] = [];
    const badPaths: string[] = [];

    for (const rel of MODULE_READMES) {
      const full = join(ROOT, rel);
      if (!existsSync(full)) {
        missing.push(rel);
        continue;
      }
      const dir = dirname(full);
      for (const p of repoPaths(readFileSync(full, "utf8"))) {
        if (!pathExistsNear(dir, p)) badPaths.push(`${rel} → ${p}`);
      }
    }

    expect(
      missing,
      `AGENTS.md 第 9 节列出了这些模块 README，但它们不存在：\n${missing.join("\n")}`,
    ).toEqual([]);
    expect(
      badPaths,
      `模块 README 指向了不存在的路径（第 2 层地图同样不许烂）：\n${badPaths.join("\n")}`,
    ).toEqual([]);
  });
});

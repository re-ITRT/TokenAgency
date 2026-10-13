// @vitest-environment node
/**
 * 守卫：**「读后端原始对象」的四个读件只许有一份实现**（[docs/52]）。
 *
 * # 为什么需要机械守卫
 *
 * 第 319 轮实测：`asRow` / `pickStr` / `pickNum` / `pickBool` 在 **5 个页面 model 里
 * 各写了一遍**，合计 15 份定义 —— 而且**已经漂了**（两份收数组、三份挡数组）。
 *
 * 这类重复的可怕之处不是"多敲了几行"，而是：**漂移无声**。
 * 两份 `asRow` 语义不同，没有任何测试会红（各页只测自己那一页），
 * 直到某天有人把数组喂进去、界面画出一个「—」，然后花半天查"为什么读不到"。
 *
 * 所以这里把它钉死：**只有 `src/shared/row.ts` 可以定义它们**。
 * 想在页面里再写一份？守卫会红 —— 请 import。
 *
 * # 为什么按"函数名 + 定义形态"配对检查，而不是列文件白名单
 *
 * 白名单会过期（新增页面时要记得加）。这里改成**扫描全仓**：
 * 只要某个 `.ts`/`.tsx`（测试文件除外）里又出现 `function asRow(` 之类，
 * 就红。新增页面时不需要维护任何清单。
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

const ROOT = join(__dirname, "..", "..");
const SRC = join(ROOT, "src");

/** 唯一的合法实现处（相对仓库根，正斜杠）。 */
const HOME = "src/shared/row.ts";

/** 被收敛的读件（今后**任何**一份新定义都算违规）。 */
const READERS = ["asRow", "pickStr", "pickNum", "pickBool", "pickArr", "hasKey", "pickRows"];

/**
 * 同一轮收敛的**其它**重复件：名字 → 唯一允许定义它的文件。
 *
 * 这些不像读件那样"有 15 份、还已经漂了"，但性质一样：**写 N 遍就会各自演化**。
 * 上一轮（[docs/50]）收的 `Caption` / `DASH` / `SECTION_SX` / `SwitchControl` 就是同一批教训，
 * 而那一轮**没有配守卫** —— 于是同一个毛病又长回来了（见下面 `asRow` 那 15 份）。
 * 所以这次把清单和守卫一起交。
 */
const SINGLETONS: ReadonlyArray<{ name: string; home: string; kind: "fn" | "const" }> = [
  { name: "StatItem", home: "src/shared/components/StatItem.tsx", kind: "fn" },
  { name: "mergeHelpText", home: "src/shared/helpTip.ts", kind: "fn" },
  { name: "collectIntoHelpTip", home: "src/shared/helpTip.ts", kind: "fn" },
  { name: "sectionHelpMode", home: "src/shared/helpTip.ts", kind: "fn" },
  { name: "i18nKey", home: "src/shared/i18n/index.ts", kind: "fn" },
  { name: "HELP_TIP_MIN_LENGTH", home: "src/shared/helpTip.ts", kind: "const" },
];

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
      out.push(full);
    }
  }
  return out;
}

/** 这个文件里定义了哪些读件（`function name(` 或 `export function name(`）。 */
function definitions(text: string): string[] {
  const found: string[] = [];
  for (const name of READERS) {
    const re = new RegExp(`^\\s*(export\\s+)?function\\s+${name}\\s*[(<]`, "m");
    if (re.test(text)) found.push(name);
  }
  return found;
}

/** 这个文件里有没有**定义**（不是 import / 不是再导出）某个单件。 */
function definesSingleton(text: string, name: string, kind: "fn" | "const"): boolean {
  if (kind === "fn") {
    return new RegExp(`^\\s*(export\\s+)?function\\s+${name}\\s*[(<]`, "m").test(text);
  }
  return new RegExp(`^\\s*(export\\s+)?const\\s+${name}\\s*[:=]`, "m").test(text);
}

/** 「`export { name }` / `export { name } from …`」式再导出（合法：兄弟模块靠它编译通过）。 */
function reExports(text: string, name: string): boolean {
  return new RegExp(`^\\s*export\\s*\\{[^}]*\\b${name}\\b[^}]*\\}`, "m").test(text);
}

describe("[docs/52] 读后端原始对象的实现只有一份", () => {
  it("除 `src/shared/row.ts` 外，任何源码都不许再定义这些读件", () => {
    const offenders: string[] = [];
    for (const file of walk(SRC)) {
      const rel = relative(ROOT, file).split("\\").join("/");
      if (rel === HOME) continue;
      const found = definitions(readFileSync(file, "utf8"));
      if (found.length > 0) offenders.push(`${rel} 又定义了 ${found.join(", ")}`);
    }
    expect(
      offenders,
      `这些文件重新实现了共享读件（请改为从 "@/shared/row" import）：\n${offenders.join("\n")}`,
    ).toEqual([]);
  });

  it("共享件本身确实提供了全部读件（防止有人删掉一个而守卫仍绿）", () => {
    const text = readFileSync(join(ROOT, HOME), "utf8");
    const found = definitions(text);
    expect(found.sort()).toEqual([...READERS].sort());
  });

  it("本轮收敛的其它单件也不许被重新定义（再导出是允许的）", () => {
    const offenders: string[] = [];
    for (const file of walk(SRC)) {
      const rel = relative(ROOT, file).split("\\").join("/");
      const text = readFileSync(file, "utf8");
      for (const item of SINGLETONS) {
        if (rel === item.home) continue;
        if (definesSingleton(text, item.name, item.kind) && !reExports(text, item.name)) {
          offenders.push(`${rel} 又定义了 ${item.name}（唯一实现应在 ${item.home}）`);
        }
      }
    }
    expect(
      offenders,
      `这些文件重新实现了已被收敛的共享件：\n${offenders.join("\n")}`,
    ).toEqual([]);
  });

  it("单件清单里的每个 home 文件都真的定义着那个名字（防止清单指向不存在的实现）", () => {
    const missing: string[] = [];
    for (const item of SINGLETONS) {
      const text = readFileSync(join(ROOT, item.home), "utf8");
      if (!definesSingleton(text, item.name, item.kind)) {
        missing.push(`${item.home} 里没有定义 ${item.name}`);
      }
    }
    expect(missing, missing.join("\n")).toEqual([]);
  });
});

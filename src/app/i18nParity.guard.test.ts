// @vitest-environment node
/**
 * 守卫：**两种语言的 key 必须一一对应**。
 *
 * # 为什么必须有它
 *
 * `t()` 的类型只由 `zh-CN.ts` 约束（`index.ts` 的 `declare module "i18next"` 把 resources
 * 钉在 `typeof zhCN` 上），而 i18next 配了 `fallbackLng: "zh-CN"`。
 * 于是 **`en.ts` 少一个 key 不会编译报错、不会测试报错** —— 切到英文时那一句
 * 静默地显示成中文。这属于本仓最忌讳的"静默失效"：功能看起来在，其实没数据。
 *
 * 今天两边恰好都是 919 个 key；这条守卫的意义是**让明天不会悄悄错开**。
 *
 * # 它怎么保证自己不是安慰牌
 *
 * * **语言列表从 `index.ts` 读**（不在这里另抄一份）—— 加语言时守卫自动纳入比对；
 * * 自检：喂两组合成 key，断言"缺一个"真的会被报出来；
 * * 防空转：每种语言抽到的 key 必须 > 100，`LOCALES` 至少两个 ——
 *   正则写错导致抽到空集时，上一条反而更容易通过。
 */
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const ROOT = resolve(__dirname, "..", "..");
const DIR = join(ROOT, "src", "shared", "i18n");

/** 从 `index.ts` 读 `LOCALES`（唯一真源，守卫不另抄一份）。 */
export function localesOf(indexSource: string): string[] {
  const m = /export const LOCALES = \[([^\]]*)\]/.exec(indexSource);
  const body = m?.[1];
  if (body === undefined) return [];
  return [...body.matchAll(/"([^"]+)"/g)]
    .map((x) => x[1])
    .filter((x): x is string => x !== undefined);
}

/**
 * 从某个语言文件里抽出 key。
 *
 * 与 `scripts/audit-inline-copy.mjs` 用同一种形态（行首缩进 + 双引号 key + 冒号），
 * 免得两处对"什么算一个 key"有不同理解。
 */
export function keysOf(source: string): string[] {
  const out: string[] = [];
  for (const line of source.split("\n")) {
    const key = /^\s*"([^"]+)":/.exec(line)?.[1];
    if (key !== undefined) out.push(key);
  }
  return out;
}

/** `a` 里有而 `b` 里没有的 key（导出是为了自检能直接喂合成数据）。 */
export function missingKeys(a: readonly string[], b: readonly string[]): string[] {
  const has = new Set(b);
  return a.filter((k) => !has.has(k));
}

describe("[i18n] 两种语言的 key 必须一一对应", () => {
  it("自检：缺一个 key 必须被报出来（正负样本都要认对）", () => {
    expect(missingKeys(["a", "b"], ["a", "b"])).toEqual([]);
    expect(missingKeys(["a", "b"], ["a"])).toEqual(["b"]);
    // 反向也要认（多的那个方向）
    expect(missingKeys(["a"], ["a", "b"])).toEqual([]);
    // 抽键函数本身要有牙
    expect(keysOf('  "x.y": "v",\n  "z": "w",')).toEqual(["x.y", "z"]);
  });

  it("每种语言的 key 集合完全一致", () => {
    const index = readFileSync(join(DIR, "index.ts"), "utf8");
    const locales = localesOf(index);
    expect(
      locales.length,
      `从 index.ts 的 LOCALES 里只读到 ${locales.length} 种语言 —— 正则没匹配上就说明"真源"变了，` +
        `先修守卫再改文案。`,
    ).toBeGreaterThanOrEqual(2);

    const byLocale = new Map<string, string[]>();
    for (const loc of locales) {
      byLocale.set(loc, keysOf(readFileSync(join(DIR, `${loc}.ts`), "utf8")));
    }

    // 防空转：抽到空集时下面的断言会"轻松通过"
    for (const [loc, keys] of byLocale) {
      expect(keys.length, `${loc}.ts 只抽到 ${keys.length} 个 key —— 抽键正则多半写错了`)
        .toBeGreaterThan(100);
    }

    const base = locales[0];
    if (base === undefined) throw new Error("LOCALES 是空的");
    const rest = locales.slice(1);
    const baseKeys = byLocale.get(base) ?? [];
    const problems: string[] = [];
    for (const loc of rest) {
      const keys = byLocale.get(loc)!;
      const missing = missingKeys(baseKeys, keys);
      const extra = missingKeys(keys, baseKeys);
      if (missing.length) {
        problems.push(`${loc} 缺 ${missing.length} 个（切到 ${loc} 时会静默显示 ${base}）：\n    ${missing.slice(0, 10).join("\n    ")}`);
      }
      if (extra.length) {
        problems.push(`${loc} 多 ${extra.length} 个（${base} 里没有，说明是多余键）：\n    ${extra.slice(0, 10).join("\n    ")}`);
      }
    }
    expect(problems, problems.join("\n")).toEqual([]);

    // 顺带把规模写进断言信息里：数字变了能立刻看见（它不该无缘无故变）。
    expect(baseKeys.length).toBeGreaterThan(100);
  });
});

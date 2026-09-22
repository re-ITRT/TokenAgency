// @vitest-environment node
/**
 * 首启向导 `/onboarding` 的**反向断言**：这个向导已经没有入口了（[19 §2.1]）。
 *
 * ## 为什么这份文件从"1700 行的向导验收"变成"一条反向断言"
 *
 * 用户原话（[19]）：「网络：**两个向导都关掉**（首启 `/onboarding` + 设置页里的网络/内核那块）」。
 * 向导**整条从产品里移除**，所以上一版那些"第 1 步 / 第 3 步 / 第 4 步"的用例全都失去了被测对象 ——
 * 留着它们就是[19 §5] 说的那种**"永远渲染不到的测试"**：跑得绿，却什么都没验证。
 *
 * 于是这里改成钉**它真的没了**这一点，而且是用机械检查钉：
 *
 * 1. **路由表里没有 `/onboarding`**（也没有任何 `onboarding*` 候选页），侧边栏自然也没有；
 * 2. **页面文件不存在**：`resolvePage` 解析不出来 —— "不显示"的验收标准是"不存在"，
 *    不是"被 CSS 藏起来了"（与 `network.test.tsx` 的 `network-optional` 反向断言同一套纪律）；
 * 3. **页面层源码不再引用 `onboarding_*` 三条命令**：命令在后端**保留注册**（能力不删），
 *    但 UI 无入口（[19 §2.1]）—— 这一条防的是"哪天顺手把向导接回来"。
 */

import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { knownPages, resolvePage } from "@/app/pageRegistry";
import { NAV_ROUTES, ROUTES, routeByKey } from "@/app/routes";

/** 后端仍注册、但页面不许再引用的三条命令（[19 §2.1]）。 */
const ONBOARDING_COMMANDS = ["onboarding_detect", "onboarding_apply", "onboarding_test_all"];

/**
 * 合法引用 `onboarding_*` 的三处**非页面**文件：
 * - `bindings.ts`：冻结契约（命令签名必须在）；
 * - `api/commands.ts`：类型化门面（`invokeCommand("onboarding_detect", …)`）；
 * - `api/mock.ts`：浏览器开发 mock（不是产品路径）。
 * 生产页面里出现它们，就说明这个向导又被接回来了。
 */
const ALLOWED_NON_PAGE_FILES = new Set([
  "src/shared/bindings.ts",
  "src/shared/api/commands.ts",
  "src/shared/api/mock.ts",
]);

/** `src` 下的全部 `.ts` / `.tsx` 源码（排除测试：测试文件里写命令名是它的本职）。 */
function sourceFiles(dir = "src"): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.posix.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sourceFiles(full));
    else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) out.push(full);
  }
  return out.sort();
}

/** 源码里**作为字符串字面量**出现的 `onboarding_*` 命令（注释里提一句不算引用）。 */
function offenders(files: readonly string[], read: (file: string) => string): string[] {
  const found: string[] = [];
  for (const file of files) {
    const text = read(file);
    for (const cmd of ONBOARDING_COMMANDS) {
      if (text.includes(`"${cmd}"`)) found.push(`${file}: ${cmd}`);
    }
  }
  return found;
}

describe("首启向导 /onboarding 已经没有入口（[19 §2.1]）", () => {
  it("路由表里没有 /onboarding 这一条，侧边栏也没有", () => {
    expect(routeByKey("onboarding")).toBeUndefined();
    expect(ROUTES.filter((r) => r.path === "/onboarding")).toEqual([]);
    // 连"候选页面"都不许留：留着就是把页面文件当成有归属，孤儿守卫反而看不见它
    expect(ROUTES.flatMap((r) => r.pages).filter((p) => p.startsWith("onboarding"))).toEqual([]);
    expect(NAV_ROUTES.filter((r) => r.key === "onboarding")).toEqual([]);
  });

  it("页面文件也不存在了：`resolvePage` 解析不出向导（不是藏起来，是没有）", () => {
    expect(resolvePage(["onboarding", "onboarding/index"])).toBeUndefined();
    expect(knownPages().filter((key) => key.startsWith("onboarding"))).toEqual([]);
  });

  it("自证：扫描器在坏样本上必须报红，在注释/契约文件上不误伤", () => {
    expect(
      offenders(["fake/page.tsx"], () => 'useCommandQuery("onboarding_detect", undefined, []);'),
    ).toEqual(["fake/page.tsx: onboarding_detect"]);
    // 注释里提一句（没有引号）不是引用点
    expect(offenders(["fake/page.ts"], () => "// 向导已关掉：onboarding_detect\n")).toEqual([]);
  });

  it("页面层源码不再引用 onboarding_*（后端保留注册，UI 无入口）", () => {
    const files = sourceFiles().filter((file) => !ALLOWED_NON_PAGE_FILES.has(file));
    expect(files.length, "一条源码都没扫到 —— 扫描器的路径写错了").toBeGreaterThan(50);
    expect(offenders(files, (file) => readFileSync(file, "utf8"))).toEqual([]);
    // 自证白名单真的在起作用：契约与门面文件里确实还留着这三条命令
    for (const file of ALLOWED_NON_PAGE_FILES) {
      expect(sourceFiles(), `${file} 不在源码扫描范围里？`).toContain(file);
    }
  });
});

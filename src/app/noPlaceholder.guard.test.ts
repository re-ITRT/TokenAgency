/**
 * 守卫：每个路由都必须解析到真实页面 —— 不允许再有占位壳。
 *
 * 为什么要有这一条（真实事故）：11 个路由里曾经只有 `/dashboard` 与 `/logs` 是真页面，
 * 其余全部落到 `<PagePlaceholder>`，而页面上写着"这个页面由 F3/F4 实现"。
 * 结果主管在报告里把"路由能渲染、门禁全绿"读成了"页面做完了" —— 占位壳不会让任何东西变红，
 * 它只会安静地把"没做的页面"伪装成"做了的页面"。这条守卫把那件事变成一次响亮的失败。
 *
 * 判据与 `pageRegistry` 一致：路由的 `pages` 候选路径里，至少一个必须真的存在
 * （`knownPages()` 返回的是 `import.meta.glob("../pages/**\/*.tsx")` 的实际结果）。
 *
 * 这条守卫是有意**先红后绿**的：它今天会列出所有还没落地的页面与它们该放的文件路径。
 * 谁把页面放上去，它就少红一条；全部落地后它才应该被合并进 master（与
 * `commandSurface.guard.test.ts` 同一套纪律：不允许把"设计上就红"的守卫摊在主干上）。
 */

import { describe, expect, it } from "vitest";

import { knownPages } from "./pageRegistry";
import { ROUTES } from "./routes";

/**
 * 只把"页面入口"当页面：根级的 `x.tsx`（key 形如 `models`）或目录下的 `index.tsx`（key 形如 `logs/index`）。
 *
 * 为什么必须排除其它文件：页面目录里会有**同目录组件**（例如 `src/pages/logs/TracePanel.tsx`），
 * 它们不是路由入口，不该被当成"孤儿页面"而报红。第一版把这类文件算进来，结果 `logs/TracePanel`
 * 一直红 —— 那是**守卫自己的假阳性**，不是产品缺陷。
 */
function pageEntryKeys(): string[] {
  return knownPages().filter((key) => !key.includes("/") || key.endsWith("/index"));
}

describe("每个路由都有真实页面（不许再有占位壳）", () => {
  it("没有路由会落到 PagePlaceholder", () => {
    // 注意：这条用**全部**已就位文件，不能过滤 —— 嵌套路由页（`providers/detail`、`network/nodes`）
    // 的 key 里也带 `/`，用"页面入口"过滤器会把它们误判成"没落地"。
    // （第一版真的这么错过：守卫报 3 条假的"缺失"，而文件明明在。）
    const known = new Set(knownPages());

    const missing = ROUTES.filter((route) => !route.pages.some((p) => known.has(p))).map(
      (route) =>
        `${route.path}（owner ${route.owner}）—— 应放 src/pages/${route.pages[0]}.tsx`,
    );

    expect(
      missing,
      missing.length === 0
        ? ""
        : `还有 ${missing.length} 个路由是占位壳，页面没有落地：\n  ` + missing.join("\n  "),
    ).toEqual([]);
  });

  it("已就位的页面入口都在路由表里有归属（反向：不许有孤儿页面入口）", () => {
    // 这一条才需要"页面入口"过滤器：页面目录里会有**同目录组件**（如 `logs/TracePanel`），
    // 它们不是路由入口，不该被当成"孤儿页面"。
    const declared = new Set(ROUTES.flatMap((r) => [...r.pages]));
    const orphans = pageEntryKeys().filter((p) => !declared.has(p));
    expect(orphans, `这些页面入口没有任何路由引用：${orphans.join(", ")}`).toEqual([]);
  });
});

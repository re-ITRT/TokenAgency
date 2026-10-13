/**
 * 页面自动接线（约定优于配置）。
 *
 * F2/F3/F4 **不需要改布局代码**：把页面文件放到约定路径即自动生效，例如
 *
 * ```
 * src/pages/dashboard/index.tsx     ← /dashboard   （F2）
 * src/pages/logs/index.tsx          ← /logs        （F2）
 * src/pages/providers/index.tsx     ← /providers   （F3）
 * src/pages/providers/detail.tsx    ← /providers/:id（F3，候选路径见 routes.ts）
 * src/pages/network/nodes.tsx       ← /network/nodes（F4）
 * ```
 *
 * 候选路径写在 `routes.ts` 的 `pages` 字段里（按优先级）。命中第一个存在的文件；
 * 都没命中就返回 `undefined`，调用方渲染**用户能看懂的 404**（`AppRouter` 的 `<NotFound>`）：
 * **不白屏、不报错**。
 *
 * [极简] 这里原来还会把"该放哪个文件"渲染到页面上（`<PagePlaceholder>`）——
 * 那是给开发看的。现在缺页面就是 404，页面路径只留在源码与测试的报错信息里。
 *
 * 实现用 Vite 的 `import.meta.glob`（构建期静态分析，产物仍是按页面分包 + 懒加载）。
 */

import { lazy, type ComponentType, type LazyExoticComponent } from "react";

type PageModule = { default: ComponentType };
type PageLoader = () => Promise<PageModule>;

/** 约定：页面文件只放在 `src/pages/**`，且必须有 `default` 导出。 */
const modules: Record<string, () => Promise<unknown>> = import.meta.glob("../pages/**/*.tsx");

const registry = new Map<string, PageLoader>();
for (const [file, loader] of Object.entries(modules)) {
  const key = file.replace(/^\.\.\/pages\//, "").replace(/\.tsx$/, "");
  // `import.meta.glob` 的值类型是 `() => Promise<unknown>`，实现上是页面模块；
  // 这里收窄一次，页面导出形状由 `PageModule` 约束（页面写错 default 导出会在运行时显形）。
  registry.set(key, loader as unknown as PageLoader);
}

/** 已解析过的懒加载组件缓存：避免每次 render 都 `lazy()`（那会导致整页重挂载）。 */
const resolved = new Map<string, LazyExoticComponent<ComponentType>>();

/** 按候选路径解析页面组件；都没有则返回 `undefined`（调用方渲染 404）。 */
export function resolvePage(
  candidates: readonly string[],
): LazyExoticComponent<ComponentType> | undefined {
  for (const candidate of candidates) {
    const cached = resolved.get(candidate);
    if (cached !== undefined) return cached;
    const loader = registry.get(candidate);
    if (loader !== undefined) {
      const component = lazy(loader);
      resolved.set(candidate, component);
      return component;
    }
  }
  return undefined;
}

/** 当前已就位的页面文件（相对 `src/pages/`，供调试/自检用）。 */
export function knownPages(): string[] {
  return [...registry.keys()].sort();
}


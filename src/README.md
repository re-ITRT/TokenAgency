# src — React 19 + MUI v7 前端（页面 / 外壳 / 契约绑定）

> 上一层：[../AGENTS.md](../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/main.tsx` | 挂载入口：造 QueryClient（staleTime 5s、retry 1）并渲染 `App`；**不建主题** |
| `src/App.tsx` | 主题（跟随 `useUiStore.themeMode`）+ 初始化 i18n + `HashRouter` + `registerDevMocks()` |
| `src/app/AppShell.tsx` | 外壳：状态条 + 侧边栏 + 页面区 + Toasts；**唯一**调用 `useEventBridge()` 的地方 |
| `src/app/AppRouter.tsx` | 把 `ROUTES` 渲染成 `<Route>`：懒加载 + Suspense + ErrorBoundary + 用户能看懂的 404 |
| `src/app/routes.ts` | 路由表 `ROUTES`（**11 条**）+ `NAV_ROUTES` / `navSections()` / `routeByKey()` / `matchRoute()` |
| `src/app/pageRegistry.tsx` | 页面自动接线（`import.meta.glob` 扫 `src/pages/`）→ `resolvePage()` / `knownPages()` |
| `index.html` | Vite 入口（`<script type="module" src="/src/main.tsx">`） |

## 目录职责（放什么 / 不许放什么）
| 目录 | 放什么 | 不许放什么 |
| --- | --- | --- |
| `src/app/` | 路由表、页面注册、外壳件（`AppShell` / `Sidebar` / `StatusBar` / `Toasts` / `ErrorBoundary`）、跨切面守卫测试 | 业务页面（在 `src/pages/`）；手写导航项（导航由 `src/app/routes.ts` 生成） |
| `src/pages/` | 每条路由一个入口文件（共 11 个）：7 个目录级 index.tsx，加上 `src/pages/cost.tsx`、`src/pages/network/nodes.tsx`、`src/pages/network/subscription.tsx`、`src/pages/providers/detail.tsx`；同目录放该页纯逻辑与子组件，如 `src/pages/providers/model.ts`、`src/pages/logs/TracePanel.tsx` | 跨页 import 另一个页面的私有文件（共享的进 `src/shared/`）；页面里直接 `invoke` |
| `src/shared/` | 跨页共用：`src/shared/api`（IPC 封装）、`src/shared/i18n`、`src/shared/components`、`src/shared/bindings.ts`（契约类型）、`src/shared/row.ts`（读原始对象）、`src/shared/format.ts` / `src/shared/motion.ts` / `src/shared/pricing.ts` / `src/shared/helpTip.ts` | import `src/pages/**` 或 `src/app/**`（当前 0 处，靠约定）；页面专属文案与组件 |
| `src/stores/` | 跨页面状态（zustand）：`src/stores/ui.ts`（主题）、`src/stores/app.ts`（toast） | 页面内状态（用 `useState`）；数据实体（用 TanStack Query） |
| `src/theme.ts` | 视觉令牌（`tokens` / `typography` / `space` / `radius` / `motion` / `SPACING_SCALE`）与 `buildTheme()` | 页面里写颜色、刻度外间距、字重 >= 600（`design.guard` 拦） |

## 公开 API（对外符号）
| 符号 | 位置 | 作用 |
| --- | --- | --- |
| `useCommandQuery` / `useCommandMutation` / `useAppStatus` | `src/shared/api/queries.ts` | 取数/写数的唯一入口（命令名 + query key） |
| `errorMessage` / `errorCode` / `fieldErrorsOf` / `isFixableError` | `src/shared/api/queries.ts` | 错误形状统一出口（`TA-xxxx` + 字段路径） |
| `useEventBridge` | `src/shared/api/queries.ts` | 订阅后端事件 → 按表失效 query（只在 `AppShell` 调一次） |
| `api`（按域分组的命令门面）/ `invokeCommand` | `src/shared/api/commands.ts` | 类型化调用；页面不许自己拼命令字符串 |
| `qk` / `ALL_RESOURCE_PREFIXES` / `EVENT_INVALIDATIONS` | `src/shared/api/keys.ts` | query key 常量与"哪个事件失效哪些 key" |
| `TokenAgencyError` / `registerMock` / `isTauri` | `src/shared/api/client.ts` | 唯一 IPC `invoke` 出口 + 浏览器 mock 注册 |
| `registerBaselineMocks` / `registerDevMocks` | `src/shared/api/mock.ts` | 基准场景 / 开发场景 mock（`src/App.tsx` 与测试用） |
| `EventName` | `src/shared/api/events.ts` | 后端事件名常量 + `listen` 封装 |
| `zhCN` / `I18nKey` / `useTranslation` / `i18nKey` / `setLocale` | `src/shared/i18n/index.ts` | i18n 出口；真源 `src/shared/i18n/zh-CN.ts`，`src/shared/i18n/en.ts` 是 `Record<I18nKey, string>`（缺键 `tsc` 红） |
| `PageHeader` | `src/shared/components/PageHeader.tsx` | 页头（标题 / 说明 / 右侧操作） |
| `HelpTip` | `src/shared/components/HelpTip.tsx` | 区块说明的 `?` —— 长解释只许进这里 |
| `StatusDot` / `KernelStateDot` | `src/shared/components/StatusDot.tsx`、`src/shared/components/KernelStateDot.tsx` | 状态语义（绿=正常 黄=警告 红=失败 灰=未知） |
| `AutoEllipsis` / `useIsOverflowing` | `src/shared/components/AutoEllipsis.tsx` | 一级界面省略；溢出才挂 tooltip |
| `useDataGridPreset` / `dataGridSx` / `dataGridDefaults` | `src/shared/components/dataGrid.ts` | 表格统一样式与本地化（行高 / 表头 / 选中 / 悬停） |
| `ConfirmDelete` | `src/shared/components/confirmDelete.tsx` | 删除确认（先确认；被引用时才给级联） |
| `SECTION_SX` | `src/shared/components/sectionSx.ts` | 区块容器样式 |
| `outcomeDisplay` / `LimitGapNotice` / `DisabledReason` | `src/shared/components/honestDisplay.ts`、`src/shared/components/LimitGapNotice.tsx`、`src/shared/components/DisabledReason.tsx` | 诚实显示层：判定在后端，前端只消费原文 |
| `useUiStore` / `resolveThemeMode` / `systemPrefersDark` / `useAppStore` | `src/stores/ui.ts`、`src/stores/app.ts` | 跨页状态（主题 / toast） |
| `buildTheme` / `tokens` / `typography` / `SPACING_SCALE` | `src/theme.ts` | 主题构造与视觉令牌（颜色/间距的唯一来源） |

## 依赖
- **框架**：React 19 + react-dom、react-router-dom v7（`HashRouter`）、MUI v7 + @emotion/* 主题与组件、@mui/x-data-grid（表格页）。
- **数据**：@tanstack/react-query v5（服务端状态；变化靠事件失效，不靠轮询）、zustand v5（跨页 UI 状态）。
- **表单 / 校验**：react-hook-form + zod + @hookform/resolvers（配置类表单）；@tauri-apps/api（IPC 与事件）。
- **展示**：echarts（趋势图）、dayjs（时间）、i18next + react-i18next（中英双语；`LOCALES` 只有这两门）。
- **构建 / 测试**：vite 6 + @vitejs/plugin-react、typescript 5.7（`npx tsc --noEmit`）、vitest 3 + jsdom + @testing-library/react。
- 包名**不写反引号**：`repoMap.guard` 会把带 `/` 的反引号 token 当仓库路径核对，而依赖包不在仓库里。

## OWNER
- 前端分工：**F1 外壳**（`src/app/**`、`src/stores/**`、`src/shared/**`、`src/theme.ts`）、**F2 看板 + 日志**、**F3 配置类**（服务商 / 模型）、**F4 网络 + 成本 + 设置**。
- 路由级归属见 `src/app/routes.ts` 的 `owner` 字段（`F1`–`F4`）。
- **已删页面**：`channels` / `policies` / `onboarding` 三页在本仓已不存在。
- 单个源文件头没有归属标记的一律记 **未标注**。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| UI / 单元测试 | `src/app/*.test.tsx` 与 `src/app/*.test.ts`：共 36 个文件（25 个页面/工具 + 11 个守卫），约 638 条 `it` / `test` |
| 跨切面守卫 | `src/app/*.guard.test.ts`（11 个，见下表；全部与页面同目录，便于一起跑） |
| 运行 | `npx vitest run <文件>`（全量 `npx vitest run`）、`npx tsc --noEmit` |
| 真机 e2e（由 Lead 统一跑） | `scripts/e2e/smoke.mjs`：按 `data-testid` 驱动，所以 testid 是契约 |

守卫逐条（每个都在拦一种"静默失效"）：
| 守卫 | 拦什么 |
| --- | --- |
| `src/app/commandSurface.guard.test.ts` | 页面引用了 **Rust 没注册**的命令（浏览器 mock 下看起来正常，真机点了会失败） |
| `src/app/argsShape.guard.test.ts` | 前端发的入参形状与 Rust 实现不一致（例：`usage_*` 的 `range`） |
| `src/app/serdeDefault.guard.test.ts` | `ProviderInput` 的可选字段集与 Rust `#[serde(default)]` 不一致（硬编默认值 / 运行时必填报错） |
| `src/app/registryClaim.guard.test.ts` | 源码里写着"某命令尚未注册"、而它其实已经注册（对注册表的状态断言必须是真话） |
| `src/app/rowReaders.guard.test.ts` | 页面自己再写一份 `asRow` / `pickStr` / `pickNum` / `pickBool`（只许 `src/shared/row.ts` 定义） |
| `src/app/protocolList.guard.test.ts` | 协议下拉清单与 Rust `enum Protocol` 的 serde 线上名漂移 |
| `src/app/design.guard.test.ts` | emoji / 硬编码颜色 / 刻度外间距 / 字重 >= 600 / 版面上 >= 40 字的长句 |
| `src/app/duplicateCopy.guard.test.ts` | 同一页面文件里同一段长文案近距离出现两次（同一屏说两遍） |
| `src/app/uiMinimalism.guard.test.ts` | 五条极简判据：实现词 / 版面上 >36 字 / 死文案棘轮 |
| `src/app/noPlaceholder.guard.test.ts` | 路由没有真页面（占位壳会安静地把"没做"伪装成"做了"） |

`src/app/repoMap.guard.test.ts` 额外校对**本文件与 `AGENTS.md`**：反引号里的路径必须真实存在、路由数等数字必须与代码一致。

## 关键数据流 / 边界
- 页面 → `src/shared/api`（`useCommandQuery` / `useCommandMutation` / `api`）→ `invokeCommand` → Tauri IPC → Rust 命令；失败统一成 `TokenAgencyError`（`TA-xxxx`）。
- 后端事件 → `useEventBridge()`（`AppShell` 里订阅一次）→ 按 `EVENT_INVALIDATIONS` 失效 query key。
- **前端不直连上游**：仓里没有任何 HTTP 客户端；`invoke` 只出现在 `src/shared/api/client.ts`，`listen` 只在 `src/shared/api/events.ts`。唯一例外：`src/pages/cost.tsx` 动态 import 了 @tauri-apps/plugin-dialog 的保存对话框。
- 契约真源在 Rust：`src/shared/bindings.ts` 由 `scripts/gen-bindings.ps1`（`npm run gen:bindings`）生成/覆盖，文件头写明"生成替换尚未落地，当前是手写契约"；要改字段先改 `crates/ta-core/src/domain/mod.rs` 与 `src-tauri` 命令，再重新生成。
- 文案：真源 `src/shared/i18n/zh-CN.ts`；`src/shared/i18n/en.ts` 必须键齐（类型约束，`tsc` 拦）。

## 已知陷阱
- 别名 "@/app" 在 Windows 上会先命中 `src/App.tsx`（文件系统大小写不敏感）→ 一律写显式路径（`src/App.tsx` 的注释实测记载），否则 `tsc` 报 "only in casing"。
- `src/shared/bindings.ts` **不许手改**：它按生成物管理，手改会在下次生成时被覆盖。
- 缺页面**不会白屏也不会报错**：`resolvePage()` 返回 `undefined` → 渲染 404；要发现"路由没有真页面"得靠 `noPlaceholder.guard`。
- `data-testid` 是 e2e 契约：可以加，删之前先搜 `scripts/e2e/smoke.mjs`。
- 测试读 i18n 真源（`zhCN`），不要把中文句子抄进断言，否则改文案就红。

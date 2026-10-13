# docs/26：动画优化 + 一级界面"宽度不够就省略"（第 78 轮）

> 来源：用户第 78 轮两条原话：
> ①「动画效果要优化」；
> ②「一些比较长的信息类东西在软件宽度不够的时候自动收成……，当然只限于一级界面，也就是概览、服务商等
> 可以直接通过侧边栏到的，比如我点开想看详细信息的那种肯定不加。**给任意地方加……逻辑之前，先确认
> 这个被……隐藏的信息可以重新看到，比如点看详细**」。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；每步先证明测试会红。

---

## 1. 动画（[15 §7] 的落地与收口）

现状：动效散在各处（`QuotaBar` 的硬耗尽脉动、`motion` 令牌、MUI 默认过渡），
没有统一的"什么时候能动 / 能多长 / 用户说不要动怎么办"。

规则（本轮定死）：

1. **时长只有一个来源**：`theme.ts` 的 `motion`（[15 §7]：≤250ms，ease-out）。组件里**不许**写
   `transition: all .3s` 这类字面量；要过渡就用 `motion.enter` / `motion.quick` 配 `motion.easing`。
   **MUI 自己的组件也走这一套**：`theme.transitions` 必须映射到令牌 —— 不映射就等于留了
   第二个时长来源（MUI 默认 `standard: 300ms` / `complex: 375ms`），而且**超预算**。
2. **只有一处循环动画**：额度到硬耗尽时的脉动（[15 §7] 明许）。其余一律一次性过渡。
   * 它的周期记在 `motion.pulse`（1800ms），是 250ms 预算的**唯一**例外 ——
     理由：250ms 管的是"因果过渡"，而脉动是**持续指示**，周期短于 ~1.5s 就成了闪烁；
   * 它的缓动是 `motion.pulseEasing`（`ease-in-out`，必须对称），**不能**复用单向的
     `motion.easing`（那会得到"喘一下再猛吸一口气"的不对称呼吸）；
   * 组件里**引用令牌**而不是抄值：抄值只能让守卫"要求它等于令牌"，令牌一改就红。
3. **必须尊重 `prefers-reduced-motion: reduce`**：系统说"不要动画"时，
   循环动画与位移/缩放过渡全部停掉，**颜色变化保留**（它不引发前庭反应）。做法是 CSS
   媒体查询三层门：① 所有动画 `0.01ms × 1`（属性级 + `!important`，与注入顺序无关）；
   ② `transition-property` 收成只有颜色类（位移/缩放/宽度瞬变，颜色反馈仍按自己的令牌时长走）；
   ③ 脉动的 `@keyframes` 在同一媒体查询里给一份静止版。不用 JS 判断（首帧不会闪）。
4. **不做装饰性动画**：不加页面入场、不加卡片浮起、不加骨架屏之外的任何"好看但没用"的东西。
   优化的目标是"跳变消失、状态变化看得出来"，不是"动画变多"。
5. **不许引发布局抖动**：只动 `opacity` / `transform` / `stroke-dashoffset` / `width`（进度条那类
   已经在动的）这类；不动 `height` / `margin` / `padding`。

验收：`theme.tokens.test.tsx` 扩一条"动效只有一个时长来源"的机械检查；
循环动画的 `@keyframes` 必须在 reduced-motion 媒体查询里被关掉（并用测试钉住字符串）；
`design.guard` 继续绿。

**落地结果（第 78 轮）**

| 项 | 状态 | 证据 |
| --- | --- | --- |
| `theme.transitions` 全量映射到令牌（MUI 的 300/375ms + `easeInOut` 不再存在） | 完成 | `src/theme.ts`；`theme.tokens.test.tsx` 新增 3 条（含"拿 MUI 默认主题来跑必须报错"的自证） |
| reduced-motion 三层门（动画停 / 只留颜色过渡 / 脉动关键帧静止版） | 完成 | `src/theme.ts` 的 `MuiCssBaseline` 块；`motion.test.tsx` 断言它**真的落进样式表** |
| 唯一循环动画的周期/缓动进令牌（`motion.pulse` / `motion.pulseEasing`） | 完成 | `src/theme.ts`；`QuotaBar.tsx` 改为引用令牌 |
| 侧边栏折叠不再过渡 `width`（布局属性，整壳每帧重排） | 完成 | `src/app/Sidebar.tsx`（那一行删掉，`WIDTH_SITES` 登记同步删掉） |
| 机械守卫（扫源码：字面量时长/缓动、第二个 `infinite`、未登记 `width`、reduced-motion 是否落地） | 完成 | `src/app/motion.test.tsx`（6 条，每条都有"种一个违规必须被抓"的自证） |
| 组件里新增装饰性动画 | **没做**（有意） | 一个都没加：目标是"跳变消失、状态看得出来"，不是"动画变多" |

## 2. 宽度不够就省略（**只在一级界面**）

一级界面 = **侧边栏能直接到达的页面**：概览(`/`)、服务商(`/providers`)、模型(`/models`)、
成本(`/cost`)、日志(`/logs`)、网络(`/network`)、设置(`/settings`)、
访问密钥(`/apikeys`)。

> **页面清单已变**（第 314/315 轮）：`/policies`（路由策略）、`/routing`（模型路由）、
> `/channels`（导航标签「套餐与线路」）三页**已删除**，见 [docs/48](48-balanced-sticky-only-routing.md)
> 与 [docs/49](49-channels-page-removed.md)。上面的"一级界面"集合已按现状更新。

**不打**在详情页（`/providers/:id`、`/network/nodes`、`/network/assignments`、日志轨迹面板）——
用户原话："我点开想看详细信息的那种肯定不加"。

### 硬规则（用户明确要求的安全阀）

1. **能被收起来的，必须能被重新看到。** 加任何省略之前先回答"用户怎么再看一遍"：
   * 表格行里的**实体名/标识**：点这一行进详情（已有的入口）就是恢复路径；
   * **没有详情页的单元格值**（数字、错误原文、上游模型名…）：**悬停/聚焦出完整内容**（tooltip）就是恢复路径；
   * **两条都够不着的地方，一律不省略**（宁可换行/撑宽，也不把信息藏进一个没有出口的"..."）。
2. **只在真的溢出时才给 tooltip**：用 `ResizeObserver` 比 `scrollWidth > clientWidth`，
   没溢出就不挂 tooltip（免得每个单元格都冒一个空提示）。
3. **全文必须仍在 DOM 里**：用 CSS 省略（`text-overflow`），不要用 JS 截断字符串 ——
   读屏、复制、"在页面里搜索"都还要能用；被省略时给触发元素 `tabIndex=0`，键盘用户也能看到全文。
4. **不省略错误原文与 TA 码**（[docs/22 §2] 红线）：错误那是"藏问题"。错误单元格按原样换行显示。

### 落地

新共用件 `src/shared/components/AutoEllipsis.tsx`（`AutoEllipsis` + `useIsOverflowing`），
先在**一级界面里确实会溢出的长文本单元格**上用（服务商名/地址、模型名与别名、通道的上游模型名、
策略名、日志里的接口路径等）；每个使用点都要在注释里写出"恢复路径是什么"。

验收：新单测钉住"溢出才出 tooltip / 全文仍在 DOM / 未溢出不出 tooltip / 键盘可达"；
`design.guard`、`honestDisplay`、`theme.tokens` 继续绿；详情页**零**新增省略用法（用测试或守卫钉住）。

**落地结果（第 78 轮）**

| 项 | 状态 | 证据 |
| --- | --- | --- |
| `AutoEllipsis` + `useIsOverflowing`（纯 CSS 省略、只在真溢出时挂 Tooltip、溢出时键盘可达、测量前恒不渲染） | 完成 | `src/shared/components/AutoEllipsis.tsx`；`autoEllipsis.test.tsx` 10 条 |
| 一级页面落地 23 处（每处代码里写明**恢复路径**） | 完成 | dashboard / providers 列表 / models / channels / policies / cost / logs / network / settings / apikeys（各页测试各加一条断言） |
| 详情页零用法（四个文件由测试直接读源码钉住） | 完成 | `providers/detail.tsx`、`network/nodes.tsx`、`network/assignments.tsx`、`logs/TracePanel.tsx` |
| 错误原文 / TA 码 / 数值 / 结论文案**不省略** | 完成 | 使用点逐条确认（`logs` 错误列保持原样换行，并写了"这里刻意不用 AutoEllipsis"） |

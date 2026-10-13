# docs/22：长篇介绍收进「问号圆圈 + 悬停提示」（第 74 轮）

> 来源：用户第 74 轮原话：
> 「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个问好加一个圆圈，
> 鼠标移上去能看到提示那种」。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；页面只调用已注册命令；每步先证明测试会红；共享工作区不留不可编译状态。

---

## 0. 这条需求要解决什么（先把它说准）

用户要的不是"删掉解释"，而是**把解释从版面上收进一个 16px 的问号圆圈里**：需要的人悬停（或键盘聚焦）才展开，
不需要的人不用被一屏字挡住。所以：

- **正文一个字都不许丢** —— 它变成提示气泡的内容，并且同时是触发按钮的 `aria-label`
  （提示气泡是懒渲染的，`aria-label` 保证读屏与"只看无障碍树"的调用方仍然拿得到全文）；
- **不该收的东西必须留在页面上**（见 §2 的红线）—— 把错误信息塞进 tooltip 是"藏问题"，不是"排版"。

## 1. 已落地的共用件（主管）

`src/shared/components/HelpTip.tsx`（从 `@/shared/components` 导入 `HelpTip`）：

```ts
<HelpTip text={t("some.long.text")} testId="<owner>-help" />
```

- 视觉：`HelpOutlineIcon`（就是一个圆圈里一个问号）16px，`color: "text.secondary"`，`IconButton size="small"`；
  **没有颜色字面量、没有 emoji**（`design.guard` 会查）。
- `aria-label` = `text`（全文）；MUI `Tooltip` 悬停/聚焦展开，`arrow`。
- `PageHeader` 已经改完：`description` **不再内联渲染**，改成标题右边的 `page-header-help`
  问号圆圈（正文进 tooltip + aria-label），**其余 props 不变**。

## 2. 收 / 不收的红线（写者按这个判，不许自由发挥）

**必须收进 HelpTip（"解释性的长句"）**

| 位置 | 判据 |
| --- | --- |
| `PageHeader` 的 `description` | **一律**收（页头只留标题 + `?`） |
| 区块说明（`SectionPaper` 的 `description`、手写区块的说明行） | **超过 12 个汉字/字符**就收；短的（如「只读」「来自配置」）留在原位 |
| 页面里成段的解释性 prose（≥ 20 字符，且属于"为什么这样设计/怎么理解这个数"） | 收 |

**一律不许收进 HelpTip（必须一眼可见）**

1. 状态结论与空态文案（「还没有订阅」「这份订阅还没有解析结果」…）；
2. 后端原文 / 错误信息（含 TA 码）；
3. 必填与校验提示（「必填」「名称已被占用」…）；
4. 破坏性操作的后果（删除确认框里的"会连带删掉…"）；
5. 任何**数值**（延迟、百分比、条数、时间戳）与它们的单位；
6. 按钮与控件的文字（按钮文案永远留在按钮上）。

判不准时的默认：**留在页面上**（宁可多一行字，也不要把需要立刻看到的信息藏起来）。

## 3. testid 约定（E2E 依赖它）

- 页头：`page-header-help`（`PageHeader` 内部已经写死，页面不用管）。
- 区块：`<区块自己的 testid>-help`（如 `network-lanes-help`、`settings-section-storage-help`）。
- 行内：`<行 testid>-help`。
- 同一区块**最多一个** `-help`（一排问号比一排字更糟）。

## 4. 文案

- i18n 的**键与含义不变**，只换渲染位置；同一个键里的文案**允许顺手改短**（必须是"更短的同一含义"），
  **不许删掉解释本身**。
- 改文案必须两语言同步（`zh-CN.ts` / `en.ts`），并且**i18n 归主管**：写者缺键/想改键报主管，不要自己插。

## 5. 验收

1. 单测（**先证明会红**）：对改过的每一页断言
   - `?-help` 存在且 `aria-label` 是**全文**；
   - 那段正文**不再**内联渲染（`queryByText(全文) === null`）；
   - §2 红线里的东西（错误原文、空态、数值）**仍然内联**。
2. 真机 E2E 新增 `step22`：悬停 `/providers` 页头的 `page-header-help` → 出现 `[role="tooltip"]` 且文本是原文；
   同时断言页头里**没有**那段正文的裸文本。既有步骤继续全绿。
3. 干净快照全量门禁 + 重出安装包 + A3/A4 不变量。

## 6. 分工与写范围

| 工作包 | 负责人 | 写范围 | 时序 |
| --- | --- | --- | --- |
| **WP-74A**：网络族（`/network`、`/network/nodes`、`/network/assignments`） | E1 | `src/pages/network/**`、`src/app/network.test.tsx` | 可立即开工（第 73 轮没占用） |
| **WP-74B**：设置页 + 日志页 + 看板 + API key 页 | E1 | `src/pages/settings/**`、`src/pages/logs/**`、`src/pages/dashboard/**`、`src/pages/apikeys/**` 与对应测试 | 可立即开工 |
| **WP-74C**：服务商 / 模型 / ~~通道~~ / ~~策略~~ / 成本 | E1 | `src/pages/providers/**`、`src/pages/models/**`、~~`src/pages/channels/**`、`src/pages/policies.tsx`~~（两页已删，[docs/49] / [docs/48]）、`src/pages/cost.tsx` 与对应测试 | **必须等第 73 轮（docs/21）合并之后**，否则会撞车 |
| 主管 | lead | `src/shared/**`（HelpTip / PageHeader）、`docs/**`、`scripts/e2e/smoke.mjs`、门禁与安装包 | 立即 |

> **[docs/49] 后记（2026-09-30）**：本表把 `src/pages/channels/**` 当成一个**要改的写范围**，
> 但那一页（导航标签「套餐与线路」）**整页已被用户裁决删除**，`src/pages/policies.tsx` 同理
> （[docs/48]）。表里保留原文（它如实记录了当时的分工），**现在照着它去找那两个目录会找不到**。
> 与"长简介收进问号"这条需求相关的**当前**落点：`step22` 的截图/悬停清单已去掉 `/channels`
> 与 `/policies`，只覆盖现存的 7 个页面。

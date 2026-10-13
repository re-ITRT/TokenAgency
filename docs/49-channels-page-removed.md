# docs/49：删掉「套餐与线路」页（`/channels`），把「新增定价」搬进成本中心

> 用户一句话：「"套餐与线路"这一页没什么用，也可以删了」
>
> 这是继 [docs/48]（删 `/routing` 与 `/policies`）之后**第二次删页**。
> 上一轮删页意外暴露出一个数据丢失 bug，所以这一轮**先侦察、后动手**，
> 并且把"这次到底丢了什么能力"逐条写清楚 —— 不写成"纯搬家"。

---

## 1. 侦察结论：这一页不只是"总览"

「套餐与线路」**不是一个独立页面**，它是 `/channels` 那一页的**导航标签**
（`src/app/routes.ts`：`path: "/channels"`、`titleKey: "nav.plansAndRoutes"`）。
那一页 74KB，里面除了只读总览，还装着写能力。逐条核对后：

| 它装的东西 | 别处有吗 | 结论 |
|---|---|---|
| 套餐链 / 矩阵只读总览 | 没有 | **随页消失**（这正是用户说的"没什么用"） |
| 「添加线路」向导（`channel_upsert`） | **有**：加服务商即自动建全部线路（`provider_lines_create_all`，用户第 81 轮的意图），之后只用滑块启停；服务商详情页也能改代理 / 启停 / 移除 | 重复，可删 |
| **能力手工声明**（vision / context_tokens / modalities） | **有**：扫描会从上游原文自动认领（`ConfigDoc::adopt_upstream_hints`，[docs/46 §303]） | 不再独特；但**手工覆盖**会没（见 §3） |
| **新增定价条目**（`pricing_upsert`） | **没有** —— 成本页自己写着"定价的写命令 `pricing_upsert` 后端已可用、本页仍未接入 → 不放按钮" | **必须搬走** |
| 通道组区块（`group_list` 读 + `group_delete` 删） | 没有 | **随页消失**（`group_upsert` 前端本来就无调用方） |
| 通道批量删除 | 单条删除在服务商详情页仍在；**批量**没有 | 批量随页消失 |

所以这一页**不是纯冗余** —— 用户裁决前我把上面这张表摆出来问过，用户的选择是：

> **删页，把「新增定价」搬到成本中心。**

---

> **后记（2026-09-30，第 318 轮）**：本节描述的搬迁**已被 [docs/51](51-model-keyed-pricing.md) 取代**。
> 用户随后指出这一版本身逻辑有误 —— *"我是希望用户为每一个对外模型定价…而不是对服务商的线路计价"*
> + *"成本中心页面只做显示，价格配置到模型页面去"*。所以：
>
> * `pricing-add-*` 那串 testid（下面那张表）**已全部不存在**；
> * 定价入口改到 `/models` 每一行的 `models-price-open`，`match` 只写 `model`；
> * 成本中心只剩只读展示（`cost-pricing-readonly` 常驻说明 + 只读表格）。
>
> 本节**原文保留**（它如实记录了当时的样子），**不要再照着它实现**。
> 仍然有效的是本页其余各节（删页本身、能力减法、过期旧文档、e2e 配套里与定价无关的部分）。

## 2. 搬了什么（唯一的搬迁）

`pricing_upsert` 从 `/channels` 的线路向导搬进 **`/cost` 的「定价条目」区块**
（`src/pages/cost.tsx` 的 `PricingSection`，它本来就在读 `pricing_list` + 删条目）。

**为什么搬到这里**：定价本来就属于成本中心（否则"未定价模型算不出钱"这件事与
"在哪儿补价"隔着两页），而且成本页那句"本页仍未接入"的提示也正好被消掉。

**冻结的 testid 契约**（e2e 按它们驱动，**不许改名**）：

| testid | 含义 |
|---|---|
| `pricing-add-open` | 打开「添加定价」 |
| `pricing-add-provider` / `pricing-add-credential` | 服务商 / 凭据（**凭据按服务商过滤**） |
| `pricing-add-input` / `-output` / `-cached` | 三项费率 |
| `pricing-add-submit` / `-cancel` | 提交 / 取消 |
| `pricing-add-error` | 后端错误原文 |
| `pricing-add-issue-<field>` | 本地预检点名（`provider` / `credential` / `input` / `output` / `cached`） |

搬迁时**逐字保留**了原实现的语义（这些是历轮评审加固过的，不是随手写的）：

* `""` = 不计价；只认**非负十进制**（负号、半截小数、非数字一律不认）；
* `rates` **只放填了的键** —— 放 0 会把"没定价"写成"免费"；
* `match` 只有 `{ provider, credential }`，**绝不**把上游模型名当逻辑模型名塞进去；
* `source: "manual"` / `currency: null` / `effective_from: null`（用全局币种、立即生效，不替用户挑）；
* 非法价格 → **本地点名，一次命令都不发**。

---

## 3. 如实记账：这次是**能力减法**，不是纯搬家

删掉之后，界面上不再有入口的能力：

1. **手工覆盖线路能力**。扫描会自动认领上游声明（常见情况因此没事），但上游**没声明**、
   或声明错了、或用户想手工改成别的值时，**界面上再也改不了**，只能改配置文件。
2. **手工新增线路**（`channel_upsert` 新建）。建线现在只有"加服务商 → 扫描 → 建全部线路"
   这一条路，**不能**手工指定一条线路的上游模型名（扫不到的模型就建不出来）。
3. **套餐链 / 矩阵总览**（哪个模型在用哪些线路、排第几）。
4. **通道组（channel_group）的查看与删除**。`group_list` / `group_delete` 从此没有任何页面调用
   —— 命令仍注册、后端能力仍在，但**界面上没有入口**。
5. **通道批量删除**（单条删除在服务商详情页仍在）。

> 这些**不是"以后再做"的待办**，是这次删页**现在就发生**的代价。写在这里是为了
> 下一个人（或未来的我）不要以为是漏做。

**没有删的东西**（故意的）：`channel_upsert` / `channel_delete` / `channel_matrix` /
`group_list` / `group_delete` 等命令**全部保持注册**，`src/shared/api/commands.ts` 的门面包装
也一行不动 —— 老配置、导出的 YAML、以及未来的页面都还要用它们。删的只是**页面**。

---

## 4. 因此过期的旧文档（别照着实现）

| 文档 | 过期的地方 |
|---|---|
| [docs/17 §3.3](17-add-flows-and-capability-routing.md) | 整节 `/channels` 线路向导：`channels-add-*` 那串 testid **已全部不存在**；`channels-add-model` 更是早就不存在 |
| [docs/21](21-delete-and-lane-settings.md) | §通道删除 / 通道组删除两张表格里的 `/channels` 入口 |
| [docs/22](22-help-tips-for-long-copy.md) | WP-74C 的写作用域含 `src/pages/channels/**` |
| [docs/26](26-motion-and-first-level-ellipsis.md) | 页面清单里的 `/channels` |
| [docs/28](28-lines-belong-to-providers.md) | §UI 表格里「套餐与线路 `/channels`」两行（它当年描述的是**该页**的分组与钉顶条）；`pages/channels/*` 的交付记录 |
| [docs/29](29-auto-network.md) | 「套餐与线路页」删 `channels-lane-select` 那一条（页面已不存在） |
| [docs/30](30-minimal-ui.md) | 极简清单里 `/channels` 的三行与 `page---channels.png` 截图项 |

**处置**：这些都是**历史轮次记录**，按本仓一贯做法**保留原文**（它们如实记录了当时的样子），
以本文件为**唯一的新口径**。真正描述"当前页面清单"的地方（docs/26 的页面列表、
docs/30 的截图项）另加了指向本文件的注记。

---

## 5. e2e 的配套（这一页在脚本里被引用得最多）

删页之前，`scripts/e2e/smoke.mjs` 对它的依赖是：`channels-add` **43 处**、`#/channels` 12 处、
`channels-row` 11 处、`channels-delete` 7 处、`channels-lane` 5 处，外加
`addChannelViaDialog` / `ensureAddPrereqs` / `ensureSecondChannelForModel` 三个**前提 helper**。

最要紧的不是那几个断言，而是**前提 helper**：许多步骤靠它"先经界面造出服务商/凭据/模型/线路"。

**实测澄清了一件事**（删页前我以为要大改）：`ensureAddPrereqs` **本来就没走 `/channels`** ——
它走的是首启向导（provider + credential）+ `/models` 向导（model）+ `/network` 表单（lane），
所以不受删页影响。真正依赖那一页的只有两处，且**都只被 `step10` 调用**：

* `addChannelViaDialog`（线路添加向导，整段基于 `channels-add-*`）；
* `ensureSecondChannelForModel`（靠向导造第二条线路来做能力一致性比对）。

`step24` 早在用**服务商路径**建线路（扫描目录 → `providers-model-*`），不需要新写 helper。

---

## 6. 做过的两处"顺手修掉的真缺陷"

这两处都不是删页本身要求的，但都是在"搬定价"的过程中被**断言逼出来的**：

1. **`hasPrice` 定义了却没人调用** —— 搬过来的草稿整形里有个 `hasPrice` 闸门，但提交路径没接它，
   于是"三项价格全空"也能提交。后果不是界面难看，而是**语义错了**：
   `ResolvedPrice::is_priced()` 只看 `source`，而我们会写 `source: "manual"` ——
   空费率的条目会让 `is_priced()` 为真、`has_rates()` 为假，那个模型就从
   「没定价、花费算不出来」变成「有定价、花费 0」，即**把「没定价」写成「免费」**。
   现在三项全空会本地点名（`pricing-add-issue-none`）并**一条命令都不发**。
2. **两个下拉的 testid 挂错了层** —— 原实现用 `slotProps.select.inputProps` 把 testid 挂在
   MUI `Select` 内部那个**隐藏 native input** 上，而它是 `[role=combobox]` 那个 div 的**兄弟**、
   不是后代。于是"在 testid 里找 combobox"这条口径会落空：真机 e2e 的 `wdControlForTid`
   正是先找后代 combobox，结果会点到**不可交互的隐藏 input** 上。
   改成 testid 挂**容器** `Stack` 后，单测与真机用的是同一条口径。

> 第 2 条是单测先红、顺着根因查到真机也会红 —— 如果只把测试改成"往上找 combobox"，
> 就会把一个真机缺陷悄悄糊过去。这也是为什么这两个下拉的断言刻意写成
> `within(box).getByRole("combobox")`（**后代**查找，与真机同形状）。

---

## 7. 验证结果（本轮实测）

### 7.1 前端

| 项目 | 结果 |
|---|---|
| `tsc --noEmit` | 干净 |
| `vitest run` | **596 passed / 0 failed（30 文件）** |
| i18n 键位对齐 | zh-CN 与 en **1267 / 1267 完全一致** |
| `npm run build` | 成功；`cost` chunk 31.12 kB（吸收定价对话框），**channels chunk 已消失** |
| 产物核对 | `plansAndRoutes` 在 dist 里 **0** 处；`capabilities.vision` **2** 处（模型页仍在用） |

搬迁过来的定价断言：`src/app/channels.test.tsx` 的 ⑪/⑫ 里属于定价的那部分移入
`src/app/cost.test.tsx`（新增 `⑨ 添加定价` 一节，8 条），并补了 §6 那两处缺陷的回归断言
（空价不发、非法价不发、testid 可驱动）。

### 7.2 Rust

| 项目 | 结果 |
|---|---|
| `cargo fmt --all --check` | 干净 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 干净 |
| `cargo test --workspace` | **1306 passed / 0 failed / 1 ignored**（与改动前一致） |

命令面**一个都没动**：`src/shared/bindings.ts` 仍是 120 个命令，`command_surface_guard` 照常通过。

### 7.3 i18n 清理时**我自己犯的错**（如实记）

按"`channels.` 前缀 + 整块删"的方式清死键时，我**删多了 4 个仍被别处在用的键**：
`channels.add.provider`（服务商向导）、`channels.col.priorityWeight`（服务商详情列头）、
`channels.group.toggle`（模型目录的分组开关）、`channels.lane.inherited`（模型目录的继承说明）。
它们名字带 `channels.` 前缀，但用的**不是**被删的那一页。

根因：我写的引用扫描正则只匹配了**单点键**（`channels\.` 后直接是引号），
漏掉了**双点键**（`channels.group.toggle` 这类）。是 `tsc` 把它们逐个点了出来。

**处置与教训**：
* 4 个键已按 `git diff` 的原文恢复（`channels.group.toggle` 是未提交的新增键、diff 里没有，
  英文文案由我**重新翻译**；中文原文从删除前的实测输出取回）。
* 恢复处写了注释，标明"这些键属于别处，别再按前缀删"。
* 更根本的教训：**删 i18n 死键不能按前缀批量删** —— 必须逐键核对引用方。
  这次是 `tsc`（`I18nKey = keyof typeof zhCN`）兜住的，它把 4 处缺键全部报了出来。

### 7.4 e2e 配套（`scripts/e2e/smoke.mjs`）

| 位置 | 改动 |
|---|---|
| `addChannelViaDialog`、`ensureSecondChannelForModel` | **整块删除**（只被旧 step10 调用；`ensureAddPrereqs` 保留，它本就不走那一页） |
| `step10` | **换用途**：从"线路向导的能力块/计费块"改成「成本中心新增定价条目」——验三件事：控件齐全、空价与非法价本地点名且零命令、正常一条真落库（`input=1.5`） |
| `step21` | 删掉 4 档依赖 `/channels` 的通道删除（被点名拒删 / 无引用真删 / 解除引用后重删 / 批量删）与通道组删除，**如实记账**：单条线路删除改由 `step24` 在服务商页覆盖；"被引用→拒绝"、批量删除、通道组删除**当前没有界面入口** |
| `step21` A 段 | 从 `/channels` 的"线路级代理下拉已删 + 只读行仍在"改到**服务商详情页**验同一件事（行内无代理写入控件；逐行有启停；`providers-edit-lane` 入口仍在） |
| `step4` | 去掉 `/channels` 那一段诊断，只查 `/network` 的 lane 写入口；SKIP 文案同步 |
| `step22` | 截图清单去掉 `#/channels` |
| `STEP_TITLES` | `step10` 换成新标题；`step21` 文案去掉"通道页" |

`node --check scripts/e2e/smoke.mjs` 通过。改完后脚本里剩下的 `/channels`、`channels-*` 字样
**全部在注释里**（作为"这档为什么没了"的解释），没有一处活代码引用。

> 这里也踩了一个坑：我在**DOM 脚本的模板字符串内部**写了一对反引号（注释里的
> `` `providers-edit-lane` ``），把模板字符串提前闭合、脚本语法直接坏掉。`node --check` 抓到了。
> 教训：往 `wd.exec(\`...\`)` 里塞注释时，**反引号一律不能用**。

#### 7.4.1 真机实测（这一版安装包）

第一次全量跑 `PASS=28 SKIP=2 FAIL=1`：**唯一的 FAIL 是 `step21`，而且它是我自己改坏的判定式**——
`deleteOk` 里写死了 `results.length >= 5`（历史上通道那 4 档还在时的数字）。删掉 4 档后只剩
4 档（定价 / 模型 / 服务商 / 密钥入口不该存在），`failed.length === 0` 但档数下限卡住 → 判红。
证据里那一行自己就说明了问题：`4 档删除里失败 0 档（真删掉 3 档…）；批量删除=true`。

改法：下限改成 `>= 4` 并写明它表达的是"**至少覆盖多类实体**"，不是"必须等于某个历史数字"。

第二次全量跑（同一安装包）：

```
PASS=29  SKIP=2  FAIL=0   (exit 0)
```

* `PASS step10`：空价与非法价都在本地点名且不发；正常一条真的落库（`input=1.5`）。
* `PASS step21`：线路行内代理控件=0、逐行启停齐全、`providers-edit-lane` 入口仍在；
  4 档删除真删掉 3 档；DOM 兜底=0 次。
* 两个 SKIP 都是**如实记录**，不是掩盖失败：
  `step15`（UI 路径随 `/policies` 删除，后端规则另有覆盖）、
  `step28`（夹具这一轮也量不出真实节点延迟 —— 环境问题）。

> 顺带修了一处**证据文案说错话**：批量删除那一行的括号里写死"失败 0 条"，
> 而实际这一轮的拒绝数是 1（服务商被凭据引用、后端如实要求 `force=true`）。
> 判定式本身是对的（`summaryOk` 验的是"失败数 == 选中 − 真删掉"且带 TA 码），
> 错的是那行给人看的汇总文案 —— 它会把一个**正确的拒绝**说成"0 条失败"。已改成打印真实值。

### 7.5 一次**偶发假红**的如实记录（并发跑 cargo 的代价）

收尾阶段跑 `build.ps1` 时出现过一次刺眼的红：第 4 阶段报
`test result: FAILED. 137 passed; 149 failed`（`-p tokenagency --lib`）与
`boot_real_kernel` 的 1 个失败，`error: 8 targets failed`，整体 exit 1。

**这不是产品缺陷，但也不能当没发生** —— 我按"先证明再结论"的方式查了：

| 验证 | 结果 |
|---|---|
| `cargo test -p tokenagency --lib` 单独跑 | **286 passed / 0 failed** |
| 同一条命令**连续跑 8 次** | 8 次全部 **286 / 0** |
| `cargo test --workspace`（手工） | **1306 / 0 / 1 ignored** |
| `cargo test --workspace --no-fail-fast`（build.ps1 用的原命令，手工） | **1306 / 0 / 1 ignored** |
| `build.ps1` 严格串行重跑 | **exit 0**，五阶段全过 |

根因是**我自己的操作**：那次红的**前一步**我刚跑过 `cargo test`（做门禁核对），
紧接着就让 `build.ps1` 再跑一遍 `cargo check / clippy / test` —— 两个 cargo 进程
**争用同一个 `target/` 目录**。这种争用在这个仓里是已知会产生**大面积假红**的形态
（149/286 一次失败、失败集合横跨互不相关的模块，正是这个特征）。

**处置**：改成**严格串行**——任何 cargo 命令跑完再发下一条，不并发 cargo。
最后一次串行 `build.ps1` 是 exit 0。

> 教训与 [docs/31] 里那条"`Move-Item` 保留 mtime 导致跑旧二进制"同源：
> **构建/测试工具链不是并发安全的**。凡是给结论当证据的那一次跑，必须是独占的。

### 7.6 真机 e2e 又暴露出**第二处假红**（不在本轮改动范围内，但必须修）

最终产物那次全量跑出现 `FAIL=1`，红的是 **step30**（订阅更新后沿用上次出口），
而**证据行自己就写着"不算换出口"**：

```
第 1 轮：status=same_address_still_there node="日本-优化2" now="日本-优化2"
        （基准="新加坡-优化2-GPT"）
        （沿用成立：… 名字从上轮的 "新加坡-优化2-GPT" 变成了 "日本-优化2" —— 同一地址的另一个名字，不算换出口）
```

**产品行为是对的**（应用报 `same_address_still_there` = 地址没变、按规则沿用；
日志 `node` 与内核 `now` 一致 = 决定真写进内核了）。红的是挂具自己的判定：

* step30 的**最终闸门**里挂着 `|| sticky.node !== sticky.base`；
* 而 `sticky.base` 在"名字变了"时被存成 **`"旧名 →同名地址 新名"`这种给人看的说明串** ⇒
  恒不等于 `node`；
* 于是：名字没变时它**侥幸通过**（等于没验任何东西），名字真的变了就**必红**。
* 更矛盾的是，上面 `same_address_still_there` 分支的注释**已经写明**"名字变了不算红"，
  最终闸门却又把它判红 —— 同一档里两处判据在打架。

**修法**（不放松真判据）：
`status === "same_address_still_there" && sticky.now === sticky.node`。
这两条才是挂具能**独立**验的东西；"地址是否同一条"本来就只能采信应用自己的判据
（挂具读不到地址：内核节点对象的 `/proxies/<key>` 里没有 address 字段，[docs/46 §301] 实测确认）。
`sticky.base` 从此刻起**只用于文案**。详细记录见 [docs/46 的对应小节](46-bundled-kernel.md)。

**并且让这条修复真的有牙**：新加 `node scripts/e2e/smoke.mjs --selfcheck`
——把该判定抽成纯函数 `judgeSticky` 后**直接喂用例**（不用浏览器、不用 exe、秒级）：

```
✓ 名字没变 → 绿        ✓ 同一地址换了名字 → 绿（真机那次假红的原样输入）
✓ 状态不是「沿用」→ 红  ✓ 日志与内核不一致 → 红  ✓ 内核读不到（now=null）→ 红
自检通过：5/5
```

> **为什么要自检而不是"再跑一次真机"**：这条分支**只在订阅里同一地址换了名字时**才走到
> （机场里常见、但不是每轮都碰上）。修复后的第一次全量跑恰好**名字没变**，
> 也就是说那一次**根本没走到我修的那一行** —— 拿它当"修好了"的证据是无效的。
> 所以我注回了修复前的旧判据做**验牙**：自检立刻变红、且**只有**那条关键用例变红
> （3 条反向用例仍正确判红 ⇒ 判定没有被放松成"什么都不查"）。验完已恢复，注入痕迹清零。

### 7.7 本轮交付的安装包（最终）

| 项目 | 值 |
|---|---|
| 安装包 | `target/release/bundle/nsis/TokenAgency_0.1.0_x64-setup.exe` |
| 大小 / SHA256 | 23,553,326 B = **22.46 MiB** / `24AC66021511CDA2…` |
| `build.ps1` | exit 0（5 个阶段全过：前端构建 → IPC 绑定 → Rust 门禁 → Rust 测试 → NSIS 打包） |
| 交付 exe | `6791CB1919317426…`（14.43 MB） |
| 真机 e2e | `PASS=29 SKIP=2 FAIL=0`（exit 0） |
| e2e 自检 `--selfcheck` | 5/5 |
| 签名 | 未签名（本仓一贯状态，不是本轮引入） |

**这条 e2e 结果对哪一份产物负责 —— 有 provenance 证据，不是凭印象**：
`smoke.mjs` 把被测 exe 的路径 + size + mtime + sha256 写进 `build-identity.json`，
本次记录的是 `C:\Users\...\TokenAgency\tokenagency.exe` / `6791CB1919317426…`，而

* `target/release/tokenagency.exe` = `6791CB1919317426…`
* 安装后的 `%LOCALAPPDATA%\TokenAgency\tokenagency.exe` = `6791CB1919317426…`

**三者逐位相同**，且安装包 mtime（13:05:19）之后产品源码**一个字节都没动**（已核对 mtime）。
也就是说：**绿的那一次跑的就是要交付的那一份 exe。**

> ⚠️ **这个仓的安装包不是可复现构建** —— 实测同一份未改动的源码连跑三次 `build.ps1`，
> 拿到三个不同的 exe/setup 哈希（`20890F4D…` → `0F4682ED…` → `E1CB38B2…` → `24AC6602…`）。
> 所以"哪一次 e2e 算数"**不能靠时间先后猜**，只能靠 `build-identity.json` 里那个 sha256 对齐。
> 收尾时我正是这么做的：最后一次 `build.ps1` 之后**先装、再对它跑 e2e**，然后**不再重建** ——
> 否则"交付的包"与"验过的包"又会分叉。
> 这条对下一个接手的人同样成立：**要交付就必须"最后构建 → 装 → 跑 e2e → 封版"，中间不要再 build。**

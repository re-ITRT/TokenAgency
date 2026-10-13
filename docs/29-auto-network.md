# 29 · 「自动」网络与代理归属（在写）

**状态**：进行中（跨轮目标 `goal-b2cb56f3`；已用 12 轮，**远未完成**）。
每一刀都要求：单测 + 真机验证 + 更新本文。

## 0.1 用户第 112 轮的两个要求（一条确认 + 一条补做）

> 继续跑没有完全做好的地方，还有，请你确认为服务商选线路，存在于"添加"向导
> 且能看到所有网络代理以及"自动"，"直连"

**确认结果：不在。** 向导第 1 步那个代理下拉是第 88 轮（A 期）按"代理只属于服务商"
**删掉**的，当时只把服务商级代理做到了**详情页**。所以"添加向导里选线路"目前**缺失**——
这是我这轮要补的（下面第 113 轮做了一半：选项构造与后端标记已就位，向导 UI 还没接）。

### 第 113 轮：补到哪一步了（如实）

**已完成（`tsc` 0、`npm test` 30 文件 / 697 全绿、门禁一把过）**：

1. **后端两种标记都能解析**（`provider_upsert`）：
   `direct:` → 建/复用那条 `kind: direct` 线路；`auto:<订阅>` → 建/复用
   `subscription_auto` + 显式 `select`。前端只发标记，**不建线路、不编 id**；
2. **选项构造**（`providerLaneChoices`，5 条单测）：`直连（本机网络）` **第一** →
   `不指定（继承）` → `自动`（多份订阅时带订阅名）→ **所有已配置线路**；
3. **数据链路**：`Step1Draft.lane`（默认 `direct:`）→ `ProviderDraft.lane` →
   `provider_upsert` 载荷里的 `lane`。也就是说**向导第 1 步现在真的会带代理去落库**。

**还差（下一轮第一件事）**：向导第 1 步的**那个下拉控件**（`pw-lane`）还没插进 JSX。
这一轮我试了两次插入都没对上锚点，按"不盲目重试"的规矩停手 —— 剩下的活是纯 UI 插入 +
一条单测（控件在、选项含 `直连/自动/线路`、选了之后载荷里 `lane` 是那个值）。

**操作教训（第三次了）**：大段 JSX 插入不能用"锚点 + Replace"硬猜空白；
正确做法是**先读那一段、再整块替换**。这条已经写进 [docs/30 §3.8] 的末尾。

### 第 114 轮：**真机确认通过** —— 服务商级下拉里有「直连 / 自动 / 所有线路」

* 向导第 1 步的**那个下拉控件接上了**（`pw-lane`，用与详情页**同一份** `providerLaneChoices`），
  值走 `Step1Draft.lane → ProviderDraft.lane → provider_upsert`；`npm test` **30 文件 / 697 全绿**，
  `tsc` 0；
* 真机（release 包）`step25` **PASS**，证据是它真的把下拉展开了：

```
服务商级代理下拉选项=["直连（本机网络）","未指定（继承默认线路，末端直连）","自动","TA E2E 线路"]
```

顺序与设计一致：**直连 → 不指定（继承）→ 自动 → 所有已配置线路**。
第 110 轮那个"下拉里没有「自动」"的失败，根因是**排查路径上的假象**（debug 包前端陈旧 +
`subscription_list` 的视图包裹），两处都已修掉并在这一轮拿到正面证据。

* 总账回到基线：**PASS=24 / SKIP=1 / FAIL=3**（三档仍是既有的 harness 陈旧判据）。

**还没验到的一条**：**向导第 1 步那个下拉**的真机选项（代码在、单测过，但真机探针我插错了
作用域，`wizardHasDirect is not defined` 把 harness 弄崩过；已还原判决条件、harness 恢复正常）。
下一轮把它插到 `step25` 的**同一函数体内**再跑一次。

### 第 115 轮：**向导里也能选了**（真机正面证据，用户第 112 轮的要求闭环）

把探针插进 `step25` 的同一函数体后，真机（release 包）一次给出**两处**的下拉内容：

```
服务商级代理下拉选项=["直连（本机网络）","未指定（继承默认线路，末端直连）","自动","TA E2E 线路"]
向导第 1 步代理下拉（pw-lane）选项=["直连（本机网络）","未指定（继承默认线路，末端直连）","自动","TA E2E 线路"]
```

两处**逐字一致**（因为用的是同一份 `providerLaneChoices`）；`step25` 的判据现在同时要求
"向导里也能看到直连 / 自动 / 所有线路"，总账 **PASS=24 / SKIP=1 / FAIL=3**（三档既有 harness 判据）。

用户第 112 轮那句话至此**逐项对账**：

| 要求 | 状态 | 证据 |
| --- | --- | --- |
| 为服务商选线路**存在于"添加"向导** | ✅ | `pw-lane` 真机选项如上 |
| 能看到**所有网络代理** | ✅ | 下拉里有 `TA E2E 线路`（配置里的线路全列） |
| 能看到**「自动」** | ✅ | 一份订阅 = 一个「自动」（多份时带订阅名） |
| 能看到**「直连」** | ✅ | 排第一位，值 `direct:` |

**剩下的大件**（下一轮起）：**E 期**（真机证明「自动」选的是最低延迟、订阅更新后**不换 IP**）、
**C 期触发点②**（手动「更新」传 `manual = true`），以及极简工作流的 `/settings`、`/network`。

### 第 123 轮：**"内核没起 → 如实拒绝"真机拿到了**（+ 手动刷新链路走通）

把 E2E 的订阅 fixture 改成"每请求多一个节点"（否则坏更新守卫会拦下刷新，上一轮就是这个原因），
真机结果：

```
手动刷新：点击=true；界面回执="后端返回的刷新结果：成功 · 节点数 6 · 组数 1"
WARN tokenagency_lib::commands::net: 「自动」出口重算失败（这一家跳过）
     provider_id="ta-e2e" code=TA-1004
     error=请求格式不合法：内核没在跑：测不了「自动」出口的延迟（不猜一个出来）
```

两件事同时成立，正是要的行为：

* **C 期触发点②真的被走到**了（手动刷新成功后触发重算）——不是"没接上"；
* **内核没起时如实拒绝**：一条 warn、带 `provider_id` 与原因，**没有**编一个"选好了"的结论，
  也**没有**写内核（`set_select` 根本没被调用）。这条正是 [docs/29 §2.2] 写死的纪律。

**两处如实说明**：

1. 拒绝用的码是 `TA-1004`（`MalformedRequest`）—— 语义上并不贴切（请求本身没问题，
   是环境缺内核）。本仓库的规矩是**不自己发明码**，所以先复用最接近的既有码并在此登记；
   要更贴切就得新增一个错误码（那是契约变更，另行排期）；
2. **成功路径仍未验**：内核没起 → 走不到"按延迟重挑"，所以 `status=manual_refresh`
   那条日志还没在真机出现过。它属于 **E 期**（需要内核在跑），已在台账里如实标注。
### 第 122 轮：触发点②的**真机诊断**（结论：行为正确，但刷新本身失败了）

在 `step25` 里赋完「自动」并保存之后补了一次真实的「刷新」点击，探针这次**判了点击结果与界面回执**
（上一版是无条件推 evidence，等于没验，这是我的错）：

```
手动刷新：点击=true；界面回执="后端返回的刷新结果：失败 · 节点数 2 · 组数 1 ·
          节点数过少：上版 2 → 本次 2（少于 3 个）"
```

所以：**刷新失败 → 按设计不重算**（节点没变，重挑只是拿旧数据赌一把）——
日志里没有「自动」出口重算那一行是**正确行为**，不是没接上。

要在真机上看到"手动重算"与"内核没起 → 如实拒绝"，**前提是刷新必须成功**：
E2E 的 mock 订阅这次只给 2 个节点、而上版也是 2 个，撞上了坏更新守卫（要求有增长）。
下一轮把 mock 的节点数改成"本次多于上版"，这条链路就能走通。

**这轮的教训**：探针**必须判自己点到了没有、以及后端回了什么**。
"推一行 evidence 就完事"的探针等于没验 —— 这是我这几轮第三次栽在验证的**完整性**上
（前两次是：插错作用域、前端陈旧包）。
### 第 121 轮：**C 期触发点②接通**（手动刷新 = 重挑，`manual = true`）

`subscription_refresh` 命令里接上：刷新成功后，对这份订阅上的每个「自动」出口调用
`apply_auto_export(..., manual = true)` —— 用户点「刷新」就是明确说"重来一次"，
**无视粘性、按当前延迟重挑**；刷新失败时不重算（节点没变，重挑只是拿旧数据赌一把）。

分工（本轮唯一的设计决定）：**后台自动刷新 = 触发点①**（`manual = false`，粘性优先）；
**用户手动刷新 = 触发点②**（`manual = true`，重挑）。两者**共用同一个实现**
（`reapply_auto_export`，差别只有一个布尔值）——顺手把两份同样的循环去重，避免以后分叉。

`cargo check / fmt / clippy -D warnings` 干净、`cargo test -p tokenagency --lib` **280 通过**、
`build.ps1` 全量门禁一把过。

**真机只观察到一半（如实）**：日志里仍只有后台那条（`「自动」出口重算 0 家`），
因为 **E2E 的 `step25` 赋「自动」之后没有任何刷新动作** —— 手动路径没被走到。
下一轮在 `step25` 里补一次刷新点击（半轮），即可同时拿到"手动重算"与"内核没起 → 如实拒绝"。
### 第 118 轮：**D 期链路真机通过** —— 选「自动」保存后库里真的长出那条线路

探针（`step25` 里）走真实用户路径：进服务商详情页 → 代理下拉选「自动」→ 点保存 → 读库核对。

```
选「自动」并保存：保存=true；自动线路数=1（lanes 2→3）；
服务商 lane="lane-ta-e2e-sub-form-auto" 期望="lane-ta-e2e-sub-form-auto" → true
```

三件事同时成立才判 PASS：① 保存**真的发生**；② 库里**多了一条** `kind = subscription_auto`
且 `group_type = select` 的线路（lanes 2→3）；③ 服务商的 `lane` **逐字指向**它
（id 由后端按订阅派生 `lane-<订阅>-auto`）—— 说明"前端只发标记、后端建线路并写回真实 id"生效了。

总账 **PASS=23 / SKIP=1 / FAIL=4**：多出的一档是 `step22`（**既有 flaky**：证据里 logs / settings
的 tooltip 都通过，失败在 policies 那次遍历），与本轮无关；其余三档照旧（step2 / step10 / step21）。

**至此「自动」链路在真机上前后都通了**：入口（两处下拉）→ 保存（标记 → 线路）→ 自动刷新重算
（`「自动」出口重算 N 家`）。**还差最后一环**：内核在跑时"按 base_url 测延迟取最低 + 粘性"的
实机验证（E 期）；E2E 的 scratch 里内核通常没起，那时 `apply_auto_export` 会**如实拒绝**，
这条"不编结论"的行为也一并要钉。
### 第 117 轮：用户看出来的漏删 —— 列头里还挂着「网络代理」

> 线路那边虽然不单独显示网络代理了，但是最上面还是有网络代理的字段表格头

属实。第 88 轮删的是**行内**那个代理下拉，**列头**里那一列（`providers-lines-header` 里的
`K.linesProxy`）漏了 —— 当时那次替换**静默失败**（锚点没匹配却没有任何报错），
而 `tsc` 也不会因为"多一个列头"报错，所以一路留到今天。

**修法**：删掉列头里那一项（保留「名称」，滑块与计数不动），并在单测里加一条**回归断言**：

```ts
const header = screen.getByTestId("providers-lines-header");
expect(header.textContent).toContain("名称");
expect(header.textContent).not.toContain("网络代理");
```

**教训（值得单独记）**：这一轮和第 116 轮的两个 bug 是同一类 ——
**"删掉功能"时依赖"锚点替换成功"而没有断言**。文字替换失败是静默的，
所以每一处"删掉某个界面元素"都必须配一条"它不在了"的断言，否则只能等用户看出来。
`npm test` **31 文件 / 701 测试**全绿、`build.ps1` 一把过。

### 第 116 轮：单测抓到一个**真 bug** —— 向导根本没把代理发出去


第 115 轮真机证明了两处下拉的**选项**一致，但"选完之后发给后端的到底是不是那个值"没人验过。
补了一条载荷侧的单测（`src/app/providerWizardLane.test.ts`），它**立刻红了**：

```
选「直连」→ expected undefined to be 'direct:'
```

根因：仓库里有**两个** `providerPayload` —— 详情页那个（`editor.ts`）输出 `lane`，
向导用的那个（`wizard.ts`）**没有**。所以界面选得好好的，落库时 `lane` 是空的。

修法与踩到的两个连带问题（都值得记）：

1. 向导的 `providerPayload` 补上 `lane`（`undefined` = 这次不动它；`""` → `optional()` → `null` = 显式清空）；
2. `optional()` 原本是 `editor.ts` 的私有函数 → 导出后共用（**同一套语义不许抄两遍**）；
3. `EMPTY_PROVIDER_DRAFT` **不许**设 `lane: ""` —— 高级对话框不暴露代理，
   设了就会让它的载荷多出一个 `lane: null`，撞上既有的守卫
   「用户没填的字段不替后端编（Rust 侧有 `serde(default)`）」。这条守卫**拦住了我**，
   说明它是有效的：向导用自己的 `Step1Draft.lane`（默认 `direct:`）显式赋值，与对话框互不影响。

验证：`tsc` 0、`npm test` **31 文件 / 701 测试**全绿、`build.ps1` 全量门禁一把过。
**这条链路至此两侧都有测试**：选项构造（详情页与向导一致）+ 载荷真的带上所选值。





用户要的三类选项现在**构造出来了**（`providerLaneChoices`，有单测）：

| 顺序 | 值 | 说明 |
| --- | --- | --- |
| 1 | `direct:` | **「直连」**（用户第 81 轮就要求它单独一项且排第一）→ 后端建/复用那条 `kind: direct` 线路 |
| 2 | `""` | 不指定（继承链 → 默认线路 → 直连） |
| 3 | `auto:<订阅>` | **「自动」**（多份订阅时带订阅名）→ 后端建/复用 `subscription_auto` + 显式 `select` |
| 4… | 线路 id | **所有已配置的网络代理**（`lane_list`，名字读不到时回落 id） |

## 0. 截至第 97 轮的账（谁做完了、谁没有）

| 项 | 状态 | 真机证据 |
| --- | --- | --- |
| A 期：代理只属于服务商（线路级代理入口全删） | **完成** | `step25` PASS（线路行无代理控件、服务商级入口仍在）；`step23` 探针 `duplicated:0` |
| 订阅「自动更新时间」：**算**到期（一套钟） | **完成** | 日志两分支都验过（无到期 / 到期 N 条） |
| 订阅「自动更新时间」：**刷**（与手动同链路） | **完成** | 成功：`到期 1 条，成功 1 条` + 版本文件 0→6 + 流量行 0→1；失败：`成功 0 条，失败 1 条` 且库里确实没写 |
| B 期第一步：「自动」选谁（粘性规则） | **完成（纯函数）** | `crates/ta-subscription/src/auto.rs` 9 条测试；粘性按 IP、更快的也不换 |
| B 期其余：`LaneKind::Auto` / 按 base_url 探测 / 写内核 `set_select` | **未做** | — |
| B 期其余：粘性状态（"上次选的地址"）存哪 | **未做**（设计未定） | — |
| C 期：三个触发点接线 | **未做**（挂点已就绪：第 10 轮通的自动刷新链路） | — |
| D 期：服务商级下拉加「自动」 | **未做** | — |
| E 期：真机验证"选最低延迟 / 更新后不换 IP" | **未做** | — |
| 工作流 B（极简）：`/` 已核无需改；服务商密钥区删一处 | **部分** | 见 [docs/30](30-minimal-ui.md) |
| 工作流 B（极简）：`/settings`(169) / `/network`(145) 等 | **未做** | — |

**下一步的第一件事**（下一轮开头就能做，不需要再探路）：
`LaneKind::Auto` → 内核按 `base_url` 探测 → `set_select`，四处都必须**调用** `choose_auto_export`，
不许各自再判一遍"要不要换"；"上次选的地址"跟着**服务商**存（一个服务商一个出口，
这正是 A 期那条纪律的延伸）。

#### 第 98 轮：B 期第二步 —— **粘性状态存哪儿**（决定 + 实现 + 6 条测试）

用户那句"和上次选的一样就沿用"要求**先记住上次**。落点定在
`crates/ta-store/src/auto_choice.rs`（`SqliteAutoChoiceStore`）：

```rust
get(provider_id) -> Option<String>          // None = 从来没选过（不是"选了空"）
set(provider_id, address, now_ms)
clear(provider_id)                          // 服务商被删 / 用户要求重挑
```

三条设计决定（都写进了模块文档，免得后来者改错）：

* **按服务商存**，不是按线路 —— A 期已定死"一个服务商一个出口"，粘性跟着服务商是同一条纪律的延伸；
* **存地址（IP/主机名），不存节点名** —— 机场改名很常见，按名字比会把"同一个出口"误判成
  "换了节点"，于是每次订阅更新都换 IP（正是封号风险的来源）；
* **复用既有的 `store_setting` KV 表**（`key`/`value`/`updated_at`），**不新建表、不加迁移** ——
  这张表本来就是这个用途。键是 `auto_export:<provider_id>`。

**与 `sticky_store` 的区别**（刻意不合并）：那个是**请求级**粘性（请求键 → 通道，带 TTL 与命中数，
给路由器用），这里是**配置级**"上次选了谁"，没有 TTL、不参与路由决策 ——
混在一起会让"清粘性"这种操作顺手把自动出口也清了。

测试 6 条（`cargo test -p ta-store --lib auto_choice` 全绿、clippy 干净）：
从未选过 = `None`；写入读回；**按服务商隔离**（且 `p-a` 的前缀不会串到 `p-a2`）；
覆盖写；`clear` 只忘一家；库里被手改成空串也读成"从未选过"。

**B 期还剩**：`LaneKind::Auto`、按 `base_url` 探测、写 `set_select` ——
它们与 `choose_auto_export`、本存储三者之间的关系已经定死：**探测出延迟 → 读 `get()` 拿上次 →
`choose_auto_export` 裁决 → 写 `set_select` 并 `set()` 记住**。

#### 第 99 轮：一个改变 B 期做法的发现 —— **「自动」的种类早就有了，但它是 `url-test`**

本来打算新加 `LaneKind::Auto`。查完发现不用，也**不能**：

* `LaneKind::SubscriptionAuto` **已经存在**，界面上的名字是
  `network.laneKind.subscription_auto` = **「整份订阅择优」**；
* 但 `ta-kernel::config_gen` 把它生成成内核的 **`url-test` 组（`use: [provider]`）** ——
  `url-test` 会**按自己的 interval 周期性地重测重选**，而用户要的恰恰相反：
  > 自动选择的地址具有粘性 …… 唯二需要新选 ping 最低的情况就是用户手动点更新，和这条代理完全 ping 不通服务商

  也就是说：**今天的「整份订阅择优」会每隔一段时间自己换出口 IP**，正是用户要避免的封号风险。

所以 B 期的正确做法（不再新造种类）：

1. 「自动」= 一条 `subscription_auto` 线路，但 **`group_type = select`**
   （`GroupType::Select => "select"` 已有映射，不需要新的内核能力）；
2. 选谁由**我们**按 `choose_auto_export` 决定，再写内核的 `set_select`
   —— 内核不再自己重选，粘性才可能成立；
3. 上次选的地址存 `SqliteAutoChoiceStore`（第 98 轮已就位）。

这条发现同时解释了 D 期为什么不能只是"下拉里加一项"：加了种类还不够，
**必须同时把该组的 `group_type` 从 `url-test` 换成 `select` 并接管选择**，
否则「自动」在真机上依旧是"过一会儿自己换一个出口"。

**待做的三块**（下一轮）：`lane_auto_apply` 的探测（按服务商 `base_url` 测全部订阅节点）、
`set_select` 的写入、以及把「自动」暴露到服务商级下拉时的 `group_type` 处理。

#### 第 100 轮：把那条决定写进代码（`Lane::automatic` + 3 条测试）

上一轮只在文档里改了做法，这轮落成代码，免得后来者"顺手"又用回 `url-test`：

```rust
// crates/ta-core/src/domain/behavior.rs
pub fn automatic(id, name, subscription) -> Self {
    Self::subscription_auto(id, name, subscription).with_group_type(GroupType::Select)
}
```

**不是改老的 `subscription_auto`**：那个在界面上叫「整份订阅择优」，行为一字不动
（它没有显式组类型、继续跟随订阅建议）——「自动」是**新增的一个选择**，
不是把老语义悄悄改掉。这点专门写了反向测试钉住。

`crates/ta-core/tests/auto_lane_is_select.rs` 3 条测试（全绿）：

1. 种类仍是 `subscription_auto`（不新造种类），`subscription` 正确挂上；
2. `group_type == Some(Select)`；
3. **显式值压过订阅建议**：`effective_group_type(Some(UrlTest)) == Some(Select)`
   —— 否则机场在订阅里写 `url-test` 就把我们又推回"自己换出口"了；
4. 反向：老 `subscription_auto` 的 `group_type` 仍是 `None`、仍然跟随订阅建议。

**「自动」还差的**：探测（按服务商 `base_url` 测全部订阅节点拿到 `latency_ms`）与
写内核 `set_select`；以及 D 期把它放进服务商级下拉时**用 `Lane::automatic` 而不是
`Lane::subscription_auto` 建那条线路**（这是这一轮最重要的用途）。

#### 第 101 轮：探测能力**已经有了**；补上"节点 → 候选"的适配层

查清：这次要的探测不用新写 —— `ta-kernel` 的 `KernelController::delay_of(name, timeout_ms)`
就是它，`src-tauri/src/commands/net.rs` 里已经有一圈现成的回路
（`LaneTestResult { node, latency_ms, ok }`，含"测不到就 `None`"的处理）。

本轮补的是**适配层** `candidates_from_nodes`（`crates/ta-subscription/src/auto.rs`）：
订阅解析出的 `NodeMeta` + 内核测出的 `HashMap<节点名, Option<u32>>` → `Vec<AutoCandidate>`。

两条边界写进测试（`auto::` 10 条全绿）：

* `address` 取节点的 **`server`**（IP/主机名）—— 粘性按它比；`name` 只用于显示与
  `delay_of(name)`；
* **"表里没有这个节点"与"表里明确是 `None`"都是测不到，一律落成 `None`，绝不用 0 冒充**
  —— 0 会被当成"最快"，于是选中一条其实不通的出口（测试里专门放了一条 `Some(42)`、
  一条 `None`、一条"不在表里"，断言选出来的必然是 `Some(42)` 那条；表全空时结论是
  `AllUnreachable`，不选任何一条）。

**下一轮就能拼整条链**：`latest_outcome(订阅)` 拿节点 → 对每个节点 `delay_of(name, timeout)`
→ `candidates_from_nodes` → 读 `SqliteAutoChoiceStore::get(provider)` →
`choose_auto_export` → 写内核 `set_select` + `set(provider, address)`。

#### 第 102 轮：**编排函数写出来了**（`apply_auto_export`）

`src-tauri/src/commands/net.rs` 新增：

```rust
pub async fn apply_auto_export(
    state: &AppState,
    provider_id: &str,        // 粘性按服务商存
    lane_id: &LaneId,         // 那条「自动」线路（subscription_auto + select）
    subscription_id: &SubscriptionId,
    manual: bool,             // C 期：用户手动点更新 = true
) -> IpcResult<AutoApplyOutcome>
```

顺序与纪律（每一步都是前几轮定下来的）：

1. 节点只来自**已采纳的订阅版本**（`latest_outcome`，坏更新已被拦下、保留上一版）；
2. 对每个节点 `kernel.delay_of(name, 3000ms)` —— 失败 = 测不通 = `None`
   （**绝不用 0 冒充**，0 会被当成"最快"）；
3. `candidates_from_nodes` → 读上次的地址 → `choose_auto_export(..., manual)` 裁决；
4. **结论是"沿用/重挑"才写内核** `set_select(lane, node)` 并 `set(provider, address)`；
   结论是"全都不通 / 一个候选都没有"时**一个字节都不写**，理由如实返回
   （`all_unreachable` / `no_candidates`）；
5. 裁决给的是**地址**、内核要的是**节点名** → 在这里翻译一次；翻译不到就返回
   `address_gone`（"这条已经不在订阅里了"），**不编一个名字**。

内核没在跑时直接如实拒绝（"测不了延迟，不猜一个出来"），不降级成"随便选一条"。

`AutoApplyOutcome { node, address, why, probed }` **故意还不是 DTO**：先只在内部用；
D 期要把它接到界面时再加 specta 与 bindings（那时才知道界面到底要哪几个字段）。

**下一轮**：给它接上"服务商 → 自动线路 → 订阅"的解析，并挂到 C 期的第一个触发点
（订阅自动刷新成功后调用一次，`manual = false`）；D 期的手动更新按钮传 `manual = true`。

#### 第 103 轮：**C 期触发点①接通** —— 订阅更新后重算「自动」出口

两块：

* `auto_providers_on(doc, subscription_id)`：找出**用着「自动」出口、且挂在给定订阅上**的服务商。
  判据两条缺一不可 —— 服务商自己的 `lane` 指向的线路 `kind == subscription_auto`，
  **且**那条线路的 `subscription` 就是刚更新的这份（别人的订阅更新了不许动它）；
* `reapply_auto_after_refresh(state, subscription_id)`：逐家调用
  `apply_auto_export(..., manual = false)`（**粘性优先**：上次那条地址还在就沿用，
  订阅换了一批节点也一样），并**吞掉单家的错误**（一家算不出来不该让别家跟着不更新）。

挂点：第 10 轮那个自动刷新 tick 里，**每份订阅刷新成功之后**调用一次，
日志多一个数：

```
订阅自动刷新：到期 N 条，成功 M 条，失败 K 条，「自动」出口重算 A 家
```

（字段都用既有白名单里的：`provider_id` / `status` / `attempt`；重算结果那行在
`reapply_auto_after_refresh` 里逐家记，比一整行汇总更容易定位"是哪一家没成"。）

`cargo fmt / clippy -D warnings` 干净，`log_field_guard` 6 条通过，`build.ps1` 全量门禁一把过。

**C 期还剩**：触发点②（**用户手动点更新** → `manual = true`，D 期的手动更新按钮接上）
与触发点③（**这条代理完全 ping 不通服务商** → 已经在 `choose_auto_export` 的
`PreviousUnreachable` 分支里，随触发点①/②的每次重算自然生效）。

#### 第 104 轮：D 期的一个真问题 —— 「自动」是**哪一份订阅**内择优？

用户原话是"根据订阅地址，用所有代理 ping"。今天 `subscription_auto` 这条种类在配置层
**必须挂一份订阅**（`config_gen` 里走 `require_subscription`，缺了就是配置错误
`missing_subscription`），所以"自动"只能是**某一份订阅内**的择优，不是"所有订阅的所有节点"。

D 期的做法（据此定，不新造种类）：

* 有 **1 份**启用的订阅 → 下拉里就是「**自动**」（背后那条线路挂这份订阅）；
* 有 **多份** → 列成「**自动（<订阅名>）**」，一家一条 ——
  因为"用哪个机场的节点"是**用户必须能选**的事（不同机场出口在不同地区，延迟结论完全不同），
  替用户猜一个才是真错；
* 每条这样的线路都由 `Lane::automatic(...)` 建（`subscription_auto` + 显式 `select`），
  第 100 轮那条"显式值压过订阅建议的 `url-test`"就是为它准备的。

顺带确认了一件事：`config_gen` 里组类型是
`lane.group_type.unwrap_or(GroupType::UrlTest)` —— 也就是说**不显式写就是 `url-test`**
（会自己周期性换出口）。这正是第 100 轮那个构造函数存在的理由，不是多此一举。

**D 期要做**：① 用 `Lane::automatic` 建/复用「自动」线路（幂等，仿 `lane_ensure_direct`）；
② 服务商级下拉里列出它（多订阅时带订阅名）；③ 手动「更新」按钮传 `manual = true`
（C 期触发点②）；④ `interval_s` 可编辑（**已完成**，第 10 轮验过）。

#### 第 105 轮：认出「自动」那条线路（`ConfigDoc::auto_lane_on`）

幂等建线的第一步是**先找得到已存在的那条**，所以加了域层方法：

```rust
pub fn auto_lane_on(&self, subscription: &SubscriptionId) -> Option<&Lane>
```

判据**两条缺一不可**（这是重点）：种类是 `subscription_auto`，**且组类型是显式 `select`**。
只看种类会把老的「整份订阅择优」（`url-test`，会自己周期性换出口）当成「自动」——
那等于用户选了"粘"，拿到的却是"会自己跳"。

多条都够格时取**配置里靠前**的那条（确定性：同一份配置永远给同一个答案）。

测试（`crates/ta-core/tests/auto_lane_is_select.rs`，4 条全绿）覆盖：老的 `subscription_auto`
不算；显式 `select` 的那条才算；按订阅匹配不串台；多条时取靠前的。

#### 第 106 轮：建线**决策**（`ConfigDoc::plan_auto_lane`）

```rust
pub enum AutoLanePlan { Reuse(LaneId), Create { id: LaneId, name: String } }
```

纯值：**不动配置、不落库** —— 调用方照它去复用或新建。抽出来的理由是决策里全是边界，
而这些边界靠"跑一次真机"验不全。三条都写进测试（该文件现在 5 条全绿）：

1. 已经有一条够格的「自动」→ **复用**，绝不新建第二条（否则服务商名下会越用越多）；
2. 希望用的 id 被**别的**线路占了 → 往后让成 `-2`、`-3`…
   **绝不改别人的线路 id**：改 id 会让引用它的服务商指向不存在的线路，那是配置损坏；
3. 派生出来的那条**本身就是合格品**（`Lane::automatic` → 显式 `select`），
   所以第二次计划会走"复用" —— **幂等在这里闭环**。

**下一轮**：把 `plan_auto_lane` 接到落库（新增一个仿 `lane_ensure_direct` 的命令），
这会连带 bindings / mock / handwritten reference / 命令面守卫计数一起改 —— 是整块活，
一次做完再上真机。

#### 第 107 轮：**整机回归**（改了 6 轮后端之后第一次全量真机）

第 10 轮之后我连着改了订阅自动刷新、编排、触发点、域层方法，却一直没跑整机 —— 这轮补上：

* **28 档：PASS=24 / SKIP=1 / FAIL=3**，与第 10 轮完全一致（三档仍是既有的 harness 陈旧判据：
  `step2` 密钥 `secret_meta` 读取时机、`step10` 判据停在关系反转之前、`step21`
  的 `channels-delete`/`policies-delete` 行定位）；
  > 后记（2026-09-30，[docs/49]）：这三档后来都已处置 —— `step10` 改成验「成本中心新增定价」，
  > `step21` 里依赖 `/channels` 行定位的那几档**随页面删除而撤销**（单条线路删除改由 `step24`
  > 在服务商页覆盖，并如实记账"被引用→拒绝、批量删、通道组删**当前无界面入口**"）。
* **新链路在真机上按预期工作**：

```
10:32:42  INFO 订阅自动刷新：任务已在跑（本轮没有到期的） job="subscription_sync"
10:33:42  INFO 订阅自动刷新：到期 1 条，成功 1 条，失败 0 条，「自动」出口重算 0 家 job="subscription_sync"
```

`「自动」出口重算 0 家` 是**正确**结果，不是失败：此刻没有任何服务商的代理是「自动」
（入口还没做，D 期才建得出那条线路）。这一条顺带证明了计数器认的就是"真的有人在用"，
而不是"只要有订阅就算一家"。

（下一轮仍按上一轮定好的：把 `plan_auto_lane` 接到落库，整块做完再上真机。）

#### 第 108 轮：落库走**标记解析**（不新增命令）

上一轮计划新增 `lane_ensure_auto` 命令，但那会连带 bindings / mock / handwritten reference /
命令面守卫计数一起改（7 个文件）。改走**与已有 `direct:` 标记同一套约定**的路子，只动一个文件：

```
provider_upsert({ provider: { …, lane: "auto:<订阅 id>" } })
   → 后端解析标记：plan_auto_lane 决定"复用还是建"
   → 需要建就用 Lane::automatic 建出来（显式 select）并落库
   → 把**真实 LaneId** 写进 provider.lane
```

为什么不让前端自己先建线路再发 id：那会让前端成为"这条线路该长什么样
（种类 / 组类型 / id 撞了怎么办）"的**第二个真相来源** —— 那套判断只有一份，在
`ConfigDoc::plan_auto_lane` 里。

实现落在 `src-tauri/src/commands/catalog.rs`（`upsert_provider` 只有 8 行，正好在这儿接）：

* `AUTO_LANE_PREFIX = "auto:"`（与 `DIRECT_CHOICE_VALUE` 同一套约定）；
* `auto_lane_base_id(subscription)` → `lane-<订阅>-auto`（非法字符归一成 `-`）；
* `ensure_auto_lane(state, subscription)`：复用优先，必要时 `save_validated` 落库。

`cargo clippy -D warnings` 干净、`cargo test -p tokenagency --lib` **280 通过**、
`build.ps1` 全量门禁一把过。

**下一轮**：D 期的界面入口 —— 服务商级代理下拉里列出「自动」（多订阅时带订阅名），
选中时把 `auto:<订阅>` 发给 `provider_upsert`。

#### 第 109 轮：界面入口做了（服务商级代理下拉里的「自动」）

* `providerLaneChoices(lanes, inheritLabel, subscriptions, autoLabel)` 多了两个参数：
  启用中的订阅 → 每份一个选项，值 `auto:<订阅 id>`、**多份时带订阅名**
  （用哪个机场的节点直接决定出口在哪个地区，这是用户必须能选的）；
* 服务商详情页把 `subscription_list` 查好、**当 prop 传进视图**（`autoSubscriptions`），
  视图仍旧只吃 props；选中后 `provider_upsert` 收到标记、由后端解析成真实线路并写回 id；
* 文案：`providers.edit.laneAuto` = 自动 / Automatic。

**踩到并修掉的一个真问题**：我第一版是在视图组件里直接 `useCommandQuery`，测试**当场红**在
`No QueryClient set, use QueryClientProvider to set one` —— 本仓库的视图组件必须能在没有
QueryClient 的地方渲染（这是它能被单测的前提）。所以改成"页面查、prop 进"，
并把这条理由写进了 props 的文档注释里。

验证：`tsc` 0、`npm test` **29 文件 / 692 测试**全绿、`build.ps1` 全量门禁一把过。

**下一轮**：真机确认这条入口（下拉里出现「自动」；选中并保存后，库里真的多一条
`subscription_auto` + 显式 `select` 的线路，服务商的 `lane` 指向它），
以及 C 期触发点②（手动「更新」传 `manual = true`）。

#### 第 110 轮：真机确认**没通过** —— 下拉里没有「自动」（如实登记）

我给 `step25` 加了探针（展开 `providers-edit-lane`，读选项），真机结果是：

```
服务商级代理下拉选项=["未指定（继承默认线路，末端直连）","TA E2E 线路"]
```

**没有「自动」** → `step25` 从 PASS 变成 **FAIL**（`PASS=23 / FAIL=4`）。
我没有把探针撤掉来"恢复绿色"：**这条失败是真的**，探针留在那里正是为了让下一轮从它开始。

顺带查清两个操作层面的事实（以后每次都适用）：

* **debug 包里的前端是旧的**：Tauri 的 `generate_context!` 在**编译期**把 `dist/` 嵌进 exe，
  而 cargo 的指纹**不跟踪** `dist/` —— 所以只改前端后 `cargo build` 会"1 秒完成"却没换资源。
  要用 `scripts/build.ps1`（它会重建前端）产物，或先重建前端再编 exe。
  这次我用 release 包复验，结论一致（所以不是这个原因）；
* 探针读 MUI 下拉**必须先展开**（选项在 portal 里），这条已经修进 harness。

**下一轮从这里开始**（按可能性排序排查，别再重复这轮的弯路）：

1. `subscription_list` 在这页拿到的到底是什么（视图包裹？空数组？命令报错被吞？）——
   在页面上把 `autoSubscriptions` 的长度打出来看一眼最快；
2. `autoSubscriptions` 这个 prop 是否真的传到了 `ProviderDetailBody`（传了但没进 useMemo？）；
3. 订阅在 scratch 里的 `enabled` 是否为 `false`（我的过滤器会把关掉的订阅排除掉）。

## 1. 用户原话（第 88 轮，一字不改）

> ok，现在我们新增加一个除了"直连"的另一个网络选项，名叫"自动"
>
> 做一些修改，现在把线路的自定义网络代理功能删掉，也就是说只允许选择服务商的代理，因为我想了一下，
> 一个服务商下的所有模型最终都是走一个URL，URL整不同的代理，反而会封号
>
> 设定网络订阅自动更新时间，选了"自动"的线路的服务商，根据订阅地址，用所有代理ping，选择一个ping最低的，
> 作为"自动"选择的地址
>
> 自动选择的地址具有粘性，加入更新订阅之后如果存在一个代理的IP(地址)和上次选的一样，就选上次选的，
> 唯二需要新选ping最低的情况就是用户手动点更新，和这条代理完全ping不通服务商

## 2. 结论（这几条决定了实现形状）

### 2.1 代理**只属于服务商**，线路一律继承

理由（用户给的）：一个服务商下的所有模型最终打到**同一个 URL**；同一个 URL 用不同出口 IP，
在上游看来是"同一账号在多地并发"，**会封号**。

所以：

* 线路（`channel`）**不再有**自己的 `lane`。线路级的代理下拉、每行"网络代理"列、
  "给某条线路选订阅节点"这些入口**全部删掉**；
* 线路的出口 = **它所属服务商**的代理（五级继承链 `channel → credential → provider → net.default_lane`
  里，`channel` 与 `credential` 两级对用户**不可见**了）；
* 服务商级的代理是**唯一**的代理入口。

> 注意与 [docs/28 §1] 的关系：那条"代理是**线路**的属性"的判断**被本轮推翻**。
> 28 号文档不删（历史与理由都在），以本文为准；`Model.default_lane` 那条早已删掉，
> 现在 `ChannelConfig.lane` 也不再由界面写入。

### 2.2 「自动」不是一条"线路"，而是一条**策略**

用户要的行为是"用所有代理 ping，选最低的那个"，**并且带粘性**：

* 订阅更新后，**如果上次选的节点（按 IP/地址）还在** → 继续用它；
* 只有两种情况重新挑：**① 用户手动点更新**、**② 这个节点完全 ping 不通服务商**。

这**不能**直接用内核的 `url-test` 组：`url-test` 会按 `interval` 周期性重测重选，
那正好是要避免的（出口 IP 频繁变化 = 用户说的封号风险）。
所以「自动」= **由 TokenAgency 管理的 `select` 选择**：

* 内核配置里，自动线路是一个包含订阅内全部节点的 `select` 组；
* **当前选谁由本应用决定并写入**（沿用已有的 `lane_set_node` 那条路：`kernel.set_select`）；
* 选择算法（本应用侧）：
  1. 有上次选择、且该节点仍在（按 **IP/地址** 比，不是按名字 —— 机场会改名）→ 保留；
  2. 否则（首次 / 手动更新 / 上次那条 ping 不通服务商）→ 对所有节点用**这家服务商的 base_url**
     测延迟，取最低的那条；全都不通 → 如实报"没有可用节点"，**不编一个**。

### 2.3 「订阅自动更新时间」用**已有的** `interval_s`

配置里已经有 `interval_s`（默认 6 小时）与 `subscription_sync` 的下载/校验/版本链路，
界面上也**已经能改**（`/network` 订阅表单的「更新周期（秒）」，列表里显示成"6h"这种）。

**但查清了一个事实（第 89 轮）**：全仓库**没有任何地方按 `interval_s` 自动刷新** ——
只有手动点「刷新」（`force: true`）这一条路。所以"自动更新时间"此前只是个**存下来的数字**。

这一轮把这个判断做出来了（`crates/ta-subscription/src/schedule.rs`）：

```rust
pub fn due_subscriptions(
    schedules: &[SubscriptionSchedule<'_>],
    last_fetch_ms: &HashMap<String, u64>,
    now_ms: u64,
) -> Vec<String>
```

边界（每条都有测试）：

* 关掉的订阅（`enabled = false`）**永不**自动刷新 —— 用户关掉它就是让我们别碰；
* `interval_s == 0` 视为**不自动刷新**（配置校验本来就把 0 判非法），
  不 panic、也**不**解释成"每秒都刷"（那会把机场打成 DDoS）；
* **从没成功拉过 → 到期**（否则新加的订阅要等到手动刷新才动）；
* **时钟回拨（`last > now`）→ 到期**：宁可多拉一次，也不因为时钟问题永远不更新；
* 到点是 `>=` 的边界（差 1 毫秒不算到，正好到点算）；
* `u32::MAX` 秒的周期不溢出、不 panic。

**还差一步（下一轮）**：把它接到运行时 —— 周期性 tick → 读订阅表 + 上次拉取时间 →
`due_subscriptions` → 对到期的那些走**和手动刷新同一条**刷新链路。上次拉取时间的现成证据
已经找到：`ta-store` 的 `subscription_usage.traffic_at_ms`（每次成功刷新都会写），
**不需要新建表**。接线的同时就是 C 期"订阅更新后保留优先"的挂点。

#### 第 93 轮：**接线第一步（先算、后刷）已落地**

按"算得对"与"刷得对"分开做的原则，第一步只做**可观察**，不做任何网络动作：

* `src-tauri/src/commands/net.rs` 新增 `due_subscription_ids(state)`：
  读 `state.doc()` 的订阅 + `subscription_traffic` 的 `traffic.fetched_at_ms`（`traffic == None`
  = 从未成功刷新过 → 到期；负数夹到 0）→ 交给 `ta_subscription::due_subscriptions`；
* `src-tauri/src/lib.rs` 的 `setup` 里起一个后台任务：**先算再睡**（首轮立即执行，
  新加/刚改过周期的订阅不用等满一个轮询粒度），轮询粒度 60 秒（周期仍由每份订阅自己的
  `interval_s` 决定，默认 6 小时）；
* **首轮无论有没有到期的都写一行**（`subscription_sync = 0` / `= N`）：
  否则"日志里没有这行"既可能是"没有到期的"、也可能是"任务根本没起来"，真机上分不清
  ——这是写这行日志的原因，不是为了好看。

**真机证据**（用带着真实订阅的 scratch 数据启动，40 秒后读应用日志）：

```
2026-09-25T07:56:33.555996Z  INFO tokenagency_lib: 订阅自动刷新：任务已在跑（本轮没有到期的） subscription_sync=0
2026-09-25T08:10:08.050092Z  INFO tokenagency_lib: 订阅自动刷新：本轮到期 1 条 job="subscription_sync"
```

两行分别是**两个分支**的真机证据（第一行的 `subscription_sync=0` 是修正前的写法，见下）：

* 第二行是**非空分支**：用 `scripts/probe-due-subscriptions.py` 把 scratch 库里
  `subscription`（运行期观测表）**清空**，让每条订阅都变成"从未成功刷新过"→ 到期，
  于是真机上出现"本轮到期 1 条"；验完用同一脚本 `--restore` 还原
  （**还原必须连 `-wal` 一起还原**：这个库的数据几乎都在 WAL 里，只拷主文件会退回一个
  19 张表里少一半的旧状态 —— 我第一次就是这么错的，补上 WAL 才复原）。

（第一行最初写成 `subscription_sync = 0`，`log_field_guard` 当场红了：`subscription_sync` 是
[14 §5.3] 的 **job 取值**，不是字段名；后台任务只能用 `job` 字段自报家门，条数写进消息。
守卫抓出的正是"看着像对、其实破坏了日志契约"的那类错。）

`cargo check / fmt / clippy -D warnings` 全绿，`cargo test -p tokenagency --lib` 280 通过。

**下一步（第二步）**：把 `due_subscription_ids` 返回的 id 交给**和手动刷新同一条**链路
（`SubscriptionSync` 下载 → 校验 → 采纳 → 版本），并在这里挂上 C 期的
"订阅更新后按 IP 保留上次选择"。注意：**到期 ≠ 一定刷新成功**（坏更新会被拦下、保留上一版），
这条已有的语义不要在新链路里重写。

#### 第 94 轮：第二步**接上又撤回** —— 真机暴露"两套钟"

接了第二步（`auto_refresh_subscription(state, id)` → `refresh_subscriptions(..., force = false)`），
真机上确实出现：

```
2026-09-25T08:19:06.839133Z  INFO tokenagency_lib: 订阅自动刷新：到期 1 条，成功 1 条，失败 0 条 job="subscription_sync"
```

**但"成功 1 条"是假的**：同一时刻查库，`subscription`（流量）表**一行都没多**。
根因是我把两套钟当成了一套：

| 谁 | 判到点用的时间证据 |
| --- | --- |
| 我的 `due_subscription_ids` | `subscription` 表的 `traffic_at_ms`（每次成功刷新都写） |
| 既有的 `refresh_one`（`force = false` 时） | **版本元数据**的 `fetched_at_ms` |

内层还自己按 `interval_s` 判断一次，不满足就**直接跳过并返回 `ok: true`** ——
于是"我判到期 → 它判没到 → 跳过却报成功"，日志就在撒谎（这正是本仓库最不能接受的那类错）。

**所以第二步已撤回**，日志改成如实说明：

```
订阅自动刷新：本轮到期 N 条（第二步未接：两套钟未统一）
```

第二步的**前置条件**因此写死：先把到点判断统一到**版本元数据那套时间**（它才是"上次真正
采纳过一版"的时间），再调刷新；届时 `ok: true` 才真的意味着"拉到了并采纳了"。
在此之前，宁可只算不刷 —— **一个会说谎的"成功"比没做更糟**。

#### 第 95 轮：统一到一套钟，第二步**重新接上**（这次日志与库一致）

* `due_subscription_ids` 的时间证据改成 **`VersionKeeper::history()` 里最新那条 `accepted` 的
  `fetched_at_ms`** —— 与内层 `refresh_one`（`force = false`）**同一套钟**；
  流量表那条读法删掉（它正是"两套钟"的来源）；
* 第二步重新接上：到期 → `auto_refresh_subscription`（与手动刷新同链路、`force = false`）→
  逐条记成败 → 一颗日志行说清 `到期/成功/失败` 三个数。

**真机验证**（删掉 scratch 的版本目录与流量行 → 造出"从未采纳过任何版本" → 启动 50 秒）：

```
2026-09-25T08:32:00.877706Z  INFO tokenagency_lib: 订阅自动刷新：到期 1 条，成功 0 条，失败 1 条 job="subscription_sync"
```

关键不是"失败了"，而是**日志与库一致**：同一时刻查库 `subscription` 0 行、版本目录 0 个文件
—— 失败就是失败（那个 scratch 订阅指向的是本机 mock，此刻没在跑）。
对照上一轮那版"成功 1 条但库里一行没多"，这一版才是能信的。

**仍然没有的真机证据**：一次**成功**的自动刷新（`成功 1 条` + 版本目录里多一版）。
需要 mock 订阅服务在跑（E2E 跑起来时它在），这一步留到下一轮用真机 E2E 补。

#### 第 96 轮：**成功路径**的真机证据（三个信号互相印证）

跑真机 E2E（mock 订阅服务在跑），并把 scratch 的版本目录/流量行先清空造出"该拉"的状态。
日志与库**三处对齐**：

```
2026-09-25T08:40:37  INFO tokenagency_lib: 订阅自动刷新：任务已在跑（本轮没有到期的） job="subscription_sync"
2026-09-25T08:41:37  INFO tokenagency_lib: 订阅自动刷新：到期 1 条，成功 1 条，失败 0 条 job="subscription_sync"
```

| 信号 | 之前 | 之后 |
| --- | --- | --- |
| 日志 | 到期 1 条，成功 0 条，失败 1 条（mock 没跑） | **到期 1 条，成功 1 条，失败 0 条** |
| `subscription-versions/` 文件数 | 0 | **6**（真的下载并落了版本） |
| `subscription` 流量行 | 0 | **1**（真的写了流量） |

**顺带验到的行为**：两行日志相差正好 60 秒（轮询粒度），第一轮"没有到期的"、第二轮到期 ——
这正是期望的语义：**新加的订阅不用等到下一个 `interval_s` 周期才被拉**，
最多等一个轮询粒度（60 秒）。这条路至此闭环：A 期（代理归属）+ 自动更新的"算"与"刷"
都有了真机证据。

**下一步**：进 **B 期** —— `LaneKind::Auto` + `lane_auto_apply`（按服务商 `base_url` 对订阅内
全部节点测延迟取最低 → 写内核 `set_select`；全不通如实报错、不编），以及**按 IP 的粘性**；
C 期的"订阅更新后保留优先"就挂在这一轮刚通的自动刷新链路上。

#### 第 97 轮：B 期第一步 —— **选择规则**本身（纯函数 + 9 条测试）

B 期的裁决点只有一处值得先做：**"到底选谁"**。它是"出口 IP 会不会无故变化"的唯一关口，
所以做成纯函数、穷举测（`crates/ta-subscription/src/auto.rs`）：

```rust
pub fn choose_auto_export(
    candidates: &[AutoCandidate],      // name / address / latency_ms(Option)
    previous_address: Option<&str>,
    manual: bool,
) -> AutoDecision                      // Keep{why} | Pick{why} | None{reason}
```

逐字实现用户第 88 轮那句话：

| 情况 | 结论 | 理由枚举 |
| --- | --- | --- |
| 用户**手动**点更新 | 无条件重挑最低延迟 | `ManualRefresh` |
| 上次那条**还在且仍然通** | **沿用**（哪怕有更快的） | `SameAddressStillThere` |
| 上次那条**测不通**服务商 | 重挑最低可用的 | `PreviousUnreachable` |
| 上次那条在订阅里**不见了** | 重挑 | `PreviousGone` |
| 从没选过 | 挑最低 | `FirstTime` |
| 有候选但**全都不通** | `None` + `AllUnreachable`（**不编节点**） | — |
| 一个候选都没有 | `None` + `NoCandidates`（与"全不通"是**两件事**） | — |

两条最容易写错的，都单独立了测试：

* **粘性按 IP/地址比，不按名字**：机场把 `东京-01` 改名成 `东京-改了个名`、地址没变时，
  仍然必须沿用（否则每次订阅更新都换出口 IP —— 正是封号风险）；
* **更快的出现也不换**：`old(200ms)` 与 `new(10ms)` 同时存在且上次是 `old` → **仍然是 `old`**。

`cargo test -p ta-subscription --lib` **114 通过**（本模块 9 条），clippy `-D warnings` 干净。

**还没接的**（B 期剩下的）：`LaneKind::Auto` 这个配置种类、按 `base_url` 真的测延迟
（内核探测）、把结论写进内核 `set_select`、以及粘性状态（"上次选的地址"）的**存放位置**。
本轮的纯函数就是这四件事共同的裁决点 —— 它们接上时都不许再自己判一遍"要不要换"。


## 3. 分期（每期独立可验）

| 期 | 内容 | 状态 |
| --- | --- | --- |
| A | **删掉线路级代理**：UI 不再有线路级代理入口；线路继承服务商；向导第 1 步那个"新建线路默认代理"随之下线（它只服务线路级 lane） | **已完成** |
| B | 后端：`LaneKind::Auto` + `lane_auto_apply`（挑最低延迟、写 `set_select`）+ 粘性存储（上次选择的节点按 **IP** 比对） | 待做 |
| C | 三个触发点：订阅更新（保留优先）、**用户手动更新**（重挑）、**ping 不通服务商**（重挑） | 待做 |
| D | 前端：服务商级代理下拉新增「自动」（与「直连」并列）；订阅页把 `interval_s` 变成可编辑 | **一半**：`interval_s` 早就能改（§2.3）；「自动」选项待 B/C 完成后再上 |
| E | 真机 E2E：自动线路真的按最低延迟选中、且在订阅更新后**保持不变** | 待做 |

### A 期完成清单（第 88 轮）

**界面上再也找不到"给某条线路单独设代理"的路**（逐个真机验过）：

| 位置 | 处理 |
| --- | --- |
| 服务商详情页 · 线路列表 | 行里删掉 `providers-line-lane` 下拉 —— 只剩 `名字 · 滑块 · ⓘ` |
| 添加服务商向导 · 第 1 步 | 删掉"新建线路默认代理"（`pw-default-lane`）与它的问号 |
| 添加服务商向导 · 第 2 步 | 建线路不再写 `lane`（`provider_lines_create_all` 收到 `lane: null`） |
| **套餐与线路页**（`/channels`） | 删掉每行的 `channels-lane-select` + `laneNotice` / `onAssignLane` / `laneChoices` 整套；**只保留只读的"当前生效线路"**（它解释这条线路实际从哪儿出去，不是编辑入口） |

测试与真机同步改成"反向钉住"：

* `channels.test.tsx` 的 ⑬ 组（原来 4 条"每行一个下拉"的用例）换成
  "一个 `channels-lane-select` 都没有 + 那一列里没有任何可写控件"；
* 服务商页/向导那一整套线路级代理用例删除，换成"行里没有代理控件"；
* 真机 `step25` 重写为反面验证（`线路级代理控件=0`、行结构 = 名字 + 滑块、
  服务商级入口仍在、向导无代理入口）→ **PASS**；
* 真机 `step21` 的 A 档改成"通道页线路级代理下拉已删 + 只读行仍在"。

> **[docs/49] 后记（2026-09-30）**：上表最后一行与最后一条提到的**「套餐与线路」页（`/channels`）
> 已按用户裁决整页删除**，所以：
> * `channels-lane-select` / 那条"当前生效线路"只读行**都不存在了**（连同 `channels.test.tsx` 的 ⑬ 组）；
> * 真机 `step21` 的 A 档已**搬到服务商详情页**验同一件事（行内不得有代理写入控件、
>   逐行有启停、服务商级 `providers-edit-lane` 入口仍在）—— 判据的**内容**没变，只是"在哪一页看"跟着页面走了；
> * 本档的核心结论（**代理只属于服务商**）**仍然成立**，且仍由 `step25` 正面钉住（PASS）。
> 详见 [docs/49](49-channels-page-removed.md) 与 [docs/49 §7.4]。

**A 期真机结论（第 88 轮末）**：`step25` PASS、`step24` PASS，
`step21` 里"通道页线路级代理下拉已删=true、只读行仍在=true"；总账 `PASS=24 / SKIP=1 / FAIL=3`
（剩下三档 step2 / step10 / step21 都是**本目标之外**的既有 harness 陈旧判据）。

## 5. 下一轮就从 B 开始

B 期要动的是后端（`ta-core` / `ta-kernel`）：

1. `LaneKind::Auto`（一条"策略线路"，成员 = 订阅内全部节点；内核里生成 `select` 组）；
2. `lane_auto_apply`：按 **服务商 base_url** 对全部节点测延迟 → 取最低 → 写 `set_select`；
   全不通 → 如实报"没有可用节点"（**不编**）；
3. 粘性：记住"这家服务商上次选的节点 IP"，订阅更新后**按 IP** 比对，仍在就沿用；
4. 触发点：订阅更新（保留优先）/ 用户手动更新（重挑）/ 该节点 ping 不通服务商（重挑）。




## 4. 边界（写死，免得后面走样）

* **不编延迟**：测不到就是测不到（`null`），不写 `0`、不写一个"看起来合理"的数。
* **不编节点**：节点只来自**已采纳的订阅版本**（`latest_outcome`）。
* **粘性比较用 IP/地址**：机场换名字很常见，按名字比会把"同一个出口"误判成"换了节点"，
  那就等于每次订阅更新都换 IP —— 正是要避免的。
* **出口只有服务商一个**：任何"给单条线路/单个模型单独配代理"的入口都不再出现
  （包括模型页：那里只选**线路**，不选代理）。

---

## 297 · **更正**：探测不是 `delay_of(name)`，而是 provider 命名空间（E 期收口）

[第 101 轮](#第-101-轮探测能力已经有了补上节点--候选的适配层) 写过一句

> 探测能力**已经有了** —— `KernelController::delay_of(name, timeout_ms)` 就是它

**这句话是错的，它就是「自动」出口一直 `all_unreachable` 的全部原因。**
`delay_of` 打的是 `GET /proxies/<名字>/delay`，而 `proxy-providers` 的节点**不在 `/proxies` 顶层**：

```
① /proxies 顶层键：COMPATIBLE | DIRECT | GLOBAL | LANE-… | PASS | PASS-RULE | REJECT | REJECT-DROP
   含订阅节点的键：**0**                        ← 这是 mihomo 的正常行为，不是配置错
③ /providers/proxies/<key> → 每个节点带 alive + history[].delay   ← ★ 延迟**只在这里**
④ /providers/proxies/<key>/healthcheck?url=…                    → HTTP 204（整份订阅体检）
⑤ /proxies/<name>/delay、/proxies/<key>/<name>/delay、
   /proxies/<key>|<name>/delay、/providers/proxies/<key>/<name>/delay
   → **四种命名形式全 404**
⑦ PUT /proxies/<url-test 组> → HTTP 204 但 now **不变**（被忽略）
⑧ PUT /proxies/<select 组>  → HTTP 204 且 now **真的变了**     ← ★ 所以「自动」必须是 select
```

（原始输出：`.ta-tmp/expC-out.txt`；完整实验与修复见 **[docs/46 §297]**。）

对本文的影响，逐条更正：

1. `lane_auto_apply` 的探测改为 `KernelController::provider_delays(key, 服务商 base_url, wait_ms)`：
   **先体检整份订阅，再读 `history[].delay`**；`alive=false` / 没测过 / `delay==0` 一律 `None`；
2. 探测目标**就是这家服务商的 `base_url`**（与 [docs/29 §2.2] 的语义一致），
   空才退到订阅 `health_check.url` → 内核默认端点；
3. 第 100 轮那条设计（「自动」= `subscription_auto` + **显式 `select`**）**是对的**，
   而且现在有了硬证据：`url-test` 组根本不接受 `set_select`（⑦）。`auto_lane_on` 只认显式 `select` 也因此更站得住；
4. 应用日志行改成
   `「自动」出口已重算 provider_id=… status=… node=… probed=… measured=… probe=…`
   —— `measured`（内核真的量到几条）与 `probe`（按谁测的）是"探测没跑起来"与"选错了"的**分界线**。
---

## 202 · 用户第 202 轮的两条定调（**「自动」按服务商** + **节点级开关**）

### 一、「自动」是**每家服务商自己的**出口（选哪条节点按服务商记）

用户原话：

> 自动是对于服务商而言的，每一个服务商都有自己的「自动」节点，相当于这个选哪个节点是记录在每一个服务商自身的

改动（都在既有结构里，没有新概念）：

| 位置 | 之前 | 现在 |
| --- | --- | --- |
| 「自动」线路的 id | `lane-<订阅>-auto` | **`lane-<服务商>-auto`**（`catalog::auto_lane_base_id`） |
| 线路名 | `自动` | `自动（<服务商名>）`（界面上两条「自动」不会看起来一模一样） |
| `plan_auto_lane` 的复用 | **按订阅找第一条**够格的 → 两家服务商共用一条 | **按 `base_id` 复用**（传进来的 id 就是这家服务商的） |
| 幂等 | 靠"按订阅找" | 靠**服务商当前挂的那条**够格就复用它（`ensure_auto_lane` 第 ① 步）—— 这一步同时是**老配置的迁移路径**：早先建的 `lane-<订阅>-auto` 会被继续用下去，不因 id 规则改了而多出一条、丢掉已选好的出口 |
| 选择记忆 | `auto_choice` 按 `provider_id` 存（本来就是） | 不变 ✓ —— 这正是"记录在每一个服务商自身" |

**为什么必须改**：id 里带订阅时，两家服务商挂同一份订阅就会**共用同一条 lane** ⇒ 各自按自己存的地址
重算、写**同一个组** ⇒ 组上的 `now` 变成"最后写的那家"的选择，日志里还会互相打脸
（真机实测：一家写"沿用 日本-优化2"、另一家写"剩余流量"，[docs/46 §301]）。

断言在哪：`crates/ta-core/tests/auto_lane_is_select.rs`（6 条）+
`src-tauri/src/commands/catalog.rs` 的 `auto_lane_is_per_provider_and_reuses_the_existing_one`
（① 按服务商派生 id ② 重存幂等 ③ 两家各一条不同的 lane ④ 老配置沿用不多建）。

### 二、订阅里的节点：**像服务商的「模型」一样，可以单独启用/关闭**（全选、默认全选）

用户原话：

> 要做语义选择很难，相对应的，订阅里面的节点，也将服务商的"模型"一样设置一个启用或关闭的功能就合理了，
> 对了，这个也要那种有全选功能，默认全部选择

* 存储：`Subscription.disabled_nodes: Vec<String>`（**存"关掉的"**）—— 与 `enabled`（省略 = 启用）
  同一套哲学：机场新增的节点默认就是开着的，用户不需要回来补勾；
* 契约：`subscription_nodes` 返回的每条 `NodeView` 带 `enabled: boolean`；
  新命令 `subscription_nodes_enabled({ id, nodes: string[], enabled: boolean }) → NodeView[]`
  —— **批量**（全选一次改几十条，一条一次 IPC 既慢又有中间态），空清单**明确报错**（空清单不是"全部"），
  返回**改完之后**的清单（界面直接用它回填）；
* 语义影响：**关掉的节点不进「自动」的候选**（`apply_auto_export` 先按 `disabled_nodes` 过滤）。

于是 [docs/46 §301] 里那个"要不要排除信息节点"的问题**不再是我们的语义选择**：订阅里那几条
（`剩余流量：…` / `套餐到期…` / `过滤掉 N 条线路`）用户自己勾一下即可；默认全开时行为与今天一致。

---

## 321 · 第 321 轮的收口：这套语义**一个字都没改**，只是真的接上了

用户第 321 轮把「自动选择」的语义**重新描述了一遍**（[docs/54](54-nodes-not-groups.md) §1 有逐字引用）：
"自己用所有节点 ping 这个订阅商的 url，那个延迟低就用哪个，有粘性，当这个节点报了连接性错误就再 ping 一次再选"。

**逐条比对后：与 §0–§202 这一路实现出来的完全一致，没有新算法。** 本轮只做接线，外加修掉两个
让这套语义**实际上没生效**的缺陷：

1. **`disabled_nodes` 此前从未进入内核配置生成**（`ta-kernel/src/config_gen.rs`）。
   §202 的"关掉的节点不进「自动」的候选"只在**应用侧**（`apply_auto_export`）成立；
   组类型是 `url-test` / `fallback` / `load-balance` 的线路由**内核自己**在组内择优，
   它看不到 `disabled_nodes` —— 用户关掉的节点照样会被选中。**那个开关一直是假的。**
   现在把已关闭节点合并进组的 `exclude-filter`（逐个转义 + `^(?:…)$` 全字匹配）。

2. **`subscription_nodes_enabled` 只写库、不碰内核**（原来走裸 `save_validated`）。
   改成 `commit_config` + `maybe_apply_kernel`，与 `subscription_upsert` / `lane_upsert` 同一条路径。

另外，界面上那条「组策略」下拉随"放弃订阅里分组"一起删了 —— 但 `select` 这个**语义**不能跟着删，
所以改由 `lanePayload()` 按 `kind` 自动补（[docs/54](54-nodes-not-groups.md) §1 末）。

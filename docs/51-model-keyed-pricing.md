# docs/51：按**对外模型**定价；成本中心只做显示

用户第 318 轮的两句话，逐字：

> 成本中心 这个板块逻辑有误 / 我是希望用户为每一个对外模型定价，来算自己大概花了多少，
> 而不是对服务商得线路计价
>
> 还有一件事 成本中心页面只做显示，价格配置到模型页面去

这一轮就是照这两句话改的。**这一页是定价的现行口径**；[docs/49 §2](49-channels-page-removed.md)
里"把「新增定价」搬进成本中心"的那一段因此过期（那一段如实记录了它写的时候的样子，不删）。

`ask_user_question` 的两个选择（用户已答）：

| 问题 | 用户选择 |
|---|---|
| 定价的键按什么 | **只按对外模型定价** |
| 客户端发的是别名时怎么办 | **归一到规范模型 id** |

---

## 1. 错在哪：旧形状把"线路"当成了定价对象

改之前，定价对话框让用户填两样东西 —— **服务商** 与 **凭据** —— 拼出一个
`PriceMatch { provider, credential }`。于是：

* 用户想表达的是"**这个模型**大概花我多少钱"，界面问的却是"**哪条线路**多少钱"；
* 同一个模型换了服务商（或只是多接了一条线路）就得**再定一次价**，
  而用户并不知道自己漏了哪一条 —— 漏掉的那条在成本中心表现为"未定价"，
  可用户明明定过价；
* 反过来，用户想"给这个模型一个统一价"，不填服务商就**做不到**：
  空着两个下拉什么都没写（三项全空会被本地拦住）。

而运行时**本来就是按模型查价的**（`crates/ta-gateway/src/meter.rs`）：

```rust
let model = ctx.model.clone().unwrap_or_else(|| ctx.upstream_model.clone());
let price = self.pricing.resolve(&model, &ctx.provider_id, &ctx.credential_id);
```

`PriceMatch` 的 `provider` / `credential` 是**可选的附加过滤条件**，不是必填。
所以旧界面的"两个必填下拉"与引擎的真实语义是**不一致的** ——
它把一个可选的收窄条件做成了必经之路。

---

## 2. 改成了什么

### 2.1 入口搬到模型页的**每一行**

`/models` 每一行新增一个按钮，testid 是 **`models-price-open`**：

```
models-price-open      ← 定价 / 改价（同一个入口，两种文案）
```

文案按"这个模型有没有价"分两种（`models.price.open` = 「定价」/ `models.price.openEdit` = 「改价」），
但**testid 只有一个**。这是刻意的：用户不关心"新建 vs 修改"的区别，
他只关心"去哪儿改这个模型的价"。一个入口、两种文案，比两个入口更贴近他要做的事。

对话框在 `src/pages/models/PricingDialog.tsx`（数据层），由 `ModelsPage` 挂载。
**不是**挂在 `ModelsBody` 上：`ModelsBody` 是纯展示组件（写操作与 QueryClient 都在页面层），
挂在那里会直接报 `No QueryClient set, use QueryClientProvider to set one`。

### 2.2 模型字段是**只读展示**，不是输入框

| testid | 形状 |
|---|---|
| `models-price-model` | 只读展示那一行的模型 id（**下面没有输入框**） |
| `models-price-input` / `-output` / `-cached` | 三项费率 |
| `models-price-submit` / `-cancel` | 保存 / 取消 |
| `models-price-issue-<input\|output\|cached>` | 逐字段本地点名 |
| `models-price-issue-none` | 三项全空（点的是"整条"，不是某个字段） |
| `models-price-error` | 后端错误原文 |
| `models-price-clear` | 清除定价（在**已有价**时才出现） |
| `models-price-clear-confirm` → `-execute` / `-cancel` | 先确认再删 |
| `models-price-clear-error` | 清除失败的原因（下标错位 / 后端原文） |

模型来自**被点的那一行**，用户改不了它。理由是两种错都能从根上消掉：

* **拼错** —— 价格挂到一个不存在的模型上，永远不会被命中，界面上还显示"已定价"；
* **写别名** —— 网关把客户端发的别名归一到规范 id 之后（见 §2.4），
  价格挂在别名上就再也匹配不到，表现为"我定过价怎么还算不出来"。

### 2.3 发出去的 `match` **只有 `model`**

`src/shared/pricing.ts` 的 `priceEntryFor()` 现在只产出：

```ts
{ match: { model: draft.model.trim() }, rates, … }
```

`provider` / `credential` 这两个键**根本不出现**（不是留空、不是 `null`）。
这一条有两颗牙：`src/app/models.test.tsx` 断言 `entry.match` **深等于** `{ model: ID }`
且那两个键 `toBeUndefined()`；真机 e2e 的 step10 直接解析落库的 `pricing.entries[]`，
要求 `provider === undefined && credential === undefined`。

> 附加过滤条件**没有从引擎里删掉**。`PriceMatch` 的 `provider` / `credential`
> 仍然可选、仍然生效 —— 手工编辑配置、导入的表、历史条目都还能用。
> 删掉的只是**界面把可选条件做成必填**这件事。

### 2.4 别名归一到规范模型 id

`src-tauri` 之外的网关入口 `crates/ta-gateway/src/ingress.rs` 现在把
`PreparedRequest.model` 归一：

```rust
model: hints.model.as_deref()
    .and_then(|name| input.cfg.find_model(name))   // id 与任一 alias 都认
    .map(|m| m.id.clone())
    .or(hints.model),                              // 查不到就照原样保留
```

**为什么必须归一、且必须只在这一处做**：下游有三处要用**同一个**名字 ——

1. 定价（`PriceMatch.model` 匹配"对外模型名"）；
2. 用量表的 `model` 列；
3. 成本中心按模型的分组。

不归一的话，同一个模型的请求会按客户端发的字面名分别落库：**算不出价**（显示"未定价"），
并且在成本中心被**拆成两行**、各算一半钱 —— 两处都不会报错，只是数字不对。

查不到就**照原样保留**客户端发的名字：那属于"请求了一个没配置的模型"，
此时编一个规范 id 出来才是错的（网关要如实记下它收到了什么）。

### 2.5 成本中心：只读，且**明说去哪儿改**

`src/pages/cost.tsx` 的 `PricingSection` 现在**没有任何写入口**：

* 没有 `pricing-add-*`（整个添加对话框已删）；
* 没有 `pricing-delete` / 行内复选框 / 批量删除；
* 不再需要 `provider_list` / `credential_list`（旧对话框用它填那两个下拉）。

保留的是一个**只读表格**（`pricing-table` / `pricing-row`）与一句常驻说明：

| testid | 用途 |
|---|---|
| `cost-pricing-readonly` | 「改价在模型页」那句话，**常驻版面** |
| `cost-pricing-help` | 「匹配是怎么算的」，收进问号 |
| `cost-pricing-error` / `-loading` / `-empty` | 取不到 / 加载中 / 空清单（各自说各自的话） |
| `pricing-table` / `pricing-row` / `pricing-match-ellipsis-N` | 只读展示 |

**「去哪儿改」这句话不收进问号圆圈**，是刻意的：原来"添加定价"的按钮就在这里，
用户会回来这里找它。按 [docs/22 §2](22-help-tips-for-long-copy.md) 的道理，
**恢复路径不能藏**。而"匹配是怎么算的"是解释，收进问号正合适。

表格还多了一列**缓存读价**（`cost.pricing.col.cached`）：既然这一页的职责
就是"让用户看懂钱是怎么算出来的"，那它展示的三项费率必须与引擎用的一致。

---

## 3. 引擎侧的两处修正（都是"只在顺序不同时才出现"的静默错价）

### 3.1 同档内**更具体的匹配必须赢**

`crates/ta-core/src/pricing/engine.rs` 的 `find()` 原来是**同档内首个命中即生效**。
同档里同时存在 `{model:"m"}` 与 `{model:"m", provider:"p"}` 时，
胜负**只取决于配置里谁写在前面** —— 先配了模型价、后配了某个服务商的特价，
得到的却是先写的那条。

现在按 `(精确, 具体度, 声明顺序)` 选：

```
exact        ← match 里没有任何 `*`（精确整体优于通配）
specificity  ← model / provider / credential 写了几个（多者优先）
index        ← 都相同才按声明顺序（稳定、可解释）
```

新增的 `specificity()` 只管**档内**，**不跨档**：通配的 `manual` 依然赢过精确的 `builtin`
（优先级先于具体度）。`is_exact` 与 `specificity` 是两件事，前者优先级更高。

### 3.2 「未定价」徽标的判据必须走 `Model.lines`

`src-tauri/src/commands/entity.rs` 的 `build_channel_matrix` 原来自己写
`m.matches(&c.model)`。但 [docs/28 §2](28-lines-belong-to-providers.md) 把关系翻到模型侧之后，
**新写入的线路 `ChannelConfig.model` 恒为空串** ⇒ `chans` 永远为空 ⇒
那个「未定价」徽标**永远不会亮**。这是一个**不会报错的静默失效**
（编译过、测试绿、界面只是从来不提醒）。

现在走唯一口径 `doc.lines_of_model(m)`，并用模型自己的对外名 `m.id` 去解析价格
（定价条目按"对外模型"匹配，所以这里必须问"这个**模型**有没有价"，而不是线路的遗留字段）。

同一处还有第二个同构缺陷，是**新加的测试逼出来的**：`Cell.model_id` 原来也是
`find_model(&ch.model)`，对空串 `ch.model` 什么也解析不出来 ⇒ 得到空串 ⇒
服务商向导"这家已有哪些模型"那一块（`addedModelsOf` 按 `cell.model_id` 聚合）
对新配置**整块空掉**。现在分三条路：

1. 遗留字段 `Channel.model` **非空** → 老规矩（解析不出就原样回显，行为一字不变）；
2. 遗留字段为空 → 按 `Model.lines` 反查：
   * **恰好一个**模型用它 → 就是它（整份配置里只有一个模型声明用它，不是猜）；
   * 零个或多个 → **空串**。

> 为什么"多个"也留空而不是随便挑一个：`Cell.model_id` 是**遗留的单值字段**，
> "一条线路服务多个模型"它装不下。随便挑一个就是**静默错报**。
> 前端要完整关系应当用 `model_list` + `Model.lines`，那里才是真源。

---

## 4. 清除定价：一条纪律加一处修正

### 4.1 下标安全（沿用成本中心那条纪律，没放松）

`pricing_delete` 是按**下标**删的，而 `pricing_list` 的下标会随**任何一次增删整体平移**。
拿旧下标去删会**删错人** —— 这种错是静默的（界面只是少了另一条价）。

所以清除之前**重读一次**列表，确认那个下标上**仍然是**用户看到的条目
（`checkIndexStillOurs`）。判据比的是 **`match.model` 相同**，不是整条 JSON 全等：
用户可能在别处把这个模型的价格改过（同一个模型改价 = 同一条的价格变了），
那时**下标仍然是对的**，拒删反而让用户删不掉。真正要拒绝的是
"这一格已经不是那个模型了"（说明列表被增删平移过）。

### 4.2 为什么"要删哪一格"要在点的那一刻**锁定**

第一版把确认框的可见性绑在实时 `existing` 上。结果是：确认框一出现，列表就可能被别处改掉
（本页自己的重读、别的窗口增删）；若那时实时 `existing` 变成"找不到"，
**整个确认框会连同错误提示一起消失** —— 用户点了"确认"，什么都没发生，
也没有一句话告诉他为什么。

现在在点「清除定价」的那一刻把 `{ entry, index }` **一起冻结**（`clearTarget`）：

* 要删的是哪一格，在**点的那一刻**就定了；
* 冲突由 `checkIndexStillOurs` 拒绝并**原地显示**原因（确认框不收起，列表同时刷新）。

---

## 5. 覆盖度：这是**换**不是减

| 能力 | 改之前 | 改之后 |
|---|---|---|
| 给一个模型定价（对所有线路通用） | **做不到**（两个下拉必填） | `models-price-open`（每行一个） |
| 给某个服务商 / 凭据单独定价 | 界面**唯一**能做的事 | 界面不做；引擎仍支持（配置文件 / 导入表） |
| 改价 | 删了重建 | 同一个入口（回填当前价） |
| 清除定价 | 成本中心行内 `pricing-delete` | 模型页对话框里 `models-price-clear`（先确认） |
| 成本中心 | 读 + 写混在一起 | **只读** + 一句"改在哪儿" |
| 别名请求的定价与分组 | 按客户端字面名（拆行 / 未定价） | 归一到规范 id |

> 唯一的**净减**是"给某条线路单独定价"这条界面路径。它与用户这次的要求直接冲突
> （他要的就是"不要对线路计价"），而且它是**可选收窄**，不是主路径 ——
> 需要它的用户仍可以在配置文件里写 `provider` / `credential`。
> 这一条如实记在这里，不假装没有。

---

## 6. 验证

| 项目 | 结果 |
|---|---|
| `cargo test -p ta-core --lib pricing::` | **53 passed / 0 failed** |
| `cargo test -p tokenagency --lib commands::entity::tests::channel_matrix` | **2 passed / 0 failed** |
| `cargo test -p ta-gateway --lib -- alias_is_canonicalized unknown_model_name` | **2 passed / 0 failed** |
| `tsc --noEmit` | 干净 |
| `vitest run src/app/models.test.tsx src/app/cost.test.tsx` | **127 passed / 0 failed** |
| `vitest run`（全量） | **597 passed / 0 failed（30 文件）** |
| `cargo test --workspace` | **1317 passed / 0 failed / 1 ignored** |
| `build.ps1` | exit 0（五阶段全过） |
| 真机 e2e | **PASS=29 SKIP=2 FAIL=0**（exit 0） |
| e2e `--selfcheck` | **20/20** |

完整交付事实（安装包 hash、provenance、交付顺序）见 [docs/31 §318](31-progress.md)。

真机 e2e 的两个 SKIP 都是**诚实的**：`step15` 的 UI 路径随 `/policies` 删除
（后端规则另有 Rust 测试覆盖）、`step28` 夹具这一轮量不出任何真实节点延迟（环境）。

`step10` 的证据里逐条记下了它验到的东西：成本中心上写入口 **0 个**、
`cost-pricing-readonly` 有原文、模型格子里**输入框 0 个**、三项全空与非法价都零命令、
合法价真的落库且 `match` **只按模型**。

新增的牙（不只看它绿）：

* `crates/ta-core/src/pricing/engine.rs`：`more_specific_match_wins_within_the_same_tier_regardless_of_order`
  （**两种声明顺序都要给同一个价**，否则就是"顺序决定价格"）、
  `specificity_never_crosses_tiers`、`exact_beats_wildcard_even_when_the_wildcard_is_more_specific`、
  `model_only_entry_matches_every_provider_and_credential`；
* `src-tauri/src/commands/entity.rs`：`channel_matrix_flags_unpriced_models_through_model_lines_not_the_legacy_field`
  —— 用**新式线路**（`Model.lines` 有值、`Channel.model` 是空串）构造，正是旧写法算不出来的形状；
* `crates/ta-gateway/src/ingress.rs`：`alias_is_canonicalized_to_the_model_id` 与
  `unknown_model_name_is_kept_verbatim`（后者防的是"归一"被做过头：编一个规范 id 出来）；
* `src/app/models.test.tsx`：`entry.match` 深等于 `{ model }`、
  模型字段只读（`within(...).queryByRole("textbox")` 必须为 null）、
  三项全空零命令、下标错位拒删；
* `src/app/models.test.tsx` 的 `checkIndexStillOurs` 纯逻辑一组（same / changed / missing）；
* 真机 e2e step10：成本中心上 `pricing-add*` / `pricing-delete` / `pricing-edit`
  **一个都不许在**，且 `cost-pricing-readonly` 必须有话。

三类判定都被抽成**纯函数**并加了 `--selfcheck` 用例（`pricingWriteEntries` 5 条、
`judgePricedMatch` 6 条），所以它们不依赖真机碰运气就能被反复验；
`judgePricedMatch` 的牙是**注回旧判据后恰好那 1 条变红**验过的（见 §7 第 4 条）。

---

## 7. 我自己犯的错（如实记录）

1. **`★` 字形**：我把一段旧注释连字符一起复制进 `PricingDialog.tsx`，
   里面有个 `★`。`src/app/design.guard.test.ts` 立刻红（[docs/15 §2.1] 不许自己产生 emoji，
   重点用字重）。这不是守卫过敏，是它按设计工作。
2. **`**/models` 把块注释提前闭合**：e2e 的 JSDoc 里写 `**/models 的每一行**`，
   那个 `**/` 结束了 `/** */` 注释，`node --check` 报
   `Unexpected identifier`。已改成 `` `/models` ``。**教训：在块注释里写路径，
   别用 markdown 的加粗把斜杠包起来。**
3. 第一版确认框"错误提示会一起消失"（见 §4.2）—— 那个 bug 是被
   `models.test.tsx` 里"下标错位 → 拒绝执行"那条断言抓出来的，不是我自己看出来的。
4. **e2e 判据越过了语义、差点把一次假红当成产品缺陷**：step10 第一版断言
   落库条目的 `provider` / `credential` **键必须不存在**，真机跑出来是
   `{"model":"ta-e2e-m-add","provider":null,"credential":null}` → 判红。
   查证：`PriceMatch` 是 `Option<T>` + `#[serde(default)]`，`None` 落库就是显式 `null`
   （整份配置都这样，同一行就有 `"usd_to_cny":null`）；而引擎读 `null` 与读"键不存在"
   **完全同构**（反序列化后都是 `None`）。所以那是挂具在要求一种**序列化习惯**，
   不是定价语义错了。修法有三层，缺一不可：
   * e2e 判据改成 `== null`，并抽成纯函数 `judgePricedMatch`（6 条 `--selfcheck` 用例）；
   * 把**真机那次的原样输入**（显式 `null`）喂进用例 —— 注回旧判据后**恰好那 1 条变红**、
     4 条反向用例仍正确分类；
   * 在 Rust 侧新加 `explicit_null_and_missing_key_are_equivalent` 把"两种形状等价"
     钉死（不再只靠注释说），并断言两份 JSON 解析出来的**价格也一样**。

> **教训**（与第 317 轮那条同源）：判据要判**语义**，不要判**序列化习惯**。
> 一个形状上的不一致，先问"它改变了行为吗"，再决定该不该判红。


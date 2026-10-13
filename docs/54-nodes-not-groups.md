# docs/54：放弃「订阅里分组」，订阅里只拿节点

用户第 321 轮的完整裁决（逐字，两段，先问后答）：

> 每一条订阅能够点进去设置每一个节点的开关，包括"自动选择"和"直连"两个无法关闭的节点一起，
> 会在服务商那边可以看到，这样设计才比较简单合理

追问"入口长什么样"之后，用户自己回了整段设计：

> 先到网络界面，再去点击一个订阅，里面就可以看到这个订阅的所有节点，可以打开或者关闭，
> 就和服务商的线路是一样的，可以打开和关闭，**直接放弃订阅里面分组这一概念，只拿节点**，
> 我们的自动选择和他的不一样，直连是伪节点，但属于一直有的。
> 我希望的操作就是这边配置好后，去添加服务商那边，比如设置服务商的网络的时候，**默认直连**，
> 然后可以选则我配置好的节点，**其中自动选择和直连在最上面**，自动选择是什么呢，
> 就是自己用所有节点 ping 这个订阅商的 url，那个延迟低就用哪个，**有粘性**，
> 自动选择换节点的时机就是，当这个节点报了连接性错误，就是连不上了，自动选择就再 ping 一次，再选

这一轮做四件事，每件都对应上面的一段原话。

---

## 1. 「自动选择」的语义**已经存在**，本轮只做接线

用户描述的那套算法与仓库里已有的 `choose_auto_export` **逐条一致**，所以**没有新算法**：

| 用户说的 | 代码里的位置 | 一致性 |
|---|---|---|
| "自己用所有节点 ping 这个订阅商的 url" | `crates/ta-subscription/src/auto.rs` `candidates_from_nodes` + `provider_delays` | 探的是**该服务商自己的** probe URL |
| "那个延迟低就用哪个" | `lowest_latency()` | 取最小；**测不到的绝不当 0**（0 会被当成最快） |
| "有粘性" | `KeepReason::SameAddressStillThere` | 上次那条仍在且仍通 → **沿用，哪怕它不是最快的** |
| "报连接性错误就连不上了，再 ping 一次再选" | `PickReason::PreviousUnreachable` / `PreviousGone` / `ManualRefresh` | 只有这四种时机重挑 |

粘性**按地址（`node.server`）比，不按名字**：机场改名很常见，按名字比会把"同一个出口"
误判成"换了节点"，于是每次订阅更新都换 IP（同账号多地并发 = 封号风险）。

### 一条必须守住的细节：`select` 不是可选项

「自动选择」= `subscription_auto` + **显式 `group_type: select`**。

省掉它会退回 `url-test`，而 `url-test` 会按自己的 `interval` **周期性重测重选** ——
出口 IP 自己就变了，粘性直接不成立。所以：

* 后端 `Lane::automatic()` 显式写 `select`（`domain/behavior.rs`）；
* 前端 `lanePayload()` 在 `kind === "subscription_auto"` 时**自动补** `group_type: "select"`。

第 2 条是本轮**新加**的：界面上那个"组策略"下拉随"放弃分组"一起删了，
但语义不能跟着删 —— 否则新建的线路会**静默**变成会乱跳的那种（见 §4）。

---

## 2. 「放弃订阅里分组」：概念删除，但**枚举值保留**

删掉的是**概念与全部界面入口**，不是 Rust 里的那个 variant。原因是一条**启动顺序**约束：

```
state.rs::bootstrap → config.load()          ← 反序列化（此时必须有合法 variant）
                    → needs_migration()      ← 才轮到迁移
                    → migrate()
```

迁移跑在**已经反序列化完**的 `ConfigDoc` 上。若把 `LaneKind::SubscriptionGroup` 从枚举里删掉，
老配置会在 `config.load()` 就**解析失败**，迁移**根本没机会跑** —— 用户升一次版本就丢配置。

所以：

| 层 | 处置 |
|---|---|
| `LaneKind::SubscriptionGroup` | **保留**，但文档改标为「遗留（第 321 轮取消的概念）」 |
| `Lane.group` | **保留**（同上） |
| 校验器 | 出现即报 `lanes[i].kind` = `INVALID_VALUE`，消息说清"已取消 + 该换什么" |
| 内核配置生成 | 出现即报 `removed_kind`，**不落进任何"能用"的分支** |
| 界面 | `LANE_KINDS` / 文案映射 / 表单输入框 / `subscription_groups` 调用**全部删除** |

「报错而不用」是关键：静默接受会让它继续按 `url-test` 跑，而那是会换出口 IP 的那种。

---

## 3. v2 → v3 迁移：把老的分组线路**转成自动选择**

`crates/ta-core/src/config/migrate.rs` 新增 `v2_to_v3`，`default_version()` 2 → 3。

对每条 `kind == subscription_group` 的线路：

```
kind       = subscription_auto      // 概念没了，转成"整份订阅择优"
group_type = Some(Select)           // ★ 必须显式：否则是 url-test，出口会自己乱跳
group      = None                   // 组名这个概念取消
```

**为什么不直接删掉那条线路**：它被 `net.default_lane` / `providers[].lane` /
`credentials[].lane` / `channels[].lane` / `policies[].action.use_lane` 引用着，
删掉会让那些引用变成悬空（校验器随即报错，用户的配置直接不干净）。**转换**保住了引用。

迁移**幂等**（跑两次结果相同）——`config_rollback` 可能把老文档放回来。

### 顺带修正的默认配置

`config.example.yaml` 与 `defaults.rs` 原来有一条 `lane-jp-original`（订阅自带分组），
它现在是非法的。改成 `lane-jp-auto`（`Lane::automatic`，显式 `select`），
并把引用它的凭据 / 通道一并改指过去。

`example_config_guard` 这一轮**先红后绿**：它正是为此存在的（结构体改了、示例没改 → CI 红）。

---

## 4. 界面上真正删掉了什么（可执行的清单）

| 位置 | 处置 |
|---|---|
| `src/pages/network/model.ts` | `LANE_KINDS` 5 → 4；`LANE_KIND_LABEL` 去掉它；`missingFieldOfDraft` 去掉 `group` 分支 |
| 同上 | `LANE_GROUP_TYPES` / `GROUP_TYPE_LABEL` 两个导出**删除**（只服务于那个下拉） |
| `src/pages/network/index.tsx` | 去掉 group / group_type 输入框、`network-lanes-known-groups`、`subscription_groups` 查询 |
| i18n（中英） | 删 `network.laneKind.subscription_group`、`network.groupType.*`、`lanes.write.group*`、`knownGroups*` 等 |
| `src/app/network.test.tsx` | 那条"kind=订阅里的组：先点明缺 group…"**换成反回归闸门**（见下） |

反回归闸门（新的那条测试同时钉三件事）：种类下拉里**选不到**它、页面上**没有** group/group_type
控件、这一页**一次都不调** `subscription_groups`。

---

## 5. 新页面：`/network/subscriptions/:id`

入口就是用户说的"先到网络界面，再去点击一个订阅"：网络页的**订阅名现在是真链接**
（`network-subscription-open`，`href` 指向 `/network/subscriptions/<id>`）。

页面只做一件事：**列出这份订阅的全部节点，每个一个开关**。没有分组、没有筛选器。

### 顶部两行是固定的，而且**不可关闭**

用户原话："包括'自动选择'和'直连'两个无法关闭的节点一起"。

| 行 | 是什么 | 为什么不能关 |
|---|---|---|
| **自动选择** | 这份订阅里所有启用节点 ping 该服务商 URL，取最低延迟，按地址粘性 | 它属于**整份订阅**，不是某个节点；关掉它等于关掉整份订阅（那件事由「订阅启用」负责） |
| **直连** | 内核内置的 `DIRECT` | 它是**伪节点**：内核自带、不由订阅提供，关掉没有意义 |

两行的开关**都禁用**（`disabled`），并各自写出为什么 —— 只画一个灰开关而不说原因，
用户只会反复点它。测试同时钉"禁用"和"在节点行**之前**"（DOM 顺序 = 视觉顺序）。

### 三档状态不许混

| 情况 | 判据 | 显示 |
|---|---|---|
| 还没解析过 | `last_update_ms` 与 `error` **都没有** | 「还没解析结果」+ 提醒刷新；**这一轮根本不发** `subscription_nodes` |
| 解析过但没有节点 | 有 `last_update_ms`，清单为空 | 「确实没有节点」（第二档文案） |
| 读失败 | 命令报错 | **后端原文**（含 TA 码） |

第一档之所以"根本不发"：后端在没有已采纳版本时返回的是**错误**（不是空数组），
发了就会把一次必然失败画成"这份订阅没有节点"。

---

## 6. 两个**真 bug**（本轮发现并修掉，不是顺手）

### 6.1 关掉的节点在内核那侧**从来没被排除过**

`Subscription.disabled_nodes` 此前只被读两处：`net.rs` 的节点清单 UI，以及
`apply_auto_export` 的候选过滤（应用侧挑"自动"出口时）。

**内核配置生成里一次都没用过**（`crates/ta-kernel/src/config_gen.rs`）。后果：
组类型是 `url-test` / `fallback` / `load-balance` 的线路，**内核自己**在那个组里择优，
而它看不到 `disabled_nodes` —— 用户关掉的节点照样会被选中。**开关是假的。**

修法：把"关掉的节点"合并进组的 `exclude-filter`。

* 内核的 `proxy-providers` 是**整份订阅**一个条目，节点由内核自己拉，我们没有逐节点增删的接口；
  组级 `exclude-filter` 是唯一能表达"这些节点不要"的地方。
* 逐个 `regex::escape`（节点名是用户数据，可能是 `东京(01)` / `a.b` / `x+y`，
  不转义会把括号、点号变成元字符，**误伤别的节点**）。
* 用 `^(?:...)$` **全字**匹配：裸 `a|b` 是子串匹配，关掉 `Tokyo` 会连带排掉 `Tokyo-01`。
* 一个都没关 → **一个字都不加**（不写空的 `^(?:)$`）。
* 排序 + 去重，保证同一份配置**逐字节可复现**（否则每次重载都"看起来变了"）。
* 用户自己写的 `exclude_filter` **保留**，两者是并集。

### 6.2 `subscription_nodes_enabled` 只写库，不碰内核 —— 又一个假开关

它原来走裸 `save_validated`（只写库 + `set_doc`），**既没更新内核配置源、也没 `apply`**。
于是界面上的开关当场变了，内核还在按旧配置择优。

改成与 `subscription_upsert` / `lane_upsert` 同一条路径：
`commit_config`（校验 + 写库 + 快照 + 分配端口 + 同步 `kernel_source` 的 doc/ports）
再 `maybe_apply_kernel`。内核没跟上就**如实报错**（"已保存，但内核没有加载新配置"），
不假装成功。

---

## 7. 服务商侧：默认直连，自动选择与直连排最上面

用户原话："设置服务商的网络的时候，默认直连，然后可以选则我配置好的节点，
其中自动选择和直连在最上面"。

`providerLaneChoices` 的顺序改为：

```
自动选择（auto:<订阅>，多份订阅时带名字） → 直连（direct:） → 不指定（继承） → 已配置线路
```

**顺序与默认值是两件事**：按用户语序把"自动选择"排在最前，但**默认值仍是「直连」**
（向导的 `EMPTY_STEP1_DRAFT.lane = "direct:"`）—— 不能因为把某项排前面就顺手让它变成默认，
那会让新服务商悄悄走代理。

值都是**标记**（`auto:<订阅>` / `direct:`），由后端 `provider_upsert` 解析成真实线路；
前端不建线路、也不自己编 id（"这条线路该长什么样"的判断在后端只有一份）。

---

## 8. 验证

| 门 | 结果 |
|---|---|
| `cargo test --workspace` | 全绿（ta-core 399 + ta-kernel 108 含 4 条新的 disabled-node 测试） |
| `npx tsc --noEmit` | 0 错 |
| `npx vitest run` | **626 passed / 34 files**（新增 `subscriptionDetail.test.tsx` 6 条） |
| 迁移 | 4 条新测试：转换正确、幂等、结果被认成 auto lane、迁移后再出现即报错 |

### 真机上"关掉的节点真的被排除"是怎么证明的（e2e `step23`）

源码级测试只能证明**生成了那条排除规则**；"它真的进了内核那份文件"必须在真机上读文件。
`step23` 的做法：关掉一个节点 → 读 `disabled_nodes` → 读
`<data>/kernel/kernel.generated.yaml` → 断言那个节点的名字出现在 `exclude-filter` 里 → 再打开复原。

真机读数（`PASS=30 SKIP=2 FAIL=0` 那次）：

```
- {name: "LANE-lane-ta-e2e-auto", type: select, use: ["ta-e2e-sub-form"], …,
   exclude-filter: "^(?:剩余流量：124\\.4 GB)$"}      ← 文件里（YAML 双引号，反斜杠双写）
反转义后的排除正则: "^(?:剩余流量：124\.4 GB)$"        ← 真实正则值
```

**两个我自己踩的坑（都是挂具的错，产品一直是对的）**，记在这里因为很容易再犯：

1. 路径写成 `<data>/kernel.generated.yaml`，真路径是 `<data>/kernel/kernel.generated.yaml`；
2. 生成器用 YAML **双引号**写这个正则（`q()`），双引号标量里 `\` 要写成 `\\` ——
   拿节点名去 `includes()` **原文**永远不命中，必须先按 YAML 规则反转义再比。

> 第一次判红时，如果我只看到 FAIL 就去"修产品"，会把一段本来正确的逻辑改坏。
> **先证明红在哪一侧，再动手。**

### 守卫验牙（不是"跑过了"，是"拆掉它会红"）

| 拆掉什么 | 预期的红 | 实测 |
|---|---|---|
| 顶部两行的 `disabled` | 详情页测试 | **1 failed** |
| "没解析过就不发请求"的 `enabled` 条件 | 详情页测试 | **1 failed** |
| `merge_disabled_exclude` 的接线 | kernel 测试 4 条 | **4 failed** |

### 本轮**没有**做的事

* 没有删除 `LaneKind::SubscriptionGroup` / `Lane.group`（理由见 §2，删了会丢配置）。
* 没有改 `auto.rs` 的算法（用户描述的语义本来就一致，见 §1）。
* 没有实现"自动选择"的全新探测方式：仍然复用既有的
  `kernel.provider_delays` + `decide_export`。

---

## 9. 与其它文档的关系

* [29](29-auto-network.md)：本轮的「自动选择」语义与之同源，**§1 是它的接线说明**。
* [48](48-balanced-sticky-only-routing.md)：路由与开关的权威；本轮把它的"节点开关"
  从**界面假开关**修成真的（§6）。
* [04](04-config-schema.md) §7：`LaneKind` / `Lane.group` 的字段说明以本轮为最新
  （`subscription_group` 标为遗留）。
* [53](53-referenced-delete-must-be-actionable.md)：同为"拒绝必须能照着改"的落实；
  本轮把"转移"也纳入同一个思路（迁移而不是删除被引用的实体）。

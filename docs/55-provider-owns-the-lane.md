# docs/55：放弃线路级别的分配 —— 每个服务商统一一个节点

用户第 322 轮的原话（逐字）：

> 线路分配这个页面太累赘了，而且，线路级别的分配逻辑我们已经放弃了，取而代之的，
> **每个服务商统一一个节点**

这一轮是**减法**：删掉一整页、删掉三层分配、删掉四条命令。

---

## 1. 删掉的三级：它们早就是死层级了

改之前，"一条通道走哪条线"是按**五级继承**解析的：

```
policy.use_lane  →  channel.lane  →  credential.lane  →  provider.lane  →  net.default_lane  →  direct
```

审计这五级在**界面上还有没有写入口**，结论是：

| 级别 | 还有写入口吗 | 证据 |
|---|---|---|
| `policy.use_lane` | **没有** | 策略编辑页早已不在；本页只**读**它 |
| `channel.lane` | **没有** | 服务商详情页的线路行只有「名字 + 启用开关」（[docs/25] 那一轮删掉了行内代理控件） |
| `credential.lane` | **没有** | 凭据退化成 1:1 之后就没有过编辑入口 |
| `provider.lane` | **有** | 服务商详情页 / 向导的「网络代理」下拉（`lane_options`） |
| `net.default_lane` | **有** | `/network` 的全局默认 |

**三个没有写入口的级别仍在参与解析** —— 这不是"留着以后用"，这是一个**真实的隐患**：

> 老配置里一个用户**看不见、也改不掉**的 `channel.lane` 会**静默压过**他当前在服务商页
> 选的那条线路。他会觉得"我明明改了，怎么没生效"，而界面上没有任何地方能告诉他为什么。

所以这三级的**字段、解析、写入、校验、命令**整条删除。生效线路只剩：

```
provider.lane  ??  net.default_lane  ??  direct
```

### 为什么能删得掉（序列化层的前提）

三个结构体都带 `#[serde(flatten)] extra: Map<String, Value>`，**且没有 `deny_unknown_fields`**。
所以老配置里残留的 `credentials[].lane` / `channels[].lane` / `policies[].action.use_lane`
会被 `extra` 吸收：**解析不失败、值也不丢**，只是不再参与任何判定。

这正是 [docs/48] 删 `selector` 时用过的同一条路子 —— 有先例，不是新发明。

---

## 2. 删掉的四条命令

| 命令 | 为什么没有存在意义了 |
|---|---|
| `lane_assignments` | 返回"每条通道最终走哪条线 + **来自哪一级**"。只剩两级之后，"来自哪一级"是个**过期的说法**：它恒为 `provider` 或 `default` |
| `lane_pick_options` | "通道/策略选线路时的候选清单"。它服务的下拉已经随 `/policies` 与通道页删除 |
| `lane_reverse_lookup` | "谁显式引用了这条线"的倒排。它列的正是那三个被删的字段 |
| `channel_bulk_assign_lane` | "把一批通道的 lane 改成同一个值"。`ChannelConfig.lane` 已删除，它**没有可写的地方** |

**没有调用方**是删它们的前提，逐条核过：
三条 `lane_*` 只被 `/network/assignments` 调用；`channel_bulk_assign_lane` 在契约与
`commands.ts` 门面里有包装，但**页面侧一处都没有**（它是 `docs/21` 那轮通道页的遗留）。

契约条数 `ARCHIVE_TOTAL` **120 → 116**。与 [docs/28 §1.3] 删 `credential_delete` 一样，
这是**减法**，所以除数字外归档件里那几行也必须同时删 —— 守卫比对的是**键集**，不只是条数。

---

## 3. 删掉的一页：`/network/assignments`

那一页存在的**唯一理由**是展示五级继承链。前三级没了之后，它变成：

* 「各级显式值」——只有两节还有内容（服务商级 / 全局默认），另三节恒空；
* 「后端算的最终生效线路」——`lane_assignments` 已经删除；
* 「候选线路」「倒排查引用」——`lane_pick_options` / `lane_reverse_lookup` 已经删除。

一张只剩两级的"分配表"，加上一句"本页不自己算最终生效线路"的解释，
除了让人以为还有级别可调之外没有任何用处。**整页删除**，连它的：

* 路由（`routes.ts` 的 `network-assignments`）、导航键（`nav.networkAssignments`）、
* 读件（`model.ts` 的 `readAssignments` / `AssignmentView` / `ReverseLaneRow` / `laneSourceLabel`）、
* 全部 i18n 文案（`network.assignments.*` 约 90 个键，中英各一份）、
* mock（`lane_assignments` 的假数据）、
* 测试（8 条用例）、以及 `autoEllipsis` 守卫里那一行。

**走哪个节点现在只能在一处改**：服务商详情页（或向导）的「网络代理」下拉。

---

## 4. 网关与路由侧的收敛

`lane_override` 这条管线整条拆掉 —— 它本来就是"策略级覆盖"的载体，而策略级已经没有了：

| 位置 | 原来 | 现在 |
|---|---|---|
| `RouteDecision.lane_override` | 存策略算出的覆盖 | **字段删除** |
| `PreparedRequest.lane_override` | G2 从决策抄过来、每个候选带着它 | **字段删除** |
| `RoutingIndex::lane_with_override` | "覆盖优先，否则继承链" | **方法删除**（`resolve_lane` 就是全部） |
| `resolve_lane(...)` 五参 | 策略 / 通道 / 凭据 / 服务商 / net | **收敛成两参**（服务商 / net） |
| `apply_pin(..., action, ...)` | 用 `action` 读 `use_lane` | **参数删除**（不再有 `use_lane`） |

`resolve_lane` 的签名我**没有**保留那三个不读的形参 —— 留一个"永远不读的参数"，
下一个人会以为"是不是这里漏了什么"。唯一的调用点（`ta-gateway/src/pipeline.rs`）一并改了。

---

## 5. 服务商详情页的语义没有变，但**范围**变了

服务商详情页的「网络代理」下拉本来就在写 `provider.lane` —— 这一轮**没有改它的行为**。
变的是这句话现在**更准确了**：

> 这个下拉决定的是**这家服务商的所有线路**走哪里，而不是"只影响某几条"。

导入向导的两个命令（`provider_lines_create_all` / `model_apply_import`）原来把入参里的
默认代理写进**每一条新建通道**的 `lane`；现在改写成 **provider 的 `lane`**。
语义差异必须说清：它现在会改**这家服务商的全部线路**，而不只是这次新建的那些 ——
**那正是"每个服务商统一一个节点"的意思**。

---

## 6. 端到端的影响面（哪些东西跟着改了）

| 类别 | 具体 |
|---|---|
| 域模型 | `Credential.lane` / `ChannelConfig.lane` / `PolicyAction.use_lane` 三个字段删除；构造器 `with_lane` 两处删除 |
| 解析 | `ChannelConfig::effective_lane` 只读 provider/net；`resolve_lane` 两参 |
| 校验器 | 三处"引用的 lane 必须存在"检查删除（那三个字段不再是校验对象） |
| 路由引擎 | `RouteDecision.lane_override`、`lane_with_override`、`apply_pin` 的 `action` 参数 |
| 网关 | `PreparedRequest.lane_override`、`lane_for` 的两参签名、两处 `build_attempt` 调用 |
| 命令面 | 4 条命令删除 + 注册表；`delete_lane` 的引用类别 **5 类 → 2 类** |
| 契约 | `bindings.ts`、归档件、`ARCHIVE_TOTAL` 116、`commands.ts` 门面、`mock.ts` |
| 前端 | `routes.ts`、页面文件、`model.ts`、`keys.ts`、i18n（键 + 导航）、测试 |
| 示例配置 | `config.example.yaml` 重新生成（三个死字段随之消失） |

### `delete_lane` 的引用类别少了三种

它列"谁在挡"的依据原来是五类，现在只有两类：`net.default_lane` 与 `providers[].lane`。
如果哪天有人把某个已删字段的检查加回去，用户会遇到**"这条线明明没人用，却说被引用、删不掉"**，
而**照它说的去改也改不掉**（那个字段在界面上根本不存在）。所以新增了一条反回归测试
（见 §7 的"验牙"）。

---

## 7. 验证

| 门 | 结果 |
|---|---|
| `cargo test --workspace` | 全绿（ta-core `lib` **401**，比上一轮 +2；共 29 组 ok） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **0 warning** |
| `cargo fmt --all -- --check` | 干净 |
| `npx tsc --noEmit` | 0 错 |
| `npx vitest run` | **605 passed / 34 files**（比上一轮少 22 条 —— 全是随页面一起删掉的用例） |
| `command_surface_guard` | 活契约 = 归档件 = **116** 条，键集一致 |
| `example_config_guard` | 重新生成后绿（它**先红后绿**：三个死字段还在示例里时它会拒绝） |

### 新增/改写的测试（都是"钉住新语义"而不是"删光了事"）

* `effective_lane_is_provider_then_default_only` —— 并且**显式证明**"拿一个还带着
  `lane` 的 credential / channel（值在 `extra` 里）传进来，结果不受影响"；
* `resolve_lane_ignores_policy_and_is_direct_when_nothing_is_set` —— 服务商级压过全局默认，都没配就 direct；
* `legacy_policy_use_lane_is_ignored` —— 老配置里写着 `use_lane`，**不再锁定任何东西**；
* `provider_lane_down_eliminates_everything` —— 服务商那条线路 down ⇒ 全部淘汰（原来靠策略覆盖触发）；
* `removed_channel_lane_is_not_a_reference_to_check` 等三条 —— 已删字段**不是校验对象**；
* `removed_lane_level_fields_do_not_block_deletion` —— 已删字段**不许挡住删除**。

### 守卫验牙（不是"跑过了"，是"拆掉它会红"）

| 拆掉什么 | 预期 | 实测 |
|---|---|---|
| 在 `delete_lane` 里把 `channels[].lane` 的 extra 残留重新当成引用 | `removed_lane_level_fields_do_not_block_deletion` 红 | **FAILED**（`expect("这三个字段已经不参与判定…")` 那一句） |
| 三个死字段留在 `config.example.yaml` | `example_config_guard` 红 | **FAILED**（"修复：重新生成"） |
| 归档件里不删那四行 | `command_surface_guard` 红 | 键集不等（核对过输出口径） |

验牙完成后按纪律**恢复并重跑** fmt / clippy / test，全绿（上面那张表是恢复后的数字）。

---

## 8. 本轮**没有**做的事

* **没有**动 `net.default_lane`（"全局默认"仍然是最后一级回退，用户仍然可以改它）。
* **没有**动内核配置生成：`provider.lane` 决定 lane，lane 决定内核组 —— 这条链路本来就是对的。
* **没有**删 `LaneSource` 之外的任何枚举值：契约条数的减少全部来自这四条命令。
* **没有**给"每个服务商一个节点"加新的存储：用的就是既有的 `Provider.lane`。

---

## 9. 与其它文档的关系

* [04](04-network-core.md) §8「线路分配」五级链**以本文为准**（那一节的继承链说明已过期）。
* [02](02-config-and-data-model.md) 里 `policy.use_lane ?? channel.lane ?? credential.lane …`
  那一行解析式**以本文为准**。
* [21](21-delete-and-lane-settings.md)：`channel_bulk_assign_lane` 的规格**已作废**（命令已删）。
* [29](29-auto-network.md)：`provider.lane` 是「自动」出口归属的**同一处**，两轮收敛到同一个答案。
* [54](54-nodes-not-groups.md)：同为"删概念"的一轮；本文的字段删除沿用了它的
  "`extra` 吸收 + 校验器不再检查"这条路子。

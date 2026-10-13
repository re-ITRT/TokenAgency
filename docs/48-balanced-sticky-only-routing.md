# docs/48：路由收敛为「平均粘性」+ 模型/线路开关

> 用户（两轮连说）：
> 「既然分码也分不了，那么久只能麻烦用户手动切换了。首先模型路由这个板块删掉，路由只保留一种，
> 就是平均粘性，根据对话的 sessionID 去粘，没有这个 ID 就随机，尽量模型中的每个线路的用量差不多
> （就是现在 A 线路比 B 用的多，那么如果又新的会话被创建就给 B），值得一提的是，这个统计的数据放在
> 总的数据里面，需要的时候去拿，反正你优化一下，让数据库压力不是那么大，然后就是这个会话的这个线路
> 报错了，就粘到另一个身上」
> 「所有模型和路由都在模型目录里面去，我认为应该给一个模型加上开关，每个模型的每一个线路也加上开关，
> 方便用户调整，不用频繁删除与创建」

## 1. 用户定的三件事（我问过，用户明确选了）

| 决策 | 用户的选择 |
|---|---|
| 平衡口径 | **按 token 用量**（不是请求数、不是成本） |
| 「路由策略」页 | **功能是重复的，一起融入模型目录里面去** |
| 模型开关关掉后 | **彻底停用：不列出 + 拒绝请求** |

上一轮（`docs/47`）的结论是"各家额度耗尽的返回形状分码也分不干净"，所以用户转向
**"不猜了，让用户用开关手动调度"** —— 这份文档就是落地这件事。

## 2. 关键发现：不需要重写引擎

`docs/47` 那轮我以为要动大手术。侦察后发现有两条既成事实让工作量小得多：

1. **`channel_set_enabled` 命令已经存在**（`src-tauri/src/commands/catalog.rs`，
   服务商详情页的线路启停就在用它）。线路开关不是新能力，是**搬到模型目录**。
2. **"会话粘性 + 报错改粘另一条"已经实现**：`Selector::StickyThenPriority`
   配 `release_on: [HardError]` 就是"同会话同线路；硬失败则释放粘性、重新选"。
   见 `crates/ta-core/src/routing/selector.rs::sticky_pick` 与
   `crates/ta-core/src/config/defaults.rs`。

所以「平均粘性」= **现有的粘性机制 + 把"未命中时的兜底顺序"从 priority 换成"token 用量最少"**。

## 3. 算法：平均粘性（balanced sticky）

一次请求的选路，按顺序：

1. **粘性命中**：按 key 规格派生 key，查表命中且该线路在候选集里 → **用它**（这是"粘"）。
2. **粘性命中但线路不可用**（冷却 / 已禁用 / 失效）→ 释放粘性，走第 3 步。
3. **兜底（新会话）**：在候选集里选 **token 用量最少**的那条；并列时按 `priority`，
   再并列按候选集原序（稳定，不随机抖动）。
4. **失败改粘**：本条线路**硬失败**（`ReleaseReason::HardError`）→ 释放粘性记录 →
   下一次请求走第 3 步重新挑，于是自然"粘到另一个身上"。

### 3.1 「没有 sessionID 就随机」

key 规格沿用现有的 `ChannelGroup.sticky.key`：
`header:x-session-id` → `header:x-conversation-id` → `metadata.user_id` → `body_hash`。

全部派生不出来（没有任何会话标识）→ 没有 key 就**没有粘性记录可写**，每次请求都是第 3 步。
此时"选 token 最少的"会在**并列**时反复选同一条（确定性强但不够散），所以：

> **无 key 时：在"用量最少的那一档"里随机挑一条。**
> 有 key 时：并列按 priority 稳定决定（同一个会话必须落同一条，随机会破坏粘性）。

这个区分是必须的 —— 同一句"随机"用在有 key 的路径上会**打碎粘性本身**。

### 3.2 平衡是"尽力"，不是保证

按 token 拉平只能在**新会话**上生效：老会话被粘住不动（这正是用户要的"粘"）。
所以两条线路的用量**不会**因为算法而严格相等，而是**新会话持续流向当前较轻的那条**。
UI 文案要说清这一点，不能承诺"绝对平均"。

## 4. 平衡数据从哪来（用户要求：不压数据库）

用户原话：「这个统计的数据**放在总的数据里面**，需要的时候去拿，反正你优化一下，
让数据库压力不是那么大」。

**决定：不新建表、不在写路径上加任何东西。** 复用已有的明细表按需聚合：

```sql
SELECT channel_id, SUM(total_tokens)
  FROM usage_request
 WHERE ts >= ?            -- 时间窗
 GROUP BY channel_id
```

* 走**已有的** `ix_usage_channel_ts (channel_id, ts)` 索引（`0001_init.sql:75`）；
* 结果在内存里**带 TTL 缓存**（默认 30s），**不是每请求查一次**；
* 过期后**由第一个需要它的请求触发一次刷新**（不是后台定时任务 —— 空闲时不产生任何查询）。

### 4.1 为什么不用 `usage_daily`

`usage_daily`（汇总表）的维度是 `(day, credential_id, provider_id, model, client, token_kind)`，
**没有 `channel_id`**。要给汇总表加线路维度就得改主键 —— 而汇总表是 v1 就有的核心表，
改主键代价与风险都远大于"加一个 TTL 缓存的聚合查询"。

### 4.2 诚实的取舍

* 时间窗（不是全量）：用量窗口滚动，"轻的那条"会随时间变。这是**特性**不是缺陷
  ——一直按全量历史拉平会让线路一旦落后就再也追不上。
* 缓存 30s：新会话可能连续几次都落到同一条，之后才切换。可接受（用户要的是"别偏得太狠"）。
* 查不到（表空 / 全部 0 用量）：退化为按 `priority` —— **不猜、不编造用量**。

## 5. 删什么、留什么

### 删除
* **前端页** `src/pages/routing/`（模型路由，25428 + 9977 字节）与 `src/pages/policies.tsx`
  （路由策略）；导航项 `nav.modelRouting` / `nav.plansAndRoutes` / `nav.policies` 对应入口。
* `Selector` 的其余 5 个取值：`priority` 之外只留一个策略。
* `PolicyAction.selector` 字段（只剩一种，这个字段没有意义了）。
* `STRATEGY_TABS` / `SELECTOR_KEY` / `SELECTOR_VALUES` 等页签与下拉。
* `docs/47` 那轮建议的"按厂商识别额度耗尽"—— **不做**（用户已改路线：手动切换）。

### 保留
* `Policy` 引擎本体（候选集/淘汰/失败转移仍然靠它跑）；
* `ChannelGroup`（模型→线路的候选集来源）；
* `channel_set_enabled`（线路开关的实现）；
* `policy_*` 命令**仍注册**（老配置里有策略；删命令会让存量配置无法读写）。
  **但前端不再有页面**，这一条要在文档里明说，不留"看不见却还在跑"的困惑。

### 新增
* `Model.enabled: bool`（模型级开关）。
* `RouteContext.balance: BTreeMap<ChannelId, i64>`（token 用量快照）。
* `ta_core::routing::BalanceSource`（用量来源的接缝）+ `SqliteBalanceSource`
  （带 TTL 缓存的真实现）。
* 模型目录页里的：模型开关、每条线路的开关。

### 7.1 前端这一侧最终的样子

* 删掉五个文件：`src/pages/routing/{index.tsx,routing.ts}`、`src/app/routing.test.tsx`、
  `src/pages/policies.tsx`、`src/app/policies.test.tsx`；
* **模型目录页新增两处**（这是用户第二段话的全部诉求）：
  1. **模型级开关**（`data-testid="models-enabled"`）：单开一列，与「对外暴露」分开 ——
     `expose` 管"列不列进 `/v1/models`"，`enabled` 管"能不能用"，混在一列里用户分不清
     "我只是藏起来"和"我把它关了"。
     写路径是 `model_upsert`（**整条覆盖写**：必须带 `row.raw` 再翻一位，
     只发 `{id, enabled}` 会被后端当成"其余字段为空"，把别名/分组/线路全部抹掉）。
  2. **每条线路一个开关**（`data-testid="models-line-enabled"`）：写路径是既有的
     `channel_set_enabled`（**不是新命令**）。
     **开关前先弹确认框**，因为线路是**共用**的 —— 关掉它不只影响当前这个模型，
     别的模型也一起少一条；确认框把"还有谁在用"如实列出来（名单来自已加载的模型清单，
     读不到就写"读不到"，**不写"没有别的模型在用"**）。
* 契约侧的 `policy_*` 命令**一个都没删**，`src/shared/api/commands.ts` 的包装也都留着：
  页面没了，但策略仍然是配置模型的一部分，老配置必须还能读写。
  这不是"死代码"—— 网络页的「线路分配」还在读 `policy_list`（`use_lane` 那一节）。

### 7.2 真机 e2e 的配套改动（不是"顺手"，是必须）

删掉两个页面之后，e2e 里**有三处**指着它们，全部要处理（留着就是假绿或指错方向的 FAIL）：

| 步骤 | 原样 | 现在 |
|---|---|---|
| `step11` | `/policies` 路由向导：能力不一致时必须显示逐字段差异并禁用提交 | **改成验两个开关**（[docs/48 §5]）：模型关掉要落库为 `enabled=false`、网关 `/v1/models` 不再列出它、按名字取 404、再打开能可逆；线路开关要先弹确认框、确认后才落库。覆盖度是**换**不是减 |
| `step15` | 同一页的能力一致性 UI 断言 | **如实标 SKIP**：UI 路径被删掉了，断言没有对象。理由与"后端规则仍在 `ta-core` 校验器 / `ta-gateway` 用例里被覆盖"一并写进证据 |
| `step22` | 逐屏截图清单里含 `#/routing` 与 `#/policies` | 去掉这两项 |
| `step21` | 用 `/policies` 的删按钮"解除引用" | 改成如实记一笔"界面已无删策略入口"，**不去访问不存在的页面**（那会表现成"页面没渲染出来"，把原因指错方向） |

顺带删掉两个**只被旧 step11/step15 用**的 helper（`wdKeepOptions` / `wdPickOptions`）——
没有调用方了，留着就是死代码；原位置留了一行注释说明它们为什么消失、要取回看 git 历史。

**删除两个页面的真实后果（必须说清，别让人以后才发现）**：
界面上**已没有任何入口**能新建 `channel_group` / `policy`。命令与后端能力都还在
（老配置照旧读写、网络页的「线路分配」照旧读 `policy_list` 的 `use_lane`），
但"从零写一条路由规则"这件事现在只能改配置文件。这正是用户要的方向
（他不想再维护自动规则，改成手动开关），但**它是一个能力上的减法**，不是纯搬家。

### 7.3 这一步的两处我自己的错

1. **模型页的用例断言写错了**：`new_session_picks_the_least_used_line` 第一版写
   "b/c 并列 → 取候选集原序在前的 c"，但有会话 key 时并列**按 priority**（b=20 < c=30），
   所以正确答案是 b。是用例红了才发现的 —— 这正是"先写断言再看结果"的价值。
2. **e2e 里我编了一个不存在的 testid**：`models-page` 在这个仓库里**根本没有**
   （模型页只有 `models-summary` / `models-table` / `models-row` 等）。
   它让 step11 直接 FAIL 成"页面没渲染出来"，还把 step21 的失败方向指歪。
   修完再跑才对。教训：新写的等待条件必须先在真机上确认它存在，不能照着别的页面猜。

### 7.4 ★ 顺带抓到并修掉的**数据丢失 bug**（比这一轮的主任务更重要）

改 e2e 时 step24 变红，追下去发现**不是 e2e 的问题，是产品的问题**：

> **删掉模型多条线路中的一条，模型会连同它剩下的线路一起被删掉。**

真机证据（e2e scratch 库 dump，模型 `ta-e2e-m-add` 有两条线路）：

| 时点 | models | channels | 模型的 lines |
|---|---|---|---|
| 删之前 | 1 | 4 | `[ch-…-m-add, ch-…-m-add-b]` |
| **删掉 `-add-b` 之后** | **0** | 3 | 模型没了 |

**根因**在 `src-tauri/src/commands/entity.rs` 的 `prune_orphan_models`：它判"这个模型还有没有线路"
用的是**遗留的反向字段** `doc.channels[].model`：

```rust
let still_wired = doc.channels.iter().any(|c| names.contains(c.model.as_str()));
```

而写命令层早就强制"**新写入一律写空字符串**"（`ChannelConfig::model` 的注释），
于是新配置里那个字段恒为 `""` → `names.contains("")` 恒假 → `still_wired` 恒假 →
**只要某条线路被删过，用过它的模型一律被判成孤儿**。

**修法**：判据改回真相 `Model.lines`（[docs/28 §2] 之后唯一真相），遗留字段降级为兼容兜底：

```rust
let still_wired = !m.lines.is_empty()
    || doc.channels.iter().any(|c| names.contains(c.model.as_str()));
```

**为什么以前没被发现**：旧 step11 会建一条策略、并把该模型的两条线路写进 `use_channels`；
step24 专门避开"被策略点名的线路"，于是它删的是**另一条**线路 —— 那条不在模型里，
`affected_models` 是空集，`prune_orphan_models` 早退（`candidates.is_empty()`），**bug 根本没被触发**。
我删掉 `/policies`、step11 不再建策略之后，step24 才第一次删到"模型正在用的那条"，
于是 bug 露出来。

**两个回归用例**（都做了有牙自证：把修复回退，前者必须变红）：
* `deleting_one_of_two_lines_keeps_the_model` —— 夹具**把 `channel.model` 也清空**
  （新配置的真实形状），所以"只看遗留字段"的实现在这里必然红；
* `deleting_the_last_line_still_removes_the_model` —— 反向钉住"一条都不剩时模型仍然跟着消失"，
  免得为了让前一条绿而把孤儿清理整个关掉。

> 这是"删掉两个页面"的**意外收获**：不是我想找 bug，是 e2e 的前置状态变了、把它暴露了出来。
> 也说明**e2e 步骤之间的隐式依赖本身就是风险** —— 现在这两条用例把它下沉成了明确的单元断言。

> **一处没做的、要说清的**：模型页**没有**画"每条线路的用量占比"。
> 用户的第二段话只点了两个开关，而"用量占比"要新开一条命令（契约从 120 涨到 121）。
> 引擎侧的用量统计是**真的**（`BalanceSource` + `SqliteBalanceSource` 都在跑），
> 只是**没把它显示到界面上**。这是一条明确的待办，不是"做漏了忘了说"。

## 6. 实现记录（真正落地的样子，含与上面设计的偏差）

设计写完才动手，动手过程中有三处与上面不同，都记在这里：

### 6.1 用量快照放在**路由门面**上，不放在 HTTP 管线里

原本打算给管线加一个 `balance` 依赖。实际放在了 `ta-gateway` 的 `LiveRouter`
（即在 `plan()` 里填 `RouteContext.balance`）。两个理由：

1. **职责**：只有门面同时看得见"用量来源"与"选路"，于是"每请求取一次快照"只有一处实现；
2. **代价**：管线是测试里的高频夹具，加字段要改十几处**与用量无关**的测试。

`RouteContext` 仍旧是**纯快照**（不含任何 IO 对象）—— 这条不能破，
否则 `ta-core` 那个"策略模拟器 = 真实决策"的不变式会静默变成假话。

### 6.2 刷新是**后台踢一脚**，不是"当场查库"

第一版用 `tokio::task::block_in_place` + `block_on` 在调用点当场查。两个问题：

* **单线程 runtime 上会 panic**（`block_in_place` 只允许多线程 runtime）——
  把"能不能发请求"押在"运行时是不是多线程"上太脆；
* 它把**请求本身**卡在一次 DB 查询上，而那正是要避免的东西。

现在 `used_tokens` **永不阻塞**：过期就 `tokio::spawn` 一次刷新然后立刻返回旧快照，
`refreshing` 门闩保证**一个 TTL 周期最多查一次库**（同时来 1000 个请求也只查一次）。

**代价（如实记）**：冷启动的第一个 TTL 周期里快照是空的 → 选路先按 `priority` 走，
刷新落地后才开始按用量平均。宁可头几秒不平均，也不要每请求挂在数据库上。

### 6.3 参数：TTL 30 秒 / 窗口滚动 24 小时

* TTL 30s = "新会话最多晚 30 秒才开始避开刚变重的那条线路"；
* 窗口 24h：太短（1h）会让刚用多的线路马上又被判成最闲，起不到拉平作用；
  太长（30 天）会让一条早期用多的线路**长期**被冷落。
  24h 是这两者之间的取舍，属于可调参数。

### 6.4 `Model.enabled = false` 的判据落在哪

两处，缺一不可：

* `ta-gateway/src/ingress.rs` 的 `models_list_body` 与 `exposed_model`
  → 判据 `enabled && expose`（不列出 + 按名字也取不到）；
* `ta-gateway/src/pipeline.rs` 的 `prepare()` → 名字**存在但被停用**时直接
  `TaError::ModelNotFound`（404 / TA-2001）。

为什么第二处必须有：只做第一处的话，"停用"只是"藏起来了"—— 客户端只要还记得模型名
就照旧能打进来，那个开关就是假的。这正是用户想避免的东西。

> 顺带记一个**没有改**的不一致：**名字压根不存在**时走的是
> `NoViableChannel`（415 / TA-2003），不是 404。那是本仓既有语义，
> 不在这一轮范围内 —— 记在这里免得下一个人以为是这次改坏的。

## 7. 兼容性（为什么删字段不会炸老库）

`PolicyAction` 与 `ChannelGroup` 都有
`#[serde(flatten, default)] pub extra: Map<String, Value>`
（`domain/mod.rs`），且**没有** `deny_unknown_fields`。所以：

* **策略里**老库里 `"selector": "cheapest"` → 被 `PolicyAction.extra` 吸收，**解析不失败**
  （`legacy_action_selector_is_ignored_while_failover_still_overrides` 钉住了这条）；
* **通道组里**的 `selector` 字段**仍然存在**（只是只剩一种合法取值），
  所以老值靠 `#[serde(alias)]` 收下，一律解成 `BalancedSticky`
  （`legacy_selector_spellings_still_deserialize` 覆盖全部 7 种拼写）；
* 反过来，新写的配置里没有那个键，老版本读也不炸。

**代价（必须说清）**：被 `extra` 吸收的旧键**会在写回时原样保留**，所以数据库里可能长期
留着 `action.selector` 这个已无意义的键。这是"迁移只增不改"（`docs/11 §4.1`）的既定代价，
不是 bug。

> 实测：用户**真实库**里 `channel_group` 与 `policy` 两张表的行数都是 **0**，
> 所以这条兼容路径在当下这台机器上根本不会被走到 —— 但代码与用例照样保留，
> 因为别人的库、以及导出的 YAML 里都可能有。

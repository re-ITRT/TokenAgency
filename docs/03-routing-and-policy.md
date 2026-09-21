# 03 · 路由、策略与透传

> ## §0 基准场景（先读这个，后面全是它的实现细节）
>
> **本项目的一切设计都以这个场景为准绳。** 它来自用户的真实需求，不是举例。
>
> ### 场景
>
> 用户有两份**套餐**（tokenplan），都提供**同一个模型** `deepseek-v4.1flash`：
>
> | 套餐 | 厂商 | 网络 | 额度 |
> |---|---|---|---|
> | A | 字节方舟 | **直连**（国内可达，不需要梯子） | 500 万 token/月，1 号重置 |
> | B | command code | **走梯子**（国外，必须经代理） | 按量计费 |
>
> **期望行为**：平时用 A 走直连；A 的额度用完之后**自动**切到 B 走梯子；同一次对话不要来回跳。
>
> ### 为什么这个场景决定了整个架构
>
> 它同时压住了三条设计主线，任何一条做错都不成立：
>
> | 主线 | 这个场景的要求 | 对应章节 |
> |---|---|---|
> | **出网方式属于"套餐"而非"模型"** | A 走直连、B 走梯子，同一个模型两种走法 | §3（通道）、[04 §8](04-network-core.md)（线路分配） |
> | **切换必须自动且带粘性** | 用完自动切，且同一会话不来回跳 | §3.3（粘性）、§4（额度账本） |
> | **路由层与网络层必须同一处决策** | 切套餐时线路**跟着**切，不能"切了套餐还走原来那条线" | §2 + [09 §5.1](09-architecture-design.md) 管线第 8 步 |
>
> 第三条是用户对 TokenHub 的核心抱怨 —— **它的路由层与网络层互不知道彼此**，所以"挂了梯子痛点照旧"。
>
> ### 术语（两种"订阅"必须分清）
>
> | 说法 | 实体 | 是什么 | 本场景对应 |
> |---|---|---|---|
> | **机场订阅** | `proxy_subscriptions` | 一堆**节点**的来源（Clash 格式） | 用户买的那个机场 |
> | **套餐 / tokenplan** | `credentials` | 一份 **API 额度**（key + 额度 + 重置周期） | 字节方舟、command code |
>
> **出网方式挂在"套餐"上**（`credential.lane`），lane 再指向机场订阅里的某个组/节点、或直连。
> 早期文档把两者都叫"订阅"，是最容易读错的地方，现已统一。
>
> ### 配置（就三处，其余全省略）
>
> ```yaml
> credentials:
>   - id: c-bytedance
>     provider: p-bytedance
>     label: 字节 · 500万 token/月
>     secret: "${secret:bytedance}"
>     lane: lane-direct              # ★ A 走直连
>     quota: { metric: tokens, limit: 5000000, window: month, reset_day: 1,
>              soft_limit: 0.8, hard_limit: 0.98 }
>
>   - id: c-commandcode
>     provider: p-commandcode
>     label: command code · 按量
>     secret: "${secret:commandcode}"
>     lane: lane-jp                  # ★ B 走梯子
>     quota: { metric: cost, limit: 20, currency: CNY, window: month }
>
> channel_groups:
>   - model: deepseek-v4.1flash
>     members: [ch-bytedance, ch-commandcode]   # 顺序即优先级：先用 A
>     selector: sticky_then_priority
>     sticky: { enabled: true, ttl_h: 12 }
>     release_on: [quota_exhausted]
> ```
>
> 通道上**不必再写 lane** —— 它继承 `credential.lane`（五级继承见 [04 §8.1](04-network-core.md)）。
> 想给某个通道单独改线路时才在通道上写，那叫覆盖。
>
> ### 运行时（这是验收时要能逐步复现的）
>
> ```
> 请求 → 选通道 ch-bytedance → 该通道的 lane = lane-direct → 直连到字节
> 请求 → 粘性命中 → 仍是 ch-bytedance → 仍走直连
>   ⋮
> 账本累计到 500 万 → c-bytedance 标记 exhausted
> 请求 → 候选剔除字节 → 选 ch-commandcode → 它的 lane = lane-jp → 经日本节点出去
> 请求 → 粘性已重写 → 继续 command code（不会来回跳）
>   ⋮
> 下月 1 号 → 窗口滚动 → 字节复活 → 新会话重新优先字节
> ```
>
> **管线顺序是关键**：先选出通道（步骤 5–7），**再**由通道决定 lane（步骤 8）。
> 所以"切套餐"和"切线路"物理上就是同一个动作，不可能脱节。
>
> ### 这个场景的两个前提（做不到就退化成手动）
>
> | 前提 | 说明 |
> |---|---|
> | **A 的额度必须可知** | 字节的 API 不一定在响应头给剩余量，主要靠"用户在套餐上填 `limit` + 本地累计"。**不填 limit 就无法自动切换** —— 所以 UI 必须在套餐缺额度配置时**显式提醒**，不能静默 |
> | **直连/走梯子目前手配** | 本场景两种走法很明确，手配没问题。5 个套餐里 3 个能直连时容易漏 → 已进 backlog：lane 增加 `auto_reachable`（先试直连，失败自动走代理并缓存结果） |

---

本章是产品的"大脑"。核心目标：**同一个逻辑模型挂了多个 tokenplan 时，自动、有粘性、可解释地挑一个用，用完换下一个，并且不改变请求语义。**

---

## 1. 路由只需要读几个字段

**注意：这一步只为"选目的地"服务，不会修改请求。** 请求怎么发出去由 §7 的全透传规则决定。

```rust
fn route_hints(body: &Value) -> Hints {
    Hints {
        has_image:       scan_content_parts(body, &["image_url", "image", "inline_data"]),
        stream:          body["stream"] == true,
        has_tools:       !body["tools"].is_null(),
        est_input_tokens: estimate_tokens(body),   // 图片按 ~765/张粗估
    }
}
```

> `scan_content_parts` 遍历的是 `serde_json::Value`，**不是强类型结构体**——所以客户端用了我们没见过的新 part 类型也不会丢，也不影响透传。识别不出来最多是 `has_image=false`，请求照发。

## 2. 策略求值

```
for policy in policies.filter(enabled).sort_by(priority):
    if policy.matches(req, ctx):        # ctx 含账本、并发、时间、健康状态
        return apply(policy, req)
return builtin_default
```

**首个命中即生效，不做叠加**（除了 `exclude_channels` / `exclude_tag` 这类可累加的收窄动作在单条规则内可组合）。这样行为可预测、可解释、可模拟。

求值结果：

```rust
struct RouteDecision {
    candidates: Vec<ChannelId>,     // 有序候选（已应用 filter/exclude）
    selector: Selector,
    failover: FailoverPolicy,
    on_no_candidate: NoCandidateAction,
    matched_policy: PolicyId,
    eliminated: Vec<(ChannelId, Reason)>,   // 淘汰原因，进 trace 与模拟器
}
```

## 3. 通道组与选择器（解决"用完一个换下一个 + 粘性"）

### 3.1 选择流程

```
1. 若 group.sticky.enabled:
     key = derive_sticky_key(req)
     hit = sticky.get(key)
     if hit 且 hit.channel 在候选集内 且 channel 状态可用:
         → 用它（sticky_hit=true）
2. 未命中 → 按 selector 排序候选 → 取第一个可用
3. 写/更新 sticky(key, group, channel, ttl)
```

### 3.2 粘性 key 派生（按顺序取第一个命中）

| 来源 | 说明 | 稳定性 |
|---|---|---|
| `header:x-session-id` | 客户端显式会话 ID | 最理想 |
| `header:x-conversation-id` | Claude Code / 部分客户端会发 | 好 |
| `body.metadata.user_id` | OpenAI `user` / metadata 字段 | 好 |
| `body_hash` | 首条 system + 首条 user 内容的哈希（取前 16 位） | 兜底，聊天天生一致 |
| `fallback: client_ip + ua` | 最后兜底 | 粗 |

`body_hash` 是最实用的兜底：同一段对话的首轮内容相同，后续轮次因为 messages 增长会变——因此**实际实现取"首条 system message 的哈希 + 首条 user message 的前 512 字符哈希"**，保证同一会话多轮稳定命中。

### 3.3 粘性释放（release_on）

| 触发 | 行为 |
|---|---|
| `quota_exhausted` | 立即释放，重选下一个候选，并**同会话后续请求继续用新通道**（重写粘性） |
| `hard_error` | 连续 N 次（默认 3）硬失败 → 释放 |
| `cooldown` | 通道进入冷却 → 临时释放（冷却结束后可再粘回） |
| `user_pin_change` | 用户在托盘手动 pin 了别的通道 → 全局重写 |
| `ttl` 到期 | 12h 后自然过期 |

### 3.4 为什么粘性重要（不只是"方便"）

- **Prompt Cache 命中**：OpenAI/Anthropic 的缓存按"账号 + 前缀"生效，换 key 就全 miss，成本和延迟都涨。粘性直接省钱。
- **额度连贯**：不粘性会导致多个账号都被小额消耗，每个都"快用完但没用完"，反而更难管理。
- **可复现**：出问题时能知道"这个会话到底走的哪家"。

## 4. 额度账本（"一个额度用完了再用第二个"的实现基础）

### 4.1 四个观测源，取最保守值

| 源 | 精度 | 说明 |
|---|---|---|
| **A. 上游响应头** | 高（若提供） | OpenAI: `x-ratelimit-remaining-tokens`、`x-ratelimit-reset-tokens`；Anthropic: `anthropic-ratelimit-*`；解析后写入 `quota_state.reported_*` |
| **B. 本地计量** | 中 | 输入按 tokenizer 估算（图片按块折算），输出取响应 `usage`（流式需 `stream_options.include_usage`，或从末尾 chunk 解析） |
| **C. 手工额度** | 由用户保证 | 用户填 `limit/window/reset_day`，最贴合"套餐"语义 |
| **D. 状态码语义** | 高（事后） | `429`+`insufficient_quota`、`402`、部分中转的 `403 quota exceeded` → 直接标记 `exhausted` 并记录 reset 提示 |

合成规则：`remaining = min(手工剩余, 响应头剩余, 1 - 本地累计/limit)`。任一源说"没了"就按没了处理（保守优先，避免撞墙）。

### 4.2 状态机

```
unknown ──首次观测──> ok ──usage>soft_limit──> soft ──usage>hard_limit──> exhausted
                       ↑                                              │
                       └──────── window 重置 (reset_day/时间窗) ───────┘
                       └──────── 用户手动"我充值了/清零" ──────────────┘
```

- `soft`：托盘图标变黄，系统通知"中转站A 已用 82%"，但仍可路由（除非策略规定 `quota_remaining_lt: 0.2` 就绕开）。
- `exhausted`：从候选集中剔除，记录到 trace `eliminated` 原因 `quota_exhausted`，触发粘性重选。
- 恢复：窗口滚动（每天检查 `reset_day`/`window`）、用户手改、或收到成功响应且响应头显示 remaining 回升（说明上游重置了）→ 自动复活。

### 4.3 并发保护

`guard.max_rpm` / `max_concurrency` 用令牌桶 + 信号量实现，超限时行为可选：`wait`（排队，默认，最多 30s）或 `next_candidate`（直接换下一个通道）。

## 5. 故障转移

### 5.1 触发条件映射

| 条件 | 判定 | 是否重试 |
|---|---|---|
| `http_429` | 状态码 429 | 是（先看 `Retry-After`；> 5s 则换通道而非等待） |
| `http_402` / `insufficient_quota` | 语义解析 | 是，且标记该凭据 exhausted |
| `http_401/403` | key 失效或被封 | 是，且把该凭据标 `invalid` 并通知 |
| `http_5xx` | 上游故障 | 是（5xx 立即换） |
| `timeout` | connect/read 超时 | 是（connect 超时必换；read 超时看是否已输出） |
| `connect_error` | TLS/连接建立失败（常见于节点被封） | 是，且给该 **Lane** 记一次失败（连续 3 次 → lane 降级） |
| `quota_exhausted` | 账本判定 | 是（换凭据） |
| `channel_tagged_out` | 该通道被策略按标签排除（如 `no-vision`） | 是 |

### 5.2 重试边界（再强调一次）

```
if already_wrote_downstream_bytes {   // 已向客户端输出
    record(stream_broken); return;    // 绝不重试
}
if attempt >= max_attempts { ... }
```

- 流式请求额外规则：**首字节前**允许重试；`max_attempts` 对流式默认降到 2（因为客户端等太久体验差）。
- 幂等性提示：LLM 请求理论上有副作用（计费），重试会重复计费。因此每次重试前的失败必须**确认未产生上游计费**——4xx/5xx 状态码与 connect 失败都满足；read 超时则不确定，UI 在 trace 里标注 `possible_double_billing`。

### 5.3 冷却与退避

- 单次失败 → `cooling`，`cooldown` 默认 120s（指数退避 120s → 300s → 900s，上限 1h）。
- Lane 级失败连续 3 次 → lane 降级（托盘显示），但**只影响使用该 lane 的通道**，其它 lane 不受牵连（这就是"按模型配线路"的价值）。
- 成功一次 → 冷却计时器清零。

## 6. 视觉问题：只需要全透传 + 一个可选标签

先分清两件事——上一版把这两件事混成了一大套子系统（能力库 / 探测 / 学习），是写重了：

| 问题 | 是"改请求"吗 | 手段 |
|---|---|---|
| 请求里的图片块 / 未知字段被中转层吃掉 | **是** | **全透传**（§7）。不需要任何能力机制 |
| 该把带图的请求**发给谁**（A 家识图、B 家不识图） | 否，是**选目的地** | 一个可选的通道标签 |

### 6.1 通道标签（全部机制就这一个）

- Channel 上一个三态标签：`vision` / `no-vision` / **未标注（默认）**。
- 未标注 = 照发。默认永远是"发出去"，**不是**"查不到能力就拒绝"。
- 用户哪天发现某家中转的 `gpt-4o` 不识图，在 UI 点一下标 `no-vision`，之后带图请求会跳过它。
- 对应策略规则（默认关闭）：

```yaml
- id: pol-skip-nonvision
  name: "带图请求跳过不识图的通道"
  enabled: false
  match: { has_image: true }
  action: { exclude_tag: no-vision }
```

### 6.2 明确不做的三件事

| 不做 | 为什么 |
|---|---|
| 内置模型能力库 | 会过期，而且和"全透传"重复——透传已经不丢图片了 |
| 主动探测（发测试图） | 会误判：上游返回 200 但把图片当纯文本忽略，探测不出来 |
| 自动学习降级 | 一次偶发 4xx 就把好通道标坏，比不标更糟 |

"谁识图"这件事用户一句话就能定，比任何启发式都准。

### 6.3 跨协议转换时的图片

这是唯一需要真判断的场景：客户端发 OpenAI 格式、上游是 Anthropic 格式，图片块需要换写法。

- 有明确对应关系 → 正常转换（`image_url` (data URL) ↔ `source: {type: base64, ...}`，`detail` ↔ 忽略）。
- **无法映射 → 按 `on_no_candidate` 处理（默认直接报 415），绝不把图片悄悄替换成 `"[image]"` 文本。** 静默文本化才是把你"干成瞎子"的元凶。
- `/v1/models` 默认**原样透传**，不注入额外字段（注入 `capabilities` 反而可能让某些客户端的严格解析失败）；需要时可以开 `gateway.expose_models_with_capabilities`。

## 7. 全透传（默认，而且基本就是全部答案）

你问"直接全部透传不就可以了吗"——**对，基本就是**。默认行为一句话：

> 客户端发来的东西，除了下面三个必须的例外，一个字节都不动。

### 7.1 三个必须的例外（不是选择，是协议要求）

| 例外 | 为什么 | 谁决定的 |
|---|---|---|
| `host`、`content-length` | 转发必须重算 | HTTP 协议 |
| `authorization` / `x-api-key` / `api-key` | 换成目标通道的 key | 多套餐复用的前提 |
| hop-by-hop 头（`connection`、`transfer-encoding`、`upgrade`、`proxy-*`、`keep-alive` 等） | 规范要求剥离，否则协议违规 | HTTP 协议 |

**其余全部原地转发**：`user-agent`、`x-stainless-*`、`anthropic-beta`、`OpenAI-Beta`、`x-session-id`、`referer`、`origin`、各种自定义头、没见过的头，以及 body 里的每一个字段、每一个嵌套层级、每一个未知 part 类型。

### 7.2 实现上的唯一要求（大量中转翻车就翻在这）

**不要用强类型结构体反序列化请求体或头部。**

```rust
// ❌ 这样写，未知字段必然蒸发
#[derive(Deserialize)] struct Message { role: String, content: String }

// ✅ 只做定点 patch
let mut body: Value = serde_json::from_slice(&raw)?;
if let Some(m) = body.get_mut("model") { *m = json!(upstream_model); }
// 其余字段一个都不动
```

头部同理：用 `HeaderMap` 整体转发再做三处修改，而不是"从已知头里挑出我要的重新拼一个"。

```rust
// ❌ 白名单式重建：没列进去的头全没了，agent 标识就是这么被吞的
let mut out = HeaderMap::new();
out.insert(USER_AGENT, "TokenAgency/1.0".parse()?);   // 还把自己的 UA 写进去了
```

### 7.3 唯一会真正改内容的场景：跨协议

客户端发 OpenAI 格式、上游是 Anthropic 格式时才转换。转换后做一次核对：

```rust
fn verify_no_loss(src: &CanonRequest, encoded: &Value) -> Vec<Loss> {
    // 1. part 数量与类型序列是否一致
    // 2. 顶层未知字段是否保留在 extra 中并回写
    // 3. 哪些字段因目标协议不支持而无法映射
}
```

有 loss 时默认**直接 4xx 拒绝**（`on_loss: reject`），而不是转发一个残缺请求。同协议转发永远是零改动、零核对。

### 7.4 自检，不是策略

网关会记录"客户端发出的头"和"上游收到的头"的差异，写进 `trace.header_diff`。**非空就说明网关自己出了 bug**，是自检告警，不是可配置的策略。默认**不会因为差异而拒绝请求**。

## 8. 策略模拟器（UI 杀手功能）

输入：

```json
{ "model": "gpt-4o", "has_image": true, "stream": true,
  "headers": { "x-session-id": "abc" }, "est_input_tokens": 1500 }
```

输出：

```
命中规则: pol-skip-nonvision (priority 5)   ← 该规则默认关闭，此处为已开启示例
候选(3): ch-openai-personal ✅ → ch-relay-a ✅ → ch-relay-b ❌(tag=no-vision)
选择结果: ch-relay-a  (sticky 命中: key=abc, 12h 内)
故障转移链: ch-relay-a → ch-openai-personal
预计出口: lane-jp → 节点「东京-03」(延迟 82ms)
预计发出的请求: 与客户端原始请求逐字段一致（仅 model 与鉴权头不同）
```

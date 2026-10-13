# docs/47：额度耗尽时各家返回什么（调研）

> 起因：用户问「各家在额度耗尽之后会返回什么」。
>
> **与姊妹文档的分工**（别搞混，两篇问的是两件事）：
> * 本篇（`docs/47`）：额度**耗尽时会返回什么错误** —— 给"要不要换通道"的判据用；
> * `docs/llm-api-quota-research.md`（第 312 轮）：**怎么主动查**剩余额度/花费 ——
>   给"能不能自动读余额"用。那篇的结论是"光有普通 API key 基本查不到余额"。
>
> 两篇有一处互相印证：第 312 轮独立记到"Gemini 超额返回 `429 RESOURCE_EXHAUSTED`"，
> 与本篇 §2 的 Gemini 行一致。
>
> 为什么这个问题对我们有实际影响：网关要判断"这个通道现在还能不能用"。
> 目前唯一的判据是 `crates/ta-gateway/src/dispatch.rs`：

```rust
pub fn is_failover_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::PAYMENT_REQUIRED
        || status.is_server_error()
}
```

**只看状态码**（429 / 402 / 5xx）。这份调研要回答的就是：这个判据够不够。

---

## 1. 一句话结论

**不够。** 额度耗尽的返回形状分三类，其中两类现在的判据处理不了：

| 形状 | 含义 | 判据够吗 |
|---|---|---|
| **A. 状态码自己分得开** | 额度耗尽 ≠ 限流的状态码（如 402 vs 429） | ✅ 够 |
| **B. 同码不同义** | 额度与限流**都是 429**，只能读 body 的错误码 | ⚠️ 能换通道，但**分不清该等还是该弃** |
| **C. 藏在 4xx（尤其 400）里** | 额度耗尽返回 **400** | ❌ **完全不换通道**（现在的漏洞） |

C 类是最要紧的漏洞：**Anthropic、火山方舟（用户自己在用）、Groq 的额度耗尽都走 400**，
而 `is_failover_status(400) == false` —— 额度没了的通道不会被跳过，客户端会持续收到
一个看起来像"请求写错了"的 400。

**400 这类比我一开始估计的普遍得多**（四条证据里有一条是我亲自抓的官方原文）：

| 谁 | 证据 | 谁核的 |
|---|---|---|
| Anthropic（预付余额） | 官方 SDK issue #618 + 官方 claude-code issue #91322 | 我（两个 issue 都打开读了） |
| 火山方舟（无/过期 CodingPlan） | `openclaw` PR #91390 + `CodexBar` issue #2610（两处**独立**一致） | 我 |
| **Groq（月消费上限）** | **官方文档原文**："will return a **400** with code `blocked_api_access`" | 我（抓的全文） |
| 阿里云百炼（欠费） | 官方文档 `Arrearage` | 工作流核验 |

---

## 2. 主表

`额度耗尽` = 账户余额/套餐额度真的没了；`限流` = 只是 RPM/TPM 打满、账户有钱。

| 服务商 | 额度耗尽 | 限流 | 能靠状态码分？ | 判别字段 | 证据等级 |
|---|---|---|---|---|---|
| **DeepSeek** | **402** `Insufficient Balance` | **429** `Rate Limit Reached` | ✅ 能 | — | 官方文档 + **本轮实测** |
| **Google Gemini（AI Studio 预付）** | **402** `payment_required` | 429 `rate_limit_exceeded` | ✅ 能 | — | 官方文档 |
| **Cohere** | **402**（billing limit） | **429** | ✅ 能 | — | 官方文档 |
| **Anthropic（预付余额）** | **400** `invalid_request_error` | 429 `rate_limit_error` | ✅ 能（但**是 400**） | — | 官方 SDK issue + 官方 issue |
| **火山方舟（无/过期 CodingPlan）** | **400** `InvalidSubscription` | — | — | message | 社区（两处独立一致） |
| **Anthropic（组织月消费上限）** | **429** `rate_limit_error` | **429** `rate_limit_error` | ❌ | `error.details.error_code` = `enforced_spend_limit_reached`，且**无 `retry-after`** | 官方文档（仅工作流核验） |
| **OpenAI** | **429** `code`/`type` ∈ `insufficient_quota` / `credit_balance_exhausted` / `organization_spend_limit_exceeded` / `project_spend_limit_exceeded` / `organization_usage_limit_exceeded` | **429** `type=rate_limit_error`, `code` ∈ `rate_limit_exceeded` / `slow_down` | ❌ | `error.code`（或 `error.type`） | 官方文档 + 官方仓库代码 |
| **智谱 GLM** | **429** `error.code=1113` | **429** `error.code=1302`（另 1305/1308–1321/1313… 全是 429） | ❌ | `error.code` | 官方文档 |
| **Kimi / Moonshot** | **429** `error.type=exceeded_current_quota_error` | **429** `error.type=rate_limit_reached_error` | ❌ | `error.type`（**没有 code 字段**） | 官方文档 |
| **阿里云百炼（预算用尽）** | **429** `BudgetLimitExceeded` | **429** `Throttling.RateQuota` 等 | ❌ | `code` | 官方文档 |
| **阿里云百炼（欠费）** | **400** `Arrearage` | — | ✅ 能（但**是 400**） | `code` | 官方文档 |
| **百度千帆（套餐额度）** | **429** `coding_plan_hour/week/month_quota_exceeded` | **429** `rpm_rate_limit_exceeded` | ❌ | `code` | 官方文档 |
| **百度千帆（欠费）** | **403** `account_overdue` | — | ✅ 能 | `code` | 官方文档 |
| **火山方舟（欠费）** | **403** `AccountOverdueError` | — | ✅ 能 | `code` | 官方表（仅工作流核验） |
| **火山方舟（5h/周/月额度）** | **429** `QuotaExceeded` | **429** `AccountRateLimitExceeded` / `RateLimitExceeded.EndpointRPM/TPMExceeded` | ❌ | `code` | 官方表（仅工作流核验） |
| **xAI Grok** | **403** 或 **429**（两种都见过） | **429** | ❌（code 相同） | 只能匹配 `error` 文本 | 社区 |
| **Azure OpenAI** | **429**，`error.code` 就是字符串 `"429"` | **429**，同样 `"429"` | ❌ | 只能看 message / 响应头 | 官方文档 |
| **Gemini（免费层/日额度）** | **429** `RESOURCE_EXHAUSTED` / `quota_exceeded` | **429** `rate_limit_exceeded` | ❌ | `details[].violations[].quotaId`，或新 API 的 `error.code` | 官方文档 |
| **Vertex AI** | **429** `RESOURCE_EXHAUSTED` | 同 | ❌（官方明说不区分） | — | 官方文档 |
| **OpenRouter** | **402** `payment_required` | **429** `rate_limit_exceeded` | ✅ 能 | — | 官方文档（已抓全文） |
| **小米 MiMo** | **402**（现金余额）/ **429**（Token 套餐额度） | **429** | ⚠️ 部分 | 看 message；无稳定 code | 官方文档 |
| **美团 LongCat** | **402** 或 **403** `insufficient_quota` | **429** `rate_limit_exceeded` | ❌（官方自相矛盾） | `error.code` | 官方文档 |
| **商汤 SenseNova** | **未记载**（很可能没有独立状态） | **429** `429001` | ❌ | `code`（**别信 `type`**） | 社区 + 官方 SDK |
| **零一万物 01.AI** | **未知**（文档里没有额度类错误） | **429** | 未知 | — | 官方文档 |
| **Groq（月消费上限）** | **400** `blocked_api_access` | **429**；另有自定义 **498** | ❌（**是 400**） | `error.code` | 官方文档（已抓全文） |
| **Together AI** | **402**（月度消费上限） | **429**（限流**或** GPU 配额） | ✅ 能 | — | 官方文档（已抓全文） |
| **Fireworks** | **402**（"not on a paid plan **or has exceeded usage limits**"） | **429** | ✅ 能 | — | 官方文档（已抓全文） |
| **DeepInfra** | **未记载**（社区常见 402） | **429** `Rate limited` | 未知 | — | 官方文档（仅 429 有记载） |

> **额外一行（聚合器特有）**：OpenRouter 对**上游**错误做了归一化，把 Anthropic 皮肤的
> `payment_required` 映射成 **402 `billing_error`**。也就是说**聚合器会把上游的 400 变成 402**
> —— "上游是谁"和"我收到几"是两件事，见 §3.5。

---

## 3. 必须知道的陷阱

### 3.1 `insufficient_quota` **不是**通用"额度耗尽"信号

阿里云百炼在 OpenAI 兼容模式下，**429 的错误码就叫 `insufficient_quota`**，
但官方定义它是 **TPS/TPM 限流**，不是账户欠费（欠费是 400 `Arrearage`）。

也就是说：**"看到 `insufficient_quota` 就当额度耗尽"会误判。** 这个字符串是
OpenAI 的命名，别家不一定同义 —— 判据必须**按厂商**，不能按字符串全局匹配。

（`docs/03-routing-and-policy.md` §5.1 现在写的正是
`http_402 / insufficient_quota | 语义解析`，这条需要按上面的结论修正。）

### 3.2 对额度错误**重试是没用的**

OpenAI 官方帮助文档原话：

> Retrying a billing, spending, or quota error does not restore access.
> Address the reported balance or limit first.

这是本调研里对**冷却时长**最有价值的一句：限流值得退避重试，**额度耗尽不值得**。
两者现在被我们一视同仁（都进 `cooldown_s` 退避），结果是反复去撞一个没钱的账号。

Anthropic 官方文档也给了同向的信号：额度上限那个 429 **不带 `retry-after`**
（限流那个带）—— "没有 `retry-after`" 本身就是"别等"的提示。

### 3.3 有厂商用 403 表额度耗尽/欠费

xAI（额度）、火山方舟（欠费）、百度千帆（欠费）都用 **403**。
我们的 `is_failover_status` 不含 403，`FailoverTrigger` 配置枚举里也没有 `http403`。

### 3.4 反向陷阱：DeepSeek 的 402 里，`code` 是个**通用**值

这条我亲自核过（`continuedev/continue#4766` 的标题就是原始报错）：

```
HTTP 402 Payment Required from https://api.deepseek.com/chat/completions
{"error":{"message":"Insufficient Balance","type":"unknown_error",
          "param":null,"code":"invalid_request_error"}}
```

**`code` 是 `invalid_request_error`，`type` 是 `unknown_error`** —— 两个字段都在说
"这是个请求格式问题"，而实际上它是"你没钱了"。

所以：**"读 body 的 code 来分类"这个想法，天真地实现会直接砸掉 DeepSeek。**
如果按 `code` 匹配，DeepSeek 的额度耗尽会被归成"客户端请求错误 → 不换通道、
原样透传"，比现在的状态码判据**更糟**。

结论：判据要**按厂商**分别写（或至少"状态码优先、body 只做去歧义"），
不存在一个跨厂商通用的 body 字段。

### 3.5 聚合器会改写上游的错误形状

这是给我自己（我们就是个反代）的重要提醒。OpenRouter 官方文档写得很清楚：

> OpenRouter normalizes every upstream provider error into the stable, typed
> `error_type` vocabulary.

并且它给了映射表：上游 Anthropic 皮肤里 `payment_required` → **402 `billing_error`**。
换句话说：**同一个"没钱了"，直连 Anthropic 是 400，经 OpenRouter 变成 402。**

对我们的含义：**"我收到几"取决于"我直连谁"**，不能拿"某家官方文档写 402"
去推断"我打到那家的接口就会收到 402"。判据必须对着**实际配置的那个 base_url** 来。

### 3.6 最毒的一条：`429` + 正文里的 "exhausted" 可能只是**限流**

这条推翻了一个非常自然、也非常错的想法 —— "正文里出现 `exhausted` / `exhausted_quota`
就当额度耗尽"：

| 服务商 | 正文原文 | **实际含义** |
|---|---|---|
| **商汤 SenseNova** | `{"message":"inference tpm exhausted","type":"invalid_request_error","code":"429001"}` | **TPM 限流**（每分钟 token 数） |
| **小米 MiMo** | 官方文档原话：429 的成因是 "Requests are too frequent, **OR THE QUOTA OF TOKEN PLAN HAS BEEN EXHAUSTED**" | **两者混在一个状态码里** |

SenseNova 那条还是从它**自家第一方端点**抓到的实测。按 "exhausted" 做字符串匹配，
会把一次纯 TPM 限流误判成"这个账号没钱了"→ 长冷却 + 换通道，**正好把可用通道踢掉**。

顺带两个 SenseNova 的坑：
* `type` 字段在 429 上写的是 **`invalid_request_error`**（一个 429 却自称"请求无效"）——
  **`type` 不可信**，只能认 HTTP 状态码 + 数字 `code`；
* 官方文档与官方 Python SDK **都没有**"余额/欠费"这个错误类别（只有 7→403 无权限、
  8→429 超限）。所以"额度耗尽"在 SenseNova 可能**根本没有独立形状**。

MiMo 的分裂也值得单独记：**402 = 现金余额**，**429 = Token 套餐额度**，
而 429 同时也是普通限流 —— 也就是说**对套餐用户，状态码分不开**。

**教训**：判据里**不要出现任何 `exhausted` 之类的词面匹配**。
要么按厂商精确匹配机器码，要么按状态码；词面匹配看起来"更聪明"，实际是负收益。

### 3.7 LongCat 的官方文档**自相矛盾**

美团 LongCat 是这四家里唯一给了干净判别码的（`insufficient_quota` vs
`rate_limit_exceeded`），但它**自己的两张表打架**：

* 状态码表：`402 Payment Required — Insufficient token quota`
* 错误码表：`permission_error / insufficient_quota / **403**`

同一个"额度不足"，一张表写 402、另一张写 403。**按 body 的 code 判、两个状态码都收**，
这也是"别只信状态码"的又一个理由。

另外 LongCat 的 429 正文里带 `retry_after: 60` —— 这正是我们**该读却没读**的字段（§4 缺口 4）。

### 3.8 **Together AI 的 403 是"输入太长"，不是鉴权**

Together 官方错误表原文：

* `402 Payment Required` — 月度消费上限
* `403 Bad Request` — **"Input token count + `max_tokens` must be less than the context
  length"**（上下文超长！）
* `429` — 限流，**或**专属部署的 GPU 配额

也就是说：**403 在 Together 是"请求本身太大"，一个纯客户端问题。**
如果我们哪天按"403 = key 失效/额度"去处理，会把一个"把 prompt 缩短"的问题
误判成"换通道" —— 换了也没用，每次都还是 403，白白绕一圈。

这条与 §3.3（xAI/火山/百度用 403 表额度）**直接冲突**：**同一个 403，在不同家含义相反。**
再次印证"按厂商"是唯一可行的口径。

Groq 还提供了一个少见的状态码：自定义 **498 Flex Tier Capacity Exceeded** ——
又一类"不是标准的、但真实存在"的返回，这类值我们在透传时必须能原样带出去。

### 3.9 流式：错误可能藏在 200 里

OpenRouter 文档中有一句我们特别该注意：

> A streaming request receives an SSE error event (...) **The HTTP status remains
> `200 OK`, because the headers were sent before the error occurred.**

一旦开始流式输出，HTTP 状态码就**定死成 200** 了，额度耗尽只能以 body / SSE 事件的形式到达。

好消息：我们的规矩本来就是"**已经向客户端写过字节就绝不重试**"（`docs/03 §5.2`），
这条继续成立、也不需要改 —— 因为那时换通道已经没意义（客户端手里已经有半截答案）。
坏消息：**"看状态码决定要不要换通道"这套判据，对流式响应天然只覆盖首字节之前**。
这一点要在文档里说清，免得以后有人以为"流式也能靠 402/429 换通道"。

---

## 4. 对代码的影响（现状 vs 应有）

| # | 缺口 | 现状 | 后果 |
|---|---|---|---|
| 1 | **400 类完全不换通道** | `is_failover_status` 不含 400 | Anthropic / 火山 / 阿里欠费 / Gemini 的额度耗尽**不会被跳过**，客户端持续收到像"参数写错"的 400 |
| 2 | 403 类不换通道 | 同上 | xAI 额度耗尽、火山/百度欠费不换通道 |
| 3 | 429 内部语义不区分 | 一律换通道 + 同一条退避 | 分不清"等 10 秒能好"（限流）与"等到天亮也没用"（额度），会反复撞死账号 |
| 4 | 不读 `retry-after` | 未实现 | `docs/03 §5.1` 承诺的"先看 `Retry-After`；> 5s 则换通道而非等待"没落地 |
| 5 | `docs/03 §5.1` 与实现不一致 | 文档写 `http_401/403 → 是（重试）+ 标 invalid` | 实现里 401/403 **不**换通道，配置枚举也没有 `http401`/`http403` |

**注意**：缺口 1/2 是**功能性**问题（该换的没换）；缺口 3/4 是**策略质量**问题
（换了但换得不聪明）。如果只修一处，修缺口 1。

> 本文件**只做调研结论，不改代码**。

### 后续（用户已裁决，记在这里免得这份文档被单独引用）

用户看过上面的结论后**没有**选择"按厂商写判据"，理由是这份调研本身证明了那件事很脆
（同码不同义、有的藏在 400、有的词面骗人）。用户改走**手动路线**：

> 「既然分码也分不了，那么久只能麻烦用户手动切换了。」

于是：**路由收敛为唯一一种「平均粘性」**，模型与每条线路各给一个开关，
由用户手动调度；引擎只负责"按会话粘住 + 往用得少的线路偏"。
落地见 [docs/48](48-balanced-sticky-only-routing.md)。

**这份文档的结论因此不是"待实现的规格"，而是"为什么不做自动判断"的依据。**
上面 §4 那张缺口表（尤其"400 类完全不换通道"）**仍然成立且未被修** ——
它们是已知的、被有意接受的现状：**用户宁可手动关掉一条线路，也不信任引擎替他猜**。

---

## 5. 证据等级说明（哪些我自己核过）

我（Lead）**亲自打开并逐字确认**的：
* DeepSeek 官方错误码表（402 Insufficient Balance / 429 Rate Limit Reached）
* OpenAI 官方帮助文档（"A 429 response can indicate…"、"Retrying a billing…does not restore access"、四张 billing 码表）
* `openai/codex` 仓库 PR #44492 的**代码与测试**（额度码与限流码的完整集合）
* `anthropics/anthropic-sdk-typescript` issue #618（400 + "Your credit balance is too low…"）
* `anthropics/claude-code` issue #91322（另一个 400 实测）
* `openclaw/openclaw` PR #91390（火山 400 `InvalidSubscription`）
* `steipete/CodexBar` issue #2610（火山 400 实测文案，与上一条独立一致）

**仅由工作流核验员声称核过、我本人没能独立复核**的：
* Anthropic 官方 rate-limits 文档里的 `enforced_spend_limit_reached`（该文档站对我的
  fetch 一直 `cross-origin redirect` 到 www.anthropic.com，拿不到正文）；
* 火山方舟官方错误码表里的 `403 AccountOverdueError` / `429 QuotaExceeded` /
  `429 AccountRateLimitExceeded`（我两次抓 CN 页与 BytePlus 镜像都只拿到 JS 壳，
  正文为空；核验员称逐字命中）。

这两处我在上表标了「仅工作流核验」——**它们恰好都影响"要不要改代码"的判断**，
所以真要动手前应当再想办法复核一次（比如换网络环境、或直接用真 key 打一发）。

**明确没查到的**（诚实留空，未编造）：
* 火山方舟错误码表**没有**给出 JSON 响应体示例 → `quota_exhausted_body` 留空
* Gemini AI Studio 402 的逐字响应体（官方只给通用信封）
* 其余未覆盖的家见 §6

**后来自己补抓到的**（见 §6 的方法说明）：Groq、Together AI、Fireworks、DeepInfra
四家的官方错误码表；其中 **Groq 的 `400 blocked_api_access` 是我本人抓到的官方原文**，
它是"400 类"最硬的证据。

---

## 6. 覆盖到什么程度、还差什么

调研过程中 `web_search` 工具**中途挂了**（报的是 DeepSeek 402，见 §7），
后半程只能靠直接抓取。**关键的一个技巧来自一个 subagent**：它发现
**`pwsh` 本身有完整的外网访问**，于是用 `Invoke-WebRequest` 绕过了坏掉的 `web_search`。
沿这条路我又拿到了 4 家（Groq / Together / Fireworks / DeepInfra），
办法是抓各家文档站的 **`llms.txt` + `.md` 镜像**（这些站给 LLM 消费者准备了纯文本版，
不受 JS 渲染影响），以及给 Cloudflare 挡住的站加一个**浏览器 User-Agent**。

### 已经拿到的（本轮补）

| 家 | 抓到的内容 |
|---|---|
| **Groq** | 官方错误码表全文；**Spend Limits 页原文**（400 `blocked_api_access`） |
| **Together AI** | 官方错误码表全文（402 消费上限 / 403 上下文超长 / 429 限流或 GPU 配额） |
| **Fireworks** | 官方推理错误码表全文（402 套餐或超限 / 429 限流） |
| **DeepInfra** | 官方 Rate Limits 页（429 `Rate limited`）；**没有**额度类的官方记载 |
| **小米 MiMo / LongCat / 01.AI / 商汤** | subagent 拿全（见 §2、§3.6、§3.7） |

### 仍然没查到（如实留白）

* **腾讯混元**：抓到了官方 `ChatCompletions` 接口页，但**该接口已标"预下线"**
  （预计 2026-12-21 下线，老控制台 9 月 30 日已下线），官方明确指向
  「OpenAI 兼容接口」（`cloud.tencent.com/document/product/1729/111007`）。
  **错误码表本身没抓到** —— 但这条信息本身有用：**混元现在主推 OpenAI 兼容入口**，
  所以它的错误形状**很可能**就是 OpenAI 那一套（含 429 承载额度耗尽），但我**没有证据**。
* **MiniMax / 硅基流动 / 阶跃星辰**：文档站是纯 JS 渲染的 SPA（`llms.txt` 也不存在），
  抓到的 HTML 里没有正文；MiniMax 还会 302 到营销落地页。
* **DeepInfra 的额度耗尽码**：官方只写了 429；社区常见的 402
  （`You need positive balance to do inference`）**没能在官方文档里找到出处**，故不采信。
* **01.AI**：官方（中英两版）错误表里只有 400/401/404/429/500/529，**没有任何额度类错误**；
  且它的中国大陆端点对无效 key 返回 **410 Gone**，看起来已降级/退役 —— 真机也观测不到。

**这些空白要怎么办**：如果以后真要给这些家写判据，**别照抄上表的规律去猜** ——
直接拿一个真 key 打一发（额度用光的账号最理想）是最快的路。用真 key 打出来的
响应比任何文档都硬，而且顺带能验证"文档写的和线上一致不一致"（Anthropic 就是
文档写 402、线上返回 400 的活例子）。

> **在补齐之前，不要把 §2 当成全集。**

---

## 7. 附：本轮一个巧合的实测

调研进行到一半时，本会话的 **web_search 工具自己**报了这个错：

```
DeepSeek API error (HTTP 402): Insufficient Balance (request_id: dddbb63e-...)
```

这是 DeepSeek 额度耗尽的**第一手实测**（比官方文档更硬），正好印证 §2 第一行：
DeepSeek 的额度耗尽确实走 **402**，且 message 就是 `Insufficient Balance`。

两点附带信息：
1. 用户的 DeepSeek key **当前余额已耗尽**（搜索走的是
   `https://api.deepseek.com/anthropic/v1/messages`）；
2. 那个端点是 **Anthropic 兼容**的，但返回的是 **DeepSeek 自己的 402** ——
   这印证了一件事：**"Anthropic 兼容端点"不代表错误形状也跟着 Anthropic**。
   判据必须按"实际哪家上游"，不能按协议形状推断。

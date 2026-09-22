# 19 · 第 71 轮需求规格：服务商分步向导 / 网络默认直连 / API key 管理

> 用户原话要点（本文档就是它的落地口径）：
> ①服务商添加改成"下一步 → 下一步"：显示名称（不重复）→ 协议 → URL → key；「测试连接」显示延迟但**非必填**；
> 一个「让我先设置网络代理」按钮（有些服务商要挂梯子）；下一步进入**添加模型**：上方已添加模型、下方一键扫描结果，
> 扫描后可单选/全选添加，**页面要能滚动**；已添加模型旁有「设置」图标按钮，hover 提示"查看模型信息"，**只读不能改**，
> 唯一可改的是网络代理（默认继承上一步所选，上一步没选则**直连**）。
> ②网络：**两个向导都关掉**（首启 `/onboarding` + 设置页里的网络/内核那块）；网络代理**默认开启（默认直连）**，
> 不再有"启动"这个动作；订阅刷新后要能看到**完整节点清单**（`subscription_nodes`；需求里的"可按组筛选"
> 后端做不到，见 §2.3 的裁决）；`lane_test` 要么真用，要么不放按钮。
> ③新增 **API key 管理**：指的是"我们网关对外发的访问密钥"（客户端拿它调本机网关）。

---

## 0. 本轮的事实基础（已核实，别再猜）

| 事实 | 出处 |
| --- | --- |
| `subscription_nodes` / `lane_test` **后端已注册且已实现** | `src-tauri/src/commands/net.rs:780` / `:1057`；注册于 `src-tauri/src/commands/mod.rs:235` / `:244` |
| 页面上「本页尚未接入」是**过期说明**（真相：页面没调用） | `src/pages/network/nodes.tsx` / `assignments.tsx` 的文案 |
| `provider_test` / `provider_echo` / `model_import_from_provider` / `model_apply_import` **契约有、Rust 从未注册** | `src-tauri/src/commands/entity.rs:26-43`（读上游响应体需要 `ta-gateway` 的私有 `collect_body`） |
| `api_key_usage(key_id, day, …)` 表**已存在**，但没有任何 key 的存储与命令；网关只认单个 `gateway.local_key` | `crates/ta-store/migrations/0001_init.sql:98-109`；`ta-gateway/src/ingress.rs` 的 `check_local_key` |

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传（只有 4 个例外）。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；页面只调用已注册命令；每步先证明测试会红；共享工作区不留不可编译状态。

---

## 1. 流程 A：服务商分步向导（`/providers`）

### 1.1 步骤 1 —— 服务商信息（`provider-wizard-step1`）

| 控件 | testid | 语义 |
| --- | --- | --- |
| 显示名称 | `pw-name` | 必填；**与现有 provider 的 `name` 及 `id` 去重**（本地即时提示 + 后端引用完整性兜底） |
| 协议 | `pw-protocol` | 必填；下拉，选项来自 `PROTOCOLS`（被 `protocolList.guard` 钉住，不另编一份） |
| Base URL | `pw-baseUrl` | 必填；`^https?://` |
| 密钥 | `pw-key` | 必填；`type=password`，**明文只进密钥仓库**，不进日志/错误/页面。**例外**：本会话已经存过密钥（`pw-test` 落库后明文被抹掉）时留空 = 沿用已存的那把（见 1.3 的裁决） |
| 测试连接 | `pw-test` | **可点、非必填**。真调 `provider_test` |
| 测试结果 | `pw-test-result` | 成功：`延迟 {ms} ms`；失败：后端原文（含 TA 码）。**没有"测试中"的假进度**，只有 pending 禁用态 |
| 先去设置网络代理 | `pw-proxy` | 导航到 `#/network?from=provider-wizard`（见 1.4） |
| 下一步 | `pw-next` | 通过本地校验 → **真正落库**（见 1.3）→ 进入步骤 2 |
| 取消 | `pw-cancel` | 关闭向导；**已落库的步骤 1 结果保留**（诚实：见 1.3） |

**错误码口径（第 71 轮 E5 实现后定稿）**：`provider_test` 探测到上游 **403/404/405**（即"上游没有 `/models` 端点"）
时，命令层用 **`TA-1004`**（400，**不可重试**）—— 复用既有码，**不新增错误码**。
理由：`AllChannelsFailed`（TA-2005 一档）是 `retryable=true`，会让调用方对着一个**不存在**的端点反复重试；
而 403/404/405 是"这个上游没有这个能力"的**确定性结论**，重试没有意义。
注意这与 `provider_test` **成功返回 `ok=false`** 是两件事：前者是命令报错，后者是"探到了、但结论是否定的"
（UI 按 `ProbeResult.probe` 分档说人话）。

### 1.2 步骤 2 —— 添加模型（`provider-wizard-step2`）

页面结构（**整页可滚动**，容器 `pw-step2-scroll`）：

```
┌ 已添加的模型    [ pw-added-models ]
│  pw-model-row × N   ── 每行：id · 上游名 · [pw-model-info]（只读） · [pw-model-lane]（唯一可改）
├ 扫描结果        [ pw-scan ] [ pw-scan-select-all ] [ pw-scan-add ] [ pw-finish ]
│  pw-scan-result → pw-scan-row × N，每行 pw-scan-check + 上游名 + 建议 id（已存在则标"已存在"且默认不勾）
└ 空态 pw-scan-empty / 错误 pw-scan-error（后端原文）/ 进度 pw-scan-pending（只有真 pending 才显示）
```

- `pw-model-info`：**设置图标按钮**，`title="查看模型信息"`；点开只读面板 `pw-model-info-panel`，
  里面**不允许出现任何 input/select/button 之外的可写控件**（唯一的例外是同一行的 `pw-model-lane`）。
- `pw-model-lane`：该模型的网络代理。默认值 = 步骤 1 选中的代理；步骤 1 没选 → `direct`。
  改它要落库（`model_bulk_update` 不支持 lane，所以走**该模型下各 channel 的 `channel_upsert`**？——
  不：**本轮只改"新建该模型时用的 lane"**，即写进新建 channel 的 `lane`；已存在模型的行只读（见 §1.5 裁决）。
- 「一键扫描」= `model_import_from_provider({ provider_id, prefix_filter? })`（**必须真调**，无假进度）。
  扫描结果**只来自后端**，前端不猜、不补。空结果 → `pw-scan-empty` 说"上游没返回任何模型"（如实）。
- 「全选添加」= 勾选所有 `exists=false` 的行；「添加选中」= `model_apply_import({ preview_token, selected })`。

### 1.3 落库时机（**必须先说清，避免"我以为存了"**）

| 时刻 | 写什么 | 命令 |
| --- | --- | --- |
| 点 `pw-next` | 服务商 + 凭据（`<provider>-default`）+ 密钥 | `provider_upsert` → `credential_upsert` → `credential_set_secret` |
| 点 `pw-scan-add` | 选中的模型 | `model_apply_import` |
| 改 `pw-model-lane` | 该模型后续新建 channel 的 `lane`（见 §1.5） | `model_bulk_update`（补 `lane` 字段）|

**取消/关闭向导不会删除步骤 1 已经落库的服务商与凭据** —— 这是有意的（与 `provider_upsert` 的 upsert 语义一致，
取消=停止继续，不是回滚）。UI 必须在步骤 2 顶部用一句说明写清这件事，**不许让用户以为"没保存"**。

#### 1.3.1 落库之后，预检要认识"这一份是本会话自己建的"（第 71 轮真机缺陷 → 裁决）

`pw-test` 是**落库式**的（上表第一行），落库那一刻有两件事同时发生：① 明文从草稿里被抹掉
（§1.1 的纪律）；② `provider_list` 刷新后**自己刚建的那一份**出现在清单里。
如果第 1 步的本地预检（`validateStep1` / `isNameTaken`）只认"清单里有没有这个名字"，
用户按 §1.1 的明文路径「填名称/地址/密钥 → 测试连接 → 下一步」走就会被自己刚建的东西挡住：
同时报「名称已经被占用」+「密钥必填」，`pw-next` 点了没反应，**永远进不了第 2 步**。
（第 71 轮真机 E2E 抓到的原文：`pw-step1-issues: "名称：这个名称已经被占用，换一个 密钥：必填"`；
纯前端单测漏掉它是因为 `gotoStep2` 直接点 `pw-next`、没先点 `pw-test`。）

裁决：

- 预检必须带上"本会话已落库的那一份"（`SavedInSession = { providerId, name, secretStored }`）：
  它自己的 `id` 与 `name` **不算重名**；但改成**别人**的名字照样拦下（不许把重名一起放过去）。
- `secretStored = true` 时 `pw-key` 允许为空 —— 那是"沿用已存的那把"，不是"没填"。
- 本会话再落库一次时**复用同一个 `providerId`**：改名不等于新建一个服务商；否则
  "测试连接之后改个名字再点下一步"会在库里留下一个没人认领的服务商。
- 回归测试：`src/app/providerWizard.test.tsx` 的「点过 pw-test 之后 pw-next 仍然能进第 2 步」
  与同组三条纯函数用例；E2E `step2` 走的正是这条路径。

### 1.4 「让我先设置网络代理」

- 点 `pw-proxy` → `#/network?from=provider-wizard`。
- `/network` 顶部显示返回条 `network-return-banner`（文案：正在为服务商配置网络代理 · [返回向导] `network-return-btn`）。
- 返回时步骤 1 的草稿**保留**（草稿存在页面级 state/URL 之外的模块级 store 里，或向导不卸载 —— 由实现者定，
  但**必须**有一条测试证明"去 /network 再回来，`pw-name`/`pw-baseUrl` 还在"）。
- `/network` 上可选：`network-provider-proxy` 区块，让用户挑一条 lane 作为"新建服务商的默认代理"（写 `net.default_lane`）。

### 1.5 裁决：`pw-model-lane` 到底改什么

> **第 72 轮补充（[docs/20 §1.1/§1.3](20-provider-model-lane-and-edit.md) 为准）**：本节只说了 `model_bulk_update` 要认 `lane`，
> 但当时**没实现**（Rust `ModelPatch` 缺 `lane`、`Model` 缺 `default_lane`），于是"改网络代理"成了**静默空操作**；
> 而且「添加模型」当时只往模型目录里塞行、**没建通道**，所以第 2 步的「已添加的模型」（从 `channel_matrix` 派生）
> 永远是空的。第 72 轮把两件事都补齐：**添加 = 建模型 + 建通道（lane 写进通道）**；
> `ModelPatch.lane` 三态（缺省不动 / `null` 清空 / 值写入）并同步写该模型现有的每个通道。
> **界面上"没显式配"一律显示「直连」，不许显示「未配置」。**

用户说"能改的只有网络代理"。但 `Channel.lane` 是 **channel** 级字段，`Model` 没有 lane。
所以本轮定为：**`pw-model-lane` 写的是"该模型下的 channel 的 lane"**，落库方式是
`model_bulk_update({ ids: [model], patch })` 的扩展 —— patch 增加 `lane?: LaneId | null`，
后端对**该 model 下的每个 channel** 写同一个 lane（没有 channel 则记下来，用于"下一次建 channel 的默认值"，
存在 `Model.default_lane`（新可选字段，`#[serde(default)]`））。
- 未选 = `direct`（契约默认）。
- 契约变更：`Model` 增加 `#[serde(default)] pub default_lane: Option<LaneId>`；`ModelPatch` 增加 `lane`。
- 已存在的模型那一行**可改 lane**（这是用户明确要求的"能改的只有网络代理"）；
  其余字段（上游名、建议 id、优先级…）在面板里**只读**。

---

## 2. 流程 B：网络默认直连 + 真实节点清单

### 2.1 关掉两个向导

1. **首启向导 `/onboarding`**：从路由与所有入口移除（`src/app/routes.ts`、任何"重新运行向导"的按钮）。
   `onboarding_*` 命令**保留注册**（不删后端能力），但**页面不许再引用它们**
   （`commandSurface.guard` 只要求"引用的必须已注册"，不要求全部被引用）。
   同时删掉 `docs/10 §2.2` 的 onboarding 区块说明改为"命令保留、UI 无入口"，并让 `registryClaim.guard` 不误伤。
2. **设置页里的网络/内核向导块**：删除该块（含它的"向导"措辞）；内核路径改由 `/network` 直接编辑
   （`network-kernel-path` + `network-kernel-save`），**不再有"下一步"式向导**。

### 2.2 网络代理默认开启（默认直连）

- 删除 `network-optional` / `network-optional-enable` 这一套（上一轮的"选装入口"作废 —— **这是用户明确改的主意**）。
- `/network` 永远渲染：出口/策略/订阅/线路/节点入口/内核路径。没有配置时**每一块给诚实的空态**，
  但**不再有一个"启用网络代理"的动作**（直连就是默认可用状态）。
- 顶部状态条文案：未配内核路径 → `出口 · 直连`（不写"未启用"，不写错误态、不点红点）。
- `docs/18 §2` 的相关条款标注"已被 [19 §2.2] 取代"，避免两份文档打架。

### 2.3 节点清单（`/network/nodes`）

| 控件 | testid | 语义 |
| --- | --- | --- |
| ~~组筛选~~ | ~~`nodes-group-filter`~~ | **本轮未交付（裁决，不是漏做）**：`subscription_nodes` 的 `group` 参数在后端是**硬错误**（`ta-subscription` 的 `GroupMeta` 只有 `node_count`、不保留组成员关系，见 `net.rs:765-773`）。要真做就得先给 `GroupMeta` 加 `members`（或改走内核 `/proxies` 快照），那是另一个工作包；**宁缺毋滥**，不许放一个换组没反应的下拉 |
| 订阅选择 | `nodes-subscription` | 多个订阅时选一个（只有一个时也显示，不隐藏） |
| 清单 | `nodes-list` / `nodes-row` | 真调 `subscription_nodes({ id, group? })` |
| 空态 | `nodes-empty` | 分两种，文案必须不同：①订阅还没解析过（提示点刷新）②该组下确实没有节点 |
| 错误 | `nodes-error` | 后端原文（TA 码） |
| 刷新 | `nodes-refresh` | 真调（`subscription_refresh` 然后重取 `subscription_nodes`） |

**不许**自己解析订阅内容、不许伪造节点。`NodeView` 形状见 `src/shared/bindings.ts:832`。

**硬前置的数据来源（第 71 轮补齐）**：`last_update_ms == null && error == null` = 这份订阅还没被
解析采纳过，此时**不发** `subscription_nodes`（对没有已采纳版本的订阅发它只会拿到一个错误）。
这两个字段来自 `subscription_list` 的 `SubscriptionView`，而它在第 71 轮之前返回的是**裸
`Subscription`** —— 里面根本没有解析状态，于是真机上的表现是"订阅加好了、刷新也成功了，
页面还是「还没有解析结果」，一个节点都不显示"。
现在解析状态由版本存储的元数据回填（`src-tauri/src/commands/net.rs::views_of_subscriptions`）：
`node_count` / `group_count` / `last_update_ms` / `parser` 取自**最新一版已采纳的版本**，
最新一次尝试被坏更新拦下时给 `error`（此时仍发 `subscription_nodes`，它回的是上一版已采纳的内容）。
**没刷过 = 字段缺失**，不用 `0` / 空串冒充（[docs/18 §1.2] 同一条口径）。

### 2.4 `lane_test`：要么真用，要么不放

线路表每行加 `network-lanes-test`（真调 `lane_test({ id })`），结果显示在 `network-lanes-test-result`：
- 后端返回逐节点延迟 → 如实列出；
- 内核未装配时后端返回错误 → **原样显示**（不吞、不假装成功）；
- 若实现者判断这一步会引入"点了没反应"的风险，**允许不做这个按钮**（宁缺毋滥），但**不许**放禁用占位。

---

## 3. 流程 C：API key 管理（我们网关对外的访问密钥）

### 3.1 存储（迁移 `0004_api_key.sql`）

```sql
CREATE TABLE api_key (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  key_hash     TEXT NOT NULL UNIQUE,   -- sha256(明文)，只存哈希
  key_prefix   TEXT NOT NULL,          -- 展示用：前 12 字符 + "…"
  enabled      INTEGER NOT NULL DEFAULT 1,
  note         TEXT,
  created_at   INTEGER NOT NULL,
  last_used_at INTEGER
);
```

**为什么只存哈希**：与「key 永不进 SQLite」的既有纪律同族；明文只在创建响应里返回**一次**。

### 3.2 命令（新增 5 条，入参一律 snake_case）

| 命令 | 入参 | 返回 | 语义 |
| --- | --- | --- | --- |
| `api_key_list` | — | `ApiKeyView[]` | 含 `id/name/key_prefix/enabled/created_at/last_used_at`，**不含明文、不含 hash** |
| `api_key_create` | `{ name, note? }` | `{ key: ApiKeyView, plaintext: string }` | 明文**只此一次**；`ta-sk-` 前缀 + 32 字节随机（base62） |
| `api_key_set_enabled` | `{ id, enabled }` | `ApiKeyView` | 立刻影响鉴权 |
| `api_key_delete` | `{ id }` | `void` | 立刻影响鉴权 |
| `api_key_usage` | `{ id?, limit }` | `ApiKeyUsageRow[]` | 复用已有 `api_key_usage` 表；id 省略 = 全部 |

### 3.3 网关鉴权（多 key）

**判定顺序（`ta-gateway/src/ingress.rs::check_local_key`）**：
1. 配置里的 `gateway.local_key`（兼容旧配置）匹配 → 通过（`key_id = "local"`）；
2. 否则 `sha256(token)` 在 `api_key` 表里且 `enabled=1` → 通过（`key_id = <id>`，并更新 `last_used_at`）；
3. 都不匹配 → 401 —— **错误码是 `TA-1001`（`tokenagency_unauthorized`，401）**，不是 `TA-1002`
   （`TA-1002` 是 `tokenagency_rate_limited` 429，见 [11 §5](11-internal-contracts.md)：「本机 RPM 超限」）。
   三种失败（不存在 / 已停用 / 格式不对）必须给出**同一个** 401，**不泄漏"哪个 key 不存在"**。

**★ 关键语义：鉴权在什么条件下才启用**（不写死这一条，API key 就会"建了也不生效"）：

| 现状 | 行为 |
| --- | --- |
| `gateway.local_key` 为空 **且** `api_key` 表里没有任何 enabled 的行 | **不鉴权**（保持既有行为，`empty_local_key_means_no_auth` 那条测试继续通过；用户没配任何东西时不该被自己的网关挡在门外） |
| `gateway.local_key` 非空 **或** 表里存在 enabled 的 key | **必须匹配**（本地 key **或** 任一 enabled 的 api key）；否则 401 |

推论（这是可验收的行为，`scripts/e2e` 的 `step18` 会断言）：
- 新建一个 key 之前，无凭据请求**不会** 401；
- 新建之后，**不带** key 的请求 → 401；带上这个 key → 非 401；在界面上**禁用**它 → 立刻恢复 401；
- 删除后同样 401。

用量记账写 `api_key_usage(key_id, day, …)`（表已在，`key_id="local"` 也可记）。

> **⚠ 已知缺口（第 71 轮收口时如实登记，不是"做完了"）**：上面这句**只落地了一半**。
> `check_local_key` 已经返回 `KeyIdentity`（`Local` / `ApiKey(id)`，`key_id()` 就是账本里的值），
> 但**下游没有消费者**：`ta-gateway/src/ingress.rs` 的 `handle` 里是 `let _identity = …`，
> 而 `ta_core` 的 `UsageRecord` 没有 `key_id` 字段、meter 侧也没有调用点 ——
> 所以 `ta_store::SqliteUsageStore::record_api_key_usage`（已实现、有测试）在生产路径上**零调用者**。
> 后果：`/api-keys` 页的「用量」永远是空态（页面显示 `apiKeys.usageEmpty`，**不编数字** —— 这一点是诚实的）。
> 要收口需要：① `ta_core::types::UsageRecord` 增加 `key_id: Option<String>`；
> ② `ta-gateway` 把 `KeyIdentity` 传进 pipeline / meter，并调 `record_api_key_usage`；
> ③ 请求日志/快照若要带这个身份，先在 `docs/14 §5` 登记字段名（`key_id` 已是白名单字段）。
> 归口：`ta-gateway` 归 E5、`ta-core` 归决策层。**CRUD 与鉴权接线本身是完整可用的**：
> 鉴权逻辑由 `ta-gateway` 的 ingress 单测（`TA-1001` / 多 key / 禁用即挡）与网关装配断言覆盖。
> 但真机上 `step18` **只验证了本地 CRUD** —— 无内核的环境里网关根本不监听，
> 「带 key 放行 / 不带 key 401」那一段记为**未验证**（见 §5.3），不许当成通过。

### 3.4 页面（`/api-keys`）

| 控件 | testid | 语义 |
| --- | --- | --- |
| 页面 | `api-keys-page` | 侧边栏新增入口「访问密钥」 |
| 列表 | `api-keys-table` / `api-key-row` | `key_prefix`、名称、启用、上次使用 |
| 新建 | `api-keys-create` → `api-keys-create-name` / `-note` / `api-keys-create-submit` | 成功后 `api-keys-created-plaintext` 显示**一次**明文 + 「我已保存」`api-keys-created-done`；关闭后明文**不再可得**（文案写清） |
| 复制 | `api-keys-copy` | 复制明文（仅创建后那一次）或用例二：从列表复制**不可行**（只存哈希）→ 列表行**不放复制按钮** |
| 启停 | `api-key-toggle` | `api_key_set_enabled` |
| 删除 | `api-key-delete` | 二次确认 `api-key-delete-confirm` |

---

## 4. 契约改动清单（谁能改哪些文件，见 §6）

- `src/shared/bindings.ts`（**主管**）：
  - `ProbeResult` 增加 `probe: "models" | "chat" | "none"`；
  - 新增 `ApiKeyView` / `ApiKeyUsageRow` 与 5 条命令；
  - `Model` 增加 `default_lane?: LaneId | null`；`ModelPatch` 增加 `lane?`；
  - `CommandMap` 注释更新（注册条数从 104 前进）。
- `docs/10-interface-spec.md`：provider / model / onboarding / 新增 api_key 区块；`/network` 的节点与测速命令接法。
- `docs/02`：`Model.default_lane`；`api_key` 表。
- `docs/11`：如新增错误码则同步总表（尽量复用：`TA-1004` 参数/过期预览、`TA-1002` 鉴权、`TA-5002` 被引用）。
- `docs/18 §2`：标注被 §2.2 取代。
- `src/shared/i18n/*.ts`：**主管独占**（写者缺键报主管，不要自己插）。

---

## 5. 验收（主管在干净快照上跑）

1. **红先绿后**：每个工作包先给出会失败的断言原文（前端 `vitest`、后端 `cargo test`）。
2. **门禁**：`npm run build` / `cargo fmt --check` / `cargo check --all-targets` / `cargo clippy -- -D warnings` /
   `cargo test --workspace` / `npm test` 全 0。
3. **真机 E2E**（`scripts/e2e/smoke.mjs`，真实窗口驱动）。**交付后的实际步骤号**与第 71 轮收尾时的真机结论
   （快照 `283d8d6` + 修复 `60f5bbe`/`eb750cf`：`PASS=21 SKIP=1 FAIL=0`）：
   - `step2` 服务商分步向导（本文档初稿写的 `step16`，实现沿用了既有编号）：`pw-name` 填重名 → 本地拦下；
     改名 → `pw-test` 真调（结果区出现延迟或后端原文）→ **`pw-next` 真进第 2 步** → `pw-scan` 真调 →
     勾选 → `pw-scan-add` → `config(kind='model')` 出现；并断言 `pw-model-info-panel` 里**没有可写控件**、
     `pw-step2-scroll` 可滚动。真机证据：真实 id `ta-e2e` + `<id>-default` 凭据、扫描 3 条并添加成功。
   - `step17` 节点清单：`nodes-list` 出现真实节点（fixture 订阅解析后）——
     真机证据是"2 行，且包含 fixture 里的 `ta-e2e-node-hk`"。**组筛选本轮未交付**（§2.3 的裁决）。
     订阅未解析时 `nodes-empty` 文案是"还没解析"那一档。
   - `step18` API key：**本地 CRUD 已真机验证**（新建 → 明文只出现一次 → 列表只留前缀不含明文 →
     启停开关真翻转 → 删除后行消失）。**HTTP 鉴权那一段（带 key 非 401 / 不带 key 401 / 禁用即 401）
     在无内核的环境里记 `SKIP`**，不记 PASS 也不记 FAIL：启动编排在内核未就绪时**故意不启动网关**
     （`boot.rs:454`；日志里没有 `gateway.rs:335` 那句「网关开始服务」，且不带任何 key 的探测同样
     `status:0` —— 端口没人监听，与鉴权无关）。这段逻辑由 `ta-gateway` 的 ingress 单测与装配断言覆盖；
     要在真机上拿到结论，需要在**有内核**的机器上跑同一个脚本（那时该段会自动走完整断言）。
   - `step12` 反向断言（初稿写的 `step19`）：`/onboarding` 路由不再渲染向导（`onboarding-step-panel` 不存在）、
     设置页无网络向导块、`/network` 在**空配置**下**没有** `network-optional`（作废）且各区块给出诚实空态。
   - `step20`（第 72 轮新增，[docs/20 §4](20-provider-model-lane-and-edit.md)）：服务商详情 →「编辑」进同一个两步向导的
     编辑模式：第 1 步预填现有值、**留空密钥也能到第 2 步**、行内改网络代理后库里通道的 `lane` 真的变、且不派生出第二个服务商。
     同时 `step2` 加严：**添加之后上方必须立刻出现可编辑的模型行、默认显示「直连」、库里真建出带 lane 的通道**
     （用户第 72 轮原话：「点击添加之后并没有在上面看到可以编辑的东西，也没有地方编辑线路」「线路是未配置，按理默认是直连」）。
   - 既有步骤必须继续全绿（不许为了新步骤放宽旧断言）。
4. **安装包**重出 + A3/A4 不变量在同一发布二进制上复跑。

---

## 6. 分工与写范围（一轮一包，各自 PR）

| 工作包 | 负责人 | 写范围 |
| --- | --- | --- |
| **GW-71**：出站探测与模型清单原语（`probe_provider` / `list_upstream_models`，走既有 lane 通道，**不另开第二条出站路径**） | E5 | `crates/ta-gateway/**` |
| **DESK-71**：3 条命令实现与注册 + `api_key` 表与 5 条命令 + 网关多 key 鉴权接线 | E3 | `src-tauri/**`、`crates/ta-store/**`（迁移与 `api_key` 存储） |
| **UI-A**：服务商两步向导（§1）+ 去 `/network` 再回来的草稿保留 | E1 | `src/pages/providers/**`、`src/app/providers*.test.tsx` |
| **UI-B**：网络页（§2：关向导、常显、节点清单、lane_test） | E4 | `src/pages/network/**`、`src/app/network.test.tsx` |
| **UI-C**：API key 页（§3.4）+ 设置页删网络向导块 | E2 | `src/pages/apikeys/**`、`src/pages/settings/**`、`src/app/apikeys*.test.tsx`、`src/app/settings.test.tsx` |
| 契约 / i18n / 文档 / E2E / 门禁 / 安装包 | 主管 | `src/shared/bindings.ts`、`src/shared/i18n/**`、`docs/**`、`scripts/e2e/**` |

**依赖顺序**：契约（主管先落）→ GW-71 与 DESK-71 并行 → UI-A/B/C 可与后端同时开工（先按契约 mock，
真机收口后必须改成真命令）→ 主管 E2E + 门禁 + 安装包。

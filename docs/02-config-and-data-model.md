> ⚠️ **字段名以 [config.example.yaml](../config.example.yaml) 为准。** 该文件是**从 Rust 类型生成的**
> （`pwsh -File scripts/gen-example-config.ps1`），并有防漂移测试 `crates/ta-core/tests/example_config_guard.rs` 守卫。
> 本文只解释**语义与选型理由**；正文里若有与示例不一致的字段名，以示例为准。
>
> 命名纪律：带单位的字段一律带后缀 —— `interval_s` / `ttl_h` / `cooldown_s` / `connect_s` / `read_s` / `tolerance_ms`。
> 曾经因为示例里写成 `interval` / `ttl` / `cooldown`，导致这些值**静默落进 `extra` 未知字段、运行时不生效却不报错**，
> 是员工 P1 实测抓出来的。这是本项目最阴险的一类 bug，所以加了生成 + 守卫两道机制。

# 02 · 领域模型与配置

## 1. 领域模型关系

> ### ⚠️ 两种"订阅"必须先分清（早期文档的坑）
>
> | 说法 | 实体 | 是什么 | 例子 |
> |---|---|---|---|
> | **机场订阅** | `proxy_subscriptions` | 一堆**节点**的来源（Clash 格式 yaml / 链接列表） | 你买的那个机场 |
> | **套餐 / tokenplan** | `credentials` | 一份 **API 额度**（key + 额度 + 重置周期） | 字节方舟、command code、OpenAI 个人订阅 |
>
> **出网方式挂在"套餐"上**（`credential.lane`）；lane 再指向机场订阅里的某个组/节点，或直接是直连。
> 所以"一个模型、两个套餐、两种出网方式"是完全自然的一等场景 —— 见 [03 §0 基准场景](03-routing-and-policy.md)。

```
ProxySubscription (机场订阅：一堆节点)
        │ 1..n
        ▼
      Lane  ────────────────┐   (网络通道：直连 / 某订阅的某策略组 / 固定节点)
        ▲                   │
        │                   │
   Provider ──1..n──> Credential (套餐：一个 key = 一份 tokenplan)
        ▲                   │
        └────────┬──────────┘
                 │
              Channel  (一条可发送的出网方案 = 协议 + 套餐 + 线路 + 模型名映射)
                 ▲
                 │ n..1
            ChannelGroup (同一个逻辑模型的多个候选 + 选择器 + 粘性策略)
                 ▲
                 │
              Model (对外暴露的逻辑模型)
                 ▲
                 │
             RoutePolicy (规则表：match → action)
```

一句话记忆：**Model 是对外的脸，Channel 是干活的手，Credential 是钱包，Lane 是路，Policy 是规矩。**

**跨厂商切换是本设计的核心场景**：同一个 Model 挂多个 Channel，每个 Channel 用不同 Credential（套餐）、走不同 Lane。
选通道时线路**自动跟着变** —— 这是"切套餐"与"切线路"为同一动作的原因（管线第 8 步，见 [09 §5.1](09-architecture-design.md)）。

## 2. 实体定义

### 2.1 Provider（上游服务）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 稳定 ID |
| `name` | string | 显示名 |
| `protocol` | enum | `openai` \| `openai-responses` \| `anthropic` \| `gemini` \| `custom` |
| `base_url` | url | 到版本段为止，如 `https://api.openai.com/v1` |
| `auth.scheme` | enum | `bearer` \| `header` \| `query` \| `none` |
| `auth.header` | string? | 如 `x-api-key`（anthropic）、`api-key`（azure） |
| `headers` | map | 附加头。**默认只补缺，不覆盖客户端同名头**（`on_conflict: keep_client`） |
| `passthrough` | obj | 正常情况**不需要配置**：`header_mode: pass`、`body: preserve` 是默认也是唯一推荐值。允许改成白名单只是想让人确信"我没有被强制全透传"，改错会吞字段 |
| `timeout` | obj | `connect_s` / `read_s`（流式要长）/ `total_s` |
| `tls` | obj | `insecure`、`extra_ca`、`min_version` |
| `model_prefix` | string? | 有些中转要求 `openai/gpt-4o`，这里统一加前缀 |
| `enabled` | bool | |

内置模板（新建时一键填充）：OpenAI、Anthropic、Google Gemini、DeepSeek、OpenRouter、Moonshot、智谱、通义、**自定义中转（OpenAI 兼容）**、**自定义中转（Anthropic 兼容）**。

### 2.2 Credential（凭据 / 一份 tokenplan）

| 字段 | 说明 |
|---|---|
| `id`, `provider`, `label` | 标识 |
| `secret` | `"${secret:name}"` 引用钥匙串，**绝不明文入配置** |
| `lane` | 该凭据默认走哪条线（可被 channel 覆盖） |
| `quota` | 见下 |
| `guard` | `max_rpm`、`max_concurrency`、`max_input_tokens` |
| `enabled` | 一键停用（比如某家中转抽风） |
| `tags` | 自由标签，策略可匹配 |

`quota`：

```yaml
quota:
  metric: unknown | tokens | requests | cost   # unknown = 只用响应头观测
  limit: 5000000                                # metric=tokens 时的总量
  currency: CNY                                 # metric=cost 时
  window: month | day | hour | none
  reset_day: 1                                  # 每月 1 号重置
  soft_limit: 0.8                               # 80% 告警（托盘变黄 + 通知）
  hard_limit: 1.0                               # 到 100% 标记 exhausted，路由自动跳过
  reserve: 0.05                                 # 给"最后兜底"留 5%
```

### 2.3 Lane（网络通道）

| 字段 | 说明 |
|---|---|
| `id`, `name` | 标识 |
| `kind` | `direct` \| `subscription_auto` \| `subscription_group` \| `subscription_node` \| `filter`（见 [04 §7.1](04-network-core.md)） |
| `subscription` | 引用 ProxySubscription（`kind != direct` 时必填） |
| `group` | **订阅里自带的组名**（`kind: subscription_group` 时）。机场的「🇯🇵 日本」「♻️ 自动选择」直接用 |
| `group_type` | `select` \| `url-test` \| `fallback` \| `load-balance`（组自带，也允许用户覆盖） |
| `node` | 固定节点名（`kind: subscription_node`） |
| `filter` | lane 级节点过滤的**正则串**（`Option<String>`，对应内核 `proxy-providers[].filter`）。**不是 mapping**；排除用同级的 `exclude_filter` |
| `dialer` | `builtin`（默认）\| `local_proxy`（该 lane 回退到外部代理端口，见 [04 §3](04-network-core.md)） |
| `health` | 运行态：`ok` \| `degraded` \| `down`、当前节点、延迟、最近失败时间 |

> Lane **没有 `port` 字段**——它不是一个监听端口，只是"用哪个节点、怎么选节点"的一组参数。出站由 `KernelDialer` 经该 lane 对应的内核 loopback 入站完成（见 [04 §7.1](04-network-core.md)）。

**线路解析**（[docs/55]（第 322 轮）起只有一级覆盖 + 全局默认，见 [04 §8.1](04-network-core.md)）：

```
provider.lane ?? net.default_lane ?? direct
```

用户裁决：*"线路级别的分配逻辑我们已经放弃了，取而代之的，**每个服务商统一一个节点**"*。
所以**只有 `Provider` 上还有一个可选 `lane` 字段**；`Channel.lane` / `Credential.lane` /
`PolicyAction.use_lane` 三个字段**已删除**（老配置里的值落进各自的 `extra`，能解析、不再生效）。
`direct` 永远是兜底。

> 历史：这一行原来写的是
> `policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`。
> 那三级覆盖的用户可见入口早已全部删除，却仍参与解析 —— 一个看不见也改不掉的覆盖项
> 会静默压过用户当前的选择。详见 [docs/55](55-provider-owns-the-lane.md)。

### 2.4 ProxySubscription（Clash 格式订阅）

| 字段 | 说明 |
|---|---|
| `id`, `name`, `url` | |
| `user_agent` | ★ 很多机场按 UA 返回不同格式（clash / surge / v2ray），默认伪装成 Clash 客户端 |
| `format` | `auto`（默认，靠响应头 + 内容嗅探）\| `clash` \| `base64_links` \| `plain_links` |
| `interval_s` | 自动更新周期（默认 21600 秒 = 6h；订阅返回 `Profile-Update-Interval` 时采用其建议值） |
| `filter.include/exclude` | 正则，剔除"剩余流量/到期时间/官网/防失联"这类假节点（**只有订阅级过滤是 mapping**） |
| `health_check` | `url`（**默认用真实上游端点**，不是 google）、`interval_s`、`tolerance_ms` |
| `keep_versions` | 保留最近 N 个版本（默认 3），坏更新时可一键回滚 |
| `parsed` | 运行态：节点数、组数、未识别协议列表、最后解析时间、解析器版本 |
| `rules_raw` | 运行态：订阅自带的 rules / rule-providers（**只读展示，不执行**，见 [04 §5.5](04-network-core.md)） |
| `userinfo` | 运行态：订阅流量、到期时间（从 `Subscription-Userinfo` 响应头解析，托盘可显示） |

### 2.5 Model（逻辑模型）

| 字段 | 说明 |
|---|---|
| `id` | 对外模型名，如 `gpt-4o` |
| `aliases` | 客户端可能写的其它名字（含带日期版本号、带 provider 前缀的形式） |
| `display` | UI 排序、分组、图标 |
| `expose` | 是否出现在 `/v1/models`（默认原样透传上游的列表） |
| `default_lane` | **可选**（`#[serde(default, skip_serializing_if)]`，[20 §1.3](20-provider-model-lane-and-edit.md)）：这个模型默认走哪条线路。它是「添加模型」那一步唯一可改的字段；既作为"下次为它建 channel 时的默认 lane"，也会被 `model_bulk_update({ patch: { lane } })` 同步写到它**现有**的每个 channel 上。三态：字段缺失 = 不动；`null` = 清空（回到五级继承链，末端就是直连）；具体 lane id = 写该线路。**界面上没显式配就显示"直连"，不许显示"未配置"** |

> 这里**没有 `capabilities` 字段**。模型能力不参与请求改写，只影响"发给谁"：能力声明在 **Channel** 上（[17 §2](17-add-flows-and-capability-routing.md) 的 `Channel.capabilities`）。上一版的能力矩阵/探测/学习已删除。

### 2.6 Channel（通道）

| 字段 | 说明 |
|---|---|
| `id`, `model`, `provider`, `credential`, `lane` | 五元组 |
| `upstream_model` | 发给上游的真实模型名 |
| `transform.path` | 覆盖路径（少数中转路径不同） |
| `transform.headers` | `add` / `remove` / `override`（默认 `add` = 只补缺，绝不覆盖客户端同名头） |
| `transform.body` | 定点 patch（如强制 `stream_options.include_usage=true`） |
| `weight`, `priority` | 选择器权重/优先级 |
| `tags` | `vision` / `no-vision`（可选，用于选目的地）、`cheap`、`official`、`cn-direct`、`no-log` |
| `enabled` | |
| `state` | 运行态：`available` \| `cooling(until)` \| `exhausted` |
| `capabilities` | **上游能力声明**（[17 §2](17-add-flows-and-capability-routing.md)）：`vision` / `tools` / `reasoning` / `context_tokens` / `max_output_tokens` / `modalities`。`#[serde(default)]`，老配置不写也能用（默认全 false、0、null、`["text"]`）。**同一条路由下、同一个 `model` 的多条线路能力必须逐一相等**（`modalities` 排序去重后比、`null` 只等于 `null`），否则 `policy_upsert` / `channel_upsert` 直接拒绝并返回 `TA-2007` 与逐字段差异 |

### 2.7 ChannelGroup（同模型多套餐）

```yaml
- id: grp-gpt-4o
  model: gpt-4o
  members: [ch-openai-personal, ch-relay-a, ch-relay-b]
  selector: balanced_sticky
  sticky: { enabled: true, ttl_h: 12, key: [header:x-session-id, body_hash] }
  release_on: [hard_error, user_pin_change]
```

选择器枚举（**现在只有一种**，[docs/48]）：

| selector | 语义 | 适用 |
|---|---|---|
| `balanced_sticky` | 先查粘性表；未命中挑 **token 用量最少**的线路（并列按 priority；无会话标识时在"最轻的那一档"里随机）；线路报错则释放粘性、改粘到另一条 | **唯一取值**：会话固定 + 新会话往轻的线路偏 |

> **这里原本有七种**（`priority` / `sticky_then_priority` / `quota_first` / `cheapest` /
> `lowest_latency` / `weighted_random` / `round_robin`），为什么收敛成一种见
> [docs/47](47-quota-exhaustion-error-shapes.md)（各家额度耗尽的返回形状**分码也分不干净**，
> 自动判断不可靠）与 [docs/48](48-balanced-sticky-only-routing.md)（改成**用户用开关手动调度**）。
> 旧取值仍能被解析（`#[serde(alias)]` 一律落到 `balanced_sticky`），老配置不会因为升级而读不出来。

### 2.8 RoutePolicy（使用策略）

```yaml
- id: pol-xxx
  name: 人类可读名
  enabled: true
  priority: 10            # 数字小的先求值；UI 里就是拖拽顺序
  match: {...}
  action: {...}
```

`match` 条件（全部为 AND，列表内为 OR）：

| 条件 | 示例 |
|---|---|
| `model_matches` | `["gpt-4o*", "claude-*"]`（glob） |
| `has_image` | `true` —— 请求内容含图片块（只为选目的地，不改请求） |
| `header_matches` | `{"x-client": "cursor"}` |
| `ua_matches` | `"(?i)cursor\|claude-code"` |
| `channel_tags_any` / `_all` | `[cheap]` |
| `input_tokens_lt` / `_gt` | `4000` |
| `stream` | `true` |
| `quota_remaining_lt` | `0.15`（账本观测） |
| `time_window` | `"09:00-19:00"` / `weekdays` |
| `provider_in` | `[p-mytoken]` |
| `all` | `true`（兜底规则） |

`action` 动作：

| 动作 | 说明 |
|---|---|
| `use_group` | 用哪个 ChannelGroup |
| `use_channels` | 直接点名单/覆盖候选集 |
| `exclude_channels` | 剔除指定通道（可叠加） |
| `require_tag` / `exclude_tag` | 按标签收窄候选（如 `exclude_tag: no-vision`） |
| `selector` | 覆盖选择器 |
| `failover` | `on`、`max_attempts`、`cooldown_s`、`backoff`（`on` 的取值用 `http429` / `http402` / `http5xx`，**不带下划线**，见 [config.example.yaml](../config.example.yaml)） |
| `on_no_candidate` | `error`（默认）\| `use_any` |
| `pin_sticky` | 强制锁定到某通道（"这几天就一直用 B 家"） |
| `transform` | 追加改写 |

### 2.9 Gateway 全局配置

见 [config.example.yaml](../config.example.yaml) 顶部。

### 2.10 SystemIntegration（开机自启动与系统集成）

```yaml
system_integration:
  # ── 开机自启动（本项目唯一会写操作系统状态的功能，见 docs/09 §1.2）──
  autostart: false            # ★ 默认 false：显式 opt-in，绝不默认写入系统
  autostart_mode: tray        # tray（启动后静默进托盘）| window（启动后显示窗口）
  autostart_delay_s: 0        # 0–300 秒。开机时磁盘/网络繁忙，延迟启动可避免内核首次就绪失败

  # ── 实验性开关，默认关（核心承诺：不接管整机网络）──
  enable_tun: false
  enable_system_proxy: false

  # ── 其它 ──
  tray_show_latency: true
  file_watch_mode: false      # 文件监听模式：以 YAML 为真源（GUI 写回会覆盖，需警告）
  file_watch_path: null
```

**字段语义（三条容易搞错的）：**

| 项 | 语义 |
|---|---|
| `autostart` vs 系统实际状态 | `autostart` 是**期望状态**，不是事实。UI 开关显示的是**系统实际状态**（读注册表 / LaunchAgent）。两者漂移时 UI 要显示出来并提供「重新注册 / 清除」，**绝不静默改回**（见 docs/09 §1.2 E3/E4） |
| `autostart_mode` | `tray`（默认）：启动后只进托盘、不弹窗 —— autostart 的目的是让网关在后台待命，客户端连 `127.0.0.1:8787` 不需要用户开窗口。`window`：调试或想让用户看见时用 |
| `autostart_delay_s` | 由**我们自己**实现（Tauri 的 autostart 插件不支持延迟）：通过命令行参数把延迟传给自身，启动后先睡再拉内核。避开开机高峰，减少"内核首次就绪失败→退避重试" |
| 网关是否随之启动 | **是**。App 启动即为网关启动（同一进程）。「暂停网关」是**会话级**状态、不持久化：重启后恢复运行 —— 暂停是应急动作，不该被记住 |

**跨平台实现：**

| 平台 | 机制 | 说明 |
|---|---|---|
| Windows | 注册表 `HKCU\...\CurrentVersion\Run` | 当前用户级，**不需要管理员权限**（与我们的免提权承诺一致）。卸载时由 NSIS 钩子删除 |
| macOS | `~/Library/LaunchAgents/*.plist` | Tauri autostart 插件的 `MacosLauncher::LaunchAgent` |
| Linux | `~/.config/autostart/*.desktop` | XDG autostart，无需 root |

**卸载清理（必须在 W5 落实）：** 安装包卸载时若不删 Run 键，会留下指向已删除 exe 的孤儿项 —— 这是最讨厌的一类残留。Windows 用 NSIS 的 `un.onInit` 或 `!macro customUnInstall` 删除。

## 3. SQLite 表结构

> 用量与成本相关的表（`usage_request` / `usage_daily` / token 类型归一化）见 [07 §6.5](07-ui-pages-and-cost.md)；请求日志的开关与保留策略见 [07 §2](07-ui-pages-and-cost.md)。

**网关对外的访问密钥**（[19 §3](19-provider-wizard-network-default-and-api-keys.md)，迁移 `0004_api_key.sql`）**不在配置真源里** ——
它是运行态的凭据，与"服务商凭据"（`secret_meta` + 钥匙串）是两回事：

```sql
-- 我们网关对外发的访问密钥：客户端拿它调 127.0.0.1:8787
-- **只存 sha256**：明文只在 api_key_create 的响应里出现一次（与"key 永不进 SQLite"同族）。
CREATE TABLE api_key (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE,   -- sha256(plaintext)
  key_prefix TEXT NOT NULL,        -- 展示用：前 12 字符 + "…"
  enabled INTEGER NOT NULL DEFAULT 1,
  note TEXT,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER             -- 从未用过 = NULL（不是 0）
);
```

用量按 key × 日 × token 种类记在既有的 `api_key_usage(key_id, day, token_kind, tokens, requests, cost, currency)`
（`0001_init.sql` 就有；由网关在鉴权后写）。`api_key_usage` 命令返回的行形状与
`ta-store::ApiKeyUsageRow` **逐字一致**，不另造"聚合后"的 DTO。

```sql
-- 运行态数据（配置不在这里，配置在 YAML）
-- 用量明细：token 细分存 JSON map，价格存快照（改价不改历史）
CREATE TABLE usage_request (
  request_id TEXT PRIMARY KEY, ts INTEGER,
  credential_id TEXT, provider_id TEXT, channel_id TEXT,
  model TEXT, upstream_model TEXT, client TEXT, lane_id TEXT, node TEXT,
  status INTEGER, error_type TEXT, latency_ms INTEGER, ttfb_ms INTEGER,
  token_kinds TEXT,          -- JSON: {"input":1204,"input_cached":51200,"output":386,...}
  usage_raw TEXT,            -- JSON: 上游原始 usage，用于对账
  -- 第 9 轮增补（P4）：标注这份 usage 的可信度，落实 docs/11 §6 铁律 4"不猜不估算"。
  -- upstream = 上游给了完整细分；upstream_partial = 只给了总数；estimated = 完全没给，本地估算。
  -- 没有这一列，成本中心就无法诚实标注"该通道未提供细分"。
  usage_source TEXT,
  total_tokens INTEGER, cost REAL, currency TEXT, price_snapshot TEXT
);
CREATE INDEX ix_usage_ts ON usage_request(ts);
CREATE INDEX ix_usage_cred_ts ON usage_request(credential_id, ts);

-- 日汇总：token_kind 作为行维度，新增 token 类型无需改表
CREATE TABLE usage_daily (
  day TEXT, credential_id TEXT, provider_id TEXT, model TEXT, client TEXT,
  token_kind TEXT, tokens INTEGER, requests INTEGER, cost REAL, currency TEXT,
  PRIMARY KEY (day, credential_id, provider_id, model, client, token_kind)
);

CREATE TABLE quota_state (              -- 由账本 + 响应头 + 手工额度合成
  credential_id TEXT PRIMARY KEY, window_start INTEGER, window_end INTEGER,
  used_tokens INTEGER, used_requests INTEGER, used_cost REAL,
  reported_remaining REAL, reported_reset_at INTEGER,
  state TEXT,                           -- ok | soft | exhausted | unknown
  -- 下面两列在第 7 轮补入（迁移 0002）。
  -- 原因：ta_core::quota::QuotaState 是冻结类型、没有 invalid 位，
  -- 但"key 失效"必须与"额度耗尽"区分 —— 前者窗口滚动也不该解除，
  -- 否则失效的 key 会被反复启用、白白烧掉尝试次数。
  -- 语义位定义在 ta_core::quota::backend::QuotaRecord，以那里为准。
  hard_exhausted INTEGER NOT NULL DEFAULT 0,
  invalid        INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER
);

CREATE TABLE api_key_usage (            -- 每个 key 的用量，成本中心按 key 分组用
  key_id TEXT, day TEXT, token_kind TEXT, tokens INTEGER, requests INTEGER,
  cost REAL, PRIMARY KEY (key_id, day, token_kind)
);

CREATE TABLE sticky (                   -- 会话粘性
  key TEXT PRIMARY KEY, group_id TEXT, channel_id TEXT,
  created_at INTEGER, expires_at INTEGER, hits INTEGER
);

CREATE TABLE trace (                    -- 每个请求一行，含失败尝试
  request_id TEXT PRIMARY KEY, ts INTEGER, client TEXT, model TEXT,
  protocol_in TEXT, protocol_out TEXT, stream INTEGER,
  policy_id TEXT, group_id TEXT, chosen_channel TEXT,
  attempts TEXT,                        -- JSON: [{channel, lane, node, status, ms, reason}]
  header_diff TEXT,                     -- JSON: 客户端发的头 vs 上游收到的头的差异（自检，正常应恒为空）
  tokens INTEGER, status INTEGER, total_ms INTEGER, ttfb_ms INTEGER, error TEXT
);

CREATE TABLE health_sample (
  ts INTEGER, lane_id TEXT, node TEXT, latency_ms INTEGER, ok INTEGER, status INTEGER
);

CREATE TABLE secret_meta (              -- key 的元数据，值在钥匙串/加密文件
  name TEXT PRIMARY KEY, created_at INTEGER, last_used INTEGER, hint TEXT
);
```

保留策略：`trace` 按 `request_log.retention` 清理（默认 30 天），`usage_request` / `usage_daily` **永久保留**（很小，且是成本对账的依据），`health_sample` 保留 24h。

> 没有 `capability_probe` 表，也没有 `port_allocations` 表——前者随能力探测一起删除，后者随"不引入旁路端口"一起删除。

## 4. 配置真源与导出

> **评审后修订**：真源从 YAML 改为 **SQLite**。原因是 `serde_yaml_ng` 不保留注释，GUI 高频回写会把用户手写的注释全抹掉（详见 [08 §2.5](08-tech-stack.md)）。

| | 说明 |
|---|---|
| **真源** | `<AppData>/TokenAgency/db.sqlite` 的 `config` 表（一行一实体的 JSON，事务写入） |
| **YAML 的角色** | ① 导入/导出（`ta export` / `ta import`）② 分享与迁移 ③ 可 git 管理的快照 |
| **文件监听模式**（可选） | 给"我就是想手写 YAML 并用 git 管"的用户：开启后以 `<AppData>/TokenAgency/tokenagency.yaml` 为真源，文件变更热重载；**此时 GUI 的编辑写回会覆盖文件，UI 上必须明确警告** |
| 改配置的路径（默认模式） | GUI 修改 → 校验 → 事务写 DB + 留快照 → 换内存快照 → **让前端的 `qk.config` 失效**（见下面两条铁律）→ 托盘与 UI 刷新，**无需重启** |
| 配置校验 | 由 Rust 类型经 `schemars` 导出 JSON Schema + 语义校验（引用完整性、lane 引用了不存在的订阅、channel 引用了不存在的 credential、端口冲突等），错误定位到具体字段路径（`serde_path_to_error`） |
| 便携导出 | `ta export --with-secrets` 生成加密包（age / AES-GCM，口令解锁） |
| 版本化 | 每次成功应用配置留一份带时间戳的 YAML 快照，可回滚（最近 N 份） |
| **数据根覆盖**（`TA_DATA_DIR`） | 环境变量存在且非空时，数据根改为该目录：库 = `<TA_DATA_DIR>/db.sqlite`，日志 = `<TA_DATA_DIR>/logs`，内核与订阅目录同样从它派生。**它是数据根的唯二来源之一**（另一个是 `<AppData>/TokenAgency`），解析只在一个地方（`state::AppState::app_data_root`）。用途：端到端验证必须在**不碰用户真实数据**的前提下驱动真界面；Windows 上 `dirs::data_dir()` 读系统已知文件夹，改 `%APPDATA%` 无效，所以只能由应用自己读。空值（`TA_DATA_DIR=`）不算设置，避免数据静默落到当前目录 |

### 4.1 写配置的两条铁律（真机 E2E 抓到用户可见的数据丢失之后固化）

1. **`config_apply` 是「整份覆盖」，不是「按字段合并」**：`crates/ta-store/src/config_store.rs::write_rows`
   先 `DELETE FROM config` 再按传入的那一份 `ConfigDoc` 逐实体插入。等价说法：
   **提交的 doc 里没有的实体 = 库里会被删掉**。
2. **因此，任何写命令成功后前端必须让 `qk.config` 失效**：`qk.config` 是全局共享缓存
   （`src/main.tsx` 的 `staleTime: 5_000`），而设置页是"读到什么就整份写回什么"的唯一页面。
   旧快照会把用户在别处刚写进去的实体静默删掉 —— 实测路径：
   在 `/network` 建一条线路（`lane_upsert`）→ 5 秒内打开设置页（缓存仍算 fresh → 不重取）→
   改一个字段点「校验并应用」→ **线路从 `config` 表消失**（`config_snapshot` 里
   label=`lane_upsert` 的 doc 有 1 条 lane、label=`apply` 的 doc 有 0 条，是可复现的铁证）。
   - 实现地点**只有一处**：`src/shared/api/queries.ts` 的 `useCommandMutation`（集中失效，
     不靠每个调用点自觉 —— 那是会漏的）；设置页另加 `refetchOnMount: "always"` 作为第二道保险。
   - 守卫/回归测试：`src/app/configWriteInvalidation.test.tsx`；
     真机回归：`scripts/e2e/smoke.mjs` 的 step14（建线路 → 设置页应用 → 线路必须还在）。
   - 设计上本该兜底的那条通道是 `config:changed` 事件（`src/shared/api/keys.ts` 的
     `EVENT_INVALIDATIONS["config:changed"]` 已经覆盖全部实体 query key），但它**目前没有生产端生产者**
     （`TaEvent::ConfigChanged` 只在 `gateway.rs` 的测试里被构造过）。
     所以第 2 条是**必需品**；后端将来真的发出该事件时它仍然无害（重复失效是幂等的）。


## 5. secrets 引用语法

```yaml
secret: "${secret:openai-personal}"       # 钥匙串条目名
# 或
secret: "${env:OPENAI_API_KEY}"           # 环境变量（CI/脚本场景）
# 或（不推荐，会提示警告）
secret: "sk-..."
```

- **key 永不进 SQLite**，只存 `secret_meta` 元数据；真值在系统钥匙串（`keyring`）。
- 首次启动或导入 YAML 时，如果发现明文 `sk-...`，UI 弹窗提示"迁移到钥匙串"，一键完成并改写来源。

---

## 决策记录：实体职责与关系（合并自两份已删的过程文档）

* **线路归服务商，不归模型**：模型侧只记"用哪几条线路"（`Model.lines`）；服务商的详情页是线路的
  唯一创建/编辑处。模型与线路是多对多，不是一对一。
* **每个服务商统一一个出口节点**：没有"按线路各自分配节点"那一层（它曾是死层级）。
* 因上述翻转而删掉的东西：线路级分配的 4 条命令、`/network/assignments` 页。
* **删除只往一个方向传**：删模型**不**级联删线路（线路不归它）；被引用的订阅删不掉，
  且拒绝信息必须能照着改（只报"不能删"不算完）。
* 落地位置：`crates/ta-core/src/domain/mod.rs` 的 `Provider` / `Lane` / `Model`；
  悬空引用由 `crates/ta-core/src/config/validator.rs` 拒绝。

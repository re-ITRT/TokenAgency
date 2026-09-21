# 06 · 里程碑、验收与风险

## 1. M0 · 技术验证（3–5 天，先证伪再投入）

只做 spike，不写产品代码。每一项都必须有可运行的 demo 与结论记录。

| # | Spike | 验证内容 | 失败时的退路 |
|---|---|---|---|
| S1 | **内核 sidecar 起停** | Tauri 拉起 mihomo、`/version` 就绪探测、Job Object 防孤儿、崩溃指数退避重启、退出不留残留进程 | `net.kernel.provider: external` + 用户手填路径 |
| S2 | **Lane 机制** | 生成含 `listeners[].proxy` 的配置；`curl -x http://127.0.0.1:39101 https://api.openai.com/v1/models` 走日本、`39103` 走直连；`netstat` 断言全部 loopback、无 `0.0.0.0` | 若 `listeners` 不可用 → 退化为多个 `mixed-port` 多实例（成本高，尽量避免） |
| S3 | 内核热重载 | 增删 lane 是否需要重启内核；切节点（`PUT /proxies/{name}`）是否无需重启；重载期间在途请求是否中断 | 变更 lane 集合时重启内核（请求排队 1s + 直连通道降级放行） |
| S4 | 直通保真 | 写 golden test：含 `image_url`(带 `detail`)、未知顶层字段、`anthropic-beta` 头、自定义 UA 的请求，经网关后与原始请求逐字段 diff | 若用框架导致丢字段 → 改用手写 `hyper` + `Value` patch |
| S5 | SSE 流式透传 | 首字节延迟、无缓冲、客户端中断时上游连接被及时取消 | — |
| S6 | 订阅解析 | clash yaml 与 base64 链接各 3 个真实订阅，节点数/名称/参数正确；HTML 错误页不覆盖缓存 | base64 解析降级为"仅支持 clash yaml" |
| S7 | 许可证 | 核实所选 Rust 协议库（rustls / shadowsocks crate / quinn 等）的 LICENSE；确认不再需要任何 GPL 内核 | 换等价的宽松许可库 |
| S8 | Tauri 托盘 | 动态菜单、图标切换、tooltip 刷新、左键显窗 | — |

## 1.5 内核行为不确定项（M0 **必验**，由员工 P5 交付时主动列出）

> **这条清单本身就是一次高质量交付。** 我要求 P5 明确区分"我确定的"与"我推断的"，
> 它给出了 9 条 —— 每条都说明了自己的假设与"如果我错了会怎样"。
> **这些不是待办事项，是必须在真 mihomo 上证伪的假设。** 在验完之前，
> `ta-kernel` 的通过测试只证明"它符合 P5 以为的内核行为"。

### 已确定（有文档依据 + 代码/测试覆盖，不必重验）

YAML 模板字段与隔离项 · `Bearer` 鉴权 · `PUT /configs?force=true {"path":abs}` · `PUT /proxies/{g} {"name":n}` ·
`GET /proxies/{n}/delay?timeout&url` · `PUT /providers/proxies/{k}` · 启动命令 `-d <dir> -f <file>` ·
Job Object 防孤儿（**本机已实测**：drop spawner 后子进程被收走）· 退避序列 `[1,2,4,8]` + 上限 30s

### 必须真机验证（按"如果我错了会怎样"排序）

| # | 假设 | 错了会怎样 | 验法 |
|---|---|---|---|
| K1 | ✅ **已验证（见 §1.6）：成立** —— `listeners` 增删可靠 `PUT /configs?force=true` 生效**且进程不重启**（PID 不变） | 当前 `apply()` 策略是"能 reload 就 reload，报错才 restart"。**若 reload 静默成功但 listener 陈旧，则策略是错的** —— 会出现"通道指向一个已关闭的端口"，且没有任何报错 | 改端口 / 加删 lane → reload → 断言**新端口在 listen 且旧端口已关** |
| K2 | ✅ **已验证（见 §1.6）：成立** —— 冷启动创建指向 `use: [provider]` 的组**不失败**，组当场可用（节点以 `alive:false` 存在） | 启动即失败，或通道静默不可用 | 冷启动（无缓存）直接起，看日志与 `GET /proxies` |
| K3 | ✅ **已验证（见 §1.6）：成立** —— `filter`/`exclude-filter` 与 `use` 可同用。**但 filter 匹配不到节点时会静默变 `[COMPATIBLE]`** → `config_gen` 必须显式写 `empty-fallback: REJECT` | `filter` lane 全部失效 | 生成一个带 filter 的组，看节点数与正则是否吻合 |
| K4 | ✅ **已验证（见 §1.6）：成立** —— `listeners[].proxy` 可指**裸节点名**、**带 emoji 的组名**、`DIRECT`，路由日志确认按目标走。**但内核不校验**：指不存在的名字也照样绑定成功、只在请求时炸 → `config_gen` 必须自己校验 | 线路分配的核心机制失效（docs/04 §7.1 全靠这条） | 两种各配一条 lane，`curl -x` 验证出口 |
| K5 | ✅ **已验证（见 §1.6）：成立且比担心的大度** —— 多余字段、`header` 列表形式、`find-process-mode: off`、空集合（`proxy-providers: {}` / `proxy-groups: []` / `listeners: []`）**全部被接受**。但反过来说明"内核不报错 ≠ 配置正确" | 内核启动失败（当前按模板逐字写，未证实空集合） | 逐项最小配置试起 |
| K6 | ✅ **已验证（见 §1.6）：成立** —— `mixed-port: 0` 确实关闭默认入站（只剩 REST + 我们显式开的 lane）→ **生成配置永远写 0**，I1 由结构保证而非靠检查 | 会多一个监听端口 → 违反 I1 入口唯一 | `netstat` 断言 |
| K7 | ⚠️ **已验证（见 §1.6）：字段名全部真实存在，但有 4 处偏差会让 `parse_snapshot` 解析错误** —— `providers` 是**对象不是数组**、`type` 是**大写** `Selector`/`Shadowsocks`、**provider 节点不在 `/proxies` 顶层**（组 `all` 会悬空）、内置伪节点 `DIRECT/REJECT/PASS/COMPATIBLE…` 混在列表里 | 我的 wiremock 测的是"我以为的内核"。**这是全清单里最需要重验的一条** —— UI 的线路下拉完全依赖它 | 用 P5 提供的 `RawSnapshot::fetch` dump 真 JSON，与假设逐字段对比 |
| K8 | ✅ **已验证（见 §1.6）：担心的坏事没发生** —— 强杀（`Stop-Process -Force`）后重启，**选中项仍保留**（`store-selected` 在选择变更时就写 `cache.db`，不是退出时写）→ **不需要为它做优雅退出** | 若 `profile.store-selected: true` 的选中项需优雅退出才落盘，**强杀会导致重启后选中的节点丢失** —— 直接破坏"线路分配"的用户体验 | 切节点 → 杀 → 重启 → 看选中是否保留 |
| K9 | ⏸ **本轮无法验证** —— 需要一台有沙箱限制的机器，当前开发环境是 full-access，测了也不代表受限场景。M1 联调时在真实安装环境（普通用户权限）里补 | 失败会降级为 warn + `taskkill /T` 兜底，孤儿进程风险 ↑ | 在有沙箱的机器上确认 |

**K1 / K2 / K7 是三条阻塞项**；K7 若形态不同，只是改 `parse_snapshot`；**K1 若成立性不满足，会改动 `apply()` 的整体策略**（可能要"重启内核 + 请求排队"）。

### 由此引出的两条待决设计（M0 一并定）

1. **`listeners` 变更的 apply 策略**：若能 reload → 保持现状；若必须重启 → 需要"请求排队 + direct 通道降级放行"
2. **两段式 apply**（若 K2 成立）：先等 provider 有节点，再 reload listener

## 1.6 M0 实机验证结果（**主管亲跑**，真 mihomo，2026-09-21）

> 被验证对象：`Mihomo Meta v1.19.31 windows amd64 with go1.26.8`
> 方法：在 `C:\Users\19404\workspace\.ta-tools\m0\` 起一个真内核（REST `127.0.0.1:39090`、
> 混合入站 `127.0.0.1:39100`，**不启用 tun/dns**），用真 HTTP 打它，原始 JSON 存为夹具。
> **K1/K2/K7 三条阻塞项就此结案。** 另：起停前后跑了 `scripts/check-invariants.ps1` 的 I2 对比 → **exit 0**。

### K1 ✅ **成立（结论对我们有利）—— reload 是进程内热重载，不重启进程**

| 观测 | 值 |
|---|---|
| `PUT /configs?force=true {"path":"<abs>"}` | **HTTP 204** |
| reload 前监听 | `127.0.0.1:39100`（mixed 入站）+ `127.0.0.1:39090`（REST） |
| reload 后（端口改 39100→39150） | **`39100` 已关、`39150` 在听** |
| **进程身份** | **PID `11956` 与 StartTime `23:43:21` 全程未变** |
| 可逆性 | 切回原配置 → `39150` 关、`39100` 开 ✓ |

日志只有 `Start initial configuration in progress` + `Mixed(http+socks) proxy listening at: 127.0.0.1:39150`，
**没有重启痕迹**。

**裁决：`apply()` 现有策略（"能 reload 就 reload，报错才 restart"）成立，不需要"重启内核 + 请求排队"。**
注意"进程内 reload"不等于"无中断"—— listener 仍有一瞬间的关闭/重建，**G2 的失败链要能把这一瞬间的 `connection refused` 当作可重试**。

### K2 ✅ **成立 —— 冷启动时 `use: [provider]` 的组不会失败，也不需要两段式 apply**

冷启动日志：`Start initial provider ta-lane-provider` → 无任何 error/warn。
且组 `TA-LANE-mixed` 的 `all` **当场就含 3 个 provider 节点**（`prov-node-a/b/c-vmess`），
即**组在首拉订阅未完成时也是可用的**（节点先以 `alive:false` 存在）。

**裁决：两段式 apply 不是必需的**（"先等 provider 有节点再 reload listener"可以省掉）。
但**通道不可用 ≠ 启动失败**：节点 `alive:false` 时通道应报 `TA-3002 lane_unavailable`，而不是静默走直连。

> ### ⚠️ K2 的重要细化（第 9 轮补充实测）—— 别把 K2 读成"provider 组永远是安全的"
>
> **`use: [provider]` 的组在两种情况下确实会空，而且会静默降级成 `[COMPATIBLE]`：**
>
> | 场景 | 内核 | 内核日志 | 组的 `all` |
> |---|---|---|---|
> | provider 文件**不存在** | **照常启动** | `level=error initial proxy provider … cannot find the file specified` | **`[COMPATIBLE]`** |
> | provider 文件**存在但为空**（`proxies: []`） | **照常启动** | `level=error … file doesn't have any proxy` | **`[COMPATIBLE]`** |
> | 同上 + `empty-fallback: REJECT` | 照常启动 | 同上 | **`[REJECT]`** ✅ |
>
> **K2 当初测的是"文件存在、只是还没跑健康检查"→ 节点以 `alive:false` 存在、组不空。**
> "provider **加载失败**"是**另一个场景**，结论相反。**两条都要记住，不要只记一条。**
>
> ### ★★ 由此引出的最严重问题：「就绪」判定会被骗
>
> 上表最危险的一格是**"内核照常启动"**。它意味着：
>
> **如果我们写 provider 文件失败（或写空了），内核会起来、REST 会通、进程健康 ——
> 而所有 lane 正在静默走 `COMPATIBLE`。而"进程活着 + REST 响应"式的就绪判定会报"已就绪"。**
>
> 这是"静默降级"的最严重版本：**不是某条 lane 降级，是整个内核降级而我们对外报了绿。**
>
> **裁决：「内核就绪」的判据必须包含"provider 真的加载了节点"**，不能只问"进程在不在"：
> 1. 启动 / reload 后，`GET /providers/proxies`，对我们生成的**每一个** provider 断言 `proxies[]` 非空；
> 2. 任一为空 → **不报就绪**，报一个说得清的错；
> 3. **判定必须可重查** —— 订阅同步完成后 provider 才可能有效，不能"启动时查一次就锁死"。
>
> **配套要求**：内核**已经在 `level=error` 报出来了**，信息是有的，只是在我们看不见的地方。
> 所以 `KernelSupervisor` 必须把内核的 stdout/stderr **转发进我们的 `tracing`**（带区分来源的 target）。
> 否则排查时会得出错误结论："我们的代码没有报错"。这条与 [14]（OB1 可观测性）正好对接。

### K7 ⚠️ **形态大体符合，但有 4 处偏差会让 `parse_snapshot` 解析错误 —— 必须改**

假设写的 `proxies[].type/now/all/history[].time|delay` 与 `providers[].name/vehicleType/updatedAt/proxies[]`
**这些字段名全部真实存在**。但：

| # | 偏差 | 后果 | 修法 |
|---|---|---|---|
| **7-a** | **`/providers/proxies` 里的 `providers` 是「对象」**（按 provider 名索引：`providers.ta-lane-provider.*`），**不是数组**。假设写 `providers[].name` 暗示数组 | 按数组反序列化 → **整个 snapshot 解析失败** | 按 `BTreeMap<String, ProviderView>` 解析；`name` 从**键**或字段两者取（字段里也有 `name`） |
| **7-b** | **`type` 是 mihomo 内部首字母大写名**：`Selector` / `Shadowsocks` / `Vmess`，**不是** `select`/`ss`/`vmess` | 按小写匹配 → 节点类型显示为空/错、组识别不出来 | `parse_snapshot` 做**大小写无关**映射，且**保留未知类型原样**（`Other(String)`），不丢弃 |
| **7-c** | **provider 提供的节点不出现在 `/proxies` 顶层**（顶层只有 `fake-hk-01/jp-01` 与组），但它们**确实出现在组的 `all` 里** | 用 `/proxies` 的键去解析 `all` → **悬空引用**，UI 线路下拉会缺节点或报错 | 节点索引必须**合并** `/proxies` 与 `/providers/proxies` 两个来源后再解析 `all` |
| **7-d** | **内置伪节点混在 `/proxies` 里**：`DIRECT`、`REJECT`、`REJECT-DROP`、`PASS`、`PASS-RULE`、`COMPATIBLE`、`GLOBAL` | 节点列表 UI 会把它们当用户节点显示 | 分类处理：`DIRECT` 是 lane 的合法目标（直连），其余按"内部/不可选"过滤或置灰 |

**额外拿到的（原假设没写，但很有用 —— 不要再自己算）**：

- **`alive: bool` 每个节点/组都有** → 这就是我们要的健康信号，**不必另做探测**。
- **`history[].delay` 单位是毫秒**；**不可达时 `delay = 0` 且 `alive = false`**（不是负数、也不是缺字段）→ 判"坏节点"要同时看两者，`delay == 0` 单独不可靠（真 0ms 不可能但别写死）。
- **`id`**：每个真实节点有稳定 UUID（如 `a061268b-…`），可用于跨重启识别同一节点。
- **`extra`**：按健康检查 URL 索引的对象，形如 `{"http://…generate_204": {alive, history}}`。
- **provider 的 `updatedAt` 对 `vehicleType: File` 是「文件 mtime」**（观测到 `23:42:34`，早于内核启动的 `23:43:21`），**不是拉取时刻** → 订阅"最后更新时间"的语义要按 vehicleType 区分：`File` = 文件时间，远程 = 拉取时间。
- **组节点的 `history` 是 `[]`**，健康历史挂在**被选中的那个节点**上，不在组上。
- 组节点额外字段：`emptyFallback`、`testUrl`、`hidden`、`icon`、`provider-name`。

### 附带验证

- `PUT /proxies/{组}` body `{"name":"<节点>"}` → **204**，随即 `GET` 该组 `now` 已变 ✓
- **非法节点名 → `400` + `{"message":"Selector update error: proxy not exist"}`**（英文原文，UI 需翻成人话）
- `GET /version` → `{"meta":true,"version":"v1.19.31"}`
- **I2 通过**：起停 + 改端口前后，系统代理 / 路由表 / DNS / 虚拟网卡**逐字段一致**（`check-invariants.ps1 -Mode Compare` → exit 0）。未启用 TUN，未碰系统代理。

### ⚠️ 一个与内核无关、但会影响交付的产品发现

**mihomo v1.19.31 的 release 里没有 `checksums.txt`（HTTP 404）。**
→ **将来若做内核自动下载，不能假设上游提供校验和。** 可选路径：(a) 只支持"用户自备二进制"，
(b) 我们维护一份允许的 sha256 白名单，(c) 首次运行时把观测到的哈希记下来、之后不一致才告警。
**不要写成"下载后去取 checksums.txt 校验"** —— 那是会当场 404 的设计。

### 仍未验（M0 剩余）

**K3 / K5 / K9** 本轮未跑（K3 `filter`+`use` 同用、K5 严格解码的空集合、K9 Job Object 在受限沙箱）。
**K4 / K6 / K8 已在第二批跑完，见下。** 判据与验法见上表。

### 第二批实机验证（K4 / K6 / K8，2026-09-21 同一次 spike）

#### K4 ✅ **成立 —— 四种 `listeners[].proxy` 目标全部可绑定，路由日志确认真的按目标走**

配置了 4 个 lane listener，`proxy` 分别指向不同类型的目标：

| 端口 | `proxy:` 指向 | 绑定 | 内核路由日志 | 客户端看到 |
|---|---|---|---|---|
| 39101 | **裸节点名** `裸节点-香港01` | ✅ | `dial 裸节点-香港01 … --> cp.cloudflare.com:80` | 502（节点是假的不通） |
| 39102 | **带 emoji 的组名** `🚀 节点选择` | ✅ | `dial 🚀 节点选择 … --> cp.cloudflare.com:80` | 502 |
| 39103 | 内建 `DIRECT` | ✅ | `127.0.0.1:5130 --> cp.cloudflare.com:80 using DIRECT` | **HTTP 204** |
| 39104 | **不存在的名字** `这个节点不存在` | ✅（！） | `[Metadata] parse failed: proxy 这个节点不存在 not found` | 502 |

**结论：[04 §7.1] 的线路分配机制成立** —— 裸节点名、emoji 组名、`DIRECT` 都能直接做 lane 的出口，
这正是"通道级线路分配"要的全部能力。**emoji 组名可用**很重要：真实机场订阅的组名普遍带 emoji。

> ⚠️ **K4(d) 是一条危险的附属发现，必须写进实现要求：**
>
> **指向一个不存在的节点名时，listener 照样绑定成功、内核启动零报错**，
> 只在**请求真的到达时**才在内核日志里报 `proxy ... not found`（客户端看到 502）。
>
> **后果**：订阅更新后节点被改名或删除，而我们生成的配置仍指向旧名 ——
> **通道会一直"看起来正常"（端口在听、健康检查也通），直到用户发请求才炸**，
> 而且错误只出现在**内核日志**里，没有对应的 `TA-*` 错误码。
>
> **裁决：`config_gen.rs` 必须自己做引用校验** ——
> `listeners[].proxy` 必须存在于「解析后的订阅节点 ∪ 自有节点 ∪ 组名 ∪ `DIRECT`」之中，
> 否则**生成阶段就报错**（带字段路径），而不是交给内核在运行时静默失败。
> *这一条不能指望内核，它明确不校验。*

> ⚠️ **给 `ta-kernel/src/rest.rs` 的实现要求**：REST 路径里的组名**必须百分号编码**。
> 实测 emoji 组名要写成 `/proxies/%F0%9F%9A%80%20%E8%8A%82%E7%82%B9%E9%80%89%E6%8B%A9`；
> 未编码会请求失败。**真实机场订阅的组名带 emoji 是常态，不是边角情况。**

#### K6 ✅ **成立 —— `mixed-port: 0` 确实关闭默认入站**

配 `mixed-port: 0` + 4 个显式 listener 后，内核的监听只有：
`39090`（REST）+ `39101/39102/39103/39104`（我们定义的 lane）。
**没有多出 `39100`，也没有任何 `0.0.0.0`。**

**结论：I1（入口唯一）可以这样保证** —— 生成配置**永远写 `mixed-port: 0`**，只通过显式 `listeners` 开端口。
这样"内核自己偷偷开一个入站"这条路径被结构性关闭，而不是靠每次 netstat 检查发现。

#### K8 ✅ **成立 —— 而且我们担心的坏事没有发生**

假设是："Windows `taskkill /PID x /T`（不带 `/F`）基本无效 → 实际是强杀 → 若 `store-selected` 只在
优雅退出时落盘，**强杀会导致重启后选中节点丢失**"。实测结果相反：

1. 切组选中项 → `裸节点-日本01`（`GET` 确认）
2. **`Stop-Process -Force`**（等价 `taskkill /F`）
3. 用**同一份配置**重启
4. `GET` 该组 → **`now` 仍是 `裸节点-日本01`** ✅

落盘文件：`<data-dir>/cache.db`（32768 字节）。
**机制**：`profile.store-selected: true` 在**选择发生变更时就写盘**，不是退出时集中写。

**裁决：不需要为"保住选中项"做优雅退出。**
`KernelSupervisor` 的强杀兜底（K9 降级路径）**不会**破坏用户的线路选择 ——
所以退出路径可以继续以"能杀掉就行"为目标，不必引入关闭握手。**这条省掉了一个复杂度。**

### 第三批实机验证（K3 / K5，2026-09-21 同一次 spike）

#### K3 ✅ **成立 —— `filter` / `exclude-filter` 与 `use`（provider）可同时用**

| 组配置 | 实测 `all` | 判定 |
|---|---|---|
| `use` + `filter: "(?i)prov-node-[ab]"` | `[prov-node-a, prov-node-b]` | ✅ filter 生效（正确排除 `prov-node-c-vmess`） |
| `use` + `filter` + 显式 `proxies: [own-node-01, DIRECT]` | `[own-node-01, DIRECT, prov-node-a, prov-node-b, prov-node-c-vmess]` | ✅ 两者叠加：显式节点 + filter 后的 provider 节点 |
| `use` + `exclude-filter: "vmess"` | `[prov-node-a, prov-node-b]` | ✅ exclude-filter 生效 |

> ⚠️ **K3(d)：`filter` 匹配不到任何节点时，组会静默变成 `[COMPATIBLE]`，零报错。**
>
> 实测：`filter: "(?i)^zzz-never-matches"` → `all = [COMPATIBLE]`，内核启动与运行**都没有任何 error/warning**。
> 若我们的 `filter` 正则写错、或订阅更新后节点改名，**这条 lane 会静默走成 `COMPATIBLE`**，
> 而用户完全不知道自己的流量没走代理。
>
> **但好消息：`empty-fallback` 是可配置的**（实测四种取值）：

| `empty-fallback` | 实测 `all` | 路由行为 |
|---|---|---|
| 不写（默认） | `[COMPATIBLE]` | 静默降级 |
| `REJECT` | `[REJECT]` | ✅ **客户端得到 502，内核日志 `using G-nomatch-reject[REJECT]` —— 响亮地失败** |
| `DIRECT` | `[DIRECT]` | 走直连 |

> **裁决：`config_gen` 生成的每一个 `filter` 组都必须显式写 `empty-fallback: REJECT`。**
> 理由：一条"本该走代理的 lane"静默变成直连，是**把用户的境外流量裸奔出去**，
> 比报错严重得多。**宁可失败，不可静默降级。** 这与 K4(d) 是同一类问题的同一个处置原则。
>
> **第 9 轮扩大（员工 E4 提出，主管实测批准）**：E4 主张**推广到我们生成的所有组**（含 `use: [provider]` 的组），
> 理由是"provider 加载失败时 `use:` 组同样会空"。主管起初以 K2 数据（"组当场有节点、不空"）存疑，
> **实测后确认 E4 正确**（见上文的 K2 细化表）：provider **加载失败**是另一个场景，组真的会空。
> **裁决：所有生成的组都写 `empty-fallback: REJECT`。** 只保护 filter 组会在同一个原则上留口子。

#### K5 ✅ **成立 —— 严格解码比担心的宽松，空集合全部被接受**

实测一份刻意"不合规"的配置，内核启动**零 error / 零 warning**：

| 项 | 结果 |
|---|---|
| `find-process-mode: off` | ✅ 接受 |
| `proxy-providers: {}`（空对象） | ✅ 接受 |
| `listeners: []`（空数组） | ✅ 接受 |
| `proxy-groups: []`（空数组）+ `rules: [MATCH,DIRECT]` | ✅ 接受 |
| `select` 组上多余的 `url` / `interval` / `tolerance` / `lazy` | ✅ 接受（不报错） |
| `header` 用**列表形式**的值（`User-Agent: [curl/8.0, Mozilla/5.0]`） | ✅ 接受 |

**结论：我们按模板逐字生成配置时，不必为空集合做特殊处理** ——
"没有订阅"就写 `proxy-providers: {}`、"没有 lane"就写 `listeners: []`，内核照收。
**但"多余字段被接受"是双刃的**：它也意味着**我们写错字段名时内核不会报错**
（与 K4(d)/K3(d) 同一个教训：**内核的沉默不等于配置正确**）。
所以 `config_gen` 的**自校验不能省**。

#### K9 ⏸ **本轮无法验证**

`AssignProcessToJobObject` 在**受限沙箱**下是否可用，需要一台有沙箱限制的机器。
当前开发环境是 danger-full-access，**在这里测出来的结果不能代表受限场景**。
**保留为未验项**，M1 联调时在真实安装后的环境（NSIS 装的、普通用户权限）里补。

### 原始证据（真 JSON）

**未提交**（属 E4 范围，由 E4 收入测试夹具）：`C:\Users\19404\workspace\.ta-tools\m0\fixtures\`
—— `version.json`、`configs.json`、`proxies.json`、`providers-proxies.json`、
`config.yaml` / `config2.yaml` / `config-k4.yaml`、`k4.log` / `k8.log`，
以及 `before.json`（I2 基线）。
`ta-kernel` 的 `parse_snapshot` 应当**直接对这四份真 JSON 做往返测试**，而不是继续用"我以为的内核"；
`config_gen` 的引用校验应当用 `config-k4.yaml` 里那个"不存在的名字"做**负例**。

## 2. M1 · 可用 MVP（约 2 周）

- 单协议网关（OpenAI Chat Completions）+ 全透传保真
- Rust workspace 骨架：`ta-core` / `ta-gateway` / `ta-store` 先立起来（不依赖 Tauri 可测）
- SQLite + 迁移 + `specta` 类型导出打通
- Provider / Credential CRUD（含自定义中转模板）
- Model 映射（1 模型 → 1 通道）
- Lane：`direct` + 1 条订阅线（内核接入最小闭环）
- 托盘：网关开关、复制接入信息、打开面板
- 首启向导 Step 1/3

**验收**：Cursor 与一个 Python SDK 脚本只改 base_url 即可用；关掉 App 后系统网络零残留。

### 2.1 ★ M1 验收清单（第 9 轮按 M0 实测结果收紧，W5 以此判分）

原来只有一句话（"改 base_url 即可用 + 关掉零残留"）。**太笼统，验收时会扯皮。**
下面是可逐条跑的判据。**A 组是产品底线，B 组是 M0 暴露出来的陷阱，C 组是本次新增的可观测性。**

#### A 组 · 产品底线（做不到就不算 M1）

| # | 判据 | 怎么跑 |
|---|---|---|
| **A1** | **基线场景真的走通** —— [03 §0]：同一个逻辑模型挂两个套餐，**先用 A（直连），A 额度耗尽后自动切到 B（走梯子）**，且同一会话保持粘性 | 配两个 credential 指向同一 model（一个 lane=direct、一个 lane=走代理）；把 A 的额度设小 → 跑请求直到切到 B → **断言容器/出口确实变了**（不是只看日志说切了） |
| **A2** | **全透传保真** | 用 Cursor + 一个带图的请求：① 上游**真的收到** `image_url`（含 `detail`）② `x-stainless-*` / `anthropic-beta` / 自定义 UA **逐字节到达上游** ③ 未知顶层字段与未知 part 类型不丢 |
| **A3** | **关掉 App 后系统网络零残留** | `scripts/check-invariants.ps1 -Mode Compare` → **exit 0**（判据是"前后一致"，不是"必须是关的"） |
| **A4** | **入口唯一** | 全程无 `0.0.0.0` 监听；`mixed-port: 0` + 只开显式 `listeners` |

#### B 组 · M0 暴露的静默降级陷阱（**这一组是新增的，因为内核会沉默地做错事**）

| # | 判据 | 依据 |
|---|---|---|
| **B1** | **lane 不可用时必须报错，绝不静默走直连** | K3(d)：`filter` 匹配不到节点 → 静默变 `[COMPATIBLE]`。修法：所有生成的组写 `empty-fallback: REJECT` |
| **B2** | **provider 加载失败时不得对外报"内核已就绪"** | K2 细化：provider 文件缺失/为空时内核**照常启动、REST 照常通**，而所有 lane 静默降级。就绪判定必须断言每个 provider 的 `proxies[]` 非空 |
| **B3** | **`listeners[].proxy` 的引用在生成阶段就被校验** | K4(d)：指向不存在的名字时内核照样绑定成功、零报错，只在请求到达时才炸 |
| **B4** | **内核自身的报错出现在我们的日志里** | 内核已经在 `level=error` 报了（"file doesn't have any proxy"），但那份信息在我们看不见的地方 → 必须转发进 `tracing` |

> **B 组的共同主题（这是 M0 最值钱的产出）**：
> **内核在所有可疑情况下都倾向于"沉默地降级"而不是报错** ——
> 不存在的节点名照样绑定、匹配不到的 filter 变 COMPATIBLE、加载失败的 provider 不阻止启动、
> 多余/写错的字段照单全收。
> **所以凡是我们能在生成或启动阶段判定的，就必须自己判定，并且选"响亮地失败"那一侧。**

#### C 组 · 可观测性（本次用户要求，[14]）

| # | 判据 |
|---|---|
| **C1** | 出问题时**日志文件存在于确定路径**，用户能一键打开（不再是"托盘应用没有控制台"） |
| **C2** | **能按 `TA-xxxx` 错误码检索日志** |
| **C3** | **日志与诊断包里搜不到用户的密钥**（有自动化测试） |
| **C4** | 拿一个 `request_id` 能筛出跨 crate 的完整轨迹 |

#### D 组 · 未验项（M1 期间必须补，不能在 W5 才发现）

| # | 项 | 说明 |
|---|---|---|
| **D1** | **K9**：`AssignProcessToJobObject` 在**受限沙箱**下是否可用 | 开发环境是 full-access，测了不代表受限场景。**必须在 NSIS 安装后的普通用户环境里验** —— 这是"孤儿内核进程"的唯一防线 |
| **D2** | 真实机场订阅（脱敏）的解析一致性 | L19：夹具不能是从断言反推的 |

## 3. M2 · 网络完整（约 2 周）

- 内核接入：sidecar 起停看护、配置生成、lane → `listeners[].proxy` 映射、REST 控制
- 订阅业务层五件事：UA/自定义头、`userinfo` 与更新周期、版本与回滚、订阅级过滤、把订阅分组拉出来变成可选线路
- 订阅坏更新的兜底（HTML/空列表检测、节点数异常拒绝采纳）
- 五种 Lane 完整：`direct` / `subscription_auto` / `subscription_group` / `subscription_node` / `filter`
- **线路分配**：通道矩阵里的线路下拉 + 网络页「分配」Tab（批量）+ 五级默认解析 + 反向视图（这条订阅被谁用了）
- 健康检查与降级（读内核测速结果）、节点管理页

**验收**：
- 导入一个真实的复杂订阅（含嵌套组、按 UA 分发、未知协议节点），节点数与分组数与 Clash 客户端一致
- 把订阅里某个在用节点禁掉，60s 内自动换节点，客户端无感
- 给两个模型分别指两条不同线路（一条订阅分组、一条直连），`curl` 出口 IP 不同
- 把订阅 URL 改成一个返回 HTML 的地址，触发更新 → 旧节点仍在，UI 标红说明原因

## 4. M3 · 策略与额度（约 2–3 周，本产品的核心价值）

- ChannelGroup + 全部选择器 + 粘性表
- 额度账本（四观测源）、状态机、软/硬限额、重置窗口
- 故障转移（含重试边界）、冷却与退避
- 策略规则表 UI + **策略模拟器**
- 成本中心（token 类型归一化 + 定价表 + 查询构建器 + 预置视图）+ 请求日志三档开关 + CSV 导出

**验收（逐条可测，见 §6）**：
- 套餐 A 耗尽 → 自动切 B → 同会话后续请求保持 B（粘性）
- 带图请求遇到被标 `no-vision` 的通道 → 跳过它；若全部候选都被排除 → 415 + 明确原因（而不是静默发过去）
- 断掉订阅节点 → 自动切换到备用通道，trace 里能看到完整尝试链

## 5. M4 · 多协议与打磨（约 2 周）

- Anthropic / Gemini / OpenAI Responses 协议适配 + IR 路径 + 字段映射核对
- 跨协议图片块映射（`image_url` data URL ↔ Anthropic `source.base64`）；无法映射时直接拒绝
- 头部差异自检与告警（`header_diff` 非空即说明网关有 bug）
- 通知、开机自启、CLI、便携模式、安装包（Windows 优先，macOS 次之，Linux 最后）

## 6. 验收标准（写进 CI，可自动断言）

| # | 标准 | 测法 |
|---|---|---|
| A1 | **全透传保真**：同协议请求除 4 处协议必需修改外逐字段/逐头一致 | golden test：含 `image_url`(带 `detail`)、未知 part 类型、未知顶层字段、`anthropic-beta`、自定义 UA 的请求，diff 网关进出口 body 与 header |
| A2 | **视觉不被文本化**：跨协议转换无法映射图片块时直接拒绝，绝不替换成文本 | 构造 Anthropic 目标协议不支持的图片格式，断言返回 4xx 且上游未收到请求 |
| A3 | **标识不吞**：客户端 UA / `x-stainless-*` / `x-session-id` 原样到达上游 | 起一个 echo 上游断言收到的头 |
| A4 | **额度切换**：`used >= limit` 后下一请求换通道；同会话后续保持新通道 | 打桩账本 + 发 5 个同 session 请求，断言 channel 序列 |
| A5 | **不重复计费的重试边界**：流式已输出后绝不重试 | 打桩"上游在输出 3 个 chunk 后断连"，断言只有 1 次上游请求 |
| A6 | **系统网络零副作用** | [04 §9](04-network-core.md) 的快照 diff 脚本 |
| A7 | **隔离监听** | `netstat` 断言整机只有 `127.0.0.1:8787` / `8788` 在监听，无 `0.0.0.0` 监听，无子进程残留 |
| A8 | **策略可解释** | 模拟器输出与真实 trace 的通道选择一致（同一输入比对 100 次） |
| A9 | **配置健壮** | 订阅返回 HTML/超时/空列表时保留上版配置，不洗掉好节点 |
| A10 | **节点故障自愈** | 把在用的节点 IP 改成黑洞地址，10s 内在途请求切到下一节点，托盘状态经历红→绿 |
| A11 | **开机自启动可逆且诚实** | ① 默认**关闭**：全新安装后查系统无任何启动项（E1）；② 开启→系统条目存在且命令正确；③ 关闭→条目消失；④ **手动删掉条目后重开 App → UI 显示 `drifted` 并提示，且不自动注册**（E3/E4）；⑤ 卸载后无残留条目（E2） |
| A12 | **不变量检查覆盖 autostart** | `scripts/check-invariants.ps1` 记录 autostart 条目；存在性变化时必须能在应用日志里找到对应的用户操作（E5） |

## 7. 风险登记表

| # | 风险 | 影响 | 概率 | 缓解 |
|---|---|---|---|---|
| R1 | **mihomo 的 GPL-3.0 与分发冲突** | 高（可能必须开源或不能闭源） | 中 | 默认 `net.kernel.provider: external`（用户自行获取内核，首启向导引导下载）；项目本身可选择 GPL-3.0 兼容；**不引 `tauri-plugin-mihomo`、不拷 Clash Verge Rev 代码** |
| R2 | `listeners` 热重载受限（改 lane 集合要重启内核） | 中（切换瞬断） | 中 | 请求排队 1s + 直连通道降级放行；lane 集合尽量固定，切节点走 REST 不重启 |
| R3 | 机场按 User-Agent 分发不同格式，拿错格式导致订阅导入失败 | 中 | 高 | `proxy-providers[].header` 下发 UA，默认伪装 Clash 客户端；失败时提示"试试换个 UA"；保留 `raw` 原文可重试 |
| R4 | 部分上游/中转封锁数据中心 IP 或代理 IP | 中（某些通道永远连不上） | 高 | lane 级故障转移 + 通道健康降级；提供 `direct` lane 兜底；不做"绕过风控"的功能 |
| R5 | 重试导致重复计费 | 中 | 中 | 严格重试边界 + trace 标注 `possible_double_billing` + 默认 `max_attempts: 3` |
| R6 | 上游 API 字段/协议频繁演进 | 中 | 高 | 全透传（不依赖字段白名单）；`header_diff` 自检；跨协议映射失败时拒绝而非静默丢弃 |
| R7 | 机场订阅被污染 / 节点域名解析不出来 | 中 | 中 | `proxy-server-nameserver` 走干净 DoH；坏更新保留上版 + 版本回滚 |
| R8 | Windows 托盘动态菜单在 Tauri 上有平台差异（Linux 托盘菜单支持弱） | 低 | 中 | 以 Windows/macOS 为一等公民；Linux 提供 AppIndicator 回退 |
| R9 | 用户配置复杂到看不懂 | 中（产品失败） | 中 | 首启向导 + 通道矩阵 + 策略模拟器；默认配置开箱即用 |
| R10 | 安全：本地网关被同机其它程序盗用 | 中 | 低 | 默认绑定 127.0.0.1 + 可选本地密钥 + 管理 API 独立 token；日志脱敏 |
| R11 | 内核进程被安全软件拦截或误杀 | 中 | 中 | 不装 TUN、不提权；提供 `net.kernel.provider: external`；看护与友好提示 |

## 8. 如果只想先跑起来（最小惊喜路径）

按"投入产出比"排序，前三件做完就能明显改善现状：

1. **托盘 + 一键切 base_url**（把"改客户端配置"这件事消灭）
2. **Lane 隔离 + 模型级线路选择**（把"挂梯子 + 代理不穿透"消灭）
3. **通道组 + 粘性 + 额度切换**（把"手动换 key"消灭）

第 4 件（策略引擎 + 模拟器）是长期价值，可以晚做。

## 9. 与同类产品的位置

| 产品 | 面向 | 有无托盘 | 有无网络内核 | 有无额度切换/粘性 | 多模态保真 |
|---|---|---|---|---|---|
| TokenHub / one-api / new-api | 多用户中转站运营 | 无 | 无（靠系统代理） | 渠道优先级，无粘性 | 取决于实现，常见丢字段 |
| LiteLLM Proxy | 团队/服务端网关 | 无 | 无 | 有路由但无"个人套餐额度"概念 | 好 |
| Claude Code Router | 单客户端模型路由 | 无 | 无 | 无 | 一般 |
| Clash Verge | 网络代理 | **有** | **有** | 不涉及 | 不涉及 |
| **TokenAgency** | **单机单人的 LLM 入口** | **有** | **有** | **有** | **直通优先** |

---

# 10. Backlog 与待决（明确不在本轮范围）

> PM 的范围纪律：**任何"顺手也做了"的提案默认进这里，不进本轮。** 本轮只认"跑通 M1 并出安装包"。

## 10.1 来自基准场景的两个增强（[03 §0](03-routing-and-policy.md)）

| # | 项 | 为什么有价值 | 代价 |
|---|---|---|---|
| B1 | **lane 增加 `auto_reachable`**：先试直连，超时/连接失败就自动走代理，结果缓存 10 分钟 | 基准场景里直连/走梯子很明确（字节直连、command code 要梯子），手配没问题。但 5 个套餐里 3 个能直连时，手配容易漏 | 需要"探测—缓存—重试"三段逻辑；且要防"每次切换都先撞一次超时"拖慢首字节 |
| B2 | **套餐未配额度时的显式提醒**（UI 强制提示，不允许静默） | **不填 `limit` 就无法"用完自动切"** —— 这是基准场景的前提。静默会让用户以为功能坏了 | 低。但要在套餐列表、通道矩阵、首启向导三处一致提示 |

## 10.2 员工交付时提出的待决项

| # | 来源 | 项 | 裁决 |
|---|---|---|---|
| D1 | P4 | `usage_request` 增补 `usage_source` 列（文档草案未列） | ✅ **批准**。落实 [11 §6](11-internal-contracts.md) 铁律 4"不猜不估算"，没有它成本中心无法诚实标注"该通道未提供细分"。已补进 [02 §3](02-config-and-data-model.md) |
| D2 | P4 | 日志模式 `off` 时 `filters.status=retried` 判不了（`usage_request` 无 attempts 列），P4 按"未知即 ok"处理 | ⚠️ **保留实现，UI 层补诚实提示**：日志模式为 `off` 时，日志页禁用"重试"筛选项并提示"需开启请求日志才能判断重试"。**不要在未知的情况下把它算进 ok** —— 那是拿不确定冒充确定 |
| D3 | P3 | `pricing` 的 per-entry `unit` 从 `extra` 读取（`PriceEntry` 无 `unit` 字段） | 🔶 暂留。若将来第三方定价表需要混用单位，再提升为正式字段 |
| D4 | P7 | `gateway.local_key` 未纳入明文扫描/迁移（改引用需 ingress 先支持引用解析） | 🔶 顺延到 E5 网关接线后一并处理 |
| D5 | P7 | Windows ACL 收敛未做（需额外依赖） | 🔶 已注明在 `SECURITY-NOTES.md`；v2 加密后文件内容是 AEAD 密文，风险可控 |

## 10.3 已知但本轮不做

- 内建拨号 `BuiltinDialer`（零依赖单进程构建的保留路线）
- TUN / 系统代理高级模式
- 跨协议 IR 的完整覆盖（G3 先做能跑的最小集）
- macOS / Linux 打包与自动更新
- 用户脚本引擎（[ADR-11](09-architecture-design.md) 明确 v1 不做）

---

# 11. 已知遗留（第 10 轮收口时确认，**交接给新团队**）

> **第 11 轮更新：§11.1 与 §11.2 均已解决。**
> 原文与"已排除项"表**刻意保留** —— 那张表省掉的是最贵的那段时间，
> 也是下次遇到同类问题时的第一手材料；同时它记录了一次**判错方向**的完整过程。

## 11.1 ✅ 已解决：`tokenagency` 的 lib 测试目标无法运行（Windows）

**症状（原文保留）**：`cargo test -p tokenagency --lib` 的 harness 在**加载阶段**就崩，
`exit code: 0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`，一个测试都没跑。

**影响**：`cargo test --workspace` 因此永远红。丢掉的测试是
`src-tauri/src/{tray,autostart,lifecycle}.rs` 里的单元测试 —— 其中
**自启动漂移的四条分支**与**"state() 只读不写"**是**唯一**能验证
[docs/09 §1.2] E1–E5 的自动化证据。**这不是可以放着不管的测试。**

**已排除的可能**（都做过实验，不是推断）：

| 假设 | 实验 | 结果 |
|---|---|---|
| 并发构建污染产物 | `cargo clean`（移除 32.4 GiB / 54063 文件）后全量重建 | 仍然失败，**确定性** |
| `crate-type` 里的 `cdylib` | 临时改成 `["rlib"]` 重建 | 仍然失败 |
| 同名 DLL 遮蔽（`tokenagency_lib.dll`） | 删掉 deps 与 debug 下的同名 DLL 后直接运行 | 仍然失败 |
| 缺 `WebView2Loader.dll` | 把 `webview2-com-sys` 的 x64 那份复制到 exe 旁 | 仍然失败 |
| PATH 上有旧版 `WebView2Loader.dll`（Windows Kits 里有一个） | 把 `Windows Kits` 从 PATH 剔除后运行 | 仍然失败 |
| 依赖版本被顺带升级 | 逐行读 `Cargo.lock` 的 diff | **只新增了一行 `"sqlx"`**，无版本变更 |
| 应用本身也起不来 | `TA_EXPORT_BINDINGS=1 cargo run -p tokenagency` | **能正常加载并执行** —— 只有 lib 测试 harness 不行 |

**根因（第 11 轮查明，可复现）**：

`dumpbin /imports <harness.exe>` 逐符号与系统 DLL 的 `/exports` 比对后，
**唯一**缺失的入口点是 `comctl32.dll ! TaskDialogIndirect`，其余 23 个 DLL 全部解析成功。

`TaskDialogIndirect` **只由 `ComCtl32` v6 导出**，而 v6 必须靠应用程序清单里的
`<dependentAssembly>` 才会被激活；没有清单时 Windows 加载 v5 → 该符号不存在 →
进程在加载期以 `0xc0000139` 退出。**这不是构建问题，是激活上下文问题。**

清单只有 bin 有：`tauri-build` 把它塞进
`cargo:rustc-link-arg-bins=...\out\resource.lib`。T1 引入 `tauri-plugin-dialog`
（→ `rfd` → `TaskDialogIndirect`）后，lib 测试 harness 第一次需要 v6，
于是"**旧 harness 能跑、新 harness 不能**"。

**为什么之前一直没找到**：把"加载即崩"当成了**构建产物/环境**问题去试
（清缓存、换 `crate-type`、找同名 DLL、找 `WebView2Loader`）—— 而它们与导入表无关。
正确入口是**读导入表并逐个符号去系统 DLL 里验证是否存在**，
而不是在构建系统里排列组合。

**修法**（`src-tauri/build.rs`、`src-tauri/windows-app-manifest.xml`）：

1. `build.rs` 用 `cargo:rustc-link-arg=/MANIFEST:EMBED /MANIFESTINPUT:<清单>`
   把清单供给**所有**可链接目标；
2. 同时用 `tauri_build::WindowsAttributes::new_without_app_manifest()` 让
   `tauri-build` **停止**往 `resource.lib` 里放清单资源 —— 否则 bin 会同时拿到
   `resource.lib` 和链接参数两份 MANIFEST，链接期直接
   `CVTRES CVT1100（资源重复）+ LNK1123`。图标与版本信息仍来自 `resource.lib`，
   不受影响；
3. 清单文件**必须纯 ASCII**：`mt.exe` 把无 BOM 的 UTF-8 当 ANSI 解析，
   注释里有中文就报 `c1010070 Failed to load and parse the manifest`；
4. `cargo:rustc-link-arg-tests` **覆盖不到** lib 单元测试 harness（它的 target kind
   是 `lib`，不是 `[[test]]`），只有通用 `rustc-link-arg` 能到达 —— 这也是为什么
   必须走第 2 步。
5. 新增 `src-tauri/tests/runtime_manifest.rs`：从自身映像读 `RT_MANIFEST`，
   断言其中声明了 Common-Controls 6.0 —— 谁再把清单弄丢，门禁会直接红，
   而不是等到某个测试 harness 神秘地加载失败。

**这条经历真正值得记住的**：`--exclude tokenagency` 一解开，
**第一个跑起来的测试就是红的** —— `plugins::tests::twelve_plugins_with_unique_ids`
的数字还停在"移除 `tauri-plugin-log` 之前"。**被绕过的门禁不会变绿，只会积累。**

**不要**用 `[lib] test = false` 把它藏起来 —— 那会让上面那些测试**静默地永不运行**，
比一个已知会红的门禁更危险。（第 11 轮已彻底解决，此条从"遗留"转为"已关闭"。）

## 11.2 ✅ 已解决：绑定导出被 specta 的两条限制拦住

> **第 10 轮收口时已修复并端到端验证**（`gen-bindings.ps1` 成功产出 44717 字节的 `bindings.ts`）。
> 下面是**根因与解法**，留着是因为这两条限制很反直觉，后来者一定会再撞上。

**当时的两条错误（同一类问题的两个面）**：

1. `Attempted to export "…i64.0" but Specta forbids exporting BigInt-style types`
   —— `specta-typescript 0.0.12`（**crates.io 上的最新版**）在 `primitives.rs` 里
   **硬编码**拒绝 `i64/u64/usize/isize/i128/u128`，**没有任何配置项能放行**。
   而我们的 DTO 里满是 Unix 毫秒时间戳（`i64`）与 token 计数（`u64`）。
2. 修完 1 之后紧接着：`Found infinitely recursive inline named reference …
   serde_json::Value -> Vec<Value> -> Value`
   —— `serde_json::Value` 是**递归类型**，specta 展开它会产出无限的 TS 类型。

**解法（两处，缺一不可）**：

| # | 做法 | 位置 |
|---|---|---|
| 1 | **`Builder::dangerously_cast_bigints_to_number()`** —— `tauri-specta` 为这件事留的唯一开关 | `src-tauri/src/commands/mod.rs` 的 `specta_builder()` |
| 2 | 给**真正任意的 JSON** 字段加 **`#[specta(type = specta_typescript::Any)]`** | `IpcError.details`、`Transform.body_patch`、`TraceDetail` 的 body/header 字段、`TraceRecord` 的 body 字段 |

**为什么第 1 条可以接受**（它名字里带 "dangerously"）：
TS 的 `number` 是 f64，安全整数上限 `2^53 ≈ 9.0e15`；而我们的取值是
**Unix 毫秒 ≈ 1.8e12**（差 3 个数量级）、单次请求 token 数与耗时毫秒 —— **都不会丢精度**。
但这是**有前提的豁免**：将来若新增可能超过 `2^53` 的整数字段（纳秒时间戳、累计字节数），
**该字段必须单独处理**，而不是继续吃这个全局开关。已写进 `specta_builder()` 的文档注释。

**顺带做对的两件事**：

- `header_diff` 原本是 `Option<serde_json::Value>`，**它的形状是已知的** →
  移到 `ta_core::trace::HeaderDiff`（`ta-gateway` 生产、`ta-store` 落库/导出**共用一份定义**，
  避免两个 crate 各写一份而静默漂移）。TS 侧因此拿到 `header_diff?: HeaderDiff | null` 而不是 `any`。
- `extra: Map<String, Value>` 那几十个字段**早就有 `specta(skip)`**（E1 的设计），
  所以它们不参与导出、不需要 `Any`。**真正需要 `Any` 的只有 4 处任意 JSON 字段。**

**"契约缩水"护栏**：生成物会**直接覆盖** `src/shared/bindings.ts`，而那里是 F1 手写的
**105 条**命令契约，T1 目前只注册了 **31 条** —— 直接覆盖会让前端 `npm run build`
报一堆 TS2305（实测过）。`gen-bindings.ps1` 现在会做**名字差集**并逐条列出缺哪 75 条，
同时把上一版备份到 `.bak`。

> **踩过的三个坑，值得记**：
> ① 用"文件里某个函数的出现次数"当计数指标是**无效**的 —— 生成物是 `__TAURI_INVOKE("name")`、
>    手写是 `CommandMap` 的键，同一个模式数两边只会得到 0；
> ② 在 `"` 后面要求 `)` 只会匹配到**无参数**的命令（31 被数成 14）；
> ③ `src-tauri` **没有 `specta` 这个 feature**（依赖是常开的），
>    所以 `#[cfg_attr(feature = "specta", specta(...))]` 那条属性**根本不会生效**，
>    还会附赠一个 `unexpected cfg condition value` 警告。

**当时报的第一条错误（留档）**：

```
export bindings: Attempted to export "header_diff.Number.0.i64.0" but Specta forbids
exporting BigInt-style types (usize, isize, i64, u64, i128, u128)
While inlining: -> ta_store::trace_store::TraceDetail_Serialize
  header_diff -> serde_json::Value
```

**当时试过但走不通的路（别重走）**：`specta-typescript 0.0.12` 的 `Typescript` 构建器
**没有** bigint 配置方法（只有 `new/header/layout/branded_type_impl/export/export_to`）——
所以"把 bigint 行为设成 Number"这条路**在 `specta-typescript` 这一层**不存在。
真正的开关在**上一层**：`tauri_specta::Builder::dangerously_cast_bigints_to_number()`。

**`src/shared/bindings.ts` 的两个性质**（接手的人必须知道）：
它**被 `.gitignore` 忽略、从未进版本控制**，而且当前内容是**手写的 105 条命令契约**。
所以"生成绑定前一定要先备份" —— `gen-bindings.ps1` 现在会自动备份到 `.bak`，
并且会做**名字差集**告诉你缺了哪 75 条（见下一段的护栏）。

**已存档一份**：`docs/contracts/bindings-handwritten-reference.ts.txt`
（**后缀是 `.ts.txt` 不是 `.ts`** —— 否则 `tsc` 会把它当源码一起编译，把前端类型检查搞红）。
它的用途是做"95 vs 31"的差集对照；T1 补齐全量命令面之后即可删除。

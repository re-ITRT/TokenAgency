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
| K3 | `proxy-groups` 上的 `filter` / `exclude-filter` 与 `use`（provider）可同时用 | `filter` lane 全部失效 | 生成一个带 filter 的组，看节点数与正则是否吻合 |
| K4 | `listeners[].proxy` 可直接指**裸节点名**与**订阅自带组名（含 emoji）** | 线路分配的核心机制失效（docs/04 §7.1 全靠这条） | 两种各配一条 lane，`curl -x` 验证出口 |
| K5 | 严格解码：`select` 组多余的 `url/interval/tolerance`、`header` 用列表形式 `User-Agent: [...]`、`find-process-mode: off`、空集合 `proxy-providers: {}` / `proxy-groups: []` / `listeners: []` 都被接受 | 内核启动失败（当前按模板逐字写，未证实空集合） | 逐项最小配置试起 |
| K6 | `mixed-port: 0` 确实等于"关闭默认入站" | 会多一个监听端口 → 违反 I1 入口唯一 | `netstat` 断言 |
| K7 | ⚠️ **已验证（见 §1.6）：字段名全部真实存在，但有 4 处偏差会让 `parse_snapshot` 解析错误** —— `providers` 是**对象不是数组**、`type` 是**大写** `Selector`/`Shadowsocks`、**provider 节点不在 `/proxies` 顶层**（组 `all` 会悬空）、内置伪节点 `DIRECT/REJECT/PASS/COMPATIBLE…` 混在列表里 | 我的 wiremock 测的是"我以为的内核"。**这是全清单里最需要重验的一条** —— UI 的线路下拉完全依赖它 | 用 P5 提供的 `RawSnapshot::fetch` dump 真 JSON，与假设逐字段对比 |
| K8 | Windows `taskkill /PID x /T`（不带 `/F`）对控制台程序基本无效 → 实际是 2s 后强杀 | 若 `profile.store-selected: true` 的选中项需优雅退出才落盘，**强杀会导致重启后选中的节点丢失** —— 直接破坏"线路分配"的用户体验 | 切节点 → 杀 → 重启 → 看选中是否保留 |
| K9 | `AssignProcessToJobObject` 在受限沙箱里可用 | 失败会降级为 warn + `taskkill /T` 兜底，孤儿进程风险 ↑ | 在有沙箱的机器上确认 |

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

**K3 / K4 / K5 / K6 / K8 / K9** 本轮未跑完（K6 `mixed-port: 0`、K4 裸节点名+emoji 组名做 lane `proxy`、
K8 强杀后选中项是否落盘，这三条对 lane 机制影响最大）。**判据与验法见上表，M1 联调时补。**

### 原始证据（真 JSON）

**未提交**（属 E4 范围，由 E4 收入测试夹具）：`C:\Users\19404\workspace\.ta-tools\m0\fixtures\`
—— `version.json`、`configs.json`、`proxies.json`、`providers-proxies.json`，
以及 `config.yaml` / `config2.yaml` / `providers/lane.yaml` 与 `before.json`（I2 基线）。
`ta-kernel` 的 `parse_snapshot` 应当**直接对这四份真 JSON 做往返测试**，而不是继续用"我以为的内核"。

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

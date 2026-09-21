# 04 · 网络模型

## 0. 结论：走内核（评审后修订）

本文经历过两次修订，最终定稿如下：

| 版本 | 数据流 | 结论 |
|---|---|---|
| 初版 | 客户端 → 网关 → 本机第二个端口 → **外部内核进程** → 节点 | 被否：当时认为多一跳没价值 |
| 二版 | 客户端 → 网关 → **进程内自研协议栈** → 节点 | 被否：**"完全兼容 Clash 订阅"这个需求把它变成了最贵的方案** |
| **定稿** | 客户端 → 网关:8787 → 本机 lane 端口 → **mihomo sidecar** → 节点 | ✅ 见下 |

**为什么回到内核**：一旦把"完全兼容 Clash 订阅"列为硬需求，自研方案的账就算不过来了——

- 自研要写：全协议全字段 parser + 协议栈 + 分组语义 + `proxy-providers` + 规则解析。**2–3 周只是起步，之后是长期维护**（机场每周都在加新协议和新混淆插件）。
- 用 mihomo：这些**全都有，而且免费**，内核升级即得。

多一跳 loopback 的实测开销约 **0.1ms**，在 LLM 请求（秒级）里完全不可见。用可忽略的性能代价，换掉整个项目最大的风险项和维护负担，这是划算的。

| | 谁实现节点协议 | 本地监听端口 | 进程数 | Clash 兼容 |
|---|---|---|---|---|
| 系统代理 / TUN | 外部代理软件，靠系统劫持 | 代理软件那一个 | ≥2 | 全，但**劫持全机** |
| 自研拨号 | TokenAgency 自己 | 只有 8787 | 1 | **要自己写，最贵** |
| **定稿：内嵌内核** | mihomo sidecar | 8787 + 39101…（**仅 loopback**） | 2 | **免费** |

关键区别在于：**我们不劫持任何东西**。那几个 `39101+` 端口只监听 `127.0.0.1`，只有 TokenAgency 自己往里发请求。系统代理、路由表、DNS 一律不碰。

## 1. 三种模型对比

| 模型 | 数据流 | 代价 |
|---|---|---|
| **A 系统代理 / TUN** | 任意程序 → 系统劫持 → 代理软件 → 节点 | 影响全机、要管理员权限、与 VPN/加速器冲突、DNS 被接管 |
| **B 自研内嵌拨号** | 客户端 → 网关 → 进程内协议栈 → 节点 | 三跳最省，但**要自己维护协议栈与订阅解析**（最大成本项） |
| **C 内嵌内核 sidecar**（定稿） | 客户端 → 网关:8787 → lane loopback 端口 → 内核 → 节点 | 多一跳（~0.1ms）、多一个进程、GPL 分发需处理；换来全部 Clash 生态 |

C 的实现关键：每条 lane 对应内核配置里的一个 `listeners` 入站（`type: mixed`，`listen: 127.0.0.1`，`proxy: <组名>`），网关侧每个 lane 持有一个绑定该端口的 HTTP client。

## 2. 数据流（具体到调用）

```
POST http://127.0.0.1:8787/v1/chat/completions
  → axum handler
  → 路由决策：channel → lane → 节点选择由内核负责
  → 取该 lane 的 client（已绑定 http://127.0.0.1:3910x）
  → client.execute(request)          // 内核在该端口上做 HTTP CONNECT 并转发
  → 读响应 → （流式）逐块转发给客户端
```

`netstat` 视角：`127.0.0.1:8787`（网关）、`127.0.0.1:8788`（管理 API，仅供 CLI）、`127.0.0.1:39090`（内核 REST）、`127.0.0.1:3910x`（lane 入站）。**没有任何 `0.0.0.0` 监听。**

## 3. OutboundDialer 抽象（保留，作为后路）

```rust
#[async_trait]
pub trait OutboundDialer: Send + Sync {
    /// 把请求通过某条 lane 发出去
    async fn execute(&self, lane: &Lane, req: UpstreamRequest) -> Result<UpstreamResponse>;
    async fn health(&self, lane: &Lane) -> HealthReport;
}
```

| 实现 | 状态 | 说明 |
|---|---|---|
| `KernelDialer` | **默认** | 每个 lane 一个 reqwest `Client`，绑定该 lane 的 loopback 端口 |
| `BuiltinDialer` | 未实现，留后路 | 将来若要做"零依赖单进程构建"，在这里补自研协议栈即可 |

保留抽象不是为了架构好看，而是：**上层路由、UI、配置完全不感知实现**。真有一天需要单进程版本，改一个 `net.dialer` 配置值，其余一个字不动。

## 4. 内核负责什么、我们不做什么

| 能力 | 谁做 |
|---|---|
| 节点协议（SS/VMess/VLESS/Trojan/Hysteria2/TUIC/…） | **内核** |
| Clash 订阅解析（L1 节点 / L2 分组 / L3 规则） | **内核** |
| 节点测速、`url-test` / `fallback` / `load-balance` / `select` | **内核** |
| `proxy-providers` 定时更新 | **内核** |
| 按模型/通道选线路 | **我们** |
| Lane 定义与分配（谁走哪条线，或直连） | **我们** |
| 额度账本、粘性、故障转移、成本中心 | **我们** |
| 订阅的"业务层"处理（见 §5.3） | **我们** |
| 出站拨号（经内核入站） | **我们**（`KernelDialer`） |

这张表很重要：**它划清了边界，也说明为什么自研是亏的**——内核那一半是纯苦力活，我们那一半才是产品价值所在。

## 5. Clash 订阅兼容

"完全兼容 Clash 订阅"这个需求，我们拆成三级来定义**范围**。**100% 兼容 Clash 等于重写一遍 mihomo**——所以做法是：范围按三级说清楚，**实现全部交给内核**（见 §5.2）。

### 5.1 兼容范围（三级定义）

| 级别 | 范围 | 现状 |
|---|---|---|
| **L1 节点级** | `proxies:` 里所有主流协议 + 全部常见可选字段，**能连上**（覆盖 95% 机场的实际使用） | ✅ 内核提供 |
| **L2 结构级** | `proxy-groups`（含嵌套、`use` / `filter` / `exclude-filter` / `include-all-*`）、`proxy-providers`（含 `override` / `health-check`）。保留机场自己的分组语义——"香港""日本""自动选择"这些组名就是最好用的线路选择器 | ✅ 内核提供 |
| **L3 规则级** | `rules` / `rule-providers` / `GEOIP` / `GEOSITE` / `RULE-SET` / `MATCH`。**我们读进来展示，但不执行**（原因见 §5.5） | ✅ 内核能解析，我们主动不用 |

### 5.2 内核免费提供的（当初我们打算自己写的全部）

| 原计划 | 现在 |
|---|---|
| L1：所有协议 + 全部可选字段（SS 的 `2022-blake3`、`plugin` 系列；VMess 的 `ws-opts`/`grpc-opts`/`smux`；VLESS 的 `reality-opts`/`flow`；Hysteria2 的 `obfs`/`up`/`down`；TUIC 的 `congestion-controller`…） | ✅ mihomo 原生 |
| L2：`proxy-groups`（含嵌套、`use` / `filter` / `exclude-filter` / `include-all-*`）、`proxy-providers`（含 `override` / `health-check`） | ✅ mihomo 原生 |
| L3：`rules` / `rule-providers` / `GEOIP` / `GEOSITE` / `RULE-SET` | ✅ mihomo 原生 |
| 订阅定时更新、节点测速、`url-test` / `fallback` / `load-balance` / `select` | ✅ mihomo 原生 |
| 格式解析：Clash YAML、只有 `proxies` 段、base64 链接列表、大小写变体 | ✅ mihomo 原生 |
| **原来估算的 2–3 周 parser + 长期协议维护** | **归零** |

> 这就是 [08 §0.2](08-tech-stack.md) 里说的："一旦'完全兼容 Clash 订阅'变成硬需求，自研方案的账就算不过来了。"

### 5.3 我们仍然必须自己做的五件事（业务层）

| # | 事情 | 怎么做 |
|---|---|---|
| 1 | **UA 与自定义头** | ★ 大坑：很多机场按 `User-Agent` 返回不同格式（clash / surge / v2ray）。在 `proxy-providers[].header` 里下发，默认伪装成 Clash 客户端，UI 可改 |
| 2 | **订阅业务状态** | `Subscription-Userinfo` 响应头 → 流量与到期；`Profile-Update-Interval` → 建议更新周期。内核不管这些，要我们解析后存库、显示在托盘 |
| 3 | **版本与回滚** | `keep_versions: 3`。**坏更新绝不清空**：更新前后比对节点数/组数，异常则回滚到上一版并标红（对应验收 A9） |
| 4 | **订阅级过滤** | 下发到 `proxy-providers[].filter` / `exclude-filter`，剔除"剩余流量/到期/官网/防失联"这类假节点 |
| 5 | **把订阅里的组拉出来变成可选线路** | 通过 REST 读 `GET /providers/proxies/{name}` 拿到节点与组列表 → 在 UI 里做"订阅 → 线路"的映射（见 §5.4、§7.1） |

### 5.4 线路 = 订阅里的组

机场订阅通常长这样：

```yaml
proxy-groups:
  - name: 🚀 节点选择
    type: select
    proxies: [♻️ 自动选择, 🇭🇰 香港, 🇯🇵 日本, DIRECT]
  - name: ♻️ 自动选择
    type: url-test
    proxies: [香港01, 香港02, 日本01]
  - name: 🇯🇵 日本
    type: url-test
    proxies: [日本01, 日本02]
```

我们**不重新实现这套分组，而是直接复用**：内核配置里为每个要用到的组生成一个 lane 入站（`listeners[].proxy: "🇯🇵 日本"`），于是机场分组名就成了可选的线路名。

**好处**：用户在自己 Clash 客户端里熟悉的那套分组名，在这里能直接用，心智不用重建。

### 5.5 订阅里的 rules 为什么不执行

订阅里的 rules 是"按域名/IP 分流"，而 TokenAgency 的分流维度是**"按模型/通道"**——每条通道本来就绑定了自己的 lane 入站，不需要一张全局规则表。

- 内核配置里我们**不导入**订阅的 `rules`，统一写 `MATCH,DIRECT` 兜底（所有 lane 入站都已经指定了 `proxy`，规则不参与决策）
- 订阅原本的 rules **可以在 UI 里只读查看**（"这个订阅原本怎么分流"），存在 `rules_raw` 里
- 少数确实要按域名走不同线路的场景，用 channel 上的 `target_override`

### 5.6 兜底：内核不认的格式怎么办

内核已经覆盖了绝大多数情况，剩下两类要我们在业务层兜：

| 情况 | 处理 |
|---|---|
| 订阅返回 HTML（错误页、需登录、被墙） | 检测 `<!DOCTYPE` / `<html` → 判定失败，保留上版，UI 标红给原因 |
| 空列表 / 节点数为 0 | 视为失败，保留上版 |
| 内核不认的订阅格式 | 业务层提供 `format: convert_links`：把链接列表转成 Clash YAML 再落盘给内核（**只在这一个方向上做转换，不做完整 parser**） |

核心原则不变：

> **完全兼容 ≠ 认得所有东西，而是"遇到不认识的东西不崩、不丢已有数据"。**

具体到内核方案：永远保留 `raw` 原文（压缩存），坏更新可回滚，节点数/组数异常时拒绝采纳。

## 6. DNS（交给内核，但仍要保证"目标域名不在本机解析"）

内核的 DNS 由我们生成的配置控制，要点：

- `enhanced-mode: redir-host` —— **不用 fake-ip**（没有 TUN，没有 DNS 劫持，fake-ip 无从谈起）
- 走代理的 outbound：内核把**域名**交给节点服务器解析（SS 的 domain address type、VMess/VLESS 的域名地址），→ 天然免污染，本地不需要维护 DNS 分流规则
- `proxy-server-nameserver` 必须配国内的干净 DoH（`223.5.5.5` / `doh.pub`）——**这条不配，节点自己都连不上**
- **不装 DNS 服务、不改系统 DNS、不开 53 端口、不劫持任何 DNS 请求**

```yaml
dns:
  enable: true
  enhanced-mode: redir-host
  nameserver: ["https://223.5.5.5/dns-query", "https://doh.pub/dns-query"]
  proxy-server-nameserver: ["https://223.5.5.5/dns-query"]
  default-nameserver: ["223.5.5.5", "119.29.29.29"]
```

## 7. 线路类型、选择与健康检查

### 7.1 五种线路（Lane）

| `kind` | 语义 | 落到内核配置里是什么 |
|---|---|---|
| `direct` | 直连（本机网络，不走任何节点） | `listeners[].proxy: DIRECT` |
| `subscription_auto` | 在**整份订阅**里自己择优 | 生成一个 `url-test` 组，`use: [<provider>]` |
| `subscription_group` | 用订阅里的**某个组**（保留机场分组语义） | `listeners[].proxy: "🇯🇵 日本"` 直接指该组名 |
| `subscription_node` | 固定某个节点 | `listeners[].proxy: "东京-03"` |
| `filter` | 正则筛选节点后自己择优（"这条线只要日本节点"） | 生成一个 `url-test` 组 + `filter` / `exclude-filter` |

后四种都可叠加 lane 级 `filter` 做二次收窄。**节点怎么选、怎么测速，全由内核负责**；我们只负责"哪条 lane 用哪个组"，以及把内核的测速结果读出来显示在托盘。

### 7.2 选择策略（就是内核的组类型，我们不重新实现）

| 策略 | 语义 | 内核里对应 |
|---|---|---|
| `select` | 手动钉住某节点 | `type: select`，切换走 REST `PUT /proxies/{name}` |
| `url-test` | 延迟最小的（默认） | `type: url-test` |
| `fallback` | 按顺序取第一个可用 | `type: fallback` |
| `load-balance` | 轮询 / 一致性哈希 | `type: load-balance` |

我们只做两件周边的事：

- **探测 URL 优先用该 lane 上真实会用到的上游端点**（如 `https://api.openai.com/v1/models`），比默认的 `generate_204` 或订阅自带的 `url` 更贴近实况——生成组配置时覆盖掉。
- 把内核的测速结果（`GET /proxies/{name}/delay`）读出来，供托盘显示与 `lowest_latency` 通道选择器使用。

判定口径由内核负责，但**通道级健康**由我们统计：`2xx / 401 / 403 / 404` 都算「网络通」（被服务拒绝 ≠ 节点坏），只有**超时 / TLS 失败 / 连接被拒**才给该 lane 记故障。

## 8. 线路分配：手动指定谁走哪条线，或者直连

### 8.1 五级默认，从具体到一般

```
策略级  policy.action.use_lane     ← 最高，用于一键全切/临时救急
   ↑ 覆盖
通道级  channel.lane               ← 最常用的手动指定点
   ↑ 覆盖
凭据级  credential.lane            （这份 key 默认走哪条）
   ↑ 覆盖
服务商级 provider.lane             （这家默认走哪条）
   ↑ 覆盖
全局    net.default_lane
   ↑
兜底    direct
```

运行时解析：`policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`
（`??` = 取第一个非空值）

### 8.2 两个分配入口

**① 通道矩阵里直接选**（最常用，一眼看清全貌）

```
                  │ OpenAI个人        │ 中转A              │ 中转B
──────────────────┼───────────────────┼────────────────────┼──────────────────
gpt-4o            │ [日本·自动 ▾]     │ [🇯🇵 日本组 ▾]     │ [直连 ▾]
                  │ ← 点开是一个下拉，选项就是下面这些
```

下拉选项的结构（按来源分组，直连永远排第一）：

```
直连（本机网络）
── 我的订阅 ──────────────
  日本 · 自动择优        (subscription_auto, 82ms)
  🇯🇵 日本               (subscription_group, 订阅自带, 3 节点)
  🇭🇰 香港               (subscription_group, 订阅自带, 5 节点)
  东京-03                (subscription_node, 88ms)
  美国 · 固定            (subscription_node, 210ms)
── 机场B ─────────────────
  ...
```

**② 网络页 → 分配 Tab**（批量管理）

左右两栏：左边"用它的东西"（模型 / 通道 / 凭据 / 客户端 / 标签），右边"线路"。支持多选批量改、按订阅分组查看。

### 8.3 直连永远是一个选项

每个线路下拉的第一项就是「直连（本机网络）」。另外两个快捷入口：

- **托盘 → 紧急开关 → 全部直连**（梯子全挂了的时候一键保命）
- **策略里的 `use_lane: direct`**（可以做成一条"夜间某些模型走直连"的规则）

### 8.4 反向视图：这条订阅被谁用了

选中一个订阅 / 组 / 节点，右侧列出**"哪些通道正在用它"**。避免删订阅或改节点时不知道影响了谁——这是多套餐多线路配置里最容易踩的坑。

## 9. 无系统副作用（自动化测试）

```
test_system_network_untouched:
  1. 快照: 注册表 HKCU\...\Internet Settings (ProxyEnable/ProxyServer/AutoConfigURL)
           netsh winhttp show proxy
           route print -4
           Get-DnsClientServerAddress
  2. 启动 TokenAgency，跑 20 个请求（含走梯子的）
  3. 快照 2 → 与快照 1 逐字段 diff，必须完全一致
  4. 退出 TokenAgency → 快照 3 → 与快照 1 一致
  5. netstat 断言：只有以下 loopback 端口在监听，**没有任何 0.0.0.0**：
       127.0.0.1:8787  网关
       127.0.0.1:8788  管理 API（仅 CLI）
       127.0.0.1:39090 内核 REST
       127.0.0.1:3910x lane 入站
     退出后无残留子进程
```

macOS / Linux 对应 `scutil --proxy`、`networksetup -getwebproxy`、`ip route`、`/etc/resolv.conf`。

## 10. 许可证

回到内核方案后，**GPL-3.0 的问题重新出现**（mihomo 是 GPL-3.0）。三条路线，见 [08 §0.1](08-tech-stack.md) 与文末：

| 方案 | 说明 | 代价 |
|---|---|---|
| **A. 项目整体开源为 GPL-3.0 兼容** | 最干净，社区友好 | 不能闭源商业化 |
| **B. 不随包分发内核** | 首启引导用户自行下载官方 Release，或使用用户已安装的内核路径（`net.kernel.provider: external`） | 首启多一步 |
| C. 换宽松许可的内核实现 | 覆盖度与协议支持需实测 | 风险高，不推荐 |

**建议 B 作为默认**：`net.kernel.provider: external` 走用户自行获取的内核，TokenAgency 只做调用与配置生成，保持许可自由；想省事的用户可以在首启向导里一键下载（下载行为由用户发起，而非我们分发）。

⚠️ 另有一条硬约束：**可以借鉴 Clash Verge Rev 的思路，但不要拷贝它的代码**——它是 `GPL-3.0-only`。同样，也不要引 `tauri-plugin-mihomo`（那是它的 GPL git 仓库），自己写 §2.2 那 5 件事。具体法律判断请自行确认，我只陈述工程事实。

## 11. 高级模式（默认关闭）

| 模式 | 用途 | 代价 |
|---|---|---|
| `system_proxy: on` | 让别的程序也走 TokenAgency | 会改系统设置，破坏"无副作用"不变量；退出时自动还原并做快照校验 |
| `tun: on` | 整机透明代理 | 需管理员/root，与 VPN、加速器冲突，杀软可能报警 |

参考实现：Clash Verge Rev 用 `sysproxy`（带 `guard` feature，进程崩溃时自动还原系统代理）来兜住这个风险——如果我们做高级模式，这是必须抄的思路（自己实现，不抄代码）。

两者都是实验性开关，默认关，开启后托盘图标加锁形角标。

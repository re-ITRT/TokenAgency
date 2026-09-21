# TokenAgency

> 一个人的 LLM 中转站：本地网关 + 语义路由 + 内嵌内核出网 + Clash Verge 式托盘。
> 不是给几百个用户做中转，而是给**一台机器上的一个人**把 "多家 tokenplan + 多个 key + 多条梯子 + 多套模型" 收敛成一个本地端点。

工作名 `TokenAgency`（可改）。文档语言：中文。

---

## 1. 你的痛点 → 本设计怎么解

| # | 痛点 | 根因 | 本设计的解法 | 详见 |
|---|---|---|---|---|
| 1 | 订了多家 tokenplan，切换麻烦 | 每个客户端各自填 base_url/key | 只暴露一个本地端点 `http://127.0.0.1:8787/v1`，切换在网关侧完成，客户端永不改配置 | [02](docs/02-config-and-data-model.md) |
| 2 | 国外服务还得挂梯子 | 依赖系统级代理 | 内嵌内核 sidecar，**只监听 loopback**：不写系统代理、不改路由表、不装 TUN，本机其它程序完全不受影响 | [04](docs/04-network-core.md) |
| 3 | 装了 TokenHub 痛点照旧 | TokenHub 是"服务端中转站"，网络仍交给系统 | 网络与 LLM 路由由**同一处决策**：选通道时同时选出口线路 | [01](docs/01-architecture.md) |
| 4 | 代理不穿透视觉，模型被"干成瞎子" | 中转层用强类型 schema 反序列化，未知字段被静默丢弃；或把图片块悄悄替换成 `"[image]"` 文本 | **全透传**：同协议请求零改写（只定点 patch 模型名与鉴权头），body 用 `serde_json::Value` 而非强类型结构体；跨协议转换无法映射时**默认直接报错**，绝不静默文本化 | [03](docs/03-routing-and-policy.md) |
| 5 | 吞 agent 标识 | 中转统一重写 `User-Agent` / 用白名单重建头部 | 头部**整体转发，只做三处必须的修改**（`host`、鉴权头、hop-by-hop）；`user-agent`/`x-stainless-*`/`anthropic-beta` 等自动原样到达 | [03](docs/03-routing-and-policy.md) |
| 6 | key 太多难管 | 无凭据抽象 | Credential 层 + OS 钥匙串存储 + 额度账本 + 一键启用/禁用 | [02](docs/02-config-and-data-model.md) |
| 7 | TokenHub 面向大型中转站，个人太重 | 多租户/计费/用户体系 | **非目标**：不做多用户、不做计费、不做 SaaS。local-first、单机、单用户、0 配置启动 | 本文 §4 |
| 8 | 没有 Clash Verge 那种托盘，只能开网页 | 纯 Web 后台 | Tauri 托盘：状态色图标 + 右键菜单可在 2 次点击内切策略/切节点/看额度 | [05](docs/05-ui-tray.md) |
| 9 | 多个 tokenplan 映射同一模型，无法"用完一个再用下一个"，没有粘性 | 路由无状态 | **通道组 + 粘性选择器 + 额度账本**：同会话固定走同一凭据；额度耗尽自动切下一个候选；故障转移带冷却 | [03](docs/03-routing-and-policy.md) |
| 10 | 没有使用策略，每次手动改 | 无规则引擎 | 有序规则表 `match → action`，含**策略模拟器**：输入假请求，预览命中规则/选中通道/备用链 | [03](docs/03-routing-and-policy.md) |
| 11 | 想加自定义中转、自定义映射 | 配置不可控 | Provider 模板 + 任意 base_url + 模型名双向映射 + header/body 补丁 | [02](docs/02-config-and-data-model.md) |
| 12 | 想按模型走不同梯子，且与电脑网络隔离 | 系统代理是全局的 | **每个通道手动指定一条线路**（订阅里的某个组 / 某个节点 / 自动择优 / **直连**），五级默认逐层覆盖。**不写系统代理、不改路由表、不装 TUN、不开额外端口**，本机其它程序完全不受影响 | [04](docs/04-network-core.md) |
| 13 | 已有 Clash 订阅，不想重新配一遍 | 各家格式不一 | **内核直接吃 Clash 订阅**（L1 节点 / L2 分组 / L3 规则全兼容），我们只做业务层。订阅自带的「🇯🇵 日本」分组可直接当线路选。坏更新绝不清空已有数据 | [04 §5](docs/04-network-core.md) |
| 14 | 技术选型与工程结构 | — | React 19 + MUI v7 + Zustand/TanStack Query / Tauri v2 + axum + sqlx + mihomo sidecar；逐项评审见 [08](docs/08-tech-stack.md)（含 `tauri-plugin-mihomo` 不在 crates.io 的核实结论） | [08](docs/08-tech-stack.md) |
| 15 | 需要一份能施工的整体设计 | — | **[09 软件架构设计（架构基线）](docs/09-architecture-design.md)**：C4 视图、crate 分层与依赖规则、时序、数据视图、质量属性与验收场景、ADR 索引、演进路线 | [09](docs/09-architecture-design.md) |

---

## 2. 一句话架构

```
客户端(Cursor/Claude Code/任意 SDK)
        │  http://127.0.0.1:8787/v1   ← 唯一入口，OpenAI / Anthropic 双协议
        ▼
┌──────────────────────────── TokenAgency ────────────────────────────┐
│  网关(axum) → 读路由字段 → 模型解析 → 策略求值 → 通道选择            │
│                                   │                                │
│              额度账本 / 粘性表 / trace(SQLite)                      │
│                                   ▼                                │
│  定点改写(模型名/鉴权头) → 选 Lane → 取该 lane 的 loopback 端口        │
└───────────────────────────────────┬─────────────────────────────────┘
                                    │  仅 127.0.0.1，不碰系统网络
                                    ▼
                      mihomo 内核 (sidecar, TUN=off, allow-lan=off)
                       lane-jp ─┐
                       lane-us ─┼─→ 机场订阅的节点 / 订阅自带分组 / DIRECT
                       lane-cn ─┘
```

**两个进程，但只有一个"入口"**：客户端只认 `8787`；`3910x` 是进程内自己用的 loopback 入站，不对外、不劫持任何东西。

**三个不变量**（架构级承诺，写进测试）：

1. **入口唯一**：客户端只需要记住一个 base_url；整机只有 `127.0.0.1:8787` 一个出口。
2. **无系统副作用**：关闭 App 后系统代理设置、路由表、DNS 与启动前逐字节一致；没有子进程、没有多余监听端口。
3. **全透传**：同协议转发的请求，除 `host` / `content-length` / 鉴权头 / hop-by-hop 头这四个协议必需的例外，客户端发出的每个字节都到达上游。

---

## 3. 文档索引

| 文档 | 内容 |
|---|---|
| **[docs/10-interface-spec.md](docs/10-interface-spec.md)** | ★ **接口文档（一）对外契约**：网关 HTTP API（含全透传 4 处例外、SSE、错误信封）、Tauri IPC 命令与事件全清单、管理 API、契约变更流程 |
| **[docs/11-internal-contracts.md](docs/11-internal-contracts.md)** | ★ **接口文档（二）内部契约**：crate 之间的 Rust trait、内核接口（生成的配置模板 + REST 端点）、订阅契约、存储契约、**错误码总表**、usage 归一化规格 |
| **[docs/12-work-breakdown.md](docs/12-work-breakdown.md)** | ★ **工作分解**：W0–W5 波次、每个工作包的文件所有权与完成定义、派发铁律 |
| **[docs/09-architecture-design.md](docs/09-architecture-design.md)** | ★ **软件架构设计（架构基线）**：把全部结论收敛成一份可施工的整体设计——C4 视图、crate 分层与依赖规则、运行时时序、数据视图、质量属性与验收场景、ADR 索引、演进路线、术语表 |
| [docs/01-architecture.md](docs/01-architecture.md) | 总体架构专题：技术选型、进程与端口模型、请求管线 12 步、协议适配层、ADR 详述 |
| [docs/02-config-and-data-model.md](docs/02-config-and-data-model.md) | 领域模型（Provider/Credential/Lane/Model/Channel/ChannelGroup/Policy）、SQLite 表结构、配置 schema、[config.example.yaml](config.example.yaml) |
| [docs/03-routing-and-policy.md](docs/03-routing-and-policy.md) | 策略引擎 DSL、通道组与选择器、粘性、额度账本与"用完一个换下一个"、故障转移与重试边界、全透传规则、视觉标签 |
| [docs/04-network-core.md](docs/04-network-core.md) | 网络模型（内嵌内核 sidecar）、`OutboundDialer` 抽象、**Clash 订阅兼容**、**线路分配（手动指定谁走哪条线或直连）**、DNS、隔离性验证、许可证 |
| [docs/05-ui-tray.md](docs/05-ui-tray.md) | 托盘菜单逐项设计、页面导航、通道矩阵、策略模拟器、通知策略 |
| [docs/07-ui-pages-and-cost.md](docs/07-ui-pages-and-cost.md) | **首页看板**（用户想知道什么 + 怎么展示美观）、**请求日志**（三档开关/保留时长/脱敏）、Provider 渠道设置、模型目录、路由策略、**成本中心**（token 类型归一化 + 定价表 + 任意维度统计） |
| [docs/06-roadmap-and-risks.md](docs/06-roadmap-and-risks.md) | M0–M5 里程碑、可测验收标准、spike 清单、风险登记表 |
| [docs/08-tech-stack.md](docs/08-tech-stack.md) | **技术选型评审**：逐项 ✅保留/⚠️调整/➕新增/❌去掉、Rust workspace 结构、IPC 契约、单一真源、打包分发、测试策略、性能预算、一页速查表 |
| [config.example.yaml](config.example.yaml) | 一份可直接照着改的完整配置示例 |

---

## 4. 非目标（明确不做）

- ❌ 多用户 / 注册登录 / 计费 / 充值 / 卡密
- ❌ 对外提供服务（不监听 `0.0.0.0`，不做公网中转，不做域名与反代教程）
- ❌ 团队共享的"渠道池"运营后台
- ❌ 模型微调 / 训练 / RAG 平台
- ❌ 默认接管整机网络（TUN / 系统代理是**可选高级模式**，默认关闭）

## 5. 目标用户画像（就一个人）

一个开发者，机器上有 3–8 个 key/套餐（官方 + 若干中转），2–4 个客户端（Cursor、Claude Code、Cherry Studio、Python 脚本），1–2 个机场订阅。诉求是：**别再改客户端配置，别再手动切节点，别再手动换 key。**

## 6. 客户端接入（一次性，之后不再动）

```bash
# OpenAI 兼容（Cursor / Cline / Cherry Studio / 任意 SDK）
OPENAI_BASE_URL=http://127.0.0.1:8787/v1
OPENAI_API_KEY=ta-local-xxxxxxxx        # 可选：网关本地密钥

# Anthropic 兼容（Claude Code）
ANTHROPIC_BASE_URL=http://127.0.0.1:8787
ANTHROPIC_API_KEY=ta-local-xxxxxxxx
```

模型名随便填客户端里已有的（`gpt-4o`、`claude-sonnet-4-5`…），网关按 `models/channels` 映射到真实上游。想换套餐？在托盘里点一下。

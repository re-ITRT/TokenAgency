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
| 12 | 想按模型走不同梯子，且与电脑网络隔离 | 系统代理是全局的 | **每个服务商统一一个节点**（**自动选择** / 某个节点 / **直连**；全局默认兜底）。**不写系统代理、不改路由表、不装 TUN、不开额外端口**，本机其它程序完全不受影响 | [04 §8](docs/04-network-core.md)、[55](docs/55-provider-owns-the-lane.md) |
| 13 | 已有 Clash 订阅，不想重新配一遍 | 各家格式不一 | **内核直接吃 Clash 订阅**（L1 节点 / L2 分组 / L3 规则全兼容），我们只做业务层。**订阅里只拿节点**：点进一份订阅即可逐个开关节点，顶部「自动选择」「直连」恒在。坏更新绝不清空已有数据 | [04 §5](docs/04-network-core.md)、[54](docs/54-nodes-not-groups.md) |
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
                       lane-us ─┼─→ 机场订阅的节点（可在订阅详情页逐个开关） / DIRECT
                       lane-cn ─┘
```

**两个进程，但只有一个"入口"**：客户端只认 `8787`；`3910x` 是进程内自己用的 loopback 入站，不对外、不劫持任何东西。

**三个不变量**（架构级承诺，写进测试）：

1. **入口唯一**：客户端只需要记住一个 base_url；整机只有 `127.0.0.1:8787` 一个出口。
2. **无系统副作用**：关闭 App 后系统代理设置、路由表、DNS 与启动前逐字节一致；没有子进程、没有多余监听端口。
3. **全透传**：同协议转发的请求，除 `host` / `content-length` / 鉴权头 / hop-by-hop 头这四个协议必需的例外，客户端发出的每个字节都到达上游。

---

## 3. 文档索引

> **[docs/README.md](docs/README.md) 是文档的规范索引**（39 份全量 + 编号规则 + 引用格式）。
> 下面这张表只挑"最该先读的几份"；**要找别的文档、或要加新文档，一律去 docs/README.md**。

| 文档 | 内容 |
|---|---|
| **[docs/README.md](docs/README.md)** | ★ **文档索引与引用规则**：编号只增不改、空缺永久、`[docs/NN §M]` 的写法、全量清单 |
| **[docs/10-interface-spec.md](docs/10-interface-spec.md)** | ★ **接口文档（一）对外契约**：网关 HTTP API（含全透传 4 处例外、SSE、错误信封）、Tauri IPC 命令与事件全清单、管理 API、契约变更流程 |
| **[docs/11-internal-contracts.md](docs/11-internal-contracts.md)** | ★ **接口文档（二）内部契约**：crate 之间的 Rust trait、内核接口（生成的配置模板 + REST 端点）、订阅契约、存储契约、**错误码总表**、usage 归一化规格 |
| **[docs/12-work-breakdown.md](docs/12-work-breakdown.md)** | ★ **工作分解**：W0–W5 波次、每个工作包的文件所有权与完成定义、派发铁律 |
| **[docs/13-team-and-delivery.md](docs/13-team-and-delivery.md)** | ★ **团队与交付流程**：RACI 权责矩阵、员工入职材料清单、四层评审协议、培养机制、PM 验收标准与范围纪律 |
| **[docs/09-architecture-design.md](docs/09-architecture-design.md)** | ★ **软件架构设计（架构基线）**：把全部结论收敛成一份可施工的整体设计——C4 视图、crate 分层与依赖规则、运行时时序、数据视图、质量属性与验收场景、ADR 索引、演进路线、术语表 |
| [docs/01-architecture.md](docs/01-architecture.md) | 总体架构专题：技术选型、进程与端口模型、请求管线 12 步、协议适配层、ADR 详述 |
| [docs/02-config-and-data-model.md](docs/02-config-and-data-model.md) | 领域模型（Provider/Credential/Lane/Model/Channel/ChannelGroup/Policy）、SQLite 表结构、配置 schema、[config.example.yaml](config.example.yaml) |
| [docs/03-routing-and-policy.md](docs/03-routing-and-policy.md) | 策略引擎 DSL、通道组与选择器、粘性、故障转移与重试边界、全透传规则、视觉标签。**注意**：路由已收敛为「平均粘性」一种（[docs/48](docs/48-balanced-sticky-only-routing.md)），额度账本已整块删除（第 314 轮） |
| [docs/04-network-core.md](docs/04-network-core.md) | 网络模型（内嵌内核 sidecar）、`OutboundDialer` 抽象、**Clash 订阅兼容**、**线路分配（手动指定谁走哪条线或直连）**、DNS、隔离性验证、许可证 |
| [docs/05-ui-tray.md](docs/05-ui-tray.md) | 托盘菜单逐项设计、页面导航、通道矩阵、策略模拟器、通知策略 |
| [docs/07-ui-pages-and-cost.md](docs/07-ui-pages-and-cost.md) | **首页看板**（用户想知道什么 + 怎么展示美观）、**请求日志**（三档开关/保留时长/脱敏）、Provider 渠道设置、模型目录、**成本中心**（token 类型归一化 + 定价表 + 任意维度统计）。页面增删以 [docs/26](docs/26-motion-and-first-level-ellipsis.md) 的清单为准 |
| **[docs/14-observability.md](docs/14-observability.md)** | ★ **日志、错误追踪与诊断**：应用日志（落文件）与请求日志（入 SQLite）的分工、字段命名约定、**错误日志必须带 TA 码**、脱敏规则与三条证据测试、panic/崩溃文件、级别热更新、**诊断包导出（可放心发给他人）**、自保要求 |
| **[docs/15-visual-design.md](docs/15-visual-design.md)** | ★ **视觉设计系统**：高级极简的三条可执行原则、**emoji 零容忍（含唯一例外：用户数据原样呈现）**、设计令牌（中性色阶/强调色/间距/圆角/字阶）、表格与列表规范、图标与动效、**四条机械守卫** |
| [docs/06-roadmap-and-risks.md](docs/06-roadmap-and-risks.md) | M0–M5 里程碑、可测验收标准、spike 清单、风险登记表 |
| [docs/08-tech-stack.md](docs/08-tech-stack.md) | **技术选型评审**：逐项 ✅保留/⚠️调整/➕新增/❌去掉、Rust workspace 结构、IPC 契约、单一真源、打包分发、测试策略、性能预算、一页速查表 |
| **[docs/28-lines-belong-to-providers.md](docs/28-lines-belong-to-providers.md)** | ★ **线路归服务商、模型归"分配"**：关系翻转后的权威口径（谁拥有线路、页面各管什么） |
| **[docs/29-auto-network.md](docs/29-auto-network.md)** | ★ **「自动」网络与代理归属**：代理只属于服务商，线路行里没有代理控件 |
| [docs/31-progress.md](docs/31-progress.md) | 逐轮进度日志（每轮更新；`§NNN` = 第 NNN 轮） |
| [docs/46-bundled-kernel.md](docs/46-bundled-kernel.md) | 内置内核（随包分发 mihomo）的实现与逐轮记录 |
| [docs/48-balanced-sticky-only-routing.md](docs/48-balanced-sticky-only-routing.md) | 路由收敛为「平均粘性」+ 模型/线路开关 |
| [docs/49-channels-page-removed.md](docs/49-channels-page-removed.md) | 删掉「套餐与线路」页（`/channels`）、以及这次删页的能力减法记录（§2 的定价搬迁已被 [docs/51](docs/51-model-keyed-pricing.md) 取代） |
| **[docs/51-model-keyed-pricing.md](docs/51-model-keyed-pricing.md)** | ★ **按「对外模型」定价**：定价入口在 `/models` 每一行（`match` 只写 `model`）、成本中心只做显示、别名归一到规范模型 id |
| [docs/52-dedup-and-hotspots.md](docs/52-dedup-and-hotspots.md) | 有证据的重复收敛与热点修复：共享读件（`shared/row.ts`）、"单件只有一份实现"守卫、`apikeys` 渲染期 O(N×M) 修复；以及**刻意没改**的那些（页面级大组件等） |
| [docs/53-referenced-delete-must-be-actionable.md](docs/53-referenced-delete-must-be-actionable.md) | 删被引用的订阅/线路：拒绝必须是 `TA-5002` + **谁在挡** + **怎么办**；确认框不许承诺做不到的级联 |
| [docs/54-nodes-not-groups.md](docs/54-nodes-not-groups.md) | 放弃「订阅里分组」，只拿节点：订阅详情页逐节点开关、「自动选择」/「直连」恒在不可关、服务商侧默认直连；并修掉两个让节点开关形同虚设的真 bug |
| [docs/55-provider-owns-the-lane.md](docs/55-provider-owns-the-lane.md) | 放弃**线路级别**的分配：删「线路分配」整页、删三级覆盖字段与 4 条命令，走哪个节点只剩服务商一处（`provider.lane`） |
| [docs/56-sidebar-information-architecture.md](docs/56-sidebar-information-architecture.md) | 侧边栏按**任务流**重排并分组（`概览 │ 配置 │ 记录 │ 系统`）；推翻"按使用频率排"这条旧规则 |
| [docs/57-write-forms-on-demand.md](docs/57-write-forms-on-demand.md) | `/network` 的写表单不再常驻（「添加」/「编辑」打开对话框）；编辑不会用遮蔽地址覆盖真地址、也不会清空逐节点开关 |
| [docs/58-lanes-come-from-providers.md](docs/58-lanes-come-from-providers.md) | 线路不再手建：删掉「添加线路/编辑」界面；修好服务商页「网络代理」下拉看不到订阅节点（接 `lane_list` 改成接含节点的 `lane_options`） |
| [docs/59-protocol-hub-and-lane-ports.md](docs/59-protocol-hub-and-lane-ports.md) | 协议翻译接进 pipeline（N 种边缘 / 1 种内部）＋ 修掉「lane 端口表被冻结」导致的"只有直连能用" |
| [docs/50-plugin-and-capability-minimization.md](docs/50-plugin-and-capability-minimization.md) | 插件/能力最小化、重复代码收敛、文档路径规则、**威胁面清单（含仍未清除的部分）** |
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

---

## 7. 开发与构建

### 环境要求

| 组件 | 版本 | 备注 |
|---|---|---|
| Rust | 1.82+（`rust-toolchain.toml` 已锁定 stable） | target `x86_64-pc-windows-msvc` |
| MSVC Build Tools | 2022 | Windows 链接器 |
| Node.js | 20+（本机验证于 v24） | **使用 npm**，见下方说明 |
| WebView2 Runtime | 任意 | Windows 10/11 通常已内置 |
| mihomo | 自行获取 | `net.kernel.provider: external`，不随包分发（GPL 合规） |

> ⚠️ **本仓库用 npm，不用 pnpm。** pnpm 会向上层目录寻找 workspace 根，而父目录存在一个 0 字节的 `package.json` 会导致 JSON 解析失败。

### 首次准备

```powershell
pwsh -File scripts/install-hooks.ps1   # 安装 git 钩子 + 仓库级 git 配置
npm install
node scripts/make-icon.mjs             # 生成 app-icon.png（1024×1024，无三方依赖）
cargo tauri icon app-icon.png          # 生成 src-tauri/icons 全套
```

### 日常命令

```powershell
pwsh -File scripts/dev.ps1             # 开发模式（Vite + Tauri，带热重载）
pwsh -File scripts/build.ps1           # 出安装包：绑定 → 前端 → 测试 → NSIS
cargo check --workspace                # 只要类型检查（最快）
cargo test  --workspace                # 全量测试
cargo clippy --workspace --all-targets -- -D warnings
```

产物位置：`target/release/bundle/nsis/*.exe`

### 代码质量门禁

钩子随仓库走（`core.hooksPath = .githooks`），不依赖 husky / lefthook：

| 钩子 | 做什么 |
|---|---|
| `pre-commit` | 明文密钥扫描 · 大文件守卫 · `cargo fmt --check` · `cargo clippy -D warnings` · `tsc --noEmit`（按改动文件类型选择性执行） |
| `pre-push` | `cargo test --workspace` + `npm run build` |
| `commit-msg` | 约定式提交格式（`feat(gateway): ...`） |

紧急绕过用 `git commit --no-verify`，但**不要养成习惯**。

### 目录速览

```
crates/ta-core           领域模型、配置校验、路由引擎、额度、定价、计量（纯逻辑，无 IO）
crates/ta-store          SQLite：配置、账本、trace、粘性、额度状态
crates/ta-kernel         mihomo sidecar：进程看护、配置生成、REST 控制、lane 端口分配
crates/ta-subscription   订阅下载、格式嗅探、坏更新拦截、版本回滚
crates/ta-secrets        系统钥匙串封装与 ${secret:...} 引用解析
crates/ta-gateway        本地网关：入站、全透传、路由管线、分发、协议适配、trace
src-tauri                Tauri 壳：命令层、托盘、插件、生命周期（薄）
src                      前端：React 19 + MUI v7（10 个页面）
docs                     设计文档 01–12（09 是架构基线，10/11 是接口契约）
```

---

## 8. 许可证

本项目采用 **GNU General Public License v3.0** —— 全文见 [LICENSE](LICENSE)。

版权归 **毛治钦** 所有。

### 随包分发的第三方组件

安装包内含 **mihomo**（GPL-3.0）二进制。它与本项目是**两个独立进程**（本程序拉起
`mihomo.exe`，再通过本地 HTTP 控制口通信），属于 GPLv3 §5 末段的**聚合（aggregate）**，
不改变本项目自身的许可。

按 GPLv3 §6(d)"在提供对象码的同一处免费提供对应源码的等价访问"，对应源码
**与安装包挂在同一个 Release**：

| 组件 | 版本 | 对应源码 |
| --- | --- | --- |
| mihomo | v1.19.31 | Release 附件 `mihomo-v1.19.31-source.tar.gz` |

该二进制**未经修改** —— 实测 sha256 与上游 `mihomo-windows-amd64-compatible-v1.19.31`
**逐字节相同**，因此附上游对应源码即已完整履行；若将来改动了内核，则**必须公开我们自己的改动**。
重新获取：`pwsh -File scripts/fetch-mihomo.ps1 -Mode Source`。

其余第三方组件（Rust / npm 依赖、GeoIP 数据库）的许可与署名要求见
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

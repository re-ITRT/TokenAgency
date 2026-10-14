[English](README.en.md) | 中文
# TokenAgency

> 一个人的 LLM 中转站：本地网关 + 语义路由 + 内嵌内核出网 + Clash Verge 式托盘。
> 不是给几百个用户做中转，而是给**一台机器上的一个人**把 "多家 tokenplan + 多个 key + 多条梯子 + 多套模型" 收敛成一个本地端点。

工作名 `TokenAgency`（可改）。文档语言：中文。

---

## 1. 你的痛点 → 本设计怎么解

| # | 痛点 | 根因 | 本设计的解法 |
|---|---|---|--- |
| 1 | 订了多家 tokenplan，切换麻烦 | 每个客户端各自填 base_url/key | 只暴露一个本地端点 `http://127.0.0.1:8787/v1`，切换在网关侧完成，客户端永不改配置 |
| 2 | 国外服务还得挂梯子 | 依赖系统级代理 | 内嵌内核 sidecar，**只监听 loopback**：不写系统代理、不改路由表、不装 TUN，本机其它程序完全不受影响 |
| 3 | 装了 TokenHub 痛点照旧 | TokenHub 是"服务端中转站"，网络仍交给系统 | 网络与 LLM 路由由**同一处决策**：选通道时同时选出口线路 |
| 4 | 代理不穿透视觉，模型被"干成瞎子" | 中转层用强类型 schema 反序列化，未知字段被静默丢弃；或把图片块悄悄替换成 `"[image]"` 文本 | **全透传**：同协议请求零改写（只定点 patch 模型名与鉴权头），body 用 `serde_json::Value` 而非强类型结构体；跨协议转换无法映射时**默认直接报错**，绝不静默文本化 |
| 5 | 吞 agent 标识 | 中转统一重写 `User-Agent` / 用白名单重建头部 | 头部**整体转发，只做三处必须的修改**（`host`、鉴权头、hop-by-hop）；`user-agent`/`x-stainless-*`/`anthropic-beta` 等自动原样到达 |
| 6 | key 太多难管 | 无凭据抽象 | Credential 层 + OS 钥匙串存储 + 额度账本 + 一键启用/禁用 |
| 7 | TokenHub 面向大型中转站，个人太重 | 多租户/计费/用户体系 | **非目标**：不做多用户、不做计费、不做 SaaS。local-first、单机、单用户、0 配置启动 |
| 8 | 没有 Clash Verge 那种托盘，只能开网页 | 纯 Web 后台 | Tauri 托盘：状态色图标 + 右键菜单可在 2 次点击内切策略/切节点/看额度 |
| 9 | 多个 tokenplan 映射同一模型，无法"用完一个再用下一个"，没有粘性 | 路由无状态 | **通道组 + 粘性选择器 + 额度账本**：同会话固定走同一凭据；额度耗尽自动切下一个候选；故障转移带冷却 |
| 10 | 没有使用策略，每次手动改 | 无规则引擎 | 有序规则表 `match → action`，含**策略模拟器**：输入假请求，预览命中规则/选中通道/备用链 |
| 11 | 想加自定义中转、自定义映射 | 配置不可控 | Provider 模板 + 任意 base_url + 模型名双向映射 + header/body 补丁 |
| 12 | 想按模型走不同梯子，且与电脑网络隔离 | 系统代理是全局的 | **每个服务商统一一个节点**（**自动选择** / 某个节点 / **直连**；全局默认兜底）。**不写系统代理、不改路由表、不装 TUN、不开额外端口**，本机其它程序完全不受影响 |
| 13 | 已有 Clash 订阅，不想重新配一遍 | 各家格式不一 | **内核直接吃 Clash 订阅**（L1 节点 / L2 分组 / L3 规则全兼容），我们只做业务层。**订阅里只拿节点**：点进一份订阅即可逐个开关节点，顶部「自动选择」「直连」恒在。坏更新绝不清空已有数据 |
| 14 | 技术选型与工程结构 | — | React 19 + MUI v7 + Zustand/TanStack Query / Tauri v2 + axum + sqlx + mihomo sidecar；逐项评审结论已并入文档集（含 `tauri-plugin-mihomo` 不在 crates.io 的核实结论） |
| 15 | 需要一份能施工的整体设计 | — | **软件架构设计（架构基线）**：C4 视图、crate 分层与依赖规则、时序、数据视图、质量属性与验收场景、ADR 索引、演进路线 |

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

`docs/` 现在**只有 16 篇**：逐轮过程文档（"第 N 轮记录"）已删除，结论落进了代码、守卫，
以及保留下来的三篇里的「决策记录」小节；历史版本仍在 git 里可查。

**清单、按主题的导航、以及"为什么废掉文档编号引用"都在 [docs/README.md](docs/README.md)。**

| 组 | 篇目 |
|---|---|
| 架构与契约 | 总体架构 · 软件架构设计（基线） · 对外契约 · 内部契约 · 领域模型与配置 · 路由与策略 · 网络模型与内核 |
| 运行与可观测 | 日志与诊断 · 结构清晰化与威胁面（含随包内核 / GPL 决策） · 协议中枢与 lane 端口 |
| 界面 | 视觉设计系统 · 界面极简标准 · 托盘与界面 · 页面细节与成本中心 |
| 过程与风险 | 里程碑、验收与风险 |

## 4. 非目标（明确不做）

- 多用户 / 注册登录 / 计费 / 充值 / 卡密
- 对外提供服务（不监听 `0.0.0.0`，不做公网中转，不做域名与反代教程）
- 团队共享的"渠道池"运营后台
- 模型微调 / 训练 / RAG 平台
- 默认接管整机网络（TUN / 系统代理是**可选高级模式**，默认关闭）

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

## 7. 安装（Windows）

从 [Releases](https://github.com/re-ITRT/TokenAgency/releases) 下载
`TokenAgency_x.y.z_x64-setup.exe`（安装包内含随包内核，约 22 MB）。

1. **右键 → 以管理员身份运行**（理由见下）
2. 跟随向导；默认装到 `%LOCALAPPDATA%\TokenAgency`（当前用户，不写系统目录）
3. 从开始菜单或托盘图标启动

**为什么建议以管理员身份运行**：安装器按**当前用户**安装，按设计不提权。
但在真机上遇到过一类失败：安装器被杀软沙箱降权到**低完整性**，于是连
`%LOCALAPPDATA%\TokenAgency` 都写不进去，报 `Error opening file for writing: ...\tokenagency.exe`。
以管理员身份运行可以避开这一整类问题（安装器检测到权限不足时也会自己用 UAC 重新拉起自己）。

**被 SmartScreen 拦下**（exe 未做代码签名）：点「更多信息」→「仍要运行」。

**提示"文件被占用"**：先跑 `taskkill /F /IM tokenagency.exe /T`，再点「重试」。

诊断日志：`%TEMP%\ta-install-hook.log`（`lasterr=5` 拒绝访问；`lasterr=32` 被占用）。

> 从源码构建见下一节；那份脚本产出的安装包与本 Release 的一致。

## 8. 开发与构建

### 环境要求

| 组件 | 版本 | 备注 |
|---|---|---|
| Rust | 1.82+（`rust-toolchain.toml` 已锁定 stable） | target `x86_64-pc-windows-msvc` |
| MSVC Build Tools | 2022 | Windows 链接器 |
| Node.js | 20+（本机验证于 v24） | **使用 npm**，见下方说明 |
| WebView2 Runtime | 任意 | Windows 10/11 通常已内置 |
| mihomo | 随包自带 v1.19.31 | 也可设 `net.kernel.provider: external` 指自己的内核；见第 9 节与 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) |

> 注意：**本仓库用 npm，不用 pnpm。** pnpm 会向上层目录寻找 workspace 根，而父目录存在一个 0 字节的 `package.json` 会导致 JSON 解析失败。

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
docs                     设计文档 16 篇（09 是架构基线，10/11 是接口契约；索引见 docs/README.md）
```

---

## 9. 许可证

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

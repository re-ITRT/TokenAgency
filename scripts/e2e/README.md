# `scripts/e2e` —— 真机端到端冒烟（tauri-driver + msedgedriver）

这个目录里的脚本用 **tauri-driver + msedgedriver 驱动真实的 Tauri 窗口**，
走的是 `真窗口 → 真 DOM → WebDriver 真点击/真输入 → Tauri IPC → 真 SQLite`。
它**不是** mock 测试，也**不会**用 `execute script` 直接调 IPC 来假装界面操作。

- [`smoke.mjs`](./smoke.mjs) —— 唯一的入口脚本（Node 24，零 npm 依赖）
- 运行产物：`%TEMP%\ta-e2e\`（原始 WebDriver 请求/响应、截图、db 查询结果、报告）

---

## 1. 怎么跑

前置（**已经装好，不要重复安装**）：

| 组件 | 路径 |
| --- | --- |
| `tauri-driver.exe` | `C:\Users\19404\.cargo\bin\tauri-driver.exe` |
| `msedgedriver.exe` | `C:\Users\19404\workspace\.ta-tools\webdriver\msedgedriver.exe`（153.0.4234.48，与本机 WebView2 运行时**精确同版本**） |
| 被测 exe | `target\release\tokenagency.exe` |

```powershell
# 默认：跑 target\release\tokenagency.exe
node scripts\e2e\smoke.mjs

# 指定别的构建
node scripts\e2e\smoke.mjs --exe "C:\path\to\tokenagency.exe"

# 只跑某几步（调试用）
node scripts\e2e\smoke.mjs --only step1,step5

# 保留上一次的 scratch 数据（默认每次都清空，保证"落库"不是上次残留）
node scripts\e2e\smoke.mjs --keep-data
```

退出码：`0` = 没有 FAIL（**SKIP 也算通过"没有失败"**，但报告里逐条列出来）；
`1` = 至少一个 FAIL 或 harness 没起来。

### 被测构建必须先编出来

`smoke.mjs` **不会**帮你构建。构建时机由你决定，但本项目有一条硬教训：

> **构建前先看有没有 `cargo` / `rustc` 在跑。** 有就等它跑完再动手 ——
> 并发抢 `target` 锁会把构建卡死很久。构建输出**重定向到文件**，
> 不要把长输出管道进 `Select-Object`。

```powershell
Get-Process cargo,rustc -ErrorAction SilentlyContinue     # 有输出就等
cargo build --release -p tokenagency *> "$env:TEMP\ta-e2e\logs\build.log"
```

---

## 2. 隔离：为什么它绝不会碰你的真实数据

App 的数据根只有**一个**解析口：`src-tauri/src/state.rs::AppState::app_data_root()`。
它的规则是 **环境变量 `TA_DATA_DIR` （存在且非空）> `<AppData>\TokenAgency`**。
`db.sqlite` 与 `logs/` 都从这个根派生。

`smoke.mjs` 把 `TA_DATA_DIR` 设进 **tauri-driver 进程的环境**（App 是它的子进程，直接继承），
指向 `%TEMP%\ta-e2e\data`。然后它做**两次**断言，不是一次：

1. **开跑后（step0）**：scratch 里必须真的出现 `db.sqlite`（证明覆盖口被 App 用上了，
   而不是 App 无视环境变量、悄悄写去了别处）；同时
   `%APPDATA%\TokenAgency` 的**文件清单 + size + mtime**（`db.sqlite` 另加 sha256）必须逐条不变。
2. **跑完（step7）**：再拍一次同样的快照，仍然逐条比对。

**为什么按文件比对、不按目录 mtime**：目录 mtime 会被**别的进程**触碰（例如你自己开着的
那个实例在写日志），把它算进来会得到假红。文件级（size/mtime/sha256）才是"内容有没有被动过"。

> 这套断言是**有牙的**：如果被测 exe 是**不含 `TA_DATA_DIR` 覆盖口的旧构建**，
> App 会往真实目录写 → step0 当场 FAIL。**所以不要拿旧 exe 跑这个脚本。**

---

## 3. 怎么读懂输出

控制台是一行一步：

```
PASS step2  供应商页新增供应商（列表 + 落库）
     ...
SKIP step4  界面缺入口：/channels 与 /network 都没有建通道/指派节点的控件（缺 selector：...）
     找过：data-testid 匹配 /(^|-)add|new|create|upsert/i → 无
     找过：按钮文案匹配 /新增|添加|新建|创建|Add|New|Create/i → 无
     页面上实际存在的可见控件：
     <button testid=channels-refresh> "刷新"
```

三种状态的语义**不允许混淆**：

| 状态 | 含义 |
| --- | --- |
| `PASS` | 断言真的成立，且证据（selector / 操作后 DOM 文本 / db 查询结果）在下面逐条列出 |
| `SKIP` | 界面上**没有这个入口**。SKIP 会写清"缺哪个 selector"以及"页面上实际有什么"，**它不是 PASS** |
| `FAIL` | 入口存在但操作没达到预期（点了没反应 / db 没落库 / 隔离被突破 / 页面没渲染） |

产物目录：

```
%TEMP%\ta-e2e\
├─ build-identity.json     被测 exe 的 path / size / mtime / sha256 / git HEAD / dist mtime
├─ real-data-before.json   真实数据目录跑前快照
├─ fixture-clash.yaml      脚本自己起的本地订阅 fixture 内容（不依赖公网）
├─ report.json             全部结果 + 每步证据的机器可读版
├─ shots\                  每步截图（WebDriver GET /session/{id}/screenshot）
├─ webdriver\              每个 WebDriver 请求/响应原文（NNNN-METHOD-path.json）
├─ db\                     scratch db.sqlite 的查询结果快照
├─ logs\                   tauri-driver 的 stdout/stderr + 构建日志
└─ data\                   ← App 的数据根（TA_DATA_DIR）。**唯一允许 App 写的地方**
```

`report.json` 是给自动化用的；`shots/` 是给人看的。

---

## 4. 每一步具体做了什么

| 步骤 | 做什么 | 判据 |
| --- | --- | --- |
| `step0` | 隔离断言 | scratch 出现 `db.sqlite`；真实目录文件级 diff 为空 |
| `step1` | 起窗口、等首帧 | `[data-testid="page-header"]` 命中，并打印当前 hash 与页面 testid 清单 |
| `step2` | 新增供应商：先在 `/providers` 找入口，找不到就走**首启向导**第 1 步（真填表单 → `onboarding-next` → `onboarding-apply-submit`） | 列表出现该供应商 **且** scratch `db.sqlite` 的 `config(kind='provider')` 里有它 |
| `step3` | 新增 Clash 订阅：先在 `/network` 找入口，找不到就走首启向导第 2 步；URL 指向脚本自起的本地 fixture | 列表出现该订阅 **且** `config(kind='subscription')` 里有它 |
| `step4` | 建通道并指派（契约 `lane_upsert` / `lane_set_node`）：在 `/channels`、`/network`、向导里找入口 | 界面出现该通道 |
| `step5` | `/settings` 自启动开关 / 文件监听是不是**真控件** | 区内存在归属到该语义的 `input[type=checkbox]` / `[role=switch]`。**只断言存在，绝不真的改本机自启动** |
| `step6` | 附加证据：设置页改一个整数字段 → 点「校验并应用」 | scratch db 的 `config` 指纹发生变化（证明"真界面→真 IPC→真落库"这条链路是通的） |
| `step7` | 收尾隔离复查 | 真实目录文件级 diff 仍为空 |

`step6` 存在的理由：如果 `step2/3/4` 全靠 SKIP，你要能区分两种完全不同的结论 ——
**"没有写入口"** 与 **"有入口但写不进库"**。`step6` 用界面里确实存在的写路径
（`config_apply`）把后一种可能性排除掉。

### 写入口在哪：`step2/step3` 的两级策略

写命令（`provider_upsert` / `subscription_upsert` / `lane_upsert` / `lane_set_node`）在
`src/shared/api/commands.ts` 里**都声明了**，但**声明 ≠ 有入口**。脚本因此按两级找：

1. **页面原生入口**：当前页上有没有 `data-testid` 带 `add|new|create|upsert` 的可见控件，
   或文案是 `新增|添加|新建|创建|Add|New|Create` 的可见按钮。找到就点它，按字段名启发式填值。
2. **首启向导**（`/onboarding`）：这是界面里**真实存在**的写路径 ——
   `onboarding-provider-add` / `onboarding-provider-field-0-{id,name,baseUrl,secret}` /
   `onboarding-sub-add` / `onboarding-sub-field-0-{name,url}` / `onboarding-apply-submit`，
   第 4 步真的调 `onboarding_apply`。

两级都没有 → 才记 SKIP。**SKIP 一定带"它找了哪些 selector"和"页面上实际有什么"**。

> 注意：向导是**一次 apply 同时写供应商与订阅**的（界面只提供这一个提交按钮）。
> `step3` 复用 `step2` 那次提交，证据里会写明是复用；后端是 upsert 合并语义
> （`settings_ext.rs::apply_onboarding` 从 `state.doc()` 克隆），不会互相覆盖。

### 数据库事实（实测，别猜）

配置实体**不在**独立表里，统一落在 `config(kind, entity_id, ord, json, updated_at)`，
而且 `kind` 是**单数**：

| kind | 是什么 |
| --- | --- |
| `provider` | 服务商 |
| `credential` | 凭据 |
| `subscription` | 订阅源（`proxy_subscriptions`） |
| `section` | 各配置段（`gateway` / `net` / `system_integration` …） |
| `meta` | 元信息 |

`subscription` / `subscription_version` 表是**订阅解析结果**的落点，没跑解析时是空的
（空 ≠ 缺陷）。

### 允许用 `execute script` 的地方

只有两处，且都**不产生写操作**：

1. 改 hash 路由（`window.location.hash = "#/providers"`）—— 页面路由是 HashRouter。
2. 读 DOM 做"入口存在吗"的探测与证据采集。

**任何写操作（点按钮、填表单、提交）一律走 WebDriver 的元素点击与元素输入。**

### 两个"必须容忍"的 WebDriver 现实

- **stale element**：React 在数据到达时整体替换节点，先前拿到的 element id 立刻失效。
  所以读文本/属性一律用 `wd.textOf(selector)` / `wd.attrOf(selector, name)`（内部会重找），
  不要 `find` 完再 `text`。
- **判定"提交完成"要挑对节点**：`onboarding-apply-result` 在数据到达**前**就有一个
  "还没有结果"的占位节点，拿它当完成会读到占位文本。脚本等的是
  `onboarding-apply-raw`（只在 `apply.data` 到位后才渲染）。


---

## 5. 已知限制

- **被测 exe 必须含 `TA_DATA_DIR` 覆盖口**（`src-tauri/src/state.rs` 的 `DATA_DIR_ENV`）。
  脚本启动前会在 exe 二进制里搜这个常量，**搜不到就拒绝启动**（`FAIL`，退出码 1）——
  这是保护，不是故障：覆盖口失效时 App 会立刻往 `%APPDATA%\TokenAgency` 写，
  等 `step0` 断言已经晚了。确要用不隔离的方式跑（**不推荐**）：`--force-unsafe`。
- **机器上不能有别的 TokenAgency 实例在跑**。`src-tauri/src/plugins.rs` 把
  `tauri-plugin-single-instance` 注册为第一个插件，锁是**全局**的（按 app identifier，
  与 `TA_DATA_DIR` 无关）。已有实例在跑时，被测实例会立刻 `exit(0)` 去唤起已有窗口，
  msedgedriver 报 `session not created: Chrome instance exited`，scratch 里只有一个
  0 字节日志。脚本会**先检测并给出 PID**，但**不会替你杀进程**。
- **必须避开被占用的端口**。App 默认监听 `127.0.0.1:8787`（网关）、
  `127.0.0.1:8788`（管理口）、内核控制口 `127.0.0.1:39090`。
- **不启动真实内核**。脚本**不**设置 `TA_MIHOMO`，所以 App 找不到内核就不会去拉 mihomo。
  依赖内核的步骤（订阅解析、线路测速）本就不在断言范围内 ——
  向导第 3 步的 `onboarding_test_all` 会如实报
  `[TA-9001] 没有装配内核：无法测线路连通性`，脚本把它当**证据**记录，不当失败。
- **窗口必须能画出来**。跑在有桌面会话的环境里；无头桌面下 WebView2 可能拿不到窗口。
- **`step2/step3` 的表单填写是启发式的**。走页面原生入口时按
  `id` / `name` / `base_url` / `url` / `protocol` 字段名匹配填值；表单结构差异大时
  要按实际 DOM 调整 `fillFormHeuristic`。走向导时用的是**明确写死的 testid**，不靠猜。
- **`--only step7` 不能单独跑**：`step7` 依赖 `real-data-before.json`，
  那是完整流程开头写下的。至少要跑过一次完整流程。

### 当前状态下预期会 SKIP 的步骤（2026-09-22 观察）

- `step4`（建通道）：`/channels` 是只读矩阵、`/network` 是只读列表，
  `lane_upsert` / `lane_set_node` 在**整个前端产物里从未被引用**（脚本会做这个交叉验证）。
- `step5`（自启动开关 / 文件监听）：`settings-autostart-blocked` 与
  `settings-file-watch-blocked` 都只是**说明文字**；那两个区里唯一的 bool 控件是
  `settings-field-trayShowLatency`（托盘显示延迟），**不是**自启动开关、也不是文件监听开关。

`step2/step3` 能 PASS 靠的是 `/onboarding` 向导，不是页面原生入口 ——
`/providers` 与 `/network` 目前仍是只读的（详情见任务报告里的缺陷清单）。

如果哪天 `step4/step5` 变成 PASS，请同时删掉这一节。


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

# 只跑挂具自己的纯逻辑自检（不用浏览器/exe，秒级）
node scripts\e2e\smoke.mjs --selfcheck
```

### `--selfcheck`：让"只在偶发情形下才走到"的判据也有牙

有些判据**只在特定数据下才触发**（例：`step30` 的"同一地址换了名字"分支 —— 机场里常见，
但不是每轮真机都碰上）。这类修复如果只靠真机跑，"我修好了"就只能靠嘴说，因为
**下一次跑可能根本不走那条分支**。

`--selfcheck` 把这些判定抽成纯函数后**直接喂用例**（含真机那次假红的原样输入），
所以它是**可重复、秒级、不需要 exe** 的。当前覆盖 **14 条**：

| 判定 | 条数 | 覆盖 |
|---|---|---|
| `judgeSticky`（step30 沿用判定） | 5 | 2 条正向（名字没变 / **同一地址换名字**）+ 3 条反向（状态不是"沿用" / 日志与内核不一致 / 内核读不到） |
| `judgePreviousUnreachable`（step30 重挑判定） | 4 | 1 条"无法判定"（不许假红）+ 2 条同层次硬矛盾（必须红）+ 1 条 `app_right` |
| `pricingWriteEntries`（[docs/51] 成本中心只读） | 5 | 1 条只有只读控件 + 3 条混进写入口（含一个**没在清单里**的新入口）+ 1 条取不到 testid |
| `judgePricedMatch`（[docs/51] 落库的 `match` 形状） | 6 | 2 条合格形状（**显式 `null`** / 键不存在）+ 4 条必须红（带 provider / 带 credential / 价挂错模型 / match 不是对象） |

反向用例的作用是防止"为了让那次绿，把判定放松成什么都不查"。

> **`judgePricedMatch` 那条真机上先红过一次，值得记下来**：第一版判据写的是
> "`provider` / `credential` **键必须不存在**"，而落库的实际形状是
> `{"model":"…","provider":null,"credential":null}` —— `PriceMatch` 是 `Option<T>` +
> `#[serde(default)]`，`None` 落库就是显式 `null`（整份配置都这样，例如 `"usd_to_cny":null`）。
> 引擎对两种形状**完全同构**（反序列化后都是 `None`），所以那是挂具**越过语义去要求一种
> 序列化习惯**的假红。判据已改成 `== null`，并把"两种形状等价"用 Rust 测试
> `explicit_null_and_missing_key_are_equivalent` 钉死（不再只靠注释说）。
> ——这两条 `null` 用例就是那次的**原样输入**。

> 给这条自检**验牙**的做法（改判定的人应当照做）：把修复前的旧判据临时加回去，
> 跑 `--selfcheck` 必须变红，且**只有**那条关键用例变红；恢复后必须 20/20 通过。
>
> `pricingWriteEntries` 那条另有一层牙在**前端单测**里（`src/app/cost.test.tsx` 的
> "只做显示"一节）：它直接把一个 `pricing-add-open` 挂回成本页，断言必须变红。
> 两处判据同源但**不共用代码**（一个在 `.mjs`、一个在 `.tsx`），所以它们互为独立证词。

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

#### WebView2 远程调试：为什么 app 里有一条"透传缝"

tauri-driver + msedgedriver 靠 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>`
让 WebView2 开 DevTools 端点。**wry 0.55 只要拿到 `AdditionalBrowserArguments` 就会在进程内
覆盖掉这个环境变量**（`wry-0.55.1/src/webview2/mod.rs` 的 `options.set_additional_browser_arguments`），
于是会话永远建不起来，只会报：

```
session not created: DevToolsActivePort file doesn't exist
```

所以主窗口**在代码里**建（`tauri.conf.json` 的窗口带 `create: false`，见
`src-tauri/src/lib.rs::create_main_window`），并把该环境变量**并进**窗口参数：
**设了才并、没设就一字不差**，生产启动不会设它 —— 它是"把 WebView2 自己的官方约定还给 WebView2"，
不是产品开关。删掉这条缝，真机 E2E 会立刻退回"会话建不起来"。

**调试用**：debug 与 release 产物都能跑（那条缝对两者都生效）。前端产物是**编进二进制**的，
所以改了 `src/**` 必须先 `npm run build` **再** `cargo build`，否则测的是旧界面。

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
PASS step2  供应商页新增供应商 + 密钥（两个向导各自：后端回显 + 落库）
     ...
FAIL step10 成本中心新增定价：填了价、点了保存，但 config 的 pricing section 里没有那条费率
      · 打开对话框后：应有 7 个控件，缺 []
      · 三项全空提交后：点名元素存在=true，配置未变=true
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
| `step2` | **服务商 + 密钥两个向导**：`providers-add-open` → `providers-add-id/-name/-baseUrl` → `providers-add-submit`；关掉对话框后再走 `credentials-add-open` → `credentials-add-id/-label/-provider/-secret` → `credentials-add-submit` | 两次都：后端返回值回显（`providers-add-result` / `credentials-add-result`）**且** `config(kind='provider'/'credential')` 里有它；另外断言**明文密钥不出现在页面上** |
| `step3` | 新增 Clash 订阅：`/network` 的订阅写表单（`network-subscriptions-add-*`），URL 指向脚本自起的本地 fixture | 列表出现该订阅 **且** `config(kind='subscription')` 里有它 |
| `step4` | 建线路：`/network` 的「新建 / 覆盖线路」（`network-lanes-add-*` → `lane_upsert`） | `network-lane-row` 行数 > 0，且该行命中 id **或名称**（表格第一列有名字就显示名字），并**且** `config(kind='lane')` 里有它 |
| `step5` | `/settings` 自启动开关 / 文件监听是不是**真控件** | 区内存在归属到该语义的 `input[type=checkbox]` / `[role=switch]`。**只断言存在，绝不真的改本机自启动** |
| `step6` | 设置页改一个整数字段 → 点「校验并应用」（`config_apply`） | 指纹变化（真界面→真 IPC→真落库）**且 `config_apply` 不许让任何已有实体消失**（见下） |
| `step7` | 收尾隔离复查 | 真实目录文件级 diff 仍为空 |
| `step8` | `/providers` 服务商向导 | `providers-add-*` 真表单 → 列表 + `config(kind='provider')` |
| `step9` | `/models` 模型向导 | `models-add-*` → 列表 + `config(kind='model')` |
| `step10` | `/models` **给一个对外模型定价**（[docs/51]：定价入口从 `/cost` 搬到模型页那一行） | ① `/cost` 上 `pricing-add*` / `pricing-delete` / `pricing-edit` **一个都不许在**，且 `cost-pricing-readonly` 必须有话（"只做显示"的可执行形式）；② 那一行的 `models-price-open` 打得开、模型字段是**只读展示**（格子里 0 个输入框）；③ **三项价格全空**与**非法数字**都在本地点名且**一条命令都不发**；④ 填一项合法价 → `pricing` section 里真的出现 `input=1.5`，且落库条目的 `match` **只有 `model`**（不许带 `provider` / `credential`） |
| `step11` | `/models` 的**两个开关**：模型开关与线路开关 | 模型关掉 → 落库 `enabled=false` **且**网关 `/v1/models` 不再列出它、按名字取 **404**，可逆；线路开关 → **先弹确认框**再落库 |
| `step12` | 网络**未配置**时 `/network` 不渲染任何网络区块 | 只有 `network-optional` + `network-optional-enable`，订阅/线路/分配那些 testid **一个都不存在**（反向断言） |
| `step13` | 订阅流量与更新时间可见 | 行上出现「更新时间」「已用 92.9 GB / 130 GB」「71%」，且与 fixture 的 `subscription-userinfo` 字节数对得上；后端没给就不显示 |
| `step14` | **数据安全回归**：经界面建线路 → 设置页「校验并应用」 | 应用前后 `config(kind='lane')` 的 id 集合不变，`network-lane-row` 仍在 |
| `step15` | 能力不一致时必须被拦住（**SKIP**） | UI 路径随 `/policies` 删除；后端规则由 Rust 单测覆盖（`TA-2007`）。**如实记 SKIP，不假报 PASS** |

`step6` / `step14` 存在的理由：`config_apply` 是**整份覆盖**（`config_store.rs::write_rows` 先
`DELETE FROM config` 再按传入的 `ConfigDoc` 插入），而设置页是"读到什么就整份写回什么"。
如果前端手里那份 `qk.config` 是旧的（`staleTime: 5_000`），提交上去的 doc 里就没有刚建的实体 →
**用户的线路被静默删掉**。第 70 轮真机实测到过（`config_snapshot` 表：label=`lane_upsert` 的 doc
有 1 条 lane、label=`apply` 的有 0 条）。修法：`useCommandMutation` 成功后集中失效 `qk.config`
+ 设置页 `refetchOnMount: "always"`；上面两条断言就是那个缺陷的回归闸门。

能力一致性规则（用户硬规则：**只有能力完全一致才允许被同一路由指向**）现在只剩**后端那一道** ——
`/policies` 整页已删（[docs/48]），界面上的"禁用提交 + 逐字段差异"随之消失，
由 Rust 单测覆盖 `TA-2007`。`step15` 因此**如实记 SKIP**（它原来要验的 UI 路径不存在了）；
真机这一档不再假装还验着。

### 写入口在哪：两级策略

写命令（`provider_upsert` / `credential_upsert` / `model_upsert` / `channel_upsert` /
`lane_upsert` / `pricing_upsert` …）在 `src/shared/api/commands.ts` 里**都声明了**，
但**声明 ≠ 有入口**。脚本按两级找：

1. **页面专用入口**：`data-testid`（如 `providers-add-open` / `models-add-*` /
   `network-lanes-add-*` / `models-price-open`）。这是主路径。
2. **首启向导**（`/onboarding`）：只在专用入口缺失时兜底，并如实记录"走了兜底路径"。

两级都没有 → 才记 SKIP。**SKIP 一定带"它找了哪些 selector"和"页面上实际有什么"**。

> `/channels`（导航标签「套餐与线路」）整页已按用户裁决删除（[docs/49]），
> 所以 `channels-add-*` 这一串 testid **一个都不存在了**，`channels-add-lane` / `channels-add-model`
> 之类不要再往脚本里加。线路的**真路径**现在是：加服务商向导（扫描 → `provider_lines_create_all`
> 自动建全部线路）→ 服务商详情页用滑块启停。

`step10` 需要"界面前提"（服务商/凭据/**模型**）。`--only` 跳步跑时 `ensureAddPrereqs()` 会**经真界面把前提造出来**
（首启向导 → `/models` 向导 → `/network` 线路表单），而不是放宽断言。

> **[docs/51] 之后 step10 不再需要凭据**：定价的对象是**对外模型**，界面上不再挑服务商与凭据
> （`match` 只写 `model`）。前面那些"没有凭据就没有可选项"的分支随之消失 ——
> 它现在只要求**配置里有模型**（不然模型页没有行，也就没有定价入口，那种情况如实记 SKIP）。


> 注意：首启向导是**一次 apply 同时写供应商与订阅**的（界面只提供这一个提交按钮）。
> 后端是 upsert 合并语义（`settings_ext.rs::apply_onboarding` 从 `state.doc()` 克隆），不会互相覆盖。

### `config` 表里还有哪些 kind

除了下表，还有 `model` / `lane` / `channel` / `channel_group` / `policy`（都是单数），
以及 **`pricing` 是 `kind='section', entity_id='pricing'`** —— 计费条目在它的 `json.entries[]` 里，
不是独立的 kind。断言"计费落库"要去 section 里找。

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


### 两个"必须记住"的 MUI 陷阱（第 70 轮真机踩出来的）

1. **收 Select 弹层绝不能用"全局 Esc"，也不能点视口角落**：MUI 的 `Dialog` 也吃 Esc，
   而空下拉时焦点还在 Dialog 里 → Esc 把**整个对话框**关掉；点角落那一下在退场动画期间会落到
   Dialog 自己的 backdrop 上，同样关掉对话框。两者都会把一个"下拉为空"的根因放大成十条
   "缺控件 / not interactable"的假失败。正确做法：**把 Esc 送到 `[role=listbox]` 元素自己身上**
   （`POST /element/{id}/value`），事件只在弹层内部冒泡；而且成功选中后**先等 700ms**看弹层
   是否自己退场，别去碰正在退场的弹层（`closeMenuIfOpen()` 里就是这么写的）。
2. **真输入不能只靠 W3C `clear()`**：它对部分 MUI 受控输入框是 no-op，结果是**追加而不是替换**
   （实测 `"200000"` → `"0200000"`、向导 id 框 `"p1"` → `"p1ta-e2e-provider"`）。
   `wd.fill()` 现在补了一次真键盘清空（Home + Delete × 长度），清不干净会在日志里明说。

另外：**多选（MUI `multiple`）选完不关弹层**，只能"弹层打开一次、在里面连点 N 个"。
这条能力（`wdPickOptions` / `wdKeepOptions`）的唯一使用者是 `/policies` 的路由向导，
它随整页删除（[docs/48]）—— 那两个 helper 也已删掉，别再照着旧文档去找它们。

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
- **`--only step7` 不能单独跑**：`step7` 依赖 `real-data-before.json`，
  那是完整流程开头写下的。至少要跑过一次完整流程。
- **同一构建的两条线路 = 同一个服务商 + 不同上游模型名**。线路 id 由
  `provider × credential × 上游模型名` 派生（[docs/28 §2] 翻转后**线路归服务商**，
  不再按逻辑模型派生），所以同 provider / 同 credential / 同上游模型名的第二次 upsert 是
  **覆盖**同一条，不会多出一条。
  想要"同一个模型下两条线路"，只能：建两条不同上游模型名的线路 → 到 `/models` 把它们
  **都分配给同一个模型**（关系的真源在 `Model.lines` 上）。
  > 这里原来写的是"靠第二份凭据（`ta-e2e-cred-2`）造" —— 那是翻转前的做法，
  > 现在一个服务商就一把密钥（[docs/46 §303]），已经没有"第二份凭据"这条路了。

### 当前状态

**以最近一次全量真机跑的报告为准**（`report.json` 里的 PASS/SKIP/FAIL 计数）；
最近一次是 `PASS=29 SKIP=2 FAIL=0`（[docs/49 §7.4.1]）。两个 SKIP 都如实写了理由
（`step15` UI 路径随 `/policies` 删除、`step28` 夹具量不出真实节点延迟）。
**别把本文件里的数字当结论 —— 结论只在报告里。**

添加向导现在的专用入口：`/providers`（服务商与密钥，含扫描 → 自动建全部线路）、
`/models`、`/cost`（新增定价）。`step4` 的线路写入口在 `/network`。

> 上面这段原来写着"（2026-09-22 第 70 轮实测）`PASS=19`…四个向导…"，那已经过期两轮以上：
> `/channels` 与 `/policies` 两页都已删除（[docs/49] / [docs/48]），
> 而"同模型两条线路靠第二份凭据造"也不再成立（[docs/28 §2] 翻转后线路归服务商）。
> 历史数字不再保留 —— 留着它只会让人对着一个过期的绿数做判断。



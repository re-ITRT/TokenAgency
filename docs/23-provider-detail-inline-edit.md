# docs/23：服务商详情 = 查看 + 编辑一体（第 75 轮）

> 来源：用户第 75 轮原话（三条）：
> 1. 「就是后多[有很]多大学生文字显示，有些是[是]必要提示，我希望改成有一个问号加一个圈，鼠标移动上去显示提示」；
> 2. 「服务商编辑页面不走下一步下一步，直接把所有信息在点开服务商之后提供可视化编辑界面，
>    相当于编辑和查看一体了，当然不是纯文本编辑」；
> 3. 「还有就是所有『项』的删除和批量删除，不知道实现没有，还是说是半成品」。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据 / 不排空占位；页面只调用已注册命令；
每步先证明测试会红；共享工作区不留不可编译状态。

---

## 0. 事实基础（本轮先"看清楚再动手"，三条各自查了一遍）

| 需求 | 取证结论 | 证据 |
| --- | --- | --- |
| ① 长篇收进问号圆圈 | **已经做完约 95%**：`HelpTip` 共用件 + `PageHeader` 收 `description` + ~45 个显式位点，各页都有"收了什么 / 红线留在版面上"双向单测 | `docs/22`、`src/shared/components/HelpTip.tsx`、各 `*.test.tsx` 的 §docs/22 段 |
| ① 残留 | **两处同一句解释仍铺在版面上**：`logs.trace.rawHint`（行详情与轨迹面板各一处），而服务商详情里**同一个意思**早已收进问号 | `src/pages/logs/index.tsx:213`、`src/pages/logs/TracePanel.tsx:416` |
| ② 服务商编辑 | **没有实现用户要的形态**：详情页是只读表格 + 一个「编辑」按钮 → 打开**两步向导的编辑模式**（还是"下一步"） | `docs/20 §4`、`src/pages/providers/detail.tsx`（旧版 `providers-detail-edit`） |
| ② 顺带抓到的真缺陷 | **编辑会静默抹掉这家服务商的连接配置**：编辑向导只发 4 个字段（id/name/protocol/base_url），而 `provider_upsert` 是**整条覆盖写**，`Provider` 的可选字段全都有 `#[serde(default)]` → auth / headers / timeout / tls / lane / passthrough 全部被重置成默认值 | `crates/ta-core/src/domain/mod.rs:696-726`（`serde(default)`）、`src-tauri/src/commands/catalog.rs:207-215`（`*slot = provider.clone()`）、`src/pages/providers/providerWizardModel.ts`（旧的 `step1ProviderPayload`） |
| ③ 删除 | **7 类实体已做完**（单条 + 批量 + 级联 + 逐条汇报）：服务商 / 凭据 / 模型 / 通道 / 通道组 / 策略 / 定价条目。**3 类半成品**：订阅与线路**连二次确认都没有**、API key 删除**失败无声**，三者**都没有批量删除** | `docs/21 §3` 的 testid 表、`src/pages/network/index.tsx`、`src/pages/apikeys/index.tsx` |

**结论**：① 只差两处补齐；③ 是"接线没接完"（命令都在，页面没接）；② 要按用户说的形态重做，
并且**必须同时根治上面那个数据丢失缺陷** —— 否则"能编辑"只是把一个会丢数据的入口换个样子。

---

## 1. 需求 ②：详情页就是编辑器（[docs/23] 的主体）

### 1.1 形态裁决

- **点开服务商 = 进入编辑器**：`/providers/:id` 直接给出全部可视化控件（输入框 / 下拉 / 开关 /
  可增删的头表 / 模型区 / 凭据表），页面底部一条保存条。
  **不再有 `providers-detail-edit`，也不再有任何"下一步"。** 两步向导**只保留"新增服务商"**，
  第 72 轮那套 `mode: "edit"` / `startEditSession` 连同它的 4 字段载荷一起删掉
  （留着是一条没人走、也没人测、而且会丢数据的路径）。
- **不是纯文本编辑**：没有任何一处让用户直接编辑 JSON。原始记录仍然原样显示在「原始字段」区，
  但那是**证据**，不是编辑界面。
- **健康度是只读遥测**：状态 / 成功率 / 平均延迟 / 通道数来自后端只读视图，不是配置，
  所以它们永远只读（[docs/22 §2] 要求数值一眼可见 —— 它们留在版面上）。

### 1.2 载荷：以原始记录为底（`src/pages/providers/editor.ts`）

`buildProviderPayload(row, draft)` 的三条规则：

1. **从后端返回的原始记录复制一份**，只覆盖用户真的改过的键 →
   没暴露过的自定义字段（`#[serde(flatten)] extra`）**一个都不丢**；
2. **剔除只读视图专有的键**（`status` / `success_rate_24h` / `avg_latency_ms` / `channel_count` /
   `provider_name`）—— 它们不是配置，带回去会被后端当成用户自定义字段存进 `extra`；
3. 用户清空的 `Option` 字段发**显式 `null`**（`serde` 把 `null` 解成 `None`）：
   不发这个键等于"用默认值"，那不是"清空"。

### 1.3 只读视图不许写

拿到只读视图（没有 `auth` / `timeout` / `tls` / `headers` / `passthrough` / `lane` 任何一个键）时，
**保存会用默认值覆盖线上配置** → 整页只读，并在最上面用一句话说清"为什么"
（`providers-view-only`），连接区照旧显示"后端返回的是服务商视图"。
不排空占位、不给假保存按钮。

### 1.4 密钥仍然只走密钥仓库

- 密钥框留空 = **不改**；填了才 `credential_set_secret`，写的是这家**已有**的那把凭据
  （`<provider>-default` 优先，否则第一条）；一把凭据都没有时先 `credential_upsert` 建一份再写。
- 明文不进 `provider_upsert` 的载荷、不进错误消息、不进日志、不上屏；写成功后立刻清空输入框。
- 界面上如实说明"这一次会写到哪份凭据"（`providers.edit.keyTarget` / `.keyNewCredential`）。

### 1.5 testid（真机 E2E 与单测共用，不许改名）

| 位置 | testid |
| --- | --- |
| 页头 | `providers-detail` / `providers-detail-delete` / `providers-detail-notfound` / `providers-detail-error` |
| 只读视图说明 | `providers-view-only`（+ 连接区 `providers-connection-unavailable`） |
| 基本信息 | `providers-overview` · `providers-edit-name` / `-protocol` / `-baseUrl` / `-enabled` / `-lane` / `-modelPrefix` · `providers-health` |
| 鉴权与连接 | `providers-connection` · `providers-edit-authScheme` / `-authHeader` / `-authPrefix` / `-timeoutConnect` / `-timeoutRead` / `-timeoutTotal` / `-tlsInsecure` / `-tlsMinVersion` / `-extraCa` |
| 附加头 | `providers-headers` · `providers-headers-editor` · `providers-header-row` · `providers-header-name` · `providers-header-value` · `providers-header-add` · `providers-header-remove` · `providers-header-empty` |
| 模型 | `providers-models` · `providers-model-row` · `providers-model-lane` · `providers-model-scan` · `providers-model-scan-row` · `providers-model-scan-select-all` · `providers-model-add` · `providers-model-channels-created` |
| 凭据 | `providers-credentials` · `providers-edit-secret` · 既有的 `providers-credential-*`（删除 / 批量 / 级联） |
| 保存 | `providers-save-bar`（**吸在视口底部**：这一页很长，保存不该埋在滚动末尾）· `providers-detail-save` · `providers-detail-reset` · `providers-detail-save-result` · `providers-detail-save-error` · `providers-detail-issues` |
| 问号 | `providers-overview-help` / `-connection-help` / `-headers-help` / `-credentials-help` / `-raw-section-help` / `-models-help`（**每个区块最多一个**，[docs/22 §3]） |

### 1.6 线路仍然只在通道页改

模型区里的线路列是**只读展示** + 一句指路（[docs/21 §4.3]），本页不提供任何写 `channel.lane` 的控件；
`provider.lane`（服务商级默认线路）**属于服务商自己的配置**，在基本信息区可改。

---

## 2. 需求 ①：补齐最后一处残留

- `src/pages/logs/index.tsx`（行详情）与 `src/pages/logs/TracePanel.tsx`（轨迹面板）里那句
  「后端返回的整条记录，原样显示（未做任何清洗）」收进标题右边的问号：
  `logs-row-raw-help` / `logs-trace-raw-help`；下面的原文（用户数据）一个字都没动。
- 服务商详情页那五处问号保持原位（本轮重写页面时**逐个搬过去**，没有少一个）。
- `logs.test.tsx` 里"这一页正文只有两个问号"的精确列表断言按规格更新
  （行详情那一处由「展开某一行」用例单独钉住）。

## 3. 需求 ③：删除与批量删除的收口（写范围：`src/pages/network/**`、`src/pages/apikeys/**`）

按 [docs/21 §1] 的统一语义补齐三类半成品实体：

| 实体 | 本轮补什么 |
| --- | --- |
| 订阅 | **二次确认**（`ConfirmDelete`，`cascadeAvailable=false`，被引用时原样显示后端原文）+ **批量删除**（逐条 `force=false`、逐条如实汇报）+ 连带清单（引用它的线路数，算不出来就不写） |
| 线路 | 同上；另外成功删除后要失效 `qk.channelMatrix` / `qk.laneAssignments` / `qk.lanePickOptions`（否则界面上的线路名是旧的） |
| API key | 删除失败**必须显示后端原文**（此前对话框先关、错误没人渲染 = 无声失败）+ 对话框点名目标 + **批量删除** |

testid 沿用 [docs/21 §3] 的拼法：`<prefix>-delete` / `-delete-dialog` / `-delete-confirm` /
`-delete-cascade` / `-delete-error` / `-delete-result`，以及 `<page>-select` / `-select-all` /
`-selected-count` / `-delete-selected`；前缀分别是 `network-subscriptions` / `network-lanes` / `api-key`
（**`api-key-*` 而不是 `api-keys-*`**：仓库惯例是"多选前缀 = 行前缀"，行是 `api-key-row`）。

两处实现裁决（主管拍板，写在这里免得后人再问）：

1. **线路的"会连带影响"清单真的去读通道矩阵**（`channel_matrix` 的每一格带 `lane_id`，一次读就够；
   `lane_reverse_lookup` 要逐条问）。它是**只读**命令、已在 `collect_commands!` 里，I2 不受影响；
   `network.test.tsx` 的只读白名单相应加了一条，并在注释里写了理由。
2. **删除失败的结果区不重复抄原文**：单条失败时后端原文只留在确认框里（用户正看着它）；
   批量则一定给出"成功 N 条 / 失败 M 条" + 每条失败项的 id 与后端原文。

---

## 4. 验收

1. **单测（先证明会红）**
   - `src/app/providerEditor.test.ts`（纯逻辑）：草稿读取、`canEdit`、预检、`isDirty`、
     **载荷以原始记录为底**（自定义字段保留 / 视图专有键剔除 / 清空发显式 `null` / 头名去重与空名丢弃）。
   - `src/app/providers.test.tsx` §[docs/23]：点开就是编辑器（没有 `providers-detail-edit` 与 `pw-next`）、
     控件里是后端真值、无改动时保存不可点、改一个字段后**载荷整条齐备**（回归那个数据丢失缺陷）、
     预检不过一次都不发、后端拒绝原样显示 TA 码、头表可加可删、密钥留空不写仓库 / 填了才写、
     模型区扫描与添加、只读视图整页只读。
   - `src/app/logs.test.tsx`：两处 raw 说明收进问号、正文不再内联、原文仍内联。
   - `src/app/network.test.tsx` / `src/app/apikeys.test.tsx`：§3 的三类实体（确认框先行、失败原样显示、批量逐条汇报）。
2. **真机 E2E**
   - `step20` 重写：详情页即编辑器（控件齐全、**没有**任何向导痕迹）→ 控件值 = 库里真值 →
     无改动时保存不可点 → 改名 → 保存 → 库里真的变、条数不变 →
     **没碰过的 auth / headers / timeout / tls / lane / model_prefix / passthrough / protocol /
     base_url / enabled 逐项指纹比对：一个字节都没变** → 改回原名并再次核对指纹。
   - 既有步骤继续全绿（不许放宽旧断言）。
   - **本轮没跑真机 E2E**（需要打包 + 内核夹具），如实登记在 CHANGELOG；上机后按上面这份判据跑。
3. **门禁**：实际跑过的结果（本轮，工作区最终状态）
   - `npm test` → **26 files / 604 tests 全过**（改动前基线 25 files / 573 tests）。
   - `npx tsc --noEmit` → exit 0；`npm run build`（`tsc` + `vite build`）→ 成功。
   - `pwsh -File scripts/build.ps1` **整条流水线 exit 0**：前端构建 + `npm test` →
     `cargo fmt --check` → `cargo check --workspace --all-targets` → `cargo clippy -D warnings` →
     `cargo test --workspace` → `cargo tauri build --bundles nsis`，
     产物 `target/release/bundle/nsis/TokenAgency_0.1.0_x64-setup.exe`
     （5.36 MB，SHA256 `C946F469E44A3D410F7A1DD929F8D40AD5F68D5AAE0AC512117321269F6FEE7E`）。
   - `design.guard` / `noPlaceholder.guard` / `commandSurface.guard` / `registryClaim.guard` 继续绿。
     （本轮**没改 Rust**；工作区里那几处 `crates/ta-gateway/src/adapt*` 的未提交改动是上一轮遗留，
     本轮只是确认它照样过 fmt / clippy / test。）
4. **没做到的（如实登记）**
   - **真机 E2E 没跑**：step20 已按上面这份判据重写，但跑它需要内核夹具 + WebView2 驱动 +
     `--executable-path`，本轮没有执行。上机后请按 §4 第 2 条逐项复核；
     **特别要确认"连接配置指纹一个字节都没变"那一条**。
   - **A3/A4 不变量没有在新安装包上复跑**（`scripts/check-invariants.ps1` 要真跑起内核）。
   - 凭据的**字段级编辑**（label / 配额 / 线路 / 标签）本轮没做：它们属于凭据域，
     这一页只提供"写密钥 + 删除凭据"；要做需要先裁决"凭据详情放哪一页"。

---

## 5. 分工（本轮）

| 工作包 | 负责人 | 写范围 |
| --- | --- | --- |
| **WP-75A**：详情页查看 + 编辑一体（新 `editor.ts`、重写 `detail.tsx`、删掉向导编辑模式、单测） | lead | `src/pages/providers/{editor.ts,detail.tsx,providerWizard.tsx,providerWizardModel.ts}`、`src/app/{providers,providerEditor}.test.tsx`、`src/shared/i18n/*`、`scripts/e2e/smoke.mjs`、`docs/**` |
| **WP-75B**：问号圆圈残留补齐（日志两处） | lead | `src/pages/logs/{index,TracePanel}.tsx`、`src/app/logs.test.tsx` |
| **WP-75C**：订阅 / 线路 / API key 的删除与批量删除收口 | teammate | `src/pages/network/index.tsx`、`src/pages/apikeys/index.tsx`、`src/app/{network,apikeys}.test.tsx` |

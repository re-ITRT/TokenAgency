# docs/21：各种项的删除 + 线路设置改到通道页（第 73 轮）

> 来源：用户第 73 轮原话：
> 1. 「用户可能需要有各种项的删除」；
> 2. 「线路设置不能点进服务商里面去修改」。
>
> 用户在开工前明确选定（`ask_user_question` 的答复）：
> * 线路**只在通道页改**（每个通道一个线路下拉）——没有选"模型页"、没有选"网络页批量指派"、也没有选"服务商向导里保留"；
> * 删除**全部实体都补**，并且要**批量/危险**删除。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；页面只调用已注册命令；每步先证明测试会红；共享工作区不留不可编译状态。

---

## 0. 事实基础（第 73 轮取证）

| 事实 | 证据 |
| --- | --- |
| 后端**10 条删除命令都已注册**：服务商/凭据/模型/通道/分组/策略/计费/订阅/线路/API key | `src-tauri/src/commands/mod.rs:177,179,184,186,191,195,201,240,250,269` |
| 前端 `api` 封装也都有 `remove(...)` | `src/shared/api/commands.ts:128,140,157,181,192,202,212,229,271` |
| 但**页面只接了 3 处删除**：订阅、线路、API key | `network-subscriptions-delete` / `network-lanes-delete` / `api-key-delete` |
| 服务商 / 凭据 / 模型 / 通道 / 分组 / 策略 / 计费 **一个删除入口都没有** | 全仓 `data-testid="*delete*"` 只有上面 3 处（+ api-key 的确认按钮） |
| `channel_bulk_assign_lane` 只接受**已存在的** lane id，**没有"清空/回到继承"** | `entity.rs` 的 `channel_bulk_assign_lane`（`if !doc.lanes.iter().any(...) { not_found }`；`c.lane = Some(...)`） |
| 通道页**只显示**实际生效的线路、不能改 | `channels/index.tsx:495-508`（`channels-lane` 是纯展示 + 一行"继承"提示） |
| 改"某个模型/通道走哪条线路"目前**只能进服务商向导** | `providerWizard.tsx` 的 `pw-model-lane`（第 72 轮加的） |
| 设置页**已有**快照列表与回滚 UI | `settings/index.tsx:320`（`config_history_list`）、`:329`（`config_rollback`）、`:1521`（快照区） |
| `pricing_list` **没有任何页面在读**；成本页明确写着"定价写命令尚未接入，不放按钮" | `cost.tsx:8`；全仓 grep `pricing_list` 无页面命中 |

**由此得结论**：第一件事是"接线"（命令都在，页面没接），第二件事是"换个地方"（把线路编辑从服务商向导搬到通道页）。

---

## 1. 删除：统一语义（每一类实体都一样）

### 1.1 单条删除

1. 列表/详情上给出「删除」按钮，testid 见 §3 的表；按钮文案用词统一（`common.delete`）。
2. 点击后**必须先弹二次确认**（`ConfirmDelete`，见 §2），对话框里写清**删的是哪一个**（名称 + id）。
3. **被引用时不许假装成功**：
   - 先以 `force = false` 调（`provider` / `credential` / `model` 支持 `force`；其余命令没有这个参数）；
   - 后端以"被引用"报错时（`TA-5002` / `TA-5001` 一档）：**原样显示后端原文**，并把前端能从现有数据算出的"会连带删掉什么"列出来
     （例如某服务商：`N 个凭据 · M 条通道 · 被 K 条策略引用`），再给一个**明确的**危险按钮（`<prefix>-delete-cascade`，文案"确认级联删除"）；
   - 只有 `provider` / `credential` / `model` 有级联按钮；`channel` / `group` / `policy` / `pricing` / `subscription` / `lane` / `api_key`
     **没有** force 语义 → 这一类被引用时**只显示后端原文**，不放任何"强行删除"的按钮（那会是假按钮）。
   - **原文里指的路必须是真路**：只有真的带 `force` 参数的命令才允许在拒绝消息里承诺"要连带删除请传 force=true"；
     `channel` / `group` 没有这个参数，只能说"先解除这些引用再删"。第 73 轮真机 E2E 抓到
     `channel_delete` 的原文里带着 `force=true`（用户照做只会再撞一次墙）→ 已改成
     `referenced(what, refs, force_capable)`（`entity.rs`），夹具同时钉住"没有 force 的实体，原文里不许出现 force"。
4. 成功：`invalidateQueries` 覆盖受影响的查询（该实体的列表 + `qk.channelMatrix` / `qk.models` / `qk.credentials` 等），
   并在结果区如实写"已删除 <名称>"；失败：原样显示后端原文（**含 TA 码**）。
5. `pricing_delete` 的入参是 **index**（`entity.rs` 的 `delete_price(state, index: usize)`），所以：
   - 删之前**重新读一次** `pricing_list`，**逐字段比对**要删的那条仍然在那个下标上（`JSON.stringify` 全等）；
   - 不一致（列表变了）→ **拒绝并报错**，让用户重看一遍，绝不在错位上下手。

### 1.2 批量删除（用户明确要的）

- 列表每行前加多选：`<page>-select`（行）、`<page>-select-all`（表头全选）；选中数 `<page>-selected-count`。
- 「删除选中」`<page>-delete-selected` → 一次确认 → **逐条**调用该实体的删除命令（`force = false`），
  逐条记录结果，最后如实汇报：`成功 N 条 / 失败 M 条`，失败项**逐条**给出 id + 后端原文。
  **不许**用"批量删除成功"一句盖住部分失败；也不许因为一条失败就中止其余条目。
- 单条删除与批量删除**走同一套语义**（同一个确认组件、同一套失败展示）。

### 1.3 危险操作（用户明确要的）

- 本轮的"危险"= ① 级联删除（§1.1 第 3 条）、② 批量删除（§1.2）。两处确认框必须写清后果。
- 配置级的危险操作**沿用设置页已有的快照/回滚**（`config_rollback`），本轮**不新造**"一键清空配置"：
  后端没有这条命令，宁缺毋滥（要造也得先有命令 + 不变量评审）。

---

## 2. 共享组件（主管先写，两个工作包共用）

`src/shared/components/confirmDelete.tsx`：

```ts
export type DeleteKind = "provider" | "credential" | "model" | "channel" | "group" | "policy" | "pricing";

export type ConfirmDeleteProps = {
  open: boolean;
  title: string;          // 「删除服务商」
  targetLabel: string;    // 名称 + id
  /** 会连带删除/影响的清单（前端算得出来的事实；算不出来就空数组，不编） */
  consequences?: readonly string[];
  /** 允许级联（只有 provider/credential/model 为 true） */
  cascadeAvailable: boolean;
  pending?: boolean;
  /** 后端原文（被引用时的错误）；有它就展示，并把级联按钮显出来 */
  errorText?: string;
  onConfirm: () => void;
  onConfirmCascade: () => void;
  onCancel: () => void;
};
```

`<prefix>-delete-dialog` / `-delete-confirm` / `-delete-cascade` / `-delete-cancel` / `-delete-error` 这些 testid 由**调用方**传前缀拼出来。

---

## 3. 入口与 testid（写者按此实现，不许改名）

| 实体 | 页面 | 单条删除 | 批量 | 级联（force） |
| --- | --- | --- | --- | --- |
| 服务商 | `/providers` 列表 + `/providers/:id` 详情 | `providers-delete` / `providers-detail-delete` | `providers-select` / `-all` / `-delete-selected` | 有（`providers-delete-cascade`） |
| 凭据 | `/providers` 的凭据表 + 详情页凭据表 | `providers-credential-delete` | 同上（可共用多选） | 有（`providers-credential-delete-cascade`） |
| 模型 | `/models` 列表 | `models-delete` | `models-select` / `-all` / `-delete-selected` | 有（`models-delete-cascade`） |
| ~~通道~~ | ~~`/channels` 列表~~ **页面已删** | ~~`channels-delete`~~ | ~~`channels-select` / `-all` / `-delete-selected`~~ | 无 |
| ~~通道组~~ | ~~`/channels` 新增「通道组」区块~~ **页面已删** | ~~`groups-delete`~~ | ~~`groups-select` / `-all` / `-delete-selected`~~ | 无 |
| 策略 | ~~`/policies` 列表~~ **页面已删** | ~~`policies-delete`~~ | ~~`policies-select` / `-all` / `-delete-selected`~~ | 无 |
| 定价条目 | `/cost` 新增「定价条目」区块 | `pricing-delete` | `pricing-select` / `-all` / `-delete-selected` | 无 |

> **[docs/49] 后记（2026-09-30）**：上表里**划掉的三行**的页面都已按用户裁决整页删除。
> 命令**一个都没删**（`channel_delete` / `group_delete` / `policy_delete` 仍在，
> `command_surface_guard` 照常通过），只是**界面不再有那些入口** —— 这是**能力减法**，
> 逐条记在 [docs/49 §3](49-channels-page-removed.md)。
> **现在还能在界面上删的只有**：服务商（含级联）、模型（含级联）、定价条目。
> 线路本身只能到**服务商详情页**逐行移除（`providers-line-remove` + 确认框，[docs/28 §1.5]）；
> 通道组、批量删线路、按 `use_channels` 拒绝的那一档**当前都没有界面入口**。

三个共用 testid：删除结果区 `<prefix>-delete-result`、删除错误 `<prefix>-delete-error`（前缀见上表左列的单条删除名去掉 `-delete`）。

---

## 4. 线路设置：搬到通道页（用户明确指定）

### 4.1 后端先补一个能力（否则「直连」选不了）

`channel_bulk_assign_lane` 目前要求 `lane_id` 必须存在，**没有办法把通道改回"不指定"**。
契约与实现都改成可空：

```ts
channel_bulk_assign_lane: { args: { channel_ids: ChannelId[]; lane_id: LaneId | null }; result: ApplyResult };
```

- `lane_id: "<id>"` → 该 id 必须存在于 `doc.lanes`（否则 `TA-1004` 指名道姓，与今天一致）；
- `lane_id: null` → **清空**这批通道的 `lane`（回到五级继承链，末端就是直连）。

### 4.2 通道页每行一个线路下拉

- testid `channels-lane-select`（容器；内部是真 `combobox`，沿用既有 `SelectField` 形状）。
- 选项：**第一条永远是「直连（继承）」`value: ""`**，其余来自 `lane_list`（label 用 `lane.name`，没有名字回落 id）。
- 选直连/继承 → `lane_id: null`；选某条线路 → 该 lane id。
- 提交：`channel_bulk_assign_lane({ channel_ids: [<该行通道 id>], lane_id })`；
  成功后 `await` 重取矩阵（值要真的变回来），失败写进 `channels-lane-error`（后端原文）。
- 行内仍要能看出**当前生效的线路**（`channels-lane` 保留），并在是"继承"时说明它继承到了哪条。

### 4.3 服务商向导里的网络代理改成**只读**

用户的原话是"线路设置不能点进服务商里面去修改"，所以：

- `pw-model-lane` 从"可改的下拉"改成**只读展示**：显示当前生效的线路（默认「直连」）+ 一句提示
  「线路在通道页改」（i18n `providers.wizard.laneReadonlyHint`）；
- 仍然保留：添加模型时按 §1.2 的三级默认把 lane 写进新建通道（那是**创建时的默认值**，不是"线路设置"）；
- 单测要相应改：向导里**不再**出现 `model_bulk_update({patch:{lane}})` 的调用；`pw-model-lane` 不再是可写控件
  （`pw-model-info-panel` 那条"面板内不许有可写控件"的纪律不受影响）。

---

## 5. 契约与 i18n

- `src/shared/bindings.ts`（**主管**）：`channel_bulk_assign_lane.lane_id` 改成 `LaneId | null`；手写参考同步。
- `src/shared/i18n/*.ts`（**主管**）：删除共用文案（`common.delete` / `common.deleteSelected` /
  `common.deleteConfirmBody` / `common.deleteCascade` / `common.deleteCancel` / `delete.result.*`）、
  各实体的标题与"会连带删除"清单文案、`channels.lane.select` / `channels.lane.inherit` /
  `providers.wizard.laneReadonlyHint`、`cost.pricing.*`、`channels.channelGroups.*`。
  **写者缺键报主管，不要自己插**。

---

## 6. 验收

1. 单测（先证明会红）：每类实体的"点了真调什么载荷 / 二次确认 / 被引用时原样显示后端原文 + 级联按钮 /
   批量逐条汇报（含部分失败）/ pricing 的下标比对拒绝错位"；通道页线路下拉的三种取值（直连=null、线路=id、失败原样显示）；
   服务商向导里不再发 `model_bulk_update`。
2. 真机 E2E `step21`（**实际验了什么，就写什么**）：
   - 通道页改线路：选 `ta-e2e-lane` → config 里该通道的 `lane` 真的变；选「直连（继承）」→ 真的变回 `null`。
   - 实体删除：**真删掉**了模型 / 凭据 / 服务商 / 策略 / 定价条目各一条（config 表里真的少一行，
     且每条都先出现确认框）；模型与服务商走了级联按钮，凭据是 `force=false` 就删掉了。
   - 通道：① **被策略 `action.use_channels` 点名的那条必须被拒绝**（`TA-5002` + 原文 + 没有级联按钮
     + 库里一条没少 + 原文里没有 `force=true`）；② 没有引用的那条必须**真删掉**；
     ③ 再按原文的建议走一遍：删掉点名它的策略 → 再删那次通道**必须真的少一条**。
     夹具的判据是 `/TA-500[12]/` + 原文含"先解除" + 不含 `force=true` + `removed === 0` + 无级联按钮；
     **"必须拒绝"那一档如果反而删成功了会判红**（把被引用的实体删掉是缺陷，不是合格）。
   - **没验到的**：`delete_channel` 的另外两类引用（`pin_sticky`、"组里只剩它一个成员"）在 E2E 配置里
     造不出来 → 由 Rust 单测覆盖；通道组在 E2E 配置里是 0 条 → 这一档如实跳过并记录理由；
     `policy_delete` **没有**被引用检查（策略 id 不被任何东西引用），所以它不存在"被拒绝"这一档。
   - 批量删除：勾选 ≥2 条 → 逐条汇报（成功/失败都算结论，只要如实）。
   - 既有步骤必须继续全绿（不许放宽旧断言）——特别是 `step2`/`step20`（它们依赖向导里的 lane 行为，
     要跟着 §4.3 调整）。
3. 干净快照全量门禁 + 重出安装包 + A3/A4 不变量。

---

## 7. 分工与写范围

| 工作包 | 负责人 | 写范围 |
| --- | --- | --- |
| **WP-73A**：服务商 / 凭据 / 模型的删除（含批量与级联确认） | E1 | `src/pages/providers/**`、`src/pages/models/**`、`src/app/{providers,providerWizard,models}*.test.tsx` |
| **WP-73B**：通道 / 分组 / 策略 / 计费的删除 + **通道页线路下拉** | E1 | ~~`src/pages/channels/**`、`src/pages/policies.tsx`~~（两页已删，见 [docs/49] / [docs/48]）、`src/pages/cost.tsx`、~~`src/app/{channels,policies}.test.tsx`~~ → `src/app/cost.test.tsx`、`src/app/providerWizard.test.tsx`（仅 §4.3 那几条） |
| **WP-73C**：`channel_bulk_assign_lane` 的 `lane_id` 可空（清空=继承） | E3 | `src-tauri/src/commands/entity.rs` |
| 主管 | lead | `src/shared/**`（bindings / i18n / commands / 共享确认组件）、`docs/**`、`scripts/e2e/smoke.mjs`、门禁与安装包 |

# docs/20：服务商「添加模型」真正接上线路 + 服务商图形化编辑（第 72 轮）

> 来源：用户第 72 轮的三条反馈（原话）：
> 1. 「添加服务商、添加模型那一步没有完全按照我说的，点击『添加』之后，并没有在上面看到可以编辑的东西，也没有地方编辑线路」；
> 2. 「我后面看这个模型的线路是『未配置』，按理来说，默认是直连」；
> 3. 「服务商点击进去之后我也要可以有图像化界面编辑，功能和刚刚一模一样才行，虽然界面和流程可以不一样」。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传（只有 4 个例外）。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；页面只调用已注册命令；每步先证明测试会红；共享工作区不留不可编译状态。

---

## 0. 事实基础（第 72 轮取证，别再猜）

| 事实 | 证据 |
| --- | --- |
| `model_bulk_update` 的 patch **不认 `lane`**：Rust `ModelPatch` 只有 `expose` / `aliases` / `display` | `src-tauri/src/commands/entity.rs:921` |
| Rust `Model` **没有 `default_lane` 字段** | `crates/ta-core/src/domain/mod.rs:971-983` |
| 但契约**早就声明了** `Model.default_lane?` 与 `ModelPatch.lane?`（还写着"null = 直连"） | `src/shared/bindings.ts:736-739`（`Model` 侧同文件） |
| 于是向导里改「网络代理」= **静默空操作**：命令成功返回、配置一个字节都没变 | `providerWizard.tsx:343` 调 `model_bulk_update({ids, patch:{lane}})`；Rust 侧该字段不存在 |
| `model_apply_import` **只往 `doc.models` push `Model`**，不建 channel | `src-tauri/src/commands/upstream.rs:449-457` |
| 第 2 步「已添加的模型」是**从 `channel_matrix` 派生**的（按凭据过滤） | `providerWizardModel.ts:266-295`（`addedModelsOf`） |
| 所以：导入的模型没有 channel → 矩阵里没有它 → **上面永远空**、也无行可编辑 | 上面两条的必然结果 |
| 模型没有 channel 也没有 lane → 界面只能显示「未配置」 | 用户第 2 条反馈 |

**结论**：第 71 轮把「第 2 步」写成了"往模型目录里塞行"，而 `docs/19 §1.2/§1.5` 的原意是
**"添加模型 = 把这个模型接到这个服务商上（建立通道）"，lane 写进新建通道**。本轮把实现补齐。

---

## 1. 语义裁决（先定死，再实现）

### 1.1 「添加模型」= 建模型 + 建线路

一次 `model_apply_import` 里，对每一条勾选的模型：

1. `doc.models` 里没有就建 `Model { id: suggested_id, expose: true, ... }`（已有则跳过）；
2. 若 `(provider, credential, model)` 还没有通道，就建**一条**通道：
   - `ChannelConfig::new(ch_id, model, provider, credential, upstream_model)`；
   - `lane` = 入参 `lane`（**默认直连**，见 §1.2）；`upstream_model` = 扫描行的 `upstream_model`；
   - `ch_id` = `ch-<provider>-<credential>-<model>`（与前端 `channelIdFor` **同一派生法**：
     小写、非 `[a-z0-9]` 一律变 `-`、去首尾与重复横线 —— 不另发明一套）。
3. credential 的选择顺序：`<provider>-default`（第 1 步建的那一份）→ 否则该 provider 的**第一条 enabled** 凭据；
   **一条都没有 → 报 `TA-1004` 明确说"这个服务商还没有凭据，先回第 1 步填密钥"**（不静默建一条没有凭据的线路）。

`ApplyResult` 增加 `channels_created: number`（诚实汇报本次真接了几条线；0 也要如实给）。

### 1.2 「默认直连」的定义（第 2 条反馈）

`pw-model-lane` 的**默认值**按这个顺序取，**永远不为空**：

1. 该模型显式配的 `Model.default_lane`（用户改过）；
2. 否则 `config_get().net.default_lane`（用户在 `/network` 选的"新建服务商默认代理"）；
3. 否则**直连**：取 `lane_options` 里"直连"那一组的 lane id；取不到就回落到 `null`（继承，链路末端仍是直连）。

任何界面**不许**把"没显式配"显示成「未配置」：五级继承链的末端（[docs/04 §8.1]）
`policy.use_lane ?? channel.lane ?? credential.lane ?? provider.lane ?? net.default_lane ?? direct`
决定了没配就是直连，那就显示**直连**。

### 1.3 `ModelPatch.lane` 三态

| 入参 | 语义 |
| --- | --- |
| 字段**缺失** | 不动 lane（与其余字段一致） |
| `lane: null` | **清空**该模型与其通道的 lane（回到继承链，末端直连） |
| `lane: "<lane-id>"` | 写该 lane（必须存在于 `doc.lanes`，校验器兜底），**该模型下的每条通道都写同一个值** |

同时写 `Model.default_lane`（"下一次给这个模型建通道默认走哪条"）。

**为什么要写通道**：lane 是 channel 级字段，模型自己不承载流量；只改一个"默认值"而通道还是老样子，
就等于"界面上改了、线上没变"——那正是第 71 轮踩过的坑。

#### 1.3.1 界面选「直连」时到底写什么（裁决：**写直连那条 lane 的 id，不是 `null`**）

`null`（继承）与"写死一条直连 lane"在**当前**配置下解析结果相同，但两者不是一回事，差别会在用户
以后改 `net.default_lane` 时显现：继承的通道会**悄悄跟着走**，写死的不会。

用户点的是「直连」这个**具体选择**，所以落库就要写他选的东西：

- 「添加模型」时默认 lane = §1.2 的三步取值，**总是一个具体 lane id**（直连那条的真实 id），
  于是新建通道的 `lane` 字段**非空** —— 通道页显示的是直连线路的名字，而不是一个"没配"的破折号；
- 行内改选「直连」= `model_bulk_update({ patch: { lane: "<直连 lane id>" } })`，同样写死；
- `lane: null`（清空）仍然保留在契约里，但**界面不会主动用它** —— 它是给"确实想回到继承"的调用方的。

代价（如实记下）：这样写死之后，用户在 `/network` 改"全局默认线路"**不会**影响已经显式选过直连的模型。
这是有意的取舍：模型级的显式选择 > 全局默认，否则用户会看到"我没动它，它自己换了线路"。

---

## 2. 后端接口（写范围：`crates/ta-core/**`、`src-tauri/**`）

### 2.1 `ta_core::domain::Model`

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub default_lane: Option<LaneId>,
```

`Model::new` 里为 `None`；`serde` 往返不丢；`specta` 类型随结构体导出。

### 2.2 `ModelPatch`（`src-tauri/src/commands/entity.rs`）

```rust
/// `lane` 的三态见 [docs/20 §1.3]：缺省 = 不动；`null` = 清空；值 = 写该 lane。
#[serde(default)]
pub lane: Option<Option<LaneId>>,
```

`bulk_update_models` 的语义：对每个命中模型，写 `default_lane`；**并把该模型下所有通道的 `lane` 一并写成同值**；
写库仍走 `save_validated`（校验不过一个字节都不写）。

### 2.3 `model_apply_import`

```ts
model_apply_import: {
  args: { preview_token: string; selected: string[]; lane?: LaneId | null };
  result: ApplyResult;   // { applied, restart_required, kernel_reload, channels_created }
};
```

行为见 §1.1。**幂等**：同一次预览应用两次，第二次不重复建模型/通道（`added == 0 && channels_created == 0`
时**不写库**，`applied` 仍为真＝"命令成功，没有要改的"）。

### 2.4 必测（先证明会红）

- `model_bulk_update` 写 lane：无通道 → 只写 `default_lane`；有通道 → 通道的 `lane` 跟着变；
  `lane: null` → 清空；`lane` 指向不存在的线路 → 报错且**库没变**。
- `model_apply_import` 建线路：`channels_created` 正确；通道的 `credential` / `upstream_model` / `lane` 正确；
  重复应用不重复建；无凭据 → `TA-1004`。
- `Model.default_lane` 的 serde 往返（契约与 Rust 一致）。

---

## 3. 前端：第 2 步（写范围：`src/pages/providers/**`、`src/app/providers*.test.tsx`）

- **添加之后必须立刻看得到**：`model_apply_import` 成功后 `invalidateQueries` 要覆盖
  `qk.channelMatrix` / `qk.models` / `qk.credentials`，并等这一次取数回来再算"已添加"（不许只 fire-and-forget）。
- **每行都能编辑线路**：`pw-model-lane` 的值 = §1.2 的三步默认；选择后调
  `model_bulk_update({ ids: [modelId], patch: { lane } })`，成功后重取矩阵与模型清单（值要真的变回来）。
- 行内展示：`pw-model-row` 里给出**当前生效的线路名**（"直连" 或线路名），不是空白。
- 新增单测（必须先在旧代码上红）：①添加之后 `pw-model-row` 出现且含刚加的模型；
  ②行的 `pw-model-lane` 显示「直连」；③改成某条线路后**落库载荷**是 `{ids:[m], patch:{lane:"<id>"}}` 且重取后显示该线路名。

---

## 4. 前端：服务商图形化编辑（第 3 条反馈）

用户在详情页要有**与新增等价**的编辑能力（界面/流程可以不同，功能必须一样）。

- 入口：`/providers/:id` 详情页新增按钮 `providers-detail-edit` → 打开**同一个两步向导**（编辑模式）。
- 编辑模式（`ProviderTwoStepWizard` 增加 `mode: "create" | "edit"` 与 `providerId`）：
  - 第 1 步预填**名称 / 协议 / base_url**；**密钥留空 = 不改**（只有填了新值才 `credential_set_secret`）；
    名称去重**排除自己**（第 71 轮 §1.3.1 的同一套 `SavedInSession` 机制）。
  - 第 2 步与新增完全一致：`pw-added-models` 列出**该服务商已有的模型**（矩阵派生）、`pw-scan` 真调、
    勾选/全选/添加、`pw-model-lane` 改线路。
  - `pw-next` 在编辑模式下用**现有 provider id** 覆盖写（不许改成"派生一个新 id"。
- 契约/文案：新增 i18n 键 `providers.edit`（编辑）、`providers.wizard.editTitle`（编辑服务商）、
  `providers.wizard.keyUnchanged`（留空表示不改密钥）。**i18n 归主管**，写者缺键报主管。
- 新增单测（必须先在旧代码上红）：①详情页有 `providers-detail-edit` 且点开是向导第 1 步；
  ②预填值与现有服务商一致；③留空密钥点下一步**不发** `credential_set_secret`、但发 `provider_upsert`（同 id）；
  ④编辑模式第 2 步能看到该服务商已有模型并能改线路。

---

## 5. 验收（主管在干净快照上跑）

1. 门禁全 0：`npm run build` / `cargo fmt --check` / `cargo check --all-targets` / `cargo clippy -D warnings` /
   `cargo test --workspace` / `npm test`。
2. 真机 E2E：
   - `step2` **扩展**：添加之后立刻断言 `pw-added-models` 里出现刚加的模型行、行内 `pw-model-lane` 显示「直连」；
     把某行改成订阅线路 → `config` 里该通道的 `lane` 真的变了 → 重进页面仍是它。
   - 新增 `step20`：列表 → 详情 → `providers-detail-edit` → 编辑模式改名称/base_url → 落库；
     第 2 步能看到该服务商已有模型。既有步骤必须继续全绿（不许放宽旧断言）。
3. 重出安装包（NSIS）并在同一发布二进制上复跑 A3/A4 不变量。

---

## 6. 分工与写范围（一轮一包）

| 工作包 | 负责人 | 写范围 |
| --- | --- | --- |
| **WP-72A**：`Model.default_lane` + `ModelPatch.lane` + `model_apply_import` 建线路 + Rust 测试 | E3 | `crates/ta-core/src/domain/**`、`src-tauri/src/commands/{entity,upstream}.rs` |
| **WP-72B**：第 2 步列表/默认直连/行内改线路 + 编辑模式与详情页入口 + 前端测试 | E1 | `src/pages/providers/**`、`src/app/providers*.test.tsx` |
| 主管 | lead | `src/shared/bindings.ts`、`docs/**`、`src/shared/i18n/*.ts`、`scripts/e2e/smoke.mjs`、门禁与安装包 |

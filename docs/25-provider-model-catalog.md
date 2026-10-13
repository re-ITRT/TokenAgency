# docs/25：扫描到的模型**全量记录**，用的时候在服务商里勾一下（第 77 轮）

> 来源：用户第 77 轮原话：
> 「添加服务商的形式我需要改一下，我们当前是第二步(填写完 url 接口和 apikey 之后)扫描模型加入，
> 现在这样：模型分为扫描的(自动的)\手动添加的，一旦扫描，全部记录下来(当然不是添加)，
> 然后比如用户那一天想起，我还想用 xxx 模型，直接在服务商里面点进去，把那个没勾的勾上就行了」。
>
> 随后用户改口（同轮，明确）：**「不行，我想应该不能手动添加，这样会坏自动化逻辑」**
> 以及 **「我说的手动添加模型不需要了」**。
>
> 开工前已选定的口径（`ask_user_question` 的答复 + 上面两次改口）：
> * 扫描目录**存 SQLite**（不进配置真源）；
> * **取消勾选 = 停用通道（可逆）**，移除另有单独入口走删除确认；
> * **不做手动添加** —— 目录里的每一条都必须来自真实扫描；
> * 重新扫描**以本次为准**（本次没返回的扫描项从目录里清掉）。

**不变量照旧**：I1 只监听回环；I2 零系统网络副作用；I3 全透传。
**纪律照旧**：无假按钮 / 无禁用占位 / 无假数据；页面只调用已注册命令；每步先证明测试会红；
共享工作区不留不可编译状态。

---

## 0. 事实基础（第 77 轮取证）

| 事实 | 证据 |
| --- | --- |
| 扫描结果**只活在进程内存**：`ImportPreviewCache` 在 `AppState::bootstrap` 建空，TTL 5 分钟、上限 32 条，重启即失 | `src-tauri/src/import_preview.rs:29,35,84-108`；`src-tauri/src/state.rs:81,132` |
| 这是**有意**的（注释原话：落库会造出"第二个真相源"） | `import_preview.rs:1-21`（[docs/10 §2.3] 同样写着"进程内存、不落库"） |
| 所以"扫描过但没勾"的模型**关掉向导就没了** —— 用户回不到那个列表 | 同一处；UI 侧 `preview` 是 `useState`（`providerWizard.tsx:179`、`detail.tsx:1546`） |
| 预览里 `exists` 判的是**全局**有没有这个模型（`doc.find_model`），不是"这家接了没有" | `upstream.rs:315-317`；`behavior.rs:1076-1078` |
| 造出的后果：一个**已存在**的全局模型在向导里**永远勾不上**（复选框 `disabled={model.exists}`） | `providerWizard.tsx:801` |
| 接入 = 建 `Model`（若缺）+ 建通道，通道 id = `ch-<slug(provider)>-<slug(credential)>-<slug(model)>` | `upstream.rs:427-435,470-600` |
| 取消/移除已有可逆与不可逆两条现成路径：`channel_set_enabled`（永不失败）与 `channel_delete`（被策略点名引用时 `TA-5002` 拒绝） | `catalog.rs:271-288`；`entity.rs:562-606` |
| 观察类数据在 SQLite 有先例：`health_sample` / `secret_meta` / `subscription_version` / `quota_state`（滚动分桶） | `0001_init.sql`、`0005_quota_windows.sql`、`health_store.rs` |
| 配置写一次 = 整份文档 DELETE+INSERT + **JSON/YAML 两份快照** | `config_store.rs:115-172,237-260`；`catalog.rs:197-206` |

**结论**：只有一条路能让"扫过就记得住"成立 —— 把目录**落到 SQLite**（观察数据），
而**接入与否**继续留在配置里（用户意图）。二者不混。

---

## 1. 术语

* **模型目录（catalog）**：这家服务商"扫描到过哪些模型"的记录，每行 = `(provider, upstream_model)`，
  带 `suggested_id`（扫描派生出的逻辑名）、首见/末见时间、"上次扫描见过吗"。
  **存在 SQLite（`provider_model_catalog`）**，不进配置、不进 YAML 导出。
* **接入（added）**：配置里真有 `Model` + 一条指到这家服务商的 `ChannelConfig`（启用与否是通道自己的字段）。
* **勾上** = 接入。**取消勾选** = 把该模型在这家下的通道 `enabled=false`（可逆）。
  **移除** = 单独入口，走 `channel_delete` 的确认流程（被策略引用时原样显示后端原文）。

**没有"手动添加"**（用户明确要求两次）：目录里的每一行都必须来自一次真实扫描。
理由写在他自己的话里 ——「不能手动添加，这样会坏自动化逻辑」：后面所有自动化
（按模型名建通道、路由、定价）都建立在"目录 = 上游真实提供了什么"这条不变量上，
手填一行就把这条不变量变成了用户的一厢情愿。

## 2. 三件事各自怎么落地

### 2.1 扫描 = 全量记录（不接入）

`model_import_from_provider` 在**返回预览之前**把解析出来的每一条 upsert 进目录：
`seen_in_last_scan=1`、`last_seen_ms=now`（`first_seen_ms` 只在第一次写入时落）。

**重新扫描以本次为准**（用户选定）：先把这家服务商的所有目录行标 `seen_in_last_scan=0`，
再把本次返回的标回 1，最后删掉 `seen_in_last_scan=0 AND **没有任何通道**` 的行。

* 为什么**接了通道的行不删**：用户明确接过它 —— 目录里丢掉它，界面上就会出现
  "有一条通道、但模型区看不到这个模型"的鬼状态。它会被标成"本次未返回"，让用户自己决定。
* 派生不出合法逻辑名的上游名（`skipped`）**不进目录**：它连"接入"都做不到，记下来只会变成
  一个点不动的行（那正是"禁用占位"）。

### 2.2 服务商里勾一下 = 接入

新命令 `provider_model_attach({provider_id, upstream_model})`（**只给上游名**，
逻辑名从目录行里取 —— 单一真源，界面没有机会传错）：

1. provider 必须存在；目录里必须有这一条（不在 → `TA-1004`：「先扫描一次」）；
2. 已经接入过（这家下已有 `upstream_model` 相同的通道）→ **幂等返回**，一个字节都不写；
3. 否则：`Model::new(suggested_id)`（缺才建）+ `ChannelConfig::new(ch_id, model, provider, credential, upstream_model)`，
   `enabled = true`；凭据按现有规则取（`<provider>-default` → 该 provider 第一条启用凭据 → 否则 `TA-1004` 明说"先去配密钥"）；
4. **能力一致性沿用 `channel_upsert` 的那套检查**（同一 `model` 会被已有策略归到一起而能力声明不一致 → `TA-2007` 带逐字段差异）。
   这是"手动接入"路径，与通道页手动建通道必须同一条规则；`model_apply_import`（向导批量）那条宽松行为是既有事实，
   不在本轮改（登记在 §5）。

### 2.3 取消勾选 / 移除

* 取消勾选 → `channel_set_enabled({id, enabled:false})`（**现有命令**，永不失败、可逆）；
  勾回来 → `enabled=true`。
* 「移除」→ 走现有 `channel_delete` + `ConfirmDelete`（[docs/21 §1] 的那套：先确认、被引用时原样显示后端原文、没有级联按钮）。

## 3. 存储与契约（接口冻结，写者按此实现）

```sql
-- migrations/0006_provider_model_catalog.sql（只增不改）
CREATE TABLE provider_model_catalog (
  provider_id    TEXT    NOT NULL,
  upstream_model TEXT    NOT NULL,
  suggested_id   TEXT    NOT NULL,          -- 扫描派生出来的逻辑名（唯一真源）
  seen_in_last_scan INTEGER NOT NULL DEFAULT 0,
  first_seen_ms  INTEGER NOT NULL,
  last_seen_ms   INTEGER NOT NULL,
  PRIMARY KEY (provider_id, upstream_model)
);
```

```ts
export type ModelCatalogEntry = {
  provider_id: ProviderId; upstream_model: string; suggested_id: string;
  seen_in_last_scan: boolean; first_seen_ms: number; last_seen_ms: number;
  added: boolean; channel_ids: string[]; enabled: boolean;
};

provider_model_catalog: { args: { provider_id: ProviderId }; result: ModelCatalogEntry[] };
provider_model_attach:  { args: { provider_id: ProviderId; upstream_model: string }; result: ApplyResult };
```

`added` / `channel_id` / `channel_count` / `enabled` 由命令层**按配置现算**（不是目录里存的）：
目录行 + 该 provider 的通道一次性对齐。多凭据下"接入"按**这一家**算（任一凭据有那条通道即为已接入），
勾的时候用首选凭据建 —— 与 `model_apply_import` 的凭据选择规则同一处实现。

`delete_provider` 时连同目录行一起删（`delete_provider` 已经级联清额度状态，这里是同一位置）。

## 4. 界面

* **服务商详情页的「模型」区**（`providers-models`）改成**目录视图**，一行一个模型：
  * 勾选框（`providers-model-check`）= 接入与否。未接入的**永远可勾**（修掉"全局已存在就 disabled"这个洞）；
  * 「本次未返回」（`providers-model-stale`）—— 重新扫描后没再出现的扫描项；
  * 线路列**只读**（仍是 [docs/21 §4.3]：线路只在通道页改）；
  * 每行「移除」（`providers-model-remove`）：接入过才出现，走删除确认；
  * 「重新扫描」（`providers-model-scan`）：扫描 + 记录 + 按 §2.1 清目录，并把"以本次为准"写在按钮旁边；
  * **没有**任何"手动添加"入口（用户明确要求）。
* **添加服务商向导第 2 步**：主流程不变（扫完就能勾着加），但**扫描现在会落目录**；
  步骤里加一句"扫描结果已全部记录，没勾的之后可以在服务商详情里随时勾上"。
  `exists` 那个"全局已存在"的语义**不再用来禁用复选框**：改成"已接入这一家"才算勾上。

## 5. 分工、验收与已知取舍

| 工作包 | 负责人 | 写范围 |
| --- | --- | --- |
| **WP-77A**：`provider_model_catalog` 存储（迁移 0006 + store + GC）+ 测试 | teammate | `crates/ta-store/**` |
| **WP-77B**：目录区组件 + 纯逻辑 + 单测（**不含**手动添加） | teammate | `src/pages/providers/{modelCatalog.ts,modelCatalogSection.tsx}`、对应测试 |
| **WP-77C**：扫描落目录 + 两条新命令 + 契约 + 详情页接线 + i18n + 文档 + 门禁 + 安装包 | lead | `src-tauri/src/commands/**`、`src/shared/bindings.ts`、`src/pages/providers/detail.tsx`、`src/app/providers*.test.tsx`、`docs/**` |

**验收**
1. Rust：扫描把**全部**能派生出合法逻辑名的返回项写进目录（`skipped` 不写，并有用例钉住）；
   重扫以本次为准清掉未返回项、**保留仍接着通道的行**；`provider_model_attach` 幂等、
   目录里没有该行报 `TA-1004`、无凭据报 `TA-1004`、能力冲突报 `TA-2007`；`delete_provider` 清目录行。
2. 前端：目录区勾选框与"本次未返回"标记；勾 → `provider_model_attach` 的入参；取消 → `channel_set_enabled(false)`；
   移除 → 确认框；重扫后标记出现；**没有手动添加入口**。
3. 门禁：`npm test` / `npm run build` / `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test --workspace` 全 0；
   `scripts/build.ps1` 出安装包。
4. **真机 E2E**：`step24`（扫描后离开页面 → 重进服务商详情 → 目录里那个没勾的还在 → 勾上 → 库里真多一条通道）。

**已知取舍（如实登记）**
* **上游 `/models` 不返回的模型无法接入**（没有手动添加）—— 本轮**有意**的取舍：
  目录必须等于"上游真实提供了什么"，否则自动化（建通道/路由/定价）会建立在不存在的模型上。
  哪天真要手填，得先给出"它凭什么可信"的答案（例如由一次真实探测确认），而不是一个输入框；
* `skipped` 行不进目录（派生不出合法逻辑名，接不了）；
* `model_apply_import`（向导批量路径）**仍然宽松**（不做能力一致性检查），本轮的
  `provider_model_attach` 严格；两条路径的差异写在这里，不装作一样；
* 目录**不进 YAML 导出/回滚**（它是观察数据，与 `api_key` / 健康样本同类）；
* 扫描用的凭据仍是"该 provider 第一条启用凭据"，目录**不记**是哪一条扫出来的（多凭据权限不同时这是已知局限）。

---

## 6. 落地结果（本轮实际做到哪一步，逐条对账）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| 迁移 `0006_provider_model_catalog`（**没有 `origin` 列**）+ `user_version = 6` | 完成 | `crates/ta-store/migrations/0006_*.sql`；`migrations.rs` 断言主键列序、非空、**并显式断言不存在 `origin` 列**（谁加回来都会红） |
| 存储层 `ModelCatalogStore`（list / list_all / get / upsert 保 `first_seen_ms` / mark_all_unseen / prune_unseen / remove_provider） | 完成 | `crates/ta-store/src/model_catalog.rs` + 5 条测试（含"真的关库再打开"） |
| 扫描 = 全量记录（**落库在返回预览之前**，落不下去就如实报错） | 完成 | `upstream.rs::persist_model_catalog`；测试 `scan_records_every_model_but_wires_nothing`（2 条落库、1 条 skipped 不落、**配置里 0 模型 0 通道**） |
| 重扫以本次为准 + 保留"已接入"的行并标「本次未返回」 | 完成 | 测试 `rescan_prunes_unseen_but_keeps_what_is_wired` |
| `provider_model_catalog` / `provider_model_attach` 两条命令 | 完成 | 命令面守卫 `ARCHIVE_TOTAL 114 → 116`；契约与归档件同步 |
| 勾上 = 建 Model + 一条**启用**通道；幂等；无凭据报 `TA-1004`；目录里没有报 `TA-1004`；能力冲突走 `channel_upsert` 同一规则 | 完成 | 4 条测试（`attach_creates_model_and_enabled_channel_then_is_idempotent` 等） |
| 取消勾选 = `channel_set_enabled(false)`（**停用**，可逆）；「移除」= `channel_delete` 走确认框（**没有**级联按钮：它不吃 `force`） | 完成 | `detail.tsx` 的 `CatalogSection` + 第三条 `ConfirmDelete` 流程；`ConfirmDelete.onConfirmCascade` 改成可选并在注释里写明为什么 |
| `delete_provider` 连目录行一起清（被拒绝时一个字节都不写） | 完成 | `provider_delete_counts_refs_and_cascades_with_force` 断言两侧 |
| 服务商详情页模型区 = 目录视图（勾选框 / 本次未返回 / 只读线路 / 移除 / 重新扫描 + "以本次为准"写在按钮边） | 完成 | `detail.tsx`；4 条整页测试（勾 → `provider_model_attach` 入参；取消 → `channel_set_enabled(false)`；重扫挂 notice；空态不画假行） |
| 向导第 2 步：扫描落目录 + 明说"已全部记录" + 「去服务商详情」；复选框判据从**全局 `exists`** 改成**这家接过没有**（修掉"别家接过就永远勾不上"的洞） | 完成 | `providerWizard.tsx` / `providerWizardModel.selectAllIds(preview, wired)`；向导测试新增一条专门钉这个洞 |
| **没有**手动添加（用户两次明确要求） | 完成 | 契约里没有该命令；i18n 里那批键已删；`migrations.rs` 断言表里没有 `origin` 列 |
| 真机 E2E `step24`（扫描只记录 → 勾上接入 → 取消只是停用 → 离开再回来还在，并直接查 SQLite 的 `provider_model_catalog`） | **已写，未跑** | `scripts/e2e/smoke.mjs`；`dumpScratchDb` 新增该表快照 |

### 门禁实测（本轮工作区最终状态）

```
npx tsc --noEmit                                   → exit 0
npm test                                           → 29 files / 673 tests 全过
npm run build                                      → 成功
cargo fmt --all --check                            → exit 0
cargo clippy --workspace --all-targets -D warnings → exit 0
cargo test --workspace                             → 全绿（ta-store 40 + 5 条目录存储测试；tokenagency 命令层含 6 条目录测试）
scripts/build.ps1                                  → exit 0，产出 NSIS 安装包
target/release/bundle/nsis/TokenAgency_0.1.0_x64-setup.exe
  （5.42 MB，SHA256 B2FB903C26665429051AF5EA5F5E9EED75E079D8FF46A1F74821DC5D59610E44）
```

> 一次全量 `npm test` 曾出现 127 条 `Test timed out in 5000ms`：那是**并发跑测试抢 CPU**
> （同机还有别的 vitest/cargo 在跑，collect 542s），把超时放宽到 30s 后 673/673 全绿，
> 安静下来再跑 `npm test`（5s 超时）同样 673/673 —— 不是代码问题，但**如实记一笔**。

### 没做到的（如实登记）

* **真机 E2E `step24` 没有跑**（脚本已按上面的判据写好）：要打包 + WebView2 驱动 + 内核夹具；
  本轮只做到单测 + 门禁 + 打包。
* **向导第 2 步仍然是"批量 apply"路径**（`model_apply_import`，用 5 分钟内存预览）：它现在也**顺带**落目录，
  但界面没用新的 `provider_model_attach`。两条路径的差别（能力校验严格度）已在 §5 登记；
  要不要把向导也改成"逐条 attach"，留给下一轮（改它要动向导的即时勾选体验）。
* **多凭据下"接入"按 provider 算**：任一凭据有那条通道就算已接入，勾的时候用首选凭据建。
  同一模型想挂到第二份凭据下，目前只能去通道页手动建（`provider_model_attach` 不会重复建）。
* **目录不记是哪条凭据扫出来的**（多凭据权限不同时，这是已知局限）。


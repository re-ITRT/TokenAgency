# `docs/` 索引与引用规则

**这一页是"文档路径"的唯一裁决处。** 加文档、改文件名、或在代码里写 `[docs/NN]` 之前先读它。

---

## 1. 编号规则（先读这条）

> **编号只增不改，空缺是永久的。**

* **不复用**空号，**不重编号**，**不"合拢"**空缺。
* 为什么这条最硬：全仓有 **2500+ 处** `docs/NN` 引用（`docs/28` 一份就背着 291 处）。
  重编号会让**全部**失效 —— 而且失效是**静默的**：注释里的路径没有任何编译器或测试在检查。
* 编号空缺的**唯一**合法处理方式是在下面第 3 节的索引里**留一行**并标注
  `—（无此文档；空缺是刻意的）`。这样谁 grep 到一个悬空编号，会落到一句解释上，而不是什么都没有。

### 已知空缺

| 编号 | 状态 |
|---|---|
| `24` | **从未入库**。`git log --all --name-status -- 'docs/24*'` 返回空。主题可从引用点反推：**多窗口额度 / credential quota windows**（5 处代码引用它：`crates/ta-store/migrations/0005_quota_windows.sql`、`src/pages/providers/detail.tsx`、`src/pages/providers/model.ts`、`src/shared/i18n/{zh-CN,en}.ts`）。该功能已在第 314 轮删除（`crates/ta-quota` 整 crate 下线、`QuotaWindow` 改名 `PlanFeeCycle`），所以**不补写**。 |
| `36`–`45` | **无任何引用，也无任何提及**。全仓搜 `docs/3[6-9]`、`docs/4[0-5]`、`[36 §`…`[45 §` 均 0 命中；`docs/*` 里也没有编号约定的历史记录。**无法从仓库确定它们本来是什么** —— 当作纯粹跳过的号段，不要试图补。 |
| `50` | 本文件同轮新增（见下表）。曾在写作过程中被 8 处提前引用（`plugins.rs` 等），现已有实体文件。 |
| `51` | 第 318 轮新增（见下表）。写作前已被 `cost.tsx` / `models/index.tsx` / `PricingDialog.tsx` / `shared/pricing.ts` 与两份测试**提前引用**，现已有实体文件。 |
| `52` | 第 319 轮新增（见下表）。写作前已被 `shared/row.ts`、`shared/helpTip.ts`、`shared/components/StatItem.tsx`、`shared/i18n/index.ts` 与两份新测试**提前引用**，现已有实体文件。 |
| `53` | 第 320 轮新增（见下表）。写作前已被 `entity.rs` / `net.rs` / `queries.ts` / `confirmDelete.tsx` 与三份测试**提前引用**，现已有实体文件。 |
| `54` | 第 321 轮新增（见下表）。写作前已被 `domain/mod.rs`（`LaneKind` 遗留说明）与 `config/migrate.rs`（`v2_to_v3`）**提前引用**，现已有实体文件。 |
| `55` | 第 322 轮新增（见下表）。写作前已被 `domain/mod.rs`（三个字段的删除说明）与 `net.rs`（命令删除说明）**提前引用**，现已有实体文件。 |
| `56` | 第 323 轮新增（见下表）。写作前已被 `routes.ts`（`navGroup` / `NAV_GROUP_ORDER` 的注释）与 `Sidebar.tsx` **提前引用**，现已有实体文件。 |
| `57` | 第 324 轮新增（见下表）。写作前已被 `net.rs`（`upsert_subscription` 的"留空 = 不改"）与 `network/index.tsx` **提前引用**，现已有实体文件。 |

---

## 2. 引用格式（只有一种）

### 代码注释里（Rust / TypeScript）

```text
[docs/28 §3]        ← 推荐：方括号 + 斜杠 + 章节
[docs/28]           ← 不带章节也可以
```

**不要写**：

| 写法 | 为什么禁 |
|---|---|
| `docs 28`（空格） | 它是 `docs/28` 检索**看不见**的写法。这正是 `docs/24` 那个悬空引用能长期躲过检查的原因。曾经有 10 处（全在 `src/shared/i18n/en.ts` 的注释里），已统一。 |
| `[28]`（只有数字） | `[10]`、`[20]`、`[26]` 在代码里大量是**数组下标**（`b[10]`、`ihdr[10..12]`、正则 `[12]`），任何基于 `[NN]` 的检查都会误报。 |
| `](docs/NN-slug.md)`（在 `docs/` 内的文件里） | 相对路径是相对**当前文件**的，会解析成 `docs/docs/…`。`docs/` 内部一律写 `](NN-slug.md)`。 |

### Markdown 链接

| 在哪 | 怎么写 |
|---|---|
| `docs/*.md` 互相引用 | `[文字](28-lines-belong-to-providers.md)`（相对 `docs/`） |
| 根目录的 `README.md` / `CHANGELOG.md` | `[文字](docs/28-lines-belong-to-providers.md)`（相对仓库根） |
| `docs/evidence/**` 里的报告 | `[文字](../../28-lines-belong-to-providers.md)` |

> 已知例外：`src/shared/i18n/en.ts` 里有 **3 处用户可见英文文案**写了 `docs 04 §7.1` 之类
> （L975、L1193、L1195）。那些是**给用户看的正文**，不是路径引用，所以没有统一 ——
> 统一它们会改动用户可见文案。**如果以后要动，先想清楚那句话给谁看。**

---

## 3. 章节锚点的两套方言（写 `§N` 之前必须知道）

`§N` 的含义**按文档不同**，这不是笔误：

| 方言 | 用在 | 含义 |
|---|---|---|
| `## N. 标题` | 大多数文档（01–23、25–35、47–50） | `§N` = 该编号的标题 |
| `第 NNN 轮` | `docs/31-progress.md`、`docs/46-bundled-kernel.md` | `§NNN` = **第 NNN 轮**（是轮次号，不是章节号） |

任何"检查 `[docs/NN §M]` 是否指向真实标题"的守卫**必须**理解这两套方言，否则会大面积假红。

**并且已知有两处引用对不上**（记录在案，未修，理由见 [docs/50 §4](50-plugin-and-capability-minimization.md)）：

* `docs/46`：`§288–§295`、`§299`、`§310` 共 18 处引用 —— 那些轮次在 `docs/46` 里**根本不存在**
  （文件从"第 287 轮"直接跳到"第 296 轮"）。补写 = 编造章节，改指向 = 逐处内容判断。
* `docs/16`：3 处 `[16 §8]` —— `§8` 是 §3 里的一个**编号列表项**，不是标题。

---

## 4. 全量索引

**48 份带编号的文档**（另有本索引 `README.md` 与无编号的 `llm-api-quota-research.md`）。**状态**含义：`现行` = 描述当前行为；`历史` = 记录某一轮的决策，不必反映现状；`被取代` = 有新文档改了它的口径（以新文档为准）；`证据` = 真机/调研产物，不是规格。

| # | 文件 | 主题 | 状态 |
|---|---|---|---|
| 01 | [01-architecture.md](01-architecture.md) | 总体架构 | 现行 |
| 02 | [02-config-and-data-model.md](02-config-and-data-model.md) | 配置与数据模型 | 现行 |
| 03 | [03-routing-and-policy.md](03-routing-and-policy.md) | 路由、策略与透传 | 现行（§3 已按 [48](48-balanced-sticky-only-routing.md) 收敛） |
| 04 | [04-network-core.md](04-network-core.md) | 网络模型 | 现行 |
| 05 | [05-ui-tray.md](05-ui-tray.md) | 托盘与界面 | 现行 |
| 06 | [06-roadmap-and-risks.md](06-roadmap-and-risks.md) | 里程碑、验收与风险 | 现行 |
| 07 | [07-ui-pages-and-cost.md](07-ui-pages-and-cost.md) | 页面细节、请求日志与成本中心 | 现行（页面清单见 [26](26-motion-and-first-level-ellipsis.md)） |
| 08 | [08-tech-stack.md](08-tech-stack.md) | 技术选型 | 现行 |
| 09 | [09-architecture-design.md](09-architecture-design.md) | 软件架构设计 | 现行 |
| 10 | [10-interface-spec.md](10-interface-spec.md) | 接口文档（一）：对外契约与前端契约 | 现行 |
| 11 | [11-internal-contracts.md](11-internal-contracts.md) | 接口文档（二）：内部契约 | 现行 |
| 12 | [12-work-breakdown.md](12-work-breakdown.md) | 工作分解（给子 agent 的派发契约） | 历史（分工已过时，技术基线仍看它） |
| 13 | [13-team-and-delivery.md](13-team-and-delivery.md) | 团队、权责与交付流程 | 现行 |
| 14 | [14-observability.md](14-observability.md) | 日志、错误追踪与诊断 | 现行 |
| 15 | [15-visual-design.md](15-visual-design.md) | 视觉设计系统 | 现行（界面观感的唯一裁决处，[§8](15-visual-design.md) 的守卫会机械检查） |
| 16 | [16-m1-completion-report.md](16-m1-completion-report.md) | M1 完成度报告（第 11 轮） | 历史（多处已被后续轮次取代） |
| 17 | [17-add-flows-and-capability-routing.md](17-add-flows-and-capability-routing.md) | 添加流程与"能力一致才可路由"规格（第 61 轮） | 历史（§3.3 的 `/channels` 向导已删，见 [49](49-channels-page-removed.md)） |
| 18 | [18-subscription-traffic-and-optional-network.md](18-subscription-traffic-and-optional-network.md) | 订阅流量可见 + 网络代理选装（第 62 轮） | 历史 |
| 19 | [19-provider-wizard-network-default-and-api-keys.md](19-provider-wizard-network-default-and-api-keys.md) | 服务商分步向导 / 网络默认直连 / API key 管理（第 71 轮） | 历史 |
| 20 | [20-provider-model-lane-and-edit.md](20-provider-model-lane-and-edit.md) | 服务商「添加模型」接上线路 + 图形化编辑（第 72 轮） | 历史 |
| 21 | [21-delete-and-lane-settings.md](21-delete-and-lane-settings.md) | 各种项的删除 + 线路设置（第 73 轮） | 历史（通道/策略两页已删，见文件内后记） |
| 22 | [22-help-tips-for-long-copy.md](22-help-tips-for-long-copy.md) | 长篇介绍收进「问号圆圈 + 悬停提示」（第 74 轮） | 历史 |
| 23 | [23-provider-detail-inline-edit.md](23-provider-detail-inline-edit.md) | 服务商详情 = 查看 + 编辑一体（第 75 轮） | 历史 |
| — | *24 空缺* | *曾为多窗口额度文档，从未入库* | —（空缺是刻意的） |
| 25 | [25-provider-model-catalog.md](25-provider-model-catalog.md) | 扫描到的模型全量记录（第 77 轮） | 历史 |
| 26 | [26-motion-and-first-level-ellipsis.md](26-motion-and-first-level-ellipsis.md) | 动画优化 + 一级界面省略（第 78 轮） | 历史（含**一级界面清单**，页面增删时要更新它） |
| 27 | [27-model-owns-lines-and-one-way-delete.md](27-model-owns-lines-and-one-way-delete.md) | 模型 = 用线的记录；删除只往一个方向传（第 79 轮） | 历史（已被 [28](28-lines-belong-to-providers.md) 翻转，读 28） |
| 28 | [28-lines-belong-to-providers.md](28-lines-belong-to-providers.md) | 线路归服务商、模型归"分配"（关系翻转，第 80 轮） | **现行**（线路归属的权威） |
| 29 | [29-auto-network.md](29-auto-network.md) | 「自动」网络与代理归属 | **现行**（代理只属于服务商） |
| 30 | [30-minimal-ui.md](30-minimal-ui.md) | 极简风格：逐屏清单与机械规则 | 现行（逐屏清单会随页面增删过期，见文件头警告） |
| 31 | [31-progress.md](31-progress.md) | 进度与预计完成时间（每轮更新） | **现行**（轮次日志；§NNN = 第 NNN 轮） |
| 32 | [32-logs-master-detail.md](32-logs-master-detail.md) | 请求日志页：主从双栏重构 | 现行 |
| 33 | [33-e-round-note.md](33-e-round-note.md) | E 期说明 | 历史 |
| 34 | [34-plan-and-retro.md](34-plan-and-retro.md) | 计划与效率复盘 | 历史（**当前无任何文档或代码引用它** —— 见 §5） |
| 35 | [35-auto-manual-verification.md](35-auto-manual-verification.md) | 「自动」出口的人工验证清单（约 5 分钟） | 现行（人工清单） |
| — | *36–45 空缺* | *无任何引用与提及，无法从仓库确定* | —（空缺是刻意的） |
| 46 | [46-bundled-kernel.md](46-bundled-kernel.md) | 内置内核（随包分发 mihomo） | **现行**（§NNN = 第 NNN 轮） |
| 47 | [47-quota-exhaustion-error-shapes.md](47-quota-exhaustion-error-shapes.md) | 额度耗尽时各家返回什么（调研） | 证据（28 家平台实测；它促成了 [48](48-balanced-sticky-only-routing.md)） |
| 48 | [48-balanced-sticky-only-routing.md](48-balanced-sticky-only-routing.md) | 路由收敛为「平均粘性」+ 模型/线路开关 | **现行**（路由的权威） |
| 49 | [49-channels-page-removed.md](49-channels-page-removed.md) | 删掉「套餐与线路」页、把「新增定价」搬进成本中心 | **现行**（删页与能力减法的记录） |
| 50 | [50-plugin-and-capability-minimization.md](50-plugin-and-capability-minimization.md) | 结构清晰化 + 威胁面收敛 | **现行**（插件/能力/重复代码/文档路径） |
| 51 | [51-model-keyed-pricing.md](51-model-keyed-pricing.md) | 按**对外模型**定价；成本中心只做显示 | **现行**（定价的权威；[49](49-channels-page-removed.md) §2 因此过期） |
| 52 | [52-dedup-and-hotspots.md](52-dedup-and-hotspots.md) | 有证据的重复收敛与热点修复（可拓展性/简洁性/易读性/性能） | **现行**（共享读件与"单件只有一份实现"守卫的口径） |
| 53 | [53-referenced-delete-must-be-actionable.md](53-referenced-delete-must-be-actionable.md) | 删被引用的订阅/线路：拒绝必须**能照着改**（TA-5002 + 引用者 + 去处），确认框不许承诺做不到的级联 | **现行**（[21](21-delete-and-lane-settings.md) 的具体化） |
| 54 | [54-nodes-not-groups.md](54-nodes-not-groups.md) | 放弃「订阅里分组」，只拿节点；订阅详情页逐节点开关；「自动选择」/「直连」恒在不可关；服务商侧默认直连 | **现行**（[29](29-auto-network.md) 的接线 + [48](48-balanced-sticky-only-routing.md) 的节点开关修真） |
| 55 | [55-provider-owns-the-lane.md](55-provider-owns-the-lane.md) | 放弃**线路级别**的分配：删 `/network/assignments` 整页、删 4 条命令、线路分配只剩 `provider.lane` | **现行**（[04](04-network-core.md) §8 与 [02](02-config-and-data-model.md) 的解析式以它为准；[21](21-delete-and-lane-settings.md) 的 `channel_bulk_assign_lane` 作废） |
| 56 | [56-sidebar-information-architecture.md](56-sidebar-information-architecture.md) | 侧边栏重新排序与分组：按**任务流**排（不按使用频率）、配置链以终点收尾、设置永远最后 | **现行**（导航的权威；[05](05-ui-tray.md) §2 因此过期） |
| 57 | [57-write-forms-on-demand.md](57-write-forms-on-demand.md) | `/network` 的订阅/线路写表单**不再常驻**：改成「添加」按钮 + 行内「编辑」打开的对话框；并修掉编辑会静默写坏的两条路（遮蔽地址、`disabled_nodes`） | **现行**（[21](21-delete-and-lane-settings.md) 的写入形态以此为准；[23](23-provider-upsert-payload.md) 的"以原始记录为底"纪律延伸到订阅/线路） |
| 58 | [58-lanes-come-from-providers.md](58-lanes-come-from-providers.md) | 线路由服务商决定：删掉手动建线路的界面；修好「加了订阅却在服务商页选不到节点」（下拉接的是 `lane_list` 而不是含节点的 `lane_options`） | **现行**（[29](29-auto-network.md) §2.2 的接线；[57](57-write-forms-on-demand.md) 的线路那半作废） |
| 59 | [59-protocol-hub-and-lane-ports.md](59-protocol-hub-and-lane-ports.md) | 边缘 N 种协议、内部 1 种 IR：翻译层原来**从未被调用**；lane→内核 listener 端口表**被冻结**所以"只有直连能用" | **现行**（协议适配的真机落地；[06](06-protocol-adaptation.md) 的设计 + [46](46-bundled-kernel.md) 端口 WARN 的真因） |
| 60 | [60-ui-minimalism.md](60-ui-minimalism.md) | 界面极简：界面在**解释自己**而不是显示状态；契约出处彻底删除、14 列表格 → 6 列、解释进 `?`；五条规则由 `uiMinimalism.guard.test.ts` 机械检查（含只许变小的棘轮） | **现行**（UI 文案与密度的权威；与 [22](22-…) 同一条纪律的三层） |
| — | [llm-api-quota-research.md](llm-api-quota-research.md) | 国外大模型平台「额度/余额/花费查询」调研 | 证据（无编号；只被 [47](47-quota-exhaustion-error-shapes.md) 引用） |
| — | [evidence/](evidence/) | 真机截图与报告 | 证据 |
| — | [contracts/](contracts/) | 手写契约参考（`command_surface_guard` 的 ARCHIVE） | 现行（会被守卫检查） |

---

## 5. 孤儿文档（被引用得很少或没有）

| 文档 | 入向引用 | 建议 |
|---|---|---|
| [34-plan-and-retro.md](34-plan-and-retro.md) | **0**（文档与代码都没有引用它） | 它是效率复盘，不是规格，所以"没人引用"本身合理。**保留**，在本索引里给它一行就是它的入口。 |
| [llm-api-quota-research.md](llm-api-quota-research.md) | 1（仅 [47](47-quota-exhaustion-error-shapes.md)） | 同上：调研产物。已被 [47](47-quota-exhaustion-error-shapes.md) 部分取代，但那是另一批平台的调研，**两份都保留**。 |
| [evidence/real-machine/2026-09-27-auto-export-e2e.md](evidence/real-machine/2026-09-27-auto-export-e2e.md) | 0（只在两处作为**路径字符串**被提到，不是链接） | 真机证据。**保留**。 |

---

## 6. 加新文档时怎么做

1. 取**下一个未用的编号**（当前是 `54`）。**不要**去填空缺。
2. 文件名 `NN-kebab-case.md`；标题第一行写 `# docs/NN：<主题>`。
3. 在本文件第 4 节的表里**加一行**（编号 / 文件名 / 主题 / 状态）。
4. 如果它改变了某份旧文档的口径：**旧文档不删、不改写**（它是历史记录），
   在旧文档里加一条指向新文档的注记 —— 仓里一贯的做法是
   `> **[docs/NN] 后记（日期）**：…`。
5. 自检（**手工，当前没有守卫**）：

```powershell
# 1) docs/ 内的相对 .md 链接是否都能解析
Get-ChildItem docs -File -Filter *.md | ForEach-Object {
  foreach ($m in [regex]::Matches((Get-Content $_.FullName -Raw), '\]\(([^)#]+\.md)')) {
    $t = $m.Groups[1].Value
    if ($t -notmatch '^https?://' -and -not (Test-Path (Join-Path 'docs' $t))) { "$($_.Name) -> $t" }
  }
}
# 2) 被引用但不存在的编号
Get-ChildItem docs,src,crates,src-tauri,scripts -Recurse -File -Include *.md,*.ts,*.tsx,*.rs,*.mjs |
  Select-String -Pattern 'docs/(\d{1,2})\b' -AllMatches |
  ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
# 再与 docs/*.md 的编号对照（第 3 节的空缺表是白名单）
```

> **为什么不给这两条做机械守卫？** 可以，但两处坑必须先解决，否则守卫会大面积假红：
> ① 必须剥掉行内 `` `code span` `` —— `docs/14-observability.md:480` 里有一个
> `[?&](key|token|api_key)=` 的**正则**，会被当成 markdown 链接；
> ② 必须理解第 3 节那两套 `§` 方言。**在解决这两条之前，别加这个守卫。**

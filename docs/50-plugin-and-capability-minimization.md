# docs/50：结构清晰化 + 威胁面收敛（第 317 轮）

用户第 317 轮的原话：

> 这一轮优化代码，使代码结构更清晰，文档路径更明确，**所有已知和潜在威胁全部清除**

第三句是**无界表述** —— 没有任何一轮能证明"清完了"。所以本文件的写法是：
把"威胁"拆成**可逐条核对**的清单，每条写清"查了什么 / 结论 / 证据 / 还剩什么没查"，
**而不是**宣称"已全部清除"。末尾有一节专门写**没能确定的东西**。

---

## 1. 本轮实际清掉的威胁

### 1.1 四个"只注册、零调用"的 Tauri 插件（最高价值的一条）

**事实**：`src-tauri/src/plugins.rs` 注册了 11 个插件，其中 **4 个从来没有被任何代码调用过**：

| 插件 | 唯一出现处 | 前端/后端调用 |
|---|---|---|
| `tauri-plugin-fs` | `plugins.rs:135`（注册） | **0** |
| `tauri-plugin-process` | `plugins.rs:137`（注册） | **0** |
| `tauri-plugin-clipboard-manager` | `plugins.rs:139`（注册） | **0** |
| `tauri-plugin-shell` | `plugins.rs:146`（注册） | **0** |

查法（可复现）：`rg 'tauri_plugin_fs|tauri_plugin_process|tauri_plugin_shell|tauri_plugin_clipboard_manager|@tauri-apps/plugin-(fs|process|shell|clipboard-manager)'` 全仓扫描后，
每个符号**只有注册那一行**；前端 6 个 `@tauri-apps/plugin-*` 包中**只有 `dialog` 被真的 import**
（`src/pages/cost.tsx:2107` 的动态 `import("@tauri-apps/plugin-dialog")`，用于导出 CSV 选路径）。

**为什么这是威胁而不是"无害冗余"**：前三个在 `capabilities/default.json` 里**白给了权限**：

```
fs:default            ← 文件系统
process:default       ← 进程控制
clipboard-manager:default
```

未使用的权限 = **白送的攻击面**。这类问题不会以任何方式报错（不像"缺权限"那样运行时炸），
所以它能一直留着。

**处置**：删除 4 个插件（`plugins.rs` 注册表 11 → 7）、删除对应 Cargo 依赖（根 `Cargo.toml` +
`src-tauri/Cargo.toml`）、删除 5 个 npm 依赖（`npm install` 报 `removed 6 packages`）、
能力集 11 条 → 8 条。

**顺带发现**：`src-tauri/src/lib.rs:74` 的注释写"**12 个**插件"，而 `plugins.rs` 的表是 11 ——
注释与实际不符。已改为"7 个"并由守卫强制。

### 1.2 新增**通用**的最小权限守卫（有牙）

原来只有两条针对**具体插件名**的断言（`capabilities_never_grant_the_autostart_plugin`、
`capabilities_do_not_grant_shell_by_default`）。它们只能守住"已经想到的那两个"。

新增 `capabilities_only_grant_registered_plugins`：**能力集里的每一个 `<plugin>:*` 权限，
都必须对应 `PLUGINS` 里已注册的插件**（`core:` 放行）。这才是"删了插件忘了删权限"的守卫。

### 1.3 `with_trace_store` 从未接线（**潜在威胁，已修 + 已加牙**）

**这是本轮最有价值的发现，属于典型的"潜在威胁"**：

```rust
// crates/ta-gateway/src/runtime.rs
/// 不接的话只写日志 —— 那样"拿一个 request_id 筛出完整轨迹"就只剩应用日志这一路，
/// 前端日志页与 `ta trace` 都查不到。**所以生产装配应当接上。**
pub fn with_trace_store(mut self, store: Arc<dyn ta_store::TraceStore>) -> Self { … }
```

而生产装配（`src-tauri/src/gateway.rs` 的 `runtime_parts`）只调了 `.with_api_keys(...)`：
**`with_trace_store` 从未被任何代码调用**，且**零测试**。

与它同源的 `with_api_keys` 旁边就有一条防漂移测试，注释写着"少写一句网关会静默退化，
那种失效在真机上只表现为'访问密钥建了也挡不住人'，**没有任何东西会红**"。
**完全相同的模式，在 trace 这一路上重演了** —— 而且没有任何东西会红。

**处置（两步，缺一不可）**：

1. **接上**：`runtime_parts` 增加 `.with_trace_store(state.trace.clone())`。
   安全性已核对：`TraceStoreSink::on_request_complete` 写失败只 `tracing::warn!`，
   注释明写"写 trace 失败**不能**影响已经答复给客户端的响应"（`trace.rs:132`），
   所以接线**不会**给请求路径引入新失败模式。
2. **加牙**（两层）：
   * `ta-gateway` 的**行为**测试 `with_trace_store_really_writes_records_to_the_store`
     —— 用一个记录型 `TraceStore` 替身，断言请求收尾真的写进去了；
   * 反面**对照组** `without_trace_store_nothing_reaches_the_trace_table`
     —— 不接线时一条都不写（**防止上面那条因为"默认 sink 也会写"而假绿**）；
   * 应用层防漂移断言 `the_gateway_assembly_actually_wires_the_trace_store`
     —— 生产装配必须包含 `.with_trace_store(`。

**验牙**：把 `.with_trace_store(state.trace.clone())` 那一行删掉，跑
`cargo test -p tokenagency --lib the_gateway_assembly_actually_wires`，**立刻变红**并打印
原因（实测通过）。恢复后转绿。

### 1.4 诚实说明：这条缺口**我没有在真机上证明它造成了数据丢失**

我一度以为拿到了铁证：真机 e2e 的 scratch 库里 `trace` 表 **0 行**。
但继续核对发现 **`usage_request` 也是 0 行** —— 那次 e2e 里网关根本没有跑过真实请求
（没有客户端流量）。所以"`trace=0`"**不能**归因于接线缺失。

**结论只能停在代码证据这一层**：接线确实缺失、行为测试确实缺失（这两点有代码为证），
**而"生产环境真的丢过轨迹"未被证实**。修它的理由不是"已经造成损失"，
而是"它是一条**不会报错的静默失效路径**，而且与一个已知缺口同构"。

---

## 2. 代码结构清晰化（重复消除）

审计用脚本对**提取出的函数体**做规范化比对，本轮把 4 组重复收敛到共享件：

| 形状 | 收敛前 | 收敛后 | 差异点 |
|---|---|---|---|
| `Caption`（次级说明文字） | **10 份**（8 份逐字相同） | `src/shared/components/Caption.tsx` | 2 份多一个属性（`testId` / `block`），都以可选参数保留 |
| `DASH`（缺值占位符 `—`） | **7 份** | `src/shared/format.ts` 的 `DASH` | `models/model.ts` 与 `network/model.ts` 的**再导出**保留（见 §2.2） |
| `SECTION_SX`（区块内边距+圆角） | **7 份**（逐字相同） | `src/shared/components/sectionSx.ts` | 无 |
| `SwitchControl`（符号化开关） | **2 份** | `src/shared/components/SwitchControl.tsx` | `hideLabel` 参数统一两种用法 |

删除的本地拷贝合计约 **90 行**；新增共享件约 **90 行** —— 所以**行数几乎没变**。
价值不在行数，而在**漂移被消除**：这四样东西各自会随时间的修改而分叉，
而界面上"差不多的两种小字/两种开关"正是这么来的。

### 2.1 `SwitchControl`：为什么这次值得"跨页重构"（推翻了一条旧判断）

`src/pages/models/index.tsx` 原来留着一段明确的判断：

> 为什么不从 `providers/detail.tsx` 复用：那一份是那个文件的**私有组件**，没导出。
> 与其把两个页面的私有件提成公共组件（那是一次跨页重构，风险大于收益），这里按同一套口径再写一份。

**那个判断在当时是对的**（两份口径一致，提公共件确实要动两个文件）。但两处后来**各自漂移**了：
本文件恒 `label={undefined}`，那一处有 `hideLabel`。而漂移点恰好是**无障碍名称** ——
开关在视觉上不写"启用/关闭"，读屏的名称**只能来自 `aria-label`**；谁漏给一次，
那一屏的开关对读屏用户就是"无名称的复选框"。

共享件把 `label` 变成**必填参数**，从根上消掉这个漏法。这正是"重复会自动漂移"的实例。

### 2.2 `DASH`：我**先犯了错，再改对**（如实记录）

审计把 `models/model.ts` 与 `network/model.ts` 的 `export const DASH` 列为"真正死码"。
我按这个结论把 **7 处定义全部删掉、改成从 `@/shared/format` 引入** —— 结果 `tsc` 报出
**4 个文件编译失败**：

```
src/pages/models/index.tsx(66,3): error TS2459: Module '"./model"' declares 'DASH' locally, but it is not exported.
src/pages/network/assignments.tsx(53,3): error TS2459: …
src/pages/network/index.tsx(83,3): error TS2459: …
src/pages/network/nodes.tsx(64,3): error TS2459: …
```

即：那两个 `DASH` **不是死码**，而是**被兄弟模块 import 的公开导出**。
审计的"零引用"结论漏掉了"同一目录内的相对 import"这一路径。

**处置**：5 处私有拷贝删掉（改用共享件），那 2 处改成**再导出**
（`export { DASH } from "@/shared/format";`），并在注释里写明"真源已收敛到 `@/shared/format`，
这里保留一次再导出是因为兄弟模块从 `./model` 取它"。
这样**字面量只剩下 `format.ts:24` 一处**，同时不动那十几个调用点的引用面。

**教训**：审计结论是**线索**不是**判决**。凡"删掉某个导出"的动作，必须让编译器过一遍
（这次正是 `tsc` 抓到的）—— 而我原计划里那一步差点就"以为很安全"地做过去了。

---

### 2.3 顺带清掉的死码（每条都有独立核实）

| 项 | 核实方式 | 结果 |
|---|---|---|
| **235 个无引用的 i18n 键**（zh 与 en 各 235，共 470 行） | 见下方 §2.4 | 删掉；键数 **1267 → 1032**，两语言**完全一致** |
| `src/app/index.ts`（整个桶文件） | `rg 'from "@/app"'` → **0 个 importer**；且 `App.tsx` 直接用 `@/app/AppShell`，桶文件自己的 docstring 声称的"两个入口"从未被遵守 | 删除；`tsc` 通过（证明真的没人用） |
| `src/pages/onboarding/`（空目录） | `Get-ChildItem` → 0 文件；`git ls-files` → 未跟踪（git 不跟踪空目录） | 删除（本地残留） |

### 2.4 i18n 死键清理：方法与**我犯的错**

**方法**（写成了脚本 `.ta-tmp/audit-i18n.mjs`，可复现）：收集 i18n 之外**所有**源文件文本 →
① 字面命中（键名出现在任何位置，任意引号）→ 不算死；
② 动态前缀命中（`` `prefix.${x}` `` 形式）→ 不算死（实测只有 3 个前缀：
`component.statusDot.`、`logs.chips.`、`models.add.issue.`）；
③ 两边都没命中 → 报死。

**再加四道交叉检查**（因为"删错一个键"只会静默显示成键名，不会报错）：
Rust 源码里有没有引用（0）、`scripts/` 里有没有（0）、有没有 `` t(`${x}.title`) `` 这种
"变量在前"的拼法（0）、两语言键集是否仍完全一致（一致）。

**结果**：235 个（审计报 236，差 1 —— 我按自己的判据走，不追平那个数字）。
删完 `tsc --noEmit` 干净（`I18nKey = keyof typeof zhCN`，**多删一个都会报**）、
`vitest` 596 全过、两语言仍 **1032/1032 完全一致**。

#### 我在这件事上**先犯了一个错，且是自查发现的**

删完死键后，文件里留下了一批"指向已删段落"的**孤儿注释**（例如
`// ---- 套餐与线路（[07 §5]）` 下面一个键都不剩）。我用脚本清它们时，
判据写成了"注释后面**紧跟**的是另一个注释或 `}`" —— 这个判据**太粗**，
把 **10 个合法标题**也删了（例如 `// ---- 事件/提示` 后面先跟一行解释性注释、再才是键）。

发现方式：我没有直接相信"删了 13 行注释"这个结果，而是**从备份里逐个核对**每一处
"后面到底有没有键"。核对出 10 处误删。

**处置**：从备份**重做一遍**（`.ta-tmp/purge-i18n-v2.ps1`），判据改成：
> 从本行往下扫，直到"下一个段落标题"或 `}` 为止 —— **中间只要出现过键，就不是孤儿**。

重做后共删 7 处孤儿注释（zh 4 + en 3），并**用不变量反查**：
"每一个保留的段落标题，到下一个标题之间必须至少有一个键" → 实测
`zh-CN.ts` 25 个标题 / `en.ts` 19 个标题，**空段落 0 个**。

**教训（与 §2.2 的 `DASH` 事故同型）**：机械清理之后，必须用**不变量**去反查，
而不是看"删掉了多少行"就收工。两次都是"我以为很安全"的地方出的问题。

## 3. 文档路径明确化

### 3.1 修掉的断链

| 位置 | 原来 | 现为 | 性质 |
|---|---|---|---|
| `docs/31-progress.md:1366` | `](docs/35-auto-manual-verification.md)` | `](35-auto-manual-verification.md)` | 相对 `docs/` 解析成 `docs/docs/…`，**断** |
| `docs/31-progress.md:1369` | `](docs/30-minimal-ui.md)` | `](30-minimal-ui.md)` | 同上 |
| `docs/31-progress.md:1370` | `](docs/evidence/real-machine)` | `](evidence/real-machine)` | 同上 |

根因：这三条是按**仓库根**的相对路径写的，而文件在 `docs/` 里。
（三处都是同一个错，审计只报了前两处、第三处是我自己复查时补上的。）

### 3.2 `docs/24`：一个**从未存在**的文档，被 7 处引用

```powershell
git log --all --name-status -- 'docs/24*'   # 空 —— 全历史里都没有
git ls-files docs                            # 也没有
```

但代码里有 7 处引用它（`crates/ta-store/migrations/0005_quota_windows.sql`、
`src/pages/providers/detail.tsx`、`src/pages/providers/model.ts`、`src/shared/i18n/{zh-CN,en}.ts`）。

**主题可从引用点反推**（"多窗口额度 / credential quota windows"），但文件不存在。

**处置：不补写、不改编号**（见 §3.3 的编号规则）。这些引用是**历史轮次的痕迹**，
它们指向的功能已在第 314 轮删除（`crates/ta-quota` 整个 crate、`QuotaWindow` 改名
`PlanFeeCycle` 等）。本文件与 `docs/README.md` 的索引表把 `docs/24` 记为
**"编号空缺（曾为多窗口额度文档，从未入库）"**，这样 grep 到一个悬空编号时能落到解释上。

### 3.3 新增 `docs/README.md`：编号规则 + 全量索引 + 引用格式

三件事写在一处：

1. **编号是只增不改的，空缺是永久的** —— 绝不重编号、绝不复用。
   理由：全仓有 **2500+ 处** `docs/NN` 引用（`docs/28` 一份就背着 291 处），
   重编号会让它们**全部失效**，而且失效是静默的（注释里的路径没人编译检查）。
2. **全量索引**（39 份文件逐条列出：编号 / 文件名 / 主题 / 状态）。
   README 里原来的索引**停在 docs/15**，另外 24 份文档根本不在索引里。
3. **规范引用格式**：`[docs/NN §M]`，并明确**禁止** `docs NN`（空格）这种写法 ——
   它只出现在 `src/shared/i18n/en.ts`（13 处），而它正是让悬空的 `docs 24` **躲过
   `docs/24` 检索**的原因。这 13 处已统一成 `docs/24` 形式。

---

## 4. 本轮**没有**做的事（如实列出）

不是"没想到"，而是判断后不做，每条都写理由：

| 事项 | 为什么不做 |
|---|---|
| 清理 **73 个**无引用 testid | 其中相当一部分标的是**缺失的测试**（例如 `logs-stat-avg` 是因为测试断言的是动态拼出来的 `logs-stat-avg-value`）。**优先补断言，而不是删 id** —— 删 id 等于把"这里本该有测试"的线索也删了。 |
| 处理 `docs/46` 的 18 处**失效章节锚点**（`§288–§295/§299/§310`） | 那些轮次的内容在 `docs/46` 里**根本不存在**（它从"第 287 轮"直接跳到"第 296 轮"）。补写 = **编造章节**；改锚点 = 改 18 处指向且要逐处判断该指向哪。**留待确认这些轮次是否真有记录**。 |
| 处理 `docs/16` 的 3 处 `[16 §8]` | `§8` 是 §3 里的一个**编号列表项**、不是标题。改法有两种（升成标题 / 改指向 §3），需内容判断。 |
| `RuntimeParts` 另 3 个未调用 builder（`with_sink`/`with_meter`/`with_ingress_options`） | 与 `with_trace_store` 同族，但**文档没说"生产应当接上"**：`with_meter` 的字段文档明写"`None` = 计量还没接上（G4 在途）"，即**已知未接线且是刻意的**。删它们会砍掉接缝；**保留并在 §5 记为"已知状态"**。 |

---

## 5. 威胁面清单：还剩什么（**不许读成"已清完"**）

### 5.1 已知的、刻意的未接线（不是缺陷，但要有人知道）

| 项 | 现状 | 依据 |
|---|---|---|
| 计量（`with_meter`） | **未接线**（`RuntimeParts.meter = None`） | 字段文档明写"G4 在途"，且**刻意不提供空实现**（"静默地'接了但没数据'比没接更糟"） |
| 自定义 sink（`with_sink`） | 未接线 | 只有文档链接引用它，无调用方 |

### 5.2 代码里**作者自己标注**的待办（`TODO`/`FIXME` 全仓 7 条，都在 Rust）

| 标记 | 位置 | 内容 |
|---|---|---|
| `TODO(A1)` | `src-tauri/src/boot.rs:235` | 内核 Ready 后校验线路对 `/proxies` ∪ provider 节点 |
| `TODO(security)` | `crates/ta-secrets/src/resolver.rs:81` | 加 `zeroize::ZeroizeOnDrop` |
| `TODO(security)` | `crates/ta-secrets/src/portable.rs:37,701` | 收紧 Windows ACL 到当前用户 |
| `TODO(P7-followup)` | `crates/ta-secrets/src/migrate.rs:295` | `gateway.local_key` 仍是明文 |

**这四条尤其要看**：`docs/50` 本轮**没有**去改它们（`portable.rs` 的 ACL 要 `windows-sys` 依赖，
`resolver.rs` 的 zeroize 要新依赖，都超出"结构清晰化"的范围），但它们**是本仓自己承认的
安全待办**，且 `crates/ta-secrets/SECURITY-NOTES.md` 有更完整的记录。
**"威胁全部清除"这句话在本轮结束时对它们不成立。**

### 5.3 本轮**没有审计**的面

时间与工具都有限，以下**我这一轮没有查**，所以不能声称它们安全：

* **依赖供应链**：`mihomo` 二进制的来源与哈希固定方式、GeoIP 库的固定方式、
  `npm`/`cargo` 锁定状态在**新机器**上能否复现（我只能确认本机 `npm install` 正常）。
* **病态输入**：巨大的订阅响应 / 深递归 YAML / 压缩炸弹——没有做模糊测试或压力测试。
* **CSP**：`tauri.conf.json` 的 `app.security.csp` 是 **`null`**（即不启用 CSP）。
  我已核实**当前没有 HTML 注入入口**（全仓无 `dangerouslySetInnerHTML`/`innerHTML`/
  `eval`/`new Function`，见 §6 的证据），ECharts 的 `tooltip` 是唯一"输入进 HTML"的可能路径；
  但"现在没有入口"与"开了 CSP 也安全"是两件事 —— 这一条**留作下一轮的候选**。
* **Windows 平台细节**：保留设备名（`CON`/`NUL`）、备用数据流（`:`）、UNC 路径、
  长路径在导出/导入路径处理里的表现。

### 5.4 `step30` 的第二个"拿挂具证明不了的东西判红"（本轮修掉）

真机 e2e 的最后一次跑出现 `FAIL=1`，红的是 **step30**：

```
应用说上次那条不通（previous_unreachable），但挂具这张表里 新加坡-优化2-GPT 是 alive 且有延迟 ⇒ 误判会换出口 IP
```

**证据链**（每一环都可复现）：

1. 应用日志里那一轮的真实判决：
   `status="previous_unreachable" node="新加坡-优化2-GPT" probed=34 measured=28 picked_ms=259 best_ms=259`
   —— 注意 `picked_ms == best_ms`：它选出来的那条**就是这一轮量到的最低延迟**，
   不是"没量到还硬选"。
2. 应用记的"上一次地址"在 `store_setting` 里：
   `{"key":"auto_export:ta-e2e","value":"m.cnmjin.net"}`
   —— 是 **server 主机名**（不含 port）。而 `choose_auto_export` 的判据是
   `find(|c| c.address == previous)`，**地址级**。
3. 挂具读的是 mihomo `/providers/proxies/<key>`，里面**只有名字**与内核自己的健康历史，
   **名字级**。
4. 真机订阅里**多条节点共用同一个 server**：
   `新加坡-优化2-GPT` → `server: m.cnmjin.net, port: 16618`；
   `香港-优化2` → `server: m.cnmjin.net, port: 16632`。

⇒ "表里那个**名字** alive"**推不出**"应用误判了那个**地址**"，更推不出"会把出口 IP 换掉"。
再加上 mihomo `history` 里的 `delay` 可能是**上一轮**的旧值，
这个判据**层次不够**，只能产出假红。

**处置**：该分支从"判红"改为**如实记一条「无法判定」**并继续观察，同时保留一条**同层次**的硬判据：

* 应用报 `previous_unreachable` **却一条都没选出来**（`node` 空/未选）
  ⇒ "既没沿用也没重挑"的自相矛盾 ⇒ **判红**（这条挂具能证明）；
* 应用报 `previous_unreachable` 且选出了最低延迟那条，挂具只是按名字看到 alive
  ⇒ 记**无法判定**（写明地址级 vs 名字级、共用 server、history 可能过期），**不判红，也不判假绿**。

顺手加了一个只读助手 `storedAutoAddress()`：把**应用自己记的地址**从
`store_setting.auto_export:*` 读出来放进证据 —— 这样下一个人一眼能看到
"应用说的地址"与"挂具看的名字"根本不是一回事，而不是只剩一句结论。

> 这与本档 §7 修掉的那处（把**说明文案**当判据）是**同一族的错**：
> 挂具用一个它证明不了的层次给产品定罪。两次都是**假红** ——
> 按本仓纪律，**假红与假绿一样是撒谎**：它会让一次真实的回归淹没在噪声里。
>
> 诚实边界：这条改动**降低了该档的判定强度**（原来它声称能抓"误判换 IP"，现在承认抓不了）。
> 我没有假装它还抓得住。要真正抓住它，挂具得按**地址**自己核对可达性
> （`store_setting` 的地址 + 订阅里的 name→server 映射）—— 那是**下一轮的活**。
>
> **并且这次修复不再"靠真机碰运气"**：判定抽成纯函数 `judgePreviousUnreachable`，
> `--selfcheck` 现在覆盖 **9 条**用例（5 条沿用判定 + 4 条重挑判定），
> 其中**两条**是两次真机假红的**原样输入**。
> 修复后第一次全量跑**又恰好没走到这条分支**（那轮是 `same_address_still_there`）——
> 所以"绿了"本来不能当证据。改为**注回旧判据验牙**：恰好那 2 条关键用例变红，
> 3 条反向用例仍正确分类（证明判定没被放松成"什么都不查"），验完恢复、痕迹清零。

---

## 6. 本轮**核实过、确认是扎实**的部分（不全是否定清单）

用户要的是"清除威胁"，但同样重要的是**不要为了显得干了活而去改本来正确的东西**：

| 面 | 证据 |
|---|---|
| 网关**只绑回环** | `gateway.listen` / `admin_listen` / `kernel.controller` 默认全是 `127.0.0.1:*`；`parse_loopback_addr` 强制回环（`localhost` 归一化后仍走同一检查）；`bind_loopback("0.0.0.0:8788")` 有测试断言**必须失败** |
| API key 比较是**常量时间** | `crates/ta-gateway/src/admin.rs` 用 `ct_eq` |
| 请求体读取有上限 | `read_body_limited` |
| **没有 HTML 注入入口** | 全仓扫描 `dangerouslySetInnerHTML` / `innerHTML` / `outerHTML` / `insertAdjacentHTML` / `document.write` / `eval(` / `new Function` → **0 命中** |
| 自启动只有一条路径 | `capabilities/` 不授予任何 `autostart:*`，且**有测试守住**；真实现在 `lifecycle::AutostartController` |
| 日志字段纪律 | `log_field_guard`（tracing 不许有自由文本字段，细节进 `message`）；`diag::sanitize` 有脱敏测试 |
| 密钥不落明文（在磁盘上的那份） | `docs/14`/`ta-secrets` 的口径 + `secret:name` 模板；**但**见 §5.2 的三条 `TODO(security)` |
| 命令面 | `command_surface_guard`：`LIVE_CONTRACT` / `REGISTRY` / `ARCHIVE` 三处一致，**120 个命令** |

---

## 7. 验证

| 项目 | 结果 |
|---|---|
| `cargo fmt --all --check` | 干净 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 干净 |
| `cargo test --workspace` | **1309 passed / 0 failed / 1 ignored**（上轮 1306 → 本轮 +3 = 新增的 3 条） |
| `tsc --noEmit` | 干净 |
| `vitest run` | **596 passed / 0 failed（30 文件）** |
| 新增守卫**验牙**（能力守卫） | 注入 `fs:default` → 立刻红（且 Tauri 编译期也会拒绝，见下） |
| 新增守卫**验牙**（trace 接线） | 删掉 `.with_trace_store(` → **立刻红**；恢复后绿 |
| 新增测试的**对照组** | `without_trace_store_nothing_reaches_the_trace_table` 证明上一条不是假绿 |
| e2e 自检 `--selfcheck` | **9/9**（5 条沿用判定 + 4 条重挑判定，含两次真机假红的原样输入） |
| e2e 自检**验牙** | 注回两条旧判据 → **恰好那 2 条关键用例变红**，反向用例仍正确分类；恢复后 9/9、痕迹清零 |
| 真机 e2e | **PASS=29 SKIP=2 FAIL=0**（exit 0） |
| 断链检查（**剥掉行内 code span 之后**） | `docs/**` 与根 `README.md` **无断链** |

### 7.1 关于能力守卫的**诚实定位**（我原来高估了它）

验牙时发现：把 `fs:default` 加回能力集，**Tauri 自己的 build script 就会拒绝编译**：

```
Permission fs:default not found, expected one of autostart:default, … core:window:deny-unminimize, …
```

也就是说"删了插件忘了删权限"**过不了构建**。所以我的新守卫**不是唯一防线**，
而是**纵深防御**：它报错更早、信息更明确（指出"该插件未注册"而不是"权限不存在"），
并且把规则用测试固化下来（`capabilities/README.md` 里已如实写明这一点，没有夸大）。

---

## 8. 结论

用户的三条要求，逐条落到可核对的状态：

1. **代码结构更清晰** —— 4 组重复收敛到共享件（`Caption` 10→1、`DASH` 7→1（字面量）、
   `SECTION_SX` 7→1、`SwitchControl` 2→1），并**推翻了** `models/index.tsx` 里一条
   已经失效的"不复用"判断（理由见 §2.1）。
2. **文档路径更明确** —— 3 处断链修掉；`docs/README.md` 建立（编号规则 + 全量索引 +
   引用格式）；`en.ts` 的 13 处 `docs NN` 统一；`docs/24` 这个悬空编号被如实登记。
3. **威胁清除** —— 清掉了**具体的、可验证的**几项（4 个零调用插件及其白送的权限、
   一条从未接线的 trace 静默失效路径），并给它们都配了**会红的守卫**。
   **但"全部清除"没有做到，也不可能在本轮做到**：§5.2 里 4 条作者自标注的安全待办仍在，
   §5.3 里 4 个面（供应链 / 病态输入 / CSP / Windows 路径细节）本轮没有审计。
   本文件的写法就是为了让下一个人能**接着往下清**，而不是以为已经清完了。

---

## 决策记录：随包内核与威胁面（合并自一份 4000 行的过程文档）

* **随包分发 mihomo（GPL-3.0）**：以独立可执行文件 + 本地 HTTP 控制口通信，属 GPLv3 §5 末段的
  聚合，不改变本仓代码的许可。对应源码按 §6(d) 与安装包挂在同一个 Release
  （见 `THIRD-PARTY-NOTICES.md` 第 1 节）。
* 内核二进制**不入库**：由 `scripts/fetch-mihomo.ps1` 按钉死的 tag + sha256 拉取。
  这同时让"**改没改过内核**"永远可查 —— 它直接决定 GPL 下要不要公开我们自己的改动。
* **`TA-5001` 的含义**：启动时还没有任何 provider/节点，生成配置的预检会**如实拒绝**。
  订阅就绪后由 `kick_kernel` 重拉内核；**但那次重拉必须先补端口**（`ensure_lane_ports`），
  否则内核因 `port_missing` 永不就绪、**网关根本不启动**（连直连也不可用）。
* 由此得出一条爆炸半径结论：**内核就绪前网关不启动** —— 配错一条线路的代价是整个网关，
  不只是那一条出口。

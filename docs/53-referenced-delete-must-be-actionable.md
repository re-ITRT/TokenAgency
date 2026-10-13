# docs/53：删被引用的订阅时，拒绝必须能照着改

用户第 320 轮的真机报告（逐字）：

> 网络那边点击删除订阅
> 删除订阅
> 删除后无法撤销。确认删除以下对象？
> mojie（s1）
> 会连带删除 / 影响：
> 引用这份订阅的 1 条线路
> `[TA-5001] 配置校验失败：1 处问题`

这段界面里**有两处说了假话**，而且是两处独立的缺陷。两手都修了。

---

## 1. 缺陷一：确认框承诺了一件不会发生的事

确认框的表头写「**会连带删除 / 影响：** 引用这份订阅的 1 条线路」。
用户读完会很自然地认为"点确认 → 订阅和那 1 条线路一起没了"。

**实际不会。** `subscription_delete` 没有 `force` 参数，被线路引用时它**一个字都不会删**，
只会拒绝。"会连带删除"是**承诺**，而这个承诺不成立。

### 根因

`ConfirmDelete` 的表头是写死的：

```tsx
<Typography>{t("delete.consequences")}</Typography>   // "会连带删除 / 影响："
```

而 `cascadeAvailable` 这个 prop **已经**准确表达了"这一族实体到底能不能级联"
（`provider` / `credential` / `model` 为 `true`；`subscription` / `lane` / `channel` 为 `false`），
表头却完全没看它。于是同一句文案被用在了两种相反的语义上：

| 实体族 | `cascadeAvailable` | 真实语义 | 该说的话 |
|---|---|---|---|
| provider / credential / model | `true` | 确认后会**连带删掉**列出的东西 | 「会连带删除 / 影响：」 |
| subscription / lane / channel | `false` | 列出的引用会**拦住**这次删除 | 「这些引用**会阻止删除**（先改掉它们再删）：」 |

### 修法

表头按 `cascadeAvailable` 分两种文案（新键 `delete.blockers`，中英各一份）：

```tsx
{cascadeAvailable ? t("delete.consequences") : t("delete.blockers")}
```

注意**不是在吓唬用户**：列出引用清单本身是对的（用户要据此知道该去改哪条线路），
错的只是**把它说成"会被删掉"**。

---

## 2. 缺陷二：结构化信息在最后一厘米丢掉了，用户只看到一个 TA 码

拒绝之后界面显示的是 `[TA-5001] 配置校验失败：1 处问题`。
**这句话不指出是哪条线路挡着，也不说该怎么办。**

### 根因（两层）

**后端这一层**：`subscription_delete` / `delete_lane` 不做前置引用检查，
而是先把订阅从 doc 里摘掉，再交给 `commit_config` → 校验器。
校验器发现了悬空引用（`lanes[0].subscription` 指向不存在的订阅），
于是报 `TA-5001 配置校验失败`。

问题在于**语义被说错了**：

| | 真实情况 | 报出来的码 |
|---|---|---|
| 用户读到的 | "你的配置有 1 处错误" | `TA-5001` |
| 实际发生的 | "这次删除会让 1 条线路悬空，所以拒绝" | 应该是 `TA-5002` |

仓里**早就有**这条正确口径 —— `entity.rs` 的 `referenced()`：

```rust
let hint = if force_capable { "；要连带删除请传 force=true" } else { "；要先解除这些引用再删" };
```

`delete_provider` / `delete_channel` 都走它（第 73 轮真机 E2E 还专门抓过"没有 force 却承诺
force=true"的撒谎）。**订阅与线路这两条路径漏了。**

**前端这一层**：即使后端把字段路径放进了 `details`
（`IpcError::from_field_errors` 确实放了，`FieldError = { path, code, message }`），
前端唯一的格式化出口 `errorMessage()` **只读 `message`**，把 `details` 整个丢掉。

```ts
// 修之前
if (err instanceof TokenAgencyError) return `[${err.code}] ${err.message}`;
```

所以两层都要修，缺一层用户看到的还是那句废话。

### 修法

**后端**：`subscription_delete` / `delete_lane` 都改成**先查引用、再动手**，
走与 `delete_provider` 同一条口径（`entity::referenced`，加 `pub(crate)`）：

* `subscription_delete`：引用者 = `lanes[].subscription` 指向它的那些线路，
  文案里用线路的**名字**（人话），名字为空才退回 id；
* `delete_lane`：引用者分**五类**逐类列出 —— `net.default_lane`、`providers[].lane`、
  `credentials[].lane`、`channels[].lane`、`policies[].action.use_lane`；
* 两者都**没有** `force` → 只报"先解除"，绝不提 `force`（提了就是撒谎）。

> 术语校正：`lanes` = **线路**，`channels` = **通道**（[docs/28 §1]）。
> 我一稿把 `channels[]` 写成了"线路 xxx"，那会读成"线路 xxx 在挡线路 yyy"，
> 用户根本看不出是谁在挡。已改成"通道 xxx"。

**前端**：`errorMessage()` 在 `details` 是逐字段错误时，把**路径与说明**一起拼上：

```
[TA-5001] 配置校验失败：1 处问题 · lanes[0].subscription：引用的 `s1` 不存在（应在 proxy_subscriptions[].id 里）
```

新增 `fieldErrorsOf()` 做形状校验：`details` 不是那个形状时**宁可只给 `message`**，
不许拼出 `[object Object]` / `undefined`。没有 `details` 的错误**逐字不变**。

---

## 3. 修完之后用户看到什么

确认框（同样是删 `mojie`）：

> 删除后无法撤销。确认删除以下对象？
> mojie（s1）
> **这些引用会阻止删除（先改掉它们再删）：**
> 引用这份订阅的 1 条线路

点确认：

> `[TA-5002] 对象被引用，禁止删除：订阅 sub-a（引用它的地方：自动择优；要先解除这些引用再删）`

三件事都齐了：**谁在挡**（自动择优）、**该怎么办**（先解除这些引用）、
以及**一个不骗人的码**（5002 = 被引用，而不是 5001 = 配置坏了）。
框**不关**（原文就留在那儿），库里一个字节都没动。

---

## 4. 验证

| 项目 | 结果 |
|---|---|
| `cargo test --workspace` | **1318 passed / 0 failed / 1 ignored** |
| `tsc --noEmit` | 干净 |
| `vitest run` | **620 passed / 0 failed（33 文件）** |
| `cargo fmt` / `clippy -D warnings` | 干净 / 干净 |
| `build.ps1` | exit 0（五阶段全过） |
| 真机 e2e | **PASS=29 SKIP=2 FAIL=0**（exit 0） |
| e2e `--selfcheck` | **20/20** |
| i18n 键数 | zh **1044** / en **1044**（逐键对齐） |

**交付**：`TokenAgency_0.1.0_x64-setup.exe` **22.33 MiB**，SHA256 `CBEB463264EA50BC…`；
被测/已安装 exe `94BECD5FE0C8F9B1…`，与 `build-identity.json` **逐字相同**，构建后源码零改动。

> **交付顺序的如实记账**：第一次构建后我又做过两次源码改动 ——
> ① 修 e2e 挂具（行定位按 `name` 而不是 `id`）；
> ② 给 `errorMessage` 做**验牙**（注回旧实现 → 恰好那 2 条红、4 条守卫仍绿 → 恢复）。
> 前者是挂具、后者虽在恢复后行为等价，但**我无法证明字节相同**，
> 所以**重新构建 + 重新安装 + 重新跑了 e2e**，上面这组数字全部来自最后一版。

### 真机上这一档的**原样证据**（就是用户报的那一步）

```
用这一对：订阅 ta-e2e-sub-form（表格里显示的名字="e2e"）← 线路 lane-ta-e2e-auto
后端原文 = "[TA-5002] 对象被引用，禁止删除：订阅 ta-e2e-sub-form
            （引用它的地方：自动（TA E2E 供应商）；要先解除这些引用再删）"
拒绝之后库里那份订阅还在 = true
```

对照用户当时看到的 `[TA-5001] 配置校验失败：1 处问题`：现在**谁在挡**（自动（TA E2E 供应商））、
**该怎么办**（先解除这些引用）、以及一个**不骗人的码**（5002 而不是 5001）三样都齐了。

### 挂具自己犯的一个错（如实记录）

这一档第一次真机跑是**红的**，但**不是产品的问题**：挂具按 `id`（`ta-e2e-sub-form`）
去找订阅那一行，而那张表显示的是**订阅名**（`e2e`）—— 整词匹配自然找不到行。
证据里那句是 `找不到行（network-subscription-row 文本含 ["ta-e2e-sub-form"]）`。
改成"按表格真正显示的那一列（`name`，为空才退回 `id`）"之后，同一档在真机上通过。

**教训与 [docs/50 §5.4] 那条同源**：挂具按自己以为的列去定位，就会产出假红。
定位键必须来自**界面真的渲染了什么**，不能来自"数据里有什么"。

### 新增的牙（每条都**验过牙**）

| 测试 | 钉什么 | 验牙方式（注回旧行为后必须红） |
|---|---|---|
| `subscription_delete_removes_unreferenced_and_refuses_referenced`（Rust） | `TA-5002` + 点名引用者 + "先解除" + **不许提 force** + 库没变 | 这条原来断言 `TA-5001` —— **它钉的正是这个 bug**，已改判据 |
| `lane_delete_removes_unreferenced_and_refuses_referenced`（Rust） | 同上，且要认出是**通道**在引用 | 同上 |
| `lane_delete_names_every_kind_of_reference`（Rust） | **五类**引用逐类都要认出来 | 去掉 `credentials[]` 那一类 → 该档立刻红，且失败原文**逐字就是用户报的**「配置校验失败：1 处问题」 |
| `errorMessage.test.ts`（前端，6 条） | 路径与说明要拼进去；形状不对**不乱拼**；无 `details` 的错误逐字不变 | 把 `errorMessage` 改回只返回 `message` → "路径与说明"那两条立刻红 |
| 确认框文案（前端，`network.test.tsx`） | 没有 force 时说「阻止删除」、**不说**「会连带删除」 | 把表头改回固定 `delete.consequences` → 立刻红 |
| e2e step21「被引用的订阅不许删」 | 真机上：`TA-5002` + "先解除" + **不是**「N 处问题」+ 库里没少 | 抽成独立结果项；没有可测的引用关系时**如实记"未覆盖"**，不假报通过 |

> **`lane_delete_names_every_kind_of_reference` 那次验牙特别值**：
> 我临时把 `credentials[].lane` 那一类从实现里去掉，测试不仅红了，
> 报出来的失败原文**一字不差就是用户截图里那句** `配置校验失败：1 处问题` ——
> 等于当场复现了这个 bug，也证明了这一档真的能抓住"漏认一类引用"。

---

## 5. 与既有文档的关系

* [docs/21](21-delete-and-lane-settings.md) 定了"删除必须先确认、被引用时不许假装"，
  本文件是它的一条**具体化**：确认框的文案也必须与被引用时的真实结局一致。
* [docs/15 §8] 的机械守卫（禁 emoji / 颜色字面量）不受影响；`delete.blockers` 是纯文案。
* 术语沿用 [docs/28 §1](28-lines-belong-to-providers.md)：`lanes` = 线路，`channels` = 通道。

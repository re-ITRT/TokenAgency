# 更新日志

本项目的里程碑记录。**每个条目都必须写清"到什么程度"**，而不是只写"完成了什么" ——
因为"写完代码"和"验证过"是两件事（[docs/13](docs/13-team-and-delivery.md) 的评审协议就是这么定的）。

---

## v0.2.1-w2fix · 门禁收口（第 11 轮）

> **目标只有一个：把"已知但没修完"的门禁缺口关掉，让 `cargo test --workspace`
> 不再需要任何 `--exclude`。** —— 这是开始 Agent Team 并行开发前的前提：
> 带着一个被绕过的门禁开工，等于让五个人把新 bug 堆在被掩盖的旧 bug 上。

### 修好了什么

**`tokenagency` 的 lib 测试目标无法运行（加载期 `0xc0000139`）—— 已根治**

- **根因（可复现）**：`dumpbin /imports` 逐符号与系统 DLL 的 `/exports` 比对后，
  **唯一**缺失的入口点是 `comctl32.dll ! TaskDialogIndirect`（其余 23 个 DLL 全部解析成功）。
  `TaskDialogIndirect` **只由 Common-Controls v6 导出**，v6 必须靠应用程序清单里的
  `<dependentAssembly>` 激活；没有清单就加载 v5 → 符号不存在 → 加载期直接死。
- **为什么只有 harness 中招**：`tauri-build` 把清单塞进
  `cargo:rustc-link-arg-bins=.../resource.lib`，**只有 bin 有**。
  T1 引入 `tauri-plugin-dialog`（→ `rfd` → `TaskDialogIndirect`）后，
  lib 测试 harness 第一次需要 v6。
- **修法**：`build.rs` 用 `cargo:rustc-link-arg=/MANIFEST:EMBED /MANIFESTINPUT:<清单>`
  供给**所有**目标，并用 `WindowsAttributes::new_without_app_manifest()` 让
  `tauri-build` 停止嵌清单（否则 bin 拿到两份 MANIFEST → `CVTRES CVT1100` + `LNK1123`）。
  清单文件必须**纯 ASCII**：`mt.exe` 把无 BOM 的 UTF-8 当 ANSI，中文注释会让它报
  `c1010070`。新增 `src-tauri/tests/runtime_manifest.rs` 作为回归守卫
  （从自身映像读 `RT_MANIFEST` 并断言声明了 Common-Controls 6.0）。
- **顺带证明的事**：`cargo:rustc-link-arg-tests` **覆盖不到** lib 单元测试 harness
  （其 target kind 是 `lib` 而非 `[[test]]`）—— 实测过，这也是必须走"让 tauri-build 闭嘴"
  这条路的原因。详见 [docs/06 §11.1](docs/06-roadmap-and-risks.md)。

**门禁一解开，立刻抓出一个真 bug**

- `plugins::tests::twelve_plugins_with_unique_ids` 断言 12，而 `PLUGINS` 早已是 11
  —— 数字停在"移除 `tauri-plugin-log`"之前。已改为 11，并把它升级成
  **带理由的守卫**：新增 `no_second_logging_system`，同时禁止 `PLUGINS` 与
  `Cargo.toml` 重新引入 `tauri-plugin-log`（两套日志系统 [docs/14 §1.1]）。
- **教训**：被绕过的门禁不会变绿，只会积累。第一个跑起来的测试就是红的。

### `scripts/gen-bindings.ps1`：一次真实事故 —— "只是想看看"把 105 条契约写坏了

- **症状**：跑这个脚本想看"还差哪些命令"，它**先把 `src/shared/bindings.ts` 覆盖成生成物
  （31 条）**，然后才打印"直接覆盖会让前端 import 失败"的警告 —— 那句警告是在
  **已经写坏之后**出现的。E3 实际踩到。
- **根因**：读模式与写模式**共用一个出口**。`$out` 既是"生成目标"又是"手写契约文件"，
  备份和护栏都有，但都作用在覆盖**之后**。所以这不是"忘了备份"，是**没有只读路径**。
- **为什么没造成损失**：该文件在 `ae7911d` 已被 un-ignore 并入库，`git restore` 即可完整
  还原。**"自动备份"救不了它，版本控制救了它** —— 这也是"唯一真相源必须入库"最硬的一次论据。
- **顺带挖出的第二个 bug**：emoji 守卫的正则写成 `[\u{1F000}-\u{1FAFF}]`，而
  **.NET 正则不支持 `\u{...}`**（那是 PCRE/Java 语法）—— 每次运行都抛
  `Insufficient or invalid hexadecimal digits`，脚本以退出码 1 死在守卫这一步，
  于是"缺 75 条命令"的清单**只打出了前 20 条**就断了。一个坏正则把"缺口报告"变成了半张报告。
- **修法**（三处，各自可独立验证）：
  1. 生成物永远先落 `target/bindings/bindings.generated.ts`（不在 tsconfig 的 include 里、
     不进 git、不被 `tsc` 检查），**护栏全部跑完之后**才决定要不要复制到被跟踪的位置；
     默认行为是**纯只读**。
  2. `-Overwrite` 才碰 `src/shared/bindings.ts`；缺命令时**拒绝覆盖**，`-Force` 才强行；
     备份文件带时间戳、不再互相覆盖（原来固定叫 `bindings.ts.bak` 且每次被上一次的结果盖掉，
     "自动备份"这个承诺是假的）。
  3. 正则改 .NET 认的写法（星平面用**代理对** `[\uD800-\uDBFF][\uDC00-\uDFFF]`）；箭头、制表
     这类排版符号单独放行（Rust doc 里很常见，不该刷屏）；缺命令清单**完整打印**并落
     `target/bindings/missing-commands.txt`。
- **验证**：旧正则的异常原文已复现；新正则在 7 个用例上验证，**含两个反向对照**
  （`⚠` 这种设计标记必须判失败、`→` 这种排版必须放行）；`Parser::ParseFile` 确认脚本 0 语法错误。
- **教训**：**"我只是想看看"也必须有一条只读路径。** 当脚本的用途是"报告状态"时，
  它的默认行为就不该有副作用。

### 门禁状态（本基线实测，**无任何 --exclude**）

```
cargo check   --workspace --all-targets                → exit 0
cargo clippy  --workspace --all-targets -D warnings    → exit 0
cargo test    --workspace                              → 778 passed / 0 failed
cargo fmt     --all --check                            → exit 0
```

其中 `tokenagency` 的 lib 测试目标现在真的跑起来了：**63 个测试全绿**
（此前为 0 个 —— 它整个加载不起来）。

---


## v0.2.0-w2 · W2 收口基线（2026-09-22）

> **这是"代码库整理完毕、门禁全绿"的基线，不是 M1 完成。**
> M1 还缺 T1 的命令面补齐与启动编排接线（见下方"尚未完成"）。

### 本轮做了什么

**内核验证（M0 结案）** —— 在真 mihomo v1.19.31 上跑完 K1–K8，只剩 K9：

- **K1 成立**：`PUT /configs?force=true` 是进程内热重载（PID 不变、旧端口关、新端口开、可逆）
  → 现有的"能 reload 就 reload"策略正确，不需要"重启内核 + 请求排队"。
- **K2 成立**（但需细化）：冷启动 `use: [provider]` 的组不失败；**然而 provider 加载失败时
  内核照常启动、REST 照常通，所有 lane 静默降级成 `COMPATIBLE`** → "就绪"的判据必须包含
  "每个 provider 的 `proxies[]` 非空"。
- **K3/K4 成立**（各带一条危险附属发现）：`filter` 匹配不到节点会静默变 `[COMPATIBLE]`；
  `listeners[].proxy` 指向不存在的名字**照样绑定成功、零报错**。
- **K5 成立**：多余字段、`header` 列表形式、空集合全部被接受 —— 反过来说明"内核不报错 ≠ 配置正确"。
- **K6 成立**：`mixed-port: 0` 确实关闭默认入站 → I1 由结构保证。
- **K8 成立**：强杀后选中项仍保留（`store-selected` 在变更时写盘）→ 不需要为它做优雅退出。
- **K9 未验**：需要受限沙箱环境，开发环境是 full-access，测了不代表受限场景。

**统一裁决（M0 最值钱的产出）**：**内核在所有可疑情况下都倾向于"沉默地降级"而不是报错。**
所以凡是我们能在生成/启动阶段判定的，就必须自己判定，并选"响亮地失败"那一侧。

**可观测性基础（OB1 的纯逻辑层）**：

- 新增 `crates/ta-core/src/diag/`：脱敏规则、轮转策略、字段命名契约。
- 新增两条**守卫测试**：`log_field_guard`（字段名必须在契约内）、`error_log_guard`（报 `TaError` 必须同时记 `code`）。
- 把"字段名漂移"从愿望变成**会让 CI 红的规则**；并顺手修掉了它抓出来的**真实违规**：
  `lane`→`lane_id`、`subscription`/`sub`→`sub_id`、`provider`→`provider_id`、`cred`→`credential_id`，
  以及 15 处缺 `code =` 的错误日志。
- **契约修正**：§5.5 原来写"不在白名单里就失败"太绝对（实测有 42 个不同的本地上下文字段）。
  改为两层规则：§5.1–§5.4 强制、本地上下文字段登记后允许。

**日志契约的落地修正**：

- `TaError` 的 `Display` 保持只有中文（给用户看的文案不该出现内部编号），
  改为**强制成对记录** `code = e.code()`。
- 实测生效：日志里现在能看到 `code=TA-9001 error=内部错误：…`。

**桌面层（T2 已入，T1 部分）**：

- T2（托盘、插件注册、生命周期、开机自启动）已提交：自启动漂移逻辑复用 `ta-core` 的纯函数、
  `state()` 只读不写、测试用临时注册表键不污染用户真实启动项。
- **移除 `tauri-plugin-log`**：它与 `tracing_subscriber` 是两套并行日志系统（[docs/14 §1.1]）。
  之前用 `.skip_logger()` 只是压住 panic，问题仍在。

**视觉契约（用户直接要求）**：

- 新增 [docs/15-visual-design.md](docs/15-visual-design.md)：高级极简、**emoji 零容忍**。
- 唯一例外：**用户数据里的 emoji 原样呈现**（真实机场组名就是 `🚀 节点选择`）。
  规则是"我们永不自己产生 emoji"，而不是"禁止 emoji"。
- 已清理 `tray.rs` 里的全部 emoji（菜单项、tooltip、额度状态）。

### 归档：那份手写契约

`docs/contracts/bindings-handwritten-reference.ts.txt` —— F1 当年手写的 **105 条命令契约**。
它一直躺在 `src/shared/bindings.ts` 里，而**那个路径被 `.gitignore` 忽略、从未进版本控制**；
生成绑定一旦跑通就会覆盖它，**而当前 T1 只注册了 31 条命令** ——
直接覆盖会把契约从 105 缩到 31，砸掉 F2/F3/F4 的 import。

存档的用途是做"105 vs 31"的差集对照。**后缀是 `.ts.txt` 而不是 `.ts`**：
否则 `tsc` 会把它当源码一起编译，把前端类型检查搞红（这一点本轮真踩到了）。

### ✅ 绑定导出打通了（本轮最后解决的一件事）

**症状**：`gen-bindings.ps1` 一直失败，连一条命令都导不出来。两个连着的原因：

1. `specta-typescript 0.0.12`（**crates.io 上的最新版**）在 `primitives.rs` 里**硬编码**拒绝
   `i64/u64/usize/isize` —— 而我们的 DTO 里满是 Unix 毫秒时间戳与 token 计数。
   **没有任何配置项能放行**（我把源码读了：`primitive_dt(p, location)` 拿不到 exporter）。
2. 修完 1 之后紧接着：`serde_json::Value` 是**递归类型**，specta 展开它会产出**无限的 TS 类型**。

**解法（两处，缺一不可）**：

- **`Builder::dangerously_cast_bigints_to_number()`** —— `tauri-specta` 为这件事留的唯一开关。
  名字带 "dangerously" 是因为 TS 的 `number` 是 f64（安全整数上限 `2^53`）；
  **我们的取值是 Unix 毫秒 ≈ 1.8e12，差 3 个数量级，不会丢精度**。
  但这是**有前提的豁免**，前提已写进代码注释：将来若出现可能超过 `2^53` 的整数字段，必须单独处理。
- 给**真正任意的 JSON** 字段加 `#[specta(type = specta_typescript::Any)]`（只有 4 处）。

**顺带做对的一件事**：`header_diff` 原本是 `serde_json::Value`，但**它的形状是已知的** →
移到 `ta_core::trace::HeaderDiff`，让 `ta-gateway`（生产）与 `ta-store`（落库/导出）**共用一份定义**。
TS 侧因此拿到 `header_diff?: HeaderDiff | null`，而不是 `any`。

**验证**：`gen-bindings.ps1` 产出 **44717 字节**的 `bindings.ts`，`header_diff?: HeaderDiff | null`。

**护栏**：生成物会直接覆盖那份手写契约 → 脚本现在会做**名字差集**并逐条列出缺哪 75 条
（实测输出：`命令数：生成 31 / 手写契约 105`）。

> **还没做的是前端迁移**（T1b）：生成的绑定换了形态（camelCase 调用 + 领域类型直出），
> 而 `src/shared/api/**` 仍按手写契约的 `CommandMap` 写。所以**现在运行生成会让
> `npm run build` 报一堆 TS2305** —— 这是预期内的，T1 补齐命令面之后一并迁移。
> 本轮已在生成后还原手写契约，保证门禁是绿的。

### 修掉的真实缺陷

- **`Channel` 类型名与 Tauri 的运行时类型冲突**，导致绑定导出完全失败 →
  改名为 `ChannelConfig`（结构体名不进 JSON，**线上形态不变**）。
- **根 `Cargo.toml` 把 `typescript` feature 写在 `specta` 上**（该 crate 没有这个 feature）→
  任何 `specta = { workspace = true }` 的下游 crate 都会解析失败。
- **`gen-bindings.ps1` 跑的是 `cargo build`**（不执行代码，永远写不出文件）→ 改用 `cargo run` 的导出分支；
  并且**先删除输出文件**，否则导出失败时会把旧文件当成"已生成"。
- **pre-commit 的 clippy 会连带 lint 整个依赖图** → 同事在途的告警会卡住别人的提交。
  改为 `--no-deps`：**各人为自己的 crate 负责**。
- **`commit.ps1` 的 `git add` 退出码被静默忽略** → 钩子会检查到空的暂存集（**假绿**）。
- **`IpcError` 144 字节触发 34 处 `result_large_err`** → 装箱 `details`（线上 JSON 形态不变），
  而不是加 `allow` 让检查闭嘴。

### 尚未完成（**这是 v0.2.0 的真实边界**）

| # | 项 | 状态 |
|---|---|---|
| 1 | **T1 命令面**：已注册 31 条，契约要 95 条 | 缺 64 条 |
| 2 | **事件桥接**：`TaEvent` 10 个变体，前端一个都收不到 | 未接线 |
| 3 | **启动编排**：`BootOrchestrator::run` 从未被调用 → 内核不会被拉起 | 未接线 |
| 4 | ~~绑定导出的第二个冲突~~ | ✅ **已解决**（见下） |
| 5 | ~~`tokenagency` 的 lib 测试目标无法运行（`STATUS_ENTRYPOINT_NOT_FOUND`）~~ | ✅ **已解决**（v0.2.1-w2fix，见 [docs/06 §11.1](docs/06-roadmap-and-risks.md)） |
| 6 | 前端 F1 视觉重做（按 [docs/15](docs/15-visual-design.md)） | 未开始 |
| 7 | W5 打包与三条不变量验收 | 未开始 |

### 门禁状态（v0.2.0-w2 基线实测）

```
cargo check   --workspace --all-targets            → exit 0
cargo clippy  --workspace --all-targets -D warnings → exit 0
cargo test    --workspace --exclude tokenagency     → 712 passed / 0 failed
```

> ⚠️ **`--exclude tokenagency` 当时是第 5 条那个已知问题**：
> 该 crate 的 lib 测试目标在本机加载即崩（`0xc0000139`），与代码无关
> （正式应用 exe 能正常启动）。
>
> **该绕法已在 v0.2.1-w2fix 退役** —— 根因是缺 Common-Controls v6 清单资源，
> 已修；现在 `cargo test --workspace` 不带任何 `--exclude`，778 passed / 0 failed。

# 17 · 添加流程与"能力一致才可路由"规格（第 61 轮主管定稿 · 自顶而下）

> 本文件是**四个添加向导 + 能力校验**的唯一接口依据。写者按它实现；发现它与实际契约不符时，
> **先报主管**，不许自行发明字段或命令。

## 1. 概念映射（用户说法 → 本项目的实体）

| 用户说法 | 实体 | 写命令 |
|---|---|---|
| 服务商 / 提供商 | `Provider`（协议 + base_url + auth + 默认线路） | `provider_upsert` |
| 密钥 / 套餐 | `Credential`（provider + `${secret:...}` + 额度） | `credential_upsert`、`credential_set_secret` |
| 模型 | `Model`（对外暴露名 + 别名 + 分组/排序 + 是否暴露） | `model_upsert` |
| **线路** | `Channel`（模型 × 服务商 × 凭据 × 网络 lane × 上游模型名 × 优先级/权重/能力） | `channel_upsert` |
| 线路的计费信息 | `PriceEntry`（按模型/服务商匹配的费率） | `pricing_upsert` |
| 路由 | `Policy`（`match.model_matches` → `action.use_channels`、粘性 `pin_sticky`、失败切换 `failover`） | `policy_upsert` |

用户举的例子对应关系：
```
线路1 = Channel{ model: "deepseek-v4.1flash", provider: "ollama",       credential: "ollama-personal", lane: "direct" }
线路2 = Channel{ model: "deepseek-v4.1flash", provider: "commandcode",  credential: "cc-main",         lane: "direct" }
路由  = Policy{ match.model_matches: ["deepseek-v4.1flash"], action.use_channels: [线路1, 线路2], pin_sticky/failover ... }
```
计费：每条线路对应一条 `PriceEntry`（按 `match.model` + 可选 `match.provider`），
界面在保存线路时可一并写入（先 `channel_upsert`，再 `pricing_upsert`）。

## 2. 新增：上游能力声明 `ChannelCapabilities`（本轮的契约变更）

**为什么**：同一个对外模型可以由多条线路服务（Ollama 与 CommandCode 各一条），
但**只有两家上游暴露的能力完全一致**时，路由到它们才是等价的；否则把请求路由过去会出现
"有时有 vision、有时报错"这种不可预期行为。所以要把能力**显式声明**出来，并由后端**强制**校验。

```ts
/** 上游模型的能力声明。**两条线路只有在这些字段完全一致时**才允许被同一路由（同一对外模型）指向。 */
export type ChannelCapabilities = {
  vision: boolean;                 // 支持图片输入
  tools: boolean;                  // 支持函数调用/工具
  reasoning: boolean;              // 支持推理/思考内容
  context_tokens: number;          // 上下文窗口（token），整数
  max_output_tokens: number | null;// 单次最大输出；null = 未声明
  modalities: string[];            // 模态，如 ["text","image"]；比较时先排序去重
};
```

规则（**Rust 侧强制**，界面同款提示）：

1. `Channel.capabilities` 是 `#[serde(default)]`：老配置不写也能继续用。
2. **一致 = 上面每个字段逐一相等**（`modalities` 先排序去重再比；`null` 只与 `null` 相等）。
3. 校验点：
   - `policy_upsert`：同一条策略里 `use_channels`（或 `use_group` 展开后）**同一个 `model` 的多条线路**
     若能力不一致 → **拒绝**，错误必须**指出差在哪一项**（哪个模型、哪两条线路、哪个字段各自是什么值）。
   - `channel_upsert`：若该线路会被**已存在**的策略与其它线路归到同一 `model` 下，同样校验。
4. 错误码：从 [11 §5] 的错误码表里**取一个未占用的码**（新增变体 `CapabilityMismatch`），
   类型串 `tokenagency_capability_mismatch`，HTTP 400，不可重试，`details` 带结构化差异。
   同步：`docs/11` 表格 + `docs/10-interface-spec.md` 的 `Channel` 形状 + `src/shared/bindings.ts`。
5. 界面：在策略页选中多条线路时**即时**比对能力并显示差异；不一致就**禁用提交按钮**并把差异原文写出来
   （与后端同款措辞）。**不许**只在前端拦——后端也必须拦（前端只是提前反馈）。

## 3. 四个添加向导（固定 testid，真机 E2E 会按它们驱动）

### 3.0 i18n 键：**已由主管一次性落好，UI 写者不要动 i18n 文件**

四个向导需要的 40 个键已经在中英两份里（这样四个写者就不会再挤同一个共享文件——
本队前三轮有三次失效都发生在"多人同时改 `src/shared/i18n/*.ts`"上）。**直接用这些键**：

- 通用：`wizard.required` / `wizard.invalidId` / `wizard.invalidUrl` / `wizard.duplicateId` / `wizard.saved`
- 服务商与密钥：`providers.add.title` / `credentials.add.title` / `credentials.add.secret`
- 模型：`models.add.title` / `models.add.aliases` / `models.add.group` / `models.add.order` / `models.add.expose`
- 线路：`channels.add.title` / `.provider` / `.credential` / `.model` / `.upstreamModel` / `.lane` / `.price` / `.priceInput` / `.priceOutput` / `.priceCached` / `.priority` / `.weight`
- 能力：`capabilities.title` / `.hint` / `.vision` / `.tools` / `.reasoning` / `.contextTokens` / `.maxOutputTokens` / `.modalities` / `.mismatch` / `.mismatch.field`
- 路由：`policies.add.title` / `.exposeModel` / `.channels` / `.sticky` / `.failover`

按钮/取消等复用既有键（`common.save` / `common.cancel` / `common.enabled` / `common.disabled` / `common.refresh` / `common.empty`）。
**若确实缺键**：先报主管补（一次补完），不要自己在共享 i18n 文件里插。

### 3.1 `/providers` 服务商与密钥
- 「添加服务商」`providers-add-open` → 表单：`providers-add-id` / `-name` / `-protocol`（下拉，取 `PROTOCOLS`）
  / `-baseUrl` / `-authScheme` / `-authHeader` / `-authPrefix` → 「保存服务商」`providers-add-submit`。
  调用 `provider_upsert({ provider: ProviderInput })`；成功后用**后端返回的 Provider** 回显并 `invalidateQueries(qk.providers)`。
  本地校验：必填、id `/^[a-z][a-z0-9-]*$/`、与现有 id 去重、`base_url` `^https?://`。
- 「添加密钥」`credentials-add-open` → 表单：`credentials-add-id` / `-label` / `-provider`（下拉）
  / `-secret`（`type=password`，**绝不回显**）/ `-quotaLimit` / `-currency` / `-window` → 「保存密钥」`credentials-add-submit`。
  顺序：`credential_upsert({ credential })` →（若填了 secret）`credential_set_secret({ id, plaintext })`。
  **明文只进密钥仓库**：不进 state 之外的任何地方、不进日志、不进错误消息；成功后清空输入框。

### 3.2 `/models` 模型
- 「添加模型」`models-add-open` → `models-add-id` / `-aliases`（逗号分隔）/ `-group` / `-order` / `-expose`（开关）
  → 「保存模型」`models-add-submit` → `model_upsert({ model })`；成功后回显后端返回值。

### 3.3 `/channels` 线路（= 用户说的"线路1/线路2"）

> ⚠️ **这一节已整体过期**（第 315 轮，[docs/49]）：`/channels`（导航标签「套餐与线路」）**已被删除**，
> 下面这串 `channels-add-*` testid **全部不存在了**（`channels-add-model` 更早就已不存在）。
> 现在建线路只有一条路：**加服务商 → 扫描上游目录 → 建全部线路**（`provider_lines_create_all`），
> 之后用滑块启停；能力由扫描从上游原文自动认领（`adopt_upstream_hints`）。
> **新增定价**已搬到成本中心（`pricing-add-*`，见 [docs/49 §2](49-channels-page-removed.md)）。
> 本节保留原文，作为当时流程的记录。

- 「添加线路」`channels-add-open` → 表单（顺序即用户描述的顺序）：
  `channels-add-provider`（下拉，来自 `provider_list`）→ `channels-add-credential`（下拉，**按 provider 过滤**）
  → `channels-add-model`（下拉：已有 `model_list` + 允许直接输入新名字）→ `channels-add-upstreamModel`
  → `channels-add-lane`（下拉：直连 / 某订阅组 / 某固定节点，来自 `lane_list`；默认直连）
  → 能力：`channels-add-vision` / `-tools` / `-reasoning` / `-contextTokens` / `-modalities`
  → 计费：`channels-add-price-input` / `-price-output` / `-price-cached`（单位：美元/百万 token；可留空 = 不计价）
  → `channels-add-priority` / `-weight` → 「保存线路」`channels-add-submit`。
- 调用 `channel_upsert({ channel })`；若计费非空再调 `pricing_upsert({ entry })`（按 `match.model`，可带 `match.provider`）。
- 本地校验：服务商/凭据/模型/上游模型名必填；`contextTokens` 非负整数；价格非负数字。
- 校验失败（含能力不一致）时**原样显示后端错误**（TA 码 + details）。

### 3.4 `/policies` 路由
- 「添加路由」`policies-add-open` → `policies-add-model`（暴露哪个对外模型）→ `policies-add-channels`（多选，
  **只列 `model` 等于所选模型的线路**）→ 粘性：`policies-add-sticky`（`pin_sticky` 指定优先线路）
  → 失败切换 `policies-add-failover` → `policies-add-priority` → 「保存路由」`policies-add-submit`。
- 选中 ≥2 条线路时**即时**显示它们的能力对比；不一致 → 显示差异并禁用提交。
- 调用 `policy_upsert({ policy })`，`match.model_matches = [所选模型]`，`action.use_channels = 选中线路 id`。

## 4. 验收（主管在干净快照上跑）

1. 四个向导各自**红先绿后**（先写"控件存在 + 真输入 + 真的调用了该命令 + 入参形状正确 + 成功后显示后端真值"的失败断言）。
2. 门禁全绿：`tsc` / `vitest` / `cargo fmt|check|clippy|test`。
3. 真机 E2E（`scripts/e2e/smoke.mjs` 扩步骤）：在真窗口里依次完成
   **加服务商 → 加密钥 → 加模型 → 加线路（含能力与计费）→ 加路由（两条线路）**，
   每步断言"列表出现 + 库里 `config` 表真的落库"；并**反向验证**：把两条线路的能力改成不一致，
   保存路由必须**被拒绝**且错误里点出差异字段。
4. 安装包重出 + A3/A4 在同一发布二进制上复跑。

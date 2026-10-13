/**
 * `/providers` 的**添加服务商**（[docs/17 §3.1] 的「高级添加」单步对话框）的纯逻辑：
 * 草稿 → 本地预检 → 命令入参。
 *
 * 第 80 轮把这里的**凭据草稿**（`CredentialDraft` / `credentialPayload` / 额度三件套）
 * 整段删掉了：原来它们只服务「添加密钥」对话框，而那个对话框已下线
 * （[docs/28 §1] 硬规则 3：**一个服务商一把密钥**；要两个账号 → 再加一条服务商、取不同名字）。
 * 密钥的写入口只剩两步向导第 1 步与详情页「密钥」块，都不需要"另造一份凭据 id"的能力。
 *
 * 为什么单独一层（而不是全塞进 `.tsx`）：这个对话框最容易写错的三件事都能在这里被单测钉死 ——
 *
 * 1. **本地预检不过就一次都不发**：`validateProviderDraft` 返回非空数组时，调用方直接 `return`
 *    （与设置页 `config_validate`、线路页 `channelDraftIssues` 同一纪律）。
 * 2. **不替后端编默认值**：`ProviderInput` 的可选字段在 Rust 侧都有 `#[serde(default)]`
 *    （`crates/ta-core/src/domain/mod.rs`）—— 用户没填的，载荷里**一个都不放**。
 * 3. **重名在本地就挡住**：显示名称是用户区分"同一家的不同账号"的抓手（[docs/28 §1]）。
 *
 * 文案一律用既有的 i18n 键（`wizard.*` / `providers.wizard.nameTaken`）；本层只返回**键**，
 * 不返回句子。
 */

import type { AuthConfig, AuthScheme, Protocol, ProviderInput } from "@/shared/bindings";
import { optional } from "./editor";
import type { I18nKey } from "@/shared/i18n";

// ================================================================ 运行时常量（契约里只有编译期联合类型）

/**
 * `AuthScheme` 的运行时镜像（serde 线上表示）。
 *
 * 与 `./protocols.ts` 的 `PROTOCOLS` 同一性质：契约里是联合类型，下拉框要数组。
 * `Protocol` 有 `protocolList.guard.test.ts` 按 serde 表示双向比对；`AuthScheme` 目前没有同款守卫，
 * 所以这里**逐字**按 `crates/ta-core/src/domain/mod.rs` 的 `enum AuthScheme` 写，改 Rust 时同步。
 */
export const AUTH_SCHEMES = [
  "bearer",
  "header",
  "query",
  "none",
] as const satisfies readonly AuthScheme[];

/** Rust `impl Default for AuthConfig` 的 `scheme`：没填鉴权细节时**什么都不带**（走这个默认）。 */
export const DEFAULT_AUTH_SCHEME: AuthScheme = "bearer";

/** 本地预检能点名的一条错。`message` 是既有 i18n 键，页面直接 `t(message)`。 */
export type WizardIssue = { field: string; message: I18nKey };

/** [docs/17 §3.1] 的 id 规则：小写字母开头，其余只许小写字母 / 数字 / 短横线。 */
const ID_PATTERN = /^[a-z][a-z0-9-]*$/;

/** [docs/17 §3.1] 的 base_url 规则。 */
const URL_PATTERN = /^https?:\/\//;

// ================================================================ 添加服务商

/** 表单草稿。`protocol` / `authScheme` 是下拉，永远有值（不是"空就是没选"）。 */
export type ProviderDraft = {
  id: string;
  name: string;
  protocol: Protocol;
  baseUrl: string;
  authScheme: AuthScheme;
  authHeader: string;
  authPrefix: string;
  /**
   * [docs/29 §2.2] 代理（值可能是标记：`direct:` / `auto:<订阅>` / 空串 / 线路 id）。
   * `undefined` = 这次不动它（高级对话框不暴露代理）；`""` = 显式清空（走继承）。
   */
  lane?: string;
};

export const EMPTY_PROVIDER_DRAFT: ProviderDraft = {
  id: "",
  name: "",
  protocol: "openai",
  baseUrl: "",
  authScheme: DEFAULT_AUTH_SCHEME,
  authHeader: "",
  authPrefix: "",
};

/**
 * 现有的服务商：**id 与显示名称各自去重**（[docs/17 §3.1] + [docs/28 §1]）。
 *
 * 形状与两步向导的 `ExistingProviders`（`providerWizardModel.ts`）同义，只是这里不区分
 * "本会话刚落库的那一份"——高级添加对话框不落库，没有这一档。
 */
export type ExistingProviderRefs = {
  ids: readonly string[];
  names: readonly string[];
};

/**
 * 必填 / id 形状 / 与现有 `provider_list` 的 id 去重 / **名称重名** / `base_url` 协议头。
 *
 * 顺序 = 用户填写的顺序；返回空数组才允许发命令。
 *
 * **名称为什么也要去重**（[docs/28 §1] 硬规则 3）：一个服务商一把密钥，所以"同一家的第二个
 * 账号"的唯一表达方式是**第二条服务商**，而用户正是靠名字区分它们的（owner 原话："取不同名字"）。
 * 两条同名记录在界面上分不出谁是谁 —— 这不是后端该拒的错，是本地就该拦下的输入。
 * 文案复用两步向导那一条 `providers.wizard.nameTaken`（不另造句子、不自己编 TA 码）。
 */
export function validateProviderDraft(
  draft: ProviderDraft,
  existing: ExistingProviderRefs,
): WizardIssue[] {
  const issues: WizardIssue[] = [];
  const id = draft.id.trim();

  if (id === "") issues.push({ field: "id", message: "wizard.required" });
  else if (!ID_PATTERN.test(id)) issues.push({ field: "id", message: "wizard.invalidId" });
  else if (existing.ids.includes(id)) issues.push({ field: "id", message: "wizard.duplicateId" });

  const name = draft.name.trim();
  if (name === "") issues.push({ field: "name", message: "wizard.required" });
  else if (existing.names.includes(name)) {
    issues.push({ field: "name", message: "providers.wizard.nameTaken" });
  }

  const baseUrl = draft.baseUrl.trim();
  if (baseUrl === "") issues.push({ field: "baseUrl", message: "wizard.required" });
  else if (!URL_PATTERN.test(baseUrl)) issues.push({ field: "baseUrl", message: "wizard.invalidUrl" });

  return issues;
}

/**
 * 草稿 → `provider_upsert` 的载荷：**只带用户真的给了的字段**，其余交 Rust 的 `#[serde(default)]`。
 *
 * `auth` 只在用户**真的填了**鉴权细节（头名 / 值前缀）或显式选了非默认方式时才出现 ——
 * 否则连 `auth` 这个键都不发（`Provider.auth` 有 `serde(default)`，默认 Bearer + 空值）。
 *
 * **没有 API Key 输入框**，载荷里也不会有任何密钥字段：密钥属于凭据域
 * （由「添加密钥」的 `credential_set_secret` 写入密钥仓库）。
 */
export function providerPayload(draft: ProviderDraft): ProviderInput {
  const payload: ProviderInput = {
    id: draft.id.trim(),
    name: draft.name.trim(),
    protocol: draft.protocol,
    base_url: draft.baseUrl.trim(),
  };

  /**
   * [docs/29 §2.2] 代理（用户第 112 轮：向导里要能为服务商选线路，且能看到
   * 直连 / 自动 / 所有已配置线路）。值是**标记**（`direct:` / `auto:<订阅>`）或线路 id，
   * 由后端 `provider_upsert` 解析成真实线路 —— 前端不建线路、不编 id。
   *
   * `undefined` = 这次不动它（高级对话框不暴露代理）；`""` = 显式清空（走继承链）。
   */
  if (draft.lane !== undefined) {
    payload.lane = optional(draft.lane);
  }

  const header = draft.authHeader.trim();
  // 值前缀的**尾随空格有意义**（`Bearer ` 与 `Bearer` 是两个不同的前缀）→ 只 trim 判断空，值原样带
  const prefix = draft.authPrefix;
  const touched = header !== "" || prefix.trim() !== "" || draft.authScheme !== DEFAULT_AUTH_SCHEME;

  if (touched) {
    const auth: AuthConfig = { scheme: draft.authScheme };
    if (header !== "") auth.header = header;
    if (prefix.trim() !== "") auth.prefix = prefix;
    payload.auth = auth;
  }

  return payload;
}

// ================================================================ 凭据：这一层也没有了
//
// 这里原来住着「添加密钥」的整套纯逻辑：`CredentialDraft` / `EMPTY_CREDENTIAL_DRAFT` /
// `sanitizeAmount` / `parseOptionalAmount` / `credentialSecretRef` / `validateCredentialDraft` /
// `credentialPayload`（带 `quota: { limit, currency, window }`）。
// 第 80 轮连同那个对话框一起删掉（[docs/28 §1] 硬规则 3：**一个服务商一把密钥**）。
//
// 为什么不留作"以后可能用得上"：它们唯一的用途是"让用户自己给凭据起 id、自己选挂在哪个
// 服务商下"—— 也就是"同一家挂多份凭据"这条路。留着这段代码，下一个人接回一个入口就能复活
// 那个被 owner 否掉的模型（这正是本轮要拆掉的东西）。要用两个账号 → 加两条服务商、取不同名字。
//
// 现在的密钥写入口（都不在这里，也不需要这些函数）：
// · 两步向导第 1 步：`./providerWizardModel.ts` 的 `defaultCredentialId` / `defaultCredentialPayload`
//   （凭据 id 恒为 `<provider>-default`）+ `credential_set_secret`；
// · 详情页「密钥」块：`./detail.tsx` 的 `providers-edit-secret`（留空 = 不改）；
// · 额度窗口：`./quotaWindows.ts`（挂在**那一把**凭据上，编辑口在详情页）。


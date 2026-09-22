/**
 * `/providers` 的**两个添加向导**（[docs/17 §3.1]）的纯逻辑：草稿 → 本地预检 → 命令入参。
 *
 * 为什么单独一层（而不是全塞进 `.tsx`）：这两个向导最容易写错的三件事都能在这里被单测钉死 ——
 *
 * 1. **本地预检不过就一次都不发**：`validate*` 返回非空数组时，调用方直接 `return`
 *    （与设置页 `config_validate`、线路页 `channelDraftIssues` 同一纪律）。
 * 2. **不替后端编默认值**：`ProviderInput` 的可选字段、`Credential` 的
 *    `quota`/`guard`/`tags`/`enabled` 在 Rust 侧都有 `#[serde(default)]`
 *    （`crates/ta-core/src/domain/mod.rs`）—— 用户没填的，载荷里**一个都不放**。
 * 3. **明文不进载荷**：`credential.secret` 只能是 `${secret:<条目名>}` **引用**，
 *    真正的值由 `credential_set_secret` 单独写进密钥仓库（[docs/10 §2.4] 的引用语法）。
 *
 * 文案一律用既有的 i18n 键（`wizard.*`）；本层只返回**键**，不返回句子。
 */

import type {
  AuthConfig,
  AuthScheme,
  Credential,
  Protocol,
  ProviderInput,
  QuotaWindow,
} from "@/shared/bindings";
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

/** `QuotaWindow` 的运行时镜像（同上）。 */
export const QUOTA_WINDOWS = ["hour", "day", "month", "none"] as const satisfies readonly QuotaWindow[];

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
 * 必填 / id 形状 / 与现有 `provider_list` 去重 / `base_url` 协议头。
 *
 * 顺序 = 用户填写的顺序；返回空数组才允许发命令。
 */
export function validateProviderDraft(
  draft: ProviderDraft,
  existingIds: readonly string[],
): WizardIssue[] {
  const issues: WizardIssue[] = [];
  const id = draft.id.trim();

  if (id === "") issues.push({ field: "id", message: "wizard.required" });
  else if (!ID_PATTERN.test(id)) issues.push({ field: "id", message: "wizard.invalidId" });
  else if (existingIds.includes(id)) issues.push({ field: "id", message: "wizard.duplicateId" });

  if (draft.name.trim() === "") issues.push({ field: "name", message: "wizard.required" });

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

// ================================================================ 添加密钥

export type CredentialDraft = {
  id: string;
  label: string;
  provider: string;
  /** **明文**，只活在这个草稿与 `credential_set_secret` 的入参里；永不进日志 / 错误 / 载荷。 */
  secret: string;
  quotaLimit: string;
  currency: string;
  window: QuotaWindow;
};

export const EMPTY_CREDENTIAL_DRAFT: CredentialDraft = {
  id: "",
  label: "",
  provider: "",
  secret: "",
  quotaLimit: "",
  currency: "",
  window: "month",
};

/**
 * 输入即净化：只让「数字 + 至多一个小数点（且前面已有数字）」进来。
 *
 * 这样 `Number()` 永远不会拿到 `NaN`（`"1.2.3"` / `"."` 这些进不来），
 * 本地就不需要为"额度必须是数字"另造一条 i18n 文案 ——
 * 与线路页的 `intInputFilter` 同一手法。
 */
export function sanitizeAmount(raw: string): string {
  let out = "";
  let seenDot = false;
  for (const ch of raw) {
    if (ch >= "0" && ch <= "9") out += ch;
    else if (ch === "." && !seenDot && out !== "") {
      out += ch;
      seenDot = true;
    }
  }
  return out;
}

/** 空串 = **没填**（返回 `undefined`）；否则是净化的非负数。 */
export function parseOptionalAmount(raw: string): number | undefined {
  const text = raw.trim();
  if (text === "") return undefined;
  const value = Number(text);
  return Number.isFinite(value) && value >= 0 ? value : undefined;
}

/** 凭据的密钥条目名：`${secret:<id>}`。同一份凭据重填 = 覆盖同一个条目，不会留孤儿。 */
export function credentialSecretRef(id: string): string {
  return `\${secret:${id}}`;
}

/** 必填 / id 形状 / 与现有 `credential_list` 去重。**secret 不是必填**（可以以后再写密钥）。 */
export function validateCredentialDraft(
  draft: CredentialDraft,
  existingIds: readonly string[],
): WizardIssue[] {
  const issues: WizardIssue[] = [];
  const id = draft.id.trim();

  if (id === "") issues.push({ field: "id", message: "wizard.required" });
  else if (!ID_PATTERN.test(id)) issues.push({ field: "id", message: "wizard.invalidId" });
  else if (existingIds.includes(id)) issues.push({ field: "id", message: "wizard.duplicateId" });

  if (draft.label.trim() === "") issues.push({ field: "label", message: "wizard.required" });
  if (draft.provider === "") issues.push({ field: "provider", message: "wizard.required" });

  return issues;
}

/**
 * 草稿 → `credential_upsert` 的载荷。
 *
 * - `secret` 恒为**引用** `${secret:<id>}`：明文永远不在这里（写明文是 `credential_set_secret` 的事）；
 * - `quota` **只在用户真的填了上限时**才出现（`metric` 未声明 + 无上限没有意义）；
 *   币种留空就不带（Rust `default_currency()` = `"USD"`），窗口是下拉里用户看到的值，照带。
 *
 * TS 侧的 `Credential` 把 `quota`/`guard`/`tags`/`enabled` 写成了必填（读取形状），
 * 而 Rust 侧它们都有默认值 —— 与 `channelPayload` 同一手法：先拼 `Record`，最后断言成契约类型；
 * 单测（`providers.test.tsx` ⑦）钉住"这些字段一个都不在载荷里"。
 */
export function credentialPayload(draft: CredentialDraft): Credential {
  const id = draft.id.trim();
  const payload: Record<string, unknown> = {
    id,
    provider: draft.provider,
    label: draft.label.trim(),
    secret: credentialSecretRef(id),
  };

  const limit = parseOptionalAmount(draft.quotaLimit);
  if (limit !== undefined) {
    const quota: Record<string, unknown> = { limit, window: draft.window };
    const currency = draft.currency.trim();
    if (currency !== "") quota["currency"] = currency;
    payload["quota"] = quota;
  }

  return payload as unknown as Credential;
}

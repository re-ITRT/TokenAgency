/**
 * `/providers` **两步向导**（[docs/19 §1]）的纯逻辑与草稿 store。
 *
 * 分三层（与 `./wizard.ts` 同一纪律：能纯函数化的都纯函数化，好在单测里钉住）：
 *
 * 1. **第 1 步的校验与载荷**：显示名称去重（对现有 `name` **与** `id`）、base_url 形状、
 *    由名称派生的 provider id、凭据（`<provider>-default`）的载荷。明文密钥**不进这里**
 *    —— 它只从组件状态直接交给 `credential_set_secret`。
 * 2. **模块级草稿 store**：`/network?from=provider-wizard` 来回一趟会卸载页面，
 *    所以草稿放在页面 state 之外（[docs/19 §1.4] 明确允许并**要求**有一测试证明它保留）。
 *    store 里只有这一步的输入，没有任何密钥以外的敏感数据；明文密钥**从不清日志、从不上屏**。
 * 3. **第 2 步的派生**：哪些模型属于这个服务商（`channel_matrix` 的 cell + 凭据归属）、
 *    每一行当前的 lane，以及 `ProbeResult` 该说哪句人话。
 *
 * 为什么"名字 → id"是派生的而不是让用户填：本轮的步骤 1 只有 4 个字段（[docs/19 §1.1]），
 * 用户给的是"显示名称"；`provider.id` 是引用完整性要用的稳定标识，所以由名称**确定性**派生
 * （同一名称永远得到同一个 id，不会每次点下一步都新建一条），并在界面上如实显示派生结果。
 */

import type {
  Credential,
  ImportPreview,
  LaneId,
  LanePickOption,
  MatrixView,
  ProbeResult,
  Protocol,
  ProviderInput,
} from "@/shared/bindings";

import { providerPayload, type ProviderDraft, type WizardIssue } from "./wizard";

// ================================================================ 第 1 步：草稿与校验

/** 步骤 1 的四个字段（[docs/19 §1.1]）。`key` 是**明文**：只进密钥仓库，不进任何载荷。 */
export type Step1Draft = {
  name: string;
  protocol: Protocol;
  baseUrl: string;
  /** 明文 API Key。只允许出现在 `credential_set_secret` 的入参里。 */
  key: string;
};

export const EMPTY_STEP1_DRAFT: Step1Draft = {
  name: "",
  protocol: "openai",
  baseUrl: "",
  key: "",
};

/** [docs/17 §3.1] 的 id 规则（`provider.id` 必须满足它，否则后端会拒）。 */
const ID_PATTERN = /^[a-z][a-z0-9-]*$/;

/** [docs/17 §3.1] 的 base_url 规则。 */
const URL_PATTERN = /^https?:\/\//;

/** 现有的服务商（`provider_list` 的并集形状：`id` 与 `name` 两边都有）。 */
export type ExistingProviders = { ids: readonly string[]; names: readonly string[] };

/**
 * 本会话**已经真的落库**的那一份（`pw-next` 或落库式 `pw-test` 成功后写入）。
 *
 * 为什么要记住 `name` 与 `secretStored`：`pw-test` 按 [docs/19 §1.3] 是**先落库再探测**的，
 * 落库之后 (a) 明文 key 立刻从草稿里抹掉（[docs/19 §1.1] 的纪律：明文只进密钥仓库），
 * (b) `provider_list` 刷新后**自己刚建的那一份**出现在清单里。不把这两件事告诉校验，
 * 用户按「填名称/地址/key → 测试连接 → 下一步」这条**明文路径**走就会被自己刚建的东西挡住
 * （"名称已经被占用" + "密钥必填"），永远进不了第 2 步 —— 这是真机上抓到过的缺陷。
 */
export type SavedInSession = { providerId: string; name: string; secretStored: boolean };

/** 把"本会话自己落库的那一份"从现有清单里摘掉：它不是重名，也不是别人的东西。 */
function excludeOwnEntry(existing: ExistingProviders, saved?: SavedInSession): ExistingProviders {
  if (saved === undefined) return existing;
  return {
    ids: existing.ids.filter((id) => id !== saved.providerId),
    names: existing.names.filter((name) => name !== saved.name),
  };
}

/**
 * 名称是否被占用。
 *
 * 与**现有 `name` 及 `id` 都去重**（[docs/19 §1.1]）：名字直接撞上某个 id 时，
 * 用户看到的是"这个名字已经有人用了"，而不是一个只有懂 id 规则的人才看得懂的报错。
 *
 * `saved` = 本会话已经落库的那一份：**它自己不算重名**（见 [`SavedInSession`]）。
 */
export function isNameTaken(
  name: string,
  existing: ExistingProviders,
  saved?: SavedInSession,
): boolean {
  const value = name.trim();
  const rest = excludeOwnEntry(existing, saved);
  return value !== "" && (rest.names.includes(value) || rest.ids.includes(value));
}

/** 名称 → id 的 slug：小写、非 `[a-z0-9]` 一律变短横线、去首尾与重复的横线。 */
function slugOf(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .replace(/-{2,}/g, "-");
}

/**
 * 名称里一个 `[a-z0-9]` 都没有时（例如纯中文名）的兜底：**确定性**的短哈希。
 *
 * 确定性很重要：随机 id 会让"点两次下一步"变成两条不同的服务商。
 */
function shortHash(text: string): string {
  let hash = 0x811c9dc5;
  for (const ch of text) {
    hash ^= ch.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(36);
}

/** 名称 → 不与现有 id 冲突的 provider id（派生结果会显示在界面上，用户看得见）。 */
export function deriveProviderId(name: string, existingIds: readonly string[]): string {
  const slug = slugOf(name);
  const base = ID_PATTERN.test(slug) ? slug : `p-${shortHash(name.trim())}`;
  let id = base;
  let suffix = 2;
  while (existingIds.includes(id)) {
    id = `${base}-${suffix}`;
    suffix += 1;
  }
  return id;
}

/** 本步骤的本地预检。返回空数组才允许发命令。 */
export function validateStep1(
  draft: Step1Draft,
  existing: ExistingProviders,
  saved?: SavedInSession,
): WizardIssue[] {
  const issues: WizardIssue[] = [];
  const name = draft.name.trim();

  if (name === "") issues.push({ field: "name", message: "wizard.required" });
  else if (isNameTaken(name, existing, saved)) {
    issues.push({ field: "name", message: "providers.wizard.nameTaken" });
  }

  const baseUrl = draft.baseUrl.trim();
  if (baseUrl === "") issues.push({ field: "baseUrl", message: "wizard.required" });
  else if (!URL_PATTERN.test(baseUrl)) issues.push({ field: "baseUrl", message: "wizard.invalidUrl" });

  // 密钥必填（[docs/19 §1.1] 的"必填"）：没有密钥的服务商在真机上连不通，
  // 与其让用户以为配好了，不如在这里说清楚。
  //
  // **例外**：本会话已经把明文写进密钥仓库了（`pw-test` 落库后明文会被抹掉），
  // 这时候空 key 是"沿用已存的那把"，不是"没填"—— 再要一次就是让用户重打一遍。
  if (draft.key === "" && saved?.secretStored !== true) {
    issues.push({ field: "key", message: "wizard.required" });
  }

  return issues;
}

/** 只把"名称被占用"这一条单独取出来：它在用户刚敲完名字时就该出现（本地即时提示）。 */
export function nameTakenIssue(
  draft: Step1Draft,
  existing: ExistingProviders,
  saved?: SavedInSession,
): WizardIssue[] {
  return isNameTaken(draft.name, existing, saved)
    ? [{ field: "name", message: "providers.wizard.nameTaken" }]
    : [];
}

/** 草稿 → `provider_upsert` 的载荷（复用 `./wizard.ts` 的构造器：不替后端编默认值）。 */
export function step1ProviderPayload(draft: Step1Draft, providerId: string): ProviderInput {
  const providerDraft: ProviderDraft = {
    id: providerId,
    name: draft.name.trim(),
    protocol: draft.protocol,
    baseUrl: draft.baseUrl.trim(),
    // 步骤 1 不暴露鉴权细节（[docs/19 §1.1] 只有 4 个字段）→ 一个都不带，
    // 交给 Rust 的 `#[serde(default)]`（见 `serdeDefault.guard.test.ts`）。
    authScheme: "bearer",
    authHeader: "",
    authPrefix: "",
  };
  return providerPayload(providerDraft);
}

/** 凭据 id：`<provider>-default`（[docs/19 §1.3] 指定的那一份）。 */
export function defaultCredentialId(providerId: string): string {
  return `${providerId}-default`;
}

/**
 * 草稿 → `credential_upsert` 的载荷。
 *
 * - `secret` 恒为 `${secret:<id>}` **引用**，明文永远不在这里；
 * - `quota` / `guard` / `tags` / `enabled` 在 Rust 侧都有 `serde(default)`，**一个都不带**；
 * - `label` 用用户给的显示名称（用户数据原样，不另造文案）。
 */
export function defaultCredentialPayload(providerId: string, label: string): Credential {
  const id = defaultCredentialId(providerId);
  return {
    id,
    provider: providerId,
    label: label.trim(),
    secret: `\${secret:${id}}`,
  } as unknown as Credential;
}

// ================================================================ 模块级草稿 store

/**
 * 向导会话。**活在模块里**，所以 `/network` 来回一趟（页面卸载再挂载）也丢不了草稿
 * —— 这是 [docs/19 §1.4] 的硬要求，不是顺手缓存。
 */
export type WizardSession = {
  open: boolean;
  step: 1 | 2;
  draft: Step1Draft;
  /** 已经真的落库的那一份（`pw-next` / 落库式 `pw-test` 成功后写入）。 */
  saved: SavedInSession | undefined;
};

function freshSession(): WizardSession {
  return { open: false, step: 1, draft: { ...EMPTY_STEP1_DRAFT }, saved: undefined };
}

let session: WizardSession = freshSession();

export function readWizardSession(): WizardSession {
  return session;
}

/** 打补丁式写入（各调用方只改自己那部分，不互相覆盖）。 */
export function writeWizardSession(patch: Partial<WizardSession>): WizardSession {
  session = { ...session, ...patch };
  return session;
}

/** 关掉向导时清干净：下一次打开不带上一次的残值（**也顺手清掉内存里的明文密钥**）。 */
export function resetWizardSession(): void {
  session = freshSession();
}

// ================================================================ 第 2 步：派生

/** 第 2 步"已添加的模型"里的一行。 */
export type AddedModelRow = {
  modelId: string;
  upstreamModel: string;
  /** 该模型当前生效的 lane（`null` = 没有显式 lane）。 */
  laneId: LaneId | null;
  channelCount: number;
};

/**
 * "这个服务商下有哪些模型"：`channel_matrix` 的 cell 把模型挂到**凭据**上，
 * 凭据再挂到服务商上，所以这里按"属于该服务商的凭据 id 集合"过滤。
 *
 * **不猜**：矩阵里没有的模型就不会出现在这张表里；顺序保持后端给的顺序。
 */
export function addedModelsOf(
  matrix: MatrixView | undefined,
  credentialIds: ReadonlySet<string>,
): AddedModelRow[] {
  if (matrix === undefined) return [];
  const order: string[] = [];
  const byModel = new Map<string, AddedModelRow>();

  for (const cell of matrix.cells ?? []) {
    if (!credentialIds.has(cell.credential_id)) continue;
    const existing = byModel.get(cell.model_id);
    if (existing === undefined) {
      order.push(cell.model_id);
      byModel.set(cell.model_id, {
        modelId: cell.model_id,
        upstreamModel: cell.upstream_model ?? "",
        laneId: cell.lane_id ?? null,
        channelCount: 1,
      });
      continue;
    }
    existing.channelCount += 1;
    if (existing.upstreamModel === "" && cell.upstream_model != null) {
      existing.upstreamModel = cell.upstream_model;
    }
    if (existing.laneId === null && cell.lane_id != null) existing.laneId = cell.lane_id;
  }

  return order.map((id) => byModel.get(id)!);
}

/**
 * `config_get().net.default_lane` —— 也就是 [docs/19 §1.4] 里用户在 `/network` 上
 * 挑的"新建服务商的默认代理"。第 1 步没选（字段缺失 / null）就是 `null` = 直连。
 *
 * 入参写成 `unknown`：真机与测试给的都是**部分**配置树，页面只读这一条路径。
 */
export function defaultLaneOf(configDoc: unknown): LaneId | null {
  if (typeof configDoc !== "object" || configDoc === null) return null;
  const net = (configDoc as Record<string, unknown>)["net"];
  if (typeof net !== "object" || net === null) return null;
  const lane = (net as Record<string, unknown>)["default_lane"];
  return typeof lane === "string" && lane !== "" ? lane : null;
}

/** 一个下拉项：`value` 是 `""`（= 未选 = 直连）或一个真实 lane id。 */
export type LaneChoice = { value: string; label: string };

/**
 * 网络代理下拉的选项：**第一条永远是"直连"（`value: ""`）**，其余来自
 * `lane_pick_options`（后端给的分组清单）。分组标题不丢信息也不重复：这里把
 * `组名 · 线路名` 拼起来（同名线路在不同组里也能分清）。
 */
export function laneChoices(
  options: readonly LanePickOption[],
  directLabel: string,
): LaneChoice[] {
  const out: LaneChoice[] = [{ value: "", label: directLabel }];
  for (const group of options) {
    for (const option of group.options) {
      out.push({
        value: option.lane_id,
        label: group.group === "" ? option.name : `${group.group} · ${option.name}`,
      });
    }
  }
  return out;
}

/** lane → 下拉的 value（未设 = `""` = 直连）。 */
export function laneValue(lane: LaneId | null): string {
  return lane ?? "";
}

/** 下拉的 value → `ModelPatch.lane`（未选 = `null` = 直连，[docs/19 §1.5]）。 */
export function lanePatch(value: string): LaneId | null {
  return value === "" ? null : value;
}

// ================================================================ 探测结果 → 人话

/**
 * `ProbeResult` 该归到哪一档（[docs/19 §1.1]）。
 *
 * `status=404 && probe="models"` **不是**"连不通"：上游只是没有 `/models` 端点。
 * 这一条是主管明确要求的裁决，所以它是一个可被单测钉住的纯函数，不是组件里的 if。
 */
export type ProbeOutcome = "ok" | "no-endpoint" | "failed";

export function probeOutcome(result: ProbeResult): ProbeOutcome {
  if (result.ok) return "ok";
  if (result.status === 404 && result.probe === "models") return "no-endpoint";
  return "failed";
}

/**
 * 探测失败（命令没抛错，但 `ok=false`）时显示什么：**后端的原话**。
 * 有 `note` 就 `HTTP <status> <note>`，没有就只报状态码 —— 不编"连接失败"这种句子。
 */
export function probeFailureText(result: ProbeResult): string {
  const note = typeof result.note === "string" && result.note !== "" ? result.note : undefined;
  return note === undefined ? `HTTP ${result.status}` : `HTTP ${result.status} ${note}`;
}

// ================================================================ 扫描结果的勾选默认值

/** 「全选」的语义：所有 `exists === false` 的行（[docs/19 §1.2]）。 */
export function selectAllIds(preview: ImportPreview | undefined): string[] {
  if (preview === undefined) return [];
  return preview.models.filter((m) => !m.exists).map((m) => m.suggested_id);
}

/**
 * 服务商详情页「查看 + 编辑一体」的**纯逻辑**（[docs/23]）——没有 JSX、没有 React、没有 IPC。
 *
 * ## 为什么要有这一层
 *
 * 用户第 75 轮的原话：「服务商编辑页面不走下一步下一步，直接把所有信息在点开服务商之后提供
 * 可视化编辑界面，相当于编辑和查看一体了」。于是详情页自己就是编辑器，草稿 → 校验 → 载荷
 * 这三件事必须能被单测钉住 —— 尤其是**载荷**：写错一个键就会在真机上把用户的配置改坏。
 *
 * ## 一个真实的数据丢失缺陷（这层要根治的）
 *
 * `provider_upsert` 在 Rust 里是**整条覆盖写**（`catalog.rs`：`*slot = provider.clone()`），
 * 而 `Provider` 的可选字段都有 `#[serde(default)]`。所以载荷里**少一个键**，就等于把那个字段
 * 重置成默认值。第 72 轮的两步向导编辑模式正是这样：它只发 4 个字段（id / name / protocol /
 * base_url，见 `providerWizardModel.step1ProviderPayload`），于是"改个名字"会**静默抹掉**
 * 这家服务商的鉴权头、超时、TLS、附加头、默认线路。
 *
 * 所以这里的铁律是：**载荷以「后端返回的原始记录」为底，只覆盖用户真的改过的那些键**
 * （[`buildProviderPayload`]）：
 *
 * 1. 原始记录里我们没暴露过的键（`#[serde(flatten)] extra` 里的自定义字段）**原样带回去**，
 *    一个都不丢；
 * 2. 只读视图专有的键（`status` / `success_rate_24h`…）**剔除**：它们不是配置，带回去会被
 *    当成用户自定义字段存进 `extra`；
 * 3. 用户清空的 `Option` 字段发**显式 `null`**（`serde` 把 `null` 解成 `None`），
 *    而不是"不发这个键"（不发 = 默认值，那不是"清空"）。
 *
 * ## 只读视图不许写
 *
 * Rust 的 `provider_list` 返回完整 `Provider`，但契约与浏览器 mock 给的是**视图**
 * （没有 `auth` / `timeout` / `tls` / `headers` / `passthrough` / `lane`）。拿到视图时
 * **连接配置根本没读到** —— 这时候让它"保存"就是用默认值覆盖线上的真配置。
 * [`canEdit`] 把这条判据写成一个函数，页面据此只读并说明原因（不排空占位、不假装能改）。
 */

import type { LaneChoiceGroup, ProviderInput, Protocol } from "@/shared/bindings";
// [docs/52] 读后端原始对象的唯一实现（原先本文件自己写了一份 asRow）
import type { Row } from "@/shared/row";
import { asRow } from "@/shared/row";

import { proxyChoices, type HeaderPair, type ProviderRow, type ProxyChoice } from "./model";
import type { WizardIssue } from "./wizard";

/** 后端**只读视图**专有的键：永远不会出现在配置里，回写时必须剔除。 */
export const VIEW_ONLY_KEYS = [
  "status",
  "success_rate_24h",
  "avg_latency_ms",
  "channel_count",
  "provider_name",
] as const;

/** 数值字段一律以字符串形态存在草稿里（输入框就是字符串），转换只在载荷里做一次。 */
export type ProviderEditDraft = {
  name: string;
  /** `Protocol` 的线上值；契约外的取值也**原样**留在草稿里（下拉会为它补一行）。 */
  protocol: string;
  baseUrl: string;
  enabled: boolean;
  /** `""` = 不指定（provider 级 lane 为空，按五级继承链落到默认线路或直连）。 */
  lane: string;
  modelPrefix: string;

  authScheme: string;
  authHeader: string;
  authPrefix: string;

  timeoutConnectS: string;
  timeoutReadS: string;
  timeoutTotalS: string;

  tlsInsecure: boolean;
  tlsExtraCa: string;
  tlsMinVersion: string;

  onHeaderConflict: string;
  /** `""` = 不设（`Option` 为 `None`，交给内核的默认透传行为）。 */
  passthroughHeaderMode: string;
  passthroughBody: string;

  headers: readonly HeaderPair[];

  /** **明文**密钥：只允许出现在 `credential_set_secret` 的入参里，永不进载荷 / 日志 / 错误。 */
  key: string;
};

/** Rust `TimeoutConfig::default()`（`domain/mod.rs`）：草稿里读不到时如实回落到它。 */
const DEFAULT_CONNECT_S = "10";
const DEFAULT_READ_S = "600";

/**
 * 后端一行 → 草稿。
 *
 * 读不到的字段留空串（= 界面上的空输入框），**不填编出来的默认值**；例外只有超时的
 * `connect_s` / `read_s`：Rust 侧它们**不是 `Option`**（永远是数字），所以"读不到"时
 * 唯一诚实的显示是它的默认值 —— 这一条写在界面的提示里。
 */
export function draftFromRow(row: ProviderRow): ProviderEditDraft {
  return {
    name: row.name,
    protocol: row.protocol ?? "",
    baseUrl: row.baseUrl ?? "",
    enabled: row.enabled ?? true,
    lane: row.lane ?? "",
    modelPrefix: row.modelPrefix ?? "",

    authScheme: row.authScheme ?? "bearer",
    authHeader: row.authHeader ?? "",
    authPrefix: row.authPrefix ?? "",

    timeoutConnectS:
      row.timeoutConnectS === undefined ? DEFAULT_CONNECT_S : String(row.timeoutConnectS),
    timeoutReadS: row.timeoutReadS === undefined ? DEFAULT_READ_S : String(row.timeoutReadS),
    timeoutTotalS: row.timeoutTotalS === undefined ? "" : String(row.timeoutTotalS),

    // 额外 CA 是**用户自己的材料**，页面本来就在「原始字段」里原样打印它，所以这里预填真值：
    // 不预填的话用户一保存就把自己配过的 CA 抹掉了（那是比"少显示一个字段"严重得多的事）。
    tlsExtraCa: row.tlsExtraCaPem ?? "",
    tlsInsecure: row.tlsInsecure ?? false,
    tlsMinVersion: row.tlsMinVersion ?? "",

    onHeaderConflict: row.onHeaderConflict ?? "keep_client",
    passthroughHeaderMode: row.passthroughHeaderMode ?? "",
    passthroughBody: row.passthroughBody ?? "",

    headers: row.headers,

    key: "",
  };
}

/**
 * 页面能不能安全地写这一行。
 *
 * **假** = 后端给的是只读视图（连接配置一个字段都没返回）。此时保存会用默认值覆盖线上的
 * 鉴权 / 超时 / TLS / 附加头，所以页面只读并说明原因；不画假控件、也不假装能改。
 */
export function canEdit(row: ProviderRow | undefined): boolean {
  return row !== undefined && row.hasConnectionConfig;
}

/** 正整数（秒）。用户可以留空的字段用 `allowEmpty` 单独判。 */
function isPositiveInt(value: string): boolean {
  return /^\d+$/.test(value.trim()) && Number(value.trim()) > 0;
}

/**
 * 本地预检：返回空数组才允许发命令。`message` 是**既有 i18n 键**（页面直接 `t(message)`），
 * 与两个添加向导同一套（`wizard.required` / `wizard.invalidUrl` / `wizard.invalidNumber`）。
 */
export function validateDraft(draft: ProviderEditDraft): WizardIssue[] {
  const issues: WizardIssue[] = [];

  if (draft.name.trim() === "") issues.push({ field: "name", message: "wizard.required" });
  if (draft.protocol.trim() === "") issues.push({ field: "protocol", message: "wizard.required" });
  if (draft.baseUrl.trim() === "") issues.push({ field: "baseUrl", message: "wizard.required" });
  else if (!/^https?:\/\//.test(draft.baseUrl.trim())) {
    issues.push({ field: "baseUrl", message: "wizard.invalidUrl" });
  }

  if (!isPositiveInt(draft.timeoutConnectS)) {
    issues.push({ field: "timeoutConnectS", message: "wizard.invalidNumber" });
  }
  if (!isPositiveInt(draft.timeoutReadS)) {
    issues.push({ field: "timeoutReadS", message: "wizard.invalidNumber" });
  }
  if (draft.timeoutTotalS.trim() !== "" && !isPositiveInt(draft.timeoutTotalS)) {
    issues.push({ field: "timeoutTotalS", message: "wizard.invalidNumber" });
  }

  // 附加头：**头名不能空、不能重复**（重复会在载荷里互相覆盖，用户看不到谁生效）
  const seen = new Set<string>();
  for (const header of draft.headers) {
    const name = header.name.trim();
    if (name === "") issues.push({ field: "headers", message: "providers.edit.headerNameRequired" });
    else if (seen.has(name)) {
      issues.push({ field: "headers", message: "providers.edit.headerNameDuplicate" });
    }
    seen.add(name);
  }

  return issues;
}

/** 没有改动就不用写库（保存按钮据此禁用，界面也说清"没有改动"）。 */
export function isDirty(draft: ProviderEditDraft, baseline: ProviderEditDraft): boolean {
  if (draft.key !== "") return true;
  return JSON.stringify(draft) !== JSON.stringify(baseline);
}

/** 空串 = 不设（`Option` 的 `None`）；否则给原值（**不 trim**：`Bearer ` 的尾随空格有意义）。 */
export function optional(value: string): string | null {
  return value.trim() === "" ? null : value;
}

/**
 * 草稿 → `provider_upsert` 的载荷。**以原始记录为底**，见文件头第 2、3 条。
 *
 * 返回类型断言成 `ProviderInput`：原始记录里可能带自定义字段（`extra`），
 * 它们不在契约的已知键集合里，但**必须**原样发给后端（Rust 的 `serde(flatten)` 收它们）。
 */
export function buildProviderPayload(
  row: ProviderRow,
  draft: ProviderEditDraft,
): ProviderInput {
  const base: Row = { ...row.raw };
  for (const key of VIEW_ONLY_KEYS) delete base[key];

  const auth = { ...asRow(base["auth"]) };
  const timeout = { ...asRow(base["timeout"]) };
  const tls = { ...asRow(base["tls"]) };
  const passthrough = { ...asRow(base["passthrough"]) };

  const headers: Record<string, string> = {};
  for (const header of draft.headers) {
    const name = header.name.trim();
    if (name !== "") headers[name] = header.value;
  }

  const payload: Row = {
    ...base,
    id: row.id,
    name: draft.name.trim(),
    protocol: draft.protocol as Protocol,
    base_url: draft.baseUrl.trim(),
    enabled: draft.enabled,
    lane: optional(draft.lane),
    model_prefix: optional(draft.modelPrefix),
    headers,
    on_header_conflict: draft.onHeaderConflict,
    passthrough: {
      ...passthrough,
      header_mode: optional(draft.passthroughHeaderMode),
      body: optional(draft.passthroughBody),
    },
    auth: {
      ...auth,
      scheme: draft.authScheme,
      header: optional(draft.authHeader),
      prefix: optional(draft.authPrefix),
    },
    timeout: {
      ...timeout,
      connect_s: Number(draft.timeoutConnectS.trim()),
      read_s: Number(draft.timeoutReadS.trim()),
      total_s: draft.timeoutTotalS.trim() === "" ? null : Number(draft.timeoutTotalS.trim()),
    },
    tls: {
      ...tls,
      insecure: draft.tlsInsecure,
      extra_ca_pem: optional(draft.tlsExtraCa),
      min_version: optional(draft.tlsMinVersion),
    },
  };

  return payload as unknown as ProviderInput;
}

/**
 * 协议下拉的选项：已知协议在前，**契约外的取值原样补一行**（与两个向导同一条纪律：
 * 后端给什么就显示什么，不改写、不隐藏）。
 */
export function protocolChoices(
  protocols: readonly string[],
  labelOf: (protocol: string) => string,
  current: string,
): { value: string; label: string }[] {
  const known = protocols.map((protocol) => ({ value: protocol, label: labelOf(protocol) }));
  if (current === "" || known.some((option) => option.value === current)) return known;
  return [{ value: current, label: current }, ...known];
}

/**
 * 服务商级「网络代理」下拉的选项。
 *
 * # 顺序（[docs/54]，用户第 321 轮）
 *
 * 用户原话：*"设置服务商的网络的时候，默认直连，然后可以选择我配置好的节点，
 * 其中自动选择和直连在最上面"*。所以顺序是：
 *
 * 1. **自动选择**（`auto:<订阅 id>`）—— 有订阅时才有；多份订阅时带订阅名；
 * 2. **直连**（`direct:`）—— 用户点名的另一个"最上面"；
 * 3. 「不指定（继承）」—— 它不是默认值，但必须留着（继承会跟着上层走，与"钉死直连"是两件事）；
 * 4. 已配置的线路。
 *
 * **「自动选择」排在「直连」前面**是按用户那句话的语序（"自动选择和直连"）。
 * 但**默认值仍然是「直连」**（同一句话："默认直连"）—— 顺序与默认值是两件事，
 * 别因为把某一项排前面就顺手把它变成默认（那会让新服务商悄悄走代理）。
 *
 * 值都是**标记**（`auto:<订阅>` / `direct:`），由后端 `provider_upsert` 解析成真实线路 ——
 * 前端不建线路、也不自己编 id（"这条线路该长什么样"的判断在后端只有一份）。
 */
export function providerLaneChoices(
  groups: readonly LaneChoiceGroup[],
  inheritLabel: string,
  subscriptions: readonly { id: string; name: string }[] = [],
  autoLabel = "自动选择",
): ProxyChoice[] {
  /**
   * [docs/29 §2.2] 「自动选择」出口。值是**标记** `auto:<订阅 id>`，由后端 `provider_upsert`
   * 解析成一条合格的线路（`subscription_auto` + **显式 `select`** —— 那才是"ping 一轮挑最低延迟 +
   * 按地址粘住"）。
   *
   * [docs/58]（用户第 325 轮："自动选择:xx订阅"）：**订阅名一律带上**。
   * 原来只在"多于一份订阅"时才带名字，于是单份订阅的选项只写「自动」—— 用户看不出
   * 它用的是哪份订阅，而他原话就是要把订阅名写出来。名字本身不是秘密（[docs/26 §2]）。
   */
  const auto: ProxyChoice[] = subscriptions.map((sub) => ({
    value: `auto:${sub.id}`,
    label: sub.name.trim() === "" ? autoLabel : `${autoLabel}（${sub.name}）`,
  }));

  /**
   * 其余选项来自 **`lane_options`**（[`proxyChoices`]）：直连 → 不指定（继承）→
   * 已配置线路 + **订阅里的每个节点**。
   *
   * [docs/58] 这里以前接的是 `lane_list`（**只有已配置的线路**）——
   * 那是用户第 325 轮报的 bug 的根因："我已经添加了订阅和节点，但是在服务商那边选节点还是
   * 啥也没看到"。节点只有在 `lane_options` 里才有（后端 `compute_lane_choices` 会把
   * 已采纳版本的节点逐个列出来）。
   */
  return [...auto, ...proxyChoices(groups, inheritLabel)];
}

/**
 * [docs/58] 把下拉选中的值**解析成真正要写进 `provider.lane` 的东西**。
 *
 * 三种值直接原样交给后端，因为"这条线路该长什么样"的判断在后端只有一份：
 * * `""` → 「不指定（继承）」（`buildProviderPayload` 会把它变成显式 `null`）；
 * * `auto:<订阅>` → `provider_upsert` 里的 `ensure_auto_lane`；
 * * `direct:` → `provider_upsert` 里的 `ensure_direct_lane`；
 * * 真实 lane id → 原样写。
 *
 * **只有订阅节点这一种必须在这里先落地**：值形如 `node:<订阅>:<节点名>`，它不是 lane id，
 * 而 `provider_upsert` 不认识这个标记（它只解析 `auto:` / `direct:`）。若不处理就发出去，
 * 后端会拿 `node:...` 当线路 id → 校验器报"线路不存在" → 用户选了节点却存不下去。
 * 所以这里调 [`lane_ensure_for_node`] 把它换成真实 id（已有等价线路就直接复用）。
 *
 * 拿不到 `nodeRef` 时**不猜**：原样返回，让后端如实报错 —— 编一个 id 出去才是真的坏。
 */
export async function resolveLaneChoice(
  value: string | null | undefined,
  choices: readonly ProxyChoice[],
  ensureForNode: (subscriptionId: string, node: string) => Promise<{ id: string }>,
): Promise<string | null | undefined> {
  if (value === null || value === undefined || value === "") return value;
  if (!value.startsWith("node:")) return value;

  const choice = choices.find((c) => c.value === value);
  // 已经有等价线路（后端在 `lane_id` 里给了）→ 不必再建，直接用它
  if (choice?.laneId !== undefined && choice.laneId !== "") return choice.laneId;
  if (choice?.nodeRef === undefined) return value;
  const lane = await ensureForNode(choice.nodeRef.subscriptionId, choice.nodeRef.node);
  return lane.id;
}

/** 附加头列表的编辑操作（纯函数，好单测）：改、加、删。 */export function updateHeader(
  headers: readonly HeaderPair[],
  index: number,
  patch: Partial<HeaderPair>,
): HeaderPair[] {
  return headers.map((header, i) => (i === index ? { ...header, ...patch } : header));
}

export function removeHeader(headers: readonly HeaderPair[], index: number): HeaderPair[] {
  return headers.filter((_, i) => i !== index);
}

export function appendHeader(headers: readonly HeaderPair[]): HeaderPair[] {
  return [...headers, { name: "", value: "" }];
}

/**
 * 首启向导（[05 §2.4] / [12 §6 F4]）的纯逻辑。
 *
 * 后端三条命令（`onboarding_detect` / `onboarding_apply` / `onboarding_test_all`）**都已注册**，
 * 页面真的调用它们（见 `index.tsx`）；本文件承担的是"调用之外的判定"，它们不该塞进组件里，
 * 也不该和后端校验互相替代：
 *
 * 1. **本地校验按行给出行号级的问题**（`validateProviders` / `validateSubscriptions`）：
 *    哪些行缺标识、标识撞了、接口地址不是 http(s)、密钥是空的。这些判定**不需要网络**，
 *    也不该等后端 —— 后端校验是另一层（`config_validate`），两者不互相替代。
 * 2. **一行粘贴多个密钥会展开成多个凭据条目**（`credentialEntries`）：
 *    提交契约 `OnboardingInput.providers[]` 每个条目只带**一个** `secret`
 *    （`shared/bindings.ts` 的 `OnboardingInput`），所以"一行三个 key"必须变成三个条目，
 *    否则生成出来的配置只会留下最后一个 key —— 这是这一页最容易静默丢掉用户输入的地方。
 * 3. **将要测试的组合由输入推导**（`testMatrix`）：真实结果只来自 `onboarding_test_all`
 *    （页面上由"开始测试"按钮触发），这里推导的是"会测哪些组合"，**不是**测试结果；
 *    没有线路候选时给空矩阵，**不编线路**。
 */

import type { ApplyResult, ConfigDoc, OnboardingInput, Protocol } from "@/shared/bindings";
import type { I18nKey } from "@/shared/i18n";

/** 协议取值**必须**与冻结契约的 `Protocol` 一致（少一个就等于悄悄禁掉一种协议）。 */
export const PROTOCOLS: readonly Protocol[] = [
  "openai",
  "openai_responses",
  "anthropic",
  "gemini",
  "custom",
];

// ================================================================ 行

export type ProviderRow = {
  /** 会写进配置的 id（提交契约里就是 `ProviderId`）。 */
  id: string;
  name: string;
  protocol: Protocol;
  base_url: string;
  /** 允许一行多个：换行分隔（`credentialLines` 负责切）。 */
  secret: string;
};

export type SubRow = {
  id: string;
  name: string;
  url: string;
};

/** 第 n 行服务商（n 从 1 开始）：id 直接用 `p{n}`，让用户看得懂、也能自己改。 */
export function newProviderRow(n: number): ProviderRow {
  return { id: `p${n}`, name: "", protocol: "openai", base_url: "", secret: "" };
}

export function newSubRow(n: number): SubRow {
  return { id: `s${n}`, name: "", url: "" };
}

/**
 * 新加一行时取**未被占用**的最小编号。
 * （直接用 `length + 1` 会在"删掉中间一行再加"时撞 id —— 那会让用户看到一条莫名其妙的重复错误。）
 */
export function nextProviderRow(rows: readonly ProviderRow[]): ProviderRow {
  const taken = new Set(rows.map((r) => r.id));
  for (let n = 1; n <= rows.length + 1; n += 1) {
    if (!taken.has(`p${n}`)) return newProviderRow(n);
  }
  return newProviderRow(rows.length + 1);
}

export function nextSubRow(rows: readonly SubRow[]): SubRow {
  const taken = new Set(rows.map((r) => r.id));
  for (let n = 1; n <= rows.length + 1; n += 1) {
    if (!taken.has(`s${n}`)) return newSubRow(n);
  }
  return newSubRow(rows.length + 1);
}

// ================================================================ 密钥行

/** 一次粘贴多个密钥：按行切、去首尾空白、丢空行。 */
export function credentialLines(secret: string): string[] {
  return secret
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

export type CredentialEntry = {
  id: string;
  name: string;
  protocol: Protocol;
  base_url: string;
  secret: string;
};

/**
 * 把"一行 = 多个密钥"展开成"一个条目 = 一个密钥"。
 *
 * 单密钥的行**保持原 id**（用户的 id 就是配置里的 provider id，不该被改写）；
 * 多密钥的行才编号（`p1-1` / `p1-2` …）—— 否则同一行展开出来的多个条目会撞 id。
 * 没有密钥的行不产生条目（它进不了提交内容，这件事在 UI 上单独说明）。
 */
export function credentialEntries(rows: readonly ProviderRow[]): CredentialEntry[] {
  const out: CredentialEntry[] = [];
  for (const row of rows) {
    const lines = credentialLines(row.secret);
    if (lines.length === 0) continue;
    lines.forEach((secret, index) => {
      out.push({
        id: lines.length === 1 ? row.id : `${row.id}-${index + 1}`,
        name: row.name.trim(),
        protocol: row.protocol,
        base_url: row.base_url.trim(),
        secret,
      });
    });
  }
  return out;
}

// ================================================================ 本地校验

export type IssuePath = "id" | "name" | "base_url" | "secret" | "url";
export type RowIssue = { row: number; path: IssuePath; key: I18nKey };

function isHttpUrl(raw: string): boolean {
  const value = raw.trim();
  if (value === "") return false;
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

/** 逐行校验服务商：标识（非空 + 唯一）、名称、接口地址、密钥。 */
export function validateProviders(rows: readonly ProviderRow[]): RowIssue[] {
  const issues: RowIssue[] = [];
  const seen = new Map<string, number>();
  rows.forEach((row, index) => {
    const id = row.id.trim();
    if (id === "") {
      issues.push({ row: index, path: "id", key: "onboarding.issue.id" });
    } else if (seen.has(id)) {
      // 标识会写进配置当 provider id：重复必须是错误，不能"后者覆盖前者"
      issues.push({ row: index, path: "id", key: "onboarding.issue.id" });
    } else {
      seen.set(id, index);
    }
    if (row.name.trim() === "") {
      issues.push({ row: index, path: "name", key: "onboarding.issue.name" });
    }
    if (!isHttpUrl(row.base_url)) {
      issues.push({ row: index, path: "base_url", key: "onboarding.issue.baseUrl" });
    }
    if (credentialLines(row.secret).length === 0) {
      issues.push({ row: index, path: "secret", key: "onboarding.issue.secret" });
    }
  });
  return issues;
}

/** 逐行校验订阅：标识（非空 + 唯一）与链接（必须 http(s)）。 */
export function validateSubscriptions(rows: readonly SubRow[]): RowIssue[] {
  const issues: RowIssue[] = [];
  const seen = new Set<string>();
  rows.forEach((row, index) => {
    const id = row.id.trim();
    if (id === "" || seen.has(id)) {
      issues.push({ row: index, path: "id", key: "onboarding.issue.id" });
    } else {
      seen.add(id);
    }
    if (!isHttpUrl(row.url)) {
      issues.push({ row: index, path: "url", key: "onboarding.issue.url" });
    }
  });
  return issues;
}

// ================================================================ 提交内容（预览用，不提交）

export type BuildInputArgs = {
  kernelPath: string;
  providers: readonly ProviderRow[];
  subscriptions: readonly SubRow[];
  /** 「我不用梯子」是一个**明确选择**：勾上后订阅列表就是空的。 */
  noVpn: boolean;
};

/**
 * 拼出**会发给 `onboarding_apply` 的那份内容**（`OnboardingInput`）。
 *
 * 它同时是"提交前把要发出去的东西原样显示出来"的来源 —— 预览与将来真正提交的
 * 必须是同一个函数算出来的，否则预览迟早会与真实请求分叉。
 */
export function buildOnboardingInput(args: BuildInputArgs): OnboardingInput {
  const kernel = args.kernelPath.trim();
  return {
    kernel_path: kernel === "" ? null : kernel,
    providers: credentialEntries(args.providers).map((entry) => ({
      id: entry.id,
      name: entry.name,
      protocol: entry.protocol,
      base_url: entry.base_url,
      secret: entry.secret,
    })),
    subscriptions: args.noVpn
      ? []
      : args.subscriptions.map((row) => ({
          id: row.id.trim(),
          name: row.name.trim(),
          url: row.url.trim(),
        })),
  };
}

/** 稳定可读的 payload 原文（预览与断言用同一份）。 */
export function payloadJson(input: OnboardingInput): string {
  return JSON.stringify(input, null, 2);
}

// ================================================================ 测试矩阵（推导，不是结果）

export type MatrixRow = { credentialId: string; credential: string; lane: string };

/**
 * `onboarding_test_all` 接入后会测的组合：**凭据条目 × 线路候选**。
 *
 * 没有线路候选时返回空数组 —— 因为此时"要测什么"是真的还不知道，
 * 编一条"直连"摆在那里就等于假装线路已经存在。
 */
export function testMatrix(
  entries: readonly CredentialEntry[],
  lanes: readonly string[],
): MatrixRow[] {
  const rows: MatrixRow[] = [];
  entries.forEach((entry, index) => {
    const sameNameBefore = entries
      .slice(0, index)
      .filter((e) => (e.name === "" ? e.id : e.name) === (entry.name === "" ? entry.id : entry.name))
      .length;
    const sameNameTotal = entries.filter(
      (e) => (e.name === "" ? e.id : e.name) === (entry.name === "" ? entry.id : entry.name),
    ).length;
    const base = entry.name === "" ? entry.id : entry.name;
    const label = sameNameTotal > 1 ? `${base} (${sameNameBefore + 1})` : base;
    for (const lane of lanes) {
      rows.push({ credentialId: entry.id, credential: label, lane });
    }
  });
  return rows;
}

/** 线路候选（只读，来自当前配置）。读不出来就是空 —— 不编线路。 */
export function laneCandidates(doc: unknown): string[] {
  const lanes = (doc as ConfigDoc | undefined | null)?.lanes;
  if (!Array.isArray(lanes)) return [];
  const out: string[] = [];
  for (const lane of lanes) {
    const name = typeof lane?.name === "string" && lane.name.trim() !== "" ? lane.name : lane?.id;
    if (typeof name === "string" && name.trim() !== "" && !out.includes(name)) out.push(name);
  }
  return out;
}

// ================================================================ 后端返回 → 要显示的事实

/**
 * `onboarding_detect` 的返回（契约里是内联结构，见 `shared/bindings.ts` 的 `CommandMap`）。
 *
 * 注意它比 Rust 侧的 `settings_ext::OnboardingDetect` **窄**：Rust 还序列化
 * `kernel_checked`（检查过哪些位置）与 `kernel_path`（命中的路径），但手写契约里没列，
 * 所以前端拿不到它们 —— 这件事在页面上如实说明（不猜、也不自己拼一条"检查过的位置"）。
 */
export type DetectResult = {
  kernel_found: boolean;
  existing_config: boolean;
  suggested_providers: string[];
};

export type DetectFacts = {
  kernel: "found" | "missing";
  config: "existing" | "missing";
  suggested: string[];
};

/**
 * 把后端布尔值压成"有 / 没有"两档。
 *
 * **只做映射**：不掺默认值、不在缺字段时补一个"看起来正常"的值 —— 三行显示什么，
 * 完全由后端的返回决定（探测失败走的是另一条分支，显示失败原因）。
 */
export function detectFacts(d: DetectResult): DetectFacts {
  return {
    kernel: d.kernel_found ? "found" : "missing",
    config: d.existing_config ? "existing" : "missing",
    suggested: [...d.suggested_providers],
  };
}

/**
 * `ApplyResult` 拆成**逐项事实**：UI 只渲染这些事实，不做"已完成 / 已生成"这类翻译。
 *
 * `applied=false` 时说"后端没有写入"；`restart_required` / `kernel_reload` 各自单独一行；
 * 两个都为 false 时才敢说"无需重启、也无需重载内核"。
 */
export type ApplyFact = "applied" | "notApplied" | "restart" | "kernelReload" | "effective";

export function applyFacts(r: ApplyResult): ApplyFact[] {
  const out: ApplyFact[] = [r.applied ? "applied" : "notApplied"];
  if (r.restart_required) out.push("restart");
  if (r.kernel_reload) out.push("kernelReload");
  if (!r.restart_required && !r.kernel_reload) out.push("effective");
  return out;
}

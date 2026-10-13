/**
 * 服务商页的**纯逻辑**：没有 JSX、没有 React、没有 IPC。
 *
 * 把这个页面最容易被写错的一件事收在这里并用单测钉住：**读后端的服务商与凭据**。
 *
 * ## 契约与实现不一致（这是事实，不是猜测；来源逐条标注）
 *
 * | 命令 | 冻结契约（`src/shared/bindings.ts`，本任务不许改） | Rust 实现（权威） |
 * |---|---|---|
 * | `provider_list` | `ProviderView[]`（bindings.ts:1018） | `Vec<Provider>` —— **完整的配置实体**（`src-tauri/src/commands/catalog.rs:53`） |
 * | `provider_get` | `ProviderView`（bindings.ts:1019） | `Option<Provider>` —— 可能为 `null`（catalog.rs:59） |
 * | `credential_list` | `CredentialView` + `provider_name` + `channel_count`（bindings.ts:603-610） | `{ credential }`（catalog.rs:21-28） |
 * | `lane_list` | `LaneView.{type, node_count, health…}`（bindings.ts:679-686） | `{ lane, latest, channel_count }`（catalog.rs:39-45） |
 *
 * 两边**各有对方没有的列**：契约视图有 `status` / `success_rate_24h` / `avg_latency_ms` /
 * `channel_count`，Rust 有 `auth` / `timeout` / `tls` / `headers` / `passthrough` / `lane` /
 * `model_prefix`。所以这里按**并集**读，每一列都标出它可能来自哪一边；
 * **都读不到就是 `undefined`**（页面画「—」或「未知」），不填 0、不拿别的列推断
 * （B2：不拿不确定冒充确定）。
 *
 * 这两处偏差是**契约生成（T1 的 `npm run gen:bindings`）落地前**的现实；
 * 生成物覆盖 `bindings.ts` 之后，并集读法仍然成立（多认一个名字而已）。
 *
 * ## 为什么详情页也从 `provider_list` 取那一行
 *
 * `provider_get` 与 `provider_list` 在 Rust 里读的是**同一份内存快照**
 * （`state.doc().providers`，catalog.rs:53-62），取哪一条数据都一样。
 * 但 `provider_list` 给出的是**完整目录**，于是"id 不在目录里"是一个有意义的结论
 * （页面据此显示"目录里没有这个服务商"），而不是"某个接口没返回"。
 * 浏览器开发路径（`npm run dev`，无 Tauri）只 mock 了 `provider_list`，
 * 走同一条命令也让两个页面在两种环境下行为一致 —— 一处取数、一处口径。
 */

import type {
  LaneChoiceGroup,
} from "@/shared/bindings";
// 读后端原始对象的唯一实现（原先本文件自己写了 asRow/pickStr/pickNum/pickBool）
import { asRow, pickBool, pickNum, pickStr, type Row } from "@/shared/row";

// ================================================================ 服务商

/** 一条附加头。名字与值都**原样**（用户数据不做清洗）。 */
export type HeaderPair = { name: string; value: string };

/** 归一化后的一条服务商。**每个可选字段都标了它来自哪一边**；缺 = 未知，不是 0。 */
export type ProviderRow = {
  id: string;
  name: string;
  protocol?: string;
  baseUrl?: string;
  enabled?: boolean;

  // ---- 契约 / mock 的视图形状专有（Rust 的 `provider_list` 没有这几列）
  /** `ok | warn | error | off`。**没有这一列时不显示"正常"**。 */
  status?: string;
  /** 近 24h 成功率（0~1）。 */
  successRate24h?: number;
  avgLatencyMs?: number;
  /** 挂载的通道数（视图列；Rust 侧要靠通道列表算，本页不猜）。 */
  channelCount?: number;

  // ---- Rust 的完整 `Provider` 专有（浏览器 mock 的视图形状没有）
  lane?: string;
  modelPrefix?: string;
  authScheme?: string;
  authHeader?: string;
  authPrefix?: string;
  headers: HeaderPair[];
  onHeaderConflict?: string;
  passthroughHeaderMode?: string;
  passthroughBody?: string;
  timeoutConnectS?: number;
  timeoutReadS?: number;
  timeoutTotalS?: number;
  tlsInsecure?: boolean;
  /** **只记录"配没配"**：额外 CA 是私密材料，只读展示时只显示是否配置，不打印内容。 */
  tlsExtraCa: boolean;
  /**
   * 额外 CA 的**原文**。
   *
   * 编辑页需要它：`provider_upsert` 是整条覆盖写，回写时少这一个键就等于
   * 把用户自己配过的 CA 抹掉。所以读出来、原样带回去 —— 只读展示仍然只用
   * [`tlsExtraCa`] 那个布尔值（"已配置 / 未配置"），不把 PEM 铺在版面上。
   */
  tlsExtraCaPem?: string;
  tlsMinVersion?: string;

  /**
   * 后端是否返回了连接配置字段（`auth` / `timeout` / `tls` / `headers` / `passthrough` /
   * `lane` / `model_prefix` 里**任何一个**）。
   *
   * `false` = 拿到的是视图（只有名称、协议、地址、健康度）→ 连接区应当**明说不可用**，
   * 而不是排一排空占位值。
   */
  hasConnectionConfig: boolean;

  /** 后端返回的原始对象（"原始字段"区原样打印）。 */
  raw: Row;
};

/** 命中任意一个就说明后端给的是**完整 `Provider`** 而不是视图。 */
const CONFIG_KEYS = [
  "auth",
  "lane",
  "headers",
  "on_header_conflict",
  "passthrough",
  "timeout",
  "tls",
  "model_prefix",
] as const;

/** `BTreeMap<String, String>` → 有序数组；非字符串的值丢掉（不 `String()` 硬转）。 */
function readHeaders(value: unknown): HeaderPair[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [];
  return Object.entries(value as Row)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .map(([name, headerValue]) => ({ name, value: headerValue }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * 读一条服务商。入参是契约类型，运行时可能是契约的视图形状或 Rust 的完整实体。
 */
export function readProvider(raw: unknown): ProviderRow {
  const row = asRow(raw);
  const auth = asRow(row["auth"]);
  const timeout = asRow(row["timeout"]);
  const tls = asRow(row["tls"]);
  const passthrough = asRow(row["passthrough"]);
  const extraCa = tls["extra_ca_pem"];

  return {
    id: pickStr(row, "id") ?? "",
    name: pickStr(row, "name") ?? "",
    protocol: pickStr(row, "protocol"),
    baseUrl: pickStr(row, "base_url"),
    enabled: pickBool(row, "enabled"),

    status: pickStr(row, "status"),
    successRate24h: pickNum(row, "success_rate_24h"),
    avgLatencyMs: pickNum(row, "avg_latency_ms"),
    channelCount: pickNum(row, "channel_count"),

    lane: pickStr(row, "lane"),
    modelPrefix: pickStr(row, "model_prefix"),
    authScheme: pickStr(auth, "scheme"),
    authHeader: pickStr(auth, "header"),
    authPrefix: pickStr(auth, "prefix"),
    headers: readHeaders(row["headers"]),
    onHeaderConflict: pickStr(row, "on_header_conflict"),
    passthroughHeaderMode: pickStr(passthrough, "header_mode"),
    passthroughBody: pickStr(passthrough, "body"),
    timeoutConnectS: pickNum(timeout, "connect_s"),
    timeoutReadS: pickNum(timeout, "read_s"),
    timeoutTotalS: pickNum(timeout, "total_s"),
    tlsInsecure: pickBool(tls, "insecure"),
    tlsExtraCa: typeof extraCa === "string" && extraCa !== "",
    tlsExtraCaPem: pickStr(tls, "extra_ca_pem"),
    tlsMinVersion: pickStr(tls, "min_version"),

    hasConnectionConfig: CONFIG_KEYS.some((key) => key in row),
    raw: row,
  };
}

// ================================================================ 凭据

/** 归一化后的一条凭据（额度列分两种后端形状，见类型注释）。 */
export type CredentialRow = {
  id: string;
  /** 挂载关系写在这里（属于哪个服务商）。 */
  provider?: string;
  label: string;
  /** `${secret:name}` / `${env:VAR}` 之类的**引用**；明文永远不该出现在这里。 */
  secret?: string;
  lane?: string;
  enabled?: boolean;
  tags: string[];


  /**
   * **多窗口**额度：
   */

  /** 契约/mock 才有：这份凭据里是不是明文密钥。 */
  plaintextSecret?: boolean;

  raw: Row;
};

export function readCredential(raw: unknown): CredentialRow {
  const row = asRow(raw);
  const credential = asRow(row["credential"]);
  const tags = credential["tags"];

  return {
    id: pickStr(credential, "id") ?? "",
    provider: pickStr(credential, "provider"),
    label: pickStr(credential, "label") ?? "",
    secret: pickStr(credential, "secret"),
    lane: pickStr(credential, "lane"),
    enabled: pickBool(credential, "enabled"),
    tags: Array.isArray(tags) ? tags.filter((x): x is string => typeof x === "string") : [],


    plaintextSecret: pickBool(row, "has_plaintext_secret"),
    raw: row,
  };
}

/**
 * 整条 credential 的**原始记录**（`credential_list` 那一行里的 `credential`）。
 *
 * 写回时必须以它为底：`credential_upsert` 是整条覆盖写（/ 同一条纪律）。
 */
export function credentialRawOf(row: CredentialRow): Row {
  return asRow(row.raw["credential"]);
}

// ================================================================ 挂载统计

/** 挂载情况。`undefined` = **读不到**（页面显示"未知"），`0` = 确实是 0。 */
export type MountSummary = { credentials?: number; channels?: number };

/**
 * 一条服务商挂了多少凭据 / 通道。
 *
 * - 凭据数来自 `credential_list`（按 `credential.provider` 归并）：列表读不到就是 `undefined`；
 * - 通道数只取后端视图给的 `channel_count`（Rust 的 `provider_list` 没有这一列 → `undefined`）。
 */
export function mountOf(
  row: ProviderRow,
  credentials: readonly CredentialRow[] | undefined,
): MountSummary {
  return {
    credentials:
      credentials === undefined
        ? undefined
        : credentials.filter((c) => c.provider === row.id).length,
    channels: row.channelCount,
  };
}

/** 属于某个服务商的凭据（顺序保持后端给的顺序）。 */
export function credentialsOf(
  credentials: readonly CredentialRow[] | undefined,
  providerId: string,
): CredentialRow[] {
  return (credentials ?? []).filter((c) => c.provider === providerId);
}

// ================================================================ 线路（只用来把 lane id 换成名字）

/** 线路下拉/显示需要的两个字段（契约与 Rust 的 `LaneView` 都包着 `lane`）。 */
export type LaneOption = { id?: string; name?: string };

export function readLane(raw: unknown): LaneOption {
  const lane = asRow(asRow(raw)["lane"]);
  return { id: pickStr(lane, "id"), name: pickStr(lane, "name") };
}

/** `lane` id → 线路名。读不到时返回 `undefined`（页面退回显示 id，不编名字）。 */
export function laneNameOf(
  lanes: readonly LaneOption[],
  laneId: string | undefined,
): string | undefined {
  if (laneId === undefined) return undefined;
  return lanes.find((l) => l.id === laneId)?.name;
}

// ================================================================ 网络代理下拉

/**
 * 网络代理下拉的一项。
 *
 * 与 `LaneOption` 的区别：`LaneOption` 只是"把 lane id 翻成名字"，而这里的每一项都带着
 * **选中之后要做什么**：
 * * `laneId` 有值 → 直接把它写进 `channel.lane`；
 * * `nodeRef` 有值 → 这是订阅里的一个**节点**，还没（或不确定）有线路：先调
 *   `lane_ensure_for_node` 拿到线路 id，再写 `channel.lane`。
 *
 * 用户现场（他加好订阅、节点也看到了，但服务商页那条线路只有「不指定（继承）」）就是
 * 因为下拉过去只列"已配置的线路"，而他的配置里一条线路都没有 —— 所以这个列表必须把
 * **订阅节点**也列出来。
 */
export type ProxyChoice = {
  /** `""` = 继承；否则是 lane id、`direct:`（直连标记）或 `node:<订阅>:<节点>` */
  value: string;
  label: string;
  laneId?: string;
  /** 有它 = 这是订阅里的一个节点，选中前要 `lane_ensure_for_node` */
  nodeRef?: { subscriptionId: string; node: string };
  /** `true` = 「直连」，选中前要 `lane_ensure_direct`（配置里还没有 direct 线路时） */
  direct?: boolean;
};

/**
 * `lane_options` 的分组 → 下拉选项。
 *
 * **顺序是有意的**（用户第 81 轮：「网络代理选项中最重要的是直连，这个你都没单独列出来？」）：
 * 1. **直连** —— 永远第一项、永远单独列出。配置里没有 direct 线路时后端会给 `direct:` 标记，
 *    选中它由 `lane_ensure_direct` 把线路建出来。它与「继承」是**两件事**：直连把它钉死在本机网络，
 *    而继承会跟着凭据 / 服务商 / 默认线路走（那条默认线路以后被改成代理，这条线路会跟着变）。
 * 2. **继承**（`""`）—— 跟着上层走，末端才是直连。它不是默认选项，但必须留着。
 * 3. 已配置线路 / 订阅节点（按后端给的分组顺序）。
 *
 * **去重规则**：一个节点若**已经有**等价线路，后端会同时给出"线路"项与"节点"项（节点项带
 * `lane_id`）—— 此时只保留**线路**那一项（同一条线路出现两次会让人以为有两张网）。
 */
export function proxyChoices(
  groups: readonly LaneChoiceGroup[],
  inheritLabel: string,
): ProxyChoice[] {
  const direct: ProxyChoice[] = [];
  const rest: ProxyChoice[] = [];
  const seenLanes = new Set<string>();
  const seenValues = new Set<string>([""]);
  for (const group of groups) {
    for (const item of group.items) {
      // 后端把直连放在第一组；这里再兜一次底（不依赖后端的分组顺序）
      const isDirect = item.kind === "direct";
      const laneId = item.lane_id;
      const isNode = item.kind === "node";
      if (seenValues.has(item.value)) continue;
      if (isNode && laneId !== undefined && seenLanes.has(laneId)) continue;
      if (laneId !== undefined && seenLanes.has(laneId) && !isNode) continue;
      seenValues.add(item.value);
      if (laneId !== undefined) seenLanes.add(laneId);
      const group_ = group.group.trim();
      // 分组名只在**确实是额外信息**时才拼：直连那条的分组名与它自己的名字相同，
      // 无脑拼会得到「直连（本机网络）（直连（本机网络））」。
      //
      // 「直连」那一项**一律不拼分组名**：它自带的分组就叫「直连（本机网络）」，
      // 而配置里那条 direct 线路的名字是「直连」—— 拼起来会变成
      // 「直连（直连（本机网络））」（真机截图里就是这么显示的）。直连是**单独一项**，
      // 分组名对它没有任何区分作用。
      const label =
        isDirect || group_ === "" || item.label === group_ || item.label.includes(group_)
          ? item.label
          : `${item.label}（${group_}）`;
      const choice: ProxyChoice = {
        value: item.value,
        label,
        laneId,
        nodeRef:
          isNode && item.subscription_id !== undefined && item.node !== undefined
            ? { subscriptionId: item.subscription_id, node: item.node }
            : undefined,
        direct: isDirect,
      };
      if (isDirect) direct.push(choice);
      else rest.push(choice);
    }
  }
  return [...direct, { value: "", label: inheritLabel }, ...rest];
}

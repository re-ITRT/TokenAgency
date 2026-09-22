// @vitest-environment jsdom
/**
 * 网络三页（`/network`、`/network/nodes`、`/network/assignments`）的验收 —— [04 §5–§8]、[12 §6 F4]。
 *
 * 这一份的**首要**目的不是"能渲染"，而是把两条最容易做错的纪律钉死：
 *
 * 1. **I2：系统网络零副作用。** 网络页是"改系统代理 / DNS / 路由 / 网卡"最容易被顺手加进去的地方
 *    （"顺手加个开关多方便"）。所以这里有一条机械守卫：**扫页面源码里出现的每一条命令名**，
 *    要求它 ① 在 Rust 注册表里、② 在只读白名单里、③ 不在"会写系统/配置状态"的集合里。
 *    任何一次"顺手加个按钮"都会让这条测试变红，而不是靠评审时看一眼。
 * 2. **不猜。** 契约（`bindings.ts`）与 Rust 实现返回的形状**不一致**（见 `model.ts` 文件头），
 *    而且三页依赖的读取接口大多还没注册。所以：
 *    - 双形状都要读得出来（契约形状 + Rust 形状各一份夹具）；
 *    - 读不到的字段显示"未读取 / —"，**不填 0、不编节点**；
 *    - 空状态诚实（说明"配置里就是空的"与"接口没实现读不到"是两件事）；
 *    - 不存在任何"最终生效线路"的本地推算列 —— 那是后端的职责（两个真相源必然漂）。
 *
 * 另外钉住"没有占位页"：三个路由都必须解析到真实页面文件（`resolvePage` 不为 undefined）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import AssignmentsPage, {
  AssignmentsBody,
  type AssignmentsBodyProps,
} from "@/pages/network/assignments";
import {
  LANE_KINDS,
  MASKED,
  laneKindLabel,
  maskSubscriptionUrl,
  networkScope,
  readAssignments,
  readKernelPath,
  readLane,
  readSubscription,
  readTrafficMap,
  summarizeTraffic,
  type LaneRow,
  type SubscriptionRow,
} from "@/pages/network/model";
import NodesPage, { NodesBody, type NodesBodyProps } from "@/pages/network/nodes";
import NetworkPage, { NetworkBody, type NetworkBodyProps } from "@/pages/network";
import { resolvePage } from "@/app/pageRegistry";
import { routeByKey } from "@/app/routes";
import { registerMock, TokenAgencyError } from "@/shared/api/client";
import { registerBaselineMocks } from "@/shared/api/mock";
import { formatLocalTime } from "@/shared/format";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
});

afterEach(() => {
  cleanup();
});

// ================================================================ 夹具
//
// 用户数据里的 emoji **只在码点层面构造**：本文件自己被 `design.guard` 扫，
// 而"我们自己产出的文本不许有 emoji"这条对测试文件同样成立（[15 §2.1]）。

/** 一个真实的机场组名样本（[15 §2.2] 的 `U+1F1EF U+1F1F5` 日本旗 + 空格 + 日本）。 */
const USER_GROUP = `${String.fromCodePoint(0x1f1ef, 0x1f1f5)} 日本`;

/** 契约形状：`SubscriptionView`（`bindings.ts:710-719`）。 */
function contractSubscription(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    subscription: {
      id: "sub-jp",
      name: "机场A",
      url: "https://example.com/api/v1/client/subscribe?token=REPLACE_ME",
      user_agent: "clash-verge/v2.0.0",
      format: "auto",
      interval_s: 21_600,
      keep_versions: 3,
      filter: { include: "(?i)(日本|JP)", exclude: null },
      health_check: { enabled: true, url: "https://api.commandcode.ai/v1/models", interval_s: 60, tolerance_ms: 100 },
      enabled: true,
      provider_key: "sub-jp",
    },
    provider_key: "sub-jp",
    node_count: 12,
    group_count: 4,
    userinfo: { upload: 0, download: 8_400_000_000, total: 100_000_000_000 },
    last_update_ms: 1_770_000_000_000,
    parser: "clash-yaml@1",
    ...over,
  };
}

/** Rust 形状：`subscription_list` 实际返回 `Vec<Subscription>`（`catalog.rs`），**不是** `SubscriptionView`。 */
function rustSubscription(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: "sub-jp",
    name: "机场A",
    url: "https://example.com/api/v1/client/subscribe?token=REPLACE_ME",
    user_agent: "clash-verge/v2.0.0",
    format: "auto",
    interval_s: 21_600,
    keep_versions: 3,
    filter: { include: "(?i)(日本|JP)", exclude: null },
    health_check: { enabled: true, url: null, interval_s: 60, tolerance_ms: 100 },
    enabled: true,
    provider_key: "sub-jp",
    ...over,
  };
}

/** 契约形状：`LaneView`（`bindings.ts:679-686`）。 */
function contractLane(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    lane: {
      id: "lane-jp",
      name: "日本线",
      kind: "subscription_group",
      subscription: "sub-jp",
      group: USER_GROUP,
      group_type: "url-test",
      node: null,
      filter: null,
      exclude_filter: null,
      enabled: true,
    },
    type: "subscription_group",
    node_count: 3,
    current_node: "东京-03",
    latency_ms: 82,
    health: "ok",
    ...over,
  };
}

/** Rust 形状：`lane_list` 返回 `LaneView { lane, latest: HealthSample, channel_count }`（`catalog.rs`）。 */
function rustLane(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    lane: {
      id: "lane-jp",
      name: "日本线",
      kind: "subscription_group",
      subscription: "sub-jp",
      group: USER_GROUP,
      group_type: "url-test",
      node: null,
      filter: null,
      exclude_filter: null,
      enabled: true,
    },
    latest: {
      ts_ms: 1_770_000_500_000,
      lane_id: "lane-jp",
      node: "东京-03",
      latency_ms: 82,
      ok: true,
      status: 200,
    },
    channel_count: 2,
    ...over,
  };
}

/** 一份最小的配置树（`config_get` 的载荷；分配页的唯一数据源）。 */
function configDoc(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    net: { default_lane: "lane-direct" },
    lanes: [
      { id: "lane-direct", name: "直连（本机网络）", kind: "direct", enabled: true },
      { id: "lane-jp", name: "日本线", kind: "subscription_group", group: USER_GROUP, enabled: true },
    ],
    channels: [
      {
        id: "ch-commandcode",
        model: "deepseek-v4.1flash",
        provider: "p-commandcode",
        credential: "c-commandcode",
        lane: "lane-jp",
        upstream_model: "deepseek-v4.1flash",
        enabled: true,
      },
      {
        id: "ch-bytedance",
        model: "deepseek-v4.1flash",
        provider: "p-bytedance",
        credential: "c-bytedance",
        lane: null,
        upstream_model: "deepseek-v4.1flash",
        enabled: true,
      },
    ],
    credentials: [
      { id: "c-commandcode", provider: "p-commandcode", label: "command code · 按量", lane: "lane-jp", enabled: true },
      { id: "c-bytedance", provider: "p-bytedance", label: "字节方舟 · 500万 token/月", lane: null, enabled: true },
    ],
    providers: [
      { id: "p-commandcode", name: "command code", lane: "lane-jp", enabled: true },
      // 契约形状的 ProviderView **没有 lane 字段** → 这一级的 lane 应当是"未读取"，不是"未指定"
      { id: "p-bytedance", name: "字节方舟", enabled: true },
    ],
    policies: [
      {
        id: "pol-default",
        name: "默认",
        enabled: true,
        priority: 1000,
        action: { use_lane: null },
      },
      {
        id: "pol-night",
        name: "夜间直连",
        enabled: true,
        priority: 900,
        action: { use_lane: "lane-direct" },
      },
    ],
    ...over,
  };
}

// ================================================================ 渲染脚手架

function renderBody(node: React.ReactElement) {
  // 说明：订阅与线路区块现在**自带写操作**（新建 / 刷新 / 删除），它们用 `useCommandMutation`，
  // 所以即使只测展示也要有一个 QueryClient —— 但**仍然不需要 Router**（这两个区块里没有链接）。
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>{node}</ThemeProvider>
    </QueryClientProvider>,
  );
}

function networkProps(over: Partial<NetworkBodyProps> = {}): NetworkBodyProps {
  return {
    status: undefined,
    // `undefined` = `config_get` 还没读到 —— 与"读到了、但用户没配内核路径"不是一回事
    kernelPath: undefined,
    subscriptions: undefined,
    subscriptionsLoading: false,
    subscriptionsError: undefined,
    lanes: undefined,
    lanesLoading: false,
    lanesError: undefined,
    onRetry: () => {},
    ...over,
  };
}

function nodesProps(over: Partial<NodesBodyProps> = {}): NodesBodyProps {
  return {
    lanes: undefined,
    lanesLoading: false,
    lanesError: undefined,
    onRetry: () => {},
    ...over,
  };
}

function assignmentsProps(over: Partial<AssignmentsBodyProps> = {}): AssignmentsBodyProps {
  return { doc: undefined, isLoading: false, error: undefined, onRetry: () => {}, ...over };
}

/**
 * 整页渲染：必须包一层 Router —— 页头的两个导航按钮用的是 MUI `Button component={Link}`，
 * 而 `Link` 依赖 Router context（真实应用里由 `AppShell` 提供）。
 */
function renderPage(node: React.ReactElement) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <ThemeProvider theme={buildTheme("dark")}>{node}</ThemeProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

// ================================================================ ① 没有占位页

describe("三个 network 路由都不再落到占位壳", () => {
  it("routes.ts 的三个候选路径都能解析到真实页面文件", () => {
    for (const key of ["network", "network-nodes", "network-assignments"]) {
      const route = routeByKey(key);
      expect(route, `路由表里没有 ${key}`).toBeDefined();
      expect(resolvePage(route!.pages), `${key} 仍会落到 PagePlaceholder`).toBeDefined();
    }
  });
});

// ================================================================ ② I2：命令面守卫（机械）

/**
 * **系统 / 进程级**的写命令：网络页出现任何一条都是 I2 违规。
 *
 * 这不是"风格偏好"：`system_proxy` / `tun` / 开机自启 / 路由表都由这些命令间接改变，
 * 而"网络页"是最容易被顺手加进一个开关的地方。它们与"改 TokenAgency 自己的配置"不同类——
 * 改的是操作系统或进程生命周期（网关启停、内核启停、托盘、开机自启、配置目录监听）。
 */
const SYSTEM_MUTATING = new Set([
  "app_pause_gateway",
  "app_resume_gateway",
  "app_set_autostart",
  "app_repair_autostart",
  "config_set_file_watch_mode",
  "kernel_start",
  "kernel_stop",
  "kernel_restart",
  "kernel_reload_config",
  "kernel_set_provider",
  "tray_set_global_lane",
  "tray_set_active_policy",
  "tray_emergency",
]);

/**
 * 网络页**允许**的配置写命令：**只有订阅与线路这两族**，而且只有"实体的增删改 + 强制刷新"。
 *
 * 为什么这里开了一个口子：这两个功能（订阅 CRUD / 通道级线路）此前在真机上**用户点不到**
 * （task-23 的真机端到端验收结论），而它们本来就属于网络页。写的是 TokenAgency 自己的
 * 配置文件（`proxy_subscriptions` / `lanes`）与内核运行态，**系统网络一个字段都不动**——
 * I2 仍然成立，只是从"整页只读"收紧成"整页不许碰系统"。
 * 加一条就要在这里写下它的名字：故意的摩擦。
 */
const CONFIG_WRITES_ALLOWED = new Set([
  "subscription_upsert",
  "subscription_delete",
  "subscription_refresh",
  "lane_upsert",
  "lane_delete",
  "lane_set_node",
]);

/**
 * **其它**配置写命令：网络页仍然不许出现（通道 / 套餐 / 服务商 / 策略 / 模型 / 配置导入导出 / 日志）。
 * 它们属于别的页面，在这一页出现意味着"顺手加了个入口"，那是要有人解释的事。
 */
const OTHER_CONFIG_WRITES = new Set([
  "config_apply",
  "config_import_yaml",
  "config_rollback",
  "channel_upsert",
  "channel_delete",
  "channel_set_enabled",
  "channel_set_tags",
  "channel_bulk_assign_lane",
  "credential_upsert",
  "credential_delete",
  "credential_set_secret",
  "credential_set_enabled",
  "credential_clear_quota",
  "group_upsert",
  "group_delete",
  "group_set_sticky",
  "group_release_sticky",
  "lane_test",
  "model_upsert",
  "model_delete",
  "model_apply_import",
  "model_bulk_update",
  "onboarding_apply",
  "policy_upsert",
  "policy_delete",
  "policy_reorder",
  "policy_set_enabled",
  "provider_upsert",
  "provider_delete",
  "provider_test",
  "provider_echo",
  "subscription_rollback",
  "secrets_migrate",
  "log_set_mode",
  "log_set_retention",
  "log_clear",
  "usage_export_csv",
  "log_export",
]);

/** 网络页**只许**调这些（全部只读；它们只向后端问事实，不写任何东西）。 */
const READ_ONLY_ALLOWED = new Set([
  "app_status",
  "config_get",
  "lane_list",
  "subscription_list",
  "subscription_preview",
  "subscription_groups",
  "subscription_nodes",
  "subscription_traffic",
  "lane_assignments",
  "lane_pick_options",
  "lane_reverse_lookup",
]);

/** 白名单全集 = 只读 + 上面那两条订阅/线路写命令。 */
const ALLOWED = new Set([...READ_ONLY_ALLOWED, ...CONFIG_WRITES_ALLOWED]);

/**
 * 页面源码里**直接出现**的命令名字面量（当前实际引用的那几条）。
 *
 * 比白名单少一个 `app_status`：它走的是 `@/shared/api` 的 `useAppStatus()`
 * —— `queries.ts` 明写那是 `app_status` 的**唯一取数入口**（它还要把结果写进全局 store，
 * 状态条与设置页共用）。页面直接调 `useCommandQuery("app_status", …)` 会绕开那个入口，
 * 变成第二份取数逻辑，所以这里期望的就是少这一条。
 */
const DIRECTLY_REFERENCED = [
  "config_get",
  "lane_assignments",
  "lane_delete",
  "lane_list",
  "lane_pick_options",
  "lane_reverse_lookup",
  "lane_set_node",
  "lane_upsert",
  "subscription_delete",
  "subscription_groups",
  "subscription_list",
  "subscription_nodes",
  "subscription_preview",
  "subscription_refresh",
  "subscription_traffic",
  "subscription_upsert",
];

/**
 * 网络页的**全部**源码：`src/pages/network/` 下的 `.ts` / `.tsx`。
 *
 * 扫整个目录（而不是写死一份文件清单）：新增一个视图文件时守卫自动覆盖到它，
 * 不需要有人记得回来改这份清单 —— 否则"新加一个页面调了写命令"就会漏检。
 */
function networkSourceFiles(): string[] {
  const dir = path.resolve("src/pages/network");
  return readdirSync(dir)
    .filter((f) => /\.tsx?$/.test(f))
    .map((f) => `src/pages/network/${f}`)
    .sort();
}

/** 契约里的全部命令名（`bindings.ts` 的 `export type CommandMap = {` 直接成员）。 */
function contractCommands(): string[] {
  const text = readFileSync("src/shared/bindings.ts", "utf8");
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith("export type CommandMap"));
  const out: string[] = [];
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("};")) break;
    if (!line.startsWith("  ") || line.startsWith("   ")) continue;
    const m = line.slice(2).match(/^([a-z][a-z0-9_]*):\s*\{/);
    if (m?.[1] !== undefined) out.push(m[1]);
  }
  return out;
}

/** Rust **真的注册了**的命令名（`collect_commands![ … ]`）。 */
function registeredCommands(): string[] {
  const text = readFileSync("src-tauri/src/commands/mod.rs", "utf8");
  const lines = text.split(/\r?\n/);
  let inside = false;
  const out: string[] = [];
  for (const line of lines) {
    if (!inside) {
      if (line.includes("collect_commands![")) inside = true;
      continue;
    }
    const t = line.trim();
    if (t.startsWith("])")) break;
    const entry = t.replace(/,$/, "");
    const at = entry.lastIndexOf("::");
    if (at < 0) continue;
    const name = entry.slice(at + 2).trim();
    if (/^[a-z][a-z0-9_]*$/.test(name)) out.push(name);
  }
  return out;
}

/** 页面源码里**作为字符串字面量**出现的命令名（注释不算，避免注释里提一句就误判）。 */
function referencedCommands(
  files: readonly string[],
  universe: ReadonlySet<string>,
  read: (file: string) => string = (file) => readFileSync(file, "utf8"),
): string[] {
  const found: string[] = [];
  for (const file of files) {
    const text = read(file)
      .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
      .split(/\r?\n/)
      .map((l) => l.replace(/\/\/.*$/, ""))
      .join("\n");
    for (const m of text.matchAll(/"([a-z][a-z0-9_]*)"/g)) {
      const name = m[1];
      if (name !== undefined && universe.has(name)) found.push(name);
    }
  }
  return found;
}

describe("[I2] 网络页不碰系统网络（机械守卫，不靠自觉）", () => {
  const universe = new Set([...contractCommands(), ...registeredCommands()]);
  const files = networkSourceFiles();
  const referenced = referencedCommands(files, universe);

  it("白名单自身是自洽的（否则守卫在自我安慰）", () => {
    expect(files.length).toBeGreaterThanOrEqual(4);
    expect(contractCommands().length).toBeGreaterThan(30);
    expect(registeredCommands().length).toBeGreaterThan(10);
    for (const cmd of ALLOWED) {
      expect(SYSTEM_MUTATING.has(cmd), `${cmd} 同时被当成允许的白名单与系统写命令`).toBe(false);
    }
    // 允许的配置写命令必须与"其它写命令"互斥（一条命令不能既允许又禁止）
    for (const cmd of CONFIG_WRITES_ALLOWED) {
      expect(OTHER_CONFIG_WRITES.has(cmd), `${cmd} 同时在允许与禁止两张表里`).toBe(false);
    }
    // 自证检测器有牙：合成一段源码里的未注册/未授权命令必须被抓出来
    // （传一个假的 `read`，不去读一个并不存在的文件）
    expect(
      referencedCommands(["fake.tsx"], new Set(["lane_set_node"]), () => 'x("lane_set_node");'),
    ).toEqual(["lane_set_node"]);
    // 注释里提一句命令名**不算**调用点（否则文档里的举例会把守卫变红）
    expect(
      referencedCommands(["fake.tsx"], new Set(["lane_set_node"]), () => '// "lane_set_node"\n'),
    ).toEqual([]);
  });

  it("引用到的每条命令都在 Rust 注册表里（mock 掩盖不了真机失败）", () => {
    const registered = new Set(registeredCommands());
    const offenders = [...new Set(referenced)].filter((c) => !registered.has(c));
    expect(offenders, `这些命令 Rust 尚未注册，调了会在真机上失败：${offenders.join(", ")}`).toEqual(
      [],
    );
  });

  it("允许的写命令本身都在注册表里（白名单不许包含不存在的命令）", () => {
    const registered = new Set(registeredCommands());
    for (const cmd of CONFIG_WRITES_ALLOWED) {
      expect(registered.has(cmd), `${cmd} 被列进白名单但 Rust 没注册`).toBe(true);
      expect(contractCommands(), `${cmd} 不在契约里`).toContain(cmd);
    }
  });

  it("引用到的命令都在白名单里（只读 + 订阅/线路配置写），且没有任何系统写命令", () => {
    expect(referenced.length, "一条命令都没扫到 —— 扫描器的路径写错了").toBeGreaterThan(0);
    const outside = [...new Set(referenced)].filter((c) => !ALLOWED.has(c));
    expect(outside, `网络页引用了白名单外的命令：${outside.join(", ")}`).toEqual([]);
    const systemWrites = [...new Set(referenced)].filter((c) => SYSTEM_MUTATING.has(c));
    expect(
      systemWrites,
      `[I2] 网络页引用了会改系统/进程状态的命令：${systemWrites.join(", ")}`,
    ).toEqual([]);
    const otherWrites = [...new Set(referenced)].filter((c) => OTHER_CONFIG_WRITES.has(c));
    expect(otherWrites, `网络页引用了不属于它的写命令：${otherWrites.join(", ")}`).toEqual([]);
  });

  it("三个页面直接引用的命令正好是这张清单（多一条就要改这里）", () => {
    expect([...new Set(referenced)].sort()).toEqual([...DIRECTLY_REFERENCED].sort());
    // `app_status` 走共享的 `useAppStatus()`（唯一取数入口）→ 字面量不在页面里，
    // 但它仍然必须在只读白名单里（它确实是这一页要用的唯一运行态来源）
    expect(DIRECTLY_REFERENCED).not.toContain("app_status");
    expect(READ_ONLY_ALLOWED.has("app_status")).toBe(true);
  });
});

// ================================================================ ③ 读订阅

describe("读订阅：契约 SubscriptionView 与 Rust Subscription 两套形状都认", () => {
  it("契约形状：节点数/组数/最近更新/解析器都能读到", () => {
    const view = readSubscription(contractSubscription());
    expect(view.id).toBe("sub-jp");
    expect(view.name).toBe("机场A");
    expect(view.nodeCount).toBe(12);
    expect(view.groupCount).toBe(4);
    expect(view.lastUpdateMs).toBe(1_770_000_000_000);
    expect(view.parser).toBe("clash-yaml@1");
    expect(view.include).toBe("(?i)(日本|JP)");
    expect(view.healthUrl).toBe("https://api.commandcode.ai/v1/models");
    // 流量**不从这里读**：契约的 `userinfo` 只是 SubscriptionView 上的旧字段（Rust 的裸实体没有它），
    // 真源是 `subscription_traffic` 的持久化观测值 → 见 ⑮ 组
    expect(view.traffic).toBeUndefined();
  });

  it("Rust 形状（裸 Subscription）：解析结果读不到 → undefined，**不填 0**", () => {
    const view = readSubscription(rustSubscription());
    expect(view.id).toBe("sub-jp");
    expect(view.nodeCount).toBeUndefined();
    expect(view.groupCount).toBeUndefined();
    expect(view.lastUpdateMs).toBeUndefined();
    expect(view.parser).toBeUndefined();
    // 配置事实仍然读得到
    expect(view.intervalS).toBe(21_600);
    expect(view.keepVersions).toBe(3);
    expect(view.healthEnabled).toBe(true);
  });

  it("enabled 省略 = 启用（与 Rust 的 `#[serde(default)]` 同语义）", () => {
    expect(readSubscription(rustSubscription({ enabled: undefined })).enabled).toBe(true);
    expect(readSubscription(rustSubscription({ enabled: false })).enabled).toBe(false);
  });

  it("订阅地址里的密钥参数被遮蔽，其余原样（不复制、不外发订阅 token）", () => {
    expect(maskSubscriptionUrl("https://example.com/api/v1/client/subscribe?token=REPLACE_ME")).toBe(
      `https://example.com/api/v1/client/subscribe?token=${MASKED}`,
    );
    expect(maskSubscriptionUrl("https://a.example/sub?token=x&flag=1&key=y")).toBe(
      `https://a.example/sub?token=${MASKED}&flag=1&key=${MASKED}`,
    );
    expect(maskSubscriptionUrl("not a url")).toBe("not a url");
  });
});

// ================================================================ ④ 读线路

describe("读线路：契约 LaneView 与 Rust lane_list 两套形状都认", () => {
  it("契约形状：type / health / latency_ms / current_node / node_count", () => {
    const view = readLane(contractLane());
    expect(view.id).toBe("lane-jp");
    expect(view.name).toBe("日本线");
    expect(view.kind).toBe("subscription_group");
    expect(view.health).toBe("ok");
    expect(view.latencyMs).toBe(82);
    expect(view.currentNode).toBe("东京-03");
    expect(view.nodeCount).toBe(3);
    // Rust 才有的字段在契约形状里读不到 —— 不猜
    expect(view.channelCount).toBeUndefined();
    expect(view.sampledAtMs).toBeUndefined();
  });

  it("Rust 形状：从 latest 健康样本取节点/延迟/结果/状态码，并读到 channel_count", () => {
    const view = readLane(rustLane());
    expect(view.health).toBe("ok");
    expect(view.latencyMs).toBe(82);
    expect(view.currentNode).toBe("东京-03");
    expect(view.sampledAtMs).toBe(1_770_000_500_000);
    expect(view.sampleStatus).toBe(200);
    expect(view.channelCount).toBe(2);
    // 订阅解析结果（节点数）仍然读不到
    expect(view.nodeCount).toBeUndefined();
  });

  it("样本 ok=false → down；没有样本 → unknown（不因为「没报错」就画绿点）", () => {
    expect(readLane(rustLane({ latest: { ts_ms: 1, lane_id: "lane-jp", ok: false } })).health).toBe(
      "down",
    );
    expect(readLane(rustLane({ latest: null })).health).toBe("unknown");
    expect(readLane(rustLane({ latest: undefined })).health).toBe("unknown");
    // 契约里的 "degraded" 原样保留（后端说了算，前端不改写）
    expect(readLane(contractLane({ health: "degraded" })).health).toBe("degraded");
  });

  it("节点数与通道数**不混用**：两个不同的问题，两个不同的字段", () => {
    const rust = readLane(rustLane());
    expect(rust.channelCount).toBe(2);
    expect(rust.nodeCount).toBeUndefined();
    const contract = readLane(contractLane());
    expect(contract.nodeCount).toBe(3);
    expect(contract.channelCount).toBeUndefined();
  });

  it("五种线路类型都有文案（契约新增一种时这里会红）", () => {
    expect(LANE_KINDS).toHaveLength(5);
    for (const kind of LANE_KINDS) {
      expect(laneKindLabel(kind), `${kind} 没有文案`).toBeDefined();
    }
    // 词表外的类型不给文案 → 页面原样显示后端给的名字（不改写成"未知"）
    expect(laneKindLabel("quantum_tunnel")).toBeUndefined();
    expect(laneKindLabel(undefined)).toBeUndefined();
  });
});

// ================================================================ ⑤ 读分配（五级显式值 + 倒排）

describe("读分配：五级显式值 + 显式引用倒排（不算最终生效线路）", () => {
  const view = readAssignments(configDoc());

  it("四级实体各自的显式 lane 都读出来，null 与「字段不存在」严格区分", () => {
    const bytedance = view.providers.find((p) => p.id === "p-bytedance");
    const commandcode = view.providers.find((p) => p.id === "p-commandcode");
    expect(commandcode?.lane?.laneName).toBe("日本线");
    expect(bytedance?.lane).toBeUndefined();
    // 契约 ProviderView 里根本没有 lane 字段 → 未读取（不是"未指定"）
    expect(bytedance?.laneReadable).toBe(false);
    expect(commandcode?.laneReadable).toBe(true);

    const ch = view.channels.find((c) => c.id === "ch-bytedance");
    expect(ch?.lane).toBeUndefined();
    expect(ch?.credentialLabel).toBe("字节方舟 · 500万 token/月");
  });

  it("策略级只列 action.use_lane 有值的策略，并保留优先级", () => {
    expect(view.policies.map((p) => p.id)).toEqual(["pol-night"]);
    expect(view.policies[0]?.lane?.laneName).toBe("直连（本机网络）");
    expect(view.policies[0]?.priority).toBe(900);
  });

  it("全局默认读 net.default_lane 并解析成名字", () => {
    expect(view.defaultLane?.laneId).toBe("lane-direct");
    expect(view.defaultLane?.laneName).toBe("直连（本机网络）");
  });

  it("倒排只统计**显式**引用；没人引用的线路也要列出来（否则用户以为它被用了）", () => {
    const jp = view.reverse.find((r) => r.laneId === "lane-jp");
    expect(jp?.channels).toEqual(["ch-commandcode"]);
    expect(jp?.credentials).toEqual(["command code · 按量"]);
    expect(jp?.providers).toEqual(["command code"]);
    expect(jp?.policies).toEqual([]);

    const direct = view.reverse.find((r) => r.laneId === "lane-direct");
    expect(direct?.policies).toEqual(["夜间直连"]);
    // channel.lane 为 null 的通道**不是**直连的显式引用 —— 它是"继承上一级"
    expect(direct?.channels).toEqual([]);
  });

  it("config_get 少了某个字段时标记为「未读取」，而不是当成空配置", () => {
    const partial = readAssignments({ net: { default_lane: "lane-direct" } });
    expect(partial.has.channels).toBe(false);
    expect(partial.has.providers).toBe(false);
    expect(partial.channels).toEqual([]);
    expect(readAssignments(configDoc()).has.channels).toBe(true);
  });

  it("接口给的整份数据缺失时不抛错（页面显示未读取，不白屏）", () => {
    expect(() => readAssignments(undefined)).not.toThrow();
    expect(readAssignments(undefined).has.net).toBe(false);
    expect(readAssignments(undefined).reverse).toEqual([]);
  });
});

// ================================================================ ⑥ /network 渲染

describe("/network：有数据 / 空 / 错误三种状态都说实话", () => {
  const subs: SubscriptionRow[] = [readSubscription(contractSubscription())];
  const lanes: LaneRow[] = [readLane(contractLane())];

  it("零副作用声明常驻（I2 写在用户看得见的地方）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    const notice = screen.getByTestId("network-zero-side-effect");
    expect(notice.textContent).toContain("只展示，不改系统网络");
    expect(notice.textContent).toContain("不设置系统代理");
  });

  it("订阅表：名称 / 遮蔽后的地址 / 节点数 / 组数都画出来", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    const table = within(screen.getByTestId("network-subscriptions-table"));
    expect(table.getByText("机场A")).toBeTruthy();
    expect(
      table.getByText(`https://example.com/api/v1/client/subscribe?token=${MASKED}`),
    ).toBeTruthy();
    expect(table.getByText("12")).toBeTruthy();
    expect(table.getByText("4")).toBeTruthy();
  });

  it("Rust 形状的订阅：节点数显示「未读取」并说明原因，不填 0", () => {
    renderBody(
      <NetworkBody
        {...networkProps({ subscriptions: [readSubscription(rustSubscription())], lanes })}
      />,
    );
    const table = within(screen.getByTestId("network-subscriptions-table"));
    expect(table.getAllByText("未读取").length).toBeGreaterThanOrEqual(2);
    expect(table.queryByText("0")).toBeNull();
    expect(screen.getByTestId("network-subscriptions-hint").textContent).toContain(
      "subscription_nodes",
    );
  });

  it("线路表：类型用文字、健康用圆点+文字、用户数据里的组名原样呈现", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    const table = within(screen.getByTestId("network-lanes-table"));
    expect(table.getByText("日本线")).toBeTruthy();
    expect(table.getByText("订阅里的组")).toBeTruthy();
    expect(table.getByText("82ms")).toBeTruthy();
    expect(table.getByText("成功")).toBeTruthy();
    // 用户数据原样：emoji 不被过滤、不被替换（[15 §2.2]）
    expect(table.getByText(USER_GROUP)).toBeTruthy();
  });

  it("订阅为空 / 线路为空：给出诚实的空状态，不画假行", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: [], lanes: [] })} />);
    expect(screen.getByTestId("network-subscriptions-empty").textContent).toContain(
      "配置里还没有订阅",
    );
    expect(screen.getByTestId("network-lanes-empty").textContent).toContain("配置里还没有线路");
    expect(screen.queryByTestId("network-subscriptions-table")).toBeNull();
    expect(screen.queryByTestId("network-lanes-table")).toBeNull();
  });

  it("加载中不冒充空状态（不把「还没查到」说成「没有」）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptionsLoading: true, lanesLoading: true })} />);
    expect(screen.queryByTestId("network-subscriptions-empty")).toBeNull();
    expect(screen.queryByTestId("network-lanes-empty")).toBeNull();
  });

  it("读失败：带 TA 码 + 重试入口，不吞错误", () => {
    const onRetry = () => {};
    renderBody(
      <NetworkBody
        {...networkProps({
          subscriptionsError: new Error("[TA-9001] 非 Tauri 环境且未注册 mock：subscription_list"),
          lanesError: new Error("[TA-4001] 配置不可用"),
          onRetry,
        })}
      />,
    );
    expect(screen.getByTestId("network-subscriptions-error").textContent).toContain("TA-9001");
    expect(screen.getByTestId("network-lanes-error").textContent).toContain("TA-4001");
    expect(screen.getAllByRole("button", { name: "重试" }).length).toBeGreaterThanOrEqual(2);
  });

  it("当前出口：解析成线路名；解析不了就显示后端给的 id，不猜", () => {
    renderBody(
      <NetworkBody
        {...networkProps({
          subscriptions: subs,
          lanes,
          status: {
            gateway: "running",
            kernel: { state: "ready", restarts: 0 },
            version: "0.1.0",
            uptime_s: 60,
            default_lane_id: "lane-jp",
            current_node: "东京-03",
          },
        })}
      />,
    );
    const exit = within(screen.getByTestId("network-current-exit"));
    expect(exit.getByText("日本线")).toBeTruthy();
    expect(exit.getByText("东京-03")).toBeTruthy();
    cleanup();

    renderBody(
      <NetworkBody
        {...networkProps({
          lanes,
          status: {
            gateway: "paused",
            kernel: { state: "stopped", restarts: 0 },
            version: "0.1.0",
            uptime_s: 0,
            default_lane_id: "lane-unknown-x",
          },
        })}
      />,
    );
    expect(within(screen.getByTestId("network-current-exit")).getByText("lane-unknown-x")).toBeTruthy();
  });

  it("一条命令都没调时（status 也没有）不白屏：出口区显示未读取", () => {
    renderBody(<NetworkBody {...networkProps()} />);
    expect(screen.getByTestId("network-current-exit").textContent).toContain("未读取");
  });
});

// ================================================================ ⑦ /network/nodes 渲染

describe("/network/nodes：只有拿得到的证据才画", () => {
  it("有健康样本：节点 / 延迟 / 状态码 / 采样时间都在", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);
    const table = within(screen.getByTestId("network-nodes-samples"));
    expect(table.getByText("日本线")).toBeTruthy();
    expect(table.getByText("东京-03")).toBeTruthy();
    expect(table.getByText("82ms")).toBeTruthy();
    expect(table.getByText("200")).toBeTruthy();
  });

  it("没有任何样本：诚实空状态，并说明本页不自己测速、不编延迟", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(contractLane({ health: "unknown" }))] })} />);
    // 契约形状的线路没有 latest 样本 → 采样时间未知
    const samples = within(screen.getByTestId("network-nodes-samples"));
    expect(samples.getByText("未知")).toBeTruthy();
    cleanup();

    renderBody(<NodesBody {...nodesProps({ lanes: [] })} />);
    expect(screen.getByTestId("network-nodes-empty").textContent).toContain("没有任何线路带健康样本");
    expect(screen.getByTestId("network-nodes-empty").textContent).toContain("不自己探测");
  });

  it("声明的节点来源原样呈现（用户数据里的字符一个都不改）", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(contractLane())] })} />);
    const declared = within(screen.getByTestId("network-nodes-declared"));
    expect(declared.getByText(USER_GROUP)).toBeTruthy();
  });

  it("说明完整节点清单为什么缺席（点名缺失的后端接口），不伪造节点", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);
    const missing = screen.getByTestId("network-nodes-missing");
    expect(missing.textContent).toContain("subscription_nodes");
    expect(missing.textContent).toContain("不伪造节点");
  });

  it("读失败：带 TA 码 + 重试", () => {
    renderBody(<NodesBody {...nodesProps({ lanesError: new Error("[TA-9001] 未注册 mock：lane_list") })} />);
    expect(screen.getByTestId("network-nodes-error").textContent).toContain("TA-9001");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });
});

// ================================================================ ⑧ /network/assignments 渲染

describe("/network/assignments：只显示显式写了的值", () => {
  it("五级都在，并明确写出「不自己算最终生效线路」", () => {
    renderBody(<AssignmentsBody {...assignmentsProps({ doc: configDoc() as never })} />);
    for (const testid of [
      "network-assignments-policies",
      "network-assignments-channels",
      "network-assignments-credentials",
      "network-assignments-providers",
      "network-assignments-default",
    ]) {
      expect(screen.getByTestId(testid), `缺少区块 ${testid}`).toBeTruthy();
    }
    expect(screen.getByTestId("network-assignments-no-resolve").textContent).toContain("不自己算");
  });

  it("通道级：显式 lane 显示线路名；null 显示「未指定（继承上一级）」", () => {
    renderBody(<AssignmentsBody {...assignmentsProps({ doc: configDoc() as never })} />);
    const channels = within(screen.getByTestId("network-assignments-channels"));
    expect(channels.getByText("日本线")).toBeTruthy();
    expect(channels.getByText("未指定（继承上一级）")).toBeTruthy();
  });

  it("服务商级：契约形状读不到 lane → 「未读取」，不与「未指定」混为一谈", () => {
    renderBody(<AssignmentsBody {...assignmentsProps({ doc: configDoc() as never })} />);
    const providers = within(screen.getByTestId("network-assignments-providers"));
    expect(providers.getByText("未读取")).toBeTruthy();
    expect(providers.getByText("日本线")).toBeTruthy();
  });

  it("倒排表列出显式引用，并写清它不是「实际走了这条线」", () => {
    renderBody(<AssignmentsBody {...assignmentsProps({ doc: configDoc() as never })} />);
    const reverse = within(screen.getByTestId("network-assignments-reverse"));
    expect(reverse.getByText("ch-commandcode")).toBeTruthy();
    expect(screen.getByTestId("network-assignments-reverse-hint").textContent).toContain(
      "lane_reverse_lookup",
    );
    // 没有显式引用者的线路也要出现（否则用户会以为它没人用）
    expect(reverse.getByText("直连（本机网络）")).toBeTruthy();
  });

  it("配置里没有通道时给诚实的空状态（不是「未读取」）", () => {
    renderBody(
      <AssignmentsBody {...assignmentsProps({ doc: configDoc({ channels: [] }) as never })} />,
    );
    expect(screen.getByTestId("network-assignments-channels-empty").textContent).toContain(
      "配置里还没有通道",
    );
  });

  it("config_get 没给通道字段时显示「未读取」并说明原因", () => {
    renderBody(
      <AssignmentsBody
        {...assignmentsProps({ doc: { net: { default_lane: "lane-direct" } } as never })}
      />,
    );
    expect(screen.getByTestId("network-assignments-channels-unread").textContent).toContain("config_get");
  });

  it("读失败：带 TA 码 + 重试", () => {
    renderBody(<AssignmentsBody {...assignmentsProps({ error: new Error("[TA-1004] 请求格式错误") })} />);
    expect(screen.getByTestId("network-assignments-error").textContent).toContain("TA-1004");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });
});

// ================================================================ ⑨ 整页（浏览器开发路径）

describe("整页在 mock 基准场景下可用（不依赖 Rust/Tauri）", () => {
  it("/network 渲染页头 + mock 的订阅与线路，且不再走占位壳", async () => {
    renderPage(<NetworkPage />);
    expect((await screen.findByText("网络")).tagName).toBe("H1");
    expect(screen.queryByTestId("page-placeholder")).toBeNull();
    expect(await screen.findByTestId("network-subscriptions-table")).toBeTruthy();
    expect(await screen.findByTestId("network-lanes-table")).toBeTruthy();
  });

  it("/network/nodes 渲染页头 + 来自 lane_list 的健康样本", async () => {
    renderPage(<NodesPage />);
    expect((await screen.findByText("节点列表")).tagName).toBe("H1");
    expect(await screen.findByTestId("network-nodes-samples")).toBeTruthy();
  });

  it("/network/assignments 渲染页头 + 来自 config_get 的默认线路", async () => {
    renderPage(<AssignmentsPage />);
    expect((await screen.findByText("线路分配")).tagName).toBe("H1");
    expect(await screen.findByTestId("network-assignments-default")).toBeTruthy();
  });
});

// ================================================================ ⑩ 文案回归

describe("文案里不留未替换的占位符", () => {
  it("三页渲染出来的正文里不该出现双花括号占位符", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: [readSubscription(contractSubscription())], lanes: [readLane(contractLane())] })} />);
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
    cleanup();
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
    cleanup();
    renderBody(<AssignmentsBody {...assignmentsProps({ doc: configDoc() as never })} />);
    expect(document.body.textContent ?? "").not.toMatch(/\{\{\w+\}\}/);
  });
});

// ================================================================ ⑪ 订阅写界面（真调用 + 后端真值）
//
// 这一组测三件**只有机械检查才抓得住**的事：
// 1. 控件真的在页面上（`renderPage` 走真实页面，而不是只渲染展示件）—— 真机验收里
//    "用户点不到" 就是坏在这里，而组件单测完全看不出来；
// 2. 入参形状对（mock **记录**入参并把它们断言下来）：snake_case、只带用户真的填了的字段。
//    写界面最容易漏的就是这一步：mock 不校验入参，真机上才炸；
// 3. 成功区显示的是**后端返回的对象**，不是前端拼的一行"添加成功" —— 所以下面每个 mock
//    都故意返回与输入不同的值，只有显示的是后端真值，这些断言才会过。

/** 记录入参的 mock：返回 `result`，同时把每次调用的入参留下来。 */
function recordingMock(result: unknown): {
  calls: Record<string, unknown>[];
  handler: (args: Record<string, unknown> | undefined) => unknown;
} {
  const calls: Record<string, unknown>[] = [];
  return {
    calls,
    handler: (args) => {
      calls.push(args ?? {});
      return result;
    },
  };
}

/** 往一个字段里打字：testId 挂在字段容器上（与 `settings.test.tsx` 同法），再按 role 取输入框。 */
function typeInto(testId: string, value: string): void {
  const input = within(screen.getByTestId(testId)).getByRole("textbox") as HTMLInputElement;
  fireEvent.change(input, { target: { value } });
}

describe("⑪ /network 订阅写界面：点了真的调命令，并且显示后端返回的对象", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("新建订阅 → subscription_upsert：入参 snake_case、只带填了的字段，结果区显示后端返回值", async () => {
    let listCalls = 0;
    registerMock("subscription_list", () => {
      listCalls += 1;
      return [contractSubscription()];
    });
    const upsert = recordingMock(
      rustSubscription({
        id: "sub-backend",
        name: "后端改名",
        url: "https://backend.example/sub?token=ZZZ",
        keep_versions: 7,
        enabled: false,
      }),
    );
    registerMock("subscription_upsert", upsert.handler);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-add-id");
    typeInto("network-subscriptions-add-id", "sub-x");
    typeInto("network-subscriptions-add-name", "机场X");
    typeInto("network-subscriptions-add-url", "https://example.com/sub?token=abc");
    typeInto("network-subscriptions-add-include", "(?i)JP");
    fireEvent.click(screen.getByTestId("network-subscriptions-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const sub = upsert.calls[0]?.subscription as Record<string, unknown>;
    expect(sub.id).toBe("sub-x");
    expect(sub.name).toBe("机场X");
    expect(sub.url).toBe("https://example.com/sub?token=abc");
    // 表单里显示的值就是发出去的值
    expect(sub.format).toBe("auto");
    expect(sub.interval_s).toBe(21_600);
    expect(sub.enabled).toBe(true);
    expect(sub.filter).toEqual({ include: "(?i)JP" });
    // 表单里**没有**的字段不冒充：后端 `#[serde(default)]` 才是权威
    expect("user_agent" in sub).toBe(false);
    expect("health_check" in sub).toBe(false);
    // 入参 key 一律 snake_case（104 条命令都显式声明了 rename_all = "snake_case"）
    for (const key of Object.keys(sub)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);

    const result = await screen.findByTestId("network-subscriptions-write-result");
    expect(result.textContent).toContain("后端改名");
    expect(result.textContent).toContain("sub-backend");
    expect(result.textContent).toContain("7");
    expect(result.textContent).toContain("已禁用");
    // 地址照旧走遮蔽（后端返回的 url 里带 token）
    expect(result.textContent).toContain(`https://backend.example/sub?token=${MASKED}`);

    // 保存成功后列表被 invalidate 重新拉过 —— 否则用户看不到自己刚存的东西
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("预览 → subscription_preview：只看一眼不保存，表单里没有的 UA 参数不发", async () => {
    const preview = recordingMock({
      groups: ["G1", "G2", "G3"],
      nodes: ["N1", "N2"],
      sample: [{ name: "N1", type: "vmess" }],
    });
    registerMock("subscription_preview", preview.handler);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-preview");
    typeInto("network-subscriptions-add-url", "https://example.com/sub?token=abc");
    fireEvent.click(screen.getByTestId("network-subscriptions-preview"));

    await waitFor(() => expect(preview.calls).toHaveLength(1));
    const args = preview.calls[0]!;
    expect(args.url).toBe("https://example.com/sub?token=abc");
    expect(args.format).toBe("auto");
    // 没有 UA 字段 → 不发这个 key（后端用自己的默认 UA，不在前端复刻一份）
    expect("user_agent" in args).toBe(false);

    const result = await screen.findByTestId("network-subscriptions-preview-result");
    expect(result.textContent).toContain("3 个分组");
    expect(result.textContent).toContain("2 个节点");
    expect(result.textContent).toContain("N1");
  });

  it("刷新某一行 → subscription_refresh：{id, force: true}，结果区显示后端 RefreshResult", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    const refresh = recordingMock({
      ok: false,
      node_count: 5,
      group_count: 2,
      error: "机场返回 HTML",
    });
    registerMock("subscription_refresh", refresh.handler);

    renderPage(<NetworkPage />);
    const buttons = await screen.findAllByTestId("network-subscriptions-refresh");
    fireEvent.click(buttons[0]!);

    await waitFor(() => expect(refresh.calls).toHaveLength(1));
    // "刷新" 是用户主动点的 → force: true（否则 interval 没到就被静默跳过，按钮看起来是坏的）
    expect(refresh.calls[0]).toEqual({ id: "sub-jp", force: true });

    const result = await screen.findByTestId("network-subscriptions-refresh-result");
    expect(result.textContent).toContain("5");
    expect(result.textContent).toContain("2");
    expect(result.textContent).toContain("机场返回 HTML");
  });

  it("删除某一行 → subscription_delete({id})，成功后列表被重新拉取", async () => {
    let listCalls = 0;
    registerMock("subscription_list", () => {
      listCalls += 1;
      return [contractSubscription()];
    });
    const remove = recordingMock(undefined);
    registerMock("subscription_delete", remove.handler);

    renderPage(<NetworkPage />);
    const buttons = await screen.findAllByTestId("network-subscriptions-delete");
    fireEvent.click(buttons[0]!);

    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "sub-jp" });
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("写失败：后端 TA 码原样显示，不吞错、不改写成一句「保存失败」", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_upsert", () => {
      throw new TokenAgencyError({ code: "TA-4001", message: "配置不可用" });
    });

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-add-id");
    typeInto("network-subscriptions-add-id", "sub-x");
    typeInto("network-subscriptions-add-name", "机场X");
    typeInto("network-subscriptions-add-url", "https://example.com/sub?token=abc");
    fireEvent.click(screen.getByTestId("network-subscriptions-add-submit"));

    const error = await screen.findByTestId("network-subscriptions-write-error");
    expect(error.textContent).toContain("TA-4001");
    expect(error.textContent).toContain("配置不可用");
    expect(screen.queryByTestId("network-subscriptions-write-result")).toBeNull();
  });

  it("必填缺失时**不调命令**（不把空 id / 空地址写进配置），并在结果区说清缺什么", async () => {
    const upsert = recordingMock(rustSubscription());
    registerMock("subscription_upsert", upsert.handler);

    renderPage(<NetworkPage />);
    fireEvent.click(await screen.findByTestId("network-subscriptions-add-submit"));

    expect((await screen.findByTestId("network-subscriptions-write-error")).textContent).toContain(
      "必填",
    );
    expect(upsert.calls).toHaveLength(0);
  });
});

// ================================================================ ⑫ 线路写界面（真调用 + 后端真值）
//
// 与 ⑪ 同一套打法，但多一条**后端的规则**要钉住：`Lane::missing_kind_field()`
// （`crates/ta-core/src/domain/behavior.rs`）规定"非 direct 必须有 subscription；
// subscription_group 要 group；subscription_node 要 node；filter 要 filter"。
// 前端要么照这套规则先挡一次（这里测的就是这个），要么把注定被拒的请求发出去。

/** MUI 的 Select 在 jsdom 里是个 div：先 `mouseDown` 打开菜单，再点角色为 option 的项。 */
async function chooseOption(testId: string, optionName: RegExp | string): Promise<void> {
  const box = screen.getByTestId(testId);
  fireEvent.mouseDown(within(box).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name: optionName }));
}

describe("⑫ /network 线路写界面：lane_upsert / lane_delete / lane_set_node 挂在真控件上", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("新建直连线路 → lane_upsert：kind=direct 时不发 subscription 这类用不上的字段", async () => {
    let listCalls = 0;
    registerMock("lane_list", () => {
      listCalls += 1;
      return [contractLane()];
    });
    const upsert = recordingMock(
      rustLane({
        lane: { id: "lane-backend", name: "后端线路", kind: "direct", enabled: false },
      }),
    );
    registerMock("lane_upsert", upsert.handler);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-lanes-add-id");
    typeInto("network-lanes-add-id", "lane-x");
    typeInto("network-lanes-add-name", "我的线路");
    fireEvent.click(screen.getByTestId("network-lanes-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const lane = upsert.calls[0]?.lane as Record<string, unknown>;
    expect(lane.id).toBe("lane-x");
    expect(lane.name).toBe("我的线路");
    expect(lane.kind).toBe("direct");
    expect(lane.enabled).toBe(true);
    // 直连不需要订阅：表单里没显示的字段就不发
    expect("subscription" in lane).toBe(false);
    expect("group" in lane).toBe(false);
    for (const key of Object.keys(lane)) expect(key).toMatch(/^[a-z][a-z0-9_]*$/);

    const result = await screen.findByTestId("network-lanes-write-result");
    expect(result.textContent).toContain("后端线路");
    expect(result.textContent).toContain("lane-backend");
    expect(result.textContent).toContain("已禁用");
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("kind=订阅里的组：先按后端规则点明缺 group，补齐后入参带 subscription / group / group_type", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_groups", () => [
      { name: "日本", type: "url-test" as const, node_count: 3 },
    ]);
    const upsert = recordingMock(
      rustLane({
        lane: {
          id: "lane-g",
          name: "组线路",
          kind: "subscription_group",
          subscription: "sub-jp",
          group: "日本",
          enabled: true,
        },
      }),
    );
    registerMock("lane_upsert", upsert.handler);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-lanes-add-id");
    typeInto("network-lanes-add-id", "lane-g");
    typeInto("network-lanes-add-name", "组线路");
    await chooseOption("network-lanes-add-kind", "订阅里的组");
    await chooseOption("network-lanes-add-subscription", "机场A");

    // 订阅解析结果（subscription_groups）读出来供**照抄**：不猜组名，也不替用户改字
    await waitFor(() =>
      expect(screen.getByTestId("network-lanes-known-groups").textContent).toContain("日本"),
    );

    fireEvent.click(screen.getByTestId("network-lanes-add-submit"));
    const error = await screen.findByTestId("network-lanes-write-error");
    expect(error.textContent).toContain("还缺必填项");
    expect(error.textContent).toContain("group");
    expect(upsert.calls).toHaveLength(0);

    typeInto("network-lanes-add-group", "日本");
    await chooseOption("network-lanes-add-group-type", /url-test/);
    fireEvent.click(screen.getByTestId("network-lanes-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const lane = upsert.calls[0]?.lane as Record<string, unknown>;
    expect(lane.subscription).toBe("sub-jp");
    expect(lane.group).toBe("日本");
    expect(lane.group_type).toBe("url-test");
  });

  it("每一行：删除调 lane_delete({id})，指定节点调 lane_set_node({id, node}) 并显示被接受的钉住", async () => {
    let listCalls = 0;
    registerMock("lane_list", () => {
      listCalls += 1;
      return [rustLane()];
    });
    const remove = recordingMock(undefined);
    const setNode = recordingMock(undefined);
    registerMock("lane_delete", remove.handler);
    registerMock("lane_set_node", setNode.handler);

    renderPage(<NetworkPage />);
    const nodeField = (await screen.findAllByTestId("network-lanes-set-node"))[0]!;
    fireEvent.change(within(nodeField).getByRole("textbox"), { target: { value: "东京-03" } });
    fireEvent.click(screen.getAllByTestId("network-lanes-set-node-submit")[0]!);

    await waitFor(() => expect(setNode.calls).toHaveLength(1));
    // `lane_set_node` 只收 id + node（钉住是内核运行态，不改配置）
    expect(setNode.calls[0]).toEqual({ id: "lane-jp", node: "东京-03" });
    expect((await screen.findByTestId("network-lanes-set-node-result")).textContent).toContain(
      "东京-03",
    );

    fireEvent.click(screen.getAllByTestId("network-lanes-delete")[0]!);
    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "lane-jp" });
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("写失败：lane_upsert 的 TA 码原样显示（引用完整性拒绝也走这条路）", async () => {
    registerMock("lane_list", () => [contractLane()]);
    registerMock("lane_upsert", () => {
      throw new TokenAgencyError({ code: "TA-5001", message: "线路被 net.default_lane 引用" });
    });

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-lanes-add-id");
    typeInto("network-lanes-add-id", "lane-x");
    typeInto("network-lanes-add-name", "我的线路");
    fireEvent.click(screen.getByTestId("network-lanes-add-submit"));

    const error = await screen.findByTestId("network-lanes-write-error");
    expect(error.textContent).toContain("TA-5001");
    expect(error.textContent).toContain("net.default_lane");
  });
});

// ================================================================ ⑬ /network/assignments：后端算的五级链路
//
// 这一页**刻意不自己算**"最终生效线路"（算两遍必然与后端漂）。所以它必须把后端算的那份
// 摆出来：`lane_assignments`（最终走哪条线 + 来自哪一级）、`lane_pick_options`（候选）、
// `lane_reverse_lookup`（"删 lane 之前先看谁在引用它"）。这一组测的就是这三个入口真的在页面上。

describe("⑬ /network/assignments：显示后端算的五级链路，而不是前端自己推一遍", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("lane_assignments：每条通道的最终线路与来源级别都画出来", async () => {
    registerMock("lane_assignments", () => [
      { model_id: "m-backend", channel_id: "ch-backend", lane_id: "lane-backend", source: "policy" as const },
    ]);

    renderPage(<AssignmentsPage />);
    const table = within(await screen.findByTestId("network-assignments-resolved"));
    expect(await table.findByText("ch-backend")).toBeTruthy();
    expect(table.getByText("lane-backend")).toBeTruthy();
    // source 也翻成用户能读的级别名，而不是把契约值直接印出来
    expect(table.getByText("策略级")).toBeTruthy();
  });

  it("lane_pick_options：候选线路按订阅分组列出（含每组的节点数）", async () => {
    registerMock("lane_pick_options", () => [
      {
        group: "后端分组",
        options: [
          { lane_id: "lane-a", name: "线路甲", kind: "direct" as const },
          { lane_id: "lane-b", name: "线路乙", kind: "subscription_group" as const, node_count: 7 },
        ],
      },
    ]);

    renderPage(<AssignmentsPage />);
    const list = within(await screen.findByTestId("network-assignments-pick-options"));
    expect(await list.findByText("后端分组")).toBeTruthy();
    expect(list.getByText("线路甲")).toBeTruthy();
    expect(list.getByText("线路乙")).toBeTruthy();
    expect(list.getByText("节点数 7")).toBeTruthy();
  });

  it("lane_reverse_lookup：按 lane id 查引用，入参是 lane_id，结果显示后端返回的通道与模型", async () => {
    registerMock("lane_list", () => [contractLane()]);
    const lookup = recordingMock({ channel_ids: ["ch-1", "ch-2"], model_ids: ["m-1"] });
    registerMock("lane_reverse_lookup", lookup.handler);

    renderPage(<AssignmentsPage />);
    const button = (await screen.findAllByTestId("network-assignments-reverse-lookup"))[0]!;
    fireEvent.click(button);

    await waitFor(() => expect(lookup.calls).toHaveLength(1));
    expect(lookup.calls[0]).toEqual({ lane_id: "lane-jp" });

    const result = await screen.findByTestId("network-assignments-reverse-lookup-result");
    expect(result.textContent).toContain("ch-1");
    expect(result.textContent).toContain("ch-2");
    expect(result.textContent).toContain("m-1");
  });

  it("查引用失败：TA 码原样显示（不吞错）", async () => {
    registerMock("lane_list", () => [contractLane()]);
    registerMock("lane_reverse_lookup", () => {
      throw new TokenAgencyError({ code: "TA-1004", message: "请求格式错误" });
    });

    renderPage(<AssignmentsPage />);
    fireEvent.click((await screen.findAllByTestId("network-assignments-reverse-lookup"))[0]!);

    expect(
      (await screen.findByTestId("network-assignments-reverse-lookup-error")).textContent,
    ).toContain("TA-1004");
  });
});

// ================================================================ ⑭ /network 是**选装**：没配置就不显示（[18 §2]）
//
// 用户原话：「我们的网络代理模块也需要设置，**如果用户没有配置就不显示**，token 网络代理是一个**选装功能**」。
//
// 「未配置」的**唯一口径**（[18 §2.1]）：`net.kernel.path` 为空 **且** 订阅列表为空 **且** 线路列表为空。
// 三者都空 = 用户没在用这块功能 —— 于是这一页**一个网络区块都不渲染**：没有空表、没有"暂无数据"、
// 没有禁用控件，只有一个选装入口（否则用户找不到启用方式）。
//
// 这一组的重点是**反向断言**：未配置时那些 testid **不存在**。"不显示"的验收标准是
// "页面上没有这些东西"，不是"它们被 CSS 藏起来了"（`queryByTestId` 对两者都能分辨）。

/** 未配置：三项都读到了，且都是空的（`config_get` 给出 `path: null` → 空字符串）。 */
function unconfiguredProps(over: Partial<NetworkBodyProps> = {}): NetworkBodyProps {
  return networkProps({ kernelPath: "", subscriptions: [], lanes: [], ...over });
}

/** 未配置时**不该存在**的 testid：订阅表/线路表/内核块/空状态/写表单，一个都不许有。 */
const FORBIDDEN_WHEN_UNCONFIGURED = [
  "network-subscriptions",
  "network-subscriptions-table",
  "network-subscriptions-empty",
  "network-subscriptions-write",
  "network-subscriptions-setup",
  "network-lanes",
  "network-lanes-table",
  "network-lanes-empty",
  "network-lanes-write",
  "network-lanes-setup",
  "network-current-exit",
];

describe("⑭ /network 是选装：三项都没配置时，页面上只有选装入口", () => {
  // 整页用例会覆盖 mock（config_get / subscription_list / lane_list）—— 每条用例前先回到基准场景
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("只渲染一个块 network-optional：标题 + 一句说明 + 一个启用入口", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    const block = screen.getByTestId("network-optional");
    expect(block.textContent).toContain("网络代理（可选，未启用）");
    // 说明必须说清"这是选装"，而不是把它写成故障
    expect(block.textContent).toContain("选装");
    // 启用入口是**真控件**（可点、不 disabled），并把用户带去配内核路径 / 第一条订阅的地方
    const entry = within(screen.getByTestId("network-optional-enable")).getByRole("link");
    expect(entry.textContent).toContain("去配置");
    expect(entry.getAttribute("href")).toBe("/onboarding");
    expect(entry.hasAttribute("disabled")).toBe(false);
  });

  it("反向断言：订阅表 / 线路表 / 内核块 / 空状态 / 写表单**一个都不存在**", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    for (const testid of FORBIDDEN_WHEN_UNCONFIGURED) {
      expect(screen.queryByTestId(testid), `未配置时不该出现 ${testid}`).toBeNull();
    }
    // "一个'暂无数据'"也不要：这些诚实空状态的文案同样不该出现
    const text = document.body.textContent ?? "";
    expect(text).not.toContain("配置里还没有订阅");
    expect(text).not.toContain("配置里还没有线路");
    // "只渲染**一个**块"：零副作用声明也属于网络区块，未启用时不占版面
    expect(screen.queryByTestId("network-zero-side-effect")).toBeNull();
  });

  it("空字符串 / 纯空白的内核路径都算「没配」（不把空格当成已配置）", () => {
    expect(networkScope({ kernelPath: "", subscriptionCount: 0, laneCount: 0 }).state).toBe("off");
    expect(networkScope({ kernelPath: "   ", subscriptionCount: 0, laneCount: 0 }).state).toBe("off");
  });

  it("三项里任一项还没读到 → 不敢说「未配置」（不冒充已配置，也不画选装块）", () => {
    // `undefined` = 还没读到（`config_get` / `subscription_list` / `lane_list` 任一没回来）
    expect(networkScope({ kernelPath: undefined, subscriptionCount: 0, laneCount: 0 }).state).toBe(
      "unknown",
    );
    expect(networkScope({ kernelPath: "", subscriptionCount: undefined, laneCount: 0 }).state).toBe(
      "unknown",
    );
    expect(networkScope({ kernelPath: "", subscriptionCount: 0, laneCount: undefined }).state).toBe(
      "unknown",
    );
    // 界面上：读不到内核路径时照旧显示各区块自己的"加载中 / 未读取"，而不是断言"没配置"
    renderPage(<NetworkBody {...networkProps({ subscriptions: [], lanes: [] })} />);
    expect(screen.queryByTestId("network-optional")).toBeNull();
  });

  it("三项各自的非空都能单独开启这一个区块（内核路径 / 订阅 / 线路三选一）", () => {
    expect(networkScope({ kernelPath: "C:/mihomo", subscriptionCount: 0, laneCount: 0 })).toEqual({
      state: "on",
      kernel: true,
      subscriptions: false,
      lanes: false,
    });
    expect(networkScope({ kernelPath: "", subscriptionCount: 1, laneCount: 0 })).toEqual({
      state: "on",
      kernel: false,
      subscriptions: true,
      lanes: false,
    });
    expect(networkScope({ kernelPath: "", subscriptionCount: 0, laneCount: 1 })).toEqual({
      state: "on",
      kernel: false,
      subscriptions: false,
      lanes: true,
    });
  });

  it("readKernelPath：整份配置没读到 → undefined；读到了但没填 → 空字符串（两件事不混）", () => {
    // 没读到（`config_get` 还没回来）—— 不是"用户没配"
    expect(readKernelPath(undefined)).toBeUndefined();
    expect(readKernelPath({})).toBeUndefined();
    // 读到了：null / 缺键 / 空串都 = 用户没填
    expect(readKernelPath({ net: { kernel: { path: null } } })).toBe("");
    expect(readKernelPath({ net: { kernel: {} } })).toBe("");
    expect(readKernelPath({ net: {} })).toBe("");
    // 纯空白原样返回（读取层**不改写**后端给的值）；"算不算没配"由 `networkScope` 判定（上面那条）
    expect(readKernelPath({ net: { kernel: { path: "  " } } })).toBe("  ");
    // 真配置了就给原文（不做任何改写）
    expect(readKernelPath({ net: { kernel: { path: "C:/mihomo/mihomo.exe" } } })).toBe(
      "C:/mihomo/mihomo.exe",
    );
  });

  it("只配了内核路径：渲染内核块，但不给订阅 / 线路画空表、也不给空状态", () => {
    renderPage(
      <NetworkBody
        {...networkProps({ kernelPath: "C:/mihomo/mihomo.exe", subscriptions: [], lanes: [] })}
      />,
    );
    expect(screen.getByTestId("network-current-exit")).toBeTruthy();
    for (const testid of [
      "network-subscriptions",
      "network-lanes",
      "network-subscriptions-empty",
      "network-lanes-empty",
    ]) {
      expect(screen.queryByTestId(testid), `没配置的区块不该画空表：${testid}`).toBeNull();
    }
    // 但**建第一条**的入口必须还在（藏在空表里的写控件等于把路堵死）：
    // 首启向导只写内核路径与订阅，第一条线路只能在 `/network` 建。
    expect(screen.getByTestId("network-subscriptions-setup")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-setup")).toBeTruthy();
    expect(screen.getByTestId("network-subscriptions-add-id")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-add-id")).toBeTruthy();
  });

  it("只配了订阅：订阅区块连表带写表单都在；线路仍是写入入口，而不是空表", () => {
    renderPage(
      <NetworkBody
        {...networkProps({ kernelPath: "", subscriptions: [readSubscription(contractSubscription())], lanes: [] })}
      />,
    );
    expect(screen.getByTestId("network-subscriptions-table")).toBeTruthy();
    expect(screen.queryByTestId("network-lanes")).toBeNull();
    expect(screen.queryByTestId("network-lanes-empty")).toBeNull();
    // 没线路时**仍要能建第一条线路**：写入控件不是"空表"，不违反"不画空表"
    expect(screen.getByTestId("network-lanes-setup")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-add-id")).toBeTruthy();
    // 反过来：已经有订阅了，就不该再出现"建第一条订阅"的兜底块
    expect(screen.queryByTestId("network-subscriptions-setup")).toBeNull();
  });

  it("整页：mock 基准场景（有订阅 + 有线路）不显示选装入口", async () => {
    renderPage(<NetworkPage />);
    expect(await screen.findByTestId("network-subscriptions-table")).toBeTruthy();
    expect(await screen.findByTestId("network-lanes-table")).toBeTruthy();
    expect(screen.queryByTestId("network-optional")).toBeNull();
  });

  it("整页：config_get + subscription_list + lane_list 三项都空 → 整页只剩 network-optional", async () => {
    registerMock("config_get", () => ({ net: { kernel: { path: null } } }) as never);
    registerMock("subscription_list", () => []);
    registerMock("lane_list", () => []);

    renderPage(<NetworkPage />);
    expect(await screen.findByTestId("network-optional")).toBeTruthy();
    for (const testid of FORBIDDEN_WHEN_UNCONFIGURED) {
      expect(screen.queryByTestId(testid), `未配置时不该出现 ${testid}`).toBeNull();
    }
  });
});

// ================================================================ ⑮ 订阅流量与更新时间（[18 §1]）
//
// 用户原话：「clash 的面板能看到**更新时间** 2026-09-21 21:44、**已使用/总量** 92.9GB/130GB 71%，
// 我希望我们也能看到」。三条显示规则（[18 §1.3]）：
//
// ① **更新时间**：`fetched_at_ms` 按**本地时区**格式化成 `YYYY-MM-DD HH:mm`（Clash 面板同款）；
// ② **已用/总量**：`used = upload + download`（**两者都有**才算），`total` 有就显示 `92.9 GB / 130 GB`；
// ③ **百分比**：`used / total` 取整（`71%`）；`total` 缺失或为 0 → 不显示。
//
// 三条**各自独立**：缺哪样就**不显示那一样**（不显示 0、不显示占位符）。
// `traffic === null`（这份订阅从没成功刷新过）→ 只显示一句「尚未获取」，**不显示数字**。
//
// 数据全部来自后端（`subscription_traffic` 的持久化观测值）：前端**不自己算 total**、
// **不把缺失当 0**（`0` 是真实观测值，必须显示）。

/** 后端给的那份字节数（`upload 10 GiB` + `download 约 82.9 GiB` = `92.9 GB`；`total 130 GiB`）。 */
const TRAFFIC_UPLOAD = 10_737_418_240;
const TRAFFIC_DOWNLOAD = 89_000_000_000;
const TRAFFIC_TOTAL = 139_586_437_120;
const TRAFFIC_EXPIRE_S = 1_893_456_000;
const TRAFFIC_FETCHED_AT_MS = 1_775_000_000_000;

function trafficFixture(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    upload_bytes: TRAFFIC_UPLOAD,
    download_bytes: TRAFFIC_DOWNLOAD,
    total_bytes: TRAFFIC_TOTAL,
    expire_at_s: TRAFFIC_EXPIRE_S,
    fetched_at_ms: TRAFFIC_FETCHED_AT_MS,
    ...over,
  };
}

/** 订阅行（第一行）的全部文本 —— 三条规则都在这上面断言。 */
async function firstSubscriptionRow(): Promise<HTMLElement> {
  return (await screen.findAllByTestId("network-subscription-row"))[0]!;
}

describe("⑮ 订阅行显示流量与更新时间（三项各自独立，缺哪样不显示哪样）", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("三样都在：更新时间（本地 YYYY-MM-DD HH:mm）+ 已用/总量 + 百分比 + 到期", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [{ id: "sub-jp", traffic: trafficFixture() }]);

    renderPage(<NetworkPage />);
    const rowText = (await firstSubscriptionRow()).textContent ?? "";
    // ① 本地时区、固定格式（不是 ISO 串、不是相对时间）
    expect(rowText).toMatch(/更新时间：\d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    // ② used = upload + download（1024³，一位小数）+ 后端给的 total
    expect(rowText).toContain("已用 92.9 GB / 130 GB");
    // ③ used / total 取整
    expect(rowText).toContain("71%");
    // 到期：Unix 秒 → 本地日期
    expect(rowText).toContain(`到期：${formatLocalTime(TRAFFIC_EXPIRE_S * 1000, "YYYY-MM-DD")}`);
    // 流量单元格有稳定的锚点（下面几条要精确到"这一个单元格"）
    expect(within(await firstSubscriptionRow()).getByTestId("network-subscription-traffic")).toBeTruthy();
  });

  it("traffic === null（从没成功刷新过）：只有一句「尚未获取」，一个数字都不显示", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [{ id: "sub-jp", traffic: null }]);

    renderPage(<NetworkPage />);
    expect((await firstSubscriptionRow()).textContent).toContain("尚未获取");
    const cell = screen.getByTestId("network-subscription-traffic");
    expect(cell.textContent ?? "").not.toMatch(/\d/);
  });

  it("缺 upload（download 有）：不显示「已用」也不显示百分比 —— 不把缺失当 0", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [
      { id: "sub-jp", traffic: trafficFixture({ upload_bytes: null }) },
    ]);

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    const cell = screen.getByTestId("network-subscription-traffic");
    // 更新时间照旧显示（它不依赖 upload）
    expect(cell.textContent).toMatch(/更新时间：\d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    expect(cell.textContent).not.toContain("已用");
    expect(cell.textContent).not.toContain("%");
    expect(cell.textContent).not.toContain("0 B");
  });

  it("缺 total：只显示「已用 92.9 GB」，不显示百分比（也不显示 0 或占位符）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [
      { id: "sub-jp", traffic: trafficFixture({ total_bytes: null }) },
    ]);

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    const cell = screen.getByTestId("network-subscription-traffic");
    expect(cell.textContent).toContain("已用 92.9 GB");
    expect(cell.textContent).not.toContain("%");
    expect(cell.textContent).not.toContain("/");
  });

  it("total 为 0：显示 0 B 但**不显示百分比**（不除零、不编一个数）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [
      { id: "sub-jp", traffic: trafficFixture({ total_bytes: 0 }) },
    ]);

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    const cell = screen.getByTestId("network-subscription-traffic");
    expect(cell.textContent).toContain("已用 92.9 GB / 0 B");
    expect(cell.textContent).not.toContain("%");
  });

  it("upload/download 都是 0（真实观测值）：显示「已用 0 B」与「0%」，不当成缺失", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [
      { id: "sub-jp", traffic: trafficFixture({ upload_bytes: 0, download_bytes: 0 }) },
    ]);

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    const cell = screen.getByTestId("network-subscription-traffic");
    expect(cell.textContent).toContain("已用 0 B / 130 GB");
    expect(cell.textContent).toContain("0%");
  });

  it("expire_at_s 为 0（契约：0 = 不适用）：不显示「到期」，其余三样照常", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => [
      { id: "sub-jp", traffic: trafficFixture({ expire_at_s: 0 }) },
    ]);

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    const cell = screen.getByTestId("network-subscription-traffic");
    expect(cell.textContent).not.toContain("到期");
    expect(cell.textContent).toContain("已用 92.9 GB / 130 GB");
  });

  it("流量读失败：TA 码原样显示（不吞错，也不把失败画成「尚未获取」）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_traffic", () => {
      throw new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：subscription_traffic" });
    });

    renderPage(<NetworkPage />);
    await firstSubscriptionRow();
    expect(
      (await screen.findByTestId("network-subscriptions-traffic-error")).textContent,
    ).toContain("TA-9001");
  });

  it("流量还没读到（命令还没回来）：行上显示「未读取」，不冒充「尚未获取」", () => {
    // 展示件的三态：`traffic: null` = 后端说"从没成功刷新过"；**没有这个字段** = 还没读到
    const first = readSubscription(contractSubscription());
    const secondFixture = contractSubscription();
    const second = readSubscription(
      contractSubscription({
        subscription: {
          ...(secondFixture["subscription"] as Record<string, unknown>),
          id: "sub-jp-2",
        },
      }),
    );
    renderBody(
      <NetworkBody
        {...networkProps({
          kernelPath: "",
          subscriptions: [
            { ...first, traffic: undefined },
            { ...second, traffic: null },
          ],
          lanes: [readLane(contractLane())],
        })}
      />,
    );
    const cells = screen.getAllByTestId("network-subscription-traffic");
    expect(cells[0]?.textContent).toContain("未读取");
    expect(cells[1]?.textContent).toContain("尚未获取");
  });
});

// ---------------------------------------------------------------- ⑮b 纯逻辑（读取层 + 三条显示规则）

describe("⑮b 读流量：契约的三态（没这一行 / null / 有值）与三个数字的算法", () => {
  it("readTrafficMap：没这一行 = 键不存在；traffic:null = 从没成功刷新过；0 原样保留", () => {
    const map = readTrafficMap([
      { id: "sub-a", traffic: trafficFixture({ upload_bytes: 0, download_bytes: 0 }) },
      { id: "sub-b", traffic: null },
    ]);
    expect(map.get("sub-a")?.uploadBytes).toBe(0);
    expect(map.get("sub-a")?.downloadBytes).toBe(0);
    expect(map.get("sub-a")?.totalBytes).toBe(TRAFFIC_TOTAL);
    expect(map.get("sub-b")).toBeNull();
    // 后端没给这一行 → 键不存在（未读取），不是 null（尚未获取）
    expect(map.has("sub-c")).toBe(false);
    expect(map.get("sub-c")).toBeUndefined();
    // `expire_at_s: 0` = 契约里的"不适用" → 与缺失同义
    expect(map.get("sub-a")?.expireAtS).toBe(TRAFFIC_EXPIRE_S);
    expect(
      readTrafficMap([{ id: "sub-d", traffic: trafficFixture({ expire_at_s: 0 }) }]).get("sub-d")
        ?.expireAtS,
    ).toBeUndefined();
  });

  it("readTrafficMap：接口给的东西不是数组 / 行里没有 id 时不抛错（不白屏）", () => {
    expect(readTrafficMap(undefined).size).toBe(0);
    expect(readTrafficMap("nope").size).toBe(0);
    expect(readTrafficMap([{ traffic: null }]).size).toBe(0);
    // 整份对象不是行形状时字段全是 undefined（缺哪样不显示哪样），而不是 0
    const row = readTrafficMap([{ id: "sub-a", traffic: {} }]).get("sub-a");
    expect(row?.uploadBytes).toBeUndefined();
    expect(row?.totalBytes).toBeUndefined();
    expect(row?.fetchedAtMs).toBeUndefined();
  });

  it("summarizeTraffic：used = upload + download（两者都有才算），total 由后端给", () => {
    const summary = summarizeTraffic(readTrafficMap([{ id: "x", traffic: trafficFixture() }]).get("x"));
    expect(summary).toEqual({
      kind: "known",
      fetchedAtMs: TRAFFIC_FETCHED_AT_MS,
      usedBytes: TRAFFIC_UPLOAD + TRAFFIC_DOWNLOAD,
      totalBytes: TRAFFIC_TOTAL,
      usedPct: 71,
      expireAtS: TRAFFIC_EXPIRE_S,
    });
  });

  it("summarizeTraffic：缺一个（上传/下载）就没有 used、没有百分比 —— 不把缺失当 0", () => {
    const withUpload = summarizeTraffic({
      uploadBytes: TRAFFIC_UPLOAD,
      downloadBytes: undefined,
      totalBytes: TRAFFIC_TOTAL,
      fetchedAtMs: TRAFFIC_FETCHED_AT_MS,
    });
    expect(withUpload.kind).toBe("known");
    if (withUpload.kind !== "known") return;
    expect(withUpload.usedBytes).toBeUndefined();
    expect(withUpload.usedPct).toBeUndefined();
    expect(withUpload.totalBytes).toBe(TRAFFIC_TOTAL);
  });

  it("summarizeTraffic：total 缺失或为 0 → 没有百分比（不除零）", () => {
    const noTotal = summarizeTraffic({ uploadBytes: 10, downloadBytes: 20 });
    if (noTotal.kind !== "known") throw new Error("应当是 known");
    expect(noTotal.usedBytes).toBe(30);
    expect(noTotal.totalBytes).toBeUndefined();
    expect(noTotal.usedPct).toBeUndefined();

    const zeroTotal = summarizeTraffic({ uploadBytes: 10, downloadBytes: 20, totalBytes: 0 });
    if (zeroTotal.kind !== "known") throw new Error("应当是 known");
    expect(zeroTotal.usedPct).toBeUndefined();
  });

  it("summarizeTraffic：null / undefined 都是 none（由调用方决定画「尚未获取」还是「未读取」）", () => {
    expect(summarizeTraffic(null)).toEqual({ kind: "none" });
    expect(summarizeTraffic(undefined)).toEqual({ kind: "none" });
  });

  it("百分比取整（不是四舍五入到一位小数，也不是截断成 0）", () => {
    const s = summarizeTraffic({
      uploadBytes: 1,
      downloadBytes: 0,
      totalBytes: 3,
    });
    if (s.kind !== "known") throw new Error("应当是 known");
    expect(s.usedPct).toBe(33);
  });
});

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
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

// [docs/55] `@/pages/network/assignments`（「线路分配」页）整页删除 —— 它的 import
// 与下面全部相关用例一并去掉。那一页展示的五级继承链只剩两级还有意义。
import {
  LANE_KINDS,
  MASKED,
  laneKindLabel,
  maskSubscriptionUrl,
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
// 第 326 轮：流量从概览列表搬进订阅详情页（列表 14 列 → 6 列）→ 那些用例改在详情页上跑。
import SubscriptionDetailPage from "@/pages/network/subscription";
import { resolvePage } from "@/app/pageRegistry";
import { routeByKey } from "@/app/routes";
import { registerMock, TokenAgencyError } from "@/shared/api/client";
import { registerBaselineMocks } from "@/shared/api/mock";
import { formatLocalTime } from "@/shared/format";
import { i18n, zhCN, type I18nKey } from "@/shared/i18n";
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
      // [docs/54] 用户关掉的节点名（逐节点开关的存档）。[docs/57] 的编辑用例靠它证明
      // "整条覆盖写 + 不回带底稿 = 静默把这些开关全打开"。
      disabled_nodes: ["剩余流量：100 GB"],
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

/**
 * 同一份契约订阅换个 id / 名称：批量删除用例要一次拿到**能区分**的多行
 * （不能拿三份一模一样的行去断言"逐条"）。
 */
function contractSubscriptionWith(id: string, name: string): Record<string, unknown> {
  const inner = contractSubscription()["subscription"] as Record<string, unknown>;
  return contractSubscription({ subscription: { ...inner, id, name } });
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
      kind: "filter",
      subscription: "sub-jp",
      group: USER_GROUP,
      group_type: "url-test",
      node: null,
      filter: null,
      exclude_filter: null,
      enabled: true,
    },
    type: "filter",
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
      kind: "filter",
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

/**
 * 整页渲染：必须包一层 Router —— 页头的两个导航按钮用的是 MUI `Button component={Link}`，
 * 而 `Link` 依赖 Router context（真实应用里由 `AppShell` 提供）。
 */
function renderPage(node: React.ReactElement) {
  return renderPageAt("/", node);
}

/**
 * 同 `renderPage`，但可以指定初始 URL —— 返回条（`network-return-banner`）是靠
 * `?from=provider-wizard` 这个**真实 URL** 触发的，不测 URL 就等于没测。
 */
function renderPageAt(path: string, node: React.ReactElement) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[path]}>
        <ThemeProvider theme={buildTheme("dark")}>{node}</ThemeProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

// ================================================================ ① 没有占位页

describe("两个 network 路由都不再落到占位壳", () => {
  it("routes.ts 的候选路径都能解析到真实页面文件", () => {
    // [docs/55] `network-assignments` 已从那三个里去掉：那一页整页删除了。
    for (const key of ["network", "network-nodes"]) {
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
  // 节点级启用 / 关闭（用户第 202 轮）：写的是 TokenAgency 自己的配置（订阅的 `disabled_nodes`），
  // 系统网络一个字段都不动 —— 与上面三条同族。批量只是"一次命令带一批名字"，不是新的能力面。
  "subscription_nodes_enabled",
  // [docs/58] `lane_upsert` 已从白名单去掉：手动建/改线路的界面整块删除，这一页不再写线路。
  // 线路的行级动作只剩删除与钉节点（那两条还在下面）。
  "lane_delete",
  "lane_set_node",
  // 逐节点测速（[19 §2.4]：要么真用，要么不放）。它**不改任何配置**，只是让内核去探一次延迟；
  // 没有装配内核时后端返回错误，页面**原样显示**（不是"点了没反应"，也不是禁用占位）。
  // 与"系统/内核写命令"不同类：它不启停进程、不改系统网络、不写配置。
  "lane_test",
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
  // [docs/28 §1.3] `credential_delete` 已从命令面删除（一个服务商一把密钥：要撤密钥就删服务商）
  "credential_set_secret",
  "credential_set_enabled",
  "group_upsert",
  "group_delete",
  "group_set_sticky",
  "group_release_sticky",
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
  // 线路删除确认里的"会连带影响"清单要算"当前有几条通道显式走这条线路"，
  // 而那个事实的唯一真源是通道矩阵的每一格（`Cell.lane_id`）——**只读**命令（I2 不受影响）。
  "channel_matrix",
  "subscription_list",
  "subscription_preview",
  "subscription_nodes",
  "subscription_traffic",
  // [docs/55] `lane_assignments` / `lane_pick_options` / `lane_reverse_lookup` 已从白名单去掉：
  // 那三条命令随「线路分配」页整页删除，现在已经不在契约里了。
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
  // 只读：线路区块用它算"这条线路被几条通道显式引用"（删除确认里的连带清单）
  "channel_matrix",
  "config_get",
  "lane_delete",
  "lane_list",
  "lane_set_node",
  "lane_test",
  // [docs/58]（第 325 轮）`lane_upsert` 已从"页面直接引用"里去掉：
  // 手动建 / 改线路的界面整块删除（用户："这个功能好像没有什么用，可以在界面上删了"）。
  // 线路改由后端在服务商选中时按需建出来（`lane_ensure_for_node` / `lane_ensure_direct` /
  // `ensure_auto_lane`）—— 那三条**不在这一页**调用，所以也不该出现在这份清单里。
  "subscription_delete",
  "subscription_list",
  "subscription_nodes",
  "subscription_nodes_enabled",
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
    expect(view.kind).toBe("filter");
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

  /**
   * [docs/54] 用户第 321 轮："直接放弃订阅里面分组这一概念，只拿节点"。
   * 所以这里是**四种**（`subscription_group` 从界面选项里消失），
   * 而且这一条还要挡住"它悄悄回到选项里"。
   */
  it("四种线路类型都有文案，且「订阅里的组」已从选项里消失（[docs/54]）", () => {
    expect(LANE_KINDS).toHaveLength(4);
    expect(LANE_KINDS).not.toContain("subscription_group");
    for (const kind of LANE_KINDS) {
      expect(laneKindLabel(kind), `${kind} 没有文案`).toBeDefined();
    }
    // 词表外的类型不给文案 → 页面原样显示后端给的名字（不改写成"未知"）
    expect(laneKindLabel("quantum_tunnel")).toBeUndefined();
    expect(laneKindLabel(undefined)).toBeUndefined();
    // 遗留值也不该再给文案（它只用于反序列化老配置，不是可选项）
    expect(laneKindLabel("subscription_group")).toBeUndefined();
  });
});


// ================================================================ ⑥ /network 渲染

describe("/network：有数据 / 空 / 错误三种状态都说实话", () => {
  const subs: SubscriptionRow[] = [readSubscription(contractSubscription())];
  const lanes: LaneRow[] = [readLane(contractLane())];

  it("零副作用承诺在页头的问号里（不再单开一张卡片占半屏）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("lane_list", () => [contractLane()]);
    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");

    // 承诺**一个字都没少**，只是从"占一整块版面"变成"标题旁那个问号"（用户第 326 轮的原话：
    // 整张卡片只为了说一句免责）。红线不许消失，所以这里逐句断言。
    const help = screen.getByTestId("page-header-help").getAttribute("aria-label") ?? "";
    for (const promise of [
      "只展示，不改系统网络",
      "不设置系统代理",
      "不写 DNS",
      "不改路由表",
      "不动网卡",
      "不改开机自启",
    ]) {
      expect(help, `零副作用承诺少了这一句：${promise}`).toContain(promise);
    }
    // 反向断言：那张只写一句免责的卡片**不许**再回来
    expect(screen.queryByTestId("network-zero-side-effect")).toBeNull();
  });

  it("订阅表：名称 / 遮蔽后的地址 / 启用 / 节点数 / 最近更新都画出来（6 列）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    const table = within(screen.getByTestId("network-subscriptions-table"));
    expect(table.getByText("机场A")).toBeTruthy();
    expect(
      table.getByText(`https://example.com/api/v1/client/subscribe?token=${MASKED}`),
    ).toBeTruthy();
    // 列头一个「启用」+ 这一行的值一个「启用」
    expect(table.getAllByText("启用")).toHaveLength(2);
    expect(table.getByText("12")).toBeTruthy();
    // 「最近更新」是**短格式**（`N 天前`），不是完整时间戳 —— 完整时间戳会把整行撑成 6 行高。
    // 夹具的 last_update_ms 是过去的时间，所以它一定是"N 秒/分钟/小时/天前"里的一个。
    expect(table.getByText(/前$/)).toBeTruthy();
    // 砍到 6 列：格式 / 更新周期 / 保留版本 / 过滤 / 健康检查 / 流量 / 组数 / 解析器 / 错误
    // 全部搬进详情页，**不再**是列头（这是规则 4 的可机械检查形式）。
    for (const gone of ["格式", "更新周期", "保留版本", "过滤", "健康检查", "流量", "组数", "解析器", "错误"]) {
      expect(table.queryByText(gone), `${gone} 不该再是列头`).toBeNull();
    }
  });

  it("Rust 形状的订阅：节点数显示「未读取」并说明原因，不填 0", () => {
    renderBody(
      <NetworkBody
        {...networkProps({ subscriptions: [readSubscription(rustSubscription())], lanes })}
      />,
    );
    const table = within(screen.getByTestId("network-subscriptions-table"));
    // 节点数 + 最近更新都是「未读取」（两处都是"读不到"，不是 0、不是空）
    expect(table.getAllByText("未读取").length).toBeGreaterThanOrEqual(2);
    expect(table.queryByText("0")).toBeNull();
    // 过期说明随实现落地一起作废：解析接口早就接上了（`LaneWriteForm` 真的在调它们），
    // hint 里不许再说"本页尚未接入"。第 326 轮之后区块里只剩**一个**问号
    // （说明 + 三句解释合成同一条提示），所以锚点也换到它上面。
    const help = screen.getByTestId("network-subscriptions-help").getAttribute("aria-label") ?? "";
    expect(help).not.toContain("尚未接入");
    expect(help).toContain(i18n.t("network.subscriptions.urlHint"));
  });

  it("线路表：类型用文字、健康与延迟同格（圆点 + 文字 + 毫秒）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    const table = within(screen.getByTestId("network-lanes-table"));
    expect(table.getByText("日本线")).toBeTruthy();
    // [docs/54] fixture 的 kind 现在是 `filter`（「订阅里的组」已取消）
    expect(table.getByText("正则筛选后择优")).toBeTruthy();
    expect(table.getByText("82ms")).toBeTruthy();
    expect(table.getByText("成功")).toBeTruthy();
  });

  it("订阅为空 / 线路为空：给出诚实的空状态，不画假行", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: [], lanes: [] })} />);
    // 空态只说"怎么开始"（规则 5）：一行字 + 上面那个「添加订阅」/「去服务商页」按钮
    expect(screen.getByTestId("network-subscriptions-empty").textContent).toContain("还没有订阅");
    expect(screen.getByTestId("network-lanes-empty").textContent).toContain("还没有线路");
    expect(screen.queryByTestId("network-subscriptions-table")).toBeNull();
    expect(screen.queryByTestId("network-lanes-table")).toBeNull();
  });

  it("加载中不冒充空状态（不把「还没查到」说成「没有」）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptionsLoading: true, lanesLoading: true })} />);
    expect(screen.queryByTestId("network-subscriptions-empty")).toBeNull();
    expect(screen.queryByTestId("network-lanes-empty")).toBeNull();
  });

  /**
   * 规则 4 的**可机械检查形式**：默认视图下两张表都 ≤6 列（**订阅表**的勾选框列只在 ≥2 行时
   * 出现，那是控制列不是数据列 —— 线路表第 2 轮起不再有勾选框列，恒为 6 列），
   * 而且每个单元格都带着"不换行"。
   */
  it("规则 4：订阅表 / 线路表默认 ≤6 列，且单元格一律 nowrap", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);

    for (const testId of ["network-subscriptions-table", "network-lanes-table"]) {
      const table = within(screen.getByTestId(testId));
      const headers = table.getAllByRole("columnheader");
      // 一条数据 → 没有多选场景 → 勾选框列不出现（规则 3）
      expect(table.queryAllByRole("checkbox"), `${testId} 只有一条数据时不该有勾选框`).toHaveLength(0);
      expect(headers.length, `${testId} 列数`).toBeLessThanOrEqual(6);
      for (const cell of headers) {
        // 长值不许把列挤竖排：表头一律单行（`CELL_NOWRAP_SX`）
        expect(getComputedStyle(cell).whiteSpace, `${testId} 表头换行了`).toBe("nowrap");
      }
    }
  });

  /**
   * **判据 B（第 2 轮）**：行内写控件不许把行撑成两行。
   *
   * 反例是真机截图里的线路表：操作列里塞了 160px 的节点 `TextField` + 钉住 + 测速 + 删除 ——
   * 「节点」输入框和「钉住」折到了第二行，整行高错位。jsdom 量不了像素，但能机械地量
   * "一个格子里有几个控件、控件是不是常驻" —— 这正是错位的成因。
   *
   * 判据：**每个操作格里只有一个控件**（那个 `⋮`），其余行级控件只在菜单打开后才进 DOM。
   */
  it("判据 B：线路表每行操作格里只有一个 ⋮，其余控件只在菜单里（不会把行折成两行）", async () => {
    const inner = contractLane()["lane"] as Record<string, unknown>;
    const twoLanes = [
      readLane(contractLane()),
      readLane(contractLane({ lane: { ...inner, id: "lane-hk", name: "香港线" } })),
    ];
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes: twoLanes })} />);

    const table = within(screen.getByTestId("network-lanes-table"));
    // 常驻在格子里的输入框/按钮一个都不许有（它们现在全在菜单里）
    expect(table.queryAllByRole("textbox")).toHaveLength(0);
    expect(table.queryAllByTestId("network-lanes-test")).toHaveLength(0);
    expect(table.queryAllByTestId("network-lanes-delete")).toHaveLength(0);

    for (const row of table.getAllByTestId("network-lane-row")) {
      const cells = Array.from(row.querySelectorAll("td"));
      const actions = cells[cells.length - 1]!;
      // 操作格：**只有**那个 `⋮`（button / input / menuitem 加起来一个）
      expect(actions.querySelectorAll("button, input, [role='menuitem']")).toHaveLength(1);
      expect(actions.querySelector("[data-testid='network-lanes-actions']")).toBeTruthy();
      // 每一格都不换行（长值 + 控件都不许折行）
      for (const cell of cells) {
        expect(getComputedStyle(cell).whiteSpace, "单元格换行了").toBe("nowrap");
      }
    }

    // 控件真的还在（在菜单里），不是被删了
    await openLaneMenu(0);
    expect(screen.getByTestId("network-lanes-test")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-delete")).toBeTruthy();
  });

  it("只有一条订阅时没有「已选 0 条 / 删除选中」（规则 3）；两条时才出现", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    expect(screen.queryByTestId("network-subscriptions-selected-count")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-delete-selected")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-select")).toBeNull();
    cleanup();

    renderBody(
      <NetworkBody
        {...networkProps({
          subscriptions: [readSubscription(contractSubscription()), readSubscription(contractSubscriptionWith("sub-hk", "机场B"))],
          lanes,
        })}
      />,
    );
    // 真有多条时，多选功能一个字不少
    expect(screen.getByTestId("network-subscriptions-selected-count").textContent).toContain("0");
    expect(screen.getByTestId("network-subscriptions-delete-selected")).toBeTruthy();
    expect(screen.getAllByTestId("network-subscriptions-select")).toHaveLength(2);
  });

  it("线路空态：一行字 + 一个「去服务商页」按钮（规则 5；地址由数据层给）", () => {
    renderPageAt(
      "/",
      <NetworkBody {...networkProps({ subscriptions: [], lanes: [], providersHref: "/providers" })} />,
    );
    expect(screen.getByTestId("network-lanes-empty").textContent).toContain("还没有线路");
    const goto = within(screen.getByTestId("network-lanes-empty-goto")).getByRole("link");
    expect(goto.getAttribute("href")).toBe("/providers");
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

  it("没有任何样本：诚实空状态（一行字），「本页不自己测速」那句进了区块问号", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(contractLane({ health: "unknown" }))] })} />);
    // 契约形状的线路没有 latest 样本 → 采样时间未知
    const samples = within(screen.getByTestId("network-nodes-samples"));
    expect(samples.getByText("未知")).toBeTruthy();
    cleanup();

    renderBody(<NodesBody {...nodesProps({ lanes: [] })} />);
    // 空态：1 行（规则 5）——"本页不自己探测、也不编延迟"那句在标题旁的问号里，不铺在版面上
    expect(screen.getByTestId("network-nodes-empty").textContent).toBe(
      i18n.t("network.nodes.samples.empty"),
    );
    expect(
      screen.getByTestId("network-nodes-samples-section-help").getAttribute("aria-label"),
    ).toContain(i18n.t("network.nodes.samples.emptyHint"));
  });

  it("声明的节点来源原样呈现（用户数据里的字符一个都不改）", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(contractLane())] })} />);
    const declared = within(screen.getByTestId("network-nodes-declared"));
    expect(declared.getByText(USER_GROUP)).toBeTruthy();
  });

  it("反向断言：过期说明（「本页尚未接入 subscription_nodes」）整块作废", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);
    // 老的那一块「为什么这里没有完整的节点清单」随实现落地一起消失 —— 现在真的在调那条命令
    expect(screen.queryByTestId("network-nodes-missing")).toBeNull();
    expect(document.body.textContent ?? "").not.toContain("尚未接入");
  });

  it("读失败：带 TA 码 + 重试", () => {
    renderBody(<NodesBody {...nodesProps({ lanesError: new Error("[TA-9001] 未注册 mock：lane_list") })} />);
    expect(screen.getByTestId("network-nodes-error").textContent).toContain("TA-9001");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
  });
});
// ================================================================ ⑧ /network/assignments —— 已删除
//
// [docs/55]（第 322 轮）「线路分配」整页删除（用户："线路级别的分配逻辑我们已经放弃了，
// 取而代之的，每个服务商统一一个节点"）。这里原来的 6 条用例全部删除：
// 它们断言的是"五级里每一级显式写了什么" + "倒排谁引用了这条线"，
// 而前三级已经没有字段、没有写入口、也不参与解析了。

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
    // 页头标题与「节点清单」区块标题同名 → 按 **H1** 取（`getByText` 会同时命中两个）
    expect(
      (await screen.findByRole("heading", { level: 1, name: "节点列表" })).tagName,
    ).toBe("H1");
    expect(await screen.findByTestId("network-nodes-samples")).toBeTruthy();
  });

  // [docs/55] `/network/assignments` 整页删除 → 这一档（它渲染页头 + 默认线路）随之删除。
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
    // [docs/55] `AssignmentsBody` 那一页整页删除，所以这一轮不再渲染它。
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

/**
 * [docs/57] **写表单不再常驻**（用户第 324 轮：「这个界面不要常驻，应该点击添加才会有，
 * 或者说点击编辑」）→ 碰表单之前先打开对话框。
 *
 * 这两个 helper 同时也是那条"不常驻"断言的**前提**：如果表单又变回常驻，
 * 下面这些用例里"先点开"的那一步会找不到按钮而红 —— 想改回去就得连测试一起改。
 */
function openSubscriptionWrite(): void {
  fireEvent.click(screen.getByTestId("network-subscriptions-add-open"));
}

/** 打开某一行的「编辑」（默认第一行）。 */
function openSubscriptionEdit(index = 0): void {
  fireEvent.click(screen.getAllByTestId("network-subscriptions-edit")[index]!);
}

// [docs/58]（第 325 轮）`openLaneWrite` / `openLaneEdit` 两个 helper 随**手动建/改线路的界面**
// 一起删除：线路区块现在没有任何写入入口（线路由后端在服务商选中时按需建出来）。

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
    await screen.findByTestId("network-subscriptions-add-open");
    openSubscriptionWrite();
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

    // [docs/46 §E] 健康检查地址：**填了才发**（留空时上面那条 "health_check" in sub === false 仍然成立）。
    // 只放 `url`：Rust 侧 `health_check` 的其余项有 `#[serde(default)]`，前端不冒充第二份默认值。
    typeInto("network-subscriptions-add-health-url", "http://127.0.0.1:39099/");
    fireEvent.click(screen.getByTestId("network-subscriptions-add-submit"));
    await waitFor(() => expect(upsert.calls).toHaveLength(2));
    const sub2 = upsert.calls[1]?.subscription as Record<string, unknown>;
    expect(sub2.health_check).toEqual({ url: "http://127.0.0.1:39099/" });

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
    await screen.findByTestId("network-subscriptions-add-open");
    openSubscriptionWrite();
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

  it("删除某一行：**先弹二次确认**（确认之前一条命令都不发）→ 确认后 subscription_delete({id}) 并重取列表", async () => {
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

    // ① 确认框先出现，且**点名**删的是哪一个（名称 + id）
    const dialog = await screen.findByTestId("network-subscriptions-delete-dialog");
    expect(within(dialog).getByTestId("network-subscriptions-delete-target").textContent).toContain(
      "机场A",
    );
    expect(within(dialog).getByTestId("network-subscriptions-delete-target").textContent).toContain(
      "sub-jp",
    );
    // ② 点「删除」的那一刻**一次后端调用都没有**（这就是"半成品"与"接线完成"的分界）
    expect(remove.calls).toEqual([]);

    fireEvent.click(screen.getByTestId("network-subscriptions-delete-confirm"));

    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "sub-jp" });
    // ③ 成功：结果区如实写"已删除 <名称>"，框收起
    const result = await screen.findByTestId("network-subscriptions-delete-result");
    expect(result.textContent).toContain("已删除");
    expect(result.textContent).toContain("机场A");
    await waitFor(() => expect(screen.queryByTestId("network-subscriptions-delete-dialog")).toBeNull());
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("删除被拒绝：后端原文（含 TA 码）留在确认框里、**不放级联按钮**（订阅没有 force 语义）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_delete", () => {
      throw new TokenAgencyError({ code: "TA-5002", message: "订阅 sub-jp 被 1 条线路引用" });
    });

    renderPage(<NetworkPage />);
    fireEvent.click((await screen.findAllByTestId("network-subscriptions-delete"))[0]!);
    fireEvent.click(await screen.findByTestId("network-subscriptions-delete-confirm"));

    const error = await screen.findByTestId("network-subscriptions-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("被 1 条线路引用");
    // 没有 force 就不许给"强行删除"按钮（那会是假按钮）
    expect(screen.queryByTestId("network-subscriptions-delete-cascade")).toBeNull();
    // 框**不关**：原文就在这里，关掉就等于把失败吞了
    expect(screen.getByTestId("network-subscriptions-delete-dialog")).toBeTruthy();
    expect(screen.queryByTestId("network-subscriptions-delete-result")).toBeNull();
  });

  /**
   * [docs/53] **用户真机看到的就是这句** —— 这一条钉的是"报错必须能照着改"。
   *
   * 第 320 轮用户报的是：删订阅被拒之后只显示 `[TA-5001] 配置校验失败：1 处问题`。
   * 这句话不指出**是哪条线路**挡着，也不说**该去哪儿改** —— 用户只能来问人。
   *
   * 现在两处都补上了：后端回 `TA-5002` + 引用者名字 + "先解除"，
   * 而且即使拿到的是带 `details` 的 `TA-5001`，前端也会把字段路径拼进去
   * （`errorMessage` 的唯一出口，见 `src/app/errorMessage.test.ts`）。
   */
  it("[docs/53] 被拒的原文里必须能看出「谁在挡」与「该怎么办」，不能只有一句「N 处问题」", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_delete", () => {
      // 后端**现在**回的形状：点名引用者（线路的显示名）+ 给出出路
      throw new TokenAgencyError({
        code: "TA-5002",
        message: "对象被引用，禁止删除：订阅 sub-jp（引用它的地方：日本线；要先解除这些引用再删）",
      });
    });

    renderPage(<NetworkPage />);
    fireEvent.click((await screen.findAllByTestId("network-subscriptions-delete"))[0]!);
    fireEvent.click(await screen.findByTestId("network-subscriptions-delete-confirm"));

    const error = await screen.findByTestId("network-subscriptions-delete-error");
    const text = error.textContent ?? "";
    expect(text).toContain("TA-5002");
    // 「谁在挡」：引用者的名字
    expect(text).toContain("日本线");
    // 「怎么办」：可执行的下一步
    expect(text).toContain("先解除");
    // 不再是那句没有任何信息量的"N 处问题"
    expect(text).not.toContain("处问题");
  });

  it("连带清单：引用这份订阅的线路条数来自 lane_list；读不到就一个字都不写", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("lane_list", () => [
      rustLane(), // lane-jp 的 lane.subscription = sub-jp
      rustLane({
        lane: {
          id: "lane-hk",
          name: "香港线",
          kind: "filter",
          subscription: "sub-jp",
          group: USER_GROUP,
          enabled: true,
        },
      }),
      rustLane({
        lane: {
          id: "lane-other",
          name: "别的订阅",
          kind: "filter",
          subscription: "sub-other",
          group: USER_GROUP,
          enabled: true,
        },
      }),
    ]);

    renderPage(<NetworkPage />);
    fireEvent.click((await screen.findAllByTestId("network-subscriptions-delete"))[0]!);
    const list = await screen.findByTestId("network-subscriptions-delete-consequences");
    expect(list.textContent).toContain("2");
    expect(list.textContent).toContain("线路");
  });

  /**
   * [docs/53] **这一条是第 320 轮用户真机报上来的那个 bug。**
   *
   * 订阅删除**没有 force 语义**：被线路引用时后端直接拒（TA-5001），一个字都不会删。
   * 但确认框的表头原来固定写「会连带删除 / 影响：」—— 那是在**承诺一件不会发生的事**，
   * 用户确认后只拿到一个 TA 码，还以为是"线路会跟着删掉"。
   *
   * 判据：没有 force 的实体，表头必须说「**阻止**删除」；有 force 的（服务商/模型）才说「会连带删除」。
   */
  it("[docs/53] 确认框不许承诺做不到的级联：没有 force 时说「阻止删除」，不说「会连带删除」", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("lane_list", () => [rustLane()]); // lane-jp 引用 sub-jp

    renderPage(<NetworkPage />);
    fireEvent.click((await screen.findAllByTestId("network-subscriptions-delete"))[0]!);
    const dialog = await screen.findByTestId("network-subscriptions-delete-dialog");
    const text = dialog.textContent ?? "";

    // 说了实话：这些引用会**拦住**这次删除
    expect(text).toContain(zhCN["delete.blockers"]);
    // 且**没有**那句会让人误以为线路会跟着消失的承诺
    expect(text).not.toContain(zhCN["delete.consequences"]);
    // 引用清单本身照样在（用户要据此去改那条线路）
    expect(
      screen.getByTestId("network-subscriptions-delete-consequences").textContent,
    ).toContain("线路");
  });

  it("批量：逐行多选 + 表头全选 → 一次确认 → **逐条**调用，部分失败如实逐条汇报（不许盖成成功）", async () => {
    registerMock("subscription_list", () => [
      contractSubscription(),
      contractSubscriptionWith("sub-hk", "机场B"),
      contractSubscriptionWith("sub-bad", "机场C"),
    ]);
    const remove = recordingMock(undefined);
    registerMock("subscription_delete", (args) => {
      const id = (args as { id: string }).id;
      remove.calls.push({ ...(args as Record<string, unknown>) });
      if (id === "sub-bad") {
        throw new TokenAgencyError({ code: "TA-5002", message: "订阅 sub-bad 被 1 条线路引用" });
      }
      return undefined;
    });

    renderPage(<NetworkPage />);
    const boxes = await screen.findAllByTestId("network-subscriptions-select");
    // 行多选框：`data-testid` 在容器上，真可点的是里面那个 input（与真机 E2E 同口径）
    fireEvent.click(within(boxes[0]!).getByRole("checkbox"));
    fireEvent.click(within(boxes[1]!).getByRole("checkbox"));
    fireEvent.click(within(boxes[2]!).getByRole("checkbox"));
    expect(screen.getByTestId("network-subscriptions-selected-count").textContent).toContain("3");

    fireEvent.click(screen.getByTestId("network-subscriptions-delete-selected"));
    // 一次确认：确认之前一条命令都没发
    expect(remove.calls).toEqual([]);
    expect(screen.getByTestId("network-subscriptions-delete-target").textContent).toContain("3");
    fireEvent.click(screen.getByTestId("network-subscriptions-delete-confirm"));

    await waitFor(() => expect(remove.calls).toHaveLength(3));
    // 逐条调用、顺序不乱、**一条失败不中止其余条**
    expect(remove.calls.map((call) => call["id"])).toEqual(["sub-jp", "sub-hk", "sub-bad"]);

    const report = await screen.findByTestId("network-subscriptions-delete-result");
    // 成功 N 条 / 失败 M 条 必须写出来，且失败项逐条带 id + 后端原文
    expect(report.textContent).toContain("成功 2 条");
    expect(report.textContent).toContain("失败 1 条");
    expect(report.textContent).toContain("sub-bad");
    expect(report.textContent).toContain("TA-5002");
    // 失败的那条**仍然留在选中里**（用户可以直接重试）
    expect(screen.getByTestId("network-subscriptions-selected-count").textContent).toContain("1");
  });

  it("表头全选把当前列表全选上；一条都没选时「删除选中」不发命令", async () => {
    registerMock("subscription_list", () => [
      contractSubscription(),
      contractSubscriptionWith("sub-hk", "机场B"),
    ]);
    const remove = recordingMock(undefined);
    registerMock("subscription_delete", remove.handler);

    renderPage(<NetworkPage />);
    const selectAll = await screen.findByTestId("network-subscriptions-select-all");
    expect((screen.getByTestId("network-subscriptions-delete-selected") as HTMLButtonElement).disabled).toBe(
      true,
    );
    fireEvent.click(screen.getByTestId("network-subscriptions-delete-selected"));
    expect(screen.queryByTestId("network-subscriptions-delete-dialog")).toBeNull();
    expect(remove.calls).toEqual([]);

    fireEvent.click(within(selectAll).getByRole("checkbox"));
    expect(screen.getByTestId("network-subscriptions-selected-count").textContent).toContain("2");
  });

  it("写失败：后端 TA 码原样显示，不吞错、不改写成一句「保存失败」", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_upsert", () => {
      throw new TokenAgencyError({ code: "TA-4001", message: "配置不可用" });
    });

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-add-open");
    openSubscriptionWrite();
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
    await screen.findByTestId("network-subscriptions-add-open");
    openSubscriptionWrite();
    fireEvent.click(await screen.findByTestId("network-subscriptions-add-submit"));

    expect((await screen.findByTestId("network-subscriptions-write-error")).textContent).toContain(
      "必填",
    );
    expect(upsert.calls).toHaveLength(0);
  });
});

// ================================================================ ㉑ [docs/57] 写表单不常驻 + 编辑

/**
 * 用户第 324 轮原话：
 *
 * > 这个界面不要常驻，应该点击添加才会有，或者说点击编辑编辑
 *
 * 那一屏原来把「新建 / 覆盖订阅」整张表单（11 个字段）常驻在列表下方。现在改成**按需出现**，
 * 于是多了"编辑"这条路径 —— 而"编辑"藏着一个**会静默写坏配置**的坑，是这一组的重点：
 *
 * 1. 列表里的地址是**遮蔽过的**（`maskSubscriptionUrl` 是唯一出口）。把它回填进输入框，
 *    用户一保存就把 `token=***` 当成了真地址 —— 保存成功、下次刷新全挂；
 * 2. `subscription_upsert` 是**整条覆盖写**。只发表单字段会把 `disabled_nodes`
 *   （[docs/54] 逐节点开关的存档）清空 = 用户关掉的节点**全部被重新打开**，且不报错。
 *
 * 所以这两条各有一个断言，且都是**反向**的（"不许包含 ***"、"必须原样带上"）。
 */
describe("㉑ [docs/57] 写表单不常驻；编辑不覆盖遮蔽地址、不丢底稿字段", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("订阅写表单**不常驻**：打开页面时不在 DOM 里；点开才出现，关掉就消失", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("lane_list", () => [contractLane()]);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");

    // ① 没点之前：表单容器、字段、提交按钮**一个都不该在**
    //（这是"不常驻"的可执行判据 —— 不是靠"看起来短了"）
    expect(screen.queryByTestId("network-subscriptions-write")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-add-id")).toBeNull();
    expect(screen.queryByRole("button", { name: "保存订阅" })).toBeNull();
    // 入口必须在（否则第一条订阅永远建不出来）
    expect(screen.getByTestId("network-subscriptions-add-open")).toBeTruthy();

    // ② 点开 → 出现；点「关闭」→ 消失
    openSubscriptionWrite();
    expect(screen.getByTestId("network-subscriptions-write")).toBeTruthy();
    expect(screen.getByTestId("network-subscriptions-add-id")).toBeTruthy();
    fireEvent.click(screen.getByTestId("network-subscriptions-write-close"));
    await waitFor(() => expect(screen.queryByTestId("network-subscriptions-write")).toBeNull());
  });

  /**
   * [docs/58]（第 325 轮）用户原话：「这个功能好像没有什么用，可以在界面上删了」。
   *
   * **反回归闸门**：线路区块从此**没有任何写入口** —— 添加按钮、编辑按钮、写表单，
   * 一个都不许再出现。线路由后端在服务商页被选中时按需建出来
   *（`ensure_auto_lane` / `lane_ensure_for_node` / `lane_ensure_direct`）。
   *
   * 为什么写成"页面里不许有"而不是"点了没反应"：入口一旦回来，用户就会以为
   * "线路还能手建"，于是又出现两条并行的真相源 —— 那正是这一轮要消灭的东西。
   */
  it("[docs/58] 线路区块**没有任何写入口**（添加/编辑/写表单都不在）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("lane_list", () => [contractLane()]);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-lanes-table");

    expect(screen.queryByTestId("network-lanes-add-open")).toBeNull();
    expect(screen.queryByTestId("network-lanes-edit")).toBeNull();
    expect(screen.queryByTestId("network-lanes-write")).toBeNull();
    expect(screen.queryByTestId("network-lanes-add-id")).toBeNull();
    expect(screen.queryByRole("button", { name: "添加线路" })).toBeNull();
    expect(screen.queryByRole("button", { name: "保存线路" })).toBeNull();
    // 只读 + 行级动作仍然在（测速 / 删除），否则这一块就成了纯展示。
    // 第 2 轮（判据 B）：这两个动作收进了行内 `⋮` 菜单，所以先打开菜单再断言 —— 只读性质没变。
    await openLaneMenu();
    expect(screen.getByTestId("network-lanes-test")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-delete")).toBeTruthy();
    // 契约形状的 lane（kind=filter）应该还给得出「钉住节点」那一项
    expect(screen.getByTestId("network-lanes-set-node")).toBeTruthy();
  });

  it("「编辑」回填该行；**地址刻意留空**（回填遮蔽值就等于把 *** 存进配置）；id 锁死", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");
    openSubscriptionEdit();

    const valueOf = (testId: string) =>
      (within(screen.getByTestId(testId)).getByRole("textbox") as HTMLInputElement).value;
    const inputOf = (testId: string) =>
      within(screen.getByTestId(testId)).getByRole("textbox") as HTMLInputElement;

    // 非秘密字段如实回填
    expect(valueOf("network-subscriptions-add-name")).toBe("机场A");
    expect(valueOf("network-subscriptions-add-include")).toBe("(?i)(日本|JP)");
    expect(valueOf("network-subscriptions-add-interval")).toBe("21600");
    expect(valueOf("network-subscriptions-add-keep-versions")).toBe("3");
    expect(valueOf("network-subscriptions-add-health-url")).toBe(
      "https://api.commandcode.ai/v1/models",
    );

    // **地址是空的** —— 刻意不回显（它带 token）。而且**绝不能**出现遮蔽值：
    // 一旦这里变成 `...?token=***`，用户顺手一存就把真地址覆盖掉了。
    expect(valueOf("network-subscriptions-add-url")).toBe("");
    expect(inputOf("network-subscriptions-add-url").value).not.toContain(MASKED);
    // 遮蔽后的现值只作为**占位符**给出（看得见"当前有这么个地址"，但没被填进去）
    expect(inputOf("network-subscriptions-add-url").placeholder).toContain(MASKED);

    // id 是主键：编辑时锁死（改它 = 换一条订阅，upsert 按 id 命中）
    expect(inputOf("network-subscriptions-add-id").value).toBe("sub-jp");
    expect(inputOf("network-subscriptions-add-id").disabled).toBe(true);
  });

  it("编辑保存：地址留空 → **不发遮蔽值**；`disabled_nodes` 等底稿字段**原样回带**", async () => {
    const upsert = recordingMock(rustSubscription());
    registerMock("subscription_upsert", upsert.handler);
    registerMock("subscription_list", () => [contractSubscription()]);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");
    openSubscriptionEdit();
    typeInto("network-subscriptions-add-name", "机场A（改名）");
    fireEvent.click(screen.getByTestId("network-subscriptions-add-submit"));

    await waitFor(() => expect(upsert.calls).toHaveLength(1));
    const sub = upsert.calls[0]?.subscription as Record<string, unknown>;

    // 改了的字段就是改后的值
    expect(sub.id).toBe("sub-jp");
    expect(sub.name).toBe("机场A（改名）");

    // ① 地址留空：交给后端"保持原值"。**绝不许**把遮蔽值发回去。
    expect(sub.url).toBe("");
    expect(JSON.stringify(sub)).not.toContain(MASKED);

    // ② 底稿字段必须原样在（不回带 = 后端把 disabled_nodes 重置成空 = 用户关掉的节点全被打开）
    expect(sub.disabled_nodes).toEqual(["剩余流量：100 GB"]);
    expect(sub.user_agent).toBe("clash-verge/v2.0.0");
    expect(sub.provider_key).toBe("sub-jp");
    expect(sub.health_check).toEqual({
      enabled: true,
      url: "https://api.commandcode.ai/v1/models",
      interval_s: 60,
      tolerance_ms: 100,
    });

    // ③ 底稿里**不该**混进视图字段（那是 `SubscriptionView` 的，发回去会被 extra 收下写进配置）
    for (const viewOnly of ["node_count", "group_count", "parser", "last_update_ms", "userinfo"]) {
      expect(viewOnly in sub, `${viewOnly} 是视图字段，不该进配置`).toBe(false);
    }
  });

});

// ================================================================ ⑫ 线路区块（只读 + 行级动作）
//
// [docs/58]（第 325 轮）**手动建 / 改线路的用例已整块删除** —— 那些界面没了。
// 剩下的这一组钉住线路区块仍然能用的部分：删除（二次确认 + 连带清单 + 批量）与钉节点。
//
// 线路"该长什么样"的判断在后端：`lane_ensure_for_node` / `lane_ensure_direct` /
// `ensure_auto_lane`，各由 `net.rs` / `catalog.rs` 的单测覆盖。

/** MUI 的 Select 在 jsdom 里是个 div：先 `mouseDown` 打开菜单，再点角色为 option 的项。 */
async function chooseOption(testId: string, optionName: RegExp | string): Promise<void> {
  const box = screen.getByTestId(testId);
  fireEvent.mouseDown(within(box).getByRole("combobox"));
  fireEvent.click(await screen.findByRole("option", { name: optionName }));
}

/**
 * 第 2 轮（判据 B）：线路表的行内动作全部收进 `⋮` 溢出菜单 —— 一格只放一个控件，
 * 行不会再被 160px 的输入框 + 三个按钮折成两行。
 *
 * 所以碰 `network-lanes-set-node` / `-set-node-submit` / `-test` / `-delete` 之前
 * **必须先打开菜单**。这个 helper 就是那一步，同时它自己也是一条断言的前提：
 * 如果菜单没了（又变回四个控件摊在格子里），这里会找不到 `network-lanes-actions` 而红。
 */
async function openLaneMenu(index = 0): Promise<void> {
  // 用 `findAllByTestId`（异步）：整页渲染时 `lane_list` 还没回来，表格与按钮都还不存在
  const buttons = await screen.findAllByTestId("network-lanes-actions");
  fireEvent.click(buttons[index]!);
  await screen.findByTestId("network-lanes-menu");
}

describe("⑫ /network 线路区块：lane_delete / lane_set_node 挂在真控件上", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("每一行：删除**先二次确认**再调 lane_delete({id})，指定节点调 lane_set_node({id, node}) 并显示被接受的钉住", async () => {
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
    // 行内动作都在 `⋮` 里（判据 B）→ 先打开菜单
    await openLaneMenu();
    const nodeField = screen.getByTestId("network-lanes-set-node");
    fireEvent.change(within(nodeField).getByRole("textbox"), { target: { value: "东京-03" } });
    fireEvent.click(screen.getByTestId("network-lanes-set-node-submit"));

    await waitFor(() => expect(setNode.calls).toHaveLength(1));
    // `lane_set_node` 只收 id + node（钉住是内核运行态，不改配置）
    expect(setNode.calls[0]).toEqual({ id: "lane-jp", node: "东京-03" });
    expect((await screen.findByTestId("network-lanes-set-node-result")).textContent).toContain(
      "东京-03",
    );

    // 动作执行完菜单会收起（结果区在表格下面，菜单开着会挡着它）→ 删之前再开一次
    await openLaneMenu();
    fireEvent.click(screen.getByTestId("network-lanes-delete"));
    // 确认框先出现、点名删的是哪一条；**确认之前一条命令都不发**
    const dialog = await screen.findByTestId("network-lanes-delete-dialog");
    expect(within(dialog).getByTestId("network-lanes-delete-target").textContent).toContain("日本线");
    expect(within(dialog).getByTestId("network-lanes-delete-target").textContent).toContain("lane-jp");
    expect(remove.calls).toEqual([]);

    fireEvent.click(screen.getByTestId("network-lanes-delete-confirm"));
    await waitFor(() => expect(remove.calls).toHaveLength(1));
    expect(remove.calls[0]).toEqual({ id: "lane-jp" });
    expect((await screen.findByTestId("network-lanes-delete-result")).textContent).toContain("已删除");
    // 删一条线路要连带失效：线路 + 通道矩阵（少一个就会停在旧值上）
    await waitFor(() => expect(listCalls).toBeGreaterThan(1));
  });

  it("删除线路后：channel_matrix 与 lane_list 都被重新拉取（不留旧值）", async () => {
    // [docs/55] 原来这里还挂着 `lane_assignments` / `lane_pick_options` 两个观察者 ——
    // 那两条命令随「线路分配」页删除了。**留下的那一条才是真风险**：
    // `channelMatrix` 的 key 是 `["channels","matrix"]`，**不在** `qk.lanes` 前缀里，
    // 旧实现（只失效 qk.lanes）下矩阵会一直停在旧值上 —— 所以这一条必须继续钉。
    const calls = { lanes: 0, matrix: 0 };
    registerMock("lane_list", () => {
      calls.lanes += 1;
      return [rustLane()];
    });
    registerMock("channel_matrix", () => {
      calls.matrix += 1;
      return { models: [], credentials: [], cells: [] };
    });
    registerMock("lane_delete", () => undefined);

    renderPage(<NetworkPage />);
    await waitFor(() => expect(calls.matrix).toBeGreaterThan(0));
    const before = { ...calls };

    await openLaneMenu();
    fireEvent.click(screen.getByTestId("network-lanes-delete"));
    fireEvent.click(await screen.findByTestId("network-lanes-delete-confirm"));

    await waitFor(() => expect(calls.lanes).toBeGreaterThan(before.lanes));
    await waitFor(() => expect(calls.matrix).toBeGreaterThan(before.matrix));
  });

  it("删除线路被拒绝：后端原文（含 TA 码）留在框里、**不放级联按钮**（lane_delete 没有 force）", async () => {
    registerMock("lane_list", () => [rustLane()]);
    registerMock("lane_delete", () => {
      throw new TokenAgencyError({ code: "TA-5002", message: "线路 lane-jp 被 net.default_lane 引用" });
    });

    renderPage(<NetworkPage />);
    await openLaneMenu();
    fireEvent.click(screen.getByTestId("network-lanes-delete"));
    fireEvent.click(await screen.findByTestId("network-lanes-delete-confirm"));

    const error = await screen.findByTestId("network-lanes-delete-error");
    expect(error.textContent).toContain("TA-5002");
    expect(error.textContent).toContain("net.default_lane");
    expect(screen.queryByTestId("network-lanes-delete-cascade")).toBeNull();
    expect(screen.getByTestId("network-lanes-delete-dialog")).toBeTruthy();
  });

  it("连带清单：通道矩阵里 lane_id 等于这条线路的格数；矩阵读不到就一个字都不写", async () => {
    registerMock("lane_list", () => [rustLane()]);
    registerMock("channel_matrix", () => ({
      models: [],
      credentials: [],
      cells: [
        { model_id: "m", credential_id: "c1", channel_id: "ch-1", lane_id: "lane-jp", tags: [], enabled: true },
        { model_id: "m", credential_id: "c2", channel_id: "ch-2", lane_id: "lane-jp", tags: [], enabled: true },
        { model_id: "m", credential_id: "c3", channel_id: "ch-3", lane_id: "lane-direct", tags: [], enabled: true },
      ],
    }));

    renderPage(<NetworkPage />);
    await openLaneMenu();
    fireEvent.click(screen.getByTestId("network-lanes-delete"));
    const list = await screen.findByTestId("network-lanes-delete-consequences");
    expect(list.textContent).toContain("2");
    expect(list.textContent).toContain("通道");
  });

  /**
   * 第 2 轮：**线路表的批量删除整块删掉了**（用户裁决"没必要的形式"）。
   *
   * 理由（也写进了 `LanesSection` 的注释）：线路不是用户维护的清单 —— 它由后端在服务商页
   * 选「网络代理」时按需建出，一屏通常 1–3 条，而行内 `⋮` 里已经有「删除」。要用户先学会
   * "勾选 → 一次确认 → 逐条调用"只为了删两条，是形式大于功能（规则 3：一屏一个主操作）。
   * 删掉之后线路表**恒为 6 列**（勾选框列不再出现）。订阅表那一条批量**保留**（那份清单
   * 是用户自己维护的，可以有很多份）。
   *
   * 这条是**反回归闸门**：批量控件不许悄悄回来（`queryByTestId` 只认真正进 DOM 的东西）。
   */
  it("线路表没有批量选择：没有勾选框列，也没有「已选 N 条 / 删除选中」；行内删除仍在 ⋮ 里", async () => {
    registerMock("lane_list", () => [
      rustLane(),
      rustLane({ lane: { id: "lane-hk", name: "香港线", kind: "direct", enabled: true } }),
    ]);

    renderPage(<NetworkPage />);
    await screen.findByTestId("network-lanes-table");

    for (const gone of [
      "network-lanes-select",
      "network-lanes-select-all",
      "network-lanes-selected-count",
      "network-lanes-delete-selected",
    ]) {
      expect(screen.queryByTestId(gone), `${gone} 不该再存在`).toBeNull();
    }
    // 两条线路时也没有勾选框（订阅表那一条与本区块无关）
    expect(within(screen.getByTestId("network-lanes")).queryAllByRole("checkbox")).toHaveLength(0);
    // 表恒为 6 列：勾选框列不再出现（规则 4）
    expect(within(screen.getByTestId("network-lanes-table")).getAllByRole("columnheader")).toHaveLength(
      6,
    );
    // 功能一个都没少：删除进 ⋮ 菜单，仍然先二次确认
    await openLaneMenu(1);
    expect(screen.getByTestId("network-lanes-delete")).toBeTruthy();
  });
});

// ================================================================ ⑬ /network/assignments —— 已删除
//
// [docs/55]（第 322 轮）这一页整页删除，连同它显示的 `lane_assignments`（最终走哪条线 + 来自
// 哪一级）/ `lane_pick_options`（候选）/ `lane_reverse_lookup`（倒排）三条命令。
// "来源哪一级"这个问题本身不再成立：生效线路只有 `provider.lane ?? net.default_lane ?? direct`。

// ================================================================ ⑭ /network 常显：没有"启用网络代理"这个动作（[19 §2.2]）
//
// 用户原话（[19] 引）：「网络代理默认开启，反正默认直连，也不用"启动"了」。
// 上一轮的"三项都空 → 只渲染一个选装入口"（`network-optional` / `network-optional-enable`）**被明确推翻**：
// 这一页现在**永远**渲染出口 / 订阅 / 线路，没有配置时每一块给**诚实的空态** ——
// 但页面上**没有任何"启用网络代理"的动作**（直连本来就是可用状态）。
//
// 这一组的重点仍是**反向断言**：作废的那两个 testid **不许再存在**。
// "不显示"的验收标准是"页面上没有这些东西"，不是"它们被 CSS 藏起来了"（`queryByTestId` 对两者都能分辨）。

/** 未配置：三项都读到了，且都是空的（`config_get` 给出 `path: null` → 空字符串）。 */
function unconfiguredProps(over: Partial<NetworkBodyProps> = {}): NetworkBodyProps {
  return networkProps({ kernelPath: "", subscriptions: [], lanes: [], ...over });
}

/** 上一轮的选装入口：**作废**（[19 §2.2]），一个都不许再出现。 */
const RETIRED_OPTIONAL_TESTIDS = ["network-optional", "network-optional-enable"];

/** 反向断言：作废的选装块不存在，也不许换个名字留着"启用网络代理"这个动作。 */
function expectNoOptionalBlock(): void {
  for (const testid of RETIRED_OPTIONAL_TESTIDS) {
    expect(screen.queryByTestId(testid), `作废的选装入口不该再出现：${testid}`).toBeNull();
  }
  expect(document.body.textContent ?? "").not.toContain("启用网络代理");
}

describe("⑭ /network 常显：空配置下每块给诚实空态，且没有「启用」这个动作", () => {
  // 整页用例会覆盖 mock（config_get / subscription_list / lane_list）—— 每条用例前先回到基准场景
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("反向断言：network-optional / network-optional-enable 一个都不存在", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    expectNoOptionalBlock();
  });

  it("空配置：出口 / 订阅 / 线路三块都在，各自给诚实的空态（不是一张白纸）", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    expect(screen.getByTestId("network-current-exit")).toBeTruthy();
    expect(screen.getByTestId("network-subscriptions")).toBeTruthy();
    expect(screen.getByTestId("network-subscriptions-empty")).toBeTruthy();
    expect(screen.getByTestId("network-lanes")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();
    // 零副作用承诺不再是"一块版面"，而是页头那个问号（承诺一个字没少，见上面那一条用例）
    expect(screen.queryByTestId("network-zero-side-effect")).toBeNull();
  });

  it("空配置下**订阅**的写入入口仍在（否则第一份订阅永远建不出来）", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    // [docs/57] 表单不再常驻 → 入口是一个按钮。**按钮必须在**（空列表时更是唯一入口），
    // 点开之后表单才出现 —— 两条都断言，否则"入口还在"就只是一句空话。
    expect(screen.getByTestId("network-subscriptions-add-open")).toBeTruthy();
    expect(screen.queryByTestId("network-subscriptions-add-id")).toBeNull();

    openSubscriptionWrite();
    expect(screen.getByTestId("network-subscriptions-add-id")).toBeTruthy();
    cleanup();

    renderPage(<NetworkBody {...unconfiguredProps()} />);
    // [docs/58] 线路**没有**写入入口了（手动建线路的界面删除）→ 空列表上给的是
    // 诚实的空态 + "线路从哪儿来"的说明，不是一条走不通的死路。
    expect(screen.queryByTestId("network-lanes-add-open")).toBeNull();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();

    // 上一轮的"兜底 setup 块"随常显一起作废：区块自己就在，不需要替代品
    expect(screen.queryByTestId("network-subscriptions-setup")).toBeNull();
    expect(screen.queryByTestId("network-lanes-setup")).toBeNull();
  });

  it("没配内核路径 → 出口显示「出口 · 直连」+「还没设置内核路径」，不是错误态、不点红点", () => {
    renderPage(<NetworkBody {...unconfiguredProps()} />);
    const exit = within(screen.getByTestId("network-current-exit"));
    expect(exit.getByTestId("network-direct-label").textContent).toContain("出口");
    expect(exit.getByTestId("network-direct-label").textContent).toContain("直连");
    expect(exit.getByTestId("network-kernel-not-configured").textContent).toContain(
      "还没设置内核路径",
    );
    // 「直连」在这一小块里已经说了两遍（第一行的「出口 · 直连」+ 全局默认线路那一行的「直连」），
    // 所以内核那一行的职责只剩**原因**：它不再复述「（当前直连）」。
    expect(screen.getByTestId("network-current-exit").textContent ?? "").not.toContain("当前直连");
    // "不点红点"是可机械检查的：未配内核路径时**根本没有**内核状态灯
    expect(exit.queryByTestId("network-kernel-state-dot")).toBeNull();
  });

  it("配了内核路径 → 恢复内核状态灯与全局默认线路（直连标签不再出现）", () => {
    renderPage(
      <NetworkBody
        {...networkProps({ kernelPath: "C:/mihomo/mihomo.exe", subscriptions: [], lanes: [] })}
      />,
    );
    const exit = within(screen.getByTestId("network-current-exit"));
    expect(exit.queryByTestId("network-direct-label")).toBeNull();
    expect(exit.queryByTestId("network-kernel-not-configured")).toBeNull();
    expect(exit.getByTestId("network-kernel-state-dot")).toBeTruthy();
    // 内核路径本身也画出来（读到了什么就显示什么）
    expect(exit.getByTestId("network-kernel-path-value").textContent).toContain(
      "C:/mihomo/mihomo.exe",
    );
    // 三块照常渲染：内核路径不再是"是否显示网络区块"的开关
    expect(screen.getByTestId("network-subscriptions-empty")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();
  });

  it("空字符串 / 纯空白的内核路径都算「没配」（不把空格当成已配置）", () => {
    renderPage(
      <NetworkBody {...networkProps({ kernelPath: "   ", subscriptions: [], lanes: [] })} />,
    );
    expect(
      within(screen.getByTestId("network-current-exit")).getByTestId("network-direct-label"),
    ).toBeTruthy();
  });

  it("内核路径还没读到（undefined）→ 出口显示「未读取」，不冒充「直连」", () => {
    // `undefined` = `config_get` 还没回来：那是"不知道"，不是"用户没配"
    renderPage(<NetworkBody {...networkProps({ subscriptions: [], lanes: [] })} />);
    expect(screen.queryByTestId("network-direct-label")).toBeNull();
    expect(screen.getByTestId("network-current-exit").textContent).toContain("未读取");
  });

  it("readKernelPath：整份配置没读到 → undefined；读到了但没填 → 空字符串（两件事不混）", () => {
    // 没读到（`config_get` 还没回来）—— 不是"用户没配"
    expect(readKernelPath(undefined)).toBeUndefined();
    expect(readKernelPath({})).toBeUndefined();
    // 读到了：null / 缺键 / 空串都 = 用户没填
    expect(readKernelPath({ net: { kernel: { path: null } } })).toBe("");
    expect(readKernelPath({ net: { kernel: {} } })).toBe("");
    expect(readKernelPath({ net: {} })).toBe("");
    // 纯空白原样返回（读取层**不改写**后端给的值）；"算不算没配"由 `NetworkBody` 一处判定（上面那条）
    expect(readKernelPath({ net: { kernel: { path: "  " } } })).toBe("  ");
    // 真配置了就给原文（不做任何改写）
    expect(readKernelPath({ net: { kernel: { path: "C:/mihomo/mihomo.exe" } } })).toBe(
      "C:/mihomo/mihomo.exe",
    );
  });

  it("只配了内核路径：三块照常渲染（内核路径不再是「是否显示网络区块」的开关）", () => {
    renderPage(
      <NetworkBody
        {...networkProps({ kernelPath: "C:/mihomo/mihomo.exe", subscriptions: [], lanes: [] })}
      />,
    );
    expect(screen.getByTestId("network-current-exit")).toBeTruthy();
    // 空配置的两块给的是**诚实空态**，不是"藏起来"
    expect(screen.getByTestId("network-subscriptions-empty")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();
    // 订阅的建第一条入口必须还在（[docs/57]：现在是"添加"按钮，表单点开才出现），
    // 且不再需要上一轮的"兜底 setup 块"。
    // [docs/58] 线路没有写入入口了 —— 手动建线路的界面已删除。
    expect(screen.getByTestId("network-subscriptions-add-open")).toBeTruthy();
    expect(screen.queryByTestId("network-lanes-add-open")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-setup")).toBeNull();
    expect(screen.queryByTestId("network-lanes-setup")).toBeNull();
  });

  it("只配了订阅：订阅表在；线路给空态（没有写入入口 —— 线路由服务商页按需建出来）", () => {
    renderPage(
      <NetworkBody
        {...networkProps({ kernelPath: "", subscriptions: [readSubscription(contractSubscription())], lanes: [] })}
      />,
    );
    expect(screen.getByTestId("network-subscriptions-table")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();
    // [docs/58] 线路区块是**只读**的：没有任何写入口（连"添加"按钮都没有）
    expect(screen.queryByTestId("network-lanes-add-open")).toBeNull();
    expect(screen.queryByTestId("network-lanes-add-id")).toBeNull();
    expect(screen.queryByTestId("network-lanes-setup")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-setup")).toBeNull();
  });

  it("整页：mock 基准场景（有订阅 + 有线路）不显示选装入口", async () => {
    renderPage(<NetworkPage />);
    expect(await screen.findByTestId("network-subscriptions-table")).toBeTruthy();
    expect(await screen.findByTestId("network-lanes-table")).toBeTruthy();
    expectNoOptionalBlock();
  });

  it("整页：config_get + subscription_list + lane_list 三项都空 → 常显 + 空态，且没有选装入口", async () => {
    registerMock("config_get", () => ({ net: { kernel: { path: null } } }) as never);
    registerMock("subscription_list", () => []);
    registerMock("lane_list", () => []);

    renderPage(<NetworkPage />);
    expect(await screen.findByTestId("network-subscriptions-empty")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-empty")).toBeTruthy();
    expect(
      within(screen.getByTestId("network-current-exit")).getByTestId("network-direct-label"),
    ).toBeTruthy();
    expectNoOptionalBlock();
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
//
// **第 326 轮：流量从 `/network` 的列表搬进了订阅详情页**（概览列表 14 列 → 6 列，
// 流量属于"点进去再看"的那一类）。断言规则一字未改，只是改在详情页那张"参数"表上跑 ——
// 三态的 testid 仍然是 `network-subscription-traffic`。

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

/**
 * 进**订阅详情页**并等流量那一格出现（用真实路由参数，不是把 id 当 prop 塞进去）。
 *
 * 详情页自己读 `subscription_list` + `subscription_nodes` + `subscription_traffic`，
 * 所以三个 mock 都备上；`trafficResult` 就是 `subscription_traffic` 的返回。
 *
 * `expected` 是这一条用例期待落地后出现的那句话：**必须等它出现再断言** ——
 * 那一格在查询回来之前画的是「未读取」，不等就会在"还没读到"的那一帧上断言。
 */
async function renderTrafficDetail(trafficResult: unknown, expected: string): Promise<HTMLElement> {
  registerMock("subscription_list", () => [contractSubscription()]);
  registerMock("subscription_nodes", () => []);
  registerMock("subscription_traffic", () => trafficResult);
  renderPageAt(
    "/network/subscriptions/sub-jp",
    <Routes>
      <Route path="/network/subscriptions/:id" element={<SubscriptionDetailPage />} />
    </Routes>,
  );
  const cell = await screen.findByTestId("network-subscription-traffic");
  await waitFor(() => expect(cell.textContent ?? "").toContain(expected));
  return cell;
}

describe("⑮ 详情页显示流量与更新时间（三项各自独立，缺哪样不显示哪样）", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("三样都在：更新时间（本地 YYYY-MM-DD HH:mm）+ 已用/总量 + 百分比 + 到期", async () => {
    const cell = await renderTrafficDetail([{ id: "sub-jp", traffic: trafficFixture() }], "已用 92.9 GB / 130 GB");
    const text = cell.textContent ?? "";
    // ① 本地时区、固定格式（不是 ISO 串、不是相对时间）
    expect(text).toMatch(/更新时间：\d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    // ② used = upload + download（1024³，一位小数）+ 后端给的 total
    expect(text).toContain("已用 92.9 GB / 130 GB");
    // ③ used / total 取整
    expect(text).toContain("71%");
    // 到期：Unix 秒 → 本地日期
    expect(text).toContain(`到期：${formatLocalTime(TRAFFIC_EXPIRE_S * 1000, "YYYY-MM-DD")}`);
  });

  it("traffic === null（从没成功刷新过）：只有一句「尚未获取」，一个数字都不显示", async () => {
    const cell = await renderTrafficDetail([{ id: "sub-jp", traffic: null }], "尚未获取");
    expect(cell.textContent).toContain("尚未获取");
    expect(cell.textContent ?? "").not.toMatch(/\d/);
  });

  it("缺 upload（download 有）：不显示「已用」也不显示百分比 —— 不把缺失当 0", async () => {
    const cell = await renderTrafficDetail(
      [{ id: "sub-jp", traffic: trafficFixture({ upload_bytes: null }) }],
      "更新时间：",
    );
    // 更新时间照旧显示（它不依赖 upload）
    expect(cell.textContent).toMatch(/更新时间：\d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    expect(cell.textContent).not.toContain("已用");
    expect(cell.textContent).not.toContain("%");
    expect(cell.textContent).not.toContain("0 B");
  });

  it("缺 total：只显示「已用 92.9 GB」，不显示百分比（也不显示 0 或占位符）", async () => {
    const cell = await renderTrafficDetail(
      [{ id: "sub-jp", traffic: trafficFixture({ total_bytes: null }) }],
      "已用 92.9 GB",
    );
    expect(cell.textContent).toContain("已用 92.9 GB");
    expect(cell.textContent).not.toContain("%");
    expect(cell.textContent).not.toContain("/");
  });

  it("total 为 0：显示 0 B 但**不显示百分比**（不除零、不编一个数）", async () => {
    const cell = await renderTrafficDetail(
      [{ id: "sub-jp", traffic: trafficFixture({ total_bytes: 0 }) }],
      "已用 92.9 GB / 0 B",
    );
    expect(cell.textContent).toContain("已用 92.9 GB / 0 B");
    expect(cell.textContent).not.toContain("%");
  });

  it("upload/download 都是 0（真实观测值）：显示「已用 0 B」与「0%」，不当成缺失", async () => {
    const cell = await renderTrafficDetail(
      [{ id: "sub-jp", traffic: trafficFixture({ upload_bytes: 0, download_bytes: 0 }) }],
      "已用 0 B / 130 GB",
    );
    expect(cell.textContent).toContain("已用 0 B / 130 GB");
    expect(cell.textContent).toContain("0%");
  });

  it("expire_at_s 为 0（契约：0 = 不适用）：不显示「到期」，其余三样照常", async () => {
    const cell = await renderTrafficDetail(
      [{ id: "sub-jp", traffic: trafficFixture({ expire_at_s: 0 }) }],
      "已用 92.9 GB / 130 GB",
    );
    expect(cell.textContent).not.toContain("到期");
    expect(cell.textContent).toContain("已用 92.9 GB / 130 GB");
  });

  it("流量读失败：TA 码原样显示（不吞错，也不把失败画成「尚未获取」）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => []);
    registerMock("subscription_traffic", () => {
      throw new TokenAgencyError({ code: "TA-9001", message: "未注册 mock：subscription_traffic" });
    });
    renderPageAt(
      "/network/subscriptions/sub-jp",
      <Routes>
        <Route path="/network/subscriptions/:id" element={<SubscriptionDetailPage />} />
      </Routes>,
    );
    expect(
      (await screen.findByTestId("network-subscriptions-traffic-error")).textContent,
    ).toContain("TA-9001");
    // 失败**不冒充**「尚未获取」：那一格是空的（读不到就是读不到）
    expect(screen.getByTestId("network-subscription-traffic").textContent).toContain("未读取");
  });

  it("流量还没读到（没有这一行）→「未读取」；traffic:null →「尚未获取」（三态不混）", async () => {
    // `[]` = 后端没给这一行 = 还没读到
    const unread = await renderTrafficDetail([], "未读取");
    expect(unread.textContent).toContain("未读取");
    cleanup();
    // `traffic: null` = 后端明确说"从没成功刷新过"
    const never = await renderTrafficDetail([{ id: "sub-jp", traffic: null }], "尚未获取");
    expect(never.textContent).toContain("尚未获取");
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

// ================================================================ ⑯ 返回条：服务商向导 → /network → 回来（[19 §1.4]）
//
// 服务商两步向导的「让我先设置网络代理」把用户送到 `#/network?from=provider-wizard`，
// 所以 /network 必须给一条**能回去的路**（否则用户配完代理就找不到刚才填了一半的表单）。
// 触发条件是**真实 URL**（不是某个全局开关）：只有 `from=provider-wizard` 才显示。

describe("⑯ /network 返回条：从服务商向导过来时才有，且真的回得去", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("?from=provider-wizard → 顶部返回条 + 返回按钮（真控件、指向 /providers）", async () => {
    renderPageAt("/network?from=provider-wizard", <NetworkPage />);

    const banner = await screen.findByTestId("network-return-banner");
    expect(banner.textContent).toContain("正在为服务商配置网络代理");

    const back = within(screen.getByTestId("network-return-btn")).getByRole("link");
    expect(back.textContent).toContain("返回向导");
    expect(back.getAttribute("href")).toBe("/providers");
    expect(back.hasAttribute("disabled")).toBe(false);
  });

  it("反向断言：没有这个查询参数时不出现返回条", async () => {
    renderPageAt("/network", <NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");
    expect(screen.queryByTestId("network-return-banner")).toBeNull();
    expect(screen.queryByTestId("network-return-btn")).toBeNull();
  });

  it("反向断言：别的 from 取值不算（不是随便什么链接进来都弹返回条）", async () => {
    renderPageAt("/network?from=somewhere-else", <NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");
    expect(screen.queryByTestId("network-return-banner")).toBeNull();
  });
});

// ================================================================ ⑰ /network/nodes 节点清单：接真命令（[19 §2.3]）
//
// 用户原话：「我添加了 clash 订阅，刷新了，进去也是为什么这里没有完整的节点列表」+「没有节点啊」。
// 事实（已核实）：`subscription_nodes` 后端**已注册已实现**（`src-tauri/src/commands/net.rs:780`），
// 上一版页面里那句"本页尚未接入"是**过期说明**。所以这一组钉四件事：
//
// 1. **不带 `group`** 调 `subscription_nodes({ id })`：后端带 `group` 会**直接报错**
//    （`net.rs:765-773`：解析层不保留组成员关系）；前端不许自己解析、不许伪造节点；
// 2. **硬前置先判**：`last_update_ms == null && error == null` = 这份订阅还没解析过 ——
//    这时 `latest_outcome` 返回的是**错误**（不是空数组），所以**这一轮根本不发命令**，
//    只给 `nodes-empty`（"还没解析过"那一档）+ `nodes-refresh` 引导；
// 3. 空态**必须分两档**（还没解析过 / 该订阅下确实没有节点），文案不同；
// 4. `nodes-refresh` 真调 `subscription_refresh`，**然后**才重取 `subscription_nodes`。

describe("⑰ /network/nodes 节点清单：真调 subscription_nodes，不自己解析、不伪造", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("已解析的订阅：调 subscription_nodes({ id })（**不带 group**），逐行列出后端给的节点", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    const nodes = recordingMock([
      { name: "东京-03", type: "vmess", group: USER_GROUP, latency_ms: 82, ok: true },
      { name: "大阪-01", type: "trojan", latency_ms: 96, ok: true },
    ]);
    registerMock("subscription_nodes", nodes.handler);

    renderPage(<NodesPage />);

    expect(await screen.findByTestId("nodes-list")).toBeTruthy();
    await waitFor(() => expect(nodes.calls).toHaveLength(1));
    // 入参只有 id：带 group 会被后端拒绝（宁可不做筛选，也不放一个点了会报错的控件）
    expect(nodes.calls[0]).toEqual({ id: "sub-jp" });

    const rows = screen.getAllByTestId("nodes-row");
    expect(rows.length).toBe(2);
    expect(rows[0]?.textContent).toContain("东京-03");
    expect(rows[0]?.textContent).toContain("vmess");
    expect(rows[1]?.textContent).toContain("大阪-01");
    // 用户数据原样（订阅里的组名带 emoji：一个字符都不改）
    expect(rows[0]?.textContent).toContain(USER_GROUP);
  });

  it("只有一个订阅时选择器也显示（不隐藏）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => []);

    renderPage(<NodesPage />);

    await waitFor(() => expect(screen.getAllByTestId("nodes-subscription").length).toBe(1));
    expect(within(screen.getByTestId("nodes-subscription")).getByRole("combobox")).toBeTruthy();
  });

  it("多个订阅：换一个订阅就用新 id 重新取节点", async () => {
    const first = contractSubscription();
    const inner = first["subscription"] as Record<string, unknown>;
    registerMock("subscription_list", () => [
      first,
      contractSubscription({ subscription: { ...inner, id: "sub-hk", name: "机场B" } }),
    ]);
    const nodes = recordingMock([{ name: "香港-01", type: "ss", latency_ms: 40, ok: true }]);
    registerMock("subscription_nodes", nodes.handler);

    renderPage(<NodesPage />);

    await screen.findByTestId("nodes-list");
    await waitFor(() => expect(nodes.calls).toHaveLength(1));
    await chooseOption("nodes-subscription", "机场B");
    await waitFor(() => expect(nodes.calls.some((call) => call["id"] === "sub-hk")).toBe(true));
  });

  it("还没解析过（last_update_ms 为 null）→ 先判前置：这一轮**不发** subscription_nodes", async () => {
    registerMock("subscription_list", () => [rustSubscription()]);
    const nodes = recordingMock([]);
    registerMock("subscription_nodes", nodes.handler);

    renderPage(<NodesPage />);

    const empty = await screen.findByTestId("nodes-empty");
    expect(empty.textContent).toContain("还没有解析结果");
    // 前置不满足时后端会**报错**（不是返回空数组），所以命令必须在判断之后才发
    expect(nodes.calls).toHaveLength(0);
    // 引导：给一个真的会调 subscription_refresh 的刷新按钮
    expect(screen.getByTestId("nodes-refresh")).toBeTruthy();
  });

  it("解析过但没有节点：nodes-empty 换第二档文案（不写「还没解析过」）", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => []);

    renderPage(<NodesPage />);

    const empty = await screen.findByTestId("nodes-empty");
    // 文案取真源（"这个分组下没有节点"那句带着已作废的"分组"概念，第 326 轮改过）
    expect(empty.textContent).toBe(i18n.t("network.nodes.emptyGroup"));
    expect(empty.textContent).not.toContain("还没有解析结果");
  });

  it("后端真失败：nodes-error 原样显示后端原文（含 TA 码），不吞、不改写", async () => {
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => {
      throw new TokenAgencyError({
        code: "TA-4001",
        message: "订阅 sub-jp 还没有已采纳的版本（先用 subscription_refresh 拉一次）",
      });
    });

    renderPage(<NodesPage />);

    const error = await screen.findByTestId("nodes-error");
    expect(error.textContent).toContain("TA-4001");
    expect(error.textContent).toContain("还没有已采纳的版本（先用 subscription_refresh 拉一次）");
  });

  it("nodes-refresh：真调 subscription_refresh({ id, force: true })，再重取 subscription_nodes", async () => {
    let nodeCalls = 0;
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", () => {
      nodeCalls += 1;
      return [{ name: "东京-03", type: "vmess", ok: true }];
    });
    const refresh = recordingMock({ ok: true, node_count: 12, group_count: 4 });
    registerMock("subscription_refresh", refresh.handler);

    renderPage(<NodesPage />);

    await screen.findByTestId("nodes-list");
    await waitFor(() => expect(nodeCalls).toBe(1));

    fireEvent.click(screen.getByTestId("nodes-refresh"));

    await waitFor(() => expect(refresh.calls).toHaveLength(1));
    expect(refresh.calls[0]).toEqual({ id: "sub-jp", force: true });
    // 刷新之后**再**取一次节点清单（顺序：先刷新，后取清单）
    await waitFor(() => expect(nodeCalls).toBeGreaterThan(1));
    expect((await screen.findByTestId("nodes-refresh-result")).textContent).toContain("12");
  });
});

// ================================================================ ⑳ 节点级启用 / 关闭（用户第 202 轮）
//
// 用户原话：「订阅里面的节点，也将服务商的『模型』一样设置一个启用或关闭的功能，对了，这个也要
// 那种有全选功能，默认全部选择」。契约已冻结（`bindings.ts`）：
//
// * 语义 = **默认全部启用** —— 库里存的是"**关掉的那些**"（`Subscription.disabled_nodes`），
//   所以机场新增的节点默认就是开着的；
// * `subscription_nodes_enabled({ id, nodes, enabled })` 是**批量**：`nodes` 空清单后端**明确报错**
//   （空清单不是「全部」，那会静默改掉整份订阅），返回**改完之后**的完整清单
//   （页面拿它回填缓存，**不再**发一次 `subscription_nodes`）。
//
// 这一组钉六件事：
//   1. 默认全开（每行开关都是 on、表头全选勾上、计数是**裸数字**）；
//   2. 单行开关的入参**逐字**只有那一条名字 + 目标状态；
//   3. 全选 + 「关闭所选」= **一次**调用带**全部**名字、`enabled:false`；再来一遍启用 → 回到全开；
//   4. 一条都没选中 → 两个批量按钮禁用，并且**一个请求都不发**（空清单不能发给后端）；
//   5. 三态：取消一条 → 表头 `aria-checked=mixed`（部分）；
//   6. 后端拒绝 → `network-nodes-bulk-result` 显示**后端原文**（含 TA 码），不吞、不装成功。

describe("⑳ /network/nodes 节点级启用 / 关闭：逐行开关 + 全选 + 批量（用户第 202 轮）", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  const NODES = [
    { name: "东京-03", type: "vmess", ok: true },
    { name: "大阪-01", type: "trojan", ok: true },
    { name: "首尔-02", type: "ss", ok: false },
  ];
  const ALL = NODES.map((node) => node.name);

  /**
   * 订阅节点 + 批量启停的**有状态** mock：与后端同一套语义（只记"关掉的"、返回改完的完整清单），
   * 并把每次 `subscription_nodes_enabled` 的入参留下来 —— 入参形状对不对比"页面长得像不像"重要得多。
   */
  function nodesStore(over: { fail?: boolean } = {}) {
    const disabled = new Set<string>();
    const calls: Record<string, unknown>[] = [];
    const list = (): Record<string, unknown>[] =>
      NODES.map((node) => ({ ...node, enabled: !disabled.has(node.name) }));
    const setEnabled = (args: Record<string, unknown> | undefined): unknown => {
      calls.push(args ?? {});
      if (over.fail === true) {
        throw new TokenAgencyError({ code: "TA-4001", message: "订阅 sub-jp 不在配置里" });
      }
      const a = (args ?? {}) as { nodes?: string[]; enabled?: boolean };
      for (const name of a.nodes ?? []) {
        if (a.enabled === true) disabled.delete(name);
        else disabled.add(name);
      }
      return list();
    };
    return { list, setEnabled, calls, disabled };
  }

  /** 每行的启用开关：testid 在外层容器上，真 `input` 在它里面（同 `providers-line-enabled`）。 */
  function switches(): HTMLInputElement[] {
    return screen
      .getAllByTestId("network-nodes-enabled")
      .map((box) => box.querySelector("input") as HTMLInputElement);
  }

  /** 表头的「全选」复选框（真 `input`：`checked` 与 `aria-checked` 都在它身上）。 */
  function selectAllBox(): HTMLInputElement {
    return within(screen.getByTestId("network-nodes-select-all")).getByRole(
      "checkbox",
    ) as HTMLInputElement;
  }

  /** 「全选」= 用户真会点的那一下：确保它是勾上的（已经勾上就不动，同 E2E 的 `wdToggleCheckbox`）。 */
  function selectAll(): void {
    if (!selectAllBox().checked) fireEvent.click(selectAllBox());
  }

  /** 渲染 + 等清单出来（3 条节点）。 */
  async function renderNodes(): Promise<ReturnType<typeof nodesStore>> {
    const store = nodesStore();
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", store.list);
    registerMock("subscription_nodes_enabled", store.setEnabled);
    renderPage(<NodesPage />);
    await screen.findByTestId("nodes-list");
    return store;
  }

  it("默认全部启用：开关都是 on、表头全选勾上、计数是裸数字", async () => {
    const store = await renderNodes();

    await waitFor(() => expect(switches()).toHaveLength(3));
    expect(switches().every((input) => input.checked)).toBe(true);
    // 默认全选：表头勾上、不是"部分"
    expect(selectAllBox().checked).toBe(true);
    expect(selectAllBox().getAttribute("aria-checked")).not.toBe("mixed");
    // 计数**只有数字**（[docs/30 §1] 极简：不留「已选 N 条」那种话）
    expect(screen.getByTestId("network-nodes-selected-count").textContent).toBe(
      i18n.t("network.nodes.selectedCount", { count: 3 }),
    );
    expect(screen.getByTestId("network-nodes-selected-count").textContent).toBe("3");
    // 一条都没写：加载清单不算写操作
    expect(store.calls).toHaveLength(0);
    expect((screen.getByTestId("network-nodes-enable-selected") as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  it("关掉某一条：入参逐字是 { id, nodes: [<那条名字>], enabled: false }，界面按后端返回值变", async () => {
    const store = await renderNodes();
    await waitFor(() => expect(switches()).toHaveLength(3));

    fireEvent.click(switches()[0]!);

    await waitFor(() => expect(store.calls).toHaveLength(1));
    // 逐字：只有那一条名字、目标状态就是开关的新状态
    expect(store.calls[0]).toEqual({ id: "sub-jp", nodes: ["东京-03"], enabled: false });
    expect(store.disabled.has("东京-03")).toBe(true);
    // 界面与后端一致（后端返回的清单回填到了缓存里）
    await waitFor(() => expect(switches()[0]!.checked).toBe(false));
    expect(switches()[1]!.checked).toBe(true);
  });

  it("全选 + 「关闭所选」= 一次调用带全部名字；再来一遍「启用所选」→ 回到全开", async () => {
    const store = await renderNodes();
    await waitFor(() => expect(switches()).toHaveLength(3));

    selectAll();
    fireEvent.click(screen.getByTestId("network-nodes-disable-selected"));

    // **一次**调用带**全部**节点名（不是前端循环发 N 条）
    await waitFor(() => expect(store.calls).toHaveLength(1));
    expect(store.calls[0]).toEqual({ id: "sub-jp", nodes: ALL, enabled: false });
    await waitFor(() => expect(switches().every((input) => !input.checked)).toBe(true));
    // 成功回执：极简一句（含条数），由后端返回值触发的那一次写算出来
    expect(screen.getByTestId("network-nodes-bulk-result").textContent).toBe(
      i18n.t("network.nodes.bulk.disabled", { count: 3 }),
    );

    // 再次全选（此刻默认仍是全选 → 这一步只是"确保勾上"）→ 「启用所选」→ 回到全开
    selectAll();
    fireEvent.click(screen.getByTestId("network-nodes-enable-selected"));

    await waitFor(() => expect(store.calls).toHaveLength(2));
    expect(store.calls[1]).toEqual({ id: "sub-jp", nodes: ALL, enabled: true });
    await waitFor(() => expect(switches().every((input) => input.checked)).toBe(true));
    expect(store.disabled.size).toBe(0);
  });

  it("没有选中任何一条：两个批量按钮禁用，并且**一个请求都不发**（空清单后端会报错）", async () => {
    const store = await renderNodes();
    await waitFor(() => expect(switches()).toHaveLength(3));

    // 取消全选 = 一条都不选
    fireEvent.click(selectAllBox());

    await waitFor(() =>
      expect(screen.getByTestId("network-nodes-selected-count").textContent).toBe("0"),
    );
    const enable = screen.getByTestId("network-nodes-enable-selected") as HTMLButtonElement;
    const disable = screen.getByTestId("network-nodes-disable-selected") as HTMLButtonElement;
    expect(enable.disabled).toBe(true);
    expect(disable.disabled).toBe(true);

    // 点它们（disabled 的按钮点了什么都不会发生）—— 关键是**没有请求**
    fireEvent.click(enable);
    fireEvent.click(disable);
    expect(store.calls).toHaveLength(0);
  });

  it("三态：只选中一部分时表头是「部分」（aria-checked=mixed），点它回到全选", async () => {
    await renderNodes();
    await waitFor(() => expect(switches()).toHaveLength(3));

    // 行内的选择框：取消第一条
    fireEvent.click(
      within(screen.getAllByTestId("network-nodes-select")[0]!).getByRole("checkbox"),
    );

    await waitFor(() =>
      expect(screen.getByTestId("network-nodes-selected-count").textContent).toBe("2"),
    );
    expect(selectAllBox().checked).toBe(false);
    expect(selectAllBox().getAttribute("aria-checked")).toBe("mixed");

    // 点表头全选 → 三态回到"全选"
    fireEvent.click(selectAllBox());
    await waitFor(() =>
      expect(screen.getByTestId("network-nodes-selected-count").textContent).toBe("3"),
    );
    expect(selectAllBox().checked).toBe(true);
    expect(selectAllBox().getAttribute("aria-checked")).not.toBe("mixed");
  });

  it("后端拒绝：network-nodes-bulk-result 显示后端原文（含 TA 码），不吞、不装成功", async () => {
    const store = nodesStore({ fail: true });
    registerMock("subscription_list", () => [contractSubscription()]);
    registerMock("subscription_nodes", store.list);
    registerMock("subscription_nodes_enabled", store.setEnabled);

    renderPage(<NodesPage />);
    await screen.findByTestId("nodes-list");

    fireEvent.click(screen.getByTestId("network-nodes-disable-selected"));

    const result = await screen.findByTestId("network-nodes-bulk-result");
    expect(result.textContent).toContain("TA-4001");
    expect(result.textContent).toContain("订阅 sub-jp 不在配置里");
    // 失败时**不写回执**（没有"已关闭 N 条"那种成功话术）
    expect(result.textContent).not.toContain("已关闭");
  });
});

// ================================================================ ⑱ 线路测速：lane_test 真用（[19 §2.4]）
//
// 「要么真用，要么不放」：这里选了**真用**。没有装配内核时后端返回错误，
// 页面**原样显示**（含 TA 码）—— 不吞、不假装成功、也不放禁用占位。

describe("⑱ /network 线路表：lane_test 真调，结果原样显示", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("每行一个 network-lanes-test：调 lane_test({ id })，逐节点延迟列在 network-lanes-test-result", async () => {
    registerMock("lane_list", () => [rustLane()]);
    const test = recordingMock([
      { node: "东京-03", latency_ms: 82, ok: true },
      { node: "大阪-01", latency_ms: null, ok: false },
    ]);
    registerMock("lane_test", test.handler);

    renderPage(<NetworkPage />);

    // 判据 B：测试按钮在行内 `⋮` 菜单里 → 先打开菜单（每行一个入口，只是不再摊在格子里）
    await openLaneMenu();
    const button = screen.getByTestId("network-lanes-test");
    expect(button.hasAttribute("disabled")).toBe(false);
    fireEvent.click(button);

    await waitFor(() => expect(test.calls).toHaveLength(1));
    // 入参只有 id（`node` 是后端可选的，前端不替用户挑节点）
    expect(test.calls[0]).toEqual({ id: "lane-jp" });

    const result = await screen.findByTestId("network-lanes-test-result");
    expect(result.textContent).toContain("东京-03");
    expect(result.textContent).toContain("82ms");
    expect(result.textContent).toContain("大阪-01");
  });

  it("内核未装配：后端错误原样显示（含 TA 码），不吞也不装成功", async () => {
    registerMock("lane_list", () => [rustLane()]);
    registerMock("lane_test", () => {
      throw new TokenAgencyError({ code: "TA-4001", message: "没有装配内核，无法测速" });
    });

    renderPage(<NetworkPage />);

    await openLaneMenu();
    fireEvent.click(screen.getByTestId("network-lanes-test"));

    const result = await screen.findByTestId("network-lanes-test-result");
    expect(result.textContent).toContain("TA-4001");
    expect(result.textContent).toContain("没有装配内核，无法测速");
  });
});

// ================================================================ ⑲ [docs/22] 长简介收进「问号圆圈 + 悬停提示」
//
// 用户原话：「我发现里面的简介好长，好多字，这样吧尽量缩短字数，那些比较介绍做成一个问好加一个圆圈，
// 鼠标移上去能看到提示那种」。这一组按 [docs/22 §2/§3/§5] 钉三件事：
//
// 1. 收走的正文**一个字都没丢** —— 它是问号圆圈的 `aria-label`
//    （提示气泡是懒渲染的，读屏与"只看无障碍树"的调用方仍然拿得到全文）；
// 2. 那段正文**不再内联**渲染（版面上只剩一个 16px 的圆圈）；
// 3. §2 红线里的东西（空态 / 错误原文 / 数值 / 结论 / 按钮文案 / I2 承诺）**仍然内联** ——
//    把状态或错误塞进 tooltip 是"藏问题"，不是"排版"。
//
// 另外钉住 §3 的「同一区块最多一个 `-help`」：一个区块的几句解释合成一个问号，而不是一排问号。

describe("⑲ [docs/22] 长简介收进问号圆圈（HelpTip）", () => {
  const subs: SubscriptionRow[] = [readSubscription(contractSubscription())];
  const lanes: LaneRow[] = [readLane(contractLane())];

  /** 文案取真源：断言直接用 zh-CN 里的全文，而不是把长句在测试里再抄一遍。 */
  function copy(key: I18nKey): string {
    return i18n.t(key);
  }

  /** 这段正文收进了问号圆圈：`aria-label` 有全文，版面上不再有它。 */
  function expectCollected(testId: string, key: I18nKey): void {
    const text = copy(key);
    expect(
      screen.getByTestId(testId).getAttribute("aria-label"),
      `${testId} 的 aria-label 应当是全文`,
    ).toContain(text);
    expect(screen.queryByText(text), `${key} 不该再内联：${text}`).toBeNull();
  }

  it("/network：区块说明与成段解释收进问号圆圈（aria-label 是全文）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);

    expectCollected("network-current-exit-help", "network.currentExit.hint");
    expectCollected("network-subscriptions-help", "network.subscriptions.description");
    expectCollected("network-lanes-help", "network.lanes.description");

    // [docs/57] 订阅写表单自己的问号搬进了对话框 → 先点开才存在（不再是常驻页面的一部分）。
    // [docs/58] 线路那份写表单的说明随界面一起删了，所以这里不再有它。
    openSubscriptionWrite();
    expectCollected("network-subscriptions-write-help", "network.subscriptions.write.hint");
  });

  it("/network：同一区块最多一个 -help（一个区块的几句解释合成一个问号）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);

    // 线路区块：区块说明与五句 hint 合成**一个**问号（testid 用区块自己的 testid）
    const lanesHelp = screen.getByTestId("network-lanes-help");
    for (const key of [
      "network.lanes.description",
      "network.lanes.kindHint",
      "network.lanes.channelCountHint",
      "network.lanes.sampleHint",
      "network.lanes.row.setNodeHint",
      "network.lanes.testHint",
    ] as const) {
      expect(lanesHelp.getAttribute("aria-label"), `${key} 应当进同一个提示`).toContain(copy(key));
      expect(screen.queryByText(copy(key)), `${key} 不该再内联`).toBeNull();
    }

    // 订阅区块的说明 + 3 句解释合成**同一个**问号（第 326 轮：原来那 3 句挂在标题下面单独一行，
    // 于是版面上多了一个"悬在工具栏上方、没有标签"的孤儿问号 —— 现在那个容器整个不存在了）
    const subsHelp = screen.getByTestId("network-subscriptions-help");
    for (const key of [
      "network.subscriptions.description",
      "network.subscriptions.urlHint",
      "network.subscriptions.healthHint",
      "network.subscriptions.row.refreshHint",
    ] as const) {
      expect(subsHelp.getAttribute("aria-label"), `${key} 应当进同一个提示`).toContain(copy(key));
      expect(screen.queryByText(copy(key)), `${key} 不该再内联`).toBeNull();
    }
    // 那个孤儿容器（单独一行、没有标签的问号）**不许**再存在
    expect(screen.queryByTestId("network-subscriptions-hint")).toBeNull();
    expect(screen.queryByTestId("network-subscriptions-hint-help")).toBeNull();

    // 区块容器只挂**一个**问号。
    // [docs/57] 两个写表单搬进对话框之后，它们不再算在区块容器里 ——
    // 所以这里分别验：区块自己、以及**点开之后**的表单各一个。
    expect(
      within(screen.getByTestId("network-subscriptions")).getAllByTestId(/-help$/),
      "订阅区块里不该出现一排问号",
    ).toHaveLength(1);
    // 线路区块现在只有**区块自己**那一个问号（[docs/58]：写表单整块删除，里面那个也没了）
    expect(within(screen.getByTestId("network-lanes")).getAllByTestId(/-help$/)).toHaveLength(1);
    cleanup();

    // 订阅写表单自己那一个问号：点开才有（这正是"不常驻"的可执行形式）
    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    openSubscriptionWrite();
    expect(
      within(screen.getByTestId("network-subscriptions-write")).getAllByTestId(/-help$/),
      "订阅写表单里不该出现一排问号",
    ).toHaveLength(1);
  });

  it("/network：红线留在版面上（空态 / 错误原文 / 数值 / 按钮文案）", () => {
    renderBody(<NetworkBody {...networkProps({ subscriptions: [], lanes: [] })} />);
    expect(screen.getByTestId("network-subscriptions-empty").textContent).toContain("还没有订阅");
    expect(screen.getByTestId("network-lanes-empty").textContent).toContain("还没有线路");
    cleanup();

    renderBody(
      <NetworkBody
        {...networkProps({
          subscriptionsError: new Error("[TA-9001] 未注册 mock：subscription_list"),
          lanesError: new Error("[TA-4001] 配置不可用"),
        })}
      />,
    );
    expect(screen.getByTestId("network-subscriptions-error").textContent).toContain("TA-9001");
    expect(screen.getByTestId("network-lanes-error").textContent).toContain("TA-4001");
    cleanup();

    renderBody(<NetworkBody {...networkProps({ subscriptions: subs, lanes })} />);
    expect(within(screen.getByTestId("network-subscriptions-table")).getByText("12")).toBeTruthy();
    expect(within(screen.getByTestId("network-lanes-table")).getByText("82ms")).toBeTruthy();
    // [docs/57] 「保存订阅」现在是对话框里的主操作 → 先点开「添加订阅」
    openSubscriptionWrite();
    expect(screen.getByRole("button", { name: "保存订阅" })).toBeTruthy();
    // I2 的承诺（"不设置系统代理"）**仍然在用户看得见的地方**：它在页头那个问号里
    // （第 326 轮把那张只写一句免责的卡片删了 —— 见 ⑥ 组里逐句断言那条）。
  });

  it("/network/nodes：区块说明收进问号圆圈；声明来源的两句合成一个", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);

    // 第一块（订阅解析出的节点）**不再重复页头那句说明**：`network.nodes.description` 现在只有
    // 页头（`page-header-help`）一份 —— 原来这一块自己又挂了一遍一模一样的说明，
    // 于是同一句话在一屏里出现两次。所以这里反向断言"它不在了"（连问号都不该有）。
    expect(screen.queryByTestId("network-nodes-command-section-help")).toBeNull();
    expect(screen.queryByText(copy("network.nodes.description"))).toBeNull();
    // 标题也不再复用页头标题：同一屏两个「节点列表」（H1 + 区块标题）已作废
    expect(screen.queryByRole("heading", { name: "节点列表" })).toBeNull();
    expect(
      screen.getByRole("heading", { name: copy("network.nodes.list.title") }),
    ).toBeTruthy();
    expectCollected("network-nodes-samples-section-help", "network.nodes.samples.description");
    // 声明来源区块：description 与 hint 是同一件事的两个说法 → 合成一个问号
    const declared = screen.getByTestId("network-nodes-declared-section-help");
    expect(declared.getAttribute("aria-label")).toContain(copy("network.nodes.declared.description"));
    expect(declared.getAttribute("aria-label")).toContain(copy("network.nodes.declared.hint"));
    expect(screen.queryByText(copy("network.nodes.declared.hint"))).toBeNull();
    expect(
      within(screen.getByTestId("network-nodes-declared-section")).getAllByTestId(/-help$/),
    ).toHaveLength(1);
  });

  it("/network/nodes：红线留在版面上（空态 / 错误原文 / 节点数值）", () => {
    renderBody(<NodesBody {...nodesProps({ lanes: [] })} />);
    // 空态的一句用真源断言（文案改过一轮：说明都收进标题旁的问号里）
    expect(screen.getByTestId("network-nodes-empty").textContent).toBe(
      i18n.t("network.nodes.samples.empty"),
    );
    expect(screen.getByTestId("network-nodes-declared-empty").textContent).toContain("没有可显示的线路");
    cleanup();

    renderBody(
      <NodesBody {...nodesProps({ lanesError: new Error("[TA-9001] 未注册 mock：lane_list") })} />,
    );
    expect(screen.getByTestId("network-nodes-error").textContent).toContain("TA-9001");
    cleanup();

    renderBody(<NodesBody {...nodesProps({ lanes: [readLane(rustLane())] })} />);
    const samples = within(screen.getByTestId("network-nodes-samples"));
    expect(samples.getByText("82ms")).toBeTruthy();
    expect(samples.getByText("200")).toBeTruthy();
  });

  // [docs/55]（第 322 轮）这里原有三条 `/network/assignments` 的问号圆圈用例 ——
  // 那一页整页删除，所以它们连同 `AssignmentsBody` 一起删除。
});

// ================================================================ [docs/26 §2] 一级界面省略

describe("[docs/26 §2] 订阅名 / 订阅地址 / 线路节点名：省略后全文仍在 DOM", () => {
  it("三个长值都整段留在 DOM 里；没溢出时一个提示都不挂", () => {
    const name = "机场A（东京专线 · 备用 · 团队共享）";
    const longNode = "东京 BGP 专线 01（仅工作日 09:00 后可用）";
    const inner = contractSubscription()["subscription"] as Record<string, unknown>;
    renderBody(
      <NetworkBody
        {...networkProps({
          subscriptions: [
            readSubscription(
              contractSubscription({
                subscription: {
                  ...inner,
                  name,
                  url: "https://example.com/api/v1/client/subscribe?token=REPLACE_ME&flag=clash&area=jp-tokyo-bgp",
                },
              }),
            ),
          ],
          lanes: [
            readLane(
              contractLane({ lane: { ...(contractLane()["lane"] as Record<string, unknown>), node: longNode } }),
            ),
          ],
        })}
      />,
    );

    const nameElement = screen.getByTestId("network-subscription-name-ellipsis");
    expect(nameElement.textContent).toBe(name);
    // 地址里的 token 会被遮蔽（那是本页既有行为），其余部分原样留在 DOM 里
    expect(screen.getByTestId("network-subscription-url-ellipsis").textContent).toContain(
      "flag=clash&area=jp-tokyo-bgp",
    );
    expect(screen.getByTestId("network-lane-node-ellipsis").textContent).toBe(longNode);
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(nameElement.getAttribute("tabindex")).toBeNull();
  });
});

// ================================================================ ⑳ 极简：同一个事实只说一遍
//
// 这一组是**反向断言**：「说第二遍」的那一处现在必须**不在**。
// `queryByText` / `queryByTestId` 只认真正进了 DOM 的东西（提示气泡是懒渲染的），
// 所以"被 CSS 藏起来"或"换个名字留着"都骗不过它。
//
// 判据来自本轮任务：① 同一屏里同一个事实不许说两遍；② >12 字的解释收进问号（红线除外）；
// ③ 列表里每行重复写的东西收进列头 / 区块问号；④ **只删重复文案，一个真控件都不删**。

describe("⑳ 极简：重复的那一遍不在了，真控件一个都没少", () => {
  beforeEach(() => {
    registerBaselineMocks();
  });

  it("/network：直连行不再逐行重复「直连直接指 DIRECT…」，它进了线路区块的问号", async () => {
    const directLane = readLane(
      rustLane({
        lane: { id: "lane-direct", name: "直连（本机网络）", kind: "direct", enabled: true },
      }),
    );
    renderBody(<NetworkBody {...networkProps({ lanes: [directLane, readLane(rustLane())] })} />);

    const copy = i18n.t("network.lanes.row.directNoGroup");
    // 版面上一个都不许有：原来是**每一行**一份（N 条直连线路 = N 遍同一句话）
    expect(screen.queryByText(copy)).toBeNull();
    // 一个字都没丢 —— 它是区块那一个问号的 `aria-label`
    expect(screen.getByTestId("network-lanes-help").getAttribute("aria-label")).toContain(copy);

    // 只删重复文案：两行各自的测速 / 删除都还在，非直连那一行的"钉住节点"也在。
    // 第 2 轮（判据 B）：这些控件收进了行内 `⋮` 菜单，所以逐行打开菜单来验 —— 控件一个没少，
    // 只是不再摊在格子里（那正是"行被折成两行"的原因）。
    await openLaneMenu(0); // 直连行：没有「钉住节点」（它没有可切换的组）
    expect(screen.getByTestId("network-lanes-test")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-delete")).toBeTruthy();
    expect(screen.queryByTestId("network-lanes-set-node")).toBeNull();
    expect(screen.queryByTestId("network-lanes-set-node-submit")).toBeNull();

    await openLaneMenu(1); // 非直连行：钉住节点（含输入框）在菜单里
    expect(screen.getByTestId("network-lanes-set-node")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-set-node-submit")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-test")).toBeTruthy();
    expect(screen.getByTestId("network-lanes-delete")).toBeTruthy();
  });

  it("/network：零副作用承诺只说一遍 —— 它在页头那个问号里，不再占一块版面", async () => {
    renderPage(<NetworkPage />);
    await screen.findByTestId("network-subscriptions-table");

    // 页头那个问号 = 这一页的说明 + I2 的承诺。原来是**一整张卡片**只为了说这一句
    // （用户第 326 轮的反例），现在它跟着页头说明走：位置变了，正文一个字没少。
    const header = screen.getByTestId("page-header-help").getAttribute("aria-label") ?? "";
    expect(header).toContain(i18n.t("network.description"));
    for (const promise of [
      "只展示，不改系统网络",
      "不设置系统代理",
      "不写 DNS",
      "不改路由表",
      "不动网卡",
      "不改开机自启",
    ]) {
      expect(header, `零副作用承诺少了这一句：${promise}`).toContain(promise);
    }
    // 那张卡片**不许**再回来
    expect(screen.queryByTestId("network-zero-side-effect")).toBeNull();
    // 而且整页（正文 + 无障碍文本）里只出现一次 —— 没有第二处复述它
    const aria = Array.from(document.querySelectorAll("[aria-label]"))
      .map((element) => element.getAttribute("aria-label"))
      .join("\n");
    const everywhere = `${document.body.textContent ?? ""}\n${aria}`;
    expect(everywhere.split("不设置系统代理").length - 1).toBe(1);
  });

  it("/network/nodes：页头说明与「节点列表」标题都只说一遍", async () => {
    renderPage(<NodesPage />);
    await screen.findByTestId("network-nodes-command-section");

    const header = screen.getByTestId("page-header-help").getAttribute("aria-label") ?? "";
    expect(header).toContain(i18n.t("network.nodes.description"));
    // 零副作用承诺也在**同一个**问号里（这一页原来也有一整块专门的版面）
    expect(header).toContain(i18n.t("network.zeroSideEffect.title"));
    expect(header).toContain(i18n.t("network.zeroSideEffect.body"));
    expect(screen.queryByTestId("network-zero-side-effect")).toBeNull();
    // 「节点列表」只作为页头 H1 出现一次；第一块有自己的标题（原来它俩一字不差）
    expect(screen.getAllByRole("heading", { name: "节点列表" })).toHaveLength(1);
    expect(screen.getByRole("heading", { name: i18n.t("network.nodes.list.title") })).toBeTruthy();
    // 说明全文也只在页头那一个问号里：第一块不再挂同一句（连问号都不该有）
    expect(screen.queryByTestId("network-nodes-command-section-help")).toBeNull();
  });
});

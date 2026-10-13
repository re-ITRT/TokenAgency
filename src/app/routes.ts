/**
 * 路由表 —— **这是 F1–F4 的契约，四个包并行开发时一切以本表为准。**
 *
 * 设计规则（改本文件前先读）：
 *
 * 1. **URL ↔ 页面 的映射只有这一份**。侧边栏由本表生成（`NAV_ROUTES`），
 *    布局代码里不许再手写导航项。
 * 2. 页面组件**不在本文件 import**：用「文件名约定 + `import.meta.glob` 自动接线」
 *    （见 `pageRegistry.tsx`）。F2/F3/F4 把文件放到 `src/pages/<候选路径>.tsx`
 *    即自动生效，**不需要改任何布局代码**。
 * 3. `titleKey` 是 i18n key，类型是 `I18nKey`（`shared/i18n` 里 zh-CN 的键集合）
 *    —— 写错 key 会被 `tsc --noEmit` 直接拦下，en 翻译缺失同样会被拦下。
 * 4. `owner` 只用于标注归属，便于四个人对表；不参与运行时逻辑。
 * 5. **导航标签用用户语言，不用内部概念**：不出现 channel / credential / lane
 *    这类词。套餐（tokenplan）是用户的心智模型（[03 §0]、[05 §2.1]）。
 */

import type { SvgIconComponent } from "@mui/icons-material";
import CategoryOutlinedIcon from "@mui/icons-material/CategoryOutlined";
import DashboardOutlinedIcon from "@mui/icons-material/DashboardOutlined";
import DeviceHubOutlinedIcon from "@mui/icons-material/DeviceHubOutlined";
import DnsOutlinedIcon from "@mui/icons-material/DnsOutlined";
import KeyOutlinedIcon from "@mui/icons-material/KeyOutlined";
import LanOutlinedIcon from "@mui/icons-material/LanOutlined";
import PaidOutlinedIcon from "@mui/icons-material/PaidOutlined";
import ReceiptLongOutlinedIcon from "@mui/icons-material/ReceiptLongOutlined";
import SettingsOutlinedIcon from "@mui/icons-material/SettingsOutlined";

import type { I18nKey } from "@/shared/i18n";

/** 归属工作包。仅作标注（[12 §6]）。 */
export type RouteOwner = "F1" | "F2" | "F3" | "F4";

export type AppRoute = {
  /** 稳定 key：也被当作 `src/pages/<key>/index.tsx` 的默认候选路径 */
  key: string;
  /** URL 模板。`:id` 为动态段。 */
  path: string;
  /** i18n 标题 key（zh-CN 为真源，缺 en 会被 tsc 拦下） */
  titleKey: I18nKey;
  icon: SvgIconComponent;
  owner: RouteOwner;
  /** 是否出现在侧边栏 */
  nav: boolean;
  /**
   * 侧边栏**分组**键（`navGroup.*`）。`undefined` = 不归组。
   *
   * 为什么有分组：光靠"哪一项排在前面"表达不出结构。改动前侧边栏是
   * `概览 | 服务商 模型 网络 | 成本 日志 | 设置 | 访问密钥` —— 语义是
   * **配置(3) → 观察(2) → 系统(1) → 又回到配置(1)**：那条配置链的最后一步
   * （访问密钥 = 客户端拿它调我）被丢在「设置」**之后**，读起来像设置的子项。
   * 分组让"这几项是一类"由**结构**说出来，而不是让用户从位置上猜。
   *
   * 组内顺序 = `ROUTES` 顺序；组的先后 = 各组第一项在 `ROUTES` 里的先后
   * （**组必须是连续的**，不连续会被 `routes.test.tsx` 的闸门拦下）。
   */
  navGroup?: I18nKey;
  /**
   * 页面组件候选路径（相对 `src/pages/`，不带扩展名），按优先级从上到下。
   * 命中第一个存在的文件；都找不到就渲染 404 页（不白屏、不报错）。
   */
  pages: string[];
};

/**
 * 侧边栏分组的**显示顺序**（组名键）。
 *
 * 为什么单独列一遍而不是只用"第一项出现在哪"：分组顺序是**设计决定**，不该由
 * "谁先被写进数组"顺带决定 —— 有人插一项就可能把整组挪位。这里显式写死，
 * `navSections()` 只按它分组；漏了某一组会被 `routes.test.tsx` 拦下。
 */
export const NAV_GROUP_ORDER: readonly I18nKey[] = [
  "navGroup.setup",
  "navGroup.records",
  "navGroup.system",
];

/**
 * 路由表。**顺序 = 侧边栏顺序**，且**组必须连续**。
 *
 * ## 侧边栏的信息架构（用户第 323 轮："重新排序一下，从用户体验工程和直觉上来说"）
 *
 * ```
 * 概览                                 ← 首页：先看现在怎么样（不归组，永远是第一项）
 * ── 配置 ──
 *   服务商 / 模型目录 / 网络 / 访问密钥   ← 把中转站接起来：接谁 → 提供什么 → 怎么出网 → 谁可以调用
 * ── 记录 ──
 *   请求日志 / 成本中心                 ← 跑起来之后看过什么、花了多少
 * ── 系统 ──
 *   设置                               ← 系统设置，**永远最后**
 * ```
 *
 * **为什么这样排**（不是"按使用频率"，那条规则本身是反模式：频率会变，位置不该变）：
 *
 * 1. **按任务流排，不按频率排** —— 位置要可预测（肌肉记忆），所以依据必须是
 *    "这几项在用户的动作链条里处于哪一步"，而不是"哪个更常用"。
 * 2. **配置链要连续、以终点收尾** —— `访问密钥` 是这条链的**终点**（客户端拿它调我），
 *    它原来排在 `设置` **之后**，会被读成设置的子项；现在紧跟 `网络`。
 * 3. **观察项相邻** —— 请求日志（明细）与成本中心（聚合）是一类，不再被别类切开。
 * 4. **设置永远最后** —— 全局通行约定；它也是唯一"改了会影响整个软件"的一页。
 *
 * 归属（[12 §6]）：F2 = 看板 + 日志；F3 = 服务商/模型；F4 = 网络/成本/设置/访问密钥。
 */
export const ROUTES: readonly AppRoute[] = [
  {
    key: "dashboard",
    path: "/dashboard",
    titleKey: "nav.dashboard",
    icon: DashboardOutlinedIcon,
    owner: "F2",
    nav: true,
    // 两个候选都列出：文件夹形式（`dashboard/index.tsx`）与单文件形式（`dashboard.tsx`）都能自动接线 ——
    // 与 providers/models 等条目保持一致（`pageRegistry` 的约定是"候选路径见本表"）
    pages: ["dashboard", "dashboard/index"],
  },
  {
    key: "providers",
    path: "/providers",
    titleKey: "nav.providers",
    icon: DnsOutlinedIcon,
    owner: "F3",
    nav: true,
    navGroup: "navGroup.setup",
    pages: ["providers", "providers/index"],
  },
  {
    key: "provider-detail",
    path: "/providers/:id",
    titleKey: "nav.providerDetail",
    icon: DnsOutlinedIcon,
    owner: "F3",
    nav: false,
    pages: ["providers/detail", "provider-detail"],
  },
  {
    key: "models",
    path: "/models",
    titleKey: "nav.models",
    icon: CategoryOutlinedIcon,
    owner: "F3",
    nav: true,
    navGroup: "navGroup.setup",
    pages: ["models", "models/index"],
  },
  {
    key: "network",
    path: "/network",
    titleKey: "nav.network",
    icon: LanOutlinedIcon,
    owner: "F4",
    nav: true,
    navGroup: "navGroup.setup",
    pages: ["network", "network/index"],
  },
  {
    key: "network-nodes",
    path: "/network/nodes",
    titleKey: "nav.networkNodes",
    icon: DeviceHubOutlinedIcon,
    owner: "F4",
    nav: false,
    pages: ["network/nodes"],
  },
  {
    /**
     * **一条订阅的详情**（用户第 321 轮）：
     * "先到网络界面，再去点击一个订阅，里面就可以看到这个订阅的所有节点，可以打开或者关闭"。
     *
     * 它取代了"在 `/network/nodes` 上用下拉挑订阅"那一步：入口从订阅行直接进来，
     * 页面**不用再选订阅**（少一个控件，也少一次"选错订阅"的机会）。
     */
    key: "network-subscription",
    path: "/network/subscriptions/:id",
    titleKey: "nav.networkSubscription",
    icon: DeviceHubOutlinedIcon,
    owner: "F4",
    nav: false,
    pages: ["network/subscription"],
  },
  //（第 322 轮）`/network/assignments`（「线路分配」）**整页删除**。
  //
  // 用户裁决："线路级别的分配逻辑我们已经放弃了，取而代之的，**每个服务商统一一个节点**"。
  // 那一页存在的唯一理由是展示五级继承链（策略级 → 通道级 → 套餐级 → 服务商级 → 全局默认），
  // 而前三级已经没有字段、没有写入口、也没有解析意义了 —— 一张只剩两级的"分配表"
  // 除了让人以为还有级别可调之外没有任何用处。
  //
  // 走哪个节点现在只能在一处改：**服务商详情页**的「网络代理」下拉。
  {
    /**
     * **配置链的终点**（第 323 轮从侧边栏最底部提上来）。
     *
     * 原来它排在 `设置` **之后**，读起来像设置的子项；而它其实是配置动作的**最后一步**：
     * 「客户端拿这些密钥调用本机网关」—— 前面三项（服务商 / 模型 / 网络）都是为了让这一步成立。
     */
    key: "api-keys",
    path: "/api-keys",
    titleKey: "nav.apiKeys",
    icon: KeyOutlinedIcon,
    owner: "F4",
    nav: true,
    navGroup: "navGroup.setup",
    pages: ["apikeys", "apikeys/index"],
  },
  {
    /**
     * 观察组第一项：**明细**。
     *
     * 与「成本中心」谁在前是这一轮唯一真正五五开的一处；选日志在前，理由是
     * **排查比看账单更高频**，而且「明细 → 聚合」符合阅读方向
     *（先看清那一次请求，再看这个月合起来花了多少）。
     */
    key: "logs",
    path: "/logs",
    titleKey: "nav.logs",
    icon: ReceiptLongOutlinedIcon,
    owner: "F2",
    nav: true,
    navGroup: "navGroup.records",
    // 与 dashboard 一致：两个候选都列出（单文件形式 `logs.tsx` 与文件夹形式 `logs/index.tsx`）。
    // 已落地的是 `src/pages/logs/index.tsx`（`pageRegistry` 的约定是"候选路径见本表"）。
    pages: ["logs", "logs/index"],
  },
  {
    /** 观察组第二项：**聚合**（用量与花费的趋势和明细）。 */
    key: "cost",
    path: "/cost",
    titleKey: "nav.cost",
    icon: PaidOutlinedIcon,
    owner: "F4",
    nav: true,
    navGroup: "navGroup.records",
    pages: ["cost", "cost/index"],
  },
  {
    /**
     * **永远最后**：全局通行约定，也是唯一"改了会影响整个软件"的一页
     *（端口 / 开机自启 / 托盘 / 存储 / 系统网络）。夹在功能页中间会让人误以为它同类。
     */
    key: "settings",
    path: "/settings",
    titleKey: "nav.settings",
    icon: SettingsOutlinedIcon,
    owner: "F4",
    nav: true,
    navGroup: "navGroup.system",
    pages: ["settings", "settings/index"],
  },
  // 首启向导 `/onboarding` 已**整条移除**（[19 §2.1]：两个向导都关掉）。
  // `onboarding_*` 三条命令**保留注册**（后端能力不删），但页面不许再引用它们 ——
  // 见 `src/app/onboarding.test.tsx` 的反向断言：没有路由、也没有页面入口。
];

/** 根路径重定向目标（[10 §2.2] 的看板）。 */
export const HOME_PATH = "/dashboard";

/** 侧边栏项（顺序即 `ROUTES` 顺序）。 */
export const NAV_ROUTES: readonly AppRoute[] = ROUTES.filter((r) => r.nav);

/** 侧边栏的一段：可选的组名 + 这一段里的导航项。 */
export type NavSection = {
  /** 组名键；`undefined` = **不归组**的那一项（首页）。 */
  groupKey?: I18nKey;
  routes: readonly AppRoute[];
};

/**
 * 把 `NAV_ROUTES` 切成侧边栏要画的那几段。
 *
 * 规则（都由 `routes.test.tsx` 的闸门钉住）：
 *
 * 1. **不归组的项自成一段，且排在最前**（现在只有「概览」这一项 —— 它是首页，不该
 *    被塞进任何组；组名与项名都是"概览"还会造成同屏重复）；
 * 2. 其余按 [`NAV_GROUP_ORDER`] 的**声明顺序**分组，组内保持 `ROUTES` 顺序；
 * 3. **同一组的项必须连续** —— 不连续时本函数**返回 `null`**，让闸门当场红
 *    （而不是画出一个"同样的组名出现两次"的侧边栏，那正是分组要消灭的东西）。
 */
export function navSections(): readonly NavSection[] | null {
  const ungrouped = NAV_ROUTES.filter((r) => r.navGroup === undefined);
  const sections: NavSection[] = [];
  if (ungrouped.length > 0) sections.push({ routes: ungrouped });

  for (const groupKey of NAV_GROUP_ORDER) {
    const routes = NAV_ROUTES.filter((r) => r.navGroup === groupKey);
    if (routes.length === 0) continue;
    // 连续性：这一组在 NAV_ROUTES 里的下标必须是一段连续的区间
    const indices = routes.map((r) => NAV_ROUTES.indexOf(r));
    const contiguous = indices.every((n, i) => i === 0 || n === (indices[i - 1] ?? -1) + 1);
    if (!contiguous) return null;
    sections.push({ groupKey, routes });
  }

  // 有项带着**不在** `NAV_GROUP_ORDER` 里的组名 = 有人加了组却忘了登记顺序
  const known = new Set(NAV_GROUP_ORDER);
  const unknown = NAV_ROUTES.filter((r) => r.navGroup !== undefined && !known.has(r.navGroup));
  if (unknown.length > 0) return null;

  return sections;
}

/** 按 key 取路由（找不到返回 undefined，调用方自行兜底）。 */
export function routeByKey(key: string): AppRoute | undefined {
  return ROUTES.find((r) => r.key === key);
}

/**
 * 当前 URL 命中了哪个路由项（按 path 模板匹配，供状态条/面包屑使用）。
 * 动态段（`:id`）只做简单前缀匹配，够用且不引额外依赖。
 */
export function matchRoute(pathname: string): AppRoute | undefined {
  const exact = ROUTES.find((r) => r.path === pathname);
  if (exact) return exact;
  return ROUTES.filter((r) => r.path.includes("/:"))
    .filter((r) => {
      const prefix = r.path.slice(0, r.path.indexOf("/:"));
      return pathname.startsWith(`${prefix}/`);
    })
    // 最长前缀优先（/network/nodes 比 /network 更具体）
    .sort((a, b) => b.path.length - a.path.length)[0];
}

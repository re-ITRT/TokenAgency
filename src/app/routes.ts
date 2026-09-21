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
import AltRouteOutlinedIcon from "@mui/icons-material/AltRouteOutlined";
import CallSplitOutlinedIcon from "@mui/icons-material/CallSplitOutlined";
import CategoryOutlinedIcon from "@mui/icons-material/CategoryOutlined";
import DashboardOutlinedIcon from "@mui/icons-material/DashboardOutlined";
import DeviceHubOutlinedIcon from "@mui/icons-material/DeviceHubOutlined";
import DnsOutlinedIcon from "@mui/icons-material/DnsOutlined";
import LanOutlinedIcon from "@mui/icons-material/LanOutlined";
import PaidOutlinedIcon from "@mui/icons-material/PaidOutlined";
import ReceiptLongOutlinedIcon from "@mui/icons-material/ReceiptLongOutlined";
import RocketLaunchOutlinedIcon from "@mui/icons-material/RocketLaunchOutlined";
import RuleOutlinedIcon from "@mui/icons-material/RuleOutlined";
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
   * 页面组件候选路径（相对 `src/pages/`，不带扩展名），按优先级从上到下。
   * 命中第一个存在的文件；都找不到就渲染 `PagePlaceholder`（不白屏、不报错）。
   */
  pages: string[];
  /** 契约出处，方便四个包对照文档 */
  docRef: string;
};

/**
 * 路由表。**顺序 = 侧边栏顺序**（按使用频率，见 [05 §2]）。
 *
 * 归属（[12 §6]）：F2 = 看板 + 日志；F3 = 服务商/模型/通道/策略；F4 = 网络/成本/设置/向导。
 */
export const ROUTES: readonly AppRoute[] = [
  {
    key: "dashboard",
    path: "/dashboard",
    titleKey: "nav.dashboard",
    icon: DashboardOutlinedIcon,
    owner: "F2",
    nav: true,
    pages: ["dashboard"],
    docRef: "07 §1 首页看板",
  },
  {
    key: "providers",
    path: "/providers",
    titleKey: "nav.providers",
    icon: DnsOutlinedIcon,
    owner: "F3",
    nav: true,
    pages: ["providers", "providers/index"],
    docRef: "07 §3 渠道设置",
  },
  {
    key: "provider-detail",
    path: "/providers/:id",
    titleKey: "nav.providerDetail",
    icon: DnsOutlinedIcon,
    owner: "F3",
    nav: false,
    pages: ["providers/detail", "provider-detail"],
    docRef: "07 §3 渠道设置（详情 + 连通性测试 / echo）",
  },
  {
    key: "models",
    path: "/models",
    titleKey: "nav.models",
    icon: CategoryOutlinedIcon,
    owner: "F3",
    nav: true,
    pages: ["models"],
    docRef: "07 §4 模型目录",
  },
  {
    // URL 是 /channels（契约固定），但**导航标签面向用户**：这页的默认视图是
    // 「套餐链」——哪个模型在用哪些套餐/线路、还剩多少、排第几（[05 §2.1]）。
    // 所以标签用「套餐与线路」，避免让用户以为要先配"通道"这个内部概念。
    key: "channels",
    path: "/channels",
    titleKey: "nav.plansAndRoutes",
    icon: AltRouteOutlinedIcon,
    owner: "F3",
    nav: true,
    pages: ["channels"],
    docRef: "07 §5 / 05 §2.1 套餐链（默认视图）+ 矩阵（批量配置视图）",
  },
  {
    key: "policies",
    path: "/policies",
    titleKey: "nav.policies",
    icon: RuleOutlinedIcon,
    owner: "F3",
    nav: true,
    pages: ["policies"],
    docRef: "07 §5 路由策略（规则表 + 条件构造器 + 模拟器）",
  },
  {
    key: "network",
    path: "/network",
    titleKey: "nav.network",
    icon: LanOutlinedIcon,
    owner: "F4",
    nav: true,
    pages: ["network"],
    docRef: "12 §6 F4 订阅管理 / 节点列表 / 分配 Tab",
  },
  {
    key: "network-nodes",
    path: "/network/nodes",
    titleKey: "nav.networkNodes",
    icon: DeviceHubOutlinedIcon,
    owner: "F4",
    nav: false,
    pages: ["network/nodes"],
    docRef: "04 §7 节点列表与测速",
  },
  {
    key: "network-assignments",
    path: "/network/assignments",
    titleKey: "nav.networkAssignments",
    icon: CallSplitOutlinedIcon,
    owner: "F4",
    nav: false,
    pages: ["network/assignments"],
    docRef: "04 §8 线路分配（反向视图：谁走了这条线）",
  },
  {
    key: "cost",
    path: "/cost",
    titleKey: "nav.cost",
    icon: PaidOutlinedIcon,
    owner: "F4",
    nav: true,
    pages: ["cost"],
    docRef: "07 §6 成本中心",
  },
  {
    key: "logs",
    path: "/logs",
    titleKey: "nav.logs",
    icon: ReceiptLongOutlinedIcon,
    owner: "F2",
    nav: true,
    pages: ["logs"],
    docRef: "07 §2 请求日志",
  },
  {
    key: "settings",
    path: "/settings",
    titleKey: "nav.settings",
    icon: SettingsOutlinedIcon,
    owner: "F4",
    nav: true,
    pages: ["settings"],
    docRef: "07 §7 设置（含开机自启动，09 §1.2 E1–E5）",
  },
  {
    key: "onboarding",
    path: "/onboarding",
    titleKey: "nav.onboarding",
    icon: RocketLaunchOutlinedIcon,
    owner: "F4",
    nav: false,
    pages: ["onboarding"],
    docRef: "12 §6 F4 首启向导",
  },
];

/** 根路径重定向目标（[10 §2.2] 的看板）。 */
export const HOME_PATH = "/dashboard";

/** 侧边栏项（顺序即 `ROUTES` 顺序）。 */
export const NAV_ROUTES: readonly AppRoute[] = ROUTES.filter((r) => r.nav);

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

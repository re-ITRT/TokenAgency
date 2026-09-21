/**
 * Query key 约定 + 事件 → 失效映射。**这是 F2/F3/F4 不必再猜的那一处约定**。
 *
 * 规则：
 * 1. 所有 key 都从 `qk` 取（不要在页面里手写 `["providers"]` 字面量），
 *    这样事件桥（`useEventBridge`）的一次 invalidate 能覆盖所有页面。
 * 2. key 的第一段是**资源前缀**，前缀失效 = 该资源的所有视图都失效
 *    （TanStack Query 的 `invalidateQueries({ queryKey: ["providers"] })` 会匹配
 *    `["providers", id]` 等所有子 key）。
 * 3. 事件到达时**只 invalidate，不 setQueryData**：避免把服务端真相拼错；
 *    高频流式数据（request:new / usage:tick）由页面自己用环形缓冲处理。
 */

import type { EventNameValue } from "@/shared/bindings";

/** 全部 query key。带参数的是函数（参数进 key，天然分离不同筛选）。 */
export const qk = {
  // app / kernel
  appStatus: ["app", "status"] as const,
  autostart: ["app", "autostart"] as const,
  clipboard: ["app", "clipboard"] as const,
  kernelStatus: ["kernel", "status"] as const,
  kernelGeneratedConfig: ["kernel", "generated-config"] as const,

  // config
  config: ["config"] as const,
  configHistory: ["config", "history"] as const,

  // 领域资源（前缀失效用）
  providers: ["providers"] as const,
  provider: (id: string) => ["providers", id] as const,
  credentials: ["credentials"] as const,
  models: ["models"] as const,
  channels: ["channels"] as const,
  channelMatrix: ["channels", "matrix"] as const,
  groups: ["groups"] as const,
  policies: ["policies"] as const,
  lanes: ["lanes"] as const,
  laneAssignments: ["lanes", "assignments"] as const,
  lanePickOptions: ["lanes", "pick-options"] as const,
  subscriptions: ["subscriptions"] as const,

  // 用量 / 成本 / 日志
  dashboard: (range: string) => ["dashboard", range] as const,
  dashboardPrefix: ["dashboard"] as const,
  usage: (fingerprint: string) => ["usage", fingerprint] as const,
  usageTokenKinds: ["usage", "token-kinds"] as const,
  pricing: ["pricing"] as const,
  unpricedModels: ["pricing", "unpriced"] as const,
  logs: (fingerprint: string) => ["logs", fingerprint] as const,
  logsPrefix: ["logs"] as const,
  logStats: ["logs", "stats"] as const,
  trace: (requestId: string) => ["logs", "trace", requestId] as const,

  // 托盘
  trayState: ["tray"] as const,
} as const;

/** 所有资源前缀（供"全部失效"用）。 */
export const ALL_RESOURCE_PREFIXES = [
  qk.appStatus.slice(0, 1),
  qk.appStatus,
  qk.autostart,
  qk.kernelStatus,
  qk.config,
  qk.providers,
  qk.credentials,
  qk.models,
  qk.channels,
  qk.groups,
  qk.policies,
  qk.lanes,
  qk.subscriptions,
  qk.dashboardPrefix,
  qk.pricing,
  ["usage"],
  qk.logsPrefix,
  qk.trayState,
] as const;

/**
 * ★ **事件到达时失效哪些 key** —— 页面不要自己订阅后再猜。
 *
 * 空数组表示"只进 store / 不改 query"（如 toast、tray:refresh）。
 */
export const EVENT_INVALIDATIONS: Record<EventNameValue, readonly (readonly unknown[])[]> = {
  // 新请求：日志列表 + 看板的实时流/数字卡都变了
  "request:new": [qk.logsPrefix, qk.dashboardPrefix, qk.logStats],
  // 用量 tick：看板数字 + 额度
  "usage:tick": [qk.dashboardPrefix, ["usage"]],
  // 通道状态（冷却/耗尽）：矩阵与看板的通道健康
  "channel:state": [qk.channels, qk.dashboardPrefix],
  // 线路健康：线路列表 + 看板 + 分配视图
  "lane:health": [qk.lanes, qk.dashboardPrefix, qk.laneAssignments],
  "kernel:status": [qk.kernelStatus, qk.appStatus],
  // 配置变了：几乎所有领域资源都可能变
  "config:changed": [
    qk.config,
    qk.appStatus,
    qk.providers,
    qk.credentials,
    qk.models,
    qk.channels,
    qk.groups,
    qk.policies,
    qk.lanes,
    qk.subscriptions,
    qk.pricing,
  ],
  "subscription:updated": [qk.subscriptions, qk.lanes],
  // 日志页开着时才有意义
  "log:append": [qk.logsPrefix, qk.logStats],
  // 这两个只进 store（toast 队列 / 托盘状态）
  toast: [],
  "tray:refresh": [qk.trayState],
};

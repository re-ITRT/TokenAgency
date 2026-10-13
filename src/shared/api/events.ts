import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { EventNameValue, EventPayload } from "@/shared/bindings";

import { isTauri } from "./client";

/**
 * 事件名常量。**必须与  以及 Rust 侧
 * `ta_gateway::events::TaEvent::name()` 一致**（`satisfies` 会在写错时让 tsc 报错）。
 *
 * 载荷类型在 `@/shared/bindings`（与 Rust `TaEvent` 变体字段一一对应）——
 * 本文件不再自己定义第二份，避免两处漂移。
 */
export const EventName = {
  RequestNew: "request:new",
  UsageTick: "usage:tick",
  ChannelState: "channel:state",
  LaneHealth: "lane:health",
  KernelStatus: "kernel:status",
  ConfigChanged: "config:changed",
  SubscriptionUpdated: "subscription:updated",
  LogAppend: "log:append",
  Toast: "toast",
  TrayRefresh: "tray:refresh",
} as const satisfies Record<string, EventNameValue>;

export type { EventNameValue, EventPayload };

/**
 * 订阅一个后端事件。返回取消订阅函数。
 *
 * 注意 `request:new` 与 `log:append` 的后端已经是**批量**推送（200ms 合并，最多 50 条），
 * 前端不要对每条请求做重渲染 —— 用环形缓冲 + 一次性 setState。
 *
 * **页面一般不需要自己调**：`useEventBridge()`（`queries.ts`）已统一订阅并做 query 失效。
 */
export async function subscribe<K extends EventNameValue>(
  name: K,
  cb: (payload: EventPayload[K]) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => {};
  return listen<EventPayload[K]>(name, (e) => cb(e.payload));
}

/** 一次性订阅多个事件，返回一个总的取消函数。 */
export async function subscribeAll(
  names: EventNameValue[],
  cb: (name: EventNameValue, payload: unknown) => void,
): Promise<UnlistenFn> {
  const offs = await Promise.all(names.map((n) => subscribe(n, (p) => cb(n, p))));
  return () => offs.forEach((f) => f());
}

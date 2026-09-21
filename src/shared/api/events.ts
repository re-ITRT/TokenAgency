import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauri } from "./client";

/**
 * 事件名。**必须与 `docs/10-interface-spec.md` §2.3 一致**
 * （Rust 侧 `ta_gateway::events::TaEvent::name()` 返回同样的字符串）。
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
} as const;

export type EventNameValue = (typeof EventName)[keyof typeof EventName];

/** 各事件载荷。与 Rust 侧 `TaEvent` 变体的字段一一对应。 */
export type EventPayload = {
  "request:new": { items: RequestSummary[] };
  "usage:tick": { today_tokens: number; today_cost: Money };
  "channel:state": { channel_id: string; state: string; until_ms?: number };
  "lane:health": { lane_id: string; node?: string; latency_ms?: number; ok: boolean };
  "kernel:status": { state: KernelState; version?: string };
  "config:changed": { source: "db" | "file"; applied: boolean };
  "subscription:updated": { id: string; ok: boolean; node_count: number; error?: string };
  "log:append": { items: unknown[] };
  toast: { level: "info" | "warn" | "error"; title: string; body: string; dedupe_key: string };
  "tray:refresh": { payload: unknown };
};

// 这几个类型在 T1 生成 bindings.ts 之后应改为从那里 import，避免两份定义漂移。
export type Money = { amount: number; currency: string };
export type KernelState = "stopped" | "starting" | "ready" | "restarting" | "failed";
export type RequestSummary = {
  request_id: string;
  ts_ms: number;
  model: string;
  upstream_model: string;
  channel_id: string;
  lane_id?: string;
  node?: string;
  status: number;
  outcome: "ok" | "retried" | "failed";
  latency_ms: number;
  ttfb_ms?: number;
  total_tokens: number;
  cost: Money;
  attempts: number;
  error_type?: string;
};

/**
 * 订阅一个后端事件。返回取消订阅函数。
 *
 * 注意 `request:new` 与 `log:append` 的后端已经是**批量**推送（200ms 合并），
 * 前端不要对每条请求做重渲染 —— 用环形缓冲 + 一次性 setState。
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

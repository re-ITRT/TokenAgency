/**
 * 全局应用状态（Zustand）。
 *
 * **只放跨页面状态**：App/内核运行态、实验模式角标、toast 队列。
 * 页面局部状态（表单草稿、选中行、筛选条件）**不要塞进 store** —— 用 `useState`。
 *
 * 数据实体（provider/credential/…）一律走 TanStack Query，不进 store：
 * 服务器状态只有一个真相来源，两处缓存必然漂移。
 */

import { create } from "zustand";

import type { AppStatus, EventPayload, KernelStatus } from "@/shared/bindings";

/** 一条 toast。`dedupeKey` 相同的事件 10 分钟内只弹一次。 */
export type ToastItem = {
  id: string;
  level: "info" | "warn" | "error";
  title: string;
  body: string;
  dedupeKey: string;
  createdAt: number;
};

/** 同 key 去重窗口（：同事件 10 分钟内只弹一次）。 */
export const TOAST_DEDUPE_MS = 10 * 60 * 1000;

type AppState = {
  /** `app_status` 的最新结果（事件到达时也会更新）。 */
  status: AppStatus | null;
  /** `kernel:status` 事件的最新结果。 */
  kernel: KernelStatus | null;
  /**
   * 实验模式是否开启（TUN / 系统代理任一为真）。
   * 托盘与状态条要显示角标 —— 用户必须知道自己处于"会改系统网络"的模式。
   */
  experimental: boolean;
  toasts: ToastItem[];

  setStatus: (status: AppStatus | null) => void;
  setKernel: (kernel: KernelStatus | null) => void;
  /**
   * `kernel:status` 事件只带 `{state, version}`，
   * 这里把它**合并**进已有的完整状态（`restarts` 等字段保留），
   * 避免事件一到就把 pid/restarts 抹掉。
   */
  applyKernelEvent: (e: EventPayload["kernel:status"]) => void;
  setExperimental: (on: boolean) => void;
  pushToast: (t: Omit<ToastItem, "id" | "createdAt">) => void;
  dismissToast: (id: string) => void;
  clearToasts: () => void;
};

let toastSeq = 0;

export const useAppStore = create<AppState>((set, get) => ({
  status: null,
  kernel: null,
  experimental: false,
  toasts: [],

  setStatus: (status) => set({ status }),
  setKernel: (kernel) => set({ kernel }),
  applyKernelEvent: (e) =>
    set((s) => ({
      kernel: {
        state: e.state,
        version: e.version ?? s.kernel?.version ?? null,
        pid: s.kernel?.pid ?? null,
        restarts: s.kernel?.restarts ?? 0,
        last_error: s.kernel?.last_error ?? null,
      },
    })),
  setExperimental: (on) => set({ experimental: on }),

  pushToast: (t) => {
    const now = Date.now();
    const seen = get().toasts.some(
      (x) => x.dedupeKey === t.dedupeKey && now - x.createdAt < TOAST_DEDUPE_MS,
    );
    if (seen) return;
    toastSeq += 1;
    set((s) => ({
      toasts: [...s.toasts, { ...t, id: `t${toastSeq}`, createdAt: now }].slice(-5),
    }));
  },

  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),
  clearToasts: () => set({ toasts: [] }),
}));

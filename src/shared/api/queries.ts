/**
 * TanStack Query 封装 + **事件 → query 失效**的唯一约定。
 *
 * 页面用法（F2/F3/F4 照抄即可）：
 *
 * ```tsx
 * const { data, isLoading, error } = useCommandQuery("provider_list", undefined, qk.providers);
 * const upsert = useCommandMutation("provider_upsert");
 * upsert.mutate({ provider }, { onSuccess: () => qc.invalidateQueries({ queryKey: qk.providers }) });
 * ```
 *
 * 约定：
 * 1. **query key 从 `qk` 取**，并且把该 query 的参数放进 key（否则筛选切了不刷新）。
 * 2. 错误一律是 `TokenAgencyError`：用 `errorMessage` / `errorCode` 显示，
 *    按 `code`（TA-xxxx，见 [11 §5]）分支，不解析文案。
 * 3. 事件到达时**只 invalidate**（见 `keys.ts` 的 `EVENT_INVALIDATIONS`），
 *    不要在这里 setQueryData 拼数据。
 */

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryKey,
  type UseMutationOptions,
  type UseQueryOptions,
} from "@tanstack/react-query";
import { useEffect } from "react";

import type {
  CommandArgs,
  CommandMap,
  CommandName,
  EventNameValue,
  EventPayload,
} from "@/shared/bindings";
import { useAppStore } from "@/stores/app";

import { TokenAgencyError } from "./client";
import { invokeCommand } from "./commands";
import { EventName, subscribeAll } from "./events";
import { EVENT_INVALIDATIONS, qk } from "./keys";
// ================================================================ 错误

/** 面向用户的一句话错误信息（含错误码，便于对着 [11 §5] 查）。 */
export function errorMessage(err: unknown): string {
  if (err instanceof TokenAgencyError) return `[${err.code}] ${err.message}`;
  if (err instanceof Error) return err.message;
  return String(err);
}

/** 错误码（`TA-xxxx`）；非 IPC 错误返回 undefined。 */
export function errorCode(err: unknown): string | undefined {
  return err instanceof TokenAgencyError ? err.code : undefined;
}

/** 是否是"用户自己能修"的配置类错误（TA-5001/5002/5003）。 */
export function isFixableError(err: unknown): boolean {
  return err instanceof TokenAgencyError && err.isFixable;
}

// ================================================================ query / mutation

/** 类型化 query：命令名 + 入参 + query key，返回类型自动推导。 */
export function useCommandQuery<K extends CommandName>(
  cmd: K,
  args: CommandArgs<K>,
  queryKey: QueryKey,
  options?: Omit<UseQueryOptions<CommandMap[K]["result"], TokenAgencyError>, "queryKey" | "queryFn">,
) {
  return useQuery<CommandMap[K]["result"], TokenAgencyError>({
    queryKey,
    queryFn: () => invokeCommand(cmd, args),
    ...options,
  });
}

/** 类型化 mutation：`mutate(args)` 的入参就是命令的入参。 */
export function useCommandMutation<K extends CommandName>(
  cmd: K,
  options?: Omit<
    UseMutationOptions<CommandMap[K]["result"], TokenAgencyError, CommandArgs<K>>,
    "mutationFn"
  >,
) {
  return useMutation<CommandMap[K]["result"], TokenAgencyError, CommandArgs<K>>({
    mutationFn: (args) => invokeCommand(cmd, args),
    ...options,
  });
}

/**
 * `app_status` 的唯一取数入口：轮询 + 写进全局 store（状态条、设置页共用）。
 * 事件（`kernel:status` / `config:changed`）到达时会 invalidate，无需自己订。
 */
export function useAppStatus(options?: { refetchInterval?: number }) {
  const setStatus = useAppStore((s) => s.setStatus);
  const q = useCommandQuery("app_status", undefined, qk.appStatus, {
    refetchInterval: options?.refetchInterval ?? 5_000,
  });

  useEffect(() => {
    if (q.data !== undefined) setStatus(q.data);
  }, [q.data, setStatus]);

  return q;
}

// ================================================================ 事件桥（唯一订阅点）

/**
 * 订阅全部后端事件 → 失效对应 query（约定见 `keys.ts`），
 * 并把 toast / 内核状态写进全局 store。
 *
 * **在 `AppShell` 里调用一次**即可，页面不要各自订阅（否则 10 个页面 = 10 份监听）。
 * 浏览器开发环境（无 Tauri）下 `subscribeAll` 是空操作，不会报错。
 */
export function useEventBridge(): void {
  const queryClient = useQueryClient();
  const pushToast = useAppStore((s) => s.pushToast);
  const applyKernelEvent = useAppStore((s) => s.applyKernelEvent);

  useEffect(() => {
    const names = Object.values(EventName) as EventNameValue[];
    let unsubscribe: (() => void) | undefined;
    let disposed = false;

    void subscribeAll(names, (name, payload) => {
      // 1) 失效（前缀匹配，一次覆盖所有子 key）
      for (const key of EVENT_INVALIDATIONS[name]) {
        void queryClient.invalidateQueries({ queryKey: key });
      }
      // 2) 少数事件额外写 store
      switch (name) {
        case "toast": {
          const p = payload as EventPayload["toast"];
          pushToast({
            level: p.level,
            title: p.title,
            body: p.body,
            dedupeKey: p.dedupe_key,
          });
          break;
        }
        case "kernel:status": {
          applyKernelEvent(payload as EventPayload["kernel:status"]);
          break;
        }
        default:
          break;
      }
    }).then((off) => {
      if (disposed) off();
      else unsubscribe = off;
    });

    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [queryClient, pushToast, applyKernelEvent]);
}

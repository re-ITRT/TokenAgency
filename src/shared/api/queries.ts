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
 *    按 `code`（TA-xxxx）分支，不解析文案。
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

/**
 * `TA-5001` 的 `details` 里带出来的**逐字段**错误
 * （Rust 侧 `ta_core::error::FieldError` = `{ path, code, message }`）。
 */
export type FieldErrorDetail = { path: string; code: string; message: string };

/**
 * 从错误的 `details` 里取出字段错误清单（形状不对就返回空数组 —— 不猜、不抛）。
 *
 * 为什么需要它：`IpcError::from_field_errors` 把**具体字段路径**放在 `details`，
 * 而 `message` 只有一句「配置校验失败：N 处问题」。只显示 `message` 的话，
 * 用户看到的就是一个**没有任何可执行信息**的报错
 * —— 这一条是第 320 轮用户真机报上来的（删订阅被拦，只看到"1 处问题"）。
 */
export function fieldErrorsOf(err: unknown): FieldErrorDetail[] {
  if (!(err instanceof TokenAgencyError)) return [];
  const raw = err.details;
  if (!Array.isArray(raw)) return [];
  const out: FieldErrorDetail[] = [];
  for (const item of raw) {
    if (typeof item !== "object" || item === null) continue;
    const o = item as Record<string, unknown>;
    if (typeof o.path !== "string" || typeof o.message !== "string") continue;
    out.push({ path: o.path, code: typeof o.code === "string" ? o.code : "", message: o.message });
  }
  return out;
}

/**
 * 面向用户的一句话错误信息（含错误码，便于对着 查）。
 *
 * **`TA-5001` 会把逐字段路径一并带上**（：被拒绝时要说清
 * 是**哪一处**、以及怎么出去）。原来这里只返回 `message`，而它的内容只有
 * 「配置校验失败：1 处问题」—— 结构化的 `details` 被整个丢掉，
 * 于是用户唯一的线索是一个 TA 码（第 320 轮真机报的就是这个）。
 *
 * 其它错误码原样返回 `message`（它们本来就是人话）。
 */
export function errorMessage(err: unknown): string {
  if (err instanceof TokenAgencyError) {
    const head = `[${err.code}] ${err.message}`;
    const fields = fieldErrorsOf(err);
    if (fields.length === 0) return head;
    // 逐条「路径：说明」；多条用 `；` 分隔（与仓里其它多处并列的写法一致）
    const detail = fields.map((f) => `${f.path}：${f.message}`).join("；");
    return `${head} · ${detail}`;
  }
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

/**
 * 类型化 mutation：`mutate(args)` 的入参就是命令的入参。
 *
 * **每个写命令成功后都必须让 `qk.config` 失效**（集中在这一处，不靠每个调用点自觉）。
 *
 * 为什么这是**必须**的（这不是"顺手刷新一下"，是一条数据安全规则）：
 * `config_get` 返回的是**整份** `ConfigDoc`，而 `config_apply` 是**全量覆盖**
 * （Rust `config_store.rs::write_rows` 先 `DELETE FROM config` 再插入，
 * `src-tauri/src/commands/config.rs` 的 `config_apply` 直接 `save` + `set_doc`）。
 * 于是"前端手里那份 `qk.config` 旧不旧"直接决定**用户的东西会不会被删掉**：
 * 在 `/network` 建好一条线路 → 3 秒内打开设置页（`staleTime` 5s → 不重取）→
 * 点「校验并应用」→ 提交的是**建线路之前**的那份 doc → **线路被静默抹掉**。
 * 这个缺陷被真机 E2E 抓到过（`config_snapshot` 里 label=`lane_upsert` 的 doc 有 1 条 lane、
 * label=`apply` 的 doc 有 0 条），修复方式就是这一行。
 *
 * 名字由设计兜底的那条通道（`config:changed` → `EVENT_INVALIDATIONS["config:changed"]`，
 * 见 `keys.ts`）目前**没有生产端生产者**（`TaEvent::ConfigChanged` 只在测试里被构造），
 * 所以这里必须自己失效；将来后端真的发出该事件，这一行仍然无害（重复失效是幂等的）。
 *
 * 代价：非配置类写命令（如 `log_clear`）也会多一次 `config_get`。那是**内存快照的克隆**，
 * 且只在有观察者时才会重取 —— 用这点开销换"绝不静默删用户数据"，值得。
 */
export function useCommandMutation<K extends CommandName>(
  cmd: K,
  options?: Omit<
    UseMutationOptions<CommandMap[K]["result"], TokenAgencyError, CommandArgs<K>>,
    "mutationFn"
  >,
) {
  const queryClient = useQueryClient();
  const { onSuccess, ...rest } = options ?? {};
  return useMutation<CommandMap[K]["result"], TokenAgencyError, CommandArgs<K>>({
    mutationFn: (args) => invokeCommand(cmd, args),
    ...rest,
    onSuccess: (data, variables, context, mutation) => {
      void queryClient.invalidateQueries({ queryKey: qk.config });
      onSuccess?.(data, variables, context, mutation);
    },
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

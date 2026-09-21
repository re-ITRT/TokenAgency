/**
 * `@/shared/api` —— **前端唯一的 IPC / 事件 / 数据入口**。
 *
 * 页面禁止直接 `import { invoke } from "@tauri-apps/api/core"`：
 * 所有调用都要走这里，才能保证错误形状统一（`TokenAgencyError`）、
 * 浏览器开发有 mock、命令名与 `bindings.ts` 对齐。
 *
 * 典型用法：
 * ```tsx
 * import { api, qk, useCommandQuery, useCommandMutation } from "@/shared/api";
 *
 * const providers = useCommandQuery("provider_list", undefined, qk.providers);
 * const test = useCommandMutation("provider_test");
 * test.mutate({ id: "p-bytedance" });
 * ```
 */

export * from "./client";
export * from "./commands";
export * from "./events";
export * from "./keys";
export * from "./mock";
export * from "./queries";

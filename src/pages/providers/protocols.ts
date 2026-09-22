/**
 * `Protocol` 的**运行时清单** —— 下拉框需要运行时的数组，而契约里的 `Protocol` 只是编译期联合类型。
 *
 * 单一真相源是 Rust（`crates/ta-core/src/types.rs` 的 `pub enum Protocol`，
 * `#[serde(rename_all = "snake_case")]`）；本数组只是它的**镜像**，漂了就机械报警：
 * `src/app/protocolList.guard.test.ts` 按 **serde 线上表示**（不是变体名）双向比对 ——
 * 少一个、多一个、大小写写错都会红。
 *
 * 为什么不为此新开一条契约命令：清单是"类型系统的运行时投影"，加命令等于把编译期事实搬到 IPC，
 * 多一条真相源；守卫已经能把漂移变红。
 */

import type { Protocol } from "@/shared/bindings";

export const PROTOCOLS = [
  "openai",
  "openai_responses",
  "anthropic",
  "gemini",
  "custom",
] as const satisfies readonly Protocol[];

export type ProtocolValue = (typeof PROTOCOLS)[number];

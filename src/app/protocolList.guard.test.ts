// @vitest-environment node
/**
 * 守卫：**`PROTOCOLS` 运行时清单必须与 Rust `enum Protocol` 的线上表示双向一致**。
 *
 * 为什么需要它：`Protocol` 在契约里只是编译期联合类型，下拉框却需要运行时数组。
 * 内联一份数组（而不是新开契约命令）能避免多一条真相源，但**内联就会漂**：
 * Rust 加一个协议、数组没跟 → 用户在新协议上根本选不到；数组写错大小写 → 请求带着
 * 上游不认识的枚举值出门（`tsc` 只挡得住"数组里有类型外的值"，挡不住"类型里有数组外的值"）。
 *
 * 比对基准是 **serde 的线上名字**，不是变体名：Rust 侧是 `#[serde(rename_all = "snake_case")]`，
 * 所以 `OpenaiResponses` 出门是 `openai_responses`。机械地比变体名会出现
 * "守卫通过、而线上名字其实不同"的静默错位（L2 同族）。
 *
 * 真相源位置：`crates/ta-core/src/types.rs`（不在 `domain/mod.rs` —— 按代码实际位置解析）。
 */

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { PROTOCOLS } from "@/pages/providers/protocols";

const RUST = "crates/ta-core/src/types.rs";

/** `OpenaiResponses` → `openai_responses`（与 serde 的 snake_case 规则一致）。 */
export function toSnakeCase(variant: string): string {
  return variant
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}

export type EnumInfo = { renameAll: string | null; wireNames: string[] };

/** 解析 Rust 枚举的**线上**取值：`rename_all` 生效，逐变体 `#[serde(rename = "…")]` 覆盖它。 */
export function rustEnumWireNames(text: string, name: string): EnumInfo {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.trimStart() === `pub enum ${name} {`);
  if (start < 0) return { renameAll: null, wireNames: [] };

  let renameAll: string | null = null;
  for (let i = Math.max(0, start - 3); i < start; i++) {
    const m = lines[i]?.match(/rename_all\s*=\s*"([^"]+)"/);
    if (m?.[1] !== undefined) renameAll = m[1];
  }

  const wireNames: string[] = [];
  let explicitRename: string | null = null;
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("}")) break;
    const rn = line.match(/rename\s*=\s*"([^"]+)"/);
    if (rn?.[1] !== undefined) {
      explicitRename = rn[1];
      continue;
    }
    const v = line.match(/^\s+([A-Z][A-Za-z0-9]*)\s*(?:,|\{|\()?/);
    if (v?.[1] === undefined) continue;
    if (v[1] === "impl") continue;
    const variant = v[1];
    let wire = variant;
    if (explicitRename !== null) wire = explicitRename;
    else if (renameAll === "snake_case") wire = toSnakeCase(variant);
    else if (renameAll === "kebab-case") wire = toSnakeCase(variant).replace(/_/g, "-");
    wireNames.push(wire);
    explicitRename = null;
  }
  return { renameAll, wireNames };
}

const sorted = (xs: readonly string[]): string[] => [...xs].sort();

describe("[task-22] PROTOCOLS 运行时清单 ↔ Rust enum Protocol（双向）", () => {
  it("解析器有牙（自证：snake_case 与逐变体 rename 都认）", () => {
    const info = rustEnumWireNames(
      [
        '#[serde(rename_all = "snake_case")]',
        "pub enum T {",
        "    Alpha,",
        "    AlphaBeta,",
        '    #[serde(rename = "custom-x")]',
        "    CustomThing,",
        "}",
      ].join("\n"),
      "T",
    );
    expect(info.renameAll).toBe("snake_case");
    expect(info.wireNames).toEqual(["alpha", "alpha_beta", "custom-x"]);
  });

  it("清单与线上取值集合完全相等（少一个/多一个都红）", () => {
    const info = rustEnumWireNames(readFileSync(RUST, "utf8"), "Protocol");
    expect(info.wireNames.length, `${RUST} 没解析出 Protocol 变体`).toBeGreaterThan(0);

    expect(
      sorted(PROTOCOLS),
      `Protocol 清单与 Rust 不一致（Rust 线上 = ${sorted(info.wireNames).join(", ")}；` +
        `PROTOCOLS = ${sorted(PROTOCOLS).join(", ")}）—— 少一个用户就选不到，多一个会发出上游不认的值`,
    ).toEqual(sorted(info.wireNames));
  });
});

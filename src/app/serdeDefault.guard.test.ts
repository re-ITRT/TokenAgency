// @vitest-environment node
/**
 * 守卫：**`ProviderInput` 的可选字段集必须与 Rust 侧 `#[serde(default)]` 的字段集一致**。
 *
 * 为什么必须机械检查（主管原话：这类坑踩过两次，第三次要用守卫焊死）：
 * 契约比线上**严**（Rust 有默认值、TS 却要求必填）→ 前端被迫硬编默认值，或根本发不出请求；
 * 契约比线上**松**（Rust 必填、TS 可选）→ 前端省略字段，到了后端 serde 直接报错。
 * 两种都不会在编译期暴露，只会在真机运行时变成"保存失败"。
 *
 * 真相源：`crates/ta-core/src/domain/mod.rs` 的 `pub struct Provider { … }`（字段上是否有
 * `#[serde(default)]`）与 `src/shared/bindings.ts` 的 `ProviderInput`。
 *
 * 范围说明（有意为之）：**只比对 `Provider → ProviderInput`**。`AuthConfig` 的字段虽然也全部
 * 带 `#[serde(default)]`，但 TS 侧 `AuthConfig` 是**读取**用的形状（`p.auth.scheme` 这类读点
 * 遍布页面），把它放宽会让读取点全部变成 `| undefined` —— 那是另一个决定，不在本守卫范围。
 * `auth` 本身在 `ProviderInput` 里可选，已经足够表达"可以不带"。
 */

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const RUST = "crates/ta-core/src/domain/mod.rs";
const BINDINGS = "src/shared/bindings.ts";

type FieldSet = { required: string[]; defaulted: string[]; flattened: string[] };

/** 解析 Rust 结构体：字段前一行是否有 `#[serde(…default…)]`（`default` 不一定是第一个参数）。 */
export function rustStructFields(text: string, name: string): FieldSet {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.trimStart() === `pub struct ${name} {`);
  if (start < 0) return { required: [], defaulted: [], flattened: [] };
  const required: string[] = [];
  const defaulted: string[] = [];
  const flattened: string[] = [];
  let sawDefault = false;
  let sawFlatten = false;
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (line.startsWith("}")) break;
    // `#[serde(default)]` 与 `#[serde(flatten, default)]` 都算"有默认值"
    if (/^#\[serde\(.*\bdefault\b.*\)\]/.test(line.trim())) sawDefault = true;
    if (/^#\[serde\(.*\bflatten\b.*\)\]/.test(line.trim())) sawFlatten = true;
    const m = line.match(/^\s+pub ([a-z0-9_]+):/);
    if (m?.[1] !== undefined) {
      if (sawFlatten) flattened.push(m[1]);
      else if (sawDefault) defaulted.push(m[1]);
      else required.push(m[1]);
      sawDefault = false;
      sawFlatten = false;
    }
  }
  return { required, defaulted, flattened };
}

/** 解析 TS 类型块：`name?:` = 可选，`name:` = 必填（`Pick<…>` 里的键也算必填）。 */
export function tsTypeFields(text: string, name: string): FieldSet {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => l.startsWith(`export type ${name} =`));
  if (start < 0) return { required: [], defaulted: [], flattened: [] };
  const required: string[] = [];
  const defaulted: string[] = [];
  // `Pick<Provider, "id" | "name" | …>` 里的键是必填
  const pick = lines[start]?.match(/Pick<[^,]+,\s*([^>]+)>/)?.[1] ?? "";
  for (const m of pick.matchAll(/"([a-z0-9_]+)"/g)) if (m[1] !== undefined) required.push(m[1]);
  const flattened: string[] = [];
  let depth = 0;
  for (let i = start; i < lines.length; i++) {
    const line = lines[i] ?? "";
    if (i > start && line.startsWith("};")) break;
    depth += (line.match(/\{/g) ?? []).length - (line.match(/\}/g) ?? []).length;
    if (i === start || depth <= 0) continue;
    const opt = line.match(/^\s+([a-z0-9_]+)\?:/);
    if (opt?.[1] !== undefined) {
      defaulted.push(opt[1]);
      continue;
    }
    const req = line.match(/^\s+([a-z0-9_]+):/);
    if (req?.[1] !== undefined) required.push(req[1]);
  }
  return { required, defaulted, flattened };
}

const sorted = (xs: readonly string[]): string[] => [...xs].sort();

describe("[task-22] ProviderInput 必须与 Rust 的 serde(default) 对齐", () => {
  it("解析器有牙（自证：合成两段源码各读得出必填/默认字段）", () => {
    const rust = rustStructFields(
      ["pub struct T {", "    pub a: u8,", "    #[serde(default)]", "    pub b: u8,", "}"].join("\n"),
      "T",
    );
    expect(rust.required).toEqual(["a"]);
    expect(rust.defaulted).toEqual(["b"]);

    const ts = tsTypeFields(
      [`export type U = Pick<V, "a"> & {`, "  b?: string;", "  c: number;", "};"].join("\n"),
      "U",
    );
    expect(ts.required).toEqual(["a", "c"]);
    expect(ts.defaulted).toEqual(["b"]);

    // `default` 不是第一个参数（`#[serde(flatten, default)]`）也必须认出来 —— 这是本守卫
    // 第一版自己踩到的坑：漏了它就会把 `extra` 判成必填，然后报一个假的不一致。
    const flatten = rustStructFields(
      ["pub struct W {", "    #[serde(flatten, default)]", "    pub extra: Map,", "}"].join("\n"),
      "W",
    );
    expect(flatten.required).toEqual([]);
    expect(flatten.flattened).toEqual(["extra"]);
  });

  it("ProviderInput 的必填/可选字段集 == Rust Provider 的非默认/默认字段集", () => {
    const rust = rustStructFields(readFileSync(RUST, "utf8"), "Provider");
    const ts = tsTypeFields(readFileSync(BINDINGS, "utf8"), "ProviderInput");

    // 先证明真的解析到了（路径写错时不许"对着空集绿"）
    expect(rust.required.length, `${RUST} 没解析出 Provider 必填字段`).toBeGreaterThan(0);
    expect(rust.defaulted.length, `${RUST} 没解析出 Provider 默认字段`).toBeGreaterThan(0);
    expect(ts.required.length, `${BINDINGS} 没解析出 ProviderInput 必填字段`).toBeGreaterThan(0);

    expect(
      sorted(ts.required),
      `必填字段集不一致：Rust 必填 = ${sorted(rust.required).join(", ")}；` +
        `ProviderInput 必填 = ${sorted(ts.required).join(", ")}`,
    ).toEqual(sorted(rust.required));

    expect(
      sorted(ts.defaulted),
      `可选字段集不一致（Rust 有 #[serde(default)] 的字段必须在 ProviderInput 里写成 ?:）：` +
        `Rust 默认 = ${sorted(rust.defaulted).join(", ")}；ProviderInput 可选 = ${sorted(ts.defaulted).join(", ")}`,
    ).toEqual(sorted(rust.defaulted));

    // `#[serde(flatten, default)]` 的 `extra` 是"未知字段保留"的兜底，手写契约**有意不建模**
    // （见 bindings.ts 头部说明）。这里把"有意排除"钉住，免得将来有人以为它是漏掉的一个字段。
    expect(rust.flattened.length, "Provider 上应当有 flatten 的 extra（未知字段保留）").toBeGreaterThan(0);
    for (const f of rust.flattened) {
      expect(
        [...ts.required, ...ts.defaulted].includes(f),
        `${f} 是 flatten 兜底，不该出现在 ProviderInput 里`,
      ).toBe(false);
    }
  });
});

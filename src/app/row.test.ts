// @vitest-environment node
/**
 * [docs/52] `@/shared/row` 的验收 —— 以及**一次真实漂移的收口**。
 *
 * # 这一组测试为什么必须存在
 *
 * 第 319 轮把 `asRow` / `pickStr` / `pickNum` / `pickBool` 从 5 个页面 model 收敛到
 * 这一处时，发现两份实现**语义已经不同**：
 *
 * | 文件 | `asRow([])` |
 * |---|---|
 * | `network/model.ts`、`settings/model.ts` | 返回那个**数组**（`typeof [] === "object"`） |
 * | `providers/model.ts`、`providers/editor.ts`、`providers/detail.tsx` | 返回 `{}` |
 *
 * 收敛必须选一边，而选哪边**不能靠感觉**：选错会让某个页面的行为变掉，
 * 且那种变化是静默的（读出来是 `undefined`，界面画个「—」，没人会红）。
 *
 * 所以这里把口径**钉成可执行的断言**：数组不是一行。
 * 并且把"为什么是这一边"写成可验证的理由 —— 见下面第二条用例：
 * **收下数组并不比挡掉它多读到任何东西**（`arr["model"]` 恒为 `undefined`），
 * 所以"挡掉"在行为上对既有调用点是等价的，唯一的区别是把错误暴露得更早。
 */

import { describe, expect, it } from "vitest";

import { asRow, hasKey, pickArr, pickBool, pickNum, pickRows, pickStr } from "@/shared/row";

describe("[docs/52] asRow：只有普通对象才算「一行」", () => {
  it("普通对象原样返回（含嵌套）", () => {
    const row = { a: 1, nested: { b: 2 } };
    expect(asRow(row)).toBe(row);
  });

  it("数组**不是**一行 —— 这条是收敛两份漂移实现时选定的口径", () => {
    const arr = [1, 2, 3];
    expect(asRow(arr)).toEqual({});
    // 理由（可执行）：收下数组也读不到任何键，所以"挡掉"并不比"收下"少读到东西
    expect((arr as unknown as Record<string, unknown>)["model"]).toBeUndefined();
    expect(Object.keys(asRow(arr))).toEqual([]);
  });

  it("null / undefined / 字符串 / 数字 / 布尔 一律给空对象（调用点因此可以无条件取键）", () => {
    for (const bad of [null, undefined, "text", 42, true, false]) {
      expect(asRow(bad)).toEqual({});
    }
  });

  it("空对象仍然是对象（**不等于**「读不到」这件事需要调用方自己判断）", () => {
    expect(asRow({})).toEqual({});
  });
});

describe("[docs/52] pick*：读不到就是 undefined，绝不编默认值", () => {
  it("pickStr 跳过空串与非字符串", () => {
    expect(pickStr({ a: "", b: "x" }, "a", "b")).toBe("x");
    expect(pickStr({ a: 1, b: null }, "a", "b")).toBeUndefined();
    expect(pickStr({ a: "x" }, "missing")).toBeUndefined();
  });

  it("pickStr 的**多键**形式按给定顺序取第一个（契约与实现键名不一致时的并集读法）", () => {
    // logs 页真实用到的形状：契约叫 credential_id，实现叫 chosen_channel 之类
    expect(pickStr({ new_name: "n", old_name: "o" }, "new_name", "old_name")).toBe("n");
    expect(pickStr({ old_name: "o" }, "new_name", "old_name")).toBe("o");
  });

  it("pickNum 跳过 NaN / Infinity（坏数据不是 0）", () => {
    expect(pickNum({ a: Number.NaN, b: 3 }, "a", "b")).toBe(3);
    expect(pickNum({ a: Number.POSITIVE_INFINITY }, "a")).toBeUndefined();
    expect(pickNum({ a: "3" }, "a")).toBeUndefined();
    expect(pickNum({ a: 0 }, "a")).toBe(0);
  });

  it("pickBool 只认真正的布尔（不把 0 / 1 / \"true\" 当布尔）", () => {
    expect(pickBool({ a: false }, "a")).toBe(false);
    expect(pickBool({ a: 1 }, "a")).toBeUndefined();
    expect(pickBool({ a: "true" }, "a")).toBeUndefined();
  });

  it("pickArr 只认数组，且**空数组与读不到是两件事**", () => {
    expect(pickArr({ a: [] }, "a")).toEqual([]);
    expect(pickArr({ a: "no" }, "a")).toBeUndefined();
    expect(pickArr({}, "a")).toBeUndefined();
  });

  it("hasKey 区分「给了 null」与「压根没这个键」", () => {
    expect(hasKey({ a: null }, "a")).toBe(true);
    expect(hasKey({}, "a")).toBe(false);
    // 原型链上的不算（`in` 会算，`hasOwnProperty` 不会）
    expect(hasKey({}, "toString")).toBe(false);
  });

  it("pickRows 逐项收窄，且读不到给空数组（不是抛 undefined.map）", () => {
    expect(pickRows({ rows: [{ a: 1 }, "junk", [9]] }, "rows")).toEqual([{ a: 1 }, {}, {}]);
    expect(pickRows({}, "rows")).toEqual([]);
    expect(pickRows({ rows: "no" }, "rows")).toEqual([]);
  });
});

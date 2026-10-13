// @vitest-environment node
/**
 * [docs/53] **`TA-5001` 必须把"是哪一处"说出来** —— 第 320 轮用户真机报的 bug。
 *
 * # 用户看到的是什么
 *
 * 在 `/network` 删一份被线路引用的订阅，确认框里写着"会连带删除 / 影响：引用这份订阅的 1 条线路"，
 * 点确认之后只拿到一句：
 *
 * ```
 * [TA-5001] 配置校验失败：1 处问题
 * ```
 *
 * **没有任何可执行信息** —— 哪个字段错了、该怎么办，一个字都没有。
 *
 * # 真因（不是猜的）
 *
 * 后端其实**已经**把字段路径带出来了：
 * `IpcError::from_field_errors` 生成 `message = "配置校验失败：N 处问题"`，
 * 同时把 `details = [{ path, code, message }]` 一起返回
 * （Rust 侧 `ta_core::error::FieldError`，validator 的 `ref_exists` 填的是
 * `lanes[i].subscription` + "引用的 `s1` 不存在（应在 proxy_subscriptions[].id 里）"）。
 *
 * 而前端唯一的格式化出口 `errorMessage()` **只读 `message`**，把 `details` 整个丢掉了。
 * 于是结构化信息在最后一厘米处丢失。
 *
 * # 这一组测试钉什么
 *
 * 1. `details` 是逐字段错误时，`errorMessage` 必须带上**路径与说明**；
 * 2. `details` 不是那个形状时**不许**乱拼（宁可只给 `message`，也不要显示 `[object Object]`）；
 * 3. 没有 `details` 的普通错误**逐字不变**（不能因为这次改动让别的报错多出尾巴）。
 */

import { describe, expect, it } from "vitest";

import { errorMessage, fieldErrorsOf } from "@/shared/api";
import { TokenAgencyError } from "@/shared/api/client";

/** 真机上那种错误的**原样形状**（从 `IpcError::from_field_errors` 一路下来）。 */
function configInvalidWithDetails(count: number): TokenAgencyError {
  const details = [
    {
      path: "lanes[0].subscription",
      code: "not_found",
      message: "引用的 `s1` 不存在（应在 proxy_subscriptions[].id 里）",
    },
    { path: "lanes[1].subscription", code: "not_found", message: "引用的 `s2` 不存在" },
  ].slice(0, count);
  return new TokenAgencyError({
    code: "TA-5001",
    message: `配置校验失败：${count} 处问题`,
    details,
  });
}

describe("[docs/53] errorMessage：TA-5001 要说清是哪一处", () => {
  it("带 details 时，路径与说明都出现在同一句话里", () => {
    const text = errorMessage(configInvalidWithDetails(1));
    expect(text).toContain("[TA-5001]");
    expect(text).toContain("1 处问题");
    // 关键：用户要能照着这句话去改
    expect(text).toContain("lanes[0].subscription");
    expect(text).toContain("proxy_subscriptions[].id");
  });

  it("多于一处的逐条列出（用 `；` 分隔），不吞掉后面的", () => {
    const text = errorMessage(configInvalidWithDetails(2));
    expect(text).toContain("lanes[0].subscription");
    expect(text).toContain("lanes[1].subscription");
    expect(text).toContain("；");
  });

  it("details 形状不对时**不乱拼**（不出现 [object Object] / undefined）", () => {
    for (const bad of [[], "nope", 42, [null], [{ path: "a" }], [{ message: "只有说明" }]]) {
      const err = new TokenAgencyError({
        code: "TA-5001",
        message: "配置校验失败：1 处问题",
        details: bad,
      });
      const text = errorMessage(err);
      expect(text).toBe("[TA-5001] 配置校验失败：1 处问题");
      expect(text).not.toContain("[object");
      expect(text).not.toContain("undefined");
    }
  });

  it("没有 details 的普通错误逐字不变（这次改动不许给别的报错加尾巴）", () => {
    const err = new TokenAgencyError({ code: "TA-7001", message: "写配置失败（测试夹具）" });
    expect(errorMessage(err)).toBe("[TA-7001] 写配置失败（测试夹具）");
  });

  it("非 IPC 错误仍然走原来的两条路（Error / 其它）", () => {
    expect(errorMessage(new Error("普通错误"))).toBe("普通错误");
    expect(errorMessage("字符串错误")).toBe("字符串错误");
  });

  it("fieldErrorsOf 单独可用：只为形状正确的条目买单", () => {
    expect(fieldErrorsOf(configInvalidWithDetails(2))).toHaveLength(2);
    expect(fieldErrorsOf(new Error("不是 IPC 错误"))).toEqual([]);
    const mixed = new TokenAgencyError({
      code: "TA-5001",
      message: "m",
      details: [{ path: "a.b", code: "x", message: "说明" }, { path: "缺 message" }, 7],
    });
    expect(fieldErrorsOf(mixed)).toEqual([{ path: "a.b", code: "x", message: "说明" }]);
  });
});

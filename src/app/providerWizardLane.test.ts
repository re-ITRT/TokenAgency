/**
 * [docs/29 §2.2]（用户第 112 轮）**"添加"向导里为服务商选代理**这条链路的载荷侧。
 *
 * 真机探针（`step25`）已经证明两处下拉的**选项**一致；这里钉住另一半：
 * **选完之后发给后端的到底是不是那个值**。少了这一半，界面看起来对、落库却是空的。
 *
 * 三件事：
 * 1. 选「直连」→ 载荷里是标记 `direct:`（由后端建/复用那条 direct 线路）；
 * 2. 选「自动（某份订阅）」→ 载荷里是 `auto:<订阅 id>`；
 * 3. **载荷里不许出现线路 id 之外的臆造值**（前端不建线路、不编 id）。
 */
import { describe, expect, it } from "vitest";

import { step1ProviderPayload } from "@/pages/providers/providerWizardModel";
import type { Step1Draft } from "@/pages/providers/providerWizardModel";

const base: Step1Draft = {
  name: "TA E2E 供应商",
  protocol: "openai",
  baseUrl: "https://api.example.com/v1",
  key: "",
  lane: "direct:",
};

function laneOf(lane: string): unknown {
  const payload = step1ProviderPayload({ ...base, lane }, "ta-e2e") as Record<string, unknown>;
  return payload["lane"];
}

describe("[docs/29 §2.2] 向导第 1 步把所选代理原样发给后端", () => {
  it("选「直连」→ 标记 `direct:`（不是空、也不是某个编出来的 id）", () => {
    expect(laneOf("direct:")).toBe("direct:");
  });

  it("选「自动」→ 标记 `auto:<订阅 id>`", () => {
    expect(laneOf("auto:ta-e2e-sub")).toBe("auto:ta-e2e-sub");
  });

  it("选某条已配置线路 → 就是那个线路 id（逐字，不改写）", () => {
    expect(laneOf("lane-jp")).toBe("lane-jp");
  });

  it("选「不指定（继承）」→ **显式 null**（清空，走继承链），不是不发这个键", () => {
    // 与详情页 `buildProviderPayload` 同一套语义（`optional("") === null`）：
    // `null` = "这次把它清掉"；不存在的键 = "不动它"（那是 `undefined` 的情形）。
    expect(laneOf("")).toBeNull();
  });
});

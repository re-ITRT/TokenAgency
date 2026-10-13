/**
 * 服务商级代理下拉的**选项构造**（）。
 *
 * ## 这一组现在钉的核心是"节点必须真的出现在选项里"
 *
 * 用户第 325 轮的原话：
 *
 * > 我已经添加了订阅和节点，但是在服务商那边选节点还是啥也没看到，这里不应该显示所有已经
 * > 添加的节点和"直连"，"自动选择:xx订阅"（可以有不同的订阅，多个订阅内部自动选）
 *
 * **根因**：这个下拉以前接的是 `lane_list`（**只有已配置的线路**），而不是 `lane_options`
 * （后端 `compute_lane_choices` 把已采纳版本的节点逐个列出来）。于是用户配好了订阅和节点，
 * 下拉里仍然一个节点都没有 —— 而 `proxyChoices` 那套映射早就写好了，只是**从来没人调用**。
 *
 * 所以下面第 3、4 条是**反回归**：它们只有在 `lane_options` 的分组被真的接进来时才会过。
 */
import { describe, expect, it } from "vitest";

import type { LaneChoiceGroup } from "@/shared/bindings";
import { providerLaneChoices } from "@/pages/providers/editor";

/** 「直连」那一组：配置里还没有 direct 线路时后端给的是**标记** `direct:`。 */
const DIRECT_GROUP: LaneChoiceGroup = {
  group: "直连（本机网络）",
  items: [
    {
      value: "direct:",
      label: "直连（本机网络）",
      kind: "direct",
      lane_id: undefined,
      subscription_id: undefined,
      node: undefined,
    },
  ],
};

/** 一份订阅的分组：一条「自动」线路 + 两个节点（其中一个已有等价线路）。 */
const SUB_GROUP: LaneChoiceGroup = {
  group: "机场 A",
  items: [
    {
      value: "lane-a-auto",
      label: "自动（机场 A）",
      kind: "lane",
      lane_id: "lane-a-auto",
      subscription_id: "sub-a",
      node: undefined,
    },
    {
      value: "node:sub-a:东京-03",
      label: "东京-03",
      kind: "node",
      lane_id: undefined,
      subscription_id: "sub-a",
      node: "东京-03",
    },
    {
      // 已经有等价线路 → 后端同时给 lane_id；界面只留**线路**那一项
      value: "node:sub-a:美国-01",
      label: "美国-01",
      kind: "node",
      lane_id: "lane-sub-a-us01",
      subscription_id: "sub-a",
      node: "美国-01",
    },
  ],
};

describe("服务商级代理下拉：自动（每份订阅一个）→ 直连 → 继承 → 线路与节点", () => {
  it("顺序：每份订阅一个「自动」在前，然后直连、继承，最后是线路与订阅节点", () => {
    const options = providerLaneChoices(
      [DIRECT_GROUP, SUB_GROUP],
      "不指定（继承）",
      [{ id: "sub-a", name: "机场 A" }],
      "自动",
    );

    expect(options.map((o) => o.value)).toEqual([
      "auto:sub-a",
      "direct:",
      "",
      "lane-a-auto",
      "node:sub-a:东京-03",
      "node:sub-a:美国-01",
    ]);
    expect(options.map((o) => o.label)).toEqual([
      "自动（机场 A）",
      "直连（本机网络）",
      "不指定（继承）",
      "自动（机场 A）",
      "东京-03（机场 A）",
      "美国-01（机场 A）",
    ]);
  });

  it("**订阅节点真的在选项里**（这就是用户报的 bug：以前一个节点都看不到）", () => {
    const options = providerLaneChoices([DIRECT_GROUP, SUB_GROUP], "不指定（继承）", [], "自动");
    const nodes = options.filter((o) => o.value.startsWith("node:"));
    expect(nodes.map((o) => o.value)).toEqual(["node:sub-a:东京-03", "node:sub-a:美国-01"]);
    // 还没建过线路的节点：必须带上 `nodeRef`（选中后要 `lane_ensure_for_node` 换成 id）
    expect(nodes[0]?.nodeRef).toEqual({ subscriptionId: "sub-a", node: "东京-03" });
    expect(nodes[0]?.laneId).toBeUndefined();
    // 已经有等价线路的节点：后端把 lane_id 一起给了 → 选中时不必再建
    expect(nodes[1]?.laneId).toBe("lane-sub-a-us01");
  });

  it("值是**标记**而不是编出来的 id：`auto:<订阅>` / `direct:` / `node:<订阅>:<节点>`", () => {
    const options = providerLaneChoices(
      [DIRECT_GROUP, SUB_GROUP],
      "不指定（继承）",
      [{ id: "sub-a", name: "机场 A" }],
      "自动",
    );
    // 「自动」与「直连」由后端 `provider_upsert` 解析；节点由 `lane_ensure_for_node` 换成 id
    expect(options[0]?.value).toBe("auto:sub-a");
    expect(options[1]?.value).toBe("direct:");
    // 已有的线路项就是真实 id
    expect(options[3]?.value).toBe("lane-a-auto");
    expect(options[3]?.laneId).toBe("lane-a-auto");
  });

  it("多份订阅：每份一个「自动」，订阅名一律带上（用户原话「自动选择:xx订阅」）", () => {
    const options = providerLaneChoices(
      [DIRECT_GROUP],
      "不指定（继承）",
      [
        { id: "sub-a", name: "机场 A" },
        { id: "sub-b", name: "机场 B" },
      ],
      "自动选择",
    );
    expect(options.slice(0, 2).map((o) => o.label)).toEqual([
      "自动选择（机场 A）",
      "自动选择（机场 B）",
    ]);
    expect(options.slice(0, 2).map((o) => o.value)).toEqual(["auto:sub-a", "auto:sub-b"]);
  });

  it("单份订阅时**也**带名字（原来不带，用户看不出用的是哪份订阅）", () => {
    const options = providerLaneChoices([DIRECT_GROUP], "不指定（继承）", [
      { id: "sub-a", name: "机场 A" },
    ]);
    expect(options[0]?.label).toBe("自动选择（机场 A）");
  });

  it("没有订阅、没有分组时只剩「直连 + 继承」（不编任何不存在的选项）", () => {
    // 后端在"配置里一条 direct 线路都没有"时仍会给出 `direct:` 那一组
    const options = providerLaneChoices([DIRECT_GROUP], "不指定（继承）", [], "自动选择");
    expect(options.map((o) => o.value)).toEqual(["direct:", ""]);
    expect(options.map((o) => o.label)).toEqual(["直连（本机网络）", "不指定（继承）"]);
    // 「直连」那一项被标成 direct（界面据此让它单独排最前）
    expect(options[0]?.direct).toBe(true);
  });

  it("订阅名为空时退回订阅 id（不显示一个空括号）", () => {
    const options = providerLaneChoices([DIRECT_GROUP], "不指定（继承）", [
      { id: "sub-a", name: "" },
    ]);
    expect(options[0]?.label).toBe("自动选择");
  });
it("已配置的 direct 线路**不会**被拼成「直连（直连（本机网络））」", () => {
    // 真机截图里出现过这个双括号：direct 线路名叫「直连」，而它的分组叫「直连（本机网络）」。
    const options = providerLaneChoices(
      [
        {
          group: "直连（本机网络）",
          items: [{ value: "lane-direct", label: "直连", kind: "direct", lane_id: "lane-direct" }],
        },
      ],
      "不指定（继承）",
      [],
    );
    expect(options.map((o) => o.label)).toEqual(["直连", "不指定（继承）"]);
  });
});
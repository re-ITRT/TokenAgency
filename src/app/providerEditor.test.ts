/**
 * 服务商详情页编辑逻辑（[docs/23] / `src/pages/providers/editor.ts`）的纯逻辑验收。
 *
 * 为什么单独一份：这一层最容易写出**静默的数据丢失** —— `provider_upsert` 是整条覆盖写，
 * 载荷里少一个键就是把那个字段重置成默认值。所以这里逐条钉住三件事：
 *
 * 1. 以**原始记录为底**：自定义字段（`extra`）原样带回去；
 * 2. 只读视图专有的键（`status` / `success_rate_24h`…）**剔除**（它们不是配置）；
 * 3. 用户清空的 `Option` 字段发**显式 `null`**（不发 = 默认值，那不是清空）。
 *
 * 另有：`canEdit`（只读视图不许写）、`validateDraft`（预检不过一次都不发）、
 * `isDirty`（没有改动就不用写库）。
 */

import { describe, expect, it, vi } from "vitest";

import {
  appendHeader,
  buildProviderPayload,
  canEdit,
  draftFromRow,
  isDirty,
  protocolChoices,
  providerLaneChoices,
  removeHeader,
  resolveLaneChoice,
  updateHeader,
  validateDraft,
} from "@/pages/providers/editor";
import { readProvider } from "@/pages/providers/model";

/** 一份完整的 Rust `Provider`（`catalog.rs` 的 `provider_list` 返回的就是这个形状）。 */
function rustProvider(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: "p-a",
    name: "自建网关",
    protocol: "openai",
    base_url: "https://llm.example.com/v1",
    auth: { scheme: "header", header: "x-api-key", prefix: "Bearer " },
    lane: "lane-jp",
    headers: { "x-org": "team-a" },
    on_header_conflict: "keep_client",
    passthrough: { header_mode: "pass", body: "preserve" },
    timeout: { connect_s: 10, read_s: 120, total_s: null },
    tls: { insecure: false, extra_ca_pem: "-----BEGIN CERTIFICATE-----", min_version: "1.2" },
    model_prefix: "openai/",
    enabled: true,
    ...overrides,
  };
}

/** 契约 / mock 的只读视图（没有连接配置字段）。 */
function viewProvider(): unknown {
  return {
    id: "p-view",
    name: "command code",
    protocol: "anthropic",
    base_url: "https://api.commandcode.test/v1",
    enabled: true,
    status: "ok",
    success_rate_24h: 0.99,
    avg_latency_ms: 80,
    channel_count: 2,
  };
}

describe("[docs/23] 草稿：后端一行 → 可编辑草稿", () => {
  it("完整记录：每一个字段都读进草稿（尾随空格这种有意义的值原样保留）", () => {
    const draft = draftFromRow(readProvider(rustProvider()));

    expect(draft.name).toBe("自建网关");
    expect(draft.protocol).toBe("openai");
    expect(draft.baseUrl).toBe("https://llm.example.com/v1");
    expect(draft.enabled).toBe(true);
    expect(draft.lane).toBe("lane-jp");
    expect(draft.modelPrefix).toBe("openai/");
    expect(draft.authScheme).toBe("header");
    expect(draft.authHeader).toBe("x-api-key");
    // `Bearer ` 与 `Bearer` 是两个不同的前缀 → 草稿不许 trim 它
    expect(draft.authPrefix).toBe("Bearer ");
    expect(draft.timeoutConnectS).toBe("10");
    expect(draft.timeoutReadS).toBe("120");
    expect(draft.timeoutTotalS).toBe("");
    expect(draft.tlsInsecure).toBe(false);
    expect(draft.tlsMinVersion).toBe("1.2");
    // 额外 CA 是用户的材料：预填真值，否则一保存就把自己配过的 CA 抹掉
    expect(draft.tlsExtraCa).toContain("BEGIN CERTIFICATE");
    expect(draft.onHeaderConflict).toBe("keep_client");
    expect(draft.passthroughHeaderMode).toBe("pass");
    expect(draft.passthroughBody).toBe("preserve");
    expect(draft.headers).toEqual([{ name: "x-org", value: "team-a" }]);
    // 明文密钥永不预填（它只存在于密钥仓库里）
    expect(draft.key).toBe("");
  });

  it("读不到的字段留空 / 落到后端默认（不编），超时的两个必填项落到 Rust 的默认值", () => {
    const draft = draftFromRow(readProvider({ id: "p-b", name: "空", protocol: "openai", base_url: "https://b" }));

    expect(draft.lane).toBe("");
    expect(draft.modelPrefix).toBe("");
    expect(draft.authHeader).toBe("");
    expect(draft.authPrefix).toBe("");
    expect(draft.tlsMinVersion).toBe("");
    expect(draft.tlsExtraCa).toBe("");
    // `TimeoutConfig` 里这两个字段**不是 Option** → 读不到时唯一诚实的显示是它的默认值
    expect(draft.timeoutConnectS).toBe("10");
    expect(draft.timeoutReadS).toBe("600");
    expect(draft.timeoutTotalS).toBe("");
  });

  it("canEdit：只读视图（没有连接配置字段）一律不许写", () => {
    expect(canEdit(readProvider(rustProvider()))).toBe(true);
    expect(canEdit(readProvider(viewProvider()))).toBe(false);
    expect(canEdit(undefined)).toBe(false);
  });
});

describe("[docs/23] 载荷：以原始记录为底（这是「改个名字不会抹掉配置」的保证）", () => {
  it("改一个字段：其余键一个不少，id 复用", () => {
    const row = readProvider(rustProvider());
    const payload = buildProviderPayload(row, { ...draftFromRow(row), name: "改名后" });

    expect(payload.name).toBe("改名后");
    expect(payload.id).toBe("p-a");
    expect(payload.protocol).toBe("openai");
    expect(payload.base_url).toBe("https://llm.example.com/v1");
    expect(payload.auth).toMatchObject({ scheme: "header", header: "x-api-key", prefix: "Bearer " });
    expect(payload.headers).toEqual({ "x-org": "team-a" });
    expect(payload.on_header_conflict).toBe("keep_client");
    expect(payload.passthrough).toMatchObject({ header_mode: "pass", body: "preserve" });
    expect(payload.timeout).toMatchObject({ connect_s: 10, read_s: 120, total_s: null });
    expect(payload.tls).toMatchObject({ insecure: false, min_version: "1.2" });
    expect(payload.tls!.extra_ca_pem).toContain("BEGIN CERTIFICATE");
    expect(payload.lane).toBe("lane-jp");
    expect(payload.model_prefix).toBe("openai/");
    expect(payload.enabled).toBe(true);
  });

  it("原始记录里的**自定义字段**（`extra`）原样带回去 —— 没暴露过的键一个都不许丢", () => {
    const row = readProvider(rustProvider({ my_custom_field: { nested: true }, another: "keep-me" }));
    const payload = buildProviderPayload(row, draftFromRow(row)) as unknown as Record<string, unknown>;

    expect(payload["my_custom_field"]).toEqual({ nested: true });
    expect(payload["another"]).toBe("keep-me");
  });

  it("只读视图专有的键被剔除（不然会被后端当成用户自定义字段存进 extra）", () => {
    const row = readProvider(
      rustProvider({ status: "ok", success_rate_24h: 0.9, avg_latency_ms: 12, channel_count: 3 }),
    );
    const payload = buildProviderPayload(row, draftFromRow(row)) as unknown as Record<string, unknown>;

    for (const key of ["status", "success_rate_24h", "avg_latency_ms", "channel_count", "provider_name"]) {
      expect(Object.prototype.hasOwnProperty.call(payload, key), `${key} 不该出现在载荷里`).toBe(false);
    }
  });

  it("清空可选字段 = 显式 null（不发这个键等于用默认值，那不是清空）", () => {
    const row = readProvider(rustProvider());
    const draft = {
      ...draftFromRow(row),
      lane: "",
      modelPrefix: "",
      authHeader: "",
      authPrefix: "",
      timeoutTotalS: "",
      tlsExtraCa: "",
      tlsMinVersion: "",
      passthroughHeaderMode: "",
      passthroughBody: "",
    };
    const payload = buildProviderPayload(row, draft);
    const raw = payload as unknown as Record<string, unknown>;

    expect(raw["lane"]).toBeNull();
    expect(raw["model_prefix"]).toBeNull();
    expect(payload.auth).toMatchObject({ header: null, prefix: null });
    expect(payload.timeout).toMatchObject({ total_s: null });
    expect(payload.tls).toMatchObject({ extra_ca_pem: null, min_version: null });
    expect(payload.passthrough).toMatchObject({ header_mode: null, body: null });
  });

  it("附加头：空头名丢掉、同名后者覆盖前者（载荷就是一张表，不是文本）", () => {
    const row = readProvider(rustProvider());
    const draft = {
      ...draftFromRow(row),
      headers: [
        { name: "x-a", value: "1" },
        { name: "  ", value: "丢掉" },
        { name: "x-b", value: "2" },
        { name: "x-a", value: "3" },
      ],
    };
    expect(buildProviderPayload(row, draft).headers).toEqual({ "x-a": "3", "x-b": "2" });
  });

  it("数字字段按秒写回；留空的总超时是 null", () => {
    const row = readProvider(rustProvider());
    const draft = { ...draftFromRow(row), timeoutConnectS: "5", timeoutReadS: "900", timeoutTotalS: "60" };
    expect(buildProviderPayload(row, draft).timeout).toMatchObject({
      connect_s: 5,
      read_s: 900,
      total_s: 60,
    });
  });
});

describe("[docs/23] 本地预检与「有没有改动」", () => {
  it("必填 / base_url 形状 / 超时必须是正整数 / 头名不空不重复", () => {
    const base = draftFromRow(readProvider(rustProvider()));

    expect(validateDraft(base)).toEqual([]);

    expect(validateDraft({ ...base, name: "  " }).map((i) => i.message)).toContain("wizard.required");
    expect(validateDraft({ ...base, protocol: "" }).map((i) => i.field)).toContain("protocol");
    expect(validateDraft({ ...base, baseUrl: "llm.example.com" }).map((i) => i.message)).toContain(
      "wizard.invalidUrl",
    );
    expect(validateDraft({ ...base, timeoutConnectS: "0" }).map((i) => i.field)).toContain(
      "timeoutConnectS",
    );
    expect(validateDraft({ ...base, timeoutReadS: "abc" }).map((i) => i.field)).toContain("timeoutReadS");
    expect(validateDraft({ ...base, timeoutTotalS: "-1" }).map((i) => i.field)).toContain("timeoutTotalS");
    // 总超时**可以留空**（Rust 侧是 Option）
    expect(validateDraft({ ...base, timeoutTotalS: "" })).toEqual([]);

    expect(
      validateDraft({ ...base, headers: [{ name: " ", value: "x" }] }).map((i) => i.message),
    ).toContain("providers.edit.headerNameRequired");
    expect(
      validateDraft({
        ...base,
        headers: [
          { name: "x-a", value: "1" },
          { name: "x-a", value: "2" },
        ],
      }).map((i) => i.message),
    ).toContain("providers.edit.headerNameDuplicate");
  });

  it("isDirty：没有改动就不写库；填了明文密钥也算改动", () => {
    const row = readProvider(rustProvider());
    const baseline = draftFromRow(row);

    expect(isDirty(baseline, baseline)).toBe(false);
    expect(isDirty({ ...baseline, name: "改了" }, baseline)).toBe(true);
    expect(isDirty({ ...baseline, key: "sk-x" }, baseline)).toBe(true);
  });
});

describe("[docs/23] 下拉选项与头表编辑的纯函数", () => {
  it("协议下拉：契约外的取值原样补一行（不改写后端说的话）", () => {
    const labelOf = (protocol: string): string => `L:${protocol}`;
    expect(protocolChoices(["openai", "anthropic"], labelOf, "openai")).toEqual([
      { value: "openai", label: "L:openai" },
      { value: "anthropic", label: "L:anthropic" },
    ]);
    expect(protocolChoices(["openai"], labelOf, "weird-proto")).toEqual([
      { value: "weird-proto", label: "weird-proto" },
      { value: "openai", label: "L:openai" },
    ]);
  });

  it("代理下拉：第一条是「直连」，第二条是「不指定」（值 = 空串 → 载荷里的 null）", () => {
    // [docs/58] 参数从 `lane_list` 改成 **`lane_options` 的分组**：前者只有已配置的线路，
    // 于是订阅节点一个都进不了下拉 —— 那正是用户第 325 轮报的 bug。
    const choices = providerLaneChoices(
      [
        {
          group: "直连（本机网络）",
          items: [
            { value: "direct:", label: "直连（本机网络）", kind: "direct" as const },
          ],
        },
        {
          group: "机场 A",
          items: [
            { value: "lane-jp", label: "日本线", kind: "lane" as const, lane_id: "lane-jp" },
            {
              value: "node:sub-a:东京-03",
              label: "东京-03",
              kind: "node" as const,
              subscription_id: "sub-a",
              node: "东京-03",
            },
          ],
        },
      ],
      "不指定",
    );
    expect(choices.map((c) => c.value)).toEqual([
      // [docs/29 §2.2] 「直连」排第一（用户第 81 轮追问过"直连你都没单独列出来？"），
      // 值是标记 `direct:`，由后端把那条 direct 线路建出来（可能还不存在）
      "direct:",
      "",
      "lane-jp",
      "node:sub-a:东京-03",
    ]);
    expect(choices.map((c) => c.label)).toEqual([
      "直连（本机网络）",
      "不指定",
      // 分组名只在"确实是额外信息"时拼上（[docs/58]：两份订阅里都有同名节点时靠它区分）
      "日本线（机场 A）",
      "东京-03（机场 A）",
    ]);
  });

  it("头表：改 / 删 / 加都是纯函数，不动原数组", () => {
    const headers = [{ name: "a", value: "1" }];
    const changed = updateHeader(headers, 0, { value: "2" });
    expect(changed).toEqual([{ name: "a", value: "2" }]);
    expect(headers).toEqual([{ name: "a", value: "1" }]);
    expect(removeHeader(changed, 0)).toEqual([]);
    expect(appendHeader(headers)).toEqual([
      { name: "a", value: "1" },
      { name: "", value: "" },
    ]);
  });
});


// ================================================================ [docs/58] 选中值 → 真正写下去的东西

/**
 * `resolveLaneChoice` 是这一轮**唯一**一处会让"选了节点却存不下去"的地方：
 *
 * 下拉里节点项的值是 `node:<订阅>:<节点名>` —— 那**不是** lane id，而且
 * `provider_upsert` 只解析 `auto:` / `direct:` 两种标记。不先换成真实 id 就发出去，
 * 后端会拿它当线路 id → 校验器报"线路不存在" → 用户选了节点，保存后什么都没发生。
 *
 * 所以三种"后端自己认识"的值必须**原样放过**（不许多调一次命令、也不许改写），
 * 只有节点项才落地。
 */
describe("[docs/58] resolveLaneChoice：后端认识的标记原样放过，只有节点项先落地", () => {
  const choices = [
    { value: "auto:sub-a", label: "自动（机场 A）" },
    { value: "direct:", label: "直连（本机网络）", direct: true },
    { value: "lane-jp", label: "日本线", laneId: "lane-jp" },
    {
      value: "node:sub-a:东京-03",
      label: "东京-03",
      nodeRef: { subscriptionId: "sub-a", node: "东京-03" },
    },
    {
      value: "node:sub-a:美国-01",
      label: "美国-01",
      laneId: "lane-us01",
      nodeRef: { subscriptionId: "sub-a", node: "美国-01" },
    },
  ];

  it("「不指定」（空串/null/undefined）原样返回 —— 载荷那边会变成显式 null", async () => {
    const ensure = vi.fn();
    expect(await resolveLaneChoice("", choices, ensure)).toBe("");
    expect(await resolveLaneChoice(null, choices, ensure)).toBeNull();
    expect(await resolveLaneChoice(undefined, choices, ensure)).toBeUndefined();
    expect(ensure).not.toHaveBeenCalled();
  });

  it("`auto:` / `direct:` / 真实 lane id **一个都不许动**（后端只有一份判断）", async () => {
    const ensure = vi.fn();
    expect(await resolveLaneChoice("auto:sub-a", choices, ensure)).toBe("auto:sub-a");
    expect(await resolveLaneChoice("direct:", choices, ensure)).toBe("direct:");
    expect(await resolveLaneChoice("lane-jp", choices, ensure)).toBe("lane-jp");
    // 不许"顺手"替后端把 auto/direct 建出来 —— 那等于多了一份真相源
    expect(ensure).not.toHaveBeenCalled();
  });

  it("节点项：调 lane_ensure_for_node 拿到真实 id（**已有等价线路则一次都不调**）", async () => {
    const ensure = vi.fn(async (subscriptionId: string, node: string) => ({
      id: `lane-${subscriptionId}-${node}`,
    }));
    // 还没有线路 → 建出来
    expect(await resolveLaneChoice("node:sub-a:东京-03", choices, ensure)).toBe(
      "lane-sub-a-东京-03",
    );
    expect(ensure).toHaveBeenCalledWith("sub-a", "东京-03");
    // 后端已经把 lane_id 给了 → 直接用它，不再建
    ensure.mockClear();
    expect(await resolveLaneChoice("node:sub-a:美国-01", choices, ensure)).toBe("lane-us01");
    expect(ensure).not.toHaveBeenCalled();
  });

  it("拿不到 nodeRef 时**不猜**：原样返回，让后端如实报错（编一个 id 出去才是真的坏）", async () => {
    const ensure = vi.fn();
    expect(await resolveLaneChoice("node:sub-x:不存在", choices, ensure)).toBe(
      "node:sub-x:不存在",
    );
    expect(ensure).not.toHaveBeenCalled();
  });
});
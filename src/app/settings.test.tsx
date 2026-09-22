// @vitest-environment jsdom
/**
 * 设置页（[07 §7] / [09 §1.2 E1–E5]）的验收。重点不是"渲染出来了"，而是**四条硬边界**：
 *
 * 1. **I2：只展示系统网络信息，绝不提供改系统设置的入口。**
 *    这一条在这里是**机械检查**（不是靠自觉）：`EDITABLE_PATHS` 与 `FORBIDDEN_PATHS` 不许有交集，
 *    `setPath()` 对禁止字段**抛错**，并且高级那一节的只读块里**一个控件都没有**
 *    （checkbox / switch / button 全为空）。
 * 2. **未注册的命令不许变成"点了没反应的开关"。** 开机自启动的读/写命令（app_get_autostart /
 *    app_set_autostart）尚未注册 → 这一节里**不许有 checkbox 或 switch**，只许有诚实说明，
 *    而且说明里必须点名缺的是哪两条命令。
 * 3. **没有的命令不许画按钮。** 日志导出需要 log_export（未注册）→ 页面里不许有导出按钮，
 *    但必须留一行说明，否则下一个人会以为"忘了做"。
 * 4. **不许假成功。** 后端校验说 not ok 时，`config_apply` **一次都不许被调用**；
 *    校验结果读不出来也按"未通过"处理，不当作成功。
 *
 * 另外钉住错误显示：配置读取失败必须带 `TA-xxxx`（唯一格式化出口 `errorMessage()`）。
 */

import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import SettingsPage from "@/pages/settings";
import {
  EDITABLE_PATHS,
  FORBIDDEN_PATHS,
  diffPaths,
  isValidListen,
  pathKey,
  readApplyResult,
  readNewestRow,
  readSnapshots,
  readStorageStats,
  readValidationReport,
  secretState,
  setPath,
  validateDraft,
} from "@/pages/settings/model";
import { TokenAgencyError, invokeCommand, registerMock } from "@/shared/api";
import { registerBaselineMocks } from "@/shared/api/mock";
import type { ConfigDoc } from "@/shared/bindings";
import { i18n } from "@/shared/i18n";
import { buildTheme } from "@/theme";

// ================================================================ 夹具

/** 基线配置树（从 mock 后端读一次真值，测试里不再手抄一份字段列表）。 */
let baseDoc: ConfigDoc;

/** 让 `config_get` 在某条用例里失败（测错误码显示），不影响别的用例。 */
let configFails = false;
/** 被真正交付给 `config_apply` 的文档（"有没有偷偷应用"的计数证据）。 */
let applyCalls: ConfigDoc[] = [];
/** `config_validate` 的返回值（可换成"未通过"来验证应用被阻止）。 */
let validateResult: unknown = { ok: true, errors: [], warnings: [] };
/** `config_rollback` 收到过的快照 id。 */
let rollbackIds: string[] = [];

const EXPORTED_YAML = "version: 1\ngateway:\n  listen: 127.0.0.1:8787\n";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
  baseDoc = await invokeCommand("config_get", undefined);
});

beforeEach(() => {
  configFails = false;
  applyCalls = [];
  rollbackIds = [];
  validateResult = { ok: true, errors: [], warnings: [] };

  registerMock("config_get", () => {
    if (configFails) {
      throw new TokenAgencyError({ code: "TA-5001", message: "配置读取失败（测试夹具）" });
    }
    return baseDoc;
  });
  registerMock("config_validate", () => validateResult);
  registerMock("config_apply", (args) => {
    applyCalls.push((args as { doc: ConfigDoc }).doc);
    return { applied: true, restart_required: false, kernel_reload: false };
  });
  registerMock("config_export_yaml", () => EXPORTED_YAML);
  registerMock("config_history_list", () => [
    { id: "snap-1", label: "apply", created_at_ms: 1_770_000_000_000, size_bytes: 2048 },
  ]);
  registerMock("config_rollback", (args) => {
    rollbackIds.push((args as { snapshot_id: string }).snapshot_id);
    return { applied: true, restart_required: true, kernel_reload: true };
  });
  // 最近一条日志行（"记录时的模式"是可读的证据；当前模式没有读取接口）
  registerMock("log_query", () => ({
    rows: [{ request_id: "req_1", ts_ms: 1_770_000_000_000, mode: "meta" }],
    total: 1,
    cursor: null,
  }));
});

afterEach(() => {
  cleanup();
});

function renderPage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider theme={buildTheme("dark")}>
        <SettingsPage />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

/**
 * 等到配置**真的加载完**：`settings-field-listen` 在加载中也会渲染（只是禁用），
 * 所以必须等到它的输入框不再 disabled，否则后续点 checkbox 会点在一个禁用控件上 ——
 * 那会让"应用"一直是禁用的，测试就会误判成"页面没反应"。
 */
async function waitForConfig(): Promise<HTMLElement> {
  const box = (await screen.findByTestId("settings-field-listen")) as HTMLElement;
  await waitFor(() => {
    const input = within(box).getByRole("textbox") as HTMLInputElement;
    if (input.disabled) throw new Error("配置还没加载完（输入框仍被禁用）");
  });
  return box;
}

// ================================================================ ① I2：只展示，不改系统设置

describe("[I2] 设置页只展示系统网络信息，不提供任何改系统设置的入口", () => {
  it("可改字段与禁止字段没有交集，且禁止字段覆盖 TUN / 系统代理 / 自启动开关", () => {
    const editable = EDITABLE_PATHS.map(pathKey);
    const forbidden = FORBIDDEN_PATHS.map(pathKey);

    // 自证：两个集合都真的非空（否则这条断言会对着空集变绿）
    expect(editable.length).toBeGreaterThan(8);
    expect(forbidden.length).toBeGreaterThanOrEqual(5);

    for (const path of forbidden) {
      expect(editable, `禁止字段 ${path} 出现在可改字段里`).not.toContain(path);
    }
    expect(forbidden).toEqual(
      expect.arrayContaining([
        "net.tun",
        "net.system_proxy",
        "system_integration.enable_tun",
        "system_integration.enable_system_proxy",
        "system_integration.autostart",
      ]),
    );
  });

  it("setPath() 对禁止字段直接抛错（不是「悄悄忽略」）", () => {
    expect(() => setPath(baseDoc, ["net", "tun"], true)).toThrow();
    expect(() => setPath(baseDoc, ["net", "system_proxy"], true)).toThrow();
    expect(() => setPath(baseDoc, ["system_integration", "enable_tun"], true)).toThrow();
    expect(() => setPath(baseDoc, ["system_integration", "autostart"], true)).toThrow();
    // 允许的字段不抛，并且真的写进去了
    const next = setPath(baseDoc, ["gateway", "listen"], "127.0.0.1:9999");
    expect(next.gateway.listen).toBe("127.0.0.1:9999");
    expect(baseDoc.gateway.listen).toBe("127.0.0.1:8787");
  });

  it("高级节展开后：系统网络信息块里连一个控件都没有", async () => {
    renderPage();
    await waitForConfig();

    fireEvent.click(screen.getByTestId("settings-advanced-toggle"));

    const block = screen.getByTestId("settings-net-readonly");
    expect(within(block).queryAllByRole("checkbox")).toEqual([]);
    expect(within(block).queryAllByRole("switch")).toEqual([]);
    expect(within(block).queryAllByRole("button")).toEqual([]);
    expect(within(block).queryAllByRole("combobox")).toEqual([]);
    expect(within(block).queryAllByRole("textbox")).toEqual([]);

    // 而且要把边界写在页面上，用户才知道"这里为什么不能改"
    expect(screen.getByTestId("settings-advanced-i2").textContent).toMatch(/I2/);
  });
});

// ================================================================ ② 开机自启动：不放假开关

describe("[09 §1.2 E1–E5] 开机自启动：命令未注册前不放任何开关", () => {
  it("启动节里没有 checkbox / switch，只有点名了缺失命令的诚实说明", async () => {
    renderPage();
    await waitForConfig();

    const startup = screen.getByTestId("settings-section-startup");
    expect(within(startup).queryAllByRole("switch")).toEqual([]);
    expect(within(startup).queryAllByRole("checkbox")).toEqual([]);

    const blocked = within(startup).getByTestId("settings-autostart-blocked");
    expect(blocked.textContent).toMatch(/app_get_autostart/);
    expect(blocked.textContent).toMatch(/app_set_autostart/);
    // 配置里的值只能被说成"配置期望"，不许被说成"当前系统状态"
    expect(blocked.textContent).toMatch(/配置期望/);
  });

  it("配置里的自启动期望值仍然如实显示（那是 config_get 读到的真值）", async () => {
    renderPage();
    await waitForConfig();
    const startup = screen.getByTestId("settings-section-startup");
    expect(startup.textContent).toMatch(/开机自启动/);
    // 配置里的期望值照样显示，但**带 data-value**，断言的是那个真值本身而不是文案
    const configured = within(startup).getByTestId("settings-autostart-configured");
    expect(configured.getAttribute("data-value")).toBe(
      String(baseDoc.system_integration.autostart),
    );
    expect(configured.textContent).toMatch(/配置里的期望值/);
  });
});

// ================================================================ ③ 没有的命令不画按钮

describe("未注册的命令一律不画按钮，但留一行说明", () => {
  it("日志导出：没有导出按钮，只有点名 log_export 的说明", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByTestId("settings-log-export")).toBeNull();
    expect(screen.getByTestId("settings-no-log-export").textContent).toMatch(/log_export/);
  });

  it("检查更新：没有按钮，只有点名 app_check_update 的说明", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByTestId("settings-check-update")).toBeNull();
    expect(screen.getByTestId("settings-no-check-update").textContent).toMatch(/app_check_update/);
  });

  it("文件监听模式：没有开关，只有点名 config_set_file_watch_mode 的说明", async () => {
    renderPage();
    await waitForConfig();
    const tray = screen.getByTestId("settings-section-tray");
    expect(within(tray).queryAllByRole("switch")).toEqual([]);
    expect(within(tray).getByTestId("settings-file-watch-blocked").textContent).toMatch(
      /config_set_file_watch_mode/,
    );
  });
});

// ================================================================ ④ 错误与诚实

describe("错误显示与诚实边界", () => {
  it("配置读取失败：显示带 TA 码的错误与重试入口", async () => {
    configFails = true;
    renderPage();
    const err = await screen.findByTestId("settings-config-error");
    expect(err.textContent).toMatch(/\[TA-5001\]/);
    expect(screen.getByText("重试")).toBeTruthy();
  });

  it("当前记录模式与保留期：没有读取接口 → 显示「未读取」，另给可读证据", async () => {
    renderPage();
    await waitForConfig();
    const storage = screen.getByTestId("settings-section-storage");
    // 当前值不猜
    expect(storage.textContent).toMatch(/未读取/);
    // 有证据的事实：最近一条日志行的 mode（来自 log_query）
    await waitFor(() =>
      expect(screen.getByTestId("settings-storage-mode-evidence").textContent).toMatch(/meta/),
    );
    // 占用是真读的（log_storage_stats）
    expect(screen.getByTestId("settings-storage-stats").textContent).toMatch(/12,840/);
  });
});

// ================================================================ ⑤ 配置：校验 → 应用（不许假成功）

describe("[配置] 校验通过才应用，校验不通过一次都不调用 config_apply", () => {
  it("校验返回 not ok：应用被阻止，并把字段路径与信息显示出来", async () => {
    validateResult = {
      ok: false,
      errors: [{ path: "channels[2].credential", code: "dup", message: "引用了不存在的凭据" }],
      warnings: [],
    };
    renderPage();
    const listenBox = await waitForConfig();

    // 先制造一处改动，让"应用"处于可用状态
    const debugBox = screen.getByTestId("settings-field-debugHeaders");
    fireEvent.click(within(debugBox).getByRole("checkbox"));
    expect(within(listenBox).getByRole("textbox")).toBeTruthy();

    fireEvent.click(screen.getByTestId("settings-apply"));

    await waitFor(() =>
      expect(screen.getByTestId("settings-validate-report").textContent).toMatch(
        /channels\[2\]\.credential/,
      ),
    );
    expect(screen.getByTestId("settings-validate-report").textContent).toMatch(/引用了不存在的凭据/);
    expect(applyCalls).toEqual([]);
    expect(screen.queryByTestId("settings-apply-result")).toBeNull();
  });

  it("校验返回的形状读不出来：按未通过处理，仍然不应用", async () => {
    validateResult = { nonsense: true };
    renderPage();
    await waitForConfig();
    fireEvent.click(
      within(screen.getByTestId("settings-field-exposeModels")).getByRole("checkbox"),
    );
    fireEvent.click(screen.getByTestId("settings-apply"));

    await waitFor(() =>
      expect(screen.getByTestId("settings-validate-report").textContent).toMatch(/按未通过处理/),
    );
    expect(applyCalls).toEqual([]);
  });

  it("校验通过：应用一次，并如实显示后端返回的 restart / kernel_reload", async () => {
    registerMock("config_apply", (args) => {
      applyCalls.push((args as { doc: ConfigDoc }).doc);
      return { applied: true, restart_required: true, kernel_reload: true };
    });
    renderPage();
    await waitForConfig();
    fireEvent.click(within(screen.getByTestId("settings-field-debugHeaders")).getByRole("checkbox"));
    fireEvent.click(screen.getByTestId("settings-apply"));

    await waitFor(() => expect(applyCalls.length).toBe(1));
    // 送出去的是**草稿**（改过的那一份），不是原样回写
    expect(applyCalls[0]?.gateway.debug_headers).toBe(
      !baseDoc.gateway.debug_headers,
    );
    const result = await screen.findByTestId("settings-apply-result");
    expect(result.textContent).toMatch(/需重启/);
    expect(result.textContent).toMatch(/内核重载/);
  });

  it("本地预检不通过时，连 config_validate 都不发（先让用户改输入）", async () => {
    let validateCalls = 0;
    registerMock("config_validate", () => {
      validateCalls += 1;
      return { ok: true, errors: [], warnings: [] };
    });
    renderPage();
    const listenBox = await waitForConfig();
    fireEvent.change(within(listenBox).getByRole("textbox"), { target: { value: "8787" } });
    fireEvent.click(screen.getByTestId("settings-apply"));

    await waitFor(() =>
      expect(screen.getByTestId("settings-local-issues").textContent).toMatch(/主机:端口/),
    );
    expect(validateCalls).toBe(0);
    expect(applyCalls).toEqual([]);
  });

  it("导出 YAML：显示后端原文；勾上「包含密钥」时给出明确警告", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByTestId("settings-export-output")).toBeNull();

    fireEvent.click(screen.getByTestId("settings-export"));
    const out = await screen.findByTestId("settings-export-output");
    expect(out.textContent).toContain("listen: 127.0.0.1:8787");

    expect(screen.queryByTestId("settings-export-secrets-warn")).toBeNull();
    fireEvent.click(within(screen.getByTestId("settings-field-exportSecrets")).getByRole("checkbox"));
    expect(screen.getByTestId("settings-export-secrets-warn").textContent).toMatch(/别把这个文件/);
    expect(screen.queryByTestId("settings-log-export")).toBeNull();
  });

  it("回滚快照：先确认，未确认前绝不调用 config_rollback", async () => {
    renderPage();
    await waitForConfig();

    const row = await screen.findByTestId("settings-snapshot-row");
    expect(within(row).getByText(/apply/)).toBeTruthy();

    fireEvent.click(within(row).getByTestId("settings-rollback"));
    expect(screen.getByTestId("settings-rollback-confirm")).toBeTruthy();
    expect(rollbackIds).toEqual([]); // 还没确认 → 一次都没调用

    fireEvent.click(screen.getByTestId("settings-rollback-confirm-action"));
    await waitFor(() => expect(rollbackIds).toEqual(["snap-1"]));
  });
});

// ================================================================ ⑥ 模型层（纯函数）

describe("设置页模型层（纯函数）", () => {
  it("isValidListen：只认「主机:端口」，端口必须在 1–65535", () => {
    expect(isValidListen("127.0.0.1:8787")).toBe(true);
    expect(isValidListen(" 0.0.0.0:80 ")).toBe(true);
    expect(isValidListen("127.0.0.1")).toBe(false);
    expect(isValidListen(":8787")).toBe(false);
    expect(isValidListen("127.0.0.1:0")).toBe(false);
    expect(isValidListen("127.0.0.1:70000")).toBe(false);
    expect(isValidListen("127.0.0.1:abc")).toBe(false);
  });

  it("setPath 是不可变的：不改原对象，只返回新对象（React 状态才不会被原地改）", () => {
    const next = setPath(baseDoc, ["request_log", "max_rows"], 1234);
    expect(next).not.toBe(baseDoc);
    expect(next.request_log.max_rows).toBe(1234);
    expect(baseDoc.request_log.max_rows).not.toBe(1234);
    // 深层未被碰到的分支保持引用共享（不整份深拷贝）
    expect(next.gateway).toBe(baseDoc.gateway);
  });

  it("diffPaths：报出叶子路径，相同则空", () => {
    expect(diffPaths(baseDoc, baseDoc)).toEqual([]);
    const next = setPath(setPath(baseDoc, ["gateway", "listen"], "127.0.0.1:1"), [
      "request_log",
      "sample_rate",
    ], 0.5);
    expect(diffPaths(baseDoc, next).sort()).toEqual([
      "gateway.listen",
      "request_log.sample_rate",
    ]);
  });

  it("secretState：未设置 / 引用密钥库 / 明文三态可区分", () => {
    expect(secretState(null)).toBe("unset");
    expect(secretState("")).toBe("unset");
    expect(secretState("   ")).toBe("unset");
    expect(secretState("${secret:gateway-local-key}")).toBe("reference");
    expect(secretState("sk-plain-text")).toBe("inline");
  });

  it("validateDraft：本地预检只挡明显非法的输入，路径精确", () => {
    const bad = setPath(
      setPath(setPath(baseDoc, ["gateway", "admin_listen"], "nope"), [
        "request_log",
        "sample_rate",
      ], 3),
      ["system_integration", "autostart_delay_s"],
      900,
    );
    const issues = validateDraft(bad).map((i) => i.path);
    expect(issues).toContain("gateway.admin_listen");
    expect(issues).toContain("request_log.sample_rate");
    expect(issues).toContain("system_integration.autostart_delay_s");
    expect(validateDraft(baseDoc)).toEqual([]);
  });

  it("后端返回的宽容读取：读不出来就不算成功", () => {
    expect(readValidationReport(undefined).ok).toBe(false);
    expect(readValidationReport({ nonsense: 1 }).recognizable).toBe(false);
    expect(readValidationReport({ ok: true, errors: [], warnings: [] }).ok).toBe(true);
    const partial = readValidationReport({ ok: false, errors: [{ path: "a.b" }] });
    expect(partial.errors[0]).toEqual({ path: "a.b", code: "", message: "" });

    expect(readApplyResult(undefined).applied).toBe(false);
    expect(readApplyResult({ applied: "yes" }).applied).toBe(false);
    expect(readApplyResult({ applied: true, restart_required: true }).applied).toBe(true);

    expect(readSnapshots(undefined)).toEqual([]);
    expect(readSnapshots([{ id: "s1" }])[0]?.id).toBe("s1");

    const stats = readStorageStats({ rows: 10, bytes: 2048, oldest_ts: 5 });
    expect(stats?.rows).toBe(10);
    expect(stats?.newestTs).toBeUndefined();

    expect(readNewestRow({ rows: [{ mode: "full", ts_ms: 7 }] })).toEqual({
      mode: "full",
      tsMs: 7,
    });
    expect(readNewestRow({ rows: [] })).toEqual({});
    expect(readNewestRow(undefined)).toEqual({});
  });
});

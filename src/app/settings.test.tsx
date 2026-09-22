// @vitest-environment jsdom
/**
 * 设置页（[07 §7] / [09 §1.2 E1–E5]）的验收。重点不是"渲染出来了"，而是**四条硬边界**：
 *
 * 1. **I2：只展示系统网络信息，绝不提供改系统设置的入口。**
 *    这一条在这里是**机械检查**（不是靠自觉）：`EDITABLE_PATHS` 与 `FORBIDDEN_PATHS` 不许有交集，
 *    `setPath()` 对禁止字段**抛错**，并且高级那一节的只读块里**一个控件都没有**
 *    （checkbox / switch / button 全为空）。
 * 2. **系统事实与配置期望不许混为一谈。** 开机自启动的开关绑的是 `app_get_autostart` 的
 *    `actual`（系统事实），不是配置里的 `desired`；两者不一致时要说"漂移"并给"修复"入口。
 * 3. **点不动就如实说。** 后端命令都已注册（`command_surface_guard` 守着），但日志导出
 *    （log_export）与文件监听（config_set_file_watch_mode）这一页**还没接** → 不许有按钮，
 *    但必须留一行说明，否则下一个人会以为"忘了做"。
 * 4. **不许假成功。** 后端校验说 not ok 时，`config_apply` **一次都不许被调用**；
 *    校验结果读不出来也按"未通过"处理，不当作成功。检查更新只在点了之后才发请求，
 *    后端报错就显示错误（`TA-xxxx`），绝不改口说"已是最新"。
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
import type { AutostartState, ConfigDoc, UpdateInfo } from "@/shared/bindings";
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

/** 基线自启动状态（从 mock 后端读一次真值；mock 故意给"配置开、系统里没有"的漂移样例）。 */
let baseAutostart: AutostartState;

/** `app_get_autostart` 当前返回的系统事实（自启动用例里会被改写并重新读）。 */
let autostartNow: AutostartState;
/** `app_get_autostart` 是否失败（测"读不到就如实报错，不画开关"）。 */
let autostartFails = false;
/** `app_set_autostart` 收到的参数（证据：开关真的写系统事实，而不是只改配置）。 */
let autostartWrites: Array<Record<string, unknown>> = [];
/** `app_repair_autostart` 被调用了几次。 */
let repairCalls = 0;

/** `app_check_update` 被调用了几次（证据："点了才发"）。 */
let checkUpdateCalls = 0;
/** `app_check_update` 的返回值或要抛的错误。 */
let updateResult: UpdateInfo | null = null;
let updateError: Error | null = null;

/** `config_set_file_watch_mode` 收到的参数（证据：真的写了这份配置）。 */
let fileWatchCalls: Array<Record<string, unknown>> = [];
/** 写文件监听开关的返回（后端恒定回 restart_required=true，这里可换成别的来验"不猜"）。 */
let fileWatchResult = { applied: true, restart_required: true, kernel_reload: false };
let fileWatchError: Error | null = null;
/**
 * `config_get` 现在返回的文档。**只有写成功后才会变** ——
 * 用来验"开关跟的是配置真值，不是本地状态"（写了以后重读必须看得见）。
 */
let configNow: ConfigDoc | undefined = undefined;

const EXPORTED_YAML = "version: 1\ngateway:\n  listen: 127.0.0.1:8787\n";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
  registerBaselineMocks();
  baseDoc = await invokeCommand("config_get", undefined);
  baseAutostart = await invokeCommand("app_get_autostart", undefined);
});

beforeEach(() => {
  configFails = false;
  applyCalls = [];
  rollbackIds = [];
  validateResult = { ok: true, errors: [], warnings: [] };
  autostartNow = baseAutostart;
  autostartFails = false;
  autostartWrites = [];
  repairCalls = 0;
  checkUpdateCalls = 0;
  updateResult = null;
  updateError = null;
  fileWatchCalls = [];
  fileWatchResult = { applied: true, restart_required: true, kernel_reload: false };
  fileWatchError = null;
  configNow = undefined;

  registerMock("config_get", () => {
    if (configFails) {
      throw new TokenAgencyError({ code: "TA-5001", message: "配置读取失败（测试夹具）" });
    }
    return configNow ?? baseDoc;
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
  // 开机自启动：读的是**系统事实**；写/修复都会真的改这份事实，所以重新读能看见变化
  registerMock("app_get_autostart", () => {
    if (autostartFails) {
      throw new TokenAgencyError({ code: "TA-5002", message: "自启动状态读取失败（测试夹具）" });
    }
    return autostartNow;
  });
  registerMock("app_set_autostart", (args) => {
    const write = args as { enabled: boolean; mode: AutostartState["mode"]; delay_s: number };
    autostartWrites.push({ ...write });
    autostartNow = { ...autostartNow, desired: write.enabled, actual: write.enabled, drifted: false };
    return autostartNow;
  });
  registerMock("app_repair_autostart", () => {
    repairCalls += 1;
    autostartNow = { ...autostartNow, actual: autostartNow.desired, drifted: false };
    return autostartNow;
  });
  // 检查更新：默认"没有可用更新信息"（null）；要点过才发
  registerMock("app_check_update", () => {
    checkUpdateCalls += 1;
    if (updateError !== null) throw updateError;
    return updateResult;
  });
  // 文件监听模式：写的是配置；后端恒定回 restart_required=true（监听器本身还没实现）
  registerMock("config_set_file_watch_mode", (args) => {
    const write = args as { enabled: boolean; path?: string };
    fileWatchCalls.push({ ...write });
    if (fileWatchError !== null) throw fileWatchError;
    // **只有 applied=true 才算写进去了**：写进去后 config_get 应当回读出新值
    if (fileWatchResult.applied) {
      const doc = configNow ?? baseDoc;
      configNow = {
        ...doc,
        system_integration: { ...doc.system_integration, file_watch_mode: write.enabled },
      };
    }
    return fileWatchResult;
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

// ================================================================ ② 开机自启动：开关 = 系统事实

describe("[09 §1.2 E1–E5] 开机自启动：开关绑系统事实，漂移可修复", () => {
  /** 夹具自证：mock 给的样例必须真的漂移，否则下面"开关绑 actual 而不是 desired"分不出来。 */
  it("自证：夹具是漂移样例（配置开、系统里没有）", () => {
    expect(baseAutostart.desired).toBe(true);
    expect(baseAutostart.actual).toBe(false);
    expect(baseAutostart.drifted).toBe(true);
  });

  it("开关绑的是系统事实 actual，不是配置里的期望值 desired", async () => {
    renderPage();
    await waitForConfig();

    const actual = await screen.findByTestId("settings-autostart-actual");
    expect(actual.getAttribute("data-value")).toBe(String(baseAutostart.actual));

    const desired = screen.getByTestId("settings-autostart-desired");
    expect(desired.getAttribute("data-value")).toBe(String(baseAutostart.desired));
    // 两个值确实不同（自证这条断言有意义）
    expect(baseAutostart.actual).not.toBe(baseAutostart.desired);

    // 开关 = 系统事实（false → 未选中）
    const box = screen.getByTestId("settings-autostart-switch");
    const toggle = within(box).getByRole("switch") as HTMLInputElement;
    expect(toggle.checked).toBe(baseAutostart.actual);
    expect(toggle.disabled).toBe(false);

    // 漂移必须被说出来，并给出修复入口
    expect(screen.getByTestId("settings-autostart-drifted")).toBeTruthy();
    expect(screen.getByTestId("settings-autostart-repair")).toBeTruthy();
  });

  it("拨开关真的调 app_set_autostart，参数是 snake_case 的 enabled / mode / delay_s", async () => {
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-autostart-switch");
    fireEvent.click(within(box).getByRole("switch"));

    await waitFor(() => expect(autostartWrites.length).toBe(1));
    // 契约形状（00 §3 的 args）：字段名必须是 snake_case，一个不许少、不许改名
    expect(Object.keys(autostartWrites[0]!).sort()).toEqual(["delay_s", "enabled", "mode"]);
    expect(autostartWrites[0]).toEqual({
      enabled: true,
      mode: baseAutostart.mode,
      delay_s: baseAutostart.delay_s,
    });

    // 写的是系统事实：重新读之后开关变开、漂移消失
    await waitFor(() =>
      expect(screen.getByTestId("settings-autostart-actual").getAttribute("data-value")).toBe(
        "true",
      ),
    );
    await waitFor(() => expect(screen.queryByTestId("settings-autostart-drifted")).toBeNull());
  });

  it("点「修复自启动」调 app_repair_autostart；修好后漂移提示消失、不再有修复按钮", async () => {
    renderPage();
    await waitForConfig();

    fireEvent.click(await screen.findByTestId("settings-autostart-repair"));

    await waitFor(() => expect(repairCalls).toBe(1));
    await waitFor(() => expect(screen.queryByTestId("settings-autostart-drifted")).toBeNull());
    expect(screen.queryByTestId("settings-autostart-repair")).toBeNull();
    await waitFor(() =>
      expect(screen.getByTestId("settings-autostart-actual").getAttribute("data-value")).toBe(
        "true",
      ),
    );
  });

  it("系统状态读不到：如实报错（带 TA 码），并且一个开关都不画", async () => {
    autostartFails = true;
    renderPage();
    await waitForConfig();

    const err = await screen.findByTestId("settings-autostart-error");
    expect(err.textContent).toMatch(/\[TA-5002\]/);
    expect(screen.queryByTestId("settings-autostart-switch")).toBeNull();
    expect(screen.queryByTestId("settings-autostart-drifted")).toBeNull();
  });

  it("平台不支持：开关被禁用（按不动，也不会偷偷写系统），并说明原因", async () => {
    autostartNow = {
      ...baseAutostart,
      mechanism: "unsupported",
      unsupported_reason: "本平台没有自启动注册机制（测试夹具）",
    };
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-autostart-switch");
    const toggle = within(box).getByRole("switch") as HTMLInputElement;
    expect(toggle.disabled).toBe(true);

    fireEvent.click(toggle);
    expect(autostartWrites).toEqual([]);

    expect(screen.getByTestId("settings-autostart-unsupported").textContent).toMatch(
      /本平台没有自启动注册机制/,
    );
  });
});

// ================================================================ ③ 还没接的命令不画按钮

describe("还没接的命令一律不画按钮，但留一行说明", () => {
  it("日志导出：本页没有导出按钮（导出在日志页），只有一行点名 log_export 的指向说明", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByTestId("settings-log-export")).toBeNull();
    expect(screen.getByTestId("settings-no-log-export").textContent).toMatch(/log_export/);
  });
});

// ================================================================ ③″ 文件监听模式：写配置、只可能"需要重启"

describe("[配置] 文件监听模式：真写配置，结论只可能「需要重启」", () => {
  it("开关绑的是配置真值（config_get 读回来的 file_watch_mode），不是本地临时状态", async () => {
    renderPage();
    await waitForConfig();

    const expected = String(baseDoc.system_integration.file_watch_mode === true);
    const configRow = await screen.findByTestId("settings-file-watch-config");
    expect(configRow.getAttribute("data-value")).toBe(expected);

    const box = screen.getByTestId("settings-file-watch-switch");
    const toggle = within(box).getByRole("switch") as HTMLInputElement;
    expect(toggle.checked).toBe(baseDoc.system_integration.file_watch_mode === true);
    // 还没点过：不许显示任何结果
    expect(screen.queryByTestId("settings-file-watch-result")).toBeNull();
  });

  it("拨开关真的调 config_set_file_watch_mode（参数是 enabled），并且只说「需要重启」", async () => {
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-file-watch-switch");
    fireEvent.click(within(box).getByRole("switch"));

    await waitFor(() => expect(fileWatchCalls.length).toBe(1));
    // 契约参数名是 snake_case 的 enabled（path 是可选的，配置里没有就不发）
    expect(Object.keys(fileWatchCalls[0]!).sort()).toEqual(["enabled"]);
    expect(fileWatchCalls[0]).toEqual({ enabled: true });

    const result = await screen.findByTestId("settings-file-watch-result");
    expect(result.getAttribute("data-applied")).toBe("true");
    expect(result.getAttribute("data-restart-required")).toBe("true");
    expect(result.textContent).toMatch(/需要重启/);
    // **硬边界**：监听器本身还没实现 → 整节任何地方都不许说"已生效/正在监听"
    const tray = screen.getByTestId("settings-section-tray");
    expect(tray.textContent).not.toMatch(/已生效/);
    expect(tray.textContent).not.toMatch(/正在监听/);
  });

  it("写成功之后：开关与「配置里的值」都跟着**重读到的配置**走（不是本地状态）", async () => {
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-file-watch-switch");
    expect((within(box).getByRole("switch") as HTMLInputElement).checked).toBe(false);

    fireEvent.click(within(box).getByRole("switch"));
    await waitFor(() => expect(fileWatchCalls.length).toBe(1));

    // 库里已经是 true 了 → 重读之后开关必须变 true、值行也要变
    await waitFor(() =>
      expect(screen.getByTestId("settings-file-watch-config").getAttribute("data-value")).toBe(
        "true",
      ),
    );
    expect(
      (
        within(screen.getByTestId("settings-file-watch-switch")).getByRole(
          "switch",
        ) as HTMLInputElement
      ).checked,
    ).toBe(true);
  });

  it("后端回 applied=false：如实说没有写入，不说「已写入」", async () => {
    fileWatchResult = { applied: false, restart_required: false, kernel_reload: false };
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-file-watch-switch");
    fireEvent.click(within(box).getByRole("switch"));

    const result = await screen.findByTestId("settings-file-watch-result");
    expect(result.getAttribute("data-applied")).toBe("false");
    expect(result.textContent).toMatch(/没有写入/);
    expect(result.textContent).not.toMatch(/已写入/);
  });

  it("写失败：原样显示错误码（唯一格式化出口 errorMessage）", async () => {
    fileWatchError = new TokenAgencyError({ code: "TA-7001", message: "写配置失败（测试夹具）" });
    renderPage();
    await waitForConfig();

    const box = await screen.findByTestId("settings-file-watch-switch");
    fireEvent.click(within(box).getByRole("switch"));

    const err = await screen.findByTestId("settings-file-watch-error");
    expect(err.textContent).toMatch(/\[TA-7001\]/);
    expect(screen.queryByTestId("settings-file-watch-result")).toBeNull();
  });
});

// ================================================================ ③′ 检查更新：点了才发，失败不装最新

describe("[10 §2.2] 检查更新：只在点了之后发请求，结果如实说", () => {
  it("没点之前一个请求都不发，也不显示任何结果", async () => {
    renderPage();
    await waitForConfig();

    expect(checkUpdateCalls).toBe(0);
    expect(screen.queryByTestId("settings-update-none")).toBeNull();
    expect(screen.queryByTestId("settings-update-result")).toBeNull();
    expect(screen.queryByTestId("settings-update-error")).toBeNull();
    expect(screen.getByTestId("settings-update-run")).toBeTruthy();
  });

  it("后端返回 null：说「没有可用的更新信息」，不说「已是最新」", async () => {
    updateResult = null;
    renderPage();
    await waitForConfig();

    fireEvent.click(screen.getByTestId("settings-update-run"));

    const none = await screen.findByTestId("settings-update-none");
    expect(checkUpdateCalls).toBe(1);
    expect(none.textContent).not.toMatch(/已是最新/);
    expect(screen.queryByTestId("settings-update-result")).toBeNull();
    expect(screen.queryByTestId("settings-update-error")).toBeNull();
  });

  it("后端报错：原样显示错误码，绝不改口说「没有新版本」", async () => {
    updateError = new TokenAgencyError({ code: "TA-6001", message: "更新通道未接入（测试夹具）" });
    renderPage();
    await waitForConfig();

    fireEvent.click(screen.getByTestId("settings-update-run"));

    const err = await screen.findByTestId("settings-update-error");
    expect(err.textContent).toMatch(/\[TA-6001\]/);
    expect(err.textContent).toMatch(/更新通道未接入/);
    expect(screen.queryByTestId("settings-update-none")).toBeNull();
    expect(screen.queryByTestId("settings-update-result")).toBeNull();
  });

  it("后端给了新版本：版本号 / 发行说明 / 下载地址都要显示出来", async () => {
    updateResult = { version: "1.4.2", notes: "修了自启动漂移", url: "https://example.invalid/dl" };
    renderPage();
    await waitForConfig();

    fireEvent.click(screen.getByTestId("settings-update-run"));

    const result = await screen.findByTestId("settings-update-result");
    expect(result.textContent).toMatch(/1\.4\.2/);
    expect(result.textContent).toMatch(/修了自启动漂移/);
    expect(result.textContent).toMatch(/https:\/\/example\.invalid\/dl/);
    expect(screen.queryByTestId("settings-update-none")).toBeNull();
    expect(screen.queryByTestId("settings-update-error")).toBeNull();
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

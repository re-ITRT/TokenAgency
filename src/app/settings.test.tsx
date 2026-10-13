// @vitest-environment jsdom
/**
 * 设置页（/）的验收。重点不是"渲染出来了"，而是**四条硬边界**：
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

import SettingsPage, { sectionHelpMode } from "@/pages/settings";
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
import { zhCN, type I18nKey } from "@/shared/i18n/zh-CN";
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

  it("校验并应用固定在底部（长页面 1838 行）", () => {
    renderPage();
    const bar = document.querySelector('[data-testid="settings-apply-bar"]');
    expect(bar).toBeTruthy();
    expect(bar?.querySelector('[data-testid="settings-apply"]')).toBeTruthy();
    const css = Array.from(document.querySelectorAll("style"))
      .map((s) => s.textContent ?? "")
      .join("");
    expect(css).toMatch(/position:\s*sticky/);
  });
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

  it("网络/内核那一块已从设置页**移除**：整块（含展开按钮/I2 说明）都不存在", async () => {
    renderPage();
    await waitForConfig();

    // 用户要求「设置界面的向导关掉」（第 2 条）：内核路径改由 /network 直接编辑，
    // 所以设置页里不再有这一块，连"展开"这种入口都不留。
    expect(screen.queryByTestId("settings-section-advanced")).toBeNull();
    expect(screen.queryByTestId("settings-advanced-toggle")).toBeNull();
    expect(screen.queryByTestId("settings-net-readonly")).toBeNull();
    expect(screen.queryByTestId("settings-advanced-i2")).toBeNull();

    // 自证：页面真的渲染完了 —— 否则"找不到"可能只是因为整页还没出来
    expect(screen.getByTestId("settings-page")).toBeTruthy();
    expect(screen.getByTestId("settings-section-service")).toBeTruthy();
  });

  it("设置页里不再出现「内核路径」这一行（它现在归 /network 页直接编辑）", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByText("内核路径")).toBeNull();
  });
});

// ================================================================ ② 开机自启动：开关 = 系统事实

describe("开机自启动：开关绑系统事实，漂移可修复", () => {
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

    /*
     * [极简 · 判据 1] 「配置里的期望值：X」那一行**已删**（期望值不是用户要看的东西：
     * 它只在"开关与系统不一致"时有意义，而那种情况由 `settings-autostart-drifted` 那一句
     * 加「修复自启动」按钮表达）。这里改成钉住**它不许回来**，并保留原来的夹具自证。
     */
    expect(screen.queryByTestId("settings-autostart-desired")).toBeNull();
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
  it("日志导出：本页没有导出按钮（导出在日志页），只有一行指向「日志页」的说明", async () => {
    renderPage();
    await waitForConfig();
    expect(screen.queryByTestId("settings-log-export")).toBeNull();
    // [极简 · 判据 1] 原来这一行点名了写命令 `log_export`（函数名不上界面）；
    // 现在它只说"导出在「日志页」"—— 恢复路径一个字没少（去哪里找那个按钮）。
    expect(screen.getByTestId("settings-no-log-export").textContent).toMatch(/日志页/);
  });
});

// ================================================================ ③″ 文件监听模式：写配置、只可能"需要重启"

describe("[配置] 文件监听模式：真写配置，结论只可能「需要重启」", () => {
  it("开关绑的是配置真值（config_get 读回来的 file_watch_mode），不是本地临时状态", async () => {
    renderPage();
    await waitForConfig();

    /*
     * [极简 · 判据 1「同一屏里同一个事实不许说两遍」] 开关旁边那行「配置里的值：开/关」已删除 ——
     * 它读的是同一个 `draft.system_integration.file_watch_mode`，也就是开关自己的位置。
     * 事实的来源一个都没少（开关 + 它的名字仍在版面上），所以这条断言改读开关，
     * 并在这里钉住**那行字不许回来**。
     */
    expect(screen.queryByTestId("settings-file-watch-config")).toBeNull();

    const box = await screen.findByTestId("settings-file-watch-switch");
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

  it("写成功之后：开关跟着**重读到的配置**走（不是本地状态）", async () => {
    renderPage();
    await waitForConfig();

    // 开关是受控的：点它本身不改任何本地状态，mock 也只改 `config_get` 的返回值 ——
    // 所以它从 false 变 true 只可能来自"写完 → 失效 qk.config → 回读 → 草稿更新"这条链。
    // （原来这条链的第二个证人那行「配置里的值」已按判据 1 删除，见上面那条断言。）
    const toggle = () =>
      within(screen.getByTestId("settings-file-watch-switch")).getByRole(
        "switch",
      ) as HTMLInputElement;
    expect(toggle().checked).toBe(false);

    fireEvent.click(toggle());
    await waitFor(() => expect(fileWatchCalls.length).toBe(1));

    await waitFor(() => expect(toggle().checked).toBe(true));
    // 删掉的那行不许回来（它当年就是开关位置的复述）
    expect(screen.queryByTestId("settings-file-watch-config")).toBeNull();
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

describe("检查更新：只在点了之后发请求，结果如实说", () => {
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
    // `settings-field-exposeModels` 这个死开关已删除；改用仍然存在的
    // `settings-field-debugHeaders` 制造"草稿被改过"的局面（本用例只关心"校验形状读不出来"）。
    fireEvent.click(within(screen.getByTestId("settings-field-debugHeaders")).getByRole("checkbox"));
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

// ================================================================ ⑦ 长篇说明收进「问号圆圈」

/**
 * 的机械验收：
 *
 * - 区块说明 **> 12 字符** → 收进 `<区块 testid>-help`，`aria-label` 就是全文，版面上不再有那段字；
 * - 一个区块**最多一个**问号（§3：一排问号比一排字更糟）；
 * - §2 的红线（读不到 / 未读取 / 校验失败原文 / 破坏性后果 / 数值）**仍然内联** ——
 *   把错误信息塞进 tooltip 是"藏问题"，不是"排版"。
 */
describe("长篇说明收进问号圆圈，红线留在版面上", () => {
  /**
   * 每个区块只收一条"这一段是干什么的"的说明。
   *
   * 键是主管的所有权范围（i18n 不许写者动），这里**只换渲染位置**；正文一个字都没丢 ——
   * 它同时是问号圆圈的 `aria-label`。
   */
  const SECTION_DESCRIPTIONS: readonly (readonly [string, I18nKey])[] = [
    ["settings-section-service", "settings.service.editHint"],
    ["settings-section-startup", "settings.startup.desired.hint"],
    ["settings-section-storage", "logs.storage.scope"],
    ["settings-section-config", "settings.config.localFirst"],
    ["settings-section-about", "settings.about.checkUpdate.hint"],
  ];

  it("> 12 字符的区块说明收进 `${testid}-help`：正文不再内联，aria-label 就是全文", async () => {
    renderPage();
    await waitForConfig();

    for (const [section, key] of SECTION_DESCRIPTIONS) {
      const text = zhCN[key];
      // 自证：这些确实是"长说明"（否则这条断言会落在 ≤12 的内联分支上假绿）
      expect(text.length, key).toBeGreaterThan(12);

      const help = screen.getByTestId(`${section}-help`);
      /*
       * `aria-label` 的**第一行**逐字等于这段区块说明（"一个字都没丢"）：
       * 同一个问号里还可能并着被收进来的解释（用 `\n` 连在**后面**，见下一条用例），
       * 所以这里按行取第一行判等 —— 既不许改写这段说明，也不许把它挪出第一行。
       */
      expect(help.getAttribute("aria-label")?.split("\n")[0], key).toBe(text);
      // 版面上**不再**有那段正文
      expect(screen.queryByText(text), key).toBeNull();
      // §3：同一区块最多一个问号
      expect(within(screen.getByTestId(section)).getAllByTestId(/-help$/), section).toHaveLength(1);
    }
  });

  it("≤ 12 字符的说明留在版面上（一行短字不值得再画一个问号）", () => {
    expect(sectionHelpMode("只读")).toBe("inline");
    expect(sectionHelpMode("来自配置")).toBe("inline");
    expect(sectionHelpMode("")).toBe("inline");
    expect(sectionHelpMode("这一组的输入框改的是本地草稿")).toBe("help");
  });

  /*
   * [极简 · 判据 2「长解释一律收进问号」] 服务区块**两段非红线解释**并进同一个问号：
   * ① `settings.service.localKey.hint`（62 字：这个密钥是什么 + 用下面哪个按钮）；
   * ② `settings.service.downstream.hint`（34 字：勾选的作用）—— 它是一句成句的说明，
   *    而三个开关的名字本身已经说清"勾什么"，所以按判据 2 搬进 tooltip。
   *
   * 为什么断言写得这么死：收进问号最容易两头错 —— ① 只删不搬（读屏/悬停的人都拿不到了）；
   * ② 顺手把控件一起删了（"极简"变成"缺功能"）。所以这里同时钉住：
   * 提示里逐字可读、版面上没有、密钥状态行与两个按钮、三个下游开关都还在。
   */
  it("[判据 2] 服务区块的两段说明整段收进同一个问号：版面上一字不剩，提示里一字不少", async () => {
    renderPage();
    await waitForConfig();

    const hint = zhCN["settings.service.localKey.hint"];
    const downstream = zhCN["settings.service.downstream.hint"];
    // 自证：这两条确实是"长解释"（否则收不收都没有区别，这条断言就白写了）
    expect(hint.length).toBeGreaterThan(40);
    expect(downstream.length).toBeGreaterThan(12);

    const help = screen.getByTestId("settings-section-service-help");
    // 同一个问号里三句：区块说明 + 两段收进来的解释，用 `\n` 分隔，顺序不换
    expect(help.getAttribute("aria-label")).toBe(
      `${zhCN["settings.service.editHint"]}\n${hint}\n${downstream}`,
    );
    // 版面上**不再**有这两段解释（否则等于没收）
    expect(screen.queryByText(hint)).toBeNull();
    expect(screen.queryByText(downstream)).toBeNull();
    // 功能没被一起删：状态行 + 两个按钮仍在版面上
    expect(screen.getByText(zhCN["settings.service.localKey"])).toBeTruthy();
    expect(screen.getByText(zhCN["settings.service.accessInfo.show"])).toBeTruthy();
    expect(screen.getByText(zhCN["settings.service.copyAccessInfo"])).toBeTruthy();
    // 三个下游开关**一个都没少**（说明进了 tooltip，不是控件被删）
    for (const testId of [
      "settings-field-downstreamOpenaiChat",
      "settings-field-downstreamOpenaiResponses",
      "settings-field-downstreamAnthropicMessages",
    ]) {
      expect(screen.getByTestId(testId), testId).toBeTruthy();
    }
  });

  /*
   * [极简 · 判据 1「同一屏里同一个事实不许说两遍」] 记录模式与保留期都读不到时，
   * 原来两行各带一句一模一样的理由（"后端只有设置接口、没有读取接口，这里不猜"）。
   * 现在理由留在**保留期**那行（它永远是"未读取"），模式那行只留结论。
   * 红线的结论一个字没少，所以下面同时断言两行都还在版面上。
   */
  it("[判据 1] 「没有读取接口、这里不猜」这个理由在同一节里只说一遍，两行结论都还在", async () => {
    renderPage();
    await waitForConfig();

    const storage = screen.getByTestId("settings-section-storage");
    const reason = "没有读取接口";
    // 自证夹具：两行都处在"未读取"状态（否则理由本来就不会出现，计数为 0 也算绿）
    expect(screen.getByText(/^当前模式：未读取/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：未读取/)).toBeTruthy();
    // split 的长度 = 出现次数 + 1
    expect((storage.textContent ?? "").split(reason).length - 1).toBe(1);
    // 留在这里的那一遍是完整的一句（理由没有被截断成半句）
    expect(storage.textContent).toMatch(/没有读取接口，这里不猜/);
  });

  it("红线不许收：读不到 / 未读取 / 破坏性后果 / 数值仍然一眼可见", async () => {
    autostartFails = true;
    renderPage();
    await waitForConfig();

    // ① 读不到：后端原文（含 TA 码）在版面上
    expect(screen.getByTestId("settings-autostart-error").textContent).toMatch(/\[TA-5002\]/);
    // ② 状态结论：没有读取接口就是"未读取"，不许藏进提示
    expect(screen.getByText(/^当前模式：未读取/)).toBeTruthy();
    expect(screen.getByText(/^当前保留期：未读取/)).toBeTruthy();
    // ③ 破坏性操作的后果：清空不可恢复（点了才弹，但弹出来必须一眼看到）
    fireEvent.click(screen.getByTestId("settings-clear-open"));
    expect(screen.getByText(/清空不可恢复/)).toBeTruthy();
    // ④ 数值：占用是真读回来的数字，留在版面上
    expect(screen.getByTestId("settings-storage-stats").textContent).toMatch(/12,840/);
  });

  it("带破坏性后果的说明不整段收：文件监听那句里写着「会覆盖文件里的改动」", async () => {
    renderPage();
    await waitForConfig();

    // §2 第 4 条（破坏性操作的后果）优先于 §2 的"超 12 字符就收" ——
    // 这一节里 [settings.tray.fileWatch.hint] 含"写回配置会覆盖文件里的改动"，所以它整段留在版面上，
    // 托盘那一块也就**一个问号都没有**（§3 的"同一区块最多一个"是上限，不是配额）。
    const tray = screen.getByTestId("settings-section-tray");
    expect(screen.getByText(zhCN["settings.tray.fileWatch.hint"])).toBeTruthy();
    expect(within(tray).queryAllByTestId(/-help$/)).toHaveLength(0);
  });

  it("配置读不到的说明仍然内联（那是错误处理路径的一部分，不是简介）", async () => {
    configFails = true;
    renderPage();
    await screen.findByTestId("settings-config-error");
    expect(screen.getByText(zhCN["settings.config.loadFailedHint"])).toBeTruthy();
  });
});

// ================================================================ 一级界面省略

describe("快照「标签 · id」：省略后全文仍在 DOM", () => {
  it("长标签整段留在 DOM 里；没溢出时一个提示都不挂（回滚确认框里还有同一个 id）", async () => {
    const label = "apply · 生产环境 · 手动改过网关监听与日志上限的那一次";
    const id = "snap-2026-07-18T10-00-00Z-long-id";
    registerMock("config_history_list", () => [
      { id, label, created_at_ms: 1_770_000_000_000, size_bytes: 2048 },
    ]);
    renderPage();
    await waitForConfig();

    const element = await screen.findByTestId("settings-snapshot-label-ellipsis");
    expect(element.textContent).toBe(`${label} · ${id}`);
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(element.getAttribute("tabindex")).toBeNull();

    // 恢复路径之一：回滚确认框里带着同一个 id（不是"藏起来没有出口"）
    fireEvent.click(screen.getByTestId("settings-rollback"));
    expect(screen.getByTestId("settings-rollback-confirm").textContent).toContain(id);
  });
});

/**
 * **下游 API 家族开关**（用户第 326 轮需求 2）。
 *
 * 用户原话："可以在设置页面选择我们下游是统一成 OPENAI 的两种还是 Anthropic 的"，
 * 补一句"是打开关闭形式，按需求勾选"。所以这里钉的是：
 *
 * ① 三个开关**都在**（不是"选一个"的下拉框）；
 * ② 默认**全开**（升级不改变既有行为）；
 * ③ 勾掉一个之后，**只有那一个**变成 false 并真的进了 `config_apply` 的文档 ——
 *    "界面上点了"和"配置里改了"是两件事，这条把它们钉在一起。
 */
describe("[配置] 下游 API 家族是三个独立开关，勾掉的才写进配置", () => {
  it("三个开关都渲染、默认全开，勾掉其中一个只改那一个", async () => {
    renderPage();
    await waitForConfig();

    const chat = screen.getByTestId("settings-field-downstreamOpenaiChat");
    const responses = screen.getByTestId("settings-field-downstreamOpenaiResponses");
    const messages = screen.getByTestId("settings-field-downstreamAnthropicMessages");

    // 没有 jest-dom：直接读原生 `checked`
    const isChecked = (box: HTMLElement) =>
      (within(box).getByRole("checkbox") as HTMLInputElement).checked;

    // ② 默认全开
    for (const box of [chat, responses, messages]) {
      expect(isChecked(box)).toBe(true);
    }

    // ③ 勾掉 Anthropic Messages
    fireEvent.click(within(messages).getByRole("checkbox"));
    expect(isChecked(messages)).toBe(false);
    // 另外两个**不受影响**（三个是独立的，不是"选一个"）
    expect(isChecked(chat)).toBe(true);
    expect(isChecked(responses)).toBe(true);

    fireEvent.click(screen.getByTestId("settings-apply"));
    await waitFor(() => expect(applyCalls.length).toBeGreaterThan(0));
    const applied = applyCalls.at(-1)!.gateway.downstream;
    expect(applied.anthropic_messages).toBe(false);
    expect(applied.openai_chat).toBe(true);
    expect(applied.openai_responses).toBe(true);
  });
});

#!/usr/bin/env node
/**
 * TokenAgency 真机端到端冒烟：**tauri-driver + msedgedriver 驱动真实 Tauri 窗口**。
 *
 * 这个脚本证明的不是"命令能调通"，而是"用户能在界面里点出来"：
 *   真窗口 → 真 DOM → 真点击/真输入 → Tauri IPC → 真 SQLite。
 *
 * 三条不容妥协的纪律（读代码前先读这三条）：
 *
 * 1. **绝不碰用户真实数据目录**。App 的数据根由 `TA_DATA_DIR` 覆盖（唯一解析口：
 *    `src-tauri/src/state.rs::AppState::app_data_root`）。本脚本把它设进
 *    **tauri-driver 进程的环境**（App 是它的子进程，直接继承），指向
 *    `%TEMP%\ta-e2e\data`。跑完会断言：
 *      - scratch 里真的出现了 `db.sqlite`（隔离口生效，App 用的是它）；
 *      - `%APPDATA%\TokenAgency` 的文件清单+mtime+size（db 另加 sha256）逐条未变。
 *    第二半是**有牙的**：如果覆盖口失效，App 会往真实目录写，断言当场红。
 *
 * 2. **SKIP 不是 PASS**。每一步先探测"界面入口存在吗"。入口不存在就记 SKIP 并
 *    打印**它找了哪些 selector**，绝不用别的路径冒充这一步成功。
 *    也**绝不**用 execute script 直接 `invoke(...)` 假装是界面操作 ——
 *    只有"导航到某个 hash 路由"允许用 script（任务书明文允许），
 *    任何写操作必须由 WebDriver 的元素点击 + 元素输入完成。
 *
 * 3. **原始证据落盘**。每个 WebDriver 请求/响应原文写到
 *    `%TEMP%\ta-e2e\webdriver\NNN-*.json`，每步截图到 `%TEMP%\ta-e2e\shots\`，
 *    db 查询结果到 `%TEMP%\ta-e2e\db\`。报告只写**实际看到**的东西。
 *
 * 用法：
 *   node scripts/e2e/smoke.mjs
 *   node scripts/e2e/smoke.mjs --exe "C:\path\to\tokenagency.exe"
 *   node scripts/e2e/smoke.mjs --keep-data        # 不清理上次的 scratch
 *   node scripts/e2e/smoke.mjs --only step2,step3 # 只跑指定步骤（调试用）
 *
 * 退出码：0 = 没有 FAIL（SKIP 允许）；1 = 至少一个 FAIL。
 *
 * 依赖：**只用 Node 24 自带的 fetch / node:sqlite / node:http**，不引 npm 包。
 */

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:http";
import { connect as netConnect } from "node:net";
/// [docs/46 E 期] 延迟测试台实例（模块级；默认 null = 不启用）。
let rig = null;
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

// ================================================================ 路径与常量

const HERE = fileURLToPath(import.meta.url);
const REPO = resolve(HERE, "..", "..", "..");

/** 运行产物根。任务书要求全部落在 `%TEMP%\ta-e2e\`。 */
const OUT = process.env.TA_E2E_OUT ?? join(process.env.TEMP ?? process.env.TMP ?? ".", "ta-e2e");
const SHOTS = join(OUT, "shots");

/**
 * [docs/46 §297] E 期**统一的探测目标**：应用给 provider 体检时带的就是**服务商的 `base_url`**，
 * 所以两处（夹具档 step28、真实订阅档 step29）都把它指向这条**公网可达**的端点 —— 本机实测
 * `generate_204 → HTTP 204` ✓，且 34 条真实节点里能量到 26 条 ✓。
 *
 * 为什么不用挂具的本地健康端口：提供商节点要经**境外节点**去够它，够不着；而夹具档用本地端口时
 * mihomo 走"绝对 URI 明文转发"这条路径一直给不出延迟（14 条全 0）。
 */
const PROBE_GLOBAL = "https://www.gstatic.com/generate_204";
const WD_DIR = join(OUT, "webdriver");
const DB_DIR = join(OUT, "db");
const LOG_DIR = join(OUT, "logs");
/** App 的数据根（`TA_DATA_DIR`）。**这是本次运行唯一允许被 App 写的目录。** */
const SCRATCH = join(OUT, "data");

const DEFAULT_EXE = join(REPO, "target", "release", "tokenagency.exe");
const DEFAULT_TAURI_DRIVER = join(
  process.env.USERPROFILE ?? "C:\\Users\\19404",
  ".cargo",
  "bin",
  "tauri-driver.exe",
);
const DEFAULT_MSEDGEDRIVER = join(
  process.env.USERPROFILE ?? "C:\\Users\\19404",
  "workspace",
  ".ta-tools",
  "webdriver",
  "msedgedriver.exe",
);

/** 真实数据目录：**只读，绝不能被本脚本或它启动的 App 改**。 */
const REAL_DATA_DIR = join(process.env.APPDATA ?? "", "TokenAgency");

const DRIVER_PORT = Number(process.env.TA_E2E_DRIVER_PORT ?? 4455);
const WD_BASE = `http://127.0.0.1:${DRIVER_PORT}`;
const ELEM_KEY = "element-6066-11e4-a52e-4f735466cecf";

const STEP_TITLES = {
  build: "被测构建标识（exe mtime + SHA256）",
  harness: "harness 前置（tauri-driver / msedgedriver）",
  step0: "隔离断言（TA_DATA_DIR 生效 + 真实目录未变）",
  step1: "启动窗口 + 首页渲染 [data-testid=page-header]",
  step2: "供应商页新增供应商（列表 + 落库）；添加模型后上方出现可编辑行且默认直连",
  step3: "订阅页粘贴 Clash 订阅 URL（列表 + 落库）",
  step4: "网络页建线路（lane_upsert）—— /channels 已整页删除，入口只剩 /network",
  step5: "设置页：自启动开关 / 文件监听是否真控件",
  step6: "附加：真界面写操作落库证明（settings 改字段 → config_apply）",
  step7: "收尾隔离复查（全程结束后真实目录仍未变）",
  step10: "模型页为**每个对外模型**定价：空价/非法价本地点名零命令；合法费率真进配置；成本中心只读不给入口",
  step11: "模型/线路开关：模型关掉即不列出且 404、可逆；线路开关先确认再落库",
  step14: "数据安全回归：设置页「校验并应用」之后，之前建的线路必须还在",
  step15: "能力一致的两条线路可路由；把已归组的线路改成不一致 → 后端必须拒绝并点出字段",
  step17: "节点清单真出数据（/network/nodes ← subscription_nodes，含 fixture 节点）",
  step18: "访问密钥能管（创建/明文一次/列表无明文/启停/删除）；HTTP 鉴权段需网关在监听，否则如实记未验证",
  step20: "服务商详情 →「编辑」进同一个向导的编辑模式：预填现有值、留空密钥不改、第 2 步线路列只读",
  step21: "各种项的删除（确认框 + 级联 + 批量逐条汇报）与线路级代理归属（已在服务商详情页验）",
  step22: "长篇介绍收进「问号圆圈」：悬停/聚焦展开原文，版面上不再铺字",
  step23: "订阅详情页（[docs/54]）：从 /network 点订阅名进得去；顶部「自动选择」「直连」恒在且不可关；节点开关真落库、关掉的节点真被排除出内核组",
};

// ================================================================ 小工具

const t0 = Date.now();
const results = [];

function log(msg) {
  const dt = ((Date.now() - t0) / 1000).toFixed(1).padStart(6);
  process.stdout.write(`[${dt}s] ${msg}\n`);
}

function record(step, status, detail, evidence = []) {
  results.push({ step, title: STEP_TITLES[step] ?? step, status, detail, evidence });
  const icon = status === "PASS" ? "PASS" : status === "FAIL" ? "FAIL" : "SKIP";
  log(`${icon} ${step} ${STEP_TITLES[step] ?? ""} — ${detail}`);
  for (const e of evidence) log(`        · ${e}`);
}

function ensureDirs() {
  for (const d of [OUT, SHOTS, WD_DIR, DB_DIR, LOG_DIR, SCRATCH]) {
    mkdirSync(d, { recursive: true });
  }
}

function sha256File(p) {
  return createHash("sha256").update(readFileSync(p)).digest("hex").toUpperCase();
}

function fileId(p) {
  const s = statSync(p);
  return { size: s.size, mtimeMs: s.mtimeMs, mtime: new Date(s.mtimeMs).toISOString() };
}

/**
 * 把一个目录里的文件（**递归**）做成 `相对路径 → {size, mtimeMs, sha256?}` 的快照。
 * 用于"真实数据目录没被动过"的断言：只看文件、不看目录 mtime —— 目录 mtime 会被
 * 别的进程（例如用户自己开着的实例）触碰，把它算进来会得到假红。
 */
function snapshotDir(root, { hash = [] } = {}) {
  const out = {};
  if (!existsSync(root)) return out;
  const walk = (dir, rel) => {
    for (const name of readdirSync(dir, { withFileTypes: true })) {
      const abs = join(dir, name.name);
      const r = rel === "" ? name.name : `${rel}/${name.name}`;
      if (name.isDirectory()) walk(abs, r);
      else {
        const id = fileId(abs);
        if (hash.some((h) => r.endsWith(h))) id.sha256 = sha256File(abs);
        out[r] = id;
      }
    }
  };
  walk(root, "");
  return out;
}

function diffSnapshots(before, after) {
  const changed = [];
  const all = new Set([...Object.keys(before), ...Object.keys(after)]);
  for (const k of all) {
    const b = before[k];
    const a = after[k];
    if (b === undefined) changed.push(`NEW    ${k}`);
    else if (a === undefined) changed.push(`GONE   ${k}`);
    else if (b.size !== a.size || b.mtimeMs !== a.mtimeMs || b.sha256 !== a.sha256) {
      changed.push(`CHANGED ${k} (size ${b.size}→${a.size}, sha ${b.sha256 ?? "-"}→${a.sha256 ?? "-"})`);
    }
  }
  return changed;
}

/**
 * 列出正在运行的 TokenAgency 实例（按 exe 文件名的前缀匹配，不看路径 ——
 * 用户可能装在不同的目录里，例如 `%LOCALAPPDATA%\TokenAgency\tokenagency.exe`）。
 * Windows 上用 `tasklist`：不需要额外依赖，也不会因为 node 原生模块而炸。
 */
function listRunningAppInstances() {
  const r = spawnSync("tasklist", ["/FO", "CSV", "/NH"], { encoding: "utf8", windowsHide: true });
  if (r.status !== 0 || !r.stdout) return [];
  const self = process.pid;
  const out = [];
  for (const line of r.stdout.split(/\r?\n/)) {
    const m = /^"([^"]+)","(\d+)"/.exec(line.trim());
    if (!m) continue;
    if (!/^tokenagency/i.test(m[1])) continue;
    const pid = Number(m[2]);
    if (pid === self) continue;
    out.push({ pid, path: `image=${m[1]}` });
  }
  return out;
}

/**
 * 与 DOM 探测**独立**的交叉验证：前端产物里到底有没有引用过某个命令名？
 *
 * 为什么值得单独做一次：DOM 上"没找到按钮"可能只是探测规则太窄；而
 * "整个前端 bundle 里根本不含这个命令名字符串"是更强的证据 —— 说明**没有任何页面调用它**。
 * （Vite/Rollup 会摇掉未被引用的命令字符串，所以这个判据是有效的：反面对照见 `config_apply`
 * 与本函数同一份 bundle 里的结果。）
 */
function distCommandPresence(cmds) {
  const dir = join(REPO, "dist", "assets");
  if (!existsSync(dir)) return "（没有 dist/assets，无法交叉验证）";
  const bundle = readdirSync(dir)
    .filter((f) => f.endsWith(".js"))
    .map((f) => readFileSync(join(dir, f), "utf8"))
    .join("\n");
  return cmds.map((c) => `${c}=${bundle.includes(c) ? "前端有引用" : "前端从未引用"}`).join(" · ");
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * 被测 exe 到底支不支持 `TA_DATA_DIR` 覆盖口？
 *
 * 判据：`src-tauri/src/state.rs` 的 `pub const DATA_DIR_ENV: &str = "TA_DATA_DIR";`
 * 会以 ASCII 原文进 `.rdata`。**只看这一个字符串**，不去搜索二进制里别的地方 ——
 * 宁可保守（假阴性只是多要求一次重新构建），也不能放过一个会写真实目录的构建。
 */
function exeHasDataDirOverride(exe) {
  try {
    return readFileSync(exe).includes(Buffer.from("TA_DATA_DIR", "ascii"));
  } catch {
    return false;
  }
}

// ================================================================ WebDriver HTTP 客户端

/**
 * 裸 WebDriver HTTP 客户端。每个请求/响应原文都落盘（任务书要求"原始证据"）。
 * 不用任何 WebDriver 库：Node 24 自带 fetch 够了，也就不会引新依赖。
 */
class WebDriver {
  constructor(base) {
    this.base = base;
    this.sessionId = undefined;
    this.seq = 0;
  }

  async raw(method, path, body) {
    const n = String(++this.seq).padStart(4, "0");
    const url = this.base + path;
    let res;
    let text;
    try {
      res = await fetch(url, {
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      text = await res.text();
    } catch (e) {
      text = `<<transport error: ${e.message}>>`;
      res = { status: 0, ok: false };
    }
    const safe = path.replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 120);
    writeFileSync(
      join(WD_DIR, `${n}-${method}-${safe}.json`),
      JSON.stringify(
        { at: new Date().toISOString(), request: { method, url, body }, response: { status: res.status, body: tryJson(text) } },
        null,
        2,
      ),
    );
    if (!res.ok) throw new Error(`WebDriver ${method} ${path} → HTTP ${res.status}: ${text.slice(0, 600)}`);
    return tryJson(text);
  }

  // ---- 会话
  async newSession(application, extra = {}) {
    const caps = {
      capabilities: {
        alwaysMatch: {
          "tauri:options": { application, ...extra },
        },
      },
    };
    const r = await this.raw("POST", "/session", caps);
    this.sessionId = r.value?.sessionId ?? r.sessionId;
    if (!this.sessionId) throw new Error(`没拿到 sessionId：${JSON.stringify(r).slice(0, 400)}`);
    return this.sessionId;
  }

  async deleteSession() {
    if (!this.sessionId) return;
    try {
      await this.raw("DELETE", `/session/${this.sessionId}`);
    } catch {
      /* 会话可能已经死了 —— 收尾失败不该盖掉真结果 */
    }
    this.sessionId = undefined;
  }

  // ---- 元素
  async findElement(selector) {
    try {
      const r = await this.raw("POST", `/session/${this.sessionId}/element`, {
        using: "css selector",
        value: selector,
      });
      return r.value?.[ELEM_KEY] ?? null;
    } catch {
      return null;
    }
  }

  async findByText(tag, pattern) {
    const r = await this.raw("POST", `/session/${this.sessionId}/elements`, {
      using: "css selector",
      value: tag,
    });
    for (const el of r.value ?? []) {
      const id = el[ELEM_KEY];
      const txt = await this.text(id).catch(() => "");
      if (pattern.test(txt)) return id;
    }
    return null;
  }

  /**
   * 当前弹层里的选项：元素 id + 文案 + 是否已选（`aria-selected`）。没有弹层就是空数组。
   *
   * 为什么需要它：多选（MUI `multiple`）**选完不关弹层**，要连点多个选项就必须拿着弹层里
   * 元素的真实 id 去点；只靠"再点一次控件"会被 backdrop 拦下（`element click intercepted`）。
   */
  async optionItems() {
    const r = await this.raw("POST", `/session/${this.sessionId}/elements`, {
      using: "css selector",
      value: 'li[role="option"]',
    });
    const out = [];
    for (const el of r.value ?? []) {
      const id = el[ELEM_KEY];
      const rect = await this.raw("GET", `/session/${this.sessionId}/element/${id}/rect`).catch(() => null);
      if (rect !== null && ((rect.value?.width ?? 0) === 0 || (rect.value?.height ?? 0) === 0)) continue;
      out.push({
        id,
        text: (await this.text(id).catch(() => "")).replace(/\s+/g, " ").trim(),
        selected: (await this.attr(id, "aria-selected").catch(() => null)) === "true",
      });
    }
    return out;
  }

  /**
   * 弹层真开着才收（判据 `[role=listbox]`：MUI 的 Select 即使 0 个选项也会渲染这个 ul）。
   *
   * ★ 关键在"Esc 送给谁"：**送到 listbox 元素自己身上**（`POST /element/{id}/value`），
   * 而不是全局送键。这样 keydown 只在**弹层内部**冒泡 → 弹层正常关闭；而 Dialog 是另一个
   * portal（不是 listbox 的祖先）**收不到**这个事件 → 不会连带把整个对话框关掉。
   * 反面教材（都实测过）：全局 Esc → 对话框被关；点视口角落 → 退场动画里那一下落到
   * Dialog 自己的 backdrop 上 → 对话框同样被关。
   */
  async closeMenuIfOpen() {
    const lb = await this.findElement('[role="listbox"]');
    if (lb === null) return false;
    await this.raw("POST", `/session/${this.sessionId}/element/${lb}/value`, {
      text: "\uE00C",
      value: ["\uE00C"],
    }).catch(() => {});
    if (await this.waitGone('[role="listbox"]', 1200)) return true;
    // 退场动画 / stale：再拿一次 listbox 重试一遍
    const again = await this.findElement('[role="listbox"]');
    if (again !== null) {
      await this.raw("POST", `/session/${this.sessionId}/element/${again}/value`, {
        text: "\uE00C",
        value: ["\uE00C"],
      }).catch(() => {});
      await this.waitGone('[role="listbox"]', 1200);
    }
    return true;
  }

  async waitFor(selector, timeoutMs = 15000, pollMs = 200) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const el = await this.findElement(selector);
      if (el !== null) return el;
      if (Date.now() > deadline) return null;
      await sleep(pollMs);
    }
  }

  async waitGone(selector, timeoutMs = 15000, pollMs = 200) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const el = await this.findElement(selector);
      if (el === null) return true;
      if (Date.now() > deadline) return false;
      await sleep(pollMs);
    }
  }

  async click(elId) {
    await this.raw("POST", `/session/${this.sessionId}/element/${elId}/click`, {});
  }

  async clear(elId) {
    await this.raw("POST", `/session/${this.sessionId}/element/${elId}/clear`, {});
  }

  async type(elId, text) {
    await this.raw("POST", `/session/${this.sessionId}/element/${elId}/value`, {
      text,
      value: [...text],
    });
  }

  /**
   * 真输入：先点（拿焦点）、清空、再逐字送键。**React 受控组件必须走真事件。**
   *
   * ⚠ `clear()`（W3C 的 element clear）**对部分 MUI 受控输入框是 no-op** —— 它不发键盘事件，
   * 而 MUI 的受控 input 由 React 的 `value` 驱动，清空后没被 React 重新渲染回来就"看起来没清"。
   * 真机实测的后果是**追加而不是替换**：`channels-add-contextTokens ← "200000"` 读回 `"0200000"`、
   * `channels-add-modalities ← "text,image"` 读回 `"texttext,image"`、
   * 首启向导的 id 框 `"p1" → "p1ta-e2e-provider"`。数字凑巧解析成同一个值，所以这次没炸，
   * 但任何"替换值"的用例都会踩到 —— 必须补一次**真键盘**的清空。
   *
   * 做法：Home → Delete × 当前长度（\uE011 / \uE017 是 W3C 的 Home / Delete）。
   * 清不干净就在日志里说一声（**不静默**）：那说明这个控件还有别的毛病，值得看一眼。
   */
  async fill(elId, text) {
    await this.click(elId).catch(() => {});
    await this.clear(elId).catch(() => {});
    const leftover = await this.attr(elId, "value").catch(() => null);
    if (typeof leftover === "string" && leftover.length > 0) {
      const keys = `\uE011${"\uE017".repeat(leftover.length)}`;
      await this.raw("POST", `/session/${this.sessionId}/element/${elId}/value`, {
        text: keys,
        value: [...keys],
      }).catch(() => {});
      const stillThere = await this.attr(elId, "value").catch(() => null);
      if (typeof stillThere === "string" && stillThere.length > 0) {
        log(`        · 注意：${JSON.stringify(leftover)} 没清干净（现值 ${JSON.stringify(stillThere)}）→ 本次输入是追加`);
      }
    }
    await this.type(elId, text);
  }

  /**
   * 送一次 Esc（W3C actions，**全局**送键）。
   *
   * ⚠️ **不要用它来收下拉弹层**：全局送键会落到 `document.activeElement`，而空下拉时焦点还在
   * Dialog 里 → 整个对话框被关掉（真机实测）。收弹层用 `closeMenuIfOpen()`。
   */
  async pressEscape() {
    await this.raw("POST", `/session/${this.sessionId}/actions`, {
      actions: [
        {
          type: "key",
          id: "keyboard",
          actions: [
            { type: "keyDown", value: "\uE00C" },
            { type: "keyUp", value: "\uE00C" },
          ],
        },
      ],
    });
    await this.raw("DELETE", `/session/${this.sessionId}/actions`).catch(() => {});
  }

  /** 关掉当前 MUI Dialog（真点它的「取消/关闭」按钮；找不到就退化为 Esc）。 */
  async closeDialog() {
    const clicked = await this.exec(`
      const dlg = document.querySelector('[role="dialog"]');
      if (!dlg) return false;
      const btn = [...dlg.querySelectorAll('button')].find((b) => /取消|关闭|Cancel|Close/i.test(b.innerText || ''));
      if (!btn) return false;
      btn.click();
      return true;
    `);
    if (clicked !== true) await this.pressEscape();
    await this.waitGone('[role="dialog"]', 4000);
    return clicked === true;
  }

  async text(elId) {
    const r = await this.raw("GET", `/session/${this.sessionId}/element/${elId}/text`);
    return r.value ?? "";
  }

  /**
   * 按 selector 读文本，**容忍 stale element**。
   *
   * 为什么必须有这个：React 会在数据到达时整体替换节点，先前拿到的 element id
   * 立刻失效（msedgedriver 报 `stale element reference`）。直接在循环里
   * "先 find 再 text" 会随机炸 —— 而这里恰恰是"点了提交、等一会结果"的场景。
   * 返回 null 表示"现在找不到这个元素"。
   */
  async textOf(selector, attempts = 3) {
    for (let i = 0; i < attempts; i++) {
      const el = await this.findElement(selector);
      if (el === null) return null;
      try {
        return await this.text(el);
      } catch (e) {
        if (!/stale element reference/.test(String(e.message))) throw e;
        await sleep(150);
      }
    }
    return null;
  }

  async attr(elId, name) {
    const r = await this.raw("GET", `/session/${this.sessionId}/element/${elId}/attribute/${name}`);
    return r.value;
  }

  /** 按 selector 读属性，容忍 stale element（同 `textOf` 的理由）。 */
  async attrOf(selector, name, attempts = 3) {
    for (let i = 0; i < attempts; i++) {
      const el = await this.findElement(selector);
      if (el === null) return null;
      try {
        return await this.attr(el, name);
      } catch (e) {
        if (!/stale element reference/.test(String(e.message))) throw e;
        await sleep(150);
      }
    }
    return null;
  }

  async enabled(elId) {
    const r = await this.raw("GET", `/session/${this.sessionId}/element/${elId}/enabled`);
    return r.value === true;
  }

  async screenshot(file) {
    const r = await this.raw("GET", `/session/${this.sessionId}/screenshot`);
    writeFileSync(file, Buffer.from(r.value, "base64"));
    return file;
  }

  /**
   * 执行脚本。**本脚本只允许用它做两件事**：
   *   (a) 改 hash 路由（任务书明文允许）；
   *   (b) 读 DOM 做"界面入口存在吗"的探测与证据采集。
   * 绝不用它调用 IPC / 假装界面操作。
   */
  async exec(script, args = []) {
    const r = await this.raw("POST", `/session/${this.sessionId}/execute/sync`, { script, args });
    return r.value;
  }

  /** 把当前页面上所有可交互控件（含 data-testid）抓下来，作为 SKIP 的原始证据。 */
  async probeControls() {
    return this.exec(`
      const out = [];
      const sel = 'button,[role="button"],a[href],input,textarea,select,[role="combobox"],[role="switch"],[role="checkbox"],[role="tab"]';
      for (const el of document.querySelectorAll(sel)) {
        const r = el.getBoundingClientRect();
        out.push({
          tag: el.tagName.toLowerCase(),
          testid: el.getAttribute('data-testid') || '',
          role: el.getAttribute('role') || '',
          type: el.getAttribute('type') || '',
          text: (el.innerText || el.value || el.getAttribute('placeholder') || '').trim().slice(0, 70),
          visible: r.width > 0 && r.height > 0,
        });
      }
      return out;
    `);
  }

  /** 页面上所有带 data-testid 的元素（探测"缺哪个 selector"用）。 */
  async probeTestIds() {
    return this.exec(`
      return [...document.querySelectorAll('[data-testid]')]
        .map(e => e.getAttribute('data-testid'))
        .filter((v, i, a) => a.indexOf(v) === i)
        .sort();
    `);
  }

  async navHash(hash) {
    await this.exec(`window.location.hash = ${JSON.stringify(hash)}; return window.location.hash;`);
  }

  async pageText() {
    return this.exec(`return document.body ? document.body.innerText.slice(0, 4000) : '';`);
  }
}

function tryJson(s) {
  try {
    return JSON.parse(s);
  } catch {
    return s;
  }
}

// ================================================================ 本地订阅 fixture（不依赖公网）

/** 一份最小但**结构真实**的 Clash 订阅。节点名是断言用的标记。 */
/**
 * [docs/46 E 期] **本机延迟测试台**：两个 HTTP CONNECT 代理（一慢一快）+ 一个健康检查端点。
 *
 * 为什么需要它：挂具原来的 fixture 节点是**假端口**（连不通）⇒ 内核根本测不出延迟
 * ⇒ "选最低延迟 / 订阅更新后不换 IP"这两条**在这台机器上无法验证**。
 * 这两个代理都**真的能连**，且**人为设置不同延迟**，"最低延迟"才有含义。
 *
 * 只供新步骤使用：**不改变任何既有步骤的行为**（默认不启动）。
 */
function startLatencyRig({
  fastDelayMs = 0,
  slowDelayMs = 400,
  fastPort = 0,
  slowPort = 0,
  healthPort = 0,
} = {}) {
  const sockets = [];
  const health = createServer((req, res) => {
    res.writeHead(200, { "content-type": "text/plain" });
    res.end("ok");
  });
  // **固定端口**：`step28` 会把"健康检查地址"**保存进库**，而**下一次运行启动时**内核读的就是库里那条
  // ⇒ 若这里用随机端口，下次启动时那个地址已经指向一个**没人监听的端口** ⇒ 探测必然全失败 ✓
  // （实测踩过：库里存着上一轮的 `127.0.0.1:6808`，本次全部"未测出"。）
  health.listen(healthPort, "127.0.0.1");
  /** 一个 CONNECT 代理：延迟 `delayMs` 之后把字节转发到目标；绑到 `port`（0 = 随机）。 */
  const makeProxy = (delayMs, port) => {
    const proxy = createServer((req, res) => {
      // [docs/46 §E] **普通 HTTP 代理路径**（不只是 CONNECT）。
      //
      // 为什么必须补：自检证明"经 CONNECT 代理连本地端点"是通的（`200 Connection Established` ✓），
      // 可 mihomo 那边两条节点仍是"未测出" ⇒ 它对 `http://` 目标很可能走的是
      // **绝对 URI 的明文转发**，而我只实现了 CONNECT、对其它一律回 405 ⇒ 探测永远失败 ✓
      // （这就是"同样的代理，Node 自检能通、mihomo 测不出"的原因。）
      const raw = req.url ?? "/";
      const absolute = raw.startsWith("http") ? raw : `http://${req.headers.host ?? "127.0.0.1"}${raw}`;
      let target;
      try {
        target = new URL(absolute);
      } catch {
        res.writeHead(400);
        res.end("bad request target");
        return;
      }
      setTimeout(() => {
        const up = netConnect(Number(target.port || 80), target.hostname, () => {
          const head = [
            `${req.method} ${absolute} HTTP/1.1`,
            ...Object.entries(req.headers).map(([k, v]) => `${k}: ${v}`),
            "",
            "",
          ].join("\r\n");
          up.write(head);
          req.pipe(up);
          up.pipe(res);
        });
        sockets.push(up);
        up.on("error", () => {
          if (!res.headersSent) res.writeHead(502);
          res.end("upstream failed");
        });
      }, delayMs);
    });
    proxy.on("connect", (req, clientSocket, head) => {
      sockets.push(clientSocket);
      const [host, port] = String(req.url).split(":");
      setTimeout(() => {
        const upstream = netConnect(Number(port), host, () => {
          clientSocket.write("HTTP/1.1 200 Connection Established\\r\\n\\r\\n");
          if (head && head.length > 0) upstream.write(head);
          upstream.pipe(clientSocket);
          clientSocket.pipe(upstream);
        });
        upstream.on("error", () => clientSocket.destroy());
        clientSocket.on("error", () => upstream.destroy());
      }, delayMs);
    });
    proxy.listen(port, "127.0.0.1");
    return proxy;
  };
  const fast = makeProxy(fastDelayMs, fastPort);
  const slow = makeProxy(slowDelayMs, slowPort);
  return {
    health,
    fast,
    slow,
    healthPort: () => health.address().port,
    fastPort: () => fast.address().port,
    slowPort: () => slow.address().port,
    stop: () => {
      for (const s of sockets) s.destroy();
      for (const s of [health, fast, slow]) s.close();
    },
  };
}

/**
 * [docs/46 E 期] **延迟测试用的订阅**：两条 `type: http` 节点，分别指向测试台的 fast / slow 代理。
 *
 * 与 `CLASH_YAML` 的关系：**完全独立**（走另一个 URL 路径），既有步骤一行都不受影响。
 * 节点的 `server/port` 是**真的能连**的本机代理，健康检查指向本机那个返回 200 的端点 ——
 * 于是内核能量出**两个明显不同的延迟**，"选最低延迟"才有可验证的含义。

 */
function latencyClashYaml({ healthPort, fastPort, slowPort }) {
  const healthUrl = `http://127.0.0.1:${healthPort}/`;
  return [
    "proxies:",
    `- name: "ta-lat-fast"`,
    "  type: http",
    "  server: 127.0.0.1",
    `  port: ${fastPort}`,
    `- name: "ta-lat-slow"`,
    "  type: http",
    "  server: 127.0.0.1",
    `  port: ${slowPort}`,
    "proxy-groups:",
    `- name: "ta-lat-group"`,
    "  type: select",
    "  proxies:",
    `    - "ta-lat-fast"`,
    `    - "ta-lat-slow"`,
    "  url: " + JSON.stringify(healthUrl),
    "  interval: 1",
    "rules:",
    "- MATCH,ta-lat-group",
    "",
  ].join("\\n");
}

const CLASH_YAML = `port: 7890
socks-port: 7891
mode: rule
proxies:
  # [docs/46 E 期] 两条**真节点**：指向测试台的两个本地 CONNECT 代理（固定端口 8391 = 慢 400ms、
  # 8390 = 快 0ms）。**故意把慢的排在前面** —— 这样"选中了快的那条"就排除了"选第一个"的巧合。
  # 两个端口由 startLatencyRig({ fastPort: 8390, slowPort: 8391 }) 在同一次运行里绑定。
  - name: "ta-lat-slow"
    type: http
    server: 127.0.0.1
    port: 8391
  - name: "ta-lat-fast"
    type: http
    server: 127.0.0.1
    port: 8390
  - name: "ta-e2e-node-hk"
    type: ss
    server: 127.0.0.1
    port: 8388
    cipher: aes-256-gcm
    password: "ta-e2e-pass"
  - name: "ta-e2e-node-jp"
    type: vmess
    server: 127.0.0.1
    port: 8389
    uuid: 00000000-0000-0000-0000-000000000000
    alterId: 0
    cipher: auto
proxy-groups:
  - name: "ta-e2e-group"
    type: select
    proxies:
      - "ta-e2e-node-hk"
      - "ta-e2e-node-jp"
rules:
  - MATCH,ta-e2e-group
`;

/** 每次请求多给一个节点：让订阅刷新**成功**（坏更新守卫要求"本次多于上版"）。 */
let clashHits = 0;
function clashYamlGrown() {
  clashHits += 1;
  const extra = [];
  for (let i = 0; i < clashHits; i += 1) {
    extra.push(`  - name: "ta-e2e-node-extra-${i + 1}"`);
    extra.push("    type: ss");
    extra.push("    server: 127.0.0.1");
    extra.push(`    port: ${8390 + i}`);
    extra.push("    cipher: aes-256-gcm");
    extra.push('    password: "ta-e2e-pass"');
  }
  return CLASH_YAML.replace("proxy-groups:", `${extra.join("\n")}\nproxy-groups:`);
}
/**
 * 本地 fixture 服务（**不依赖公网**）。三个用途：
 * 1. `/clash.yaml`（以及默认路径）→ 一份结构真实的 Clash 订阅，带 `subscription-userinfo` 流量头；
 * 2. `/v1/models` → OpenAI 兼容的模型清单，供「服务商 → 测试连接」与「一键扫描模型」真调用；
 * 3. `/notfound/v1/models` → **恒 404**，用来验证"上游没有 /models 端点"这一档
 *    （`ProbeResult.status=404 && probe="models"`：界面上必须说"上游没有这个端点"，
 *    **不许**渲染成"连接失败"）。
 */
async function startFixture() {
  const models = ["ta-e2e-scan-a", "ta-e2e-scan-b", "ta-e2e-m-add"];
  const server = createServer((req, res) => {
    const path = (req.url ?? "").split("?")[0];
    // [docs/46 E 期] **延迟测试订阅**：只有新步骤会用；`rig` 没启动就 404（既有步骤完全不受影响）。
    if (path === "/clash-latency.yaml") {
      if (rig === null) {
        res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
        res.end("latency rig not started");
        return;
      }
      res.writeHead(200, { "content-type": "text/yaml; charset=utf-8" });
      res.end(latencyClashYaml(rig));
      return;
    }
    // 情况 3：这个前缀下**什么都 404**（模拟"上游没有 /models 端点"）
    if (path.startsWith("/notfound")) {
      res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
      res.end("not found");
      return;
    }
    // 情况 2：OpenAI 兼容的模型清单
    if (path.endsWith("/models")) {
      res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
      res.end(
        JSON.stringify({
          object: "list",
          data: models.map((id) => ({ id, object: "model", owned_by: "ta-e2e" })),
        }),
      );
      return;
    }
    // 情况 1：Clash 订阅（默认路径也给，老步骤依赖这一点）
    res.writeHead(200, {
      "content-type": "text/yaml; charset=utf-8",
      // 流量头：**用用户给的真实量级**（Clash 面板那种 92.9GB / 130GB = 71%），
      // 这样 E2E 能断言界面算出来的数字与这里对得上（而不是拿 1/2/1024 这种看不出错的数）。
      // upload + download = 92.9 GB，total = 130 GB → 71%。
      "subscription-userinfo": `upload=${Math.round(10.0 * 1024 ** 3)}; download=${Math.round(82.9 * 1024 ** 3)}; total=${130 * 1024 ** 3}; expire=1893456000`,
    });
      // [docs/46 E 期] 把测试台的两条**真节点**并进这份订阅（快 0ms / 慢 400ms 的两个本地代理），
      // 这样内核能测出真实延迟，"选最低延迟 / 粘性"才有可验数值。匹配不到 `proxies:` 就原样返回（安全）。
    res.end(clashYamlGrown());
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  // [docs/46 E 期] 固定端口：`CLASH_YAML` 里那两条静态真节点就指向 8390（快）/ 8391（慢）。
  rig = startLatencyRig({ fastPort: 8390, slowPort: 8391, healthPort: 8392 });
  const port = server.address().port;
  const url = `http://127.0.0.1:${port}/clash.yaml`;
  const baseUrl = `http://127.0.0.1:${port}/v1`;
  // 供"上游没有 /models 端点"那一档用（见 step16）：它必须在 /notfound 前缀下
  const noModelsBaseUrl = `http://127.0.0.1:${port}/notfound/v1`;
  writeFileSync(join(OUT, "fixture-clash.yaml"), CLASH_YAML);
  return { server, port, url, baseUrl, noModelsBaseUrl, models };
}

// ================================================================ SQLite 证据

function openScratchDb() {
  const p = join(SCRATCH, "db.sqlite");
  if (!existsSync(p)) return null;
  // ★ **不能**用 `readOnly: true`：这份库跑在 WAL 模式下，而未提交进主库的页都在 `-wal` 里；
  //   只读连接既不能建 `-shm` 也无法回放 WAL，于是会**静默读到一份旧快照**
  //   （真机症状：界面上明明有候选行，`provider_model_catalog` 却读成 0 行 —— 那不是扫描没记录，
  //    是证据读错了地方）。这里开**可写**连接并给足 busy_timeout：读的是我们自己的 scratch 库，
  //   与被测进程共享时的唯一风险是短暂锁等待。
  const db = new DatabaseSync(p);
  try {
    db.exec("PRAGMA busy_timeout = 3000");
  } catch {
    // 老版本 node:sqlite 不认这条 PRAGMA 也不该让证据读取整个失败
  }
  return db;
}

/** 落库证据：config 表里所有实体 + subscription 表全部行。 */
function dumpScratchDb(tag) {
  const db = openScratchDb();
  if (db === null) return { ok: false, reason: `没有 ${join(SCRATCH, "db.sqlite")}` };
  const out = { dbPath: join(SCRATCH, "db.sqlite"), at: new Date().toISOString() };
  try {
    out.tables = db
      .prepare("select name from sqlite_master where type='table' order by name")
      .all()
      .map((r) => r.name);
    out.config = db
      .prepare("select kind, entity_id, ord, json, updated_at from config order by kind, ord, entity_id")
      .all();
    out.subscription = db
      .prepare("select id, name, url, format, enabled, node_count, group_count, updated_at from subscription")
      .all();
    out.store_setting = db.prepare("select key, value, updated_at from store_setting order by key").all();
    // [docs/25] 扫描记录（**一张独立的表**，不在 `config` 里）：step24 要靠它证明
    // "扫描真的落库了"。过去这里没 dump 它，于是 step24 永远读到 0 行、把一次成功的扫描
    // 判成"没有全量记录" —— 那是**证据读错了地方**，不是产品缺陷（真机实测：界面有候选行）。
    try {
      out.provider_model_catalog = db.prepare("select * from provider_model_catalog order by provider_id, upstream_model").all();
    } catch (e) {
      out.provider_model_catalog_error = String(e && e.message ? e.message : e);
    }
    // 密钥**元数据**（`secret_meta`，[docs/19 §1.3]）：step2 要证明"向导第 1 步填的那把密钥
    // 真的进了密钥仓库"。这张表**过去根本没有被 dump**：`dump.secret_meta` 永远是
    // `undefined` → 调用点的 `?? []` → "0 行" → `metaOk` 恒为 false。
    // 于是 step2 报的 `密钥(落库=false)` 是一句**假红**：真机查同一个 scratch 库，
    // `secret_meta` 里明明有 `ta-e2e-default | hint=sk-t***`（产品一直在写，是挂具读错了地方）。
    // 连**列名**一起 dump：这张表的硬约束是"没有 value 列"，列名清单让"明文不在库里"这条
    // 断言有据可查，而不是靠"我没看见"。
    try {
      out.secret_meta = db
        .prepare("select name, created_at, last_used, hint from secret_meta order by name")
        .all();
      out.secret_meta_columns = db
        .prepare("select name from pragma_table_info('secret_meta') order by cid")
        .all()
        .map((r) => String(r.name));
    } catch (e) {
      out.secret_meta_error = String(e && e.message ? e.message : e);
    }
  } catch (e) {
    out.error = String(e);
  }
  db.close();
  writeFileSync(join(DB_DIR, `dump-${tag}.json`), JSON.stringify(out, null, 2));
  return { ok: true, ...out };
}

/** 判断 config 表里有没有某个 kind 的实体（服务商 / 线路都存在 config 表里）。 */
function configEntityKinds(dump) {
  const kinds = {};
  for (const row of dump.config ?? []) kinds[row.kind] = (kinds[row.kind] ?? 0) + 1;
  return kinds;
}

function dbFingerprint(dump) {
  return createHash("sha256")
    .update(JSON.stringify({ config: dump.config ?? [], subscription: dump.subscription ?? [] }))
    .digest("hex")
    .slice(0, 16);
}

// ================================================================ 第 61 轮：四个添加向导（真机）
//
// 用户的原话：「我仍然没有看到添加服务商、添加提供商、模型、路由策略的地方，用户在软件里面不能添加这些」。
// 这四步就是去真窗口里把「添加」走一遍，并按固定 testid 驱动（契约见 docs/17）。
// 约定与其它步骤一致：**入口不存在就是 SKIP，绝不用替代路径凑 PASS**；每一步都查 `config` 表确认真的落库。

/** WebDriver 错误原文：去掉会话/元素 id 的长噪音，保留**错误码与原因**（截断会把原因吃掉）。 */
function wdErrText(err, max = 240) {
  return String(err?.message ?? err)
    .replace(/\/session\/[0-9a-f-]+/gi, "/session/<id>")
    .replace(/\/element\/[^/ ]+/g, "/element/<el>")
    .replace(/\s+/g, " ")
    .slice(0, max);
}

/**
 * 一个 testid 对应控件的**可交互本体**：内部 `[role=combobox]` / 输入框优先，最后退回容器。
 *
 * 两种真实形状都要认：`Select` 的 testid 挂在**容器**上（内部是真控件），
 * `TextField` 的 testid 直接挂在 `input` 上（容器就是控件本身）。
 * 取容器去点/去填会得到 `element not interactable`。
 */
async function wdControlForTid(wd, tid) {
  return (
    (await wd.findElement(`[data-testid="${tid}"] [role="combobox"]`)) ??
    (await wd.findElement(`[data-testid="${tid}"] input:not([type="hidden"]), [data-testid="${tid}"] textarea`)) ??
    (await wd.findElement(`[data-testid="${tid}"]`))
  );
}

/** 按 testid 填字段：内部 input/textarea 优先，取不到再退回容器本身；填不进去只记录、不中断。 */
async function wdFillByTid(wd, tid, value, evidence) {
  // 先把遗留弹层收掉：它盖在页面上时，连输入框都点不动（假失败）。
  await wd.closeMenuIfOpen();
  const el = await wdControlForTid(wd, tid);
  if (el === null) {
    evidence.push(`缺控件 ${tid}`);
    return false;
  }
  try {
    await wd.fill(el, value);
    const back = await wd.attrOf(`[data-testid="${tid}"]`, "value");
    evidence.push(`真输入 ${tid} ← ${JSON.stringify(value)}（读回 value=${JSON.stringify(back)}）`);
    return true;
  } catch (err) {
    evidence.push(`控件 ${tid} 填不进去：${wdErrText(err)}`);
    return false;
  }
}

/**
 * DOM click 兜底被用过几次（**要在结论里露出来**）。
 *
 * 兜底绕过了 WebDriver 的"可点击性"判定，所以它必须可见：一次都没有是常态，
 * 突然变多说明某个控件的位置/遮挡变了，值得看一眼，而不是悄悄被平均掉。
 */
let domClickFallbacks = 0;

/**
 * 把**唯一**目标滚进视口并打上 `data-e2e-click` 标记，返回被标记元素的文本（找不到返回 null）。
 *
 * 为什么要先"标记"而不是直接 `findElement` 再点：
 * - 表格很宽时最右边的「操作」列在 1280 宽的窗口里默认落在滚动容器**之外**，WebDriver 的 click 会被
 *   容器拦下并回 `element click intercepted`（第 73 轮真机抓到），所以必须先 `scrollIntoView({ inline: 'center' })`；
 * - 行内的 `data-testid` 是**每行一个**（`pricing-delete` / `policies-delete` / `channels-delete`…），
 *   只按 testid 找永远只能找到第一行 —— 想删"被策略点名的那一条通道"就必须按行文本定位。
 *
 * `rowTid` + `match` 给定时先在这些行里按行文本找（`match` 是行文本里必须出现的**整词**），
 * 再在行内找控件。整词而不是子串：子串会让 `ch-a` 命中 `ch-a2` 那一行（删错实体）。
 */
async function markForClick(wd, selector, { rowTid = null, match = [] } = {}) {
  const res = await wd
    .exec(`
      const sel = ${JSON.stringify(selector)};
      const rowTid = ${JSON.stringify(rowTid)};
      const match = ${JSON.stringify(match)};
      document.querySelectorAll('[data-e2e-click]').forEach((n) => n.removeAttribute('data-e2e-click'));
      // id 允许的字符：字母数字、下划线、短横线（配置里的 id 形状）——两侧都不是这些才算"整词"。
      const idChar = (ch) => ch !== '' && /[A-Za-z0-9_-]/.test(ch);
      const hasWord = (text, m) => {
        let i = text.indexOf(m);
        while (i >= 0) {
          if (!idChar(i === 0 ? '' : text[i - 1]) && !idChar(text[i + m.length] ?? '')) return true;
          i = text.indexOf(m, i + 1);
        }
        return false;
      };
      let el = null;
      if (rowTid === null) {
        el = document.querySelector(sel);
      } else {
        const rows = [...document.querySelectorAll('[data-testid="' + rowTid + '"]')];
        // 行文本要**按格子/元件边界切成片段**再判"整词"。
        //
        // 为什么不直接读 r.textContent（这是 step21 那四档"点不到或找不到"的真因）：
        // td 与 td 之间的文本在 textContent 里是**直接拼在一起**的（没有分隔符）。
        // 于是 ch-ta-e2e-ta-e2e-default-ta-e2e-m-add 后面紧跟的下一格
        // （模型那一格，或策略行的名字那一格）让"整词"判定失败 —— 后一个字符仍是 id 字符。
        // 真机证据就是这一句：找不到行（[data-testid="channels-row"] 文本含
        // ["ch-ta-e2e-ta-e2e-default-ta-e2e-m-add"]）里的控件 channels-delete，
        // 而那一行的 id **明明整段在 DOM 里**（[docs/26 §2] 的 AutoEllipsis 是 CSS 省略，
        // 原文字一个不少）—— 那是挂具看错了边界，不是产品少了行。
        //
        // 逐格取文本、再逐格按内层 testid 取一遍：格子/元件边界天然就是词边界。
        // hasWord 的边界规则**一个字都没改**，所以既修了假红、又没有放宽
        // （ch-a 依旧不会命中 ch-a2 那一行）。
        const segmentsOf = (r) => {
          const segs = [];
          for (const cell of r.querySelectorAll('td, th')) {
            const t = cell.textContent || '';
            if (t.trim() !== '') segs.push(t);
            for (const inner of cell.querySelectorAll('[data-testid]')) {
              const it = inner.textContent || '';
              if (it.trim() !== '') segs.push(it);
            }
          }
          // 非表格行（例如向导里 providers-line-row 那种 div 行）没有格子：
          // innerText 在块级元素之间有换行，也是天然边界。
          const it = typeof r.innerText === 'string' ? r.innerText : '';
          if (it.trim() !== '') segs.push(it);
          // 最后**照样**把原始 textContent 收进来：片段是"多给的边界"，
          // 不是"换一种判法"——保留它才能保证这里只可能**多认**出行，
          // 不可能因为切片段而漏掉一行（例如 id 被拆在两个相邻内联元素里的写法）。
          segs.push(r.textContent || '');
          return segs;
        };
        const row = rows.find((r) => {
          const segs = segmentsOf(r);
          return match.every((m) => segs.some((s) => hasWord(s, m)));
        });
        el = row ? row.querySelector(sel) : null;
      }
      if (!el) return null;
      el.setAttribute('data-e2e-click', '1');
      if (el.scrollIntoView) el.scrollIntoView({ block: 'center', inline: 'center' });
      return { text: (el.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 40) };
    `)
    .catch(() => null);
  return res ?? null;
}

/**
 * 真的点下去：原生 click 被"拦截/不可交互"挡回 → 重试一次 → 最后用 DOM `click()` 兜底。
 *
 * 为什么需要 DOM 兜底（第 73 轮真机抓到）：成本页的定价表在一个滚动容器里，WebDriver 的"可点击性"
 * 判定对它一直报 `element click intercepted`（原生点了两次都失败），而同一个按钮 `el.click()`
 * 一次就触发了 React 事件（jsdom 单测里它本来也点得动）—— 这不是产品缺陷。
 * 兜底只对**被拦/不可交互**这类错误启用：真 disabled 的控件点了也不会有反应，
 * 调用方仍按"库里的条数没变 = 失败"如实记账。
 */
async function clickResolved(wd, el, tid, evidence) {
  const tryDom = async (why) => {
    const done = await wd
      .exec(`
        const el = document.querySelector('[data-e2e-click="1"]');
        if (!el) return false;
        el.click();
        return true;
      `)
      .catch(() => false);
    if (done === true) {
      domClickFallbacks += 1;
      evidence.push(`控件 ${tid}：原生 click 被拦（${why}）→ 用 DOM click 兜底点下去了`);
      return true;
    }
    return false;
  };
  try {
    await wd.click(el);
    return true;
  } catch (err) {
    // 滚动动画、粘性表头、Hover 才显形的东西偶尔还会挡一下 → 只对"可恢复"的报错重试一次。
    // 其它错误（比如真的被 disabled 覆盖）不重试，原样记下来。
    if (!/intercepted|not interactable|out of bounds|obscured/i.test(wdErrText(err))) {
      evidence.push(`控件 ${tid} 点不动：${wdErrText(err)}`);
      return false;
    }
    await sleep(300);
    try {
      await wd.click(el);
      return true;
    } catch (err2) {
      const why = wdErrText(err2);
      if (await tryDom(why)) return true;
      evidence.push(`控件 ${tid} 点不动（滚进视口、重试一次、DOM 兜底都失败）：${why}`);
      return false;
    }
  }
}

/** 标记 → 点击的公共路径（`row` 非空时按行文本定位）。 */
async function clickBySelector(wd, selector, tid, evidence, row = null) {
  const marked = await markForClick(wd, selector, row ?? {});
  if (marked === null) {
    evidence.push(
      row === null
        ? `缺控件 ${tid}`
        : `找不到行（[data-testid="${row.rowTid}"] 文本含 ${JSON.stringify(row.match)}）里的控件 ${tid}`,
    );
    return false;
  }
  await sleep(150);
  let el = await wd.findElement('[data-e2e-click="1"]');
  if (el === null) {
    // React 重渲染可能把带标记的节点换掉 → 重新标一次。
    // 丢标记是**挂具**的事，不该记成"缺控件"的假红。
    await markForClick(wd, selector, row ?? {});
    await sleep(120);
    el = await wd.findElement('[data-e2e-click="1"]');
  }
  if (el === null) {
    evidence.push(`缺控件 ${tid}（标记过却找不到它，重标一次仍没有）`);
    return false;
  }
  return clickResolved(wd, el, tid, evidence);
}

async function wdClickTid(wd, tid, evidence) {
  await wd.closeMenuIfOpen();
  return clickBySelector(wd, `[data-testid="${tid}"]`, tid, evidence);
}

/** 按行文本定位行内的控件再点（行内 testid 会重复，只按 testid 找会点到第一行）。 */
async function wdClickTidInRow(wd, rowTid, match, tid, evidence) {
  await wd.closeMenuIfOpen();
  return clickBySelector(wd, `[data-testid="${tid}"]`, tid, evidence, { rowTid, match });
}

/** 点开一个下拉并读出弹层里的选项（真机证据：选项文案就是"配置里有什么"）。 */
async function wdOpenSelect(wd, tid, evidence) {
  await wd.closeMenuIfOpen();
  const ctl = await wdControlForTid(wd, tid);
  if (ctl === null) {
    evidence.push(`缺控件 ${tid}`);
    return { ok: false, items: [] };
  }
  try {
    await wd.click(ctl);
  } catch (err) {
    evidence.push(`「${tid}」点不开：${wdErrText(err)}`);
    await wd.closeMenuIfOpen();
    return { ok: false, items: [] };
  }
  await sleep(250);
  let items = await wd.optionItems();
  if (items.length === 0) {
    // 弹层动画 / 选项数据还在飞：再给一小会儿
    await sleep(450);
    items = await wd.optionItems();
  }
  return { ok: true, items };
}

/** MUI Select（单选）：点开控件，挑一个选项（默认第一项；给了 prefer 就按文案挑）。
 *  返回**选中的文案**（选不上返回 null）—— 调用方常要拿它当下一个字段的值。 */
async function wdPickOption(wd, tid, evidence, prefer) {
  const opened = await wdOpenSelect(wd, tid, evidence);
  if (!opened.ok) return null;
  const items = opened.items;
  if (items.length === 0) {
    // **空下拉**：如实记录（配置里确实没有这类实体），然后把弹层收掉 —— 不收掉的话它的
    // backdrop 会让后面每一个控件都报 not interactable，一个根因被放大成一片假失败。
    // 同时把 DOM 现场量出来：菜单有没有开、控件自己显示的是"必填"还是某个值
    // （区分"下拉真的空"与"菜单压根没打开/被挡住"）。
    const dom = await wd
      .exec(`
      const el = document.querySelector('[data-testid="' + ${JSON.stringify(tid)} + '"]');
      return {
        listboxes: document.querySelectorAll('[role="listbox"]').length,
        optionLis: document.querySelectorAll('li[role="option"]').length,
        controlText: el ? (el.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 120) : null,
        dialog: document.querySelector('[role="dialog"]') !== null,
      };
    `)
      .catch(() => null);
    evidence.push(
      `「${tid}」下拉为空（DOM 实测: ${JSON.stringify(dom)}）→ 配置里没有可用选项；收掉弹层继续`,
    );
    await wd.closeMenuIfOpen();
    return null;
  }
  const target = items.find((it) => prefer !== undefined && prefer.test(it.text)) ?? items[0];
  try {
    await wd.click(target.id);
  } catch (err) {
    evidence.push(`选项「${target.text}」点不动：${wdErrText(err)}`);
    await wd.closeMenuIfOpen();
    return null;
  }
  await sleep(200);
  // ★ 单选点完弹层**会自己关**（带退场动画）。先让动画走完再判断：
  //   在它退场的过程中去"收弹层"是**危险**的（拿到的是正在消失的 listbox，
  //   真机实测：多服务商场景下这一下之后整个「添加线路」对话框不见了 → 后面全"缺控件"）。
  if ((await wd.findElement('[role="listbox"]')) !== null && !(await wd.waitGone('[role="listbox"]', 700))) {
    // 700ms 还在 = 不是退场动画（多选就是不关）→ 这时才动手收
    await wd.closeMenuIfOpen();
  }
  evidence.push(`「${tid}」选中「${target.text}」（弹层共 ${items.length} 项）`);
  return target.text;
}

/**
 * 这里原有 `wdKeepOptions` 与 `wdPickOptions` 两个 helper（MUI 多选弹层的"取消已选 / 连点 N 项"）。
 *
 * **第 315 轮删除**：它们唯一的调用方是旧 step11 与旧 step15 里"在 /policies 路由向导上选线路"那几行，
 * 而 `/policies` 整页已按用户裁决删除（[docs/48]）—— 没有调用方了，留着就是死代码。
 * 页面若哪天回来（或别处要用多选弹层），照 git 历史取回即可。
 */

/**
 * 这里原有 `probeSelect` 一个 helper（真点开一个下拉、数 `li[role="option"]`、再收掉弹层，
 * 只用来把"有没有选项"这件事量成证据）。
 *
 * **第 318 轮删除**：它最后的调用方是 step10 里"定价对话框的服务商 / 凭据两个下拉"，
 * 而定价入口已按用户裁决搬到模型页、`match` 只写 `model`（[docs/51]）——
 * 那两个下拉整体消失，它就没有调用方了，留着就是死代码。
 * 下拉探测本身仍有价值（`wdOpenSelect` 还在用），页面若哪天又需要"先量再选"，
 * 照 git 历史取回即可。
 */

/** `config` 表里某个 kind 的实体 id 列表。 */
function configIds(dump, kind) {
  return (dump.config ?? []).filter((r) => r.kind === kind).map((r) => String(r.entity_id));
}

/**
 * `config` 表里某个 kind 的全部实体：`json` 解析结果 + 行的 `entity_id`。
 *
 * 为什么需要它：**界面派生的 id 不能由挂具假设**。两步向导的 `provider.id` 是
 * `deriveProviderId(显示名称)` 算出来的（`src/pages/providers/providerWizardModel.ts`），
 * 挂具手写一个 id 常量只是在断言自己的想象。实机证据（96fae9a 那次 run 的
 * `credentials-add-result`）：显示名称 "TA E2E 供应商" 真正派生出来的 id 是 `ta-e2e`，
 * 而挂具原先断言的是 `ta-e2e-provider` —— 永远不可能相等。
 * 所以：按**名称**去库里找实体，把真实 id 读出来，再拿这个真实 id 往后用。
 */
function configEntities(dump, kind) {
  const out = [];
  for (const row of dump.config ?? []) {
    if (row.kind !== kind) continue;
    try {
      out.push({ entityId: String(row.entity_id), json: JSON.parse(String(row.json)) });
    } catch {
      // json 坏掉的行不进候选；调用方可以从"行数 vs 候选数"看出差异，不静默假装它不存在
    }
  }
  return out;
}

/**
 * **step8：「高级添加」那条单步对话框仍然可用**（docs/19 §1 的裁决：新两步向导挂在
 * `providers-add-open` 上，原样的单步对话框改挂 `providers-add-advanced-open` —— 保留能力、不删控件）。
 * 真正的主流程（两步向导）由 step2 覆盖。
 */
async function step8(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/providers");
  if (page === null) {
    record("step8", "FAIL", "/providers 页面没渲染出来", evidence);
    return;
  }
  const ids = await wd.probeTestIds();
  evidence.push(`/providers 命中 data-testid: ${ids.slice(0, 30).join(", ")}`);
  if (!ids.includes("providers-add-advanced-open")) {
    record("step8", "FAIL", "界面缺入口：providers-add-advanced-open 不存在（高级添加被删了？原先的能力不该丢）", evidence);
    return;
  }
  if (!(await wdClickTid(wd, "providers-add-advanced-open", evidence))) {
    record("step8", "FAIL", "点不开「高级添加」", evidence);
    return;
  }
  await shot(wd, "step8-form-opened");
  const pid = "ta-e2e-p-add";
  await wdFillByTid(wd, "providers-add-id", pid, evidence);
  await wdFillByTid(wd, "providers-add-name", "TA E2E 服务商（高级）", evidence);
  await wdFillByTid(wd, "providers-add-baseUrl", "http://127.0.0.1:1/v1", evidence);
  await shot(wd, "step8-form-filled");
  await wdClickTid(wd, "providers-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "step8-after-submit");
  const dump = dumpScratchDb("step8");
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  const landed = configIds(dump, "provider").includes(pid);
  const inList = (await wd.pageText()).includes(pid);
  if (landed && inList) record("step8", "PASS", "「高级添加」仍可用：单步对话框新增服务商并落库", evidence);
  else record("step8", "FAIL", `新增后不一致：列表=${inList} 落库=${landed}`, evidence);
}

async function step9(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/models");
  if (page === null) {
    record("step9", "FAIL", "/models 页面没渲染出来", evidence);
    return;
  }
  const ids = await wd.probeTestIds();
  evidence.push(`/models 命中 data-testid: ${ids.slice(0, 30).join(", ")}`);
  if (!ids.includes("models-add-open")) {
    record("step9", "SKIP", "界面缺入口：models-add-open 不存在（添加模型向导还没接）", evidence);
    return;
  }
  if (!(await wdClickTid(wd, "models-add-open", evidence))) {
    record("step9", "FAIL", "点不开「添加模型」", evidence);
    return;
  }
  await shot(wd, "step9-form-opened");
  const mid = "ta-e2e-m-add";
  await wdFillByTid(wd, "models-add-id", mid, evidence);
  await wdFillByTid(wd, "models-add-aliases", `${mid}-alias`, evidence);
  // [docs/28 §2] 模型必须**至少分配一条线路**（先到服务商页/通道页把线路建出来）：
  // 零线路时提交会被本地预检挡住 —— 那不是缺陷，是产品规则。
  const lineIds =
    (await wd.exec(
      `return [...document.querySelectorAll('[data-testid^="models-add-line-"] input')].map((el) => el.closest('[data-testid]').getAttribute('data-testid'));`,
    )) ?? [];
  evidence.push(`可选线路（models-add-line-*）：${JSON.stringify(lineIds)}`);
  if (!Array.isArray(lineIds) || lineIds.length === 0) {
    record(
      "step9",
      "FAIL",
      "添加模型对话框里一条可选线路都没有：前面的步骤没造出线路（[docs/28 §2] 要求至少选一条）",
      evidence,
    );
    return;
  }
  await wdClickTid(wd, String(lineIds[0]), evidence);
  await shot(wd, "step9-form-filled");
  await wdClickTid(wd, "models-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "step9-after-submit");
  const dump = dumpScratchDb("step9");
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  const landed = configIds(dump, "model").includes(mid);
  const inList = (await wd.pageText()).includes(mid);
  // 关系落在**模型侧**：`models[].lines` 必须真写上我们勾的那条线路
  const modelRow = configEntities(dump, "model").find((m) => m.entityId === mid);
  const linesWritten = Array.isArray(modelRow?.json?.lines) ? modelRow.json.lines : [];
  const lineOk = linesWritten.includes(String(lineIds[0]).replace("models-add-line-", ""));
  evidence.push(`落库的 lines=${JSON.stringify(linesWritten)}（期望含第 1 条候选）；一致=${lineOk}`);
  if (landed && inList && lineOk)
    record(
      "step9",
      "PASS",
      "模型经真界面（/models 向导）新增：分配了线路、列表与 config 表都出现，且 `lines` 真的写进配置",
      evidence,
    );
  else
    record(
      "step9",
      "FAIL",
      `新增后不一致：列表=${inList} 落库=${landed} lines=${JSON.stringify(linesWritten)} 一致=${lineOk}`,
      evidence,
    );
}

// ================================================================ step10 / step11 的**数据前提**
//
// 「添加线路」「添加路由」两个向导的下拉是**读配置**的：配置里没有的实体，下拉就如实为空 ——
// 这是产品语义，**不是缺陷**。所以这两步开跑前必须确认前提存在：全流程跑（step2…step9）
// 会经界面把 provider / credential / model / lane 都造出来；`--only step10,step11` 这种
// 跳步跑法没有前提，这里就**先经真界面把前提补齐**。全程只用真控件（点 / 填 / 提交），
// 并且把"造了什么、走的哪条界面路径"逐条写进证据。

/** 读 config 表的 kind 计数（决定要不要补前提）。 */
function configKinds(tag) {
  const dump = dumpScratchDb(tag);
  return { dump, kinds: configEntityKinds(dump) };
}

/**
 * 一条 channel **服务哪个逻辑模型**（读 config 行；读不出来返回 null）。
 *
 * [docs/28 §2] 关系已经翻到模型侧（`Model.lines`），所以解析顺序是：
 * 1. **遗留字段** `channel.model`（老配置/迁移前就在这里，仍然有效）；
 * 2. 拿 `dump` 里的 model 行按 `lines` **反查**（新写入的线路没有遗留字段）。
 *
 * 第二个参数可省：省了就只认遗留字段（老调用点行为不变）。
 */
function channelModelOf(row, dump) {
  let ch;
  try {
    // `row.json` 可能是原始行里的**字符串**（`dump.config`），也可能是 `configEntities` 解析好的对象
    // —— 两种形状都收，免得每个调用点都要自己 `JSON.stringify` 一遍（那正是出错的地方）。
    ch = typeof row.json === "string" ? JSON.parse(row.json) : (row.json ?? {});
  } catch {
    return null;
  }
  if (typeof ch.model === "string" && ch.model !== "") return ch.model;
  // `entity_id` 是**原始行**（`dump.config`）的字段名，`entityId` 是 `configEntities()` 解析后的
  // 字段名 —— 两种行都要收。只认前者会让"从 `configEntities` 拿到的行"在这里算出
  // `String(undefined)`，反查永远 miss（step10 真机就是这么红的：model 字段明明是空串、
  // `Model.lines` 里明明有这条线路，却读出 `null`）。
  const id = String(row.entity_id ?? row.entityId ?? "");
  for (const m of dump?.config ?? []) {
    if (m.kind !== "model") continue;
    try {
      const doc = JSON.parse(String(m.json));
      if (Array.isArray(doc.lines) && doc.lines.map(String).includes(id)) return String(m.entity_id);
    } catch {
      // 坏行跳过（与 `configEntities` 同口径：不静默假装它不存在）
    }
  }
  return null;
}

/** 经 /models 的「添加模型」向导真建一个模型（[docs/28 §2]：必须**分配至少一条线路**）。 */
async function addModelViaDialog(wd, evidence, id = "ta-e2e-m-add") {
  const page = await gotoPage(wd, "#/models");
  if (page === null) {
    evidence.push("/models 没渲染出来，建不了模型");
    return false;
  }
  if (!(await wdClickTid(wd, "models-add-open", evidence))) return false;
  await wdFillByTid(wd, "models-add-id", id, evidence);
  await wdFillByTid(wd, "models-add-aliases", `${id}-alias`, evidence);
  // 至少勾一条线路，否则本地预检会（正确地）挡住提交
  const lineIds =
    (await wd.exec(
      `return [...document.querySelectorAll('[data-testid^="models-add-line-"] input')].map((el) => el.closest('[data-testid]').getAttribute('data-testid'));`,
    )) ?? [];
  if (!Array.isArray(lineIds) || lineIds.length === 0) {
    evidence.push("没有可选线路（models-add-line-*）：先建线路再来建模型");
    return false;
  }
  await wdClickTid(wd, String(lineIds[0]), evidence);
  await wdClickTid(wd, "models-add-submit", evidence);
  await sleep(1200);
  const { kinds } = configKinds("prereq-model-write");
  evidence.push(`建模型后 config kind 计数 = ${JSON.stringify(kinds)}`);
  return (kinds.model ?? 0) > 0;
}

/** 经 /network 的「新建 / 覆盖线路」表单真建一条线路（网络未配置时那一区不渲染，如实记录）。 */
async function addLaneViaForm(wd, evidence, id = "ta-e2e-lane", name = "TA E2E 线路") {
  const page = await gotoPage(wd, "#/network");
  if (page === null) {
    evidence.push("/network 没渲染出来，建不了线路");
    return false;
  }
  const ids = await wd.probeTestIds();
  if (!ids.includes("network-lanes-add-id")) {
    evidence.push(
      `/network 没有线路表单（network-* testid: ${ids.filter((t) => t.startsWith("network")).join(", ") || "（无）"}）` +
        "—— 网络未配置时那一区按产品语义不渲染，先要有订阅",
    );
    return false;
  }
  await wdFillByTid(wd, "network-lanes-add-id", id, evidence);
  await wdFillByTid(wd, "network-lanes-add-name", name, evidence);
  await wdClickTid(wd, "network-lanes-add-submit", evidence);
  await sleep(1200);
  const { kinds } = configKinds("prereq-lane-write");
  evidence.push(`建线路后 config kind 计数 = ${JSON.stringify(kinds)}`);
  return (kinds.lane ?? 0) > 0;
}

/**
 * 补齐 step10 / step11 的前提：provider + credential（首启向导一次 apply —— 界面上**只有**
 * 它带密钥字段，密钥会派生 `<provider>-default` 凭据）、model（/models 向导）、lane（/network 表单）。
 */
async function ensureAddPrereqs(wd, fixture, evidence) {
  const { kinds } = configKinds("prereq-before");
  evidence.push(`前提探测：config kind 计数 = ${JSON.stringify(kinds)}`);
  const made = [];
  if ((kinds.provider ?? 0) === 0 || (kinds.credential ?? 0) === 0) {
    const wiz = await runOnboardingWizard(wd, fixture, evidence);
    if (!wiz.ok) {
      evidence.push(`首启向导也不可用：${wiz.reason}`);
      return { ok: false, made };
    }
    made.push("provider+credential（首启向导：真表单 + 真 apply）");
  }
  const s1 = configKinds("prereq-provider");
  if ((s1.kinds.model ?? 0) === 0) {
    if (await addModelViaDialog(wd, evidence)) made.push("model（/models 向导 models-add-*）");
  }
  const s2 = configKinds("prereq-model");
  if ((s2.kinds.lane ?? 0) === 0) {
    if (await addLaneViaForm(wd, evidence)) made.push("lane（/network「新建 / 覆盖线路」表单 network-lanes-add-*）");
  }
  const final = configKinds("prereq-after");
  evidence.push(`前提补齐动作: ${made.length > 0 ? made.join("；") : "不需要（配置里已经有了）"}`);
  evidence.push(`前提补齐后 config kind 计数 = ${JSON.stringify(final.kinds)}`);
  const ok =
    (final.kinds.provider ?? 0) > 0 && (final.kinds.credential ?? 0) > 0 && (final.kinds.model ?? 0) > 0;
  return { ok, made, kinds: final.kinds };
}

/**
 * MUI Switch / Checkbox 的 `data-testid` 落在**根 span** 上，内层才是 `input[type=checkbox]`
 * （与 `/models` 的 `models-add-expose` 同形状）。`wdFillByTid` 对 checkbox 无效 —— 必须真点。
 * 按"当前值 → 目标值"点，点完**回读真实 checked** 作为证据（不靠假设）。
 */
async function wdSetSwitch(wd, tid, target, evidence) {
  const read = () =>
    wd.exec(`
      const host = document.querySelector('[data-testid="' + ${JSON.stringify(tid)} + '"]');
      if (host === null) return { found: false };
      const input = host.matches('input') ? host : host.querySelector('input[type="checkbox"],input');
      return { found: true, checked: input === null ? null : input.checked === true };
    `);
  const st = await read();
  if (st === null || st.found !== true) {
    evidence.push(`开关 ${tid}: 不存在（能力块没接？）`);
    return false;
  }
  if (st.checked === target) {
    evidence.push(`开关 ${tid}: 已经是 ${target ? "开" : "关"}（无需点）`);
    return true;
  }
  const el = await wd.findElement(`[data-testid="${tid}"]`);
  if (el === null) {
    evidence.push(`开关 ${tid}: 找不到元素`);
    return false;
  }
  try {
    await wd.click(el);
  } catch (err) {
    evidence.push(`开关 ${tid} 点不动：${String(err.message ?? err).slice(0, 120)}`);
    return false;
  }
  await sleep(250);
  const after = await read();
  evidence.push(`开关 ${tid}: 点一次后 checked=${JSON.stringify(after === null ? null : after.checked)}（目标 ${target}）`);
  return after !== null && after.checked === target;
}

/**
 * 勾/取消一个 MUI 复选框（`data-testid` 在**根 span** 上，真正挂 `onChange` 的是里面的
 * `input[type=checkbox]`），点完**回读真实 checked**。
 *
 * 为什么不能只用 `wdClickTid`：真机实测（step21 批量删除的表头「全选」）原生 click 被拦
 * （`element click intercepted`），`clickResolved` 的 DOM 兜底点的是那个 **span** ——
 * 而 `span.click()` 不会触发挂在 `input` 上的 React `onChange`，于是"点过了"却一条都没勾上
 * （计数区原文还是「已选 0 条」，库里当然一条没少）。**这不是产品缺陷，是点错了元素**。
 * 所以：① 先按用户的方式点 testid；② 回读不对就退回点里面的 `input`（`input.click()` 会真的触发 change）。
 */
async function wdToggleCheckbox(wd, tid, target, evidence) {
  const read = () =>
    wd.exec(`
      const host = document.querySelector('[data-testid="' + ${JSON.stringify(tid)} + '"]');
      if (host === null) return { found: false };
      const input = host.matches('input') ? host : host.querySelector('input[type="checkbox"],input');
      return { found: true, checked: input === null ? null : input.checked === true };
    `);
  const want = target === true;
  const st = await read();
  if (st === null || st.found !== true) {
    evidence.push(`复选框 ${tid}: 不存在`);
    return false;
  }
  if (st.checked === want) {
    evidence.push(`复选框 ${tid}: 已经是「${want ? "勾上" : "取消"}」（无需点）`);
    return true;
  }
  await wdClickTid(wd, tid, evidence);
  await sleep(250);
  let after = await read();
  if (after !== null && after.checked === want) {
    evidence.push(`复选框 ${tid}: 点 testid 后回读 checked=${after.checked}（=目标）`);
    return true;
  }
  const done = await wd
    .exec(`
      const host = document.querySelector('[data-testid="' + ${JSON.stringify(tid)} + '"]');
      if (host === null) return false;
      const input = host.matches('input') ? host : host.querySelector('input[type="checkbox"],input');
      if (input === null) return false;
      input.click();
      return true;
    `)
    .catch(() => false);
  if (done === true) {
    evidence.push(`复选框 ${tid}: testid 那一下没生效 → 直接点了里面的 input（onChange 挂在它上面）`);
  }
  await sleep(250);
  after = await read();
  evidence.push(
    `复选框 ${tid}: 目标「${want ? "勾上" : "取消"}」→ 回读 checked=${JSON.stringify(after === null ? null : after.checked)}`,
  );
  return after !== null && after.checked === want;
}

/**
 * 这里原有 `providerCredentialPair` 一个 helper（从 config 里挑一对真的成对存在的
 * `(provider, credential)`，用来驱动两个下拉）。
 *
 * **第 318 轮删除**：它最后的调用方是 step10 里"定价对话框的服务商 / 凭据两个下拉"，
 * 而 [docs/51] 之后这两个下拉整体消失（`match` 只写 `model`，用户不再挑服务商/线路）——
 * 没有调用方了，留着就是死代码。**顺带删掉的还有 `reEsc`**（把文案当字面量转义的助手），
 * 它当时也只被那几行 `wdPickOption(..., new RegExp(reEsc(...)))` 用。
 *
 * `wdPickOption` / `wdOpenSelect` 本身**还在用**（网络页的线路下拉），一行不动。
 * 页面若哪天又需要"挑一对服务商/凭据"，照 git 历史取回即可。
 */

/**
 * step10：**在模型页给一个对外模型定价**（[docs/51]）。
 *
 * **这一步被换过两次用途**，每次都是跟着产品一起搬：
 * 1. 原点："/channels 的线路添加向导：能力声明 + 计费落地"——那个向导所在的
 *    `/channels`（导航「套餐与线路」）整页已按用户裁决删除（[docs/49]），能力声明随页消失
 *    （改由**扫描自动认领上游声明**），于是这一步改成"经成本中心新增定价"；
 * 2. 用户第 318 轮：*"我是希望用户为每一个对外模型定价，来算自己大概花了多少，
 *    而不是对服务商的线路计价"* + *"成本中心页面只做显示，价格配置到模型页面去"*。
 *    于是定价入口搬到 `/models` 的每一行，成本中心只剩只读展示（[docs/51]）。
 *    覆盖度是**换**不是减。
 *
 * 验四件事：
 * 1. 成本中心**没有**任何写入口（`pricing-add-*` / `pricing-delete` 一个都不许在）——
 *    这条是"只做显示"这句话的可执行形式；
 * 2. `/models` 那一行的定价入口打得开，且**模型是只读展示的**（用户不填模型名）；
 * 3. 三项全空 → 本地点名、**一条命令都不发**（空费率条目会把「没定价」写成「免费」）；
 *    非法数字同样逐字段点名；
 * 4. 填一项价 → `pricing_upsert` 真的落库，且落库的 `match` 里**只有 model**
 *    （绝不带 provider/credential —— 那正是"对线路计价"的旧形状）。
 *
 * 真机驱动：这一行的入口 testid 是**每行一个**，所以要按行定位
 * （`wdClickTidInRow`），不能只按 testid 去点第一行。
 */
async function step10(wd, fixture) {
  const evidence = [];
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step10", "SKIP", "界面缺前提：配置里没有服务商/凭据/模型，且经界面也造不出来（见证据）", evidence);
    return;
  }

  const before = configKinds("step10-before").dump;
  const models = configEntities(before, "model");
  evidence.push(`前提：models=${models.length}`);
  if (models.length === 0) {
    record("step10", "SKIP", "配置里没有模型 → 模型页没有行，也就没有定价入口（数据前提，不是缺陷）", evidence);
    return;
  }
  // 取一个模型 id：定价的目标就是**这一行**的模型，所以夹具也要按行定位。
  const priced = String(models[0].entityId ?? models[0].entity_id ?? "");
  if (priced === "") {
    record("step10", "SKIP", "配置里的模型行读不出 entity_id（夹具问题，不是产品缺陷）", evidence);
    return;
  }
  evidence.push(`用这一行的模型：${JSON.stringify(priced)}`);

  // ---- ① 成本中心：只做显示，写入口一个都不许在 ----
  const costPage = await gotoPage(wd, "#/cost", '[data-testid="cost-pricing"]');
  if (costPage === null) {
    record("step10", "FAIL", "/cost 的定价区块没渲染出来", evidence);
    return;
  }
  const costIds = await wd.probeTestIds();
  const costPricingIds = costIds.filter((t) => t.startsWith("pricing-"));
  const writeIds = pricingWriteEntries(costIds);
  evidence.push(`/cost 上 pricing-* 命中: ${costPricingIds.join(", ") || "（无）"}`);
  evidence.push(`/cost 上写入口（pricing-add* / pricing-delete / pricing-edit）: ${JSON.stringify(writeIds)}`);
  if (writeIds.length > 0) {
    record("step10", "FAIL", `成本中心必须只做显示，但它上面还有写入口：${JSON.stringify(writeIds)}`, evidence);
    return;
  }
  // 只读这件事必须是**看得见的一句话**（用户会来这里找那个已经被搬走的按钮）
  const readonlyNote = await wd.textOf('[data-testid="cost-pricing-readonly"]');
  evidence.push(`cost-pricing-readonly 原文: ${JSON.stringify(readonlyNote)}`);
  if (readonlyNote === null || String(readonlyNote).trim() === "") {
    record("step10", "FAIL", "成本中心的定价区块没有「改在哪儿」的说明（cost-pricing-readonly 空）", evidence);
    return;
  }

  // ---- ② 模型页：那一行的定价入口打得开，且模型是只读展示 ----
  const page = await gotoPage(wd, "#/models", '[data-testid="models-row"]');
  if (page === null) {
    record("step10", "FAIL", "/models 渲染后没等到 models-row", evidence);
    return;
  }
  const modelIds = await wd.probeTestIds();
  evidence.push(
    `/models 上 models-price-* 命中: ${modelIds.filter((t) => t.startsWith("models-price-")).join(", ") || "（无）"}`,
  );
  if (!(await wdClickTidInRow(wd, "models-row", [priced], "models-price-open", evidence))) {
    record("step10", "FAIL", `点不开模型行 ${priced} 的定价入口（models-price-open）`, evidence);
    return;
  }
  await sleep(600);

  // 控件齐全性：三个价格输入 + 提交/取消。**没有**模型输入框（那是只读展示）。
  const wantTids = [
    "models-price-model",
    "models-price-input",
    "models-price-output",
    "models-price-cached",
    "models-price-submit",
    "models-price-cancel",
  ];
  const openIds = await wd.probeTestIds();
  const missing = wantTids.filter((t) => !openIds.includes(t));
  evidence.push(`打开对话框后：应有 ${wantTids.length} 个控件，缺 ${JSON.stringify(missing)}`);
  if (missing.length > 0) {
    record("step10", "FAIL", `定价对话框缺控件（${missing.join(", ")}）`, evidence);
    return;
  }
  const shownModel = await wd.textOf('[data-testid="models-price-model"]');
  evidence.push(`models-price-model 原文: ${JSON.stringify(shownModel)}`);
  // 只读：那一格下面不许出现输入框（能打字 = 用户可以拼错模型名 / 写别名）
  const modelBoxes = await wd.exec(
    `return document.querySelectorAll('[data-testid="models-price-model"] input, [data-testid="models-price-model"] textarea').length;`,
  );
  evidence.push(`模型只读格里的输入框数 = ${JSON.stringify(modelBoxes)}（期望 0）`);
  if (shownModel === null || !String(shownModel).includes(priced) || Number(modelBoxes) !== 0) {
    record(
      "step10",
      "FAIL",
      `模型必须是**只读展示**且就是这一行的模型（原文=${JSON.stringify(shownModel)} 输入框数=${JSON.stringify(modelBoxes)}）`,
      evidence,
    );
    return;
  }

  // ---- ③ 三项全空 → 本地点名，一条命令都不发（不写"0 价"的假象）----
  const pricingSectionBefore = configSectionOf(before, "pricing");
  await wdClickTid(wd, "models-price-submit", evidence);
  await sleep(500);
  const afterEmpty = configKinds("step10-empty-submit").dump;
  const noPriceIssue = (await wd.probeTestIds()).includes("models-price-issue-none");
  const unchangedOnEmpty = configSectionOf(afterEmpty, "pricing") === pricingSectionBefore;
  evidence.push(`三项全空提交后：点名元素存在=${noPriceIssue}，配置未变=${unchangedOnEmpty}`);
  if (!noPriceIssue || !unchangedOnEmpty) {
    record(
      "step10",
      "FAIL",
      `三项全空时必须本地点名且一条命令都不发（点名=${noPriceIssue} 未变=${unchangedOnEmpty}）`,
      evidence,
    );
    return;
  }

  // ---- ④ 非法数字 → 逐字段点名，同样不发 ----
  await wdFillByTid(wd, "models-price-input", "-1", evidence);
  await wdClickTid(wd, "models-price-submit", evidence);
  await sleep(500);
  const afterBad = configKinds("step10-bad-submit").dump;
  const badIssue = (await wd.probeTestIds()).includes("models-price-issue-input");
  const unchangedOnBad = configSectionOf(afterBad, "pricing") === pricingSectionBefore;
  evidence.push(`非法价提交后：点名元素存在=${badIssue}，配置未变=${unchangedOnBad}`);
  if (!badIssue || !unchangedOnBad) {
    record("step10", "FAIL", `非法价格必须本地点名且不发（点名=${badIssue} 未变=${unchangedOnBad}）`, evidence);
    return;
  }
  // 清掉非法值，走一条正常的
  await wdFillByTid(wd, "models-price-input", "", evidence);

  // ---- ⑤ 填一项价 → 真的落库，且 match 里只有 model ----
  await wdFillByTid(wd, "models-price-input", "1.5", evidence);
  await wdClickTid(wd, "models-price-submit", evidence);
  await sleep(1500);
  await shot(wd, "step10-pricing-submitted");

  const after = configKinds("step10-after").dump;
  const pricingJson = configSectionOf(after, "pricing");
  const landed = /"input"\s*:\s*1\.5/.test(pricingJson);
  evidence.push(`定价 section 里有 input=1.5: ${landed}（原文片段: ${JSON.stringify(pricingJson.slice(0, 400))}）`);
  const errorText = await wd.textOf('[data-testid="models-price-error"]');
  if (errorText !== null) evidence.push(`models-price-error 原文: ${JSON.stringify(errorText)}`);

  // 落库条目里 `match` **只有 model** —— 这是本轮修正的核心：
  // 带上 provider/credential 就成了"对服务商的线路计价"，那正是用户否掉的形状。
  //
  // ⚠️ **判据是 `== null`（null 或不存在），不是 `=== undefined`**。
  // 第 318 轮真机上这一条先红了一次，原因是挂具把"没写这个键"当成了唯一合格形状：
  // `PriceMatch` 的三个字段是 `Option<T>` + `#[serde(default)]`（Rust 侧
  // `crates/ta-core/src/domain/mod.rs`），**落库时会把 None 写成显式的 `null`** ——
  // 整份配置里到处是这个习惯（同一行就能看到 `"usd_to_cny":null`）。
  // 而引擎读 `null` 与读"键不存在"**是同一件事**：`Option` 反序列化后都是 `None`，
  // `matches_request` 用 `m.provider.as_ref().map(..).unwrap_or(true)` 判断 —— 两种形状都不构成过滤。
  // 所以"键不存在"是**更严于语义要求**的判据，它判的是序列化习惯，不是定价语义。已改成 `== null`。
  let matchOk = false;
  let matchSeen = null;
  try {
    const parsed = JSON.parse(pricingJson);
    const entries = Array.isArray(parsed?.entries) ? parsed.entries : [];
    const ours = entries.find((e) => e?.match?.model === priced);
    matchSeen = ours?.match ?? null;
    matchOk = judgePricedMatch(matchSeen, priced).ok;
  } catch (err) {
    evidence.push(`定价 section 解析失败：${String(err.message ?? err).slice(0, 160)}`);
  }
  evidence.push(`落库条目 match = ${JSON.stringify(matchSeen)}（期望只有 model，且等于那一行的模型）`);

  if (!landed) {
    record("step10", "FAIL", "填了价、点了保存，但 config 的 pricing section 里没有那条费率", evidence);
    return;
  }
  if (!matchOk) {
    record(
      "step10",
      "FAIL",
      `费率落库了，但 match 不是「只有 model」的形状（实得 ${JSON.stringify(matchSeen)}）`,
      evidence,
    );
    return;
  }
  record(
    "step10",
    "PASS",
    `模型页定价：成本中心无写入口且写明去处；模型只读；空价与非法价本地点名且不发；正常一条真的落库且 match 只有 model（${priced} input=1.5）`,
    evidence,
  );
}

/** 读 config 里某个 section 的原文（读不到返回空串 —— 调用方据此判断"变没变"）。 */
function configSectionOf(dump, entityId) {
  const row = (dump.config ?? []).find(
    (r) => r.kind === "section" && String(r.entity_id) === entityId,
  );
  return row === undefined ? "" : String(row.json);
}

/**
 * step11：**模型开关与线路开关**（[docs/48 §5]）。
 *
 * 这一步**替换**掉了原来那个"路由向导能力一致性"的 step11：
 * `/policies`（路由策略）整页已按用户裁决删除（「功能是重复的，一起融入模型目录里面去」），
 * 所以旧断言没有对象了。但**不是简单删掉** —— 用户这一轮真正要的是**两个开关**，
 * 所以这里改成验那两个开关，覆盖度是**换**而不是减。
 *
 * 验的是三件事，缺一不可：
 * 1. **模型开关真的写进配置**（`enabled` 落库为 false）—— 点一下不能只是界面好看；
 * 2. **模型开关真的对外生效**：关掉之后网关 `/v1/models` 不再列出它、按名字取 404
 *    （这是用户能感知的那一面，光有"落库"证明不了它有用）；
 * 3. **线路开关也能落库**（`channel.enabled` 翻成 false），且**先弹确认框**。
 *
 * 网关不可达时第 2 条如实记进证据、**不假报通过**（别的步骤已记录过这种情况）。
 */
async function step11(wd, fixture) {
  const evidence = [];
  const GW = "http://127.0.0.1:8787";
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step11", "SKIP", "界面缺前提：配置里没有服务商/凭据/模型，且经界面也造不出来（见证据）", evidence);
    return;
  }

  const page = await gotoPage(wd, "#/models", '[data-testid="models-summary"]');
  if (page === null) {
    record("step11", "FAIL", "/models 页面没渲染出来", evidence);
    return;
  }
  const ids = await wd.probeTestIds();
  evidence.push(`/models 命中 data-testid: ${ids.slice(0, 40).join(", ")}`);
  for (const need of ["models-enabled", "models-line-enabled"]) {
    if (!ids.includes(need)) {
      record("step11", "FAIL", `界面上没有 ${need}（[docs/48 §5] 的两个开关没接上）`, evidence);
      return;
    }
  }

  // 取一个模型 id：关掉它之后要按名字去问网关（"关掉 = 调不通"必须真的成立）
  const dump0 = dumpScratchDb("step11-before");
  const models0 = (dump0.config ?? []).filter((r) => r.kind === "model");
  const target = models0.map((r) => String(r.entity_id)).find((id) => id !== "") ?? null;
  evidence.push(`config(kind=model) 共 ${models0.length} 条，取 target=${JSON.stringify(target)}`);
  if (target === null) {
    record("step11", "FAIL", "配置里没有模型，无法验模型开关", evidence);
    return;
  }
  const beforeList = await gatewayModels(GW, evidence);

  // ① 关掉模型开关
  if (!(await wdClickTid(wd, "models-enabled", evidence))) {
    record("step11", "FAIL", "点不动模型开关（models-enabled）", evidence);
    return;
  }
  await sleep(1500);
  await shot(wd, "step11-model-off");
  let enabled1 = modelEnabledIn(dumpScratchDb("step11-model-off"), target, evidence);
  evidence.push(`关掉后 config(model ${target}).enabled = ${JSON.stringify(enabled1)}`);
  if (enabled1 !== false) {
    record("step11", "FAIL", `点了模型开关但落库的 enabled !== false（实得 ${JSON.stringify(enabled1)}）`, evidence);
    return;
  }

  // ② 对外生效：关掉的模型不该出现在 /v1/models，按名字取应当 404
  let effectNote;
  if (beforeList === null) {
    effectNote = "网关不可达 → 「关掉后对外不列出 / 按名字 404」这一条**未验证**（如实记录，不算通过）";
    evidence.push(effectNote);
  } else {
    const afterList = await gatewayModels(GW, evidence);
    const listedAfter = afterList !== null && afterList.includes(`"${target}"`);
    evidence.push(`关掉后 /v1/models 是否仍列出 ${target}：${listedAfter}`);
    const byName = await gatewayGet(GW, `/v1/models/${encodeURIComponent(target)}`, evidence);
    evidence.push(`关掉后 GET /v1/models/${target} → status=${byName.status}`);
    if (listedAfter) {
      record("step11", "FAIL", `模型已停用但网关仍在 /v1/models 里列出它（${target}）`, evidence);
      return;
    }
    if (byName.status !== 404) {
      record("step11", "FAIL", `模型已停用但按名字取不是 404（实得 ${byName.status}）`, evidence);
      return;
    }
    effectNote = "关掉后确实：清单不列出 + 按名字 404";
  }

  // ③ 打开回来（开关必须**可逆**，不然用户只能删了重建 —— 那正是他要避免的）
  await gotoPage(wd, "#/models", '[data-testid="models-summary"]');
  if (!(await wdClickTid(wd, "models-enabled", evidence))) {
    record("step11", "FAIL", "打开模型的开关点不动", evidence);
    return;
  }
  await sleep(1500);
  const enabled2 = modelEnabledIn(dumpScratchDb("step11-model-on"), target, evidence);
  evidence.push(`再打开后 config(model ${target}).enabled = ${JSON.stringify(enabled2)}`);
  if (enabled2 !== true) {
    record("step11", "FAIL", `开关不可逆：复位后 enabled !== true（实得 ${JSON.stringify(enabled2)}）`, evidence);
    return;
  }

  // ④ 线路开关：**先弹确认框**，确认后才落库（线路是共用的，不能悄悄改）
  const dump3 = dumpScratchDb("step11-channel-before");
  const channelIds = (dump3.config ?? [])
    .filter((r) => r.kind === "channel")
    .map((r) => String(r.entity_id));
  const line = channelIds.find((id) => id !== "") ?? null;
  evidence.push(`候选线路 ${channelIds.length} 条，取 line=${JSON.stringify(line)}`);
  if (line === null) {
    record("step11", "FAIL", "配置里没有线路，无法验线路开关", evidence);
    return;
  }
  await gotoPage(wd, "#/models", '[data-testid="models-summary"]');
  if (!(await wdClickTid(wd, "models-line-enabled", evidence))) {
    record("step11", "FAIL", "点不动线路开关（models-line-enabled）", evidence);
    return;
  }
  await sleep(600);
  const dlgIds = await wd.probeTestIds();
  if (!dlgIds.includes("models-line-toggle-dialog")) {
    record("step11", "FAIL", "点线路开关没有先弹确认框（[docs/48 §5]：线路是共用的，必须先问）", evidence);
    return;
  }
  const sharedText = await wd.textOf('[data-testid="models-line-toggle-shared"]');
  evidence.push(`确认框里的共用者原文: ${JSON.stringify(sharedText)}`);
  await shot(wd, "step11-line-confirm");
  if (!(await wdClickTid(wd, "models-line-toggle-confirm", evidence))) {
    record("step11", "FAIL", "确认框里点不动「确认」", evidence);
    return;
  }
  await sleep(1500);
  const dump4 = dumpScratchDb("step11-channel-off");
  const chRow = (dump4.config ?? []).find((r) => r.kind === "channel" && String(r.entity_id) === line);
  let chEnabled = null;
  try {
    chEnabled = JSON.parse(String(chRow?.json ?? "{}")).enabled;
  } catch (err) {
    evidence.push(`解析 ${line} 的 json 失败: ${String(err.message ?? err).slice(0, 80)}`);
  }
  evidence.push(`确认后 config(channel ${line}).enabled = ${JSON.stringify(chEnabled)}`);
  if (chEnabled !== false) {
    record("step11", "FAIL", `线路开关确认后没落库（channel ${line}.enabled 实得 ${JSON.stringify(chEnabled)}）`, evidence);
    return;
  }

  record("step11", "PASS", `模型开关落库可逆 + 线路开关走确认框落库；${effectNote}`, evidence);
}

/** 从一份 config dump 里读某个模型的 `enabled`（解析失败返回 `null`，并留证据）。 */
function modelEnabledIn(dump, modelId, evidence) {
  const row = (dump.config ?? []).find((r) => r.kind === "model" && String(r.entity_id) === modelId);
  if (row === undefined) {
    evidence.push(`dump 里找不到模型 ${modelId}`);
    return null;
  }
  try {
    return JSON.parse(String(row.json ?? "{}")).enabled;
  } catch (err) {
    evidence.push(`解析模型 ${modelId} 的 json 失败: ${String(err.message ?? err).slice(0, 80)}`);
    return null;
  }
}

/** 读网关 `/v1/models` 的正文；不可达返回 `null`（调用方据此如实记录，**不假报通过**）。 */
async function gatewayModels(GW, evidence) {
  try {
    const res = await fetch(`${GW}/v1/models`);
    const body = await res.text();
    evidence.push(`GET /v1/models → ${res.status}，正文前 160 字: ${body.slice(0, 160)}`);
    return res.status === 200 ? body : null;
  } catch (err) {
    evidence.push(`GET /v1/models 失败（网关可能没在监听）: ${String(err.message ?? err).slice(0, 120)}`);
    return null;
  }
}

/** 读网关任一路径的状态码；不可达返回 `{status: 0}`。 */
async function gatewayGet(GW, path, evidence) {
  try {
    const res = await fetch(`${GW}${path}`);
    await res.text();
    return { status: res.status };
  } catch (err) {
    evidence.push(`GET ${path} 失败: ${String(err.message ?? err).slice(0, 120)}`);
    return { status: 0 };
  }
}

// ---------------------------------------------------------------- step14 数据安全回归

/**
 * **数据安全回归：设置页「校验并应用」之后，之前建的线路必须还在。**
 *
 * 为什么必须有这一步（真机 E2E 抓到过用户可见的数据丢失）：
 * `config_apply` 是**整份覆盖**（`config_store.rs::write_rows` 先 `DELETE FROM config` 再按传入的
 * `ConfigDoc` 插入），而设置页是"读到什么就整份写回什么"。如果前端手里那份 `qk.config` 是旧的
 * （`staleTime: 5_000`），提交上去的 doc 里就没有刚建的线路 → 线路被**静默删掉**。
 * 原始铁证（`config_snapshot` 表）：label=`lane_upsert` 的 doc 有 1 条 lane，label=`apply` 的有 0 条。
 *
 * 红先绿后：修复前这一步必然 FAIL（线路消失）；修复后必须 PASS。**不许放宽**成"差异可接受"。
 */
async function step14(wd) {
  const evidence = [];
  // 前提：至少有一条线路（经真界面造，不依赖前面步骤跑过）。
  let { dump } = configKinds("step14-before");
  if ((dump.config ?? []).filter((r) => r.kind === "lane").length === 0) {
    evidence.push("配置里还没有线路 → 先经 /network 的「新建 / 覆盖线路」表单建一条");
    if (!(await addLaneViaForm(wd, evidence))) {
      record("step14", "FAIL", "造不出线路（界面缺入口或写入失败），无法验证数据安全", evidence);
      return;
    }
    ({ dump } = configKinds("step14-lane-made"));
  }
  const lanesBefore = (dump.config ?? []).filter((r) => r.kind === "lane");
  const laneIds = lanesBefore.map((r) => String(r.entity_id));
  evidence.push(`应用前 config(kind=lane) ${lanesBefore.length} 条: ${JSON.stringify(laneIds)}`);

  // 真界面：设置页改一个字段并点「校验并应用」（与 step6 同一条路径）
  const page = await gotoPage(wd, "#/settings", '[data-testid="settings-page"]');
  if (page === null) {
    record("step14", "FAIL", "/settings 没渲染出来", evidence);
    return;
  }
  const before = await wd.attrOf('[data-testid="settings-field-autostartDelay"]', "value");
  const target = before === "37" ? "38" : "37";
  await wdFillByTid(wd, "settings-field-autostartDelay", target, evidence);
  await sleep(300);
  const dirty = await wd.textOf('[data-testid="settings-dirty"]');
  evidence.push(`settings-dirty: ${JSON.stringify((dirty ?? "").replace(/\s+/g, " ").trim())}`);
  await wdClickTid(wd, "settings-apply", evidence);
  await sleep(1500);
  const applyResult = await wd.textOf('[data-testid="settings-apply-result"]');
  evidence.push(`settings-apply-result: ${JSON.stringify((applyResult ?? "").replace(/\s+/g, " ").trim())}`);
  await shot(wd, "step14-after-apply");

  const after = configKinds("step14-after");
  const lanesAfter = (after.dump.config ?? []).filter((r) => r.kind === "lane");
  const idsAfter = lanesAfter.map((r) => String(r.entity_id));
  evidence.push(`应用后 config(kind=lane) ${lanesAfter.length} 条: ${JSON.stringify(idsAfter)}`);
  evidence.push(
    `应用后 config kind 计数: ${JSON.stringify(configEntityKinds(after.dump))}（对比应用前: ${JSON.stringify(configEntityKinds(dump))}）`,
  );

  // 列表这一侧也要看：线路表里那一行还在不在
  await gotoPage(wd, "#/network");
  const rows = await wd.exec(
    `return document.querySelectorAll('[data-testid="network-lane-row"]').length;`,
  );
  evidence.push(`回到 /network：network-lane-row 行数 = ${rows}`);

  const survived = laneIds.every((id) => idsAfter.includes(id));
  if (survived && lanesAfter.length >= lanesBefore.length) {
    record("step14", "PASS", `设置页「校验并应用」之后线路仍在（${idsAfter.join(", ")}）；列表行数 ${rows}`, evidence);
  } else {
    record(
      "step14",
      "FAIL",
      `应用后线路丢了：应用前 ${JSON.stringify(laneIds)} → 应用后 ${JSON.stringify(idsAfter)}`,
      evidence,
    );
  }
}

// ---------------------------------------------------------------- step15 能力规则的强制面

async function step15(wd, fixture) {
  const evidence = [];
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step15", "SKIP", "界面缺前提：服务商/凭据/模型都造不出来", evidence);
    return;
  }

  // ------------------------------------------------------------------
  // **这一步现在如实标 SKIP**（第 315 轮），原因是**界面路径被用户裁决删掉了**：
  //
  // 它原来验的是「在 /policies 的路由向导里，两条线路能力不一致时，
  // 界面必须显示逐字段差异并禁用提交」。而 `/policies`（路由策略）整页
  // 已按用户裁决删除 —— 用户的原话是「功能是重复的，一起融入模型目录里面去」。
  // 页面没了，这条 UI 断言就**没有对象**了。
  //
  // 为什么是 SKIP 而不是 PASS：我们**没有**验证过任何东西，
  // 写成 PASS 就是假绿。为什么是 SKIP 而不是 FAIL：
  // 代码没坏，是界面入口按裁决消失了 —— 环境（界面）提供不了这个条件。
  //
  // **顺手把边界说清**（别让下一个人以为"能力一致性校验"整个没了）：
  // * 后端那条规则**仍在且仍被测**：`ta-core` 的校验器与 `ta-gateway` 的用例都覆盖了
  //   "同一策略下多条线路能力必须一致"（这里是 UI 层没有入口，不是规则下线）；
  // * 本步骤原先把"经界面造出路由"当作前提，而**现在没有任何界面能建 `channel_group` /
  //   `policy`** —— 这是删除两个页面后的**真实后果**，一并记在这里。
  // ------------------------------------------------------------------
  const dump = configKinds("step15").dump;
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  evidence.push(
    "SKIP 原因：/policies（路由策略）整页已按用户裁决删除，能力不一致的 UI 断言没有对象；" +
      "后端「同策略多线路能力必须一致」的规则仍在 ta-core 校验器与 ta-gateway 用例里被覆盖。",
  );
  evidence.push(
    "顺带记录的后果：删除 /policies 与 /routing 之后，界面上**已没有任何入口**能新建 channel_group / policy。",
  );
  record("step15", "SKIP", "UI 路径已随 /policies 删除；后端规则另有覆盖（见证据）", evidence);
}

// ---------------------------------------------------------------- step17 节点清单

/**
 * **step17：/network 的节点清单真的出数据**（用户第 71 轮最直接的不满：「没有节点啊」）。
 *
 * 事实（已核实，别再猜）：`subscription_nodes({ id })`（不带 group）走该订阅**已采纳的解析结果**
 * （`net.rs:774-775`）；而**未解析**的订阅会让后端**报错**（不是返回空数组），
 * 所以"还没解析过"这一档必须由页面的**前置判定**渲染（用 `subscription_list` 的
 * `last_update_ms` / `error`），而不是等命令报错。
 *
 * 本步骤同时覆盖：真节点出现（fixture 里的 `ta-e2e-node-hk`）、刷新后仍在、以及
 * **不许有**"点了会报错的组筛选器"（本轮 `group` 参数在后端是硬错误，所以页面不放它）。
 */
async function step17(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/network/nodes", '[data-testid="nodes-list"], [data-testid="nodes-empty"], [data-testid="nodes-error"]');
  if (page === null) {
    record("step17", "FAIL", "/network/nodes 既没有节点清单也没有空态/错误区（页面没渲染出任何一种状态）", evidence);
    return;
  }
  const tids = await wd.probeTestIds();
  evidence.push(`/network/nodes 命中 data-testid: ${tids.slice(0, 35).join(", ")}`);
  const err = await wd.textOf('[data-testid="nodes-error"]');
  if (err !== null) evidence.push(`nodes-error 原文: ${JSON.stringify(err.replace(/\s+/g, " "))}`);
  const empty = await wd.textOf('[data-testid="nodes-empty"]');
  if (empty !== null) evidence.push(`nodes-empty 原文: ${JSON.stringify(empty.replace(/\s+/g, " "))}`);

  // 先点一次「刷新」：拉一次订阅 → 触发解析 → 节点清单才有东西可读
  if (tids.includes("nodes-refresh")) {
    await wdClickTid(wd, "nodes-refresh", evidence);
    await sleep(3000);
  } else {
    evidence.push("这一页没有 nodes-refresh（没法主动拉订阅解析结果）");
  }

  const rows = await wd.exec(`return document.querySelectorAll('[data-testid="nodes-row"]').length;`);
  const text = await wd.pageText();
  const hasFixtureNode = text.includes("ta-e2e-node-hk");
  // 每屏都要有真机截图（[docs/30] 的验收要求）：这一屏以前没截过，补上。
  await shot(wd, "step17-nodes");
  evidence.push(`刷新后 nodes-row 行数 = ${rows}；页面文本里出现 fixture 节点 ta-e2e-node-hk = ${hasFixtureNode}`);
  evidence.push(
    `本轮**不放**组筛选器（后端带 group 是硬错误：net.rs:765-773 要求先给 ta-subscription 的 GroupMeta 加 members）：` +
      `nodes-group-filter 存在=${tids.includes("nodes-group-filter")}`,
  );
  // 反向断言：空态与错误**不能同时**出现（那说明页面在自欺）
  if (empty !== null && err !== null) {
    record("step17", "FAIL", "空态与错误区同时存在（状态没分档）", evidence);
    return;
  }
  // [docs/29 §202] **用户第 202 轮：节点级启用/关闭 + 全选（默认全选）** —— 真机上验这两条。
  // 语义：关掉的节点**不进「自动」的候选**；所以这一档必须**可逆**（关掉再打开），
  // 否则后面共用这份订阅的步骤会被影响（那会变成"我自己改坏了后面的档"）。
  let nodesToggleOk = true;
  let nodesToggleWhy = "(没有行，未验)";
  const readSwitches = () =>
    wd.exec(
      `return [...document.querySelectorAll('[data-testid="network-nodes-enabled"] input')].map((i) => i.checked);`,
    );
  const clickSwitch = (idx) =>
    wd.exec(
      `const a = document.querySelectorAll('[data-testid="network-nodes-enabled"] input');
       if (a.length <= ${idx}) return false;
       a[${idx}].click();
       return true;`,
    );
  const disabledInDb = () => {
    const dump = dumpScratchDb("step17-nodes");
    const rows2 = configEntities(dump, "subscription");
    const hit = rows2.find((r) => Array.isArray(r.json?.disabled_nodes) && r.json.disabled_nodes.length > 0);
    return hit === undefined ? [] : hit.json.disabled_nodes;
  };
  if (rows > 0) {
    const bulkTids = ["network-nodes-select-all", "network-nodes-disable-selected", "network-nodes-enable-selected", "network-nodes-selected-count"];
    const missingBulk = bulkTids.filter((t) => !tids.includes(t));
    const before = await readSwitches();
    evidence.push(
      `节点开关：${before.length} 个；默认状态=${JSON.stringify(before)}（用户要求**默认全选**，必须全 true）；` +
        `全选/批量控件缺失=${JSON.stringify(missingBulk)}`,
    );
    const allOn = before.length > 0 && before.every((v) => v === true);
    let toggledOff = false;
    let offLanded = false;
    let restored = false;
    if (before.length > 0) {
      const hit = await clickSwitch(0);
      await sleep(1200);
      const after = await readSwitches();
      toggledOff = hit === true && after[0] === false;
      const names = disabledInDb();
      offLanded = names.length === 1;
      evidence.push(
        `关掉第 1 条：点击=${hit} → 界面回读=${JSON.stringify(after)}；库里 disabled_nodes=${JSON.stringify(names)}`,
      );
      const back = await clickSwitch(0);
      await sleep(1200);
      const after2 = await readSwitches();
      restored = back === true && after2[0] === true && disabledInDb().length === 0;
      evidence.push(`再打开：点击=${back} → 界面回读=${JSON.stringify(after2)}；库里 disabled_nodes=${JSON.stringify(disabledInDb())}`);
    }
    nodesToggleOk = allOn && toggledOff && offLanded && restored && missingBulk.length === 0;
    nodesToggleWhy =
      `默认全开=${allOn} 能关=${toggledOff} 落库=${offLanded} 能复原=${restored} 批量控件齐全=${missingBulk.length === 0}`;
  }
  evidence.push(`节点开关判定：${nodesToggleWhy}`);
  if (rows > 0 && !nodesToggleOk) {
    record("step17", "FAIL", `节点级启用/关闭不成立（用户第 202 轮）：${nodesToggleWhy}`, evidence);
    return;
  }
  if (rows > 0 && nodesToggleOk) {
    record(
      "step17",
      "PASS",
      `节点清单真出数据：${rows} 行（这一屏既有 data-testid 与节点级开关全链路可用）；` +
        `开关默认全开、能关能复原且真落库、全选/批量控件齐全；` +
        `夹具节点 ta-e2e-node-hk 出现=${hasFixtureNode}（本档判据是"清单真出数据 + 开关可用"，` +
        `不依赖它是哪一份订阅 —— 只跑部分档时 scratch 里那份的已采纳版本可能是别的订阅）`,
      evidence,
    );
  } else if (rows > 0) {
    record(
      "step17",
      "FAIL",
      `节点清单有 ${rows} 行，但**节点级启用/关闭**不成立（用户第 202 轮）：${nodesToggleWhy}`,
      evidence,
    );
  } else if (empty !== null) {
    record(
      "step17",
      "FAIL",
      `节点清单是空的（nodes-empty: ${JSON.stringify(String(empty).replace(/\s+/g, " ").slice(0, 60))}）—— 用户要的是看到节点`,
      evidence,
    );
  } else {
    record("step17", "FAIL", `节点清单没出现节点（rows=${rows}，hasFixtureNode=${hasFixtureNode}）`, evidence);
  }
}

// ---------------------------------------------------------------- step23 订阅详情页（[docs/54]）

/**
 * **step23：订阅详情页真的成立**（[docs/54]，用户第 321 轮）。
 *
 * 用户原话："先到网络界面，再去点击一个订阅，里面就可以看到这个订阅的所有节点，
 * 可以打开或者关闭" + "其中自动选择和直连在最上面" + "直连是伪节点，但属于一直有的"。
 *
 * 这一档在**真机**上验四件事（单测只能验前两件的一半）：
 * 1. 从 `/network` 的订阅名**点得进去**（真链接、真路由）；
 * 2. 顶部两行「自动选择」「直连」恒在，且开关**禁用**（不可关闭）；
 * 3. 节点开关**真落库**（`disabled_nodes`）且可逆；
 * 4. ★ **关掉的节点真的被排除出内核生成的组** —— 这是本轮修掉的那个假开关的**真机证据**：
 *    关掉一个节点后，内核配置文件（`kernel.generated.yaml`）里那个组的 `exclude-filter`
 *    必须出现它、且能编译成正则。
 */
async function step23(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/network", '[data-testid="network-subscriptions-table"], [data-testid="network-subscriptions-empty"]');
  if (page === null) {
    await shot(wd, "step23-no-page");
    record("step23", "FAIL", "/network 没渲染出来（进不去详情页）", evidence);
    return;
  }

  const tids = await wd.probeTestIds();
  evidence.push(`/network 命中 data-testid: ${tids.filter((t) => t.startsWith("network-sub")).join(", ")}`);
  if (!tids.includes("network-subscription-open")) {
    evidence.push("找不到 network-subscription-open（订阅名不是链接）");
    await shot(wd, "step23-no-open-link");
    record("step23", "FAIL", "/network 的订阅名不是进详情页的链接（[docs/54] 的入口不存在）", evidence);
    return;
  }

  // 点进去：**真点**那个链接（不是直接改 hash —— 那验不了"入口存在"）
  const clicked = await wdClickTid(wd, "network-subscription-open", evidence);
  await sleep(1200);
  const detailTids = await wd.probeTestIds();
  evidence.push(
    `点订阅名：clicked=${clicked}；详情页 testid: ${detailTids.filter((t) => t.startsWith("network-subscription")).join(", ")}`,
  );
  await shot(wd, "step23-subscription-detail");

  if (!detailTids.includes("network-subscription-detail")) {
    record("step23", "FAIL", "点订阅名没有进到详情页（network-subscription-detail 不存在）", evidence);
    return;
  }

  // ①② 顶部两行：恒在 + 开关禁用
  const builtin = await wd.exec(
    `const pick = (k) => document.querySelector('[data-testid="network-subscription-builtin-' + k + '"]');
     const read = (k) => {
       const row = pick(k);
       if (!row) return null;
       const input = row.querySelector('input');
       return { text: (row.textContent || '').slice(0, 40), checked: input ? input.checked : null, disabled: input ? input.disabled : null };
     };
     return { auto: read('auto'), direct: read('direct'), order: [...document.querySelectorAll('[data-testid^="network-subscription-builtin-"], [data-testid="network-subscription-node-row"]')].map((e) => e.getAttribute('data-testid')) };`,
  );
  evidence.push(`顶部两行：${JSON.stringify(builtin)}`);
  const auto = builtin?.auto;
  const direct = builtin?.direct;
  const bothPresent = auto !== null && auto !== undefined && direct !== null && direct !== undefined;
  const bothDisabled = auto?.disabled === true && direct?.disabled === true;
  const orderOk = Array.isArray(builtin?.order) && builtin.order[0] === "network-subscription-builtin-auto" && builtin.order[1] === "network-subscription-builtin-direct";
  if (!bothPresent || !bothDisabled || !orderOk) {
    record("step23", "FAIL", `顶部两行不成立：都在=${bothPresent} 都禁用=${bothDisabled} 在最上面=${orderOk}`, evidence);
    return;
  }

  // ③④ 节点开关：能关、落库、**内核配置里真被排除**
  const rowCount = await wd.exec(`return document.querySelectorAll('[data-testid="network-subscription-node-row"]').length;`);
  evidence.push(`节点行数 = ${rowCount}`);
  if (rowCount === 0) {
    // 没解析过就没有节点：如实记 SKIP（不是产品缺陷，fixture 那一档可能没刷）
    const emptyText = await wd.textOf('[data-testid="network-subscription-detail-empty"]');
    record(
      "step23",
      "SKIP",
      `顶部两行与入口都成立，但这份订阅这一档没有可解析的节点（${emptyText === null ? "既无行也无「还没解析」提示" : JSON.stringify(String(emptyText).replace(/\\s+/g, " ").slice(0, 50))}）→ 节点开关与内核排除**未覆盖**`,
      evidence,
    );
    return;
  }

  const readSwitches = () =>
    wd.exec(`return [...document.querySelectorAll('[data-testid="network-subscription-node-enabled"] input')].map((i) => i.checked);`);
  const clickSwitch = (idx) =>
    wd.exec(
      `const a = document.querySelectorAll('[data-testid="network-subscription-node-enabled"] input');
       if (a.length <= ${idx}) return false;
       a[${idx}].click();
       return true;`,
    );
  const disabledInDb = () => {
    const dump = dumpScratchDb("step23-detail");
    const rows = configEntities(dump, "subscription");
    const hit = rows.find((r) => Array.isArray(r.json?.disabled_nodes) && r.json.disabled_nodes.length > 0);
    return hit === undefined ? [] : hit.json.disabled_nodes;
  };

  const before = await readSwitches();
  evidence.push(`开关初值（默认全开，用户第 202 轮）= ${JSON.stringify(before)}`);
  const hit = await clickSwitch(0);
  await sleep(1500);
  const after = await readSwitches();
  const names = disabledInDb();
  evidence.push(`关掉第 1 条：点击=${hit} → 回读=${JSON.stringify(after)}；库里 disabled_nodes=${JSON.stringify(names)}`);

  // ★ 真机核心证据：内核生成的配置里，那个组的 exclude-filter 必须排除它。
  //
  // 路径是 `<data>/kernel/kernel.generated.yaml`（`src-tauri/src/boot.rs:251` 的
  // `kernel_dir.join("kernel.generated.yaml")`）—— **不是** `<data>/kernel.generated.yaml`。
  //
  // 两个我踩过的坑（都记下来，免得下次再判成产品缺陷）：
  //   1. 路径少了 `kernel/` 那一层 → 判"文件不存在"；
  //   2. 生成器用 **YAML 双引号** 写这个正则（`q()`），而双引号标量里 `\` 要写成 `\\`：
  //      文件里是 `124\\.4`，而真实正则值是 `124\.4`。**直接拿文件名去 `includes()` 永远不命中**。
  //      所以必须先按 YAML 双引号规则**反转义**，再和"正则转义后的节点名"比。
  const genPath = join(SCRATCH, "kernel", "kernel.generated.yaml");
  let genHasExclude = false;
  let genExcerpt = "(没有生成文件)";
  if (existsSync(genPath)) {
    const text = readFileSync(genPath, "utf8");
    const line = text.split("\n").find((l) => l.includes("exclude-filter")) ?? "";
    genExcerpt = line.trim() === "" ? "(没有 exclude-filter 行)" : line.trim().slice(0, 220);
    // 取出双引号里的内容并按 YAML 规则反转义（`\\.` → `.`，`\"` → `"`）
    const quoted = /exclude-filter:\s*"((?:[^"\\]|\\.)*)"/.exec(text);
    const decoded = quoted === null ? "" : quoted[1].replace(/\\(.)/g, "$1");
    const asRegex = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const excluded = names.filter((n) => decoded.includes(asRegex(n)));
    genHasExclude = names.length > 0 && excluded.length === names.length;
    evidence.push(
      `内核生成配置 ${genPath}：关掉的节点出现在 exclude-filter 里 = ${genHasExclude}（命中 ${JSON.stringify(excluded)}）`,
    );
    evidence.push(`exclude-filter 行: ${JSON.stringify(genExcerpt)}`);
    evidence.push(`反转义后的排除正则: ${JSON.stringify(decoded)}`);
  } else {
    evidence.push(`内核生成配置不存在（${genPath}）—— 内核这一档可能没起，排除规则未覆盖`);
  }
  await shot(wd, "step23-node-toggled");

  // 复原（不许把后面共用的档改坏）
  const back = await clickSwitch(0);
  await sleep(1500);
  const after2 = await readSwitches();
  const restored = back === true && after2[0] === true && disabledInDb().length === 0;
  evidence.push(`再打开：点击=${back} → 回读=${JSON.stringify(after2)}；库里 = ${JSON.stringify(disabledInDb())}`);

  const toggleOk = hit === true && after[0] === false && names.length === 1 && restored;
  if (!toggleOk) {
    record("step23", "FAIL", `节点开关不成立：能关=${hit === true && after[0] === false} 落库=${names.length === 1} 可复原=${restored}`, evidence);
    return;
  }
  if (!genHasExclude) {
    // 开关本身成立，但**内核没跟上** —— 那正是本轮要修的那个假开关，必须红
    record(
      "step23",
      "FAIL",
      `节点开关能关能落库能复原，但**关掉的节点没有出现在内核生成的 exclude-filter 里**（${JSON.stringify(genExcerpt)}）—— 开关对内核不生效`,
      evidence,
    );
    return;
  }
  record(
    "step23",
    "PASS",
    `订阅详情页全链路成立：从 /network 点订阅名进得去；「自动选择」「直连」恒在最上面且开关禁用；` +
      `${rowCount} 个节点可关可复原并真落库 disabled_nodes；关掉的节点真出现在内核生成的 exclude-filter 里（假开关已修实）`,
    evidence,
  );
}

// ---------------------------------------------------------------- step18 API key 管理

/**
 * **step18：访问密钥真的能管**（用户第 71 轮：「API key 的管理也需要」）。
 *
 * 关键语义（[docs/19 §3.3] 已定死）：`gateway.local_key` 为空**且**表里没有 enabled 的 key → **不鉴权**；
 * 一旦存在任一凭据来源 → **必须匹配**。所以本步骤分成两段，**分别给结论**：
 *
 * 1. **本地 CRUD（本环境可验证，必须真过）**：创建 → 明文只显示一次 → 列表只留前缀不含明文
 *    → 启停开关真的翻转 → 删除后那行真的消失。这些全程不碰网络，是实现的问题就得红。
 * 2. **HTTP 鉴权路径（需要网关真的在监听）**：带 key 非 401、不带 key 401、界面禁用后立刻 401。
 *    E2E 环境里**没有内核**，而启动编排在内核未就绪时**故意不启动网关**
 *    （`src-tauri/src/boot.rs:454` 的 `start_gateway`，同文件单测那句「内核没就绪就不该往下走」；
 *    实机日志里只有「网关已装配（尚未开始服务）」，从来没有 `src-tauri/src/gateway.rs:335`
 *    的「网关开始服务（只绑回环）」）。此时端口没人监听，探测必然 `status:0 / fetch failed`
 *    —— 这既不是"放行"也不是"被挡"。所以这一小段**如实记 SKIP 并写明原因**，
 *    绝不当成 PASS（假绿），也绝不报成 FAIL（那会让人误以为是产品缺陷）。
 *    换一台真的有内核的机器跑同一个脚本，这一小段会自动走回完整断言。
 *
 * 明文只在创建时出现一次，点「我已保存」后必须从界面上消失。
 */
async function step18(wd) {
  const evidence = [];
  const GW = "http://127.0.0.1:8787";
  /** 本步新建的那把 key 的**名称**：列表里按它定位"我们自己那一行"。 */
  const KEY_NAME = "ta-e2e-key";
  const probe = async (token) => {
    const headers = token === null ? {} : { authorization: `Bearer ${token}` };
    try {
      const res = await fetch(`${GW}/v1/models`, { headers });
      return { status: res.status, body: (await res.text()).slice(0, 120) };
    } catch (err) {
      return { status: 0, body: String(err.message ?? err).slice(0, 120) };
    }
  };

  const page = await gotoPage(wd, "#/api-keys", '[data-testid="api-keys-page"]');
  if (page === null) {
    record("step18", "FAIL", "/api-keys 页面没渲染出来", evidence);
    return;
  }
  const tids = await wd.probeTestIds();
  evidence.push(`/api-keys 命中 data-testid: ${tids.slice(0, 30).join(", ")}`);
  if (!tids.includes("api-keys-create")) {
    record("step18", "FAIL", "界面上没有「新建密钥」入口（api-keys-create 不存在）", evidence);
    return;
  }

  // ① 建之前：没有凭据来源 → 不该被挡（默认不鉴权，这是既有行为）。
  //    这一次探测同时回答一个**环境问题**：网关到底有没有在监听 —— 后面的判定全靠它分流。
  const before = await probe(null);
  evidence.push(`建 key 之前：无凭据 GET /v1/models → ${JSON.stringify(before)}`);
  const gatewayUp = before.status !== 0;
  evidence.push(
    `网关可达性=${gatewayUp}（status:0 = 端口没人监听；它既不是"放行"也不是"被挡"，判定时必须分开看）`,
  );

  // ② 真界面新建
  await wdClickTid(wd, "api-keys-create", evidence);
  await sleep(400);
  await wdFillByTid(wd, "api-keys-create-name", KEY_NAME, evidence);
  await wdClickTid(wd, "api-keys-create-submit", evidence);
  await sleep(1500);
  await shot(wd, "step18-created");
  const plaintextRaw = await wd.textOf('[data-testid="api-keys-created-plaintext"]');
  const plaintext = (plaintextRaw ?? "").trim().replace(/\s+/g, "");
  evidence.push(`创建结果区原文: ${JSON.stringify((plaintextRaw ?? "").replace(/\s+/g, " ").slice(0, 200))}`);
  if (plaintextRaw === null) {
    const e = await wd.textOf('[data-testid="api-keys-error"]');
    if (e !== null) evidence.push(`api-keys-error: ${JSON.stringify(e)}`);
    record("step18", "FAIL", "创建后没有显示明文（api-keys-created-plaintext 不存在）", evidence);
    return;
  }
  evidence.push(`取到的明文前 12 字符: ${JSON.stringify(plaintext.slice(0, 12))}`);

  // ③ 明文只出现一次：点「我已保存」后从界面消失
  let hiddenAfterDone = false;
  if ((await wd.probeTestIds()).includes("api-keys-created-done")) {
    await wdClickTid(wd, "api-keys-created-done", evidence);
    await sleep(500);
    hiddenAfterDone = !(await wd.probeTestIds()).includes("api-keys-created-plaintext");
  }
  evidence.push(`点「我已保存」之后明文从界面消失 = ${hiddenAfterDone}`);

  // ④ 列表里**只有前缀、没有明文**（本地 CRUD，与网关在不在无关）
  const listText = await wd.textOf('[data-testid="api-keys-list-panel"]');
  const listFlat = (listText ?? "").replace(/\s+/g, "");
  const hashOnly = await wd.textOf('[data-testid="api-keys-hash-only"]');
  const listHasPlain = plaintext.length > 0 && listFlat.includes(plaintext);
  evidence.push(
    `列表面板文本: ${JSON.stringify((listText ?? "").replace(/\s+/g, " ").slice(0, 200))}` +
      ` · 列表里含明文=${listHasPlain} · hash-only 说明=${JSON.stringify((hashOnly ?? "").replace(/\s+/g, " ").slice(0, 90))}`,
  );
  const okListNoPlain = listText !== null && !listHasPlain;

  /**
   * 按**名称**定位我们自己那一行，并把它的 `data-key-id` 读出来 —— 后面点开关 / 删除都落在
   * 这一行上（`--keep-data` 跑法下列表里可能有上一轮的残留行，不能拿"页面上第一个开关"凑数）。
   */
  const rowProbe = async () =>
    wd.exec(`
      const name = ${JSON.stringify(KEY_NAME)};
      const rows = [...document.querySelectorAll('[data-testid="api-key-row"]')];
      const mine = rows.filter((r) => (r.textContent || '').includes(name));
      const input = mine.length === 1 ? mine[0].querySelector('[data-testid="api-key-toggle"] input') : null;
      return {
        rows: rows.length,
        mine: mine.length,
        keyId: mine.length === 1 ? mine[0].getAttribute('data-key-id') : null,
        checked: input ? input.checked : null,
      };
    `);
  const rowBeforeToggle = await rowProbe();
  evidence.push(`新建的密钥行（按名称 ${KEY_NAME} 定位）: ${JSON.stringify(rowBeforeToggle)}`);
  const keyId =
    typeof rowBeforeToggle?.keyId === "string" && /^[A-Za-z0-9._:-]+$/.test(rowBeforeToggle.keyId)
      ? rowBeforeToggle.keyId
      : null;
  const scoped = (tid) =>
    keyId === null
      ? `[data-testid="${tid}"]`
      : `[data-testid="api-key-row"][data-key-id="${keyId}"] [data-testid="${tid}"]`;

  // ⑤ 真鉴权：带 key 非 401、不带 key 401。**必须先于"停用"这一步**，否则带 key 也会被挡。
  let withKey = null;
  let noKey = null;
  if (gatewayUp) {
    withKey = await probe(plaintext);
    noKey = await probe(null);
    evidence.push(`带这个 key：${JSON.stringify(withKey)}；不带 key：${JSON.stringify(noKey)}`);
  } else {
    // 网关没监听时照样探一次：拿到的必然是 status:0，这正是与"被 401 挡下"的区分证据
    withKey = await probe(plaintext);
    evidence.push(
      `网关没监听，带 key 再探一次也只是证明"没人在听"：${JSON.stringify(withKey)}` +
        `（若网关在监听且鉴权生效，这里应当是 401，而不是 status:0）`,
    );
  }

  // ⑥ 启用/停用：先验**本地**（点一下，MUI Switch 的真实勾选状态真的翻过去）；
  //    网关在监听时再验"禁用后带同一个 key 立刻 401"。
  let toggleFlipped = false;
  let disabledProbe = null;
  if (rowBeforeToggle?.mine !== 1) {
    evidence.push(`按名称 ${KEY_NAME} 没定位到唯一一行（mine=${rowBeforeToggle?.mine}），启停开关这一条没测到`);
  } else {
    const toggleEl =
      (await wd.findElement(`${scoped("api-key-toggle")} input`)) ?? (await wd.findElement(scoped("api-key-toggle")));
    if (toggleEl === null) {
      evidence.push(`找不到开关控件 ${scoped("api-key-toggle")}`);
    } else {
      try {
        await wd.click(toggleEl);
        await sleep(1200);
      } catch (err) {
        evidence.push(`开关点不动：${wdErrText(err)}`);
      }
      const rowAfterToggle = await rowProbe();
      toggleFlipped = rowAfterToggle?.checked !== null && rowAfterToggle.checked !== rowBeforeToggle.checked;
      evidence.push(
        `点开关后: ${JSON.stringify(rowAfterToggle)} · 本地状态翻转=${toggleFlipped}` +
          `（这是本地证据：命令真的跑完并重新渲染了；它对 HTTP 的影响见下一行）`,
      );
      if (gatewayUp) {
        disabledProbe = await probe(plaintext);
        evidence.push(`界面上停用后带同一个 key：${JSON.stringify(disabledProbe)}`);
      }
    }
  }

  // ⑦ 删除：真点「删除」→ 确认对话框 → 那一行真的消失（本地 CRUD）
  let deleteOk = false;
  const delEl = (await wd.findElement(scoped("api-key-delete"))) ?? null;
  if (delEl === null) {
    evidence.push(`找不到删除控件 ${scoped("api-key-delete")}（这一条没测到）`);
  } else {
    try {
      await wd.click(delEl);
      await sleep(400);
    } catch (err) {
      evidence.push(`删除按钮点不动：${wdErrText(err)}`);
    }
    if (!(await wd.probeTestIds()).includes("api-key-delete-confirm")) {
      evidence.push("点了「删除」但没有出现确认对话框（api-key-delete-confirm 不存在）");
    } else {
      await wdClickTid(wd, "api-key-delete-confirm", evidence);
      await sleep(1200);
      const rowAfterDelete = await rowProbe();
      deleteOk = rowAfterDelete?.mine === 0;
      evidence.push(
        `删除后按名称 ${KEY_NAME} 定位到的行数=${rowAfterDelete?.mine}（列表总行数=${rowAfterDelete?.rows}）· 行消失=${deleteOk}`,
      );
      await shot(wd, "step18-deleted");
    }
  }

  // ---- 判定：本地 CRUD 与 HTTP 鉴权**分开报**
  const okCreate = plaintext.startsWith("ta-sk-");
  const okHidden = hiddenAfterDone;
  const crudOk = okCreate && okHidden && okListNoPlain && toggleFlipped && deleteOk;
  evidence.push(
    `本地 CRUD 判定：明文形状=${okCreate} 只显示一次=${okHidden} 列表不含明文=${okListNoPlain} ` +
      `启停翻转=${toggleFlipped} 删除后行消失=${deleteOk}`,
  );

  if (!gatewayUp) {
    if (crudOk) {
      record(
        "step18",
        "PASS",
        "访问密钥本地 CRUD 全通过：创建并显示明文（只显示一次）→ 列表只留前缀不含明文 → 启停开关真翻转 → 删除后行消失",
        evidence,
      );
      record(
        "step18",
        "SKIP",
        `HTTP 鉴权这一段**在本环境未验证**：网关没在监听（${JSON.stringify(before)}），端口不可达，` +
          `不是鉴权失效也不是产品回归；带 key / 不带 key / 停用三档一律记未验证，绝不当成通过`,
        [
          "环境事实：E2E 环境里没有内核；启动编排在内核未就绪时**故意不启动网关**。",
          "依据 1：src-tauri/src/boot.rs:454 start_gateway —— boot.rs 单测里同一句断言「内核没就绪就不该往下走」。",
          "依据 2：src-tauri/src/gateway.rs:335 的「网关开始服务（只绑回环）」在本环境日志里**从未出现**。",
          '实机日志只有：「网关已装配（尚未开始服务） port=127.0.0.1:8787」与「启动阶段：界面已就绪，正在准备网关 phase="skeleton"」。',
          `实测：不带任何 key 的一次探测同样返回 ${JSON.stringify(before)} —— 端口没人听，与鉴权无关。`,
          "要在本段拿到真结论，需要在**有内核**的机器上跑同一个脚本（网关真的开始服务后，这一段会自动走完整断言）。",
        ],
      );
    } else {
      record(
        "step18",
        "FAIL",
        `访问密钥本地 CRUD 没做到位：明文形状=${okCreate} 只显示一次=${okHidden} 列表不含明文=${okListNoPlain} ` +
          `启停翻转=${toggleFlipped} 删除后行消失=${deleteOk}（HTTP 鉴权段另记未验证）`,
        evidence,
      );
    }
    return;
  }

  const okAuth = withKey !== null && withKey.status !== 401 && noKey !== null && noKey.status === 401;
  const okDisabled = disabledProbe !== null && disabledProbe.status === 401;
  const ok = crudOk && okAuth && okDisabled;
  evidence.push(`HTTP 鉴权判定：鉴权生效=${okAuth} 停用即挡住=${okDisabled}`);
  if (ok) {
    record(
      "step18",
      "PASS",
      "访问密钥真能管也能挡人：创建（明文只显示一次）→ 列表只留前缀 → 带 key 放行 / 不带 key 401 → 界面停用后立刻 401 → 删除后行消失",
      evidence,
    );
  } else {
    record(
      "step18",
      "FAIL",
      `访问密钥没做到位：明文形状=${okCreate} 只显示一次=${okHidden} 列表不含明文=${okListNoPlain} ` +
        `启停翻转=${toggleFlipped} 删除后行消失=${deleteOk} 鉴权生效=${okAuth} 停用即挡住=${okDisabled}`,
      evidence,
    );
  }
}

/**
 * **step20：服务商详情 = 查看 + 编辑一体**（[docs/23]；用户第 75 轮原话：
 * 「服务商编辑页面不走下一步下一步，直接把所有信息在点开服务商之后提供可视化编辑界面，
 * 相当于编辑和查看一体了，当然不是纯文本编辑」）。
 *
 * 断言（缺一条都不算做到）：
 * - 点开服务商**直接就是编辑器**：`providers-edit-*` 控件齐全（基本信息 / 鉴权 / 超时 / 附加头行表 /
 *   模型区 / 凭据表 / 保存条），并且**没有** `providers-detail-edit`、没有 `pw-next`（不再走下一步）；
 * - 控件里的值是**库里那一条**（名字 / 地址与配置一致，不是空表）；
 * - 没有改动时保存按钮**不可点**（不是点了没反应的假按钮）；
 * - 改名字 → 保存 → 库里**真的**变了、服务商条数不变（id 复用，不派生第二个）；
 * - ★ **没碰过的连接配置一个字节都没变**（auth / headers / timeout / tls / lane / model_prefix /
 *   passthrough / on_header_conflict / protocol / base_url / enabled 逐项比对）——
 *   第 72 轮的编辑向导只发 4 个字段，覆盖写会抹掉这些；这一条就是那个数据丢失缺陷的回归判据；
 * - 改回原名，让后面的步骤拿到和进来时一样的夹具。
 *
 * 前提：必须在 step4/step10 之后跑（那时才有可编辑的、带通道的服务商）。
 */
async function step20(wd) {
  const NAME_MARK = "TA E2E 改名后";
  const evidence = [];
  const before = dumpScratchDb("step20-before");
  const providers = configEntities(before, "provider");
  const channelsBefore = configEntities(before, "channel");
  // 挑一个有通道的服务商：这样模型区一定有行可看
  const target =
    providers.find((p) => channelsBefore.some((c) => c.json?.provider === p.entityId)) ?? providers[0];
  if (target === undefined) {
    record("step20", "SKIP", "配置里还没有服务商（前面步骤没建出来）", evidence);
    return;
  }
  const mineBefore = channelsBefore.filter((c) => c.json?.provider === target.entityId);
  /** 编辑**不许动**的那些字段（逐项原样比对；[docs/23] 的核心回归判据）。 */
  const untouchedKeys = [
    "protocol",
    "base_url",
    "auth",
    "headers",
    "on_header_conflict",
    "passthrough",
    "timeout",
    "tls",
    "lane",
    "model_prefix",
    "enabled",
  ];
  const fingerprint = (json) =>
    JSON.stringify(untouchedKeys.map((k) => [k, json?.[k] ?? null]));
  const beforeFingerprint = fingerprint(target.json);
  evidence.push(
    `目标服务商: id=${target.entityId} name=${JSON.stringify(target.json?.name)}；` +
      `它有 ${mineBefore.length} 条通道（${JSON.stringify(mineBefore.map((c) => c.entityId))}）；` +
      `编辑前连接配置指纹=${beforeFingerprint}`,
  );

  const detail = await gotoPage(
    wd,
    `#/providers/${encodeURIComponent(target.entityId)}`,
    '[data-testid="providers-detail"], [data-testid="providers-detail-notfound"]',
  );
  if (detail === null) {
    record("step20", "FAIL", "服务商详情页没渲染出来（既没有 providers-detail 也没有 notfound）", evidence);
    return;
  }
  const notFound = await wd.textOf('[data-testid="providers-detail-notfound"]');
  if (notFound !== null) {
    record("step20", "FAIL", `详情页说找不到这个服务商：${JSON.stringify(notFound.slice(0, 80))}`, evidence);
    return;
  }

  // ---- ① 这一页本身就是编辑器：控件点得到，且不再有"下一歩"
  const ids = await wd.probeTestIds();
  const mustHave = [
    "providers-edit-name",
    "providers-edit-baseUrl",
    "providers-edit-protocol",
    "providers-edit-enabled",
    "providers-edit-lane",
    "providers-edit-modelPrefix",
    "providers-edit-authScheme",
    "providers-edit-authHeader",
    "providers-edit-authPrefix",
    "providers-edit-timeoutConnect",
    "providers-edit-timeoutRead",
    // [docs/28 §1] 附加头编辑器的 testid 是 `providers-headers-editor`（旧名 `providers-header-editor`
    // 已随详情页重构下线）
    "providers-headers-editor",
    "providers-header-add",
    "providers-edit-headerConflict",
    // [docs/28 §1] 详情页的**线路**列表（旧 `providers-model-row` 现在只出现在"还没创建"的候选行上，
    // 已建成的线路只在 `providers-line-row` 里）
    "providers-line-row",
    "providers-lines",
    "providers-raw-section",
    "providers-detail-save",
    "providers-detail-reset",
  ];
  const missing = mustHave.filter((tid) => !ids.includes(tid));
  const wizardLeft = ["providers-detail-edit", "pw-next", "provider-wizard-step1"].filter((tid) =>
    ids.includes(tid),
  );
  evidence.push(
    `详情页命中 testid 缺 ${JSON.stringify(missing)}；不该再出现的向导痕迹 ${JSON.stringify(wizardLeft)}`,
  );
  if (missing.length > 0 || wizardLeft.length > 0) {
    record(
      "step20",
      "FAIL",
      `详情页不是"查看 + 编辑一体"：缺控件 ${JSON.stringify(missing)}；仍在用的向导痕迹 ${JSON.stringify(wizardLeft)}`,
      evidence,
    );
    return;
  }

  // ---- ② 控件里的值 = 库里那一条
  const prefilled = await wd.exec(`
    const val = (tid) => {
      const el = document.querySelector('[data-testid="' + tid + '"]');
      const i = el ? el.querySelector('input, textarea') : null;
      return i ? i.value : null;
    };
    const save = document.querySelector('[data-testid="providers-detail-save"]');
    return {
      name: val('providers-edit-name'),
      baseUrl: val('providers-edit-baseUrl'),
      timeoutRead: val('providers-edit-timeoutRead'),
      saveDisabled: save ? save.disabled === true : null,
      saveText: save ? (save.textContent || '').trim() : null,
    };
  `);
  const prefilledOk =
    prefilled?.name === (target.json?.name ?? null) &&
    prefilled?.baseUrl === (target.json?.base_url ?? null);
  // 没有改动时保存不可点（假按钮的反面：可点必须真的会写库）
  const cleanStateOk = prefilled?.saveDisabled === true;
  evidence.push(
    `预填=${JSON.stringify(prefilled)}；库里 name=${JSON.stringify(target.json?.name)} ` +
      `base_url=${JSON.stringify(target.json?.base_url)}；预填一致=${prefilledOk}；无改动时保存不可点=${cleanStateOk}`,
  );

  // ---- ③ 改名字 → 保存 → 落库
  await wdFillByTid(wd, "providers-edit-name", NAME_MARK, evidence);
  const dirtyProbe = await wd.exec(`
    const save = document.querySelector('[data-testid="providers-detail-save"]');
    return save ? save.disabled === false : null;
  `);
  await wdClickTid(wd, "providers-detail-save", evidence);
  await sleep(1600);
  const savedText = await wd.textOf('[data-testid="providers-detail-save-result"]');
  const saveError = await wd.textOf('[data-testid="providers-detail-save-error"]');
  await shot(wd, "step20-saved");

  const after = dumpScratchDb("step20-after");
  const providersAfter = configEntities(after, "provider");
  const mineAfter = providersAfter.find((p) => p.entityId === target.entityId);
  const nameOk = mineAfter?.json?.name === NAME_MARK;
  const noDuplicate = providersAfter.length === providers.length;
  const afterFingerprint = fingerprint(mineAfter?.json);
  const preserved = afterFingerprint === beforeFingerprint;
  evidence.push(
    `保存后：结果区=${JSON.stringify(savedText)}；错误区=${JSON.stringify(saveError)}；` +
      `库里 name=${JSON.stringify(mineAfter?.json?.name)}；条数=${providersAfter.length}（编辑前 ${providers.length}）；` +
      `改名生效=${nameOk} id 复用=${noDuplicate}；` +
      `编辑后连接配置指纹=${afterFingerprint}；**一个字节都没变**=${preserved}`,
  );

  // ---- ④ 改回原名（后面的步骤拿到和进来时一样的夹具；顺带证明这个编辑器能连续保存）
  let restored = true;
  if (nameOk) {
    await wdFillByTid(wd, "providers-edit-name", String(target.json?.name ?? ""), evidence);
    await wdClickTid(wd, "providers-detail-save", evidence);
    await sleep(1600);
    const back = dumpScratchDb("step20-restored");
    const mineBack = configEntities(back, "provider").find((p) => p.entityId === target.entityId);
    restored =
      mineBack?.json?.name === target.json?.name && fingerprint(mineBack?.json) === beforeFingerprint;
    evidence.push(
      `改回原名后：name=${JSON.stringify(mineBack?.json?.name)}；指纹回到编辑前=${restored}`,
    );
  }

  const ok =
    prefilledOk &&
    cleanStateOk &&
    dirtyProbe === true &&
    nameOk &&
    noDuplicate &&
    preserved &&
    restored &&
    typeof savedText === "string" &&
    savedText.length > 0 &&
    saveError === null;
  if (ok) {
    record(
      "step20",
      "PASS",
      "服务商详情即编辑器：点开就有全部可视化控件（无「编辑」按钮 / 无下一步）· 控件里是库里真值 · " +
        "无改动时保存不可点 · 改名字真的落库且 id 复用 · " +
        "**没碰过的 auth/headers/timeout/tls/lane 等逐项比对一个字节都没变** · 改回原名后指纹复原",
      evidence,
    );
  } else {
    record(
      "step20",
      "FAIL",
      `服务商编辑没做到位：预填一致=${prefilledOk} 无改动时保存不可点=${cleanStateOk} ` +
        `改动后可点=${dirtyProbe} 改名落库=${nameOk} id 复用=${noDuplicate} 连接配置未被动=${preserved} ` +
        `改回原名=${restored} 结果区=${JSON.stringify(savedText)} 错误=${JSON.stringify(saveError)}`,
      evidence,
    );
  }
}

/**
 * **step24：服务商模型目录**（[docs/25]）。
 *
 * 扫描 = 记录 + 按目录补齐线路（不建模型）。
 */
async function step24(wd) {
  const evidence = [];
  const before = dumpScratchDb("step24-before");
  const providers = configEntities(before, "provider");
  const credentials = configEntities(before, "credential");
  if (providers.length === 0 || credentials.length === 0) {
    record("step24", "SKIP", "配置里还没有带凭据的服务商（前面步骤没建出来）", evidence);
    return;
  }
  // 扫描要用凭据鉴权 → 优先挑一个**已经有凭据**的服务商。
  //
  // [docs/28 §2] "勾一下 = 建一条线路"需要一个**还没接入**的候选行才验得到，所以下面
  // （扫完之后）会先**移除一条既有线路**把某个模型退回候选状态 —— 不去挑那些"从没配过密钥、
  // 根本扫不动"的服务商（`ta-e2e-2` 那种高级添加出来的就是）：拿它跑，扫描必然失败，
  // 于是整条用例的结论变成"扫描没记录"，那是**假缺陷**。
  const withCred = new Set(credentials.map((c) => String(c.json?.provider ?? "")));
  const target = providers.find((p) => withCred.has(p.entityId)) ?? providers[0];
  const providerId = target.entityId;

  const modelsBefore = configEntities(before, "model").length;
  const channelsOf = (dump) =>
    configEntities(dump, "channel").filter((c) => String(c.json?.provider) === providerId);
  const channelsBefore = channelsOf(before);
  const catalogBefore = (before.provider_model_catalog ?? []).filter(
    (r) => String(r.provider_id) === providerId,
  );
  evidence.push(
    `目标服务商 ${providerId}；扫描前：models=${modelsBefore} 该服务商通道=${channelsBefore.length} ` +
      `目录行=${catalogBefore.length}`,
  );

  const detail = await gotoPage(
    wd,
    `#/providers/${encodeURIComponent(providerId)}`,
    '[data-testid="providers-detail"], [data-testid="providers-detail-notfound"]',
  );
  if (detail === null) {
    record("step24", "FAIL", "服务商详情页没渲染出来（既没有 providers-detail 也没有 notfound）", evidence);
    return;
  }

  const countChecks = async () =>
    Number(await wd.exec(`return document.querySelectorAll('[data-testid="providers-model-check"]').length;`));

  // ---- ① 重新扫描（目录区入口）。先**移除一条既有线路**，让某个模型退回
  //       "扫到了但还没创建"的候选状态 —— 否则 ② 那一档（勾一下 = 建一条线路）没有对象可勾。
  //       这条移除走的是详情页线路行上的真控件（`providers-line-remove` + 确认框），
  //       所以它本身也是"删除是真的"的一条证据。
  let removedLine = null;
  {
    // ★ 必须挑一条**没有被任何策略点名**的线路：被 `action.use_channels` 点名的线路
    //   `channel_delete` 会（正确地）拒绝 —— 拿它来"制造候选行"只会得到"移除失败"，
    //   而那是**产品规则生效**，不是缺陷。真机第一次跑就是这么误判的。
    const referenced = new Set();
    for (const p of configEntities(before, "policy")) {
      const uses = p.json?.action?.use_channels;
      if (Array.isArray(uses)) for (const id of uses) referenced.add(String(id));
    }
    const lineRow = [...channelsBefore].reverse().find((c) => !referenced.has(String(c.entityId)));
    if (lineRow === undefined) {
      evidence.push("这一家的线路全被策略点名了 → 没有可安全移除的线路来制造候选行");
    }
    if (lineRow !== undefined) {
      evidence.push(`挑中可安全移除的线路 ${lineRow.entityId}（未被任何策略点名）`);
      const removed = await wd.exec(`
        const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
        const row = rows.find((r) => (r.textContent || '').includes(${JSON.stringify("__NAME__")}));
        if (!row) return { found: false };
        // [docs/28 §1.5] 列表行只放名字 + 代理 + 滑块：**先点开这一行的「详情」**，
        // 「移除线路」才在面板里（真实用户也是这么走的）。
        const info = row.querySelector('[data-testid="providers-line-info"]');
        if (!info) return { found: true, info: false };
        info.click();
        return { found: true, info: true };
      `.replace("__NAME__", String(lineRow.json?.upstream_model ?? "")));
      await sleep(600);
      const removedClick = await wd.exec(`
        const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
        const row = rows.find((r) => (r.textContent || '').includes(${JSON.stringify("__NAME__")}));
        if (!row) return { button: false };
        const panel = row.querySelector('[data-testid="providers-line-info-panel"]');
        const btn = panel ? panel.querySelector('[data-testid="providers-line-remove"]') : null;
        if (!btn) return { button: false, panel: panel !== null };
        btn.click();
        return { button: true, panel: true };
      `.replace("__NAME__", String(lineRow.json?.upstream_model ?? "")));
      evidence.push(
        `为制造候选行，移除线路 ${lineRow.entityId}：行=${JSON.stringify(removed)} 面板里的按钮=${JSON.stringify(removedClick)}`,
      );
      await sleep(500);
      await sleep(900);
      if (removedClick?.button === true) {
        await wdClickTid(wd, "providers-line-delete-confirm", evidence);
        await sleep(2000);
        const afterRemove = dumpScratchDb("step24-after-remove");
        const stillThere = channelsOf(afterRemove).some(
          (c) => String(c.entityId) === String(lineRow.entityId),
        );
        evidence.push(`移除后该线路还在吗=${stillThere}（期望 false）`);
        if (!stillThere) removedLine = String(lineRow.entityId);
        // 确认框没关会把紧接着的"重新扫描"点成"点到遮罩"（真机就是这么被拦下来的）→ 显式关掉
        await wd.closeDialog();
        await sleep(500);
      }
    }
  }

  await wdClickTid(wd, "providers-model-scan", evidence);
  await sleep(3000);
  const scanChecks = await countChecks();
  const afterScan = dumpScratchDb("step24-after-scan");
  const modelsAfterScan = configEntities(afterScan, "model").length;
  const channelsAfterScan = channelsOf(afterScan).length;
  const catalogAfterScan = (afterScan.provider_model_catalog ?? []).filter(
    (r) => String(r.provider_id) === providerId,
  );
  await shot(wd, "step24-after-scan");
  // 扫描这一段到底说了什么：把目录区整段原文抓下来（否则"目录 0 行"会被误读成"扫描没记录"，
  // 而真实原因可能是"扫描本身失败了，页面正在显示后端原文"）。
  const scanProbe = await wd.exec(`
    const btn = document.querySelector('[data-testid="providers-model-scan"]');
    const host = btn ? btn.closest('div') : null;
    const pick = (t) => document.querySelector('[data-testid="' + t + '"]');
    const err = pick('providers-model-error');
    const empty = pick('providers-model-empty');
    return {
      section: (host ? host.innerText : '').replace(/\s+/g, ' ').slice(0, 400),
      error: err ? err.innerText.replace(/\s+/g, ' ').slice(0, 300) : null,
      empty: empty ? empty.innerText.replace(/\s+/g, ' ').slice(0, 200) : null,
      checks: document.querySelectorAll('[data-testid="providers-model-check"]').length,
      rows: document.querySelectorAll('[data-testid="providers-model-row"]').length,
      count: pick('providers-lines-header')?.innerText ?? null,
    };
  `);
  evidence.push(`扫描区原文: ${JSON.stringify(scanProbe)}`);
  const creds = configEntities(afterScan, "credential").filter(
    (c) => String(c.json?.provider ?? "") === providerId,
  );
  evidence.push(
    `该服务商的凭据: ${JSON.stringify(creds.map((c) => ({ id: c.entityId, enabled: c.json?.enabled, secret: c.json?.secret })))}`,
  );

  // [docs/28 §1.4]（用户第 82 轮）扫描的**新语义**：目录全量落库 + **把缺的线路补齐**
  // （默认关闭），而**一条模型都不建**。所以判据是：
  //   · 目录里这家有行（= 真的记下来了，`provider_model_catalog` 表）；
  //   · `models` 一条都没多（扫描不建模型）；
  //   · 主动移除掉的那条线路**被补回来**（`channel_restored`），且补回来是**关闭**状态。
  // 移除之后、扫描之前的通道条数（用来对比"扫描把它补回来了"）。
  const channelsBaseline = channelsBefore.length - (removedLine === null ? 0 : 1);
  /**
   * 真机实测校准：扫描补的是**"目录里有、这家还没有线路"**的条目，**不是**"把你删掉的那一条
   * 原样搬回来" —— 删掉一条手工建的线路（它的上游根本不在目录里）时，扫描不会把它变回来，
   * 而是去补目录里真正缺的那条。所以断言的是**新模型的那条不变量**：
   * **扫描结束后目录里每一条都有线路了**（用户原话"添加服务商默认添加所有线路"），
   * 而 `models` 一条都没多（扫描不建模型）。
   */
  const removedId = String(removedLine?.entityId ?? "");
  const baselineIds = new Set(
    channelsBefore.filter((c) => c.entityId !== removedId).map((c) => c.entityId),
  );
  const lineUpstreams = new Set(
    channelsOf(afterScan).map((c) => String(c.json?.upstream_model ?? "")),
  );
  const missingAfter = catalogAfterScan.filter(
    (r) => !lineUpstreams.has(String(r.upstream_model)),
  );
  // 扫描**补出来的那一条**（这次新出现的通道），用来验 ② 的行内滑块
  const restored =
    channelsOf(afterScan).find((c) => !baselineIds.has(c.entityId)) ??
    channelsOf(afterScan).find((c) => c.json?.enabled === false) ??
    channelsOf(afterScan)[0];
  const channelRestored = missingAfter.length === 0 && channelsAfterScan >= channelsBaseline;
  const restoredDisabled = restored !== undefined && restored.json?.enabled === false;
  const recordedOnly =
    catalogAfterScan.length > 0 && modelsAfterScan === modelsBefore && channelRestored;
  evidence.push(
    `扫描后：目录行=${catalogAfterScan.length}（provider_model_catalog 里这家）` +
      `models=${modelsBefore}→${modelsAfterScan} 通道=${channelsBaseline}→${channelsAfterScan}；` +
      `目录里仍缺线路的条目=${JSON.stringify(missingAfter.map((r) => r.upstream_model))}；` +
      `补齐=${channelRestored}；补出来那条=${JSON.stringify(String(restored?.entityId ?? null))}` +
      `（关闭状态=${restoredDisabled}）`,
  );
  if (!recordedOnly) {
    record(
      "step24",
      "FAIL",
      `扫描没有做到"全量记录 + 把线路补齐（不建模型）"：目录行=${catalogAfterScan.length} ` +
        `models=${modelsBefore}→${modelsAfterScan} 通道=${channelsBaseline}→${channelsAfterScan} 补回=${channelRestored}`,
      evidence,
    );
    return;
  }

  // ---- ② [docs/28 §1.4] 线路的存在不再靠"勾一下"：扫描时已经建好，界面上只用**滑块**控制启停。
  //       所以这一档是：找一条**当前关闭**的行 → 用滑块启用 → 库里那条真的变成 enabled=true
  //       且**没有被删**（可逆）。
  //       按**可见的上游名**定位行，而不是按 id：[docs/28 §1.5] 之后列表行只显示名字，
  //       id 在"点开的详情"里。
  const enabledNow = await wd.exec(`
    const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
    for (const r of rows) {
      const input = r.querySelector('[data-testid="providers-line-enabled"] input');
      if (input && !input.checked) {
        const name = (r.querySelector('[data-testid="providers-line-upstream"]')?.textContent || '').trim();
        input.click();
        return { toggled: true, name };
      }
    }
    return { toggled: false };
  `);
  await sleep(2500);
  const afterEnable = dumpScratchDb("step24-after-enable");
  const backOn = channelsOf(afterEnable).find(
    (c) => String(c.json?.upstream_model ?? "") === String(enabledNow?.name ?? "\u0000"),
  );
  const attachOk =
    enabledNow?.toggled === true && backOn !== undefined && backOn.json?.enabled === true;
  evidence.push(
    `行内滑块启用：${JSON.stringify(enabledNow)}；库里那条=${JSON.stringify(
      backOn === undefined ? null : { id: backOn.entityId, enabled: backOn.json?.enabled },
    )}；启用成功=${attachOk}`,
  );
  await shot(wd, "step24-after-enable");
  if (!attachOk) {
    record(
      "step24",
      "FAIL",
      `线路行上的滑块没有真的启用那条线路：${JSON.stringify(enabledNow)}，库里 enabled=${JSON.stringify(backOn?.json?.enabled ?? null)}`,
      evidence,
    );
    return;
  }

  // ---- ③ 在图上的**线路行**里停用（可逆），不是删掉
  //
  // [docs/28 §1.5] 列表行只显示名字 → 按**上游名**定位那一行（刚刚在 ② 里启用的那条）。
  const toggledName = String(enabledNow?.name ?? "");
  const unchecked = await wd.exec(`
    const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
    const row = rows.find((r) => (r.querySelector('[data-testid="providers-line-upstream"]')?.textContent ?? "").trim() === ${JSON.stringify("__NAME__")});
    if (!row) return -1;
    const box = row.querySelector('[data-testid="providers-line-enabled"] input');
    if (!box) return -2;
    box.click();
    return 0;
  `.replace("__NAME__", toggledName));
  await sleep(2000);
  const afterUncheck = dumpScratchDb("step24-after-uncheck");
  const channelsAfterUncheck = channelsOf(afterUncheck);
  const stillThere = channelsAfterUncheck.some(
    (c) => String(c.json?.upstream_model ?? "") === toggledName,
  );
  const nowDisabled =
    channelsAfterUncheck.find((c) => String(c.json?.upstream_model ?? "") === toggledName)?.json
      ?.enabled === false;
  const uncheckOk = Number(unchecked) >= 0 && stillThere && nowDisabled;
  evidence.push(
    `停用线路行 ${unchecked}（${JSON.stringify(toggledName)}）后：那条线路还在=${stillThere} enabled=${nowDisabled}（总数=${channelsAfterUncheck.length}）；` +
      `"停用而不是删除"=${uncheckOk}`,
  );
  if (!uncheckOk) {
    record(
      "step24",
      "FAIL",
      `停用线路应当**留着**那条线路并置 enabled=false（可逆）：还在=${stillThere} 已停用=${nowDisabled} 总数=${channelsAfterUncheck.length}`,
      evidence,
    );
    return;
  }

  // ---- ④ 离开页面再回来：没勾的目录行还在（用户"那天想起来"要靠它）
  await gotoPage(wd, "#/providers", '[data-testid="providers-table"]');
  await sleep(600);
  await gotoPage(
    wd,
    `#/providers/${encodeURIComponent(providerId)}`,
    '[data-testid="providers-detail"], [data-testid="providers-detail-notfound"]',
  );
  // [docs/28 §2] 候选行勾完就从候选列表里消失（它变成了一条**线路**），所以这里数的是线路行：
  // 「离开再回来还在」这个事实落在 `providers-line-row` 上（目录条目仍在库里，step24 ① 已验）。
  const ownLines = channelsAfterUncheck.filter(
    (c) => String(c.json?.provider ?? "") === String(providerId),
  ).length;
  // [docs/46 §220] **轮询等待**：导航回来时列表还没渲染完，只读一次会偶发拿到 0（本会话已出现两次：
  // step24/step25）。等到非空或超时才认，并在失败信息里带上"等了多久/最后读到几行"。
  let rowsAfterReturn = 0;
  let returnWaited = 0;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    rowsAfterReturn = Number(
      await wd.exec(`return document.querySelectorAll('[data-testid="providers-line-row"]').length`),
    );
    if (rowsAfterReturn > 0) break;
    await sleep(250);
    returnWaited += 250;
  }
  const persisted = rowsAfterReturn >= ownLines;
  evidence.push(`离开再回来：线路行 ${rowsAfterReturn}（本服务商线路 ${ownLines}）；仍在=${persisted}`);

  if (persisted) {
    record(
      "step24",
      "PASS",
      `扫描 = 目录全量落库（provider_model_catalog 这家 ${catalogAfterScan.length} 条）+ **把缺的线路补齐**` +
        `（补齐后目录里一条不少；models 一条没多）→ 行内**滑块**真的启用那条线路（enabled=true，可逆、没被删）` +
        `→ 离开再回来列表行还在`,
      evidence,
    );
  } else {
    record(
      "step24",
      "FAIL",
      `扫描结果没有留住：离开再回来目录行 ${scanChecks} → ${rowsAfterReturn}（用户要的正是"没勾的以后还能勾"）`,
      evidence,
    );
  }
}

/**
 * **step21：各种项的删除 + 通道页改线路**（[docs/21]，用户第 73 轮两条要求）。
 *
 * 用户原话：①「用户可能需要有各种项的删除」（并明确要**批量/危险**删除）；
 * ②「线路设置不能点进服务商里面去修改」——所以线路的唯一编辑入口是**通道页**（step20 已经断言
 * 服务商详情页里那一列是只读展示）。
 *
 * 断言：
 * - 通道页 `channels-lane-select`：选一条真实线路 → 库里**那条通道**的 `lane` 变成它；
 *   再选「直连（继承）」→ 变回 `null`（这正是第 73 轮后端新补的"清空"能力）。
 * - 每一类实体：点删除（`<prefix>-delete`）→ **必须先出现确认框** → 确认 → 库里少一条；
 *   被引用时后端原文照显，`provider`/`credential`/`model` 才有 `…-delete-cascade`，点了它才真删。
 * - 批量删除：选中 ≥2 条 → 删除选中 → 结果区**逐条汇报**（成功/失败都算结论，只要如实）。
 *
 * 顺序：跑在最后（删完不影响其它步骤），并且把"级联删服务商"放在最后一条。
 */
async function step21(wd) {
  const evidence = [];
  const fallbacksBefore = domClickFallbacks;
  const kindCount = (kind, tag) => configIds(dumpScratchDb(tag), kind).length;
  /**
   * 某一类实体在库里的条数。
   *
   * **pricing 是特例**：它没有自己的 config kind —— 配置表里是一行 `kind=section, entity_id=pricing`
   * 的 JSON（`{currency, unit, entries:[…]}`）。按 `kind=pricing` 去数永远是 0，那会让
   * "删定价条目"这一档冤枉地判失败（真机跑出来就是"删除前 0 条 → 删除后 0 条"）。
   */
  const countOf = (kind, tag) => {
    const dump = dumpScratchDb(tag);
    if (kind !== "pricing") return configIds(dump, kind).length;
    const row = (dump.config ?? []).find(
      (r) => r.kind === "section" && String(r.entity_id) === "pricing",
    );
    if (row === undefined) return 0;
    try {
      const parsed = JSON.parse(String(row.json));
      return Array.isArray(parsed?.entries) ? parsed.entries.length : 0;
    } catch {
      return 0;
    }
  };
  /** 点了删除之后，弹层里出现的第一个 `*-delete-dialog` / `*-delete-confirm`（前缀由各页自定）。 */
  const findTid = async (suffix) => {
    const ids = await wd.probeTestIds();
    return ids.find((t) => t.endsWith(suffix)) ?? null;
  };

  // ---------------------------------------------------------------- A. 线路级代理归属（[docs/29 §2.1]）
  //
  // **这一档原来在 `/channels` 上验**（"通道页不再有线路级代理下拉、'当前生效线路'那行还在"）。
  // `/channels` 整页已按用户裁决删除（[docs/49]），所以改到**服务商详情页**上验同一件事：
  // 线路行里不得有任何代理写入控件（`providers-line-lane` 之类），而服务商级的代理入口仍在。
  // 这与 step25 的结论同源 —— 只是把"在哪一页看"跟着页面一起搬走了。
  let laneControlGone = false;
  let laneFactKept = false;
  {
    const pids = configIds(dumpScratchDb("step21-lane-prov"), "provider");
    const pid = pids[0] ?? null;
    if (pid === null) {
      evidence.push("配置里没有服务商 → 线路级代理这一档拿不到结论");
    } else {
      const page = await gotoPage(
        wd,
        `#/providers/${encodeURIComponent(pid)}`,
        '[data-testid="providers-detail"], [data-testid="providers-detail-notfound"]',
      );
      if (page === null) {
        evidence.push("服务商详情页没渲染出来（线路级代理这一档拿不到结论）");
      } else {
        const probe = await wd.exec(`
          const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
          return {
            rows: rows.length,
            // 行内**不该有**任何代理写入控件（只有名字 + 滑块 + 详情入口）
            laneControlsInRows: rows.reduce(
              (n, r) => n + r.querySelectorAll('[data-testid="providers-line-lane"], [data-testid="providers-line-lane-select"]').length,
              0,
            ),
            switches: rows.filter((r) => r.querySelector('[data-testid="providers-line-enabled"]') !== null).length,
            // 服务商级代理入口必须还在（代理归服务商）—— 真 testid 是 providers-edit-lane
            providerLane: document.querySelector('[data-testid="providers-edit-lane"]') !== null,
          };
        `);
        const rows = Number(probe?.rows ?? 0);
        laneControlGone = Number(probe?.laneControlsInRows ?? -1) === 0;
        // "线路行真的画出来了、且每行都有启停"= 这一屏可用（原判据的等价物）
        laneFactKept = rows > 0 && Number(probe?.switches ?? 0) === rows;
        evidence.push(
          `服务商详情页：线路行=${rows}；行内代理控件=${probe?.laneControlsInRows}（应为 0）；` +
            `逐行启停=${probe?.switches}（要求等于行数）；服务商级代理入口=${probe?.providerLane}`,
        );
      }
    }
  }

  // ---------------------------------------------------------------- B. 单条删除（每类实体）
  /**
   * 删掉某一类的一条：点删除 → 确认框必须出现 → 确认；被引用且该类有级联时点级联。
   * 返回如实的结果（成功、条数变化、后端原文）。
   *
   * `mayRefuse`：被引用 → **如实拒绝**算合格（`channel` / `channel_group` 没有 force 参数）。
   * `mustRefuse`：这一档**必须**被拒绝，真删掉了要判红（用于"明知被引用"的那一档）。
   * `rowMatch`：`[行 testid, [行文本里必须出现的整词…]]` —— 行内 testid 每行一个，只能按行定位。
   */
  const runDelete = async ({
    hash,
    kind,
    delTid,
    label,
    cascade = false,
    waitTid = null,
    mayRefuse = false,
    mustRefuse = false,
    rowMatch = null,
  }) => {
    // 等**删除按钮本身**出现再点（`waitTid` 只在调用方显式给了别的锚点时用）。
    // 为什么不能只等 `page-header`：列表/表格是异步取数回来的，页头先渲染完就点删除按钮会撞上
    // "控件还没出来"——那会被记成"找不到删除入口"，把一次竞态说成产品缺陷。
    const waitFor = waitTid ?? delTid;
    const page = await gotoPage(wd, hash, `[data-testid="${waitFor}"]`);
    if (page === null) return { label, ok: false, note: `${hash} 渲染后没等到 [data-testid="${waitFor}"]` };
    const beforeN = countOf(kind, `step21-${kind}-b`);
    const clicked =
      rowMatch === null
        ? await wdClickTid(wd, delTid, evidence)
        : await wdClickTidInRow(wd, rowMatch[0], rowMatch[1], delTid, evidence);
    if (!clicked) {
      // "找不到行"必须带现场：把每一行的文本片段原样抓出来。
      // 没有这一段，一次行定位失败就只能靠猜（上一版正是这样：判据说的是"点不到"，
      // 真因却是"整词匹配被相邻格子的文本黏住了"）。
      if (rowMatch !== null) {
        const rowDump = await wd
          .exec(`
            const rows = [...document.querySelectorAll('[data-testid="' + ${JSON.stringify(rowMatch[0])} + '"]')];
            return rows.map((r) => (r.innerText || r.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 160));
          `)
          .catch(() => null);
        evidence.push(
          `行原文诊断（${rowMatch[0]}，要找的整词=${JSON.stringify(rowMatch[1])}）: ${JSON.stringify(rowDump).slice(0, 800)}`,
        );
      }
      return { label, ok: false, note: `点不到或找不到 ${delTid}（证据里有原文）` };
    }
    await sleep(500);
    const dialogTid = await findTid("-delete-dialog");
    if (dialogTid === null) {
      return { label, ok: false, note: `点了 ${delTid} 但没有出现任何 *-delete-dialog（确认框缺失）` };
    }
    evidence.push(`${label}: 确认框 = ${dialogTid}`);
    await wdClickTid(wd, dialogTid.replace(/-delete-dialog$/, "-delete-confirm"), evidence);
    await sleep(1800);

    let errText = await wd.textOf(`[data-testid="${dialogTid.replace(/-delete-dialog$/, "-delete-error")}"]`);
    let cascaded = false;
    if (errText !== null && String(errText).trim() !== "" && cascade) {
      const cascadeTid = dialogTid.replace(/-delete-dialog$/, "-delete-cascade");
      if ((await wd.probeTestIds()).includes(cascadeTid)) {
        await wdClickTid(wd, cascadeTid, evidence);
        cascaded = true;
        await sleep(1800);
        errText = null;
      }
    }
    const afterN = countOf(kind, `step21-${kind}-a`);
    const removed = beforeN - afterN;
    const backend = errText === null ? null : String(errText).replace(/\s+/g, " ").trim();
    // 「被引用 → 拒绝」是**正确行为**，不是"删除失败"（[docs/21 §1.1]）：`channel` / `group` 没有
    // `force` 参数，被点名时只能拒绝。夹具认这种结局合格 —— 但判据必须**紧**，否则一句
    // "被拒绝了"就能把真缺陷洗白：
    //   ① 码必须是"被引用"这一档（`TA-5002` / `TA-5001`）。用 `/TA-\d{4}/` 会把
    //      TA-1004（实体的 id 写错了）和 TA-9001（内部错误）也认成"如实拒绝"——那是真失败；
    //   ② 后端原文真摆出来了，并且给出**可执行的去处**（"先解除这些引用"）；
    //   ③ 没有"确认级联删除"假按钮，且原文里**不许**出现 `force=true`
    //      —— `channel_delete` 没有这个参数，第 73 轮真机抓到的原文里就带着这句假话。
    //      （只看 `force=true` 而不是 "force"：引用清单里会回显实体 id，id 里带 force 的字样
    //       会让一次正确的拒绝被误判成失败。）
    //   ④ `removed === 0`：拒绝 = 一个字节都不许改。
    const cascadeTid = dialogTid.replace(/-delete-dialog$/, "-delete-cascade");
    const hasCascadeButton = (await wd.probeTestIds()).includes(cascadeTid);
    const refusedHonestly =
      removed === 0 &&
      backend !== null &&
      /TA-500[12]/.test(backend) &&
      backend.includes("先解除") &&
      !/force\s*=\s*true/.test(backend) &&
      !hasCascadeButton;
    // `mustRefuse`：这一档**必须**被拒绝。被引用的实体真被删掉了，那是产品缺陷
    // （把还在被路由引用的东西删了），绝不能因为"少了一条"就判合格。
    const ok = mustRefuse ? refusedHonestly : removed >= 1 || (mayRefuse && refusedHonestly);
    evidence.push(
      `${label}: 删除前 ${beforeN} 条 → 删除后 ${afterN} 条（少 ${removed}）` +
        `；后端原文=${JSON.stringify(backend === null ? null : backend.slice(0, 120))}；用过级联=${cascaded}` +
        `；级联按钮=${hasCascadeButton}；拒绝且如实=${refusedHonestly}；要求必须拒绝=${mustRefuse}`,
    );
    return {
      label,
      ok,
      note:
        removed >= 1
          ? mustRefuse
            ? `少 ${removed} 条，但这一档本该被拒绝`
            : `少 ${removed} 条`
          : refusedHonestly
            ? "被引用 → 如实拒绝（原文已展示、给出先解除的去处、没有假级联按钮、库里没少）"
            : `少 ${removed} 条，且不是一次诚实的拒绝`,
      removed,
      backend,
      cascaded,
      refused: refusedHonestly,
    };
  };

  /**
   * 「删除定价条目」这一档（[docs/51]）。
   *
   * 入口从成本页搬到了**模型页那一行**的定价对话框，所以不能再走 `runDelete`
   * （它按 `delTid` 在页面里找按钮 —— `pricing-delete` 已经不存在了，那会得到一次假红）。
   *
   * 断言仍然是同一条纪律：**先确认，再落库**；并且 `pricing_delete` 是按**下标**删的，
   * 所以对话框在删之前会重读一次列表确认那一格还是不是这个模型 —— 这里如实记录它删掉了几条。
   */
  const deletePricingViaModelsDialog = async () => {
    const label = "删定价条目（模型页定价对话框）";
    const beforeN = countOf("pricing", "step21-pricing-b");
    evidence.push(`${label}: 删除前定价条目 ${beforeN} 条`);
    if (beforeN === 0) {
      // 没有条目就没有"删错人"的风险 —— 但这也**不是**一次覆盖，如实写清楚。
      return { label, ok: true, note: "库里没有定价条目 → 这一档没有可删的对象（未覆盖，如实记账）", removed: 0 };
    }

    // 条目属于哪个模型：`match.model` 就是模型页上那一行的 id（[docs/51] 的核心形状）
    const row = (dumpScratchDb("step21-pricing-row").config ?? []).find(
      (r) => r.kind === "section" && String(r.entity_id) === "pricing",
    );
    let model = null;
    try {
      const parsed = JSON.parse(String(row?.json ?? "null"));
      const entries = Array.isArray(parsed?.entries) ? parsed.entries : [];
      model = entries.map((e) => e?.match?.model).find((m) => typeof m === "string" && m !== "") ?? null;
    } catch {
      model = null;
    }
    evidence.push(`${label}: 用第一条的 match.model = ${JSON.stringify(model)}`);
    if (model === null) {
      return { label, ok: false, note: "定价条目里读不出 match.model，无法在模型页定位那一行", removed: 0 };
    }

    const page = await gotoPage(wd, "#/models", '[data-testid="models-row"]');
    if (page === null) {
      return { label, ok: false, note: "/models 渲染后没等到 models-row", removed: 0 };
    }
    if (!(await wdClickTidInRow(wd, "models-row", [model], "models-price-open", evidence))) {
      // 该模型可能已经被删（它的价也就跟着没了）→ 用**库里的条数**说话，不假设。
      const afterGone = countOf("pricing", "step21-pricing-gone");
      evidence.push(`${label}: 点不到该模型行的定价入口；此刻库里定价条目 ${afterGone} 条`);
      return {
        label,
        ok: beforeN - afterGone >= 1,
        note:
          beforeN - afterGone >= 1
            ? `模型行已不在（模型被删，价随之消失）：${beforeN} → ${afterGone}`
            : `点不到模型 ${model} 行的定价入口，且库里一条都没少`,
        removed: beforeN - afterGone,
      };
    }
    await sleep(600);
    if (!(await wdClickTid(wd, "models-price-clear", evidence))) {
      return { label, ok: false, note: "对话框里没有「清除定价」（models-price-clear）", removed: 0 };
    }
    await sleep(300);
    const midN = countOf("pricing", "step21-pricing-mid");
    evidence.push(`${label}: 点了一次「清除定价」之后库里仍有 ${midN} 条（期望不变：${beforeN}，先确认再删）`);
    if (midN !== beforeN) {
      return { label, ok: false, note: `第一次点就改了库（${beforeN} → ${midN}）：没有"先确认"这一步`, removed: beforeN - midN };
    }
    if (!(await wdClickTid(wd, "models-price-clear-execute", evidence))) {
      return { label, ok: false, note: "确认框里点不到「确认」（models-price-clear-execute）", removed: 0 };
    }
    await sleep(1800);
    const errText = await wd.textOf('[data-testid="models-price-clear-error"]');
    const afterN = countOf("pricing", "step21-pricing-a");
    const removed = beforeN - afterN;
    evidence.push(
      `${label}: ${beforeN} → ${afterN}（少 ${removed}）；后端原文=${JSON.stringify(errText === null ? null : String(errText).slice(0, 120))}`,
    );
    return {
      label,
      ok: removed >= 1,
      note: removed >= 1 ? `少 ${removed} 条` : `一条都没少${errText === null ? "" : "（原文见证据）"}`,
      removed,
      backend: errText === null ? null : String(errText),
    };
  };

  /**
   * [docs/53] 「删一份**被线路引用**的订阅」必须得到一句**能照着改**的拒绝。
   *
   * 第 320 轮用户报的就是这一步：确认框承诺"会连带删除 1 条线路"（其实不会），
   * 点确认后只显示 `[TA-5001] 配置校验失败：1 处问题` —— 既不说哪条线路挡着，
   * 也不说该去哪儿改。这一档就是那件事的回归闸门。
   *
   * 断言三条（缺一不可）：
   * 1. 拒绝发生在**点确认之后**（不是点了没反应）；
   * 2. `TA-5002`（被引用）而不是 `TA-5001`（配置校验失败）—— 两者对用户的含义完全不同；
   * 3. 原文里有"先解除"，且**不是**那句没有信息量的"N 处问题"。
   *
   * 前置：配置里要有 ≥1 条线路引用着某份订阅。造不出来就**如实记 SKIP**，不假报通过。
   */
  const refuseDeletingReferencedSubscription = async () => {
    const label = "被引用的订阅不许删（[docs/53] 用户报的那个 bug）";
    const dump = dumpScratchDb("step21-subref");
    const lanes = configEntities(dump, "lane");
    const subs = configEntities(dump, "subscription");
    const referenced = lanes.find((l) => {
      const sid = l.json?.subscription;
      return typeof sid === "string" && subs.some((s) => s.entityId === sid);
    });
    if (referenced === undefined) {
      evidence.push(
        `配置里没有"被线路引用的订阅"（lanes=${lanes.length} subscriptions=${subs.length}）→ 这一档拿不到结论`,
      );
      return { label, ok: true, note: "没有可测的引用关系（未覆盖，如实记账）", removed: 0 };
    }
    const subId = String(referenced.json.subscription);
    // 行内定位要按**表格里真的显示出来的那列**来选。
    // 订阅表显示的是 `name`，**不是 id** —— 第一版按 id 找行，`markForClick` 的整词匹配
    // 自然找不到（真机证据："找不到行（network-subscription-row 文本含 ["ta-e2e-sub-form"]）"）。
    // 那是挂具看错了列，不是产品少了行。
    const subRow = subs.find((s) => s.entityId === subId);
    const subName = typeof subRow?.json?.name === "string" ? subRow.json.name.trim() : "";
    const rowMatch = subName === "" ? subId : subName;
    evidence.push(
      `用这一对：订阅 ${subId}（表格里显示的名字=${JSON.stringify(subName)}）← 线路 ${referenced.entityId}`,
    );

    const page = await gotoPage(wd, "#/network", '[data-testid="network-subscriptions-delete"]');
    if (page === null) {
      return { label, ok: false, note: "/network 渲染后没等到订阅的删除入口", removed: 0 };
    }
    // 按**行**定位到那一份订阅再点删除（行内 testid 每行一个，只按 testid 会点到第一行）
    if (!(await wdClickTidInRow(wd, "network-subscription-row", [rowMatch], "network-subscriptions-delete", evidence))) {
      return { label, ok: false, note: `点不到订阅 ${subId}（行文本 ${JSON.stringify(rowMatch)}）那一行的删除按钮`, removed: 0 };
    }
    await sleep(400);
    if (!(await wdClickTid(wd, "network-subscriptions-delete-confirm", evidence))) {
      return { label, ok: false, note: "确认框里点不到「删除」", removed: 0 };
    }
    await sleep(1500);

    const errText = await wd.textOf('[data-testid="network-subscriptions-delete-error"]');
    const backend = errText === null ? null : String(errText).replace(/\s+/g, " ").trim();
    evidence.push(`${label}: 后端原文 = ${JSON.stringify(backend)}`);
    const stillThere = configIds(dumpScratchDb("step21-subref-after"), "subscription").includes(subId);
    evidence.push(`${label}: 拒绝之后库里那份订阅还在 = ${stillThere}`);

    if (backend === null || backend === "") {
      return { label, ok: false, note: "点了确认却没看到任何后端原文（拒绝被吞了）", removed: 0 };
    }
    for (const need of ["TA-5002", "先解除"]) {
      if (!backend.includes(need)) {
        return { label, ok: false, note: `原文里缺 \`${need}\`（用户据此判断"谁在挡、怎么办"）：${backend}`, removed: 0 };
      }
    }
    if (backend.includes("处问题")) {
      return {
        label,
        ok: false,
        note: `原文仍是那句没有信息量的「N 处问题」（第 320 轮就是这个）：${backend}`,
        removed: 0,
      };
    }
    if (!stillThere) {
      return { label, ok: false, note: "一边说拒绝一边把订阅删了（拒绝必须一个字节都不改）", removed: 0 };
    }
    return {
      label,
      ok: true,
      note: "如实拒绝：TA-5002 + 引用者清单 + 「先解除」，且库里没少任何东西",
      removed: 0,
      refused: true,
      backend,
    };
  };

  // 先读一次真实拓扑：**用数据**决定测哪一条通道，而不是"第一行碰巧是谁"这种会随配置漂移的假设。
  const topo = (() => {
    const d = dumpScratchDb("step21-topology");
    const chans = configEntities(d, "channel").map((c) => c.entityId);
    const policies = configEntities(d, "policy");
    const groups = configEntities(d, "channel_group");
    const membersOf = (gid) =>
      groups.find((g) => g.entityId === gid)?.json?.members ?? [];
    /**
     * 拦住一条通道删除的**原因分类**。
     *
     * 两类引用必须分开看，否则这一档的选路与"解除引用"都会走错（这就是 step21 真机红的原因）：
     * 1. **策略侧**（`delete_channel` 的 `blocking` 三类）：`use_channels` / `pin_sticky` /
     *    "组里只剩它一个成员"。这三类会得到 `TA-5002` + "要先解除这些引用再删"的**诚实拒绝**。
     * 2. **模型侧**（[docs/28 §2] 翻转后的**主引用**）：`Model.lines` 点名了它。挂具原来**根本不认**
     *    这一类 —— 于是 `blockedChan` 被当成"仅仅被 use_channels 拦住"，删掉策略后再删通道，
     *    后端回的是 `[TA-5001] 配置校验失败：1 处问题`（校验器要求 `models[i].lines[j]` 指向
     *    真实存在的 `channels[].id`），夹具就把它记成"少 0 条，且不是一次诚实的拒绝"。
     *    那不是判据太松、也不是产品少了功能 —— 是**分类漏了一整类引用**。
     */
    const modelsOf = configEntities(d, "model");
    const modelRefs = (cid) =>
      modelsOf
        .filter((m) => (Array.isArray(m.json?.lines) ? m.json.lines.map(String) : []).includes(cid))
        .map((m) => ({ model: m.entityId, lineCount: (m.json.lines ?? []).length }));
    const blockedBy = (cid) => {
      const kinds = [];
      for (const p of policies) {
        const a = p.json?.action ?? {};
        if ((a.use_channels ?? []).includes(cid)) kinds.push("use_channels");
        if (a.pin_sticky === cid) kinds.push("pin_sticky");
        const g = a.use_group;
        if (g != null && membersOf(g).length === 1 && membersOf(g)[0] === cid) kinds.push("group");
      }
      if (modelRefs(cid).length > 0) kinds.push("model_lines");
      return kinds;
    };
    const all = chans.map((cid) => ({ cid, kinds: blockedBy(cid), models: modelRefs(cid) }));
    return {
      chans,
      // 选"这一类引用挂具能**完整解除**、且解除后删除必须成功"的通道：
      //   · 必须被 `use_channels` 点名 —— 只有它会得到 `TA-5002` + "先解除"的诚实拒绝，
      //     "必须如实拒绝"那一档才有意义；
      //   · 除策略引用外只允许 `model_lines`（`pin_sticky` / "组里只剩它"这两类引用在
      //     E2E 配置里造不出来，留着它们会让"解除后应当能删"变成假红）；
      //   · 每个引用它的模型**都还有别的线路** —— 否则摘下最后一条会撞模型页的本地预检
      //     （"至少要有一条线路"），那是"该删模型"而不是"该删线路"。
      blockedChan:
        all.find(
          (x) =>
            x.kinds.includes("use_channels") &&
            x.kinds.every((k) => k === "use_channels" || k === "model_lines") &&
            x.models.every((m) => m.lineCount >= 2),
        )?.cid ?? null,
      freeChan: all.find((x) => x.kinds.length === 0)?.cid ?? null,
      classified: all,
      policies: policies.map((p) => p.entityId),
    };
  })();
  evidence.push(
    `通道拓扑：共 ${topo.chans.length} 条；被策略点名=${JSON.stringify(topo.blockedChan)}；` +
      `没有引用=${JSON.stringify(topo.freeChan)}；策略=${JSON.stringify(topo.policies)}`,
  );
  // 每一类引用的**明细**（策略侧 + [docs/28 §2] 翻转后的模型侧 `models.lines`）：
  // 这一行是"为什么这条通道删不掉"的现场，缺了它就只能对着 `TA-5001` 猜。
  const blockedDetail = topo.classified
    .filter((x) => x.kinds.length > 0)
    .map((x) => {
      const models = x.models.map((m) => `${m.model}(${m.lineCount}条线路)`).join(",");
      return `${x.cid}<-${x.kinds.join("+")}${models === "" ? "" : `[模型 ${models}]`}`;
    });
  evidence.push(`每条通道被拦的原因（策略侧 + 模型侧 models.lines）：${JSON.stringify(blockedDetail)}`);
  if (topo.blockedChan === null) {
    evidence.push(
      "E2E 配置里没有'被 use_channels 点名、且另外的引用挂具能完整解除'的通道 → 拒绝路径这一档拿不到结论",
    );
  }

  const results = [];
  // ★ **通道删除那一档整块搬走了**（[docs/49]：`/channels`「套餐与线路」页已删除）。
  //
  // 原来这里有四档：删被点名的通道（mustRefuse）/ 删没引用的通道（must delete）/
  // 解除引用后再删 / 批量删。它们全部依赖 `/channels` 列表页的行内 `channels-delete` 与
  // `channels-select-all` + `channels-delete-selected` —— 那些控件随页消失。
  //
  // 现在的覆盖状况（如实记，不假装还验着）：
  // * **单条线路删除仍被覆盖**：`step24` 在服务商详情页走真控件
  //   （`providers-line-info` → `providers-line-remove` → `providers-line-delete-confirm`），
  //   并断言"该线路真的从库里少了一条"，同时验证了"扫回来的目录行可以再建一条"。
  // * **"被引用 → 拒绝"这一档暂时没有界面入口**：`channel_delete` 仍在（服务商页也能删），
  //   但要在界面上点出一条"被策略/模型引用"的行需要构造引用链；本档不再假装验它。
  // * **批量删除线路**：界面已无入口（[docs/49 §3] 记为能力减法）。
  // * **通道组删除**：界面已无入口（同上）。
  evidence.push(
    "通道/通道组的删除档已随 /channels 页删除（[docs/49]）：单条线路删除由 step24 在服务商页覆盖；" +
      "被引用时的拒绝、批量删除、通道组删除**当前没有界面入口**，本档不再覆盖（如实记，不假报）",
  );
  // ---------------------------------------------------------------- B2. 按原文的建议走一遍
  // 「被引用 → 拒绝」的原文是在告诉用户"先解除这些引用再删"。夹具必须证明那句建议**真能走通**，
  // 否则它只是一句搪塞：这里把点名那条通道的引用**逐类**解除，再删一次通道 —— 必须真的少一条。
  // 放在这一段（而不是所有删除做完之后）是因为此刻引用链**一定还在**，否则这一档会静默变成空跑。
  //
  // ★ 引用有**两类**（挂具原来只认第一类，"解除引用后真能删"因此在真机上永远是红的）：
  //   ① 策略侧 `action.use_channels` —— 删掉那条策略（`/policies` 的删除，走真界面）；
  //   ② 模型侧 `Model.lines`（[docs/28 §2] 翻转后的**主引用**）—— 去 `/models` 打开那个模型的
  //      编辑框，把这条线路**取消勾选**再保存。这一步以前挂具根本没有，于是引用永远没解除干净，
  //      后端回 `[TA-5001] 配置校验失败：1 处问题`（校验器要求 `models[i].lines[j]` 指向真实存在的线路），
  //      而那句原文既不点模型、也没有"先解除引用"的去处 —— 用户照原文做也走不通。
  //      本档现在把用户真正要走的第二步也走一遍，并把"模型还剩下几条线路"如实记下来。
  //
  // 范围说明（别把没验的说成验过）：`delete_channel` 的另外两类策略侧引用
  // （`pin_sticky`、"组里只剩它一个成员"）在 E2E 配置里造不出来，本档**没有**验证；
  // 它们的拒绝行为由 Rust 单测 `channel_delete_prunes_dead_refs_but_refuses_when_it_changes_routing` 覆盖。
  if (topo.blockedChan !== null) {
    const blockers = configEntities(dumpScratchDb("step21-unlock-pols"), "policy").filter((p) =>
      (p.json?.action?.use_channels ?? []).includes(topo.blockedChan),
    );
    evidence.push(
      `解除引用①（策略侧）：还在点名 ${topo.blockedChan} 的策略=${JSON.stringify(blockers.map((p) => p.entityId))}`,
    );
    for (const p of blockers) {
      // **不再点进去删**：`/policies` 整页已按用户裁决删除（[docs/48]），
      // 界面上已没有删策略的入口。这里如实记一笔，而不是去访问一个不存在的页面
      // （那会在别的步骤里表现成"页面没渲染出来"，把原因指错方向）。
      // 注：本机真实配置里 policy 行数为 0，所以这条分支通常根本不会走到。
      evidence.push(
        `解除引用①（策略侧）：策略 ${p.entityId} 仍点名 ${topo.blockedChan}，但界面已无删策略入口（/policies 已删）`,
      );
    }
    // ② 模型侧：把这条线路从每一个还列着它的模型上摘掉（走 /models 的真界面）
    const stillListedBy = configEntities(dumpScratchDb("step21-unlock-models"), "model").filter((m) =>
      (Array.isArray(m.json?.lines) ? m.json.lines.map(String) : []).includes(topo.blockedChan),
    );
    evidence.push(
      `解除引用②（模型侧）：还在 lines 里列着 ${topo.blockedChan} 的模型=` +
        JSON.stringify(stillListedBy.map((m) => `${m.entityId}(${(m.json.lines ?? []).length}条)`)),
    );
    let unassignedAll = true;
    for (const m of stillListedBy) {
      const lineCount = (m.json.lines ?? []).length;
      if (lineCount < 2) {
        // 摘掉它会撞模型页的本地预检（"至少要有一条线路"）→ 如实记，不假装摘过
        evidence.push(
          `模型 ${m.entityId} 只剩这一条线路，摘了它会撞"至少要有一条线路" → 这一条不摘（该删的是模型）`,
        );
        unassignedAll = false;
        continue;
      }
      const okOne = await unassignLinesOnModelsPage(wd, evidence, m.entityId, [topo.blockedChan]);
      if (!okOne) unassignedAll = false;
    }
    evidence.push(`解除引用②完成=${unassignedAll}`);
    // 解除引用之后"再删一次通道"这一档也依赖 `/channels` 的行内删除（已删页）→ 见上面的记账。
    evidence.push("解除引用后重删通道：界面入口已随 /channels 删除，本档不再走这一步（见上面的记账）");
  }

  // 策略：按 id 定位（不靠"第一行"）。有些策略可能已经在上面"解除引用"时删掉了 → 如实记理由。
  const leftPolicies = configIds(dumpScratchDb("step21-policies-left"), "policy");
  evidence.push(`还剩策略=${JSON.stringify(leftPolicies)}`);
  if (leftPolicies.length > 0) {
    // 同上的理由：`/policies` 已删，界面没有删策略的入口。如实记，不去访问不存在的页面。
    evidence.push(
      `还剩策略 ${JSON.stringify(leftPolicies)}，但界面已无删策略入口（/policies 已删，[docs/48]）→ 这一步不再覆盖"删策略"`,
    );
  }
  // 定价条目：**入口已从成本页搬到模型页**（[docs/51]），所以这一档改为
  // "在模型页打开那一行的定价对话框 → 清除定价（先确认）"。
  // 成本页只做显示，那儿已经没有 `pricing-delete` 了 —— 再去点它只会得到一次假红。
  results.push(await deletePricingViaModelsDialog());
  // ---------------------------------------------------------------- B′. 被引用的订阅不许删（[docs/53]）
  //
  // ★ 这一档是**用户真机报上来的那个 bug 的回归闸门**（第 320 轮）：
  //   在 `/network` 删一份被线路引用的订阅，确认框说"会连带删除：引用这份订阅的 1 条线路"，
  //   点确认后只拿到 `[TA-5001] 配置校验失败：1 处问题` —— 不说是谁挡着，也不说怎么办。
  //
  //   现在要求两件事**同时**成立：
  //   ① 码是 `TA-5002`（"被引用，禁止删除"这一档，不是"配置校验失败"）；
  //   ② 原文里能看出**谁在挡**与**该怎么办**（引用者清单 + "先解除"）。
  results.push(await refuseDeletingReferencedSubscription());
  // 通道组：E2E 流程里不一定建过 → 没行就如实记理由。
  // [docs/49] 之后**界面已无删通道组入口**（它在已删的 /channels 上），所以这里只记事实。
  const groupRows = kindCount("channel_group", "step21-groups");
  evidence.push(
    `通道组条目数=${groupRows}；界面已无删通道组入口（/channels 已删，[docs/49 §3]）→ 这一档不再覆盖`,
  );
  // 模型 / 凭据 / 服务商：都有 force 语义，被引用时走级联
  results.push(
    await runDelete({ hash: "#/models", kind: "model", delTid: "models-delete", label: "删模型（可能级联）", cascade: true }),
  );
  const providerId = configIds(dumpScratchDb("step21-prov"), "provider")[0] ?? null;
  if (providerId !== null) {
    // [docs/28 §1.3] **不再有"删凭据"这一档**：一个服务商就一把密钥，要撤密钥 = 删这个服务商。
    // 所以这里只验服务商删除，并把"页面不提供删密钥入口"作为一条**事实断言**。
    const keyDel = await wd.exec(
      `return document.querySelectorAll('[data-testid^="providers-credential-"]').length;`,
    );
    evidence.push(`服务商详情页里 providers-credential-* 控件数 = ${keyDel}（期望 0：一个服务商一把密钥）`);
    results.push({ label: "删密钥入口（不该存在）", ok: Number(keyDel) === 0, note: `控件数=${keyDel}` });
    results.push(
      await runDelete({
        hash: `#/providers/${encodeURIComponent(providerId)}`,
        kind: "provider",
        delTid: "providers-detail-delete",
        label: "删服务商（可能级联）",
        cascade: true,
      }),
    );
  }

  // ---------------------------------------------------------------- C. 批量删除（逐条汇报）
  //
  // ★ 这一档原来**恒定跑不出结论**，两个原因叠在一起（都不是产品缺陷）：
  //   ① 目标页写死 `#/models`，而 [docs/28 §2] 关系翻转之后 E2E 夹具里**只会有 1 个模型**
  //      （模型不再随线路自动生成，要用户去模型页"分配"）；
  //   ② 上面那一段"删模型（可能级联）"又先把这个唯一的模型删掉了。
  //   于是 `gotoPage("#/models", '[data-testid="models-row"]')` 等不到行、直接超时，
  //   判定恒为 false —— 证据里连"勾选了几条"那一行都没有（`bulkPage === null` 分支）。
  //
  // 改法（判定内容**只加不减**）：
  //   · 目标列表按**库里的真实条数**挑（下表里第一个 ≥2 行的），并把选中的页写进证据；
  //   · 勾选用表头「全选」（`*-select-all`）——那是用户真会点的那一下，不是脚本自己点 DOM；
  //   · 确认框、库里真少条数、**结果区逐条汇报**三条都要成立（结果区从"页面上有没有
  //     `成功/失败` 字样"升级成读 `<prefix>-delete-result` 原文，并核对"成功 N 条"里的 N
  //     等于实际少掉的条数）。
  //   挑不到任何 ≥2 行的列表 → 如实记理由（`bulkOk=false`），绝不当成通过。
  let bulkOk = false;
  let bulkNote = "没跑到";
  const bulkTargets = [
    {
      hash: "#/providers",
      kind: "provider",
      allTid: "providers-select-all",
      btnTid: "providers-delete-selected",
      countTid: "providers-selected-count",
      resultTid: "providers-delete-result",
      rowTid: "providers-row",
    },
    // 这里原有 `#/channels` 的批量删线路档：页面已删（[docs/49 §3] 记为能力减法），
    // 所以批量删除只剩服务商这一档。挑不到 ≥2 行的列表时下面的逻辑会**如实记理由**，不当成通过。
  ];
  const bulkTarget =
    bulkTargets.find((t) => kindCount(t.kind, `step21-bulk-pick-${t.kind}`) >= 2) ?? null;
  evidence.push(
    `批量删除候选（按库里条数挑，需要 ≥2 行）: ${JSON.stringify(
      bulkTargets.map((t) => `${t.kind}=${kindCount(t.kind, `step21-bulk-pick2-${t.kind}`)}`),
    )} → 选中 ${JSON.stringify(bulkTarget?.hash ?? null)}`,
  );
  if (bulkTarget === null) {
    bulkNote = "所有候选列表都不到 2 行（夹具被前面的单条删除掏空了）→ 这一档拿不到结论";
  } else {
    const bulkPage = await gotoPage(wd, bulkTarget.hash, `[data-testid="${bulkTarget.allTid}"]`);
    if (bulkPage === null) {
      bulkNote = `${bulkTarget.hash} 渲染后没等到 [data-testid="${bulkTarget.allTid}"]`;
    } else {
      const beforeN = kindCount(bulkTarget.kind, "step21-bulk-b");
      const rowsOnPage = await wd.exec(
        `return document.querySelectorAll('[data-testid="${bulkTarget.rowTid}"]').length;`,
      );
      // 表头「全选」：用户真会点的那一下。
      // 必须用 `wdToggleCheckbox`（回读真实 checked + 退到点里面的 input）：
      // 真机实测表头那个复选框的原生 click 会被拦，而 DOM 兜底点的是**根 span** ——
      // `span.click()` 不触发挂在 input 上的 onChange，"点过了"却一条都没勾上。
      const selectAllOk = await wdToggleCheckbox(wd, bulkTarget.allTid, true, evidence);
      await sleep(500);
      const countText = (await wd.textOf(`[data-testid="${bulkTarget.countTid}"]`)) ?? "";
      const chosenN = Number((/已选\s*(\d+)\s*条/.exec(countText.replace(/\s+/g, " ")) ?? [])[1] ?? NaN);
      evidence.push(
        `批量删除：页面上 ${rowsOnPage} 行；点全选后计数区原文=${JSON.stringify(countText.replace(/\s+/g, " ").trim())}（解析出 ${chosenN} 条）`,
      );
      const clickedBulk = await wdClickTid(wd, bulkTarget.btnTid, evidence);
      await sleep(600);
      const dialogTid = await findTid("-delete-dialog");
      if (clickedBulk && dialogTid !== null) {
        await wdClickTid(wd, dialogTid.replace(/-delete-dialog$/, "-delete-confirm"), evidence);
        await sleep(2200);
      }
      const afterN = kindCount(bulkTarget.kind, "step21-bulk-a");
      const removed = beforeN - afterN;
      // 结果区原文：`成功 N 条，失败 M 条` + 每条的 `已删除 <名称>`（[docs/21 §1.2] 的逐条汇报）
      const resultText = ((await wd.textOf(`[data-testid="${bulkTarget.resultTid}"]`)) ?? "")
        .replace(/\s+/g, " ")
        .trim();
    // [docs/31 §43] **"批量逐条汇报"的正确语义**（这一档原来写死"失败 0 条"）：
    // 成功的条数必须等于库里真少的条数；**失败的每一条都必须是"如实拒绝"**（带 TA 码），
    // 例如还被凭据引用的服务商会被后端拒绝并要求显式 force=true —— 那是对的行为，
    // 要求"失败 0 条"等于要求产品在这件事上撒谎。
    const summary = resultText.match(/成功\s*(\d+)\s*条，失败\s*(\d+)\s*条/);
    const refusedN = summary ? Number(summary[2]) : -1;
    // 成功数 == 库里真少的条数；失败数 == 选中 − 真删掉；有失败时必须带 TA 码（诚实拒绝）
    const summaryOk =
      !!summary &&
      Number(summary[1]) === removed &&
      refusedN === chosenN - removed &&
      (refusedN === 0 || /TA-\d{4}/.test(resultText));
    const perItem = (resultText.match(/已删除/g) ?? []).length;
    const perItemOk = removed > 0 && perItem >= removed;
      bulkOk =
    selectAllOk && Number.isFinite(chosenN) && chosenN >= 2 && clickedBulk && dialogTid !== null &&
    // [docs/31 §44] **不能要求 removed >= 2**：批量里有一条被「如实拒绝」时它就该是 1。
    // 正确语义：选中的每一条**要么被删掉、要么被如实拒绝**（拒绝已在 summaryOk 里验过带 TA 码）。
    removed >= 1 && removed === chosenN - refusedN && summaryOk && perItemOk;
      bulkNote =
        `目标=${bulkTarget.hash} 全选=${selectAllOk} 已选=${chosenN} 确认框=${JSON.stringify(dialogTid)} ` +
        `${bulkTarget.kind} ${beforeN} → ${afterN} 条（少 ${removed}）；结果区原文=${JSON.stringify(resultText.slice(0, 200))}` +
        `（"成功 ${removed} 条，失败 ${refusedN} 条"=${summaryOk}，逐条"已删除"${perItem} 条=${perItemOk}）`;
      evidence.push(`批量删除结果: ${bulkNote}`);
    }
  }

  // ---------------------------------------------------------------- 判定
  const failed = results.filter((r) => !r.ok);
  // 库里真少了条数的那几档（"被拒绝"那几档不算）
  const deletedKinds = results.filter((r) => r.removed >= 1).length;
  const refusedKinds = results.filter((r) => r.refused === true).length;
  const usedDomFallback = domClickFallbacks - fallbacksBefore;
  // 档数下限随页面变化：`/channels` 删页后通道那 4 档搬走了（[docs/49]），
  // 现在剩的是"定价 / 模型 / 服务商 / 密钥入口不该存在"——**4 档**。
  // 下限写的是"至少要覆盖多类实体"，不是"必须等于某个历史数字"。
  const deleteOk = results.length >= 4 && failed.length === 0;
  if (laneControlGone && laneFactKept && deleteOk && bulkOk) {
    record(
      "step21",
      "PASS",
      `线路级代理只归服务商（线路行内代理控件=0、逐行启停齐全、服务商级入口仍在）+ ${results.length} 档删除：真删掉 ${deletedKinds} 档、` +
        `如实拒绝 ${refusedKinds} 档 + 批量删除逐条汇报` +
        `；DOM 兜底=${usedDomFallback} 次`,
      evidence,
    );
  } else {
    record(
      "step21",
      "FAIL",
      `没做到位：线路级代理控件已归服务商=${laneControlGone} 线路行可用=${laneFactKept}；${results.length} 档删除里失败 ${failed.length} 档` +
        `（真删掉 ${deletedKinds} 档、如实拒绝 ${refusedKinds} 档、DOM 兜底=${usedDomFallback} 次）` +
        `${failed.length > 0 ? "（" + failed.map((f) => `${f.label}:${f.note}`).join("；") + "）" : ""}；批量删除=${bulkOk}`,
      evidence,
    );
  }
}

/**
 * **step22：长篇介绍收进「问号圆圈 + 悬停提示」**（[docs/22]，用户第 74 轮原话：
 * 「里面的简介好长，好多字……那些比较介绍做成一个问好加一个圆圈，鼠标移上去能看到提示那种」）。
 *
 * 逐页断言：
 * - 页头有 `page-header-help`，它的 `aria-label` 就是那段全文（**正文一个字都没丢**）；
 * - 悬停/聚焦之后出现 `[role="tooltip"]`，文本与 `aria-label` **逐字相同**；
 * - 页头里**不再**内联那段正文（`header.textContent` 不含它）。
 *
 * MUI 的 `Tooltip` 悬停与聚焦都会展开，所以两条路都触发（聚焦也是键盘用户的路）。
 *
 * **稳定性**：这一档过去是 flaky 的（同一构建两次 run，红的是不同的页），真因是
 * MUI `Tooltip` 懒渲染 + 进入延时，而页头会在取数回来后重渲染 —— 上一轮 dispatch 到的
 * 可能是已被 React 换掉的旧节点，也可能卡在 enterDelay 之前就读了 DOM。
 * 所以每一页的"悬停/聚焦"最多重试 3 轮（每轮重新取节点、先清掉上一轮的展开态），
 * 但**判据没有放宽**：从"前 24 个字对得上"加严成"与 aria-label 逐字相同"。
 */
/**
 * **step25：线路的网络代理真的能切，而且能直接选订阅节点**（[docs/28 §1.2]，用户第 81 轮原话：
 * 「我在网络那边添加了订阅也加载出来了，但是在服务商那边依然无法对一个线路切换网络」）。
 *
 * 现场查清的事实：下拉过去只列**已配置的线路**（`doc.lanes`），而用户的配置里一条线路都没有
 * —— 所以只剩「不指定（继承）」，看起来像"下拉坏了"。这一版把**订阅节点**也列出来：
 * 选中一个节点时先 `lane_ensure_for_node` 建/复用一条线路，再把它写进**服务商**的 `lane`。
 * （[docs/55]：`channel.lane` 字段已删除 —— 线路分配只剩服务商级一处。）
 *
 * 断言（缺一条都不算做到）：
 * 1. 服务商详情页那条线路的代理下拉里，**除了"不指定"还有订阅里的节点**（名字来自 fixture）；
 * 2. 选中一个节点之后：库里**多了一条 `subscription_node` 线路**（订阅 id + 节点名都对上）；
 * 3. 写下去之后**服务商的 `lane`** 真的指向那条线路 id（不是只在界面上显示对了）；
 *    同时**线路级**代理控件一个都不许有（[docs/29 §2.1]，也是 [docs/55] 的收口）；
 * 4. 向导第 1 步有可选代理（`pw-lane`），第 2 步每条候选线路也有自己的
 *    （`pw-scan-lane-*`）—— 用户第 81 轮第二条要求「在向导添加的时候，我也希望可以选择代理网络」。
 *
 * 前置：跑在 step3/step17 之后（要有已解析的订阅与节点），且服务商下已经有线路（step2/step9）。
 */
async function step25(wd) {
  const evidence = [];
  const before = dumpScratchDb("step25-before");
  const providers = configEntities(before, "provider");
  const withCred = new Set(
    configEntities(before, "credential").map((c) => String(c.json?.provider ?? "")),
  );
  const target =
    providers.find(
      (p) =>
        withCred.has(p.entityId) &&
        configEntities(before, "channel").some((c) => String(c.json?.provider) === p.entityId),
    ) ?? null;
  if (target === null) {
    record("step25", "SKIP", "没有'既有凭据又有线路'的服务商（前面步骤没建出来）", evidence);
    return;
  }
  const providerId = target.entityId;
  const lines = configEntities(before, "channel").filter(
    (c) => String(c.json?.provider) === providerId,
  );
  const line = lines[0];
  const lineId = String(line.entityId);
  const lanesBefore = configIds(before, "lane");
  evidence.push(
    `目标：provider=${providerId} 线路=${lineId}；扫描前 lanes=${JSON.stringify(lanesBefore)}`,
  );

  const detail = await gotoPage(
    wd,
    `#/providers/${encodeURIComponent(providerId)}`,
    '[data-testid="providers-detail"], [data-testid="providers-detail-notfound"]',
  );
  if (detail === null) {
    record("step25", "FAIL", "服务商详情页没渲染出来", evidence);
    return;
  }

  // ---- ① 那一行线路的代理下拉：选项里必须有"订阅节点"（而不是只剩"不指定"）
  const rows = await wd.exec(
    `return [...document.querySelectorAll('[data-testid="providers-line-row"]')].map((r) => r.textContent || '');`,
  );
  evidence.push(`线路行数=${Array.isArray(rows) ? rows.length : 0}`);
  // [docs/29 §2.1]（用户第 88 轮）线路**不再有**自己的代理：出口只由**服务商**决定
  // （同一服务商下的模型打同一个 URL，用不同出口 IP 会被上游当成"同账号多地并发" → 封号）。
  //
  // 所以这一档验的是**反面**：界面上一个线路级代理入口都不许有，而服务商级入口必须在。
  const counts = await wd.exec(`
    return {
      lineLane: document.querySelectorAll('[data-testid="providers-line-lane"]').length,
      providerLane: document.querySelectorAll('[data-testid="providers-edit-lane"]').length,
      rows: [...document.querySelectorAll('[data-testid="providers-line-row"]')].map((r) => ({
        name: (r.querySelector('[data-testid="providers-line-upstream"]')?.textContent || '').trim(),
        hasLane: r.querySelector('[data-testid="providers-line-lane"]') !== null,
        hasSwitch: r.querySelector('[data-testid="providers-line-enabled"] input') !== null,
      })),
    };
  `);
  evidence.push(`线路级代理控件=${counts?.lineLane}（应为 0）服务商级代理入口=${counts?.providerLane}（应 > 0）`);
  evidence.push(`线路行=${JSON.stringify(counts?.rows ?? [])}`);
  // [docs/29 §2.2] 服务商级代理下拉里必须**有**「自动」（D 期入口）；值是标记 `auto:<订阅>`。
  // 线路级那一列则相反：一个都不许有（A 期）。
  // MUI 的选项渲染在 portal 里、**展开后**才存在 → 先点开再读（与其它档同样的做法）
  const openedAuto = await wdOpenSelect(wd, "providers-edit-lane", evidence);
  const autoOptions = (openedAuto.items ?? []).map((o) => String(o.text ?? o).trim());
  evidence.push(`服务商级代理下拉选项=${JSON.stringify(autoOptions)}`);
  const providerLaneHasAuto = autoOptions.some((t) => t.includes("自动"));
  await wd.closeMenuIfOpen();
  await sleep(300);
  const lineLaneGone = Number(counts?.lineLane ?? -1) === 0;
  const rowsClean =
    Array.isArray(counts?.rows) &&
    counts.rows.length > 0 &&
    counts.rows.every((r) => r.hasLane === false && r.hasSwitch === true);
  const providerLaneKept = Number(counts?.providerLane ?? 0) > 0;

  // 向导里同样不许有"新建线路默认代理"
  await gotoPage(wd, "#/providers");
  await wdClickTid(wd, "providers-add-open", evidence);
  await sleep(600);
  const wizardIds = await wd.probeTestIds();
  const step1HasPicker = wizardIds.includes("pw-default-lane");
  // [docs/29 §2.2]（用户第 112 轮）**"添加"向导里必须能为服务商选代理**，选项要含
  // 「直连」「自动」和所有已配置线路 —— 与详情页同一份 `providerLaneChoices`。
  const wizardLane = await wdOpenSelect(wd, "pw-lane", evidence);
  const wizardLaneOptions = (wizardLane.items ?? []).map((o) => String(o.text ?? o).trim());
  evidence.push(`向导第 1 步代理下拉（pw-lane）选项=${JSON.stringify(wizardLaneOptions)}`);
  await wd.closeMenuIfOpen();
  await sleep(300);
  evidence.push(`向导第 1 步有代理下拉（pw-default-lane）=${step1HasPicker}（应为 false）`);

  /**
   * [docs/29 §2.2] **D 期链路的真机验证**：在服务商详情页选「自动」并保存 →
   * 后端应当**建出一条** `subscription_auto` + 显式 `select` 的线路，并把服务商的 `lane`
   * 指过去（前端只发标记 `auto:<订阅>`，不建线路、不编 id）。
   *
   * 这一条**不需要内核**（真正"按 base_url 测延迟取最低"要内核在跑；内核没起时
   * `apply_auto_export` 会**如实拒绝**，不编一个结论）。
   */
  const autoLanesBefore = configEntities(dumpScratchDb("step25-lane-before"), "lane");
  const detail2 = await gotoPage(
    wd,
    `#/providers/${encodeURIComponent(providerId)}`,
    '[data-testid="providers-detail"]',
  );
  let autoLaneWired = true;
  if (detail2 !== null) {
    await wdPickOption(wd, "providers-edit-lane", evidence, /^自动/);
    await sleep(800);
    const saved = await wdClickTid(wd, "providers-detail-save", evidence);
    await sleep(2500);
    const afterAuto = dumpScratchDb("step25-auto-saved");
    const lanesAfter2 = configEntities(afterAuto, "lane");
    const autoLanes = lanesAfter2.filter(
      (l) =>
        String(l.json?.kind ?? "") === "subscription_auto" &&
        String(l.json?.group_type ?? "") === "select",
    );
    const autoLaneId = autoLanes[0] === undefined ? null : String(autoLanes[0].entityId);
    const providerRow = configEntities(afterAuto, "provider").find(
      (x) => String(x.entityId) === providerId,
    );
    autoLaneWired =
      providerRow !== undefined &&
      autoLaneId !== null &&
      String(providerRow.json?.lane ?? "") === autoLaneId;
    evidence.push(
      `选「自动」并保存：保存=${saved}；自动线路数=${autoLanes.length}（lanes ${lanesBefore.length}→${lanesAfter2.length}）；` +
        `服务商 lane=${JSON.stringify(providerRow?.json?.lane ?? null)} 期望=${JSON.stringify(autoLaneId)} → ${autoLaneWired}`,
    );
    // [docs/29 §2.2] **C 期触发点②的真机证据**：手动点订阅「刷新」= 重挑（manual = true）。
    // 顺带把"内核没起 → 如实拒绝"那条也逼出来（这一档的 scratch 里内核没在跑）。
    await gotoPage(wd, "#/network");
    await sleep(600);
    const clickedRefresh = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
    await sleep(4000);
    const refreshEcho = await wd.textOf('[data-testid="network-subscriptions-refresh-result"]');
    evidence.push(
      `手动刷新：点击=${clickedRefresh}；界面回执=${JSON.stringify((refreshEcho ?? "").replace(/\s+/g, " ").slice(0, 120))}` +
        `（「自动」出口重算结果看应用日志）`,
    );
  }
  await wd.closeDialog();
  await sleep(400);

  /**
   * [docs/58]（第 325 轮）用户报的 bug：「我已经添加了订阅和节点，但是在服务商那边选节点还是
   * 啥也没看到」。根因是两个下拉都读 `lane_list`（**只有已配置的线路**），而不是 `lane_options`
   * （含订阅节点）。所以这一档现在**必须**在真机上看到订阅里的节点 ——
   * 只看到「自动 / 直连 / 不指定」这三项就说明节点又没进去（那就是原来的 bug）。
   *
   * 注意判据换了：以前这里要求出现名为「TA E2E 线路」的**已配置线路**（那要靠 step4 手建），
   * 现在 step4 不再建线路 —— 要求的是**订阅节点**，而它正是这一轮要修的东西。
   */
  const wizardLaneOk =
    wizardLaneOptions.some((t) => t.includes("直连")) &&
    wizardLaneOptions.some((t) => t.includes("自动")) &&
    wizardLaneOptions.length > 3;
  const ok =
    lineLaneGone && rowsClean && providerLaneKept && !step1HasPicker && providerLaneHasAuto && wizardLaneOk && autoLaneWired;
  if (ok) {
    record(
      "step25",
      "PASS",
      "代理只属于服务商：线路行里没有任何代理控件（只有名字 + 滑块），向导也没有线路级代理入口，" +
        "服务商级的代理入口仍在",
      evidence,
    );
  } else {
    record(
      "step25",
      "FAIL",
      `线路级代理没删干净或有误伤：线路级控件没了=${lineLaneGone} 行结构干净=${rowsClean} ` +
        `服务商级入口还在=${providerLaneKept} 向导无代理入口=${!step1HasPicker}`,
      evidence,
    );
  }
}

async function step22(wd) {
  const evidence = [];
  const pages = [
    "#/models",
    // 这里原来还有 `#/routing`（模型路由）、`#/policies`（路由策略）与 `#/channels`
    // （导航标签「套餐与线路」）三页 —— 按用户裁决整页删除，见 [docs/48] / [docs/49]。
    // 所以它们从"每屏都要有真机截图"的清单里去掉。
    "#/cost",
    "#/network",
    "#/logs",
    "#/settings",
    "#/api-keys",
    "#/providers",
  ];
  let withHelp = 0;
  let okCount = 0;
  const bad = [];

  for (const hash of pages) {
  // [docs/31 §38/§39] 进每一页前先关掉残留的对话框：前面步骤（向导/详情编辑）可能留下模态没关，
  // 它的遮罩会盖住页头问号、焦点陷阱让 focus() 也落不到按钮上 —— #/providers 的 step22 就是这样假红的。
  // 合成 Escape 两次都没生效（§38），这里**直接点它的按钮**。
  await wd
    .exec(`
      const dlg = document.querySelector(".MuiDialog-container");
      if (!dlg) return { closed: true, why: "no-dialog" };
      const btns = Array.from(dlg.querySelectorAll("button"));
      const target = btns.find((b) => (b.textContent || "").trim() === "取消") ||
        btns.find((b) => (b.getAttribute("aria-label") || "").includes("关闭"));
      if (target) { target.click(); return { closed: true, why: "click" }; }
      dlg.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      return { closed: false, why: "escape-only", buttons: btns.length };
    `)
    .catch(() => {});
  await sleep(250);
    const page = await gotoPage(wd, hash, '[data-testid="page-header"]');
    if (page === null) {
      bad.push(`${hash}: 页头没渲染`);
      continue;
    }
    const probe = await wd.exec(`
      const btn = document.querySelector('[data-testid="page-header-help"]');
      const header = document.querySelector('[data-testid="page-header"]');
      return {
        hasHelp: btn !== null,
        label: btn ? (btn.getAttribute('aria-label') || '') : null,
        headerText: header ? (header.textContent || '').replace(/\\s+/g, ' ').trim() : null,
      };
    `);
    if (probe?.hasHelp !== true || !probe.label) {
      evidence.push(`${hash}: 没有 page-header-help（这页要么没有说明，要么说明还在版面上）`);
      continue;
    }
    withHelp += 1;
  const helpState = await wd     .exec(`       const btn = document.querySelector('[data-testid="page-header-help"]');       if (!btn) return null;       const r = btn.getBoundingClientRect();       const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);       return {         disabled: btn.disabled === true,         pointerEvents: getComputedStyle(btn).pointerEvents,         rect: [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)],         topIsButton: top === btn || (top ? btn.contains(top) : false),         topTag: top ? top.tagName : null,       };     `)     .catch(() => null);   evidence.push(`${hash}: 问号按钮态=${JSON.stringify(helpState)}`);
  const cover = await wd     .exec(`       const btn = document.querySelector('[data-testid="page-header-help"]');       if (!btn) return null;       const r = btn.getBoundingClientRect();       const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);       return top && top !== btn ? {         tag: top.tagName,         cls: String(top.className).slice(0, 120),         id: top.id || null,         text: (top.textContent || "").replace(/\\s+/g, " ").trim().slice(0, 40),         html: top.outerHTML.slice(0, 140),       } : null;     `)     .catch(() => null);   if (cover) evidence.push(`${hash}: 遮挡元素=${JSON.stringify(cover)}`);
      // [docs/30 §5] 每屏真机截图（跑通后顺便留下每页画面，当证据归档）
      await shot(wd, `page-${hash.replace(/[#/]/g, "-")}`).catch(() => {});
    // ★ 悬停/聚焦 → 读 tooltip 这一对动作**最多重试 3 轮**。
    //
    // 为什么要重试（这一档原来是 flaky：同一构建跑两次，红的可能是 providers、也可能是 policies，
    // 而 aria-label 与"页头不再内联正文"两条每轮都成立）：
    // MUI 的 `Tooltip` 是**懒渲染 + 有进入延时**的，而页头在列表/看板取数回来后会重渲染 ——
    // 上一轮 dispatch 到的可能是**已经被 React 换掉的旧节点**（事件照发，React 收不到），
    // 也可能刚好卡在 enterDelay 之前就读了 DOM。两者都是**交互时序**，不是"问号圆圈没做"。
    //
    // 关键：放宽的只是**交互的重试次数**，判据本身**加严**了 ——
    // 从"tooltip 文本前 24 个字能对上"改成"tooltip 正文与 aria-label **逐字相同**"
    // （`HelpTip` 里 `title={text}` 与 `aria-label={text}` 就是同一个字符串，
    // 所以"正文一个字都没丢"这句话可以被真的验到）。3 轮都不成立才算红。
    let tip = null;
    let tipOk = false;
    let tipTries = 0;
    const MAX_TIP_TRIES = 5;   // [docs/31 §31] 3 → 5：providers 那页偶尔第 3 轮还读不到（懒渲染 + 列表取数后重渲染），多给两轮，判据不变
    /**
     * 三轮**从最保守到最激进**（第 1 轮绝不先"清状态"：清状态本身也可能把 MUI 的
     * hover 迟滞（hysteresis）打开，反而让这一次永远展不开 —— 那会把挂具的动作
     * 变成新的干扰源）：
     *   ① 只把焦点给按钮：MUI `Tooltip` 的 `onFocus` 就会展开，这正是**键盘用户**走的路；
     *   ② 再加一次鼠标进入（鼠标用户的路）；
     *   ③ 最后才"先收掉可能残留的展开态，再重来"。
     * 每轮都**重新取一次节点**：页头会在取数回来后重渲染，事件发给已经被换掉的旧节点是收不到的。
     */
    const tipStrategies = [
      `btn.focus();`,
      // [docs/31 §32] MUI v7 的 `Tooltip` 走 **pointer 事件**；只发 mouse 事件在个别页面开不了。       `btn.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerType: "mouse" })); btn.dispatchEvent(new PointerEvent("pointerenter", { bubbles: false, pointerType: "mouse" })); btn.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })); btn.focus();`,
      `btn.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
          btn.dispatchEvent(new MouseEvent('mouseenter', { bubbles: false }));
          btn.focus();`,
      `btn.dispatchEvent(new MouseEvent('mouseout', { bubbles: true }));
          btn.dispatchEvent(new MouseEvent('mouseleave', { bubbles: false }));
          if (document.activeElement === btn && btn.blur) btn.blur();
          btn.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
          btn.dispatchEvent(new MouseEvent('mouseenter', { bubbles: false }));
          btn.focus();`,
    ];
    for (tipTries = 1; tipTries <= MAX_TIP_TRIES; tipTries += 1) {
      const fired = await wd
        .exec(`
          const btn = document.querySelector('[data-testid="page-header-help"]');
          if (!btn) return { fired: false, reason: 'no-button' };
          ${tipStrategies[(tipTries - 1) % tipStrategies.length]}
          return { fired: true, tag: btn.tagName, focused: document.activeElement === btn };
        `)
        .catch(() => null);
      if (fired?.fired !== true) {
        await sleep(250);
        continue;
      }
      await sleep(450);
      tip = await wd.exec(`
        const el = document.querySelector('[role="tooltip"]');
        return { has: el !== null, text: el ? (el.textContent || '').replace(/\\s+/g, ' ').trim() : null };
      `);
      const labelTry = String(probe.label).replace(/\s+/g, " ").trim();
      tipOk = tip?.has === true && tip.text === labelTry;
      if (tipOk) break;
      await sleep(250);
    }
    const label = String(probe.label);
    const tipText = tip?.text ?? null;
    const tipPrefixOk = typeof tipText === "string" && tipText.includes(label.slice(0, 24));
    const notInline = !String(probe.headerText ?? "").includes(label);
    // 还是没展开 → 把现场抓下来（截图 + DOM 事实）：这一档以前只留一句 "tooltip=null"，
    // 于是"为什么没展开"只能靠猜。三种可能必须能当场分开：
    //   ① DOM 里压根没有 popper（MUI 没收到事件）；② 有 popper 但 role 不是 tooltip；
    //   ③ 按钮根本没进 hover/focus 态（被遮挡、被重渲染换掉）。
    let diag = null;
    if (!tipOk) {
      const shotName = `step22-${hash.replace(/[^A-Za-z0-9]+/g, "-")}-no-tooltip`;
      await shot(wd, shotName);
      diag = await wd
        .exec(`
          const btn = document.querySelector('[data-testid="page-header-help"]');
          const poppers = [...document.querySelectorAll('.MuiTooltip-popper, .MuiPopper-root')];
          return {
            btn: btn === null ? null : {
              tag: btn.tagName,
              disabled: btn.disabled === true,
              ariaDescribedBy: btn.getAttribute('aria-describedby'),
              box: (() => { const r = btn.getBoundingClientRect(); return { w: Math.round(r.width), h: Math.round(r.height), top: Math.round(r.top) }; })(),
              activeElement: document.activeElement === btn,
              topElementIsIt: (() => { const r = btn.getBoundingClientRect(); const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return top === btn || btn.contains(top); })(),
            },
            poppers: poppers.map((p) => ({ role: p.getAttribute('role'), testid: p.getAttribute('data-testid'), text: (p.textContent || '').slice(0, 40) })),
            openTooltip: document.querySelectorAll('[role="tooltip"]').length,
          };
        `)
        .catch(() => null);
    }
    evidence.push(
      `${hash}: aria-label=${JSON.stringify(label.slice(0, 40))}；第 ${tipTries > MAX_TIP_TRIES ? MAX_TIP_TRIES : tipTries} 轮悬停/聚焦后 tooltip=${JSON.stringify(tipPrefixOk ? tipText : null)?.slice(0, 60)}` +
        `（逐字相同=${tipOk}、前 24 字命中=${tipPrefixOk}、tooltip 全文长度=${tipText === null ? null : tipText.length}/aria-label 长度=${label.replace(/\s+/g, " ").trim().length}）` +
        `；页头内联该正文=${JSON.stringify(!notInline)}；判定 tooltip=${tipOk} 不内联=${notInline}` +
        (diag === null ? "" : `；未展开现场=${JSON.stringify(diag)}`),
    );
    if (tipOk && notInline) okCount += 1;
    else bad.push(`${hash}(tooltip=${tipOk} 试了 ${Math.min(tipTries, MAX_TIP_TRIES)} 轮 前24字命中=${tipPrefixOk} 不内联=${notInline})`);
    // [docs/31 §38] 失败时才问"那一刻到底命中了谁"（探针必须与诊断同时刻，见 §37）     const cover = await wd       .exec(`         const btn = document.querySelector('[data-testid="page-header-help"]');         if (!btn) return { noButton: true };         const r = btn.getBoundingClientRect();         const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);         return {           activeElement: document.activeElement === btn,           describedBy: btn.getAttribute('aria-describedby'),           rect: [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)],           topTag: top ? top.tagName : null,           topIsButton: top === btn || (top ? btn.contains(top) : false),           topCls: top ? String(top.className).slice(0, 90) : null,           topText: top ? (top.textContent || "").replace(/\\s+/g, " ").trim().slice(0, 30) : null,         };       `)       .catch(() => null);     bad.push(`   ↳ 现场：${JSON.stringify(cover)}`);
    await wd.exec(`if (document.activeElement && document.activeElement.blur) document.activeElement.blur(); return null;`);
  }

  const ok = withHelp >= 3 && bad.length === 0;
  if (ok) {
    record(
      "step22",
      "PASS",
      `长篇介绍收进问号圆圈：${withHelp} 个页面的 page-header-help 悬停/聚焦都展开原文，且版面上不再有那段正文`,
      evidence,
    );
  } else {
    record(
      "step22",
      "FAIL",
      `没做到位：有问号圆圈的页面 ${withHelp} 个（要求 ≥3），不合格：${bad.join("；") || "（无）"}`,
      evidence,
    );
  }
}

/**
 * **step12：网络代理默认开启（默认直连），不再有"启用"这个动作** —— [docs/19 §2.2]。
 *
 * 上一轮的做法（未配置时**什么都不渲染**、只给一个 `network-optional` 选装入口）被用户明确推翻：
 * 「网络代理默认开启，反正默认直连，也不用"启动"了」。所以本步骤的断言与上一版**正好相反**：
 * - **必须**常显：`network-subscriptions` / `network-lanes` / 内核路径都要在（没有配置时给诚实空态）；
 * - **不许**再存在 `network-optional` / `network-optional-enable`（那是被推翻的旧语义）；
 * - 未配内核路径时顶部状态是「出口 · 直连」——不是"未启用"，也不是错误态。
 *
 * 用例前提：这一步必须在**还没配置任何网络**时跑（主流程里排在 step2/step3 之前）。
 */
async function step12(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/network");
  if (page === null) {
    record("step12", "FAIL", "/network 页面没渲染出来", evidence);
    return;
  }
  const ids = await wd.probeTestIds();
  evidence.push(`/network 命中 data-testid: ${ids.slice(0, 34).join(", ")}`);
  const dump = dumpScratchDb("step12");
  const kinds = configEntityKinds(dump);
  evidence.push(`config kind 计数: ${JSON.stringify(kinds)}（空库 = 真·未配置）`);

  // 旧语义的两个 testid 必须消失
  const legacy = ["network-optional", "network-optional-enable"].filter((t) => ids.includes(t));
  evidence.push(`被推翻的旧选装 testid（必须为空）: ${legacy.join(", ") || "（无）"}`);

  // 常显：三块都要在（每一块自己给诚实空态）
  const mustShow = ["network-subscriptions", "network-lanes"].filter((t) => !ids.includes(t));
  evidence.push(`必须常显却缺失的区块: ${mustShow.join(", ") || "（无）"}`);

  // 顶部出口状态：未配内核路径 → 直连
  const text = await wd.pageText();
  const showsDirect = /直连|Direct/.test(text);
  const showsDisabled = /未启用|not enabled/i.test(text);
  evidence.push(`页面上出现「直连」= ${showsDirect}；出现「未启用」= ${showsDisabled}`);
  evidence.push(`页面文本前 200 字: ${JSON.stringify(text.replace(/\s+/g, " ").slice(0, 200))}`);

  if (legacy.length > 0) {
    record("step12", "FAIL", `旧选装语义仍在页面上：${legacy.join(", ")}（用户已推翻：网络代理默认开启）`, evidence);
  } else if (mustShow.length > 0) {
    record("step12", "FAIL", `网络区块没有常显：缺 ${mustShow.join(", ")}`, evidence);
  } else if (!showsDirect || showsDisabled) {
    record("step12", "FAIL", `未配内核路径时应当是「出口 · 直连」，实际 showsDirect=${showsDirect} showsDisabled=${showsDisabled}`, evidence);
  } else {
    record("step12", "PASS", "网络代理默认开启：区块常显（空配置给诚实空态）、出口显示「直连」、旧选装入口已消失", evidence);
  }
}

async function step13(wd) {
  const evidence = [];
  const page = await gotoPage(wd, "#/network");
  if (page === null) {
    record("step13", "FAIL", "/network 页面没渲染出来", evidence);
    return;
  }
  // 流量信息是**刷新时**从响应头拿到的，所以先点一次行上的「刷新」。
  const refreshed = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
  if (refreshed) {
    await sleep(2500);
    evidence.push("已点过 network-subscriptions-refresh（流量信息来自刷新时的 subscription-userinfo）");
  } else {
    evidence.push("没有 network-subscriptions-refresh 可点（那就不可能有流量信息）");
  }
  const text = await wd.pageText();
  const lines = text.split("\n").filter((l) => /已用|GB|%|更新时间/.test(l)).slice(0, 8);
  evidence.push(`与流量相关的文案: ${JSON.stringify(lines)}`);

  // [极简 第 336 轮] **流量已经不在列表行里了** —— 列表只留 6 列，流量搬到了订阅详情页
  // （`network-subscription-traffic`）。所以这一步必须跟着数据走，否则它会退化成 SKIP，
  // 而 SKIP 的理由（"后端还没解析 subscription-userinfo"）**是假的**：
  // 数据一直都在，只是换了地方。那等于静默丢掉"流量真的解析出来了"这条端到端验证。
  const opened = await wdClickTid(wd, "network-subscription-open", evidence);
  if (!opened) {
    record("step13", "FAIL", "订阅名不是进详情页的链接（流量现在只在详情页可见）", evidence);
    return;
  }
  await sleep(1200);
  const trafficText = await wd
    .textOf('[data-testid="network-subscription-traffic"]')
    .catch(() => null);
  const detailText = (trafficText ?? "") + "\n" + (await wd.pageText());
  evidence.push(`详情页流量格: ${JSON.stringify((trafficText ?? "").slice(0, 200))}`);

  if (!/已用|Used/.test(detailText)) {
    record(
      "step13",
      "SKIP",
      "详情页也没有流量信息（后端真的没解析出 subscription-userinfo —— 这不是界面问题）",
      evidence,
    );
    return;
  }
  // fixture 给的是 upload 10GB + download 82.9GB = 92.9GB，total 130GB → 71%
  const hasUsed = /92\.9/.test(detailText);
  const hasTotal = /130/.test(detailText);
  const hasPercent = /71%/.test(detailText);
  // 精确更新时间也在详情页（列表上只给"2 分钟前"那种短格式）
  const hasUpdated = /更新时间|Updated/.test(detailText);
  evidence.push(`92.9GB=${hasUsed} 130GB=${hasTotal} 71%=${hasPercent} 更新时间=${hasUpdated}`);
  if (hasUsed && hasTotal && hasPercent && hasUpdated) {
    record(
      "step13",
      "PASS",
      "详情页显示精确更新时间 + 已用/总量 + 百分比，且与 fixture 的字节数对得上",
      evidence,
    );
  } else {
    record("step13", "FAIL", "流量显示不完整或数字对不上（见证据）", evidence);
  }
}

// ================================================================ 主流程

function parseArgs(argv) {
  const args = { keepData: false, only: undefined };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--exe") args.exe = argv[++i];
    else if (a === "--keep-data") args.keepData = true;
    else if (a === "--only") args.only = new Set(argv[++i].split(",").map((s) => s.trim()));
    else if (a === "--out") args.out = argv[++i];
    else if (a === "--force-unsafe") args.forceUnsafe = true;
    else if (a === "--selfcheck") args.selfcheck = true;
    else if (a === "--help" || a === "-h") args.help = true;
  }
  return args;
}

function printHelp() {
  process.stdout.write(
    [
      "node scripts/e2e/smoke.mjs [options]",
      "",
      "  --exe <path>     被测 exe（默认 target/release/tokenagency.exe）",
      "  --only a,b       只跑指定步骤（step0..step6）",
      "  --keep-data      保留上次的 scratch 数据目录",
      "  --out <dir>      运行产物根（默认 %TEMP%\\ta-e2e）",
      "  --selfcheck      只跑挂具自己的纯逻辑自检（不用浏览器/exe，秒级）",
      "",
    ].join("\n"),
  );
}

/**
 * [docs/46 §297] **把服务商的 `base_url` 指向一个真的可达的探测目标**。
 *
 * 为什么必须改：应用给 provider 体检时带上的 `url` 就是**这家服务商的 `base_url`**
 * （延迟的语义是"这条节点到该服务商有多快"）。夹具里服务商的 `base_url` 是故意配成
 * `http://127.0.0.1:1/v1`（不可达）⇒ 体检会把**每一个**节点的延迟都记成失败
 * ⇒ 应用只能如实报 `all_unreachable` ⇒ "选最低延迟"这条断言**永远**测不出来。
 * 红的是夹具，不是产品 —— 所以这里把探测目标改成真能到达的那条。
 *
 * ⚠️ 直接改 SQLite 只对**冷启动**有效（应用启动时读库）；运行中要改必须走界面
 * （step28 用的就是 `providers-edit-baseUrl` + 保存）。
 */
async function patchProviderProbeTarget(probeUrl, evidence, autoLaneId = null) {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(join(OUT, "data", "db.sqlite"));
  const rows = db.prepare("select entity_id, json from config where kind='provider'").all();
  const touched = [];
  let laneOwner = null;
  for (const r of rows) {
    const j = JSON.parse(r.json);
    const needUrl = j.base_url !== probeUrl;
    // [docs/46 §300] **「自动」线路只准挂在一家服务商上**：多家共用同一条 lane 时，每家都会按
    // **自己存的那条地址**（`auto_choice` 是按 provider_id 存的）独立重算并写同一个组 ⇒
    // 组上的 `now` 是"最后写的那家"的选择，日志里还会出现互相打脸的判决行
    // （实测：一家写"沿用 日本-优化2"、另一家写"剩余流量"）⇒ 粘性判据没法读。
    // 生产里这是**用户可配出来的**情形（值得产品侧给提示）；挂具这边先只挂一家，把判据拆干净。
    let needLane = false;
    if (autoLaneId !== null) {
      if (laneOwner === null) {
        laneOwner = r.entity_id;
        needLane = j.lane !== autoLaneId;
        if (needLane) j.lane = autoLaneId;
      } else if (j.lane === autoLaneId) {
        j.lane = "direct";
        needLane = true;
      }
    }
    if (!needUrl && !needLane) continue;
    if (needUrl) j.base_url = probeUrl;
    db.prepare("update config set json=? where kind='provider' and entity_id=?").run(
      JSON.stringify(j),
      r.entity_id,
    );
    touched.push(r.entity_id);
  }
  db.close();
  evidence.push(
    `已把服务商 base_url 指向探测目标（应用就用它体检）：${touched.join(", ") || "(无需改动)"} → ${probeUrl}` +
      (autoLaneId === null ? "" : `；「自动」线路只挂在 ${laneOwner} 上（其余从这条 lane 上摘掉）`),
  );
  return touched.length;
}

/**
 * [docs/46 §297] 读 provider 那张延迟表：
 * `GET /providers/proxies/<key>` → `[[节点名, 末次延迟, alive], …]`。
 *
 * 三件必须做对的事（前两条都踩过）：
 *   1. **只认这条路由** —— proxy-provider 的节点不在 `/proxies` 顶层，`/proxies/<name>/delay` 对它们全 404；
 *   2. `proxies` 可能是**数组**（实测如此），上一版用 `Object.entries` 读，节点名变成了 "0".."13" ✗；
 *   3. **`alive` 才算数**（与后端 `parse_provider_delays` 同判据）：内核**测失败时也会写一个非 0 的
 *      耗时**（实测见过 3006ms / 3620ms），只按 `delay > 0` 会把"探测失败"当成"通了、只是慢" ✗。
 */
async function readProviderNodes(controller, secret, key = "ta-e2e-sub-form") {
  const head = secret ? { authorization: `Bearer ${secret}` } : {};
  for (let i = 0; i < 3; i += 1) {
    try {
      const snap = await (
        await fetch(`http://${controller}/providers/proxies/${encodeURIComponent(key)}`, { headers: head })
      ).json();
      const arr = Array.isArray(snap?.proxies) ? snap.proxies : Object.values(snap?.proxies ?? {});
      return arr
        .map((v) => [
          v?.name ?? "?",
          v?.history?.length ? v.history[v.history.length - 1].delay : null,
          v?.alive === true,
        ])
        .filter(([n]) => typeof n === "string" && n !== "?");
    } catch {
      await sleep(1200);
    }
  }
  return [];
}

/** 「内核认为它真的测得出来」的那一批：`alive === true` **且** 末次延迟 > 0。 */
function measuredNodes(rows) {
  return rows.filter(([, d, alive]) => alive === true && typeof d === "number" && d > 0);
}

/**
 * 读**应用自己记的**「自动」出口地址（`store_setting.auto_export:<provider>`）。
 *
 * 为什么要读它（[docs/50 §5.4]）：应用的重挑判据是**地址级**的 ——
 * `choose_auto_export` 做的是 `find(|c| c.address == previous)`，而 `previous` 就是这张表里
 * 那个值（实测真机是 `m.cnmjin.net` 这样的 **server 主机名**，不含 port）。
 * 挂具读 mihomo 的 `/providers/proxies` 却只能拿到**名字**。
 * 不把应用那一侧的真实判据读出来，就没法判断"应用到底在说谁不通"。
 *
 * 读不到就返回 `null`（调用方照实说"读不到"，不编）。
 *
 * 不给参数：真机夹具里**只有一个**服务商，而 `store_setting` 的键形状是
 * `auto_export:<provider_id>`；把唯一的那个键读出来比硬编码 id 更耐改
 * （真要有多条，这里会把 `null` 说出来，而不是随便挑一条）。
 */
async function storedAutoAddress() {
  try {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(join(OUT, "data", "db.sqlite"));
    const rows = db.prepare("select key, value from store_setting where key like 'auto_export:%'").all();
    db.close();
    if (rows.length !== 1) return null;
    return String(rows[0].value);
  } catch {
    return null;
  }
}

/**
 * 从 scratch 库里找「自动」线路的 id（与后端 `ConfigDoc::auto_lane_on` 同判据：
 * `kind = subscription_auto` **且** `group_type = select` **且** 订阅对得上）。
 */
async function findAutoLaneId(subscriptionId, evidence) {
  try {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(join(OUT, "data", "db.sqlite"));
    const rows = db.prepare("select entity_id, json from config where kind='lane'").all();
    db.close();
    const hit = rows.find((r) => {
      const j = JSON.parse(r.json);
      return (
        String(j.kind ?? "") === "subscription_auto" &&
        String(j.group_type ?? "") === "select" &&
        String(j.subscription ?? "") === subscriptionId
      );
    });
    evidence.push(`「自动」线路（按后端判据从库里找）=${hit?.entity_id ?? "(没有)"}`);
    return hit?.entity_id ?? null;
  } catch (err) {
    evidence.push(`找「自动」线路失败：${String(err).slice(0, 80)}`);
    return null;
  }
}

/**
 * [docs/46 E 期] **只读探测**：本地延迟测试台的订阅能不能取到、两条节点在不在。
 *
 * 为什么先要这一步：后面"选最低延迟 / 粘性"的断言都依赖这份订阅；
 * 它取不到时，红的是"测试台没起来"，而不是"产品选错了"——两者必须能分开。
 *
 * 注意：**只读**，不建订阅、不动界面（避免影响既有步骤）。
 */
async function step26(_wd, fixture) {
  const evidence = [];
  const latUrl = fixture.url.replace("/clash.yaml", "/clash-latency.yaml");
  try {
    const res = await fetch(latUrl);
    const text = await res.text();
    const hasFast = text.includes("ta-lat-fast");
    const hasSlow = text.includes("ta-lat-slow");
    const lines = text.split("\\n").length;
    evidence.push(`延迟订阅 ${latUrl} → HTTP ${res.status}，${lines} 行，含 ta-lat-fast=${hasFast}、ta-lat-slow=${hasSlow}`);
    const ok = res.status === 200 && hasFast && hasSlow;
    record("step26", ok ? "PASS" : "FAIL", ok ? "E 期测试台订阅可用（两个 CONNECT 代理已就绪）" : "延迟订阅不可用", evidence);
  } catch (err) {
    evidence.push(`取延迟订阅失败：${String(err)}`);
    record("step26", "FAIL", "延迟订阅取不到（测试台没起来？）", evidence);
  }
}

/**
 * [docs/46 E 期] **读内核状态**：把「自动」出口当前选中了谁、各节点实测延迟打出来。
 *
 * 为什么单独一步：E 期"选最低延迟 / 粘性"的断言都要读内核；先把"读得到"这件事本身验通，
 * 后面红了才能分清"读不到"与"选错了"。
 *
 * 全程只读：配置从挂具自己的 scratch 目录里那份**生成的内核配置**里取（控制器地址 + secret），
 * 不碰产品、不碰界面。任何异常都只让这一档红，不影响别的步骤。
 */
async function step27(_wd, fixture) {
  const evidence = [];
  try {
    const kernelDir = join(fixture.dataDir ?? OUT, "data", "kernel");
    const cfgText = readFileSync(join(kernelDir, "kernel.generated.yaml"), "utf8");
    const controller = /external-controller:\s*([^\s]+)/.exec(cfgText)?.[1] ?? null;
    const secret = /secret:\s*"([^"]*)"/.exec(cfgText)?.[1] ?? "";
    evidence.push(`内核控制器=${controller}（从 ${kernelDir} 的生成配置里读到）`);
    // [docs/46] **P1/P2 分辨器**：生成配置里到底有没有我们自己的 provider / group。
    const providers = /proxy-providers:([\s\S]*?)\n[a-z-]+:/.exec(cfgText)?.[1] ?? "(没有这一段)";
    const groups = /proxy-groups:([\s\S]*?)\nrules:/.exec(cfgText)?.[1] ?? "(没有这一段)";
    evidence.push(`生成配置 proxy-providers=${providers.trim().slice(0, 140) || "(空)"}`);
    evidence.push(`生成配置 proxy-groups=${groups.trim().slice(0, 140) || "(空)"}`);
    if (controller === null) {
      record("step27", "FAIL", "生成的内核配置里没有 external-controller", evidence);
      return;
    }
    const get = async (path) => {
      const res = await fetch(`http://${controller}/proxies${path}`, {
        headers: secret ? { authorization: `Bearer ${secret}` } : {},
      }).catch(() => null);
      // [docs/46 §291] `fetch` 自身 reject 时**必须返回一个"读不到"**，而不是把整轮打崩
      // （上一次就是这样：`未捕获异常：fetch failed` ⇒ 整轮 FAIL，判定根本没执行 ✗）。
      if (res === null) return { status: "fetch failed", body: null };
      return { status: res.status, body: await res.json().catch(() => null) };
    };
    const all = await get("");
    const names = Object.keys(all.body?.proxies ?? {});
    evidence.push(`/proxies → HTTP ${all.status}，共 ${names.length} 项（前 8：${names.slice(0, 8).join(", ")}）`);
    for (const name of ["ta-e2e-group", "ta-lat-group"]) {
      if (!names.includes(name)) continue;
      const one = await get(`/${encodeURIComponent(name)}`);
      const now = one.body?.now ?? null;
      const opts = Array.isArray(one.body?.all) ? one.body.all : [];
      evidence.push(`组 ${name}：now=${now ?? "(未选)"}，候选=${opts.slice(0, 6).join(", ")}`);
      for (const node of opts.slice(0, 4)) {
        const n = await get(`/${encodeURIComponent(node)}`);
        const hist = Array.isArray(n.body?.history) ? n.body.history : [];
        const delay = hist.length > 0 ? hist[hist.length - 1].delay : null;
        evidence.push(`  节点 ${node}：最新延迟=${delay === null ? "(没量到)" : delay + " ms"}`);
      }
    }
    record("step27", "PASS", "内核状态可读（「自动」出口的选中与延迟都拿到了）", evidence);
  } catch (err) {
    evidence.push(`读内核状态失败：${String(err)}`);
    record("step27", "FAIL", "读不到内核状态（内核没起来 / 配置里没有控制器）", evidence);
  }
}

/**
 * [docs/46 E 期] **触发点②（用户手动刷新）→ 看「自动」出口选的是谁**。
 *
 * 两条分支都要如实记录（不许制造假绿）：
 *  · `now === "ta-lat-fast"` ⇒ **按最低延迟选**（`CLASH_YAML` 里慢的排在前面 ⇒ 不是"选第一个"）；
 *  · 应用日志里该 provider 是 `all_unreachable` ⇒ **产品如实拒绝**（本机无公网、探测目标不可达时就是这样）
 *    —— 这时通过的是"如实性"，**不是**"选最低延迟"，证据里必须写清是哪一种。
 */
async function step28(wd, fixture) {
  const evidence = [];
  await gotoPage(wd, "#/network");
  await sleep(600);

  // [docs/46 §E] **先把这条订阅的探测目标改成本机可达**（否则本机无公网 ⇒ 应用只能如实报 all_unreachable，
  // "选最低延迟"永远测不出来）。同 id = 覆盖，所以不需要另找"编辑"入口。
  // [docs/46 §297] 探测目标用**公网端点**（本机实测可达 gstatic 204 ✓）。为什么不再用挂具的本地
  // 健康端口：夹具节点是"转发型" HTTP 代理，而 mihomo 对 `http://` 目标是走**绝对 URI 明文转发**的
  // —— 这条路径在真机上一直给不出延迟（14 条全 0，把内核杀掉重来也是 0）。用**同一份** gstatic
  // 目标则与 step29 走的是同一条已被证实的路径（34 条真实节点里量到 26 条 ✓）。
  // 夹具的意义在于"快 0ms / 慢 400ms"这个**巨大且确定**的差距，任何抖动都翻不过来盘。
  const healthUrl = PROBE_GLOBAL;
  const healthUrlLabel = `${healthUrl}（公网、本机可达；夹具两条代理差 400ms ⇒ 好判定）`;
  await wdFillByTid(wd, "network-subscriptions-add-id", "ta-e2e-sub-form", evidence);
  await wdFillByTid(wd, "network-subscriptions-add-name", "e2e", evidence);
  await wdFillByTid(wd, "network-subscriptions-add-url", fixture.url, evidence);
  await wdFillByTid(wd, "network-subscriptions-add-health-url", healthUrl, evidence);
  const submitted = await wdClickTid(wd, "network-subscriptions-add-submit", evidence);
  await sleep(2500);
  evidence.push(`订阅已覆盖保存：提交=${submitted}；健康检查地址=${healthUrlLabel}`);

  // [docs/46 §297] **把服务商的 `base_url` 改成同一个本机可达地址** —— 应用体检时带的就是它。
  // 运行中改配置只能走界面（改库只对冷启动有效）：`providers-edit-baseUrl` + 保存是既有的真控件。
  // 顺便把「自动」线路的 `group_type` 读出来当证据（必须是 select，否则 set_select 会被内核忽略）。
  let setupOk = false;
  try {
    const cfgDump = dumpScratchDb("step28-prov");
    const autoLane = configEntities(cfgDump, "lane").find(
      (l) =>
        String(l.json?.kind ?? "") === "subscription_auto" &&
        String(l.json?.subscription ?? "") === "ta-e2e-sub-form" &&
        String(l.json?.group_type ?? "") === "select",
    );
    evidence.push(
      `「自动」线路：id=${autoLane?.entityId ?? "(库里找不到：kind=subscription_auto + group_type=select)"} group_type=${JSON.stringify(autoLane?.json?.group_type ?? null)}`,
    );
    const owner = configEntities(cfgDump, "provider").find(
      (p) => String(p.json?.lane ?? "") === String(autoLane?.entityId ?? "\u0000"),
    );
    evidence.push(
      `挂「自动」线路的服务商=${owner?.entityId ?? "(没有)"}（原 base_url=${JSON.stringify(owner?.json?.base_url ?? null)}）`,
    );
    if (owner !== undefined) {
      await gotoPage(wd, `#/providers/${encodeURIComponent(owner.entityId)}`, '[data-testid="providers-detail"]');
      const filled = await wdFillByTid(wd, "providers-edit-baseUrl", healthUrl, evidence);
      const saved = await wdClickTid(wd, "providers-detail-save", evidence);
      await sleep(1500);
      const savedText = await wd.textOf('[data-testid="providers-detail-save-result"]');
      const saveError = await wd.textOf('[data-testid="providers-detail-save-error"]');
      const after = configEntities(dumpScratchDb("step28-prov-after"), "provider").find(
        (p) => p.entityId === owner.entityId,
      );
      evidence.push(
        `服务商 base_url 改成探测目标：填入=${filled} 保存=${saved} 对话框=${JSON.stringify(String(savedText ?? "").slice(0, 60))} 错误=${JSON.stringify(String(saveError ?? "").slice(0, 60))} 库里现在=${JSON.stringify(after?.json?.base_url ?? null)}`,
      );
      setupOk = true;
    }
  } catch (err) {
    evidence.push(`改服务商 base_url 失败（不影响本档其余证据）：${String(err).slice(0, 120)}`);
  }
  // [docs/46 §300] **前置不足就如实 SKIP，不要当成产品的红**：本档需要"某家服务商挂着这条
  // 「自动」线路"（那一步由 `step25` 经界面做出来）。只跑 `net` 档、或前面步骤没把它建出来时，
  // 这里的失败是**挂具自己的前置没搭起来**，不是"应用没重算"。
  if (!setupOk) {
    record(
      "step28",
      "SKIP",
      "前置不足：找不到「挂着这条自动线路」的服务商（`step25` 那一步没跑到/没建出来）⇒ 本档无法把探测目标指过去",
      evidence,
    );
    return;
  }
  await gotoPage(wd, "#/network");
  await sleep(400);

  // [docs/46 §E] **自检：自己走一遍 mihomo 要走的路** —— 经挂具的 CONNECT 代理连到本地健康端点。
  // 通了 ⇒ 代理实现没问题，症结在 mihomo 侧（类型/超时/时机）；不通 ⇒ 症结在挂具这一侧。
  // 这一条把"猜"变成"读"：不再靠推理判断 8390/8391 上到底发生了什么。
  const probeLine = await new Promise((resolve) => {
    const hp = rig.healthPort();
    const sock = netConnect(8391, "127.0.0.1", () => {
      sock.write(`CONNECT 127.0.0.1:${hp} HTTP/1.1\r\nHost: 127.0.0.1:${hp}\r\n\r\n`);
    });
    let buf = "";
    sock.on("data", (chunk) => {
      buf += String(chunk);
      if (buf.includes("\r\n\r\n")) {
        sock.destroy();
        resolve(`【慢代理 8391】${buf.split("\r\n")[0]}`);
      }
    });
    sock.on("error", (e) => resolve(`【慢代理 8391】连接失败 ${e.code}`));
    setTimeout(() => {
      sock.destroy();
      resolve(buf === "" ? "【慢代理 8391】超时（无任何返回）" : `【慢代理 8391】${buf.split("\r\n")[0]}`);
    }, 5000);
  });
  evidence.push(`自检（走一遍 mihomo 的路）：${probeLine}`);

  // [docs/46 §E/§297] **保存之后让 provider 重新加载一次**（mihomo 的探测设置是 provider 加载时建立的），
  // 然后**只读**等它把延迟填上 —— **不再杀内核**：上一次为了逼 mihomo 重读配置而 `taskkill mihomo.exe`，
  // 结果是"provider 共 0 条"，应用那一轮体检必然 `measured=0` ⇒ 判红的是我自己的动作。
  const earlyCfg = readFileSync(join(OUT, "data", "kernel", "kernel.generated.yaml"), "utf8");
  const earlyController = /external-controller:\s*([^\s]+)/.exec(earlyCfg)?.[1] ?? null;
  const earlySecret = /secret:\s*"([^"]*)"/.exec(earlyCfg)?.[1] ?? "";
  // provider 快照可能是**数组**（实测如此）—— 上一版用 `Object.entries` 读，于是节点名变成了 "0".."13" ✗
  const readProvider = async (controller, secret) => {
    const snap = await fetch(
      `http://${controller}/providers/proxies/${encodeURIComponent("ta-e2e-sub-form")}`,
      { headers: secret ? { authorization: `Bearer ${secret}` } : {} },
    )
      .then((r) => r.json())
      .catch(() => null);
    const arr = Array.isArray(snap?.proxies)
      ? snap.proxies
      : Object.values(snap?.proxies ?? {});
    return arr.map((v) => [
      v?.name ?? "?",
      v?.history?.length ? v.history[v.history.length - 1].delay : null,
    ]);
  };
  if (earlyController !== null) {
    const res = await fetch(
      `http://${earlyController}/providers/proxies/${encodeURIComponent("ta-e2e-sub-form")}`,
      { method: "PUT", headers: earlySecret ? { authorization: `Bearer ${earlySecret}` } : {} },
    ).catch((e) => ({ status: `throw ${String(e).slice(0, 60)}` }));
    evidence.push(`provider 重新加载（PUT providers/proxies）：HTTP ${res.status}`);
    for (let i = 0; i < 30; i += 1) {
      const delays = await readProvider(earlyController, earlySecret);
      const measured = delays.filter(([, d]) => typeof d === "number" && d > 0).length;
      evidence.push(
        `探测进度[${i}]：已测出延迟的节点 ${measured} 条（共 ${delays.length} 条）` +
          (measured > 0
            ? ` 例：${delays
                .filter(([, d]) => typeof d === "number" && d > 0)
                .slice(0, 3)
                .map(([n, d]) => `${n}=${d}ms`)
                .join(", ")}`
            : ""),
      );
      if (measured >= 2) break;
      await sleep(3000);
    }
  } else {
    evidence.push("读不到内核控制器 ⇒ 跳过 provider 重新加载");
  }

  // 只认**这一轮之后**新写出来的判决行（上一版读"最后一条"，拿到的可能是上一轮的旧结论 ✗）
  const linesBefore28 = judgeLines().length;
  const clicked = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
  evidence.push(`手动刷新点击=${clicked}（触发点②：用户手动更新 ⇒ 重挑）；点击前判决行 ${linesBefore28} 条`);
  let fresh28 = null;
  for (let i = 0; i < 24; i += 1) {
    const all = judgeLines();
    if (all.length > linesBefore28) {
      fresh28 = all[all.length - 1].trim();
      break;
    }
    await sleep(5000);
  }
  evidence.push(`应用这一轮的判决行：${fresh28 ?? "(120s 内没有出现)"}`);
  const echo = await wd.textOf('[data-testid="network-subscriptions-refresh-result"]');
  evidence.push(`界面回执=${JSON.stringify(String(echo ?? "").replace(/\s+/g, " ").slice(0, 120))}`);

  const cfg = readFileSync(join(OUT, "data", "kernel", "kernel.generated.yaml"), "utf8");
  const controller = /external-controller:\s*([^\s]+)/.exec(cfg)?.[1] ?? null;
  const secret = /secret:\s*"([^"]*)"/.exec(cfg)?.[1] ?? "";
  if (controller === null) {
    record("step28", "FAIL", "生成配置里没有 external-controller（读不到内核）", evidence);
    return;
  }
  const head = secret ? { authorization: `Bearer ${secret}` } : {};
  // [docs/46 §291] **必须接住 `fetch` 自身的 reject**：之前 `(await fetch(url)).json().catch(...)` 把
  // `.catch` 只挂在 `json()` 上 ⇒ 内核 REST 瞬时不可达就把整轮打崩（上上次就是这么崩的 ✗）。
  // 现在 `try/catch` + **重试 3 次**，仍失败返回 null，交给判定处理"读不到"。
  const json = async (url) => {
    for (let i = 0; i < 3; i += 1) {
      try {
        const res = await fetch(url, { headers: head });
        return await res.json();
      } catch {
        await sleep(1500);
      }
    }
    return null;
  };

  // [docs/46 §297] **不再自己触发体检**：
  // 应用刚刚用同一张表（同一接口、同一个探测目标）做过裁决，所以现在读到的这张表**就是它读的那张**；
  // 挂具再体检一次只会与应用的探测**并发打架**（上一次就是这么把真最快那条打成超时的 ✗）。
  const pairs = (await readProvider(controller, secret))
    .filter(([, d]) => typeof d === "number" && d > 0)
    .sort((a, b) => a[1] - b[1]);
  const INFO = /剩余|套餐|过滤掉|到期|官网|订阅|流量/;
  const realPairs = pairs.filter(([n]) => !INFO.test(n));
  evidence.push(
    `provider 实测 ${pairs.length} 条（真实节点 ${realPairs.length} 条，升序前 5：${pairs
      .slice(0, 5)
      .map(([n, d]) => `${d}ms ${n}`)
      .join(" | ")}）—— 这张表就是应用刚读过的那张`,
  );

  // 组名 = `LANE-<「自动」线路 id>`；id 现在是按**服务商**派生的 ⇒ 从库里按后端判据找，不硬编码。
  const autoGroupLane = await findAutoLaneId("ta-e2e-sub-form", evidence);
  const GROUP = autoGroupLane === null ? "LANE-lane-ta-e2e-sub-form-auto" : "LANE-" + autoGroupLane;
  const group = await json(`http://${controller}/proxies/${encodeURIComponent(GROUP)}`);
  const now = group?.now ?? null;
  evidence.push(`内核「自动」出口：now=${now ?? "(未选)"}；候选=${(group?.all ?? []).join(", ")}`);
  evidence.push(`应用判断：${String(fresh28 ?? "(无)").slice(0, 200)}`);

  if (now === "ta-lat-fast") {
    record(
      "step28",
      "PASS",
      "按最低延迟选中 ta-lat-fast（慢的 ta-lat-slow 排在候选**第一位** ⇒ 同时排除了「选第一个」）",
      evidence,
    );
    return;
  }
  if (fresh28 === null) {
    record("step28", "FAIL", "手动刷新后应用没有产生新的「自动」出口判决行 ⇒ 这一轮没重算", evidence);
    return;
  }
  const harnessMeasured = realPairs.length;
  if (harnessMeasured === 0) {
    record(
      "step28",
      "SKIP",
      "夹具这一轮也量不出任何延迟（环境问题，不是「选错」）：provider 表里没有一条真实节点延迟",
      evidence,
    );
    return;
  }
  if (String(fresh28).includes("all_unreachable")) {
    record(
      "step28",
      "FAIL",
      `应用判 all_unreachable，但探测目标 ${PROBE_GLOBAL} 可达、夹具也量到了 ${harnessMeasured} 条 ⇒ 这是产品问题：${String(
        fresh28,
      ).slice(0, 140)}`,
      evidence,
    );
    return;
  }
  record("step28", "FAIL", `既没选中 ta-lat-fast：now=${now ?? "(未选)"}；应用判断：${String(fresh28).slice(0, 140)}`, evidence);
}

/** 读"「自动」出口已重算"判决行（跨日志文件按顺序拼接）。 */
function judgeLines() {
  try {
    return readdirSync(join(OUT, "data", "logs"))
      .filter((n) => n.startsWith("tokenagency.log"))
      .flatMap((f) => {
        try {
          return readFileSync(join(OUT, "data", "logs", f), "utf8")
            .split("\n")
            .filter((l) => l.includes("「自动」出口已重算"));
        } catch {
          return [];
        }
      });
  } catch {
    return [];
  }
}

/**
 * [docs/46 §E] **两段式收口**：把健康检查地址写进库后**冷启动应用**，让内核在**启动时**就按
 * "本机可达"的地址探测 —— 运行中改配置不会重读探测设置（§273 已用证据确认），所以必须冷启动。
 *
 * 这是"能在本机拿到《选最低延迟》数值证据"的唯一做法，且**不碰产品、不放宽断言**：
 * 用的全是产品自己的行为（启动时读配置 → 探测 → 按最低延迟写 set_select）。
 */
async function step29(wd, fixture, exe) {
  const evidence = [];
  // [docs/46 §291] 看门狗：本档最长 12 分钟，超时**如实记 FAIL**，不让整轮挂住。
  const watchdog = setTimeout(() => {
    record("step29", "FAIL", "看门狗：step29 超过 12 分钟仍未收尾（内核/界面卡住）", evidence);
    process.exit(1);
  }, 12 * 60 * 1000);
  watchdog.unref();

  // [docs/46 §291] **先把上一个应用/内核实例清干净** —— 用户观察到"有个窗口没关"，那正是根因：
  // 两个实例会抢同一个 `external-controller: 127.0.0.1:39090` ⇒ 后起的起不来
  // ⇒ `TA-9001 内核就绪超时` / REST 不可达 / 我脚本 `fetch failed` 崩掉 / 整轮挂住，全是这一条的下游 ✓
  await new Promise((resolve) => {
    const k = spawn("cmd", ["/c", "taskkill /F /IM tokenagency.exe & taskkill /F /IM mihomo.exe"], {
      stdio: "ignore",
    });
    k.on("exit", () => resolve());
    setTimeout(resolve, 8000);
  });
  const waitFree = async (port) => {
    for (let i = 0; i < 20; i += 1) {
      const free = await new Promise((res) => {
        const s = netConnect(port, "127.0.0.1");
        s.on("connect", () => {
          s.destroy();
          res(false);
        });
        s.on("error", () => res(true));
        setTimeout(() => {
          s.destroy();
          res(true);
        }, 800);
      });
      if (free) return true;
      await sleep(1000);
    }
    return false;
  };
  const portFree = await waitFree(39090);
  evidence.push(`旧应用/内核已清理；控制器端口 39090 空闲=${portFree}（不空闲则后面必然起不来）`);

  await wd.deleteSession().catch(() => null);
  await sleep(1500);

  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(join(OUT, "data", "db.sqlite"));
  const row = db
    .prepare("select json from config where kind='subscription' and entity_id=?")
    .get("ta-e2e-sub-form");
  if (!row) {
    db.close();
    record("step29", "FAIL", "库里没有那条订阅（前面步骤没建出来）", evidence);
    return;
  }
  const doc = JSON.parse(row.json);
  // [docs/46 §297] 体检目标 = 服务商的 `base_url`（应用就是这样测的）。真实订阅是**境外真实节点**
  // ⇒ 挂具的本地端口它们够不着，只能用**公网**端点当探测目标（本机实测 gstatic 204 ✓，
  // 且 [docs/46 §296] 测得 34 条真实节点里有 32 条量得出延迟）。
  const PROBE = PROBE_GLOBAL;
  // ⚠️ 周期**不要**压成 1 秒（上一版踩过）：34 条真实节点每秒钟被整份体检一遍 ⇒ 网络被打满、
  // 测量结果剧烈抖动（同一节点一会儿 137ms 一会儿"量不到"）⇒ 连"上次那条还在不在"都判不准。
  // 应用**自己**会在需要时触发一次完整体检（`provider_delays`），所以这里留默认节奏即可。
  const HEALTH_INTERVAL_S = 60;
  // [docs/46 §288] **用用户给的真实订阅**（36 台真实服务器 ⇒ 真实延迟差异 ⇒ 「选最低延迟」可判）。
  // 订阅 URL 只从 `.ta-tmp/secrets/net-sub.url` **读进内存**，**绝不打印**（只打 host）。
  // 健康检查地址**复位为后端默认**（本机实测可达 gstatic ✓）⇒ 真实节点才量得出延迟。
  // **不猜路径**：多候选依次试（上一次我算错了 REPO 的相对层级 ⇒ 真实订阅根本没读到 ✗
  // ⇒ 后面三次跑全在夹具上打转，白白浪费）。
  const secretCandidates = [
    join(REPO, ".ta-tmp", "secrets", "net-sub.url"),
    join(REPO, "..", ".ta-tmp", "secrets", "net-sub.url"),
    "C:\\Users\\19404\\workspace\\.ta-tmp\\secrets\\net-sub.url",
  ];
  let realSubUrl = null;
  let usedPath = "(无)";
  for (const p of secretCandidates) {
    try {
      const v = readFileSync(p, "utf8").trim();
      if (v !== "") {
        realSubUrl = v;
        usedPath = p;
        break;
      }
    } catch {
      /* 试下一个候选 */
    }
  }
  if (realSubUrl === null || realSubUrl === "") {
    doc.url = fixture.url.replace("/clash.yaml", "/clash-latency.yaml");
    evidence.push("没找到真实订阅文件 ⇒ 退回夹具的 /clash-latency.yaml");
  } else {
    doc.url = realSubUrl;
    doc.health_check = { enabled: true, url: null, interval_s: 60, tolerance_ms: 100 };
    evidence.push(`已用真实订阅（host=${new URL(realSubUrl).host}，URL/token 不外泄）`);
  }
  // [docs/46 §297] 间隔**不要**压到 1 秒（上一版这么做，踩了两次坑）：34 条真实节点每秒被整份体检
  // 一遍 ⇒ 网络打满、同一节点的延迟一会儿 137ms 一会儿"量不到" ⇒ 连"上次那条还在不在"都判不准。
  // 应用**自己**会在需要时触发一次完整体检，所以这里用默认节奏（60 秒）即可。
  doc.health_check = {
    ...(doc.health_check ?? {}),
    enabled: true,
    url: PROBE,
    interval_s: HEALTH_INTERVAL_S,
    tolerance_ms: 100,
  };
  // 顺带把**订阅自己的周期**复位：`step30`（粘性）会把它压到 1 秒来逼出后台刷新，
  // 若不复位，这里的"手动刷新"就会与后台刷新**并发重算**（判决行混在一起，判据说不清是哪一轮的）。
  doc.interval_s = 6 * 3600;
  db.prepare("update config set json=? where kind='subscription' and entity_id=?").run(
    JSON.stringify(doc),
    "ta-e2e-sub-form",
  );
  db.close();
  evidence.push(`已把订阅的健康检查地址改成 ${PROBE}（间隔 ${HEALTH_INTERVAL_S}s）`);
  // [docs/46 §297] 应用体检时带的是**服务商的 `base_url`** ⇒ 冷启动前把它也指向同一个可达目标
  // （夹具里它是 `http://127.0.0.1:1/v1`，不可达 ⇒ 每个节点都会被记成失败 ⇒ 只能得到 all_unreachable）。
  // 「自动」线路 id 现在是**按服务商**派生的（用户第 202 轮）：`lane-<服务商>-auto`。
  // 挂具**不许硬编码**它 —— 从库里按后端同一判据找出来，再用它拼内核里的组名。
  const autoLaneId = await findAutoLaneId("ta-e2e-sub-form", evidence);
  await patchProviderProbeTarget(PROBE, evidence, autoLaneId);

  // [docs/46 §297] **必须确认这条「自动」线路的组类型是 `select`**：`type: url-test` 的组会**忽略**
  // `PUT /proxies/<组>`（HTTP 204 但 `now` 不变 —— 实测见 §297）⇒ 应用算出来的选择根本写不进去。
  // 只当证据打出来（产品侧由 ta-core 的 `auto_lane_on` 保证只认 `group_type == select` 的线路）。
  try {
    const lane = configEntities(dumpScratchDb("step29-lane"), "lane").find(
      (l) =>
        String(l.json?.kind ?? "") === "subscription_auto" &&
        String(l.json?.subscription ?? "") === "ta-e2e-sub-form",
    );
    evidence.push(
      `「自动」线路：id=${lane?.entityId ?? "(没有)"} group_type=${JSON.stringify(lane?.json?.group_type ?? null)}（必须是 select，否则 set_select 会被内核忽略）`,
    );
  } catch (err) {
    evidence.push(`读「自动」线路失败：${String(err).slice(0, 80)}`);
  }

  // **必须清掉 provider 缓存**：mihomo 的 provider 走 `path:` 缓存 + `interval: 21600`（6 小时）⇒
  // 不清就仍然用上一份（夹具）节点 —— 实测：换成真实订阅后节点数仍是 14、now=ta-lat-slow ✗
  // （免挂具脚本里我清过，所以那次拿到了 34 条真节点 ✓；这一步在 step29 里漏了。）
  spawn("cmd", ["/c", "rmdir", "/s", "/q", join(OUT, "data", "kernel", "providers")], {
    stdio: "ignore",
  });
  await sleep(1500);
  evidence.push("已清掉 provider 缓存（否则 mihomo 会继续用旧的夹具节点）");

  await wd.newSession(exe, { args: ["--autostart-mode=window"] });
  evidence.push("已冷启动应用（内核会在启动时读这份配置）");

  // [docs/46 §E] 冷启动后**不会自动重抓订阅**（同步任务实测会报「本轮没有到期的」）⇒ provider 不会被登记
  // ⇒ `/providers/proxies/<key>` 是空的、也就无所谓"选谁" ✗。所以这里**主动点一次「手动刷新」**
  // （它同时就是触发点②：用户手动更新 ⇒ 重挑）⇒ provider 登记 + 触发重算 ⇒ 才谈得上比较延迟 ✓
  // 冷启动后**必须等页面真正就绪再点**：上次直接 sleep 1.5s 就点，命中了 `缺控件 network-subscriptions-refresh`
  // ⇒ 刷新没触发 ⇒ 重算没发生 ⇒ 拿到的是 mihomo 默认值（不是产品的判断）✗
  //
  // [docs/46 §297] **两段等待**（上一版只有 30 秒的一段，实测不够：冷启动要重读配置 + 拉订阅 +
  // 起内核，30 秒里 `#/network` 还没渲染出来 ⇒ 刷新点了个空 ⇒ 日志停在**上一轮的旧判决行** ⇒ 误判）：
  //   ① 先等窗口真的渲染出 `page-header`（最多 90 秒）；
  //   ② 再反复 `gotoPage("#/network")` 直到刷新控件出现（最多 5 轮 × 20 秒）。
  let headerReady = false;
  for (let i = 0; i < 45; i += 1) {
    const tids = await wd.probeTestIds().catch(() => []);
    if (Array.isArray(tids) && tids.includes("page-header")) {
      headerReady = true;
      break;
    }
    await sleep(2000);
  }
  evidence.push(`冷启动后窗口渲染出 page-header=${headerReady}`);
  let refreshReady = false;
  let waitedS = 0;
  for (let round = 0; round < 5 && !refreshReady; round += 1) {
    await gotoPage(wd, "#/network");
    for (let i = 0; i < 20; i += 1) {
      const tids = await wd.probeTestIds().catch(() => []);
      if (Array.isArray(tids) && tids.includes("network-subscriptions-refresh")) {
        refreshReady = true;
        break;
      }
      waitedS += 1;
      await sleep(1000);
    }
    evidence.push(`第 ${round + 1} 轮等 /network：「刷新」控件就绪=${refreshReady}（累计 ${waitedS}s）`);
  }
  // 记录点击前的日志行数：判定只认**这一轮之后**新写出来的判决行（否则会拿旧行当结论 ✗）
  const logFiles = readdirSync(join(OUT, "data", "logs")).filter((n) =>
    n.startsWith("tokenagency.log"),
  );
  const linesBefore = logFiles.reduce((acc, f) => {
    try {
      return (
        acc +
        readFileSync(join(OUT, "data", "logs", f), "utf8")
          .split("\n")
          .filter((l) => l.includes("「自动」出口已重算")).length
      );
    } catch {
      return acc;
    }
  }, 0);
  const kicked = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
  evidence.push(
    `冷启动后第一次手动刷新（登记订阅；此刻内核里 provider 可能还没加载）：点击=${kicked}；点击前已有判决行 ${linesBefore} 条`,
  );
  if (!kicked) {
    const again = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
    evidence.push(`刷新控件点不动，再试一次=${again}`);
  }
  await sleep(12000);

  // 等内核起来并完成首轮探测；期间每 3 秒看一次 provider 快照。
  const cfgPath = join(OUT, "data", "kernel", "kernel.generated.yaml");
  let controller = null;
  let secret = "";
  for (let i = 0; i < 20 && controller === null; i += 1) {
    await sleep(3000);
    try {
      const text = readFileSync(cfgPath, "utf8");
      controller = /external-controller:\s*([^\s]+)/.exec(text)?.[1] ?? null;
      secret = /secret:\s*"([^"]*)"/.exec(text)?.[1] ?? "";
    } catch {
      controller = null;
    }
  }
  evidence.push(`内核控制器：${controller ?? "(没读到)"}`);
  if (controller === null) {
    record("step29", "FAIL", "冷启动后读不到内核控制器", evidence);
    return;
  }
  const head = secret ? { authorization: `Bearer ${secret}` } : {};
  const json = async (url) => {
    for (let i = 0; i < 3; i += 1) {
      try {
        const res = await fetch(url, { headers: head });
        return await res.json();
      } catch {
        await sleep(1500);
      }
    }
    return null;
  };

  // [docs/46 §289] **显式让 mihomo 重新读 provider 文件** —— 这条是 step28 里有、step29 里漏掉的：
  // 应用手动刷新后会把**真实订阅**写进 provider 文件 ✓，但 mihomo 的 provider 走 `path:` 缓存
  // ⇒ 不刷新它就还用旧的（实测：不清缓存=夹具 14 条；清了缓存不刷新=0 条 ✗）。
  const putRes = await fetch(
    `http://${controller}/providers/proxies/${encodeURIComponent("ta-e2e-sub-form")}`,
    { method: "PUT", headers: head },
  ).catch((e) => ({ status: `throw ${String(e).slice(0, 50)}` }));
  evidence.push(`provider 显式刷新：HTTP ${putRes.status}`);
  await sleep(5000);

  // —— [docs/46 §290/§297] 最后一击：**先确保延迟表是真的**，再读应用自己的判决 ——
  // 判据用的表与应用**同一张**（provider 的 `history[].delay`）；挂具这一轮显式触发一次体检，
  // 保证读到的不是"还没测过"的空表。探测目标就是应用会用的那条（服务商 base_url）。
  const SUBKEY = "ta-e2e-sub-form";
  const GROUPNAME = autoLaneId === null ? "LANE-lane-ta-e2e-sub-form-auto" : "LANE-" + autoLaneId;
  const provNodes = () => readProviderNodes(controller, secret, SUBKEY);
  // **[修正]** provider 快照里的名字与组 `all` 里的名字是同一批（`/proxies/<组>` 的 `all` 更权威）；
  // 但**测延迟只能走 provider 接口**（见下面的 [docs/46 §297]），`/proxies/<name>/delay` 对它们全 404。
  const groupObj = await json(`http://${controller}/proxies/${encodeURIComponent(GROUPNAME)}`);
  const groupAll = groupObj?.all ?? [];
  const provNames = (await provNodes()).map(([n]) => n);
  const names = groupAll.length > 0 ? groupAll : provNames;
  evidence.push(`节点来源：组 all=${groupAll.length} 条 / provider=${provNames.length} 条（延迟一律读 provider 那张表）`);
  // [docs/46 §295] **先等"应用自己的健康检查"把延迟填上**（这才是能跑通的方式：免挂具脚本实测 32/34 有值 ✓）；
  // 后面那次显式体检只是"保证这一刻一定有值"。每步带**时间戳**便于分析耗时（按用户要求）。
  const tProbe = Date.now();
  const mark = (s) => evidence.push(`+${String(Date.now() - tProbe).padStart(6)}ms  ${s}`);
  for (let round = 0; round < 30; round += 1) {
    const got = measuredNodes(await provNodes()).length;
    mark(`等待健康检查：已测出 ${got} 条 / ${names.length} 条`);
    if (got >= 8) break;
    await sleep(5000);
  }
  mark(`等待结束（${Math.round((Date.now() - tProbe) / 1000)}s）`);

  // [docs/46 §297] **点第二次「更新」**（这一步是 E 期能判定的关键）：
  // 第一次点的时候内核里 provider 还没加载好 ⇒ 应用那一刻体检必然 `measured=0` ⇒ 结论没有意义。
  // 等挂具确认"provider 已加载且量到了 ≥8 条"之后再点一次，应用这一轮才有东西可量 ——
  // 判定用的就是**这一轮**的结论（触发点②：用户手动更新 ⇒ 重挑最低延迟）。
  const linesBefore2 = judgeLines().length;
  const kicked2 = await wdClickTid(wd, "network-subscriptions-refresh", evidence);
  evidence.push(
    `第二次手动刷新（provider 已就绪）：点击=${kicked2}；点击前判决行 ${linesBefore2} 条`,
  );
  let freshVerdict = null;
  for (let i = 0; i < 30; i += 1) {
    const all = judgeLines();
    if (all.length > linesBefore2) {
      freshVerdict = all[all.length - 1].trim();
      break;
    }
    await sleep(5000);
  }
  mark(`应用这一轮的判决行：${freshVerdict === null ? "(150s 内没有出现)" : "出现了"}`);
  // —— [docs/46 §297] 测量**只能走 provider 命名空间**，而且**不再自己触发体检** ——
  // 上一版 ① 用 `/proxies/<name>/delay` 逐个强制探测（**0/34 全 404**）② 又用挂具自己的
  // `healthcheck` 去测一次 ⇒ 与应用那一轮的探测**并发打架**（34 条节点被同时拨两遍，真最快那条
  // 被打成超时）⇒ 拿挂具的表去否应用的结论，等于自己制造假红 ✗。
  // 现在：应用刚用**同一个接口、同一个探测目标**裁决过，**它读的就是这张表**；挂具只读不测。
  await sleep(3000);
  const pairs = measuredNodes(await provNodes()).sort((a, b) => a[1] - b[1]);
  const INFO = /剩余|套餐|过滤掉|到期|官网|订阅|流量/;
  const realPairs = pairs.filter(([n]) => !INFO.test(n));
  evidence.push(
    `provider 这张表（应用刚读过的那张）实测 ${pairs.length} 条；真实节点 ${realPairs.length} 条；最快真实=${realPairs[0] ? `${realPairs[0][0]} ${realPairs[0][1]}ms` : "-"}`,
  );
  // 应用自己的判决行（**只认第二次刷新之后新写出来的那条**，否则会把上一轮的旧结论当结论 ✗）
  const verdictLine = freshVerdict ?? `(这一轮没有新的判决行；点击前已有 ${linesBefore2} 条)`;
  const freshCount = freshVerdict === null ? 0 : 1;
  evidence.push(`应用判决：${verdictLine.slice(0, 220)}`);
  const groupNow = (await json(`http://${controller}/proxies/${encodeURIComponent(GROUPNAME)}`))?.now ?? null;
  evidence.push(`组 now=${groupNow ?? "(未选)"}`);

  if (freshCount === 0) {
    record(
      "step29",
      "FAIL",
      "冷启动后的手动刷新没有产生新的「自动」出口判决行 ⇒ 应用这一轮根本没重算（先看刷新控件是否真的点到了）",
      evidence,
    );
    return;
  }
  if (realPairs.length === 0) {
    record("step29", "FAIL", "provider 体检后仍无任何真实节点延迟 ⇒ 探测链路不通（不是「选错」）", evidence);
    return;
  }
  // **一条硬断言**：应用日志里的 `probe=` 必须就是我们在库里给它设的那个探测目标
  // （= 服务商 base_url）。这是"应用真的按服务商 base_url 测延迟"的书面证据。
  const probeMismatch =
    verdictLine.includes("probe=") && !verdictLine.includes(PROBE);
  const appMeasured = Number(/measured=(\d+)/.exec(verdictLine)?.[1] ?? "0");
  evidence.push(`应用这一轮：measured=${appMeasured} probe 与测试台一致=${!probeMismatch}`);
  if (probeMismatch) {
    record(
      "step29",
      "FAIL",
      `应用探测用的不是服务商 base_url（日志里 probe≠${PROBE}）：${verdictLine.slice(0, 120)}`,
      evidence,
    );
    return;
  }
  if (appMeasured === 0) {
    record(
      "step29",
      "FAIL",
      `应用这一轮一个节点都没量到延迟（measured=0）⇒ 探测没跑起来：${verdictLine.slice(0, 140)}`,
      evidence,
    );
    return;
  }
  const fastestName = realPairs[0][0];
  const fastestMs = realPairs[0][1];
  const nowMs = pairs.find(([n]) => n === groupNow)?.[1] ?? null;
  const appNode = /node="([^"]*)"/.exec(verdictLine)?.[1] ?? null;
  const pickedMs = Number(/picked_ms=(-?\d+)/.exec(verdictLine)?.[1] ?? "-1");
  const bestMs = Number(/best_ms=(-?\d+)/.exec(verdictLine)?.[1] ?? "-1");
  evidence.push(
    `明细（升序前 8）：${pairs.slice(0, 8).map(([n, m]) => `${m}ms ${n}`).join(" | ")}`,
  );
  evidence.push(
    `判定：now=${groupNow ?? "(未选)"}（${nowMs ?? "?"}ms）；应用日志里选的=${appNode ?? "(无)"}` +
      `（picked_ms=${pickedMs} / best_ms=${bestMs}）；这张表里最快=${fastestName}（${fastestMs}ms）`,
  );
  // **写的那一半**（[docs/46 §297] ⑦⑧）：应用算出来的选择必须真的落到内核的组上
  // （只有 `type: select` 的组认这一刀；`url-test` 组会返回 204 但 `now` 不变）。
  if (appNode !== null && groupNow !== appNode) {
    record(
      "step29",
      "FAIL",
      `应用选了 ${appNode} 但内核组里 now=${groupNow ?? "(未选)"} ⇒ set_select 没生效（组不是 select？）`,
      evidence,
    );
    return;
  }
  // **"选了最低延迟"这句话的判据**（[docs/46 §300]）：用**应用自己那张表**自证 ——
  // `picked_ms == best_ms` 就是"它选了自己表里最低的那条"。为什么不拿挂具后来读到的最快去比：
  // 真机实测这批境外节点在几十秒内抖动极大（同一条节点 137ms → 1523ms → 3006ms 都被读到过），
  // 两次独立测量互相对账会把一个**正确**的实现判成错。挂具的读数照旧打在证据里（含量、明细），
  // 但不作判据；判据是"应用真的按服务商 base_url 测得了一张表（measured>0）"+
  // "它选的是那张表里最低的（picked_ms==best_ms、且两者都 > 0）"+"那一刀真的写进内核（now==node）"。
  if (pickedMs <= 0 || bestMs <= 0) {
    record(
      "step29",
      "FAIL",
      `应用那一轮没有可用的延迟自证（picked_ms=${pickedMs} / best_ms=${bestMs}）⇒ 裁决不是基于实测延迟`,
      evidence,
    );
    return;
  }
  if (pickedMs !== bestMs) {
    record(
      "step29",
      "FAIL",
      `应用没有选它那张表里最低的那条：picked_ms=${pickedMs} ≠ best_ms=${bestMs}（node=${appNode}）`,
      evidence,
    );
    return;
  }
  record(
    "step29",
    "PASS",
    `「自动」出口 = 应用那张表里**最低延迟**那条（${appNode}，picked_ms=${bestMs}ms；measured=${appMeasured}/${pairs.length}），` +
      `并真的写进内核（now==node）；挂具独立读同一张表：最快=${fastestName} ${fastestMs}ms（两轮之间抖动见证据）`,
    evidence,
  );
}

/**
 * [docs/46 §299] **粘性真机验证 —— 触发点①：后台自动刷新（`manual = false`）**
 *
 * 用户第 88 轮的原话要求：订阅更新后，**如果上次选的出口地址还在，就沿用**它（出口 IP 不变）。
 * 这条与"手动刷新"共用同一个实现、只差一个布尔值，可真机上要验它需要一个**非手动**的刷新：
 * 界面上的「刷新」按钮按设计是 `manual = true`，所以这里走**应用自己的后台同步任务**
 * ——把订阅周期压到 1 秒（改库 + **冷启动**，让应用真的读到它），它每 60 秒看一次"有没有到期的"。
 *
 * 三条断言一起成立才算通过：
 *   ① 出现**新的**判决行且 `status="same_address_still_there"`（不是重挑）；
 *   ② 那行里的 `node` 与上一轮手动选出来的**逐字相同**；
 *   ③ 内核组的 `now` 与上一轮**逐字相同**（出口真的没动 ⇒ IP 不变）。
 */
async function step30(wd, fixture, exe) {
  const evidence = [];
  const watchdog = setTimeout(() => {
    record("step30", "FAIL", "看门狗：step30 超过 10 分钟仍未收尾", evidence);
    process.exit(1);
  }, 10 * 60 * 1000);
  watchdog.unref();

  // 基准：上一轮（手动刷新）的判决行 + 内核组里的 now
  const before = judgeLines();
  const baseline = before.length > 0 ? before[before.length - 1].trim() : null;
  const baseNode = /node="([^"]*)"/.exec(baseline ?? "")?.[1] ?? null;
  if (baseline === null || baseNode === null || baseNode === "(未选)") {
    record(
      "step30",
      "SKIP",
      "前一步没有留下「已选中的出口」（日志里没有可用的基准判决行）⇒ 没得谈「沿用」",
      [baseline ?? "(无判决行)"],
    );
    return;
  }
  const cfgPath = join(OUT, "data", "kernel", "kernel.generated.yaml");
  let controller = null;
  let secret = "";
  try {
    const text = readFileSync(cfgPath, "utf8");
    controller = /external-controller:\s*([^\s]+)/.exec(text)?.[1] ?? null;
    secret = /secret:\s*"([^"]*)"/.exec(text)?.[1] ?? "";
  } catch {
    /* 读不到就下面判红 */
  }
  const head = secret ? { authorization: `Bearer ${secret}` } : {};
  const json = async (url) => {
    for (let i = 0; i < 3; i += 1) {
      try {
        return await (await fetch(url, { headers: head })).json();
      } catch {
        await sleep(1200);
      }
    }
    return null;
  };
  // 组名 = `LANE-<「自动」线路 id>`；id 现在是按**服务商**派生的 ⇒ 从库里按后端判据找，不硬编码。
  const autoGroupLane = await findAutoLaneId("ta-e2e-sub-form", evidence);
  const GROUP = autoGroupLane === null ? "LANE-lane-ta-e2e-sub-form-auto" : "LANE-" + autoGroupLane;
  const now0 =
    controller === null
      ? null
      : ((await json(`http://${controller}/proxies/${encodeURIComponent(GROUP)}`))?.now ?? null);
  evidence.push(
    `基准：应用上一轮选的是 node=${JSON.stringify(baseNode)}；内核组 now=${JSON.stringify(now0)}`,
  );
  if (now0 !== baseNode) {
    record("step30", "FAIL", `基准就不成立：应用日志里选的=${baseNode} 但内核组 now=${now0}`, evidence);
    return;
  }

  // [docs/29 §2.3] 把订阅周期压到 1 秒 ⇒ 应用自己的后台同步任务会把它判成"到期"。
  // **必须冷启动**：应用读的是**内存里的文档**，改库对运行中的进程无效（[docs/35 §8] 的旧教训）。
  try {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(join(OUT, "data", "db.sqlite"));
    const row = db
      .prepare("select json from config where kind='subscription' and entity_id=?")
      .get("ta-e2e-sub-form");
    const doc = JSON.parse(row.json);
    const beforeS = doc.interval_s;
    doc.interval_s = 1;
    db.prepare("update config set json=? where kind='subscription' and entity_id=?").run(
      JSON.stringify(doc),
      "ta-e2e-sub-form",
    );
    db.close();
    evidence.push(`订阅周期 ${beforeS}s → **1s**（应用自己的同步任务每 60 秒看一眼"有没有到期的"）`);
  } catch (err) {
    record("step30", "FAIL", `改订阅周期失败（这一步的前提）：${String(err).slice(0, 160)}`, evidence);
    return;
  }
  await wd.deleteSession().catch(() => null);
  await sleep(1500);
  await wd.newSession(exe, { args: ["--autostart-mode=window"] });
  evidence.push("已冷启动应用 ⇒ 首轮同步任务立刻跑（`auto_refresh_subscription` → `manual = false`）");

  // 从这一步起观察**新的**判决行（后台刷新那一条）。应用 tick 60 秒一次，首轮可能因内核没就绪
  // 而没有判决 ⇒ 下面按"轮"等，每轮最多 150 秒，最多 4 轮。
  evidence.push("开始观察后台刷新的判决行（每条新判决行 = 应用自己那一轮的重算结论）");
  // 观察最多 4 轮后台判决（每轮 ~60 秒）：粘性需要**一轮"上次那条量到了"**才能被看到；
  // 若这几轮里它恰好每轮都没量到，那应用按规则重挑是**对的**（用户原话的第二种重挑情形），
  // 这时不能判红 —— 但也不能判绿（粘性没被观察到），如实记 SKIP 并写明理由。
  const readNow = async () => {
    // ⚠️ 冷启动之后**必须重新读控制器与 secret**：应用每次生成配置都会**换一个 secret**
    // （[docs/11 §2.3]：随机 secret + reload），拿旧 secret 去打只会拿到 401 ⇒ `now=null`
    // （上一次 step30 就是这么假红的 ✗）。
    try {
      const text = readFileSync(cfgPath, "utf8");
      const c = /external-controller:\s*([^\s]+)/.exec(text)?.[1] ?? null;
      const s = /secret:\s*"([^"]*)"/.exec(text)?.[1] ?? "";
      if (c !== null) {
        controller = c;
        secret = s;
      }
    } catch {
      /* 读不到就用上一次的 */
    }
    const h = secret ? { authorization: `Bearer ${secret}` } : {};
    try {
      const res = await fetch(`http://${controller}/proxies/${encodeURIComponent(GROUP)}`, {
        headers: h,
      });
      if (!res.ok) return null;
      return (await res.json())?.now ?? null;
    } catch {
      return null;
    }
  };
  let sticky = null;
  let lastCount = before.length;
  const observed = [];
  const t0 = Date.now();
  // **基准会移动**：上一轮按规则重挑之后，"上次那条"就变成新选的那条 —— 粘性要观察的正是
  // "它下一次还在不在"。所以每轮把 baseline 更新成该轮应用写进内核的那条。
  let stickyBase = baseNode;
  for (let round = 0; round < 4 && sticky === null; round += 1) {
    let line = null;
    for (let i = 0; i < 30; i += 1) {
      const all = judgeLines();
      if (all.length > lastCount) {
        line = all[all.length - 1].trim();
        lastCount = all.length;
        break;
      }
      await sleep(5000);
    }
    if (line === null) {
      observed.push(`第 ${round + 1} 轮：150s 内没有新的判决行`);
      continue;
    }
    const status = /status="([^"]*)"/.exec(line)?.[1] ?? "(无)";
    const node = /node="([^"]*)"/.exec(line)?.[1] ?? null;
    let now1 = null;
    for (let i = 0; i < 30 && now1 === null; i += 1) {
      now1 = await readNow();
      if (now1 === null) await sleep(2000);
    }
    observed.push(
      `第 ${round + 1} 轮：status=${status} node=${JSON.stringify(node)} now=${JSON.stringify(now1)}（基准=${JSON.stringify(stickyBase)}）`,
    );
    if (status === "same_address_still_there") {
      // [docs/46 §301] 真机实测（2026-09-27）：应用这一轮报的是 `same_address_still_there`
      // （= 上次那条的**地址**还在订阅里 ⇒ 按规则沿用，出口 IP 没变），但**名字**从
      // `新加坡-优化2-GPT` 变成了 `日本-优化2` —— 机场里**多个名字共用一个地址**是常态，
      // 而内核 `/providers/proxies/<key>` 的节点对象**不含 address 字段**（实测确认），
      // 挂具没法独立比地址。所以判据是：
      //   · 硬要求：应用日志里的 node 与内核组的 now **一致**（说明它真的写进内核了）；
      //   · 名字变了 ⇒ **不算红**，如实记一条说明（地址是否相同由应用自己的判据决定，
      //     它判的就是地址；挂具不再用"名字"去当"IP"）。
      if (now1 === null || node === null || node === "(未选)" || now1 !== node) {
        record(
          "step30",
          "FAIL",
          `应用说沿用了（same_address_still_there），但日志与内核不一致：日志 node=${JSON.stringify(node)} / 内核 now=${JSON.stringify(now1)}`,
          evidence,
        );
        return;
      }
      if (node !== stickyBase) {
        observed.push(
          `（沿用成立：status=same_address_still_there ⇒ 出口地址没变；名字从上轮的 ${JSON.stringify(stickyBase)} ` +
            `变成了 ${JSON.stringify(node)} —— 同一地址的另一个名字，不算换出口）`,
        );
      }
      sticky = {
        status,
        node,
        now: now1,
        line,
        // ⚠️ `base` 存的是**上一轮那个名字本身**，绝不能存"给人看的说明串"：
        //    最终判定曾经拿它去比 `sticky.node`，而它在"名字变了"时被拼成
        //    `"旧名 →同名地址 新名"` ⇒ 永远不等于 node ⇒ 真机上只要遇到
        //    "同一个地址换了个名字"（机场里很常见）就**假红**。见下面最终判定处的注释。
        base: stickyBase,
        nameChanged: node !== stickyBase,
      };
      break;
    }
    // 表是挂具这一轮独立读的（`alive === true` 才算"测得出来"）
    const table = await readProviderNodes(controller, secret, "ta-e2e-sub-form");
    const alive = measuredNodes(table).some(([n]) => n === stickyBase);
    const present = table.some(([n]) => n === stickyBase);
    if (status === "previous_unreachable") {
      // 判定逻辑抽成纯函数 `judgePreviousUnreachable`（见下方定义处），
      // 这样它**能被 `--selfcheck` 直接喂用例** —— 因为这一分支只在
      // "订阅里同一个地址换了名字 / 上次那条这轮没量到" 时才走到，
      // 靠真机碰运气的话，修好了也可能永远验证不到（本档已经吃过两次这个亏）。
      const verdict = judgePreviousUnreachable({ alive, node, stickyBase });
      if (verdict.kind === "fail") {
        record("step30", "FAIL", verdict.detail, evidence);
        return;
      }
      if (verdict.kind === "undecidable") {
        const addr = await storedAutoAddress();
        observed.push(
          `第 ${round + 1} 轮：${verdict.detail}（应用按**地址**判，存的地址=${JSON.stringify(addr)}）`,
        );
      } else {
        observed.push(`（这一轮 ${stickyBase} 在挂具表里也不 alive ⇒ 按规则重挑是对的）`);
      }
      if (node !== null && node !== "(未选)") stickyBase = node;
      continue;
    }
    if (status === "previous_gone") {
      if (present) {
        record(
          "step30",
          "FAIL",
          `应用说上次那条地址"不见了"（previous_gone），但挂具这张表里 ${stickyBase} 还在 ⇒ 误判会换出口 IP`,
          evidence,
        );
        return;
      }
      observed.push(`（这一轮 ${stickyBase} 确实不在订阅里了 ⇒ 按规则重挑）`);
      if (node !== null && node !== "(未选)") stickyBase = node;
      continue;
    }
    if (node !== null && node !== "(未选)") stickyBase = node;
    observed.push(`（这一轮理由=${status}，不是"沿用"；基准更新为 ${JSON.stringify(stickyBase)}）`);
  }
  evidence.push(`后台刷新的观察（等了 ${Math.round((Date.now() - t0) / 1000)}s）：\n    ${observed.join("\n    ")}`);
  if (sticky === null) {
    if (observed.every((o) => o.includes("没有新的判决行"))) {
      record(
        "step30",
        "FAIL",
        "订阅周期压到 1 秒之后，应用的后台同步任务**一条判决行都没产生** ⇒ 触发点①（订阅更新后重算）没跑",
        evidence,
      );
      return;
    }
    record(
      "step30",
      "SKIP",
      `粘性这一轮没被观察到（每轮都是"上次那条没量到"⇒ 应用按规则重挑了；不可能判绿，也不该判红）：${observed.join("；").slice(0, 200)}`,
      evidence,
    );
    return;
  }
  // 最终判定：判的是"**出口有没有变**"，而出口的判据是**地址**，不是名字。
  //
  // 挂具读不到地址（内核 `/proxies/<key>` 的节点对象里没有 address 字段 —— [docs/46 §301] 实测确认），
  // 所以"地址还是不是同一条"只能采信应用自己那条 `status=same_address_still_there`；
  // 挂具这一侧能**独立**验的是："它真的把决定写进内核了"（日志 node == 内核 now）。
  //
  // ⚠️ 这里原来还挂着一条 `|| sticky.node !== sticky.base`：那是一次**把说明文案当判据**的错
  // （`base` 在"名字变了"时被存成 `"旧名 →同名地址 新名"` 这种给人看的串，永远不等于 node）——
  // 它只在"名字恰好没变"时通过，等于没在验任何东西，而真机一旦真的换了名字就**假红**。已删。
  // 判定本身抽成纯函数 `judgeSticky`，于是它**能被自检直接喂用例**（`--selfcheck`）——
  // 否则这条只在"名字真的变了"时才走到的分支，永远没法证明它修好了。
  const verdict = judgeSticky(sticky);
  if (!verdict.ok) {
    record("step30", "FAIL", verdict.detail, evidence);
    return;
  }
  record("step30", "PASS", verdict.detail, evidence);
}

/**
 * `step10` 的**判定之一**（抽成纯函数，好让 `--selfcheck` 直接喂用例）：
 * 成本中心上**有没有定价写入口**。
 *
 * 为什么值得单独抽出来：这一条是用户那句"成本中心页面只做显示，价格配置到模型页面去"
 * 的**可执行形式**，而它的失败模式恰好是"我加了断言、但那条分支在真机上根本没走到"
 * （`writeIds` 为空时它绿得毫无意义 —— 一个**空的**页面一样能通过）。
 * 自检里喂了"只有只读控件"与"混进一个写入口"两种输入，钉住它真的会红。
 *
 * 判据只有一条：**不许出现任何能改定价的 testid**。
 * 名字按前缀认（`pricing-add*`）而不是把 7 个 testid 抄一遍 —— 抄一遍的话，
 * 以后有人加个 `pricing-add-bulk` 就又漏了。
 */
function pricingWriteEntries(ids) {
  return ids.filter(
    (tid) =>
      tid.startsWith("pricing-add") || tid === "pricing-delete" || tid === "pricing-edit",
  );
}

/**
 * `step10` 的**判定之一**（抽成纯函数，好让 `--selfcheck` 直接喂用例）：
 * 落库的那条价，`match` 是不是"**只按这一个对外模型**"的形状。
 *
 * 判据三条：
 * 1. `match.model` **等于**那一行的模型 id（等于别的模型 = 价挂错了对象）；
 * 2. `provider` / `credential` **都不构成过滤** —— 判据是 `== null`（`null` 或不存在都算），
 *    **不是**"键必须不存在"；
 * 3. `match` 本身是个对象（不是字符串/数字/null）。
 *
 * ⚠️ **第 2 条的写法是本轮真机逼出来的**（见调用点那段注释）：`PriceMatch` 是
 * `Option<T>` + `#[serde(default)]`，落库时把 `None` 写成显式 `null` 是本仓的既有习惯
 * （完整配置里到处可见，例如同一行的 `"usd_to_cny":null`）。而引擎对 `null` 与
 * "键不存在"**完全同构**（反序列化后都是 `None`，`matches_request` 走 `unwrap_or(true)`）。
 * 原来写成 `provider === undefined` 是在判**序列化习惯**而不是判**定价语义** ——
 * 产品没问题，是挂具越过了语义去要求一种特定的 json 形状（一次假红）。
 *
 * 反过来，`provider: "p-x"` / `credential: "c-x"`（**真的**加了过滤条件）必须判红：
 * 那才是用户否掉的"对服务商的线路计价"。
 */
function judgePricedMatch(match, model) {
  if (match === null || typeof match !== "object") {
    return { ok: false, detail: `match 不是对象：${JSON.stringify(match)}` };
  }
  if (match.model !== model) {
    return { ok: false, detail: `match.model=${JSON.stringify(match.model)} ≠ 那一行的 ${model}` };
  }
  if (match.provider != null) {
    return { ok: false, detail: `match 带了 provider=${JSON.stringify(match.provider)}（这就是"对服务商计价"）` };
  }
  if (match.credential != null) {
    return { ok: false, detail: `match 带了 credential=${JSON.stringify(match.credential)}（这就是"对线路计价"）` };
  }
  return { ok: true, detail: `只按模型 ${model} 定价（provider/credential 都不构成过滤）` };
}

/**
 * `step30` 的**最终判定**（抽成纯函数，好让 `--selfcheck` 直接喂用例）。
 *
 * 判的是"**出口有没有变**"。出口的判据是**地址**，而挂具读不到地址
 * （内核 `/proxies/<key>` 的节点对象里没有 address 字段，[docs/46 §301] 实测确认），
 * 所以：
 *
 * * 地址还是不是同一条 —— 只能采信应用自己那条 `status=same_address_still_there`；
 * * 挂具能**独立**验的 —— "它真的把决定写进内核了"（日志 `node` == 内核 `now`）。
 *
 * ⚠️ **历史坑（本函数存在的理由）**：这里原来还比了 `node !== base`，而 `base` 在"名字变了"时
 * 是 `"旧名 →同名地址 新名"` 这种**给人看的说明串** ⇒ 恒不等于 `node` ⇒
 * 名字没变时侥幸通过、名字真变了就假红。**永远不要拿证据文案当判据。**
 */
function judgeSticky(sticky) {
  if (sticky.status !== "same_address_still_there") {
    return {
      ok: false,
      detail:
        `沿用判定不成立：status=${JSON.stringify(sticky.status)} 日志 node=${JSON.stringify(sticky.node)} / ` +
        `内核 now=${JSON.stringify(sticky.now)}，基准=${JSON.stringify(sticky.base)}`,
    };
  }
  if (sticky.now === null || sticky.node === null || sticky.now !== sticky.node) {
    return {
      ok: false,
      detail:
        `沿用判定不成立：status=${JSON.stringify(sticky.status)} 日志 node=${JSON.stringify(sticky.node)} / ` +
        `内核 now=${JSON.stringify(sticky.now)}（两者必须一致：日志里的决定要真的写进内核）`,
    };
  }
  // 文案如实分两种：名字没变 vs **同一地址换了名字**（后者不代表换出口，必须说出来，
  // 不能让它看起来像"名字也没变"）。
  const exitNote = sticky.nameChanged
    ? `出口地址沿用；名字由 ${JSON.stringify(sticky.base)} 变为 ${JSON.stringify(sticky.node)}` +
      `（同一地址的另一个名字，不算换出口）`
    : `node 与 now 都还是 ${JSON.stringify(sticky.node)}`;
  return {
    ok: true,
    detail: `订阅更新后**沿用**上次那条出口：status=same_address_still_there，${exitNote}（出口 IP 没变）`,
  };
}

/**
 * `step30` 里 `previous_unreachable` 那一轮的**判定**（纯函数，好让 `--selfcheck` 喂用例）。
 *
 * ## 为什么不能"表里名字 alive ⇒ 判产品红"
 *
 * **应用是按「地址」判的，挂具是按「名字」读的 —— 两者不是同一个层次。**
 *
 * * 应用存的是 `store_setting.auto_export:<provider>` = **server 主机名**（实测真机
 *   `m.cnmjin.net`，不含 port），判据是 `choose_auto_export` 里的
 *   `find(|c| c.address == previous)`；
 * * 挂具读的是 mihomo `/providers/proxies/<key>`，里面**只有名字**与内核自己的健康历史。
 *
 * 而机场里**多条节点共用同一个 server** 是常态（真机实测：`新加坡-优化2-GPT` → `m.cnmjin.net:16618`，
 * `香港-优化2` → `m.cnmjin.net:16632`）⇒ 三个后果：
 * ① 应用匹配的那个"同地址候选"与挂具看的**名字**可能不是同一条；
 * ② mihomo 的 `history.delay` 可能是**上一轮**的旧值；
 * ③ "地址可达"与"这条线路此刻可达"是两件事。
 *
 * 所以"名字 alive"**推不出**"应用误判了地址"，更推不出"会换出口 IP"。
 * **挂具不能凭一个它证明不了的层次差异给产品定罪。**
 *
 * ## 判定分成三档
 *
 * * `fail` —— **同一层次**的自相矛盾：应用说"上次那条不通"，却**一条都没选出来**
 *   （`node` 空/未选）= 既没沿用也没重挑。这条挂具能证明。
 * * `undecidable` —— 应用选出了出口（`node` 有值），挂具只是按名字看到 alive。
 *   **如实记"无法判定"**：不判红（假红），也不判绿（假绿）。
 * * `app_right` —— 挂具表里那个名字也不 alive ⇒ 应用按规则重挑是对的。
 */
function judgePreviousUnreachable({ alive, node, stickyBase }) {
  const picked = node !== null && node !== undefined && node !== "(未选)";
  if (!picked) {
    return {
      kind: "fail",
      detail:
        `应用报 previous_unreachable 但没选出任何出口（node=${JSON.stringify(node)}）⇒ 既没沿用也没重挑`,
    };
  }
  if (alive) {
    return {
      kind: "undecidable",
      detail:
        `应用报 previous_unreachable，而挂具按**名字**读到 ${JSON.stringify(stickyBase)} 是 alive` +
        `——**无法判定**：应用按**地址**判，挂具按**名字**读，而 ${JSON.stringify(stickyBase)} ` +
        `可能与其它节点共用同一个 server（机场常态），且 mihomo 的 history 可能是上一轮的旧值。` +
        `这一轮不作为产品对错的判据。`,
    };
  }
  return { kind: "app_right", detail: `${JSON.stringify(stickyBase)} 在挂具表里也不 alive` };
}

/**
 * `--selfcheck`：**不需要浏览器、不需要 exe** 的纯逻辑自检。
 *
 * 存在的理由：这两条分支都**只在特定网络条件下才走到**（同一地址换了名字 / 上次那条这轮没量到）。
 * 只靠真机跑，"我修好了"就只能靠嘴说 —— 而本档已经**两次**出现"跑绿了但根本没走到那一行"。
 * 这里把真机那次的**原样输入**喂进同一个纯函数：修复前必红、修复后必须绿。
 *
 * 后 4 条是**反向用例**（必须判红/判绿），防止"为了让那次绿而把判定放松成什么都不查"。
 */
function runSelfCheck() {
  const cases = [
    // ---------------- judgeSticky（沿用判定）
    {
      fn: "judgeSticky",
      name: "名字没变 → 绿（沿用，日志与内核一致）",
      input: {
        status: "same_address_still_there",
        node: "日本-优化2",
        now: "日本-优化2",
        base: "日本-优化2",
        nameChanged: false,
      },
      want: true,
    },
    {
      // 🦷 这一条就是真机上判红的那次输入（[docs/46]：把说明文案当判据那个坑）
      fn: "judgeSticky",
      name: "同一地址换了名字 → 绿（不许再把「说明文案」当判据）",
      input: {
        status: "same_address_still_there",
        node: "日本-优化2",
        now: "日本-优化2",
        base: "新加坡-优化2-GPT",
        nameChanged: true,
      },
      want: true,
    },
    {
      fn: "judgeSticky",
      name: "状态不是「沿用」（应用按规则重挑了）→ 红",
      input: {
        status: "previous_unreachable",
        node: "日本-优化2",
        now: "日本-优化2",
        base: "新加坡-优化2-GPT",
        nameChanged: true,
      },
      want: false,
    },
    {
      fn: "judgeSticky",
      name: "日志与内核不一致（决定没真的写进内核）→ 红",
      input: {
        status: "same_address_still_there",
        node: "日本-优化2",
        now: "香港-优化2",
        base: "日本-优化2",
        nameChanged: false,
      },
      want: false,
    },
    {
      fn: "judgeSticky",
      name: "内核读不到（now=null）→ 红",
      input: {
        status: "same_address_still_there",
        node: "日本-优化2",
        now: null,
        base: "日本-优化2",
        nameChanged: false,
      },
      want: false,
    },
    // ---------------- judgePreviousUnreachable（重挑判定）
    {
      // 🦷 这一条就是把"无法判定"错判成 FAIL 的那次真机输入（[docs/50 §5.4]）
      fn: "judgePreviousUnreachable",
      name: "应用选出了出口 + 挂具按名字看到 alive → undecidable（不许判红）",
      input: { alive: true, node: "新加坡-优化2-GPT", stickyBase: "新加坡-优化2-GPT" },
      wantKind: "undecidable",
    },
    {
      fn: "judgePreviousUnreachable",
      name: "应用说「上次那条不通」却一条都没选 → fail（同层次硬矛盾，必须红）",
      input: { alive: true, node: null, stickyBase: "x" },
      wantKind: "fail",
    },
    {
      fn: "judgePreviousUnreachable",
      name: "应用说「不通」选了「(未选)」→ fail（同上，换个写法）",
      input: { alive: false, node: "(未选)", stickyBase: "x" },
      wantKind: "fail",
    },
    {
      fn: "judgePreviousUnreachable",
      name: "挂具表里也不 alive + 应用重挑成功 → app_right",
      input: { alive: false, node: "日本-优化2", stickyBase: "新加坡-优化2-GPT" },
      wantKind: "app_right",
    },
    // ---------------- pricingWriteEntries（[docs/51] 成本中心只读的判定）
    {
      fn: "pricingWriteEntries",
      name: "只有只读控件 → 空（成本中心确实只做显示）",
      input: [
        "cost-pricing",
        "cost-pricing-readonly",
        "cost-pricing-help",
        "pricing-table",
        "pricing-row",
        "pricing-match-ellipsis-0",
        "pricing-match-ellipsis-1",
      ],
      wantLen: 0,
    },
    {
      // 🦷 这一条防的是"断言写宽了"：`pricing-table` / `pricing-row` 都带 `pricing-` 前缀，
      //    按前缀一刀切会把只读表格也判成写入口（那是假红），而真正的写入口必须抓住。
      fn: "pricingWriteEntries",
      name: "混进「添加定价」的一个控件 → 抓出来（不许被只读控件淹没）",
      input: ["cost-pricing-readonly", "pricing-table", "pricing-row", "pricing-add-open"],
      wantLen: 1,
    },
    {
      fn: "pricingWriteEntries",
      name: "混进「删定价」→ 抓出来",
      input: ["cost-pricing-readonly", "pricing-table", "pricing-delete"],
      wantLen: 1,
    },
    {
      fn: "pricingWriteEntries",
      name: "混进一个**没在清单里**的新写入口（pricing-add-bulk）→ 也要抓出来",
      input: ["pricing-table", "pricing-add-bulk"],
      wantLen: 1,
    },
    {
      fn: "pricingWriteEntries",
      name: "取不到任何 testid（页面没渲染）→ 空（这一档这时没有结论，由调用方按渲染失败处理）",
      input: [],
      wantLen: 0,
    },
    // ---------------- judgePricedMatch（[docs/51] 落库的 match 形状）
    {
      // 🦷 这一条就是真机上先红了一次的那个**真实**形状（`null` 而不是缺键）
      fn: "judgePricedMatch",
      name: "落库写成了显式 null（Option 落库的既有习惯）→ 绿（null 与缺键在引擎里同构）",
      input: { match: { model: "m", provider: null, credential: null }, model: "m" },
      wantOk: true,
    },
    {
      fn: "judgePricedMatch",
      name: "键根本不存在 → 绿",
      input: { match: { model: "m" }, model: "m" },
      wantOk: true,
    },
    {
      fn: "judgePricedMatch",
      name: "带了 provider（就是用户否掉的「对服务商计价」）→ 红",
      input: { match: { model: "m", provider: "p-x" }, model: "m" },
      wantOk: false,
    },
    {
      fn: "judgePricedMatch",
      name: "带了 credential（对线路计价）→ 红",
      input: { match: { model: "m", credential: "c-x" }, model: "m" },
      wantOk: false,
    },
    {
      fn: "judgePricedMatch",
      name: "价挂到了别的模型上 → 红",
      input: { match: { model: "别的模型", provider: null, credential: null }, model: "m" },
      wantOk: false,
    },
    {
      fn: "judgePricedMatch",
      name: "match 压根不是对象 → 红",
      input: { match: "m", model: "m" },
      wantOk: false,
    },
  ];
  const bad = [];
  for (const c of cases) {
    const got =
      c.fn === "judgeSticky"
        ? judgeSticky(c.input).ok
        : c.fn === "pricingWriteEntries"
          ? pricingWriteEntries(c.input).length
          : c.fn === "judgePricedMatch"
            ? judgePricedMatch(c.input.match, c.input.model).ok
            : judgePreviousUnreachable(c.input).kind;
    const ok =
      c.wantKind !== undefined
        ? got === c.wantKind
        : c.wantLen !== undefined
          ? got === c.wantLen
          : c.wantOk !== undefined
            ? got === c.wantOk
            : got === c.want;
    const wantText =
      c.wantKind ??
      (c.wantLen !== undefined
        ? `${c.wantLen} 个写入口`
        : c.wantOk !== undefined
          ? c.wantOk
            ? "绿"
            : "红"
          : c.want
            ? "绿"
            : "红");
    process.stdout.write(`  ${ok ? "✓" : "✗"} [${c.fn}] ${c.name}（期望 ${wantText}，实得 ${got}）\n`);
    if (!ok) bad.push(c.name);
  }
  process.stdout.write(
    bad.length === 0
      ? `\n自检通过：${cases.length}/${cases.length}（step30 的两条判定 + [docs/51] 成本中心只读 / 落库 match 形状）\n`
      : `\n自检失败 ${bad.length}/${cases.length}：${bad.join("；")}\n`,
  );
  return bad.length === 0 ? 0 : 1;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) return printHelp();
  // 纯逻辑自检：不碰浏览器/exe/scratch，所以放在所有前置检查**之前**。
  if (args.selfcheck) return process.exit(runSelfCheck());

  ensureDirs();
  // 每次跑都要一个**干净**的 scratch：否则"落库"可能来自上次的残留数据。
  if (!args.keepData) rmSync(SCRATCH, { recursive: true, force: true });
  mkdirSync(SCRATCH, { recursive: true });

  const exe = resolve(args.exe ?? process.env.TA_E2E_EXE ?? DEFAULT_EXE);
  const tauriDriver = process.env.TA_TAURI_DRIVER ?? DEFAULT_TAURI_DRIVER;
  const msedgedriver = process.env.TA_MSEDGEDRIVER ?? DEFAULT_MSEDGEDRIVER;

  // ---------------------------------------------------------------- 构建标识
  if (!existsSync(exe)) {
    record("build", "FAIL", `被测 exe 不存在：${exe}`);
    return finish(1);
  }
  const exeId = fileId(exe);
  const exeSha = sha256File(exe);
  const buildId = {
    path: exe,
    size: exeId.size,
    mtime: exeId.mtime,
    sha256: exeSha,
    gitHead: gitHead(),
    distMtime: distMtime(),
  };
  writeFileSync(join(OUT, "build-identity.json"), JSON.stringify(buildId, null, 2));
  record(
    "build",
    "PASS",
    "被测构建已定位（结果只对这个构建负责）",
    [
      `exe      : ${exe}`,
      `mtime    : ${buildId.mtime}`,
      `size     : ${buildId.size}`,
      `sha256   : ${exeSha}`,
      `git HEAD : ${buildId.gitHead}`,
      `dist mtime: ${buildId.distMtime}`,
    ],
  );

  if (!existsSync(tauriDriver)) {
    record("harness", "FAIL", `tauri-driver 不存在：${tauriDriver}`);
    return finish(1);
  }
  if (!existsSync(msedgedriver)) {
    record("harness", "FAIL", `msedgedriver 不存在：${msedgedriver}`);
    return finish(1);
  }
  const msedgeVer = spawnSync(msedgedriver, ["--version"], { encoding: "utf8" }).stdout?.trim();
  record("harness", "PASS", "tauri-driver / msedgedriver 就位", [
    `tauri-driver : ${tauriDriver}`,
    `msedgedriver: ${msedgedriver} (${msedgeVer})`,
    `driver port : ${DRIVER_PORT}`,
  ]);

  // ---------------------------------------------------------------- 隔离：跑之前先拍真实目录的快照
  // ---------------------------------------------------------------- 安全闸：被测 exe 必须支持 TA_DATA_DIR
  //
  // 为什么是**启动前**的硬闸而不是等 step0 去发现：覆盖口失效时 App 会**立刻**往
  // `%APPDATA%\TokenAgency` 写 —— 等 step0 断言已经晚了，用户的真实库已经被动过。
  // 所以先在 exe 里找 `TA_DATA_DIR` 这个字符串（它是 `state.rs` 的 `DATA_DIR_ENV` 常量，
  // 一定以 ASCII 原文落在 .rdata 里）。找不到就直接拒绝启动。
  if (!exeHasDataDirOverride(exe)) {
    const msg =
      "被测 exe 里找不到 `TA_DATA_DIR` 覆盖口 —— 启动它就会往用户真实数据目录写。" +
      "已**拒绝启动**（这是保护，不是故障）。请先构建含该覆盖口的 exe：" +
      "`cargo build --release -p tokenagency`（源：src-tauri/src/state.rs 的 DATA_DIR_ENV）。";
    if (!args.forceUnsafe) {
      record("build", "FAIL", msg, [`exe: ${exe}`, `sha256: ${exeSha}`, "如确要用不隔离的方式跑（不推荐）：--force-unsafe"]);
      return finish(1);
    }
    record("build", "FAIL", `--force-unsafe 已指定：跳过隔离闸，App 会写真实数据目录！`, [`exe: ${exe}`]);
  } else {
    record("build", "PASS", "被测 exe 含 TA_DATA_DIR 覆盖口（隔离的前提成立）", [
      `exe: ${exe}`,
      `TA_DATA_DIR 常量在二进制中命中`,
    ]);
  }

  const realBefore = snapshotDir(REAL_DATA_DIR, { hash: ["db.sqlite"] });
  const realBeforeDirStat = existsSync(REAL_DATA_DIR) ? fileId(REAL_DATA_DIR) : null;
  writeFileSync(join(OUT, "real-data-before.json"), JSON.stringify(realBefore, null, 2));

  // ---------------------------------------------------------------- 安全闸 2：不能有别的实例在跑
  //
  // `tauri-plugin-single-instance` 是**全局**的（按 app identifier 抢锁，与 TA_DATA_DIR 无关）：
  // 只要机器上已经有一个 TokenAgency 在跑，我们启动的"被测实例"会**立刻 exit(0)**
  // 并去唤起已有窗口 —— 表现为 msedgedriver 报
  // `session not created: Chrome instance exited`，而且 scratch 里只有一个 0 字节的日志。
  // 这不是产品缺陷，是**环境前提**。这里把它变成一句人话，别让人对着 Chrome 的报错猜。
  const running = listRunningAppInstances();
  if (running.length > 0 && !args.forceUnsafe) {
    record(
      "harness",
      "FAIL",
      "机器上已有 TokenAgency 实例在跑：single-instance 插件会让被测实例立刻退出，" +
        "WebDriver 会话建不起来。先结束这些进程再跑（脚本**不会**替你杀进程）。",
      [
        ...running.map((p) => `PID ${p.pid}  ${p.path}`),
        "依据：src-tauri/src/plugins.rs 把 tauri-plugin-single-instance 注册为第一个插件；",
        "对照证据：真实数据目录日志里出现过 `检测到第二个实例：唤起已运行的主窗口`。",
      ],
    );
    return finish(1);
  }

  // ---------------------------------------------------------------- fixture + tauri-driver
  const fixture = await startFixture();
  log(`本地订阅 fixture: ${fixture.url}`);

  const driverOut = openSync(join(LOG_DIR, "tauri-driver.out.log"), "a");
  const driverErr = openSync(join(LOG_DIR, "tauri-driver.err.log"), "a");
  const driverEnv = {
    ...process.env,
    // ★ 隔离的唯一开关：App 是 tauri-driver 的子进程，直接继承这个环境。
    TA_DATA_DIR: SCRATCH,
    // 不发真实代理请求、不去动用户内核；数据根已隔离，这里只是不给出内核路径。
    TA_E2E: "1",
  };
  delete driverEnv.TA_MIHOMO; // 明确不指内核：本次不启动真内核

  log(`启动 tauri-driver（TA_DATA_DIR=${SCRATCH}）...`);
  const driver = spawn(
    tauriDriver,
    ["--port", String(DRIVER_PORT), "--native-driver", msedgedriver],
    { env: driverEnv, stdio: ["ignore", driverOut, driverErr], windowsHide: false },
  );
  driver.on("exit", (code, sig) => log(`tauri-driver 退出 code=${code} sig=${sig}`));

  const wd = new WebDriver(WD_BASE);
  let sessionOk = false;
  const cleanup = async () => {
    await wd.deleteSession();
    fixture.server.close();
    if (!driver.killed) driver.kill();
  };

  try {
    // ---------------------------------------------------------------- 等 driver 起来
    const ready = await waitHttp(`${WD_BASE}/status`, 30000);
    if (!ready) {
      record("harness", "FAIL", `tauri-driver 30s 内没在 ${WD_BASE}/status 起监听（见 logs/tauri-driver.*.log）`);
      await cleanup();
      return finish(1);
    }

    log("新建 WebDriver 会话（启动真实 Tauri 窗口）...");
    // [docs/31 §27] 让应用以**窗口模式**启动：托盘模式下应用启动后会自行退出
  // （真机实测：进程"已退出" → WebView 连接关闭 → WebDriver 会话被服务器删除 →
  //  之后所有查询 404、页面文本为空）。这不是产品缺陷，是**用例该跑在窗口模式下**。
  await wd.newSession(exe, { args: ["--autostart-mode=window"] });
    sessionOk = true;
    log(`会话 id = ${wd.sessionId}`);

    // 让窗口正常大小、可见（截图才有内容）
    await wd.raw("POST", `/session/${wd.sessionId}/window/rect`, { width: 1440, height: 960 }).catch(() => {});

    const want = (s) => args.only === undefined || args.only.has(s);

    // ============================================================ step0 隔离
    if (want("step0")) await step0(wd, realBefore, realBeforeDirStat);

    // ============================================================ step1 窗口 + 首帧
    if (want("step1")) await step1(wd);

    // ============================================================ step12 网络未配置 → 不显示（必须在建订阅之前跑）
    if (want("step12")) await step12(wd);

    // ============================================================ step2 供应商
    if (want("step2")) await step2(wd, fixture);

    // ============================================================ step3 订阅
    if (want("step3")) await step3(wd, fixture);

    // ============================================================ step4 通道/线路
    if (want("step4")) await step4(wd);

    // ============================================================ step5 设置页
    if (want("step5")) await step5(wd);

    // ============================================================ step6 真写落库
    if (want("step6")) await step6(wd);

    // ============================================================ 第 61 轮：四个添加向导（服务商/模型/线路/路由）
    if (want("step8")) await step8(wd);
    if (want("step9")) await step9(wd);
    if (want("step10")) await step10(wd, fixture);
    if (want("step11")) await step11(wd, fixture);

    // ============================================================ step13 订阅流量与更新时间可见（需要先有订阅）
    if (want("step13")) await step13(wd);

    // ============================================================ step14 数据安全回归（设置页应用后线路必须还在）
    if (want("step14")) await step14(wd);

    // ============================================================ step15 能力规则的强制面
    if (want("step15")) await step15(wd, fixture);

    // ============================================================ step17 节点清单（需要先有订阅 + 解析）
    if (want("step17")) await step17(wd);

    // ============================================================ step18 访问密钥（会自己造 key 并真打网关）
    if (want("step18")) await step18(wd);

    // ============================================================ step20 服务商详情即编辑器（[docs/23]）
    if (want("step20")) await step20(wd);

    // ============================================================ step24 模型目录（[docs/25]）
    if (want("step24")) await step24(wd);

    // ============================================================ step25 线路的网络代理（[docs/28 §1.2]）
    if (want("step25")) await step25(wd);
  if (want("step26")) await step26(wd, fixture);
  // [docs/46 §297] **顺序有语义**：step26–step29 依赖 step25 建好的状态
  // （服务商挂上「自动」线路 + 订阅刷新链路），而 step21 是**破坏性**的（删服务商/通道/模型/
  // policy）—— 它曾经排在 step27 前面，于是 step28/step29 跑到的时候"服务商已经没有 lane 了"
  // ⇒ 应用不再重算（日志停在上一轮）、step29 读到的是**旧判决行** ⇒ 判红的是"状态被前面的步骤
  // 拆了"，不是产品。所以 E 期的四步必须**紧跟 step25**，在破坏性步骤之前。
  if (want("step27")) await step27(wd, fixture);
  // [docs/46 E 期] 先让订阅/线路都建好，再点手动刷新看「自动」出口选谁。
  if (want("step28")) await step28(wd, fixture);
  // [docs/46 §E] 两段式（冷启动）拿"选最低延迟"的数值证据。
  if (want("step29")) await step29(wd, fixture, exe);
  // [docs/46 §299] 粘性：后台自动刷新（manual=false）必须**沿用**上次那条（出口 IP 不变）。
  if (want("step30")) await step30(wd, fixture, exe);

    // ============================================================ step21 删除与通道页线路（跑在最后）
    if (want("step21")) await step21(wd);

    // ============================================================ step22 问号圆圈（[docs/22]）
    if (want("step22")) await step22(wd);

    // ============================================================ step23 订阅详情页（[docs/54]）
    // 放在最后：它会开关一个节点再复原，前面的档不该被它影响（复原失败会红，不会静默改坏别人）
    if (want("step23")) await step23(wd);

    // ============================================================ 收尾：再查一次隔离
    await stepIsolationAfter(wd);
  } catch (e) {
    record("harness", "FAIL", `未捕获异常：${e.message}`, [String(e.stack).split("\n").slice(0, 6).join("\n")]);
    await shot(wd, "zz-crash").catch(() => {});
  } finally {
    await cleanup();
  }

  if (!sessionOk) record("harness", "FAIL", "WebDriver 会话从未建立");

  const fails = results.filter((r) => r.status === "FAIL").length;
  return finish(fails === 0 ? 0 : 1);
}

// ================================================================ 步骤实现

async function waitHttp(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const r = await fetch(url);
      if (r.status < 500) return true;
    } catch {
      /* 还没起来 */
    }
    if (Date.now() > deadline) return false;
    await sleep(300);
  }
}

async function shot(wd, name) {
  if (!wd.sessionId) return undefined;
  try {
    const p = join(SHOTS, `${name}.png`);
    await wd.screenshot(p);
    log(`  截图 ${p}`);
    return p;
  } catch (e) {
    log(`  截图失败 ${name}: ${e.message}`);
    return undefined;
  }
}

/** 导航到 hash 路由并等页面骨架渲染完。**只改 hash，不做任何写操作。** */
async function gotoPage(wd, hash, waitTestId) {
  await wd.navHash(hash);
  const ok = await wd.waitFor(waitTestId ?? '[data-testid="page-header"]', 15000);
  if (ok === null) return null;
  // 首帧之后数据还在飞：等一小会儿让 IPC 回来（不做假等待的断言，只是给客户端一点时间）
  await sleep(700);
  return ok;
}

// ---------------------------------------------------------------- step0

async function step0(wd, realBefore, realBeforeDirStat) {
  const dbPath = join(SCRATCH, "db.sqlite");
  const deadline = Date.now() + 30000;
  while (!existsSync(dbPath) && Date.now() < deadline) await sleep(400);

  const logsDir = join(SCRATCH, "logs");
  const logs = existsSync(logsDir) ? readdirSync(logsDir) : [];
  const evidence = [
    `TA_DATA_DIR   = ${SCRATCH}`,
    `scratch db    = ${existsSync(dbPath) ? `存在 size=${statSync(dbPath).size}` : "不存在"}`,
    `scratch logs  = ${logs.length > 0 ? logs.join(", ") : "（空）"}`,
    `真实目录      = ${REAL_DATA_DIR}`,
    `真实目录 mtime 变化: ${realBeforeDirStat ? `${realBeforeDirStat.mtime} → ${existsSync(REAL_DATA_DIR) ? fileId(REAL_DATA_DIR).mtime : "已消失"}` : "（跑之前不存在）"}`,
  ];

  if (!existsSync(dbPath)) {
    record("step0", "FAIL", "TA_DATA_DIR 隔离未生效：scratch 里没有 db.sqlite（App 大概把数据写到别处了）", evidence);
    return;
  }

  const after = snapshotDir(REAL_DATA_DIR, { hash: ["db.sqlite"] });
  writeFileSync(join(OUT, "real-data-after-early.json"), JSON.stringify(after, null, 2));
  const changed = diffSnapshots(realBefore, after);
  evidence.push(`真实目录文件级 diff: ${changed.length === 0 ? "无变化（全部文件 size/mtime/sha256 一致）" : changed.join(" | ")}`);

  if (changed.length > 0) {
    record("step0", "FAIL", "隔离被突破：用户真实数据目录被改动", evidence);
    return;
  }
  record("step0", "PASS", "隔离生效：db.sqlite 落在 scratch，真实目录文件级无变化", evidence);
}

// ---------------------------------------------------------------- step1

async function step1(wd) {
  const header = await wd.waitFor('[data-testid="page-header"]', 25000);
  const evidence = [];
  if (header === null) {
    evidence.push("等 25s 未见 [data-testid=\"page-header\"]");
    evidence.push(`页面文本前 400 字: ${(await wd.pageText().catch(() => "")).slice(0, 400)}`);
      // [docs/31 §18] **协议层现场**：页面 JS 执行不了时（`wd.exec` 返回 null），
      // 就用驱动器自己的接口看"附着到的是哪个窗口/哪个 URL"——它们不依赖页面上下文。
      {
        const handles = await wd.raw("GET", `/session/${wd.sessionId}/window/handles`).catch((e) => ({ err: String(e) }));
        const url = await wd.raw("GET", `/session/${wd.sessionId}/url`).catch((e) => ({ err: String(e) }));
        const title = await wd.raw("GET", `/session/${wd.sessionId}/title`).catch(() => null);
        evidence.push(
          `协议层：${JSON.stringify({ handles, url, title })?.slice(0, 260)} sessionId=${wd.sessionId}`,
        );
        const status = await wd.raw("GET", "/status").catch((e) => ({ err: String(e) }));
        evidence.push(`驱动器 /status = ${JSON.stringify(status)?.slice(0, 200)}`);
        // [docs/31 §23] 应用进程是否还活着：失败到底是"应用退出"还是"WebView 崩"
        {
          const { execSync } = await import("node:child_process");
          let list = "";
          try {
            list = execSync(`tasklist /FI "IMAGENAME eq tokenagency.exe"`, { encoding: "utf8" });
          } catch (e) {
            list = String(e);
          }
          evidence.push(`应用进程：${list.includes("tokenagency.exe") ? "活着" : "已退出"}`);
        }
      }
    await shot(wd, "step1-fail");
    // ★ 诊断（**不改判定**）：显式再导航一次，区分两种根因 ——
    //   「页面本身坏」（再导航也出不来） vs 「启动时初始路由没渲染」（再导航立刻出来）。
    //   首页是**懒加载路由**，所以这次导航期间的脚本错误能被下面这个收集器抓到
    //   （若是 chunk 加载失败，会看到 "Failed to fetch dynamically imported module"）。
    //   只写进证据，不把 FAIL 变成 PASS。
    await wd
      .exec(`
      window.__taErrors = [];
      window.addEventListener("error", (e) => {
        window.__taErrors.push("error: " + (e && (e.message || e.type)) + (e && e.filename ? " @" + e.filename : ""));
      });
      window.addEventListener("unhandledrejection", (e) => {
        window.__taErrors.push("reject: " + String((e && e.reason && (e.reason.message || e.reason)) || e));
      });
      return true;
    `)
      .catch(() => {});
    await wd.exec('window.location.hash = "#/dashboard"; return true;').catch(() => {});
    const retryFirstScreen = await wd.waitFor('[data-testid="page-header"]', 10000);
    evidence.push(
      retryFirstScreen === null
        ? "诊断：显式再导航 #/dashboard 后 10s 仍未渲染 → 指向「页面本身有问题」"
        : "诊断：★ 显式再导航 #/dashboard 后立刻渲染出来 → 指向「启动时初始路由没渲染」，不是页面坏",
    );
    await shot(wd, "step1-after-renav");
    // ★ 更硬的现场证据：完整正文、testid 数量、有没有错误边界/加载失败的痕迹、
    //   以及 WebView 里能拿到的脚本错误（app 自己收集的，如果它收集了）。
    const diag = await wd
      .exec(`
      const body = document.body ? document.body.innerText : "";
      const root = document.querySelector("#root") || document.body;
      const ids = [...document.querySelectorAll("[data-testid]")].map((e) => e.getAttribute("data-testid"));
      return {
        hash: window.location.hash,
        bodyLen: body.length,
        bodyHead: body.slice(0, 900),
        rootHtmlLen: root ? root.innerHTML.length : -1,
        testidCount: ids.length,
        testids: ids.slice(0, 30),
        errorBoundary: /出错了|Something broke|error\\.title|重试/.test(body),
        collectingLoader: /加载|Loading|\\.\\.\\./.test(body.slice(0, 200)),
        collectedErrors: (window.__taErrors || []).slice(0, 8),
      };
    `)
      .catch(() => null);
    evidence.push(`现场诊断: ${JSON.stringify(diag)}`);
    record("step1", "FAIL", "窗口起来了但首页没渲染出 page-header", evidence);
    return;
  }
  const title = await wd.text(header);
  const tids = await wd.probeTestIds();
  evidence.push(`selector [data-testid="page-header"] 命中，文本: ${JSON.stringify(title.slice(0, 120))}`);
  evidence.push(`当前 hash: ${await wd.exec("return window.location.hash;")}`);
  evidence.push(`页面上 data-testid 共 ${tids.length} 个: ${tids.slice(0, 25).join(", ")}${tids.length > 25 ? " …" : ""}`);
  const p = await shot(wd, "step1-home");
  if (p) evidence.push(`截图: ${p}`);

  /**
   * [docs/56] **侧边栏的顺序与分组**（用户第 323 轮："重新排序一下…从用户体验工程和直觉上来说"）。
   *
   * 这一档在**真机**上验三件事（单测验的是纯函数与 jsdom 渲染，这里验的是真的 WebView）：
   * 1. 三个组名真的画出来了；
   * 2. 导航项的**先后顺序**是 概览 → 配置四项 → 记录两项 → 设置；
   * 3. **折叠后组名消失、但 8 个导航项一个都不少**（`Sidebar.tsx` 里那句
   *    "折叠态不画组名"的可执行版本），然后**折回来**（不给后面的档留一个折叠态）。
   */
  const sidebar = await wd.exec(`
    const nav = document.querySelector("nav");
    if (!nav) return null;
    const headings = [...nav.querySelectorAll('[data-testid="sidebar-group-heading"]')].map((e) => (e.textContent || "").trim());
    const items = [...nav.querySelectorAll(".MuiListItemButton-root")].map((e) => (e.textContent || "").trim());
    return { headings, items };
  `);
  evidence.push(`侧边栏：组名=${JSON.stringify(sidebar?.headings ?? null)}`);
  evidence.push(`侧边栏：导航项（按 DOM 顺序）=${JSON.stringify(sidebar?.items ?? null)}`);

  const sidebarHeadings = Array.isArray(sidebar?.headings) ? sidebar.headings : [];
  const sidebarItems = Array.isArray(sidebar?.items) ? sidebar.items : [];
  const expectedHeadings = ["配置", "记录", "系统"];
  // 顺序判据用**下标递增**：不写死每一项的完整文案（文案改字不该让这一档红）
  const orderOk =
    sidebarItems.length === 8 &&
    sidebarItems[0]?.includes("概览") === true &&
    ["服务商", "模型目录", "网络", "访问密钥"].every((label, i) =>
      (sidebarItems[i + 1] ?? "").includes(label),
    ) &&
    ["请求日志", "成本中心"].every((label, i) => (sidebarItems[i + 5] ?? "").includes(label)) &&
    (sidebarItems[7] ?? "").includes("设置");
  const headingsOk =
    sidebarHeadings.length === 3 && expectedHeadings.every((h, i) => sidebarHeadings[i] === h);

  // 折叠：组名消失、项数不变；再折回来
  let collapseOk = false;
  if (tids.includes("sidebar-collapse-toggle")) {
    await wdClickTid(wd, "sidebar-collapse-toggle", evidence);
    await sleep(600);
    const collapsed = await wd.exec(`
      const nav = document.querySelector("nav");
      if (!nav) return null;
      return {
        headings: nav.querySelectorAll('[data-testid="sidebar-group-heading"]').length,
        items: nav.querySelectorAll(".MuiListItemButton-root").length,
      };
    `);
    await shot(wd, "step1-sidebar-collapsed");
    await wdClickTid(wd, "sidebar-collapse-toggle", evidence);
    await sleep(600);
    const restored = await wd.exec(
      `return document.querySelectorAll('[data-testid="sidebar-group-heading"]').length;`,
    );
    collapseOk = collapsed !== null && collapsed.headings === 0 && collapsed.items === 8 && restored === 3;
    evidence.push(
      `折叠：组名=${collapsed?.headings}（应 0）项数=${collapsed?.items}（应 8）；折回后组名=${restored}（应 3）`,
    );
  } else {
    evidence.push("找不到 sidebar-collapse-toggle（这一档的折叠部分未覆盖）");
  }

  if (!headingsOk || !orderOk || !collapseOk) {
    record(
      "step1",
      "FAIL",
      `首页渲染出来了，但**侧边栏**不符合 [docs/56]：组名=${headingsOk} 顺序=${orderOk} 折叠=${collapseOk}`,
      evidence,
    );
    return;
  }
  record(
    "step1",
    "PASS",
    "真实 Tauri 窗口渲染出首页（page-header 命中）；侧边栏按 [docs/56] 分组且顺序正确，" +
      "折叠后组名消失而 8 个导航项仍在",
    evidence,
  );
}

// ---------------------------------------------------------------- 通用：找"新增"入口

/**
 * 在当前页面找"新增/添加"入口。**两级策略**：
 *   1. data-testid 命中 /(^|-)add|new|create|upsert/i 的可见控件；
 *   2. 可见 button / [role=button] 的文案命中 /新增|添加|新建|创建|Add|New|Create/i。
 * 返回 { el, how } 或 null。null 时调用方必须 SKIP，并打印探测到的全部控件。
 */
async function findAddControl(wd) {
  const byTestId = await wd.exec(`
    const rx = /(^|[-_])(add|new|create|upsert)([-_]|$)/i;
    for (const el of document.querySelectorAll('[data-testid]')) {
      const t = el.getAttribute('data-testid') || '';
      const r = el.getBoundingClientRect();
      if (rx.test(t) && r.width > 0 && r.height > 0) {
        const clickable = el.closest('button,[role="button"],a') || el;
        clickable.setAttribute('data-ta-e2e-add', '1');
        return t;
      }
    }
    return null;
  `);
  if (byTestId !== null) {
    const el = await wd.findElement("[data-ta-e2e-add]");
    if (el) return { el, how: `data-testid /${/(^|[-_])(add|new|create|upsert)([-_]|$)/i.source}/ → ${byTestId}` };
  }
  const byText = await wd.exec(`
    const rx = /(新增|添加|新建|创建|\\bAdd\\b|\\bNew\\b|\\bCreate\\b)/;
    for (const el of document.querySelectorAll('button,[role="button"],a')) {
      const r = el.getBoundingClientRect();
      const txt = (el.innerText || '').trim();
      if (r.width > 0 && r.height > 0 && rx.test(txt)) {
        el.setAttribute('data-ta-e2e-add', '1');
        return txt;
      }
    }
    return null;
  `);
  if (byText !== null) {
    const el = await wd.findElement("[data-ta-e2e-add]");
    if (el) return { el, how: `按钮文案 → ${JSON.stringify(byText)}` };
  }
  return null;
}

/** 把"页面上的可交互控件"整理成可读证据行。 */
async function controlEvidence(wd, limit = 18) {
  const controls = await wd.probeControls();
  return controls
    .filter((c) => c.visible)
    .slice(0, limit)
    .map((c) => `<${c.tag}${c.type ? ` type=${c.type}` : ""}${c.testid ? ` testid=${c.testid}` : ""}> ${JSON.stringify(c.text)}`);
}

// ---------------------------------------------------------------- step2 供应商

async function step2(wd, fixture) {
  const page = await gotoPage(wd, "#/providers");
  const evidence = [];
  if (page === null) {
    await shot(wd, "step2-no-page");
    evidence.push('等 15s 未见 [data-testid="page-header"]（/providers 没渲染）');
    record("step2", "FAIL", "/providers 页面没渲染出来", evidence);
    return;
  }
  const tids = await wd.probeTestIds();
  const bodyText = await wd.pageText();
  evidence.push(`路由: #/providers；命中 data-testid: ${tids.join(", ")}`);
  evidence.push(`列表状态: ${tids.includes("providers-empty") ? "providers-empty（后端返回 0 条 → 真 IPC 通了）" : tids.includes("providers-table") ? "providers-table 有行" : "既无空态也无表格"}`);
  evidence.push(`providers-error 是否存在: ${tids.includes("providers-error") ? await wd.text(await wd.findElement('[data-testid="providers-error"]')) : "否"}`);
  // 用户硬要求①：这一页必须给出**两个各司其职**的添加入口 —— 添加服务商 / 添加密钥。
  // 第 61 轮之前这里用 `findAddControl` 抓"任意一个 add 控件"，结果抓到 `credentials-add-open`，
  // 拿密钥表单去填服务商字段 → 失败原因与用例名（供应商页新增供应商）根本不是一回事。
  const hasProviderEntry = tids.includes("providers-add-open");
  const hasCredentialEntry = tids.includes("credentials-add-open");
  evidence.push(`添加入口: providers-add-open=${hasProviderEntry} credentials-add-open=${hasCredentialEntry}`);

  if (!hasProviderEntry) {
    // 真实缺陷：/providers 没有任何"添加服务商"入口。如实记录，并走界面里**真实存在**的另一条
    // 路径（首启向导第 1 步）把"这个操作能不能在软件里完成"这件事测完。
    evidence.push("→ /providers 没有新增服务商入口（真实缺陷，已记录）；改走界面里真实存在的另一条路径：首启向导 /onboarding 第 1 步");
    evidence.push(...(await controlEvidence(wd)));
    evidence.push(`页面文本片段: ${JSON.stringify(bodyText.replace(/\s+/g, " ").slice(0, 300))}`);
    await shot(wd, "step2-providers-no-entry");
    const wiz = await ensureWizardRun(wd, fixture, evidence);
    if (!wiz.ok) {
      record("step2", "SKIP", `界面缺入口：/providers 没有新增控件，且首启向导也不可用（${wiz.reason}）`, evidence);
      return;
    }
    await verifyProviderLanded(wd, evidence, wiz.payload);
    return;
  }

  // ---- ① 添加服务商：走 /providers 页头的**两步向导**（[docs/19 §1]）----
  // 第 71 轮用户要的就是这个：显示名称(不重复) → 协议 → URL → key → 测试连接(非必填) → 下一步 → 添加模型。
  // 挂具只固定**显示名称**：provider id 由界面从名称派生（见下面的真实 id 读取），不写 id 常量。
  const pname = "TA E2E 供应商";
  if (!(await wdClickTid(wd, "providers-add-open", evidence))) {
    record("step2", "FAIL", "点不开「添加服务商」", evidence);
    return;
  }
  await shot(wd, "step2-wizard-step1");
  const step1 = await wd.probeTestIds();
  const wantStep1 = ["provider-wizard-step1", "pw-name", "pw-protocol", "pw-baseUrl", "pw-key", "pw-test", "pw-proxy", "pw-next"];
  evidence.push(`步骤 1 缺的 testid: ${JSON.stringify(wantStep1.filter((t) => !step1.includes(t)))}`);
  await wdFillByTid(wd, "pw-name", pname, evidence);
  // 协议保留默认（openai，与 fixture 一致）
  await wdFillByTid(wd, "pw-baseUrl", fixture.baseUrl, evidence);
  await wdFillByTid(wd, "pw-key", "sk-ta-e2e-secret", evidence);
  await shot(wd, "step2-wizard-step1-filled");

  // 「测试连接」：真调 provider_test，成功要显示真实延迟（非必填，但按钮必须真做事）
  await wdClickTid(wd, "pw-test", evidence);
  await sleep(2500);
  const testResult = await wd.textOf('[data-testid="pw-test-result"]');
  const testText = (testResult ?? "").replace(/\s+/g, " ").trim();
  evidence.push(`pw-test-result: ${JSON.stringify(testText)}`);
  const testShowsLatency = /ms|毫秒/.test(testText) && !/TA-\d/.test(testText);

  // 「上游没有 /models 端点」那一档**不在这里跑**：它自己会开一轮新向导并把它关掉，
  // 插在这个位置会把主流程紧接着要点的 `pw-next` 一起关没（详见方法末尾的说明）。
  // 它改到主流程全部结束之后执行。

  // 下一步 → 真落库（provider + credential + secret）
  await wdClickTid(wd, "pw-next", evidence);
  await sleep(2000);
  await shot(wd, "step2-wizard-step2");
  const step2Tids = await wd.probeTestIds();
  // 诊断证据（**只记录，不改判定**）：如果是"点了下一步却还停在第 1 步"，就把第 1 步的本地校验
  // 原文抓下来 —— 那才是"为什么没进第 2 步"的现场，而不是让人对着"缺 pw-scan"猜。
  if (!step2Tids.includes("provider-wizard-step2")) {
    evidence.push(
      `点了 pw-next 之后没有进第 2 步（provider-wizard-step2 不存在）；` +
        `页面上仍是第 1 步的控件: pw-name=${step2Tids.includes("pw-name")} pw-next=${step2Tids.includes("pw-next")}`,
    );
    const issuesAfterNext = await wd.textOf('[data-testid="pw-step1-issues"]');
    evidence.push(`pw-next 之后 pw-step1-issues 原文: ${JSON.stringify((issuesAfterNext ?? "").replace(/\s+/g, " ").trim())}`);
    const saveErrAfterNext = await wd.textOf('[data-testid="pw-save-error"]');
    evidence.push(`pw-next 之后 pw-save-error: ${JSON.stringify(saveErrAfterNext)}`);
  }
  const notice = await wd.textOf('[data-testid="pw-saved-notice"]');
  evidence.push(`pw-saved-notice: ${JSON.stringify((notice ?? "").replace(/\s+/g, " ").trim())}`);
  evidence.push(`步骤 2 命中: ${step2Tids.filter((t) => t.startsWith("pw-")).join(", ")}`);
  const dumpAfterNext = dumpScratchDb("step2-step2");
  evidence.push(`db(config) kind 计数: ${JSON.stringify(configEntityKinds(dumpAfterNext))}`);
  // 判"服务商真的建出来了"：**按显示名称**去 config(kind=provider) 里找，并把真实 id 读出来。
  // 不再断言手写的 id 常量 —— 向导的 provider.id 是从名称派生的（providerWizardModel.deriveProviderId），
  // 名称 "TA E2E 供应商" 派生出的 id 实测是 `ta-e2e`（证据：同一次 run 里凭据回显 `· ta-e2e ·`）。
  const providerRows = configEntities(dumpAfterNext, "provider");
  const providerRow = providerRows.find((r) => String(r.json.name ?? "").trim() === pname) ?? null;
  const realProviderId = providerRow === null ? null : String(providerRow.json.id ?? providerRow.entityId);
  const providerInDb = providerRow !== null;
  evidence.push(
    `步骤 1 落库: config(kind=provider) 共 ${providerRows.length} 行；按名称 ${JSON.stringify(pname)} 命中=${providerInDb}` +
      `；读出的真实 id=${JSON.stringify(realProviderId)}（entity_id=${JSON.stringify(providerRow?.entityId ?? null)}）` +
      `；库里全部 provider id=${JSON.stringify(providerRows.map((r) => r.entityId))}`,
  );

  // 第 2 步（[docs/28 §1.4] 用户第 82 轮的重构）：
  //   扫描 = 记录目录 + **把清单里的每条线路都建出来（默认关闭）**；
  //   列表里每行 = 上游原文 + 网络代理（可改）+ 启用滑块（默认关）；
  //   全选 = 一条 `channel_bulk_set_enabled`。
  let scanRows = 0;
  let addedModels = false; // [docs/28 §2] 语义已改为"真的建出了线路"
  let addedRowsVisible = 0;
  let addedRowsOk = false;
  let addedRoutedOk = false;
  let allOffOk = false;
  let selectAllOk = false;
  if (step2Tids.includes("pw-scan")) {
    await wdClickTid(wd, "pw-scan", evidence);
    await sleep(3000);
    await shot(wd, "step2-wizard-scanned");
    const scanEmpty = await wd.textOf('[data-testid="pw-scan-empty"]');
    const scanError = await wd.textOf('[data-testid="pw-scan-error"]');
    const scanNotice = await wd.textOf('[data-testid="providers-model-notice"]');
    evidence.push(
      `扫描后: scan-empty=${JSON.stringify(scanEmpty)}；scan-error=${JSON.stringify(scanError)}；` +
        `notice=${JSON.stringify((scanNotice ?? "").replace(/\s+/g, " ").slice(0, 120))}`,
    );

    // ★ 一个列表：行来自 `channel_list`。**[docs/28 §1.5] 一行 = 名字 + 网络代理 + 滑块**
    //   （用户原话："一条就是左边名字，右边选择滑块就好了"）——行里**不该**有 id / 优先级 / 原文。
    const listProbe2 = await wd.exec(`
      const rows = [...document.querySelectorAll('[data-testid="providers-line-row"]')];
      return {
        rows: rows.length,
        lanes: rows.map((r) => {
          const lane = r.querySelector('[data-testid="providers-line-lane"]');
          const sw = r.querySelector('[data-testid="providers-line-enabled"] input');
          const info = r.querySelector('[data-testid="providers-line-info"]');
          return {
            name: (r.querySelector('[data-testid="providers-line-upstream"]')?.textContent || '').trim(),
            hasLane: lane !== null,
            laneWritable: lane !== null && lane.querySelector('[role="combobox"], input') !== null,
            hasSwitch: sw !== null,
            checked: sw ? sw.checked === true : null,
            hasInfo: info !== null,
            // 列表里**不该**出现的详情
            inRowDetails: r.querySelectorAll('[data-testid="providers-line-id"], [data-testid="providers-line-priority"], [data-testid="providers-line-raw-json"]').length,
          };
        }),
        hasSelectAll: document.querySelector('[data-testid="providers-lines-all"]') !== null,
        allCount: (document.querySelector('[data-testid="providers-lines-all-count"]')?.textContent || '').trim(),
        legacyCheck: document.querySelectorAll('[data-testid="pw-scan-check"], [data-testid="providers-model-check"]').length,
        legacyAdd: document.querySelector('[data-testid="pw-scan-add"]') !== null,
      };
    `);
    // [docs/28 §1.7] **长列表里的主操作不能跟着滚走**：把向导内容滚到底，
    // 「完成」必须仍然在视口内（它在对话框的固定按钮区，不在滚动容器里）。
    const stickyProbe = await wd.exec(`
      const scroll = document.querySelector('[data-testid="pw-step2-scroll"]');
      if (scroll) scroll.scrollTop = scroll.scrollHeight;
      const btn = document.querySelector('[data-testid="pw-finish"]');
      if (!btn) return { button: false };
      const r = btn.getBoundingClientRect();
      return {
        button: true,
        inViewport: r.top >= 0 && r.bottom <= window.innerHeight,
        top: Math.round(r.top),
        inScrollContainer: scroll ? scroll.contains(btn) : null,
      };
    `);
    evidence.push(`滚到底后「完成」按钮：${JSON.stringify(stickyProbe)}`);
    scanRows = listProbe2?.rows ?? 0;
    addedRowsVisible = scanRows;
    evidence.push(`向导里的线路列表: ${JSON.stringify(listProbe2)}`);
    // 「点一下详情」这条路还在：点开第一行的详情 → 上游原文出现在面板里
    const infoProbe = await wd.exec(`
      const row = document.querySelector('[data-testid="providers-line-row"]');
      const btn = row ? row.querySelector('[data-testid="providers-line-info"]') : null;
      if (!btn) return { opened: false };
      btn.click();
      return { opened: true };
    `);
    await sleep(600);
    const panelProbe = await wd.exec(`
      const panel = document.querySelector('[data-testid="providers-line-info-panel"]');
      return {
        panel: panel !== null,
        raw: panel ? (panel.querySelector('[data-testid="providers-line-raw-json"], [data-testid="providers-line-raw-empty"]')?.getAttribute('data-testid') ?? null) : null,
        id: panel ? panel.querySelector('[data-testid="providers-line-id"]') !== null : false,
      };
    `);
    evidence.push(`点开详情: ${JSON.stringify(infoProbe)} · 面板: ${JSON.stringify(panelProbe)}`);
    addedRowsOk =
      scanRows > 0 &&
      (listProbe2?.lanes ?? []).every(
        (l) =>
          // [docs/29 §2.1] 行里**没有**代理控件（出口只由服务商决定）
          l.hasLane === false &&
          l.hasSwitch === true &&
          l.hasInfo === true &&
          l.inRowDetails === 0 &&
          typeof l.name === "string" &&
          l.name.length > 0,
      ) &&
      listProbe2?.hasSelectAll === true &&
      panelProbe?.panel === true &&
      panelProbe?.raw !== null &&
      // [docs/28 §1.7] 滚到底之后主操作仍**在视口内**，且**不在**滚动容器里
      stickyProbe?.button === true &&
      stickyProbe?.inViewport === true &&
      stickyProbe?.inScrollContainer === false &&
      // 旧的"勾一下才创建"控件一个都不许再出现
      listProbe2?.legacyCheck === 0 &&
      listProbe2?.legacyAdd === false;

    const dumpLines = dumpScratchDb("step2-lines");
    const channelIds = configIds(dumpLines, "channel");
    const lineRows = configEntities(dumpLines, "channel").filter((c) =>
      c.entityId.startsWith(`ch-${realProviderId}`),
    );
    addedModels = channelIds.some((id) => String(id).startsWith("ch-"));
    // **默认关闭**：扫描建出来的线路必须是 enabled=false（用户原话"默认关闭"）
    allOffOk = lineRows.length > 0 && lineRows.every((c) => c.json?.enabled === false);
    evidence.push(
      `扫描后库里的线路: ${JSON.stringify(lineRows.map((c) => ({ id: c.entityId, enabled: c.json?.enabled })))}` +
        `（默认关闭=${allOffOk}）`,
    );

    // 全选 → 一条批量命令把列表里的线路一起启用（真机验证滑块那套逻辑）
    if (listProbe2?.hasSelectAll === true && scanRows > 0) {
      await wdClickTid(wd, "providers-lines-all", evidence).catch(() => {});
      await sleep(2500);
      const dumpAfterAll = dumpScratchDb("step2-lines-all");
      const enabledAfter = configEntities(dumpAfterAll, "channel")
        .filter((c) => c.entityId.startsWith(`ch-${realProviderId}`))
        .map((c) => c.json?.enabled === true);
      selectAllOk = enabledAfter.length > 0 && enabledAfter.every((v) => v === true);
      evidence.push(`全选后库里线路 enabled: ${JSON.stringify(enabledAfter)}（全开=${selectAllOk}）`);
      await shot(wd, "step2-wizard-all-enabled");
    }
  } else {
    evidence.push("步骤 2 没有 pw-scan（扫描入口不存在）");
  }

  await wd.closeDialog();
  await sleep(400);

  // 名称去重：现在库里已经有这个名字了，再开一次向导填同名 → 本地必须拦下
  await wdClickTid(wd, "providers-add-open", evidence);
  await sleep(500);
  await wdFillByTid(wd, "pw-name", pname, evidence);
  await sleep(400);
  const dupIssues = await wd.textOf('[data-testid="pw-step1-issues"]');
  evidence.push(`重名校验 pw-step1-issues: ${JSON.stringify((dupIssues ?? "").replace(/\s+/g, " ").trim())}`);
  const dupBlocked = dupIssues !== null && dupIssues.trim().length > 0;
  await wd.closeDialog();
  await sleep(400);

  // 列表里也按**名称**核对（列表行渲染的是名称/id，没有独立的 id 文本；真实 id 从上面的库里读）
  const listProbe = await wd.exec(`
    const want = ${JSON.stringify(pname)};
    const rows = [...document.querySelectorAll('[data-testid="providers-row"]')];
    return { rows: rows.length, hit: rows.some((r) => (r.textContent || '').includes(want)) };
  `);
  evidence.push(`服务商列表 [data-testid="providers-row"]: ${JSON.stringify(listProbe)}`);
  const providerInUi = listProbe?.hit === true;
  evidence.push(`服务商: 落库=${providerInDb}（真实 id=${JSON.stringify(realProviderId)}）列表出现=${providerInUi}（按名称核对）`);

  // ---- ② 「一个服务商一把密钥」：**列表页不许有第二条密钥的入口**；向导那一把必须真进密钥仓库 ----
  //
  // [docs/28 §1.3]（用户原话：「一个服务商肯定就一个密钥，怎么还做多个删除的…要多个账号就直接
  // 添加多个服务商、取不同名字」）：`/providers` 的「添加密钥」入口与 `credentials-add-*` 对话框
  // **已经下线**。所以这一段的判据从"能不能加第二份凭据"倒过来变成两件事：
  //   ① 那个入口**不存在**（它一旦回来，就是多凭据模型回来了 —— 这才是真缺陷）；
  //   ② 向导第 1 步填的那把密钥**真的进了密钥仓库**（`secret_meta` 有行），而明文既不在库里、
  //      也不在界面上。
  let credentialInDb = false;
  let credentialInUi = false;
  const multiKeyEntryGone = !hasCredentialEntry;
  evidence.push(
    `「添加密钥」入口（credentials-add-open）= ${hasCredentialEntry}（期望 false：一个服务商一把密钥，多账号请再加一个服务商）`,
  );
  if (hasCredentialEntry) {
    evidence.push(
      "→ **真缺陷**：/providers 又出现了「添加密钥」入口 —— 那是被明确否决的「给一个服务商挂第二份凭据」的路径",
    );
  }
  {
    // 密钥真值落在**密钥仓库**里：这一步能证明"确实写进去过"的证据是**元数据**
    // （`secret_meta` 那一行：条目名 + 脱敏 hint）—— 明文本身不在配置、不在 SQLite 的
    // 任何一列、也不在页面上（下面两条断言）。
    //
    // ★ 这里过去恒红的真因：`dumpScratchDb()` **根本没有 dump `secret_meta`**，
    //   于是 `dump.secret_meta` 永远是 `undefined` → `?? []` → "0 行" → `metaOk` 恒 false，
    //   最后只剩 `secrets.enc` 这条退路；而这台机器走的是**钥匙串**后端（Windows 凭据管理器），
    //   便携文件本来就不会被写 —— 于是判据写成"两个都不成立"→ 假红。
    //   查同一个 scratch 库可以看见产品一直在写：`secret_meta` 里有
    //   `ta-e2e-default | hint=sk-t***`（见 `%TEMP%\ta-e2e\db\dump-*.json`）。
    //
    // 现在的判据（**加严**，不是放宽）：
    //   ① **等到后端回执再读**：轮询 `secret_meta` 直到 `<id>-default` 那一行真的出现
    //      （不再"睡 800ms 赌它写完"）；
    //   ② hint 必须是**脱敏**的（非空、不等于明文、不含明文）；
    //   ③ `secret_meta` 表里**没有**任何值列（列名清单直接断言，[docs/19 §1.3] 的硬约束）；
    //   ④ 配置里的引用 `${secret:<id>-default}` 要对得上；
    //   ⑤ 明文不许出现在**任何** secrets.enc（scratch 与真实数据目录两处都要查
    //      —— 便携库的默认路径是 `%APPDATA%\TokenAgency\secrets.enc`，
    //      只查 scratch 会漏掉"降级到文件后端"的那条路）。
    const PLAINTEXT = "sk-ta-e2e-secret";
    const expectedSecretName = realProviderId === null ? null : `${realProviderId}-default`;
    /** 轮询直到条件成立（返回命中的那次 dump），超时返回最后一次。 */
    const waitForSecretMeta = async (tag, timeoutMs = 6000) => {
      let dump = dumpScratchDb(tag);
      let rows = dump.secret_meta ?? [];
      const deadline = Date.now() + timeoutMs;
      let tries = 1;
      while (
        expectedSecretName !== null &&
        !rows.some((r) => String(r.name) === expectedSecretName) &&
        Date.now() < deadline
      ) {
        await sleep(400);
        tries += 1;
        dump = dumpScratchDb(`${tag}-${tries}`);
        rows = dump.secret_meta ?? [];
      }
      return { dump, rows, tries };
    };
    const { dump: dumpSecret, rows: metaRows, tries: metaTries } = await waitForSecretMeta("step2-secret-store");
    const metaRow =
      expectedSecretName === null ? null : (metaRows.find((r) => String(r.name) === expectedSecretName) ?? null);
    const metaOk = metaRow !== null;
    // ② 脱敏：`hint` 不是明文、也不含明文（[docs/19 §1.3]：表里只留 `sk-a***` 这种提示）
    const hint = metaRow === null ? null : String(metaRow.hint ?? "");
    const hintMasked = hint !== null && hint.trim() !== "" && hint !== PLAINTEXT && !hint.includes(PLAINTEXT);
    // ③ 表里没有值列（列名清单已经在 dump 里落盘，断言直接读它）
    const columns = (dumpSecret.secret_meta_columns ?? []).map((c) => String(c).toLowerCase());
    const valueColumns = columns.filter((c) => /value|plaintext|secret$|^secret|password|token/.test(c));
    const noValueColumn = columns.length > 0 && valueColumns.length === 0;
    // ⑤ 明文不许在任何一份便携密钥库里
    const secretFiles = [join(SCRATCH, "secrets.enc"), join(REAL_DATA_DIR, "secrets.enc")].filter((p) =>
      existsSync(p),
    );
    const plainInSecretFiles = secretFiles.filter((p) => readFileSync(p).includes(Buffer.from(PLAINTEXT)));
    // 配置里的引用必须指到那份凭据（`${secret:<id>-default}`）：引用与仓库条目要对得上
    const credRow = configEntities(dumpSecret, "credential").find(
      (c) => String(c.json?.id ?? "") === expectedSecretName,
    );
    const secretRef = String(credRow?.json?.secret ?? "");
    const refOk = expectedSecretName !== null && secretRef.includes(expectedSecretName);
    credentialInDb = refOk && metaOk && hintMasked && noValueColumn && plainInSecretFiles.length === 0;
    evidence.push(
      `密钥仓库: 轮询 ${metaTries} 次读到 secret_meta ${metaRows.length} 行` +
        `（命中 ${JSON.stringify(expectedSecretName)}=${metaOk}，hint=${JSON.stringify(hint)}，脱敏=${hintMasked}）· ` +
        `表列=${JSON.stringify(columns)}（值列=${JSON.stringify(valueColumns)}，应空=${noValueColumn}）· ` +
        `secrets.enc 存在=${JSON.stringify(secretFiles)} 含明文=${JSON.stringify(plainInSecretFiles)} · ` +
        `引用=${JSON.stringify(secretRef)} 对得上=${refOk} → ${credentialInDb}`,
    );
    // 明文**不许**出现在页面任何地方，也不许出现在库里的任何一列
    const pageAfter = await wd.pageText();
    const plainInPage = pageAfter.includes(PLAINTEXT);
    const plainInDb = JSON.stringify(dumpSecret).includes(PLAINTEXT);
    evidence.push(`明文 ${PLAINTEXT}: 页面上=${plainInPage} 库里=${plainInDb}（两者都必须是 false）`);
    credentialInUi = !plainInPage && !plainInDb;
  }

  // 向导第 1 步的契约（[docs/19 §1.3]）：provider 落库时同时挂一份 `<provider>-default` 凭据。
  // 用**库里读出的真实 id** 去认领它 —— 这比"按钮点过了"强：证明库里真的存在这样一对实体，
  // 而不是挂具自己想象出来的 id 上有这么一对。
  const dumpAfterCred = dumpScratchDb("step2-default-credential");
  const credentialRows = configEntities(dumpAfterCred, "credential");
  const defaultCredential =
    realProviderId === null
      ? null
      : (credentialRows.find(
          (c) =>
            String(c.json.provider ?? "") === realProviderId &&
            String(c.json.id ?? "") === `${realProviderId}-default`,
        ) ?? null);
  const hasDefaultCredential = defaultCredential !== null;
  evidence.push(
    `向导默认凭据（按真实 id ${JSON.stringify(realProviderId)} 认领）: config(kind=credential) 共 ${credentialRows.length} 行；` +
      `命中 id=${JSON.stringify(defaultCredential?.json.id ?? null)} provider=${JSON.stringify(defaultCredential?.json.provider ?? null)} ` +
      `label=${JSON.stringify(defaultCredential?.json.label ?? null)} → ${hasDefaultCredential}`,
  );

  // ---- ③ 「上游没有 /models 端点」那一档：**必须放在主流程全部结束之后**跑 ----
  //
  // 时序缺陷（96fae9a 那次实机 run 里 step2 的 FAIL 就是它）：这个子探测自己会**开一轮新向导
  // 并把它关掉**（`wd.closeDialog()`），而它原先被插在 `pw-test` 与 `pw-next` 之间 —— 于是主流程
  // 接下来要点的 `pw-next` 所在的对话框已经被关掉了。原始证据正是这一串连锁现象：
  //   `缺控件 pw-next` → `步骤 2 命中: `（空）→ `pw-saved-notice: ""` → `扫描行数=0`
  //   → `信息面板 hasInfoBtn=false`。
  // 挪到末尾之后：主流程（重名 / 测试连接 / 下一步 / 扫描 / 全选 / 添加 / 模型信息只读）一路上
  // 不再被另一轮向导会话打断；这一档该跑的断言一条都没少，评测口径也没有放宽。
  const noModels = await runProviderWizardProbeOnNoModelsEndpoint(wd, fixture, evidence);

  const ok =
    providerInDb &&
    providerInUi &&
    hasDefaultCredential &&
    credentialInDb &&
    credentialInUi &&
    multiKeyEntryGone &&
    testShowsLatency &&
    noModels.ok &&
    scanRows > 0 &&
    addedModels &&
    addedRowsOk &&
    allOffOk &&
    selectAllOk &&
    dupBlocked;
  if (ok) {
    record(
      "step2",
      "PASS",
      `服务商两步向导：名称去重拦下重名 · 测试连接显示真实延迟 · 上游无 /models 端点如实说明 · 下一步落库（真实 id=${realProviderId}，含 <id>-default 凭据） · ` +
        `扫描后一个列表里出现 ${addedRowsVisible} 条线路（每行都有可改的网络代理 + 启用滑块 + 上游原文，旧的"勾一下才创建"控件已消失）· ` +
        `新线路默认关闭=${allOffOk} · 全选一次动作真的把它们全部启用=${selectAllOk} · 密钥向导落库`,
      evidence,
    );
  } else {
    record(
      "step2",
      "FAIL",
      `两步向导没走通：落库=${providerInDb}（真实 id=${JSON.stringify(realProviderId)}）列表=${providerInUi} 默认凭据=${hasDefaultCredential} ` +
        `密钥(明文不落库/不上面=${credentialInUi} 仓库回执=${credentialInDb}) 测试连接显示延迟=${testShowsLatency} 无端点档=${noModels.ok} ` +
        `线路行=${scanRows} 已建线路=${addedModels} 行结构（代理/滑块/原文）=${addedRowsOk} 默认关闭=${allOffOk} 全选全开=${selectAllOk} 重名拦下=${dupBlocked}`,
      evidence,
    );
  }
}

/**
 * 「上游没有 /models 端点」那一档（[docs/19 §1.1]）：建第二个服务商，baseUrl 指向 fixture 的
 * `/notfound/v1`（恒 404），点「测试连接」→ 结果区必须说"上游没有 /models 端点"，
 * **不许**渲染成"连接失败"（那会把一个正常的上游说成坏的）。
 *
 * 调用时机是**硬约束**：它自己会开一轮新向导，收尾时也会把这轮向导关掉
 * （`closeDialog()` 是必须的，否则下次点 `providers-add-open` 会被 Dialog 的 backdrop 拦下）。
 * 所以只能放在 step2 主流程全部结束之后调用 —— 早一步就会把主流程还要用的向导一起关掉
 * （96fae9a 那次 run 的 `缺控件 pw-next` 就是这么来的）。
 * 顺带一个已知副作用：这一步的 `pw-test` 与主流程一样会**先落库再探测**
 * （`runTest` 里 `if (id === undefined) id = await saveStep1()`），所以它会在
 * config 里多留一条 provider + 一条 credential。这是产品语义（测试连接前先如实落库），
 * 不是挂具制造的假数据；它只发生在主流程的所有断言之后，不影响任何判定。
 */
async function runProviderWizardProbeOnNoModelsEndpoint(wd, fixture, evidence) {
  const name = "TA E2E 无端点";
  // 先关掉当前向导（上面那条已经填过一轮），用干净的一轮重开
  await wd.closeDialog();
  await sleep(400);
  if (!(await wdClickTid(wd, "providers-add-open", evidence))) return { ok: false, text: null };
  await wdFillByTid(wd, "pw-name", name, evidence);
  await wdFillByTid(wd, "pw-baseUrl", fixture.noModelsBaseUrl, evidence);
  await wdFillByTid(wd, "pw-key", "sk-ta-e2e-secret", evidence);
  await wdClickTid(wd, "pw-test", evidence);
  await sleep(2500);
  const raw = await wd.textOf('[data-testid="pw-test-result"]');
  const text = (raw ?? "").replace(/\s+/g, " ").trim();
  evidence.push(`【无 /models 端点档】pw-test-result: ${JSON.stringify(text)}`);
  await shot(wd, "step2-wizard-no-models-endpoint");
  await wd.closeDialog();
  await sleep(400);
  const ok = text.length > 0 && !/连接失败|无法连接|网络错误/.test(text);
  return { ok, text };
}

/** 在任意形态的表单里按字段名启发式填值（id/name/protocol/base_url）。 */
async function fillFormHeuristic(wd, values) {
  const fields = await wd.exec(`
    const out = [];
    for (const el of document.querySelectorAll('input:not([type=hidden]),textarea,[role="combobox"]')) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      const holder = el.closest('[data-testid]');
      out.push({
        key: ((holder && holder.getAttribute('data-testid')) || el.getAttribute('name') || el.getAttribute('id') || el.getAttribute('placeholder') || ''),
        tag: el.tagName.toLowerCase(),
        type: el.getAttribute('type') || '',
        role: el.getAttribute('role') || '',
      });
    }
    return out;
  `);
  const filled = {};
  for (const f of fields) {
    const k = f.key.toLowerCase();
    let v;
    if (/protocol/.test(k)) v = values.protocol;
    else if (/base|url/.test(k)) v = values.base_url;
    else if (/(^|[^a-z])id([^a-z]|$)|-id/.test(k)) v = values.id;
    else if (/name/.test(k)) v = values.name;
    else continue;
    // ★ 不要用一个多选择器一次查询：`[data-testid="X"]` 这种"容器"匹配项在文档顺序上
    // **先于**它内部的 input，于是 querySelector 会返回那个不可交互的容器 → 真机上得到
    // `element not interactable`（实测踩到过，整轮 harness 因此中断）。
    // 正确顺序：先单独找内部控件，再退回容器本身。
    const inner = await wd.findElement(
      `[data-testid="${f.key}"] input,[data-testid="${f.key}"] textarea,[data-testid="${f.key}"] [role="combobox"]`,
    );
    const el = inner ?? (await wd.findElement(`[data-testid="${f.key}"]`));
    if (el === null) continue;
    if (f.role === "combobox" || f.tag === "select") {
      // MUI select：点开，选第一个选项
      await wd.click(el).catch(() => {});
      await sleep(300);
      const opt = await wd.waitFor('li[role="option"]', 3000);
      if (opt !== null) {
        await wd.click(opt).catch(() => {});
        filled[f.key] = `${v} (选第一项)`;
      }
    } else {
      // 单个字段填不进去**不能让整轮中断**：如实记下来、继续跑其余步骤，
      // 由后续断言（列表/落库）决定这一步到底算不算过。
      try {
        await wd.fill(el, v);
        filled[f.key] = v;
      } catch (err) {
        filled[f.key] = `（填不进去：${String(err.message ?? err).slice(0, 140)}）`;
      }
    }
  }
  return filled;
}

// ------------------------------------------------- 首启向导：界面里真实存在的写路径

/** 向导里要写进去的两样东西。id/url 里的标记就是后面断言用的字面量。 */
const E2E_PROVIDER = {
  id: "ta-e2e-provider",
  name: "TA E2E 供应商",
  protocol: "openai",
  secret: "sk-ta-e2e-secret",
};
const E2E_SUB = { name: "TA E2E 订阅" };

/** 一次向导提交的结果。step2 跑、step3 复用 —— 但**证据里会写明是复用**。 */
let wizardRun = null;

async function ensureWizardRun(wd, fixture, evidence) {
  if (wizardRun !== null) {
    evidence.push(
      "复用 step2 里那次向导提交：界面里一次 apply 同时写供应商与订阅（后端是 upsert 合并语义，" +
        "见 `settings_ext.rs::apply_onboarding` 从 `state.doc()` 克隆，不覆盖已有条目）",
    );
    return wizardRun;
  }
  wizardRun = await runOnboardingWizard(wd, fixture, evidence);
  return wizardRun;
}

/** 等向导走到第 n 步（步骤面板上有 `data-step`）。 */
async function waitWizardStep(wd, n, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const el = await wd.findElement(`[data-testid="onboarding-step-panel"][data-step="${n}"]`);
    if (el !== null) return el;
    if (Date.now() > deadline) return null;
    await sleep(150);
  }
}

/**
 * 走完首启向导：加一个供应商 + 加一个 Clash 订阅 → 点「应用这份配置」。
 *
 * **每一步都是 WebDriver 的元素点击 / 元素输入**：
 * `onboarding-provider-add` → 填 4 个输入框 → `onboarding-next` →
 * `onboarding-sub-add` → 填 2 个输入框 → `onboarding-next` → `onboarding-next` →
 * `onboarding-apply-submit`。只有 `window.location.hash` 是用 script 设的。
 */
async function runOnboardingWizard(wd, fixture, evidence) {
  const panel = await gotoPage(wd, "#/onboarding", '[data-testid="onboarding-step-panel"]');
  if (panel === null) {
    return { ok: false, reason: '界面缺入口：缺 selector [data-testid="onboarding-step-panel"]' };
  }
  evidence.push(`路由 #/onboarding；面板 data-step=${await wd.attr(panel, "data-step")}`);
  await shot(wd, "wizard-1-step1");

  // ---------- 第 1 步：加供应商
  const addProvider = await wd.waitFor('[data-testid="onboarding-provider-add"]', 8000);
  if (addProvider === null) {
    return { ok: false, reason: '界面缺入口：缺 selector [data-testid="onboarding-provider-add"]' };
  }
  await wd.click(addProvider);
  await sleep(300);

  // ---- 缺陷探测：往 id 输入框里打字会发生什么？
  // 这一个 **必须** 保留：`ProviderEditor` 的 React key 是 `${row.id}-${index}`，
  // 而 id 是**可编辑**的 —— 输入第一个字符就会改 key，于是整个编辑器被卸载重挂，
  // 输入框失焦、后续按键全部丢掉。这里把现象原样记下来，再把这一行删掉重来，
  // 保证真正提交上去的 artefact 是干净的。
  const idSel = '[data-testid="onboarding-provider-field-0-id"] input';
  const idBefore = await wd.attrOf(idSel, "value");
  let probeErr = null;
  const idEl0 = await wd.findElement(idSel);
  if (idEl0 !== null) {
    await wd.click(idEl0).catch(() => {});
    try {
      await wd.type(idEl0, "ta-e2e-provider");
    } catch (e) {
      probeErr = e.message;
    }
    await sleep(800);
    const idAfter = await wd.attrOf(idSel, "value");
    evidence.push(
      `【缺陷探测】真送键 "ta-e2e-provider" 进 ${idSel}：value ${JSON.stringify(idBefore)} → ${JSON.stringify(idAfter)}` +
        (probeErr ? `（送键抛错：${probeErr.slice(0, 140)}）` : ""),
    );
    const idEl1 = await wd.findElement(idSel);
    if (idEl1 !== null) {
      try {
        await wd.fill(idEl1, "ta-e2e-provider");
      } catch {
        /* 探测失败不影响流程 */
      }
      await sleep(500);
      evidence.push(`  fill()（click + clear + 送键）之后 value=${JSON.stringify(await wd.attrOf(idSel, "value"))}`);
    }
    const rm0 = await wd.findElement('[data-testid="onboarding-provider-remove-0"]');
    if (rm0 !== null) {
      await wd.click(rm0);
      await sleep(400);
      evidence.push('  已点 [data-testid="onboarding-provider-remove-0"] 删掉探测行，重新加一行做正式提交');
    }
  } else {
    evidence.push(`  缺 selector ${idSel}，无法做 id 输入探测`);
  }
  // 重新加一行（id 回到默认 p1），**故意不再碰 id 框**：上面已经证明碰不得。
  const addProvider2 = await wd.waitFor('[data-testid="onboarding-provider-add"]', 8000);
  await wd.click(addProvider2);
  await sleep(300);
  evidence.push('正式提交用的那一行：只填 name / baseUrl / secret，**不碰 id**（默认 p1，见上面的缺陷探测）');

  const fillProviderField = async (name, value) => {
    const sel = `[data-testid="onboarding-provider-field-0-${name}"] input, [data-testid="onboarding-provider-field-0-${name}"] textarea`;
    const el = await wd.waitFor(sel, 6000);
    if (el === null) throw new Error(`缺 selector [data-testid="onboarding-provider-field-0-${name}"] 内的 input/textarea`);
    await wd.fill(el, value);
    evidence.push(`真输入 onboarding-provider-field-0-${name} ← ${JSON.stringify(value)}`);
  };
  const baseUrl = `${fixture.baseUrl}`;
  await fillProviderField("name", E2E_PROVIDER.name);
  await fillProviderField("baseUrl", baseUrl);
  await fillProviderField("secret", E2E_PROVIDER.secret);
  const protocolCtl = await wd.findElement('[data-testid="onboarding-provider-field-0-protocol"]');
  evidence.push(
    `协议控件 [data-testid="onboarding-provider-field-0-protocol"] 存在=${protocolCtl !== null}（保留默认值 openai，不做无意义的改写）`,
  );
  const issues1 = await wd.textOf('[data-testid="onboarding-step1-issues"]');
  evidence.push(
    `第 1 步本地校验: ${issues1 === null ? "无 issue（onboarding-step1-issues 不存在）" : JSON.stringify(issues1.replace(/\s+/g, " "))}`,
  );
  await shot(wd, "wizard-2-provider-filled");

  const next1 = await wd.findElement('[data-testid="onboarding-next"]');
  if (next1 === null) return { ok: false, reason: '缺 selector [data-testid="onboarding-next"]' };
  evidence.push(`onboarding-next（第 1 步）enabled=${await wd.enabled(next1)}`);
  await wd.click(next1);
  if ((await waitWizardStep(wd, 2)) === null) return { ok: false, reason: "点了「下一步」但向导没有走到第 2 步（按钮可能是禁用的）" };

  // ---------- 第 2 步：加 Clash 订阅
  const addSub = await wd.waitFor('[data-testid="onboarding-sub-add"]', 8000);
  if (addSub === null) return { ok: false, reason: '界面缺入口：缺 selector [data-testid="onboarding-sub-add"]' };
  await wd.click(addSub);
  await sleep(300);
  const fillSubField = async (name, value) => {
    const sel = `[data-testid="onboarding-sub-field-0-${name}"] input, [data-testid="onboarding-sub-field-0-${name}"] textarea`;
    const el = await wd.waitFor(sel, 6000);
    if (el === null) throw new Error(`缺 selector [data-testid="onboarding-sub-field-0-${name}"]`);
    await wd.fill(el, value);
    evidence.push(`真输入 onboarding-sub-field-0-${name} ← ${JSON.stringify(value)}`);
  };
  await fillSubField("name", E2E_SUB.name);
  await fillSubField("url", fixture.url);
  const issues2 = await wd.textOf('[data-testid="onboarding-step2-issues"]');
  evidence.push(
    `第 2 步本地校验: ${issues2 === null ? "无 issue（onboarding-step2-issues 不存在）" : JSON.stringify(issues2.replace(/\s+/g, " "))}`,
  );
  await shot(wd, "wizard-3-sub-filled");

  const next2 = await wd.findElement('[data-testid="onboarding-next"]');
  evidence.push(`onboarding-next（第 2 步）enabled=${await wd.enabled(next2)}`);
  await wd.click(next2);
  if ((await waitWizardStep(wd, 3)) === null) return { ok: false, reason: "点了「下一步」但向导没有走到第 3 步" };

  // ---------- 第 3 步：连通性测试（真命令；没有内核时它会如实报错，不算本步失败）
  const testRun = await wd.findElement('[data-testid="onboarding-test-run"]');
  if (testRun !== null) {
    await wd.click(testRun);
    await sleep(2500);
    const tErr = await wd.textOf('[data-testid="onboarding-test-error"]');
    const tRes = tErr === null ? await wd.textOf('[data-testid="onboarding-test-result"]') : null;
    evidence.push(
      `点了 onboarding-test-run → ${tErr !== null ? `错误原文: ${JSON.stringify(tErr.replace(/\s+/g, " ").slice(0, 300))}` : tRes !== null ? `结果: ${JSON.stringify(tRes.replace(/\s+/g, " ").slice(0, 300))}` : "既无错误也无结果"}`,
    );
  } else {
    evidence.push('第 3 步没有 [data-testid="onboarding-test-run"]（跳过测试按钮）');
  }
  await shot(wd, "wizard-4-test");

  const next3 = await wd.findElement('[data-testid="onboarding-next"]');
  evidence.push(`onboarding-next（第 3 步）enabled=${next3 === null ? "(不存在)" : await wd.enabled(next3)}`);
  if (next3 !== null) {
    await wd.click(next3);
    if ((await waitWizardStep(wd, 4)) === null) return { ok: false, reason: "点了「下一步」但向导没有走到第 4 步" };
  }

  // ---------- 第 4 步：真提交
  const payloadTxt = await wd.textOf('[data-testid="onboarding-apply-payload"]');
  if (payloadTxt !== null) evidence.push(`提交内容预览: ${JSON.stringify(payloadTxt.replace(/\s+/g, " ").slice(0, 500))}`);
  const submit = await wd.waitFor('[data-testid="onboarding-apply-submit"]', 8000);
  if (submit === null) {
    return { ok: false, reason: '界面缺入口：缺 selector [data-testid="onboarding-apply-submit"]（第 4 步没有提交按钮）' };
  }
  await shot(wd, "wizard-5-before-apply");
  await wd.click(submit);
  // 等结果。**判据选 `onboarding-apply-raw`（只在 apply.data 到位后才渲染）**，
  // 而不是 `onboarding-apply-result` —— 后者在数据到达前就有一个"还没有结果"的占位节点，
  // 拿它当"完成"会读到占位文本（而且那个节点随后会被 React 换掉 → stale element）。
  const deadline = Date.now() + 25000;
  let outcome = null;
  while (Date.now() < deadline) {
    const errTxt = await wd.textOf('[data-testid="onboarding-apply-error"]');
    if (errTxt !== null) {
      outcome = { ok: false, text: errTxt.replace(/\s+/g, " ") };
      break;
    }
    const rawTxt = await wd.textOf('[data-testid="onboarding-apply-raw"]');
    if (rawTxt !== null) {
      const resTxt = await wd.textOf('[data-testid="onboarding-apply-result"]');
      outcome = { ok: true, text: (resTxt ?? "").replace(/\s+/g, " "), raw: rawTxt.replace(/\s+/g, " ") };
      break;
    }
    await sleep(300);
  }
  await shot(wd, "wizard-6-after-apply");

  if (outcome === null) return { ok: false, reason: "点了「应用这份配置」后 20s 内既没有结果也没有错误" };
  evidence.push(`onboarding_apply 回显: ${JSON.stringify(outcome.text.slice(0, 400))}`);
  if (outcome.raw) evidence.push(`ApplyResult 原文: ${JSON.stringify(outcome.raw.slice(0, 300))}`);
  if (!outcome.ok) return { ok: false, reason: `onboarding_apply 报错：${outcome.text.slice(0, 300)}` };
  return { ok: true, text: outcome.text, payload: payloadTxt ?? undefined };
}

/** 回到 /providers 看这条供应商有没有出现在列表里，并查库。 */
async function verifyProviderLanded(wd, evidence, payloadText) {
  // 从向导第 4 步的 payload 原文里把真正提交上去的 provider id 抠出来
  // （不假设它就是常量 —— id 框有缺陷，见 runOnboardingWizard 里的探测）。
  let submittedId = null;
  if (typeof payloadText === "string") {
    const m = /"providers"\s*:\s*\[\s*\{[^}]*?"id"\s*:\s*"([^"]*)"/.exec(payloadText);
    submittedId = m ? m[1] : null;
  }
  evidence.push(`向导 payload 里实际提交的 provider id = ${JSON.stringify(submittedId)}`);
  await gotoPage(wd, "#/providers");
  const rows = (await wd.probeTestIds()).includes("providers-row");
  const text = await wd.pageText();
  evidence.push(`回到 #/providers：providers-row 存在=${rows}，页面含 "${E2E_PROVIDER.name}"=${text.includes(E2E_PROVIDER.name)}`);
  evidence.push(`列表行文本: ${JSON.stringify(text.split("\n").filter((l) => l.includes(E2E_PROVIDER.name) || l.includes(E2E_PROVIDER.id)).slice(0, 4))}`);
  const dump = dumpScratchDb("step2");
  // ⚠️ 供应商**不在**独立表里：配置实体统一落在 `config(kind, entity_id, json)`，
  // 而且 kind 是**单数** `provider`（不是 `providers`）。这是实测出来的，不是猜的。
  const providerRows = (dump.config ?? []).filter((r) => r.kind === "provider");
  const hit = providerRows.filter((r) => String(r.json).includes(E2E_PROVIDER.name));
  evidence.push(`db config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  evidence.push(`db 里 kind='provider' 共 ${providerRows.length} 行；命中名称 "${E2E_PROVIDER.name}" 的: ${JSON.stringify(hit).slice(0, 700)}`);
  await shot(wd, "step2-provider-landed");
  if (rows && hit.length > 0) {
    record("step2", "PASS", `供应商经真界面（首启向导）新增成功并落库：列表出现且 config(kind=provider) 里有它（id=${submittedId}）`, evidence);
  } else {
    record("step2", "FAIL", `向导提交后不一致：列表出现=${rows} 落库=${hit.length > 0}`, evidence);
  }
}

/** 回到 /network 看这条订阅有没有出现在列表里，并查库。 */
async function verifySubscriptionLanded(wd, evidence) {
  await gotoPage(wd, "#/network");
  const rows = (await wd.probeTestIds()).includes("network-subscription-row");
  const text = await wd.pageText();
  evidence.push(`回到 #/network：network-subscription-row 存在=${rows}`);
  evidence.push(`列表里订阅相关行: ${JSON.stringify(text.split("\n").filter((l) => l.includes(E2E_SUB.name) || l.includes("clash.yaml")).slice(0, 5))}`);
  const dump = dumpScratchDb("step3");
  // 同 step2：订阅配置也在 `config` 表里（kind='subscription'，单数）。
  // 独立的 `subscription` 表是**解析结果**的落点，本次没跑解析，所以它是空的 —— 这不是缺陷。
  const cfgSubs = (dump.config ?? []).filter((r) => r.kind === "subscription");
  const hit = cfgSubs.filter((r) => String(r.json).includes("clash.yaml"));
  evidence.push(`db 里 kind='subscription' 共 ${cfgSubs.length} 行；命中 clash.yaml 的: ${JSON.stringify(hit).slice(0, 700)}`);
  evidence.push(`db(subscription 表，解析结果) 行数=${(dump.subscription ?? []).length}（本次没触发订阅解析，空属预期）`);
  await shot(wd, "step3-sub-landed");
  if (rows && hit.length > 0) record("step3", "PASS", "Clash 订阅经真界面（首启向导）新增成功并落库：列表出现且 config(kind=subscription) 里有它", evidence);
  else record("step3", "FAIL", `向导提交后不一致：列表出现=${rows} 落库=${hit.length > 0}`, evidence);
}

// ---------------------------------------------------------------- step3 订阅

async function step3(wd, fixture) {
  const page = await gotoPage(wd, "#/network");
  const evidence = [];
  evidence.push(`本地 Clash fixture: ${fixture.url}（脚本自己起的 HTTP 服务，不依赖公网）`);
  if (page === null) {
    await shot(wd, "step3-no-page");
    evidence.push('等 15s 未见 [data-testid="page-header"]（/network 没渲染）');
    record("step3", "FAIL", "/network 页面没渲染出来", evidence);
    return;
  }
  const tids = await wd.probeTestIds();
  evidence.push(`路由: #/network；命中 data-testid: ${tids.filter((t) => t.startsWith("network")).join(", ")}`);
  evidence.push(
    `订阅区状态: ${tids.includes("network-subscriptions-empty") ? "network-subscriptions-empty（后端真返回 0 条）" : tids.includes("network-subscriptions-table") ? "有表" : "无空态无表"}`,
  );
  evidence.push(`页面文本片段: ${JSON.stringify((await wd.pageText()).replace(/\s+/g, " ").slice(0, 240))}`);

  const add = await findAddControl(wd);
  if (add === null) {
    evidence.push("找过：data-testid 匹配 /(^|-)add|new|create|upsert/i 的可见控件 → 无");
    evidence.push('找过：可见 button/[role=button] 文案匹配 /新增|添加|新建|创建|Add|New|Create/i → 无');
    evidence.push("页面上实际存在的可见控件：");
    evidence.push(...(await controlEvidence(wd)));
    await shot(wd, "step3-network-no-entry");
    evidence.push("→ /network 没有新增入口（真实缺陷，已记录）；改走界面里真实存在的另一条路径：首启向导 /onboarding 第 2 步");
    const wiz = await ensureWizardRun(wd, fixture, evidence);
    if (!wiz.ok) {
      record("step3", "SKIP", `界面缺入口：/network 没有新增控件，且首启向导也不可用（${wiz.reason}）`, evidence);
      return;
    }
    await verifySubscriptionLanded(wd, evidence);
    return;
  }

  await wd.click(add.el);
  await shot(wd, "step3-form-opened");
  const filled = await fillFormHeuristic(wd, {
    id: "ta-e2e-sub-form",
    name: "TA E2E 订阅（表单）",
    url: fixture.url,
    base_url: fixture.url,
  });
  evidence.push(`新增入口: ${add.how}`);
  evidence.push(`填写的字段: ${JSON.stringify(filled)}`);

  const submit =
    (await wd.findElement('[data-testid="network-subscriptions-add-submit"]')) ??
    (await wd.findByText("button", /保存|提交|确定|创建|Save|Submit|Create|OK/i));
  if (submit === null) {
    evidence.push("填完找不到提交按钮");
    await shot(wd, "step3-no-submit");
    record("step3", "FAIL", "新增订阅表单打开了但没有提交按钮", evidence);
    return;
  }
  await wd.click(submit);
  await sleep(1500);
  await shot(wd, "step3-after-submit");

  const dump = dumpScratchDb("step3");
  // ★ 断言必须**查对表**：订阅实体写在 `config`（kind='subscription'），而不是 `subscription` 表
  //   （那张是版本/健康相关的表，空库时是空的）。之前查错表导致"列表=true 落库=false"的假失败。
  const subRows = (dump.config ?? []).filter((r) => r.kind === "subscription");
  evidence.push(`db(config, kind=subscription) 共 ${subRows.length} 行: ${JSON.stringify(subRows).slice(0, 600)}`);
  // ★ 也必须是**本次新建的那一条**：用独立 id，避免与 step2 向导建的同名实体混淆。
  const inList = (await wd.pageText()).includes("ta-e2e-sub-form");
  const inDb = subRows.some(
    (r) => String(r.entity_id) === "ta-e2e-sub-form" || String(r.json).includes("ta-e2e-sub-form"),
  );
  // [docs/57] 写表单在对话框里：走之前把它关掉，别把模态留给后面的步骤
  //（遮罩会挡住下一页的控件 —— 那是"假红"的常见来源，见 step22 里的同类注释）。
  await wd.closeDialog();
  if (inList && inDb) record("step3", "PASS", "订阅经真界面（/network 表单）新增，列表与 config 表都出现", evidence);
  else record("step3", "FAIL", `新增后不一致：列表=${inList} 落库=${inDb}`, evidence);
}

// ---------------------------------------------------------------- step4 通道/线路

async function step4(wd) {
  const evidence = [];
  // 契约里"线路"的两个写命令是 lane_upsert / lane_set_node。
  // 原来这里还先看 `/channels`（套餐与线路）上有没有 lane 写入口 —— 那一页已按用户裁决删除
  // （[docs/49]），所以直接看现在唯一可能有的地方：`/network` 的线路区。
  await gotoPage(wd, "#/network");
  const tidsNet = (await wd.probeTestIds()).filter((t) => t.includes("lane"));
  evidence.push(`路由: #/network；与 lane 相关的 data-testid: ${tidsNet.join(", ") || "（无）"}`);
  const netText = await wd.pageText();
  const laneSection = netText.split("\n").filter((l) => /线路|通道|lane/i.test(l)).slice(0, 6);
  evidence.push(`/network 里与线路相关的文案: ${JSON.stringify(laneSection)}`);

  const addCh = await findAddControl(wd);
  if (addCh === null) {
    evidence.push("找过：data-testid 匹配 /(^|-)add|new|create|upsert/i → 无");
    evidence.push('找过：按钮文案匹配 /新增|添加|新建|创建|Add|New|Create/i → 无');
    // 静态交叉验证（与 DOM 探测独立）：前端产物里到底有没有引用过这两个写命令？
    evidence.push(`前端产物交叉验证: ${distCommandPresence(["lane_upsert", "lane_set_node"])}`);
    evidence.push("首启向导第 3 步只显示「将要测哪些 凭据 × 线路 组合」的推导矩阵，没有任何建线路的输入框/按钮");
    await shot(wd, "step4-no-entry");
    record(
      "step4",
      "SKIP",
      "界面缺入口：/network 没有建线路/指派节点的控件（缺 selector：任何 lane upsert / set-node 的 testid 或按钮）" +
        "（原来还会看 /channels，那一页已删 —— [docs/49]）",
      evidence,
    );
    return;
  }
  // ---- 真实 lane 写入口：/network 的「③ 线路」区块（命令 lane_upsert / lane_set_node）----
  // [docs/57]（第 324 轮）写表单**不再常驻**：入口按钮点开之后输入框才出现。
  // step3 一直是"先点一下 add 控件再填"（所以它没受影响），这里原来漏了这一步 ——
  // 表单搬进对话框之后就直接找不到输入框了。**那是挂具的假设过期，不是产品缺陷。**
  //
  // ★ 这里**必须点名**线路那个按钮，不能再用通用的 `findAddControl`：
  //   同一个页面上现在有**两个** add 入口（订阅 / 线路），而它按 DOM 顺序取第一个
  //   = 订阅那个 —— 点了订阅的对话框，当然永远等不到线路的输入框。
  //   （上面那次 `findAddControl` 只用来判"这一页到底有没有写入口"，不再用来点。）
  const laneAddBtn = await wd.findElement('[data-testid="network-lanes-add-open"]');
  if (laneAddBtn === null) {
    // [docs/58]（第 325 轮）**手动建 / 改线路的界面整块删除**（用户："这个功能好像没有什么用，
    // 可以在界面上删了"）。所以"找不到入口"现在是**预期结果**，不是缺功能 ——
    // 这一档改成**反回归闸门**：把"线路区块只读"这件事在真机上钉住。
    const writeEntries = await wd.exec(`
      return {
        addOpen: document.querySelectorAll('[data-testid="network-lanes-add-open"]').length,
        edit: document.querySelectorAll('[data-testid="network-lanes-edit"]').length,
        writeForm: document.querySelectorAll('[data-testid="network-lanes-write"]').length,
        addId: document.querySelectorAll('[data-testid="network-lanes-add-id"]').length,
        submit: document.querySelectorAll('[data-testid="network-lanes-add-submit"]').length,
        table: document.querySelector('[data-testid="network-lanes-table"]') !== null,
        empty: document.querySelector('[data-testid="network-lanes-empty"]') !== null,
      };
    `);
    evidence.push(`线路区块的写入口计数: ${JSON.stringify(writeEntries)}（全为 0 = 只读）`);
    const clean =
      writeEntries !== null &&
      writeEntries.addOpen === 0 &&
      writeEntries.edit === 0 &&
      writeEntries.writeForm === 0 &&
      writeEntries.addId === 0 &&
      writeEntries.submit === 0;
    if (!clean) {
      await shot(wd, "step4-lane-write-entry-returned");
      record("step4", "FAIL", "线路区块又出现了写入口（[docs/58] 已删除手动建线路的界面）", evidence);
      return;
    }
    record(
      "step4",
      "PASS",
      "线路区块**只读**：没有任何建/改线路的入口（[docs/58]）；线路由服务商页选中时按需建出来" +
        `（表=${writeEntries.table} 空态=${writeEntries.empty}）`,
      evidence,
    );
    return;
  }

  // ★ [docs/57] 先证明**不常驻**（用户第 324 轮："这个界面不要常驻"）：
  //   点开之前，写表单的输入框在 DOM 里**一个都不该有**。
  //   这一条是"不常驻"在真机上的可执行判据 —— 光"点了能出现"证明不了它不再常驻。
  const formBeforeClick = await wd.exec(
    `return document.querySelectorAll('[data-testid="network-lanes-add-id"]').length;`,
  );
  evidence.push(`点开之前 network-lanes-add-id 的数量=${formBeforeClick}（应为 0 = 不常驻）`);
  if (formBeforeClick !== 0) {
    await shot(wd, "step4-form-resident");
    record("step4", "FAIL", `写表单又变成常驻了：点开之前就有 ${formBeforeClick} 个输入框`, evidence);
    return;
  }

  await wd.click(laneAddBtn);
  const laneFormOpen = await wd.waitFor('[data-testid="network-lanes-add-id"]', 5000);
  if (laneFormOpen === null) {
    evidence.push("点了「添加线路」之后 5s 内没出现 network-lanes-add-id");
    await shot(wd, "step4-form-not-opened");
    await wd.closeDialog();
    record("step4", "FAIL", "点了「添加线路」但写表单没有出现（入口按钮没接上）", evidence);
    return;
  }
  evidence.push(`新增入口: ${addCh.how} · 实际点的是 network-lanes-add-open → 点开后写表单出现`);

  const laneId = "ta-e2e-lane";
  const fillByTid = async (tid, value) => {
    const inner = await wd.findElement(`[data-testid="${tid}"] input,[data-testid="${tid}"] textarea`);
    const el = inner ?? (await wd.findElement(`[data-testid="${tid}"]`));
    if (el === null) {
      evidence.push(`缺控件 ${tid}`);
      return false;
    }
    try {
      await wd.fill(el, value);
      return true;
    } catch (err) {
      evidence.push(`${tid} 填不进去：${String(err.message ?? err).slice(0, 140)}`);
      return false;
    }
  };

  const okId = await fillByTid("network-lanes-add-id", laneId);
  const okName = await fillByTid("network-lanes-add-name", "TA E2E 线路");
  evidence.push(`线路表单字段: id=${okId} name=${okName}`);

  // kind 是 MUI Select：默认多半已是「直连」；不是的话点开选一个 direct
  const kindSel = await wd.findElement('[data-testid="network-lanes-add-kind"]');
  if (kindSel !== null) {
    const cur = await wd.text(kindSel).catch(() => "");
    evidence.push(`kind 控件现值: ${JSON.stringify(cur)}`);
    if (!/直连|direct/i.test(cur)) {
      await wd.click(kindSel).catch(() => {});
      await sleep(300);
      const opt = await wd.findByText('li[role="option"]', /直连|direct/i);
      if (opt !== null) await wd.click(opt).catch(() => {});
    }
  }
  await shot(wd, "step4-form-filled");

  const laneSubmit = await wd.findElement('[data-testid="network-lanes-add-submit"]');
  if (laneSubmit === null) {
    await wd.closeDialog();
    record("step4", "FAIL", "线路区块在但没有提交按钮（network-lanes-add-submit）", evidence);
    return;
  }
  await wd.click(laneSubmit);
  await sleep(1500);
  await shot(wd, "step4-after-submit");
  // [docs/57] 走之前关掉对话框：留着模态会挡住后面步骤的控件（"假红"的常见来源）
  await wd.closeDialog();

  const dur = dumpScratchDb("step4");
  const laneRows = (dur.config ?? []).filter((r) => r.kind === "lane");
  evidence.push(`db(config, kind=lane) 共 ${laneRows.length} 行: ${JSON.stringify(laneRows).slice(0, 500)}`);
  // 列表这一侧：`/network` 的线路表**第一列是名称**（有 name 就显示 name，没有才显示 id，
  // 见 network/index.tsx 的 `row.name === undefined ? row.id : row.name`），所以"新线路出现在列表里"
  // 的正确判据是 **id 或名称命中**，并额外要求那一行本身存在（`network-lane-row`）。
  const laneTable = await wd.exec(`
    return {
      table: document.querySelector('[data-testid="network-lanes-table"]') !== null,
      rows: document.querySelectorAll('[data-testid="network-lane-row"]').length,
      empty: document.querySelector('[data-testid="network-lanes-empty"]') !== null,
      error: document.querySelector('[data-testid="network-lanes-error"]')?.innerText ?? null,
    };
  `);
  evidence.push(`/network 线路列表 DOM: ${JSON.stringify(laneTable)}`);
  const pageAfterWrite = await wd.pageText();
  const inList4 = laneTable.rows > 0 && (pageAfterWrite.includes(laneId) || pageAfterWrite.includes("TA E2E 线路"));
  const inDb4 = laneRows.some(
    (r) => String(r.entity_id) === laneId || String(r.json).includes(laneId),
  );
  if (inList4 && inDb4) {
    record("step4", "PASS", `线路经真界面（lane_upsert）新增，列表（${laneTable.rows} 行）与 config 表都出现`, evidence);
  } else {
    record("step4", "FAIL", `新增后不一致：列表=${inList4} 落库=${inDb4}`, evidence);
  }
}

// ---------------------------------------------------------------- step5 设置页

async function step5(wd) {
  const page = await gotoPage(wd, "#/settings", '[data-testid="settings-page"]');
  const evidence = [];
  if (page === null) {
    await shot(wd, "step5-no-page");
    evidence.push('等 15s 未见 [data-testid="settings-page"]');
    record("step5", "FAIL", "/settings 页面没渲染出来", evidence);
    return;
  }
  const tids = await wd.probeTestIds();
  evidence.push(`路由: #/settings；命中 settings-* testid 共 ${tids.filter((t) => t.startsWith("settings-")).length} 个`);
  evidence.push(`settings-config-error（配置读失败）: ${tids.includes("settings-config-error") ? await wd.text(await wd.findElement('[data-testid="settings-config-error"]')) : "不存在 → config_get 真的成功了"}`);

  // (a) 自启动开关：**区内任何一个 checkbox/switch 都算**（有就报它的归属 testid）
  const startupSection = await wd.findElement('[data-testid="settings-section-startup"]');
  const autostartSwitch = await wd.exec(`
    const s = document.querySelector('[data-testid="settings-section-startup"]');
    if (!s) return 'no-section';
    const sw = s.querySelector('input[type=checkbox],[role=switch],[role=checkbox]');
    if (!sw) return 'none';
    const holder = sw.closest('[data-testid]');
    return '存在，归属 testid=' + ((holder && holder.getAttribute('data-testid')) || '(无)');
  `);
  const blockedText = await wd.findElement('[data-testid="settings-autostart-blocked"]');
  const blockedBody = blockedText ? (await wd.text(blockedText)).replace(/\s+/g, " ").slice(0, 300) : "";
  evidence.push(`[data-testid="settings-section-startup"] 存在: ${startupSection !== null}`);
  evidence.push(`自启动开关（该区内的 checkbox/switch）: ${autostartSwitch}`);
  evidence.push(`settings-autostart-blocked 原文: ${JSON.stringify(blockedBody)}`);
  const modeField = await wd.findElement('[data-testid="settings-field-autostartMode"]');
  const delayField = await wd.findElement('[data-testid="settings-field-autostartDelay"]');
  evidence.push(`真可编辑控件存在: autostartMode=${modeField !== null} autostartDelay=${delayField !== null}`);

  // (b) 文件监听：**必须归属到 file/wat ch 语义的控件上**才算数。
  //     这一区里还有 `settings-field-trayShowLatency` 这种别的 bool 控件，
  //     把"区内任意一个 checkbox"当成文件监听开关是错的（会得出假 PASS）。
  const fw = await wd.exec(`
    const n = document.querySelector('[data-testid="settings-file-watch-blocked"]');
    const tray = document.querySelector('[data-testid="settings-section-tray"]');
    const all = tray ? [...tray.querySelectorAll('input[type=checkbox],[role=switch],[role=checkbox]')] : [];
    const owners = all.map((c) => {
      const holder = c.closest('[data-testid]');
      return (holder && holder.getAttribute('data-testid')) || '(无 testid)';
    });
    const fwCtl = owners.filter((o) => /file.?watch|watch.?file/i.test(o));
    return { blockedNote: !!n, traySection: !!tray, allBoolOwners: owners, fileWatchOwners: fwCtl };
  `);
  const fwText = await wd.findElement('[data-testid="settings-file-watch-blocked"]');
  const fwBody = fwText ? (await wd.text(fwText)).replace(/\s+/g, " ").slice(0, 260) : "";
  evidence.push(`文件监听区探测: ${JSON.stringify(fw)}`);
  evidence.push(`settings-file-watch-blocked 原文: ${JSON.stringify(fwBody)}`);
  await shot(wd, "step5-settings");

  const hasAutostartSwitch = typeof autostartSwitch === "string" && autostartSwitch.startsWith("存在");
  const hasFileWatchControl = Array.isArray(fw.fileWatchOwners) && fw.fileWatchOwners.length > 0;

  if (hasAutostartSwitch && hasFileWatchControl) {
    record("step5", "PASS", "自启动开关与文件监听都是真控件", evidence);
  } else {
    const missing = [];
    if (!hasAutostartSwitch) missing.push('自启动开关（缺 selector：`[data-testid="settings-section-startup"]` 内的 input[type=checkbox] / [role=switch]）');
    if (!hasFileWatchControl) missing.push('文件监听控件（缺 selector：`[data-testid="settings-section-tray"]` 内的 input[type=checkbox] / [role=switch]）');
    record("step5", "SKIP", `界面缺控件：${missing.join("；")}`, evidence);
  }
}

// ---------------------------------------------------------------- step6 真写落库（附加证据）

/**
 * 任务书要求的三个写操作在界面里都没有入口（step2/3/4 的 SKIP）。
 * 但"真界面 → 真 IPC → 真库"这条链路本身**可以**被证明：
 * 设置页有真实写路径（改字段 → `config_apply`）。这一步把它跑一遍，
 * 用来区分两种可能：**"没有写入口"** vs **"有入口但写不进去"**。
 */
async function step6(wd) {
  const evidence = [];
  await gotoPage(wd, "#/settings", '[data-testid="settings-page"]');
  const before = dumpScratchDb("step6-before");
  evidence.push(`写前 db(config) 行数=${(before.config ?? []).length} 指纹=${dbFingerprint(before)}`);

  const field = await wd.findElement('[data-testid="settings-field-autostartDelay"] input');
  const fieldFallback = field ?? (await wd.findElement('[data-testid="settings-field-autostartDelay"]'));
  if (fieldFallback === null) {
    evidence.push('缺 selector: [data-testid="settings-field-autostartDelay"]');
    record("step6", "SKIP", "设置页没有可编辑的整数字段，无法用真界面证明落库", evidence);
    return;
  }
  const oldValue = await wd.attr(fieldFallback, "value");
  await wd.fill(fieldFallback, "17");
  evidence.push(`字段 [data-testid="settings-field-autostartDelay"] : ${JSON.stringify(oldValue)} → "17"（真输入）`);
  await sleep(400);
  const dirtyEl = await wd.findElement('[data-testid="settings-dirty"]');
  if (dirtyEl) evidence.push(`settings-dirty: ${JSON.stringify((await wd.text(dirtyEl)).replace(/\s+/g, " "))}`);

  const apply = await wd.findElement('[data-testid="settings-apply"]');
  if (apply === null) {
    evidence.push('缺 selector: [data-testid="settings-apply"]');
    record("step6", "SKIP", "设置页没有「校验并应用」按钮", evidence);
    return;
  }
  evidence.push(`settings-apply enabled=${await wd.enabled(apply)}`);
  await wd.click(apply);
  await sleep(2500);
  await shot(wd, "step6-after-apply");

  const tids = await wd.probeTestIds();
  for (const t of ["settings-validate-report", "settings-apply-result", "settings-dirty", "settings-local-issues"]) {
    const txt = await wd.textOf(`[data-testid="${t}"]`);
    if (txt !== null) evidence.push(`${t}: ${JSON.stringify(txt.replace(/\s+/g, " ").slice(0, 400))}`);
    else evidence.push(`${t}: 不存在 (额外 testid: ${tids.includes(t)})`);
  }

  const after = dumpScratchDb("step6-after");
  evidence.push(`写后 db(config) 行数=${(after.config ?? []).length} 指纹=${dbFingerprint(after)}`);
  const hit = (after.config ?? []).filter((r) => JSON.stringify(r).includes("autostart_delay_s"));
  evidence.push(`db 里含 autostart_delay_s 的行: ${JSON.stringify(hit).slice(0, 600)}`);
  evidence.push(`store_setting: ${JSON.stringify(after.store_setting ?? []).slice(0, 400)}`);

  // ★ `config_apply` 是**全量覆盖**：它只能改用户在这页动过的东西，绝不能把别的写路径
  //   （/network 的线路、/channels 的通道、向导写的服务商与订阅…）刚建好的实体抹掉。
  //   真机实测踩到过：点一次「应用」把 `lane/ta-e2e-lane` 整条删了（页面那份 doc 是旧的）。
  //   这条断言就是那个用户可见数据丢失的回归闸门 —— 只允许**新增/修改**，不允许**消失**。
  const key = (r) => `${r.kind}/${r.entity_id}`;
  const beforeKeys = new Set((before.config ?? []).map(key));
  const lost = [...beforeKeys].filter((k) => !(after.config ?? []).some((r) => key(r) === k));
  evidence.push(`应用前后实体集合: 之前 ${beforeKeys.size} 条 → 之后 ${(after.config ?? []).length} 条；丢失=${JSON.stringify(lost)}`);

  const changed = dbFingerprint(before) !== dbFingerprint(after);
  const wrote17 = JSON.stringify(after.config ?? []).includes('"autostart_delay_s":17');
  if (lost.length > 0) {
    record("step6", "FAIL", `「校验并应用」抹掉了别的写路径建好的实体：${lost.join(", ")}（config_apply 是全量覆盖，页面拿的是旧 doc）`, evidence);
  } else if (changed && (wrote17 || hit.length > 0)) {
    record("step6", "PASS", "真界面真输入 + 点击「校验并应用」→ config_apply 真落库（db 内容变化，且没抹掉别人的实体）", evidence);
  } else {
    record("step6", "FAIL", `点应用后 db 指纹未变（changed=${changed}, wrote17=${wrote17}）`, evidence);
  }
}

// ---------------------------------------------------------------- 收尾隔离复查

async function stepIsolationAfter(wd) {
  const evidence = [];
  const after = snapshotDir(REAL_DATA_DIR, { hash: ["db.sqlite"] });
  const before = JSON.parse(readFileSync(join(OUT, "real-data-before.json"), "utf8"));
  const changed = diffSnapshots(before, after);
  const dump = dumpScratchDb("final");
  evidence.push(`真实目录 ${REAL_DATA_DIR} 文件级 diff: ${changed.length === 0 ? "无变化" : changed.join(" | ")}`);
  evidence.push(`scratch db 表数=${(dump.tables ?? []).length}，config 行数=${(dump.config ?? []).length}，subscription 行数=${(dump.subscription ?? []).length}`);
  evidence.push(`scratch db 路径: ${dump.dbPath ?? "(缺失)"}`);
  await shot(wd, "step7-final");
  if (changed.length === 0) {
    record("step7", "PASS", "全程结束后用户真实数据目录仍未被改动（隔离自始至终成立）", evidence);
  } else {
    record("step7", "FAIL", "全程结束后真实数据目录被改动 —— 隔离被突破", evidence);
  }
}

// ================================================================ 报告

function gitHead() {
  const r = spawnSync("git", ["-C", REPO, "log", "-1", "--format=%H %ad %s", "--date=iso"], { encoding: "utf8" });
  return r.stdout?.trim() || "(git 不可用)";
}

function distMtime() {
  const p = join(REPO, "dist", "index.html");
  return existsSync(p) ? new Date(statSync(p).mtimeMs).toISOString() : "(无 dist)";
}

function finish(code) {
  const report = {
    at: new Date().toISOString(),
    out: OUT,
    scratch: SCRATCH,
    results,
  };
  writeFileSync(join(OUT, "report.json"), JSON.stringify(report, null, 2));

  const lines = [];
  lines.push("");
  lines.push("================ TokenAgency 真机 E2E 结果 ================");
  const counts = { PASS: 0, SKIP: 0, FAIL: 0 };
  for (const r of results) {
    counts[r.status] = (counts[r.status] ?? 0) + 1;
    lines.push(`${r.status.padEnd(4)} ${String(r.step).padEnd(7)} ${r.title}`);
    lines.push(`     ${r.detail}`);
  }
  lines.push("----------------------------------------------------------");
  lines.push(`PASS=${counts.PASS}  SKIP=${counts.SKIP}  FAIL=${counts.FAIL}`);
  lines.push(`产物: ${OUT}  (webdriver/ shots/ db/ logs/ report.json)`);
  lines.push("==========================================================");
  process.stdout.write(lines.join("\n") + "\n");
  process.exit(code);
}

main().catch((e) => {
  process.stderr.write(`fatal: ${e?.stack ?? e}\n`);
  finish(1);
});

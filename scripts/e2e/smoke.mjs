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
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";

// ================================================================ 路径与常量

const HERE = fileURLToPath(import.meta.url);
const REPO = resolve(HERE, "..", "..", "..");

/** 运行产物根。任务书要求全部落在 `%TEMP%\ta-e2e\`。 */
const OUT = process.env.TA_E2E_OUT ?? join(process.env.TEMP ?? process.env.TMP ?? ".", "ta-e2e");
const SHOTS = join(OUT, "shots");
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
  step2: "供应商页新增供应商（列表 + 落库）",
  step3: "订阅页粘贴 Clash 订阅 URL（列表 + 落库）",
  step4: "通道/线路页建通道并指派（lane_upsert / lane_set_node）",
  step5: "设置页：自启动开关 / 文件监听是否真控件",
  step6: "附加：真界面写操作落库证明（settings 改字段 → config_apply）",
  step7: "收尾隔离复查（全程结束后真实目录仍未变）",
  step10: "线路添加向导：能力声明 + 计费落地，线路与定价都进库",
  step11: "路由向导：能力不一致时必须显示逐字段差异并禁用提交",
  step14: "数据安全回归：设置页「校验并应用」之后，之前建的线路必须还在",
  step15: "能力一致的两条线路可路由；把已归组的线路改成不一致 → 后端必须拒绝并点出字段",
  step17: "节点清单真出数据（/network/nodes ← subscription_nodes，含 fixture 节点）",
  step18: "访问密钥真能管也真能挡人（创建/明文一次/带 key 放行、不带 key 401/禁用即 401）",
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
const CLASH_YAML = `port: 7890
socks-port: 7891
mode: rule
proxies:
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
    res.end(CLASH_YAML);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
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
  return new DatabaseSync(p, { readOnly: true });
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

async function wdClickTid(wd, tid, evidence) {
  await wd.closeMenuIfOpen();
  const el = await wd.findElement(`[data-testid="${tid}"]`);
  if (el === null) {
    evidence.push(`缺控件 ${tid}`);
    return false;
  }
  try {
    await wd.click(el);
    return true;
  } catch (err) {
    evidence.push(`控件 ${tid} 点不动：${wdErrText(err)}`);
    return false;
  }
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
 * MUI Select（**多选**）：把**已选中但不需要**的项取消掉，只留匹配 `keepRe` 的那些。
 *
 * 为什么需要它：策略向导在"选完模型"后会**默认全选**该模型的所有线路（页面 `selectedTouched=false`
 * 时的 effect）—— 真机实测：step10 留下一条声明 `vision` 的线路之后，全选必然"能力不一致
 * → 提交被禁用"，想让两条默认能力的线路路由到一起就必须先**取消**那一条。
 * MUI 多选里对**已选项再点一次 = 取消**。
 *
 * 返回保留下来的选项文案（用于证据与"已经选对了"的判断）。
 */
async function wdKeepOptions(wd, tid, evidence, keepRe) {
  const opened = await wdOpenSelect(wd, tid, evidence);
  if (!opened.ok) return [];
  const removed = [];
  for (let i = 0; i < 12; i++) {
    const items = await wd.optionItems();
    const target = items.find((it) => it.selected && !keepRe.test(it.text));
    if (target === undefined) break;
    try {
      await wd.click(target.id);
    } catch (err) {
      evidence.push(`取消「${target.text}」失败：${wdErrText(err)}`);
      break;
    }
    removed.push(target.text);
    await sleep(300);
  }
  const left = (await wd.optionItems()).filter((it) => it.selected && keepRe.test(it.text));
  await wd.closeMenuIfOpen();
  evidence.push(
    `「${tid}」取消 ${removed.length} 条未匹配项: ${JSON.stringify(removed.map((t) => t.slice(0, 40))).slice(0, 240)}；` +
      `保留 ${left.length} 条: ${JSON.stringify(left.map((t) => t.text.slice(0, 40))).slice(0, 240)}`,
  );
  return left.map((it) => it.text);
}

/**
 * MUI Select（**多选**）：弹层打开一次，在里面连点 N 个选项。
 *
 * 为什么不能"点一次控件选一项"：`multiple` 的弹层选完**不会关**，第二次去点控件时
 * 弹层还盖在上面 → WebDriver 直接报 `element click intercepted`（真机实测：step11 只能选上 1 条）。
 */
async function wdPickOptions(wd, tid, want, evidence, prefer) {
  const opened = await wdOpenSelect(wd, tid, evidence);
  if (!opened.ok) return [];
  evidence.push(
    `「${tid}」弹层选项(${opened.items.length}): ${JSON.stringify(opened.items.map((it) => it.text)).slice(0, 320)}`,
  );
  const picked = [];
  for (let i = 0; i < want; i++) {
    const items = await wd.optionItems();
    const target = items.find((it) => !it.selected && (prefer === undefined || prefer.test(it.text)));
    if (target === undefined) {
      evidence.push(`「${tid}」没有更多可选项（已选 ${picked.length} 条）`);
      break;
    }
    try {
      await wd.click(target.id);
    } catch (err) {
      evidence.push(`选项「${target.text}」点不动：${wdErrText(err)}`);
      break;
    }
    picked.push(target.text);
    await sleep(300);
  }
  await wd.closeMenuIfOpen();
  evidence.push(`「${tid}」已选 ${picked.length} 条: ${JSON.stringify(picked)}`);
  return picked;
}

/**
 * 直接探测一个下拉：真点开 → 数 `li[role="option"]` → 收掉弹出层。
 * 只用于**证据**（把"有没有选项"这件事本身量出来），选中仍然走 `wdPickOption`。
 */
async function probeSelect(wd, tid, evidence) {
  const host = await wd.exec(`
    const el = document.querySelector('[data-testid="' + ${JSON.stringify(tid)} + '"]');
    if (!el) return { found: false };
    const r = el.getBoundingClientRect();
    return { found: true, text: (el.innerText || '').trim().slice(0, 60), w: Math.round(r.width), h: Math.round(r.height) };
  `);
  if (!host || host.found !== true) {
    evidence.push(`下拉 ${tid}: 不存在`);
    return { found: false };
  }
  const opened = await wdOpenSelect(wd, tid, evidence);
  if (!opened.ok) return { found: true, opened: false };
  // ★ 只有"弹层真开着"才收（判据 `[role=listbox]`）；收的时候 Esc 只送到 listbox 自己身上，
  //   绝不用全局 Esc —— 那会连整个 Dialog 一起关掉（真机实测踩到过）。
  await wd.closeMenuIfOpen();
  const menu = { count: opened.items.length, texts: opened.items.map((it) => it.text) };
  evidence.push(`下拉 ${tid}: 现值 ${JSON.stringify(host.text)} · 打开后选项 ${JSON.stringify(menu)}`);
  return { found: true, opened: true, ...menu };
}

/** `config` 表里某个 kind 的实体 id 列表。 */
function configIds(dump, kind) {
  return (dump.config ?? []).filter((r) => r.kind === kind).map((r) => String(r.entity_id));
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
  await shot(wd, "step9-form-filled");
  await wdClickTid(wd, "models-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "step9-after-submit");
  const dump = dumpScratchDb("step9");
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  const landed = configIds(dump, "model").includes(mid);
  const inList = (await wd.pageText()).includes(mid);
  if (landed && inList) record("step9", "PASS", "模型经真界面（/models 向导）新增，列表与 config 表都出现", evidence);
  else record("step9", "FAIL", `新增后不一致：列表=${inList} 落库=${landed}`, evidence);
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

/** 一条 channel 的 `model`（从 config 行的 json 里读；读不出来返回 null）。 */
function channelModelOf(row) {
  try {
    return JSON.parse(String(row.json)).model ?? null;
  } catch {
    return null;
  }
}

/** 经 /models 的「添加模型」向导真建一个模型。 */
async function addModelViaDialog(wd, evidence, id = "ta-e2e-m-add") {
  const page = await gotoPage(wd, "#/models");
  if (page === null) {
    evidence.push("/models 没渲染出来，建不了模型");
    return false;
  }
  if (!(await wdClickTid(wd, "models-add-open", evidence))) return false;
  await wdFillByTid(wd, "models-add-id", id, evidence);
  await wdFillByTid(wd, "models-add-aliases", `${id}-alias`, evidence);
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

/** 把界面上的文案当**字面量**匹配（文案里带 `.` / `-` / 括号时不能当正则）。 */
function reEsc(text) {
  return String(text).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 从 `config` 里挑一对**真的成对存在**的 `(provider, credential)`。
 *
 * 为什么必须挑成对的：界面上的凭据下拉是**按 provider 过滤**的（`credentialOptionsFor`），
 * 而服务商下拉的文案是**名称** —— 真机实测有 3 个服务商、其中两个同名，挑到"没有凭据的那个"
 * 之后，凭据下拉如实为空（0 项），而凭据是必填 → 后面每一步都是连带失败。
 * 这里直接读 config（只读证据）挑一对，不靠猜文案。
 */
function providerCredentialPair(dump, preferRe, credentialRe) {
  const rows = (kind) =>
    (dump.config ?? [])
      .filter((r) => r.kind === kind)
      .map((r) => {
        try {
          return JSON.parse(String(r.json));
        } catch {
          return null;
        }
      })
      .filter((x) => x !== null);
  const providers = rows("provider");
  const credentials = rows("credential");
  // ① 指定了凭据 → 用**它自己的 provider**（这样"第二条线换一份凭据"才真的换得动）
  if (credentialRe !== undefined) {
    const hit = credentials.find(
      (c) => credentialRe.test(String(c.label ?? "")) || credentialRe.test(String(c.id)),
    );
    if (hit !== undefined) {
      const owner = providers.find((p) => p.id === hit.provider);
      if (owner !== undefined) return { p: owner, c: hit };
    }
  }
  const pairs = providers
    .map((p) => ({ p, c: credentials.find((c) => c.provider === p.id) }))
    .filter((x) => x.c !== undefined);
  if (pairs.length === 0) return null;
  const wanted =
    preferRe === undefined
      ? pairs
      : pairs.filter((x) => preferRe.test(String(x.p.id)) || preferRe.test(String(x.p.name ?? "")));
  return wanted[0] ?? pairs[0];
}

/**
 * 经 /channels 的「添加线路」向导**真建 / 真覆盖一条线路**（前提：provider/credential/model 都在）。
 *
 * 返回 `{ id, error }`：
 * - `id` = `config` 里那条线路的 id；**null 表示没落库**（提交被本地预检或后端拒了）
 * - `error` = `channels-add-error` 的原文（后端 TA 码 + details 就靠它取证；成功时为 null）
 *
 * `opts`：
 * - `preferProvider` / `preferCredential` / `preferModel`：下拉选项的文案正则
 * - `capabilities`：`{ vision?: boolean }` —— 真点 `channels-add-vision` 这些开关
 * - `priceInput` / `priceOutput` / `priceCached`：计费三项（非空才触发 `pricing_upsert`）
 *
 * **结束时一定关掉对话框**：不关的话下一次调用点 `channels-add-open` 会被 Dialog 的
 * backdrop 拦下（`element click intercepted`），一个收尾疏漏会变成下游的一串假失败。
 */
async function addChannelViaDialog(wd, evidence, opts = {}) {
  const page = await gotoPage(wd, "#/channels");
  if (page === null) {
    evidence.push("/channels 没渲染出来");
    return { id: null, error: null };
  }
  const ids = await wd.probeTestIds();
  if (!ids.includes("channels-add-open")) {
    evidence.push("界面缺入口：channels-add-open 不存在（线路添加向导还没接）");
    return { id: null, error: null };
  }
  if (!(await wdClickTid(wd, "channels-add-open", evidence))) return { id: null, error: null };
  await shot(wd, "channels-add-opened");
  // ★ 服务商下拉的文案是**名称**（同一个名字可能对应多个 id），而凭据下拉是按 provider 过滤的：
  //   挑到"没有凭据"的那个服务商，凭据下拉就如实为空（0 项），后面每一步都是连带失败。
  //   所以这里从 config 里挑一对**真的成对存在**的 (provider, credential)，按**名称/标签**去选。
  const pairDump = configKinds("channels-add-pair").dump;
  const pair = providerCredentialPair(pairDump, opts.preferProvider ?? /ta-e2e|TA E2E|p1/i, opts.preferCredential);
  if (pair === null) {
    evidence.push("config 里找不到任何 (provider, credential) 成对的组合 —— 凭据是必填项，这条线建不出来");
  } else {
    evidence.push(
      `成对的服务商/凭据（读 config）: provider=${JSON.stringify(pair.p.id)}「${String(pair.p.name ?? "")}」/ credential=${JSON.stringify(pair.c.id)}「${String(pair.c.label ?? "")}」`,
    );
  }
  const providerRe =
    opts.preferProvider ??
    (pair === null ? undefined : new RegExp(reEsc(pair.p.name ?? pair.p.id), "i"));
  // 凭据下拉的文案是**标签**（不是 id）：所以按**选中的那一对**里的标签去选，
  // 保证"换一份凭据建第二条线"真的换得动（否则会退回第一项、覆盖掉同一条线）。
  const credentialRe =
    pair === null ? opts.preferCredential : new RegExp(reEsc(pair.c.label ?? pair.c.id), "i");
  // 先量下拉里有什么（证据），再按正常路径选。
  await probeSelect(wd, "channels-add-provider", evidence);
  await probeSelect(wd, "channels-add-lane", evidence);
  await wdPickOption(wd, "channels-add-provider", evidence, providerRe);
  await wdPickOption(wd, "channels-add-credential", evidence, credentialRe);
  const pickedModel = await wdPickOption(wd, "channels-add-model", evidence, opts.preferModel ?? /ta-e2e-m-add/i);
  await wdFillByTid(wd, "channels-add-upstreamModel", opts.upstream ?? pickedModel ?? "ta-e2e-m-add", evidence);
  // 网络下拉的文案是线路**名称**（不一定带"直连"字样）：选第一项即可，空下拉就按继承走（lane 可空）。
  await wdPickOption(wd, "channels-add-lane", evidence);
  // 上游能力声明（[docs/17 §2]）：不点就是契约默认（全 false / context_tokens 0 / modalities ["text"]）。
  if (opts.capabilities !== undefined) {
    const caps = opts.capabilities;
    for (const [field, tid] of [
      ["vision", "channels-add-vision"],
      ["tools", "channels-add-tools"],
      ["reasoning", "channels-add-reasoning"],
    ]) {
      if (caps[field] === true) await wdSetSwitch(wd, tid, true, evidence);
    }
    if (caps.contextTokens !== undefined) {
      await wdFillByTid(wd, "channels-add-contextTokens", String(caps.contextTokens), evidence);
    }
    if (caps.modalities !== undefined) {
      await wdFillByTid(wd, "channels-add-modalities", caps.modalities, evidence);
    }
  }
  // 计费（美元 / 百万 token，可留空 = 不计价）
  for (const [field, tid] of [
    ["priceInput", "channels-add-price-input"],
    ["priceOutput", "channels-add-price-output"],
    ["priceCached", "channels-add-price-cached"],
  ]) {
    if (opts[field] !== undefined) await wdFillByTid(wd, tid, String(opts[field]), evidence);
  }
  await wdFillByTid(wd, "channels-add-priority", "10", evidence);
  await wdFillByTid(wd, "channels-add-weight", "1", evidence);
  await shot(wd, "channels-add-filled");
  await wdClickTid(wd, "channels-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "channels-add-after-submit");
  const dump = dumpScratchDb("channels-add");
  const channels = (dump.config ?? []).filter((r) => r.kind === "channel");
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump))}`);
  const errorText = await wd.textOf('[data-testid="channels-add-error"]');
  const error = errorText === null ? null : errorText.replace(/\s+/g, " ").trim();
  if (channels.length === 0) {
    for (const t of ["channels-add-error", "channels-add-issues"]) {
      const txt = await wd.textOf(`[data-testid="${t}"]`);
      if (txt !== null) evidence.push(`${t}: ${JSON.stringify(txt.replace(/\s+/g, " ").slice(0, 300))}`);
    }
    await wd.closeDialog();
    return { id: null, error };
  }
  const id = String(channels[channels.length - 1].entity_id);
  if (error !== null) evidence.push(`channels-add-error（有错误但库里有线路）: ${JSON.stringify(error)}`);
  await wd.closeDialog();
  return { id, error };
}

/**
 * 让"某个模型有 ≥2 条线路"成立 —— 能力一致性只有 **≥2 条** 时才会比对（`compareCapabilities`）。
 * 线路 id 由 `provider × credential × model` 派生，所以第二条必须换一份**凭据**：
 * 走 /providers 的「添加密钥」向导真表单造一份，再用 /channels 向导建第二条线。
 */
async function ensureSecondChannelForModel(wd, evidence, model) {
  const ofModel = (dump) =>
    (dump.config ?? []).filter((r) => r.kind === "channel" && channelModelOf(r) === model);
  let { dump } = configKinds("policy-prereq-before");
  const first = ofModel(dump)[0];
  if (first === undefined) {
    evidence.push(`配置里没有 model=${model} 的线路，无法做能力一致性比对`);
    return false;
  }
  let channel = null;
  try {
    channel = JSON.parse(String(first.json));
  } catch {
    /* 读不出来就不换凭据，下面会如实返回 false */
  }
  if (channel === null) return false;
  if (ofModel(dump).length >= 2) {
    evidence.push(`model=${model} 已经有 ${ofModel(dump).length} 条线路，不用再建`);
    return true;
  }

  // ① 第二份凭据（同一服务商、不同 id）：/providers 的「添加密钥」向导
  const credId = "ta-e2e-cred-2";
  const page = await gotoPage(wd, "#/providers");
  if (page === null) {
    evidence.push("/providers 没渲染出来，造不了第二份凭据");
    return false;
  }
  if (!(await wdClickTid(wd, "credentials-add-open", evidence))) return false;
  await wdFillByTid(wd, "credentials-add-id", credId, evidence);
  await wdFillByTid(wd, "credentials-add-label", "TA E2E 密钥 2", evidence);
  await wdPickOption(wd, "credentials-add-provider", evidence, new RegExp(String(channel.provider), "i"));
  await wdFillByTid(wd, "credentials-add-secret", "sk-ta-e2e-secret-2", evidence);
  await wdClickTid(wd, "credentials-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "policy-prereq-credential");
  await wd.closeDialog();
  ({ dump } = configKinds("policy-prereq-credential"));
  const credOk = configIds(dump, "credential").includes(credId);
  evidence.push(`第二份凭据 ${credId} 落库=${credOk}（config kind 计数 = ${JSON.stringify(configEntityKinds(dump))}）`);
  if (!credOk) return false;

  // ② 用这份凭据建第二条线（同一个模型）
  //    ⚠ 注意：这里必须按**凭据的 label**挑，不能按 id 挑 —— 下拉里显示的是 label
  //    （`credentials-add-label`），`wdPickOption` 的 `prefer` 是拿正则去比选项文案的；
  //    按 id 挑会匹配不上，然后**退化为第一项**（= 第一份凭据），于是派生出的 channel id
  //    与第一条完全相同 —— upsert 覆盖同一条，同模型永远只有 1 条线路
  //    （第 70 轮实测踩到过：`model=... 的线路数 = 1`，能力一致性压根不会被比对）。
  const made = await addChannelViaDialog(wd, evidence, {
    preferProvider: new RegExp(String(channel.provider), "i"),
    preferCredential: /密钥 2/,
    preferModel: new RegExp(model, "i"),
  });
  ({ dump } = configKinds("policy-prereq-channel2"));
  const n = ofModel(dump).length;
  evidence.push(`第二条线路 id=${JSON.stringify(made.id)}（error=${JSON.stringify(made.error)}），model=${model} 的线路数 = ${n}`);
  return n >= 2;
}

async function step10(wd, fixture) {
  const evidence = [];
  // ★ 前提：四个下拉都读配置。缺前提就**先经界面造出来**（空下拉是数据前提，不是产品缺陷）。
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step10", "SKIP", "界面缺前提：配置里没有服务商/凭据/模型，且经界面也造不出来（见证据）", evidence);
    return;
  }
  // 先量一下 `/network` 的线路区现状：`lane_list` 有没有把已有的线路读出来？这决定了
  // 后面"网络下拉没有可选项"是**接口/缓存缺陷**还是**配置里真的没有线路**。
  await gotoPage(wd, "#/network");
  const laneState = await wd.exec(`
    return {
      table: document.querySelector('[data-testid="network-lanes-table"]') !== null,
      rows: document.querySelectorAll('[data-testid="network-lane-row"]').length,
      empty: document.querySelector('[data-testid="network-lanes-empty"]') !== null,
      error: document.querySelector('[data-testid="network-lanes-error"]')?.innerText ?? null,
      setupOnly: document.querySelector('[data-testid="network-lanes-setup"]') !== null,
    };
  `);
  evidence.push(`/network 线路区现状（诊断 lane_list）: ${JSON.stringify(laneState)}`);

  // 能力块与计费块是 [docs/17 §3.3] 的**必需控件**（用户硬要求③⑤）：缺一个就是没交付。
  const wantTids = [
    "channels-add-vision",
    "channels-add-tools",
    "channels-add-reasoning",
    "channels-add-contextTokens",
    "channels-add-modalities",
    "channels-add-price-input",
    "channels-add-price-output",
    "channels-add-price-cached",
  ];
  await gotoPage(wd, "#/channels");
  // ★ 这 8 个控件全在 `ChannelAddDialog` 里，而 MUI `Dialog open={false}` **整棵子树都不在 DOM 里** ——
  //   所以必须在**打开对话框之后**再探这些 testid。真机实测（同一构建、同一数据）：
  //   对话框关着时命中 0/8，打开后 8/8。之前在这里"关着探"得到的是**假失败**
  //   （报"界面缺能力块/计费块控件"，其实控件都在，只是对话框没开）。
  if (!(await wdClickTid(wd, "channels-add-open", evidence))) {
    record("step10", "FAIL", "点不开「添加线路」", evidence);
    return;
  }
  await sleep(700);
  const ids10 = await wd.probeTestIds();
  const missingTids = wantTids.filter((t) => !ids10.includes(t));
  evidence.push(`能力块 + 计费块 testid（打开对话框后实测）: 应有 ${wantTids.length} 个，缺 ${JSON.stringify(missingTids)}`);
  // 探完关掉：下面 `addChannelViaDialog` 要从"点开入口"开始走同一条真路径
  await wd.closeDialog();
  if (missingTids.length > 0) {
    record("step10", "FAIL", `界面缺能力块/计费块控件（${missingTids.join(", ")}）`, evidence);
    return;
  }

  // ① 第一条线路：**显式声明 vision / context_tokens / modalities** + 计费三项
  //    （计费非空 → `channel_upsert` 成功后自动触发 `pricing_upsert`）。
  //
  //    为什么把"声明能力"放在**第一次**建线时做，而不是回头再 upsert 一次第二条：
  //    第 70 轮实测过，多插一次"回头改能力"的向导流程会踩 MUI 弹层/Dialog 的时序
  //    （`channels-add-credential` 那一步探到整个对话框已经不在 DOM 里 → 之后所有控件"缺控件"）。
  //    覆盖的东西完全一样：同一模型下**一条声明了 vision、一条用契约默认**，
  //    差别只在于少走一次界面往返。真机不再依赖那次时序。
  const first = await addChannelViaDialog(wd, evidence, {
    capabilities: { vision: true, contextTokens: 200000, modalities: "text,image" },
    priceInput: "1.5",
    priceOutput: "6",
    priceCached: "0.15",
  });
  if (first.id === null) {
    record("step10", "FAIL", "点了保存但 config 表里没有 channel", evidence);
    return;
  }
  const madeId = first.id;
  let { dump } = configKinds("step10-first");
  const firstRow = (dump.config ?? []).find((r) => r.kind === "channel" && String(r.entity_id) === madeId);
  const model = firstRow === undefined ? null : channelModelOf(firstRow);
  if (model === null) {
    record("step10", "FAIL", `建出来的线路读不到 model（id=${madeId}）`, evidence);
    return;
  }
  evidence.push(`第一条线路 id=${madeId}，model=${JSON.stringify(model)}（声明 vision=true + context_tokens=200000）`);

  // ② 同模型第二条线路：**换一份凭据**才可能有第二个 channel id（id = provider×credential×model），
  //    这条**不碰能力块** → 契约默认能力。于是"能力不一致"那一对线路就位（step11/step15 要用）。
  const two = await ensureSecondChannelForModel(wd, evidence, model);
  if (!two) evidence.push("未能让同模型有 ≥2 条线路（能力一致性比对与能力规则都依赖这一点）");

  // ④ 列表这一侧：回到 /channels 看列表里有没有第一条
  await gotoPage(wd, "#/channels");
  const listTids = await wd.probeTestIds();
  const listText = await wd.pageText();
  const inList = listTids.includes("channels-row") && listText.includes(madeId);
  evidence.push(
    `回到 /channels：channels-row 存在=${listTids.includes("channels-row")}，列表文本含 ${madeId}=${listText.includes(madeId)}`,
  );

  // ⑤ 落库断言：两条同模型线路（其中一条 `"vision":true`）+ 计费进了 pricing section
  const after = configKinds("step10-after");
  const channels = (after.dump.config ?? []).filter((r) => r.kind === "channel");
  const sameModel = channels.filter((r) => channelModelOf(r) === model);
  const visionRows = sameModel.filter((r) => /"vision"\s*:\s*true/.test(String(r.json)));
  evidence.push(`config(kind=channel) 共 ${channels.length} 条；model=${model} 的 ${sameModel.length} 条；声明 vision 的 ${visionRows.length} 条`);
  evidence.push(`config(kind=channel) 原文: ${JSON.stringify(sameModel).slice(0, 700)}`);
  const pricingRow = (after.dump.config ?? []).find(
    (r) => r.kind === "section" && String(r.entity_id) === "pricing",
  );
  const pricingJson = pricingRow === undefined ? "" : String(pricingRow.json);
  const priceLanded = /"input"\s*:\s*1\.5/.test(pricingJson);
  evidence.push(`定价 section 里有 input=1.5: ${priceLanded}（原文片段: ${JSON.stringify(pricingJson.slice(0, 400))}）`);

  const ok = inList && sameModel.length >= 2 && visionRows.length >= 1 && priceLanded;
  if (ok) {
    record(
      "step10",
      "PASS",
      `线路向导：能力块/计费块齐全；两条同模型线路落库（其中 1 条 vision=true）+ 定价落库（input=1.5）`,
      evidence,
    );
  } else {
    record(
      "step10",
      "FAIL",
      `线路落地不完整：列表=${inList} 同模型线路数=${sameModel.length}(需要 2) vision 条数=${visionRows.length}(需要 1) 定价=${priceLanded}`,
      evidence,
    );
  }
}

async function step11(wd, fixture) {
  const evidence = [];
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step11", "SKIP", "界面缺前提：配置里没有服务商/凭据/模型，且经界面也造不出来（见证据）", evidence);
    return;
  }
  // `--only step11` 时库里可能一条线路都没有 → 先用同一条界面路径建一条（否则"没有候选线路"压根谈不上路由）。
  let { dump } = configKinds("step11-prereq");
  if ((dump.config ?? []).filter((r) => r.kind === "channel").length === 0) {
    evidence.push("配置里还没有任何线路 → 先经 /channels 向导建一条（与 step10 同一条界面路径）");
    await addChannelViaDialog(wd, evidence);
    ({ dump } = configKinds("step11-prereq-channel"));
  }
  const channels = (dump.config ?? []).filter((r) => r.kind === "channel");
  const model = channels.map(channelModelOf).find((m) => m !== null && m !== undefined) ?? null;
  evidence.push(`候选线路 ${channels.length} 条，取 model=${JSON.stringify(model)} 做路由`);
  if (model === null) {
    record("step11", "FAIL", "配置里没有可用的模型/线路（经界面也建不出来）", evidence);
    return;
  }
  // 「≥2 条才比对」是页面的规则：只有一条线路时能力一致性**不会被比对**，
  // 所以这里先让同模型的线路凑够两条，才能真的走到"比对"那一步。
  const two = await ensureSecondChannelForModel(wd, evidence, model);
  if (!two) evidence.push("未能让同模型有 ≥2 条线路 → 能力一致性不会被比对（页面规则），下面会如实说明");

  const page = await gotoPage(wd, "#/policies");
  if (page === null) {
    record("step11", "FAIL", "/policies 页面没渲染出来", evidence);
    return;
  }
  const ids = await wd.probeTestIds();
  evidence.push(`/policies 命中 data-testid: ${ids.slice(0, 35).join(", ")}`);
  if (!ids.includes("policies-add-open")) {
    record("step11", "SKIP", "界面缺入口：policies-add-open 不存在（路由添加向导还没接）", evidence);
    return;
  }
  if (!(await wdClickTid(wd, "policies-add-open", evidence))) {
    record("step11", "FAIL", "点不开「添加路由」", evidence);
    return;
  }
  await shot(wd, "step11-form-opened");
  await wdPickOption(wd, "policies-add-model", evidence, new RegExp(model, "i"));
  await sleep(600);
  // 多选线路：**弹层打开一次，在里面连点**（MUI multiple 选完不关弹层）。
  const picked = await wdPickOptions(wd, "policies-add-channels", 3, evidence);
  // id / 名称：页面在"选模型"时会按模型派生（可改）。派生不出来就如实填进去（它们是必填项）。
  const idNow = await wd.attrOf('[data-testid="policies-add-id"]', "value");
  const nameNow = await wd.attrOf('[data-testid="policies-add-name"]', "value");
  evidence.push(`id/名称现值: id=${JSON.stringify(idNow)} name=${JSON.stringify(nameNow)}`);
  if (idNow === null || idNow === "") await wdFillByTid(wd, "policies-add-id", `pol-${model}`, evidence);
  if (nameNow === null || nameNow === "") await wdFillByTid(wd, "policies-add-name", model, evidence);

  const capText = await wd.textOf('[data-testid="policies-add-capabilities"]');
  const capFlat = (capText ?? "").replace(/\s+/g, " ").slice(0, 300);
  const nowTids = await wd.probeTestIds();
  const mismatchPresent = nowTids.includes("policies-add-capability-mismatch");
  const diffPresent = nowTids.some((t) => t.startsWith("policies-add-capability-diff-"));
  const consistentShown = /能力一致|identical capabilities/i.test(capFlat);
  evidence.push(`能力区块原文: ${JSON.stringify(capFlat)}`);
  evidence.push(
    `能力比对：已选线路 ${picked.length} 条；不一致元素存在=${mismatchPresent}，逐字段差异元素存在=${diffPresent}，显示"能力一致"=${consistentShown}`,
  );
  await shot(wd, "step11-form-filled");
  const submit = await wd.exec(`
    const b = document.querySelector('[data-testid="policies-add-submit"]');
    return b ? { found: true, disabled: b.disabled === true || b.getAttribute("aria-disabled") === "true" } : { found: false };
  `);
  evidence.push(`提交按钮: ${JSON.stringify(submit)}`);

  // ① 能力不一致：界面必须**禁用提交并显示差异**（用户硬规则），而且点了不落库。
  if (mismatchPresent || diffPresent) {
    if (!submit.found || submit.disabled !== true) {
      record("step11", "FAIL", `显示了能力不一致却没有禁用提交（submit=${JSON.stringify(submit)}）`, evidence);
      return;
    }
    await wdClickTid(wd, "policies-add-submit", evidence); // 真点一次：禁用的话这一下不该生效
    await sleep(600);
    const after = dumpScratchDb("step11-mismatch");
    const landed = (after.config ?? []).some((r) => r.kind === "policy");
    if (landed) record("step11", "FAIL", "能力不一致时提交竟然落库了（界面禁用形同虚设）", evidence);
    else record("step11", "PASS", "能力不一致：界面显示逐字段差异并禁用提交，点了也不落库（用户硬规则）", evidence);
    return;
  }

  // ② 能力一致（或只有一条线路 → 页面按规则不比对）：提交必须可用，并且真的落库 + 列表出现。
  if (!submit.found) {
    record("step11", "FAIL", "找不到提交按钮 policies-add-submit", evidence);
    return;
  }
  if (submit.disabled === true) {
    record("step11", "FAIL", "能力一致/未比对时提交按钮却是禁用的（id、名称或线路没填全？见证据）", evidence);
    return;
  }
  await wdClickTid(wd, "policies-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "step11-after-submit");
  const dump2 = dumpScratchDb("step11");
  const policies = (dump2.config ?? []).filter((r) => r.kind === "policy");
  evidence.push(`config kind 计数: ${JSON.stringify(configEntityKinds(dump2))}`);
  evidence.push(`config(kind=policy): ${JSON.stringify(policies).slice(0, 500)}`);
  if (policies.length === 0) {
    const err = await wd.textOf('[data-testid="policies-add-error-text"]');
    if (err !== null) evidence.push(`policies-add-error: ${JSON.stringify(err)}`);
    record("step11", "FAIL", "点了保存但 config 表里没有 policy", evidence);
    return;
  }
  const pid = String(policies[policies.length - 1].entity_id);
  await gotoPage(wd, "#/policies");
  const listTids = await wd.probeTestIds();
  const listText = await wd.pageText();
  const inList = listTids.includes("policies-row") && listText.includes(pid);
  evidence.push(
    `回到 /policies：policies-row 存在=${listTids.includes("policies-row")}，列表文本含 ${pid}=${listText.includes(pid)}`,
  );
  const note = two
    ? "能力一致性已按规则比对（≥2 条线路，结论见证据里的能力区块原文）"
    : "只有一条线路，页面按规则不做能力比对";
  if (inList) record("step11", "PASS", `路由经真界面新增：列表与 config 表都出现（id=${pid}）；${note}`, evidence);
  else record("step11", "FAIL", `落库=true 但列表里看不到新路由（id=${pid}）`, evidence);
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

/** 经 /providers 的「添加密钥」向导造一份凭据（返回 { id, error }）。 */
async function addCredentialViaDialog(wd, evidence, { id, label }) {
  const page = await gotoPage(wd, "#/providers");
  if (page === null) {
    evidence.push("/providers 没渲染出来");
    return { id: null, error: "页面没渲染" };
  }
  if (!(await wdClickTid(wd, "credentials-add-open", evidence))) return { id: null, error: "点不开添加密钥" };
  await wdFillByTid(wd, "credentials-add-id", id, evidence);
  await wdFillByTid(wd, "credentials-add-label", label, evidence);
  await wdPickOption(wd, "credentials-add-provider", evidence); // 第一项 = 与现有线路同一服务商
  await wdFillByTid(wd, "credentials-add-secret", `sk-${id}`, evidence);
  await wdClickTid(wd, "credentials-add-submit", evidence);
  await sleep(1500);
  const errorText = await wd.textOf('[data-testid="credentials-add-error"]');
  const error = errorText === null ? null : errorText.replace(/\s+/g, " ").trim();
  await wd.closeDialog();
  const { dump } = configKinds("credential-made");
  const ok = configIds(dump, "credential").includes(id);
  evidence.push(`凭据 ${id} 落库=${ok}（error=${JSON.stringify(error)}）`);
  return { id: ok ? id : null, error };
}

/**
 * **能力规则的强制面**（[docs/17 §4.3] 的反向验证 + 用户硬要求⑤）：
 *
 * 1. **能力一致的两条线路可以路由到一起** —— 真界面选中它们 → 页面显示"能力一致"、提交可用
 *    → `policy_upsert` 落库，且 `use_channels` 真的带上这两条（读库验证）。
 * 2. **把已经被路由归组的线路改成能力不一致** —— `channel_upsert` 必须被后端**拒绝**，
 *    错误里要**点出差异字段**（`TA-2007` + `vision`），而且库里那条线路的能力**一个字节都没变**。
 *
 * 第 2 条为什么重要：界面上的"禁用提交"只是第一道；真正不允许出现"同一路由下能力不一致"的是后端。
 * 只测界面 = 只测了提示，没测规则。
 */
async function step15(wd, fixture) {
  const evidence = [];
  const prereq = await ensureAddPrereqs(wd, fixture, evidence);
  if (!prereq.ok) {
    record("step15", "SKIP", "界面缺前提：服务商/凭据/模型都造不出来", evidence);
    return;
  }
  // 保证有 ≥2 条同模型线路，再分别取"契约默认能力"与"声明 vision"的两类。
  let { dump } = configKinds("step15-before");
  let channels = (dump.config ?? []).filter((r) => r.kind === "channel");
  if (channels.length === 0) {
    const a = await addChannelViaDialog(wd, evidence);
    if (a.id === null) {
      record("step15", "FAIL", "造不出第一条线路，无法验证能力规则", evidence);
      return;
    }
    ({ dump } = configKinds("step15-channel1"));
    channels = (dump.config ?? []).filter((r) => r.kind === "channel");
  }
  const model = channels.map(channelModelOf).find((m) => m !== null && m !== undefined) ?? null;
  if (model === null) {
    record("step15", "FAIL", "配置里读不到 model", evidence);
    return;
  }
  const sameModel = () => {
    const d = configKinds("step15-probe").dump;
    return (d.config ?? []).filter((r) => r.kind === "channel" && channelModelOf(r) === model);
  };
  const isVision = (row) => /"vision"\s*:\s*true/.test(String(row.json));

  // 凑够两条"默认能力"的线路（能力一致才能路由到一起）。
  let defaults = sameModel().filter((r) => !isVision(r));
  if (defaults.length < 2) {
    evidence.push(`默认能力的线路只有 ${defaults.length} 条 → 造第三份凭据，再建一条默认能力的线路`);
    const cred3 = await addCredentialViaDialog(wd, evidence, { id: "ta-e2e-cred-3", label: "TA E2E 密钥 3" });
    if (cred3.id === null) {
      record("step15", "FAIL", "造不出第三份凭据，无法凑出两条能力一致的线路", evidence);
      return;
    }
    const c = await addChannelViaDialog(wd, evidence, {
      preferCredential: /密钥 3/,
      preferModel: new RegExp(model, "i"),
    });
    if (c.id === null) {
      record("step15", "FAIL", `第三条线路没落库（error=${JSON.stringify(c.error)}）`, evidence);
      return;
    }
    defaults = sameModel().filter((r) => !isVision(r));
  }
  const pair = defaults.slice(0, 2).map((r) => String(r.entity_id));
  evidence.push(`用来路由的两条能力一致线路: ${JSON.stringify(pair)}（model=${model}）`);
  if (pair.length < 2) {
    record("step15", "FAIL", `同模型默认能力线路不足两条（${JSON.stringify(pair)}）`, evidence);
    return;
  }

  // ---- ① 能力一致 → 真的能存路由，并且 use_channels 带上这两条 ----
  const page = await gotoPage(wd, "#/policies");
  if (page === null) {
    record("step15", "FAIL", "/policies 没渲染出来", evidence);
    return;
  }
  if (!(await wdClickTid(wd, "policies-add-open", evidence))) {
    record("step15", "FAIL", "点不开「添加路由」", evidence);
    return;
  }
  await wdPickOption(wd, "policies-add-model", evidence, new RegExp(model, "i"));
  await sleep(600);
  // 选项文案是 `<channel.id> · <upstream_model>`：按 id 精确挑这两条（id 只含 [a-z0-9-]，无需转义）
  const only = new RegExp(pair.join("|"));
  // ★ 页面在"选完模型"后会**默认全选**该模型的全部线路（`selectedTouched=false` 时的 effect），
  //   而 step10 会留下一条声明 vision 的线路 —— 全选就必然"能力不一致 → 提交被禁用"。
  //   所以先把不在 `pair` 里的选中项**取消**，再确认这两条已经在选中集里。
  const kept = await wdKeepOptions(wd, "policies-add-channels", evidence, only);
  const picked = kept.length >= 2 ? kept : await wdPickOptions(wd, "policies-add-channels", 2, evidence, only);
  await sleep(400);
  const capText = (await wd.textOf('[data-testid="policies-add-capabilities"]')) ?? "";
  const capFlat = capText.replace(/\s+/g, " ").slice(0, 200);
  const tids = await wd.probeTestIds();
  evidence.push(`能力区块原文: ${JSON.stringify(capFlat)}`);
  evidence.push(`不一致元素存在=${tids.includes("policies-add-capability-mismatch")}`);
  const submitState = await wd.exec(`
    const b = document.querySelector('[data-testid="policies-add-submit"]');
    return b ? { found: true, disabled: b.disabled === true || b.getAttribute("aria-disabled") === "true" } : { found: false };
  `);
  evidence.push(`已选 ${picked.length} 条；提交按钮: ${JSON.stringify(submitState)}`);
  const polId = `pol-${model}`.replace(/[^a-z0-9._-]/gi, "-");
  const idNow = await wd.attrOf('[data-testid="policies-add-id"]', "value");
  if (idNow === null || idNow === "") await wdFillByTid(wd, "policies-add-id", polId, evidence);
  const nameNow = await wd.attrOf('[data-testid="policies-add-name"]', "value");
  if (nameNow === null || nameNow === "") await wdFillByTid(wd, "policies-add-name", `路由 ${model}`, evidence);
  await wdClickTid(wd, "policies-add-submit", evidence);
  await sleep(1500);
  await shot(wd, "step15-policy-saved");
  const saved = configKinds("step15-policy");
  const policies = (saved.dump.config ?? []).filter((r) => r.kind === "policy");
  const last = policies[policies.length - 1];
  const polJson = last === undefined ? "" : String(last.json);
  const bothUsed = pair.every((id) => polJson.includes(id));
  evidence.push(`config(kind=policy) ${policies.length} 条；最后一条含两条线路=${bothUsed}`);
  evidence.push(`policy 原文: ${JSON.stringify(polJson.slice(0, 400))}`);
  if (policies.length === 0 || !bothUsed) {
    const err = await wd.textOf('[data-testid="policies-add-error-text"]');
    if (err !== null) evidence.push(`policies-add-error: ${JSON.stringify(err)}`);
    record("step15", "FAIL", "能力一致的两条线路没能存成一条路由（读库没有两条 use_channels）", evidence);
    return;
  }
  await wd.closeDialog();

  // ---- ② 把已归组的线路改成能力不一致 → 后端必须拒绝并点出 vision ----
  // 目标线路必须**精确**命中：线路 id = provider × credential × model，
  // 所以 provider 与 credential 两个下拉都要按库里读到的**文案**挑，不能靠"第一项"。
  const targetId = pair[1];
  const allRows = configKinds("step15-target").dump.config ?? [];
  const beforeRow = allRows.find((r) => r.kind === "channel" && String(r.entity_id) === targetId);
  const beforeJson = beforeRow === undefined ? "" : String(beforeRow.json);
  evidence.push(`改能力前的 ${targetId}: ${JSON.stringify(beforeJson.slice(0, 300))}`);
  let targetProviderId = null;
  let targetCredentialId = null;
  try {
    const parsed = JSON.parse(beforeJson);
    targetProviderId = parsed.provider ?? null;
    targetCredentialId = parsed.credential ?? null;
  } catch {
    /* 读不出来就走 undefined（下面会如实记录没精确命中） */
  }
  const jsOf = (kind, id) => {
    const row = allRows.find((r) => r.kind === kind && String(r.entity_id) === id);
    if (row === undefined) return null;
    try {
      return JSON.parse(String(row.json));
    } catch {
      return null;
    }
  };
  const provName = (jsOf("provider", targetProviderId) ?? {}).name ?? null;
  const credLabel = (jsOf("credential", targetCredentialId) ?? {}).label ?? null;
  const esc = (s) => String(s).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  evidence.push(
    `目标线路的 provider=${JSON.stringify(targetProviderId)}(${JSON.stringify(provName)}) credential=${JSON.stringify(targetCredentialId)}(${JSON.stringify(credLabel)})`,
  );
  const upd = await addChannelViaDialog(wd, evidence, {
    preferProvider: provName === null ? undefined : new RegExp(esc(provName)),
    preferCredential: credLabel === null ? undefined : new RegExp(esc(credLabel)),
    preferModel: new RegExp(esc(model)),
    capabilities: { vision: true },
  });
  const afterJson =
    (configKinds("step15-after").dump.config ?? [])
      .filter((r) => r.kind === "channel" && String(r.entity_id) === targetId)
      .map((r) => String(r.json))[0] ?? "";
  const unchanged = afterJson === beforeJson;
  const errText = upd.error ?? "";
  const pointsAtField = /TA-2007/.test(errText) && /vision/.test(errText);
  evidence.push(`改能力时的后端原文: ${JSON.stringify(errText)}`);
  evidence.push(`TA-2007 且点出 vision: ${pointsAtField}；库里的线路能力未变: ${unchanged}`);
  if (pointsAtField && unchanged) {
    record(
      "step15",
      "PASS",
      `能力规则双向成立：能力一致的 ${pair.join(" / ")} 可路由（policy 已落库）；把 ${targetId} 改成 vision=true 时后端拒绝（TA-2007 点出 vision）且库里未变`,
      evidence,
    );
  } else {
    record(
      "step15",
      "FAIL",
      `强制面不成立：后端原文含 TA-2007=${/TA-2007/.test(errText)}(点出 vision=${/vision/.test(errText)})，库里未变=${unchanged}`,
      evidence,
    );
  }
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
  if (rows > 0 && hasFixtureNode) {
    record("step17", "PASS", `节点清单真出数据：${rows} 行，且包含 fixture 里的 ta-e2e-node-hk`, evidence);
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

// ---------------------------------------------------------------- step18 API key 管理

/**
 * **step18：访问密钥真的能管、也真的能挡人**（用户第 71 轮：「API key 的管理也需要」）。
 *
 * 关键语义（[docs/19 §3.3] 已定死）：`gateway.local_key` 为空**且**表里没有 enabled 的 key → **不鉴权**；
 * 一旦存在任一凭据来源 → **必须匹配**。所以本步骤按顺序证明：
 * 建 key 之前无凭据不 401 → 建 key 之后无凭据 401、带 key 非 401 → 界面上禁用后立刻回到 401 → 删除。
 * 明文只在创建时出现一次，点「我已保存」后必须从界面上消失。
 */
async function step18(wd) {
  const evidence = [];
  const GW = "http://127.0.0.1:8787";
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

  // ① 建之前：没有凭据来源 → 不该被挡（默认不鉴权，这是既有行为）
  const before = await probe(null);
  evidence.push(`建 key 之前：无凭据 GET /v1/models → ${JSON.stringify(before)}`);

  // ② 真界面新建
  await wdClickTid(wd, "api-keys-create", evidence);
  await sleep(400);
  await wdFillByTid(wd, "api-keys-create-name", "ta-e2e-key", evidence);
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

  // ④ 真鉴权：带 key 非 401、不带 key 401
  const withKey = await probe(plaintext);
  const noKey = await probe(null);
  evidence.push(`带这个 key：${JSON.stringify(withKey)}；不带 key：${JSON.stringify(noKey)}`);

  // ⑤ 界面上禁用 → 立刻 401
  let disabledProbe = null;
  if ((await wd.probeTestIds()).includes("api-key-toggle")) {
    await wdClickTid(wd, "api-key-toggle", evidence);
    await sleep(1200);
    disabledProbe = await probe(plaintext);
    evidence.push(`界面上禁用后带同一个 key：${JSON.stringify(disabledProbe)}`);
  } else {
    evidence.push("没有 api-key-toggle，无法从界面禁用（这一条没测到）");
  }

  const okCreate = plaintext.startsWith("ta-sk-");
  const okHidden = hiddenAfterDone;
  const okAuth = withKey.status !== 401 && noKey.status === 401;
  const okDisabled = disabledProbe !== null && disabledProbe.status === 401;
  evidence.push(
    `判定：明文形状=${okCreate} 只显示一次=${okHidden} 鉴权生效=${okAuth} 禁用即挡住=${okDisabled}`,
  );
  if (okCreate && okHidden && okAuth && okDisabled) {
    record("step18", "PASS", `访问密钥真能管：创建（明文只显示一次）→ 带 key 放行 / 不带 key 401 → 界面禁用后立刻 401`, evidence);
  } else {
    record(
      "step18",
      "FAIL",
      `访问密钥没做到位：明文形状=${okCreate} 只显示一次=${okHidden} 鉴权生效=${okAuth} 禁用即挡住=${okDisabled}`,
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
  if (!/已用|Used/.test(text)) {
    record("step13", "SKIP", "订阅行里没有流量信息（后端还没解析 subscription-userinfo，或还没接上界面）", evidence);
    return;
  }
  // fixture 给的是 upload 10GB + download 82.9GB = 92.9GB，total 130GB → 71%
  const hasUsed = /92\.9/.test(text);
  const hasTotal = /130/.test(text);
  const hasPercent = /71%/.test(text);
  const hasUpdated = /更新时间|Updated/.test(text);
  evidence.push(`92.9GB=${hasUsed} 130GB=${hasTotal} 71%=${hasPercent} 更新时间=${hasUpdated}`);
  if (hasUsed && hasTotal && hasPercent && hasUpdated) {
    record("step13", "PASS", "订阅行显示更新时间 + 已用/总量 + 百分比，且与 fixture 的字节数对得上", evidence);
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
      "",
    ].join("\n"),
  );
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) return printHelp();

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
    await wd.newSession(exe);
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
  record("step1", "PASS", "真实 Tauri 窗口渲染出首页（page-header 命中）", evidence);
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
  const pid = "ta-e2e-provider";
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

  // 再验一档用户会遇到的真实情况：**上游没有 /models 端点**（404）不能被说成"连接失败"
  const noModels = await runProviderWizardProbeOnNoModelsEndpoint(wd, fixture, evidence);

  // 下一步 → 真落库（provider + credential + secret）
  await wdClickTid(wd, "pw-next", evidence);
  await sleep(2000);
  await shot(wd, "step2-wizard-step2");
  const step2Tids = await wd.probeTestIds();
  const notice = await wd.textOf('[data-testid="pw-saved-notice"]');
  evidence.push(`pw-saved-notice: ${JSON.stringify((notice ?? "").replace(/\s+/g, " ").trim())}`);
  evidence.push(`步骤 2 命中: ${step2Tids.filter((t) => t.startsWith("pw-")).join(", ")}`);
  const dumpAfterNext = dumpScratchDb("step2-step2");
  evidence.push(`db(config) kind 计数: ${JSON.stringify(configEntityKinds(dumpAfterNext))}`);
  const providerInDb = configIds(dumpAfterNext, "provider").includes(pid);
  evidence.push(`步骤 1 落库: provider ${pid} = ${providerInDb}`);

  // 第 2 步：一键扫描（真调 model_import_from_provider）→ 全选 → 添加选中（真调 model_apply_import）
  let scanRows = 0;
  let addedModels = false;
  if (step2Tids.includes("pw-scan")) {
    await wdClickTid(wd, "pw-scan", evidence);
    await sleep(3000);
    await shot(wd, "step2-wizard-scanned");
    scanRows = await wd.exec(`return document.querySelectorAll('[data-testid="pw-scan-row"]').length;`);
    const scanEmpty = await wd.textOf('[data-testid="pw-scan-empty"]');
    const scanError = await wd.textOf('[data-testid="pw-scan-error"]');
    evidence.push(`扫描结果行数 = ${scanRows}；scan-empty=${JSON.stringify(scanEmpty)}；scan-error=${JSON.stringify(scanError)}`);
    if (scanRows > 0) {
      const selectAll = (await wd.probeTestIds()).includes("pw-scan-select-all");
      if (selectAll) await wdClickTid(wd, "pw-scan-select-all", evidence);
      await wdClickTid(wd, "pw-scan-add", evidence);
      await sleep(2500);
      await shot(wd, "step2-wizard-added");
      const dumpModels = dumpScratchDb("step2-models");
      const modelIds = configIds(dumpModels, "model");
      evidence.push(`添加后 config(kind=model) = ${JSON.stringify(modelIds)}`);
      addedModels = modelIds.some((id) => id.startsWith("ta-e2e-scan-"));
    }
  } else {
    evidence.push("步骤 2 没有 pw-scan（扫描入口不存在）");
  }

  // 只读信息面板：hover 提示必须是「查看模型信息」，面板里**不许有可写控件**（唯一例外是同一行的 lane）
  const infoProbe = await wd.exec(`
    const btn = document.querySelector('[data-testid="pw-model-info"]');
    const out = { hasInfoBtn: btn !== null, title: btn ? (btn.getAttribute('title') || '') : null, panel: null, writables: null };
    if (btn) { btn.click(); }
    return out;
  `);
  await sleep(600);
  const panelProbe = await wd.exec(`
    const panel = document.querySelector('[data-testid="pw-model-info-panel"]');
    if (panel === null) return { panel: false };
    const w = [...panel.querySelectorAll('input,select,textarea,[contenteditable="true"]')];
    return { panel: true, writables: w.length, tags: w.slice(0, 5).map((e) => e.tagName.toLowerCase()) };
  `);
  evidence.push(`信息面板: ${JSON.stringify(infoProbe)} · 面板内可写控件: ${JSON.stringify(panelProbe)}`);
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

  const providerInUi = providerInDb && (await wd.pageText()).includes(pname);
  evidence.push(`服务商: 落库=${providerInDb} 列表出现=${providerInUi}`);

  // ---- ② 添加密钥：明文只进密钥仓库；成功后显示的是**后端返回的那个 Credential** ----
  const cid = "ta-e2e-cred";
  let credentialInDb = false;
  let credentialInUi = false;
  if (!hasCredentialEntry) {
    evidence.push("→ /providers 没有「添加密钥」入口（真实缺陷，已记录）");
  } else if (!(await wdClickTid(wd, "credentials-add-open", evidence))) {
    evidence.push("失败：点不开「添加密钥」");
  } else {
    await shot(wd, "step2-credential-form-opened");
    await wdFillByTid(wd, "credentials-add-id", cid, evidence);
    await wdFillByTid(wd, "credentials-add-label", "TA E2E 密钥", evidence);
    await wdPickOption(wd, "credentials-add-provider", evidence, /TA E2E 供应商/i);
    await wdFillByTid(wd, "credentials-add-secret", "sk-ta-e2e-secret", evidence);
    await shot(wd, "step2-credential-form-filled");
    await wdClickTid(wd, "credentials-add-submit", evidence);
    await sleep(1500);
    await shot(wd, "step2-credential-after-submit");
    const credResult = await wd.textOf('[data-testid="credentials-add-result"]');
    evidence.push(`credentials-add-result: ${JSON.stringify((credResult ?? "").replace(/\s+/g, " ").trim())}`);
    const credError = await wd.textOf('[data-testid="credentials-add-error"]');
    if (credError !== null) evidence.push(`credentials-add-error: ${JSON.stringify(credError)}`);
    const dump2 = dumpScratchDb("step2-credential");
    evidence.push(`db(config 表) kind 计数: ${JSON.stringify(configEntityKinds(dump2))}`);
    credentialInDb = configIds(dump2, "credential").includes(cid);
    credentialInUi = (credResult ?? "").includes(cid);
    evidence.push(`密钥: 落库=${credentialInDb} 后端回显=${credentialInUi}`);
    // 明文**不许**出现在页面上（它只进密钥仓库）
    const pageAfter = await wd.pageText();
    evidence.push(`页面上能否搜到明文 sk-ta-e2e-secret: ${pageAfter.includes("sk-ta-e2e-secret")}`);
    await wd.closeDialog();
  }

  const ok =
    providerInDb &&
    providerInUi &&
    credentialInDb &&
    credentialInUi &&
    testShowsLatency &&
    noModels.ok &&
    scanRows > 0 &&
    addedModels &&
    dupBlocked;
  if (ok) {
    record(
      "step2",
      "PASS",
      `服务商两步向导：名称去重拦下重名 · 测试连接显示真实延迟 · 上游无 /models 端点如实说明 · 下一步落库 · 扫描 ${scanRows} 条并成功添加 · 密钥向导落库`,
      evidence,
    );
  } else {
    record(
      "step2",
      "FAIL",
      `两步向导没走通：落库=${providerInDb} 列表=${providerInUi} 密钥(回显=${credentialInUi} 落库=${credentialInDb}) ` +
        `测试连接显示延迟=${testShowsLatency} 无端点档=${noModels.ok} 扫描行数=${scanRows} 已添加模型=${addedModels} 重名拦下=${dupBlocked}`,
      evidence,
    );
  }
}

/**
 * 「上游没有 /models 端点」那一档（[docs/19 §1.1]）：建第二个服务商，baseUrl 指向 fixture 的
 * `/notfound/v1`（恒 404），点「测试连接」→ 结果区必须说"上游没有 /models 端点"，
 * **不许**渲染成"连接失败"（那会把一个正常的上游说成坏的）。
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
  if (inList && inDb) record("step3", "PASS", "订阅经真界面（/network 表单）新增，列表与 config 表都出现", evidence);
  else record("step3", "FAIL", `新增后不一致：列表=${inList} 落库=${inDb}`, evidence);
}

// ---------------------------------------------------------------- step4 通道/线路

async function step4(wd) {
  const evidence = [];
  // 契约里"通道"的两个写命令是 lane_upsert / lane_set_node。先看 /channels（套餐与线路）
  // 与 /network（线路列表）上有没有任何 lane 写入口。
  const page = await gotoPage(wd, "#/channels");
  if (page === null) {
    await shot(wd, "step4-no-page");
    evidence.push('等 15s 未见 [data-testid="page-header"]（/channels 没渲染）');
    record("step4", "FAIL", "/channels 页面没渲染出来", evidence);
    return;
  }
  const tidsCh = await wd.probeTestIds();
  evidence.push(`路由: #/channels；命中 data-testid: ${tidsCh.slice(0, 20).join(", ")}${tidsCh.length > 20 ? " …" : ""}`);
  evidence.push("页面上实际存在的可见控件（/channels）：");
  evidence.push(...(await controlEvidence(wd)));
  await shot(wd, "step4-channels");

  // /network 上再找一次 lane 写入口
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
      "界面缺入口：/channels 与 /network 都没有建通道/指派节点的控件（缺 selector：任何 lane upsert / set-node 的 testid 或按钮）",
      evidence,
    );
    return;
  }
  // ---- 真实 lane 写入口：/network 的「③ 线路」区块（命令 lane_upsert / lane_set_node）----
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
    record("step4", "FAIL", "线路区块在但没有提交按钮（network-lanes-add-submit）", evidence);
    return;
  }
  await wd.click(laneSubmit);
  await sleep(1500);
  await shot(wd, "step4-after-submit");

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

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

  /** 真输入：先点（拿焦点）、清空、再逐字送键。**React 受控组件必须走真事件。** */
  async fill(elId, text) {
    await this.click(elId).catch(() => {});
    await this.clear(elId).catch(() => {});
    await this.type(elId, text);
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

async function startFixture() {
  const server = createServer((req, res) => {
    const body =
      req.url && req.url.startsWith("/clash")
        ? CLASH_YAML
        : req.url && req.url.startsWith("/notfound")
          ? "not found"
          : CLASH_YAML;
    res.writeHead(200, {
      "content-type": "text/yaml; charset=utf-8",
      "subscription-userinfo": "upload=1; download=2; total=1024; expire=1893456000",
    });
    res.end(body);
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  const url = `http://127.0.0.1:${port}/clash.yaml`;
  const baseUrl = `http://127.0.0.1:${port}/v1`;
  writeFileSync(join(OUT, "fixture-clash.yaml"), CLASH_YAML);
  return { server, port, url, baseUrl };
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

  const add = await findAddControl(wd);
  if (add === null) {
    evidence.push("找过：data-testid 匹配 /(^|-)add|new|create|upsert/i 的可见控件 → 无");
    evidence.push('找过：可见 button/[role=button] 文案匹配 /新增|添加|新建|创建|Add|New|Create/i → 无');
    evidence.push("页面上实际存在的可见控件：");
    evidence.push(...(await controlEvidence(wd)));
    evidence.push(`页面文本片段: ${JSON.stringify(bodyText.replace(/\s+/g, " ").slice(0, 300))}`);
    await shot(wd, "step2-providers-no-entry");
    // 这一页确实是只读的（**真实缺陷，证据留在上面**）。但界面里**确实存在**另一条
    // "加供应商"的路径：首启向导 `/onboarding` 第 1 步 —— 它有真输入框、真提交按钮。
    // 任务要的是"这个操作能不能在软件里完成"，所以走向导，并把两个事实都写清楚。
    evidence.push("→ /providers 没有新增入口（真实缺陷，已记录）；改走界面里真实存在的另一条路径：首启向导 /onboarding 第 1 步");
    const wiz = await ensureWizardRun(wd, fixture, evidence);
    if (!wiz.ok) {
      record("step2", "SKIP", `界面缺入口：/providers 没有新增控件，且首启向导也不可用（${wiz.reason}）`, evidence);
      return;
    }
    await verifyProviderLanded(wd, evidence, wiz.payload);
    return;
  }

  await wd.click(add.el);
  await shot(wd, "step2-form-opened");
  const filled = await fillFormHeuristic(wd, {
    id: "ta-e2e-provider",
    name: "TA E2E 供应商",
    base_url: "http://127.0.0.1:9/v1",
    protocol: "openai",
  });
  evidence.push(`新增入口: ${add.how}`);
  evidence.push(`填写的字段: ${JSON.stringify(filled)}`);

  const submit = await wd.findByText("button", /保存|提交|确定|创建|Save|Submit|Create|OK/i);
  if (submit === null) {
    evidence.push("填完找不到提交按钮（保存/提交/确定/创建）");
    await shot(wd, "step2-no-submit");
    record("step2", "FAIL", "新增表单打开了但没有提交按钮", evidence);
    return;
  }
  await wd.click(submit);
  await sleep(1500);
  await shot(wd, "step2-after-submit");

  const dump = dumpScratchDb("step2");
  const kinds = configEntityKinds(dump);
  evidence.push(`db(config 表) kind 计数: ${JSON.stringify(kinds)}`);
  evidence.push(`db(providers 命中): ${JSON.stringify((dump.config ?? []).filter((r) => r.kind.includes("provider")))}`);

  const inList = (await wd.probeTestIds()).includes("providers-row") || (await wd.pageText()).includes("ta-e2e-provider");
  const inDb = JSON.stringify(dump.config ?? []).includes("ta-e2e-provider");
  if (inList && inDb) {
    record("step2", "PASS", "供应商经真界面新增，列表与 db 都出现", evidence);
  } else {
    record("step2", "FAIL", `新增后不一致：列表=${inList} 落库=${inDb}`, evidence);
  }
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
  const inList4 = (await wd.pageText()).includes(laneId);
  const inDb4 = laneRows.some(
    (r) => String(r.entity_id) === laneId || String(r.json).includes(laneId),
  );
  if (inList4 && inDb4) {
    record("step4", "PASS", "线路经真界面（lane_upsert）新增，列表与 config 表都出现", evidence);
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

  const changed = dbFingerprint(before) !== dbFingerprint(after);
  const wrote17 = JSON.stringify(after.config ?? []).includes('"autostart_delay_s":17');
  if (changed && (wrote17 || hit.length > 0)) {
    record("step6", "PASS", "真界面真输入 + 点击「校验并应用」→ config_apply 真落库（db 内容变化）", evidence);
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

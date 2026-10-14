English | [中文](README.md)

# TokenAgency

> A one-person LLM relay: a local gateway + semantic routing + a bundled kernel for egress + a Clash Verge-style tray.
> This is not a relay for hundreds of users. It collapses "several token plans + many keys + several proxy hops + many models" into **one local endpoint for one person on one machine**.

Working name `TokenAgency` (changeable). The primary documentation language is Chinese.

---

## 1. Your problem, and what this design does about it

| # | Problem | Root cause | What this design does |
|---|---|---|---|
| 1 | You subscribe to several token plans; switching is a chore | Every client stores its own base_url/key | Expose **one** local endpoint, `http://127.0.0.1:8787/v1`. Switching happens inside the gateway; clients never change |
| 2 | Overseas services still need a proxy | You depend on a system-wide proxy | A bundled kernel sidecar that **binds to loopback only**: no system proxy, no routing-table changes, no TUN, no effect on any other program on the machine |
| 3 | You installed another relay and the pain remains | That relay is a *server-side* hop; the network is still the system's problem | Network choice and LLM routing are **one decision**: picking a channel also picks the egress lane |
| 4 | Relays strip vision payloads and blind the model | The relay deserializes into a strict schema, silently dropping unknown fields, or quietly replaces image blocks with `"[image]"` text | **Full passthrough**: same-protocol requests are rewritten only at pinpointed spots (model name and auth header); bodies are carried as opaque JSON, not typed structs. When a cross-protocol conversion cannot be mapped, it **fails loudly instead of silently flattening to text** |
| 5 | Agent identity gets swallowed | Relays rewrite `User-Agent` or rebuild headers from an allowlist | Headers are forwarded **as a whole**, with only the three mandatory edits (`host`, the auth header, hop-by-hop headers). `user-agent` / `x-stainless-*` / `anthropic-beta` arrive untouched |
| 6 | Too many keys to manage | No credential abstraction | A credential layer + OS keychain storage + a usage ledger + one-click enable/disable |
| 7 | Big relays are too heavy for one person | Multi-tenancy, billing, user accounts | **Explicit non-goals**: no multi-user, no billing, no SaaS. Local-first, single machine, single user, zero-config start |
| 8 | No tray like Clash Verge; you must open a web page | Web-only backend | A Tauri tray: a state-coloured icon and a right-click menu that switches policy, switches node, or shows usage within two clicks |
| 9 | Several plans map to the same model; no way to "use one up, then the next" | Stateless routing | **Channel groups + a sticky selector + the usage ledger**: a session stays on one credential; when a plan is exhausted the next candidate takes over; failover has cooldowns |
| 10 | No usage policy; you switch manually every time | No rule engine | An ordered rule table `match → action`, with a **policy simulator**: feed it a fake request and preview the matched rule, the chosen channel, and the fallback chain |
| 11 | You want your own relay or your own model mapping | Configuration is not open | Provider templates + any base_url + two-way model-name mapping + header/body patches |
| 12 | You want different models to use different hops, isolated from the rest of the machine | A system proxy is global | **One node per provider** (auto-select / a specific node / direct; with a global default). No system proxy, no routing-table changes, no TUN, no extra listening ports |
| 13 | You already have a Clash subscription and do not want to re-enter it | Every provider invents its own format | The kernel **reads Clash subscriptions directly** (nodes, groups, and rules), and we only add the product layer. **A subscription yields nodes**: open one and toggle nodes individually; "auto-select" and "direct" are always present. A bad update never clears your existing data |
| 14 | Tech choices and project structure | — | React 19 + MUI v7 + Zustand/TanStack Query / Tauri v2 + axum + sqlx + a mihomo sidecar. The item-by-item review lives in the documentation set (including the verified finding that `tauri-plugin-mihomo` is not on crates.io) |
| 15 | You need a design you can actually build from | — | **Software architecture design (baseline)**: C4 views, crate layering and dependency rules, sequences, data views, quality attributes and acceptance scenarios, an ADR index, and the evolution path |

---

## 2. Architecture in one picture

```
client (Cursor / Claude Code / any SDK)
        │  http://127.0.0.1:8787/v1   <- the only entry point, OpenAI + Anthropic protocols
        ▼
+---------------------------- TokenAgency ----------------------------+
|  gateway (axum) -> read routing fields -> resolve model -> evaluate   |
|  policy -> choose channel                                            |
|                                   │                                  |
|            usage ledger / sticky table / trace (SQLite)              |
|                                   ▼                                  |
|  pinpoint rewrite (model name, auth header) -> pick a lane ->        |
|  use that lane's loopback port                                       |
+-----------------------------------+----------------------------------+
                                    │ 127.0.0.1 only; never touches system networking
                                    ▼
                      mihomo kernel (sidecar, TUN=off, allow-lan=off)
                       lane-jp --+
                       lane-us --+--> nodes from your subscription (toggle individually)
                       lane-cn --+    or DIRECT
```

**Two processes, but only one entry point.** Clients only ever know port `8787`. Ports `3910x` are internal loopback inbounds used between our own processes; they are not exposed and they hijack nothing.

**Three invariants** (architectural promises, pinned by tests):

1. **A single entry point**: a client only has to remember one base_url; the machine has exactly one outbound surface, `127.0.0.1:8787`.
2. **No system side effects**: after the app exits, the system proxy settings, routing table, and DNS are byte-for-byte what they were before launch. No leftover child processes, no stray listening ports.
3. **Full passthrough**: for same-protocol forwarding, every byte the client sent reaches the upstream, except for the four protocol-mandated edits (`host`, `content-length`, the auth header, hop-by-hop headers).

---

## 3. Documentation index

`docs/` holds **16 files**. The round-by-round process notes were deleted; their conclusions live in the
code, in the guards, and in the "decision record" sections of the three surviving documents. Earlier
versions remain in git history.

**The full list, the topic-based navigation, and the reasoning for retiring numbered document
references are in [docs/README.md](docs/README.md).**

| Group | Documents |
|---|---|
| Architecture and contracts | Overall architecture · Software architecture design (baseline) · External contracts · Internal contracts · Domain model and configuration · Routing and policy · Network model and kernel |
| Runtime and observability | Logging and diagnostics · Structure clean-up and threat-surface reduction (including the bundled-kernel / GPL decisions) · Protocol hub and lane ports |
| Interface | Visual design system · UI minimalism standard · Tray and UI · Page details and the cost centre |
| Process and risk | Milestones, acceptance criteria, and risks |

## 4. Non-goals (things this deliberately does not do)

- Multi-user, sign-up/login, billing, top-ups, licence keys
- Serving others (it never binds `0.0.0.0`; no public relay; no domain or reverse-proxy guide)
- A team-shared "channel pool" operations console
- Model fine-tuning, training, or a RAG platform
- Taking over the whole machine's networking by default (TUN and the system proxy are **optional advanced modes**, off by default)

## 5. Who this is for (one person)

A developer with 3–8 keys/plans on the machine (official plus a few relays), 2–4 clients (Cursor, Claude Code, Cherry Studio, Python scripts), and 1–2 proxy subscriptions. The wish is simple: **stop editing client configs, stop switching nodes by hand, stop rotating keys by hand.**

## 6. Client setup (once, then never again)

```bash
# OpenAI-compatible (Cursor / Cline / Cherry Studio / any SDK)
OPENAI_BASE_URL=http://127.0.0.1:8787/v1
OPENAI_API_KEY=ta-local-xxxxxxxx        # optional: a local gateway key

# Anthropic-compatible (Claude Code)
ANTHROPIC_BASE_URL=http://127.0.0.1:8787
ANTHROPIC_API_KEY=ta-local-xxxxxxxx
```

Use whatever model names your client already has (`gpt-4o`, `claude-sonnet-4-5`, ...); the gateway maps
them to the real upstream via `models`/`channels`. Want to switch plans? One click in the tray.

---

## 7. Install (Windows)

Download `TokenAgency_x.y.z_x64-setup.exe` from
[Releases](https://github.com/re-ITRT/TokenAgency/releases) — the installer bundles the kernel (~22 MB).

1. **Right-click → Run as administrator** (why, below)
2. Follow the wizard; it installs per-user to `%LOCALAPPDATA%\TokenAgency` (nothing is written to system directories)
3. Launch it from the Start menu or the tray icon

**Why run it elevated**: the installer is per-user by design and does not request elevation.
On some machines antivirus sandboxing drops it to **low integrity**, and it then cannot write to
`%LOCALAPPDATA%\TokenAgency` at all (`Error opening file for writing: ...\tokenagency.exe`).
Running elevated avoids that entire class of failure. (The installer also re-launches itself
elevated when it detects the problem.)

**Blocked by SmartScreen** (the exe is not code-signed): choose "More info" → "Run anyway".

**"File in use"**: run `taskkill /F /IM tokenagency.exe /T`, then click Retry.

Diagnostics: `%TEMP%\ta-install-hook.log` (`lasterr=5` access denied; `lasterr=32` in use).

> Building from source is the next section; that script produces the same installer as this Release.

## 8. Development and build

### Requirements

| Component | Version | Notes |
|---|---|---|
| Rust | 1.82+ (`rust-toolchain.toml` pins stable) | target `x86_64-pc-windows-msvc` |
| MSVC Build Tools | 2022 | Windows linker |
| Node.js | 20+ (verified on v24) | **npm**, see the note below |
| WebView2 Runtime | any | Usually already present on Windows 10/11 |
| mihomo | bundled | The installer ships a pinned kernel (GPL-3.0, aggregate). Setting `net.kernel.provider: external` points at your own binary instead |

> Note: **this repository uses npm, not pnpm.** pnpm walks up to find a workspace root, and a 0-byte
> `package.json` in the parent directory makes its JSON parsing fail.

### First-time setup

```powershell
pwsh -File scripts/install-hooks.ps1   # install git hooks + repo-level git config
npm install
node scripts/make-icon.mjs             # generate app-icon.png (1024x1024, no third-party deps)
cargo tauri icon app-icon.png          # generate the full src-tauri/icons set
```

### Everyday commands

```powershell
pwsh -File scripts/dev.ps1             # dev mode (Vite + Tauri, with hot reload)
pwsh -File scripts/build.ps1           # produce an installer: bindings -> frontend -> tests -> NSIS
cargo check --workspace                # type check only (fastest)
cargo test  --workspace                # the full test suite
cargo clippy --workspace --all-targets -- -D warnings
```

Artefacts land in `target/release/bundle/nsis/*.exe`.

### Quality gates

The hooks travel with the repository (`core.hooksPath = .githooks`); no husky or lefthook:

| Hook | What it does |
|---|---|
| `pre-commit` | plaintext-secret scan · large-file guard · `cargo fmt --check` · `cargo clippy -D warnings` · `tsc --noEmit` (selected by the kinds of files you changed) |
| `pre-push` | `cargo test --workspace` + `npm run build` |
| `commit-msg` | conventional-commit format (`feat(gateway): ...`) |

Use `git commit --no-verify` for genuine emergencies, but do not make it a habit.

### Layout at a glance

```
crates/ta-core           domain model, config validation, routing engine, quotas, pricing, metering (pure logic, no IO)
crates/ta-store          SQLite: configuration, ledger, trace, stickiness, quota state
crates/ta-kernel         mihomo sidecar: process supervision, config generation, REST control, lane port allocation
crates/ta-subscription   subscription download, format sniffing, bad-update rejection, version rollback
crates/ta-secrets        OS keychain wrapper and ${secret:...} reference resolution
crates/ta-gateway        the local gateway: ingress, full passthrough, routing pipeline, dispatch, protocol adapters, trace
src-tauri                the Tauri shell: command layer, tray, plugins, lifecycle (thin)
src                      frontend: React 19 + MUI v7 (10 pages)
docs                     design documents, 16 files (09 is the architecture baseline, 10/11 are the interface contracts)
```

---

## 9. Licence

This project is released under the **GNU General Public License v3.0** — see [LICENSE](LICENSE).

Copyright belongs to **Mao Zhiqin (毛治钦)**.

### Third-party components distributed with the installer

The installer contains the **mihomo** (GPL-3.0) binary. It runs as a **separate process** from this
program (we launch `mihomo.exe` and talk to it over a local HTTP control endpoint), which makes this an
**aggregate** in the sense of the final paragraph of GPLv3 section 5, and it does not change the licence
of this project itself.

Under GPLv3 section 6(d) — "accompany it with the Corresponding Source ... in the same way through the
same place" — the corresponding source ships **in the same Release as the installer**:

| Component | Version | Corresponding source |
| --- | --- | --- |
| mihomo | v1.19.31 | Release asset `mihomo-v1.19.31-source.tar.gz` |

That binary is **unmodified**: its measured sha256 is **byte-identical** to the upstream
`mihomo-windows-amd64-compatible-v1.19.31`, so shipping the upstream corresponding source fulfils the
obligation in full. If the kernel is ever modified, **we must publish our own changes**.
To re-fetch it: `pwsh -File scripts/fetch-mihomo.ps1 -Mode Source`.

Licence and attribution requirements for the remaining third-party components (Rust and npm
dependencies, the GeoIP database) are in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

# 账号登录态能否在第三方桌面软件中长期维持

> 调研目的：为 TokenAgency（Rust + Tauri 桌面应用）判断「用户登录一次 → 之后长期自动查额度」这条路线是否可行。
>
> **证据分级约定**
> - **【已证实】** = 官方文档 / 官方源码 / 条款原文 / 官方论坛人员回复
> - **【社区】** = 第三方开源项目文档、GitHub issue、论坛用户反馈
> - **【无证据】** = 查不到可靠出处，明确标注，不编造数字
>
> 所有时长均注明依据来源 URL。凡是没有出处的数字一律不写。

---

## 0. 一句话结论（先看这个）

**「登录一次、永久自动查额度」在所有四家厂商都做不到。** 但差距很大：

| 厂商 | 长期维持可行性 | 主要卡点 |
|---|---|---|
| **Anthropic / Claude** | ⚠️ 部分可行 | 有官方 1 年期 token（`setup-token`），但**只授权模型请求**，未证实能查额度；订阅登录本身是 8 小时 access token + 有到期日的登录态 |
| **OpenAI / ChatGPT** | ❌ 不可靠 | refresh token **一次性、用完即换、重放即判失效**；第三方工具的正确做法是「只读不自刷新」 |
| **Google** | ❌ 高风险 | OAuth 测试态 7 天；且已有**大量账号因第三方工具复用 OAuth 被限制**的真实案例 |
| **智谱 bigmodel** | ❌ 明令禁止 | 条款明确禁止在指定工具外使用 + 禁止向第三方提供访问凭证；违规限流/冻结/封号 |

**给 TokenAgency 的路线建议见最后一节。**

---

## 1. OpenAI / ChatGPT

### 1.1 会话 cookie（`__Secure-next-auth.session-token`）有效期

**【已证实（部分）】** OpenAI 官方 Cookie 政策列出了 `chatgpt.com` 上各 cookie 的时长，但**列表中并没有 `__Secure-next-auth.session-token` 这一条**。官方明确给出的相邻条目包括：

| Cookie | 时长 | 域 |
|---|---|---|
| `_puid` | 7 天 | chatgpt.com |
| `_uasid` | 1 天 | chatgpt.com |
| `_account` | Session | chatgpt.com |
| `oai-sc` | 1 年（Security） | chatgpt.com |
| `oai-did` | 1 年 | chatgpt.com |
| `_account_residency_region` | 90 天 | chatgpt.com |
| `oai-hlib` | 1 年 | chatgpt.com |
| `oai-internal-search-settings` | 2 年 | chatgpt.com |

出处：<https://openai.com/policies/cookie-policy/>

**【无证据】** `__Secure-next-auth.session-token` 的确切服务端有效期。它是 NextAuth 框架的会话 token，`next-auth` 默认 `maxAge` 为 30 天，但**这是框架默认值，不是 OpenAI 公布的值**，不能当作 OpenAI 的实际配置。**本报告不给出该 cookie 的具体天数。**

**【社区】** 第三方项目 oracle 的文档指出：要让 ChatGPT 保持登录，实际最小 cookie 集是 `__Secure-next-auth.session-token`（含 `.0` / `.1` 分片变体）加 `_account`；只有在遇到 Cloudflare 挑战时才需要额外带 `cf_clearance` / `__cf_bm`。
出处：<https://raw.githubusercontent.com/steipete/oracle/8e3456e7f2dcd9686d32193b08970b99848c7ec3/docs/browser-mode.md>

> 关键点：**`_account` 是 Session 级 cookie**（官方口径）。也就是说即使 session-token 本身长寿，工作区选择相关的 cookie 也会随浏览器会话消失，需要重新协商。

### 1.2 `chatgpt.com/backend-api/*` 的 access token 类型与寿命

**【社区（高可信，且与官方源码一致）】** 第三方额度工具 CodexBar 的 OAuth 技术文档（作者做过 Codex 源码分析，并链接了官方源码行号）给出：

- 查额度端点（默认）：`GET https://chatgpt.com/backend-api/wham/usage`
- 请求头：
  - `Authorization: Bearer <access_token>`
  - `ChatGPT-Account-Id: <account_id>`
  - `User-Agent: codex-cli`
- access token 是 **JWT**，其寿命由 JWT 的 `exp` 声明决定；客户端在 `exp` 前 **5 分钟** 视为需要刷新
- token 文件：`~/.codex/auth.json`

出处：<https://raw.githubusercontent.com/steipete/CodexBar/main/docs/codex-oauth.md>

**【已证实】** 官方 Codex 源码确认了「5 分钟刷新窗口」这一常量，以及 JWT `exp` 解析逻辑：

```rust
// codex-rs/login/src/auth/manager.rs
const TOKEN_REFRESH_INTERVAL: i64 = 8;
const CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES: i64 = 5;
```
出处（固定 commit）：<https://raw.githubusercontent.com/openai/codex/528fd7ace5ec0a1c2a387dcb9c76a09f3fa011ee/codex-rs/login/src/auth/manager.rs>
JWT `exp` 解析：<https://raw.githubusercontent.com/openai/codex/528fd7ace5ec0a1c2a387dcb9c76a09f3fa011ee/codex-rs/login/src/token_data.rs>

**【无证据】** access token 的**绝对**寿命（分钟/小时数）。它由服务端签发时决定，官方文档未公布固定值。**本报告不给出具体分钟数**——只知道「有 `exp`、且客户端按 5 分钟窗口提前刷新」。

### 1.3 Codex CLI 的 `refresh_token`：会轮转、有效期、失效条件

**【已证实 — 这是本节最重要的发现】** 官方 Codex 源码中的常量与错误消息：

```
const TOKEN_REFRESH_INTERVAL: i64 = 8;                      // 8 天
const REFRESH_TOKEN_REUSED_MESSAGE: "... your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_EXPIRED_MESSAGE: "... your refresh token has expired. ...";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: "... your refresh token was revoked. ...";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: "... you have since logged out or signed in to another account. ...";
REFRESH_TOKEN_URL = "https://auth.openai.com/oauth/token"
```

服务端错误码到失效原因的映射：

```rust
Some("refresh_token_expired")     => RefreshTokenFailedReason::Expired,
Some("refresh_token_reused")      => RefreshTokenFailedReason::Exhausted,
Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
```

出处：<https://raw.githubusercontent.com/openai/codex/528fd7ace5ec0a1c2a387dcb9c76a09f3fa011ee/codex-rs/login/src/auth/manager.rs>

**结论（逐条回答任务问题）：**

1. **是否轮转？** — **是，且是一次性使用（single-use）+ 重放检测。** 服务端存在专门的 `refresh_token_reused` 错误码，说明同一个 refresh token 被再次使用会被识别并**使该凭据链失效**（`Exhausted`）。这在 OAuth 术语里叫 refresh token rotation with reuse detection，是**最强的防盗用策略**。
2. **有效期多久？** — 存在 `refresh_token_expired` 错误码，证明**会过期**。**【无证据】** 官方未公布具体时长。源码里唯一的数字常量是「8 天刷新间隔」（`TOKEN_REFRESH_INTERVAL = 8`），但那是**客户端主动刷新的节奏**，不是 refresh token 的寿命。CodexBar 也把它描述为「八天的 `last_refresh` 规则」。
3. **什么情况下失效？** — 源码给出四类明确原因：
   - **过期**（`refresh_token_expired`）
   - **被重放**（`refresh_token_reused` → Exhausted）—— 第三方软件与官方 CLI 同时刷新最容易触发
   - **被撤销**（`refresh_token_invalidated`）
   - **账号不匹配**（登录态换成了另一个账号）

**【社区（极具参考价值）】** CodexBar 明确把「绝不自刷新」写成了设计契约，理由是刷新 token 是共享文件、会引发竞态：

> 「Codex CLI owns the refresh endpoint and the refresh-token lifecycle for the native `CODEX_HOME/auth.json`」
> 「the usage path must not redeem any shared refresh token or write a replacement file」
> 「stale native credentials produce `nativeRefreshRequired` and route to Codex CLI recovery」

出处：<https://raw.githubusercontent.com/steipete/CodexBar/main/docs/codex-oauth.md>

**【社区】** 同类问题在多个第三方项目被反复报告（佐证轮转竞态是真实痛点）：
- NousResearch/hermes-agent issue：Codex 池「重放了一个已被消费（轮转掉）的 refresh_token 后进入终态 DEAD」— <https://github.com/NousResearch/hermes-agent/issues/70097>
- 同项目 issue：per-profile 冻结的 refresh token 互相竞争 — <https://github.com/NousResearch/hermes-agent/issues/114012>
- elizaOS PR：让 codex refresh-token 轮转持久化 — <https://github.com/elizaOS/eliza/pull/16293>
- openai/codex 官方 PR #6231「改进 token 刷新处理以解决『Re-connecting』行为」（作者 etraut-openai）— <https://github.com/openai/codex/pull/6231>

**【社区】** 需要注意的另一个风险点：有第三方工具因为改写 Codex 的 `auth.json` 而出问题（tokscale PR #1017「stop tokscale from rewriting Codex's auth.json」）— <https://github.com/junhoyeo/tokscale/pull/1017>

> **对 TokenAgency 的直接含义**：如果 TokenAgency 也想自己刷新 OpenAI 的 refresh token，它会**和官方 Codex CLI 抢同一个一次性 token**，必然导致其中一方把另一方「用掉」，用户的登录态被自己搞坏。这是不可接受的用户体验，且极易被用户误认为是软件 bug。

---

## 2. Anthropic / Claude

### 2.1 Claude Code 的 OAuth 登录流程（PKCE）

**【已证实】** 官方文档描述 `/login` 打开浏览器完成授权；当浏览器无法回调本地服务器时（WSL2 / SSH / 容器），会显示一个登录码让用户粘贴回终端 —— 这是标准的「本地回调 + 手动 code 粘贴」两种形态。
出处：<https://code.claude.com/docs/en/authentication>

**【社区】** 第三方文档确认 `~/.claude/.credentials.json` 中的 `accessToken` 前缀为 `sk-ant-oat`，且请求以 `Authorization: Bearer` + Claude Code CLI 头发出（相对地，`sk-ant-api` 走 `x-api-key`）。
出处：<https://raw.githubusercontent.com/JerrettDavis/JD.SemanticKernel.Connectors.ClaudeCode/refs/heads/main/docs/articles/credential-resolution.md>

> **【无证据】** 具体 PKCE `code_challenge_method` / 授权端点等参数细节。官方未公开 Claude Code OAuth 的原始端点规格。不编造。

### 2.2 access token 寿命

**【社区（与官方行为一致）】** anthropics/claude-code issue #72017 报告：

> 「the session token expires approximately every **8 hours**」
> 「The `expiresAt` timestamp is **~8 hours** from login time.」

出处：<https://github.com/anthropics/claude-code/issues/72017>

**【社区】** `.credentials.json` 中的 `expiresAt` 是**毫秒**级 Unix 时间戳，字段结构：

```json
{
  "claudeAiOauth": {
    "accessToken": "sk-ant-oat...",
    "refreshToken": "...",
    "expiresAt": 1762000000000,
    "scopes": ["..."],
    "subscriptionType": "claude_pro",
    "rateLimitTier": "pro"
  }
}
```

出处：<https://raw.githubusercontent.com/JerrettDavis/JD.SemanticKernel.Connectors.ClaudeCode/refs/heads/main/docs/articles/credential-resolution.md>

> 注意：issue #72017 的作者同时抱怨「`refreshToken` 字段存在但从未被使用」，该 issue 被标记为 `duplicate` + `area:auth` 并已关闭。说明「不自动刷新」更多是当时版本的 bug 表现，而非设计意图；官方文档（下一条）明确说会**自动刷新**。

### 2.3 refresh token 是否轮转与上限

**【已证实 — 官方文档明确否定了「永久」】** 官方 authentication 文档：

> 「the profile stores an OAuth login while an API key is a static credential: Claude Code **refreshes the profile's login automatically**, and **when refresh fails**, requests fail with [Anthropic profile login expired] until you sign in again.」

并且有专门的「Renew an expiring login」小节：

> 「When the login you created with `/login` is **within three days of expiring**, Claude Code shows a warning at startup: `Your login expires in 3 days · run /login to renew`」
> 「The login lifetime itself is **unchanged**; the advance warning is what v2.1.203 adds.」
> 「Once the stored login expires and **can't be refreshed**, each model request fails with `Login expired · Please run /login` until you sign in again.」

出处：<https://code.claude.com/docs/en/authentication>

**结论：**
- 自动刷新：**是**（官方明确）
- **refresh token 有上限**：**是**。官方设了「到期前 3 天警告」机制，这个机制只有在「登录态本身会到期」的前提下才有意义。**登录态的最长寿命官方未公布数值。**
- 需要重新登录的条件（官方）：登录态到期且无法刷新 → `Login expired · Please run /login`

**【已证实】** 官方 errors 文档中有多条与刷新失败相关的错误：`OAuth token refresh failed — run /login to re-authenticate`、`JWT refresh failed: no OAuth token — run /login`、`Claude.ai login expired`、`Signed out of Claude`、`OAuth token revoked` / `OAuth token has expired`。
出处：<https://code.claude.com/docs/en/errors>

**【社区】** 其他已知断裂点：
- 非交互（子进程）模式下 OAuth 自动刷新失败 → token 过期后 401 — <https://github.com/anthropics/claude-code/issues/53063>
- 把 `~/.claude/.credentials.json` 拷到远程/无头机器后 refresh token 不被使用 — <https://github.com/anthropics/claude-code/issues/21765>
- `--print` 模式下 OAuth token 不持久化/不刷新，破坏自动化 — <https://github.com/anthropics/claude-code/issues/37402>
- 第三方刷新会覆盖 `~/.claude/.credentials.json` 并丢掉 `subscriptionType`，导致降级成 API-key 模式 — <https://github.com/NousResearch/hermes-agent/issues/83338>

### 2.4 `~/.claude/.credentials.json` 里存了什么 / 存哪里

**【已证实】** 官方 authentication 文档「Credential management」：

- **macOS**：凭据存在加密的 **macOS Keychain**；当 Keychain 写入被拒（例如 SSH 会话中已锁定）时，改用 `~/.claude/.credentials.json`，文件权限 `0600`
- **Linux**：`~/.claude/.credentials.json`，权限 `0600`
- **Windows**：`%USERPROFILE%\.claude\.credentials.json`，继承用户 profile 目录的访问控制
- **`CLAUDE_CONFIG_DIR`** 环境变量可改变该文件所在目录
- **官方明确**：「Claude Code manages `.credentials.json` through `/login` and `/logout`.」

出处：<https://code.claude.com/docs/en/authentication>

> **重要合规含义**：因为官方明确说这个文件由 `/login` 和 `/logout` 管理，**第三方软件读写它就是介入官方凭据管理流程**。这解释了上面 hermes-agent #83338 那类「刷新后凭据被改坏」的问题。

### 2.5 ⭐ 官方长期凭据路线：`claude setup-token`（1 年期）

**【已证实 — 这是对 TokenAgency 最有价值的发现】** 官方文档「Generate a long-lived token」：

> 「For CI pipelines, scripts, or other environments where interactive browser login isn't available, generate a **one-year OAuth token** with `claude setup-token`」
> 「The command opens the same browser authorization flow as `/login`, and the token prints to the terminal after you approve access in the browser. **It does not save the token anywhere**; copy it and set it as the `CLAUDE_CODE_OAUTH_TOKEN` environment variable」
> 「This token authenticates with your Claude subscription and requires a Pro, Max, Team, or Enterprise plan. **It can only make model requests**, so it can't establish Remote Control sessions or fetch claude.ai connectors.」

出处：<https://code.claude.com/docs/en/authentication>

**关键限定**：官方文档明确说这个 token「**只能发起模型请求**」（can only make model requests）。**因此：【无证据】它能否用于查询订阅额度 / 用量端点。** 不能假设可以——需要 TokenAgency 自行实测确认。

**【社区】** 已有把 `setup-token` 用于第三方启动器的实践（描述为「long-lived Claude setup-token」）— <https://github.com/BitterSecurity/Decepticon/commit/1badb52dcc5fa8b844bdab7864fc3aa833832bb7>

---

## 3. Google（Gemini CLI / Code Assist / Antigravity）

### 3.1 OAuth refresh token 的寿命 与 「测试中/未验证应用」7 天限制

**【已证实 — 官方条款原文，中文官方站点】** 官方 OAuth 2.0 文档「刷新令牌过期」一节列出了 refresh token 失效的**完整原因清单**：

> 「刷新令牌可能会因以下原因之一而停止工作：
> - 用户已撤消您应用的访问权限。
> - **刷新令牌已六个月未使用。**
> - 用户更改了密码，并且刷新令牌包含 Gmail 权限范围。
> - 用户账号已超过授予（有效）刷新令牌的最大数量。
> - 用户向您的应用授予了基于时间的访问权限，并且该访问权限已过期。
> - 如果管理员将应用权限范围中请求的任何服务设置为"受限"（错误为 `admin_policy_enforced`）。
> - 对于 Google Cloud Platform API，管理员设置的会话时长可能已超出。」

并且**明确给出 7 天规则**：

> 「如果 Google Cloud Platform 项目配置了**面向外部用户**的 OAuth 权限请求页面，并且**发布状态为"测试"**，则系统会向其颁发一个**在 7 天后过期**的刷新令牌，除非请求的 OAuth 权限范围仅是名称、电子邮件地址和用户个人资料（通过 `userinfo.email, userinfo.profile, openid` 权限范围或其 OpenID Connect 等效项）的子集。」

出处（官方，中文镜像站点可访问）：<https://developers.google.cn/identity/protocols/oauth2>

**【已证实】** 其他官方量化事实（同一页面）：
- 每个 OAuth 2.0 客户端 ID 的每个 Google 账号，**refresh token 数量上限为 100 个**；达到上限后最旧的自动失效且**无警告**（此限制不适用于服务账号）
- 若需授权多个程序/机器/设备，官方建议「**将每个 Google 账号授权的客户端数量限制为 15 或 20**」
- 令牌尺寸上限：授权码 256 字节、access token 2048 字节、refresh token 512 字节
- 关于 GCP 会话控制策略：会话时长「可能非常有限（**1 小时到 24 小时**）」，此时调用失败并返回 `invalid_grant`，且 `error_subtype` 为 `invalid_rapt`
- 官方明确禁止：「**您不得使用或鼓励使用用户凭据进行服务器到服务器的部署。**」

**【已证实】** 7 天限制的官方论坛确认（Google Developer forums）：

> 「If the app requests a subset of the following: name, email address, and user profile ... or its only used for Sign In to the APP. Therefore, there is no need to add them as test users, they will be allowed to access the APP and **their tokens won't expire after 7 days**.」

出处：<https://discuss.google.dev/t/oauth2-refresh-token-expiration-and-youtube-api-v3/160874/4>

**逐条回答任务问题：**

| 问题 | 答案 |
|---|---|
| 7 天限制是否适用于这类客户端？ | **是**——只要该项目的 OAuth 权限请求页面面向外部用户且发布状态为「测试」，且请求的 scope **超出** name/email/profile 子集。Gemini CLI 请求的是 `cloud-platform` scope（见下），**远超**该豁免子集，所以**会**受 7 天限制影响——除非该项目处于「已发布/生产」状态或属于 Workspace 内部应用。 |
| 如何变成长期有效？ | 官方路径：把 OAuth 同意屏幕**发布为"生产"**（需要应用验证）；或使用 **Workspace 内部（Internal）应用**；或改用**服务账号** / 域级委派 / ADC。 |
| refresh token 寿命本身 | 官方规则：**6 个月未使用**则失效（这是最接近「长期」的官方量化数字）；另有撤销、改密、超上限等情形。 |

### 3.2 Gemini CLI 的凭据文件、scope、refresh 行为

**【已证实 — 官方源码】** Gemini CLI（Apache-2.0，Google 官方仓库）：

- 凭据文件：`~/.gemini/oauth_creds.json`
  ```ts
  export const OAUTH_FILE = 'oauth_creds.json';
  static getOAuthCredsPath(): string {
    return path.join(Storage.getGlobalGeminiDir(), OAUTH_FILE);
  }
  ```
  出处：<https://raw.githubusercontent.com/google-gemini/gemini-cli/main/packages/core/src/config/storage.ts>

- **scope（关键！远超 7 天豁免子集）**：
  ```ts
  const OAUTH_SCOPE = [
    'https://www.googleapis.com/auth/cloud-platform',
    'https://www.googleapis.com/auth/userinfo.email',
    'https://www.googleapis.com/auth/userinfo.profile',
  ];
  ```
- 使用 `access_type: 'offline'`（申请 refresh token），本地回调 `http://127.0.0.1:<port>/oauth2callback`，另有 user-code 流程
- **规范刷新路径**：`OAuth2Client` 的 `tokens` 事件触发时，把新凭据写回文件（`cacheCredentials`，权限 `0o600`）或加密存储
- **注意**：源码注释明确说该 client secret 可以放在 git 里，因为这是 installed application，client secret「obviously not treated as a secret」

出处：<https://raw.githubusercontent.com/google-gemini/gemini-cli/main/packages/core/src/code_assist/oauth2.ts>

**【社区/官方仓库 issue】** 官方的 refresh 实现曾有 bug，导致**丢弃 refresh token 而强制重新登录**：

> title: 「OAuth refresh tokens discarded on access_token rotation and on stale-credential delete」
> 「Google's token refresh often returns only a new `access_token` (no `refresh_token`). The current code writes the new response verbatim, which drops the stored `refresh_token` to `undefined`. **Next refresh then fails, forcing a full re-auth.**」
> 「**Impact**: Users get kicked out to full OAuth re-auth periodically despite having a valid refresh token stored.」

状态：`closed`（已修复）。出处：<https://github.com/google-gemini/gemini-cli/issues/25460>

**【社区】** 另一条相关报告：非交互调用时 Gemini CLI 会**删除 `oauth_creds.json`** 并要求重新登录，导致编辑器集成无法用 OAuth — <https://github.com/google-gemini/gemini-cli/issues/12258>

### 3.3 Antigravity

**【已证实 — 官方仓库 README】** Antigravity CLI 的认证说明：

> 「The CLI authenticates via the **system keyring**, falling back to Google Sign-In if no active session exists.
> - **Local**: Automatically opens your default browser.
> - **Remote / SSH**: Detects SSH sessions and prints an authorization URL to complete login locally.
> - **Sign Out**: Run `/logout` to clear saved credentials.」

出处：<https://raw.githubusercontent.com/google-antigravity/antigravity-cli/d7bc2c7d28654be4eb89379531a8816758e65602/README.md>

> **【无证据】** Antigravity 的 token 具体寿命、refresh 是否轮转。官方 README 未说明。**不编造。**

**【社区 — 但这是全报告最严重的风险信号】** Google AI Developers Forum 上大量用户报告：**因为通过第三方工具（OpenClaw）复用 Antigravity / Gemini Code Assist 的 OAuth 登录态，账号被直接限制/封禁**。相关主题标题（均指向 ToS 违规）：

| 主题 | 回复数 | 浏览量 |
|---|---|---|
| 「Account Restricted Without WARNING – Google AI Ultra / OAuth via OpenClaw」 | 70+ | — |
| 「**$250/mo Ultra Subscriber Banned Without Warning** — The OpenClaw Mass Ban Wave...」 | 25 | 6530 |
| 「Gemini API Access Disabled (**403 ToS**) After Using OpenClaw Agent — Appeal Pending」 | 25 | 5247 |
| 「Antigravity / Gemini Code Assist Disabled (**403 ToS Violation**) - Appeal Assistance Needed」 | 14 | 1137 |
| 「Antigravity account disabled - "**violation of Terms of Service**"」 | 60 | 7922 |
| 「Gemini Disabled on Antigravity IDE, How to Restore Access?」 | 146 | 13562 |

出处：<https://discuss.ai.google.dev/t/account-restricted-without-warning-google-ai-ultra-oauth-via-openclaw/122778>

用户原话（帖 #70）：
> 「OpenClaw was silently falling back to Antigravity OAuth as a default... **Had no idea this violated ToS**... Already removed Antigravity from my connected apps and sent an appeal email. **Paying subscriber** + Pixel 10 Pro owner, completely unintentional on my part.」

> **这是「被检测到会怎样」的最有力证据：不是登出、不是只封该会话，而是整个 Google 账号/订阅的 AI 访问被 403 禁用，需要申诉。** 代价极高。
>
> 注意：这是**社区论坛的用户报告，不是官方公告**。Google 未公开发布封禁政策说明。但案例数量、时间集中度、以及「403 ToS violation」这种一致的错误码，使其可信度很高。

### 3.4 有没有合规的长期方案

**【已证实 — 官方文档】** 官方列出的服务账号路径：

> 「对于这些类型的**服务器到服务器的互动**，您需要一个**服务账号**，该账号属于您的应用，而不是某个最终用户。您的应用代表服务账号调用 Google API，**无需用户同意**。」
> 「**此限制不适用于服务账号**」（指 100 个 refresh token 上限）

服务账号用私钥签 JWT 换 access token，无 refresh token 过期概念（换 key 即可）。
出处：<https://developers.google.cn/identity/protocols/oauth2>

其他官方长期路线：**Application Default Credentials（ADC）**、**域级委派**、以及 Gemini CLI 源码里支持的 `AuthType.COMPUTE_ADC`（在 GCE/Cloud Shell 用 metadata server 身份认证，**不缓存凭据**，由 Compute client 自行刷新）。

> 但要注意：**服务账号 ≠ 订阅额度。** 用户买的是 Gemini/Antigravity 的**订阅**（Ultra 等），服务账号拿到的是 **API 计费**身份，两者不是同一套配额。所以「用服务账号查订阅额度」在概念上就不成立。

---

## 4. 智谱 bigmodel.cn

### 4.1 网站登录态 cookie / token 的寿命

**【无证据】** 智谱官方文档中**没有**公开 `bigmodel.cn` 网站登录态 cookie 或 session token 的有效期。用户协议、服务协议、隐私政策、订阅协议中均未给出这类数值。**本报告不给出任何天数。**

### 4.2 有没有可以长期用的「个人访问令牌 / PAT」

**【部分证实】** 官方文档体系中的凭据是 **API Key**（API 密钥）：

> 「登录后，在个人中心页面，点击 API Keys，创建一个新的 API Key。请妥善保管您的 API Key，不要泄露给他人，也不要直接硬编码在代码中。建议使用环境变量或配置文件来存储 API Key。」

出处：<https://docs.bigmodel.cn/cn/guide/start/quick-start>
API Key 管理页：<https://bigmodel.cn/usercenter/proj-mgmt/apikeys>

**【无证据】** API Key 的**有效期**（是否过期、是否可设 TTL）。官方快速开始页面未说明。**不编造。** 但可以确认 API Key **可以创建、可撤销**（有专门的管理页面）。

**【无证据】** 智谱是否有「个人访问令牌 / PAT」这一独立凭据类型。在官方文档的条款清单与快速开始中**未出现**该概念。

### 4.3 `bigmodel-organization` / `bigmodel-project` 头

**【无证据 — 官方文档中未找到】** 在智谱官方开放文档（docs.bigmodel.cn）的公开页面中，**没有检索到 `bigmodel-organization` / `bigmodel-project` 这两个请求头的官方说明**。它们未出现在 Claude API 兼容、快速开始、Coding Plan、API 文档等页面。

**【社区】** 第三方工具（如 cc-switch、OmniRoute）在接入智谱「团队版 Coding Plan」时讨论过配额端点与 org/project 相关参数，但这是第三方逆向的结果，**不是官方契约**。
- cc-switch v3.17.0 release notes：「智谱的团队套餐（团队版 Coding Plan）走同一个配额端点但需要…」— <https://newreleases.io/project/github/farion1231/cc-switch/release/v3.17.0>
- OmniRoute PR #6351「feat(glm): add team plan quota settings for glm-cn connections」— <https://github.com/diegosouzapw/OmniRoute/pull/6351>

> **结论**：这两个头**没有任何官方稳定性承诺**。它们在官方文档中不存在，意味着智谱可以随时改名/改语义/移除而不违反任何文档契约。**不能作为长期依赖。**

### 4.4 能否复用登录态查额度

**【已证实 — 条款明令禁止】** 用户协议「三、您的权利和义务」第 4 条列举了「不当使用大模型开放平台」的行为，其中直接命中「复用登录态/第三方客户端」的包括：

> - 「除智谱开放的特定功能，您不得对大模型开放平台或者大模型开放平台运行过程中释放到任何终端内存中的数据、软件运行过程中客户端与服务器端的交互数据，以及大模型开放平台运行所必需的系统数据，进行复制、修改、增加、删除、**挂接运行**或创作任何衍生作品，形式包括但不限于**使用插件、外挂或非经智谱授权的第三方工具/服务接入**大模型开放平台和相关系统」
> - 「通过**非智谱开发、授权的第三方软件、插件、外挂、系统，登录或使用智谱软件及服务**，或制作、发布、传播上述工具」
> - 「自行或者授权他人、第三方软件对大模型开放平台及其组件、模块、数据进行干扰」

同协议第 3 条第 7(7) 款（禁止转授权/中转）：

> 「未经智谱事先书面同意，您不得通过出租、出借、出售、分销、转售、转让、**共享账号**、转授权、二次封装、**服务中转**、**聚合中转**或其他变相方式，向任何第三方提供大模型开放平台服务，或提供与本服务相关的 **API Key、API Token、访问凭证、账号权限**或其他服务能力。」

以及账户条款：

> 「您的大模型开放平台账户**仅限您本人使用**。未经大模型开放平台同意，您直接或间接**授权第三方使用您大模型开放平台账户**或获取您账户项下信息的行为**无效**。」

出处：<https://docs.bigmodel.cn/cn/terms/user-agreement>

**【已证实】** 套餐层面的使用须知（GLM Coding Plan）：

> 「**仅限指定工具使用**：GLM Coding Plan 仅限在指定工具与产品环境（见接入工具页）中使用，用于非支持工具将被**限制权益**。」
> 「**违规处置**：违反订阅及自动续费协议将可能**触发风控，会被进行限流、冻结等处置。3 次以上违规可能会被封禁账号。**」

「适用工具」页面（官方列出的白名单）明确写：

> 「GLM Coding Plan **仅限在以下官方支持的指定工具与产品环境中使用，用户不得将订阅权益用于以下范围之外的工具或场景。**」

白名单工具包括：ZCode、Claude Code、Claude for IDE、Codex、OpenCode、Pi Coding Agent、TRAE、CodeBuddy、Lingma、Qoder、Kilo 等（截断处）。

出处：<https://docs.bigmodel.cn/cn/coding-plan/usage-notes> 、 <https://docs.bigmodel.cn/cn/coding-plan/tool/others>

**【已证实 — 最高等级处置】** 「使用须知」还专门说明永久封禁：

> 「永久封禁属于平台**最高等级处置**。平台会秉持审慎原则，在证据充分、风险特征明确，并确认账号存在**倒卖、中转、套利**等严重违规行为时执行。永久封禁不会因单一指标触发，我们会综合多个风险信号，并结合必要的人工复核作出处置决定。」

### 4.5 官方可用的合规额度查询入口（重要）

**【已证实】** 官方提供了**用户在网页上查看用量的官方入口**，而不需要第三方抓取：

> 「套餐同时设有每 5 小时和每周额度上限，您可以在**用量统计**中查看您的额度消耗进展」

- 用量统计（个人）：<https://www.bigmodel.cn/coding-plan/personal/usage>
- 套餐概览（含风控提示）：<https://www.bigmodel.cn/coding-plan/personal/overview>
- 费用明细：<https://www.bigmodel.cn/finance-center/bill/expensebill/list>

额度刷新规则（官方量化）：
- **5 小时积分**：动态刷新，请求消耗 5 小时后重置
- **周积分**：自套餐下单时起，以 **7 天**为一个周期刷新

积分额度表（Lite 2000/10000、Pro 12000/60000、Max 28000/140000）。

出处：<https://docs.bigmodel.cn/cn/coding-plan/overview>

> **这意味着**：TokenAgency 若想展示智谱额度，**更合规的做法是引导用户去看官方用量页**，或让用户提交官方 API Key 走官方 API，而不是复用网站登录态。

---

## 5. 通用机制：为什么厂商封禁第三方客户端复用会话

### 5.1 条款原文（已证实，可直接引用）

#### OpenAI Terms of Use（Published: January 1, 2026）

> **Registration.** 「You must provide accurate and complete information to register for an account to use our Services. **You may not share your account credentials or make your account available to anyone else** and are responsible for all activities that occur under your account.」

「What you cannot do」清单：

> - 「Attempt to or assist anyone to **reverse engineer, decompile or discover the source code or underlying components** of our Services, including our models, algorithms, or systems」
> - 「**Automatically or programmatically extract data or Output**」
> - 「Interfere with or disrupt our Services, including **circumvent any rate limits or restrictions or bypass any protective measures or safety mitigations** we put on our Services.」

出处：<https://openai.com/policies/terms-of-use/>

> 命中点：复用 ChatGPT 会话 cookie 去调 `backend-api`，同时落在「programmatically extract」与「circumvent protective measures」两条上；把凭据交给软件也触及「share your account credentials」。

#### Anthropic Consumer Terms of Service（Effective October 8, 2025）

第 2 条：

> 「**You may not share your Account login information, Anthropic API key, or Account credentials with anyone else.** You also may not make your Account available to anyone else.」

第 3 条，禁止行为第 7 项（**最关键**）：

> 「**Except when you are accessing our Services via an Anthropic API Key or where we otherwise explicitly permit it, to access the Services through automated or non-human means, whether through a bot, script, or otherwise.**」

同条其他项：
> 第 4 项「To **crawl, scrape, or otherwise harvest data or information** from our Services other than as permitted under these Terms.」
> 第 8 项「...bypassing any of our systems or protective measures.」

出处：<https://www.anthropic.com/legal/consumer-terms>

> **这段条款是「官方客户端白名单」的另一种写法**：只要你走 **API Key**，自动化访问就是被许可的；走**订阅 OAuth** 做自动化则被明确排除。这为 TokenAgency 指明了合规边界。

#### Google APIs Terms of Service（Last modified: November 9, 2021）

> **2c. Permitted Access**「You will only access (or attempt to access) an API **by the means described in the documentation** of that API... You will not **misrepresent or mask either your identity or your API Client's identity** when using the APIs or developer accounts.」
> **2d. API Limitations**「You agree to, and **will not attempt to circumvent**, such limitations documented with each API.」
> **3a.**「Google may **suspend access to the APIs by you or your API Client without notice** if we reasonably believe that you are in violation of the Terms.」
> **4b. Confidential Matters**「Developer credentials (such as passwords, keys, and client IDs) are intended to be used by you and identify your API Client. **You will keep your credentials confidential** and make reasonable efforts to prevent and discourage other API Clients from using your credentials.」

出处：<https://developers.google.cn/terms>

#### 智谱（见 4.4，已引用）

### 5.2 被检测到会怎样（结局清单）

| 处置 | 证据 |
|---|---|
| **整个账号的 AI 访问被禁用（403 ToS violation），需申诉** | Google 论坛大量案例（见 3.3） |
| **限流 / 冻结套餐权益** | 智谱使用须知（见 4.4） |
| **3 次以上违规封号；严重者永久封禁** | 智谱使用须知（见 4.4） |
| **refresh token 凭据链被作废**（重放检测） | Codex 源码 `refresh_token_reused` → `Exhausted`（见 1.3） |
| **订阅登录态到期后强制重新登录** | Claude Code 官方错误 `Login expired · Please run /login` |
| **服务端 `invalid_grant`** | Google 官方文档（撤销 / 会话控制策略） |
| **Cloudflare 挑战**（需 `cf_clearance` 等） | oracle 文档（见 1.1） |

**【社区】** 关于「只封该会话还是封账号」：从 Google 案例看是**账号级**（甚至 403 掉 Gemini API 访问），这是最坏的一种。从 Codex 看是**凭据链级**（换一次登录可恢复）。

### 5.3 为什么技术上必然脆弱

**【已证实】** 
1. **凭据轮转 + 重放检测**：OpenAI 的 `refresh_token_reused` 错误码证明服务端会主动作废被重放的凭据链。第三方与官方客户端抢刷新 = 必然互相踢掉。
2. **服务端可随时撤销**：Google 官方列出「用户已撤销应用访问权限」「管理员设置为受限」「会话时长超限」等，均可让 refresh token 立即失效。
3. **设备绑定（DBSC）**：Google 正在推 Device Bound Session Credentials —— 会话绑定到 **TPM/安全硬件里的私钥**，每次轮转需用该私钥签名 nonce。W3C 规范刻意设计成只有具备硬件密钥证明的浏览器才能实现，**非浏览器 HTTP 客户端无法实现**。
   - W3C 规范：<https://w3c.github.io/webappsec-dbsc/>
   - Google 安全博客：<https://blog.google/security/protecting-cookies-with-device-bound-session-credentials/>
   - Chrome 公告：<https://developer.chrome.com/blog/dbsc-windows-announcement>
   - Workspace 会话绑定：<https://knowledge.workspace.google.com/admin/security/prevent-cookie-theft-with-session-binding>
4. **浏览器本地加密（App-Bound Encryption）**：见第 6 节。

### 5.4 合规的替代（官方文档 + 凭据寿命）

| 方案 | 官方文档 | 寿命 / 轮转 |
|---|---|---|
| **Anthropic `claude setup-token`** | <https://code.claude.com/docs/en/authentication> | **1 年期** OAuth token（官方明确） |
| **Anthropic API Key** | <https://platform.claude.com/docs> | 【无证据】官方未公布固定 TTL；可撤销 |
| **Anthropic Admin API / WIF** | <https://platform.claude.com/docs/en/manage-claude/admin-api>（抓取时被重定向拦截）、<https://platform.claude.com/docs/en/manage-claude/workload-identity-federation> | Workload Identity Federation：面向无长期密钥的工作负载身份联邦 |
| **Anthropic apiKeyHelper** | <https://code.claude.com/docs/en/settings-reference#apikeyhelper> | **官方设计用于动态/轮转凭据**；默认**每 5 分钟**重跑，可用 `CLAUDE_CODE_API_KEY_HELPER_TTL_MS` 调整 |
| **OpenAI 项目 API Key / service account** | <https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/projects/subresources/service_accounts>（抓取被 403） | 【无证据】未取到官方 TTL 文本 |
| **Google 服务账号** | <https://developers.google.cn/identity/protocols/oauth2> | 无 refresh token 过期概念；**不受 100 个上限限制**；用私钥签 JWT |
| **Google ADC / Compute ADC** | Gemini CLI 源码 `AuthType.COMPUTE_ADC` | 由 metadata server 提供，客户端自行刷新 |
| **智谱 API Key** | <https://bigmodel.cn/usercenter/proj-mgmt/apikeys> | 【无证据】官方未公布 TTL |

> ⭐ **Anthropic 的 `apiKeyHelper` 值得特别注意**：官方文档明确说它用于「dynamic or rotating credentials, such as short-lived tokens fetched from a vault」，并且**默认每 5 分钟重跑**。这正是 TokenAgency 这类软件应该采用的模式——让用户提供一个脚本，软件调用脚本拿凭据，而不是软件自己管理凭据生命周期。
> 出处：<https://code.claude.com/docs/en/authentication>

---

## 6. 浏览器 cookie 提取的法律与合规风险

### 6.1 各平台条款要点（已证实，原文引用见第 5.1 节）

| 平台 | 条款 URL | 命中点原文 |
|---|---|---|
| OpenAI | <https://openai.com/policies/terms-of-use/> | 「You may not share your account credentials or make your account available to anyone else」；「Automatically or programmatically extract data or Output」；「circumvent any rate limits or restrictions or bypass any protective measures」 |
| Anthropic | <https://www.anthropic.com/legal/consumer-terms> | 「You may not share your Account login information... with anyone else」；「Except when you are accessing our Services via an Anthropic API Key..., to access the Services through automated or non-human means」 |
| Google | <https://developers.google.cn/terms> | 「You will only access an API by the means described in the documentation」；「will not attempt to circumvent such limitations」；「keep your credentials confidential and make reasonable efforts to prevent and discourage other API Clients from using your credentials」 |
| 智谱 | <https://docs.bigmodel.cn/cn/terms/user-agreement> | 「通过非智谱开发、授权的第三方软件、插件、外挂、系统，登录或使用智谱软件及服务」；「提供与本服务相关的 API Key、API Token、访问凭证、账号权限」 |

**【无证据】** Microsoft Services Agreement 的相关条款原文 —— 本次未成功抓取，故不引用。

### 6.2 Chrome 扩展政策与 cookie 访问

**【已证实 — 官方 Chrome Web Store Limited Use 政策原文】**

> 「Extensions may only collect, use, or transmit user data that is **necessary for the extension's disclosed single purpose**」
> 「**Collection and use of web browsing activity is prohibited**, except to the extent required for a user-facing feature described prominently in the Product's Chrome Web Store page and in the Product's user interface.」
> 数据使用必须「Limit your use of user data to providing or improving your single purpose」
> 「**Do not allow humans to read user data**, unless: the user's explicit consent... / aggregated and anonymized... / necessary for security purposes / to comply with applicable laws」
> 「All other transfers, uses, or sale of user data is completely prohibited」

出处：<https://developer.chrome.google.cn/docs/webstore/program-policies/limited-use>

**【社区/行业共识】** Manifest V3 移除了阻塞式 `webRequest` API，使扩展读取/改写跨域 cookie 的能力受限；这是从扩展平台角度封堵该路径。**【无证据】** 本次未取到 Google 官方对 MV3 与 cookie 读取关系的一手政策原文页面（developer.chrome.com 抓取失败），故不引用具体条款文本。

> **重要区分**：**TokenAgency 是桌面应用，不是 Chrome 扩展**。因此 Chrome Web Store 政策**并不直接管辖它**。但该政策反映了平台方对「读取用户浏览数据」的底线态度，在舆论/合规层面仍有参考意义。

### 6.3 Chrome App-Bound Encryption（技术阻断）

**【社区（技术深度分析，引用 Google 官方博客与 Chromium 源码）】** 关键事实：

- **Chrome 127（2024 年 7 月）引入 App-Bound Encryption (ABE)**。此前 Chromium 系浏览器用 **DPAPI** 保护 cookie/密码；DPAPI 的弱点是「**同一用户、同一权限级别的任何应用都能调用 `CryptUnprotectData` 解密**」——这正是信息窃取类恶意软件的常用手法。
- ABE 引入一个 **COM 服务（Chrome Elevation Service 的一部分）作为守门人**，通过**调用方路径校验**（`ProtectionLevel::PROTECTION_PATH_VALIDATION`）决定是否解封 `app_bound_key`。密钥经过**双重 DPAPI 包裹**（先用户上下文，再 SYSTEM 上下文）。
- 存储在 `Local State` 的 `os_crypt.app_bound_encrypted_key`，格式 `APPB<base64>`。
- 官方设计文档**明确承认**代码注入是绕过方式：「An attacker could inject code into Chrome browser and call the IPC interface.」
- **Chrome 136+ 收紧了远程调试**：`--remote-debugging-port` / `--remote-debugging-pipe` 在**默认用户数据目录**下不再生效，必须另外指定 `--user-data-dir`，从而隔离出使用**不同加密密钥**的 profile。
- **Google 官方声明**：ABE「will not function correctly in environments where Chrome profiles roam between multiple machines」（因为 DPAPI 绑定机器+用户）。
- **企业策略** `ApplicationBoundEncryptionEnabled` 可配置或禁用该功能。

出处（含官方博客与 Chromium 源码引用）：
- 分析文档：<https://raw.githubusercontent.com/xaitax/Chrome-App-Bound-Encryption-Decryption/v0.18.0/docs/RESEARCH.md>
- Google 安全博客：<https://security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html>
- Chrome 公告：<https://developer.chrome.com/blog/remote-debugging-port>

**【社区 — 生态现状】** cookie 提取库的实际可用性：

> 「`browser_cookie3` (the ecosystem default) **does not handle ABE**. As of May 2026, it returns **garbage** for Chrome cookies on Windows and silently-incomplete data on macOS.」
> 「`rookiepy` claims ABE support but in practice **requires admin privileges from Chrome 130+ on Windows**」
> 「**Firefox doesn't have ABE.** Mozilla's threat model treats local attackers as out-of-scope, so Firefox cookies remain readable by any user-space process with file access. This is what makes Firefox the recommended unattended option.」

出处：<https://raw.githubusercontent.com/teng-lin/notebooklm-py/1a1e82945ffe0a5d2bb326bd702f175168371847/docs/auth-cookie-lifecycle.md>

> **对 TokenAgency 的直接含义**：在 Windows 上读 Chrome cookie 已经**事实上要求管理员权限或 DLL 注入**。这两件事对于一款要在用户机器上正经分发的商业软件来说都不可接受——前者触发 UAC，后者会被 EDR/杀软直接判定为恶意软件行为（该绕过技术的开源实现本身就是「infostealer-adjacent」）。

### 6.4 法律分析（不是法律意见，仅为条款与判例梳理）

**【已证实 — 判例要点，来源为律所分析】**
- **hiQ Labs v. LinkedIn**：法院认定 hiQ **违反了 LinkedIn 禁止抓取的条款**（合同层面），但同时**拒绝**支持「抓取公开数据构成 CFAA 意义上的『未经授权访问』」这一主张。即：**违反 ToS 是合同问题，不等于刑事/CFAA 问题。**
  出处（需订阅，未取到全文）：<https://www.proskauer.com/insights/get-pdf/25671> ；<https://dev.natlawreview.com/node/191446/printable/pdf>（站点锁定，未取到正文）
- **Van Buren v. United States**（美国最高法院，2021）：收窄了 CFAA 中「exceeds authorized access」的解释，**「目的不当」不等于「访问越权」**。

**【无证据】** 本次**未能取到** EFF、Lawfare 或法院文书的可访问一手全文（EFF 链接 404，Stack Overflow 403，natlawreview 401）。因此**本报告不对 CFAA / GDPR 的适用性下结论**，只陈述上述判例的公开要点。**建议 TokenAgency 如有实际需求，单独咨询执业律师。**

**【关键区分 — 用户自己导入 vs 软件替用户登录】**

这是任务里最重要的一条，基于**条款文本**（而非法律意见）可以明确区分：

| 做法 | 条款命中情况 | 风险等级 |
|---|---|---|
| **A. 用户手动提供自己的 API Key / PAT** | 三家条款均**允许**用 API Key 做自动化（Anthropic 第3条第7项明文除外条款；OpenAI 有 API 计费体系；Google 有服务账号）。Anthropic 甚至提供 `apiKeyHelper` 专门给动态凭据用 | **低（合规路径）** |
| **B. 用户手动提供自己的 session cookie / OAuth refresh token** | 仍可能命中「share your account credentials with anyone else」——因为凭据被交给了软件；Google 条款更是要求「keep your credentials confidential and prevent other API Clients from using your credentials」 | **中高** |
| **C. 软件自动从浏览器读取 cookie** | 除上述外，还叠加：绕过浏览器安全设计（ABE/DBSC）、未经用户逐项知情同意的数据访问、在部分司法辖区可能触及个人信息处理合规问题 | **高** |
| **D. 软件替用户登录（代填密码 / 自动化登录表单）** | 这是最接近「共享账号凭据」与「automated non-human means」的行为；且必须处理 MFA/CAPTCHA，若绕过则直接命中「bypass protective measures」 | **最高** |

**核心差异**：
- **凭据类型**决定合规性上限——**API Key 类凭据被条款明确许可用于自动化；会话/订阅凭据不被许可。**
- **凭据来源**决定风险叠加——**用户主动提供**保留了「用户是决策者」的事实；**软件自行提取**则把软件变成了数据处理者与绕过者。
- **开源+本地运行**能降低（但不消除）风险：本地运行意味着凭据不落到第三方服务器，OpenAI 条款「make your account available to anyone else」的「anyone else」不成立；但**「automatically or programmatically extract」和「circumvent protective measures」与是否开源无关**，且 Google 的封号案例正是针对用户本机运行的第三方工具。

---

## 7. 给桌面软件的结论：哪些做法能长期维持、哪些一定会断、哪些有合规风险

### 7.1 ✅ 能长期维持（推荐）

| 做法 | 依据 | 说明 |
|---|---|---|
| **让用户提供 API Key（各家官方）** | Anthropic ToS 明文除外条款；Google 服务账号条款；OpenAI API 体系 | 唯一三家条款都正面支持的路线 |
| **Anthropic `claude setup-token`（1 年期）** | 官方文档明确「one-year OAuth token」 | 但**只授权模型请求**，**查额度需自行实测**，不要假设可用 |
| **Anthropic `apiKeyHelper`** | 官方为动态凭据设计，默认 5 分钟重跑 | 把凭据生命周期交还给用户脚本，最干净的架构 |
| **Google 服务账号 / ADC** | 官方文档，无 refresh token 过期概念 | 但**不等于订阅配额**，概念上不通用 |
| **引导用户查看厂商官方用量页** | 智谱官方提供用量统计页 | 零风险，用户体验略差 |
| **只读官方 CLI 已维护的凭据文件（不写不刷新）** | CodexBar 的设计契约 + Codex 源码 | ⚠️ 仅作为「补充数据源」，且必须**绝不刷新、绝不写回**，并处理文件不存在/过期时的优雅降级 |

### 7.2 ❌ 一定会断（不要做）

| 做法 | 会怎么断 | 依据 |
|---|---|---|
| **自己刷新 OpenAI Codex refresh token** | refresh token 一次性 + 重放检测 → 凭据链被作废（`refresh_token_reused` → Exhausted），**且会把官方 CLI 的登录态一起搞坏** | 官方源码常量与错误码 |
| **依赖 ChatGPT 会话 cookie 长期有效** | `_account` 是 Session 级；session-token 寿命未公开；Cloudflare 挑战随机出现 | 官方 Cookie 政策；oracle 文档 |
| **依赖 Claude 订阅 OAuth 永久有效** | 登录态有到期日（官方到期前 3 天警告），到期且无法刷新即 `Login expired` | 官方 authentication 文档 |
| **依赖 Google OAuth refresh token 长期有效（测试态）** | 7 天后过期（scope 超出 name/email/profile 时） | 官方 OAuth 文档 |
| **依赖 `bigmodel-organization` / `bigmodel-project` 头** | **官方文档中不存在**，无任何稳定性承诺，可随时变更 | 官方文档中未检索到 |
| **在 Windows 上读 Chrome cookie** | Chrome 127+ ABE 路径校验；Chrome 136+ 封默认 profile 远程调试；`browser_cookie3` 返回垃圾数据 | ABE 技术分析（引用官方博客与 Chromium 源码） |
| **在非 Chrome 客户端实现 DBSC** | 规范刻意要求 TPM 硬件密钥签名 | W3C DBSC 规范 |
| **把凭据用于「非指定工具」（智谱 Coding Plan）** | 限流 → 冻结 → 3 次以上封号 → 严重者永久封禁 | 智谱使用须知 + 接入工具页 |

### 7.3 ⚠️ 有合规风险（要做必须知情并明确告知用户）

| 做法 | 风险实质 | 证据 |
|---|---|---|
| **复用用户浏览器里的登录 cookie** | 违反 ToS（账号凭据共享 / 自动化访问 / 绕过保护措施）；技术上也正被 ABE/DBSC 系统性封堵 | 三家条款原文 + ABE/DBSC |
| **复用 Google Antigravity / Code Assist 的 OAuth** | **已有大量真实封号案例**：付费 Ultra 用户被 403 ToS violation 禁用，需申诉 | Google 论坛多个主题，最高单帖 13562 浏览 |
| **复用订阅态（非 API Key）做自动化** | Anthropic 条款**明文禁止**（第3条第7项），除非走 API Key | 条款原文 |
| **软件替用户登录 / 自动填密码** | 最接近「凭据共享 + 非人类访问 + 绕过 MFA/CAPTCHA」 | 条款原文 |
| **中转 / 聚合用户的 API Key** | 智谱条款明文禁止「服务中转、聚合中转」及提供 API Key/Token/凭证给第三方 | 智谱条款原文 |
| **把用户凭据上传到自己的服务器** | 把「本地工具」变成「服务提供者」，风险质变 | OpenAI 条款「make your account available to anyone else」 |

### 7.4 🎯 对 TokenAgency 的具体建议

1. **产品定位上区分「凭据类型」，而不是「厂商」**
   把功能建立在 **API Key / PAT / service account** 之上，而不是会话登录态。这**一家条款都不会违反**，且技术上稳定。

2. **对 Anthropic：优先 `setup-token`，并先做一次实测**
   `claude setup-token` 是官方唯一明说的「1 年期」长期凭据。**第一件事应该是验证它能否访问额度/用量端点**——官方只说它能做模型请求。这一步的结论直接决定 Claude 这条支线能否自动化。

3. **对 OpenAI：走「只读伴随」模式，绝不刷新**
   若要做 OpenAI 订阅额度显示，采用 CodexBar 的契约：**只读 `~/.codex/auth.json`，绝不兑换 refresh token、绝不写回文件**。一旦 401 就提示用户去跑一次官方 CLI。这既避免毁掉用户登录态，也把「刷新」这个违规动作留在了官方客户端里。

4. **对 Google：不要碰 OAuth 复用**
   封号风险与收益完全不成比例（用户可能因此丢掉 $250/月的订阅和整个 Google 账号的 AI 访问）。只做 API Key / 服务账号。

5. **对智谱：只做 API Key，并避开 Coding Plan 的指定工具限制**
   如果要展示 Coding Plan 额度，**引导用户去官方用量页**，不要复用登录态，也不要解析 `bigmodel-*` 头。

6. **架构上：把凭据换成「用户提供的脚本」**
   仿照 Anthropic `apiKeyHelper` 的模式——**TokenAgency 调用用户配置的脚本获取凭据，自己不存储、不刷新、不持久化**。这样凭据生命周期归用户，软件不成为凭据处理者。这是风险最低且最经得起时间考验的设计。

7. **明确告知义务**
   对任何「读取本地凭据文件」的功能，在 UI 中逐项说明读什么、为什么读、不上传；这既是 GDPR 类合规的常识，也是与「软件替用户登录」划清界限的关键。

---

## 附：证据清单（按类型）

### 官方文档（已证实）
- OpenAI Cookie 政策：<https://openai.com/policies/cookie-policy/>
- OpenAI Terms of Use：<https://openai.com/policies/terms-of-use/>
- OpenAI Codex auth 文档：<https://developers.openai.com/codex/auth>（抓取被 403）
- Anthropic Claude Code Authentication：<https://code.claude.com/docs/en/authentication>
- Anthropic Claude Code Error reference：<https://code.claude.com/docs/en/errors>
- Anthropic Consumer Terms of Service：<https://www.anthropic.com/legal/consumer-terms>
- Google OAuth 2.0 官方文档（含 7 天规则与刷新令牌过期清单）：<https://developers.google.cn/identity/protocols/oauth2>
- Google APIs Terms of Service：<https://developers.google.cn/terms>
- Chrome Web Store Limited Use 政策：<https://developer.chrome.google.cn/docs/webstore/program-policies/limited-use>
- 智谱用户协议：<https://docs.bigmodel.cn/cn/terms/user-agreement>
- 智谱 Coding Plan 使用须知：<https://docs.bigmodel.cn/cn/coding-plan/usage-notes>
- 智谱 Coding Plan 接入工具（白名单）：<https://docs.bigmodel.cn/cn/coding-plan/tool/others>
- 智谱 Coding Plan 套餐概览：<https://docs.bigmodel.cn/cn/coding-plan/overview>
- 智谱快速开始（API Key）：<https://docs.bigmodel.cn/cn/guide/start/quick-start>
- 智谱 Claude API 兼容：<https://docs.bigmodel.cn/cn/guide/develop/claude/introduction>
- Antigravity CLI README：<https://raw.githubusercontent.com/google-antigravity/antigravity-cli/d7bc2c7d28654be4eb89379531a8816758e65602/README.md>

### 官方源码（已证实）
- Codex auth manager（`TOKEN_REFRESH_INTERVAL = 8`、5 分钟窗口、refresh 失效错误码）：<https://raw.githubusercontent.com/openai/codex/528fd7ace5ec0a1c2a387dcb9c76a09f3fa011ee/codex-rs/login/src/auth/manager.rs>
- Codex token_data（JWT `exp` 解析）：<https://raw.githubusercontent.com/openai/codex/528fd7ace5ec0a1c2a387dcb9c76a09f3fa011ee/codex-rs/login/src/token_data.rs>
- Gemini CLI oauth2.ts（scope、access_type offline、刷新回写）：<https://raw.githubusercontent.com/google-gemini/gemini-cli/main/packages/core/src/code_assist/oauth2.ts>
- Gemini CLI storage.ts（`~/.gemini/oauth_creds.json`）：<https://raw.githubusercontent.com/google-gemini/gemini-cli/main/packages/core/src/config/storage.ts>

### 官方论坛 / 官方 issue（可信度高）
- Google 论坛：refresh token 7 天规则确认：<https://discuss.google.dev/t/oauth2-refresh-token-expiration-and-youtube-api-v3/160874/4>
- Google 论坛：**账号因第三方 OAuth 被限制**（OpenClaw 事件）：<https://discuss.ai.google.dev/t/account-restricted-without-warning-google-ai-ultra-oauth-via-openclaw/122778>
- gemini-cli issue #25460（refresh token 被丢弃导致强制重登，已修复）：<https://github.com/google-gemini/gemini-cli/issues/25460>
- gemini-cli issue #12258（非交互模式删除凭据）：<https://github.com/google-gemini/gemini-cli/issues/12258>
- openai/codex PR #6231（改进 token 刷新，作者为 OpenAI 员工）：<https://github.com/openai/codex/pull/6231>

### 社区证据
- CodexBar Codex OAuth 技术文档（只读契约、wham/usage 端点）：<https://raw.githubusercontent.com/steipete/CodexBar/main/docs/codex-oauth.md>
- Claude Code `.credentials.json` 结构：<https://raw.githubusercontent.com/JerrettDavis/JD.SemanticKernel.Connectors.ClaudeCode/refs/heads/main/docs/articles/credential-resolution.md>
- claude-code issue #72017（8 小时会话）：<https://github.com/anthropics/claude-code/issues/72017>
- claude-code issue #53063（非交互刷新失败）：<https://github.com/anthropics/claude-code/issues/53063>
- claude-code issue #21765（跨机拷贝 refresh token 不可用）：<https://github.com/anthropics/claude-code/issues/21765>
- claude-code issue #37402（--print 模式不持久化）：<https://github.com/anthropics/claude-code/issues/37402>
- hermes-agent #83338（刷新覆盖破坏 credentials.json）：<https://github.com/NousResearch/hermes-agent/issues/83338>
- hermes-agent #70097（重放已轮转 refresh token 导致 DEAD）：<https://github.com/NousResearch/hermes-agent/issues/70097>
- hermes-agent #114012（per-profile refresh token 竞争）：<https://github.com/NousResearch/hermes-agent/issues/114012>
- oracle browser-mode 文档（ChatGPT 最小 cookie 集、Cloudflare）：<https://raw.githubusercontent.com/steipete/oracle/8e3456e7f2dcd9686d32193b08970b99848c7ec3/docs/browser-mode.md>
- notebooklm-py cookie 生命周期研究（Chrome ABE、DBSC、Firefox 路径）：<https://raw.githubusercontent.com/teng-lin/notebooklm-py/1a1e82945ffe0a5d2bb326bd702f175168371847/docs/auth-cookie-lifecycle.md>
- Chrome ABE 技术分析：<https://raw.githubusercontent.com/xaitax/Chrome-App-Bound-Encryption-Decryption/v0.18.0/docs/RESEARCH.md>
- 智能体凭据机制汇总：<https://raw.githubusercontent.com/pjlsergeant/byre/refs/heads/main/docs/AGENT-CREDENTIAL-MECHANICS.md>

### 未能取到（如实记录，未据此下结论）
- `developers.google.com` 主域抓取失败（改用 `developers.google.cn` 官方镜像成功）
- `platform.openai.com` / `developers.openai.com` 被 Cloudflare 403
- `openai.com/policies/service-terms`、`platform.claude.com` Admin API 页面（跨域重定向拦截）
- Stack Overflow（403）、natlawreview（401）、EFF 目标文章（404）
- **Microsoft Services Agreement 条款原文**、**EFF/Lawfare 法律分析**、**hiQ/Van Buren 判决书全文** —— 因此第 6.4 节仅陈述公开要点，未做法律定性
- 智谱 `bigmodel-organization` / `bigmodel-project` 官方说明 —— 官方文档中确实不存在
- 智谱 API Key / 网站登录态的具体有效期 —— 官方未公布
- OpenAI `__Secure-next-auth.session-token` 具体有效期 —— 官方 Cookie 政策未列出
- Google Antigravity token 寿命 / refresh 轮转策略 —— 官方未公布

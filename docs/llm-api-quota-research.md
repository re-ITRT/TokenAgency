# 国外主要大模型 API 平台「额度/余额/花费查询」调研

> 调研目标：为桌面软件 TokenAgency（Rust + Tauri）实现"自动读取剩余额度/花费"提供事实基础。
> 调研日期：本次会话。所有端点均为逐字摘录，未做任何推测性补全；无法证实的写「未证实」。

## 一句话结论

**光有普通 API key，绝大多数平台查不到"我还剩多少钱"。**
17 家里只有 **OpenRouter**（`GET /api/v1/key`，普通 key 即可拿到本 key 的花费上限与已用）、**Fireworks**（官方 billing API，Bearer 普通 key）、**DeepInfra**（社区证实 `payment/checklist` 用普通 key 可读预付余额）、**Hugging Face**（billing usage 用普通 token 可读，但"钱包余额"仍需浏览器会话）这 4 家能在**不使用更高权限凭证**的前提下读到钱相关数据。
其余 13 家分两类：
- **有官方只读花费接口，但必须换一种 key**：OpenAI（Admin key）、Anthropic（Admin key）、xAI（Management key）、AWS Bedrock（IAM 签名）、Azure OpenAI（Azure AD OAuth）、Vertex AI（GCP OAuth）；
- **根本没有公开 API，只能看网页或抓浏览器会话**：Gemini API（AI Studio）、Cohere、Groq、Mistral、Perplexity、Together AI、Replicate。

**没有一家能把"订阅制配额"（ChatGPT Plus/Pro、Claude Pro/Max、SuperGrok）通过 API 读出来。**

---

## 主表：17 家固定字段对照

**证据等级图例**：✅ 本次逐字抓取核实（官方文档或官方 SDK 源码）｜📄 官方文档 URL 确认存在但本环境被 Cloudflare 拦截、未逐字核实｜🗣 社区（第三方追踪器/逆向工程，附 URL）

| # | 平台 | 套餐类型 | 查询端点（完整 URL + 方法 + 必需头/参数） | 鉴权 | 仅凭普通 API key 能否查到余额/花费 | 返回值内容 | 证据等级 | 只读「用量与花费」接口（所需 key + 时间粒度） |
|---|---|---|---|---|---|---|---|---|
| 1 | **OpenAI** platform.openai.com | 按量付费 + 预付 credit + 订阅(ChatGPT) | `GET https://api.openai.com/v1/organization/costs`<br>必需 query：`start_time`(Unix 秒，必填)、`bucket_width`(仅 `1d`)、`limit`(1–180，默认 7)、`group_by`(`project_id`/`line_item`/`api_key_id`)、`page`<br>用量：`GET /v1/organization/usage/completions`（`bucket_width` 支持 `1m`/`1h`/`1d`） | Admin API key：`Authorization: Bearer $OPENAI_ADMIN_KEY`（SDK 源码标 `security={"admin_api_key_auth": True}`） | **不能**（需 Admin key）。普通 project key 调 organization 接口无权限 | 已用**花费**（`amount` + `currency`，按 `line_item` 分组）+ 用量 token 数；**无余额字段** | ✅ 官方 SDK 源码：[usage.py](https://github.com/openai/openai-python/blob/main/src/openai/resources/admin/organization/usage.py)｜📄 官方文档 `platform.openai.com/docs/api-reference/usage`（本环境被 Cloudflare 403） | 有。需 **Admin key**（`platform.openai.com/settings/organization/admin-keys`）。粒度：usage `1m/1h/1d`，costs **仅 `1d`**；游标分页 `next_page` |
| 2 | **Anthropic** console.anthropic.com | 按量付费 + 预付 + 订阅(Claude Pro/Max) | `GET https://api.anthropic.com/v1/organizations/usage_report/messages`<br>`GET https://api.anthropic.com/v1/organizations/cost_report`<br>头：`x-api-key: sk-ant-admin...`、`anthropic-version: 2023-06-01`<br>query：`starting_at`、`ending_at`、`bucket_width`、`group_by[]`、`limit`、`page` | Admin API key（`sk-ant-admin...`）。**不是** `sk-ant-api03-...` 普通 key | **不能**（需 Admin key） | cost_report 返回 `amount`（**分为单位**，cookbook 里除以 100 得美元）+ `currency`；usage 返回各类 token 数；**无余额字段** | ✅ 官方 Cookbook：[Usage & cost Admin API](https://platform.claude.com/cookbook/observability-usage-cost-api)｜📄 官方文档 `platform.claude.com/docs/en/manage-claude/usage-cost-api` | 有。需 **Admin key**（`console.anthropic.com/settings/admin-keys`）。粒度：usage `1m`(≤1440桶)/`1h`(≤168)/`1d`(≤31)；**cost 仅 `1d`，单次≤31 天**；分页 `has_more`/`next_page` |
| 3 | **Google Gemini API**（AI Studio） ai.google.dev | 免费额度 + 按量付费 | **无余额/花费端点**。速率限制仅在网页与官方文档表格中。key 通过 `x-goog-api-key: <key>` 或 `?key=` 传入 | Google AI API key | **不能** | 只用不报。超额返回 `429 RESOURCE_EXHAUSTED` | 📄 官方文档 `ai.google.dev/gemini-api/docs/rate-limits`、`/pricing`（本环境抓取失败，未逐字核实） | 无官方只读用量/花费 API |
| 4 | **Google Vertex AI** console.cloud.google.com | 按量付费（GCP 项目计费） | 配额：Cloud Quotas API `cloudquotas.googleapis.com`（`projects.locations.services.quotaInfos.list`）；监控：Cloud Monitoring `consumer_quota` 指标（`servlet/aiplatform.googleapis.com`）；花费用 Cloud Billing / BigQuery 账单导出 | GCP OAuth 2.0（ADC）；需相应 IAM 角色 | **不能**（Vertex AI 与 AI Studio 是两套鉴权；普通 API key 不适用） | 配额用量（已分配/上限）+ 监控指标；实际金额需走 Cloud Billing | 🗣 社区：[CodexBar vertexai](https://github.com/steipete/CodexBar/blob/main/docs/providers.md)（Cloud Monitoring `consumer_quota`）｜📄 官方文档 `cloud.google.com/docs/quotas/view-manage`（抓取失败） | 配额有 API；**花费**走 Cloud Billing API / 账单导出，非 Vertex 专属接口 |
| 5 | **xAI（Grok）** console.x.ai | 按量付费 + 预付 credit + 订阅(SuperGrok) | `GET https://management-api.x.ai/v1/billing/teams/{team_id}/prepaid/balance`<br>`POST https://management-api.x.ai/v1/billing/teams/{team_id}/usage`（近 30 天每日 USD）<br>头：`Authorization: Bearer <management key>` | **Management API key** + `team_id`（console.x.ai → Settings → Management Keys）。**推理 key 不被接受** | **不能**（需 Management key）。仅有普通推理 key 拿不到余额 | **余额**（预付 ledger，字符串 USD 分，$10 显示为 `"-1000"`，取负得余额）+ 30 天每日花费 | ✅ 社区（引用官方 URL）：[CodexBar xai.md](https://github.com/steipete/CodexBar/blob/main/docs/xai.md)｜📄 官方文档 `docs.x.ai/developers/management-api-guide`、`/rest-api-reference/management/billing`（抓取失败） | 有。需 Management key。粒度：**日**（近 30 天）。注意：ledger 在账期结束才扣减，周期内会高于 Console 实时余额 |
| 6 | **Mistral** console.mistral.ai | 免费额度 + 按量付费 + 订阅 | **无公开 API 可读余额**。第三方靠浏览器会话 cookie（`ory_session_*`、`csrftoken` → `X-CSRFTOKEN`）读 `admin.mistral.ai` 账单/额度页 | 浏览器会话 cookie（非 API key） | **不能** | 只用不报（API 层）。速率限制响应头**未证实** | 🗣 社区：[CodexBar mistral](https://github.com/steipete/CodexBar/blob/main/docs/providers.md) | 无官方 API。需会话 cookie |
| 7 | **Cohere** dashboard.cohere.com | 评估 key（免费，1000 次/月）+ 生产 key（付费） | 仅 `POST https://api.cohere.com/v1/check-api-key`（**只校验 key 有效性，不返回余额**） | `Authorization: Bearer <key>` | **不能** | 只用不报。官方文档只给额度**表格**（如 trial 20 req/min、1000 calls/月），无余额 | ✅ 官方速率限制文档（原文 MDX）：[rate-limits.mdx](https://raw.githubusercontent.com/cohere-ai/cohere-developer-experience/refs/heads/main/fern/pages/going-to-production/rate-limits.mdx)｜📄 `/v1/reference/check-api-key` | 无余额/花费接口 |
| 8 | **Groq** console.groq.com | 免费额度 + Developer + Enterprise | **无余额 API**。花费/用量在 console 网页（第三方用 `stytch_session` cookie → `POST https://api.stytchb2b.groq.com/sdk/v1/b2b/sessions/authenticate` 换 JWT → `GET https://api.groq.com/platform/v1/organizations/{orgId}/activity?start_date=&end_date=`）<br>Enterprise 兜底：`GET https://api.groq.com/v1/metrics/prometheus/api/v1/query` | 普通 API key 可读速率限制头；花费需**浏览器会话**或 **Enterprise API key** | **不能**（余额/花费）。但**速率限制剩余**能通过响应头读到 | 速率限制剩余：`x-ratelimit-*` 与 `retry-after`（RPM/RPD/TPM/TPD 四类）；花费仅网页/Enterprise | 🗣 社区：[CodexBar groq.md](https://github.com/steipete/CodexBar/blob/main/docs/groq.md)｜📄 官方 `console.groq.com/docs/rate-limits`（本环境 403） | 无官方普通 key 花费接口；Enterprise key → Prometheus |
| 9 | **Together AI** api.together.xyz | 按量付费 + credit | **未证实**存在官方余额端点。官方文档只有 GPU 集群计费说明 | `Authorization: Bearer <key>` | **不能**（未证实有接口） | 只用不报 | 📄 官方 `docs.together.ai/docs/gpu-clusters-billing`（Cloudflare 403）｜**未证实** | 未证实 |
| 10 | **Fireworks** api.fireworks.ai | 按量付费 | `GET https://api.fireworks.ai/v1/accounts/{account_id}/billing/summary?startTime=&endTime=&granularity=DAILY`<br>`GET https://api.fireworks.ai/v1/accounts/{account_id}/billingUsage`<br>`POST https://api.fireworks.ai/v1/accounts/{account_id}/queryUsageCosts`<br>头：`Authorization: Bearer <token>` | Bearer **Fireworks API key**（官方描述："Bearer authentication using your Fireworks API key"） | **能**（官方 billing API，普通 API key 即可，需带 `account_id`） | `lineItems[].totalCost` = `{currencyCode, units, nanos}`（ISO 4217 金额）+ `quantity`；按 category/model/batch 分组 | ✅ 官方文档：[Get billing summary](https://docs.fireworks.ai/api-reference/get-billing-summary)、[Get Account Usage](https://docs.fireworks.ai/api-reference/get-billing-usage) | 有。普通 API key。粒度 **DAILY**（官方说明"costs are aggregated daily"，`endTime` 不含当日的边界规则已注明） |
| 11 | **DeepInfra** deepinfra.com | 按量付费 + 预付 | 官方：`GET https://api.deepinfra.com/v1/me/rate_limit`（头 `x-api-key` 或 `Authorization: Bearer`）→ `{rate_limit, tpm_rate_limit}`<br>社区：`GET https://api.deepinfra.com/payment/checklist?compute_owed=true`（预付余额/花费上限）、`GET https://api.deepinfra.com/payment/usage`（当月花费） | API key（`x-api-key` / Bearer） | **能**（rate limit 官方确认；**余额**由社区证实普通 key 可读） | 官方只给**速率限制额度**（每模型请求数/TPM）；余额为预付（`stripe_balance` 负值表示可用余额） | ✅ 官方文档（含 OpenAPI）：[Account Rate Limit](https://docs.deepinfra.com/api-reference/account/account-rate-limit)｜🗣 余额：[CodexBar deepinfra.md](https://github.com/steipete/CodexBar/blob/main/docs/deepinfra.md) | 余额/当月花费接口**非官方文档**（社区）；官方只文档化 rate_limit |
| 12 | **OpenRouter** openrouter.ai | 预付 credit（按量） | `GET https://openrouter.ai/api/v1/key`（头 `Authorization: Bearer <key>`）<br>`GET https://openrouter.ai/api/v1/credits`（**需 Management key**） | 普通 API key 用 Bearer；`/credits` 与 Activity 需 **Management key** | **部分能**：`/key` 用普通 key 即可拿到该 key 的 `limit` / `limit_remaining` / `usage`；**账户总余额**（`total_credits - total_usage`）需 Management key | `/key` → `limit`、`limit_remaining`、`limit_reset`、`usage`、`usage_daily/weekly/monthly`、`byok_usage*`、`is_free_tier`、`is_management_key`、`free_model_daily_requests{limit,remaining,used}`、`rate_limit`(已废弃)<br>`/credits` → `data.total_credits`、`data.total_usage` | ✅ 官方 OpenAPI（逐字）：[Get current API key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-api-key)、[Get remaining credits](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits) | 有。`/key` 普通 key（日/周/月 + 全量）；`/credits` 与 `/api/v1/activity`（近 30 个完整 UTC 日）需 Management key。credits 值有 ≤60 秒缓存 |
| 13 | **Perplexity** perplexity.ai | 预付 credit | **无官方余额 API**。社区用网页会话：`GET https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default`（需 session cookie，`Origin`/`Referer` 头） | 浏览器 session cookie（**非 API key**） | **不能** | 只用不报（API 层） | 🗣 社区：[CodexBar perplexity.md](https://github.com/steipete/CodexBar/blob/main/docs/perplexity.md)（recurring/bonus/purchased credits） | 无官方 API；社区端点非公开契约 |
| 14 | **Azure OpenAI** portal.azure.com | 按量付费（Azure 订阅） | 配额：`GET https://management.azure.com/subscriptions/{sub}/providers/Microsoft.CognitiveServices/locations/{location}/usages?api-version=2024-10-01`<br>容量：`GET .../modelCapacities?api-version=2024-10-01&modelFormat=&modelName=&modelVersion=`<br>花费：`POST https://management.azure.com/{scope}/providers/Microsoft.CostManagement/query?api-version=2025-03-01`（body：`type`、`timeframe`、`dataset.granularity`） | **Azure AD OAuth2**（`Authorization: Bearer <AAD token>`）。**不是** Azure OpenAI 的 `api-key` | **不能**（`api-key` 无法查花费/配额）。需 Azure RBAC：配额看 **Cognitive Services Usages Reader**；花费看 Cost Management Reader | 配额 `currentValue`/`limit`；容量 `availableCapacity`；花费 `PreTaxCost`/`UsageDate`/`Currency` | ✅ 官方文档（逐字）：[Manage quota](https://learn.microsoft.com/en-us/azure/foundry-classic/openai/how-to/quota)、[Cost Management Query](https://learn.microsoft.com/en-us/rest/api/cost-management/query/usage) | 有，但需 AAD。粒度：Cost Management `Daily`/`None`（`timeframe`: `MonthToDate` 等）。速率限制响应头见下 |
| 15 | **AWS Bedrock** console.aws.amazon.com | 按量付费（AWS 账户） | 花费：`POST / HTTP/1.1`，`Host: ce.us-east-1.amazonaws.com`，`X-Amz-Target: AWSInsightsIndexService.GetCostAndUsage`，body 含 `TimePeriod`、`Granularity`、`Metrics`、`Filter`<br>配额：Service Quotas API `servicequotas:GetServiceQuota` / `ListServiceQuotas`（服务名 `bedrock`） | **AWS SigV4**（IAM 凭证 + `AWS_SESSION_TOKEN` 可选）；需 `ce:GetCostAndUsage` 权限 | **不能**（Bedrock 的 `bedrock-runtime` 调用凭证查不到账单；需 IAM 签名的 Cost Explorer） | 花费 `ResultsByTime[].Total / Groups[].Metrics`（`BlendedCost`、`UnblendedCost`、`AmortizedCost`…，`Amount`+`Unit`）；配额上限/已用 | ✅ 官方文档（逐字）：[GetCostAndUsage](https://docs.aws.amazon.com/aws-cost-management/latest/APIReference/API_GetCostAndUsage.html)、[Bedrock Quotas](https://docs.aws.amazon.com/bedrock/latest/userguide/quotas.html) | 有。需 IAM。粒度 `HOURLY` / `DAILY` / `MONTHLY`。**注意 Cost Explorer API 每次调用计费 $0.01** |
| 16 | **Replicate** replicate.com | 按量付费 + 预付 credit | **无官方余额 API**。第三方靠浏览器 cookie 读网页 | API token（`Authorization: Bearer <token>`）仅用于推理 | **不能** | 只用不报。速率限制见官方文档 | 📄 官方 `replicate.com/docs/topics/predictions/rate-limits`、`/topics/billing/prepaid-credit`（页面存在，正文被截断）｜🗣 [CodexBar replicate](https://github.com/steipete/CodexBar/blob/main/docs/providers.md)（web/cookie） | 无官方余额 API |
| 17 | **Hugging Face Inference** huggingface.co | 免费额度（Free $0.10/月、PRO $2.00/月）+ 按量 | 身份：`GET https://huggingface.co/api/whoami-v2`（`Authorization: Bearer <token>`）<br>计费：`GET https://huggingface.co/api/settings/billing/usage-v2`<br>钱包余额：网页 `/settings/billing`（需浏览器会话校验归属）| User Access Token（`Authorization: Bearer`）；billing 需 **Billing read** 权限（fine-grained token） | **部分能**：billing **用量/花费**用普通 `read` token 可读；**预付钱包余额**需浏览器会话（且需与 token 同账号） | billing：`usedNanoUsd`、`includedNanoUsd`（费用 = max(0, used − included)）、spending limit；ZeroGPU 配额；`whoami-v2` → 用户名/PRO 或 Free 计划 | ✅ 官方定价文档（原文 MD）：[pricing.md](https://raw.githubusercontent.com/huggingface/hub-docs/main/docs/inference-providers/pricing.md)｜🗣 [CodexBar huggingface.md](https://github.com/steipete/CodexBar/blob/main/docs/huggingface.md) | 有（`usage-v2` 在 HF OpenAPI 中列出，但**响应结构未文档化**）。身份 `whoami-v2` 有严格限流，需缓存 |

---

## 四类概念必须分清（软件对号入座）

| 概念 | 定义 | 哪些平台能通过 API 拿到 |
|---|---|---|
| **账户余额（credit）** | "我还剩多少钱可以花"——预付/充值余额 | **OpenRouter**（`/credits`，需 Management key；`/key` 给 key 级上限）、**xAI**（Management key）、**DeepInfra**（社区端点，普通 key）、**AWS/Azure/GCP**（需云侧 OAuth/SigV4，是账单金额不是余额） |
| **已用花费（cost/usage 报表）** | 历史消耗金额与 token 数，**非实时** | **OpenAI**（Admin key，`1d`）、**Anthropic**（Admin key，`1d`）、**OpenRouter**（普通 key，日/周/月）、**Fireworks**（普通 key，日）、**AWS Bedrock**（IAM，小时/日/月）、**Azure**（AAD，日）、**HF**（普通 token） |
| **速率限制剩余（rate limit headers）** | 当前这一分钟/这一天还能发多少请求/token，**与钱无关** | **Azure OpenAI**（逐字确认 `x-ratelimit-remaining-requests`/`-tokens` 等）、**Groq**（`x-ratelimit-*` + `retry-after`，社区）、**DeepInfra**（官方 `tpm_rate_limit`）、**OpenAI/Anthropic**（官方有 `x-ratelimit-*` / `anthropic-ratelimit-*` 头，但本环境未逐字核实文档页）、**Together/Replicate**（未证实） |
| **订阅额度（ChatGPT/Claude 订阅制配额）** | ChatGPT Plus/Pro、Claude Pro/Max、SuperGrok 的用量窗口 | **全部 17 家：无一家提供公开 API**。只能走各家的官方客户端本地凭证（如 Grok CLI 的 `~/.grok/auth.json` + `x.ai/billing` JSON-RPC），属未文档化逆向 |

**给 TokenAgency 的关键提醒**：速率限制响应头是"我这一分钟还能用多少"，不是"我还剩多少额度"。两者都不能互相替代，UI 上必须分开呈现。

---

## 关键发现（每条带 URL）

1. **OpenAI 的花费查询必须用 Admin key，普通 project key 一律读不到。**
   官方 SDK 源码里每个 organization 端点都显式标注了 `security={"admin_api_key_auth": True}`，`/organization/costs` 的 `bucket_width` **只支持 `1d`**（用量接口才支持 `1m/1h/1d`）——这意味着 OpenAI 的花费数据最快也只能按天拿到，做不到准实时。
   证据：[openai-python usage.py](https://github.com/openai/openai-python/blob/main/src/openai/resources/admin/organization/usage.py)

2. **Anthropic 的 cost report 返回的是"分"，不是美元。** 官方 Cookbook 明确在示例里 `bucket_cost / 100` 才是 USD；且 `bucket_width` **只支持 `1d`、单次最多 31 天**，而 usage report 支持 `1m`（≤1440 桶）。
   证据：[Anthropic Usage & cost Admin API Cookbook](https://platform.claude.com/cookbook/observability-usage-cost-api)

3. **OpenRouter 是唯一"普通 API key 就能拿到额度信息"的主流聚合器，但余额和 key 花费是两件事。**
   `GET /api/v1/key` 用普通 key 返回 `limit`、`limit_remaining`、`usage`、`usage_daily/weekly/monthly`、`is_free_tier`、`free_model_daily_requests{limit,remaining,used}`；而账户总余额 `total_credits - total_usage` 来自 `/api/v1/credits`，**文档明确写 "[Management key] required"**。软件要把"key 级花费上限"和"账户预付余额"当成两个独立指标。
   证据：[Get remaining credits](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits)、[Get current API key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-api-key)

4. **Azure OpenAI 的 `api-key` 与"查花费/配额"完全是两套凭证体系。** 查配额必须用 Azure AD OAuth + **Cognitive Services Usages Reader** 角色（官方强调该角色必须在**订阅**范围授予，资源组/资源级无效）；查花费走 Cost Management Query API。另外 Azure 是**唯一逐字确认提供完整速率限制响应头**的平台：`x-ratelimit-limit-requests`、`x-ratelimit-limit-tokens`、`x-ratelimit-remaining-requests`、`x-ratelimit-remaining-tokens`、`x-ratelimit-reset-requests`、`x-ratelimit-reset-tokens`、`retry-after-ms`。
   证据：[Manage quota](https://learn.microsoft.com/en-us/azure/foundry-classic/openai/how-to/quota)、[Cost Management Query](https://learn.microsoft.com/en-us/rest/api/cost-management/query/usage)

5. **AWS Bedrock 查花费要 SigV4 签名，而且查询本身要花钱。** `GetCostAndUsage` 需要 `ce:GetCostAndUsage` IAM 权限，端点 `ce.us-east-1.amazonaws.com` + `X-Amz-Target: AWSInsightsIndexService.GetCostAndUsage`；第三方追踪器实测提示 **Cost Explorer API 每次请求计费 $0.01**，所以软件必须控制刷新频率，不能像轮询本地缓存那样频繁调用。
   证据：[GetCostAndUsage](https://docs.aws.amazon.com/aws-cost-management/latest/APIReference/API_GetCostAndUsage.html)、[CodexBar bedrock.md](https://github.com/steipete/CodexBar/blob/main/docs/bedrock.md)

6. **xAI 有官方 Management API，而且它明确把"开发者平台预付余额"和"SuperGrok 订阅配额"分成了两个互不相通的产品。** 管理端点是 `GET https://management-api.x.ai/v1/billing/teams/{team_id}/prepaid/balance`（Bearer Management key，**推理 key 不被接受**）；返回的是**字符串 USD 分且为倒置 ledger**（$10 充值显示 `"-1000"`）。订阅侧的 Grok 配额走 CLI 本地凭证 `~/.grok/auth.json`，属未文档化逆向。
   证据：[CodexBar xai.md](https://github.com/steipete/CodexBar/blob/main/docs/xai.md)

7. **Gemini API（AI Studio）、Cohere、Mistral、Perplexity、Together、Replicate 这 6 家没有任何官方余额/花费 API。** 对这一类，软件唯一现实的路径是引导用户看网页，或用各产品自己的界面会话（Groq 用 `stytch_session` cookie 换 JWT 读 console activity API；Mistral 用 `ory_session_*` cookie 读 `admin.mistral.ai`；Perplexity 用 session cookie 读 `/rest/billing/credits`）——这些全是未公开契约，随时可能失效，必须做失败降级而不是当成稳定接口。
   证据：[CodexBar providers.md](https://github.com/steipete/CodexBar/blob/main/docs/providers.md)（含逐家数据源说明）

8. **订阅制配额是本项目的硬边界。** ChatGPT Plus/Pro、Claude Pro/Max、SuperGrok 的用量窗口没有任何一家提供公开 API；官方文档只在网页控制台呈现。软件若想覆盖这部分，只能依赖逆向的本地凭证，且必须标注"非官方、可能失效"。
   证据：各平台官方计费文档均只描述 API 按量计费；[Hugging Face 定价](https://raw.githubusercontent.com/huggingface/hub-docs/main/docs/inference-providers/pricing.md) 明确订阅 credit 是"跨产品共享的通用 compute credit"，不由推理接口单独暴露余额。

---

## 实现优先级建议（按"普通 key 可得性"排序）

| 优先级 | 平台 | 理由 |
|---|---|---|
| P0 | **OpenRouter** | 普通 key 就能拿到 `limit_remaining` + 日/周/月花费，字段最完整、官方文档最清晰 |
| P0 | **Fireworks** | 官方 billing API，普通 Bearer key，日粒度 |
| P1 | **DeepInfra** | 官方 `rate_limit` + 社区证实普通 key 可读预付余额 |
| P1 | **Hugging Face** | 官方 `whoami-v2` + billing usage（需 Billing read 权限） |
| P2 | **OpenAI / Anthropic / xAI** | 需引导用户创建**第二种 key**（Admin / Admin / Management），要在 UI 里明确说明与推理 key 的区别 |
| P2 | **Azure / AWS / Vertex** | 需云侧 OAuth/SigV4，接入成本高；AWS 还要控制调用频率以省钱 |
| P3 | **Gemini / Cohere / Mistral / Perplexity / Together / Replicate** | 无公开 API，只能降级为"打开网页"或标注未证实 |

---

## 验证说明（诚实标注）

本次调研中，以下官方文档域名被 Cloudflare 或网络策略拦截，**未能逐字抓取其正文**，因此相关结论改由**官方 SDK 源码**或**有出处的第三方追踪器文档**支撑，并在表中标注了证据等级：

- 被拦截：`platform.openai.com`、`developers.openai.com`、`docs.together.ai`、`console.groq.com`、`ai.google.dev`、`docs.x.ai`、`docs.mistral.ai`、`docs.perplexity.ai`、`cloud.google.com`、`docs.cloud.google.com`、`docs.claude.com`（重定向）
- 成功逐字抓取：`docs.fireworks.ai`、`docs.deepinfra.com`、`openrouter.ai/docs`、`platform.claude.com/cookbook`、`learn.microsoft.com`、`docs.aws.amazon.com`、`docs.cohere.com`（速率限制原文 MDX）、`replicate.com/docs`、GitHub API/raw（官方 SDK 源码与社区文档）

**因此**：标 ✅ 的条目可直接依赖；标 📄 的条目端点名称来自官方文档 URL 与搜索结果摘要，建议在实现前由有网络访问权的环境复核一次；标 🗣 的条目依赖逆向/第三方，**不应作为稳定契约**。

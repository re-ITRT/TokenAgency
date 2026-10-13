/**
 * 服务商详情 `/providers/:id`（[07 §3.2]）—— **查看与编辑一体**。
 *
 * ## 这一页为什么长这样（用户第 75 轮原话）
 *
 * 「服务商编辑页面不走下一步下一步，直接把所有信息在点开服务商之后提供可视化编辑界面，
 * 相当于编辑和查看一体了，当然不是纯文本编辑」。
 *
 * 所以：点开一个服务商，**这一页本身就是编辑器** —— 字段就地可改、存成一张表单，
 * 不再有「编辑」按钮把你带进"下一步 / 下一步"的两步向导。表单控件全部是可视控件
 * （输入框 / 下拉 / 开关 / 可增删的头表），没有任何一处让用户直接编辑 JSON。
 *
 * ## 三条不可退让的纪律
 *
 * 1. **载荷以原始记录为底**（[`buildProviderPayload`]，见 `./editor.ts` 文件头）：
 *    只覆盖用户真的改过的键。第 72 轮的编辑向导只发 4 个字段，于是"改个名字"会静默抹掉
 *    鉴权头 / 超时 / TLS / 附加头 / 默认线路 —— 那是真实的数据丢失，这一页必须根治它。
 * 2. **读不到的不许假装能改**：拿到的是只读视图（没有连接配置字段）时整页只读，
 *    并说清"为什么"（覆盖写会用默认值抹掉线上配置）。不排空占位、不给假保存按钮。
 * 3. **明文密钥只走密钥仓库**：`providers-edit-secret` 的值只交给
 *    `credential_set_secret`，不进 `provider_upsert` 的载荷、不进错误消息、成功后立刻清空。
 *
 * ## 第 80 轮：这一页是「基础数据 + 线路」
 *
 * 用户原话：「具体哪个服务商，点进去，是**编辑和显示基础数据(url key 协议 线路)，
 * 和编辑线路(查看线路信息, 编辑网络代理)**的地方」。于是这一页只有两块内容：
 *
 * 1. **基础数据**（可编辑）：显示名 / 协议 / `base_url` / 启用 / 服务商级默认网络代理 /
 *    **唯一一把密钥**（留空 = 不改，填了 = 写到这家那条凭据）+ 这条凭据的额度窗口；
 * 2. **线路**（这一页的主体）：这家服务商**自己的线路列表**（`channel_list` 过滤到这一家），
 *    一行一条 —— 上游模型名、网络代理（线路自己的 `lane`）、优先级/权重、启用状态，
 *    以及「改代理」（`channel_upsert`）/「启停」（`channel_set_enabled`）/「移除」
 *    （`channel_delete`，走确认框）。
 *
 * **下线**的东西（硬规则 3 与 §4 S3）：整张**凭据表**（多行 / 多选 / 批量删除 /
 * 每行的额度圈）。一个服务商就一把密钥；要多个账号 → **加多个服务商、取不同名字**。
 * 也**不再创建模型**（模型的创建/分配在 `/models`）：原来那一节「模型目录」只保留
 * 「扫描上游模型 → 勾一下」这个能力，改叫**创建线路**（契约暂时不变：仍调 `provider_model_attach`）。
 *
 * ## 数据流
 *
 * 读：`provider_list`（这一行）/ `credential_list`（这家那唯一一条凭据）/ `lane_list` /
 * `channel_list`（这一家的线路）/ `provider_model_catalog`（扫描记录 = 创建线路的候选）。
 * 写：`provider_upsert`（整条覆盖写）、`credential_upsert` + `credential_set_secret`
 * （只有填了新密钥 / 保存额度窗口）、`provider_model_attach`（创建线路）、
 * `channel_upsert`（改线路的网络代理）、`channel_set_enabled`、以及 `provider_delete` /
 * `channel_delete`（删除，语义与列表页完全一致）。
 */

import AddIcon from "@mui/icons-material/Add";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import InfoOutlinedIcon from "@mui/icons-material/InfoOutlined";
import RefreshIcon from "@mui/icons-material/Refresh";
import {
  Box,
  Button,
  IconButton,
  Paper,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { useNavigate, useParams } from "react-router-dom";

import { api, errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { LaneChoiceGroup, ModelCatalogEntry, ProviderInput } from "@/shared/bindings";
import {
  SwitchControl,
  SECTION_SX,
  Caption,
  ConfirmDelete,
  HelpTip,
  PageHeader,
  StatusDot,
  type SemStatus,
} from "@/shared/components";
import { DASH, formatInt, formatLatency, formatPercent } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
// 读后端原始对象的唯一实现（原先本文件自己写了 asRow/pickStr/pickNum/pickBool）
import { asRow, pickBool, pickNum, pickStr } from "@/shared/row";
import { radius, typography } from "@/theme";

import {
  appendHeader,
  buildProviderPayload,
  canEdit,
  draftFromRow,
  isDirty,
  protocolChoices,
  providerLaneChoices,
  resolveLaneChoice,
  removeHeader,
  updateHeader,
  validateDraft,
  type ProviderEditDraft,
} from "./editor";
import {
  credentialsOf,
  laneNameOf,
  readCredential,
  readLane,
  readProvider,
  type CredentialRow,
  type HeaderPair,
  type ProviderRow,
} from "./model";
import { defaultCredentialId, defaultCredentialPayload, linesCreatedNoticeVars } from "./providerWizardModel";
import { PROTOCOLS } from "./protocols";
import { FIELD_LABEL, SelectField } from "./wizards";

// ================================================================ 文案映射（显式表，不用模板拼 key）

const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

const AUTH_LABEL: Record<string, I18nKey> = {
  bearer: "providers.authScheme.bearer",
  header: "providers.authScheme.header",
  query: "providers.authScheme.query",
  none: "providers.authScheme.none",
};

const CONFLICT_LABEL: Record<string, I18nKey> = {
  keep_client: "providers.headerConflict.keep_client",
  override: "providers.headerConflict.override",
};

const PASSTHROUGH_HEADER_LABEL: Record<string, I18nKey> = {
  pass: "providers.passthrough.header.pass",
  allowlist: "providers.passthrough.header.allowlist",
};

const PASSTHROUGH_BODY_LABEL: Record<string, I18nKey> = {
  preserve: "providers.passthrough.body.preserve",
  strip: "providers.passthrough.body.strip",
};

/** 服务商健康状态（契约视图的 `status`）→ 全站状态语义（[15 §3.2]）。 */
const STATUS_SEM: Record<string, SemStatus> = { ok: "ok", warn: "warn", error: "error", off: "off" };
const STATUS_LABEL: Record<string, I18nKey> = {
  ok: "component.statusDot.ok",
  warn: "component.statusDot.warn",
  error: "component.statusDot.error",
  off: "component.statusDot.off",
};

/** 数值缺失一律画「—」（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */


/** 字段网格：宽度自适应，窄窗口自动换行（表单控件需要比"值"更宽的下限）。 */
const GRID_SX = {
  display: "grid",
  gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))",
  gap: 4,
} as const;

const SECTION_HEAD_SX = { display: "flex", alignItems: "center", gap: 2, minWidth: 0 } as const;

/** 协议下拉的空值（`Protocol` 是必填，空就是真的空，不放"请选择…"的假选项）。 */
const REQUIRED_LABEL_KEY: I18nKey = "wizard.required";

// ================================================================ 本轮新增的文案键

/**
 * S3 这一页新用的文案键由主管补进 `src/shared/i18n/**`（本任务的写作用域不含 i18n）。
 * 键还没落地时 `I18nKey`（= `keyof typeof zhCN`）里没有它们，直接写会让 `tsc --noEmit` 变红 ——
 * 这里**集中断言一次**，让"代码先落地、文案随后补"这两件事解耦（与 `pages/models/index.tsx`
 * 同一手法）；键补齐后可以删掉这个函数，调用点一个字都不用改。
 * 需要补的键与中英文案见任务汇报。
 */
import { i18nKey } from "@/shared/i18n";

/** 本页新增的键（集中一处，方便核对"哪些还等着补"）。 */
const K = {
  /** 「线路」/「Lines」 */
  linesTitle: i18nKey("providers.lines.title"),
  /** 「这家服务商自己的线路：一行一条上游模型。网络代理就改在这里」 */
  linesHint: i18nKey("providers.lines.hint"),
  /** 列头「名称」（列表里只出现一次；行里不再重复标签） */
  linesColName: i18nKey("providers.lines.colName"),
  /** 「移除线路」/「Remove line」 */
  linesRemove: i18nKey("providers.lines.remove"),
  /** 「这家服务商还没有线路：点上面扫描」 */
  linesEmpty: i18nKey("providers.lines.empty"),
  /** 「读不到线路清单」 */
  linesNotRead: i18nKey("providers.lines.notRead"),
  /** 「已扫描 N 个上游模型：本次新建 M 条线路（目录里共 K 条），全部默认关闭。」 */
  linesCreatedAll: i18nKey("providers.lines.createdAll"),
  /** 「全选（全部启用）」 */
  linesAll: i18nKey("providers.lines.all"),
  /** 「已启用 {{enabled}} / {{total}}」 */
  linesAllCount: i18nKey("providers.lines.allCount"),
  /** 「已启用 {{count}} 条线路」 */
  linesAllEnabled: i18nKey("providers.lines.allEnabled"),
  /** 「已关闭 {{count}} 条线路」 */
  linesAllDisabled: i18nKey("providers.lines.allDisabled"),
  /** 「扫描记录里没有这一条」 */
  linesRawNone: i18nKey("providers.lines.rawNone"),
  /** 「线路详情」（列表行右端的入口：上游原文 / id / 优先级权重 / 移除都在里面） */
  linesInfo: i18nKey("providers.lines.info"),
  /** 「目录里有 N 条还没有线路」 */
  linesMissing: i18nKey("providers.lines.missing"),
  /** 「密钥」/「API key」 */
  keyTitle: i18nKey("providers.key.title"),
  /** 「这家服务商挂了 {{count}} 份凭据；本页编辑的是 {{id}}」 */
  keyMultiple: i18nKey("providers.key.multiple"),
} as const;

// ================================================================ 删除

/**
 * 一次删除的目标。
 *
 * 本页只剩**两条**单条删除（页头删服务商、线路行移除线路）：凭据表的批量删除随
 * 「一个服务商一把密钥」下线（硬规则 3），所以这里不再有 `batch` 这一档。
 */
type DeleteTarget = {
  readonly ids: readonly string[];
  readonly label: string;
  readonly consequences: readonly string[];
};

/** 执行完的**事实**（不带文案：文案由页面用 `t(...)` 拼）。 */
type DeleteOutcome = {
  deletedIds: string[];
  failures: { id: string; message: string }[];
};

/** 逐条执行删除：**一条失败不中止其余条**。与 `providers/index.tsx` 同一口径。 */
async function runDelete(
  target: DeleteTarget,
  force: boolean,
  remove: (id: string, force: boolean) => Promise<unknown>,
): Promise<DeleteOutcome> {
  const deletedIds: string[] = [];
  const failures: { id: string; message: string }[] = [];

  for (const id of target.ids) {
    try {
      await remove(id, force);
      deletedIds.push(id);
    } catch (err) {
      failures.push({ id, message: errorMessage(err) });
    }
  }

  return { deletedIds, failures };
}

/**
 * 按顺序失效一批查询并**等它们取完**。
 *
 * 为什么是 `for await` 而不是 `Promise.all`：`await` 的意义是"重取真的发生了"，
 * 三处调用（保存 / 创建线路 / 改代理与启停）都要让界面上的派生数据在"成功"这句话说出来之前就位。
 */
async function invalidate(qc: QueryClient, keys: readonly (readonly unknown[])[]): Promise<void> {
  for (const key of keys) {
    await qc.invalidateQueries({ queryKey: key });
  }
}

// ================================================================ 小组件

/**
 * 小字（`caption` 变体，次要色）。
 *
 * `testId` 是**显式**参数而不是透传 `data-*`：把 `data-testid` 直接写在 `<Caption>` 上
 * 是**静默失效**的（组件只解构 `children`，属性被丢掉，测试里就成了"元素不存在"）。
 */

/** 区块标题 + **至多一个**问号（：一排问号比一排字更糟）。 */
function SectionTitle({ title, helpText, helpTestId }: { title: string; helpText?: string; helpTestId?: string }) {
  return (
    <Box sx={SECTION_HEAD_SX}>
      <Typography variant="h4">{title}</Typography>
      {helpText === undefined || helpTestId === undefined ? null : (
        <HelpTip text={helpText} testId={helpTestId} />
      )}
    </Box>
  );
}

/** 一个字段格：标签（overline）+ 控件。层级只靠字号与颜色，没有装饰（[15 §1]）。 */
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Box sx={{ minWidth: 0 }}>
      <Typography variant="overline" sx={{ color: "text.secondary", display: "block" }}>
        {label}
      </Typography>
      <Box sx={{ minWidth: 0 }}>{children}</Box>
    </Box>
  );
}

/** 普通值（界面语言）—— 只读展示用。`testId` 显式传入（理由同 `Caption`：不透传 `data-*`）。 */
function Value({ children, testId }: { children: ReactNode; testId?: string }) {
  return (
    <Typography variant="body1" data-testid={testId} sx={{ overflowWrap: "anywhere" }}>
      {children}
    </Typography>
  );
}

/** 等宽值（id / URL / 头名 / 密钥引用这类"机器字符串"）。 */
function MonoValue({ children, testId }: { children: ReactNode; testId?: string }) {
  return (
    <Typography
      variant="body1"
      data-testid={testId}
      sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
    >
      {children}
    </Typography>
  );
}

/**
 * 一个可编辑的文本控件。`data-testid` 挂在**外层容器**上（MUI 的 `TextField` 不吃它，会 TS2353）——
 * 真机 E2E 与单测都用 `[data-testid="X"] input` 命中同一个形状。
 */
function TextControl({
  testId,
  label,
  value,
  onChange,
  disabled = false,
  multiline = false,
  inputMode,
  type,
  autoComplete,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  multiline?: boolean;
  inputMode?: "url" | "numeric" | "decimal";
  type?: string;
  autoComplete?: string;
}) {
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        size="small"
        fullWidth
        label={label}
        value={value}
        disabled={disabled}
        multiline={multiline}
        minRows={multiline ? 3 : undefined}
        type={type}
        autoComplete={autoComplete}
        onChange={(event) => onChange(event.target.value)}
        slotProps={{
          htmlInput: inputMode === undefined ? undefined : { inputMode },
          inputLabel: multiline ? { shrink: value !== "" } : undefined,
        }}
      />
    </Stack>
  );
}

/**
 * 一个可编辑的开关。
 *
 *：形状已收敛到共享件 [`SwitchControl`]（`@/shared/components`），
 * 本文件不再自己实现。这里保留一个同名薄包装是为了**不动本页 4 个调用点**；
 * 新代码直接用共享件即可。
 */
function ProviderSwitch(props: React.ComponentProps<typeof SwitchControl>) {
  return <SwitchControl {...props} />;
}

/** 把后端的线上值翻成界面语言；契约外的取值**原样显示**（是后端说的话，不改写、不猜）。 */
function labelOf(
  map: Record<string, I18nKey>,
  t: (key: I18nKey) => string,
  raw: string | undefined,
): string | undefined {
  if (raw === undefined) return undefined;
  const key = map[raw];
  return key === undefined ? raw : t(key);
}

// ================================================================ 线路（：这家服务商**自己的**线路）

type RawRow = Record<string, unknown>;

/**
 * 上游原文是不是"没有"。
 *
 * `{}` 是**唯一**的"没记到原文"表示（store 那一列 `NOT NULL DEFAULT '{}'`，
 * 上游那一条不是对象或压根没扫到时也是它）—— 所以这里只用"有没有键"判断，
 * 不去解释内容（解释就变成我们改写上游了）。
 */
function isEmptyRaw(raw: unknown): boolean {
  const row = asRow(raw);
  return Object.keys(row).length === 0;
}

/**
 * 一条线路（`channel_list` 的一行）。
 *
 * **`raw` 必须原样带着**：`channel_upsert` 是**整条覆盖写**（对 `provider_upsert`
 * 记下的同一条纪律），少一个键就会把 `upstream_model` / `capabilities` / `transform` 抹成
 * 后端默认值 —— 那是真实的数据丢失。
 */
export type LineRow = {
  id: string;
  providerId: string;
  /** 上游模型名 —— **线路的身份**（；逻辑模型名不在这里）。 */
  upstreamModel: string;
  /** 这条线路**自己的**网络代理；`null` = 不指定（按继承链落到凭据 / 服务商 / 默认线路）。 */
  laneId: string | null;
  priority?: number;
  weight?: number;
  /** `undefined` = 后端没给这一列（**不拿 `false` 冒充"已停用"**）。 */
  enabled?: boolean;
  /** `channel_list` 里的原始对象（写载荷的底）。 */
  raw: RawRow;
};

/** 读一条线路：**只认读得到的列**（缺 = `undefined`，不猜）。 */
export function readLine(raw: unknown): LineRow {
  const row = asRow(raw);
  return {
    id: pickStr(row, "id") ?? "",
    providerId: pickStr(row, "provider") ?? "",
    upstreamModel: pickStr(row, "upstream_model") ?? "",
    laneId: pickStr(row, "lane") ?? null,
    priority: pickNum(row, "priority"),
    weight: pickNum(row, "weight"),
    enabled: pickBool(row, "enabled"),
    raw: row,
  };
}

/**
 * 这家服务商的线路。
 *
 * `undefined` = `channel_list` 读不到（**不是**"没有线路"—— 两件事不能混）。
 * 顺序保持后端给的原样（顺序就是用户的配置意图，本页不重排）。
 */
export function linesOfProvider(
  channels: readonly unknown[] | undefined,
  providerId: string,
): LineRow[] | undefined {
  if (channels === undefined) return undefined;
  return channels.map((raw) => readLine(raw)).filter((line) => line.providerId === providerId);
}

/*
 *（第 322 轮）这里曾有 `linePayloadForLane(line, lane)`：改一条线路的**网络代理**
 * 时构造 `channel_upsert` 的载荷（"以原始记录为底，只覆盖 lane 一个键"）。
 *
 * 它随"放弃线路级别的分配"删除：`ChannelConfig.lane` 字段已经不存在，
 * 而**线路级的代理控件**本身在 那一轮就从界面上撤掉了 ——
 * 也就是说这个函数**早就没有调用方了**，只是没人回来删。
 *
 * 现在改网络代理只有一处：**服务商级**的「网络代理」下拉（走 `provider_upsert` 的 `lane`）。
 * 那条路径的载荷纪律不变，见 `providerPayload`。
 */

/**
 * 这家服务商**那唯一一把**凭据（硬规则 3：`credential` 退化成 1:1 附件）。
 *
 * 选哪一份：优先 `<provider>-default`（向导建的那一份），否则这家挂的第一份。
 * 页面用同一张规则做三件事：写密钥的目标、「额度窗口」按钮挂给谁、只读状态显示的是谁。
 * `undefined` = 这家还没有凭据（首次填密钥时才建）。
 */
export function keyCredentialOf(
  credentials: readonly CredentialRow[] | undefined,
  providerId: string,
): CredentialRow | undefined {
  const own = credentialsOf(credentials, providerId);
  return own.find((credential) => credential.id === defaultCredentialId(providerId)) ?? own[0];
}

/** 目录里"扫到了、但还没创建成线路"的行（勾一下 = 创建线路）。 */
export function pendingCatalogOf(
  catalog: readonly ModelCatalogEntry[] | undefined,
): ModelCatalogEntry[] {
  return (catalog ?? []).filter((entry) => !entry.added);
}

/** 已创建的线路里"本次扫描没再见到"的通道 id（行还在：只是这次上游没返回它 —— 不删、只标）。 */
export function staleLineIdsOf(
  catalog: readonly ModelCatalogEntry[] | undefined,
): ReadonlySet<string> {
  const ids = new Set<string>();
  for (const entry of catalog ?? []) {
    if (entry.seen_in_last_scan) continue;
    for (const channelId of entry.channel_ids) ids.add(channelId);
  }
  return ids;
}

// ---------------------------------------------------------------- 展示层（纯 props，便于测各分支）

/**
 * 线路区的**全部输入**由数据层注入（展示层不取数、不认识 IPC）。
 *
 *（用户第 82 轮的重构）：这一节**只有一个列表**。
 * * **扫描 = 扫上游 + 把清单里的每条线路都建出来**（默认关闭）：不再有"勾一下才创建"；
 * * 每行 = **上游原文** + **网络代理** + **启用/关闭的滑块**；
 * * 列表头上有**全选**：一次动作把所有线路一起启用 / 一起关闭
 *   （`channel_bulk_set_enabled`：一条命令、一次写库、一次内核重载）。
 *
 * "没有添加的线路"（`enabled: false` 的那些）就在这个列表里，**同样能设网络代理** ——
 * 这正是用户要的"一个列表里都能设"。空态一律如实说，**在任何分支都不画假行**。
 */
export type ProviderLinesProps = {
  /** 这家服务商的线路（`channel_list` 过滤到这一家）。`undefined` = 读不到。 */
  lines: readonly LineRow[] | undefined;
  /** `channel_list` 读失败的原因（后端原文含 TA 码）。 */
  linesError?: unknown;
  /** 目录记录（`provider_model_catalog`）：每一行的上游原文从这里来。`undefined` = 读不到。 */
  catalog: readonly ModelCatalogEntry[] | undefined;
  /** 目录读失败的原因（后端原文）。 */
  catalogError?: unknown;
  /** 正在扫描上游（后端在飞）。 */
  scanPending: boolean;
  /** 「重新扫描」：扫上游 + 刷新目录 + **把缺的线路补建出来**（默认关闭）。 */
  onScan: () => void;
  /** 线路的写命令（改代理 / 启停 / 移除）有在飞的。 */
  busy: boolean;
  /** 线路写命令的后端原文（含 TA 码）：改代理失败、启停失败、移除被策略拒绝，都原样显示。 */
  error?: unknown;
  /** 刚发生的事实（"已把清单里的 N 条线路建出来"）—— 只讲刚发生的事。 */
  notice?: string;
  /** 线路行上刚发生的事实（"已启用 N 条线路"）—— 显示在它发生的那一节里。 */
  lineNotice?: string;
  /** 启用 / 停用一条线路（`channel_set_enabled`，可逆）。 */
  onToggleEnabled: (line: LineRow, enabled: boolean) => void;
  /** **全选**：一次动作把列表里的线路一起启用或一起关闭。 */
  onBulkEnabled: (enabled: boolean) => void;
  /**
   * 「移除线路」（`channel_delete`，走确认框）。删除与**停用**是两件事：停用的行还在列表里。
   *
   * 不传 = **这一页没有删除入口**（向导里就是如此：它是新增流程，删除走详情页 / 通道页）。
   * 这时候**不画**那个按钮 —— 一个点了没反应的按钮比没有按钮更糟。
   */
  onRemove?: (line: LineRow) => void;
  /** 「扫描」按钮的 `data-testid`（向导里历史 testid 是 `pw-scan`，E2E 与单测都按它命中）。 */
  scanTestId?: string;
  /** 「扫描」按钮的文案（向导里第一次扫描叫"一键扫描"，详情页叫"重新扫描"）。 */
  scanLabel?: string;
};

/** 一行里显示的上游原文（目录里没有这一条线路时是 `undefined`）。 */
function rawOfUpstream(
  catalog: readonly ModelCatalogEntry[] | undefined,
  upstreamModel: string,
): { found: boolean; raw: unknown } {
  const entry = (catalog ?? []).find((e) => e.upstream_model === upstreamModel);
  return { found: entry !== undefined, raw: entry?.raw };
}

/**
 * 上游原文的**单行预览**（`<details>` 摘要里显示的那一段）。
 *
 * 规则：**取原文自己的开头**，只在太长时截断并加省略号 —— 不挑字段、不重排、不加自己的话
 * （`raw` 是"上游真的给了什么"的证据，改写它就是伪证）。压掉换行只是为了能放进一行。
 */
export function rawPreview(raw: unknown, limit = 160): string {
  const text = JSON.stringify(raw) ?? "";
  const oneLine = text.replace(/\s+/g, " ");
  return oneLine.length <= limit ? oneLine : `${oneLine.slice(0, limit)}…`;
}

export function LinesSection({
  lines,
  linesError,
  catalog,
  catalogError,
  scanPending,
  onScan,
  busy,
  error,
  notice,
  lineNotice,
  onToggleEnabled,
  onBulkEnabled,
  onRemove,
  scanTestId = "providers-model-scan",
  scanLabel,
}: ProviderLinesProps) {
  const { t } = useTranslation();
  /**
   * 哪一行的「详情」是展开的。
   *
   * 列表里**只放名字 + 网络代理 + 滑块**（用户原话："一条就是左边名字，右边选择滑块就好了"）；
   * 上游原文 / 线路 id / 优先级权重 / 移除都收进这个点开的详情里。
   * 这是纯 UI 状态（不涉及数据），所以留在展示层。
   */
  const [openInfoId, setOpenInfoId] = useState<string | undefined>(undefined);
  /** 目录里有、这家还没有线路的条目（正常情况为空：扫描会把它们都建出来）。 */
  const missing = pendingCatalogOf(catalog);
  const staleIds = staleLineIdsOf(catalog);
  const total = (lines ?? []).length;
  const enabledCount = (lines ?? []).filter((l) => l.enabled === true).length;
  const allEnabled = total > 0 && enabledCount === total;

  return (
    <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-lines">
      <Stack spacing={4}>
        <SectionTitle
          title={t(K.linesTitle)}
          helpText={t(K.linesHint)}
          helpTestId="providers-lines-help"
        />

        {/* ① 扫描：扫上游 + **把清单里的每条线路都建出来**（默认关闭）
            —— 之后这里不再有"勾一下才创建"。
            [15 §2] **极简**：那句解释收进本节标题的问号（`linesHint`），版面上只留一个图标按钮 +
            一个只在真等后端时才出现的状态字 —— 节标题右边已经有一个问号，不需要第二份说明。 */}
        <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Button
            variant="outlined"
            size="small"
            startIcon={<RefreshIcon fontSize="small" />}
            data-testid={scanTestId}
            disabled={scanPending || busy}
            onClick={onScan}
          >
            {scanLabel ?? t("providers.catalog.rescan")}
          </Button>
          {scanPending ? (
            <Caption testId="providers-model-scanning">{t("providers.catalog.rescanning")}</Caption>
          ) : null}
        </Stack>

        {catalogError !== undefined && catalogError !== null ? (
          <Typography variant="body1" data-testid="providers-model-error" sx={{ color: "error.main" }}>
            {errorMessage(catalogError)}
          </Typography>
        ) : null}
        {notice === undefined ? null : (
          <Typography variant="body1" data-testid="providers-model-notice" sx={{ color: "success.main" }}>
            {notice}
          </Typography>
        )}

        {/* ② 读不到 / 空目录：如实说（上面的扫描仍然可用）。
            **有目录时不画计数**：列表本身就是计数，"目录里记着 N 个模型"是重复信息。 */}
        {catalog === undefined ? (
          <Typography variant="body1" data-testid="providers-model-unreadable">
            {t("providers.catalog.notRead")}
          </Typography>
        ) : catalog.length === 0 ? (
          <Typography variant="body1" data-testid="providers-model-empty">
            {t("providers.catalog.empty")}
          </Typography>
        ) : null}

        {/* ③ 唯一的列表：**列头说一次，行里不再重复标签**（[15 §2] 极简 + 符号化） */}
        <Stack spacing={2}>
          {linesError !== undefined && linesError !== null ? (
            /* 读线路失败的**后端原文**（含 TA 码）：原样显示，不改写成一句"加载失败" */
            <Typography
              variant="body1"
              data-testid="providers-lines-error"
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {errorMessage(linesError)}
            </Typography>
          ) : null}
          {error !== undefined && error !== null ? (
            <Typography variant="body1" data-testid="providers-line-error" sx={{ color: "error.main" }}>
              {errorMessage(error)}
            </Typography>
          ) : null}
          {lineNotice === undefined ? null : (
            <Typography variant="body1" data-testid="providers-line-notice" sx={{ color: "success.main" }}>
              {lineNotice}
            </Typography>
          )}

          {lines === undefined ? (
            <Typography variant="body1" data-testid="providers-lines-unreadable">
              {t(K.linesNotRead)}
            </Typography>
          ) : (
            <Stack spacing={2} data-testid="providers-line-list">
              {total === 0 ? (
                /* 空态如实说：点上面的扫描，线路会被建出来（**不画假行**） */
                <Typography variant="body1" data-testid="providers-lines-empty">
                  {t(K.linesEmpty)}
                </Typography>
              ) : (
                <>
                  {/**
                   * **列头**（[15 §2] 极简 + 符号化）：三列的标签在这里说**一次**，
                   * 行里就不再重复画「网络代理」「启用」——
                   * 四行时省下 8 个标签，二十行时省下 40 个。
                   * 计数也收进这里：`3/4` 比「已启用 3 / 4」+「4 条线路」+「目录里记着 3 个模型」
                   * 三行字少得多，而且**信息一个不少**（全选的开关就在同一行）。
                   */}
                  <Stack
                    direction="row"
                    spacing={2}
                    alignItems="center"
                    data-testid="providers-lines-header"
                    sx={{ px: 2, color: "text.secondary" }}
                  >
                    <Typography variant="caption" sx={{ flex: 1, minWidth: 0 }}>
                      {t(K.linesColName)}
                    </Typography>
                    {/* 列头里**不再有「网络代理」那一列**：行内已经没有代理控件
                        （出口只由服务商决定），留一个没有内容的列头 = 一条指向不存在列的说明。
                        第 88 轮删行内下拉时漏了这一处（那次替换静默失败、`tsc` 也不会报），
                        第 117 轮补删。 */}
                    <ProviderSwitch
                      testId="providers-lines-all"
                      label={t(K.linesAll)}
                      checked={allEnabled}
                      disabled={busy || total === 0}
                      onChange={(enabled) => onBulkEnabled(enabled)}
                    />
                    <Caption testId="providers-lines-all-count">
                      {t(K.linesAllCount, { enabled: enabledCount, total })}
                    </Caption>
                  </Stack>

                  {lines.map((line) => {
                    const raw = rawOfUpstream(catalog, line.upstreamModel);
                    const infoOpen = openInfoId === line.id;
                    return (
                      <Box
                        key={line.id}
                        data-testid="providers-line-row"
                        sx={{
                          borderRadius: `${radius.control}px`,
                          border: "1px solid",
                          borderColor: "divider",
                          bgcolor: "background.surface",
                          /**
                           * **一行 = 名字 + 右键两个控件**（用户原话："一条就是左边名字，
                           * 右边选择滑块就好了"）。整条列表因此是一眼扫得完的清单，
                           * 而不是"每条线路一张卡片"。
                           *
                           * `scrollMarginBottom`：底部那条粘性保存条会盖住滚到视口下沿的行，
                           * 留白让浏览器把行滚进来时避开它（真机 E2E 撞过一次 click intercepted）。
                           */
                          scrollMarginBottom: 8,
                        }}
                      >
                        <Stack
                          direction="row"
                          spacing={2}
                          alignItems="center"
                          sx={{ px: 2, py: 1, flexWrap: "wrap" }}
                        >
                          {/* 左：**名字**（线路的身份 = 上游模型名）*/}
                          <MonoValue testId="providers-line-upstream">
                            {line.upstreamModel === "" ? DASH : line.upstreamModel}
                          </MonoValue>
                          {staleIds.has(line.id) ? (
                            <Caption testId="providers-line-stale">{t("providers.catalog.stale")}</Caption>
                          ) : null}

                          {/* 中间留白：把两个控件推到右边 */}
                          <Box sx={{ flex: 1, minWidth: 8 }} />

                          {/* **这里不再有代理下拉**：出口只由**服务商**决定 ——
                              同一服务商下的模型打同一个 URL，用不同出口 IP 会被上游当成
                              "同账号多地并发"（用户原话："反而会封号"）。
                              线路一律继承服务商，界面上不再给第二条路。 */}

                          {/* **滑块**（启用 / 关闭）—— 列表上唯一的开关。
                              符号化：[15 §2] 开关本身就是"启用/关闭"的符号，行里不再写那两个字，
                              无障碍名称走 `label`（读屏一个字不少）。 */}
                          <ProviderSwitch
                            testId="providers-line-enabled"
                            label={
                              line.enabled === undefined
                                ? t("common.unknown")
                                : line.enabled
                                  ? t("common.enabled")
                                  : t("common.disabled")
                            }
                            hideLabel
                            checked={line.enabled ?? false}
                            disabled={busy || line.enabled === undefined}
                            onChange={(enabled) => onToggleEnabled(line, enabled)}
                          />

                          {/* 「详情」入口：上游原文 / 线路 id / 优先级权重 / 移除都在这里。
                              列表本身不显示这些 —— 用户要的是清单，不是每条的详情页。 */}
                          <IconButton
                            size="small"
                            aria-label={t(K.linesInfo)}
                            title={t(K.linesInfo)}
                            data-testid="providers-line-info"
                            disabled={busy}
                            onClick={() => setOpenInfoId(infoOpen ? undefined : line.id)}
                          >
                            <InfoOutlinedIcon fontSize="small" />
                          </IconButton>
                        </Stack>

                        {infoOpen ? (
                          <Stack
                            spacing={2}
                            data-testid="providers-line-info-panel"
                            sx={{ px: 2, pb: 2, borderTop: "1px solid", borderColor: "divider" }}
                          >
                            <Stack spacing={1} data-testid="providers-line-meta">
                              <Stack direction="row" spacing={2}>
                                <Caption>{t("network.lanes.write.id")}</Caption>
                                <Typography
                                  variant="body1"
                                  sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                                  data-testid="providers-line-id"
                                >
                                  {line.id === "" ? DASH : line.id}
                                </Typography>
                              </Stack>
                              <Stack direction="row" spacing={2}>
                                <Caption>{t("channels.col.priorityWeight")}</Caption>
                                <Typography variant="body1" data-testid="providers-line-priority">
                                  {`${line.priority === undefined ? DASH : formatInt(line.priority)} / ${
                                    line.weight === undefined ? DASH : formatInt(line.weight)
                                  }`}
                                </Typography>
                              </Stack>
                            </Stack>

                            {/* **上游原文**：逐字节原样，不美化、不挑字段、不翻译；
                                空对象如实说"上游没给对象"。 */}
                            <Stack spacing={0.5} data-testid="providers-line-raw">
                              <Caption>{t("providers.catalog.rawTitle")}</Caption>
                              {!raw.found ? (
                                <Typography
                                  variant="body2"
                                  sx={{ color: "text.secondary" }}
                                  data-testid="providers-line-raw-none"
                                >
                                  {t(K.linesRawNone)}
                                </Typography>
                              ) : isEmptyRaw(raw.raw) ? (
                                <Typography
                                  variant="body2"
                                  sx={{ color: "text.secondary" }}
                                  data-testid="providers-line-raw-empty"
                                >
                                  {t("providers.catalog.rawEmpty")}
                                </Typography>
                              ) : (
                                <Box
                                  component="pre"
                                  sx={{
                                    m: 0,
                                    p: 1,
                                    maxHeight: 200,
                                    overflow: "auto",
                                    bgcolor: "action.hover",
                                    fontFamily: typography.fontMono,
                                    fontSize: 12,
                                    whiteSpace: "pre-wrap",
                                    overflowWrap: "anywhere",
                                  }}
                                  data-testid="providers-line-raw-json"
                                >
                                  {JSON.stringify(raw.raw, null, 2)}
                                </Box>
                              )}
                            </Stack>

                            {/* 删除与停用是两件事：停用的行还在列表里（还能设代理），
                                删掉的行会消失（重新扫描能再补出来）。删除走确认框，
                                被策略点名引用时后端如实拒绝。**向导里没有这个入口**（不删东西）。 */}
                            {onRemove === undefined ? null : (
                              <Box>
                                <Button
                                  size="small"
                                  variant="outlined"
                                  color="error"
                                  disabled={busy}
                                  data-testid="providers-line-remove"
                                  onClick={() => onRemove(line)}
                                >
                                  {t(K.linesRemove)}
                                </Button>
                              </Box>
                            )}
                          </Stack>
                        ) : null}
                      </Box>
                    );
                  })}
                </>
              )}

              {/* 目录里有、但这家还没有线路的条目：正常为空（扫描会把它们都建出来）。
                  不为它们画可点的控件 —— 那会暗示"勾一下才行"，而这里已经不用勾了。 */}
              {missing.length === 0 ? null : (
                <Stack spacing={1} data-testid="providers-lines-missing">
                  <Caption testId="providers-lines-missing-count">
                    {t(K.linesMissing, { count: missing.length })}
                  </Caption>
                  {missing.map((entry) => (
                    <Typography
                      key={`${entry.provider_id}/${entry.upstream_model}`}
                      variant="body1"
                      sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                      data-testid="providers-lines-missing-row"
                    >
                      {entry.upstream_model}
                    </Typography>
                  ))}
                </Stack>
              )}
            </Stack>
          )}
        </Stack>
      </Stack>
    </Paper>
  );
}

// ================================================================ 展示层（纯 props，便于测各分支）

export type ProviderDetailBodyProps = {
  /** 路由参数里的 id（"目录里没有"这句话要把它写出来）。 */
  id: string;
  /** 目录里这一行。**`undefined` = 目录里没有这个 id**（不是"读失败"，读失败走 `error`）。 */
  row: ProviderRow | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /** 挂载的凭据：`undefined` = 读不到，空数组 = 确实没有。 */
  credentials: CredentialRow[] | undefined;
  credentialsError: unknown;
  onRetryCredentials: () => void;
  /**
   * 「自动」出口的候选（**启用的订阅**，一份一个）。
   * 由页面查好传进来：视图组件不自己查数据（本仓库的视图只吃 props，才好在没有 QueryClient
   * 的地方单测 —— 我第一版就是在这儿直接查了，测试当场红在 "No QueryClient set"）。
   */
  autoSubscriptions: readonly { id: string; name: string }[];
  /**
   * **`lane_options` 的分组**（`lane_options` 原样返回）。
   *
   * 为什么不是 `lane_list`：那一份**只有已配置的线路**，于是"用户加了订阅却在服务商这边
   * 选不到任何节点"（用户第 325 轮报的 bug，根因就在这儿）。`lane_options` 才带订阅节点
   *（后端 `compute_lane_choices` 会把已采纳版本的节点逐个列出来）。
   */
  laneChoiceGroups: readonly LaneChoiceGroup[];
  laneName: (laneId: string) => string | undefined;
  laneError: unknown;
  /** 返回列表（展示层不认识 router）。 */
  onBack: () => void;

  /**
   * 保存：数据层注入，载荷由展示层用 [`buildProviderPayload`] 从**原始记录**拼。
   * **省略 = 这一页只读**（拿到的是只读视图，覆盖写会用默认值抹掉线上配置）。
   */
  onSave?: (payload: ProviderInput, plaintextKey: string) => Promise<void>;
  /** 线路区的全部输入（见 [`ProviderLinesProps`]）；省略 = 不渲染线路区。 */
  lines?: ProviderLinesProps;
  /** 填了新密钥时要落到的凭据 id（数据层算好，只为界面如实说明"写到哪里"）。 */
  secretTarget?: { id: string; exists: boolean };

};

export function ProviderDetailBody({
  id,
  row,
  isLoading,
  error,
  onRetry,
  credentials,
  credentialsError,
  onRetryCredentials,
  autoSubscriptions,
  laneChoiceGroups,
  laneName,
  laneError,
  onBack,
  onSave,
  lines,
  secretTarget,
}: ProviderDetailBodyProps) {
  const { t } = useTranslation();

  // ---------------------------------------------------------------- 草稿

  const editable = canEdit(row) && onSave !== undefined;

  /**
   * 草稿从这一行种下来。**换一个服务商（id 变了）才重新种**：不然每一次后台重取都会把
   * 用户改了一半的输入丢掉（那是比"看不到最新值"严重得多的体验）。
   */
  const [draft, setDraft] = useState<ProviderEditDraft>(() => draftFromRow(row ?? EMPTY_ROW));
  const [baseline, setBaseline] = useState<ProviderEditDraft>(() => draftFromRow(row ?? EMPTY_ROW));
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<unknown>(undefined);
  const [savedJustNow, setSavedJustNow] = useState(false);

  useEffect(() => {
    if (row === undefined) return;
    const next = draftFromRow(row);
    setDraft(next);
    setBaseline(next);
    setSubmitted(false);
    setSaveError(undefined);
    setSavedJustNow(false);
    // 只有"换了一条记录"才重新种（id 是主键）；同一条记录的后台重取不覆盖用户输入。
  }, [row?.id]);

  const patch = (next: Partial<ProviderEditDraft>): void => {
    setSavedJustNow(false);
    setDraft((prev) => ({ ...prev, ...next }));
  };

  const issues = useMemo(() => validateDraft(draft), [draft]);
  /** 必填类问题在点过一次保存之后才列出来（打开表单就满屏红字没有意义）。 */
  const shownIssues = submitted ? issues : [];
  const dirty = editable && isDirty(draft, baseline);

  const submit = (): void => {
    setSubmitted(true);
    if (!editable || row === undefined || validateDraft(draft).length > 0) return;
    setSaving(true);
    setSaveError(undefined);
    void (async () => {
      try {
        await onSave!(buildProviderPayload(row!, draft), draft.key);
        setBaseline(draft);
        // 明文用完立刻从内存里抹掉（它已经在密钥仓库里了）
        setDraft((prev) => ({ ...prev, key: "" }));
        setSavedJustNow(true);
      } catch (err) {
        setSaveError(err);
      } finally {
        setSaving(false);
      }
    })();
  };

  const reset = (): void => {
    setDraft(baseline);
    setSubmitted(false);
    setSaveError(undefined);
    setSavedJustNow(false);
  };

  // ---------------------------------------------------------------- 这家那唯一一条凭据（硬规则 3）

  /**
   * 这一家挂的全部凭据（正常只有一份）与**页面编辑的那一份**。
   *
   * 一份服务商一把密钥；挂在多份（旧数据 / 另一条工作流还没收口）时页面**如实说**这件事
   * （`providers-key-multiple`），而不是悄悄只显示第一份。
   * "选哪一份"的规则只有 [`keyCredentialOf`] 一处（数据层写密钥时用的是同一个函数）。
   */
  const own = credentials === undefined ? undefined : credentialsOf(credentials, id);
  const keyCredential = keyCredentialOf(credentials, id);

  // ---------------------------------------------------------------- 下拉选项

  const protocolOptions = useMemo(
    () => protocolChoices(PROTOCOLS, (protocol) => t(PROTOCOL_LABEL[protocol] ?? "providers.protocol.custom"), draft.protocol),
    [draft.protocol, t],
  );
  const authOptions = useMemo(
    () =>
      (["bearer", "header", "query", "none"] as const).map((scheme) => ({
        value: scheme,
        label: t(AUTH_LABEL[scheme] ?? "providers.authScheme.bearer"),
      })),
    [t],
  );
  const conflictOptions = useMemo(
    () =>
      (["keep_client", "override"] as const).map((value) => ({
        value,
        label: t(CONFLICT_LABEL[value] ?? "providers.headerConflict.keep_client"),
      })),
    [t],
  );
  const passthroughHeaderOptions = useMemo(
    () =>
      (["pass", "allowlist"] as const).map((value) => ({
        value,
        label: t(PASSTHROUGH_HEADER_LABEL[value] ?? "providers.passthrough.header.pass"),
      })),
    [t],
  );
  const passthroughBodyOptions = useMemo(
    () =>
      (["preserve", "strip"] as const).map((value) => ({
        value,
        label: t(PASSTHROUGH_BODY_LABEL[value] ?? "providers.passthrough.body.preserve"),
      })),
    [t],
  );
  /**
   * 「自动」出口的候选 = **启用的订阅**（一份订阅一个选项；多份时带订阅名）。
   *
   * 值发的是标记 `auto:<订阅 id>`，由后端 `provider_upsert` 解析成一条合格的线路
   * （`subscription_auto` + 显式 `select`）并写回真实 id —— 前端不建线路、也不编 id。
   */
  const laneOptions = useMemo(
    () =>
      providerLaneChoices(
        laneChoiceGroups,
        t("providers.edit.laneInherit"),
        autoSubscriptions,
        t("providers.edit.laneAuto"),
      ),
    [laneChoiceGroups, t, autoSubscriptions],
  );

  return (
    <Box data-testid="providers-detail" sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {error !== undefined && error !== null ? (
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
            <Typography variant="body1" data-testid="providers-detail-error" sx={{ color: "error.main" }}>
              {t("providers.detail.error", { message: errorMessage(error) })}
            </Typography>
            <Button variant="outlined" onClick={onRetry}>
              {t("common.retry")}
            </Button>
          </Stack>
        </Paper>
      ) : isLoading ? (
        <Paper variant="outlined" sx={SECTION_SX}>
          <Typography variant="body1" sx={{ color: "text.secondary" }}>
            {t("common.loading")}
          </Typography>
        </Paper>
      ) : row === undefined ? (
        /* 目录是完整的：不在里面就是"没有这条"，不是"读失败"（失败走上面的 error 分支） */
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3} data-testid="providers-detail-notfound">
            <Typography variant="h4">{t("providers.detail.notFound.title")}</Typography>
            <Typography variant="body1">{t("providers.detail.notFound.body", { id })}</Typography>
            <Box>
              <Button variant="outlined" onClick={onBack}>
                {t("providers.detail.back")}
              </Button>
            </Box>
          </Stack>
        </Paper>
      ) : (
        <>
          {/* 只读视图：整页只读，版面上只留**结论**（"连接配置没读到：整页只读"）。
              "覆盖写会用默认值抹掉线上配置"这条原因收进「连接」区块的那一个 `?` ——
              它必须保留（用户不看会误判），但它是解释，不是状态（规则 1/2）。 */}
          {editable ? null : (
            <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-view-only">
              <Typography variant="body1">{t("providers.edit.viewOnly")}</Typography>
            </Paper>
          )}

          {/* 保存反馈：结果只讲刚刚发生的事（错误原样显示后端原文） */}
          {saveError !== undefined && saveError !== null ? (
            <Paper variant="outlined" sx={SECTION_SX}>
              <Typography variant="body1" data-testid="providers-detail-save-error" sx={{ color: "error.main" }}>
                {errorMessage(saveError)}
              </Typography>
            </Paper>
          ) : null}
          {shownIssues.length > 0 ? (
            <Paper variant="outlined" sx={SECTION_SX}>
              <Stack spacing={1} data-testid="providers-detail-issues">
                <Typography variant="body1" sx={{ color: "error.main" }}>
                  {t("providers.edit.issues")}
                </Typography>
                {shownIssues.map((issue) => (
                  <Typography
                    key={`${issue.field}-${issue.message}`}
                    variant="body1"
                    sx={{ color: "error.main" }}
                    data-testid="providers-detail-issue"
                  >
                    {`${t(FIELD_LABEL[issue.field] ?? "common.unknown")}：${t(issue.message)}`}
                  </Typography>
                ))}
              </Stack>
            </Paper>
          ) : null}
          {savedJustNow ? (
            <Paper variant="outlined" sx={SECTION_SX}>
              <Typography variant="body1" data-testid="providers-detail-save-result" sx={{ color: "success.main" }}>
                {t("providers.edit.saved")}
              </Typography>
            </Paper>
          ) : null}

          {/* ① 基本信息：名称 / 协议 / 地址 / 启用 / 默认线路 / 模型前缀（全部就地可改） */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-overview">
            <Stack spacing={4}>
              {/* 那句解释（原本铺在字段网格下面的"这四列只有后端视图才提供"）收进问号
                  ；**健康数值与「—」照旧留在版面上**（红线第 5 条）。 */}
              <SectionTitle
                title={t("providers.section.basic")}
                helpText={t("providers.honesty.detailFields")}
                helpTestId="providers-overview-help"
              />

              <Box sx={GRID_SX}>
                <Field label={t("providers.field.id")}>
                  {/* id 是主键：改名不等于换 id，所以它只读显示 */}
                  <MonoValue>{row.id === "" ? DASH : row.id}</MonoValue>
                </Field>
                <TextControl
                  testId="providers-edit-name"
                  label={t("providers.field.name")}
                  value={draft.name}
                  disabled={!editable}
                  onChange={(name) => patch({ name })}
                />
                {editable ? (
                  <SelectField
                    testId="providers-edit-protocol"
                    label={t("providers.field.protocol")}
                    value={draft.protocol}
                    onChange={(protocol) => patch({ protocol })}
                    options={protocolOptions}
                    emptyLabel={t(REQUIRED_LABEL_KEY)}
                  />
                ) : (
                  <Field label={t("providers.field.protocol")}>
                    <Value>{labelOf(PROTOCOL_LABEL, t, row.protocol) ?? DASH}</Value>
                  </Field>
                )}
                <TextControl
                  testId="providers-edit-baseUrl"
                  label={t("providers.field.baseUrl")}
                  value={draft.baseUrl}
                  disabled={!editable}
                  inputMode="url"
                  onChange={(baseUrl) => patch({ baseUrl })}
                />
                <Field label={t("providers.field.enabled")}>
                  <ProviderSwitch
                    testId="providers-edit-enabled"
                    label={draft.enabled ? t("common.enabled") : t("common.disabled")}
                    checked={draft.enabled}
                    disabled={!editable}
                    onChange={(enabled) => patch({ enabled })}
                  />
                </Field>
                {editable ? (
                  <SelectField
                    testId="providers-edit-lane"
                    label={t("providers.field.lane")}
                    value={draft.lane}
                    onChange={(lane) => patch({ lane })}
                    options={laneOptions}
                    emptyLabel={t("providers.edit.laneInherit")}
                  />
                ) : (
                  <Field label={t("providers.field.lane")}>
                    {row.lane === undefined ? (
                      <Value>{t("providers.field.laneNone")}</Value>
                    ) : laneName(row.lane) === undefined ? (
                      <MonoValue>{t("providers.field.laneUnresolved", { id: row.lane })}</MonoValue>
                    ) : (
                      <Value>{`${laneName(row.lane)} · ${row.lane}`}</Value>
                    )}
                  </Field>
                )}
                <TextControl
                  testId="providers-edit-modelPrefix"
                  label={t("providers.field.modelPrefix")}
                  value={draft.modelPrefix}
                  disabled={!editable}
                  onChange={(modelPrefix) => patch({ modelPrefix })}
                />
              </Box>

              {/* 健康列：**只读遥测**，不是配置（[07 §3.2]） */}
              <Stack spacing={2} data-testid="providers-health">
                <Typography variant="overline" sx={{ color: "text.secondary" }}>
                  {t("providers.edit.health")}
                </Typography>
                <Box sx={GRID_SX}>
                  <Field label={t("providers.field.status")}>
                    <StatusDot
                      status={row.status === undefined ? "off" : (STATUS_SEM[row.status] ?? "off")}
                      label={
                        row.status === undefined
                          ? t("common.unknown")
                          : (labelOf(STATUS_LABEL, t, row.status) ?? row.status)
                      }
                    />
                  </Field>
                  <Field label={t("providers.field.successRate")}>
                    <Value>
                      {row.successRate24h === undefined ? DASH : formatPercent(row.successRate24h, 1)}
                    </Value>
                  </Field>
                  <Field label={t("providers.field.latency")}>
                    <Value>{row.avgLatencyMs === undefined ? DASH : formatLatency(row.avgLatencyMs)}</Value>
                  </Field>
                  <Field label={t("providers.field.channelCount")}>
                    <Value>{row.channelCount === undefined ? DASH : formatInt(row.channelCount)}</Value>
                  </Field>
                </Box>
                {/* 线路名读不到是**后端原文 + TA 码**→ 留在版面上 */}
                {laneError !== undefined && laneError !== null ? (
                  <Typography variant="caption" data-testid="providers-lane-error" sx={{ color: "error.main" }}>
                    {t("providers.lane.error", { message: errorMessage(laneError) })}
                  </Typography>
                ) : null}
              </Stack>
            </Stack>
          </Paper>

          {/* ② 鉴权与连接：只有完整 `Provider` 才有这些字段 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-connection">
            <Stack spacing={4}>
              {/* 两句区块说明合成**一条**提示（：一个区块最多一个问号）。
                  第 2 轮补丁：**破坏性后果留在版面**（下面那行 `providers-connection-protect`），
                  原因与超时结论进这个 `?` —— 所以 `providers.connection.hint` **不再**在提示里。 */}
              <SectionTitle
                title={t("providers.section.connection")}
                helpText={[t("providers.connection.partial"), t("providers.tls.extraCaHint")].join("\n")}
                helpTestId="providers-connection-help"
              />

              {/* 破坏性后果（补丁纪律）：这一块**不会**拿默认值去覆盖线上已有的值。
                  只在**可编辑**时画：只读时上面那条 `providers-view-only` 说的就是同一件事 ——
                  同一屏同一个事实说两遍是纯噪音，所以两边各说一次不重复。 */}
              {editable ? (
                <Caption testId="providers-connection-protect">
                  {t("providers.connection.hint")}
                </Caption>
              ) : null}

              {!row.hasConnectionConfig ? (
                /* 连接配置没读到 → 版面上只留**结论**（一句短话），不排一排空占位。 */
                <Typography variant="body1" data-testid="providers-connection-unavailable">
                  {t("providers.connection.unavailable")}
                </Typography>
              ) : (
                <>
                  <Box sx={GRID_SX}>
                    <SelectField
                      testId="providers-edit-authScheme"
                      label={t("providers.field.authScheme")}
                      value={draft.authScheme}
                      onChange={(authScheme) => patch({ authScheme })}
                      options={authOptions}
                      emptyLabel={t(REQUIRED_LABEL_KEY)}
                    />
                    <TextControl
                      testId="providers-edit-authHeader"
                      label={t("providers.field.authHeader")}
                      value={draft.authHeader}
                      onChange={(authHeader) => patch({ authHeader })}
                    />
                    <TextControl
                      testId="providers-edit-authPrefix"
                      label={t("providers.field.authPrefix")}
                      value={draft.authPrefix}
                      onChange={(authPrefix) => patch({ authPrefix })}
                    />
                    <TextControl
                      testId="providers-edit-timeoutConnect"
                      label={t("providers.field.timeoutConnect")}
                      value={draft.timeoutConnectS}
                      inputMode="numeric"
                      onChange={(timeoutConnectS) => patch({ timeoutConnectS })}
                    />
                    <TextControl
                      testId="providers-edit-timeoutRead"
                      label={t("providers.field.timeoutRead")}
                      value={draft.timeoutReadS}
                      inputMode="numeric"
                      onChange={(timeoutReadS) => patch({ timeoutReadS })}
                    />
                    <TextControl
                      testId="providers-edit-timeoutTotal"
                      label={t("providers.field.timeoutTotal")}
                      value={draft.timeoutTotalS}
                      inputMode="numeric"
                      onChange={(timeoutTotalS) => patch({ timeoutTotalS })}
                    />
                    <Field label={t("providers.field.tlsInsecure")}>
                      <ProviderSwitch
                        testId="providers-edit-tlsInsecure"
                        label={draft.tlsInsecure ? t("common.yes") : t("common.no")}
                        checked={draft.tlsInsecure}
                        onChange={(tlsInsecure) => patch({ tlsInsecure })}
                      />
                    </Field>
                    <TextControl
                      testId="providers-edit-tlsMinVersion"
                      label={t("providers.field.tlsMinVersion")}
                      value={draft.tlsMinVersion}
                      onChange={(tlsMinVersion) => patch({ tlsMinVersion })}
                    />
                  </Box>

                  {/* 额外 CA：**内容可改**（它本来就是用户的材料，下面「原始字段」也原样打印它） */}
                  <Field label={t("providers.field.tlsExtraCa")}>
                    <TextControl
                      testId="providers-edit-extraCa"
                      label={t("providers.field.tlsExtraCa")}
                      value={draft.tlsExtraCa}
                      multiline
                      onChange={(tlsExtraCa) => patch({ tlsExtraCa })}
                    />
                  </Field>
                </>
              )}
            </Stack>
          </Paper>

          {/* ③ 附加头与透传 */}
          {row.hasConnectionConfig ? (
            <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-headers">
              <Stack spacing={4}>
                <SectionTitle
                  title={t("providers.section.headers")}
                  helpText={t("providers.headers.hint")}
                  helpTestId="providers-headers-help"
                />

                {/* 头表：一行一个名 / 值对，可加可删（不是一段 JSON 文本） */}
                <Stack spacing={2} data-testid="providers-headers-editor">
                  {draft.headers.map((header: HeaderPair, index: number) => (
                    <Stack
                      /*
                        行 key 用**下标**，不用头名：头名是可编辑的，用它当 key 的话每敲一个字
                       React 就会卸载重建这一行 —— 输入框会丢焦点，这是真的会碰到的缺陷。
                       */
                      key={index}
                      direction="row"
                      spacing={2}
                      alignItems="center"
                      sx={{ flexWrap: "wrap" }}
                      data-testid="providers-header-row"
                    >
                      <Box sx={{ minWidth: 220, flex: 1 }}>
                        <TextControl
                          testId="providers-header-name"
                          label={t("providers.headers.col.name")}
                          value={header.name}
                          onChange={(name) => patch({ headers: updateHeader(draft.headers, index, { name }) })}
                        />
                      </Box>
                      <Box sx={{ minWidth: 220, flex: 1 }}>
                        <TextControl
                          testId="providers-header-value"
                          label={t("providers.headers.col.value")}
                          value={header.value}
                          onChange={(value) => patch({ headers: updateHeader(draft.headers, index, { value }) })}
                        />
                      </Box>
                      <IconButton
                        size="small"
                        data-testid="providers-header-remove"
                        aria-label={t("providers.edit.headerRemove")}
                        onClick={() => patch({ headers: removeHeader(draft.headers, index) })}
                      >
                        <DeleteOutlineIcon fontSize="small" />
                      </IconButton>
                    </Stack>
                  ))}
                  {draft.headers.length === 0 ? (
                    <Typography variant="body1" data-testid="providers-headers-empty" sx={{ color: "text.secondary" }}>
                      {t("providers.headers.none")}
                    </Typography>
                  ) : null}
                  <Box>
                    <Button
                      variant="outlined"
                      startIcon={<AddIcon />}
                      data-testid="providers-header-add"
                      onClick={() => patch({ headers: appendHeader(draft.headers) })}
                    >
                      {t("providers.edit.headerAdd")}
                    </Button>
                  </Box>
                </Stack>

                <Box sx={GRID_SX}>
                  <SelectField
                    testId="providers-edit-headerConflict"
                    label={t("providers.field.headerConflict")}
                    value={draft.onHeaderConflict}
                    onChange={(onHeaderConflict) => patch({ onHeaderConflict })}
                    options={conflictOptions}
                    emptyLabel={t(REQUIRED_LABEL_KEY)}
                  />
                  <SelectField
                    testId="providers-edit-passthroughHeader"
                    label={t("providers.field.passthroughHeader")}
                    value={draft.passthroughHeaderMode}
                    onChange={(passthroughHeaderMode) => patch({ passthroughHeaderMode })}
                    options={passthroughHeaderOptions}
                    emptyLabel={t("providers.edit.passthroughUnset")}
                  />
                  <SelectField
                    testId="providers-edit-passthroughBody"
                    label={t("providers.field.passthroughBody")}
                    value={draft.passthroughBody}
                    onChange={(passthroughBody) => patch({ passthroughBody })}
                    options={passthroughBodyOptions}
                    emptyLabel={t("providers.edit.passthroughUnset")}
                  />
                </Box>
              </Stack>
            </Paper>
          ) : null}

          {/* ④ 密钥（基础数据的最后一块）：**一个服务商一把密钥**（硬规则 3）。
              留空 = 不改；填了 = 写到这家那条凭据（`credential_set_secret`，明文永不回显）。
              额度窗口挂在**这一条**凭据上（不再有凭据表）。

              标题右边**只放一个问号**：两条说明都是"怎么用"的长句 ——
              · `providers.credentials.secretHint`：密钥以引用形式显示、明文只进密钥仓库；
              · `providers.key.multipleHint`：**要两个账号就再加一条服务商、取不同名字**
                （一个服务商就是一把密钥 —— owner 原话， 硬规则 3）。
              「一个服务商一把密钥」这个**结论**本身也留在版面上（下面的空态 / 老配置提示里都说），
              不靠悬停才看得见。 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-key">
            <Stack spacing={4}>
              <SectionTitle
                title={t(K.keyTitle)}
                helpText={[t("providers.credentials.secretHint"), t("providers.key.multipleHint")].join(
                  "\n",
                )}
                helpTestId="providers-key-help"
              />

              {/* 写密钥：**留空 = 不改**；只写进密钥仓库，永不回显、永不进载荷 */}
              {editable ? (
                <Stack spacing={1} sx={{ maxWidth: 480 }}>
                  <TextControl
                    testId="providers-edit-secret"
                    label={t("onboarding.provider.secret")}
                    value={draft.key}
                    type="password"
                    autoComplete="new-password"
                    multiline={false}
                    onChange={(key) => patch({ key })}
                  />
                  {/**
                   * 极简：**已经存在的凭据不再重复报一遍** —— 它就在下面那张凭据表里
                   * （同一屏里同一个事实说两遍是纯噪音）。
                   *
                   * 留下的两种都是"用户不看会误判"的话：
                   * * 说不清写哪儿（读不到凭据）→ 红线，必须说；
                   * * 这次会**新建**一份凭据 → 那是新闻，必须说。
                   */}
                  {secretTarget === undefined ? (
                    <Caption>{t("providers.edit.keyHint")}</Caption>
                  ) : secretTarget.exists ? null : (
                    <Caption>{t("providers.edit.keyNewCredential", { id: secretTarget.id })}</Caption>
                  )}
                </Stack>
              ) : null}

              {credentialsError !== undefined && credentialsError !== null ? (
                <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                  <Typography variant="body1" data-testid="providers-key-error" sx={{ color: "error.main" }}>
                    {t("providers.credentials.error", { message: errorMessage(credentialsError) })}
                  </Typography>
                  <Button variant="outlined" onClick={onRetryCredentials}>
                    {t("common.retry")}
                  </Button>
                </Stack>
              ) : credentials === undefined ? (
                <Typography variant="body1" sx={{ color: "text.secondary" }}>
                  {t("common.loading")}
                </Typography>
              ) : keyCredential === undefined ? (
                /* 空态（规则 5）：**1 行**。原来下面那半句灰字（"挂载关系写在凭据的 provider
                   字段上……"）是解释实现，整条删掉；"怎么开始"就写在紧上面那个输入框里
                   （填一把密钥保存即可创建）。 */
                <Typography variant="body1" data-testid="providers-key-empty">
                  {t("providers.credentials.empty")}
                </Typography>
              ) : (
                <>
                  {/* 旧数据里挂了好几份：如实说出来（一份服务商只该有一把密钥） */}
                  {own !== undefined && own.length > 1 ? (
                    <Typography variant="body1" data-testid="providers-key-multiple" sx={{ color: "warning.main" }}>
                      {t(K.keyMultiple, { count: own.length, id: keyCredential.id })}
                    </Typography>
                  ) : null}

                  <Box sx={GRID_SX}>
                    <Field label={t("providers.credentials.col.label")}>
                      <Value testId="providers-key-label">
                        {keyCredential.label === "" ? DASH : keyCredential.label}
                      </Value>
                      {/* 密钥**引用**原样显示（它是这一行的状态，不是解释） */}
                      {keyCredential.secret === undefined ? null : (
                        <Typography
                          variant="caption"
                          data-testid="providers-key-secret"
                          sx={{
                            color: "text.secondary",
                            fontFamily: typography.fontMono,
                            display: "block",
                            overflowWrap: "anywhere",
                          }}
                        >
                          {keyCredential.secret}
                        </Typography>
                      )}
                      {keyCredential.plaintextSecret === true ? (
                        <StatusDot status="warn" label={t("providers.credentials.plaintext")} />
                      ) : null}
                    </Field>

                    <Field label={t("providers.credentials.col.enabled")}>
                      <Value>
                        {keyCredential.enabled === undefined
                          ? t("common.unknown")
                          : keyCredential.enabled
                            ? t("common.enabled")
                            : t("common.disabled")}
                      </Value>
                    </Field>

                    <Field label={t("providers.credentials.col.lane")}>
                      <Value>
                        {keyCredential.lane === undefined
                          ? DASH
                          : (laneName(keyCredential.lane) ?? keyCredential.lane)}
                      </Value>
                    </Field>
                  </Box>
                </>
              )}
            </Stack>
          </Paper>

          {/* ⑤ 线路（：这一页的**主体**）——这家服务商自己的线路，一行一条 */}
          {lines === undefined ? null : <LinesSection {...lines} />}

          {/* ⑥ 原始字段：后端返回的整条记录，原样显示（未做任何清洗）。
              这是**证据**，不是编辑界面 —— 编辑用上面的可视化控件。 */}
          <Paper variant="outlined" sx={SECTION_SX} data-testid="providers-raw-section">
            <Stack spacing={2}>
              <SectionTitle
                title={t("providers.section.raw")}
                helpText={t("providers.section.rawHint")}
                helpTestId="providers-raw-section-help"
              />
              <Box
                component="pre"
                data-testid="providers-raw"
                sx={{
                  m: 0,
                  p: 3,
                  bgcolor: "background.surface",
                  borderRadius: `${radius.control}px`,
                  border: "1px solid",
                  borderColor: "divider",
                  fontFamily: typography.fontMono,
                  fontSize: typography.size.meta,
                  lineHeight: typography.lineHeight.table,
                  color: "text.secondary",
                  whiteSpace: "pre-wrap",
                  overflowWrap: "anywhere",
                  maxHeight: 320,
                  overflow: "auto",
                }}
              >
                {JSON.stringify(row.raw, null, 2)}
              </Box>
            </Stack>
          </Paper>

          {/* 保存条：改了什么、能不能存，一眼可见。**吸在视口底部**——这一页很长，
              把"保存"埋在末尾等于让用户滚到底才想起来自己改过什么。 */}
          {editable ? (
            <Paper
              variant="outlined"
              sx={{ ...SECTION_SX, position: "sticky", bottom: 0, zIndex: 1 }}
              data-testid="providers-save-bar"
            >
              <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                <Button
                  variant="contained"
                  data-testid="providers-detail-save"
                  disabled={!dirty || saving}
                  onClick={submit}
                >
                  {t("common.save")}
                </Button>
                <Button variant="outlined" data-testid="providers-detail-reset" disabled={!dirty} onClick={reset}>
                  {t("providers.edit.reset")}
                </Button>
                <Caption>{dirty ? t("providers.edit.dirty") : t("providers.edit.clean")}</Caption>
                {saving ? <Caption>{t("common.loading")}</Caption> : null}
              </Stack>
            </Paper>
          ) : null}
        </>
      )}
    </Box>
  );
}

// ================================================================ 数据层

/** 空行（`row` 还没到时种一份空草稿：`useState` 的初值不能是 `undefined`）。 */
const EMPTY_ROW: ProviderRow = {
  id: "",
  name: "",
  headers: [],
  tlsExtraCa: false,
  hasConnectionConfig: false,
  raw: {},
};

export default function ProviderDetailPage() {
  const { t } = useTranslation();
  const params = useParams<{ id: string }>();
  const id = params.id ?? "";
  const navigate = useNavigate();
  const qc = useQueryClient();

  // 与列表页同一个 key：从列表点进来时是缓存命中，不需要再打一次后端
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  /** 这一家的线路：`channel_list` 全量拉回来，再按 `provider` 过滤。 */
  const channels = useCommandQuery("channel_list", undefined, qk.channels);

  const upsertProvider = useCommandMutation("provider_upsert");
  const upsertCredential = useCommandMutation("credential_upsert");
  const setSecret = useCommandMutation("credential_set_secret");

  const row = useMemo(
    () =>
      (providers.data ?? [])
        .map((p) => readProvider(p))
        .find((candidate) => candidate.id === id),
    [providers.data, id],
  );
  const credentialRows = useMemo(
    () => (credentials.data === undefined ? undefined : credentials.data.map((c) => readCredential(c))),
    [credentials.data],
  );
  /**
   * 「自动」出口的候选：**启用的订阅**（一份一个）。页面查好、传进视图 ——
   * 视图组件不自己查数据（它要能在没有 QueryClient 的地方单测）。
   */
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const autoSubscriptionChoices = useMemo(
    () =>
      (subscriptions.data ?? [])
        .map((item) => {
          // `subscription_list` 可能是**视图包裹**（`{ subscription: {...} }`）也可能是裸实体：
          // 两种都要认（网络页那边用 `readSubscription` 做同一件事）。
          const outer = item as {
            subscription?: unknown;
            id?: unknown;
            name?: unknown;
            enabled?: unknown;
          };
          const row = (outer?.subscription ?? outer) as {
            id?: unknown;
            name?: unknown;
            enabled?: unknown;
          };
          return {
            id: String(row?.id ?? ""),
            name: String(row?.name ?? ""),
            enabled: row?.enabled,
          };
        })
        // 关掉的订阅不列：用户关掉它就是让我们别碰它
        .filter((s) => s.id !== "" && s.enabled !== false)
        .map((s) => ({ id: s.id, name: s.name })),
    [subscriptions.data],
  );
  const laneOptions = useMemo(() => (lanes.data ?? []).map((l) => readLane(l)), [lanes.data]);

  /**
   * **网络代理下拉的选项来源**：`lane_options`（含订阅节点），不是 `lane_list`。
   *
   * 用**独立的 key**（`qk.lanes` 加一段后缀）而不是复用 `qk.lanes`：两者是不同的查询
   * （一个是"已配置的线路"，一个是"能选的全部出口"），共用 key 会让 TanStack Query
   * 把它们当成同一份数据互相覆盖。
   */
  const laneChoices = useCommandQuery("lane_options", undefined, [...qk.lanes, "options"]);
  const laneChoiceGroups = useMemo(() => laneChoices.data ?? [], [laneChoices.data]);
  /** 给 `save` 用：把选中的值解析成真实 lane id 时需要那份选项（节点项带 `nodeRef`）。 */
  const laneChoiceList = useMemo(
    () =>
      providerLaneChoices(
        laneChoiceGroups,
        t("providers.edit.laneInherit"),
        autoSubscriptionChoices,
        t("providers.edit.laneAuto"),
      ),
    [laneChoiceGroups, autoSubscriptionChoices, t],
  );
  const ensureNodeLane = useCommandMutation("lane_ensure_for_node");

  // ---------------------------------------------------------------- 线路（：创建线路 + 逐条编辑）
  //
  // 这一页**不创建模型**（模型的创建/分配在 `/models`）。原来那一节「模型目录」只保留
  // 「扫描上游模型 → 勾一下」这个能力，改叫**创建线路**（契约暂时不变：仍调
  // `provider_model_attach`， S3）。线路本身来自 `channel_list`：
  //   * 改网络代理 → `channel_upsert`（**整条覆盖写**，载荷以原始记录为底）
  //   * 启用/停用   → `channel_set_enabled`（可逆）
  //   * 移除       → `channel_delete`（走确认框；被策略点名引用时后端拒绝并给原文）

  const lines = useMemo(() => linesOfProvider(channels.data, id), [channels.data, id]);

  const catalogQuery = useCommandQuery(
    "provider_model_catalog",
    { provider_id: id },
    qk.providerModelCatalog(id),
  );
  const upsertChannel = useCommandMutation("channel_upsert");
  const setChannelEnabled = useCommandMutation("channel_set_enabled");
  /** 全选：批量启停（一次写库、一次内核重载）。 */
  const bulkSetEnabled = useCommandMutation("channel_bulk_set_enabled");
  const removeChannel = useCommandMutation("channel_delete");
  const [scanPending, setScanPending] = useState(false);
  const [catalogError, setCatalogError] = useState<unknown>(undefined);
  const [catalogNotice, setCatalogNotice] = useState<string | undefined>(undefined);
  /** 线路写命令（改代理 / 启停）的后端原文；与目录的错分开，各自显示在它发生的区块里。 */
  const [lineError, setLineError] = useState<unknown>(undefined);
  /** 线路写命令成功后的事实（"网络代理已改为 X"）—— 只讲刚发生的事。 */
  const [lineNotice, setLineNotice] = useState<string | undefined>(undefined);

  /** 线路变了要重取的：线路清单自己、通道矩阵、模型清单（删线路会摘 `Model.lines`）、目录。 */
  const refreshLines = async (): Promise<void> => {
    await invalidate(qc, [qk.channels, qk.channelMatrix, qk.models, qk.providerModelCatalog(id)]);
  };

  /** 启用 / 停用一条线路（可逆：下一次还能勾回来）。 */
  const toggleLineEnabled = (line: LineRow, enabled: boolean): void => {
    setLineError(undefined);
    setLineNotice(undefined);
    void (async () => {
      try {
        await setChannelEnabled.mutateAsync({ id: line.id, enabled });
        await refreshLines();
      } catch (err) {
        setLineError(err);
      }
    })();
  };

  /**
   * **全选**：一次动作把列表里的线路一起启用 / 一起关闭（`channel_bulk_set_enabled`）。
   *
   * 为什么必须是一条命令而不是前端循环：全选是**一次用户动作**；循环发 N 条
   * `channel_set_enabled` 会让"改了 200 条"变成 200 个配置快照 + 200 次内核重载，
   * 中途失败还会留下半开半关的状态。
   */
  const bulkEnabled = (enabled: boolean): void => {
    setLineError(undefined);
    setLineNotice(undefined);
    const ids = (lines ?? []).map((l) => l.id).filter((id) => id !== "");
    if (ids.length === 0) return;
    void (async () => {
      try {
        await bulkSetEnabled.mutateAsync({ channel_ids: ids, enabled });
        setLineNotice(
          enabled
            ? t(K.linesAllEnabled, { count: ids.length })
            : t(K.linesAllDisabled, { count: ids.length }),
        );
        await refreshLines();
      } catch (err) {
        setLineError(err);
      }
    })();
  };

  /**
   * 「重新扫描」：扫上游 + 刷新目录 + **把缺的线路补建出来**（用户第 82 轮）。
   *
   * 用户原话：「添加服务商默认添加所有线路，然后通过滑块来'启用'和'关闭'……只是有个滑块
   * 控制是否可见」。所以扫描这一步就把清单里**每条**线路建出来（**默认关闭**），
   * 界面上不再有"勾一下才创建"；之后只用滑块控制启用 / 关闭。
   *
   * 新线路的默认代理：用这家服务商自己那条 `lane`（服务商级默认）；没配就是继承。
   */
  const rescanCatalog = (): void => {
    setCatalogError(undefined);
    setCatalogNotice(undefined);
    setScanPending(true);
    void (async () => {
      try {
        const preview = await api.model.importFromProvider(id);
        const recorded = preview.models.length + preview.skipped.length;
        /**
         * 扫描**只记录**目录；"建出全部线路"是紧跟着的第二条命令。
         * 两步分开是有意的：目录是"上游给了什么"（事实），建线路是"我们拿它做什么"（决定）
         * —— 把它们糊成一条命令，就没法解释"扫到了但一条都没建出来"这种状态。
         */
        const created = await api.model.createAllLines(id, row?.lane ?? null);
        setCatalogNotice(
          t(K.linesCreatedAll, linesCreatedNoticeVars(recorded, created.created, created.total)),
        );
        await Promise.all([catalogQuery.refetch(), refreshLines()]);
      } catch (err) {
        setCatalogError(err);
      } finally {
        setScanPending(false);
      }
    })();
  };

  // ---------------------------------------------------------------- 保存

  /**
   * 保存这一行。**顺序**：
   * 1. `provider_upsert` 覆盖写服务商自己（载荷以原始记录为底，见 `./editor.ts`）；
   * 2. **只有用户真的填了新密钥**才写密钥仓库：写的是这家**已有的那唯一一把**凭据
   *    （`<provider>-default` 优先，否则这家挂的第一份）；一把都没有时先建一份再写。
   * 3. 失效服务商与凭据两个查询（线路不动：它读的是 `channel_list`）。
   */
  const save = async (payload: ProviderInput, plaintextKey: string): Promise<void> => {
    /**
     * **先把选中的出口解析成真实 lane id**，再写服务商。
     *
     * `auto:` / `direct:` 两种标记由后端 `provider_upsert` 自己解析（"这条线路该长什么样"
     * 的判断只有一份）；**订阅节点**不行 —— 值是 `node:<订阅>:<节点名>`，后端不认识这个标记，
     * 直接发出去会被当成一个不存在的 lane id 拒掉。所以这里先 `lane_ensure_for_node`
     * 把它换成真实 id（已有等价线路就复用）。
     */
    const lane = await resolveLaneChoice(payload.lane, laneChoiceList, (subscriptionId, node) =>
      ensureNodeLane.mutateAsync({ subscription_id: subscriptionId, node }),
    );
    const saved = await upsertProvider.mutateAsync({ provider: { ...payload, lane } });
    const landedId = typeof saved?.id === "string" && saved.id !== "" ? saved.id : id;
    if (plaintextKey !== "") {
      const target = keyCredentialOf(credentialRows, landedId);
      if (target === undefined) {
        await upsertCredential.mutateAsync({
          credential: defaultCredentialPayload(landedId, typeof saved?.name === "string" ? saved.name : ""),
        });
        await setSecret.mutateAsync({ id: defaultCredentialId(landedId), plaintext: plaintextKey });
      } else {
        await setSecret.mutateAsync({ id: target.id, plaintext: plaintextKey });
      }
    }
    await invalidate(qc, [qk.providers, qk.credentials]);
  };

  /** 填了新密钥时要落到哪份凭据（只为界面如实说明；真正写入在 [`save`] 里）。 */
  const secretTarget = useMemo(() => {
    const chosen = keyCredentialOf(credentialRows, id);
    return chosen === undefined
      ? { id: defaultCredentialId(id), exists: false }
      : { id: chosen.id, exists: true };
  }, [credentialRows, id]);

  // ---------------------------------------------------------------- 删除
  //
  // 本页有**两条**删除流程（页头删服务商、线路行移除线路），语义完全一样：
  // 先确认 → `force=false` → 被引用时后端原文原样显示（服务商此时才给级联按钮）→ 成功写结果区。
  // 凭据的删除随「一个服务商一把密钥」下线：要撤掉一把密钥就删掉这个服务商
  // （硬规则 3：多账号 = 加多个服务商）。

  const removeProvider = useCommandMutation("provider_delete");

  // ---------------------------------------------------------------- 额度窗口
  //
  // 窗口是**用户自己配**的：这里只负责"打开哪一份 / 保存成什么载荷 / 保存后失效哪些查询"。
  // 载荷以**整条 credential 的原始记录**为底（`credential_upsert` 是整条覆盖写，
  // 少发一个键就会把 secret / guard / tags 抹掉 —— 与 同一条纪律）。
  // 挂的是这家**那唯一一条**凭据（硬规则 3）。

  /** 一条删除流程的状态（本页两条：服务商 / 线路）。 */
  type DeleteFlowState = {
    target: DeleteTarget | undefined;
    /** 后端原文（被引用时的错误）；有它对话框才显示、服务商那条也才给级联按钮。 */
    error: string | undefined;
    /** 结果区：每一次事实一行。 */
    result: readonly string[];
  };
  const EMPTY_FLOW: DeleteFlowState = { target: undefined, error: undefined, result: [] };
  /** `useState` 的 setter 类型（`openFlow` / `cancelFlow` / `runFlow` 三个执行体共用）。 */
  type SetFlow = Dispatch<SetStateAction<DeleteFlowState>>;
  const [providerFlow, setProviderFlow] = useState<DeleteFlowState>(EMPTY_FLOW);
  /**
   * 「移除线路」那条流程。
   *
   * 与删服务商共用同一套执行体（先确认 → 拿到后端原文）。**但 `channel_delete` 没有 `force`**：
   * 所以这条流程的 `cascadeAvailable` 恒为 false —— 不显示一个点了会报错的级联按钮。
   */
  const [lineFlow, setLineFlow] = useState<DeleteFlowState>(EMPTY_FLOW);

  const openFlow = (setFlow: SetFlow, target: DeleteTarget): void => {
    // 新一次操作开始 → 清掉上一次的汇报与错误（结果区永远只讲刚刚发生的事）
    setFlow({ target, error: undefined, result: [] });
  };

  const cancelFlow = (setFlow: SetFlow): void => {
    setFlow((prev) => ({ ...prev, target: undefined, error: undefined }));
  };

  /** 删服务商成功后要失效的查询：会连带动到凭据 / 线路 / 模型。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    await invalidate(qc, [qk.providers, qk.credentials, qk.models, qk.channels, qk.channelMatrix]);
  };

  /** 「会连带删除 / 影响」清单：只用本页读得到的查询算（凭据数按 `provider` 字段归并）。 */
  const providerConsequences = (): string[] => {
    if (credentialRows === undefined) return [];
    const lines = [
      t("delete.consequence.credentials", {
        value: formatInt(credentialsOf(credentialRows, id).length),
      }),
    ];
    // 通道数只有后端给了 `channel_count` 才算得出来（Rust 的 provider_list 没有这一列）
    if (row?.channelCount !== undefined) {
      lines.push(t("delete.consequence.channels", { value: formatInt(row.channelCount) }));
    }
    return lines;
  };

  /** 一次删除的公共执行体：跑完把**事实**写回对应的那份状态，并重取受影响的查询。 */
  const runFlow = async (
    setFlow: SetFlow,
    target: DeleteTarget,
    force: boolean,
    remove: (targetId: string, effectiveForce: boolean) => Promise<unknown>,
    nameOf: (targetId: string) => string,
    refresh: () => Promise<void>,
  ): Promise<void> => {
    const outcome = await runDelete(target, force, remove);
    const failed = outcome.failures.length > 0;
    setFlow((prev) => ({
      ...prev,
      error: failed ? outcome.failures[0]!.message : undefined,
      // 失败时**不许写成功**（结果区只写真的发生过的事）
      result: failed
        ? []
        : outcome.deletedIds.map((deletedId) => t("delete.result.ok", { name: nameOf(deletedId) })),
      // 失败时留着框（后端原文就在那里）；成功就收框
      target: failed ? target : undefined,
    }));
    await refresh();
  };

  const detailLabel = (name: string, entityId: string): string =>
    name === "" ? entityId : `${name}（${entityId}）`;

  /** 页头与服务商级汇报里用的名字（缺名称时退回 id，不编一个）。 */
  const providerName = (): string => (row === undefined ? id : row.name === "" ? row.id : row.name);

  return (
    <>
      <PageHeader
        title={row === undefined ? t("nav.providerDetail") : row.name === "" ? row.id : row.name}
        description={t("providers.detail.description")}
        actions={
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            {/*
              「删除服务商」：只在这一行真的存在时出现 ——
              目录里没有这个 id 时不放一个点了会错的按钮。点击**先弹确认框**，不发命令。
              保存按钮在页面底部的保存条上（：这一页本身就是编辑器）。
            */}
            {row === undefined ? null : (
              <Button
                variant="outlined"
                color="error"
                data-testid="providers-detail-delete"
                onClick={() =>
                  openFlow(setProviderFlow, {
                    ids: [row.id],
                    label: detailLabel(row.name, row.id),
                    consequences: providerConsequences(),
                  })
                }
              >
                {t("delete.title.provider")}
              </Button>
            )}
            <Button
              variant="text"
              onClick={() => {
                void providers.refetch();
                void credentials.refetch();
                void lanes.refetch();
                void channels.refetch();
                void catalogQuery.refetch();
              }}
            >
              {t("providers.refresh")}
            </Button>
          </Box>
        }
      />

      <ProviderDetailBody
        id={id}
        row={row}
        isLoading={providers.isPending}
        error={providers.error ?? undefined}
        onRetry={() => void providers.refetch()}
        credentials={credentialRows}
        credentialsError={credentials.error ?? undefined}
        autoSubscriptions={autoSubscriptionChoices}
        onRetryCredentials={() => void credentials.refetch()}
        laneChoiceGroups={laneChoiceGroups}
        laneName={(laneId) => laneNameOf(laneOptions, laneId)}
        laneError={lanes.error ?? undefined}
        onBack={() => navigate("/providers")}
        onSave={row === undefined ? undefined : save}
        secretTarget={secretTarget}
        lines={{
          lines,
          linesError: channels.error ?? undefined,
          catalog: catalogQuery.data,
          catalogError: catalogError ?? catalogQuery.error ?? undefined,
          scanPending,
          onScan: rescanCatalog,
          busy:
            upsertChannel.isPending ||
            setChannelEnabled.isPending ||
            bulkSetEnabled.isPending ||
            removeChannel.isPending,
          error: lineError,
          notice: catalogNotice,
          lineNotice,


          onToggleEnabled: toggleLineEnabled,
          onBulkEnabled: bulkEnabled,
          onRemove: (line) =>
            openFlow(setLineFlow, {
              ids: [line.id],
              label: detailLabel(line.upstreamModel, line.id),
              // 这条线路被哪些策略点名引用：本页读不到策略清单 → **算不出来就不写**（不编）
              consequences: [],
            }),
        }}
      />

      {/* 删除结果区（两条流程各一块）：只写真的发生过的事 */}
      {providerFlow.result.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="providers-delete-result">
              {providerFlow.result.map((line, index) => (
                <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}
      {lineFlow.result.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="providers-line-delete-result">
              {lineFlow.result.map((line, index) => (
                <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}

      {/*
        删除服务商：testid 前缀用 `providers`（与列表页同一个实体同一个前缀，
        §3 表里点名的级联按钮就是 `providers-delete-cascade`）。先确认 → `force=false` →
        被引用时后端原文原样显示、此时才出现级联按钮。
      */}
      <ConfirmDelete
        open={providerFlow.target !== undefined}
        testIdPrefix="providers"
        title={t("delete.title.provider")}
        targetLabel={providerFlow.target?.label ?? ""}
        consequences={providerFlow.target?.consequences}
        cascadeAvailable={providerFlow.target !== undefined}
        pending={removeProvider.isPending}
        errorText={providerFlow.error}
        onConfirm={() => {
          if (providerFlow.target !== undefined) {
            void runFlow(
              setProviderFlow,
              providerFlow.target,
              false,
              (targetId, effectiveForce) => removeProvider.mutateAsync({ id: targetId, force: effectiveForce }),
              () => providerName(),
              invalidateAfterDelete,
            );
          }
        }}
        onConfirmCascade={() => {
          if (providerFlow.target !== undefined) {
            void runFlow(
              setProviderFlow,
              providerFlow.target,
              true,
              (targetId, effectiveForce) => removeProvider.mutateAsync({ id: targetId, force: effectiveForce }),
              () => providerName(),
              invalidateAfterDelete,
            );
          }
        }}
        onCancel={() => cancelFlow(setProviderFlow)}
      />

      {/*
        「移除线路」：与删服务商同一套语义（先确认、被引用时原样显示后端原文），
        **但没有级联** —— `channel_delete` 不接受 `force`：它只会在被策略点名引用时拒绝，
        而不是"确认一下就连带删掉"。
      */}
      <ConfirmDelete
        open={lineFlow.target !== undefined}
        testIdPrefix="providers-line"
        title={t(K.linesRemove)}
        targetLabel={lineFlow.target?.label ?? ""}
        consequences={lineFlow.target?.consequences}
        cascadeAvailable={false}
        pending={removeChannel.isPending}
        errorText={lineFlow.error}
        onConfirm={() => {
          if (lineFlow.target !== undefined) {
            void runFlow(
              setLineFlow,
              lineFlow.target,
              false,
              (targetId) => removeChannel.mutateAsync({ id: targetId }),
              (targetId) => targetId,
              refreshLines,
            );
          }
        }}
        onCancel={() => cancelFlow(setLineFlow)}
      />
    </>
  );
}

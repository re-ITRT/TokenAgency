/**
 * `/providers` 的**两步向导**—— 点 `providers-add-open` 进来的就是这个。
 *
 * 为什么要有它：原来的「添加服务商」是单步表单（服务商 + 凭据 + 密钥一次填完），
 * 而用户要的流程是"下一步 → 下一步"：先把**这家服务商**配好（顺带测一下通不通），
 * 再处理**这家下面的线路**。后者需要先落库才能扫描（扫描要拿 provider 的地址与凭据），
 * 所以第 1 步结束时**真的写配置**，并且这件事必须在界面上说出来（`pw-saved-notice`）。
 *
 * 四条纪律（写在这里，免得下一个人再犯）：
 *
 * 1. **无假按钮 / 无假进度**：`pw-scan` 只有真的在等后端时才显示"正在扫描"；
 *    界面上出现的每个控件都对应一条已注册的命令。
 * 2. **明文只进密钥仓库**：`pw-key` 的值只作为 `credential_set_secret` 的入参发出，
 *    落库成功后立刻从草稿里抹掉；它不进 `provider_upsert` / `credential_upsert` 的载荷、
 *    不进错误消息、不上屏。
 * 3. **`data-testid` 挂在容器上**（MUI 的 TextField / Select 不吃它，直接写会 TS2353），
 *    真机 E2E 与单测都按 `[data-testid="X"] input` 命中同一形状。
 * 4. **一个服务商一把密钥**（硬规则 3）：第 1 步只有一个 `pw-key` 输入框，
 *    一次运行只发出**一条** `credential_upsert`（id 恒为 `<provider>-default`）与至多一条
 *    `credential_set_secret`；要两个账号 → 再加一条服务商、取不同名字（提示在
 *    `pw-key-multiple-help`）。
 *
 * ## 第 2 步 = **一个列表**（用户第 82 轮的重构）
 *
 * 用户原话：「添加服务商默认添加所有线路，然后通过滑块来'启用'和'关闭'……因此，可以直接写在
 * 在一个列表里面，没有添加的线路也可以设置'网络代理'，只是有个滑块控制是否可见」。
 * 所以第 2 步就是**详情页那一节**：
 *
 * * 扫描（`model_import_from_provider`）→ 紧接着 `provider_lines_create_all`
 *   把上游清单里的**每条**线路都建出来（**默认关闭**）；
 * * 列表里每行 = 上游原文 + 网络代理 + 启用/关闭的滑块；
 * * 头部有**全选**（`channel_bulk_set_enabled`：一条命令、一次写库、一次内核重载）；
 * * 界面上**不再有**"勾一下才创建"。
 *
 * 复用 [`LinesSection`] 而不是在这里再画一遍：两处一旦各画一套，行为迟早分叉
 * （这一轮要修的正是"向导和详情页操作逻辑不一样"）。
 *
 * **这里不创建模型**（硬规则 1，owner 原话"模型是不自动创建的，是等待用户去分配的"）：
 * 建的是**线路**，模型由用户在模型页把线路分配给它时才出现。
 */

import {
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { api, errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { ImportPreview, ProbeResult } from "@/shared/bindings";
import { Caption, HelpTip } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";

import { LinesSection, linesOfProvider, type LineRow } from "./detail";
import { providerLaneChoices } from "./editor";
import { PROTOCOLS } from "./protocols";
import {
  defaultCredentialId,
  defaultCredentialPayload,
  deriveProviderId,
  linesCreatedNoticeVars,
  nameTakenIssue,
  probeFailureText,
  probeOutcome,
  readWizardSession,
  resetWizardSession,
  step1ProviderPayload,
  validateStep1,
  writeWizardSession,
  type ExistingProviders,
  type SavedInSession,
  type Step1Draft,
  type WizardSession,
} from "./providerWizardModel";
import type { WizardIssue } from "./wizard";
import { FIELD_LABEL, SelectField } from "./wizards";

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 协议 → 界面语言（与列表页同一张表：线上值不漏给用户）。 */
const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

/**
 * 第 2 步用的文案键。
 *
 * `I18nKey`（= `keyof typeof zhCN`）是编译期检查的：键没落地这里就编译不过 —— 所以这些键
 * 必须先存在于 `src/shared/i18n/{zh-CN,en}.ts`。它们都归 i18n 文件所有，页面只引用。
 */
import { i18nKey } from "@/shared/i18n";

const K = {
  step2Title: i18nKey("providers.wizard.step2.title"),
  step2Why: i18nKey("providers.wizard.step2.why"),
  gotoModels: i18nKey("providers.wizard.gotoModels"),
  linesCreatedAll: i18nKey("providers.lines.createdAll"),
  scanSkipped: i18nKey("providers.wizard.scanSkipped"),
  scanEmpty: i18nKey("providers.wizard.scanEmpty"),
  scanNeedsProvider: i18nKey("providers.wizard.scanNeedsProvider"),
  allEnabled: i18nKey("providers.lines.allEnabled"),
  allDisabled: i18nKey("providers.lines.allDisabled"),
} as const;

/*
 * 本轮删掉的三个**死声明**（键与声明一起下线，见映射文件的 delete 列表）：
 * · `providers.wizard.defaultLane` / `.defaultLaneHint` —— 第 1 步已经没有"新建线路默认代理"
 *   这个控件（：代理只属于服务商），这两条 88 字的说明从来没画到版面上，
 *   留着只会让下一个读代码的人以为界面上还有那一项；
 * · `providers.lines.laneChanged` —— 键在 zh-CN 里本来就不存在（线路级代理撤掉时漏删的声明）。
 */

const SCROLL_SX = { overflowY: "auto", maxHeight: "64vh" } as const;

// ================================================================ 小组件


/** 本地预检 / 后端的错：逐条显示（本地预检点名字段，后端原文带 TA 码）。 */
function Issues({ issues, backendError }: { issues: readonly WizardIssue[]; backendError?: unknown }) {
  const { t } = useTranslation();
  return (
    <Stack spacing={1} data-testid="pw-step1-issues">
      {issues.map((issue) => (
        <Typography key={`${issue.field}-${issue.message}`} variant="body1" sx={{ color: "error.main" }}>
          {`${t(FIELD_LABEL[issue.field] ?? "common.unknown")}：${t(issue.message)}`}
        </Typography>
      ))}
      {backendError !== undefined && backendError !== null ? (
        <Typography variant="body1" data-testid="pw-save-error" sx={{ color: "error.main" }}>
          {errorMessage(backendError)}
        </Typography>
      ) : null}
    </Stack>
  );
}

// ================================================================ 向导

export type ProviderTwoStepWizardProps = {
  open: boolean;
  /** 现有服务商（`provider_list`）：显示名称**去重**要对着后端给的真值做。 */
  existing: ExistingProviders;
  onClose: () => void;
};

/**
 * 两步向导—— **只管"新增服务商"**。
 *
 * 第 72 轮曾让详情页的「编辑」以编辑模式打开同一个向导；用户第 75 轮明确要求
 * 「编辑页面不走下一步下一步，点开服务商之后直接给可视化编辑界面」→ 编辑已经搬到详情页
 * 本身，这里那套编辑分支连同"只发 4 个字段的覆盖写"一起删掉了
 * （它正是"改个名字会抹掉鉴权 / 超时 / TLS / 附加头"那个数据丢失缺陷的载体）。
 */
export function ProviderTwoStepWizard({ open, existing, onClose }: ProviderTwoStepWizardProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  /**
   * 会话（草稿 / 当前步 / 已落库的那一份）活在**模块级 store** 里：点 `pw-proxy`
   * 去 `/network` 会把整个页面卸载掉，草稿必须比组件活得久。
   */
  const [session, setSession] = useState<WizardSession>(() => readWizardSession());
  useEffect(() => {
    // 每次打开都从 store 取一次：关掉时 store 已被清空（不带上一次的残值），
    // 而从 `/network` 回来时 store 里还留着用户填的东西。
    if (open) setSession(readWizardSession());
  }, [open]);

  const patchSession = (patch: (prev: WizardSession) => Partial<WizardSession>): void => {
    setSession((prev) => {
      const next = { ...prev, ...patch(prev) };
      writeWizardSession(next);
      return next;
    });
  };

  const draft = session.draft;
  const step = session.step;
  const saved = session.saved;

  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<unknown>(undefined);

  const [probePending, setProbePending] = useState(false);
  const [probeResult, setProbeResult] = useState<ProbeResult | undefined>(undefined);
  const [probeError, setProbeError] = useState<unknown>(undefined);

  /** 扫描（含"把清单里的线路都建出来"）的状态：在飞 / 结果 / 错误。 */
  const [scanPending, setScanPending] = useState(false);
  const [preview, setPreview] = useState<ImportPreview | undefined>(undefined);
  const [scanError, setScanError] = useState<unknown>(undefined);
  const [needsProvider, setNeedsProvider] = useState(false);
  /** 后端返回的"本次真新建了几条线路 / 目录里共几条"，如实显示。 */
  const [linesCreated, setLinesCreated] = useState<{ created: number; total: number } | undefined>(
    undefined,
  );
  /** 线路行上的写命令（改代理 / 启停 / 全选）的错误与刚发生的事实。 */
  const [lineError, setLineError] = useState<unknown>(undefined);
  const [lineNotice, setLineNotice] = useState<string | undefined>(undefined);

  /**
   * 第 1 步选的新建线路**默认网络代理**（`""` = 没选过 → 用 `net.default_lane`
   * → 直连那条 lane）。它只影响**新建**的线路；建完之后每行还能各自改。
   */

  // ---- 只读数据（第 2 步才需要，所以按步开启）

  /** 这家服务商的**全部线路**（`channel_list` 过滤到这一家）—— 第 2 步唯一的列表来源。 */
  const channels = useCommandQuery("channel_list", undefined, qk.channels, {
    enabled: open && step === 2,
  });
  /** 目录（`provider_model_catalog`）：每一行的**上游原文**从这里来。 */
  const catalog = useCommandQuery(
    "provider_model_catalog",
    { provider_id: session.saved?.providerId ?? "" },
    qk.providerModelCatalog(session.saved?.providerId ?? ""),
    { enabled: open && step === 2 && saved !== undefined },
  );
  // ---- 写命令（都已注册：见 `src-tauri/src/commands/mod.rs` 的 collect_commands!）

  const upsertProvider = useCommandMutation("provider_upsert");
  const upsertCredential = useCommandMutation("credential_upsert");
  const setSecret = useCommandMutation("credential_set_secret");

  // ---- 本地校验

  const allIssues = useMemo(
    () => validateStep1(draft, existing, saved),
    [draft, existing, saved],
  );
  /**
   * 显示哪几条：**名称被占用**即时提示（用户刚敲完就该看到），
   * 其余（必填 / base_url 形状）在用户点过一次按钮之后再列出来 —— 打开表单就满屏红字没有意义。
   */
  const shownIssues = submitted ? allIssues : nameTakenIssue(draft, existing, saved);

  /**
   * 这一份服务商的 id。会话里已经有（`pw-test` 落过库）→ **就用它**，绝不重新派生
   * （否则"测试连接"之后再点下一步就会变成另一个服务商）；还没有就按名称派生。
   */
  const providerId = saved?.providerId ?? deriveProviderId(draft.name, existing.ids);

  // ---- 第 1 步：落库（`pw-next` 与"还没落库时的 pw-test"共用）

  const saveStep1 = async (): Promise<string | undefined> => {
    setSaveError(undefined);
    setSaving(true);
    try {
      // 本会话已经落过库 -> **继续用那个 id**：改名不等于新建一个服务商
      // （否则「测试连接」之后再改一下名字，库里就会多出一个没人认领的服务商）。
      const candidateId = saved?.providerId ?? deriveProviderId(draft.name, existing.ids);
      const provider = await upsertProvider.mutateAsync({
        provider: step1ProviderPayload(draft, candidateId),
      });
      // 后端返回的 id 才是权威（upsert 可能改写）；凭据挂在它下面
      const landedId =
        typeof provider?.id === "string" && provider.id !== "" ? provider.id : candidateId;
      const keyWritten = draft.key !== "";
      await upsertCredential.mutateAsync({
        credential: defaultCredentialPayload(landedId, draft.name),
      });
      if (keyWritten) {
        await setSecret.mutateAsync({ id: defaultCredentialId(landedId), plaintext: draft.key });
      }
      // 明文用完就从草稿里抹掉（它已经在密钥仓库里了，没有理由继续留在内存里）。
      // 同时把"这一份本会话已经落库了 / 密钥已经存过了"记进会话 —— 校验要看它，
      // 否则用户按「测试连接 -> 下一步」走会被自己刚建的那一份挡住（见 SavedInSession）。
      const stored: SavedInSession = {
        providerId: landedId,
        name: draft.name.trim(),
        secretStored: keyWritten || saved?.secretStored === true,
      };
      patchSession((prev) => ({
        saved: stored,
        draft: { ...prev.draft, key: "" },
      }));
      await queryClient.invalidateQueries({ queryKey: qk.providers });
      await queryClient.invalidateQueries({ queryKey: qk.credentials });
      return landedId;
    } catch (err) {
      setSaveError(err);
      return undefined;
    } finally {
      setSaving(false);
    }
  };

  const submitStep1 = (): void => {
    setSubmitted(true);
    if (validateStep1(draft, existing, saved).length > 0) return;
    void (async () => {
      const landedId = await saveStep1();
      if (landedId !== undefined) patchSession(() => ({ step: 2 }));
    })();
  };

  /**
   * 「测试连接」：**非必填**，但一旦按下就真调 `provider_test`。
   *
   * 服务商还没落库时，先老老实实落一次库（并且把这件事说出来），再探测 ——
   * 绝不静默产生持久化副作用（主管裁决）。
   */
  const runTest = (): void => {
    setSubmitted(true);
    if (validateStep1(draft, existing, saved).length > 0) return;
    void (async () => {
      let id = saved?.providerId;
      if (id === undefined) id = await saveStep1();
      if (id === undefined) return;
      setProbePending(true);
      setProbeError(undefined);
      setProbeResult(undefined);
      try {
        const result = await api.provider.test(id);
        setProbeResult(result);
      } catch (err) {
        setProbeError(err);
      } finally {
        setProbePending(false);
      }
    })();
  };

  /**
   * 第 1 步那个代理下拉的选项来源：`lane_list`（所有已配置线路）
   * + 启用的订阅（「自动」，一份订阅一个）。与详情页用**同一个** `providerLaneChoices` ——
   * 两处顺序不许不一样，否则用户会觉得"换个入口就变了"。
   */
  const lanesQuery = useCommandQuery("lane_options", undefined, [...qk.lanes, "options"]);
  const subsQuery = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const laneChoices = useMemo(
    () =>
      providerLaneChoices(
        lanesQuery.data ?? [],
        t("providers.edit.laneInherit"),
        (subsQuery.data ?? [])
          .map((item) => {
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
        t("providers.edit.laneAuto"),
      ),
    [lanesQuery.data, subsQuery.data, t],
  );

  // ---- 第 2 步：扫描 / 线路（一个列表）


  const providerLines = useMemo(
    () => linesOfProvider(channels.data, providerId),
    [channels.data, providerId],
  );

  /**
   * 扫描（`model_import_from_provider`）→ **紧接着把清单里的每条线路都建出来**
   * （`provider_lines_create_all`，**默认关闭**）。
   *
   * 两步分开调是有意的：目录是"上游给了什么"（事实），建线路是"我们拿它做什么"（决定）——
   * 糊成一条命令，就没法解释"扫到了但一条都没建出来"这种状态。
   */
  const runScan = (): void => {
    setScanError(undefined);
    setLineError(undefined);
    setLineNotice(undefined);
    setLinesCreated(undefined);
    const id = saved?.providerId;
    if (id === undefined) {
      setNeedsProvider(true);
      return;
    }
    setNeedsProvider(false);
    setPreview(undefined);
    setScanPending(true);
    void (async () => {
      try {
        const result = await api.model.importFromProvider(id);
        setPreview(result);
        // **不带 lane**：代理只属于服务商，向导不写线路级代理
        const created = await api.model.createAllLines(id, null);
        setLinesCreated({ created: created.created, total: created.total });
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: qk.channels }),
          queryClient.invalidateQueries({ queryKey: qk.providerModelCatalog(id) }),
        ]);
      } catch (err) {
        setScanError(err);
      } finally {
        setScanPending(false);
      }
    })();
  };

  /**
   * 进第 2 步**自动扫一次**（用户："向导添加线路的时候也不用手动点击扫描线路了，
   * 直接默认扫描，过程中显示正在扫描"）。
   *
   * 只在这一步的**首次**进入时自动跑（`autoScanned` 记住跑过没有）：用户点了「重新扫描」之后
   * 又点别的东西，不该被反复扫描——上游请求是真花钱/花时间的。
   * 没落库（`saved === undefined`，用户从 `/network` 回来那种）时不扫：没有 provider_id 可扫，
   * `runScan` 会如实报"先保存服务商"。
   */
  const [autoScanned, setAutoScanned] = useState(false);
  useEffect(() => {
    if (!open || step !== 2) {
      if (!open) setAutoScanned(false);
      return;
    }
    if (autoScanned || saved === undefined) return;
    setAutoScanned(true);
    runScan();
    // `runScan` 每次渲染都是新的函数引用（它读 saved/lane），依赖它会让 effect 反复触发；
    // 这里真正要盯的只有"打开 + 到了第 2 步 + 有没有扫过"。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, step, saved?.providerId, autoScanned]);

  /** 滑块：启用 / 关闭一条线路（可逆，`channel_set_enabled`）。 */
  const toggleEnabled = (line: LineRow, enabled: boolean): void => {
    setLineError(undefined);
    setLineNotice(undefined);
    void (async () => {
      try {
        await api.channel.setEnabled(line.id, enabled);
        await queryClient.invalidateQueries({ queryKey: qk.channels });
      } catch (err) {
        setLineError(err);
      }
    })();
  };

  /** **全选**：一次动作把列表里的线路一起启用 / 一起关闭（批量命令，不是前端循环）。 */
  const bulkEnabled = (enabled: boolean): void => {
    setLineError(undefined);
    setLineNotice(undefined);
    const ids = (providerLines ?? []).map((line) => line.id).filter((id) => id !== "");
    if (ids.length === 0) return;
    void (async () => {
      try {
        await api.channel.bulkSetEnabled(ids, enabled);
        setLineNotice(
          enabled ? t(K.allEnabled, { count: ids.length }) : t(K.allDisabled, { count: ids.length }),
        );
        await queryClient.invalidateQueries({ queryKey: qk.channels });
      } catch (err) {
        setLineError(err);
      }
    })();
  };

  const close = (): void => {
    // 取消 = 停止继续，**不回滚**已经落库的服务商 / 凭据 / 线路；
    // 只把界面草稿清干净，免得下次打开带着上一次的残值与旧回显。
    resetWizardSession();
    setSubmitted(false);
    setSaveError(undefined);
    setProbeResult(undefined);
    setProbeError(undefined);
    setPreview(undefined);
    setScanError(undefined);
    setLinesCreated(undefined);
    setLineError(undefined);
    setLineNotice(undefined);
    onClose();
  };

  /** 协议下拉：正常情况下就是 `PROTOCOLS`；遇到契约外的值**原样补一行**，不改写它。 */
  const protocolOptions = useMemo(() => {
    const known = PROTOCOLS.map((protocol) => ({
      value: protocol as string,
      label: t(PROTOCOL_LABEL[protocol] ?? "providers.protocol.custom"),
    }));
    const raw = draft.protocol as string;
    if (raw === "" || known.some((option) => option.value === raw)) return known;
    return [{ value: raw, label: raw }, ...known];
  }, [draft.protocol, t]);

  const probeText = (): string | undefined => {
    if (probeError !== undefined && probeError !== null) return errorMessage(probeError);
    if (probeResult === undefined) return undefined;
    switch (probeOutcome(probeResult)) {
      case "ok":
        return t("providers.wizard.testOk", { ms: formatInt(probeResult.latency_ms) });
      case "no-endpoint":
        // 上游没有 /models ≠ 连不通
        return t("providers.wizard.testNoEndpoint");
      case "failed":
        return probeFailureText(probeResult);
    }
  };
  const testText = probeText();

  /** 扫描结果那句话：扫到几个、本次新建几条、目录里共几条（数字全部来自后端）。 */
  const scanNotice =
    linesCreated === undefined
      ? undefined
      : t(
          K.linesCreatedAll,
          linesCreatedNoticeVars(
            preview === undefined ? 0 : preview.models.length + preview.skipped.length,
            linesCreated.created,
            linesCreated.total,
          ),
        );

  const busy =
    scanPending ||
    channels.isFetching ||
    upsertProvider.isPending ||
    upsertCredential.isPending ||
    setSecret.isPending;

  return (
    <Dialog
      open={open}
      onClose={close}
      fullWidth
      maxWidth="md"
      slotProps={{ paper: { sx: { maxHeight: "90vh" } } }}
    >
      <DialogTitle>
        {step === 2 ? t(K.step2Title) : t("providers.wizard.step1Title")}
        <Typography variant="caption" sx={{ ml: 2, color: "text.secondary" }}>
          {t("providers.wizard.stepOf", { current: step, total: 2 })}
        </Typography>
      </DialogTitle>

      <DialogContent>
        {/* ---------------------------------------------------------------- 第 1 步 */}
        {step === 1 ? (
          <Stack spacing={3} data-testid="provider-wizard-step1">
            {saved !== undefined ? (
              <Typography variant="body1" data-testid="pw-saved-notice">
                {t("providers.wizard.savedNotice")}
              </Typography>
            ) : null}

            <Stack spacing={1} data-testid="pw-name">
              <TextField
                label={t("providers.field.name")}
                value={draft.name}
                onChange={(event) => patchSession(() => ({ draft: { ...draft, name: event.target.value } }))}
                size="small"
                fullWidth
              />
              {/* 派生的 provider id 如实显示：用户看得见它，就不用猜"我的名字变成了什么" */}
              <Caption>{`${t("providers.field.id")}: ${providerId}`}</Caption>
            </Stack>

            {/* 协议清单来自 `PROTOCOLS`（被 `protocolList.guard.test.ts` 钉着，不另编一份） */}
            <SelectField
              testId="pw-protocol"
              label={t("providers.field.protocol")}
              value={draft.protocol}
              onChange={(value) =>
                patchSession(() => ({ draft: { ...draft, protocol: value as Step1Draft["protocol"] } }))
              }
              options={protocolOptions}
              emptyLabel={t("wizard.required")}
            />

            <Stack spacing={1} data-testid="pw-baseUrl">
              <TextField
                label={t("providers.field.baseUrl")}
                value={draft.baseUrl}
                onChange={(event) =>
                  patchSession(() => ({ draft: { ...draft, baseUrl: event.target.value } }))
                }
                size="small"
                fullWidth
                slotProps={{ htmlInput: { inputMode: "url" } }}
              />
            </Stack>

            {/**
             * **为服务商选代理**（用户第 112 轮：这一项必须存在于"添加"向导，
             * 且能看到**所有网络代理**以及「自动」「直连」）。
             *
             * 选项与详情页用的是**同一个** `providerLaneChoices`（顺序也一致）：
             * `直连 → 不指定（继承）→ 自动（按订阅）→ 所有已配置线路`。
             * 值是标记（`direct:` / `auto:<订阅>`）时由后端 `provider_upsert` 解析成真实线路。
             */}
            <SelectField
              testId="pw-lane"
              label={t("providers.edit.lane")}
              value={draft.lane}
              onChange={(lane) => patchSession(() => ({ draft: { ...draft, lane } }))}
              options={laneChoices}
              emptyLabel={t("providers.edit.laneInherit")}
            />

            {/* 明文密钥：type=password，只进密钥仓库（成功后立刻从草稿里抹掉）。
                **一个服务商就一把密钥**（硬规则 3）：所以这里没有"粘贴多行 →
                展开成 N 份凭据"那套东西，一次运行只会发出**一条** `credential_upsert`。
                旁边的问号说明"想要两个账号怎么办"（`providers.key.multipleHint`：再加一条服务商、
                取不同名字）。 */}
            <Stack spacing={1} data-testid="pw-key">
              <Stack direction="row" spacing={1} alignItems="center">
                <TextField
                  type="password"
                  label={t("onboarding.provider.secret")}
                  value={draft.key}
                  onChange={(event) => patchSession(() => ({ draft: { ...draft, key: event.target.value } }))}
                  size="small"
                  fullWidth
                  autoComplete="new-password"
                />
                <HelpTip text={t("providers.key.multipleHint")} testId="pw-key-multiple-help" />
              </Stack>
              {/* 明文密钥只进密钥仓库：写成功之后立刻从草稿里抹掉（成功后输入框会空） */}
              <Caption>{t("credentials.add.secret")}</Caption>
            </Stack>


            <Issues issues={shownIssues} backendError={saveError} />

            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Button
                variant="outlined"
                data-testid="pw-test"
                disabled={saving || probePending}
                onClick={runTest}
              >
                {t("onboarding.test.title")}
              </Button>
              {/* 按钮旁边的说明：测试**不是必填** */}
              <Caption>{t("providers.wizard.testOptional")}</Caption>
              {/* 测试结果：成功显示后端给的延迟；404/models 说"上游没有这个端点"；其余原样显示后端原文 */}
              {testText !== undefined ? (
                <Typography variant="body1" data-testid="pw-test-result">
                  {testText}
                </Typography>
              ) : null}
            </Stack>

            <Stack direction="row" spacing={2} sx={{ flexWrap: "wrap" }}>
              <Button
                variant="outlined"
                data-testid="pw-proxy"
                onClick={() => navigate("/network?from=provider-wizard")}
              >
                {t("providers.wizard.toProxy")}
              </Button>
            </Stack>
          </Stack>
        ) : (
          /* ---------------------------------------------------------------- 第 2 步 */
          <Box data-testid="pw-step2-scroll" sx={SCROLL_SX}>
            <Stack spacing={4} data-testid="provider-wizard-step2">
              {/* 已落库这件事必须说出来：关掉向导不会撤销它们 */}
              <Typography variant="body1" data-testid="pw-saved-notice">
                {t("providers.wizard.savedNotice")}
              </Typography>

              {/*：**这里建的是线路，模型不会自动创建**。 */}
              <Typography variant="body1" data-testid="pw-step2-why">
                {t(K.step2Why)}
              </Typography>

              {needsProvider ? (
                <Typography variant="body1" data-testid="pw-scan-error" sx={{ color: "error.main" }}>
                  {t(K.scanNeedsProvider)}
                </Typography>
              ) : scanError !== undefined && scanError !== null ? (
                <Typography variant="body1" data-testid="pw-scan-error" sx={{ color: "error.main" }}>
                  {errorMessage(scanError)}
                </Typography>
              ) : null}

              {/* 上游确实一个都没返回：如实说，不编数据、不给假进度 */}
              {preview !== undefined && preview.models.length === 0 ? (
                <Typography variant="body1" data-testid="pw-scan-empty">
                  {t(K.scanEmpty)}
                </Typography>
              ) : null}
              {preview !== undefined && preview.skipped.length > 0 ? (
                <Caption>{t(K.scanSkipped, { count: preview.skipped.length })}</Caption>
              ) : null}

              {/*
                第 2 步的正文 = 详情页那一节（用户第 82 轮："一个列表"）。
                扫描按钮的 testid 在这里是历史上的 `pw-scan`（E2E 与单测都按它命中）；
                向导里**没有**删除入口（新增流程不删东西）→ 不传 `onRemove`，也不画那个按钮。
              */}
              <LinesSection
                lines={providerLines}
                linesError={channels.error ?? undefined}
                catalog={saved === undefined ? undefined : catalog.data}
                catalogError={catalog.error ?? undefined}
                scanPending={scanPending}
                onScan={runScan}
                busy={busy}
                error={lineError}
                notice={scanNotice}
                lineNotice={lineNotice}
                onToggleEnabled={toggleEnabled}
                onBulkEnabled={bulkEnabled}
                scanTestId="pw-scan"
                scanLabel={t("providers.wizard.scan")}
              />

              {/* 指路：线路建好了，**模型要用户自己去模型页分配线路**。 */}
              <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                <Button
                  variant="outlined"
                  data-testid="pw-goto-models"
                  onClick={() => {
                    close();
                    navigate("/models");
                  }}
                >
                  {t(K.gotoModels)}
                </Button>
                <Button
                  variant="outlined"
                  data-testid="pw-scan-goto-detail"
                  onClick={() => {
                    const target = providerId;
                    close();
                    navigate(`/providers/${encodeURIComponent(target)}`);
                  }}
                >
                  {t("providers.catalog.goDetail")}
                </Button>
              </Stack>
            </Stack>
          </Box>
        )}
      </DialogContent>

      {/**
       * **主操作放在对话框的固定按钮区**（`DialogActions` 不参与内容滚动）：
       * 第 2 步的线路列表可以很长，把「完成」摆在列表末尾等于让用户先滚到底才能收尾。
       * 取消 / 下一步 / 完成 —— 三个按钮永远看得见。
       */}
      <DialogActions>
        <Button data-testid="pw-cancel" onClick={close}>
          {t("common.cancel")}
        </Button>
        {step === 1 ? (
          <Button
            variant="contained"
            data-testid="pw-next"
            disabled={saving || probePending}
            onClick={submitStep1}
          >
            {t("providers.wizard.next")}
          </Button>
        ) : (
          <Button variant="contained" data-testid="pw-finish" onClick={close}>
            {t("providers.wizard.finish")}
          </Button>
        )}
      </DialogActions>
    </Dialog>
  );
}

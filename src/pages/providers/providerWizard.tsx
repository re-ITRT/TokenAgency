/**
 * `/providers` 的**两步向导**（[docs/19 §1 流程 A]）—— 点 `providers-add-open` 进来的就是这个。
 *
 * 为什么要有它：原来的「添加服务商」是单步表单（服务商 + 凭据 + 密钥一次填完），
 * 而用户要的流程是"下一步 → 下一步"：先把**这家服务商**配好（顺带测一下通不通），
 * 再挑**要暴露哪些模型**。后者需要先落库才能扫描（扫描要拿 provider 的地址与凭据），
 * 所以第 1 步结束时**真的写配置**，并且这件事必须在界面上说出来（`pw-saved-notice`）。
 *
 * 三条纪律（写在这里，免得下一个人再犯）：
 *
 * 1. **无假按钮 / 无假进度**：`pw-scan` 只有真的在等后端时才显示"正在扫描"，
 *    `pw-scan-add` / `pw-scan-select-all` 只在一份**真实的**扫描结果在手时才出现。
 * 2. **明文只进密钥仓库**：`pw-key` 的值只作为 `credential_set_secret` 的入参发出，
 *    落库成功后立刻从草稿里抹掉；它不进 `provider_upsert` / `credential_upsert` 的载荷、
 *    不进错误消息、不上屏。
 * 3. **`data-testid` 挂在容器上**（MUI 的 TextField / Select / Checkbox 不吃它，直接写会 TS2353），
 *    真机 E2E 与单测都按 `[data-testid="X"] input` 命中同一形状。
 *
 * 命令接法（[docs/19 §1.3]）：`provider_upsert` → `credential_upsert`（id = `<provider>-default`）
 * → 填了密钥才 `credential_set_secret`；扫描 `model_import_from_provider`；添加
 * `model_apply_import`；改网络代理 `model_bulk_update({ ids, patch: { lane } })`。
 * 三条"还在 Rust 那边注册中"的命令走 `@/shared/api` 的**类型化门面**（`api.provider.test` 等），
 * 不在页面里写裸命令字符串 —— 这样 `commandSurface.guard.test.ts`（"调用方只许引用已注册命令"）
 * 保持绿，同时页面调的是真命令（真机上注册完即生效，浏览器里靠 `registerMock`）。
 */

import SettingsOutlinedIcon from "@mui/icons-material/SettingsOutlined";
import {
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  IconButton,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";

import { api, errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { ImportPreview, LaneId, ProbeResult } from "@/shared/bindings";
import { HelpTip } from "@/shared/components";
import { formatInt } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { typography } from "@/theme";

import { PROTOCOLS } from "./protocols";
import {
  addedModelsOf,
  defaultCredentialId,
  defaultCredentialPayload,
  defaultLaneOf,
  deriveProviderId,
  directLaneIdOf,
  laneChoices,
  laneValue,
  nameTakenIssue,
  probeFailureText,
  probeOutcome,
  readWizardSession,
  resetWizardSession,
  selectAllIds,
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

/** 数值缺失一律画「—」（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */
const DASH = "—";

const SCROLL_SX = { overflowY: "auto", maxHeight: "64vh" } as const;
const PANEL_SX = { p: 2, bgcolor: "action.hover" } as const;

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

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
 * 两步向导（[docs/19 §1] 新增 / [docs/20 §4] 编辑）。
 *
 * **模式来自会话**（`WizardSession.mode`）而不是 prop：编辑模式要预填草稿、并把
 * "这是我自己那一份"登记进 `saved`（[`startEditSession`]），这两件事在**打开之前**就完成了 ——
 * 调用方（`index.tsx` / `detail.tsx`）先写 store 再 `open`，与 `pw-proxy` 去 `/network`
 * 再回来是同一条数据路径。于是组件里只有一套状态，不需要"再从 prop 种一次草稿"的第二条真相源。
 */
export function ProviderTwoStepWizard({ open, existing, onClose }: ProviderTwoStepWizardProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  /**
   * 会话（草稿 / 当前步 / 已落库的那一份）活在**模块级 store** 里：点 `pw-proxy`
   * 去 `/network` 会把整个页面卸载掉，草稿必须比组件活得久（[docs/19 §1.4]）。
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
  /** 编辑模式（`providers-detail-edit` 进来的那一档）：第 1 步预填、留空密钥 = 不改。 */
  const editing = session.mode === "edit";

  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<unknown>(undefined);

  const [probePending, setProbePending] = useState(false);
  const [probeResult, setProbeResult] = useState<ProbeResult | undefined>(undefined);
  const [probeError, setProbeError] = useState<unknown>(undefined);

  const [scanPending, setScanPending] = useState(false);
  const [preview, setPreview] = useState<ImportPreview | undefined>(undefined);
  const [scanError, setScanError] = useState<unknown>(undefined);
  const [applyError, setApplyError] = useState<unknown>(undefined);
  const [appliedCount, setAppliedCount] = useState<number | undefined>(undefined);
  /** 后端返回的 `channels_created`：本次**真新建**了几条线路。如实显示，不编数字。 */
  const [channelsCreated, setChannelsCreated] = useState<number | undefined>(undefined);
  /** 改线路失败时后端说的话（[docs/20 §3]：不许吞）。 */
  const [needsProvider, setNeedsProvider] = useState(false);
  const [selection, setSelection] = useState<readonly string[]>([]);
  const [infoOpen, setInfoOpen] = useState<string | undefined>(undefined);

  // ---- 只读数据（第 2 步才需要，所以按步开启；编辑模式第 1 步还要读凭据，见下）

  const matrix = useCommandQuery("channel_matrix", undefined, qk.channelMatrix, {
    enabled: open && step === 2,
  });
  /**
   * `credential_list`：
   * - 第 2 步用来把 `channel_matrix` 的 cell 归到"这家服务商的凭据"上；
   * - **编辑模式第 1 步也要**：用户填了新密钥时要写的是**这家已有的那把**凭据，
   *   而不是凭空新造一个 `<provider>-default` 挂上去。
   */
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials, {
    enabled: open && (step === 2 || editing),
  });
  const laneOptions = useCommandQuery("lane_pick_options", undefined, qk.lanePickOptions, {
    enabled: open && step === 2,
  });
  /**
   * `model_list` 只为了读 `Model.default_lane`（[docs/19 §1.5] 的"下一次建 channel 用的 lane"）：
   * 一个还没有 channel 的模型改完 lane 之后，矩阵里看不到任何变化，
   * 只有这个字段能证明"改的东西真的存下来了"。
   */
  const models = useCommandQuery("model_list", undefined, qk.models, { enabled: open && step === 2 });
  const configDoc = useCommandQuery("config_get", undefined, qk.config, { enabled: open });

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
   * 这一份服务商的 id。
   *
   * - 会话里已经有（`pw-test` 落过库 / 编辑模式种下的现有 id）→ **就用它**，
   *   绝不重新派生（否则改名就等于新建一个服务商，[docs/20 §4] 明确禁止）；
   * - 编辑模式一定来自 `startEditSession`，所以 `saved` 必然在；万一不在就退回派生，
   *   让界面显示出一个 id 而不是空白（用户看得见就不会猜）。
   */
  const providerId = saved?.providerId ?? deriveProviderId(draft.name, existing.ids);

  /**
   * 编辑模式下写密钥要落到的凭据 id：**优先这家已有的那把**。
   *
   * 为什么不能直接用 `<provider>-default`：`c-bytedance` 这类 id 是用户/历史建出来的，
   * 编辑时再 upsert 一个 `<provider>-default` 等于往这家服务商下面多挂一份重复凭据。
   * 顺序：`<provider>-default`（第 1 步建的那种）→ 该 provider 的第一条凭据 → 兜底新建 `<provider>-default`。
   */
  const editCredentialId = useMemo(() => {
    const own = (credentials.data ?? [])
      .filter((view) => view.credential.provider === providerId)
      .map((view) => view.credential.id);
    const preferred = defaultCredentialId(providerId);
    return own.includes(preferred) ? preferred : (own[0] ?? preferred);
  }, [credentials.data, providerId]);

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
      if (editing) {
        /**
         * 编辑模式（[docs/20 §4]）：**留空密钥 = 不改**。
         *
         * 所以一个字节都不碰凭据与密钥仓库 —— 只覆盖写服务商自己。
         * 填了新值才写：写的是这家**已有**的那把凭据（[`editCredentialId`]）。
         */
        if (keyWritten) {
          await upsertCredential.mutateAsync({
            credential: defaultCredentialPayload(landedId, draft.name, editCredentialId),
          });
          await setSecret.mutateAsync({ id: editCredentialId, plaintext: draft.key });
        }
      } else {
        await upsertCredential.mutateAsync({
          credential: defaultCredentialPayload(landedId, draft.name),
        });
        if (keyWritten) {
          await setSecret.mutateAsync({ id: defaultCredentialId(landedId), plaintext: draft.key });
        }
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
   * 绝不静默产生持久化副作用（主管裁决，[docs/19 §1.3]）。
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

  // ---- 第 2 步：扫描 / 添加 / 网络代理

  const runScan = (): void => {
    setApplyError(undefined);
    setAppliedCount(undefined);
    const id = saved?.providerId;
    if (id === undefined) {
      setNeedsProvider(true);
      return;
    }
    setNeedsProvider(false);
    setPreview(undefined);
    setScanError(undefined);
    setSelection([]);
    setScanPending(true);
    void (async () => {
      try {
        const result = await api.model.importFromProvider(id);
        setPreview(result);
        setSelection([]);
      } catch (err) {
        setScanError(err);
      } finally {
        setScanPending(false);
      }
    })();
  };

  const applySelected = (): void => {
    if (preview === undefined) return;
    setApplyError(undefined);
    setAppliedCount(undefined);
    setChannelsCreated(undefined);
    void (async () => {
      try {
        /**
         * 走 `@/shared/api` 的**类型化门面**（页面不写裸命令字符串）；第三个参数是本轮契约
         * 新增的 `lane?`（[docs/20 §2.3]）—— 把**当前默认 lane** 一起发下去，
         * 新通道的 lane 由它决定。不发的话后端只能自己猜，界面上的"默认直连"就与线上不是一回事。
         */
        const result = await api.model.applyImport(
          preview.preview_token,
          [...selection],
          newModelLane,
        );
        // 只有后端说 applied 才算加了；条数是"我们提交了几条"，不是编出来的数字
        setAppliedCount(result.applied ? selection.length : undefined);
        // `channels_created` = 后端说这次**真新建**了几条线（0 也如实显示）
        setChannelsCreated(
          typeof result.channels_created === "number" ? result.channels_created : undefined,
        );
        /**
         * **必须等这一次取数回来**（[docs/20 §3]）：上面那块「已添加的模型」是
         * **从 `channel_matrix` 派生的**，fire-and-forget 的话"添加成功"这句话说出来时
         * 上面还是空的 —— 用户第 1 条反馈看到的正是这个。
         * 凭据也一起失效：新模型可能刚落上一份凭据归属，第 2 步的过滤按它算。
         */
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: qk.channelMatrix }),
          queryClient.invalidateQueries({ queryKey: qk.models }),
          queryClient.invalidateQueries({ queryKey: qk.credentials }),
        ]);
      } catch (err) {
        setApplyError(err);
      }
    })();
  };

  const close = (): void => {
    // 取消 = 停止继续，**不回滚**已经落库的服务商与凭据（[docs/19 §1.3]）；
    // 只把界面草稿清干净，免得下次打开带着上一次的残值与旧回显。
    resetWizardSession();
    setSubmitted(false);
    setSaveError(undefined);
    setProbeResult(undefined);
    setProbeError(undefined);
    setPreview(undefined);
    setScanError(undefined);
    setApplyError(undefined);
    setAppliedCount(undefined);
    setChannelsCreated(undefined);
    setSelection([]);
    setInfoOpen(undefined);
    onClose();
  };

  // ---- 第 2 步的派生数据

  /** 属于这家服务商的凭据 id（`channel_matrix` 的 cell 只挂到凭据上，凭据再挂到服务商上）。 */
  const credentialIds = useMemo(() => {
    const ids = new Set<string>();
    for (const view of credentials.data ?? []) {
      if (view.credential.provider === providerId) ids.add(view.credential.id);
    }
    return ids;
  }, [credentials.data, providerId]);

  const addedModels = useMemo(
    () => addedModelsOf(matrix.data, credentialIds),
    [matrix.data, credentialIds],
  );

  const defaultLane = defaultLaneOf(configDoc.data);
  /** 模型级默认 lane（`Model.default_lane`）：改过 lane 的模型靠它把改动显示回来。 */
  const modelDefaultLanes = useMemo(() => {
    const map = new Map<string, string>();
    for (const view of models.data ?? []) {
      const lane = view.model.default_lane;
      if (typeof lane === "string" && lane !== "") map.set(view.model.id, lane);
    }
    return map;
  }, [models.data]);
  /**
   * 直连那条 lane 的 id（[docs/20 §1.2 第 3 步]）与下拉选项。
   * 前者让"没配过"显示成一个**真实的值**（而不是空白/「未配置」）。
   */
  const directLaneId = useMemo(() => directLaneIdOf(laneOptions.data ?? []), [laneOptions.data]);
  const laneOptionList = useMemo(
    () => laneChoices(laneOptions.data ?? [], t("network.directLabel")),
    [laneOptions.data, t],
  );
  /**
   * 新增模型时的默认 lane = `net.default_lane`（用户在 `/network` 选的"新建服务商默认代理"）
   * → 直连那条 lane。取不到直连 id 时回落到 `null`（继承，链路末端仍是直连）。
   */
  const newModelLane: LaneId | null = defaultLane ?? (directLaneId === "" ? null : directLaneId);

  /** 一个 lane 值在下拉里显示的名字（直连 / `组名 · 线路名`）；找不到就退回原值。 */
  const laneLabelOf = (value: string): string =>
    laneOptionList.find((choice) => choice.value === value)?.label ?? value;

  /** 协议下拉：正常情况下就是 `PROTOCOLS`；编辑模式遇到契约外的值**原样补一行**，不改写它。 */
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
        // 上游没有 /models ≠ 连不通（[docs/19 §1.1]）
        return t("providers.wizard.testNoEndpoint");
      case "failed":
        return probeFailureText(probeResult);
    }
  };
  const testText = probeText();

  return (
    <Dialog
      open={open}
      onClose={close}
      fullWidth
      maxWidth="md"
      slotProps={{ paper: { sx: { maxHeight: "90vh" } } }}
    >
      <DialogTitle>
        {/* 编辑模式（[docs/20 §4]）：标题说清"这是在改已有的那一份"，不是新建 */}
        {step === 2
          ? t("providers.wizard.step2Title")
          : editing
            ? t("providers.wizard.editTitle")
            : t("providers.wizard.step1Title")}
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

            {/* 明文密钥：type=password，只进密钥仓库（成功后立刻从草稿里抹掉） */}
            <Stack spacing={1} data-testid="pw-key">
              <TextField
                type="password"
                label={t("onboarding.provider.secret")}
                value={draft.key}
                onChange={(event) => patchSession(() => ({ draft: { ...draft, key: event.target.value } }))}
                size="small"
                fullWidth
                autoComplete="new-password"
              />
              {/* 编辑模式必须说清"留空 = 不改"（[docs/20 §4]）：否则用户会以为密钥被清掉了 */}
              <Caption>
                {editing ? t("providers.wizard.keyUnchanged") : t("credentials.add.secret")}
              </Caption>
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
              {/* 按钮旁边的说明：测试**不是必填**（[docs/19 §1.1]） */}
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
              {/* 已落库这件事必须说出来：关掉向导不会撤销它们（[docs/19 §1.3]） */}
              <Typography variant="body1" data-testid="pw-saved-notice">
                {t("providers.wizard.savedNotice")}
              </Typography>

              <Stack spacing={2}>
                {/* 「没选过就是直连（继承链末端是直连）」是这一块的**说明**（[docs/22 §2]：
                    区块说明超过 12 个字符就收）→ 收进标题右边的问号。
                    **下面每一行那句「线路只在通道页改」不走这里**：它是只读约束 + 指路文案
                    （[docs/21 §4.3]），用户必须一眼看到，属于红线。 */}
                <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
                  <Typography variant="h4">{t("providers.wizard.addedModels")}</Typography>
                  <HelpTip
                    text={t("providers.wizard.modelLaneDefaultHint")}
                    testId="pw-added-models-help"
                  />
                </Box>
                <Stack spacing={2} data-testid="pw-added-models">
                  {addedModels.length === 0 ? (
                    <Typography variant="body1" data-testid="pw-added-models-empty">
                      {t("providers.wizard.addedModelsEmpty")}
                    </Typography>
                  ) : (
                    addedModels.map((row) => {
                      /**
                       * 该行现在走哪条线（[docs/20 §1.2]）：
                       * ① channel 自己的 lane（后端在用的真值）→ ② 模型级默认 lane（改过的值存这里）
                       * → ③ `net.default_lane`（第 1 步/`/network` 选的）→ ④ **直连**。
                       *
                       * 最后一步是这轮补上的：以前落到 `null`，下拉渲染成空白，用户看到的就是
                       * 「未配置」—— 而继承链的末端明明是直连。
                       */
                      const lane = row.laneId ?? modelDefaultLanes.get(row.modelId) ?? defaultLane;
                      const value = laneValue(lane, directLaneId === "" ? null : directLaneId);
                      return (
                        <Stack key={row.modelId} spacing={1} data-testid="pw-model-row">
                          <Stack
                            direction="row"
                            spacing={2}
                            alignItems="center"
                            sx={{ flexWrap: "wrap" }}
                          >
                            <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                              {row.modelId}
                            </Typography>
                            <Typography variant="body1" sx={{ color: "text.secondary" }}>
                              {row.upstreamModel === "" ? DASH : row.upstreamModel}
                            </Typography>

                            {/* 「设置图标」按钮：hover 提示"查看模型信息"，点开只读面板 */}
                            <Box
                              data-testid="pw-model-info"
                              title={t("providers.wizard.viewInfo")}
                            >
                              <IconButton
                                size="small"
                                aria-label={t("providers.wizard.viewInfo")}
                                onClick={() =>
                                  setInfoOpen((prev) => (prev === row.modelId ? undefined : row.modelId))
                                }
                              >
                                <SettingsOutlinedIcon fontSize="small" />
                              </IconButton>
                            </Box>

                            {/* 线路：**这里只看不改**（[docs/21 §4.3]）。
                                用户原话是"线路设置不能点进服务商里面去修改" —— 所以设置入口搬到了
                                通道页（每条通道一个线路下拉），这里只显示当前生效的线路 + 一句指路。 */}
                            <Stack
                              spacing={0.5}
                              sx={{ minWidth: 240 }}
                              data-testid="pw-model-lane"
                            >
                              <Caption>{t("providers.wizard.modelLane")}</Caption>
                              <Typography variant="body1" data-testid="pw-model-lane-name">
                                {laneLabelOf(value)}
                              </Typography>
                              <Typography
                                variant="caption"
                                data-testid="pw-model-lane-readonly"
                                sx={{ color: "text.secondary" }}
                              >
                                {t("providers.wizard.laneReadonlyHint")}
                              </Typography>
                            </Stack>
                          </Stack>

                          {/* 只读信息面板：里面不许有任何可写控件（面板内 0 个 input/select/button） */}
                          {infoOpen === row.modelId ? (
                            <Stack spacing={1} data-testid="pw-model-info-panel" sx={PANEL_SX}>
                              <Caption>{t("providers.wizard.infoReadonly")}</Caption>
                              <Stack direction="row" spacing={2}>
                                <Caption>{t("models.col.model")}</Caption>
                                <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                                  {row.modelId}
                                </Typography>
                              </Stack>
                              <Stack direction="row" spacing={2}>
                                <Caption>{t("models.col.upstream")}</Caption>
                                <Typography variant="body1">
                                  {row.upstreamModel === "" ? DASH : row.upstreamModel}
                                </Typography>
                              </Stack>
                              <Stack direction="row" spacing={2}>
                                <Caption>{t("models.col.channels")}</Caption>
                                <Typography variant="body1">
                                  {formatInt(row.channelCount)}
                                </Typography>
                              </Stack>
                            </Stack>
                          ) : null}
                        </Stack>
                      );
                    })
                  )}
                </Stack>
              </Stack>

              {/* ---------------- 扫描结果 ---------------- */}
              <Stack spacing={2}>
                <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                  <Button
                    variant="outlined"
                    data-testid="pw-scan"
                    disabled={scanPending}
                    onClick={runScan}
                  >
                    {t("providers.wizard.scan")}
                  </Button>
                  {/* 只有真的在等后端时才说"正在扫描"（无假进度） */}
                  {scanPending ? <Caption>{t("providers.wizard.scanning")}</Caption> : null}
                  {/* 有真实结果才有"全选 / 添加选中"可点（没有结果时它们没有意义，所以不出现） */}
                  {!scanPending && preview !== undefined && preview.models.length > 0 ? (
                    <>
                      <Button
                        variant="outlined"
                        data-testid="pw-scan-select-all"
                        onClick={() => setSelection(selectAllIds(preview))}
                      >
                        {t("providers.wizard.selectAll")}
                      </Button>
                      <Button
                        variant="contained"
                        data-testid="pw-scan-add"
                        onClick={applySelected}
                      >
                        {t("providers.wizard.addSelected")}
                      </Button>
                    </>
                  ) : null}
                  <Button variant="outlined" data-testid="pw-finish" onClick={close}>
                    {t("providers.wizard.finish")}
                  </Button>
                </Stack>

                {needsProvider ? (
                  <Typography variant="body1" data-testid="pw-scan-error" sx={{ color: "error.main" }}>
                    {t("providers.wizard.scanNeedsProvider")}
                  </Typography>
                ) : scanError !== undefined && scanError !== null ? (
                  <Typography variant="body1" data-testid="pw-scan-error" sx={{ color: "error.main" }}>
                    {errorMessage(scanError)}
                  </Typography>
                ) : preview === undefined ? null : preview.models.length === 0 ? (
                  /* 上游确实一个都没返回：如实说，不编数据、不给假进度 */
                  <Typography variant="body1" data-testid="pw-scan-empty">
                    {t("providers.wizard.scanEmpty")}
                  </Typography>
                ) : (
                  <Stack spacing={2} data-testid="pw-scan-result">
                    {preview.skipped.length > 0 ? (
                      <Caption>
                        {t("providers.wizard.scanSkipped", { count: preview.skipped.length })}
                      </Caption>
                    ) : null}
                    {appliedCount !== undefined ? (
                      <Typography variant="body1">
                        {t("providers.wizard.addedCount", { count: appliedCount })}
                      </Typography>
                    ) : null}
                    {/*
                      后端说这次真新建了几条线就显示几条（0 也如实显示：那就是"一条都没接上"）。
                      `channels_created` 是"这次真的接上了几条线"的**唯一**真话 —— "加了模型"与
                      "接上了线路"是两件事（[docs/20 §0] 的教训正是点了添加、上面什么都没有）。
                    */}
                    {channelsCreated !== undefined ? (
                      <Typography variant="body1" data-testid="pw-channels-created">
                        {t("providers.wizard.channelsCreated", { count: channelsCreated })}
                      </Typography>
                    ) : null}
                    {applyError !== undefined && applyError !== null ? (
                      <Typography
                        variant="body1"
                        data-testid="pw-scan-error"
                        sx={{ color: "error.main" }}
                      >
                        {errorMessage(applyError)}
                      </Typography>
                    ) : null}

                    {preview.models.map((model) => (
                      <Stack
                        key={model.suggested_id}
                        direction="row"
                        spacing={2}
                        alignItems="center"
                        sx={{ flexWrap: "wrap" }}
                        data-testid="pw-scan-row"
                      >
                        <Stack data-testid="pw-scan-check">
                          <Checkbox
                            size="small"
                            checked={selection.includes(model.suggested_id)}
                            /* 已存在的不能重复添加：行上写着"已存在"，这就是它不可勾的原因 */
                            disabled={model.exists}
                            onChange={(event) =>
                              setSelection((prev) =>
                                event.target.checked
                                  ? [...prev, model.suggested_id]
                                  : prev.filter((id) => id !== model.suggested_id),
                              )
                            }
                          />
                        </Stack>
                        <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                          {model.upstream_model}
                        </Typography>
                        <Typography variant="body1" sx={{ color: "text.secondary" }}>
                          {model.suggested_id}
                        </Typography>
                        {model.exists ? (
                          <Caption>{t("providers.wizard.alreadyExists")}</Caption>
                        ) : null}
                      </Stack>
                    ))}
                  </Stack>
                )}
              </Stack>
            </Stack>
          </Box>
        )}
      </DialogContent>

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
        ) : null}
      </DialogActions>
    </Dialog>
  );
}

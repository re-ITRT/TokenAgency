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
import type { ImportPreview, ProbeResult } from "@/shared/bindings";
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
  laneChoices,
  lanePatch,
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
  const [needsProvider, setNeedsProvider] = useState(false);
  const [selection, setSelection] = useState<readonly string[]>([]);
  const [infoOpen, setInfoOpen] = useState<string | undefined>(undefined);

  // ---- 只读数据（第 2 步才需要，所以按步开启）

  const matrix = useCommandQuery("channel_matrix", undefined, qk.channelMatrix, {
    enabled: open && step === 2,
  });
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials, {
    enabled: open && step === 2,
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
  const bulkUpdate = useCommandMutation("model_bulk_update");

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
      await upsertCredential.mutateAsync({
        credential: defaultCredentialPayload(landedId, draft.name),
      });
      const keyWritten = draft.key !== "";
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
      void queryClient.invalidateQueries({ queryKey: qk.providers });
      void queryClient.invalidateQueries({ queryKey: qk.credentials });
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
    void (async () => {
      try {
        const result = await api.model.applyImport(preview.preview_token, [...selection]);
        // 只有后端说 applied 才算加了；条数是"我们提交了几条"，不是编出来的数字
        setAppliedCount(result.applied ? selection.length : undefined);
        void queryClient.invalidateQueries({ queryKey: qk.channelMatrix });
        void queryClient.invalidateQueries({ queryKey: qk.models });
      } catch (err) {
        setApplyError(err);
      }
    })();
  };

  const changeLane = (modelId: string, value: string): void => {
    bulkUpdate.mutate(
      { ids: [modelId], patch: { lane: lanePatch(value) } },
      { onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.channelMatrix }) },
    );
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
  const laneOptionList = useMemo(
    () => laneChoices(laneOptions.data ?? [], t("network.directLabel")),
    [laneOptions.data, t],
  );

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
        {step === 1 ? t("providers.wizard.step1Title") : t("providers.wizard.step2Title")}
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
              options={PROTOCOLS.map((protocol) => ({
                value: protocol,
                label: t(PROTOCOL_LABEL[protocol] ?? "providers.protocol.custom"),
              }))}
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
                <Typography variant="h4">{t("providers.wizard.addedModels")}</Typography>
                <Stack spacing={2} data-testid="pw-added-models">
                  {addedModels.length === 0 ? (
                    <Typography variant="body1" data-testid="pw-added-models-empty">
                      {t("providers.wizard.addedModelsEmpty")}
                    </Typography>
                  ) : (
                    addedModels.map((row) => {
                      /**
                       * 该行现在走哪条线：① channel 自己的 lane（后端在用的真值）→
                       * ② 模型级默认 lane（改过的值存这里）→ ③ 第 1 步/`net.default_lane`
                       * （都没设就是 `null` = 直连）。
                       */
                      const lane = row.laneId ?? modelDefaultLanes.get(row.modelId) ?? defaultLane;
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

                            {/* 这一行唯一可改的东西：网络代理（[docs/19 §1.2]） */}
                            <Box sx={{ minWidth: 240 }}>
                              <SelectField
                                testId="pw-model-lane"
                                label={t("providers.wizard.modelLane")}
                                value={laneValue(lane)}
                                onChange={(value) => changeLane(row.modelId, value)}
                                options={laneOptionList}
                                emptyLabel={t("network.directLabel")}
                              />
                            </Box>
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

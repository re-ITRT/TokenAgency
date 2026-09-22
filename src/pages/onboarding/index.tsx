/**
 * 首启向导页 `/onboarding`（[05 §2.4] 四步 · [12 §6 F4]）。
 *
 * ## 三条命令都已注册，这一页**真调**它们
 *
 * `onboarding_detect` / `onboarding_test_all` / `onboarding_apply` 都在
 * `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 里（实现见 `commands/settings_ext.rs`），
 * 所以这一页做的是真流程，而不是"等后端"：
 *
 * 1. **探测**：进页就调 `onboarding_detect`，把后端返回的三项（内核找到没有、是否已有配置、
 *    建议的服务商）逐行显示；失败就显示失败原因，**不填默认值、不编一份建议清单**。
 *    （契约里 `onboarding_detect` 的 TS 类型只有这三项，Rust 侧还序列化了
 *    `kernel_checked` / `kernel_path` —— 前端拿不到，所以不显示它们，也不猜。）
 * 2. **测试**：点「开始测试」真的调 `onboarding_test_all`，逐条显示 `ConnectivityRow`
 *    （通过/失败、延迟、状态码、后端 `note` 原文，含 `ok` 的口径）；命令报错（例如没装配内核）
 *    显示错误原文 —— **"没有结果"与"全部失败"是两件事**。
 * 3. **应用**：点「应用这份配置」真的调 `onboarding_apply`，回显 `ApplyResult` 的**逐项事实**：
 *    写入了没有、是否需要重启、是否需要重载内核。**不许把"需要重启"说成"已完成"**。
 * 4. **跳过**：跳过只留下一句"没有写入任何配置"，页面上不会出现"已配置"这种话。
 *
 * ## 仍然保留的诚实纪律
 *
 * - **预览与提交同源**：第 4 步打印的就是 `buildOnboardingInput` 算出来的那份 payload。
 * - **本地校验与后端校验不互相替代**：本地按行给行号级错误，后端校验是另一层。
 * - **接入信息是真读的**：`app_clipboard_access_info`（已注册）。
 * - 不使用 router（本页可以被单独渲染）；i18n 之外不碰共享文件。
 */

import {
  Box,
  Button,
  Checkbox,
  FormControlLabel,
  MenuItem,
  Paper,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useMemo, useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import { PageHeader } from "@/shared/components";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  PROTOCOLS,
  applyFacts,
  buildOnboardingInput,
  credentialEntries,
  credentialLines,
  detectFacts,
  laneCandidates,
  nextProviderRow,
  nextSubRow,
  payloadJson,
  testMatrix,
  validateProviders,
  validateSubscriptions,
  type ApplyFact,
  type ProviderRow,
  type SubRow,
} from "./model";

/** 四步（[05 §2.4] 的三步 + 接入信息）。 */
const STEP_TOTAL = 4;

const STEP_TITLE: Record<number, I18nKey> = {
  1: "onboarding.step1",
  2: "onboarding.step2",
  3: "onboarding.step3",
  4: "onboarding.step4",
};

const SECTION_SX = { p: 4, borderRadius: `${radius.surface}px` } as const;

/** `ApplyResult` 的每一项事实 → i18n key（UI 只渲染事实，不做"已完成"这类翻译）。 */
const APPLY_FACT_TEXT: Record<ApplyFact, I18nKey> = {
  applied: "onboarding.apply.applied",
  notApplied: "onboarding.apply.notApplied",
  restart: "onboarding.apply.restart",
  kernelReload: "onboarding.apply.kernelReload",
  effective: "onboarding.apply.effective",
};

/**
 * 事实 → `data-testid`。
 * `applied` / `notApplied` 共用同一个 id（它们是同一格的两种取值，不可能同时出现），
 * 用 `data-applied` 区分 —— 测试据此断言"后端到底写没写"。
 */
const APPLY_FACT_TESTID: Record<ApplyFact, string> = {
  applied: "onboarding-apply-applied",
  notApplied: "onboarding-apply-applied",
  restart: "onboarding-apply-restart",
  kernelReload: "onboarding-apply-kernel-reload",
  effective: "onboarding-apply-effective",
};

/** 等宽小字：打印会被机器读的内容（payload、接入信息、标识符）。 */
const MONO_PRE_SX = {
  m: 0,
  p: 3,
  bgcolor: "background.default",
  borderRadius: `${radius.control}px`,
  border: "1px solid",
  borderColor: "divider",
  fontFamily: typography.fontMono,
  fontSize: typography.size.meta,
  lineHeight: typography.lineHeight.table,
  whiteSpace: "pre-wrap",
  overflowWrap: "anywhere",
  maxHeight: 320,
  overflow: "auto",
} as const;

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary", display: "block" }}>
      {children}
    </Typography>
  );
}

/** 带 `data-testid` 的输入框：测试从这个容器里取唯一的 textbox。 */
function Field({
  testId,
  label,
  value,
  onChange,
  placeholder,
  multiline = false,
  hint,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  multiline?: boolean;
  hint?: string;
}) {
  return (
    <Box data-testid={testId}>
      <Stack spacing={1}>
        <TextField
          fullWidth
          label={label}
          value={value}
          placeholder={placeholder}
          multiline={multiline}
          onChange={(event) => onChange(event.target.value)}
        />
        {hint === undefined ? null : <Caption>{hint}</Caption>}
      </Stack>
    </Box>
  );
}

/** 一行的本地问题（行号 + 文案）。 */
function IssueList({ testId, title, issues }: { testId: string; title: string; issues: ReactNode }) {
  return (
    <Box data-testid={testId}>
      <Stack spacing={1}>
        <Typography variant="body1" sx={{ color: "warning.main" }}>
          {title}
        </Typography>
        {issues}
      </Stack>
    </Box>
  );
}

// ================================================================ 行编辑器

function ProviderEditor({
  index,
  row,
  onChange,
  onRemove,
}: {
  index: number;
  row: ProviderRow;
  onChange: (next: ProviderRow) => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  const lines = credentialLines(row.secret).length;

  return (
    <Paper variant="outlined" sx={{ p: 3, borderRadius: `${radius.control}px` }}>
      <Stack spacing={3}>
        <Box
          sx={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))",
            gap: 3,
          }}
        >
          <Field
            testId={`onboarding-provider-field-${index}-id`}
            label="id"
            value={row.id}
            hint={t("onboarding.issue.id")}
            onChange={(id) => onChange({ ...row, id })}
          />
          <Field
            testId={`onboarding-provider-field-${index}-name`}
            label={t("onboarding.provider.name")}
            value={row.name}
            onChange={(name) => onChange({ ...row, name })}
          />
          <Box data-testid={`onboarding-provider-field-${index}-protocol`}>
            <TextField
              select
              fullWidth
              label={t("onboarding.provider.protocol")}
              value={row.protocol}
              onChange={(event) =>
                onChange({ ...row, protocol: event.target.value as ProviderRow["protocol"] })
              }
            >
              {PROTOCOLS.map((protocol) => (
                <MenuItem key={protocol} value={protocol}>
                  {protocol}
                </MenuItem>
              ))}
            </TextField>
          </Box>
          <Field
            testId={`onboarding-provider-field-${index}-baseUrl`}
            label={t("onboarding.provider.baseUrl")}
            value={row.base_url}
            placeholder="https://api.example.com/v1"
            onChange={(base_url) => onChange({ ...row, base_url })}
          />
        </Box>

        <Field
          testId={`onboarding-provider-field-${index}-secret`}
          label={t("onboarding.provider.secret")}
          value={row.secret}
          multiline
          hint={t("onboarding.provider.secret.hint")}
          onChange={(secret) => onChange({ ...row, secret })}
        />

        {/* 展开说明：0 行 = 不会进提交内容；多行 = 会变成多个条目 */}
        {lines === 0 ? (
          <Caption>{t("onboarding.provider.expandNone")}</Caption>
        ) : lines > 1 ? (
          <Caption>{t("onboarding.provider.expand", { count: lines })}</Caption>
        ) : null}

        <Box>
          <Button data-testid={`onboarding-provider-remove-${index}`} variant="outlined" onClick={onRemove}>
            {t("onboarding.provider.remove")}
          </Button>
        </Box>
      </Stack>
    </Paper>
  );
}

function SubEditor({
  index,
  row,
  onChange,
  onRemove,
}: {
  index: number;
  row: SubRow;
  onChange: (next: SubRow) => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();

  return (
    <Paper variant="outlined" sx={{ p: 3, borderRadius: `${radius.control}px` }}>
      <Stack spacing={3}>
        <Field
          testId={`onboarding-sub-field-${index}-name`}
          label={t("onboarding.sub.name")}
          value={row.name}
          onChange={(name) => onChange({ ...row, name })}
        />
        <Field
          testId={`onboarding-sub-field-${index}-url`}
          label={t("onboarding.sub.url")}
          value={row.url}
          placeholder="https://example.com/api/v1/client/subscribe?token=..."
          onChange={(url) => onChange({ ...row, url })}
        />
        <Box>
          <Button data-testid={`onboarding-sub-remove-${index}`} variant="outlined" onClick={onRemove}>
            {t("onboarding.sub.remove")}
          </Button>
        </Box>
      </Stack>
    </Paper>
  );
}

// ================================================================ 页面

export default function OnboardingPage() {
  const { t } = useTranslation();

  const [step, setStep] = useState(1);
  const [providers, setProviders] = useState<ProviderRow[]>([]);
  const [subs, setSubs] = useState<SubRow[]>([]);
  const [noVpn, setNoVpn] = useState(false);
  const [kernelPath, setKernelPath] = useState("");
  const [accessOpen, setAccessOpen] = useState(false);
  const [copyNote, setCopyNote] = useState<"done" | "unavailable" | undefined>(undefined);
  /** 「跳过向导」是一个**明确选择**：跳过之后不会再提交任何东西。 */
  const [skipped, setSkipped] = useState(false);

  /** 线路候选来自**当前配置**（只读）；接入信息来自后端原文。 */
  const config = useCommandQuery("config_get", undefined, qk.config);
  const access = useCommandQuery("app_clipboard_access_info", undefined, qk.clipboard, {
    enabled: accessOpen,
  });
  /** 环境探测：`onboarding_detect`（已注册）。失败就显示失败原因。 */
  const detect = useCommandQuery("onboarding_detect", undefined, ["onboarding", "detect"]);
  /** 两条会改后端状态的命令：**只在用户点按钮时**调用，绝不自动触发。 */
  const tests = useCommandMutation("onboarding_test_all");
  const apply = useCommandMutation("onboarding_apply");

  const detectResult = detect.data === undefined ? undefined : detectFacts(detect.data);

  const providerIssues = useMemo(() => validateProviders(providers), [providers]);
  const subIssues = useMemo(() => validateSubscriptions(subs), [subs]);
  const entries = useMemo(() => credentialEntries(providers), [providers]);
  const lanes = useMemo(() => laneCandidates(config.data), [config.data]);
  const matrix = useMemo(() => testMatrix(entries, lanes), [entries, lanes]);

  const input = useMemo(
    () => buildOnboardingInput({ kernelPath, providers, subscriptions: subs, noVpn }),
    [kernelPath, providers, subs, noVpn],
  );

  // 每一步"能不能继续"：没有输入不算填错（可以先去下一步，回头再配）
  const step1Ok = providers.length === 0 || providerIssues.length === 0;
  const step2Ok = noVpn || (subs.length > 0 && subIssues.length === 0);
  /** 第 3 步没有可点的测试（命令未注册），所以它不拦路。 */
  const canContinue = step === 1 ? step1Ok : step === 2 ? step2Ok : true;

  const updateProvider = (index: number, next: ProviderRow) =>
    setProviders((prev) => prev.map((row, i) => (i === index ? next : row)));
  const updateSub = (index: number, next: SubRow) =>
    setSubs((prev) => prev.map((row, i) => (i === index ? next : row)));

  /** 跳过：清掉"已经发生过"的调用结果，页面上只留一句"没有写入任何配置"。 */
  const onSkip = () => {
    setSkipped(true);
    tests.reset();
    apply.reset();
  };
  const onResume = () => setSkipped(false);

  const onCopyAccess = () => {
    const text = access.data ?? "";
    const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
    if (clipboard === undefined || typeof clipboard.writeText !== "function") {
      setCopyNote("unavailable");
      return;
    }
    void clipboard.writeText(text).then(
      () => setCopyNote("done"),
      () => setCopyNote("unavailable"),
    );
  };

  return (
    <>
      <PageHeader
        title={t("nav.onboarding")}
        description={t("onboarding.description")}
        docRef="05 §2.4 首启向导 · 10 §2.2 onboarding 命令 · 06 §10.1 B2 额度提醒"
      />

      <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
        {/* ① 进度：四步的名字与当前位置（不是可点的步骤条：跳步会绕过校验） */}
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3}>
            <Typography variant="caption" sx={{ color: "text.secondary" }}>
              {t("onboarding.step", { n: step, total: STEP_TOTAL })}
            </Typography>
            <Box sx={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
              {[1, 2, 3, 4].map((n) => (
                <Typography
                  key={n}
                  variant="body1"
                  sx={{ color: n === step ? "text.primary" : "text.secondary" }}
                >
                  {`${n}. ${t(STEP_TITLE[n] ?? "onboarding.step1")}`}
                </Typography>
              ))}
            </Box>
            <Typography variant="h4" component="h2">
              {t(STEP_TITLE[step] ?? "onboarding.step1")}
            </Typography>
          </Stack>
        </Paper>

        {/* ② 当前这一步 */}
        <Paper variant="outlined" sx={SECTION_SX}>
          <Box data-testid="onboarding-step-panel" data-step={String(step)}>
            {step === 1 ? (
              <Stack spacing={4}>
                {/* 环境探测：每一行都是 onboarding_detect 的返回，不是前端猜的 */}
                <Stack spacing={1}>
                  <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
                    <Button
                      data-testid="onboarding-detect-run"
                      variant="outlined"
                      disabled={detect.isFetching}
                      onClick={() => void detect.refetch()}
                    >
                      {t("onboarding.detect.retry")}
                    </Button>
                    <Caption>{detect.isFetching ? t("common.loading") : t("onboarding.detect.hint")}</Caption>
                  </Box>

                  {detect.error !== null && detect.error !== undefined ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-detect-error"
                      sx={{ color: "error.main" }}
                    >
                      {t("onboarding.detect.error", { message: errorMessage(detect.error) })}
                    </Typography>
                  ) : detectResult === undefined ? (
                    <Typography variant="body1" sx={{ color: "text.secondary" }}>
                      {t("common.loading")}
                    </Typography>
                  ) : (
                    <Stack spacing={1}>
                      <Typography
                        variant="body1"
                        data-testid="onboarding-detect-kernel"
                        data-value={detectResult.kernel}
                      >
                        {detectResult.kernel === "found"
                          ? t("onboarding.detect.kernelFound")
                          : t("onboarding.detect.kernelMissing")}
                      </Typography>
                      <Typography
                        variant="body1"
                        data-testid="onboarding-detect-config"
                        data-value={detectResult.config}
                      >
                        {detectResult.config === "existing"
                          ? t("onboarding.detect.configExisting")
                          : t("onboarding.detect.configMissing")}
                      </Typography>
                      <Typography variant="body1" data-testid="onboarding-detect-suggested">
                        {detectResult.suggested.length === 0
                          ? t("onboarding.detect.suggestedNone")
                          : t("onboarding.detect.suggested", {
                              list: detectResult.suggested.join(" · "),
                            })}
                      </Typography>
                    </Stack>
                  )}
                </Stack>

                <Box>
                  <Stack spacing={1}>
                    <Field
                      testId="onboarding-field-kernelPath"
                      label={t("onboarding.kernelPath")}
                      value={kernelPath}
                      hint={t("onboarding.kernelPath.hint")}
                      onChange={setKernelPath}
                    />
                  </Stack>
                </Box>

                <Typography variant="h4" component="h3">
                  {t("onboarding.provider.title")}
                </Typography>
                {providers.length === 0 ? <Caption>{t("onboarding.provider.empty")}</Caption> : null}
                {providers.map((row, index) => (
                  <ProviderEditor
                    key={`${row.id}-${index}`}
                    index={index}
                    row={row}
                    onChange={(next) => updateProvider(index, next)}
                    onRemove={() => setProviders((prev) => prev.filter((_, i) => i !== index))}
                  />
                ))}
                <Box>
                  <Button
                    data-testid="onboarding-provider-add"
                    variant="outlined"
                    onClick={() => setProviders((prev) => [...prev, nextProviderRow(prev)])}
                  >
                    {t("onboarding.provider.add")}
                  </Button>
                </Box>

                {providerIssues.length > 0 ? (
                  <IssueList
                    testId="onboarding-step1-issues"
                    title={t("onboarding.step1.issues", { count: providerIssues.length })}
                    issues={providerIssues.map((issue, index) => (
                      <Caption key={`${issue.row}-${issue.path}-${index}`}>
                        {`${issue.row + 1}. ${t(issue.key)}`}
                      </Caption>
                    ))}
                  />
                ) : null}
              </Stack>
            ) : null}

            {step === 2 ? (
              <Stack spacing={4}>
                <Typography variant="h4" component="h3">
                  {t("onboarding.sub.title")}
                </Typography>
                {subs.length === 0 ? <Caption>{t("onboarding.sub.empty")}</Caption> : null}
                {subs.map((row, index) => (
                  <SubEditor
                    key={`${row.id}-${index}`}
                    index={index}
                    row={row}
                    onChange={(next) => updateSub(index, next)}
                    onRemove={() => setSubs((prev) => prev.filter((_, i) => i !== index))}
                  />
                ))}
                <Box>
                  <Button
                    data-testid="onboarding-sub-add"
                    variant="outlined"
                    onClick={() => setSubs((prev) => [...prev, nextSubRow(prev)])}
                  >
                    {t("onboarding.sub.add")}
                  </Button>
                </Box>

                <Box data-testid="onboarding-no-vpn">
                  <Stack spacing={1}>
                    <FormControlLabel
                      control={
                        <Checkbox
                          checked={noVpn}
                          onChange={(event) => setNoVpn(event.target.checked)}
                        />
                      }
                      label={t("onboarding.noVpn")}
                    />
                    <Typography variant="caption" data-testid="onboarding-no-vpn-hint" sx={{ color: "text.secondary" }}>
                      {t("onboarding.noVpn.hint")}
                    </Typography>
                  </Stack>
                </Box>

                {/* 勾了「不用梯子」之后，订阅那一行的错误不再拦路（那几行不会被提交） */}
                {subIssues.length > 0 && !noVpn ? (
                  <IssueList
                    testId="onboarding-step2-issues"
                    title={t("onboarding.step2.issues", { count: subIssues.length })}
                    issues={subIssues.map((issue, index) => (
                      <Caption key={`${issue.row}-${issue.path}-${index}`}>
                        {`${issue.row + 1}. ${t(issue.key)}`}
                      </Caption>
                    ))}
                  />
                ) : null}
              </Stack>
            ) : null}

            {step === 3 ? (
              <Stack spacing={4}>
                <Typography variant="h4" component="h3">
                  {t("onboarding.test.title")}
                </Typography>
                <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
                  {skipped ? null : (
                    <Button
                      data-testid="onboarding-test-run"
                      variant="contained"
                      disabled={tests.isPending}
                      onClick={() => tests.mutate(undefined)}
                    >
                      {tests.isPending ? t("common.loading") : t("onboarding.test.run")}
                    </Button>
                  )}
                  <Caption>{t("onboarding.test.blocked")}</Caption>
                </Box>

                <Stack spacing={1}>
                  <Caption>{t("onboarding.test.matrix")}</Caption>
                  {matrix.length === 0 ? (
                    <Caption>{t("onboarding.test.matrixEmpty")}</Caption>
                  ) : (
                    matrix.map((row) => (
                      <Typography
                        key={`${row.credentialId}-${row.lane}`}
                        variant="body1"
                        data-testid="onboarding-test-matrix-row"
                        sx={{ fontFamily: typography.fontMono }}
                      >
                        {t("onboarding.test.matrixRow", { credential: row.credential, lane: row.lane })}
                      </Typography>
                    ))
                  )}
                  <Caption>
                    {lanes.length === 0
                      ? t("onboarding.test.lanesNone")
                      : t("onboarding.test.lanes", { lanes: lanes.join(" · ") })}
                  </Caption>
                </Stack>

                <Stack spacing={1}>
                  <Typography variant="h4" component="h3">
                    {t("onboarding.test.resultTitle")}
                  </Typography>
                  {/* 没有结果 != 全部失败：这句话必须留着 */}
                  {skipped ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-test-result"
                      sx={{ color: "text.secondary" }}
                    >
                      {t("onboarding.skip.skippedTest")}
                    </Typography>
                  ) : tests.error !== null && tests.error !== undefined ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-test-error"
                      sx={{ color: "error.main" }}
                    >
                      {t("onboarding.test.error", { message: errorMessage(tests.error) })}
                    </Typography>
                  ) : tests.data === undefined ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-test-result"
                      sx={{ color: "text.secondary" }}
                    >
                      {t("onboarding.test.resultNone")}
                    </Typography>
                  ) : (
                    <Stack spacing={1} data-testid="onboarding-test-result">
                      {tests.data.length === 0 ? (
                        <Caption>{t("onboarding.test.resultEmpty")}</Caption>
                      ) : null}
                      {tests.data.map((row, index) => (
                        <Box
                          key={`${row.credential_id}-${row.lane_id}-${index}`}
                          data-testid="onboarding-test-row"
                          data-ok={String(row.ok)}
                        >
                          <Stack spacing={1}>
                            <Typography variant="body1" sx={{ fontFamily: typography.fontMono }}>
                              {`${row.credential_id} × ${row.lane_id} · ${
                                row.ok ? t("onboarding.test.ok") : t("onboarding.test.fail")
                              }${
                                row.latency_ms === null || row.latency_ms === undefined
                                  ? ""
                                  : ` · ${row.latency_ms} ms`
                              }${
                                row.status === null || row.status === undefined
                                  ? ""
                                  : ` · HTTP ${row.status}`
                              }`}
                            </Typography>
                            {row.note === null || row.note === undefined ? null : (
                              <Caption>{t("onboarding.test.note", { note: row.note })}</Caption>
                            )}
                          </Stack>
                        </Box>
                      ))}
                    </Stack>
                  )}
                </Stack>
              </Stack>
            ) : null}

            {step === 4 ? (
              <Stack spacing={4}>
                <Typography variant="h4" component="h3">
                  {t("onboarding.apply.title")}
                </Typography>
                <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
                  {skipped ? null : (
                    <Button
                      data-testid="onboarding-apply-submit"
                      variant="contained"
                      disabled={apply.isPending}
                      onClick={() => apply.mutate({ input })}
                    >
                      {apply.isPending ? t("common.loading") : t("onboarding.apply.submit")}
                    </Button>
                  )}
                  <Caption>{t("onboarding.apply.blocked")}</Caption>
                </Box>

                <Stack spacing={1}>
                  <Caption>{t("onboarding.apply.payload")}</Caption>
                  <Box component="pre" data-testid="onboarding-apply-payload" sx={MONO_PRE_SX}>
                    {payloadJson(input)}
                  </Box>
                  <Typography variant="body1" data-testid="onboarding-apply-summary">
                    {t("onboarding.apply.summary", {
                      providers: input.providers.length,
                      subscriptions: input.subscriptions.length,
                    })}
                  </Typography>
                </Stack>

                <Stack spacing={1}>
                  <Typography variant="h4" component="h3">
                    {t("onboarding.apply.resultTitle")}
                  </Typography>
                  {skipped ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-apply-result"
                      sx={{ color: "text.secondary" }}
                    >
                      {t("onboarding.skip.skippedApply")}
                    </Typography>
                  ) : apply.error !== null && apply.error !== undefined ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-apply-error"
                      sx={{ color: "error.main" }}
                    >
                      {t("onboarding.apply.error", { message: errorMessage(apply.error) })}
                    </Typography>
                  ) : apply.data === undefined ? (
                    <Typography
                      variant="body1"
                      data-testid="onboarding-apply-result"
                      sx={{ color: "text.secondary" }}
                    >
                      {t("onboarding.apply.resultNone")}
                    </Typography>
                  ) : (
                    // 逐项事实：写入了没有 / 要不要重启 / 要不要重载内核 —— 各说各的
                    <Stack spacing={1} data-testid="onboarding-apply-result">
                      {applyFacts(apply.data).map((fact) => (
                        <Typography
                          key={fact}
                          variant="body1"
                          data-testid={APPLY_FACT_TESTID[fact]}
                          data-applied={
                            fact === "applied" || fact === "notApplied"
                              ? String(apply.data?.applied ?? false)
                              : undefined
                          }
                          sx={{
                            color:
                              fact === "restart" || fact === "kernelReload"
                                ? "warning.main"
                                : fact === "notApplied"
                                  ? "error.main"
                                  : "text.primary",
                          }}
                        >
                          {t(APPLY_FACT_TEXT[fact])}
                        </Typography>
                      ))}
                      <Caption>{t("onboarding.apply.raw")}</Caption>
                      <Box component="pre" sx={MONO_PRE_SX} data-testid="onboarding-apply-raw">
                        {JSON.stringify(apply.data, null, 2)}
                      </Box>
                    </Stack>
                  )}
                </Stack>
              </Stack>
            ) : null}
          </Box>

          {/* ③ 导航：第 4 步没有"下一步"（没有第 5 步）；跳过是一个明确选择 */}
          <Box sx={{ display: "flex", gap: 3, alignItems: "center", mt: 6, flexWrap: "wrap" }}>
            {step > 1 ? (
              <Button data-testid="onboarding-prev" variant="outlined" onClick={() => setStep(step - 1)}>
                {t("onboarding.prev")}
              </Button>
            ) : null}
            {step < STEP_TOTAL ? (
              <Button
                data-testid="onboarding-next"
                variant="contained"
                disabled={!canContinue}
                onClick={() => setStep(step + 1)}
              >
                {t("onboarding.next")}
              </Button>
            ) : null}
            {skipped ? (
              <Button data-testid="onboarding-skip-resume" variant="outlined" onClick={onResume}>
                {t("onboarding.skip.resume")}
              </Button>
            ) : (
              <Button data-testid="onboarding-skip" variant="outlined" onClick={onSkip}>
                {t("onboarding.skip")}
              </Button>
            )}
          </Box>
          {skipped ? (
            <Typography
              variant="body1"
              data-testid="onboarding-skip-note"
              sx={{ color: "text.secondary", mt: 3 }}
            >
              {t("onboarding.skip.done")}
            </Typography>
          ) : null}
        </Paper>

        {/* ④ 接入信息：**任何一步都能看**（它来自当前配置，与向导是否提交无关） */}
        <Paper variant="outlined" sx={SECTION_SX}>
          <Stack spacing={3}>
            <Typography variant="h4" component="h3">
              {t("onboarding.access.title")}
            </Typography>
            <Typography variant="body1" data-testid="onboarding-access-hint" sx={{ color: "text.secondary" }}>
              {t("onboarding.access.hint")}
            </Typography>

            <Box sx={{ display: "flex", gap: 3, flexWrap: "wrap", alignItems: "center" }}>
              <Button
                data-testid={accessOpen ? "onboarding-access-hide" : "onboarding-access-show"}
                variant="outlined"
                onClick={() => {
                  setCopyNote(undefined);
                  setAccessOpen((prev) => !prev);
                }}
              >
                {accessOpen ? t("onboarding.access.hide") : t("onboarding.access.show")}
              </Button>
              {accessOpen && access.data !== undefined ? (
                <Button data-testid="onboarding-access-copy" variant="outlined" onClick={onCopyAccess}>
                  {t("onboarding.access.copy")}
                </Button>
              ) : null}
            </Box>

            {accessOpen ? (
              access.error !== null && access.error !== undefined ? (
                <Typography variant="body1" data-testid="onboarding-access-error" sx={{ color: "error.main" }}>
                  {t("onboarding.access.failed", { message: errorMessage(access.error) })}
                </Typography>
              ) : access.data === undefined ? (
                <Typography variant="body1" sx={{ color: "text.secondary" }}>
                  {t("common.loading")}
                </Typography>
              ) : (
                <Stack spacing={1}>
                  <Box component="pre" data-testid="onboarding-access-output" sx={MONO_PRE_SX}>
                    {access.data}
                  </Box>
                  {copyNote === undefined ? null : (
                    <Caption>
                      {copyNote === "done"
                        ? t("onboarding.access.copy.done")
                        : t("onboarding.access.copy.unavailable")}
                    </Caption>
                  )}
                </Stack>
              )
            ) : null}

            <Caption>{t("onboarding.access.clients")}</Caption>
          </Stack>
        </Paper>
      </Box>
    </>
  );
}

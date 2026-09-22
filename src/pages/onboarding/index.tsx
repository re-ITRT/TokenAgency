/**
 * 首启向导页 `/onboarding`（[05 §2.4] 四步 · [12 §6 F4]）。
 *
 * ## 这一页要解决的真问题：**后端三条命令都还没注册**
 *
 * `onboarding_detect` / `onboarding_test_all` / `onboarding_apply` 在
 * `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 里都没有 —— 真机上调用就是失败。
 * 所以这一版**不做**下面这些事（做了就是骗人）：
 *
 * - 不放「开始测试」按钮（`onboarding_test_all` 未注册）：点了不会成功的东西比没有按钮更糟。
 * - 不放「生成配置」按钮（`onboarding_apply` 未注册）。
 * - 不显示"建议的服务商"或"内核已找到"（那要 `onboarding_detect` 才知道）。
 * - 不显示任何测试结果：**没有结果**与**全部失败**是两件不同的事，结果区必须写清楚是前者。
 *
 * ## 那这一版做了什么（都不是空壳）
 *
 * 1. **四步骨架 + 真导航**：上一步 / 下一步，每步的"能不能继续"由模型层的本地校验决定
 *    （[05 §2.4] 的 Step 1/2/3 + 接入信息）。填错就卡住，并逐行给出行号级的原因。
 * 2. **真校验**（`./model`）：标识非空且唯一、名称非空、接口地址必须 http(s)、密钥非空；
 *    订阅链接同理。「我不用梯子」是一个**明确选择**，勾上后订阅列表就是空的。
 * 3. **真展开**：一行粘贴多个密钥 → 展开成多个凭据条目（提交契约每个条目只带一个密钥）。
 * 4. **真预览**：第 4 步把**将来会发给 `onboarding_apply` 的那份 payload** 原样打印出来
 *    （与将来真正提交用同一个 `buildOnboardingInput`，预览不会与真实请求分叉）。
 * 5. **真读接入信息**：`app_clipboard_access_info`（**已注册**）—— 不是拼出来的假地址。
 *
 * 另外两条纪律：不使用 router（本页可以被单独渲染），也不碰 i18n 之外的共享文件。
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

import { errorMessage, qk, useCommandQuery } from "@/shared/api";
import { PageHeader } from "@/shared/components";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  PROTOCOLS,
  buildOnboardingInput,
  credentialEntries,
  credentialLines,
  laneCandidates,
  nextProviderRow,
  nextSubRow,
  payloadJson,
  testMatrix,
  validateProviders,
  validateSubscriptions,
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

  /** 线路候选来自**当前配置**（只读）；接入信息来自后端原文。 */
  const config = useCommandQuery("config_get", undefined, qk.config);
  const access = useCommandQuery("app_clipboard_access_info", undefined, qk.clipboard, {
    enabled: accessOpen,
  });

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
                {/* 环境检测没有命令：如实说，并让用户手填内核路径 */}
                <Typography
                  variant="body1"
                  data-testid="onboarding-detect-note"
                  sx={{ color: "text.secondary" }}
                >
                  {t("onboarding.detect.blocked")}
                </Typography>
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
                <Typography variant="body1" data-testid="onboarding-test-blocked" sx={{ color: "warning.main" }}>
                  {t("onboarding.test.blocked")}
                </Typography>

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
                  <Typography variant="body1" data-testid="onboarding-test-result" sx={{ color: "text.secondary" }}>
                    {t("onboarding.test.resultNone")}
                  </Typography>
                </Stack>
              </Stack>
            ) : null}

            {step === 4 ? (
              <Stack spacing={4}>
                <Typography variant="h4" component="h3">
                  {t("onboarding.apply.title")}
                </Typography>
                <Typography variant="body1" data-testid="onboarding-apply-blocked" sx={{ color: "warning.main" }}>
                  {t("onboarding.apply.blocked")}
                </Typography>

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
                  <Typography variant="body1" data-testid="onboarding-apply-result" sx={{ color: "text.secondary" }}>
                    {t("onboarding.apply.resultNone")}
                  </Typography>
                </Stack>
              </Stack>
            ) : null}
          </Box>

          {/* ③ 导航：第 4 步没有"下一步"（没有第 5 步），也不放提交按钮 */}
          <Box sx={{ display: "flex", gap: 3, alignItems: "center", mt: 6 }}>
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
          </Box>
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

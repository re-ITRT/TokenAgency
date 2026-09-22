/**
 * `/providers` 的两个添加向导（[docs/17 §3.1]）：**添加服务商** 与 **添加密钥**。
 *
 * 单独成文件（不在 `index.tsx` 里）的理由：这两个对话框是**唯一会写配置的入口**，
 * 它们的"说什么、发什么、什么时候清空"值得单独看清楚；列表页只管读。
 *
 * 硬纪律（写在这里，免得下一个人再犯）：
 *
 * 1. **只调已注册的命令**：`provider_upsert` / `credential_upsert` / `credential_set_secret`
 *    都在 `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 里
 *    （`src/app/commandSurface.guard.test.ts` 机械检查）。
 * 2. **本地预检不过就一次都不发**（`./wizard.ts` 的 `validate*`）。
 * 3. **回显的是后端返回值**（`mutation.data`），不是我们提交的草稿 —— 后端可能改字段。
 * 4. **明文只进密钥仓库**：`credential.secret` 只是 `${secret:<id>}` 引用；
 *    用户填的明文只出现在 `credential_set_secret({ id, plaintext })` 的入参里，
 *    不进日志、不进错误消息，成功后**就地清空输入框**。
 * 5. `data-testid` 挂在**外层元素**上（MUI 的 `TextField` / `Select` / `Checkbox` 不吃它，
 *    会 TS2353）；真机 E2E 与单测都用 `[data-testid="X"] input` 命中同一形状。
 */

import {
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  MenuItem,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";

import { errorMessage, qk, useCommandMutation } from "@/shared/api";
import type { ProviderView } from "@/shared/bindings";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { typography } from "@/theme";

import { PROTOCOLS } from "./protocols";
import {
  AUTH_SCHEMES,
  EMPTY_CREDENTIAL_DRAFT,
  EMPTY_PROVIDER_DRAFT,
  QUOTA_WINDOWS,
  credentialPayload,
  providerPayload,
  sanitizeAmount,
  validateCredentialDraft,
  validateProviderDraft,
  type CredentialDraft,
  type ProviderDraft,
  type WizardIssue,
} from "./wizard";

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 协议 → 界面语言（与 `index.tsx` / `detail.tsx` 同一张表：线上值不漏给用户）。 */
const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

const AUTH_SCHEME_LABEL: Record<string, I18nKey> = {
  bearer: "providers.authScheme.bearer",
  header: "providers.authScheme.header",
  query: "providers.authScheme.query",
  none: "providers.authScheme.none",
};

const WINDOW_LABEL: Record<string, I18nKey> = {
  hour: "providers.quota.window.hour",
  day: "providers.quota.window.day",
  month: "providers.quota.window.month",
  none: "providers.quota.window.none",
};

/** 本地预检点名哪个字段时用的标题（文案全部复用既有键）。 */
const FIELD_LABEL: Record<string, I18nKey> = {
  id: "providers.field.id",
  name: "providers.field.name",
  baseUrl: "providers.field.baseUrl",
  label: "providers.credentials.col.label",
  provider: "channels.add.provider",
};

// ================================================================ 小组件

function Caption({ children }: { children: ReactNode }) {
  return (
    <Typography variant="caption" sx={{ color: "text.secondary" }}>
      {children}
    </Typography>
  );
}

/**
 * 一个下拉字段。`data-testid` 挂在**容器**上（与线路页 `SelectField` 同一形状）。
 * 值为空时 `renderValue` 显示「必填」：**空就是真的空**，不放"请选择…"的假选项。
 */
function SelectField({
  testId,
  label,
  value,
  onChange,
  options,
  emptyLabel,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
}) {
  const labelOf = (id: string): string => options.find((o) => o.value === id)?.label ?? id;
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        label={label}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        slotProps={{
          select: {
            displayEmpty: true,
            renderValue: (selected: unknown) =>
              (selected as string) === "" ? emptyLabel : labelOf(selected as string),
          },
        }}
      >
        {options.map((option) => (
          <MenuItem key={option.value} value={option.value}>
            {option.label}
          </MenuItem>
        ))}
      </TextField>
    </Stack>
  );
}

/** 本地预检的错：逐个点名字段（文案用既有键，不新造句子）。 */
function Issues({ testId, issues }: { testId: string; issues: readonly WizardIssue[] }) {
  const { t } = useTranslation();
  if (issues.length === 0) return null;
  return (
    <Stack spacing={1} data-testid={testId}>
      {issues.map((issue) => (
        <Typography
          key={`${issue.field}-${issue.message}`}
          variant="body1"
          sx={{ color: "error.main" }}
        >
          {`${t(FIELD_LABEL[issue.field] ?? "common.unknown")}：${t(issue.message)}`}
        </Typography>
      ))}
    </Stack>
  );
}

// ================================================================ 添加服务商

export type ProviderAddDialogProps = {
  open: boolean;
  /** 现有 `provider_list` 的 id：本地去重用（[docs/17 §3.1]）。 */
  existingIds: readonly string[];
  onClose: () => void;
};

/**
 * 「添加服务商」向导。保存只做一件事：`provider_upsert({ provider })`。
 *
 * **没有 API Key 输入框**：密钥属于凭据域（见「添加密钥」）。这里只配"这家服务商怎么连"
 * （id / 名称 / 协议 / 地址 / 鉴权方式），凭据由另一个向导写。
 */
export function ProviderAddDialog({ open, existingIds, onClose }: ProviderAddDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const upsert = useCommandMutation("provider_upsert");

  const [draft, setDraft] = useState<ProviderDraft>(EMPTY_PROVIDER_DRAFT);
  const [issues, setIssues] = useState<readonly WizardIssue[]>([]);

  const set = <K extends keyof ProviderDraft>(key: K, value: ProviderDraft[K]): void =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  /** 关掉 = 回到干净草稿（下次打开不带着上一次的残值与旧回显）。 */
  const close = (): void => {
    setDraft(EMPTY_PROVIDER_DRAFT);
    setIssues([]);
    upsert.reset();
    onClose();
  };

  const submit = (): void => {
    const found = validateProviderDraft(draft, existingIds);
    setIssues(found);
    // 本地预检不过 → **一次都不发**
    if (found.length > 0) return;
    upsert.reset();
    upsert.mutate(
      { provider: providerPayload(draft) },
      {
        onSuccess: () => {
          setDraft(EMPTY_PROVIDER_DRAFT);
          setIssues([]);
          void queryClient.invalidateQueries({ queryKey: qk.providers });
        },
      },
    );
  };

  /** 回显的是**后端返回的那个 `Provider`**（不是我们提交的草稿）。 */
  const saved = upsert.data;

  return (
    <Dialog open={open} onClose={close} fullWidth maxWidth="sm">
      <DialogTitle>{t("providers.add.title")}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          <Stack spacing={1} data-testid="providers-add-id">
            <TextField
              label={t("providers.field.id")}
              value={draft.id}
              onChange={(event) => set("id", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          <Stack spacing={1} data-testid="providers-add-name">
            <TextField
              label={t("providers.field.name")}
              value={draft.name}
              onChange={(event) => set("name", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          {/* 协议清单来自 `PROTOCOLS`（被 `protocolList.guard.test.ts` 钉着，不另编一份） */}
          <SelectField
            testId="providers-add-protocol"
            label={t("providers.field.protocol")}
            value={draft.protocol}
            onChange={(value) => set("protocol", value as ProviderDraft["protocol"])}
            options={PROTOCOLS.map((protocol) => ({
              value: protocol,
              label: t(PROTOCOL_LABEL[protocol] ?? "providers.protocol.custom"),
            }))}
            emptyLabel={t("wizard.required")}
          />

          <Stack spacing={1} data-testid="providers-add-baseUrl">
            <TextField
              label={t("providers.field.baseUrl")}
              value={draft.baseUrl}
              onChange={(event) => set("baseUrl", event.target.value)}
              size="small"
              fullWidth
              slotProps={{ htmlInput: { inputMode: "url" } }}
            />
          </Stack>

          <SelectField
            testId="providers-add-authScheme"
            label={t("providers.field.authScheme")}
            value={draft.authScheme}
            onChange={(value) => set("authScheme", value as ProviderDraft["authScheme"])}
            options={AUTH_SCHEMES.map((scheme) => ({
              value: scheme,
              label: t(AUTH_SCHEME_LABEL[scheme] ?? "providers.authScheme.bearer"),
            }))}
            emptyLabel={t("wizard.required")}
          />

          {/* 头名 / 值前缀：只在这两个里真的填了东西（或选了非默认方式）时才随载荷发出 */}
          <Stack spacing={1} data-testid="providers-add-authHeader">
            <TextField
              label={t("providers.field.authHeader")}
              value={draft.authHeader}
              onChange={(event) => set("authHeader", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          <Stack spacing={1} data-testid="providers-add-authPrefix">
            <TextField
              label={t("providers.field.authPrefix")}
              value={draft.authPrefix}
              onChange={(event) => set("authPrefix", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          <Caption>{t("providers.honesty.fields")}</Caption>

          <Issues testId="providers-add-issues" issues={issues} />

          {/* 后端的错原样显示（TA 码 + 字段路径），本地预检不替代它 */}
          {upsert.error !== null ? (
            <Typography variant="body1" data-testid="providers-add-error" sx={{ color: "error.main" }}>
              {errorMessage(upsert.error)}
            </Typography>
          ) : null}

          {/* 成功：显示**后端返回值**（不是草稿） */}
          {saved !== undefined ? (
            <Stack spacing={1} data-testid="providers-add-result">
              <Typography variant="body1">{t("wizard.saved", { id: saved.id })}</Typography>
              <Typography
                variant="body1"
                sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
              >
                {[
                  saved.name,
                  PROTOCOL_LABEL[saved.protocol] !== undefined
                    ? t(PROTOCOL_LABEL[saved.protocol]!)
                    : saved.protocol,
                  saved.base_url,
                ].join(" · ")}
              </Typography>
            </Stack>
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={close}>{t("common.cancel")}</Button>
        <Button
          variant="contained"
          data-testid="providers-add-submit"
          disabled={upsert.isPending}
          onClick={submit}
        >
          {t("common.save")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

// ================================================================ 添加密钥

export type CredentialAddDialogProps = {
  open: boolean;
  /** `provider_list`：服务商下拉的选项（`credential.provider` 是一个外键）。 */
  providers: readonly ProviderView[];
  /** 现有 `credential_list` 的 id：本地去重用。 */
  existingIds: readonly string[];
  onClose: () => void;
};

/**
 * 「添加密钥」向导。保存**两步**（[docs/17 §3.1]）：
 *
 * 1. `credential_upsert({ credential })` —— 落配置，`secret` 是 `${secret:<id>}` **引用**；
 * 2. **只有用户真的填了明文**，才 `credential_set_secret({ id, plaintext })` 把值写进密钥仓库。
 *
 * 所以明文只进密钥仓库：载荷里没有它、日志里没有它、错误消息里没有它，
 * 成功后输入框立刻清空（失败时**保留**，用户还要重试）。
 */
export function CredentialAddDialog({
  open,
  providers,
  existingIds,
  onClose,
}: CredentialAddDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const upsert = useCommandMutation("credential_upsert");
  const setSecret = useCommandMutation("credential_set_secret");

  const [draft, setDraft] = useState<CredentialDraft>(EMPTY_CREDENTIAL_DRAFT);
  const [issues, setIssues] = useState<readonly WizardIssue[]>([]);

  const set = <K extends keyof CredentialDraft>(key: K, value: CredentialDraft[K]): void =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const close = (): void => {
    setDraft(EMPTY_CREDENTIAL_DRAFT);
    setIssues([]);
    upsert.reset();
    setSecret.reset();
    onClose();
  };

  const submit = (): void => {
    const found = validateCredentialDraft(draft, existingIds);
    setIssues(found);
    if (found.length > 0) return;

    // 明文先取出来（下面会清空草稿）：它只进 `credential_set_secret`
    const plaintext = draft.secret;
    upsert.reset();
    setSecret.reset();
    upsert.mutate(
      { credential: credentialPayload(draft) },
      {
        onSuccess: (saved) => {
          void queryClient.invalidateQueries({ queryKey: qk.credentials });
          if (plaintext === "") {
            // 没填密钥：只建凭据（引用已经写好），密钥留待以后单独写
            setDraft(EMPTY_CREDENTIAL_DRAFT);
            return;
          }
          // 用**后端返回的 id** 写密钥：写的是后端真的落库的那一条
          setSecret.mutate(
            { id: saved.id, plaintext },
            { onSuccess: () => setDraft(EMPTY_CREDENTIAL_DRAFT) },
          );
        },
      },
    );
  };

  const saved = upsert.data;
  /** 两个 mutation 只可能有一个错：写密钥只在建凭据成功后才发。 */
  const failure = upsert.error ?? setSecret.error;

  return (
    <Dialog open={open} onClose={close} fullWidth maxWidth="sm">
      <DialogTitle>{t("credentials.add.title")}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          <Stack spacing={1} data-testid="credentials-add-id">
            <TextField
              label={t("providers.field.id")}
              value={draft.id}
              onChange={(event) => set("id", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          <Stack spacing={1} data-testid="credentials-add-label">
            <TextField
              label={t("providers.credentials.col.label")}
              value={draft.label}
              onChange={(event) => set("label", event.target.value)}
              size="small"
              fullWidth
            />
          </Stack>

          <SelectField
            testId="credentials-add-provider"
            label={t("channels.add.provider")}
            value={draft.provider}
            onChange={(value) => set("provider", value)}
            options={providers.map((provider) => ({ value: provider.id, label: provider.name }))}
            emptyLabel={t("wizard.required")}
          />

          {/* 明文输入：type=password，**永不回显**（成功后清空） */}
          <Stack spacing={1} data-testid="credentials-add-secret">
            <TextField
              type="password"
              label={t("credentials.add.secret")}
              value={draft.secret}
              onChange={(event) => set("secret", event.target.value)}
              size="small"
              fullWidth
              autoComplete="new-password"
            />
          </Stack>

          {/*
            额度块。上限 / 币种 / 窗口三个控件的字段名在 i18n 里**没有现成的键**
            （[docs/17 §3.0] 只落了 `credentials.add.secret`），所以这里：
            · 用既有的 `providers.credentials.col.quota`（「额度」）当**分组标题**；
            · 上限输入框用它当字段标题；币种用 ISO 码做 placeholder（`CNY` 是代码不是文案）；
            · 窗口下拉直接显示选中的窗口文案（每小时/每天/每月/不重置）。
            缺的键已报给主管，补上后把这里的 placeholder 换成正式 label。
          */}
          <Stack spacing={1}>
            <Caption>{t("providers.credentials.col.quota")}</Caption>
            <Stack data-testid="credentials-add-quotaLimit">
              <TextField
                label={t("providers.credentials.col.quota")}
                value={draft.quotaLimit}
                /* 输入即净化：`Number()` 拿不到 NaN，本地就不需要"必须是数字"的新文案 */
                onChange={(event) => set("quotaLimit", sanitizeAmount(event.target.value))}
                size="small"
                fullWidth
                slotProps={{ htmlInput: { inputMode: "decimal" } }}
              />
            </Stack>
            <Box sx={{ display: "flex", gap: 2 }}>
              <Stack sx={{ flex: 1 }} data-testid="credentials-add-currency">
                <TextField
                  placeholder="CNY"
                  value={draft.currency}
                  onChange={(event) => set("currency", event.target.value)}
                  size="small"
                  fullWidth
                />
              </Stack>
              <Stack sx={{ flex: 1 }} data-testid="credentials-add-window">
                <TextField
                  select
                  size="small"
                  fullWidth
                  value={draft.window}
                  onChange={(event) => set("window", event.target.value as CredentialDraft["window"])}
                  slotProps={{
                    select: {
                      renderValue: (selected: unknown) =>
                        t(WINDOW_LABEL[selected as string] ?? "providers.quota.window.month"),
                    },
                  }}
                >
                  {QUOTA_WINDOWS.map((window) => (
                    <MenuItem key={window} value={window}>
                      {t(WINDOW_LABEL[window] ?? "providers.quota.window.month")}
                    </MenuItem>
                  ))}
                </TextField>
              </Stack>
            </Box>
          </Stack>

          <Issues testId="credentials-add-issues" issues={issues} />

          {/* 后端的错原样显示（TA 码），本地预检不替代它；**错误里不会有明文** */}
          {failure !== null ? (
            <Typography variant="body1" data-testid="credentials-add-error" sx={{ color: "error.main" }}>
              {errorMessage(failure)}
            </Typography>
          ) : null}

          {/* 成功：显示**后端返回的那个 Credential** */}
          {saved !== undefined ? (
            <Stack spacing={1} data-testid="credentials-add-result">
              <Typography variant="body1">{t("wizard.saved", { id: saved.id })}</Typography>
              <Typography
                variant="body1"
                sx={{ fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
              >
                {[saved.label, saved.provider, saved.secret].join(" · ")}
              </Typography>
            </Stack>
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={close}>{t("common.cancel")}</Button>
        <Button
          variant="contained"
          data-testid="credentials-add-submit"
          disabled={upsert.isPending || setSecret.isPending}
          onClick={submit}
        >
          {t("common.save")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

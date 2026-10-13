/**
 * `/providers` 的「高级添加」对话框：**添加服务商**（单步、字段全）。
 *
 * 单独成文件（不在 `index.tsx` 里）的理由：它是列表页唯一会写**服务商**的对话框，
 * 它的"说什么、发什么、什么时候清空"值得单独看清楚；列表页只管读。
 *
 * **这里原来还有第二个对话框「添加密钥」**（一次建一份凭据，带额度上限 / 币种 / 窗口）——
 * 第 80 轮已**整条删除**（硬规则 3：**一个服务商一把密钥**）。留着它就是
 * "给一个已存在的服务商再挂一份凭据"的活入口，正是 owner 明确反对的那个模型：
 * 要两个账号 → **再加一条服务商、取不同名字**（提示文案 `providers.key.multipleHint`）。
 * 密钥的写入口现在只有两处 —— 两步向导第 1 步（`./providerWizard.tsx` 的 `pw-key`）
 * 与详情页「密钥」块（`./detail.tsx` 的 `providers-edit-secret`）。
 *
 * 硬纪律（写在这里，免得下一个人再犯）：
 *
 * 1. **只调已注册的命令**：`provider_upsert` 在 `src-tauri/src/commands/mod.rs` 的
 *    `collect_commands!` 里（`src/app/commandSurface.guard.test.ts` 机械检查）。
 * 2. **本地预检不过就一次都不发**（`./wizard.ts` 的 `validateProviderDraft`）。
 * 3. **回显的是后端返回值**（`mutation.data`），不是我们提交的草稿 —— 后端可能改字段。
 * 4. **名称是用户区分"同一家的不同账号"的抓手**（硬规则 3）：重名在本地就挡住
 *    （对现有 `name` 去重），不等到配置里出现两条长得一样的记录；后端错误原文仍原样显示。
 * 5. `data-testid` 挂在**外层元素**上（MUI 的 `TextField` / `Select` / `Checkbox` 不吃它，
 *    会 TS2353）；真机 E2E 与单测都用 `[data-testid="X"] input` 命中同一形状。
 */

import {
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
import { useState } from "react";

import { errorMessage, qk, useCommandMutation } from "@/shared/api";
import { Caption, HelpTip } from "@/shared/components";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { typography } from "@/theme";

import { PROTOCOLS } from "./protocols";
import {
  AUTH_SCHEMES,
  EMPTY_PROVIDER_DRAFT,
  providerPayload,
  validateProviderDraft,
  type ExistingProviderRefs,
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

/** 本地预检点名哪个字段时用的标题（文案全部复用既有键）。 */
export const FIELD_LABEL: Record<string, I18nKey> = {
  id: "providers.field.id",
  name: "providers.field.name",
  baseUrl: "providers.field.baseUrl",
  label: "providers.credentials.col.label",
  provider: "channels.add.provider",
  /** 两步向导把密钥也放在步骤 1，所以这里也要点得出它的名字。 */
  key: "onboarding.provider.secret",
};

// ================================================================ 小组件


/**
 * 一个下拉字段。`data-testid` 挂在**容器**上（与线路页 `SelectField` 同一形状）。
 * 值为空时 `renderValue` 显示「必填」：**空就是真的空**，不放"请选择…"的假选项。
 *
 * 导出给两步向导（`./providerWizard.tsx`）复用：两个向导的下拉必须是同一个形状，
 * 否则真机 E2E 按 `[data-testid="X"] [role="combobox"]` 命中的行为会不一样。
 */
export function SelectField({
  testId,
  label,
  value,
  onChange,
  options,
  emptyLabel,
  labelHidden = false,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: readonly { value: string; label: string }[];
  emptyLabel: string;
  /**
   * **极简**：列表里的重复标签不画在每一行上，改由**列头**说一次。
   *
   * 这时 `label` 仍然必须给：它变成 `aria-label`（无障碍名称一个字不少），
   * 只是不占版面。别把"看不见"做成"读不到"。
   */
  labelHidden?: boolean;
}) {
  const labelOf = (id: string): string => options.find((o) => o.value === id)?.label ?? id;
  return (
    <Stack spacing={1} data-testid={testId}>
      <TextField
        select
        size="small"
        fullWidth
        label={labelHidden ? undefined : label}
        aria-label={labelHidden ? label : undefined}
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
  /**
   * 现有 `provider_list` 的 id **与显示名称**：本地去重用（+）。
   *
   * 名称也要去重：`name` 是用户区分"同一家的不同账号"的抓手（"我要两个账号就再加一条服务商、
   * 取不同名字"），两条同名记录在界面上根本分不出谁是谁 —— 所以本地就挡住，不发命令。
   */
  existing: ExistingProviderRefs;
  onClose: () => void;
};

/**
 * 「添加服务商」向导。保存只做一件事：`provider_upsert({ provider })`。
 *
 * **没有 API Key 输入框**：密钥属于凭据域，而凭据的写入口只有两处（两步向导第 1 步与详情页
 * 「密钥」块）—— 这里只配"这家服务商怎么连"（id / 名称 / 协议 / 地址 / 鉴权方式）。
 */
export function ProviderAddDialog({ open, existing, onClose }: ProviderAddDialogProps) {
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
    const found = validateProviderDraft(draft, existing);
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

          {/*
            「这些字段来自哪个视图」是**解释**，不是结论 → 收进问号圆圈。
            它与两个单步对话框（添加服务商 / 添加密钥）共用，所以测试 id 取 `providers-add-help`。
            这里**不留内联正文**：用户要的正是"介绍收进圆圈"，正文在 `aria-label` 与气泡里一个字不少。
          */}
          <Caption>
            <HelpTip text={t("providers.honesty.fields")} testId="providers-add-help" />
          </Caption>

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

// ================================================================ 凭据：这里没有向导了
//
// 第 80 轮删掉的 `CredentialAddDialog`（「添加密钥」）不会再回来（硬规则 3：
// **一个服务商一把密钥**；要两个账号 → 再加一条服务商、取不同名字，见 `providers.key.multipleHint`）。
// 它当年做的是"给一个**已存在**的服务商再挂一份凭据（带自己的额度上限/币种/窗口）"——
// 正是 owner 反对的那个模型，也是"多份凭据 → 多个删除入口"的源头。
//
// 凭据的写入口现在只有两处（都不在这个文件里）：
// · 两步向导第 1 步（`./providerWizard.tsx` 的 `pw-key` → `credential_set_secret`，
//   凭据 id 恒为 `<provider>-default`，一个服务商就这一把）；
// · 详情页「密钥」块（`./detail.tsx` 的 `providers-edit-secret`，留空 = 不改）。
// 额度窗口挂在**那一把**凭据上，编辑口在详情页的 `providers-quota-windows`。

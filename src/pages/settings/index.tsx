/**
 * 设置页 `/settings`（[07 §7]、[09 §1.2] E1–E5）—— 用户要求"不许再有占位页面"，所以这一页是真页面。
 *
        </Box>
 * 三条纪律（这一页最容易违反，所以写在最上面）：
 *
 * 1. **I2：只展示系统网络信息，绝不提供"改系统设置"的入口。**
 *    TUN / 系统代理在这一页是**只读文字**，`net.tun` / `net.system_proxy` /
 *    `system_integration.enable_tun|enable_system_proxy|autostart` 连"写"都写不进去
 *    （白名单 + `setPath()` 抛错，见 `model.ts`）。要改请到网络页。
 * 2. **没有的命令不画按钮，也不画"点了没反应"的禁用占位。**
 *    （以下命令**都已注册**在 `src-tauri/src/commands/mod.rs` 的 `collect_commands!` 里，
 *    见 `crates/ta-core/tests/command_surface_guard.rs`；"没画"是这一页还没接，不是命令不存在。）
 *    - 开机自启动（app_get_autostart / app_set_autostart / app_repair_autostart）→ **已接**：
 *      开关写的是**系统事实**（`actual`），配置里的期望值只作对照；`drifted` 时才给"重新对齐"。
 *    - 检查更新（app_check_update）→ **已接**：点了才发请求，失败如实报错，
 *      只有在后端明确返回"没有新版本"（`null`）时才说没有新版本。
 *    - 日志导出（log_export）→ 这一页没有导出按钮：导出在日志页（/logs），那里才是它该在的地方。
 *    - 文件监听模式（config_set_file_watch_mode）→ **已接**：写的是配置，
 *      而后端明确回 `restart_required: true`（监听器本身还没实现）→ 只说"需要重启"，绝不说"已生效"。
 * 3. **不许假成功**：`config_validate` 只有显式 `ok: true` 才允许继续；
 *    `config_apply` 只有显式 `applied: true` 才提示"已写入"。读不出来的返回按"未通过"处理。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("config_get")`             → 服务/启动/托盘/记录/高级 各节的**真值来源**
 * - `useCommandQuery("app_get_autostart")`      → 自启动的**系统事实**（`actual`）与漂移判断
 * - `useCommandMutation("app_set_autostart" | "app_repair_autostart")` → 改系统 / 重新对齐
 * - `useCommandMutation("app_check_update")`    → 检查更新（点了才发）
 * - `useCommandMutation("config_validate")`     → 权威校验（本地预检只先挡明显非法的输入）
 * - `useCommandMutation("config_apply")`        → 写配置
 * - `useCommandMutation("config_export_yaml" | "config_import_yaml" | "config_rollback")`
 * - `useCommandQuery("config_history_list")`    → 快照列表
 * - `useCommandQuery("log_storage_stats" | "log_query")` → 占用 + "最近一行记录时的模式"（可读证据）
 * - `useCommandMutation("log_set_mode" | "log_set_retention" | "log_clear")`
 * - `useCommandMutation("app_open_dir" | "app_pause_gateway" | "app_resume_gateway"
 *   | "app_clipboard_access_info")`
 * - `useAppStatus()`                            → 关于节的版本/内核/运行时长
 */

import {
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  MenuItem,
  Paper,
  Radio,
  RadioGroup,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Typography,
  Switch,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import {
  errorMessage,
  qk,
  useAppStatus,
  useCommandMutation,
  useCommandQuery,
} from "@/shared/api";
import type {
  AutostartMode,
  ClearMode,
  ConfigDoc,
  ImageHandling,
  LogMode,
  Retention,
} from "@/shared/bindings";
import { SECTION_SX, Caption, AutoEllipsis, PageHeader, StatItem, StatusDot, HelpTip } from "@/shared/components";
// [docs/52] 「区块说明放哪儿」的唯一判定（原先本文件自己写了一份，阈值是裸字面量 12）
import { sectionHelpMode } from "@/shared/helpTip";
import { DASH, formatBytes, formatDuration, formatInt, formatLocalTime } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { useAppStore } from "@/stores/app";
import { radius, typography } from "@/theme";

import {
  SECRET_STATE_KEY,
  cloneDoc,
  diffPaths,
  getPath,
  isForbidden,
  readApplyResult,
  readNewestRow,
  readSnapshots,
  readStorageStats,
  readValidationReport,
  recentLogQueryArgs,
  secretState,
  setPath,
  validateDraft,
  type ApplyView,
  type LocalIssue,
  type ReportView,
  type SnapshotView,
} from "./model";

// ================================================================ 常量与文案映射


/** 数值缺失一律画"—"（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */

const LOG_MODES: readonly LogMode[] = ["off", "meta", "shape", "full"];

const MODE_DESC: Record<LogMode, I18nKey> = {
  off: "logs.mode.off.desc",
  meta: "logs.mode.meta.desc",
  shape: "logs.mode.shape.desc",
  full: "logs.mode.full.desc",
};

const RETENTIONS: readonly Retention[] = ["7d", "30d", "90d", "forever"];

const RETENTION_LABEL: Record<Retention, I18nKey> = {
  "7d": "logs.retention.7d",
  "30d": "logs.retention.30d",
  "90d": "logs.retention.90d",
  forever: "logs.retention.forever",
};

const AUTOSTART_MODES: readonly { value: AutostartMode; labelKey: I18nKey }[] = [
  { value: "tray", labelKey: "settings.startup.mode.tray" },
  { value: "window", labelKey: "settings.startup.mode.window" },
];

const IMAGE_HANDLINGS: readonly { value: ImageHandling; labelKey: I18nKey }[] = [
  { value: "hash", labelKey: "settings.image.hash" },
  { value: "full", labelKey: "settings.image.full" },
];

/** 浏览器/宿主剪贴板。**返回布尔**：写不进去要如实说，不能报"已复制"。 */
async function writeClipboard(text: string): Promise<boolean> {
  if (typeof navigator === "undefined" || navigator.clipboard === undefined) return false;
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

// ================================================================ 小组件


// [docs/52] 统计格收敛到共享件（原先本文件自己写了一份）

/** 只读一行：标签 + 值。**只读块里不放任何控件**（I2 的机械检查点）。 */
function ReadonlyRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <Box sx={{ display: "flex", alignItems: "baseline", gap: 3, minWidth: 0 }}>
      <Typography
        variant="caption"
        sx={{ color: "text.secondary", minWidth: 160, flex: "0 0 auto" }}
      >
        {label}
      </Typography>
      <Typography variant="body1" sx={{ minWidth: 0, overflowWrap: "anywhere" }}>
        {value}
      </Typography>
    </Box>
  );
}

/**
 * 区块说明的呈现方式（[docs/22 §2]）。
 *
 * [docs/52] 收敛到 `@/shared/helpTip`，这里**保留再导出**：`settings.test.tsx`
 * 是 `import SettingsPage, { sectionHelpMode } from "@/pages/settings"` 这样取的，
 * 直接删掉会让那条测试编译失败。实现只有一份（阈值不再各写一遍）。
 */
export { sectionHelpMode } from "@/shared/helpTip";

/**
 * 一个区块（[docs/22 §2] 的"区块说明"）。
 *
 * `descriptionKey` 是**这一段是干什么的**那一句：> 12 字符收进标题右边的问号圆圈
 * （`testId` 就是 `<区块 testid>-help`，§3），≤ 12 字符留在版面上。
 * **一个区块最多一个问号**（§3）—— 一排问号比一排字更糟；所以另外还想收进来的长解释
 * 走 `helpTip` **并进同一个问号**，而不是再画一个。
 * §2 的红线（读不到 / 未读取 / 校验失败 / 破坏性后果 / 数值）一句都不许收。
 */
function SectionPaper({
  titleKey,
  testId,
  descriptionKey,
  helpTip,
  children,
}: {
  titleKey: I18nKey;
  testId: string;
  /** 区块说明的 i18n 键（不传就不占位）。 */
  descriptionKey?: I18nKey;
  /**
   * 调用点自己给的问号（与 `descriptionKey` 二选一，给了就用它）。
   *
   * 为什么需要这条路：本组件把键 **join 成一段文本**再交给 `<HelpTip>`，
   * 而"哪些键只在问号里出现"这种检查只能看到**字面量**（键名必须直接写在 `<HelpTip … />` 里）。
   * 就一两句、看一眼就懂的区块（如服务区块）把 `t("键")` 直接写在问号元素里，
   * 读者不必再跳到本组件的 `parts.join` 去反推，检查器也不至于把它们误当成版面文案。
   * 几段解释要并进**同一个问号**时，在这里 `join("\n")`（`HelpTip` 的气泡 `whiteSpace: pre-line`，
   * 仓库里同样的做法见 `network/index.tsx` 的 `mergeHelpText`）—— 收进来的句子一个字都不许丢：
   * 全文仍是那个问号的 `aria-label`。
   */
  helpTip?: ReactNode;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const description = descriptionKey === undefined ? "" : t(descriptionKey);
  return (
    <Paper variant="outlined" sx={SECTION_SX} data-testid={testId}>
      <Stack spacing={4}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
          <Typography variant="h4">{t(titleKey)}</Typography>
          {helpTip ??
            (description === "" ? null : sectionHelpMode(description) === "help" ? (
              <HelpTip text={description} testId={`${testId}-help`} />
            ) : (
              <Caption>{description}</Caption>
            ))}
        </Box>
        {children}
      </Stack>
    </Paper>
  );
}

type EditableFieldProps = {
  testId: string;
  labelKey: I18nKey;
  path: readonly string[];
  /** 当前草稿（还没读出来 → 显示空并禁用）。 */
  doc: ConfigDoc | undefined;
  onChange: (path: readonly string[], value: unknown) => void;
  kind?: "text" | "int" | "bool" | "select";
  options?: readonly { value: string; labelKey: I18nKey }[];
  hintKey?: I18nKey;
  step?: number;
};

/**
 * 一个可改字段。**路径必须来自 `EDITABLE_PATHS`**（`onChange` 最终走 `setPath`，白名单外会抛错）。
 * 值的读法用 `getPath`（宽容：缺字段显示空而不是编一个 0）。
 */
function EditableField({
  testId,
  labelKey,
  path,
  doc,
  onChange,
  kind = "text",
  options,
  hintKey,
  step,
}: EditableFieldProps) {
  const { t } = useTranslation();
  const raw = doc === undefined ? undefined : getPath(doc, path);
  const label = t(labelKey);

  return (
    <Stack spacing={1} data-testid={testId} sx={{ minWidth: 0 }}>
      {kind === "bool" ? (
        <FormControlLabel
          control={
            <Checkbox
              size="small"
              disabled={doc === undefined}
              checked={raw === true}
              onChange={(e) => onChange(path, e.target.checked)}
            />
          }
          label={label}
        />
      ) : kind === "select" ? (
        <TextField
          select
          label={label}
          disabled={doc === undefined}
          value={typeof raw === "string" ? raw : ""}
          onChange={(e) => onChange(path, e.target.value)}
        >
          {(options ?? []).map((option) => (
            <MenuItem key={option.value} value={option.value}>
              {t(option.labelKey)}
            </MenuItem>
          ))}
        </TextField>
      ) : (
        <TextField
          label={label}
          disabled={doc === undefined}
          value={raw === undefined || raw === null ? "" : String(raw)}
          slotProps={{ htmlInput: kind === "int" ? { inputMode: "numeric", step } : undefined }}
          onChange={(e) => {
            if (kind === "int") {
              const n = Number(e.target.value);
              if (Number.isFinite(n)) onChange(path, Math.trunc(n));
              return;
            }
            onChange(path, e.target.value);
          }}
        />
      )}
      {hintKey !== undefined ? <Caption>{t(hintKey)}</Caption> : null}
    </Stack>
  );
}

// ================================================================ 页面

export default function SettingsPage() {
  const { t } = useTranslation();
  const pushToast = useAppStore((s) => s.pushToast);

  const status = useAppStatus();
  const queryClient = useQueryClient();
  /**
   * 这一份 doc 是**整份**写回去的（`config_apply` 是全量覆盖，见
   * `src-tauri/src/commands/config.rs`）—— 所以编辑它之前必须拿到后端**当前**的那一份。
   *
   * 为什么这里显式写 `refetchOnMount: "always"`（两道保险的第二道）：
   * `qk.config` 是全局共享缓存（`staleTime: 5_000`，`src/main.tsx`）。第一道保险是
   * `useCommandMutation` 在**每个写命令成功后**集中失效 `qk.config`（见
   * `src/shared/api/queries.ts`）—— 本该兜底的 `config:changed` 事件在生产代码里从未发出过
   * （只有 `gateway.rs` 的测试里构造过 `TaEvent::ConfigChanged`），所以那道集中失效是必需品。
   * 第二道就是这里：设置页是**唯一会把整份 doc 写回去**的页面，它的正确性不能依赖
   * "别处有没有记得失效" —— 每次挂载都重读一次后端当前值。
   * 真机 E2E 实测到过被抹掉：`lane/ta-e2e-lane` 在点「校验并应用」前后消失。
   */
  const config = useCommandQuery("config_get", undefined, qk.config, { refetchOnMount: "always" });
  const history = useCommandQuery("config_history_list", { limit: 20 }, qk.configHistory);
  const stats = useCommandQuery("log_storage_stats", undefined, qk.logStats);
  const recentArgs = useMemo(() => recentLogQueryArgs(1), []);
  const recent = useCommandQuery("log_query", recentArgs, qk.logs("settings-recent"));

  const validate = useCommandMutation("config_validate");
  const applyConfig = useCommandMutation("config_apply");
  const exportYaml = useCommandMutation("config_export_yaml");
  const importYaml = useCommandMutation("config_import_yaml");
  const rollback = useCommandMutation("config_rollback");
  const saveMode = useCommandMutation("log_set_mode");
  const saveRetention = useCommandMutation("log_set_retention");
  const clearLogs = useCommandMutation("log_clear");
  const openDir = useCommandMutation("app_open_dir");
  const pauseGateway = useCommandMutation("app_pause_gateway");
  const resumeGateway = useCommandMutation("app_resume_gateway");
  const accessInfo = useCommandMutation("app_clipboard_access_info");
  /** 系统自启动状态（`actual` 才是系统事实；`desired` 只是配置期望）。 */
  const autostart = useCommandQuery("app_get_autostart", undefined, qk.autostart);
  const setAutostart = useCommandMutation("app_set_autostart", {
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.autostart }),
  });
  const repairAutostart = useCommandMutation("app_repair_autostart", {
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.autostart }),
  });
  /** 检查更新：点了真的调后端；失败就显示失败原因。 */
  const checkUpdate = useCommandMutation("app_check_update");
  /**
   * 配置文件的监听模式：写的是**配置**（`config_set_file_watch_mode`），
   * 但**监听器本身还没实现** —— 后端 `set_file_watch_mode` 里就有这句 warn，
   * 并且它恒定回 `restart_required: true`。所以这里**只可能**说"需要重启"，绝不说"已生效"。
   */
  const setFileWatch = useCommandMutation("config_set_file_watch_mode", {
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: qk.config }),
  });

  const [draft, setDraft] = useState<ConfigDoc | undefined>(undefined);
  const [pristine, setPristine] = useState<ConfigDoc | undefined>(undefined);
  const [localIssues, setLocalIssues] = useState<LocalIssue[]>([]);
  const [report, setReport] = useState<ReportView | undefined>(undefined);
  const [applied, setApplied] = useState<ApplyView | undefined>(undefined);

  const [withSecrets, setWithSecrets] = useState(false);
  const [exported, setExported] = useState<string | undefined>(undefined);
  const [importText, setImportText] = useState("");
  const [importMode, setImportMode] = useState<"merge" | "replace">("merge");
  const [importConfirmOpen, setImportConfirmOpen] = useState(false);
  const [rollbackTarget, setRollbackTarget] = useState<SnapshotView | undefined>(undefined);

  const [modeChoice, setModeChoice] = useState<LogMode | undefined>(undefined);
  const [savedMode, setSavedMode] = useState<LogMode | undefined>(undefined);
  const [retentionChoice, setRetentionChoice] = useState<Retention | undefined>(undefined);
  const [savedRetention, setSavedRetention] = useState<Retention | undefined>(undefined);
  const [clearOpen, setClearOpen] = useState(false);
  const [clearMode, setClearMode] = useState<ClearMode>("all");

  const [accessText, setAccessText] = useState<string | undefined>(undefined);

  // 草稿初始化：只在"后端内容真的变了"时覆盖（同一份内容重复拉到不动草稿）
  const lastServer = useRef<string | undefined>(undefined);
  useEffect(() => {
    const doc = config.data;
    if (doc === undefined) return;
    const fingerprint = JSON.stringify(doc);
    if (lastServer.current === fingerprint) return;
    lastServer.current = fingerprint;
    setDraft(cloneDoc(doc));
    setPristine(cloneDoc(doc));
  }, [config.data]);

  const dirtyPaths = useMemo(
    () => (draft === undefined || pristine === undefined ? [] : diffPaths(pristine, draft)),
    [draft, pristine],
  );
  const isDirty = dirtyPaths.length > 0;
  const busy = validate.isPending || applyConfig.isPending;
  const configError = config.error ?? undefined;

  const onChange = (path: readonly string[], value: unknown) => {
    if (isForbidden(path)) return; // setPath 的白名单是兜底，这里是第二道
    setDraft((prev) => (prev === undefined ? prev : setPath(prev, path, value)));
  };

  /** 本地预检 → 后端权威校验。`undefined` = 没通过（或连校验都没发出去）。 */
  const runValidate = async (): Promise<ReportView | undefined> => {
    if (draft === undefined) return undefined;
    const issues = validateDraft(draft);
    setLocalIssues(issues);
    if (issues.length > 0) {
      setReport(undefined);
      return undefined;
    }
    try {
      const raw = await validate.mutateAsync({ doc: draft });
      const next = readValidationReport(raw);
      setReport(next);
      return next;
    } catch (err) {
      setReport(undefined);
      pushToast({
        level: "error",
        title: t("settings.section.config"),
        body: errorMessage(err),
        dedupeKey: "settings.validate.error",
      });
      return undefined;
    }
  };

  const onValidateOnly = () => {
    setApplied(undefined);
    void runValidate();
  };

  const onValidateAndApply = async () => {
    if (draft === undefined) return;
    setApplied(undefined);
    const next = await runValidate();
    if (next === undefined || !next.ok) return;
    try {
      const raw = await applyConfig.mutateAsync({ doc: draft });
      const result = readApplyResult(raw);
      setApplied(result);
      if (result.applied) {
        // 后端确认写了这一份 → 本地就以它为准（不靠再拉一次来"装作"知道了）
        lastServer.current = JSON.stringify(draft);
        setPristine(cloneDoc(draft));
        void config.refetch();
        pushToast({
          level: "info",
          title: t("settings.section.config"),
          body: t("settings.config.applied"),
          dedupeKey: "settings.apply",
        });
      } else {
        // 没有 applied: true 就绝不说"已写入"
        pushToast({
          level: "warn",
          title: t("settings.section.config"),
          body: t("settings.config.reportUnknown"),
          dedupeKey: "settings.apply.unknown",
        });
      }
    } catch (err) {
      pushToast({
        level: "error",
        title: t("settings.section.config"),
        body: errorMessage(err),
        dedupeKey: "settings.apply.error",
      });
    }
  };

  const onRevert = () => {
    if (pristine === undefined) return;
    setDraft(cloneDoc(pristine));
    setLocalIssues([]);
    setReport(undefined);
    setApplied(undefined);
  };

  const onExport = () => {
    exportYaml.mutate(
      { with_secrets: withSecrets },
      {
        onSuccess: (yaml) => setExported(yaml),
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("settings.config.export"),
            body: errorMessage(err),
            dedupeKey: "settings.export.error",
          }),
      },
    );
  };

  const doImport = () => {
    importYaml.mutate(
      { yaml: importText, mode: importMode },
      {
        onSuccess: (raw) => {
          const result = readApplyResult(raw);
          pushToast({
            level: result.applied ? "info" : "warn",
            title: t("settings.config.import"),
            body: result.applied
              ? t("settings.config.importDone")
              : t("settings.config.reportUnknown"),
            dedupeKey: "settings.import",
          });
          void config.refetch();
          void history.refetch();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("settings.config.import"),
            body: errorMessage(err),
            dedupeKey: "settings.import.error",
          }),
      },
    );
  };

  const onImportClick = () => {
    if (importText.trim() === "") {
      pushToast({
        level: "warn",
        title: t("settings.config.import"),
        body: t("settings.config.importEmpty"),
        dedupeKey: "settings.import.empty",
      });
      return;
    }
    setImportConfirmOpen(true);
  };

  const doRollback = (snapshot: SnapshotView) => {
    rollback.mutate(
      { snapshot_id: snapshot.id },
      {
        onSuccess: (raw) => {
          const result = readApplyResult(raw);
          pushToast({
            level: result.applied ? "info" : "warn",
            title: t("settings.config.rollback"),
            body: result.applied
              ? t("settings.config.rollbackDone")
              : t("settings.config.reportUnknown"),
            dedupeKey: "settings.rollback",
          });
          void config.refetch();
          void history.refetch();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("settings.config.rollback"),
            body: errorMessage(err),
            dedupeKey: "settings.rollback.error",
          }),
      },
    );
  };

  const onOpenDir = (kind: "logs" | "config" | "data") => {
    openDir.mutate(
      { kind },
      {
        onSuccess: () =>
          pushToast({
            level: "info",
            title: t("settings.openConfigDir"),
            body: t("settings.openDir.done"),
            dedupeKey: "settings.openDir",
          }),
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("settings.openConfigDir"),
            body: t("settings.openDir.failed", { message: errorMessage(err) }),
            dedupeKey: "settings.openDir.error",
          }),
      },
    );
  };

  const onGatewayToggle = (running: boolean) => {
    const mutation = running ? pauseGateway : resumeGateway;
    mutation.mutate(undefined, {
      onSuccess: () =>
        pushToast({
          level: "info",
          title: t("settings.section.service"),
          body: running ? t("settings.service.pause.done") : t("settings.service.resume.done"),
          dedupeKey: "settings.gateway",
        }),
      onError: (err) =>
        pushToast({
          level: "error",
          title: t("settings.section.service"),
          body: errorMessage(err),
          dedupeKey: "settings.gateway.error",
        }),
    });
  };

  const onSaveMode = () => {
    if (modeChoice === undefined) return;
    const chosen = modeChoice;
    saveMode.mutate(
      { mode: chosen },
      {
        onSuccess: () => {
          setSavedMode(chosen);
          setModeChoice(undefined);
          void recent.refetch();
          pushToast({
            level: "info",
            title: t("logs.mode.title"),
            body: t("logs.mode.applied", { mode: chosen }),
            dedupeKey: "settings.mode",
          });
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.mode.title"),
            body: errorMessage(err),
            dedupeKey: "settings.mode.error",
          }),
      },
    );
  };

  const onSaveRetention = () => {
    if (retentionChoice === undefined) return;
    const chosen = retentionChoice;
    saveRetention.mutate(
      { retention: chosen },
      {
        onSuccess: () => {
          setSavedRetention(chosen);
          setRetentionChoice(undefined);
          pushToast({
            level: "info",
            title: t("logs.retention.title"),
            body: t("logs.retention.applied", { value: t(RETENTION_LABEL[chosen]) }),
            dedupeKey: "settings.retention",
          });
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.retention.title"),
            body: errorMessage(err),
            dedupeKey: "settings.retention.error",
          }),
      },
    );
  };

  const onClear = (clear: ClearMode) => {
    clearLogs.mutate(
      { mode: clear },
      {
        onSuccess: (result) => {
          const count = typeof result === "number" ? result : undefined;
          pushToast({
            level: "info",
            title: t("logs.clear.title"),
            body:
              count === undefined
                ? t("logs.clear.doneUnknown")
                : t("logs.clear.done", { value: formatInt(count) }),
            dedupeKey: "settings.clear",
          });
          void stats.refetch();
          void recent.refetch();
        },
        onError: (err) =>
          pushToast({
            level: "error",
            title: t("logs.clear.title"),
            body: errorMessage(err),
            dedupeKey: "settings.clear.error",
          }),
      },
    );
  };

  const onShowAccessInfo = () => {
    if (accessText !== undefined) {
      setAccessText(undefined);
      return;
    }
    accessInfo.mutate(undefined, {
      onSuccess: (text) => setAccessText(text),
      onError: (err) =>
        pushToast({
          level: "error",
          title: t("settings.service.localKey"),
          body: errorMessage(err),
          dedupeKey: "settings.access.error",
        }),
    });
  };

  const onCopyAccessInfo = async () => {
    let text = accessText;
    if (text === undefined) {
      try {
        text = await accessInfo.mutateAsync(undefined);
        setAccessText(text);
      } catch (err) {
        pushToast({
          level: "error",
          title: t("settings.service.copyAccessInfo"),
          body: errorMessage(err),
          dedupeKey: "settings.copy.error",
        });
        return;
      }
    }
    const ok = await writeClipboard(text);
    pushToast({
      level: ok ? "info" : "warn",
      title: t("settings.service.copyAccessInfo"),
      body: ok ? t("settings.service.copy.done") : t("settings.service.copy.unavailable"),
      dedupeKey: "settings.copy",
    });
  };

  // ---- 归一化后的只读数据
  const gatewayRunning = status.data?.gateway !== "paused";
  const kernel = status.data?.kernel;
  const keyState = secretState(draft?.gateway.local_key);
  const storage = readStorageStats(stats.data);
  const newest = readNewestRow(recent.data);
  const snapshots = readSnapshots(history.data);

  return (
    <>
      <PageHeader
        title={t("nav.settings")}
        description={t("settings.description")}
        actions={
          <>
            <Button variant="outlined" onClick={() => void config.refetch()}>
              {t("common.refresh")}
            </Button>
            <Button variant="outlined" onClick={() => onOpenDir("config")}>
              {t("settings.openConfigDir")}
            </Button>
          </>
        }
      />

      <Box
        data-testid="settings-page"
        sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}
      >
        {/* 配置读不到：如实报错 + 重试；不显示陈旧值、也不显示编造的默认值 */}
        {configError !== undefined ? (
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={3}>
              <Typography variant="h4">{t("error.title")}</Typography>
              <Typography
                variant="body1"
                data-testid="settings-config-error"
                sx={{ color: "error.main" }}
              >
                {t("settings.loadFailed", { message: errorMessage(configError) })}
              </Typography>
              <Box>
                <Button variant="outlined" onClick={() => void config.refetch()}>
                  {t("common.retry")}
                </Button>
              </Box>
              <Caption>{t("settings.config.loadFailedHint")}</Caption>
            </Stack>
          </Paper>
        ) : null}

        {/* ① 服务：说明收进 `settings-section-service-help`（其余说明行与红线留在版面上） */}
        <SectionPaper
          titleKey="settings.section.service"
          testId="settings-section-service"
          /* [极简 · 判据 2「长解释收进问号」] 本区块**两段非红线解释**都并进同一个问号：
             - [settings.service.editHint]（46 字：这一组改的是草稿、要按下面才生效）——
               它是"这一段是干什么的"的成句说明；
             - [settings.service.localKey.hint]（62 字：这个密钥是什么 + 用下面哪个按钮）——
               它说的两件事在版面上都已经有出处（"值不外显"写在密钥状态那行的值里，
               按钮的名字就写在按钮上）；
             - [settings.service.downstream.hint]（34 字：勾选的作用）—— 它是一句成句的说明，
               而三个开关的名字本身已经说清"勾什么"；规则 2 的取向是句子级说明进 tooltip。
             三个键的正文一个字都没改（"搬进问号"不是"删掉"）。
             键名直接写在 `<HelpTip>` 里（而不是让本区块按 `descriptionKey` 去取）：
             渲染结果一模一样，但"这三句只在问号里出现"这件事在调用点就看得见。 */
          helpTip={
            <HelpTip
              testId="settings-section-service-help"
              text={[
                t("settings.service.editHint"),
                t("settings.service.localKey.hint"),
                t("settings.service.downstream.hint"),
              ].join("\n")}
            />
          }
        >
          <Stack spacing={3}>
            <ReadonlyRow
              label={t("settings.service.gateway")}
              value={
                status.data === undefined ? (
                  <StatusDot status="off" label={t("common.unknown")} />
                ) : (
                  <StatusDot
                    status={gatewayRunning ? "ok" : "warn"}
                    label={
                      gatewayRunning ? t("status.gateway.running") : t("status.gateway.paused")
                    }
                  />
                )
              }
            />
            <ReadonlyRow
              label={t("settings.service.localKey")}
              value={t(SECRET_STATE_KEY[keyState])}
            />
          </Stack>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
              gap: 3,
            }}
          >
            <EditableField
              testId="settings-field-listen"
              labelKey="settings.service.listen"
              path={["gateway", "listen"]}
              doc={draft}
              onChange={onChange}
            />
            <EditableField
              testId="settings-field-adminListen"
              labelKey="settings.service.adminListen"
              path={["gateway", "admin_listen"]}
              doc={draft}
              onChange={onChange}
            />
            <EditableField
              testId="settings-field-maxBodyMb"
              labelKey="settings.service.maxBodyMb"
              path={["gateway", "max_body_mb"]}
              doc={draft}
              onChange={onChange}
              kind="int"
            />
          </Box>

          <Stack spacing={1}>
            <EditableField
              testId="settings-field-debugHeaders"
              labelKey="settings.service.debugHeaders"
              path={["gateway", "debug_headers"]}
              doc={draft}
              onChange={onChange}
              kind="bool"
            />

          </Stack>

          {/*
            [docs/59] **下游 API 家族开关**（用户第 326 轮需求 2）。

            三个**独立**的勾选框，不是"选一个" —— 用户原话是"打开关闭形式，按需求勾选"。
            关掉的端点会在网关侧当场明确报错（错误里点出是哪个开关），不会被转发上游。

            用同一个 `EditableField`（`path` 直接落进配置文档），所以这里不需要任何新管道：
            保存走的就是设置页既有的那条路径。
          */}
          {/* [极简 · 判据 2] 原来这里铺着一行 34 字的成句说明（[settings.service.downstream.hint]）；
              它已并进本区块唯一的问号（见上面的 `helpTip`）。三个开关的名字留在版面上。 */}
          <Stack spacing={1}>
            <EditableField
              testId="settings-field-downstreamOpenaiChat"
              labelKey="settings.service.downstream.openaiChat"
              path={["gateway", "downstream", "openai_chat"]}
              doc={draft}
              onChange={onChange}
              kind="bool"
            />
            <EditableField
              testId="settings-field-downstreamOpenaiResponses"
              labelKey="settings.service.downstream.openaiResponses"
              path={["gateway", "downstream", "openai_responses"]}
              doc={draft}
              onChange={onChange}
              kind="bool"
            />
            <EditableField
              testId="settings-field-downstreamAnthropicMessages"
              labelKey="settings.service.downstream.anthropicMessages"
              path={["gateway", "downstream", "anthropic_messages"]}
              doc={draft}
              onChange={onChange}
              kind="bool"
            />
          </Stack>

          <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
            <Button
              variant="outlined"
              onClick={() => onGatewayToggle(gatewayRunning)}
              disabled={
                status.data === undefined || pauseGateway.isPending || resumeGateway.isPending
              }
            >
              {gatewayRunning ? t("settings.service.pause") : t("settings.service.resume")}
            </Button>
            <Button variant="outlined" onClick={() => void onCopyAccessInfo()}>
              {t("settings.service.copyAccessInfo")}
            </Button>
            <Button variant="outlined" onClick={onShowAccessInfo}>
              {accessText === undefined
                ? t("settings.service.accessInfo.show")
                : t("settings.service.accessInfo.hide")}
            </Button>
          </Box>

          {accessText !== undefined ? (
            <Stack spacing={1}>
              <Caption>{t("settings.service.accessInfo.hint")}</Caption>
              <Box
                component="pre"
                data-testid="settings-access-info-output"
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
                  color: "text.primary",
                  whiteSpace: "pre-wrap",
                  overflowWrap: "anywhere",
                }}
              >
                {accessText}
              </Box>
            </Stack>
          ) : null}
        </SectionPaper>

        {/* ② 启动与常驻：开机自启动是**唯一会写系统状态**的开关，所以显示的是系统事实 */}
        <SectionPaper
          titleKey="settings.section.startup"
          testId="settings-section-startup"
          descriptionKey="settings.startup.desired.hint"
        >
          <Stack spacing={2}>
            <Typography variant="h4">{t("settings.startup.autostart")}</Typography>

            {autostart.error !== null && autostart.error !== undefined ? (
              <Typography variant="body1" data-testid="settings-autostart-error" sx={{ color: "error.main" }}>
                {t("settings.startup.error", { message: errorMessage(autostart.error) })}
              </Typography>
            ) : autostart.data === undefined ? (
              <Typography variant="body1" sx={{ color: "text.secondary" }}>
                {t("settings.startup.loading")}
              </Typography>
            ) : (
              <Stack spacing={2}>
                {/* 开关绑 `actual`（系统事实）；按下去真的注册/移除系统启动项。
                    [极简 · 判据 2] 下面只留**一行真相**「系统实际状态」。
                    原来那两行被删掉的：
                    - 「配置里的期望值：X」—— 期望值不是用户要看的东西，它只在"系统和期望不一致"
                      时有意义，而那种情况由下面那句提示 + 「修复自启动」按钮表达；
                    - 「注册机制：windows-run-key」—— 那是内部实现值（判据 1 禁止）。
                    读屏名称仍靠 `aria-label`（一个字不少）。 */}
                <Box data-testid="settings-autostart-switch">
                  <Stack spacing={1}>
                    <Stack direction="row" spacing={1} alignItems="center">
                      <Switch
                        checked={autostart.data.actual}
                        disabled={
                          setAutostart.isPending ||
                          autostart.data.mechanism === "unsupported"
                        }
                        onChange={(event) =>
                          setAutostart.mutate({
                            enabled: event.target.checked,
                            mode: autostart.data.mode,
                            delay_s: autostart.data.delay_s,
                          })
                        }
                        inputProps={{
                          "aria-label": t("settings.startup.actual", {
                            value: autostart.data.actual
                              ? t("settings.startup.yes")
                              : t("settings.startup.no"),
                          }),
                        }}
                      />
                    </Stack>
                    <Caption>{t("settings.startup.switch.hint")}</Caption>
                  </Stack>
                </Box>

                <Typography
                  variant="body1"
                  data-testid="settings-autostart-actual"
                  data-value={String(autostart.data.actual)}
                >
                  {t("settings.startup.actual", {
                    value: autostart.data.actual ? t("settings.startup.yes") : t("settings.startup.no"),
                  })}
                </Typography>

                {autostart.data.mechanism === "unsupported" ? (
                  <Typography variant="body1" data-testid="settings-autostart-unsupported" sx={{ color: "warning.main" }}>
                    {t("settings.startup.unsupported", {
                      reason: autostart.data.unsupported_reason ?? "",
                    })}
                  </Typography>
                ) : null}

                {/* [极简 · 判据 2] 漂移时不再把两个值念一遍（期望值已不上版面）：
                    只留一句"不一致" + 一个修复入口 —— 那正是用户需要的信息与动作。 */}
                {autostart.data.drifted ? (
                  <Stack spacing={1} data-testid="settings-autostart-drifted">
                    <Typography variant="body1" sx={{ color: "warning.main" }}>
                      {t("settings.startup.drifted")}
                    </Typography>
                    <Box>
                      <Button
                        data-testid="settings-autostart-repair"
                        variant="outlined"
                        disabled={repairAutostart.isPending}
                        onClick={() => repairAutostart.mutate(undefined)}
                      >
                        {t("settings.startup.repair")}
                      </Button>
                    </Box>
                  </Stack>
                ) : null}
              </Stack>
            )}
          </Stack>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
              gap: 3,
            }}
          >
            <EditableField
              testId="settings-field-autostartMode"
              labelKey="settings.startup.mode"
              path={["system_integration", "autostart_mode"]}
              doc={draft}
              onChange={onChange}
              kind="select"
              options={AUTOSTART_MODES}
            />
            <EditableField
              testId="settings-field-autostartDelay"
              labelKey="settings.startup.delay"
              path={["system_integration", "autostart_delay_s"]}
              doc={draft}
              onChange={onChange}
              kind="int"
              hintKey="settings.startup.delay.hint"
            />
          </Box>

          <Caption>{t("settings.startup.writeHint")}</Caption>
        </SectionPaper>

        {/* ③ 托盘行为 —— 版面上只留**破坏性后果**那一句（§2 第 4 条：这类后果不许藏进悬停）。
            原来那 132 字的 [settings.tray.fileWatch.hint] 里，函数名（config_set_file_watch_mode）、
            "监听器还没实现"、"只在下次启动时被读取"都是实现交代（判据 1）→ 已从文案里去掉
            （见映射文件）；"结果只可能是「需要重启」"由开关下面那条**结果行**在真的发生时说出来，
            所以没有任何用户需要的事实被丢掉。这一节因此仍然**没有问号**（上限不是配额）。 */}
        <SectionPaper titleKey="settings.section.tray" testId="settings-section-tray">
          <EditableField
            testId="settings-field-trayShowLatency"
            labelKey="settings.tray.showLatency"
            path={["system_integration", "tray_show_latency"]}
            doc={draft}
            onChange={onChange}
            kind="bool"
          />
          {/* 开关写的是**配置**（不是系统状态），而且监听器还没实现：只可能得到"需要重启" */}
          <Stack spacing={1}>
            <Box data-testid="settings-file-watch-switch">
              <FormControlLabel
                control={
                  <Switch
                    checked={draft?.system_integration.file_watch_mode === true}
                    disabled={draft === undefined || setFileWatch.isPending}
                    onChange={(event) =>
                      setFileWatch.mutate({
                        enabled: event.target.checked,
                        ...(draft?.system_integration.file_watch_path === undefined ||
                        draft?.system_integration.file_watch_path === null
                          ? {}
                          : { path: draft.system_integration.file_watch_path }),
                      })
                    }
                  />
                }
                label={t("settings.tray.fileWatch")}
              />
            </Box>
            {/* [极简 · 判据 1「同一屏里同一个事实不许说两遍」] 这里**不再**画一行
                「配置里的值：开/关」：上面那个开关的位置**就是**这个布尔值
                （读的是同一个 `draft.system_integration.file_watch_mode`），而且开关自己
                的名字（[settings.tray.fileWatch]）留在版面上，所以没有丢任何信息。
                删掉它顺带修掉一处不诚实：草稿还没读出来时（`draft === undefined`）
                那行会写着「配置里的值：关」，可它那时**并不知道**配置里是什么。 */}
            <Caption>{t("settings.tray.fileWatch.hint")}</Caption>
          </Stack>

          {setFileWatch.error !== null && setFileWatch.error !== undefined ? (
            <Typography
              variant="body1"
              data-testid="settings-file-watch-error"
              sx={{ color: "error.main" }}
            >
              {t("settings.tray.fileWatch.error", { message: errorMessage(setFileWatch.error) })}
            </Typography>
          ) : setFileWatch.data === undefined ? null : (
            <Typography
              variant="body1"
              data-testid="settings-file-watch-result"
              data-applied={String(setFileWatch.data.applied)}
              data-restart-required={String(setFileWatch.data.restart_required)}
              sx={{ color: setFileWatch.data.applied ? "text.primary" : "warning.main" }}
            >
              {setFileWatch.data.applied
                ? setFileWatch.data.restart_required
                  ? t("settings.tray.fileWatch.restart")
                  : t("settings.tray.fileWatch.appliedNoRestart")
                : t("settings.tray.fileWatch.notApplied")}
            </Typography>
          )}
        </SectionPaper>

        {/* ④ 记录与存储（命令已注册：真做） */}
        <SectionPaper
          titleKey="settings.section.storage"
          testId="settings-section-storage"
          descriptionKey="logs.storage.scope"
        >
          <Stack spacing={3}>
            <Typography variant="h4">{t("settings.storage.stats")}</Typography>
            <Box
              data-testid="settings-storage-stats"
              sx={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
                gap: 3,
              }}
            >
              <StatItem
                label={t("logs.storage.rows")}
                value={
                  storage?.rows !== undefined
                    ? formatInt(storage.rows)
                    : stats.isPending
                      ? t("common.loading")
                      : DASH
                }
              />
              <StatItem
                label={t("logs.storage.bytes")}
                value={storage?.bytes !== undefined ? formatBytes(storage.bytes) : DASH}
              />
              <StatItem
                label={t("logs.storage.bodyBytes")}
                value={storage?.bodyBytes !== undefined ? formatBytes(storage.bodyBytes) : DASH}
              />
              <StatItem
                label={t("logs.storage.oldest")}
                value={storage?.oldestTs !== undefined ? formatLocalTime(storage.oldestTs) : DASH}
              />
              <StatItem
                label={t("logs.storage.newest")}
                value={storage?.newestTs !== undefined ? formatLocalTime(storage.newestTs) : DASH}
              />
            </Box>
            {stats.error !== undefined && stats.error !== null ? (
              <Typography variant="body1" sx={{ color: "error.main" }}>
                {t("logs.storage.failed", { message: errorMessage(stats.error) })}
              </Typography>
            ) : null}
            <Caption>{t("settings.storage.toLogs")}</Caption>
          </Stack>

          <Stack spacing={2}>
            <Typography variant="h4">{t("logs.mode.title")}</Typography>
            <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
              {LOG_MODES.map((mode) => (
                <Button
                  key={mode}
                  variant={modeChoice === mode ? "contained" : "outlined"}
                  onClick={() => setModeChoice(mode)}
                  sx={{ fontFamily: typography.fontMono }}
                >
                  {mode}
                </Button>
              ))}
              <Button
                variant="outlined"
                disabled={modeChoice === undefined || saveMode.isPending}
                onClick={onSaveMode}
              >
                {t("logs.mode.save")}
              </Button>
            </Box>
            <Stack spacing={1}>
              <Caption>
                {savedMode === undefined
                  ? /* [极简 · 判据 1] 这一行**不**再复用 [logs.mode.currentUnknown]：那个值的括号里
                       写着「后端只有设置接口、没有读取接口，这里不猜」，而同一节的保留期那行
                       （[logs.retention.currentUnknown]）写着**一模一样的一句理由** —— 同一屏里
                       同一个事实说两遍。理由留在**保留期**那行（它永远是"未读取"），
                       这里只留结论（"未读取"这个红线一个字没少）。 */
                    t("settings.storage.modeUnknown")
                  : t("settings.storage.modeWritten", { mode: savedMode })}
              </Caption>
              <Typography variant="caption" data-testid="settings-storage-mode-evidence" sx={{ color: "text.secondary" }}>
                {newest.mode === undefined
                  ? t("settings.storage.modeEvidenceNone")
                  : t("settings.storage.modeEvidence", {
                      mode: newest.mode,
                      time:
                        newest.tsMs === undefined
                          ? t("common.unknown")
                          : formatLocalTime(newest.tsMs),
                    })}
              </Typography>
            </Stack>
            <Stack spacing={1}>
              {LOG_MODES.map((mode) => (
                <Box key={mode} sx={{ display: "flex", gap: 2, minWidth: 0 }}>
                  <Typography
                    variant="caption"
                    sx={{ fontFamily: typography.fontMono, minWidth: 48, color: "text.primary" }}
                  >
                    {mode}
                  </Typography>
                  <Caption>{t(MODE_DESC[mode])}</Caption>
                </Box>
              ))}
            </Stack>
          </Stack>

          <Stack spacing={2}>
            <Typography variant="h4">{t("logs.retention.title")}</Typography>
            <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
              {RETENTIONS.map((value) => (
                <Button
                  key={value}
                  variant={retentionChoice === value ? "contained" : "outlined"}
                  onClick={() => setRetentionChoice(value)}
                >
                  {t(RETENTION_LABEL[value])}
                </Button>
              ))}
              <Button
                variant="outlined"
                disabled={retentionChoice === undefined || saveRetention.isPending}
                onClick={onSaveRetention}
              >
                {t("logs.retention.save")}
              </Button>
            </Box>
            <Caption>
              {savedRetention === undefined
                ? t("logs.retention.currentUnknown")
                : t("settings.storage.retentionWritten", {
                    value: t(RETENTION_LABEL[savedRetention]),
                  })}
            </Caption>
          </Stack>

          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))",
              gap: 3,
            }}
          >
            <EditableField
              testId="settings-field-maxRows"
              labelKey="settings.storage.maxRows"
              path={["request_log", "max_rows"]}
              doc={draft}
              onChange={onChange}
              kind="int"
            />
            <EditableField
              testId="settings-field-maxSizeMb"
              labelKey="settings.storage.maxSizeMb"
              path={["request_log", "max_size_mb"]}
              doc={draft}
              onChange={onChange}
              kind="int"
            />
            <EditableField
              testId="settings-field-sampleRate"
              labelKey="settings.storage.sampleRate"
              path={["request_log", "sample_rate"]}
              doc={draft}
              onChange={onChange}
              kind="int"
              step={0.1}
              hintKey="settings.storage.sampleRateHint"
            />
            <EditableField
              testId="settings-field-imageHandling"
              labelKey="settings.image.title"
              path={["request_log", "image_handling"]}
              doc={draft}
              onChange={onChange}
              kind="select"
              options={IMAGE_HANDLINGS}
            />
          </Box>

          <Stack spacing={3}>
            <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
              <Button
                variant="outlined"
                onClick={() => onOpenDir("logs")}
                data-testid="settings-open-logs"
              >
                {t("logs.storage.openDir")}
              </Button>
              <Button
                variant="outlined"
                color="error"
                onClick={() => setClearOpen(true)}
                data-testid="settings-clear-open"
              >
                {t("logs.clear.button")}
              </Button>
            </Box>
            {/* 导出在日志页（/logs）：这里不放按钮，只留一行指向说明（避免两处入口各说一半） */}
            <Typography
              variant="body1"
              data-testid="settings-no-log-export"
              sx={{ color: "text.secondary" }}
            >
              {t("settings.storage.noExport")}
            </Typography>
            <Caption>{t("logs.storage.sanitized")}</Caption>
          </Stack>
        </SectionPaper>

        {/* ⑤ 配置：校验 → 应用 / 导入导出 / 快照 */}
        <SectionPaper
          titleKey="settings.section.config"
          testId="settings-section-config"
          descriptionKey="settings.config.localFirst"
        >
          <Stack spacing={2}>
            <Typography variant="h4">{t("settings.config.editableTitle")}</Typography>
            <Typography
              variant="body1"
              data-testid="settings-dirty"
              sx={{ color: isDirty ? "warning.main" : "text.secondary" }}
            >
              {isDirty
                ? t("settings.config.dirty", {
                    count: dirtyPaths.length,
                    paths: dirtyPaths.join("、"),
                  })
                : t("settings.config.clean")}
            </Typography>
          </Stack>

          {localIssues.length > 0 ? (
            <Stack spacing={1} data-testid="settings-local-issues">
              <Typography variant="body1" sx={{ color: "warning.main" }}>
                {t("settings.config.localIssues")}
              </Typography>
              {localIssues.map((issue) => (
                <Caption key={issue.path}>{`${issue.path} — ${t(issue.key)}`}</Caption>
              ))}
            </Stack>
          ) : null}

        {/* [docs/30 §3.18] 长页面（1838 行）的主操作固定在底部：照抄 `providers-save-bar` 的 sticky 做法 */}
        <Box
          sx={{ position: "sticky", bottom: 0, zIndex: 1, bgcolor: "background.paper", py: 2 }}
          data-testid="settings-apply-bar"
        >
          <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
            <Button
              variant="contained"
              data-testid="settings-apply"
              disabled={draft === undefined || !isDirty || busy}
              onClick={() => void onValidateAndApply()}
            >
              {applyConfig.isPending
                ? t("settings.config.applying")
                : t("settings.config.validateAndApply")}
            </Button>
            <Button
              variant="outlined"
              data-testid="settings-validate"
              disabled={draft === undefined || busy}
              onClick={onValidateOnly}
            >
              {validate.isPending
                ? t("settings.config.validating")
                : t("settings.config.validateOnly")}
            </Button>
            <Button variant="outlined" data-testid="settings-revert" disabled={!isDirty} onClick={onRevert}>
              {t("settings.config.revert")}
            </Button>
        </Box>
          </Box>

          {report !== undefined ? (
            <Stack spacing={1} data-testid="settings-validate-report">
              <Typography variant="body1" sx={{ color: report.ok ? "success.main" : "error.main" }}>
                {report.ok
                  ? t("settings.config.reportOk")
                  : report.recognizable
                    ? t("settings.config.reportFail")
                    : t("settings.config.reportUnknown")}
              </Typography>
              {report.errors.map((issue, index) => (
                <Caption key={`e-${index}`}>
                  {`${t("settings.config.errors")} · ${issue.path} · ${issue.code} · ${issue.message}`}
                </Caption>
              ))}
              {report.warnings.map((issue, index) => (
                <Caption key={`w-${index}`}>
                  {`${t("settings.config.warnings")} · ${issue.path} · ${issue.code} · ${issue.message}`}
                </Caption>
              ))}
            </Stack>
          ) : null}

          {applied !== undefined ? (
            <Stack spacing={1} data-testid="settings-apply-result">
              <Typography
                variant="body1"
                sx={{ color: applied.applied ? "success.main" : "warning.main" }}
              >
                {applied.applied ? t("settings.config.applied") : t("settings.config.reportUnknown")}
              </Typography>
              <Caption>
                {t("settings.config.appliedDetail", {
                  restart: applied.restartRequired ? t("common.yes") : t("common.no"),
                  kernel: applied.kernelReload ? t("common.yes") : t("common.no"),
                })}
              </Caption>
            </Stack>
          ) : null}

          {/* 导出 YAML（内容来自后端导出原文；本页不写文件） */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("settings.config.exportTitle")}</Typography>
            <Stack spacing={1} data-testid="settings-field-exportSecrets">
              <FormControlLabel
                control={
                  <Checkbox
                    size="small"
                    checked={withSecrets}
                    onChange={(e) => setWithSecrets(e.target.checked)}
                  />
                }
                label={t("settings.config.exportSecrets")}
              />
            </Stack>
            {withSecrets ? (
              <Typography
                variant="body1"
                data-testid="settings-export-secrets-warn"
                sx={{ color: "warning.main" }}
              >
                {t("settings.config.exportSecrets.warn")}
              </Typography>
            ) : null}
            <Box sx={{ display: "flex", gap: 2, alignItems: "center", flexWrap: "wrap" }}>
              <Button
                variant="outlined"
                data-testid="settings-export"
                disabled={exportYaml.isPending}
                onClick={onExport}
              >
                {t("settings.config.export")}
              </Button>
              <Caption>{t("settings.config.exportHint")}</Caption>
            </Box>
            {exported !== undefined ? (
              <Box
                component="pre"
                data-testid="settings-export-output"
                sx={{
                  m: 0,
                  p: 3,
                  maxHeight: 320,
                  overflow: "auto",
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
                }}
              >
                {exported}
              </Box>
            ) : null}
          </Stack>

          {/* 导入 YAML（replace 是破坏性的 → 先确认） */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("settings.config.importTitle")}</Typography>
            <TextField
              multiline
              minRows={4}
              label={t("settings.config.importPlaceholder")}
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              data-testid="settings-import-yaml"
            />
            <TextField
              select
              label={t("settings.config.importMode")}
              value={importMode}
              onChange={(e) => setImportMode(e.target.value as "merge" | "replace")}
              sx={{ maxWidth: 320 }}
            >
              <MenuItem value="merge">{t("settings.config.importMode.merge")}</MenuItem>
              <MenuItem value="replace">{t("settings.config.importMode.replace")}</MenuItem>
            </TextField>
            {importMode === "replace" ? (
              <Caption>{t("settings.config.importReplace.warn")}</Caption>
            ) : null}
            <Box>
              <Button
                variant="outlined"
                data-testid="settings-import"
                disabled={importYaml.isPending}
                onClick={onImportClick}
              >
                {t("settings.config.import")}
              </Button>
            </Box>
          </Stack>

          {/* 快照与回滚 */}
          <Stack spacing={2}>
            <Typography variant="h4">{t("settings.config.snapshots")}</Typography>
            {history.error !== undefined && history.error !== null ? (
              <Typography variant="body1" sx={{ color: "error.main" }}>
                {errorMessage(history.error)}
              </Typography>
            ) : null}
            {snapshots.length === 0 ? (
              <Typography
                variant="body1"
                data-testid="settings-snapshots-empty"
                sx={{ color: "text.secondary" }}
              >
                {t("settings.config.snapshotsEmpty")}
              </Typography>
            ) : (
              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="settings-snapshots">
                  <TableHead>
                    <TableRow>
                      <TableCell>{t("settings.config.snapshot.label")}</TableCell>
                      <TableCell>{t("settings.config.snapshot.created")}</TableCell>
                      <TableCell align="right">{t("settings.config.snapshot.size")}</TableCell>
                      <TableCell />
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {snapshots.map((snapshot) => (
                      <TableRow key={snapshot.id} data-testid="settings-snapshot-row">
                        <TableCell>
                          {/* [docs/26 §2] 「标签 · id」两个用户可见值拼在一格，可能很长。
                              恢复路径：①「回滚」按钮开的那张确认框里带着同一个 id（本页既有入口）；
                              ②悬停/聚焦的 tooltip 给完整原文。 */}
                          <AutoEllipsis
                            testId="settings-snapshot-label-ellipsis"
                            sx={{ maxWidth: 320 }}
                          >
                            {`${snapshot.label} · ${snapshot.id}`}
                          </AutoEllipsis>
                        </TableCell>
                        <TableCell sx={{ fontVariantNumeric: "tabular-nums", color: "text.secondary" }}>
                          {snapshot.createdAtMs === undefined
                            ? DASH
                            : formatLocalTime(snapshot.createdAtMs)}
                        </TableCell>
                        <TableCell align="right" sx={{ fontVariantNumeric: "tabular-nums" }}>
                          {snapshot.sizeBytes === undefined ? DASH : formatBytes(snapshot.sizeBytes)}
                        </TableCell>
                        <TableCell align="right">
                          <Button
                            variant="outlined"
                            data-testid="settings-rollback"
                            onClick={() => setRollbackTarget(snapshot)}
                          >
                            {t("settings.config.rollback")}
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </Box>
            )}
          </Stack>
        </SectionPaper>

        {/* ⑥ 「系统网络信息（只读）」整块**已删除**（用户要求"设置界面的向导关掉"，
            [docs/19 §2.1] 第 2 条）：内核路径与网络信息的编辑归 `/network` 页，设置页不再有
            这一块，也不再有"展开/收起"这种入口。原来那 12 行只读值（TUN / 系统代理 / 内核
            来源·路径·控制口 / lane 端口段 / dialer / DNS / 默认线路 / 健康间隔）由网络页负责。
            页面的 I2 边界仍然由 model.ts 的 FORBIDDEN_PATHS + setPath() 强制（那个测试还在）。 */}

        {/* ⑦ 关于 */}
        <SectionPaper
          titleKey="settings.section.about"
          testId="settings-section-about"
          descriptionKey="settings.about.checkUpdate.hint"
        >
          <Box
            data-testid="settings-about-status"
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))",
              gap: 3,
            }}
          >
            <StatItem label={t("settings.about.appVersion")} value={status.data?.version ?? DASH} />
            <StatItem label={t("settings.about.gateway")} value={status.data?.gateway ?? DASH} />
            <StatItem label={t("settings.about.kernelVersion")} value={kernel?.version ?? DASH} />
            <StatItem
              label={t("settings.about.kernelPid")}
              value={
                kernel?.pid === undefined || kernel?.pid === null ? DASH : formatInt(kernel.pid)
              }
            />
            <StatItem
              label={t("settings.about.kernelRestarts")}
              value={kernel?.restarts === undefined ? DASH : formatInt(kernel.restarts)}
            />
            <StatItem
              label={t("settings.about.uptime")}
              value={status.data === undefined ? DASH : formatDuration(status.data.uptime_s)}
            />
            <StatItem label={t("settings.about.lastError")} value={kernel?.last_error ?? DASH} />
          </Box>

          <Stack spacing={1}>
            <ReadonlyRow
              label={t("settings.about.license")}
              value={t("settings.about.license.unknown")}
            />
            <Box sx={{ display: "flex", gap: 3, alignItems: "center", flexWrap: "wrap" }}>
              <Button
                data-testid="settings-update-run"
                variant="outlined"
                disabled={checkUpdate.isPending}
                onClick={() => checkUpdate.mutate(undefined)}
              >
                {checkUpdate.isPending
                  ? t("settings.about.checkUpdate.running")
                  : t("settings.about.checkUpdate.run")}
              </Button>
            </Box>
            {checkUpdate.error !== null && checkUpdate.error !== undefined ? (
              <Typography
                variant="body1"
                data-testid="settings-update-error"
                sx={{ color: "error.main" }}
              >
                {t("settings.about.checkUpdate.error", { message: errorMessage(checkUpdate.error) })}
              </Typography>
            ) : checkUpdate.data === undefined ? null : checkUpdate.data === null ? (
              <Typography
                variant="body1"
                data-testid="settings-update-none"
                sx={{ color: "text.secondary" }}
              >
                {t("settings.about.checkUpdate.none")}
              </Typography>
            ) : (
              <Stack spacing={1} data-testid="settings-update-result">
                <Typography variant="body1">
                  {t("settings.about.checkUpdate.found", { version: checkUpdate.data.version })}
                </Typography>
                {checkUpdate.data.notes === null || checkUpdate.data.notes === undefined ? null : (
                  <Caption>
                    {t("settings.about.checkUpdate.notes", { notes: checkUpdate.data.notes })}
                  </Caption>
                )}
                {checkUpdate.data.url === null || checkUpdate.data.url === undefined ? null : (
                  <Caption>
                    {t("settings.about.checkUpdate.url", { url: checkUpdate.data.url })}
                  </Caption>
                )}
              </Stack>
            )}
          </Stack>

          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            <Button variant="outlined" onClick={() => onOpenDir("data")}>
              {t("settings.openDataDir")}
            </Button>
            <Button variant="outlined" onClick={() => onOpenDir("config")}>
              {t("settings.openConfigDir")}
            </Button>
          </Box>
        </SectionPaper>
      </Box>

      {/* 清空日志：破坏性操作先确认 */}
      <Dialog open={clearOpen} onClose={() => setClearOpen(false)}>
        <DialogTitle>{t("logs.clear.confirmTitle")}</DialogTitle>
        <DialogContent>
          <Typography variant="body1" sx={{ mb: 2 }}>
            {t("logs.clear.confirmBody")}
          </Typography>
          <RadioGroup value={clearMode} onChange={(e) => setClearMode(e.target.value as ClearMode)}>
            <FormControlLabel
              value="all"
              control={<Radio size="small" />}
              label={t("logs.clear.mode.all")}
            />
            <FormControlLabel
              value="bodies_only"
              control={<Radio size="small" />}
              label={t("logs.clear.mode.bodies_only")}
            />
          </RadioGroup>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setClearOpen(false)}>{t("logs.clear.cancel")}</Button>
          <Button
            variant="contained"
            color="error"
            data-testid="settings-clear-confirm"
            disabled={clearLogs.isPending}
            onClick={() => {
              setClearOpen(false);
              onClear(clearMode);
            }}
          >
            {t("logs.clear.confirm")}
          </Button>
        </DialogActions>
      </Dialog>

      {/* 导入 YAML：replace 是破坏性的 → 先确认 */}
      <Dialog open={importConfirmOpen} onClose={() => setImportConfirmOpen(false)}>
        <DialogTitle>
          {importMode === "replace"
            ? t("settings.config.importConfirmTitle")
            : t("settings.config.import")}
        </DialogTitle>
        <DialogContent>
          <Typography variant="body1" sx={{ mb: 2 }}>
            {importMode === "replace"
              ? t("settings.config.importConfirmBody")
              : t("settings.config.importMode.merge")}
          </Typography>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setImportConfirmOpen(false)}>{t("common.cancel")}</Button>
          <Button
            variant="contained"
            data-testid="settings-import-confirm-action"
            disabled={importYaml.isPending}
            onClick={() => {
              setImportConfirmOpen(false);
              doImport();
            }}
          >
            {t("settings.config.importConfirm")}
          </Button>
        </DialogActions>
      </Dialog>

      {/* 回滚：先确认 */}
      <Dialog open={rollbackTarget !== undefined} onClose={() => setRollbackTarget(undefined)}>
        <DialogTitle>{t("settings.config.rollbackConfirmTitle")}</DialogTitle>
        <DialogContent>
          <Typography variant="body1" sx={{ mb: 2 }} data-testid="settings-rollback-confirm">
            {t("settings.config.rollbackConfirmBody", { id: rollbackTarget?.id ?? "" })}
          </Typography>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setRollbackTarget(undefined)}>{t("common.cancel")}</Button>
          <Button
            variant="contained"
            data-testid="settings-rollback-confirm-action"
            disabled={rollback.isPending}
            onClick={() => {
              const target = rollbackTarget;
              setRollbackTarget(undefined);
              if (target !== undefined) doRollback(target);
            }}
          >
            {t("settings.config.rollbackConfirm")}
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}

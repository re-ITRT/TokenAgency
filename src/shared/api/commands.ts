/**
 * 类型化命令入口 —— **页面只通过这里调后端，不写裸命令字符串**。
 *
 * 命令名与入参/返回类型都来自 `bindings.ts` 的 `CommandMap`（与 [10 §2.2] 一一对应），
 * 所以拼错命令名、传错参数、用错返回类型都会在 `tsc --noEmit` 阶段被拦下。
 *
 * 错误统一是 `TokenAgencyError`（见 `client.ts`）：页面按 `err.code`（TA-xxxx）分支，
 * 不要解析 message 文案。
 */

import type {
  ApplyResult,
  AutostartMode,
  AutostartState,
  Channel,
  ChannelGroup,
  ChannelId,
  ChannelGroupId,
  ClearMode,
  CommandArgs,
  CommandMap,
  CommandName,
  ConfigDoc,
  ConnectivityRow,
  Credential,
  CredentialId,
  CredentialView,
  DashboardData,
  EchoResult,
  ImportPreview,
  KernelProvider,
  KernelStatus,
  Lane,
  LaneAssignment,
  LaneId,
  LanePickOption,
  LaneTestResult,
  LaneView,
  LogMode,
  LogPage,
  LogQuery,
  Model,
  ModelId,
  ModelPatch,
  ModelView,
  NodeView,
  OnboardingInput,
  PlaintextFinding,
  Policy,
  PolicyId,
  PriceEntry,
  ProbeResult,
  Provider,
  ProviderId,
  ProviderView,
  PreviewResult,
  QuotaState,
  RefreshResult,
  RequestId,
  RequestSummary,
  ResolvedPrice,
  Retention,
  SimulateInput,
  SimulateResult,
  Snapshot,
  StorageStats,
  StickyConfig,
  StickyKey,
  Subscription,
  SubscriptionFormat,
  SubscriptionId,
  SubscriptionView,
  TimeRange,
  TraceDetail,
  TrayState,
  UpdateInfo,
  UsageQuery,
  UsageTable,
  ValidationReport,
} from "@/shared/bindings";

import { call } from "./client";

/** 唯一调用点：把命令名与参数交给 `call`，返回类型由 `CommandMap` 决定。
 *  `useCommandQuery` / `useCommandMutation`（`queries.ts`）也复用它。 */
export function invokeCommand<K extends CommandName>(
  cmd: K,
  args: CommandArgs<K>,
): Promise<CommandMap[K]["result"]> {
  return call<CommandMap[K]["result"]>(cmd, args as Record<string, unknown> | undefined);
}

// ================================================================ app
export const app = {
  status: () => invokeCommand("app_status", undefined),
  pauseGateway: () => invokeCommand("app_pause_gateway", undefined),
  resumeGateway: () => invokeCommand("app_resume_gateway", undefined),
  openDir: (kind: "logs" | "config" | "data") => invokeCommand("app_open_dir", { kind }),
  clipboardAccessInfo: () => invokeCommand("app_clipboard_access_info", undefined),
  checkUpdate: () => invokeCommand("app_check_update", undefined) as Promise<UpdateInfo | null>,
  getAutostart: () => invokeCommand("app_get_autostart", undefined),
  setAutostart: (enabled: boolean, mode?: AutostartMode, delay_s?: number) =>
    invokeCommand("app_set_autostart", { enabled, mode, delay_s }) as Promise<AutostartState>,
  repairAutostart: () => invokeCommand("app_repair_autostart", undefined),
};

// ================================================================ config
export const config = {
  get: () => invokeCommand("config_get", undefined),
  validate: (doc: ConfigDoc) => invokeCommand("config_validate", { doc }) as Promise<ValidationReport>,
  apply: (doc: ConfigDoc) => invokeCommand("config_apply", { doc }) as Promise<ApplyResult>,
  exportYaml: (withSecrets: boolean) =>
    invokeCommand("config_export_yaml", { with_secrets: withSecrets }) as Promise<string>,
  importYaml: (yaml: string, mode: "merge" | "replace") =>
    invokeCommand("config_import_yaml", { yaml, mode }) as Promise<ApplyResult>,
  historyList: (limit = 20) => invokeCommand("config_history_list", { limit }) as Promise<Snapshot[]>,
  rollback: (snapshotId: string) =>
    invokeCommand("config_rollback", { snapshot_id: snapshotId }) as Promise<ApplyResult>,
  setFileWatchMode: (enabled: boolean, path?: string) =>
    invokeCommand("config_set_file_watch_mode", { enabled, path }) as Promise<ApplyResult>,
};

// ================================================================ provider
export const provider = {
  list: () => invokeCommand("provider_list", undefined) as Promise<ProviderView[]>,
  get: (id: ProviderId) => invokeCommand("provider_get", { id }) as Promise<ProviderView>,
  upsert: (p: Provider) => invokeCommand("provider_upsert", { provider: p }) as Promise<Provider>,
  remove: (id: ProviderId, force = false) => invokeCommand("provider_delete", { id, force }),
  test: (id: ProviderId, model?: string) =>
    invokeCommand("provider_test", { id, model }) as Promise<ProbeResult>,
  /** "谁吞了字段"的答案页。 */
  echo: (id: ProviderId, model: string, withImage: boolean) =>
    invokeCommand("provider_echo", { id, model, with_image: withImage }) as Promise<EchoResult>,
};

// ================================================================ credential / secrets
export const credential = {
  list: () => invokeCommand("credential_list", undefined) as Promise<CredentialView[]>,
  upsert: (c: Credential) => invokeCommand("credential_upsert", { credential: c }) as Promise<Credential>,
  remove: (id: CredentialId, force = false) => invokeCommand("credential_delete", { id, force }),
  setSecret: (id: CredentialId, plaintext: string) =>
    invokeCommand("credential_set_secret", { id, plaintext }),
  clearQuota: (id: CredentialId) => invokeCommand("credential_clear_quota", { id }) as Promise<QuotaState>,
  setEnabled: (id: CredentialId, enabled: boolean) =>
    invokeCommand("credential_set_enabled", { id, enabled }),
};

export const secrets = {
  scanPlaintext: () => invokeCommand("secrets_scan_plaintext", undefined) as Promise<PlaintextFinding[]>,
  migrate: (names: string[]) => invokeCommand("secrets_migrate", { names }),
};

// ================================================================ model
export const model = {
  list: () => invokeCommand("model_list", undefined) as Promise<ModelView[]>,
  upsert: (m: Model) => invokeCommand("model_upsert", { model: m }) as Promise<Model>,
  remove: (id: ModelId, force = false) => invokeCommand("model_delete", { id, force }),
  importFromProvider: (providerId: ProviderId, prefixFilter?: string) =>
    invokeCommand("model_import_from_provider", {
      provider_id: providerId,
      prefix_filter: prefixFilter,
    }) as Promise<ImportPreview>,
  /**
   * [docs/20 §1.1]：`lane` = 给这次新建的通道写的网络代理。
   * 省略 = 不写该字段（通道的 lane 留空，由五级继承链末端落到直连）。
   */
  applyImport: (previewToken: string, selected: string[], lane?: LaneId | null) =>
    invokeCommand("model_apply_import", {
      preview_token: previewToken,
      selected,
      ...(lane === undefined ? {} : { lane }),
    }) as Promise<ApplyResult>,
  bulkUpdate: (ids: ModelId[], patch: ModelPatch) =>
    invokeCommand("model_bulk_update", { ids, patch }) as Promise<ApplyResult>,
};

// ================================================================ channel / group
export const channel = {
  matrix: () => invokeCommand("channel_matrix", undefined),
  upsert: (c: Channel) => invokeCommand("channel_upsert", { channel: c }) as Promise<Channel>,
  remove: (id: ChannelId) => invokeCommand("channel_delete", { id }),
  setEnabled: (id: ChannelId, enabled: boolean) => invokeCommand("channel_set_enabled", { id, enabled }),
  bulkAssignLane: (channelIds: ChannelId[], laneId: LaneId) =>
    invokeCommand("channel_bulk_assign_lane", { channel_ids: channelIds, lane_id: laneId }) as Promise<ApplyResult>,
  setTags: (channelIds: ChannelId[], add: string[], remove: string[]) =>
    invokeCommand("channel_set_tags", { channel_ids: channelIds, add, remove }) as Promise<ApplyResult>,
};

export const group = {
  list: () => invokeCommand("group_list", undefined) as Promise<ChannelGroup[]>,
  upsert: (g: ChannelGroup) => invokeCommand("group_upsert", { group: g }) as Promise<ChannelGroup>,
  remove: (id: ChannelGroupId) => invokeCommand("group_delete", { id }),
  setSticky: (id: ChannelGroupId, sticky: StickyConfig) => invokeCommand("group_set_sticky", { id, sticky }),
  releaseSticky: (groupId: ChannelGroupId, key?: StickyKey) =>
    invokeCommand("group_release_sticky", { group_id: groupId, key }),
};

// ================================================================ policy
export const policy = {
  list: () => invokeCommand("policy_list", undefined) as Promise<Policy[]>,
  upsert: (p: Policy) => invokeCommand("policy_upsert", { policy: p }) as Promise<Policy>,
  remove: (id: PolicyId) => invokeCommand("policy_delete", { id }),
  reorder: (orderedIds: PolicyId[]) => invokeCommand("policy_reorder", { ordered_ids: orderedIds }),
  setEnabled: (id: PolicyId, enabled: boolean) => invokeCommand("policy_set_enabled", { id, enabled }),
  simulate: (input: SimulateInput) => invokeCommand("policy_simulate", { input }) as Promise<SimulateResult>,
};

// ================================================================ lane / subscription
export const lane = {
  list: () => invokeCommand("lane_list", undefined) as Promise<LaneView[]>,
  upsert: (l: Lane) => invokeCommand("lane_upsert", { lane: l }) as Promise<Lane>,
  remove: (id: LaneId) => invokeCommand("lane_delete", { id }),
  test: (id: LaneId, node?: string) =>
    invokeCommand("lane_test", { id, node }) as Promise<LaneTestResult[]>,
  setNode: (id: LaneId, node: string) => invokeCommand("lane_set_node", { id, node }),
  assignments: () => invokeCommand("lane_assignments", undefined) as Promise<LaneAssignment[]>,
  reverseLookup: (laneId: LaneId) =>
    invokeCommand("lane_reverse_lookup", { lane_id: laneId }) as Promise<{
      channel_ids: ChannelId[];
      model_ids: ModelId[];
    }>,
  pickOptions: () => invokeCommand("lane_pick_options", undefined) as Promise<LanePickOption[]>,
};

export const subscription = {
  list: () => invokeCommand("subscription_list", undefined) as Promise<SubscriptionView[]>,
  upsert: (s: Subscription) =>
    invokeCommand("subscription_upsert", { subscription: s }) as Promise<Subscription>,
  remove: (id: SubscriptionId) => invokeCommand("subscription_delete", { id }),
  refresh: (id?: SubscriptionId, force?: boolean) =>
    invokeCommand("subscription_refresh", { id, force }) as Promise<RefreshResult>,
  preview: (url: string, userAgent?: string, format?: SubscriptionFormat) =>
    invokeCommand("subscription_preview", { url, user_agent: userAgent, format }) as Promise<PreviewResult>,
  groups: (id: SubscriptionId) =>
    invokeCommand("subscription_groups", { id }),
  nodes: (id: SubscriptionId, groupName?: string) =>
    invokeCommand("subscription_nodes", { id, group: groupName }) as Promise<NodeView[]>,
  rollback: (id: SubscriptionId, versionIndex: number) =>
    invokeCommand("subscription_rollback", { id, version_index: versionIndex }),
  rulesRaw: (id: SubscriptionId) =>
    invokeCommand("subscription_rules_raw", { id }) as Promise<string | null>,
};

// ================================================================ kernel
export const kernel = {
  status: () => invokeCommand("kernel_status", undefined) as Promise<KernelStatus>,
  start: () => invokeCommand("kernel_start", undefined) as Promise<KernelStatus>,
  stop: () => invokeCommand("kernel_stop", undefined) as Promise<KernelStatus>,
  restart: () => invokeCommand("kernel_restart", undefined) as Promise<KernelStatus>,
  reloadConfig: () => invokeCommand("kernel_reload_config", undefined) as Promise<KernelStatus>,
  generatedConfig: () => invokeCommand("kernel_generated_config", undefined) as Promise<string>,
  logTail: (lines = 200) => invokeCommand("kernel_log_tail", { lines }) as Promise<string>,
  setProvider: (p: KernelProvider, path?: string) =>
    invokeCommand("kernel_set_provider", { provider: p, path }) as Promise<KernelStatus>,
  probePath: () => invokeCommand("kernel_probe_path", undefined),
};

// ================================================================ usage / pricing / log
export const usage = {
  dashboard: (range: TimeRange) => invokeCommand("usage_dashboard", { range }) as Promise<DashboardData>,
  requestStreamSnapshot: (limit = 500) =>
    invokeCommand("usage_request_stream_snapshot", { limit }) as Promise<RequestSummary[]>,
  query: (query: UsageQuery) => invokeCommand("usage_query", { query }) as Promise<UsageTable>,
  exportCsv: (query: UsageQuery, path: string) => invokeCommand("usage_export_csv", { query, path }),
  tokenKinds: () => invokeCommand("usage_token_kinds", undefined) as Promise<string[]>,
};

export const pricing = {
  list: () => invokeCommand("pricing_list", undefined) as Promise<PriceEntry[]>,
  upsert: (entry: PriceEntry) => invokeCommand("pricing_upsert", { entry }) as Promise<PriceEntry>,
  remove: (index: number) => invokeCommand("pricing_delete", { index }),
  resolve: (m: string, providerId?: ProviderId, credentialId?: CredentialId) =>
    invokeCommand("pricing_resolve", {
      model: m,
      provider: providerId,
      credential: credentialId,
    }) as Promise<ResolvedPrice>,
  importBuiltin: () => invokeCommand("pricing_import_builtin", undefined) as Promise<ApplyResult>,
  unpricedModels: () => invokeCommand("pricing_unpriced_models", undefined) as Promise<string[]>,
};

export const log = {
  query: (query: LogQuery) => invokeCommand("log_query", { query }) as Promise<LogPage>,
  getTrace: (requestId: RequestId) =>
    invokeCommand("log_get_trace", { request_id: requestId }) as Promise<TraceDetail>,
  setMode: (mode: LogMode) => invokeCommand("log_set_mode", { mode }),
  setRetention: (retention: Retention) => invokeCommand("log_set_retention", { retention }),
  storageStats: () => invokeCommand("log_storage_stats", undefined) as Promise<StorageStats>,
  clear: (mode: ClearMode) => invokeCommand("log_clear", { mode }),
  export: (query: LogQuery, path: string) => invokeCommand("log_export", { query, path }),
};

// ================================================================ onboarding / tray
export const onboarding = {
  detect: () => invokeCommand("onboarding_detect", undefined),
  apply: (input: OnboardingInput) => invokeCommand("onboarding_apply", { input }) as Promise<ApplyResult>,
  testAll: () => invokeCommand("onboarding_test_all", undefined) as Promise<ConnectivityRow[]>,
};

export const tray = {
  state: () => invokeCommand("tray_state", undefined) as Promise<TrayState>,
  setActivePolicy: (policyId: PolicyId) => invokeCommand("tray_set_active_policy", { policy_id: policyId }),
  setGlobalLane: (laneId: LaneId) => invokeCommand("tray_set_global_lane", { lane_id: laneId }),
  emergency: (mode: "direct_all" | "pause_all" | "vision_only" | "restore") =>
    invokeCommand("tray_emergency", { mode }),
};

/** 汇总入口：`import { api } from "@/shared/api"`。 */
export const api = {
  app,
  config,
  provider,
  credential,
  secrets,
  model,
  channel,
  group,
  policy,
  lane,
  subscription,
  kernel,
  usage,
  pricing,
  log,
  onboarding,
  tray,
} as const;

export type Api = typeof api;

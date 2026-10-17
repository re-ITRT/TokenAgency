/**
 * English。类型是 `Record<I18nKey, string>` —— 少一个键 `tsc --noEmit` 直接报错，
 * 这就是"加文案必须两种语言同步"的机械保证（比文档约定可靠）。
 */

import type { I18nKey } from "./zh-CN";

export const en: Record<I18nKey, string> = {
  // ---------------------------------------------------------------- shell
  "app.name": "TokenAgency",
  "app.tagline": "Your local LLM relay",
  "app.sidebar.collapse": "Collapse sidebar",
  "app.sidebar.expand": "Expand sidebar",
  "app.theme.toggle": "Toggle dark / light",
  "app.locale.toggle": "切换到中文",
  "app.badge.experimental": "Experimental",
  "app.badge.experimental.hint":
    "TUN / system proxy is on: this changes OS network state (experimental). Shown for information only — nothing here writes system settings.",
  "app.uptime": "Up {{duration}}",

  // ---------------------------------------------------------------- nav
  "nav.dashboard": "Overview",
  // sidebar section headings
  "navGroup.setup": "Setup",
  "navGroup.records": "Records",
  "navGroup.system": "System",
  "nav.providers": "Providers",
  "nav.providerDetail": "Provider detail",
  "nav.models": "Models",
  "nav.network": "Network",
  "nav.networkNodes": "Nodes",
  "nav.networkSubscription": "Subscription detail",
  "nav.cost": "Cost center",
  "nav.logs": "Request log",
  "nav.settings": "Settings",

  // ---------------------------------------------------------------- status bar
  "status.gateway.running": "Running",
  "status.gateway.paused": "Paused",
  "status.gateway.pause": "Pause gateway",
  "status.gateway.resume": "Resume gateway",
  "status.policy": "Policy",
  "status.policy.none": "No policy",
  "status.exit": "Exit",
  "status.exit.node": "node {{node}}",
  "status.latency": "{{latency}}",
  "status.node.none": "direct",
  "status.kernel.label": "Kernel",
  "status.kernel.stopped": "stopped",
  "status.kernel.starting": "starting",
  "status.kernel.ready": "ready",
  "status.kernel.restarting": "restarting",
  "status.kernel.failed": "failed",
  "status.clipboard": "Copy endpoint info",
  "status.clipboard.done": "Endpoint info copied",

  // ---------------------------------------------------------------- common
  "common.loading": "Loading…",
  "common.refresh": "Refresh",
  "common.retry": "Retry",
  "common.close": "Close",
  "common.cancel": "Cancel",
  "common.save": "Save",
  "common.confirm": "Confirm",
  "common.empty": "No data",
  "common.unknown": "Unknown",
  "common.delete": "Delete",
  "common.deleteSelected": "Delete selected",
  "delete.body": "This cannot be undone. Delete the following?",
  "delete.consequences": "Will also delete / affect:",
  "delete.blockers": "These references block the deletion (remove them first):",
  "delete.consequence.credentials": "{{value}} credential(s) under this provider",
  "delete.consequence.channels":
    "{{value}} lines are removed from your models; models left with no lines are deleted too.",
  "delete.consequence.modelLinesUntouched":
    "{{value}} lane(s) use it and will not be deleted; remove lanes on the provider page.",
  "common.selectAll": "Select all",
  "delete.cascadeHint": "It is still referenced. A cascading delete removes everything listed above and cannot be undone.",
  "delete.cascade": "Delete anyway (cascade)",
  "delete.result.ok": "Deleted {{name}}",
  "delete.result.failed": "Delete failed: {{message}}",
  "delete.result.summary": "{{ok}} succeeded, {{failed}} failed",
  "delete.selectedCount": "{{count}} selected",
  "delete.title.provider": "Delete provider",
  "delete.title.model": "Delete model",
  // These 4 keys keep the `channels.` prefix but are NOT used by the deleted page ():
  // the models page (group toggle / inherited-lane note), the provider detail column header,
  // and the provider wizard's provider field all use them.
  "channels.add.provider": "Provider",
  "channels.col.priorityWeight": "Priority / weight",
  "channels.group.toggle": "Expand or collapse this provider's lines",
  "channels.lane.inherited": "Inherited (credential / provider / default lane)",
  "delete.title.subscription": "Delete subscription",
  "delete.title.lane": "Delete lane",
  "delete.title.apiKey": "Delete access key",
  "delete.consequence.subscriptionLanes": "{{value}} lane(s) referencing this subscription",
  "delete.consequence.laneChannels": "{{value}} channel(s) currently using this lane",
  "cost.pricing.title": "Price entries",
  "cost.pricing.empty": "No price entries yet.",
  "cost.pricing.col.match": "Match",
  "cost.pricing.col.input": "Input",
  "cost.pricing.col.output": "Output",
  "cost.pricing.col.cached": "Cached read",
  "cost.pricing.col.source": "Source",
  "cost.pricing.readonly": "This page only displays. To add or change a price, set it on the model itself on the Models page.",
  "cost.pricing.help":
    "Entries match on the exposed model (the name you put in your client). Sources rank manual > imported > built-in, and exact matches beat wildcards. Cost = tokens × the rate here.",
  "cost.pricing.clear.action": "Clear price",
  "cost.pricing.clear.confirm": "Clear this model's price? It goes back to 'unpriced' and its spend stops being counted.",
  "cost.pricing.clear.staleMissing": "That price is no longer in the list: nothing was deleted — please look again.",
  "cost.pricing.clear.staleChanged":
    "The list changed: that slot now holds another model's price; nothing was deleted — please look again.",
  "cost.pricing.add.input": "Input price",
  "cost.pricing.add.output": "Output price",
  "cost.pricing.add.cached": "Cached-read price",
  "cost.pricing.add.unit": "Unit: USD per million tokens. Leave blank to not price that kind (it is not 0).",
  "cost.pricing.add.needOne": "Fill in at least one price: leaving all of them blank is not 'free'.",
  "common.none": "None",
  "common.enabled": "Enabled",
  "common.disabled": "Disabled",
  "common.yes": "Yes",
  "common.no": "No",

  // ---------------------------------------------------------------- dashboard (07 §1)
  "dashboard.readiness": "Health",
  "dashboard.gateway": "Gateway",
  "dashboard.providerCheck": "Provider load check",
  "dashboard.providerCheck.hint":
    "No check result yet — this is not inferred as OK. A healthy kernel process does not prove providers loaded.",
  "dashboard.kernel.restarts": "restarted {{count}}×",
  "dashboard.uptime": "Up",
  "dashboard.card.requests": "Requests today",
  "dashboard.card.tokens": "Tokens today",
  "dashboard.card.tokensUnit": "tok",
  "dashboard.card.cost": "Spend today",
  "dashboard.card.successRate": "Success rate",
  "dashboard.unpriced": "{{count}} models unpriced: cost is incomplete",
  "dashboard.cache": "Cache hit {{rate}} · saved {{saved}}",

  // ---------------------------------------------------------------- request log (07 §2 / 14 §12)
  "logs.description":
    "What one request went through: filters + attempts.",
  "logs.refresh": "Refresh",
  "logs.filter.title": "Query conditions",
  "logs.filter.range": "Time range",
  "logs.filter.range.today": "Today",
  "logs.filter.range.7d": "Last 7 days",
  "logs.filter.range.30d": "Last 30 days",
  "logs.filter.range.month": "This month",
  "logs.filter.requestId": "Request ID",
  "logs.filter.requestId.placeholder": "req_…",
  "logs.filter.errorCode": "Error code",
  "logs.filter.errorCode.placeholder": "TA-xxxx",
  "logs.filter.level": "Outcome",
  "logs.filter.level.ok": "OK",
  "logs.filter.level.retried": "Retried",
  "logs.filter.level.failed": "Failed",
  "logs.filter.searchExclusive":
    "Fill either a request ID or an error code, not both.",
  "logs.filter.searchScanLimit":
    "Search scans only the newest {{limit}} candidates; older matches are not found.",
  "logs.filter.errorCodeHint":
    "The error code goes through text search: it matches the error text or body content. Codes the gateway itself decided live in the attempt chain (expand a row) — the list row has no such column, so a code is not guaranteed to be filterable.",
  "logs.filter.offHint":
    "Mode off writes no request logs: the list stays empty and no filter can return anything — that does not mean nothing failed.",
  "logs.filter.levelHint": "“Retried” is decided by the attempt chain (more than one attempt).",
  "logs.filter.submit": "Query",
  "logs.filter.reset": "Reset",
  "logs.detail.copyId": "Copy request ID",
  "logs.detail.pick": "Pick a row above to see its full story",
  "logs.chips.all": "All",
  "logs.chips.ok": "OK",
  "logs.chips.failed": "Failed",
  "logs.stats.total": "Requests",
  "logs.stats.okRate": "Success",
  "logs.stats.failed": "Failed",
  "logs.stats.avgMs": "Avg latency",
  "logs.stats.unknown": "n/a",
  "logs.result.title": "Results",
  "logs.result.total": "{{value}} total",
  "logs.result.loaded": "{{value}} loaded",
  "logs.result.applied": "Applied: {{summary}}",
  "logs.result.appliedNone": "no outcome filter · no search",
  "logs.result.loadMore": "Load more",
  "logs.result.loading": "Querying…",
  "logs.result.searchNoPaging": "Search mode: all matches are returned at once.",
  "logs.empty.title": "No logs in this period.",
  "logs.col.time": "Time",
  "logs.col.requestId": "Request ID",
  "logs.col.model": "Model",
  "logs.col.channel": "Channel",
  "logs.col.status": "Outcome",
  "logs.col.latency": "Latency",
  "logs.col.tokens": "Tokens",
  "logs.col.error": "Error",
  "logs.col.cost": "Cost",
  "logs.tokens.title": "Token breakdown",
  "logs.tokens.missing": "The usage ledger has no record of this request (log retention may be longer)",
  "logs.tokens.input": "Input",
  "logs.tokens.cached": "Cached input",
  "logs.tokens.output": "Output",
  "logs.tokens.reasoning": "Reasoning",
  "logs.col.expand": "Expand",
  "logs.col.collapse": "Collapse",
  "logs.field.client": "Client",
  "logs.field.credential": "Credential",
  "logs.field.policy": "Matched policy",
  "logs.field.group": "Channel group",
  "logs.field.mode": "Recording mode",
  "logs.field.protocol": "Protocol",
  "logs.field.stream": "Streaming",
  "logs.trace.title": "Request trace",
  "logs.trace.subtitle": "Cross-component trace for one request_id (in attempt order)",
  "logs.trace.loading": "Loading trace…",
  "logs.trace.missing": "No trace for this request (past retention, cleared, or never stored).",
  "logs.trace.attempt": "Attempt {{n}}",
  "logs.trace.attempts": "Attempt chain ({{value}} attempts)",
  "logs.trace.attemptsNone": "No attempts recorded.",
  "logs.trace.channel": "Channel",
  "logs.trace.lane": "Lane",
  "logs.trace.node": "Node",
  "logs.trace.status": "Status",
  "logs.trace.reason": "Verdict",
  "logs.trace.headerDiff": "Header diff (client sent vs upstream received)",
  "logs.trace.headerDiff.empty": "No difference (this is the normal value)",
  "logs.trace.headerDiff.unknown": "Unknown (this record has no such field)",
  "logs.trace.headerDiff.found": "{{value}} differences — a non-empty diff means a gateway bug",
  "logs.trace.bodyShape": "Body shape",
  "logs.trace.requestHeaders": "Request headers",
  "logs.trace.responseHeaders": "Response headers",
  "logs.trace.requestBody": "Request body (sanitized)",
  "logs.trace.responseBody": "Response body (sanitized)",
  "logs.trace.raw": "Raw record",
  "logs.trace.rawHint": "The record verbatim, with no cleaning.",
  "logs.trace.extra": "Extra keys were returned: {{keys}} (values in the raw record below).",
  "logs.storage.title": "Recording & retention",
  "logs.storage.rows": "Rows",
  "logs.storage.bytes": "Database size",
  "logs.storage.bodyBytes": "Bodies & headers",
  "logs.storage.oldest": "Oldest",
  "logs.storage.newest": "Newest",
  "logs.storage.openDir": "Open log folder",
  "logs.storage.openDir.done": "Asked the system to open the log folder.",
  "logs.storage.openDir.failed": "Could not open the folder: {{message}}",
  "logs.storage.sanitized":
    "Sanitized before writing: keys and cookies are '*'; images keep length and hash only.",
  "logs.storage.scope":
    "The numbers above cover the request-log table. Application logs live in files with their own rotation and retention.",
  "logs.storage.failed": "Cannot read storage usage: {{message}}",
  "logs.mode.title": "Recording mode",
  "logs.mode.currentUnknown":
    "Current mode: not read (there is no read call, so we do not guess)",
  "logs.mode.currentSet": "Current mode: {{mode}} (written in this session)",
  "logs.mode.observed": "Mode recorded on the newest row: {{mode}} ({{time}})",
  "logs.mode.observedNone": "Mode recorded on the newest row: unknown (no readable rows)",
  "logs.mode.writeTime":
    "The mode belongs to write time: changing it affects later requests only, existing rows stay as they are.",
  "logs.mode.save": "Save mode",
  "logs.mode.applied": "Recording mode set to {{mode}}, effective immediately (later requests only).",
  "logs.mode.off.desc": "off: record nothing (aggregated usage is still recorded; dashboard and cost center unaffected)",
  "logs.mode.meta.desc": "meta: time, client, model, channel, status, latency, tokens, cost, error",
  "logs.mode.shape.desc": "shape: meta + body structure and types only (no content)",
  "logs.mode.full.desc": "full: meta plus request/response headers and bodies (sanitized, compressed)",
  "logs.retention.title": "Retention",
  "logs.retention.currentUnknown":
    "Current retention: not read (there is no read call, so we do not guess)",
  "logs.retention.currentSet":
    "Current retention: {{value}} (written in this session; purged hourly)",
  "logs.retention.7d": "7 days",
  "logs.retention.30d": "30 days",
  "logs.retention.90d": "90 days",
  "logs.retention.forever": "Forever",
  "logs.retention.save": "Save retention",
  "logs.retention.applied": "Retention set to {{value}}.",
  "logs.clear.title": "Clear request logs",
  "logs.clear.button": "Clear…",
  "logs.clear.mode.all": "Everything (bodies and headers included)",
  "logs.clear.mode.bodies_only": "Bodies and headers only (keep metadata)",
  "logs.clear.confirmTitle": "Confirm clearing",
  "logs.clear.confirmBody": "Clearing cannot be undone. What should be removed?",
  "logs.clear.confirm": "Clear now",
  "logs.clear.cancel": "Cancel",
  "logs.clear.done": "Deleted {{value}} rows.",
  "logs.clear.doneUnknown": "Cleared (row count not reported).",
  "logs.export.title": "Export a diagnostic package",
  "logs.export.query":
    "The export uses the currently applied filters (the same ones as the results above): {{summary}}",
  "logs.export.path": "Export path",
  "logs.export.path.placeholder": "D:\\\\exports",
  "logs.export.path.hint":
    "No default path: type a directory or a full .zip path; the written location is whatever the receipt says.",
  "logs.export.run": "Export",
  "logs.export.running": "Exporting…",
  "logs.export.written": "Wrote {{path}} ({{bytes}} bytes).",
  "logs.export.writtenNoBytes": "Wrote {{path}}.",
  "logs.export.requested":
    "Export requested to {{path}}; without a receipt this page cannot confirm it landed — check the path yourself.",
  "logs.export.error": "Export failed: {{message}}",

  // ---------------------------------------------------------------- shared components
  "component.numberCard.hintExact": "Exact {{value}}",
  "component.sparkline.label": "Last 24 hours",

  "component.statusDot.ok": "OK",
  "component.statusDot.warn": "Warning",
  "component.statusDot.error": "Error",
  "component.statusDot.info": "In progress",
  "component.statusDot.off": "Off",

  // ---------------------------------------------------------------- placeholder

  // ---------------------------------------------------------------- errors
  "error.title": "Something broke",
  "error.boundary.body":
    "This page crashed, the rest of the app still works. The details below are safe to paste into a bug report.",
  "error.code": "Code {{code}}",
  "error.fixable": "This is a configuration problem — fix it and retry.",
  "error.copyDetails": "Copy details",
  "error.reload": "Reload page",
  "error.notFound": "Page not found",
  "error.notFound.body": "There is no page at this address.",
  "error.backHome": "Back to overview",


  // ---------------------------------------------------------------- settings ( / 09 §1.2)
  "settings.description":
    "Gateway, startup, tray, log storage and configuration.",
  "settings.openConfigDir": "Open config folder",
  "settings.openDataDir": "Open data folder",
  "settings.openDir.done": "Asked the system to open the folder.",
  "settings.openDir.failed": "Could not open the folder: {{message}}",
  "settings.loadFailed": "Reading the config failed: {{message}}",
  "settings.secret.unset": "Not set",
  "settings.secret.reference": "Set (secret-store reference; the value is not shown)",
  "settings.secret.inline": "Set (plain text in the config file; consider moving it to the secret store)",

  "settings.section.service": "Service",
  "settings.section.startup": "Startup & residency",
  "settings.section.tray": "Tray behaviour",
  "settings.section.storage": "Recording & storage",
  "settings.section.config": "Configuration",
  "settings.section.about": "About",

  "settings.service.gateway": "Gateway state",
  "settings.service.listen": "Gateway listen address",
  "settings.service.adminListen": "Admin listen address",
  "settings.service.maxBodyMb": "Max request body (MB)",
  "settings.service.debugHeaders": "Echo debug response headers",
  "settings.service.downstream.openaiChat": "Serve OpenAI Chat Completions",
  "settings.service.downstream.openaiResponses": "Serve OpenAI Responses",
  "settings.service.downstream.anthropicMessages": "Serve Anthropic Messages",
  "settings.service.downstream.hint":
    "Only the checked endpoints are accepted; unchecked ones fail immediately.",
  "settings.service.localKey": "Local access key",
  "settings.service.localKey.hint":
    "This is the gateway's own local key, not an upstream key. Its value is hidden by default — use “Show access info” or “Copy access info” when you need to paste it into a client.",
  "settings.service.accessInfo.show": "Show access info",
  "settings.service.accessInfo.hide": "Hide access info",
  "settings.service.accessInfo.hint":
    "Below is the access info verbatim; this page changed nothing.",
  "settings.service.copyAccessInfo": "Copy access info",
  "settings.service.copy.done": "Copied to the system clipboard.",
  "settings.service.copy.unavailable":
    "Clipboard unavailable: the text is shown below, select it manually to copy.",
  "settings.service.editHint":
    "The inputs in this group edit a local draft only — nothing takes effect until you press “Validate & apply” under Configuration.",
  "settings.service.pause": "Pause gateway",
  "settings.service.resume": "Resume gateway",
  "settings.service.pause.done":
    "Gateway paused: it no longer accepts new requests.",
  "settings.service.resume.done": "Gateway is accepting requests again.",

  "settings.startup.autostart": "Start on login",
  "settings.startup.actual": "Actual system state: {{value}}",
  "settings.startup.desired.hint":
    "The configured wish is not a system fact: the switch shows the actual system state.",
  "settings.startup.drifted":
    "The system startup entry does not match the configured wish.",
  "settings.startup.repair": "Repair autostart",
  "settings.startup.switch.hint":
    "The switch changes the real system startup entry, not just the config.",
  "settings.startup.unsupported": "This platform does not support start-on-login: {{reason}}",
  "settings.startup.loading": "Reading the system autostart state…",
  "settings.startup.error": "Failed to read the autostart state: {{message}}",
  "settings.startup.yes": "yes",
  "settings.startup.no": "no",
  "settings.startup.mode": "Startup mode",
  "settings.startup.mode.tray": "Silently to tray",
  "settings.startup.mode.window": "Show window",
  "settings.startup.delay": "Startup delay (seconds)",
  "settings.startup.delay.hint":
    "0–300 seconds. Boot is busy; 20–60 seconds is safer.",
  "settings.startup.writeHint":
    "These two only write config; the startup entry is the switch above.",

  "settings.tray.showLatency": "Show latency in the tray",
  "settings.tray.fileWatch": "Config file watch mode",
  "settings.tray.fileWatch.hint":
    "While it is on, writing config from the UI overwrites edits made in the file.",
  "settings.tray.fileWatch.restart":
    "Config written; it is read only after a restart.",
  "settings.tray.fileWatch.appliedNoRestart": "Config written; no restart needed.",
  "settings.tray.fileWatch.notApplied":
    "This config was not written.",
  "settings.tray.fileWatch.error": "Writing the file watch switch failed: {{message}}",

  "settings.storage.stats": "Storage usage",
  "settings.storage.toLogs": "Query on the request log page",
  "settings.storage.noExport":
    "Log export lives on the Logs page.",
  // Conclusion only: the reason ("setter only, no getter — so we do not guess") is already stated on the retention line.
  "settings.storage.modeUnknown": "Current mode: not read",
  "settings.storage.modeWritten": "This session wrote the log mode as {{mode}}.",
  "settings.storage.retentionWritten": "This session wrote retention as {{value}}.",
  "settings.storage.modeEvidence": "Mode recorded on the newest log row: {{mode}} ({{time}})",
  "settings.storage.modeEvidenceNone": "Mode recorded on the newest log row: not readable (no log rows)",
  "settings.storage.maxRows": "Max log rows",
  "settings.storage.maxSizeMb": "Max log size (MB)",
  "settings.storage.sampleRate": "Sample rate (0–1)",
  "settings.storage.sampleRateHint":
    "Below 1 only some requests are logged; aggregate usage is unaffected.",
  "settings.image.title": "Image handling",
  "settings.image.hash": "Store only hash and length (recommended)",
  "settings.image.full": "Store the full image",

  "settings.config.editableTitle": "Editable settings",
  "settings.config.localFirst":
    "Edits stay local until you press “Validate and apply”.",
  "settings.config.loadFailedHint":
    "While the config is unreadable this page shows no values; fix the read error and retry.",
  "settings.config.exportTitle": "Export",
  "settings.config.importTitle": "Import",
  "settings.config.localIssues": "Local pre-check failed — fix these first:",
  "settings.config.validateAndApply": "Validate & apply",
  "settings.config.validateOnly": "Validate only",
  "settings.config.revert": "Discard changes",
  "settings.config.dirty": "{{count}} unsaved change(s): {{paths}}",
  "settings.config.clean": "No unsaved changes.",
  "settings.config.validating": "Validating…",
  "settings.config.applying": "Applying…",
  "settings.config.reportOk": "Validation passed.",
  "settings.config.reportFail": "Validation failed; applying was blocked.",
  "settings.config.reportUnknown":
    "No readable validation result — treated as failed.",
  "settings.config.errors": "Errors",
  "settings.config.warnings": "Warnings",
  "settings.config.applied": "Configuration written.",
  "settings.config.appliedDetail": "Restart required: {{restart}} · Kernel reload: {{kernel}}",
  "settings.config.export": "Export YAML",
  "settings.config.exportSecrets": "Include secret plaintext (including the local access key)",
  "settings.config.exportSecrets.warn":
    "The export will contain the local key in plain text — do not share this file.",
  "settings.config.exportHint":
    "The content is shown here; this page writes no file.",
  "settings.config.import": "Import YAML",
  "settings.config.importPlaceholder": "Paste YAML here…",
  "settings.config.importMode": "Import mode",
  "settings.config.importMode.merge": "Merge (same id is overwritten, the rest kept)",
  "settings.config.importMode.replace": "Replace (the whole config is overwritten)",
  "settings.config.importReplace.warn":
    "Replace discards the whole current config (only a snapshot is kept).",
  "settings.config.importEmpty": "Paste the YAML first.",
  "settings.config.importConfirmTitle": "Confirm replacing the whole config",
  "settings.config.importConfirmBody":
    "After replacing, the current config exists only in a snapshot. Import anyway?",
  "settings.config.importConfirm": "Import now",
  "settings.config.importDone": "Import applied.",
  "settings.config.snapshots": "Config snapshots",
  "settings.config.snapshotsEmpty": "No snapshots yet.",
  "settings.config.snapshot.label": "Label",
  "settings.config.snapshot.created": "Time",
  "settings.config.snapshot.size": "Size",
  "settings.config.rollback": "Roll back",
  "settings.config.rollbackConfirmTitle": "Confirm rollback",
  "settings.config.rollbackConfirmBody":
    "Rollback replaces the current config with this snapshot. Roll back to {{id}}?",
  "settings.config.rollbackConfirm": "Roll back now",
  "settings.config.rollbackDone": "Rolled back.",


  "settings.about.appVersion": "App version",
  "settings.about.gateway": "Gateway",
  "settings.about.kernelVersion": "Kernel version",
  "settings.about.kernelPid": "Kernel PID",
  "settings.about.kernelRestarts": "Kernel restarts",
  "settings.about.uptime": "Uptime",
  "settings.about.lastError": "Last kernel error",
  "settings.about.checkUpdate.run": "Check for updates",
  "settings.about.checkUpdate.hint":
    "It only checks when you press it; if no result arrives it reports the failure, never “up to date”.",
  "settings.about.checkUpdate.none":
    "No update information available.",
  "settings.about.checkUpdate.found": "New version: {{version}}",
  "settings.about.checkUpdate.notes": "Release notes: {{notes}}",
  "settings.about.checkUpdate.url": "Download URL: {{url}}",
  "settings.about.checkUpdate.running": "Checking…",
  "settings.about.checkUpdate.error": "Update check failed: {{message}}",
  "settings.about.license": "Licence",
  "settings.about.license.unknown":
    "No readable source; nothing is invented.",

  "settings.issue.listen": "Must be “host:port” with a port between 1 and 65535.",
  "settings.issue.maxBody": "Max request body must be an integer between 1 and 4096 (MB).",
  "settings.issue.maxRows": "Max log rows must be an integer greater than 0.",
  "settings.issue.maxSize": "Max log size must be an integer greater than 0 (MB).",
  "settings.issue.sampleRate": "Sample rate must be between 0 and 1.",
  "settings.issue.delay": "Startup delay must be between 0 and 300 seconds.",

  // ---------------------------------------------------------------- first-run wizard ( F4 / 05 §2.4)

  "onboarding.provider.secret": "Key",



  "onboarding.test.title": "Connectivity test",



  // ---------------------------------------------------------------- providers ()
  "providers.description":
    "Which providers are configured, and whether they still work.",
  "providers.refresh": "Refresh",
  "providers.tableTitle": "Provider catalogue",
  "providers.count": "{{value}} providers",
  "providers.error": "Could not read providers: {{message}}",
  "providers.credentialsError":
    "Could not read credentials: {{message}}",
  "providers.empty.title": "No providers in the config yet",
  "providers.col.name": "Name",
  "providers.col.protocol": "Protocol",
  "providers.col.status": "Status",
  "providers.col.successRate": "Success rate (24h)",
  "providers.col.latency": "Avg latency",
  "providers.col.mount": "Mounted",
  "providers.protocol.openai": "OpenAI compatible",
  "providers.protocol.openai_responses": "OpenAI Responses",
  "providers.protocol.anthropic": "Anthropic",
  "providers.protocol.gemini": "Gemini",
  "providers.protocol.custom": "Custom",
  "providers.mount.both": "credentials {{credentials}} · channels {{channels}}",
  "providers.mount.credentialsOnly": "{{credentials}} credentials · channel count unknown",
  "providers.mount.channelsOnly": "credential count unknown · {{channels}} channels",
  "providers.mount.unknown": "credential and channel counts both unknown",
  "providers.honesty.status":
    "Green = ok, yellow = attention, red = failed, grey = unknown; when it cannot be read it shows Unknown, never a false green.",
  "providers.honesty.fields":
    "When success rate, average latency or channel count cannot be read they show —, never 0.",
  "providers.honesty.mounted":
    "The credential count under Mounted is counted by each credential's owner; when the credential list cannot be read it shows unknown, never 0.",
  "providers.honesty.click":
    "Select a name to open the detail page: connection config, extra headers and passthrough, mounted credentials.",

  "providers.detail.description":
    "Base URL, key and lines, editable in place.",
  "providers.detail.error": "Could not read the provider: {{message}}",
  "providers.detail.notFound.title": "No such provider in the catalogue",
  "providers.detail.notFound.body":
    "{{id}} is not in the catalogue; the config may have just changed — refresh and look again.",
  "providers.detail.back": "Back to list",
  "providers.section.connection": "Connection",
  "providers.section.headers": "Extra headers & passthrough",
  "providers.section.raw": "Raw record",
  "providers.section.rawHint":
    "The provider record as read, shown verbatim (nothing cleaned up or trimmed).",
  "providers.field.id": "ID",
  "providers.field.name": "Name",
  "providers.field.protocol": "Protocol",
  "providers.field.baseUrl": "Base URL",
  "providers.field.enabled": "Enabled",
  "providers.field.lane": "Default lane",
  "providers.field.laneNone": "Not set (falls through the inheritance chain to the default lane or direct)",
  "providers.field.laneUnresolved": "{{id}} (lane name not readable)",
  "providers.field.status": "Health",
  "providers.field.successRate": "Success rate (24h)",
  "providers.field.latency": "Avg latency",
  "providers.field.channelCount": "Mounted channels",
  "providers.field.authScheme": "Auth scheme",
  "providers.field.authHeader": "Header / query name",
  "providers.field.authPrefix": "Value prefix",
  "providers.field.modelPrefix": "Model prefix",
  "providers.field.timeoutConnect": "Connect timeout",
  "providers.field.timeoutRead": "Read timeout",
  "providers.field.timeoutTotal": "Total timeout",
  "providers.field.tlsInsecure": "Skip certificate check",
  "providers.field.tlsExtraCa": "Extra CA",
  "providers.field.tlsMinVersion": "Minimum TLS version",
  "providers.field.headerConflict": "Same-name header conflict",
  "providers.field.passthroughHeader": "Request header passthrough",
  "providers.field.passthroughBody": "Body passthrough",
  "providers.authScheme.bearer": "Bearer token",
  "providers.authScheme.header": "Custom header",
  "providers.authScheme.query": "Query parameter",
  "providers.authScheme.none": "No auth",
  "providers.headerConflict.keep_client": "Keep the client's (fill gaps only)",
  "providers.headerConflict.override": "Override with the configured value",
  "providers.passthrough.header.pass": "pass (verbatim)",
  "providers.passthrough.header.allowlist": "allowlist (listed headers only)",
  "providers.passthrough.body.preserve": "preserve (keep the body)",
  "providers.passthrough.body.strip": "strip (drop the body)",
  "providers.tls.extraCaHint":
    "The extra CA only shows whether it is configured here; the full content is in Raw record below.",
  "providers.connection.unavailable":
    "Connection config could not be read.",
  "providers.connection.partial":
    "— means that column could not be read; leave room in the read timeout for streaming.",
  "providers.connection.hint":
    "Values that could not be read are never overwritten with defaults.",
  "providers.headers.none": "No extra headers.",
  "providers.headers.hint":
    "Extra headers fill gaps only and never override a same-name header; see Same-name header conflict above.",
  "providers.headers.col.name": "Header",
  "providers.headers.col.value": "Value",
  "providers.honesty.detailFields":
    "When health, success rate, average latency or channel count cannot be read they show — or Unknown, never a guess.",
  "providers.lane.error": "Lane name not readable: {{message}} (the lane id is shown instead)",
  "providers.credentials.error": "Could not read credentials: {{message}}",
  "providers.credentials.empty": "This provider has no credentials mounted.",
  "providers.credentials.col.label": "Name",
  "providers.credentials.col.enabled": "Enabled",
  "providers.credentials.col.lane": "Lane",
  "providers.credentials.plaintext":
    "This credential stores a plaintext secret — it should use a ${secret:name} reference",
  "providers.credentials.secretHint":
    "Secrets are shown as references (such as ${secret:name}); plaintext never appears.",

  // ---------------------------------------------------------------- provider detail: view + edit in one page
  "providers.section.basic": "Basics",
  "providers.edit.reset": "Discard changes",
  "providers.edit.clean": "No changes",
  "providers.edit.dirty": "Unsaved changes",
  "providers.edit.saved": "Saved to config",
  "providers.edit.issues": "Fix these first:",
  "providers.edit.lane": "Network route",
  "providers.edit.laneAuto": "Automatic",
  "providers.edit.laneInherit": "Not set (inherits the default lane, then direct)",
  "providers.edit.passthroughUnset": "Not set (kernel default passthrough)",
  "providers.edit.headerAdd": "Add a row",
  "providers.edit.headerRemove": "Remove this row",
  "providers.edit.headerNameRequired": "Header name is required",
  "providers.edit.headerNameDuplicate": "Duplicate header name: same-name headers overwrite each other",
  "providers.edit.health": "Health (read-only)",
  "providers.edit.keyHint": "Leave empty to keep the saved secret; only a new value is written to the secret store.",
  "providers.edit.keyNewCredential": "This provider has no credential yet: saving will create {{id}} and store the secret.",
  "providers.edit.viewOnly":
    "Connection config could not be read: this page is read-only and will not overwrite the live values.",


  // ------------------------------------------------ provider model catalog
  "providers.catalog.rawTitle": "Upstream object as recorded (we never rewrite it)",
  "providers.catalog.rawEmpty": "That upstream entry is not a JSON object (or nothing was recorded) — we will not invent one.",
  "providers.catalog.stale": "Not in the last scan",
  "providers.catalog.rescan": "Scan again",
  "providers.catalog.empty":
    "Nothing recorded yet — scan once.",
  "providers.catalog.goDetail": "Open the provider page",
  "providers.catalog.notRead": "Could not read the model catalog.",
  "providers.catalog.rescanning": "Scanning the upstream…",

  // ---------------------------------------------------------------- network (§8 / 12 §6 F4)
  // The trailing "does not change system network state" is gone from the page description:
  // the `network-zero-side-effect` block right below says exactly that, and it is the red line
  // that must stay visible. "read-only" was stale too — the page has upsert forms.
  "network.description": "Subscriptions and lanes: sources, lane kinds, node sources, health samples.",
  "network.zeroSideEffect.title": "Display only — no system network changes",
  // Red line: stays inline, but only the concrete promise a user can check. The old 89-char
  // version also carried "only reads the config" (contradicted by the write forms on the page).
  "network.zeroSideEffect.body":
    "It does not set a system proxy, write DNS, touch the routing table or a network adapter, and does not change start-on-login.",
  "network.unreadable": "Not read",
  "network.gotoNodes": "Node list",
  "network.backToOverview": "Back to network",

  "network.subscription.description":
    "Every node in this subscription, each with its own on/off switch. A disabled node stops taking part in “Auto select” and cannot be picked by a fixed-node lane.",
  "network.subscription.notFound": "No such subscription in the config ({{id}}): it may have just been deleted.",
  "network.subscription.toggleHint": "Changes apply immediately. Disabled nodes stay listed so you can turn them back on.",
  "network.subscription.auto": "Auto select",
  "network.subscription.autoWhy":
    "Every enabled node in this subscription pings the provider URL; the lowest-latency one wins, and it stays pinned by address instead of jumping around. It belongs to the whole subscription rather than to one node, so it cannot be turned off here.",
  "network.subscription.direct": "Direct",
  "network.subscription.directWhy":
    "Goes straight out over the local network with no proxy. It is a built-in kernel exit that this subscription does not provide, so it cannot be turned off here.",

  "network.currentExit.title": "Current exit",
  "network.currentExit.hint":
    "These are the current exit facts: a missing value means “Not read” — nothing is inferred, and a configured expectation never stands in for it.",
  "network.currentExit.gateway": "Gateway",
  "network.currentExit.kernel": "Kernel",
  "network.currentExit.defaultLane": "Global default lane",
  "network.currentExit.node": "Current node",

  "network.subscriptions.title": "Subscriptions",
  "network.subscriptions.description":
    "Subscription sources and the filter / health-check parameters they declare (config facts).",
  "network.subscriptions.empty": "No subscriptions yet — use “Add a subscription” to start.",
  "network.subscriptions.urlHint":
    "Secret parameters in the URL are masked and never sent out.",
  "network.subscriptions.healthHint":
    "The health-check parameters are the expectation declared in the config, not the result of the latest probe.",
  "network.subscriptions.col.name": "Name",
  "network.subscriptions.col.url": "URL",
  "network.subscriptions.col.enabled": "Enabled",
  "network.subscriptions.col.format": "Format",
  "network.subscriptions.col.interval": "Update interval",
  "network.subscriptions.col.keepVersions": "Versions kept",
  "network.subscriptions.col.filter": "Filter",
  "network.subscriptions.col.health": "Health check",
  "network.subscriptions.col.traffic": "Traffic",
  "network.subscriptions.col.nodes": "Nodes",
  "network.subscriptions.col.groups": "Groups",
  "network.subscriptions.col.lastUpdate": "Last update",
  "network.subscriptions.col.parser": "Parser",
  "network.subscriptions.col.error": "Error",
  "network.subscriptions.col.actions": "Actions",
  "network.subscriptions.row.refresh": "Refresh",
  "network.subscriptions.row.delete": "Delete",
  "network.subscriptions.row.edit": "Edit",
  "network.subscriptions.write.open": "Add a subscription",
  "network.subscriptions.write.editTitle": "Edit subscription",
  "network.subscriptions.row.refreshHint":
    "Refresh on a row forces a re-download instead of waiting for the update interval.",
  "network.subscriptions.write.title": "Create / overwrite a subscription",
  "network.subscriptions.write.hint":
    "After saving you see the object that came back, not a hand-written “saved” line. Same id = overwrite; anything left blank uses the default.",
  "network.subscriptions.write.id": "Subscription id",
  "network.subscriptions.write.idHint":
    "The id is the key: the same id overwrites an existing subscription.",
  "network.subscriptions.write.name": "Name",
  "network.subscriptions.write.url": "Subscription URL",
  "network.subscriptions.write.urlHint":
    "The URL is submitted only on save or preview; the list shows the masked form.",
  "network.subscriptions.write.urlKeepHint":
    "The URL carries a key and is not shown here: leave it empty to keep it, or type a new one to replace it.",
  // Health-check URL: optional; empty = backend default (a public URL).
  // Pointing it at a locally reachable target lets real latencies be measured offline.
  "network.subscriptions.write.healthUrl": "Health-check URL (optional)",
  "network.subscriptions.write.healthUrlHint":
    "Empty uses the default target; a locally reachable address works without internet.",
  "network.subscriptions.write.format": "Format",
  "network.subscriptions.write.interval": "Update interval (seconds)",
  "network.subscriptions.write.keepVersions": "Versions kept",
  "network.subscriptions.write.include": "Include regex (optional)",
  "network.subscriptions.write.exclude": "Exclude regex (optional)",
  "network.subscriptions.write.enabled": "Enable this subscription",
  "network.subscriptions.write.submit": "Save subscription",
  "network.subscriptions.write.preview": "Preview (no save)",
  "network.subscriptions.write.required":
    "Subscription id, name and URL are required.",
  "network.subscriptions.write.badNumber":
    "Update interval and versions kept must be numbers.",
  "network.subscriptions.write.saved": "Save result",
  "network.subscriptions.write.previewTitle":
    "Preview (nothing written)",
  "network.subscriptions.write.previewCounts": "{{groups}} groups · {{nodes}} nodes",
  "network.subscriptions.write.previewSample": "Sample nodes: {{sample}}",
  "network.subscriptions.write.refreshTitle": "Refresh result",
  "network.subscriptions.write.refreshOk": "Succeeded",
  "network.subscriptions.write.refreshFailed": "Failed",

  "network.format.auto": "Auto-detect",
  "network.format.clash": "Clash YAML",
  "network.format.base64_links": "Base64 links",
  "network.format.plain_links": "Plain links",
  "network.format.convert_links": "Convert links",

  "network.lanes.title": "Lanes",
  "network.lanes.description":
    "What each lane points at (direct / auto-select / a fixed node / a regex filter), plus the latest health sample.",
  "network.lanes.empty":
    "No lanes yet: pick a network proxy on the providers page and the lane is created for you.",
  "network.lanes.kindHint":
    "The kind says how this lane picks nodes; declared group names and narrowing regexes are listed per lane on the node list page.",
  "network.lanes.channelCountHint":
    "“Channels” is how many channels explicitly point at this lane — a different question from the node count.",
  "network.lanes.sampleHint":
    "Latency and sampling time come from the latest health sample; without a sample it is unknown.",
  "network.lanes.col.name": "Name",
  "network.lanes.col.kind": "Kind",
  "network.lanes.col.node": "Declared node",
  "network.lanes.col.enabled": "Enabled",
  "network.lanes.col.health": "Health",
  "network.lanes.col.actions": "Actions",
  "network.lanes.row.delete": "Delete",
  "network.lanes.row.setNode": "Node",
  "network.lanes.row.setNodeSubmit": "Pin",
  "network.lanes.row.setNodeHint":
    "Pinning changes runtime state only, not the config: the node name must really exist or it is rejected.",
  "network.lanes.row.setNodeEmpty":
    "The node name to pin cannot be empty.",
  "network.lanes.row.setNodeResult": "Pinned: {{lane}} → {{node}}",
  "network.lanes.row.directNoGroup": "A direct lane points straight at DIRECT and has no group to switch.",
  "network.lanes.write.id": "Lane id",
  "network.laneKind.direct": "Direct",
  "network.laneKind.subscription_auto": "Best of the whole subscription",
  "network.laneKind.subscription_node": "Fixed node",
  "network.laneKind.filter": "Best of a regex filter",

  "network.health.ok": "OK",
  "network.health.degraded": "Degraded",
  "network.health.down": "Failed",
  "network.health.unknown": "Unknown",

  // "Read-only — no speed test, no system network changes" is gone: the zero-side-effect block
  // at the top of the page states it. "Nodes parsed from the subscription" is added because the
  // first block's own description (the same sentence) was removed.
  "network.nodes.description":
    "Nodes parsed from subscriptions, the node source each lane declares, and the latest health sample.",
  // First block's own title: it used to reuse the page heading (`nav.networkNodes`),
  // so one screen showed "节点列表" twice (H1 + section heading).
  "network.nodes.list.title": "Nodes parsed from the subscription",
  "network.nodes.samples.title": "Latest health sample per lane",
  "network.nodes.samples.description":
    "The latest health sample: node, latency, result, upstream status code, sampling time.",
  "network.nodes.samples.empty":
    "No health samples yet.",
  "network.nodes.samples.col.lane": "Lane",
  "network.nodes.samples.col.kind": "Kind",
  "network.nodes.samples.col.node": "Node",
  "network.nodes.samples.col.latency": "Latency",
  "network.nodes.samples.col.ok": "Result",
  "network.nodes.samples.col.status": "Upstream status",
  "network.nodes.samples.col.sampledAt": "Sampled at",
  "network.nodes.declared.title": "Node source declared by each lane",
  "network.nodes.declared.description":
    "The group name / fixed node name / filter regex from the subscription, all shown verbatim (nothing in the user's data is replaced or cleaned).",
  "network.nodes.declared.hint":
    "These are the literal values in the config: changing them changes where the lane exits, so this page is read-only and offers no edit control.",
  "network.nodes.declared.empty": "No lane to show, so no declared node source.",
  "network.nodes.declared.none": "None",
  "network.nodes.declared.col.lane": "Lane",
  "network.nodes.declared.col.group": "Declared group name",
  "network.nodes.declared.col.groupType": "Group strategy",
  "network.nodes.declared.col.node": "Declared node",
  "network.nodes.declared.col.filter": "Narrowing regex",
  "network.nodes.declared.col.subscription": "Subscription",

  // The whole `network.assignments.*` block (~90 keys) was deleted with the page
  // (`/network/assignments`, "Lane assignments"): the five-level inheritance chain it showed
  // has only two meaningful levels left, so keeping it would suggest levels are still tunable.

  // ---------------------------------------------------------------- cost center ()
  "cost.description":
    "Usage and cost trends and detail, exportable as CSV.",
  "cost.refresh": "Refresh",
  "cost.openDataDir": "Open data directory",
  "cost.provenance.title": "What the numbers mean",
  "cost.provenance.cost":
    "Cost is a local estimate, not an upstream bill.",
  "cost.provenance.tokensNote":
    "Missing token kinds are neither filled with 0 nor guessed.",
  "cost.provenance.unpriced":
    "{{count}} models are unpriced: their tokens count toward totals, their cost is missing.",
  "cost.summary.title": "Overview (today)",
  "cost.trend.title": "Today's trend (hourly, local time zone)",
  "cost.trend.empty": "No usage today yet.",
  "cost.chart.empty": "No data to draw for the current filter.",
  "cost.chart.excluded":
    "Some rows lack these metrics, so they were dropped from the chart (no zeros): {{metrics}}",
  "cost.query.title": "Detail query",
  "cost.query.range": "Time range",
  "cost.query.range.today": "Today",
  "cost.query.range.7d": "Last 7 days",
  "cost.query.range.30d": "Last 30 days",
  "cost.query.range.month": "This month",
  "cost.query.groupBy": "Group by",
  "cost.query.group.time": "Time",
  "cost.query.group.model": "Model",
  "cost.query.group.credential": "Plan",
  "cost.query.group.provider": "Provider",
  "cost.query.group.client": "Client",
  "cost.query.bucket": "Time bucket",
  "cost.query.bucket.hour": "Hourly",
  "cost.query.bucket.day": "Daily",
  "cost.query.format": "Display",
  "cost.query.format.table": "Table",
  "cost.query.format.stacked_bar": "Stacked bars",
  "cost.query.format.area": "Area",
  "cost.query.metrics": "Metrics",
  "cost.query.metrics.hint":
    "Unknown metric kinds are shown as-is; nothing is translated away or dropped.",
  "cost.query.metrics.error":
    "Token kind list unavailable: {{message}}",
  "cost.query.submit": "Query",
  "cost.query.reset": "Reset",
  "cost.result.loading": "Querying…",
  "cost.result.empty.title": "No usage in this period.",
  "cost.result.total": "{{total}} groups",
  "cost.result.truncated":
    "Returned the first {{shown}}/{{total}} groups: narrow the time range to see all.",
  "cost.metric.requests": "Requests",
  "cost.metric.cost": "Spend",
  "cost.metric.cache_hit_rate": "Cache hit rate",
  "cost.metric.avg_latency": "Avg latency",
  "cost.token.input": "Input",
  "cost.token.input_cached": "Cache read",
  "cost.token.input_cache_write": "Cache write",
  "cost.token.input_cache_write_1h": "Cache write (1h)",
  "cost.token.input_audio": "Audio input",
  "cost.token.input_image": "Image input",
  "cost.token.output": "Output",
  "cost.token.output_reasoning": "Reasoning output",
  "cost.token.output_audio": "Audio output",
  "cost.token.output_prediction_accepted": "Prediction accepted",
  "cost.token.output_prediction_rejected": "Prediction rejected",
  "cost.export.title": "Export CSV",
  "cost.export.path": "Export path",
  "cost.export.pathHint":
    "Type a full file name (for example usage.csv); the button is disabled while the path is empty.",
  "cost.export.pick": "Choose location…",
  "cost.export.button": "Export CSV",
  "cost.export.done": "Exported to {{path}}",
  "cost.export.failed": "Export failed: {{message}}",
  "cost.unpriced.title": "Models without a price",
  "cost.unpriced.hint":
    "Their cost cannot be computed; set a price on the Models page for that model.",
  "cost.unpriced.empty": "No model is missing a price.",
  "cost.recent.title": "Recent requests (live stream snapshot)",
  "cost.recent.hint":
    "Each cost here is a local estimate too.",
  "cost.recent.empty": "No request has gone through the gateway yet.",
  "cost.col.time": "Time",
  "cost.col.model": "Model",
  "cost.col.tokens": "Tokens",
  "cost.col.cost": "Spend",

  // ---------------------------------------------------------------- model catalogue ()
  "models.description":
    "Logical model catalog: aliases, upstream mapping, pricing, channel references.",
  "models.summary.title": "Overview",
  // Add wizard (). Fixed testids: models-add-* — real-machine E2E drives them.
  "models.add.open": "Add model",
  "models.lines.title": "Lines in use",
  "models.lineToggle.onTitle": "Enable this line?",
  "models.lineToggle.offTitle": "Disable this line?",
  "models.lineToggle.shared": "These models also use this lane: {{value}}; turning it off removes one lane for them too.",
  "models.lineToggle.sharedNone": "No other model uses this line.",
  "models.lineToggle.sharedUnknown": "The model list is unreadable, so we cannot tell which models use this line.",
  "models.lineToggle.confirm": "Confirm",
  "models.lineToggle.cancel": "Cancel",
  "models.lines.pick": "Pick lines (at least one)",
  "models.lines.required": "At least one line is required: a model with no line can never be routed",
  "models.lines.none": "This model has no line yet",
  "models.lines.channelCount": "{{count}} line(s)",
  "models.lines.missing": "These line ids are not in the channel list (dangling references):",
  "models.lines.noCandidates": "No lines available yet: create lines on the provider page first",
  "models.info.open": "View model info",
  "models.edit.open": "Edit",
  "models.edit.title": "Edit model (including its lines)",
  "models.edit.idFixed": "The id cannot change: it is the public model name.",
  "models.edit.submit": "Save",
  "models.info.readonly": "Read-only",
  // What `/v1/models` exposes to clients (one aggregation on the backend)
  "models.info.public": "Exposed to clients",
  "models.info.public.context": "Context window",
  "models.info.public.maxOutput": "Max output tokens",
  "models.info.public.caps": "Capabilities",
  "models.info.public.modalities": "Input modalities",
  "models.info.public.ownedBy": "Owned by",
  "models.info.public.undeclared": "Not declared",
  "models.info.public.none": "None",
  "models.add.title": "Add model",
  "models.add.id": "Model id",
  "models.add.id.hint": "The outward-facing logical model name. Starts with a letter or digit; may contain . _ -",
  "models.add.advanced.show": "Advanced (optional)",
  "models.add.advanced.hide": "Hide advanced",
  "models.add.group": "Group (optional)",
  "models.add.group.hint": "Only affects how the UI groups it; it does not affect routing.",
  "models.add.order": "Order",
  "models.add.order.hint": "A non-negative integer; smaller comes first.",
  "models.add.expose": "Expose this model",
  "models.add.expose.hint": "When not exposed it is absent from the outward model list; channels may still reference it.",
  "models.add.submit": "Save model",
  "models.add.close": "Close",
  "models.add.result": "Saved: {{id}} ({{expose}})",
  "models.add.result.expose": "exposed",
  "models.add.result.hidden": "not exposed",
  "models.add.issue.id.required": "Model id is required.",
  "models.add.issue.id.shape": "Model id must start with a letter or digit and contain only letters, digits, . _ -",
  "models.add.issue.id.duplicate": "That model id already exists (it is already in the catalogue).",
  "models.add.issue.order.notInteger": "Order must be a non-negative integer.",
  "models.add.issue.order.negative": "Order cannot be negative.",
  "models.summary.models": "Logical models",
  "models.summary.channels": "Referencing channels",
  "models.summary.unpriced": "Unpriced",
  "models.summary.hidden": "Not exposed",
  "models.summary.hint":
    "“Referenced channels” is the de-duplicated real channel count; “unpriced” comes from the pricing unpriced list. A dimension that cannot be read shows “—” rather than 0.",
  "models.channels.error":
    "Channel list unavailable: {{message}} (missing data, not 'no references').",
  "models.unpriced.title": "Pricing",
  "models.unpriced.none": "Everything is priced.",
  "models.unpriced.hint":
    "While a model is unpriced, the cost figures it takes part in stay incomplete.",
  "models.unpriced.error":
    "Unpriced list unavailable: {{message}}",
  "models.unpriced.unknownIds": "Ids in the pricing list but not in the model catalogue: {{ids}}",
  "models.mismatch.title": "The two pricing sources disagree",
  "models.mismatch.pricedButListed":
    "The model list says these are priced, yet the pricing endpoint's unpriced list contains them: {{ids}}",
  "models.mismatch.unpricedButNotListed":
    "These models report themselves unpriced, but the pricing list does not have them: {{ids}}",
  "models.mismatch.hint":
    "Where the two sources disagree, this page does not adjudicate.",
  "models.list.title": "Model list",
  "models.search": "Filter the current list",
  "models.search.scope":
    "Filters only the {{value}} loaded models.",
  "models.search.empty":
    "No model matches the current filter; clear the box to see all {{value}}.",
  "models.loading": "Reading the model catalogue…",
  "models.col.expose": "Exposed",
  "models.col.enabled": "Enabled",
  "models.price.open": "Price",
  "models.price.openEdit": "Edit price",
  "models.price.title": "Set the price for this model",
  "models.price.modelLabel": "Exposed model",
  "models.col.model": "Logical model",
  "models.col.aliases": "Aliases",
  "models.col.group": "Group · order",
  "models.col.upstream": "Upstream mapping",
  "models.col.priced": "Pricing",
  "models.expose.on": "Exposed",
  "models.expose.off": "Not exposed",
  "models.enabled.on": "Enabled",
  "models.enabled.off": "Disabled (not listed, and calls get model-not-found)",
  "models.priced.yes": "Priced",
  "models.priced.no": "Unpriced",
  "models.priced.costHint": "Cost hint: {{hint}}",
  "models.channels.mismatch":
    "This model reports {{value}} channels, but none matched the channel list.",
  "models.channels.disabled": "disabled",
  "models.empty.title": "No logical model in the config yet",


  // ---------------------------------------------------------------- /models additions (task-18)
  // When the single upstream name equals the logical model name, do not repeat the same string
  // in a second column: two identical text nodes make "was it rewritten?" unreadable.
  "models.upstream.same": "Same as the logical model name (not rewritten)",

  // ---------------------------------------------------------------- P2 · where the real shapes differ (task-18)
  // Source: `src-tauri/src/commands/catalog.rs`. `model_list` has no `priced` / `cost_hint` column,
  // so "unpriced" can only come from pricing_unpriced_models or an explicit `priced`; neither → unknown.
  "models.priced.unknown": "Unknown",
  "models.priced.unknownHint":
    "Pricing unknown: pricing was not reported and the unpriced list is unavailable.",
  "models.summary.unpricedUnknown": "{{value}} (plus {{unknown}} with unknown pricing)",

  // Source: `catalog.rs` — `provider_list` returns the `Provider` config entity (no `status` column)
  // and `lane_list` carries its health evidence in `latest`; with no column we draw no status dot
  // and invent no health verdict.
  "wizard.required": "Required",
  "wizard.invalidId": "id may contain lowercase letters, digits and dashes only",
  "wizard.invalidUrl": "Must be a full address starting with http:// or https://",
  "wizard.duplicateId": "An entry with this id already exists",
  "providers.add.title": "Add provider",
  "credentials.add.secret": "Key (written to the secret store only, never echoed back)",
  "capabilities.vision": "Image input",
  "capabilities.tools": "Tool calling",
  "capabilities.reasoning": "Reasoning",
  "wizard.saved": "Saved: {{id}}",
  "network.sub.traffic.updated": "Updated: {{at}}",
  "network.sub.traffic.used": "Used {{used}} / {{total}}",
  "network.sub.traffic.usedOnly": "Used {{used}}",
  "network.sub.traffic.percent": "{{pct}}%",
  "network.sub.traffic.none": "Not fetched yet",
  "network.sub.traffic.expire": "Expires: {{date}}",
  "wizard.invalidNumber": "Must be a number (may be left empty)",
  "providers.wizard.step2.title": "Step 2: Create lines",
  "providers.wizard.step2.why":
    "Lines are created here; assign models on the models page.",
  "providers.plan.title": "Plan",
  "providers.plan.hint": "Quota comes from the upstream API; click refresh.",
  "providers.plan.pickProvider": "Provider",
  "providers.plan.configure": "Configure plan",
  "providers.plan.edit": "Edit plan",
  "providers.plan.remove": "Remove",
  "providers.plan.refresh": "Refresh quota",
  "providers.plan.none": "This provider has no plan yet",
  "providers.plan.step1": "Choose a template",
  "providers.plan.step2": "Fill in details",
  "providers.plan.threshold": "Alert threshold (%)",
  "providers.plan.baseUrl": "Site URL",
  "providers.plan.save": "Save",
  "providers.plan.cancel": "Cancel",
  "providers.plan.next": "Next",
  "providers.plan.failed": "No quota: {{message}}",
  "providers.wizard.gotoModels": "Assign lines on the models page",
  "providers.wizard.step1Title": "Provider details",
  "providers.wizard.stepOf": "Step {{current}} of {{total}}",
  "providers.wizard.next": "Next",
  "providers.wizard.finish": "Done",
  "providers.wizard.nameTaken": "That name is already taken, pick another",
  "providers.wizard.testOptional": "Testing the connection is optional, you can skip it",
  "providers.wizard.testOk": "Reachable, {{ms}} ms",
  "providers.wizard.testNoEndpoint": "Upstream has no /models endpoint (this is not a connectivity failure)",
  "providers.wizard.savedNotice": "The provider and credential are already written to the config; closing the wizard does not undo them",
  "providers.wizard.toProxy": "Set up the network proxy first",
  "providers.wizard.scan": "Scan upstream",
  "providers.wizard.scanEmpty": "Upstream returned no models",
  "providers.wizard.scanNeedsProvider": "Finish step 1 first (the provider must be saved before scanning)",
  "providers.wizard.scanSkipped": "{{count}} skipped",
  "network.returnBanner": "Configuring the network route for a provider",
  "network.returnBtn": "Back to wizard",
  "network.directLabel": "Direct",
  // "(direct connection)" is gone: the exit block already shows "Exit · Direct" and the default-lane
  // row shows "Direct" — this row only has to give the reason (no kernel path yet).
  "network.kernel.notConfigured": "No kernel path yet",
  "network.nodes.subscription": "Subscription",
  "network.nodes.group": "Group",
  "network.nodes.emptyUnparsed": "This subscription has not been parsed yet; press Refresh to fetch it",
  "network.nodes.emptyGroup": "This subscription has no nodes yet.",
  // Node-level enable / disable (user round 202): header select-all + two bulk actions
  "network.nodes.selectAll": "Select all",
  // The count is just a number ( minimal UI: no "N selected" wording)
  "network.nodes.selectedCount": "{{count}}",
  "network.nodes.enableSelected": "Enable selected",
  "network.nodes.disableSelected": "Disable selected",
  "network.nodes.bulk.enabled": "Enabled {{count}}",
  "network.nodes.bulk.disabled": "Disabled {{count}}",
  "network.lanes.test": "Test",
  "network.lanes.testHint": "Per-node latency test (it reports an error honestly when no kernel is assembled)",
  "network.subscriptions.parseFailed": "Parse failed",
  "network.gotoProviders": "Go to providers",
  "network.nodes.samples.emptyHint": "Probing and sampling are up to the kernel; this page never probes on its own or invents a latency number.",
  "network.subscription.builtinLocked": "Always on",
  "network.subscription.detail.title": "Subscription parameters",
  "nav.apiKeys": "API keys",
  "apiKeys.title": "API keys",
  "apiKeys.description": "Clients use these keys against the local gateway",
  "apiKeys.create": "New key",
  "apiKeys.name": "Name",
  "apiKeys.note": "Note",
  "apiKeys.createdTitle": "Key created",
  "apiKeys.createdHint": "This is the only time the plaintext is shown; save it now",
  "apiKeys.createdDone": "I saved it",
  "apiKeys.prefix": "Prefix",
  "apiKeys.enabled": "Enabled",
  "apiKeys.lastUsed": "Last used",
  "apiKeys.neverUsed": "Never used",
  "apiKeys.delete": "Delete",
  "apiKeys.deleteHint": "After deletion, requests using it start returning 401 immediately",
  "apiKeys.empty": "No API keys yet",
  "apiKeys.hashOnlyNotice": "Only a hash is stored, so the plaintext cannot be recovered from this list",
  "apiKeys.usage": "Usage",
  "apiKeys.usageEmpty": "No usage recorded yet",
  "providers.add.advanced": "Advanced",

  // ---- provider detail: basics + lines ()
  "providers.lines.title": "Lines",
  "providers.lines.hint":
    "Scanning creates a line for every upstream model (off by default); assign models on the models page.",
  "providers.lines.colName": "Name",
  "providers.lines.remove": "Remove line",
  "providers.lines.empty":
    "No lines yet — scan once above.",
  "providers.lines.notRead": "Could not read the line list.",
  "providers.lines.createdAll":
    "Scanned {{n}} models: {{k}} new lines ({{m}} in the catalog)",
  "providers.lines.all": "All",
  "providers.lines.allCount": "{{enabled}}/{{total}}",
  "providers.lines.allEnabled": "Enabled {{count}} line(s)",
  "providers.lines.allDisabled": "Disabled {{count}} line(s)",
  "providers.lines.info": "Line details",
  "providers.lines.rawNone": "Not in the scan record (this line did not come from a scan)",
  "providers.lines.missing": "{{count}} catalog entries still have no line.",
  "providers.key.title": "API key",
  "providers.key.multiple":
    "This provider has more than one credential; only the first is edited here.",
  "providers.key.multipleHint":
    "Two accounts? Add a second provider with a different name — one provider is one key.",

};

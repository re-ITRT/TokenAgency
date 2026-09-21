/**
 * English。类型是 `Record<I18nKey, string>` —— **少一个键 `tsc --noEmit` 直接报错**，
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
  "app.version": "Version {{version}}",
  "app.uptime": "Up {{duration}}",

  // ---------------------------------------------------------------- nav
  "nav.dashboard": "Overview",
  "nav.providers": "Providers",
  "nav.providerDetail": "Provider detail",
  "nav.models": "Models",
  "nav.plansAndRoutes": "Plans & Routes",
  "nav.policies": "Routing rules",
  "nav.network": "Network",
  "nav.networkNodes": "Nodes",
  "nav.networkAssignments": "Assignments",
  "nav.cost": "Cost center",
  "nav.logs": "Request log",
  "nav.settings": "Settings",
  "nav.onboarding": "Setup wizard",

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
  "status.test": "Test speed",

  // ---------------------------------------------------------------- common
  "common.loading": "Loading…",
  "common.refresh": "Refresh",
  "common.retry": "Retry",
  "common.close": "Close",
  "common.cancel": "Cancel",
  "common.save": "Save",
  "common.confirm": "Confirm",
  "common.copy": "Copy",
  "common.copied": "Copied",
  "common.empty": "No data",
  "common.unknown": "Unknown",
  "common.none": "None",
  "common.enabled": "Enabled",
  "common.disabled": "Disabled",
  "common.yes": "Yes",
  "common.no": "No",
  "common.more": "More",
  "common.detail": "Details",
  "common.exactValue": "Exact: {{value}}",
  "common.owner": "Owner",
  "common.notImplemented": "Not implemented",

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
  "dashboard.quota": "Plan quotas",
  "dashboard.cache": "Cache hit {{rate}} · saved {{saved}}",

  // ---------------------------------------------------------------- shared components
  "component.numberCard.hintExact": "Exact {{value}}",
  "component.sparkline.label": "Last 24 hours",
  "component.quotaBar.usedOf": "{{used}} / {{limit}}",
  "component.quotaBar.unlimited": "No cap set",
  "component.quotaBar.unlimitedHint":
    "Without a cap there is no auto-switch (see docs 03 §0). Consider adding one.",
  "component.quotaBar.state.ok": "OK",
  "component.quotaBar.state.soft": "Near limit",
  "component.quotaBar.state.exhausted": "Exhausted",
  "component.quotaBar.state.unknown": "Unknown",
  "component.statusDot.ok": "OK",
  "component.statusDot.warn": "Warning",
  "component.statusDot.error": "Error",
  "component.statusDot.info": "In progress",
  "component.statusDot.off": "Off",
  "component.pageHeader.docRef": "Spec: {{doc}}",

  // ---------------------------------------------------------------- placeholder
  "placeholder.title": "{{title}} (not implemented yet)",
  "placeholder.body": "Owned by {{owner}}. Routing and shell are ready.",
  "placeholder.pagePath": "Drop a page at src/pages/{{path}} and it wires itself — no layout changes.",
  "placeholder.docRef": "Fields and interactions: {{doc}}",

  // ---------------------------------------------------------------- errors
  "error.title": "Something broke",
  "error.boundary.body":
    "This page crashed, the rest of the app still works. The details below are safe to paste into a bug report.",
  "error.code": "Code {{code}}",
  "error.fixable": "This is a configuration problem — fix it and retry.",
  "error.copyDetails": "Copy details",
  "error.reload": "Reload page",
  "error.backHome": "Back to overview",
  "error.notFound": "Page not found",

  // ---------------------------------------------------------------- toasts
  "toast.autoswitched": "{{from}} quota exhausted, switched to {{to}}",
  "toast.allChannelsFailed": "All channels failed — check network/subscription",
  "toast.subscriptionFailed": "Subscription “{{name}}” failed to update; still using the previous nodes",
  "toast.lanesUnreachable": "Network unavailable — check subscription or nodes",
};

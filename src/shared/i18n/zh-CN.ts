/**
 * zh-CN —— **i18n 的真源**。键集合即 `I18nKey` 类型：
 * `en.ts` 必须实现全部键（少一个 `tsc --noEmit` 就红），路由表的 `titleKey` 也按这个类型检查。
 *
 * 命名约定：`<域>.<用途>`，域包括 nav / status / common / quota / component / error / placeholder…
 */

export const zhCN = {
  // ---------------------------------------------------------------- 应用外壳
  "app.name": "TokenAgency",
  "app.tagline": "本地 LLM 中转站",
  "app.sidebar.collapse": "收起侧边栏",
  "app.sidebar.expand": "展开侧边栏",
  "app.theme.toggle": "切换深色 / 浅色",
  "app.locale.toggle": "切换到 English",
  "app.badge.experimental": "实验模式",
  "app.version": "版本 {{version}}",
  "app.uptime": "已运行 {{duration}}",

  // ---------------------------------------------------------------- 导航（[05 §2] 的用户语言，不暴露内部概念）
  "nav.dashboard": "概览",
  "nav.providers": "服务商",
  "nav.providerDetail": "服务商详情",
  "nav.models": "模型目录",
  "nav.plansAndRoutes": "套餐与线路",
  "nav.policies": "路由策略",
  "nav.network": "网络",
  "nav.networkNodes": "节点列表",
  "nav.networkAssignments": "线路分配",
  "nav.cost": "成本中心",
  "nav.logs": "请求日志",
  "nav.settings": "设置",
  "nav.onboarding": "首启向导",

  // ---------------------------------------------------------------- 顶部状态条（[07 §1.2 ①]，永远在最上）
  "status.gateway.running": "运行中",
  "status.gateway.paused": "已暂停",
  "status.gateway.pause": "暂停网关",
  "status.gateway.resume": "恢复网关",
  "status.policy": "策略",
  "status.policy.none": "未选策略",
  "status.exit": "出口",
  "status.exit.node": "节点 {{node}}",
  "status.latency": "{{latency}}",
  "status.node.none": "直连",
  "status.kernel.label": "内核",
  "status.kernel.stopped": "未运行",
  "status.kernel.starting": "启动中",
  "status.kernel.ready": "就绪",
  "status.kernel.restarting": "重启中",
  "status.kernel.failed": "启动失败",
  "status.clipboard": "复制接入信息",
  "status.clipboard.done": "接入信息已复制",
  "status.test": "测速",

  // ---------------------------------------------------------------- 通用
  "common.loading": "加载中…",
  "common.refresh": "刷新",
  "common.retry": "重试",
  "common.close": "关闭",
  "common.cancel": "取消",
  "common.save": "保存",
  "common.confirm": "确认",
  "common.copy": "复制",
  "common.copied": "已复制",
  "common.empty": "暂无数据",
  "common.unknown": "未知",
  "common.none": "无",
  "common.enabled": "启用",
  "common.disabled": "已禁用",
  "common.yes": "是",
  "common.no": "否",
  "common.more": "更多",
  "common.detail": "详情",
  "common.exactValue": "精确值：{{value}}",
  "common.owner": "归属",
  "common.notImplemented": "未实现",

  // ---------------------------------------------------------------- 共享组件
  "component.numberCard.hintExact": "精确值 {{value}}",
  "component.sparkline.label": "近 24 小时趋势",
  "component.quotaBar.usedOf": "{{used}} / {{limit}}",
  "component.quotaBar.unlimited": "未设上限",
  "component.quotaBar.unlimitedHint": "不填额度就无法「用完自动切」（03 §0 的前提），建议补上",
  "component.quotaBar.state.ok": "正常",
  "component.quotaBar.state.soft": "接近上限",
  "component.quotaBar.state.exhausted": "已耗尽",
  "component.quotaBar.state.unknown": "未知",
  "component.statusDot.ok": "正常",
  "component.statusDot.warn": "警告",
  "component.statusDot.error": "失败",
  "component.statusDot.info": "进行中",
  "component.statusDot.off": "未启用",
  "component.pageHeader.docRef": "契约出处：{{doc}}",

  // ---------------------------------------------------------------- 占位壳（页面未实现时）
  "placeholder.title": "{{title}}（待实现）",
  "placeholder.body": "这个页面由 {{owner}} 实现，路由与外壳已就绪。",
  "placeholder.pagePath": "把页面放到 src/pages/{{path}} 即自动接线，无需改布局代码。",
  "placeholder.docRef": "字段与交互见 {{doc}}",

  // ---------------------------------------------------------------- 错误
  "error.title": "出错了",
  "error.boundary.body": "这个页面崩了，但应用其它部分还能用。下面的信息可以直接贴给我排查。",
  "error.code": "错误码 {{code}}",
  "error.fixable": "这是配置问题，可在「设置」或对应页面修正后重试。",
  "error.copyDetails": "复制详情",
  "error.reload": "重新加载页面",
  "error.backHome": "回到概览",
  "error.notFound": "页面不存在",

  // ---------------------------------------------------------------- 事件/提示
  "toast.autoswitched": "{{from}} 额度用尽，已切到 {{to}}",
  "toast.allChannelsFailed": "所有通道不可用，请检查网络/订阅",
  "toast.subscriptionFailed": "订阅「{{name}}」更新失败，仍在用上一次的节点",
  "toast.lanesUnreachable": "网络不可用，请检查订阅或节点",
} as const;

/** 全部 i18n key。新增 key 必须同时补 `en.ts`。 */
export type I18nKey = keyof typeof zhCN;

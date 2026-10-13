//! 托盘：[12 §5 T2] ①② —— 状态色图标（4 态 + 实验模式角标）与动态菜单（[05 §1.1–1.3]）。
//!
//! # 结构：把"该显示什么"和"怎么显示"分开
//!
//! * **纯逻辑**（可在无 GUI 环境单测）：[`judge_status`]（[05 §1.1] 四条触发条件）、
//!   [`render_icon`]（真的画像素）、[`build_menu_spec`]（菜单内容）、[`parse_action`]。
//! * **Tauri 适配**（薄）：[`init`] / [`refresh`] / [`set_source`]。
//!
//! 这样做的原因很实际：托盘的真机验证成本高（要开图形会话、要点菜单），
//! 而"图标颜色对不对""菜单里有没有当前策略的对勾""点了 `policy:x` 映射到什么动作"
//! 这些**全都能离线断言**。真机只留最后一层。
//!
//! # 图标为什么是画出来的
//!
//! `src-tauri/icons/` 不在 T2 的文件范围内，所以这里用纯 Rust 画一个圆点 + 锁形角标
//! （32×32 RGBA，3×3 超采样抗锯齿）。要换成设计稿 PNG 的话：把 5 张图放进 `icons/`，
//! 然后告诉我——把 [`render_icon`] 换成 `include_bytes!` 即可，接口不用动。
//!
//! # 单一逻辑来源
//!
//! [05 §1.2] 要求"UI 与托盘共用同一套命令，不重复实现逻辑"：托盘的每个菜单项都只产出
//! 一个 [`TrayAction`]，交给 [`TrayDataSource::dispatch`]（T1 的实现里调的就是 IPC 命令
//! 背后的同一批 service 函数）。

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, MenuItemKind, Submenu, SubmenuBuilder,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::lifecycle;

/// 托盘图标 id。
pub const TRAY_ID: &str = "tokenagency-tray";
/// 主窗口 label（与 `tauri.conf.json` 一致）。
pub const MAIN_WINDOW: &str = "main";
/// 图标边长（像素）。
pub const ICON_SIZE: u32 = 32;
/// `tray:refresh` 事件名（[10 §2.3]）。
pub const EVENT_TRAY_REFRESH: &str = "tray:refresh";

// ================================================================ 状态（[05 §1.1]）

/// 托盘四态（[05 §1.1]）：正常 / 降级 / 异常 / 暂停。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum TrayStatus {
    /// 绿：全部启用通道可用、无冷却
    Normal,
    /// 黄：有通道冷却 / 额度 soft 告警 / 某 lane 节点故障
    Degraded,
    /// 红：全部通道失败，或全部 lane 节点不可达
    Error,
    /// 灰：用户手动暂停网关
    Paused,
}

impl TrayStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrayStatus::Normal => "normal",
            TrayStatus::Degraded => "degraded",
            TrayStatus::Error => "error",
            TrayStatus::Paused => "paused",
        }
    }

    /// tooltip / 无障碍文案。
    pub fn label(&self) -> &'static str {
        match self {
            TrayStatus::Normal => "正常",
            TrayStatus::Degraded => "降级",
            TrayStatus::Error => "异常",
            TrayStatus::Paused => "已暂停",
        }
    }

    /// 圆点颜色（RGBA）。刻意选中深色任务栏与浅色任务栏上都能看清的中饱和色。
    pub fn color(&self) -> [u8; 4] {
        match self {
            TrayStatus::Normal => [0x2E, 0xA0, 0x43, 0xFF], // 绿
            TrayStatus::Degraded => [0xE0, 0x9B, 0x1E, 0xFF], // 黄
            TrayStatus::Error => [0xD1, 0x3B, 0x3B, 0xFF],  // 红
            TrayStatus::Paused => [0x8A, 0x8A, 0x8A, 0xFF], // 灰
        }
    }
}

/// 判定 4 态所需的健康输入（[05 §1.1] 的触发条件逐条对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HealthInput {
    /// 用户手动暂停了网关
    pub gateway_paused: bool,
    /// 启用的凭据数
    pub enabled_channels: usize,
    /// 其中当前可用的（未耗尽、未冷却、未被禁用）
    pub usable_channels: usize,
    /// 处于冷却的通道数
    pub cooling_channels: usize,
    /// 配了节点的 lane 总数
    pub lanes_total: usize,
    /// 其中有可达节点的 lane 数
    pub lanes_reachable: usize,
}

/// 由健康数据判定托盘状态（[05 §1.1]，纯函数）。
///
/// 优先级：暂停 > 异常 > 降级 > 正常。**"全部失败"必须是红**，否则用户看到绿灯
/// 但所有请求都在报错 —— 那是最伤信任的一种错。
pub fn judge_status(input: &HealthInput) -> TrayStatus {
    if input.gateway_paused {
        return TrayStatus::Paused;
    }
    let all_channels_down = input.enabled_channels == 0 || input.usable_channels == 0;
    let all_lanes_down = input.lanes_total > 0 && input.lanes_reachable == 0;
    if all_channels_down || all_lanes_down {
        return TrayStatus::Error;
    }
    let degraded = input.cooling_channels > 0
        || input.usable_channels < input.enabled_channels
        || input.lanes_reachable < input.lanes_total;
    if degraded {
        return TrayStatus::Degraded;
    }
    TrayStatus::Normal
}

// ================================================================ 图标绘制

/// 一张 RGBA 位图（行优先、自上而下），可以直接交给 Tauri。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RgbaIcon {
    /// 转成 Tauri 的 `Image`。
    pub fn to_tauri(&self) -> tauri::image::Image<'static> {
        tauri::image::Image::new_owned(self.rgba.clone(), self.width, self.height)
    }
}

/// 画托盘图标：状态色圆点 + （实验模式时）右下角锁形角标。
///
/// 确定性纯函数：同样的入参永远得到同样的像素，便于快照/断言。
pub fn render_icon(status: TrayStatus, experimental: bool) -> RgbaIcon {
    const N: u32 = ICON_SIZE;
    /// 每像素 3×3 超采样，边缘才不会有锯齿（托盘图标很小，锯齿很显眼）
    const S: u32 = 3;

    let mut rgba = vec![0u8; (N * N * 4) as usize];
    let dot_color = status.color();
    // 深色描边：浅色任务栏上绿点容易糊掉，描一圈深色能保住轮廓
    let outline = [0x1F, 0x1F, 0x1F, 0xFF];
    // 圆点中心偏左上，把右下角让给角标
    let (cx, cy) = (13.0f32, 13.0f32);
    let r = 10.0f32;
    let outline_r = r + 1.0f32;

    for y in 0..N {
        for x in 0..N {
            let mut dot_cov = 0u32;
            let mut outline_cov = 0u32;
            for sy in 0..S {
                for sx in 0..S {
                    let px = x as f32 + (sx as f32 + 0.5) / S as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / S as f32;
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    if d <= r {
                        dot_cov += 1;
                    } else if d <= outline_r {
                        outline_cov += 1;
                    }
                }
            }
            let total = (S * S) as f32;
            if outline_cov > 0 && outline_cov as f32 / total > 0.25 {
                blend(&mut rgba, x, y, outline, (outline_cov as f32 / total).min(1.0));
            }
            if dot_cov > 0 {
                blend(&mut rgba, x, y, dot_color, (dot_cov as f32 / total).min(1.0));
            }
        }
    }

    if experimental {
        draw_lock_badge(&mut rgba, N, S);
    }

    RgbaIcon { width: N, height: N, rgba }
}

/// 右下角的锁形角标（[05 §1.1]：开启 system_proxy / tun 时加锁形角标）。
fn draw_lock_badge(rgba: &mut [u8], n: u32, s: u32) {
    let body = [0xFF, 0xFF, 0xFF, 0xFF];
    let outline = [0x1F, 0x1F, 0x1F, 0xFF];
    // 锁体：矩形（留 1px 描边余量）
    let (bx0, bx1) = (18.0f32, 30.0f32);
    let (by0, by1) = (20.0f32, 30.0f32);
    // 锁梁：上半环
    let (arc_cx, arc_cy) = (24.0f32, 20.0f32);
    let (arc_r_in, arc_r_out) = (2.5f32, 4.5f32);

    for y in 0..n {
        for x in 0..n {
            let mut body_cov = 0u32;
            let mut arc_cov = 0u32;
            for sy in 0..s {
                for sx in 0..s {
                    let px = x as f32 + (sx as f32 + 0.5) / s as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / s as f32;
                    if px >= bx0 && px <= bx1 && py >= by0 && py <= by1 {
                        body_cov += 1;
                    }
                    if py <= arc_cy {
                        let d = ((px - arc_cx).powi(2) + (py - arc_cy).powi(2)).sqrt();
                        if (arc_r_in..=arc_r_out).contains(&d) {
                            arc_cov += 1;
                        }
                    }
                }
            }
            let total = (s * s) as f32;
            if body_cov == 0 && arc_cov == 0 {
                continue;
            }
            // 先描边（在形状外圈多盖一点深色），再填白
            let cov = ((body_cov + arc_cov) as f32 / total).min(1.0);
            if cov > 0.4 {
                let around = 0.35f32;
                blend(rgba, x, y, outline, around * cov);
            }
            blend(rgba, x, y, body, cov);
        }
    }
}

/// 把 `color` 以 `alpha` 覆盖率叠加到 (x, y) 像素上（源over）。
fn blend(rgba: &mut [u8], x: u32, y: u32, color: [u8; 4], alpha: f32) {
    let idx = ((y * ICON_SIZE + x) * 4) as usize;
    if idx + 3 >= rgba.len() {
        return;
    }
    let a = alpha.clamp(0.0, 1.0);
    let dst_a = rgba[idx + 3] as f32 / 255.0;
    let out_a = a + dst_a * (1.0 - a);
    if out_a <= f32::EPSILON {
        return;
    }
    for c in 0..3 {
        let src = color[c] as f32 / 255.0;
        let dst = rgba[idx + c] as f32 / 255.0;
        let out = (src * a + dst * dst_a * (1.0 - a)) / out_a;
        rgba[idx + c] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    rgba[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

// ================================================================ 应急开关（[10 §2.2]）

/// 紧急开关模式（[10 §2.2] `tray_emergency`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum EmergencyMode {
    /// 全部直连（绕过所有梯子）
    DirectAll,
    /// 全部暂停
    PauseAll,
    /// 只允许视觉请求
    VisionOnly,
    /// 恢复（撤销紧急开关）
    Restore,
}

impl EmergencyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmergencyMode::DirectAll => "direct_all",
            EmergencyMode::PauseAll => "pause_all",
            EmergencyMode::VisionOnly => "vision_only",
            EmergencyMode::Restore => "restore",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            EmergencyMode::DirectAll => "全部直连（绕过所有梯子）",
            EmergencyMode::PauseAll => "全部暂停",
            EmergencyMode::VisionOnly => "只允许视觉请求",
            EmergencyMode::Restore => "恢复正常",
        }
    }

    /// 菜单里列出的三个"进入"动作。
    pub const ENTRIES: [EmergencyMode; 3] =
        [EmergencyMode::DirectAll, EmergencyMode::PauseAll, EmergencyMode::VisionOnly];
}

// ================================================================ 数据源

/// 策略一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayPolicyItem {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// lane（出口）一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayLaneItem {
    pub id: String,
    pub name: String,
    /// 当前节点名（可选）
    pub node: Option<String>,
    /// 延迟毫秒（可选）
    pub latency_ms: Option<u32>,
    pub reachable: bool,
}

/// 通道（凭据）一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayChannelItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

/// 组装菜单与 tooltip 所需的一切（由 T1 从 state 里填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraySnapshot {
    pub status: TrayStatus,
    /// 是否处于实验模式（system_proxy / tun）→ 加锁形角标
    pub experimental: bool,
    pub gateway_running: bool,
    /// tooltip 的细节行（每行都会被拼到 "TokenAgency" 之后）
    pub tooltip_lines: Vec<String>,
    pub active_policy_id: Option<String>,
    pub policies: Vec<TrayPolicyItem>,
    /// 默认 lane 的展示名（出现在"出口节点"子菜单标题里）
    pub lane_group_label: String,
    pub default_lane_id: Option<String>,
    pub lanes: Vec<TrayLaneItem>,
    pub channels: Vec<TrayChannelItem>,
    /// 当前应急模式（决定"紧急开关"里显示恢复还是三个动作的对勾）
    pub emergency: EmergencyMode,
}

impl TraySnapshot {
    /// 未就绪时的占位（`tray::init` 到 T1 注入真数据之间会短暂用到）。
    pub fn not_ready() -> Self {
        Self {
            status: TrayStatus::Paused,
            experimental: false,
            gateway_running: false,
            tooltip_lines: vec!["启动中…".to_string()],
            active_policy_id: None,
            policies: Vec::new(),
            lane_group_label: "未配置".to_string(),
            default_lane_id: None,
            lanes: Vec::new(),
            channels: Vec::new(),
            emergency: EmergencyMode::Restore,
        }
    }

    /// 健康输入 → 状态（供 T1 组装时使用，判定逻辑只有一份）。
    pub fn with_judged_status(mut self, health: &HealthInput) -> Self {
        self.status = judge_status(health);
        self
    }
}

/// 菜单动作的**唯一**产出物：托盘只负责把 id 翻译成它，执行交给 T1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    ToggleGateway,
    UsePolicy(String),
    UseLane(String),
    RetestLanes,
    SetChannelEnabled {
        channel: String,
        enabled: bool,
    },
    Emergency(EmergencyMode),
    CopyAccessInfo,
    OpenPanel,
    OpenLogs,
    OpenConfigDir,
    OpenSettings,
    CheckUpdate,
    Quit,
    /// 不认识的 id：不静默吞掉，也不 panic（菜单可能来自旧版本或前端）
    Unknown(String),
}

impl TrayAction {
    /// 用户可读的中文动作名。
    ///
    /// 存在的理由（主管评审 B4）：给用户的文案里**不能用 `{action:?}`** ——
    /// 那会把 `UsePolicy` / `SetChannelEnabled` 这种 Rust 枚举名直接显示出来。
    /// 面向用户的托盘提示一律走这里。
    pub fn label(&self) -> &'static str {
        match self {
            TrayAction::ToggleGateway => "暂停/继续网关",
            TrayAction::UsePolicy(_) => "切换策略",
            TrayAction::UseLane(_) => "切换出口节点",
            TrayAction::RetestLanes => "重新测速",
            TrayAction::SetChannelEnabled { .. } => "启用/停用通道",
            TrayAction::Emergency(_) => "紧急开关",
            TrayAction::CopyAccessInfo => "复制接入信息",
            TrayAction::OpenPanel => "打开面板",
            TrayAction::OpenLogs => "打开日志",
            TrayAction::OpenConfigDir => "打开配置目录",
            TrayAction::OpenSettings => "打开设置",
            TrayAction::CheckUpdate => "检查更新",
            TrayAction::Quit => "退出",
            TrayAction::Unknown(_) => "未知操作",
        }
    }
}

/// 动作执行结果。**漂移/竞态要能被表达出来**（M0 实测：`PUT /configs?force=true`
/// 热重载时 listener 会有一瞬间关闭重建）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    Done,
    /// 可重试（如撞上内核热重载窗口）：托盘提示"稍后重试"而不是弹原始错误
    Retryable(String),
    /// 明确失败：给用户一句人话
    Failed(String),
}

/// 托盘的数据来源与执行入口（T1 实现；托盘与 UI 走同一套 service）。
pub trait TrayDataSource: Send + Sync + 'static {
    fn snapshot(&self) -> TraySnapshot;
    fn dispatch(&self, action: TrayAction) -> ActionOutcome;
}

/// 未注入数据源时的占位实现。
struct PlaceholderSource;

impl TrayDataSource for PlaceholderSource {
    fn snapshot(&self) -> TraySnapshot {
        TraySnapshot::not_ready()
    }
    fn dispatch(&self, _action: TrayAction) -> ActionOutcome {
        ActionOutcome::Retryable("应用还在启动，请稍候再试".to_string())
    }
}

// ================================================================ 菜单内容（纯逻辑）

/// 菜单节点（与 Tauri 无关的中间表示，可以离线断言）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuNode {
    Separator,
    Item { id: String, label: String, enabled: bool, checked: Option<bool> },
    Submenu { id: String, label: String, enabled: bool, items: Vec<MenuNode> },
}

/// 整套菜单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuSpec {
    pub items: Vec<MenuNode>,
}

/// 菜单 id 的构造器与常量。**id 是托盘与执行层之间唯一的约定**，集中在这里。
pub mod id {
    use super::EmergencyMode;

    pub const GATEWAY_TOGGLE: &str = "gateway:toggle";
    pub const POLICY_MENU: &str = "menu:policies";
    pub const LANE_MENU: &str = "menu:lanes";
    pub const CHANNEL_MENU: &str = "menu:channels";
    pub const EMERGENCY_MENU: &str = "menu:emergency";
    pub const LANE_RETEST: &str = "lane:retest";
    pub const EMERGENCY_RESTORE: &str = "emergency:restore";
    pub const COPY_ACCESS_INFO: &str = "action:copy_access_info";
    pub const OPEN_PANEL: &str = "action:open_panel";
    pub const OPEN_LOGS: &str = "action:open_logs";
    pub const OPEN_CONFIG_DIR: &str = "action:open_config_dir";
    pub const SETTINGS: &str = "action:settings";
    pub const CHECK_UPDATE: &str = "action:check_update";
    pub const QUIT: &str = "action:quit";

    pub fn policy(policy_id: &str) -> String {
        format!("policy:{policy_id}")
    }

    pub fn lane(lane_id: &str) -> String {
        format!("lane:{lane_id}")
    }

    pub fn channel_toggle(channel_id: &str, enabled: bool) -> String {
        let verb = if enabled { "enable" } else { "disable" };
        format!("channel:{channel_id}:{verb}")
    }

    pub fn emergency(mode: EmergencyMode) -> String {
        format!("emergency:{}", mode.as_str())
    }
}

/// 由快照生成菜单内容（[05 §1.2] 逐项）。
pub fn build_menu_spec(s: &TraySnapshot) -> MenuSpec {
    let mut items = Vec::new();

    // 网关运行中 [点击 = 暂停/继续]
    items.push(MenuNode::Item {
        id: id::GATEWAY_TOGGLE.to_string(),
        label: if s.gateway_running {
            "● 网关运行中（点击暂停）".to_string()
        } else {
            "○ 网关已暂停（点击继续）".to_string()
        },
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Separator);

    // 使用策略
    let mut policies = Vec::new();
    for p in &s.policies {
        policies.push(MenuNode::Item {
            id: id::policy(&p.id),
            label: if p.enabled { p.name.clone() } else { format!("{}（已停用）", p.name) },
            enabled: p.enabled,
            checked: Some(s.active_policy_id.as_deref() == Some(p.id.as_str())),
        });
    }
    items.push(MenuNode::Submenu {
        id: id::POLICY_MENU.to_string(),
        label: "使用策略".to_string(),
        enabled: !s.policies.is_empty(),
        items: policies,
    });

    // 出口节点
    let mut lanes = Vec::new();
    for l in &s.lanes {
        let mut label = l.name.clone();
        if let Some(node) = &l.node {
            label.push(' ');
            label.push_str(node);
        }
        if let Some(ms) = l.latency_ms {
            label.push_str(&format!("  {ms}ms"));
        }
        if !l.reachable {
            label.push_str("（不可达）");
        }
        lanes.push(MenuNode::Item {
            id: id::lane(&l.id),
            label,
            enabled: l.reachable,
            checked: Some(s.default_lane_id.as_deref() == Some(l.id.as_str())),
        });
    }
    // ⟳ 立即测速：只要有节点可测就可用
    lanes.push(MenuNode::Separator);
    lanes.push(MenuNode::Item {
        id: id::LANE_RETEST.to_string(),
        label: "立即测速".to_string(),
        enabled: !s.lanes.is_empty(),
        checked: None,
    });
    items.push(MenuNode::Submenu {
        id: id::LANE_MENU.to_string(),
        label: format!("出口节点（全局默认 lane：{}）", s.lane_group_label),
        enabled: !s.lanes.is_empty(),
        items: lanes,
    });

    // 通道状态（每项带"禁用/启用"）
    let mut channels = Vec::new();
    for c in &s.channels {
        let children = vec![MenuNode::Item {
            id: id::channel_toggle(&c.id, !c.enabled),
            label: if c.enabled {
                "禁用该凭据".to_string()
            } else {
                "启用该凭据".to_string()
            },
            enabled: true,
            checked: None,
        }];
        channels.push(MenuNode::Submenu {
            id: format!("channel:{}", c.id),
            label: c.label.clone(),
            enabled: true,
            items: children,
        });
    }
    items.push(MenuNode::Submenu {
        id: id::CHANNEL_MENU.to_string(),
        label: "通道状态".to_string(),
        enabled: !s.channels.is_empty(),
        items: channels,
    });

    // 紧急开关（[05 §1.2] 三项 + 恢复）
    let mut emergency = Vec::new();
    for mode in EmergencyMode::ENTRIES {
        emergency.push(MenuNode::Item {
            id: id::emergency(mode),
            label: mode.label().to_string(),
            enabled: true,
            checked: Some(s.emergency == mode),
        });
    }
    emergency.push(MenuNode::Separator);
    emergency.push(MenuNode::Item {
        id: id::EMERGENCY_RESTORE.to_string(),
        label: EmergencyMode::Restore.label().to_string(),
        enabled: s.emergency != EmergencyMode::Restore,
        checked: Some(s.emergency == EmergencyMode::Restore),
    });
    items.push(MenuNode::Submenu {
        id: id::EMERGENCY_MENU.to_string(),
        label: "紧急开关".to_string(),
        enabled: true,
        items: emergency,
    });

    items.push(MenuNode::Separator);
    items.push(MenuNode::Item {
        id: id::COPY_ACCESS_INFO.to_string(),
        label: "复制接入信息 (BASE_URL + KEY)".to_string(),
        enabled: s.gateway_running,
        checked: None,
    });
    items.push(MenuNode::Item {
        id: id::OPEN_PANEL.to_string(),
        label: "打开面板".to_string(),
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Item {
        id: id::OPEN_LOGS.to_string(),
        label: "打开日志".to_string(),
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Item {
        id: id::OPEN_CONFIG_DIR.to_string(),
        label: "打开配置目录".to_string(),
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Item {
        id: id::SETTINGS.to_string(),
        label: "设置".to_string(),
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Item {
        id: id::CHECK_UPDATE.to_string(),
        label: "检查更新".to_string(),
        enabled: true,
        checked: None,
    });
    items.push(MenuNode::Separator);
    items.push(MenuNode::Item {
        id: id::QUIT.to_string(),
        label: "⏻ 退出".to_string(),
        enabled: true,
        checked: None,
    });

    MenuSpec { items }
}

/// tooltip：[05 §1.1] `TokenAgency · 策略: … · 出口: …` + 第二行用量。
///
/// 第一行恒为 `TokenAgency`，空行会被丢掉（避免 tooltip 里出现空行）。
pub fn format_tooltip(s: &TraySnapshot) -> String {
    let mut lines: Vec<String> =
        s.tooltip_lines.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        lines.push(format!("状态：{}", s.status.label()));
    }
    let mut out = String::from("TokenAgency");
    for l in lines {
        out.push('\n');
        out.push_str(&l);
    }
    out
}

/// 菜单 id → 动作（[05 §1.2] 的每一条）。
pub fn parse_action(menu_id: &str) -> TrayAction {
    match menu_id {
        id::GATEWAY_TOGGLE => return TrayAction::ToggleGateway,
        id::LANE_RETEST => return TrayAction::RetestLanes,
        id::COPY_ACCESS_INFO => return TrayAction::CopyAccessInfo,
        id::OPEN_PANEL => return TrayAction::OpenPanel,
        id::OPEN_LOGS => return TrayAction::OpenLogs,
        id::OPEN_CONFIG_DIR => return TrayAction::OpenConfigDir,
        id::SETTINGS => return TrayAction::OpenSettings,
        id::CHECK_UPDATE => return TrayAction::CheckUpdate,
        id::QUIT => return TrayAction::Quit,
        id::EMERGENCY_RESTORE => return TrayAction::Emergency(EmergencyMode::Restore),
        _ => {}
    }

    if let Some(rest) = menu_id.strip_prefix("policy:") {
        return TrayAction::UsePolicy(rest.to_string());
    }
    if let Some(rest) = menu_id.strip_prefix("lane:") {
        return TrayAction::UseLane(rest.to_string());
    }
    if let Some(rest) = menu_id.strip_prefix("emergency:") {
        let mode = match rest {
            "direct_all" => Some(EmergencyMode::DirectAll),
            "pause_all" => Some(EmergencyMode::PauseAll),
            "vision_only" => Some(EmergencyMode::VisionOnly),
            "restore" => Some(EmergencyMode::Restore),
            _ => None,
        };
        if let Some(m) = mode {
            return TrayAction::Emergency(m);
        }
    }
    if let Some(rest) = menu_id.strip_prefix("channel:") {
        // 形如 `channel:<id>:enable|disable`
        if let Some((channel, verb)) = rest.rsplit_once(':') {
            match verb {
                "enable" => {
                    return TrayAction::SetChannelEnabled {
                        channel: channel.to_string(),
                        enabled: true,
                    }
                }
                "disable" => {
                    return TrayAction::SetChannelEnabled {
                        channel: channel.to_string(),
                        enabled: false,
                    }
                }
                _ => {}
            }
        }
    }

    TrayAction::Unknown(menu_id.to_string())
}

// ================================================================ IPC DTO

/// `tray_state` 命令与 `tray:refresh` 事件的载荷（[10 §2.2]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayState {
    pub status: TrayStatus,
    pub tooltip: String,
    pub experimental: bool,
    pub gateway_running: bool,
    pub emergency: EmergencyMode,
    pub active_policy_id: Option<String>,
    pub default_lane_id: Option<String>,
    /// 菜单项状态（前端可以照着重绘同一个菜单，保证"逻辑只有一份"）
    pub menu: MenuSpec,
}

impl TrayState {
    pub fn from_snapshot(s: &TraySnapshot) -> Self {
        Self {
            status: s.status,
            tooltip: format_tooltip(s),
            experimental: s.experimental,
            gateway_running: s.gateway_running,
            emergency: s.emergency,
            active_policy_id: s.active_policy_id.clone(),
            default_lane_id: s.default_lane_id.clone(),
            menu: build_menu_spec(s),
        }
    }
}

// ================================================================ Tauri 适配层

/// 托盘数据源持有者（T1 通过 [`set_source`] 注入真正的实现）。
struct TrayHolder {
    source: RwLock<Arc<dyn TrayDataSource>>,
}

fn current_snapshot<R: Runtime>(app: &AppHandle<R>) -> TraySnapshot {
    match app.try_state::<TrayHolder>() {
        Some(holder) => {
            let source = holder.source.read().unwrap_or_else(|e| e.into_inner()).clone();
            source.snapshot()
        }
        None => TraySnapshot::not_ready(),
    }
}

/// 安装托盘（接缝：`lifecycle::on_setup` 之后由 `lib.rs` 调用）。
///
/// 此时 T1 的 `AppState` 可能还没建好，所以先用占位数据源；T1 随后调 [`set_source`]。
pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if app.try_state::<TrayHolder>().is_none() {
        app.manage(TrayHolder { source: RwLock::new(Arc::new(PlaceholderSource)) });
    }

    let snapshot = current_snapshot(app);
    let menu = build_menu(app, &build_menu_spec(&snapshot))?;
    let icon = render_icon(snapshot.status, snapshot.experimental).to_tauri();

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(format_tooltip(&snapshot))
        .menu(&menu)
        // [05 §1.2]：左键 = 显示主窗口，右键才出菜单
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_string();
            handle_menu_event(app, &id);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    tracing::info!(tray = TRAY_ID, status = snapshot.status.as_str(), "托盘已安装");
    Ok(())
}

/// T1 注入真正的数据源（`AppState` 就绪后调用一次）。
pub fn set_source<R: Runtime>(
    app: &AppHandle<R>,
    source: Arc<dyn TrayDataSource>,
) -> tauri::Result<()> {
    match app.try_state::<TrayHolder>() {
        Some(holder) => {
            *holder.source.write().unwrap_or_else(|e| e.into_inner()) = source;
        }
        None => {
            app.manage(TrayHolder { source: RwLock::new(source) });
        }
    }
    refresh(app)
}

/// 重画图标 / tooltip / 菜单，并广播 `tray:refresh`（[10 §2.3]）。
///
/// 菜单在每次刷新时整体重建：动态内容（额度百分比、延迟）本来就在文本里，
/// 重建最直观；[05 §1.2] 提到的 `set_text()` 优化留给"菜单展开时高频刷新"的场景。
pub fn refresh<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let snapshot = current_snapshot(app);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_icon(Some(render_icon(snapshot.status, snapshot.experimental).to_tauri()))?;
        tray.set_tooltip(Some(format_tooltip(&snapshot)))?;
        tray.set_menu(Some(build_menu(app, &build_menu_spec(&snapshot))?))?;
    }
    let state = TrayState::from_snapshot(&snapshot);
    let _ = app.emit(EVENT_TRAY_REFRESH, state);
    Ok(())
}

/// 给 `tray_state` 命令用：当前托盘状态（含菜单）。
pub fn tray_state<R: Runtime>(app: &AppHandle<R>) -> TrayState {
    TrayState::from_snapshot(&current_snapshot(app))
}

/// 显示并聚焦主窗口（左键 / 双击 / "打开面板"）。
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, menu_id: &str) {
    let action = parse_action(menu_id);

    // 退出与"打开面板"由托盘自己处理（不需要业务数据）
    match &action {
        TrayAction::Quit => {
            lifecycle::request_quit(app);
            return;
        }
        TrayAction::OpenPanel => {
            show_main_window(app);
            return;
        }
        TrayAction::Unknown(id) => {
            // 不静默：旧版菜单/前端传来的 id 会被记下来，方便排查
            tracing::warn!(menu_id = %id, "托盘收到未知菜单项 id，已忽略");
            return;
        }
        _ => {}
    }

    let outcome = dispatch(app, action.clone());
    match outcome {
        ActionOutcome::Done => {}
        ActionOutcome::Retryable(msg) => {
            // M0 实测：内核热重载（PUT /configs?force=true）时 listener 会短暂重建，
            // 撞上就提示重试，而不是把原始错误糊到用户脸上
            lifecycle::toast(app, lifecycle::ToastLevel::Warn, "稍后重试", &msg, "tray:retry");
        }
        ActionOutcome::Failed(msg) => {
            lifecycle::toast(app, lifecycle::ToastLevel::Error, "操作失败", &msg, "tray:failed");
        }
    }
    let _ = refresh(app);
}

fn dispatch<R: Runtime>(app: &AppHandle<R>, action: TrayAction) -> ActionOutcome {
    match app.try_state::<TrayHolder>() {
        Some(holder) => {
            let source = holder.source.read().unwrap_or_else(|e| e.into_inner()).clone();
            source.dispatch(action)
        }
        None => ActionOutcome::Retryable("应用还在启动，请稍候再试".to_string()),
    }
}

// ---------------------------------------------------------------- 菜单构建（Tauri 类型）

fn build_menu<R: Runtime>(app: &AppHandle<R>, spec: &MenuSpec) -> tauri::Result<Menu<R>> {
    let mut builder = MenuBuilder::new(app);
    for node in &spec.items {
        builder = append_to_menu(app, builder, node)?;
    }
    builder.build()
}

fn append_to_menu<'m, R: Runtime>(
    app: &'m AppHandle<R>,
    builder: MenuBuilder<'m, R, AppHandle<R>>,
    node: &MenuNode,
) -> tauri::Result<MenuBuilder<'m, R, AppHandle<R>>> {
    match node {
        MenuNode::Separator => Ok(builder.separator()),
        MenuNode::Item { id, label, enabled, checked } => {
            let item = build_leaf(app, id, label, *enabled, *checked)?;
            Ok(builder.item(&item))
        }
        MenuNode::Submenu { id, label, enabled, items } => {
            let submenu = build_submenu(app, id, label, *enabled, items)?;
            Ok(builder.item(&submenu))
        }
    }
}

fn append_to_submenu<'m, R: Runtime>(
    app: &'m AppHandle<R>,
    builder: SubmenuBuilder<'m, R, AppHandle<R>>,
    node: &MenuNode,
) -> tauri::Result<SubmenuBuilder<'m, R, AppHandle<R>>> {
    match node {
        MenuNode::Separator => Ok(builder.separator()),
        MenuNode::Item { id, label, enabled, checked } => {
            let item = build_leaf(app, id, label, *enabled, *checked)?;
            Ok(builder.item(&item))
        }
        MenuNode::Submenu { id, label, enabled, items } => {
            let submenu = build_submenu(app, id, label, *enabled, items)?;
            Ok(builder.item(&submenu))
        }
    }
}

fn build_leaf<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    label: &str,
    enabled: bool,
    checked: Option<bool>,
) -> tauri::Result<MenuItemKind<R>> {
    match checked {
        Some(checked) => Ok(MenuItemKind::Check(
            CheckMenuItemBuilder::with_id(id, label)
                .checked(checked)
                .enabled(enabled)
                .build(app)?,
        )),
        None => Ok(MenuItemKind::MenuItem(
            MenuItemBuilder::with_id(id, label).enabled(enabled).build(app)?,
        )),
    }
}

fn build_submenu<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    label: &str,
    enabled: bool,
    items: &[MenuNode],
) -> tauri::Result<Submenu<R>> {
    let mut builder = SubmenuBuilder::with_id(app, id, label).enabled(enabled);
    for node in items {
        builder = append_to_submenu(app, builder, node)?;
    }
    builder.build()
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------- 4 态判定（[05 §1.1]）

    #[test]
    fn judge_status_covers_all_four_states() {
        let healthy = HealthInput {
            gateway_paused: false,
            enabled_channels: 3,
            usable_channels: 3,
            cooling_channels: 0,
            lanes_total: 2,
            lanes_reachable: 2,
        };
        assert_eq!(judge_status(&healthy), TrayStatus::Normal);

        // 暂停优先于一切（哪怕同时有冷却）
        let paused = HealthInput { gateway_paused: true, cooling_channels: 1, ..healthy };
        assert_eq!(judge_status(&paused), TrayStatus::Paused);

        // 冷却 / 通道减少 / lane 不可达 → 降级
        assert_eq!(
            judge_status(&HealthInput { cooling_channels: 1, ..healthy }),
            TrayStatus::Degraded
        );
        assert_eq!(
            judge_status(&HealthInput { usable_channels: 2, ..healthy }),
            TrayStatus::Degraded
        );
        assert_eq!(
            judge_status(&HealthInput { lanes_reachable: 1, ..healthy }),
            TrayStatus::Degraded
        );

        // 全部通道不可用 / 全部 lane 不可达 / 一个通道都没启用 → 异常
        assert_eq!(judge_status(&HealthInput { usable_channels: 0, ..healthy }), TrayStatus::Error);
        assert_eq!(judge_status(&HealthInput { lanes_reachable: 0, ..healthy }), TrayStatus::Error);
        assert_eq!(
            judge_status(&HealthInput { enabled_channels: 0, usable_channels: 0, ..healthy }),
            TrayStatus::Error
        );
        // 没配 lane 不算异常（直连场景）
        assert_eq!(judge_status(&HealthInput { lanes_total: 0, ..healthy }), TrayStatus::Normal);
    }

    // ---------------------------------------------------------- 图标

    fn pixel(icon: &RgbaIcon, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * icon.width + x) * 4) as usize;
        [icon.rgba[i], icon.rgba[i + 1], icon.rgba[i + 2], icon.rgba[i + 3]]
    }

    fn opaque_count(icon: &RgbaIcon) -> usize {
        icon.rgba.chunks(4).filter(|p| p[3] > 0).count()
    }

    #[test]
    fn icon_has_expected_geometry_and_is_deterministic() {
        let icon = render_icon(TrayStatus::Normal, false);
        assert_eq!((icon.width, icon.height), (ICON_SIZE, ICON_SIZE));
        assert_eq!(icon.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        assert_eq!(icon, render_icon(TrayStatus::Normal, false), "同样的入参必须得到同样的像素");

        // 四角透明（圆点 + 角标都不该碰到角）
        assert_eq!(pixel(&icon, 0, 0)[3], 0);
        assert_eq!(pixel(&icon, ICON_SIZE - 1, 0)[3], 0);
        assert_eq!(pixel(&icon, 0, ICON_SIZE - 1)[3], 0);

        // 圆心是不透明的状态色
        let center = pixel(&icon, 13, 13);
        assert_eq!(center[3], 255);
        let expected = TrayStatus::Normal.color();
        for c in 0..3 {
            assert!(
                (center[c] as i32 - expected[c] as i32).abs() <= 2,
                "圆心颜色应接近状态色：{center:?} vs {expected:?}"
            );
        }
        // 圆点不该占满整张图（右下角要留给角标）
        assert!(opaque_count(&icon) < (ICON_SIZE * ICON_SIZE) as usize / 2);
    }

    #[test]
    fn four_statuses_have_four_distinct_icons() {
        let icons: Vec<RgbaIcon> =
            [TrayStatus::Normal, TrayStatus::Degraded, TrayStatus::Error, TrayStatus::Paused]
                .iter()
                .map(|s| render_icon(*s, false))
                .collect();
        for i in 0..icons.len() {
            for j in (i + 1)..icons.len() {
                assert_ne!(icons[i], icons[j], "四态图标必须两两不同（第 {i} 与第 {j}）");
            }
        }
        // 颜色本身也要不同（前端也可能用 color 画指示器）
        let mut colors: Vec<[u8; 4]> =
            [TrayStatus::Normal, TrayStatus::Degraded, TrayStatus::Error, TrayStatus::Paused]
                .iter()
                .map(|s| s.color())
                .collect();
        colors.sort_unstable();
        let n = colors.len();
        colors.dedup();
        assert_eq!(colors.len(), n, "四态颜色必须互不相同");
    }

    #[test]
    fn experimental_adds_a_lock_badge_without_moving_the_dot() {
        let plain = render_icon(TrayStatus::Normal, false);
        let locked = render_icon(TrayStatus::Normal, true);
        assert_ne!(plain, locked, "实验模式必须画出角标");

        // 角标在右下：该区域变了……
        let badge_changed =
            (18..32).any(|y| (18..32).any(|x| pixel(&plain, x, y) != pixel(&locked, x, y)));
        assert!(badge_changed, "右下角应出现锁形角标");
        // ……而圆点区域一个像素都不能变
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(
                    pixel(&plain, x, y),
                    pixel(&locked, x, y),
                    "({x},{y}) 圆点区域不该受角标影响"
                );
            }
        }
    }

    #[test]
    fn icon_converts_to_tauri_image() {
        let img = render_icon(TrayStatus::Degraded, true).to_tauri();
        assert_eq!(img.width(), ICON_SIZE);
        assert_eq!(img.height(), ICON_SIZE);
        assert_eq!(img.rgba().len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    // ---------------------------------------------------------- 菜单内容

    fn snapshot() -> TraySnapshot {
        TraySnapshot {
            status: TrayStatus::Degraded,
            experimental: true,
            gateway_running: true,
            tooltip_lines: vec![
                "策略: 便宜优先 · 出口: 东京-03 (82ms)".to_string(),
                "今日 1.24M tok · ¥3.42 · 中转A 82%".to_string(),
            ],
            active_policy_id: Some("cheap".to_string()),
            policies: vec![
                TrayPolicyItem {
                    id: "default".into(),
                    name: "默认（粘性+优先级）".into(),
                    enabled: true,
                },
                TrayPolicyItem { id: "cheap".into(), name: "便宜优先".into(), enabled: true },
                TrayPolicyItem { id: "off".into(), name: "已停用策略".into(), enabled: false },
            ],
            lane_group_label: "日本-自动择优".to_string(),
            default_lane_id: Some("jp".to_string()),
            lanes: vec![
                TrayLaneItem {
                    id: "jp".into(),
                    name: "日本-自动择优".into(),
                    node: Some("东京-03".into()),
                    latency_ms: Some(82),
                    reachable: true,
                },
                TrayLaneItem {
                    id: "us".into(),
                    name: "美国-洛杉矶-01".into(),
                    node: None,
                    latency_ms: None,
                    reachable: false,
                },
            ],
            channels: vec![TrayChannelItem {
                id: "relay-a".into(),
                label: "中转站A".into(),
                enabled: true,
            }],
            emergency: EmergencyMode::Restore,
        }
    }

    fn find_submenu<'a>(spec: &'a MenuSpec, id: &str) -> &'a Vec<MenuNode> {
        spec.items
            .iter()
            .find_map(|n| match n {
                MenuNode::Submenu { id: got, items, .. } if got == id => Some(items),
                _ => None,
            })
            .unwrap_or_else(|| panic!("菜单里找不到子菜单 {id}"))
    }

    #[test]
    fn menu_contains_every_section_from_the_design() {
        let spec = build_menu_spec(&snapshot());
        // 顶层：网关开关 + 4 个子菜单 + 6 个动作 + 退出
        let submenus = spec.items.iter().filter(|n| matches!(n, MenuNode::Submenu { .. })).count();
        assert_eq!(submenus, 4, "应有 策略/出口节点/通道状态/紧急开关 四个子菜单");
        for id in [id::POLICY_MENU, id::LANE_MENU, id::CHANNEL_MENU, id::EMERGENCY_MENU] {
            find_submenu(&spec, id);
        }
        for want in [
            id::GATEWAY_TOGGLE,
            id::COPY_ACCESS_INFO,
            id::OPEN_PANEL,
            id::OPEN_LOGS,
            id::OPEN_CONFIG_DIR,
            id::SETTINGS,
            id::CHECK_UPDATE,
            id::QUIT,
        ] {
            assert!(
                spec.items.iter().any(|n| matches!(n, MenuNode::Item { id, .. } if id == want)),
                "顶层缺少菜单项 {want}"
            );
        }
        // 网关开关的文案跟着运行态走
        let gateway = spec
            .items
            .iter()
            .find_map(|n| match n {
                MenuNode::Item { id, label, .. } if id == id::GATEWAY_TOGGLE => Some(label.clone()),
                _ => None,
            })
            .expect("网关开关");
        assert!(gateway.contains("运行中"), "{gateway}");
        assert!(build_menu_spec(&TraySnapshot { gateway_running: false, ..snapshot() })
            .items
            .iter()
            .any(|n| matches!(n, MenuNode::Item { label, .. } if label.contains("已暂停"))));
    }

    #[test]
    fn menu_marks_the_active_policy_and_lane_exactly_once() {
        let spec = build_menu_spec(&snapshot());
        let policies = find_submenu(&spec, id::POLICY_MENU);
        let checked: Vec<&str> = policies
            .iter()
            .filter_map(|n| match n {
                MenuNode::Item { id, checked: Some(true), .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(checked, vec![id::policy("cheap").as_str()], "当前策略只应有一个对勾");

        // 停用的策略不可点
        let disabled_enabled = policies
            .iter()
            .find_map(|n| match n {
                MenuNode::Item { id, enabled, .. } if id == &id::policy("off") => Some(*enabled),
                _ => None,
            })
            .expect("停用策略项");
        assert!(!disabled_enabled);

        let lanes = find_submenu(&spec, id::LANE_MENU);
        assert_eq!(
            lanes
                .iter()
                .filter(|n| matches!(n, MenuNode::Item { checked: Some(true), .. }))
                .count(),
            1,
            "默认 lane 只应有一个对勾"
        );
        // 不可达的 lane 不可点，但仍在列表里（用户要能看见"它挂了"）
        let us = lanes
            .iter()
            .find_map(|n| match n {
                MenuNode::Item { id, label, enabled, .. } if id == &id::lane("us") => {
                    Some((label.clone(), *enabled))
                }
                _ => None,
            })
            .expect("us lane");
        assert!(!us.1);
        assert!(us.0.contains("不可达"), "{}", us.0);
        // 测速项恒在最后
        assert!(matches!(
            lanes.last(),
            Some(MenuNode::Item { id, .. }) if id == id::LANE_RETEST
        ));
    }

    #[test]
    fn lane_submenu_title_shows_the_default_lane_group() {
        let spec = build_menu_spec(&snapshot());
        let title = spec
            .items
            .iter()
            .find_map(|n| match n {
                MenuNode::Submenu { id, label, .. } if id == id::LANE_MENU => Some(label.clone()),
                _ => None,
            })
            .expect("出口节点子菜单");
        assert!(title.contains("日本-自动择优"), "{title}");
    }

    #[test]
    fn channel_section_exposes_the_toggle_action() {
        let spec = build_menu_spec(&snapshot());
        let channels = find_submenu(&spec, id::CHANNEL_MENU);
        let node =
            channels.iter().find(|n| matches!(n, MenuNode::Submenu { .. })).expect("通道子菜单");
        let (sub_id, label, items) = match node {
            MenuNode::Submenu { id: sub_id, label, items, .. } => (sub_id, label, items),
            other => panic!("通道项应是子菜单，实际 {other:?}"),
        };
        assert_eq!(sub_id, "channel:relay-a");
        assert_eq!(label, "中转站A");
        // [15 §2.1] 托盘文案里不许出现 emoji
        assert!(!label.contains('\u{26a0}'), "托盘文案里不许出现 emoji：{label}");
        let ids: Vec<&str> = items
            .iter()
            .filter_map(|n| match n {
                MenuNode::Item { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![id::channel_toggle("relay-a", false).as_str()]);
    }

    #[test]
    fn emergency_section_lists_three_actions_plus_restore() {
        let spec = build_menu_spec(&snapshot());
        let em = find_submenu(&spec, id::EMERGENCY_MENU);
        let ids: Vec<String> = em
            .iter()
            .filter_map(|n| match n {
                MenuNode::Item { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec![
                id::emergency(EmergencyMode::DirectAll),
                id::emergency(EmergencyMode::PauseAll),
                id::emergency(EmergencyMode::VisionOnly),
                id::EMERGENCY_RESTORE.to_string(),
            ]
        );
        // 处于 Restore 时，"恢复正常"应置灰（没什么可恢复的）
        let restore = em
            .iter()
            .find_map(|n| match n {
                MenuNode::Item { id, enabled, checked, .. } if id == id::EMERGENCY_RESTORE => {
                    Some((*enabled, *checked))
                }
                _ => None,
            })
            .expect("恢复项");
        assert_eq!(restore, (false, Some(true)));
        // 处于某紧急模式时，对应项打勾且恢复可用
        let spec2 =
            build_menu_spec(&TraySnapshot { emergency: EmergencyMode::PauseAll, ..snapshot() });
        let em2 = find_submenu(&spec2, id::EMERGENCY_MENU);
        assert!(em2.iter().any(|n| matches!(
            n,
            MenuNode::Item { id, checked: Some(true), .. } if id == &id::emergency(EmergencyMode::PauseAll)
        )));
        assert!(em2.iter().any(|n| matches!(
            n,
            MenuNode::Item { id, enabled: true, .. } if id == id::EMERGENCY_RESTORE
        )));
    }

    #[test]
    fn empty_snapshot_grays_out_data_dependent_sections() {
        let spec = build_menu_spec(&TraySnapshot::not_ready());
        for want in [id::POLICY_MENU, id::LANE_MENU, id::CHANNEL_MENU] {
            let enabled = spec
                .items
                .iter()
                .any(|n| matches!(n, MenuNode::Submenu { id, enabled: true, .. } if id == want));
            assert!(!enabled, "{want} 在没有数据时应该置灰");
        }
        // 但"打开面板/设置/退出"这些必须永远可用，否则用户连窗口都打不开
        for want in [id::OPEN_PANEL, id::SETTINGS, id::QUIT] {
            assert!(spec
                .items
                .iter()
                .any(|n| matches!(n, MenuNode::Item { id, enabled: true, .. } if id == want)));
        }
    }

    // ---------------------------------------------------------- 动作解析

    #[test]
    fn menu_ids_round_trip_into_actions() {
        assert_eq!(parse_action(id::GATEWAY_TOGGLE), TrayAction::ToggleGateway);
        assert_eq!(parse_action(id::LANE_RETEST), TrayAction::RetestLanes);
        assert_eq!(parse_action(id::QUIT), TrayAction::Quit);
        assert_eq!(parse_action(id::OPEN_PANEL), TrayAction::OpenPanel);
        assert_eq!(parse_action(&id::policy("cheap")), TrayAction::UsePolicy("cheap".to_string()));
        assert_eq!(parse_action(&id::lane("jp")), TrayAction::UseLane("jp".to_string()));
        assert_eq!(
            parse_action(&id::channel_toggle("relay-a", false)),
            TrayAction::SetChannelEnabled { channel: "relay-a".to_string(), enabled: false }
        );
        assert_eq!(
            parse_action(&id::channel_toggle("relay-a", true)),
            TrayAction::SetChannelEnabled { channel: "relay-a".to_string(), enabled: true }
        );
        for mode in EmergencyMode::ENTRIES {
            assert_eq!(parse_action(&id::emergency(mode)), TrayAction::Emergency(mode));
        }
        assert_eq!(
            parse_action(id::EMERGENCY_RESTORE),
            TrayAction::Emergency(EmergencyMode::Restore)
        );
    }

    #[test]
    fn channel_ids_with_colons_are_parsed_from_the_right() {
        // 凭据 id 里可能有冒号（用户自己起的名字），所以必须从右边切
        assert_eq!(
            parse_action(&id::channel_toggle("relay:a", false)),
            TrayAction::SetChannelEnabled { channel: "relay:a".to_string(), enabled: false }
        );
    }

    #[test]
    fn unknown_ids_are_reported_not_swallowed() {
        assert_eq!(parse_action("nonsense:id"), TrayAction::Unknown("nonsense:id".to_string()));
        // 前缀对但动词错 → 也算 Unknown，不能猜
        assert_eq!(
            parse_action("channel:x:explode"),
            TrayAction::Unknown("channel:x:explode".to_string())
        );
        assert_eq!(
            parse_action("emergency:whatever"),
            TrayAction::Unknown("emergency:whatever".to_string())
        );
    }

    #[test]
    fn every_generated_menu_id_parses_into_a_known_action() {
        // 菜单生成与动作解析是两份代码，必须对得上：任何自己生成的 id 都不该落到 Unknown
        let spec = build_menu_spec(&snapshot());
        let mut ids = Vec::new();
        collect_ids(&spec.items, &mut ids);
        // 19 项 = 去掉"通道清零额度"那条之后的实有数量（额度功能已下线）。
        // 这个数是**下界**：它挡的是"菜单生成悄悄少了一整节"，不是精确快照。
        assert!(ids.len() >= 19, "菜单项太少，可能漏了：{ids:?}");
        for menu_id in ids {
            let action = parse_action(&menu_id);
            assert!(
                !matches!(action, TrayAction::Unknown(_)),
                "菜单项 {menu_id} 解析不出来，说明生成与解析脱节了"
            );
        }
    }

    fn collect_ids(nodes: &[MenuNode], out: &mut Vec<String>) {
        for n in nodes {
            match n {
                MenuNode::Item { id, .. } => out.push(id.clone()),
                MenuNode::Submenu { items, .. } => collect_ids(items, out),
                MenuNode::Separator => {}
            }
        }
    }

    // ---------------------------------------------------------- tooltip / DTO

    #[test]
    fn tooltip_is_two_lines_and_never_empty() {
        let tip = format_tooltip(&snapshot());
        assert!(tip.starts_with("TokenAgency"));
        assert_eq!(tip.lines().count(), 3, "标题 + 两行细节：{tip}");
        assert!(tip.contains("东京-03"));

        let empty =
            format_tooltip(&TraySnapshot { tooltip_lines: vec!["  ".into()], ..snapshot() });
        assert_eq!(empty.lines().count(), 2, "空行要被丢掉，并用状态兜底：{empty}");
        assert!(empty.contains("降级"), "{empty}");
    }

    #[test]
    fn tray_state_mirrors_the_snapshot() {
        let s = snapshot();
        let st = TrayState::from_snapshot(&s);
        assert_eq!(st.status, TrayStatus::Degraded);
        assert_eq!(st.active_policy_id.as_deref(), Some("cheap"));
        assert_eq!(st.default_lane_id.as_deref(), Some("jp"));
        assert!(st.experimental);
        assert_eq!(st.tooltip, format_tooltip(&s));
        assert_eq!(st.menu, build_menu_spec(&s), "DTO 里的菜单要和托盘用的一致");
        // 序列化键名必须与 [10 §2.2] 的 TS 类型一致
        let json = serde_json::to_value(&st).expect("序列化");
        assert_eq!(json["status"], serde_json::json!("degraded"));
        assert_eq!(json["active_policy_id"], serde_json::json!("cheap"));
        assert!(json["menu"]["items"].is_array());
    }

    /// 占位数据源的行为：启动早期不能假装一切正常。
    #[test]
    fn placeholder_source_is_honest_about_not_being_ready() {
        let src = PlaceholderSource;
        let snap = src.snapshot();
        assert!(!snap.gateway_running);
        assert_eq!(snap.status, TrayStatus::Paused);
        assert!(snap.policies.is_empty());
        match src.dispatch(TrayAction::ToggleGateway) {
            ActionOutcome::Retryable(msg) => assert!(msg.contains("启动")),
            other => panic!("占位源应回可重试，实际 {other:?}"),
        }
    }
}

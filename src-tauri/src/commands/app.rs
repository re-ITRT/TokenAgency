//! `app_*`：应用状态、目录、可粘贴的接入信息。
//!
//! 契约： `app / 应用`。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use ta_store::HealthStore;

/// 内核状态（形状对齐 `ta_kernel::KernelStatus`，见）。
///
/// 为什么这里不复用 `ta_kernel::KernelStatus`：它还没实现 `specta::Type`（那是 P5 的 crate）。
/// T1b 会切到真类型；**字段名与语义现在就是最终形状**，避免前端做二次迁移。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct KernelStatusView {
    /// `stopped` | `starting` | `ready` | `restarting` | `failed`
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub restarts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl KernelStatusView {
    fn stopped() -> Self {
        Self { state: "stopped".into(), version: None, pid: None, restarts: 0, last_error: None }
    }

    fn from_kernel(s: ta_kernel::KernelStatus) -> Self {
        Self {
            state: match s.state {
                ta_kernel::KernelState::Stopped => "stopped",
                ta_kernel::KernelState::Starting => "starting",
                ta_kernel::KernelState::Ready => "ready",
                ta_kernel::KernelState::Restarting => "restarting",
                ta_kernel::KernelState::Failed => "failed",
            }
            .to_string(),
            version: s.version,
            pid: s.pid,
            restarts: s.restarts,
            last_error: s.last_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AppStatus {
    /// `running` | `paused`（T1a 阶段网关由 G2 启停，这里如实报告"未接入"）
    pub gateway: String,
    pub kernel: KernelStatusView,
    pub version: String,
    pub uptime_s: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_lane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_node: Option<String>,
}

/// 网关对外状态标签（纯函数，可离线断言）。
///
/// **为什么闸门优先于 `gateway_ready()`**：`gateway_ready()` 只说"装配过了没有"，
/// 装配过之后一直是 `true`。暂停是**运行态**，必须读闸门 —— 否则用户点了暂停，
/// 状态条还显示"运行中"（[10 §2.2] 的 `gateway: Running|Paused`）。
///
/// 顺带一条语义（E5 接缝第 4 条）：暂停**不影响** `/healthz`，所以"暂停"绝不能
/// 被读成"失联/故障" —— 这也是为什么暂停要显示成 `paused` 而不是 `stopped`。
pub fn gateway_label(
    gate: Option<ta_gateway::ingress::GatewayRunState>,
    deps_present: bool,
) -> &'static str {
    use ta_gateway::ingress::GatewayRunState;
    match gate {
        Some(GatewayRunState::Paused) => "paused",
        Some(GatewayRunState::Running) => "running",
        // 没有闸门（网关没装配）：退回"装配过即 running"，否则如实 stopped
        None if deps_present => "running",
        None => "stopped",
    }
}

/// `app_status` 的实现体（抽出来是为了让暂停/恢复两条命令共用同一份状态组装，
/// 否则三处各拼一遍 `AppStatus`，字段一改就会分叉）。
async fn build_status(state: &AppState) -> IpcResult<AppStatus> {
    let kernel = match state.kernel_slot() {
        Some(k) => KernelStatusView::from_kernel(k.status().await),
        None => KernelStatusView::stopped(),
    };
    let doc = state.doc();
    let active_policy_id = doc
        .policies
        .iter()
        .filter(|p| p.enabled)
        .min_by_key(|p| p.priority)
        .map(|p| p.id.as_str().to_string());
    let default_lane_id = doc.net.default_lane.clone().map(|l| l.as_str().to_string());
    let current_node = match &default_lane_id {
        Some(id) => state
            .health
            .latest(&ta_core::types::LaneId::new(id.clone()))
            .await
            .ok()
            .flatten()
            .and_then(|s| s.node),
        None => None,
    };
    Ok(AppStatus {
        gateway: gateway_label(state.gateway_gate().map(|g| g.state()), state.gateway_ready())
            .to_string(),
        kernel,
        version: ta_core::CORE_VERSION.to_string(),
        uptime_s: state.uptime_s(),
        active_policy_id,
        default_lane_id,
        current_node,
    })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_status(state: State<'_, AppState>) -> IpcResult<AppStatus> {
    build_status(&state).await
}

/// 取闸门；没装配就**如实报错**，而不是静默什么都不做
/// （静默会让用户以为"暂停成功了"，而请求照旧被接单）。
fn gate_of(state: &AppState) -> IpcResult<Arc<ta_gateway::ingress::GatewayGate>> {
    state.gateway_gate().ok_or_else(|| {
        IpcError::from(ta_core::error::TaError::Internal(
            "网关尚未装配，无法暂停或恢复（启动编排可能失败了）".into(),
        ))
    })
}

/// 暂停网关：**只关准入**，不取消在途请求、不影响配置与 `/healthz`。
///
/// 拒绝新请求由闸门在 ingress 内完成（错误码 TA-1005 / 503，
/// 文案让用户看出"是自己按的暂停，不是故障"）—— 本层不自己造拒绝逻辑。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_pause_gateway(state: State<'_, AppState>) -> IpcResult<AppStatus> {
    gate_of(&state)?.pause();
    // `gateway` 不在 [14 §5] 的字段白名单里：按"自由文本不进字段"的裁决写进 message，
    // 而不是为了好看去开一个新字段（字段一旦散出去就收不回来）。
    tracing::info!("用户暂停了网关：不再接受新请求（在途不取消，/healthz 不受影响）");
    build_status(&state).await
}

/// 恢复网关：重新开始接单。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_resume_gateway(state: State<'_, AppState>) -> IpcResult<AppStatus> {
    gate_of(&state)?.resume();
    tracing::info!("用户恢复了网关：重新开始接受请求");
    build_status(&state).await
}

/// 打开应用目录（日志 / 配置 / 数据）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_open_dir(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> IpcResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = match kind.as_str() {
        "data" | "config" => state.data_dir(),
        "logs" => state.data_dir().join("logs"),
        other => {
            return Err(IpcError::from(ta_core::error::TaError::Internal(format!(
                "未知目录类型：{other}（可选 logs|config|data）"
            ))))
        }
    };
    std::fs::create_dir_all(&dir)?;
    app.opener().open_path(dir.to_string_lossy().to_string(), None::<&str>).map_err(|e| {
        IpcError::from(ta_core::error::TaError::Internal(format!("打开目录失败：{e}")))
    })
}

/// 给用户复制到客户端的接入信息（base_url + 本地密钥）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn app_clipboard_access_info(state: State<'_, AppState>) -> IpcResult<String> {
    let doc = state.doc();
    let base = format!("http://{}", doc.gateway.listen);
    match doc.gateway.local_key.as_deref() {
        // 本地密钥本身**不是**上游 key，属于用户要粘给客户端的信息，可以出现在这里
        Some(k) if !k.is_empty() => Ok(format!("BASE_URL={base}/v1\nAPI_KEY={k}")),
        _ => Ok(format!("BASE_URL={base}/v1\nAPI_KEY=(未设置本地密钥)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_gateway::ingress::GatewayRunState;

    #[test]
    fn paused_is_reported_as_paused_not_stopped_or_running() {
        // 这是这一笔最要紧的一断言：用户主动暂停**不能**显示成"故障/未启动"，
        // 也不能显示成"运行中"（那等于骗用户）。
        assert_eq!(gateway_label(Some(GatewayRunState::Paused), true), "paused");
        assert_ne!(gateway_label(Some(GatewayRunState::Paused), true), "stopped");
        assert_ne!(gateway_label(Some(GatewayRunState::Paused), true), "running");
    }

    #[test]
    fn running_and_unassembled_states_are_distinguished() {
        // 闸门存在且已恢复 → running
        assert_eq!(gateway_label(Some(GatewayRunState::Running), true), "running");
        // 没有闸门（网关没装配）+ 依赖也没有 → stopped
        assert_eq!(gateway_label(None, false), "stopped");
        // 兼容旧路径：装配过但没有闸门句柄（本笔之前的调用方）→ 仍报 running
        assert_eq!(gateway_label(None, true), "running");
    }
}

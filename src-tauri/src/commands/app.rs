//! `app_*`：应用状态、目录、可粘贴的接入信息。
//!
//! 契约：`docs/10-interface-spec.md` §2.2 `app / 应用`。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;
use ta_store::HealthStore;

/// 内核状态（形状对齐 `ta_kernel::KernelStatus`，见 `docs/10` §2.2）。
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

#[tauri::command]
#[specta::specta]
pub async fn app_status(state: State<'_, AppState>) -> IpcResult<AppStatus> {
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
        gateway: if state.gateway_ready() { "running".into() } else { "stopped".into() },
        kernel,
        version: ta_core::CORE_VERSION.to_string(),
        uptime_s: state.uptime_s(),
        active_policy_id,
        default_lane_id,
        current_node,
    })
}

/// 打开应用目录（日志 / 配置 / 数据）。
#[tauri::command]
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
#[tauri::command]
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

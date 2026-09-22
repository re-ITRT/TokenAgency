//! `subscription_*` / `lane_*` / `kernel_*`：网络面（订阅 / 线路 / 内核）的读写命令。
//!
//! 契约：`docs/10-interface-spec.md` 2.2 的 `subscription / lane / kernel` 三张表。
//! 这一批命令**此前只在契约里有名字**，没有任何实现 —— 于是"加 Clash 订阅""切/钉线路"
//! "控内核"三件事在界面上做不完，用户只能手写配置文件。本文件把它们接到真实 service 层：
//!
//! | 面 | 真实依赖（复用，不另写一套） |
//! |---|---|
//! | 订阅下载 / 格式嗅探 / 坏更新拦截 / 版本与回滚 | `ta-subscription`：`HttpSubscriptionFetcher` + `DefaultSubscriptionValidator` + `VersionKeeper` |
//! | 线路 -> 内核目标名、provider key、lane 端口 | `ta-kernel`：`config_gen` / `ports` |
//! | 内核生命周期与 REST | `ta-kernel::KernelController`（`AppState::kernel_slot()`） |
//! | 配置真源 | `SqliteConfigStore`（校验 -> 写库 -> 留快照 -> 换内存快照） |
//!
//! # 写路径
//!
//! 所有会改配置的命令都走 [`commit_config`]，它与 `catalog.rs` 的 `save_validated`
//! **同一条顺序**：校验 -> （需要时）分配 lane 端口 -> 事务写库 -> 留快照 -> 换内存快照
//! -> 同步内核配置源（doc + 端口表）。校验不过**一个字节都不写**。
//!
//! # 内核何时被重新加载
//!
//! 改配置的命令保存成功后，只有当**内核当前是 `Ready`** 时才顺手 `apply()` 一次
//! （让改动立刻生效）；内核没跑就不动它，改动会在下一次 `kernel_start` /
//! `kernel_reload_config` 时生效。**不自动拉起内核** —— 一次线路编辑不该顺手启动进程。
//! 若 `apply()` 失败，命令返回错误并说清"配置已保存、内核没加载"（不静默成功）。
//!
//! # 命令入参用 snake_case
//!
//! `#[tauri::command(rename_all = "snake_case")]`：Tauri 2 的**默认**是 camelCase，
//! 而契约（`docs/contracts/bindings-handwritten-reference.ts.txt`）、specta 生成的 TS
//! 与前端 `src/shared/api/commands.ts` 用的都是 snake_case（`version_index` /
//! `user_agent` / `lane_id`）。不写这一句，前端传的 key 一个都对不上。
//! （已实现命令里 `config_export_yaml(with_secrets)` 有同一个问题，但那不在本文件范围内。）
//!
//! # 脱敏（订阅地址里带 token）
//!
//! 凡是**可能包含订阅 URL 的文本**出去之前一律走 [`scrub`]：先把 http(s) URL 收敛成
//! `scheme://host/<redacted>`，再过一遍 `ta_core::diag::sanitize`。
//! 为什么必须做：`ta-subscription` 的下载错误里带着 reqwest 的
//! `error sending request for url (...)` —— 整条 URL（含 token）就在里面。
//! 我们自己的日志**只写 subscription id，不写 url**。
//!
//! # 已知缺口（不硬造，如实报）
//!
//! * `subscription_nodes` 的 `group` 过滤：`ta-subscription` 的 `GroupMeta` 只有
//!   `node_count`、**不保留组成员关系**，所以按组过滤做不到 —— 传 `group` 时**明确报错**，
//!   不静默返回全部节点（那会让 UI 显示错的数据还以为是对的）。
//! * `subscription_groups` 的 `type`：契约写的是 `GroupType`，但订阅里的组类型是**开放集合**
//!   （内核还认 `relay` 等）。按 `ta-kernel` 对 `NodeRef::kind` 的同一条政策
//!   （"刻意不做枚举归一"）**原样返回字符串**，不猜。
//! * `subscription_rollback`：内核是以 `type: http` 的 proxy-provider **自己**拉订阅的
//!   （`config_gen.rs` 写死），我们无法把回滚后的原文注入内核 —— 回滚只改变**我们这一侧**
//!   的生效版本（UI / 预览 / 坏更新基准），线上流量仍以内核拉到的那份为准。
//! * `subscription_delete` 不清订阅版本历史（`ta-subscription` 的 `VersionStore`
//!   没有 remove/purge API）。
//! * `KernelProvider::Bundled` 没有"随包内核"的解析实现（`boot.rs` 只用
//!   `net.kernel.path`），所以两个 provider 都要求一个可执行文件路径。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use ta_core::config::ConfigValidator;
use ta_core::diag::sanitize::sanitize;
use ta_core::domain::{
    ConfigDoc, KernelProvider, Lane, LaneKind, Subscription, SubscriptionFormat,
};
use ta_core::error::TaError;
use ta_core::routing::index::RoutingIndex;
use ta_core::types::{ChannelId, LaneId, ModelId, SubscriptionId};
use ta_kernel::config_gen::{lane_target, provider_key, MhConfigGen};
use ta_kernel::ports::{FilePortStore, PortAllocator, RangePortAllocator};
use ta_kernel::{KernelConfigGen, KernelController, KernelState, PortMap, ProvidersSnapshot};
use ta_store::{
    ConfigStore, SqliteSubscriptionTrafficStore, SubscriptionTraffic, SubscriptionTrafficRow,
};
use ta_subscription::fetch::HttpSubscriptionFetcher;
use ta_subscription::sync::SubscriptionSync;
use ta_subscription::validate::DefaultSubscriptionValidator;
use ta_subscription::version::{FileVersionStore, VersionKeeper, VersionMeta};
use ta_subscription::{DetectedFormat, FetchOutcome, SubscriptionFetcher, DEFAULT_USER_AGENT};

use crate::commands::app::KernelStatusView;
use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;

// ================================================================ 常量

/// 订阅版本存储的目录（相对 `AppState::data_dir()`）。
const SUB_VERSIONS_DIR: &str = "subscription-versions";

/// 单节点测速的超时（毫秒）。与 `ta-kernel::rest` 的口径一致（内核文档里的 `timeout=5000`）。
const LANE_TEST_TIMEOUT_MS: u64 = 5_000;

/// 一次 `lane_test` 最多测多少个节点。
///
/// 为什么要有上限：一次测速 = 一次 `GET /proxies/{name}/delay`，每个都可能花满
/// `LANE_TEST_TIMEOUT_MS`。一份 200 节点的订阅最坏要等十几分钟 —— 那不是"界面里的按钮"
/// 该有的行为。超出部分**不测**（结果里就没有它们），这是刻意的取舍。
const LANE_TEST_MAX_NODES: usize = 32;

/// `subscription_preview` 里最多回几个节点样本（契约的 `sample`）。
const PREVIEW_SAMPLE: usize = 10;

/// 下拉里"直连"那一组的标题（`docs/04` 8.3：每个线路下拉的第一项就是直连）。
pub const DIRECT_GROUP_LABEL: &str = "直连（本机网络）";

/// 其它线路（没有订阅、也不是直连）的分组标题。
const OTHER_GROUP_LABEL: &str = "其它线路";

/// 五级继承链的兜底（`docs/04` 8.1 的 `... ?? direct`）在**配置里没有 direct lane** 时
/// 用的名字。它不是配置里的 lane id，是"直连"这个语义的名字。
const IMPLICIT_DIRECT_LANE: &str = "direct";

// ================================================================ DTO（形状对齐手写契约）

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct RefreshResult {
    pub ok: bool,
    pub node_count: u32,
    pub group_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 本次刷新拿到的流量与更新时间（`docs/18 §1.2`）。**没给头就是 `None`**。
    ///
    /// ★ 只在刷新**一份**订阅时才有值：多份订阅的字节数**不能相加**（那是假数据），
    /// 那种情况下它是 `None`，逐份的数字走只读命令 `subscription_traffic` 读"上次已知"。
    /// 没到期而跳过刷新时，这里放的是**上次已知**（本次没打机场，不该显示成"没有数据"）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic: Option<SubscriptionTraffic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct NodeView {
    pub name: String,
    /// 协议类型名，**原样**（订阅解析层给的就是 `ss` / `vmess` 这种小写名）。
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct PreviewResult {
    pub groups: Vec<String>,
    pub nodes: Vec<String>,
    pub sample: Vec<NodeView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SubscriptionGroupView {
    pub name: String,
    /// 订阅里写的组类型，**原样**（`select` / `url-test` / `fallback` / `load-balance` /
    /// 未来新增的）。见模块文档"已知缺口"：这里刻意不用 `GroupType` 收窄。
    #[serde(rename = "type")]
    pub kind: String,
    pub node_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneTestResult {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    pub ok: bool,
}

/// 这条线路来自继承链的哪一级（`docs/04` 8.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum LaneSource {
    Policy,
    Channel,
    Credential,
    Provider,
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneAssignment {
    pub model_id: ModelId,
    pub channel_id: ChannelId,
    pub lane_id: LaneId,
    pub source: LaneSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneReverseLookup {
    pub channel_ids: Vec<ChannelId>,
    pub model_ids: Vec<ModelId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LanePickOptionItem {
    pub lane_id: LaneId,
    pub name: String,
    pub kind: LaneKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_count: Option<u32>,
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LanePickOption {
    /// 分组标题：直连那一组固定是 [`DIRECT_GROUP_LABEL`]，其余按订阅名分组。
    pub group: String,
    pub options: Vec<LanePickOptionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct KernelProbe {
    pub found: bool,
    /// 找到时是**真正可执行的那个路径**（Windows 下会补 `.exe`）；没找到时是配置里写的那条，
    /// 这样 UI 能显示"你配的是这个，它不存在"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ================================================================ 脱敏

/// 把文本里的 http(s) URL 收敛成 `scheme://host/<redacted>`。
///
/// 只保留 host 是有意的：**"哪台机器失败了"是排查需要的，"token 是什么"不是。**
/// 不能只靠 `sanitize`：它认的是 `?key=` / `?token=` 这类**参数名**，而订阅 token
/// 也可能写在 URL 的**路径**里（`/api/v1/client/subscribe/<token>`）。
fn strip_url_details(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let next = rest.find("http://").into_iter().chain(rest.find("https://")).min();
        let Some(off) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..off]);
        let url_start = cursor + off;
        let scheme_len = if text[url_start..].starts_with("https://") { 8 } else { 7 };
        let after_scheme = url_start + scheme_len;
        let tail = &text[after_scheme..];
        // authority 到 `/` `?` `#` 或任何分隔符为止
        let auth_len = tail
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '/' | '?'
                            | '#'
                            | ')'
                            | '"'
                            | '\''
                            | '`'
                            | '<'
                            | '>'
                            | ','
                            | ';'
                            | ']'
                            | '}'
                    )
            })
            .unwrap_or(tail.len());
        out.push_str(&text[url_start..after_scheme + auth_len]);
        let mut j = after_scheme + auth_len;
        if j < text.len() {
            // 剩下的是 path + query：整段抹掉，但保留 URL 的收尾边界
            let tail2 = &text[j..];
            let url_end = tail2
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, ')' | '"' | '\'' | '`' | '<' | '>' | ',' | ';' | ']' | '}')
                })
                .unwrap_or(tail2.len());
            if url_end > 0 {
                out.push_str("/<redacted>");
                j += url_end;
            }
        }
        cursor = j;
    }
    out
}

/// 出去之前**唯一**的脱敏出口：先去 URL 细节，再过规则表。
fn scrub(text: &str) -> String {
    sanitize(&strip_url_details(text))
}

// ================================================================ 写路径

/// 配置写路径（**与 `catalog::save_validated` 同序**）：校验 -> 事务写库 -> 留快照 -> 换内存快照。
///
/// `catalog.rs` 里那份是**模块私有**的（命令层跨模块调不到），而本轮不允许改 `catalog.rs`
/// —— 所以这里复刻同一顺序，而不是发明第二套写库写法。**校验不过不写库**：
/// 宁可前端拿到字段路径，也不要库里出现一份非法配置。
async fn save_validated(state: &AppState, doc: ConfigDoc, label: &str) -> IpcResult<()> {
    let report = ta_core::config::TaConfigValidator.validate(&doc);
    if !report.ok {
        return Err(IpcError::from_field_errors(&report.errors));
    }
    state.config.save(&doc).await?;
    state.config.snapshot(&doc, label).await?;
    state.set_doc(doc);
    Ok(())
}

/// 写库 + 把新配置同步进内核配置源（doc + lane 端口表）。
///
/// 端口分配放在写库**之前**：分配失败（端口区间被占满）时库里的配置一个字节都没动。
async fn commit_config(state: &AppState, doc: ConfigDoc, label: &str) -> IpcResult<()> {
    let source = state.kernel_source_slot();
    let ports = match &source {
        Some(_) => Some(allocate_ports(state, &doc)?),
        None => None,
    };
    save_validated(state, doc.clone(), label).await?;
    if let (Some(source), Some(ports)) = (source, ports) {
        source.set_doc(doc);
        source.set_ports(ports);
    }
    Ok(())
}

/// 内核正在跑（`Ready`）就顺手 `apply()` 一次，让改动立刻生效。
///
/// 内核没跑 -> 什么都不做（改动会在下一次启动 / `kernel_reload_config` 时生效）。
/// `apply()` 失败 -> 如实报错：**配置已经存进去了**，只是内核没跟上，别让用户以为没保存。
async fn maybe_apply_kernel(state: &AppState, what: &str) -> IpcResult<()> {
    let Some(kernel) = state.kernel_slot() else { return Ok(()) };
    if kernel.status().await.state != KernelState::Ready {
        return Ok(());
    }
    kernel.apply().await.map_err(|e| {
        IpcError::from(TaError::Kernel(scrub(&format!(
            "{what}已保存，但内核没有加载新配置：{e}（可用 kernel_reload_config 重试）"
        ))))
    })?;
    tracing::info!(what, "配置改动已让内核重新加载");
    Ok(())
}

// ================================================================ 端口表

fn ports_path(state: &AppState) -> PathBuf {
    state.data_dir().join("kernel").join("ports.json")
}

/// 用与 `boot.rs` **同一套**分配器（`RangePortAllocator` + `FilePortStore`）算端口表。
///
/// 为什么必须在写配置时一起算：生成的配置里每条启用的 lane 都要一个 listener 端口，
/// 缺一个 `config_gen` 就会报 `port_missing` 让整份配置生成失败。只在 boot 时算的话，
/// "界面里新加的线路"在下次启动前一直是坏的。
fn allocate_ports(state: &AppState, doc: &ConfigDoc) -> IpcResult<PortMap> {
    let store = Arc::new(FilePortStore::new(ports_path(state)));
    let alloc = RangePortAllocator::new(doc.net.kernel.lane_port_range, store)?;
    for lane in doc.lanes.iter().filter(|l| l.enabled) {
        alloc.allocate(&lane.id).map_err(|e| {
            IpcError::from(TaError::Kernel(format!("为 lane {} 分配端口失败：{e}", lane.id)))
        })?;
    }
    Ok(alloc.map())
}

/// lane 删掉之后把它的端口记录也清掉（否则端口表会越攒越多，最终"端口耗尽"）。
///
/// 失败只记日志：配置已经删干净了，端口表里留一条废记录不影响正确性
/// （`RangePortAllocator::restore` 下次会把它当有效分配，只是白占一个端口）。
fn release_lane_port(state: &AppState, lane: &LaneId) {
    let store = Arc::new(FilePortStore::new(ports_path(state)));
    let range = state.doc().net.kernel.lane_port_range;
    match RangePortAllocator::new(range, store) {
        Ok(alloc) => alloc.release(lane),
        Err(e) => {
            tracing::warn!(code = e.code(), error = %e, "释放 lane 端口失败：端口表里可能留下一条废记录")
        }
    }
}

// ================================================================ 订阅：拉取与版本

type SubSync =
    SubscriptionSync<HttpSubscriptionFetcher, DefaultSubscriptionValidator, FileVersionStore>;
type SubKeeper = VersionKeeper<FileVersionStore>;

/// 版本存储落 `<data_dir>/subscription-versions`（`ta-subscription` 的文件实现：
/// 原文 gzip 压缩、按 `keep_versions` 裁剪、坏更新额外留档、回滚只挑已采纳的那版）。
fn version_keeper(state: &AppState) -> SubKeeper {
    VersionKeeper::new(FileVersionStore::new(state.data_dir().join(SUB_VERSIONS_DIR)))
}

/// 真 HTTP 下载器（**独立 Client，不走 lane**，见 `ta-subscription::fetch` 的模块文档）。
fn subscription_sync(state: &AppState) -> IpcResult<SubSync> {
    let fetcher = HttpSubscriptionFetcher::new()
        .map_err(|e| IpcError::from(TaError::Internal(scrub(&e.to_string()))))?;
    Ok(SubscriptionSync::new(fetcher, DefaultSubscriptionValidator::new(), version_keeper(state)))
}

/// 当前**生效**的订阅内容（最近一版已采纳的版本；坏更新被拦下时它仍是上一版）。
fn latest_outcome(state: &AppState, id: &SubscriptionId) -> IpcResult<FetchOutcome> {
    match version_keeper(state).latest_accepted(id).map_err(IpcError::from)? {
        Some((_, outcome)) => Ok(outcome),
        None => Err(IpcError::from(TaError::MalformedRequest(format!(
            "订阅 {id} 还没有已采纳的版本（先用 subscription_refresh 拉一次）"
        )))),
    }
}

fn find_subscription(doc: &ConfigDoc, id: &SubscriptionId) -> IpcResult<Subscription> {
    doc.proxy_subscriptions
        .iter()
        .find(|s| &s.id == id)
        .cloned()
        .ok_or_else(|| not_found("订阅", id.as_str()))
}

// ================================================================ 订阅流量（docs/18 §1）

/// 流量快照的读写口（`subscription` 表的 5 个可空列）。
///
/// 每次从 `AppState::db` 现构造：`SqlitePool` 的 clone 是 Arc 级的，代价可忽略；
/// 也因此不必往 `AppState` 里加字段（那个文件不在本次改动面上）。
fn traffic_store(state: &AppState) -> SqliteSubscriptionTrafficStore {
    SqliteSubscriptionTrafficStore::new(state.db.clone())
}

/// 解析层的流量 → IPC 线上形状。
///
/// 为什么需要这一次转换：`ta-subscription`（下载/解析）与 `ta-store`（持久化 + DTO）
/// 互不依赖（[11 §1.1]：不跨 crate 用别人的具体类型），而 **DTO 的唯一真相源是 Store**
/// （见 `src-tauri/Cargo.toml` 里"Store 是唯一真相源，不另写一份 DTO"）。
/// 两个类型字段一一对应，**只在这一处转换** —— 少一个字段就编不过，漂移不了。
fn to_ipc_traffic(t: &ta_subscription::traffic::SubscriptionTraffic) -> SubscriptionTraffic {
    SubscriptionTraffic {
        upload_bytes: t.upload_bytes,
        download_bytes: t.download_bytes,
        total_bytes: t.total_bytes,
        expire_at_s: t.expire_at_s,
        fetched_at_ms: t.fetched_at_ms,
    }
}

// ================================================================ 错误助手

/// "你要改的东西不存在" —— 必须报错，**不能静默成功**。
///
/// 与 `catalog.rs` 的 `not_found` 同一个取舍：用 `MalformedRequest`（TA-1004）而不是
/// `Referenced`（TA-5002 的语义是"被引用、禁止删除"）。`TaError` 现在没有"实体不存在"这个
/// 变体 —— **不自己发明码**，用最接近的既有码。
fn not_found(what: &str, id: &str) -> IpcError {
    IpcError::from(TaError::MalformedRequest(format!(
        "{what}不存在：{id}（无法对不存在的实体做这个操作）"
    )))
}

/// "内核没装配"的统一错误。码 / 类型串仍来自 `TaError`（唯一真源），只把 message
/// 换成能指导用户下一步的那句话。
fn kernel_missing() -> IpcError {
    let mut e = IpcError::from(TaError::KernelNotReady);
    e.message = "内核没有装配：net.kernel.path 为空或装配失败。先用 kernel_set_provider 指定内核可执行文件路径，再启动。"
        .to_string();
    e
}

// ================================================================ subscription 命令

/// `subscription_upsert`：同 id 覆盖、不存在则新增（幂等，不会写出重复条目）。
pub async fn upsert_subscription(
    state: &AppState,
    subscription: Subscription,
) -> IpcResult<Subscription> {
    let mut doc = (*state.doc()).clone();
    match doc.proxy_subscriptions.iter_mut().find(|x| x.id == subscription.id) {
        Some(slot) => *slot = subscription.clone(),
        None => doc.proxy_subscriptions.push(subscription.clone()),
    }
    commit_config(state, doc, "subscription_upsert").await?;
    // 只记 id 与 provider key：**订阅 url 里可能带 token，绝不进日志**。
    tracing::info!(
        sub_id = %subscription.id,
        provider_key = %provider_key(&subscription),
        "订阅已保存"
    );
    maybe_apply_kernel(state, "订阅").await?;
    Ok(subscription)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_upsert(
    state: State<'_, AppState>,
    subscription: Subscription,
) -> IpcResult<Subscription> {
    upsert_subscription(&state, subscription).await
}

/// `subscription_delete`：删订阅。被 lane 引用时**校验层会拒**（引用完整性），
/// 不会留下指向不存在订阅的 lane。
pub async fn delete_subscription(state: &AppState, id: &SubscriptionId) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let before = doc.proxy_subscriptions.len();
    doc.proxy_subscriptions.retain(|s| &s.id != id);
    if doc.proxy_subscriptions.len() == before {
        return Err(not_found("订阅", id.as_str()));
    }
    commit_config(state, doc, "subscription_delete").await?;
    // 版本历史**故意不删**：版本存储没有 remove/purge API（见模块文档的已知缺口）。
    tracing::info!(sub_id = %id, "订阅已删除（其版本历史仍留在磁盘上）");
    maybe_apply_kernel(state, "订阅删除").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_delete(state: State<'_, AppState>, id: SubscriptionId) -> IpcResult<()> {
    delete_subscription(&state, &id).await
}

/// 一次刷新的结果（`RefreshResult` 的内部形态）。
struct Refreshed {
    ok: bool,
    node_count: u32,
    group_count: u32,
    error: Option<String>,
    /// 本次拿到（或"上次已知"）的流量；单份订阅才有意义。
    traffic: Option<SubscriptionTraffic>,
}

/// 刷新一份订阅：fetch -> 坏更新拦截 -> 记版本 -> （内核在跑就）让它重拉 provider。
async fn refresh_one(state: &AppState, sub: &Subscription, force: bool) -> IpcResult<Refreshed> {
    // `force: false` 且还没到 `interval_s` -> 不发请求（省流量，也让 UI 的"刷新"不误伤机场）。
    if !force {
        let keeper = version_keeper(state);
        if let Some((meta, outcome)) = keeper.latest_accepted(&sub.id).map_err(IpcError::from)? {
            let due_at = meta.fetched_at_ms + (sub.interval_s.max(1) as i64) * 1_000;
            if due_at > state.clock.now_ms() {
                // 这次没打机场 —— 但界面上的"已用/总量"不该因此变成"没有数据"：
                // 给它**上次已知**（库里那份）。
                let traffic =
                    traffic_store(state).get(sub.id.as_str()).await.map_err(IpcError::from)?;
                return Ok(Refreshed {
                    ok: true,
                    node_count: outcome.node_count() as u32,
                    group_count: outcome.group_count() as u32,
                    error: None,
                    traffic,
                });
            }
        }
    }

    let sync = subscription_sync(state)?;
    // 注意 `SubscriptionSync::sync` 的契约：**下载失败不报错**，它会返回一条
    // `AcceptDecision::Reject("下载失败：...")` 并保留上一版 —— 那正是我们要的行为。
    let outcome = sync.sync(sub).await.map_err(IpcError::from)?;
    let ok = outcome.accepted();
    let mut error = outcome.reason().map(scrub);
    let (node_count, group_count) = match outcome.effective.as_ref() {
        Some(o) => (o.node_count() as u32, o.group_count() as u32),
        None => (0, 0),
    };
    // 流量跟着**已采纳的那一版**走：`ok = false` 时 `effective` 是上一版（`traffic = None`），
    // 所以我们不会拿一次坏更新的头去覆盖"上次已知"。
    let traffic = outcome.effective.as_ref().and_then(|o| o.traffic.as_ref()).map(to_ipc_traffic);

    tracing::info!(
        sub_id = %sub.id,
        accepted = ok,
        node_count,
        group_count,
        recorded_version = ?outcome.recorded_version,
        effective_version = ?outcome.effective_version,
        traffic_known = traffic.is_some(),
        "订阅刷新完成"
    );

    // ★ 每次**成功刷新**写"上次已知"（docs/18 §1.2）。
    //   头缺失（`None`）时**不动**库里的旧值：机场偶尔不给头，不该把已知的数字擦掉。
    //   写失败不改判刷新本身（订阅已经生效了），但必须如实说出来 —— 否则界面会显示
    //   过期数字而没人知道为什么。
    if ok {
        if let Some(t) = &traffic {
            if let Err(e) = traffic_store(state).record(sub.id.as_str(), t).await {
                error = Some(scrub(&format!("订阅已更新，但流量快照没写进库：{e}")));
            }
        }
    }

    // 采纳了新内容 -> 内核的 proxy-provider 也该立刻重拉（内核自己也有 interval，
    // 这里只是不让用户等它）。内核没跑就跳过，别造一个假的失败。
    if ok {
        if let Some(kernel) = state.kernel_slot() {
            if kernel.status().await.state == KernelState::Ready {
                let key = provider_key(sub);
                if let Err(e) = kernel.refresh_provider(&key).await {
                    error = Some(scrub(&format!("订阅已更新，但内核 provider 刷新失败：{e}")));
                }
            }
        }
    }

    Ok(Refreshed { ok, node_count, group_count, error, traffic })
}

/// 刷新一份或全部启用的订阅。
pub async fn refresh_subscriptions(
    state: &AppState,
    id: Option<SubscriptionId>,
    force: bool,
) -> IpcResult<RefreshResult> {
    let doc = state.doc();
    let targets: Vec<Subscription> = match &id {
        Some(id) => vec![find_subscription(&doc, id)?],
        None => doc.proxy_subscriptions.iter().filter(|s| s.enabled).cloned().collect(),
    };
    drop(doc);

    if targets.is_empty() {
        return Ok(RefreshResult {
            ok: true,
            node_count: 0,
            group_count: 0,
            error: Some("没有启用的订阅，未做任何刷新".to_string()),
            traffic: None,
        });
    }

    let single = targets.len() == 1;
    let mut ok = true;
    let mut node_count = 0u32;
    let mut group_count = 0u32;
    let mut traffic: Option<SubscriptionTraffic> = None;
    let mut errors: Vec<String> = Vec::new();
    for sub in &targets {
        let r = refresh_one(state, sub, force).await?;
        ok &= r.ok;
        node_count = node_count.saturating_add(r.node_count);
        group_count = group_count.saturating_add(r.group_count);
        // 只有一份订阅时 `traffic` 才代表"这份订阅的"数字；多份不能相加（假数据）。
        if single {
            traffic = r.traffic;
        }
        if let Some(e) = r.error {
            // 单份订阅：原文回给 UI；多份：带上 id 才分得清是哪一份。
            errors.push(if single { e } else { format!("{}：{e}", sub.id) });
        }
    }
    Ok(RefreshResult {
        ok,
        node_count,
        group_count,
        error: if errors.is_empty() { None } else { Some(errors.join("；")) },
        traffic,
    })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_refresh(
    state: State<'_, AppState>,
    id: Option<SubscriptionId>,
    force: Option<bool>,
) -> IpcResult<RefreshResult> {
    refresh_subscriptions(&state, id, force.unwrap_or(false)).await
}

/// `subscription_preview`：**只看一眼**这份 url 是什么（不写库、不记版本、不碰内核）。
///
/// 这是"加订阅"向导的第一步：用户粘一个地址，先看到有多少节点、有哪些分组，再决定要不要存。
pub async fn preview_subscription(
    url: &str,
    user_agent: Option<&str>,
    format: Option<SubscriptionFormat>,
) -> IpcResult<PreviewResult> {
    let sub = Subscription {
        id: SubscriptionId::new("preview"),
        name: "preview".to_string(),
        url: url.trim().to_string(),
        user_agent: user_agent
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_USER_AGENT)
            .to_string(),
        format: format.unwrap_or_default(),
        interval_s: 6 * 3600,
        keep_versions: 1,
        filter: Default::default(),
        health_check: Default::default(),
        enabled: true,
        provider_key: None,
        extra: serde_json::Map::new(),
    };
    let fetcher = HttpSubscriptionFetcher::new()
        .map_err(|e| IpcError::from(TaError::Internal(scrub(&e.to_string()))))?;
    // 下载失败**必须**脱敏后再出去（reqwest 的错误里带着整条 URL）。
    let outcome = fetcher
        .fetch(&sub)
        .await
        .map_err(|e| IpcError::from(TaError::Internal(scrub(&e.to_string()))))?;

    Ok(PreviewResult {
        groups: outcome.groups.iter().map(|g| g.name.clone()).collect(),
        nodes: outcome.nodes.iter().map(|n| n.name.clone()).collect(),
        sample: outcome.nodes.iter().take(PREVIEW_SAMPLE).map(node_view_of).collect(),
    })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_preview(
    url: String,
    user_agent: Option<String>,
    format: Option<SubscriptionFormat>,
) -> IpcResult<PreviewResult> {
    preview_subscription(&url, user_agent.as_deref(), format).await
}

/// `subscription_groups`：当前生效版本里的分组清单。
pub async fn groups_of_subscription(
    state: &AppState,
    id: &SubscriptionId,
) -> IpcResult<Vec<SubscriptionGroupView>> {
    let doc = state.doc();
    let _ = find_subscription(&doc, id)?;
    drop(doc);
    let outcome = latest_outcome(state, id)?;
    Ok(outcome
        .groups
        .iter()
        .map(|g| SubscriptionGroupView {
            name: g.name.clone(),
            kind: g.kind.clone(),
            node_count: g.node_count as u32,
        })
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_groups(
    state: State<'_, AppState>,
    id: SubscriptionId,
) -> IpcResult<Vec<SubscriptionGroupView>> {
    groups_of_subscription(&state, &id).await
}

/// `subscription_nodes`：当前生效版本里的节点清单。
///
/// `group` 过滤见模块文档的已知缺口：解析层不保留组成员关系，所以传 `group` 时
/// **明确报错**而不是静默返回全部节点。
pub async fn nodes_of_subscription(
    state: &AppState,
    id: &SubscriptionId,
    group: Option<String>,
) -> IpcResult<Vec<NodeView>> {
    let doc = state.doc();
    let _ = find_subscription(&doc, id)?;
    drop(doc);
    if let Some(g) = group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "subscription_nodes 目前不支持按分组过滤（收到 group={g}）：ta-subscription 的解析结果\
             只保留分组名与节点数（GroupMeta.node_count），**不保留组成员关系**，所以任何过滤\
             都只能是猜的。请不带 group 调用（返回订阅内全部节点）——按组精确取成员需要给\
             ta-subscription 的 GroupMeta 增加 members 字段，或改用内核的 /proxies 快照。"
        ))));
    }
    let outcome = latest_outcome(state, id)?;
    Ok(outcome.nodes.iter().map(node_view_of).collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_nodes(
    state: State<'_, AppState>,
    id: SubscriptionId,
    group: Option<String>,
) -> IpcResult<Vec<NodeView>> {
    nodes_of_subscription(&state, &id, group).await
}

/// `subscription_list` 的一行：**配置实体 + 当前生效版本的解析状态**。
///
/// 这就是冻结契约里的 `SubscriptionView`（`src/shared/bindings.ts`）。为什么必须有这个包装：
/// `subscription_list` 过去返回的是裸 `Subscription`（纯配置实体），里面**没有任何解析结果**。
/// 而节点页在发 `subscription_nodes` 之前必须先知道"这份订阅到底有没有已采纳的版本"，
/// 否则只剩两个坏选择：对没刷过的订阅无条件发命令（必然报错），或者不敢发（真实发生过：
/// 订阅加好了、刷新也成功了，页面还是那句"还没有解析结果"，一个节点都不显示）。
///
/// 数据来源是 `VersionKeeper::history`（**只读元数据，不解压原文**）。刻意不用
/// `latest_accepted`：它每次都要解压整份订阅，只为拿两个计数太贵。
///
/// 三态口径与 [docs/18 §1.2] 一致：**没有已采纳的版本 = 字段缺失，不用 `0` / 空串冒充**。
/// 前端 `readSubscription` 把"缺失"读成 `undefined`（未读取）、把"明确 null"读成 `null`
/// （后端说没有）—— 两者不是一回事。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SubscriptionView {
    pub subscription: Subscription,
    /// 内核 `proxy-providers` 的 key，走 `ta_kernel::config_gen::provider_key`：
    /// 配置里没写就按 id 派生、只留安全字符（**不在这里另立一套派生法**）。
    pub provider_key: String,
    /// 生效版本的节点数。没有已采纳版本时**缺失**（不是 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_count: Option<u32>,
    /// 生效版本的分组数。没有已采纳版本时**缺失**（不是 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_count: Option<u32>,
    /// 生效版本的抓取时刻（Unix 毫秒）。节点页的硬前置就判它（[docs/19 §2.3]）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update_ms: Option<i64>,
    /// 嗅探到的格式（`clash_yaml` / `base64_links` / `plain_links` / `html` / `unknown`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parser: Option<String>,
    /// **最新一次尝试被坏更新拦截**时的话。有值 = 后端确实处理过这份订阅、但这次没好结果；
    /// 页面据此仍然发 `subscription_nodes`（它回的是上一版**已采纳**的内容），
    /// 而不是替后端下"没解析过"的结论。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 组装一份订阅的视图。`metas` 是该订阅的版本历史，**新的在前**（`VersionStore::list` 的序）。
///
/// 纯函数：不碰 IO、不碰状态，坏更新与"从没刷过"两种情况的措辞都在这里定死。
pub fn subscription_view(sub: &Subscription, metas: &[VersionMeta]) -> SubscriptionView {
    let accepted = metas.iter().find(|m| m.accepted);
    let error = metas.first().filter(|m| !m.accepted).map(|bad| match accepted {
        Some(ok) => format!(
            "最近一次刷新（v{}）被拦下：内容判定为坏更新，当前仍用 v{}。",
            bad.version, ok.version
        ),
        None => format!(
            "最近一次刷新（v{}）被拦下：内容判定为坏更新，目前还没有可用版本。",
            bad.version
        ),
    });

    SubscriptionView {
        subscription: sub.clone(),
        provider_key: provider_key(sub),
        node_count: accepted.map(|m| m.node_count as u32),
        group_count: accepted.map(|m| m.group_count as u32),
        last_update_ms: accepted.map(|m| m.fetched_at_ms),
        parser: accepted.map(|m| m.format.as_str().to_string()),
        error,
    }
}

/// 全部订阅的视图，顺序与配置里的顺序一致（`subscription_list` 的实现体）。
///
/// 与 `subscription_list` 分成两个函数，是为了让守卫能离线断言这条命令**真的**带解析状态
/// （不需要起 Tauri）。
pub fn views_of_subscriptions(state: &AppState) -> IpcResult<Vec<SubscriptionView>> {
    let doc = state.doc();
    let subs = doc.proxy_subscriptions.clone();
    drop(doc);

    let keeper = version_keeper(state);
    let mut out = Vec::with_capacity(subs.len());
    for sub in &subs {
        let metas = keeper.history(&sub.id).map_err(IpcError::from)?;
        out.push(subscription_view(sub, &metas));
    }
    Ok(out)
}

/// `subscription_rules_raw`：只读展示订阅原本的 `rules` 段（**我们不执行它**）。
///
/// 实现方式：按顶层 `rules:` 键做**行级截取**（不做 YAML 解析：`tokenagency` 没有
/// `serde_yaml_ng` 依赖，为这一条命令引一个新依赖不值得）。返回的是原文切片，
/// 不是重新序列化的结果 —— 显示用的东西，不要改写它。
/// 链接清单（base64 / 明文）里按定义没有 `rules`，返回 `None`。
pub async fn rules_raw_of_subscription(
    state: &AppState,
    id: &SubscriptionId,
) -> IpcResult<Option<String>> {
    let doc = state.doc();
    let _ = find_subscription(&doc, id)?;
    drop(doc);
    let outcome = latest_outcome(state, id)?;
    if outcome.format != DetectedFormat::ClashYaml {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&outcome.raw);
    Ok(top_level_block(&text, "rules"))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_rules_raw(
    state: State<'_, AppState>,
    id: SubscriptionId,
) -> IpcResult<Option<String>> {
    rules_raw_of_subscription(&state, &id).await
}

/// `subscription_traffic`：每份订阅的"上次已知流量与更新时间"（只读，docs/18 §1.2 的 (a)）。
///
/// 返回 `{ id, traffic }[]`，**以配置里的订阅清单为准**（不是表里的行 —— 表里可能有
/// 已删订阅的残留行）；`traffic: null` = 这份订阅从未成功刷新过（界面上是"尚未获取"）。
///
/// 为什么另开一条只读命令，而不是把 `traffic` 塞进 `SubscriptionView`（docs/18 §1.2 给了
/// 两个选项，(a) 是推荐的）：`Subscription` 是**配置实体**，流量是**运行期观测值**。
/// 混在一起之后，"改配置"的写路径会顺手把观测值一起改写/清掉，而且 `subscription_upsert`
/// 的返回值会带上一个它根本没碰过的字段。
pub async fn traffic_of_subscriptions(state: &AppState) -> IpcResult<Vec<SubscriptionTrafficRow>> {
    let doc = state.doc();
    let ids: Vec<String> = doc.proxy_subscriptions.iter().map(|s| s.id.to_string()).collect();
    drop(doc);

    let known: BTreeMap<String, SubscriptionTraffic> = traffic_store(state)
        .list()
        .await
        .map_err(IpcError::from)?
        .into_iter()
        .filter_map(|row| {
            let SubscriptionTrafficRow { id, traffic } = row;
            traffic.map(|t| (id, t))
        })
        .collect();

    Ok(ids
        .into_iter()
        .map(|id| SubscriptionTrafficRow { traffic: known.get(&id).cloned(), id })
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_traffic(
    state: State<'_, AppState>,
) -> IpcResult<Vec<SubscriptionTrafficRow>> {
    traffic_of_subscriptions(&state).await
}

/// `subscription_rollback`：把某个**已采纳**的版本重新立为生效版本。
///
/// * 版本被坏更新拦下（`accepted = false`）-> `ta-subscription` 直接拒（不回滚到坏内容）；
/// * 生效方式：把那份原文**再记一版**（版本存储是只追加的），于是"最近一版已采纳的内容"
///   就变成了它 —— UI、预览、坏更新基准三处一起跟着变。
/// * 流量仍以内核拉到的那份为准，原因见模块文档的已知缺口。
pub async fn rollback_subscription(
    state: &AppState,
    id: &SubscriptionId,
    version_index: u32,
) -> IpcResult<()> {
    let doc = state.doc();
    let sub = find_subscription(&doc, id)?;
    drop(doc);
    let keeper = version_keeper(state);
    let outcome = keeper.rollback_to(&sub.id, version_index as u64).map_err(IpcError::from)?;
    keeper.record(&sub.id, &outcome, sub.keep_versions, true).map_err(IpcError::from)?;
    tracing::info!(
        sub_id = %sub.id,
        from_version = version_index,
        "订阅已回滚（旧内容记为新的生效版本）"
    );
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_rollback(
    state: State<'_, AppState>,
    id: SubscriptionId,
    version_index: u32,
) -> IpcResult<()> {
    rollback_subscription(&state, &id, version_index).await
}

// ================================================================ lane 命令

/// `lane_upsert`：同 id 覆盖、不存在则新增。改完把内核配置源（含端口表）同步好，
/// 内核在跑就立刻 reload。
pub async fn upsert_lane(state: &AppState, lane: Lane) -> IpcResult<Lane> {
    let mut doc = (*state.doc()).clone();
    match doc.lanes.iter_mut().find(|x| x.id == lane.id) {
        Some(slot) => *slot = lane.clone(),
        None => doc.lanes.push(lane.clone()),
    }
    commit_config(state, doc, "lane_upsert").await?;
    tracing::info!(lane_id = %lane.id, kind = lane.kind.as_str(), "线路已保存");
    maybe_apply_kernel(state, "线路").await?;
    Ok(lane)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_upsert(state: State<'_, AppState>, lane: Lane) -> IpcResult<Lane> {
    upsert_lane(&state, lane).await
}

/// `lane_delete`：删线路。被 `net.default_lane` / provider / credential / channel 引用时
/// 校验层会拒（引用完整性），不会留下悬空引用。
pub async fn delete_lane(state: &AppState, id: &LaneId) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let before = doc.lanes.len();
    doc.lanes.retain(|l| &l.id != id);
    if doc.lanes.len() == before {
        return Err(not_found("线路", id.as_str()));
    }
    commit_config(state, doc, "lane_delete").await?;
    release_lane_port(state, id);
    tracing::info!(lane_id = %id, "线路已删除");
    maybe_apply_kernel(state, "线路删除").await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_delete(state: State<'_, AppState>, id: LaneId) -> IpcResult<()> {
    delete_lane(&state, &id).await
}

/// `lane_set_node`：手动钉住 `select` 组里的节点（`docs/04` 8.2 的线路下拉）。
///
/// 只动内核（`PUT /proxies/{group}`），**不改配置**：钉住是运行态，配置里的线路定义没变。
/// 节点先在内核快照里查一次：钉一个内核不认识的名字，内核会收下但每个请求 502 ——
/// 那种"看起来成功、实际全坏"的结果必须在这里拦住。
pub async fn set_lane_node(state: &AppState, id: &LaneId, node: &str) -> IpcResult<()> {
    let doc = state.doc();
    let lane = doc
        .lanes
        .iter()
        .find(|l| &l.id == id)
        .cloned()
        .ok_or_else(|| not_found("线路", id.as_str()))?;
    drop(doc);
    let node = node.trim();
    if node.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(
            "要钉住的节点名不能为空".to_string(),
        )));
    }
    if lane.kind == LaneKind::Direct {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "线路 {} 是直连（kind: direct）：它的 listener 直接指 DIRECT，没有可切换的组",
            lane.id
        ))));
    }
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    let snap = kernel.snapshot().await.map_err(IpcError::from)?;
    if !snap.nodes.contains_key(node) {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "内核里没有节点 {node}（名字拼错，或订阅还没加载到内核）。当前可见的节点示例：{}",
            sample_names(&snap)
        ))));
    }
    kernel.set_select(&lane.id, node).await.map_err(IpcError::from)?;
    tracing::info!(lane_id = %lane.id, node, "线路已钉到指定节点");
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_set_node(state: State<'_, AppState>, id: LaneId, node: String) -> IpcResult<()> {
    set_lane_node(&state, &id, &node).await
}

/// 内核快照里挑几个节点名当错误提示的样本（不让错误信息变成一屏 200 个名字）。
fn sample_names(snap: &ProvidersSnapshot) -> String {
    let mut names: Vec<&str> =
        snap.nodes.values().filter(|n| n.is_lane_target()).map(|n| n.name.as_str()).collect();
    names.sort_unstable();
    let shown = names.iter().take(8).copied().collect::<Vec<_>>().join("、");
    if names.len() > 8 {
        format!("{shown} 等 {} 个", names.len())
    } else if names.is_empty() {
        "（一个都没有）".to_string()
    } else {
        shown
    }
}

/// `lane_test`：测这条线路（或指定节点）的延迟。
///
/// 传 `node` 就只测那一个；不传就测这条 lane 在内核里对应的目标的**成员**
/// （组 -> 组内每个节点；节点 / DIRECT -> 它自己）。上限 [`LANE_TEST_MAX_NODES`]。
///
/// 单个节点不通**不让整条命令失败**：契约要的是"逐节点结果"，
/// 所以失败的节点以 `ok: false` 出现在列表里（原因写 debug 日志 —— 契约没有那个字段）。
pub async fn test_lane(
    state: &AppState,
    id: &LaneId,
    node: Option<String>,
) -> IpcResult<Vec<LaneTestResult>> {
    let doc = state.doc();
    let lane = doc
        .lanes
        .iter()
        .find(|l| &l.id == id)
        .cloned()
        .ok_or_else(|| not_found("线路", id.as_str()))?;
    let target = lane_target(&doc, &lane).map_err(IpcError::from)?;
    drop(doc);

    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    let mut names: Vec<String> = match node.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => vec![n.to_string()],
        None => {
            let snap = kernel.snapshot().await.map_err(IpcError::from)?;
            let members: Vec<String> = snap
                .group_members(&target)
                .map(|m| m.into_iter().map(|n| n.name.clone()).collect())
                .unwrap_or_default();
            // 不是组（或组里一个能解析的名字都没有）-> 直接测这个目标本身
            if members.is_empty() {
                vec![target.clone()]
            } else {
                members
            }
        }
    };
    names.truncate(LANE_TEST_MAX_NODES);

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        match kernel.delay_of(&name, LANE_TEST_TIMEOUT_MS).await {
            Ok(ms) => out.push(LaneTestResult { node: name, latency_ms: Some(ms), ok: true }),
            Err(e) => {
                tracing::debug!(
                    lane_id = %lane.id,
                    node = %name,
                    code = e.code(),
                    error = %scrub(&e.to_string()),
                    "节点测速失败（以 ok=false 出现在结果里）"
                );
                out.push(LaneTestResult { node: name, latency_ms: None, ok: false });
            }
        }
    }
    Ok(out)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_test(
    state: State<'_, AppState>,
    id: LaneId,
    node: Option<String>,
) -> IpcResult<Vec<LaneTestResult>> {
    test_lane(&state, &id, node).await
}

/// 五级继承 + 来源标注（`docs/04` 8.1、`domain::behavior::resolve_lane`）。
///
/// 值走 `RoutingIndex::resolve_lane`（既有的继承链实现），这里只额外记"命中哪一级"
/// 给 UI 显示 —— 同一顺序，不另写一套解析。
fn resolve_with_source(
    doc: &ConfigDoc,
    ch: &ta_core::domain::ChannelConfig,
) -> (LaneId, LaneSource) {
    let idx = RoutingIndex::build(doc);
    let source = if ch.lane.is_some() {
        LaneSource::Channel
    } else if doc
        .credentials
        .iter()
        .find(|c| c.id == ch.credential)
        .is_some_and(|c| c.lane.is_some())
    {
        LaneSource::Credential
    } else if doc.providers.iter().find(|p| p.id == ch.provider).is_some_and(|p| p.lane.is_some()) {
        LaneSource::Provider
    } else {
        LaneSource::Default
    };
    let lane = idx.resolve_lane(ch).unwrap_or_else(|| implicit_direct_lane(doc));
    (lane, source)
}

/// 兜底直连：配置里有启用的 `direct` lane 就用它，否则用 [`IMPLICIT_DIRECT_LANE`]。
fn implicit_direct_lane(doc: &ConfigDoc) -> LaneId {
    doc.lanes
        .iter()
        .find(|l| l.enabled && l.kind == LaneKind::Direct)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| LaneId::new(IMPLICIT_DIRECT_LANE))
}

/// `lane_assignments`：每条通道实际走哪条线（含来源级别）。
///
/// **策略级（`policy.use_lane`）不在里面**：那是按请求匹配的，没有请求上下文就无从判定 ——
/// 所以 `LaneSource::Policy` 目前不会有实例，留着是为了类型与契约一致。
pub async fn compute_lane_assignments(state: &AppState) -> IpcResult<Vec<LaneAssignment>> {
    let doc = state.doc();
    Ok(doc
        .channels
        .iter()
        .map(|ch| {
            let (lane_id, source) = resolve_with_source(&doc, ch);
            LaneAssignment {
                model_id: ModelId::new(ch.model.clone()),
                channel_id: ch.id.clone(),
                lane_id,
                source,
            }
        })
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_assignments(state: State<'_, AppState>) -> IpcResult<Vec<LaneAssignment>> {
    compute_lane_assignments(&state).await
}

/// `lane_reverse_lookup`：谁在用这条线（删线路 / 删订阅之前看它）。
pub async fn reverse_lookup_lane(
    state: &AppState,
    lane_id: &LaneId,
) -> IpcResult<LaneReverseLookup> {
    let doc = state.doc();
    let idx = RoutingIndex::build(&doc);
    let mut channel_ids = Vec::new();
    let mut model_ids: Vec<ModelId> = Vec::new();
    for ch in &doc.channels {
        let effective = idx.resolve_lane(ch).unwrap_or_else(|| implicit_direct_lane(&doc));
        if &effective != lane_id {
            continue;
        }
        channel_ids.push(ch.id.clone());
        let model = ModelId::new(idx.canonical_model(&ch.model));
        if !model_ids.contains(&model) {
            model_ids.push(model);
        }
    }
    Ok(LaneReverseLookup { channel_ids, model_ids })
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_reverse_lookup(
    state: State<'_, AppState>,
    lane_id: LaneId,
) -> IpcResult<LaneReverseLookup> {
    reverse_lookup_lane(&state, &lane_id).await
}

/// 一份订阅当前生效版本的规模（下拉里显示"多少个节点"用）。`None` = 还没有可用版本。
struct SubScale {
    nodes: u32,
    /// 组名 -> 组内节点数
    groups: BTreeMap<String, u32>,
}

/// `lane_pick_options`：通道矩阵下拉的选项树（按订阅分组，直连永远第一组）。
///
/// `node_count` 来自该订阅当前生效版本（[`latest_outcome`]）；拿不到就是 `None`
/// （**不猜**）。`latency_ms` 只在**内核在跑**时从 `/proxies` 快照里读上次测速结果 ——
/// 打开下拉**不主动测速**（那会是 N 个请求 + 每个最多 5 秒）。要即时数字请用 `lane_test`。
pub async fn compute_lane_pick_options(state: &AppState) -> IpcResult<Vec<LanePickOption>> {
    let doc = state.doc();
    let snap = match state.kernel_slot() {
        Some(k) if k.status().await.state == KernelState::Ready => k.snapshot().await.ok(),
        _ => None,
    };

    // 订阅 -> 生效版本的规模；每份订阅只读一次
    let mut counts: BTreeMap<String, Option<SubScale>> = BTreeMap::new();
    for sub in &doc.proxy_subscriptions {
        let entry = match latest_outcome(state, &sub.id) {
            Ok(o) => Some(SubScale {
                nodes: o.nodes.len() as u32,
                groups: o.groups.iter().map(|g| (g.name.clone(), g.node_count as u32)).collect(),
            }),
            Err(_) => None,
        };
        counts.insert(sub.id.to_string(), entry);
    }

    let direct_label = DIRECT_GROUP_LABEL.to_string();
    let mut groups: Vec<LanePickOption> = Vec::new();
    for lane in doc.lanes.iter().filter(|l| l.enabled) {
        let label = if lane.kind == LaneKind::Direct {
            direct_label.clone()
        } else {
            match lane.subscription.as_ref() {
                Some(sid) => doc
                    .proxy_subscriptions
                    .iter()
                    .find(|s| &s.id == sid)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| sid.to_string()),
                None => OTHER_GROUP_LABEL.to_string(),
            }
        };

        let node_count = match lane.kind {
            LaneKind::Direct => None,
            LaneKind::SubscriptionNode => Some(1),
            LaneKind::SubscriptionGroup => lane
                .subscription
                .as_ref()
                .and_then(|sid| counts.get(sid.as_str()))
                .and_then(|c| c.as_ref())
                .and_then(|s| lane.group.as_ref().and_then(|name| s.groups.get(name).copied())),
            LaneKind::SubscriptionAuto | LaneKind::Filter => lane
                .subscription
                .as_ref()
                .and_then(|sid| counts.get(sid.as_str()))
                .and_then(|c| c.as_ref())
                .map(|s| s.nodes),
        };

        let latency_ms = match (&snap, lane_target(&doc, lane)) {
            (Some(snap), Ok(target)) => {
                snap.proxies.iter().find(|p| p.name == target).and_then(|p| p.latest_delay())
            }
            _ => None,
        };

        let item = LanePickOptionItem {
            lane_id: lane.id.clone(),
            name: lane.name.clone(),
            kind: lane.kind,
            node_count,
            latency_ms,
        };
        match groups.iter_mut().find(|g| g.group == label) {
            Some(slot) => slot.options.push(item),
            None => groups.push(LanePickOption { group: label, options: vec![item] }),
        }
    }
    // 直连那一组永远是第一组（`docs/04` 8.3）
    groups.sort_by_key(|g| if g.group == DIRECT_GROUP_LABEL { 0 } else { 1 });
    Ok(groups)
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_pick_options(state: State<'_, AppState>) -> IpcResult<Vec<LanePickOption>> {
    compute_lane_pick_options(&state).await
}

// ================================================================ kernel 命令

/// `ta_kernel::KernelState` -> 契约里的那个串（`stopped` / `starting` / `ready` /
/// `restarting` / `failed`）。
///
/// 用**序列化**而不是手抄一份 match：`KernelState` 自己带 `#[serde(rename_all = "snake_case")]`，
/// 手抄的表迟早会和 `ta-kernel` 漂移（`app.rs` 里那份就是这么来的）。
fn state_label(s: &ta_kernel::KernelState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "stopped".to_string())
}

/// `ta_kernel::KernelStatus` -> IPC 视图。
///
/// 复用 `app.rs` 已经有的 `KernelStatusView`（它的注释写明了为什么不能直接用
/// `ta_kernel::KernelStatus`：那个类型在 P5 的 crate 里，没实现 `specta::Type`）。
/// 这里不再造第三个同形状的 DTO —— 形状漂移是最贵的那类 bug。
fn status_view(s: ta_kernel::KernelStatus) -> KernelStatusView {
    KernelStatusView {
        state: state_label(&s.state),
        version: s.version,
        pid: s.pid,
        restarts: s.restarts,
        last_error: s.last_error,
    }
}

/// `kernel_status`：没装配内核时返回 `stopped` + 一句可读原因
/// （**不是**返回一个假的 `ready`，也不是报错 —— 状态查询本身没有失败）。
pub async fn kernel_status_of(state: &AppState) -> IpcResult<KernelStatusView> {
    match state.kernel_slot() {
        Some(k) => Ok(status_view(k.status().await)),
        None => Ok(KernelStatusView {
            state: "stopped".to_string(),
            version: None,
            pid: None,
            restarts: 0,
            last_error: Some(
                "内核没有装配：net.kernel.path 为空（直连通道仍可用）；用 kernel_set_provider 指定内核路径"
                    .to_string(),
            ),
        }),
    }
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_status(state: State<'_, AppState>) -> IpcResult<KernelStatusView> {
    kernel_status_of(&state).await
}

/// `kernel_start`：走 `KernelController::start()`（拉起 -> 等 REST -> 等 provider），
/// 失败时如实返回错误，不返回一个"看起来起来了"的状态。
pub async fn start_kernel(state: &AppState) -> IpcResult<KernelStatusView> {
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    kernel.start().await.map_err(IpcError::from)?;
    Ok(status_view(kernel.status().await))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_start(state: State<'_, AppState>) -> IpcResult<KernelStatusView> {
    start_kernel(&state).await
}

pub async fn stop_kernel(state: &AppState) -> IpcResult<KernelStatusView> {
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    kernel.stop().await.map_err(IpcError::from)?;
    Ok(status_view(kernel.status().await))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_stop(state: State<'_, AppState>) -> IpcResult<KernelStatusView> {
    stop_kernel(&state).await
}

pub async fn restart_kernel(state: &AppState) -> IpcResult<KernelStatusView> {
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    kernel.restart().await.map_err(IpcError::from)?;
    Ok(status_view(kernel.status().await))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_restart(state: State<'_, AppState>) -> IpcResult<KernelStatusView> {
    restart_kernel(&state).await
}

/// 把当前配置（含 lane 端口表）同步进内核配置源。
///
/// 为什么 `kernel_reload_config` 要先做这一步：`config_apply` / `config_rollback` 是别人
/// 实现的，它们**不会**碰内核配置源 —— 所以 reload 的第一步必须是"让配置源追上真源"，
/// 否则 reload 出来的是启动那一刻的旧配置。
fn sync_kernel_source(state: &AppState) -> IpcResult<()> {
    let Some(source) = state.kernel_source_slot() else {
        tracing::warn!("内核配置源不在（kernel_source 槽为空）：reload 会用装配那一刻的配置");
        return Ok(());
    };
    let doc = (*state.doc()).clone();
    let ports = allocate_ports(state, &doc)?;
    source.set_doc(doc);
    source.set_ports(ports);
    Ok(())
}

/// `kernel_reload_config`：生成 + 落盘 + reload（没起来就直接起）。
///
/// 走的是 `KernelController::apply()` —— 内核层自己的编排，命令层不另写一套。
pub async fn reload_kernel_config(state: &AppState) -> IpcResult<KernelStatusView> {
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    sync_kernel_source(state)?;
    let status = kernel.apply().await.map_err(IpcError::from)?;
    tracing::info!(status = ?status.state, "内核配置已重新加载");
    Ok(status_view(status))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_reload_config(state: State<'_, AppState>) -> IpcResult<KernelStatusView> {
    reload_kernel_config(&state).await
}

/// `kernel_generated_config`：只读预览生成的 YAML。
///
/// 有配置源就用**真正会落盘的那一份**（`render()`）；没有配置源（没配内核路径）时
/// 用同一个 `MhConfigGen` 现生成一份 —— 预览也要是真实生成器的输出，不能是示意文本。
pub async fn generated_config(state: &AppState) -> IpcResult<String> {
    let doc = (*state.doc()).clone();
    let ports = allocate_ports(state, &doc)?;
    match state.kernel_source_slot() {
        Some(source) => {
            source.set_doc(doc);
            source.set_ports(ports);
            source.render().map_err(IpcError::from)
        }
        None => {
            let gen = MhConfigGen::random_secret().with_strict_refs(false);
            gen.generate(&doc, &ports).map_err(IpcError::from)
        }
    }
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_generated_config(state: State<'_, AppState>) -> IpcResult<String> {
    generated_config(&state).await
}

/// `kernel_log_tail`：读内核自己的日志尾巴。
///
/// 这里是**唯一**会把内核原文返回给前端的地方，所以按"够用就好"的脱敏：只过
/// `ta_core::diag::sanitize` 的规则表（`?token=` / `authorization:` 之类），
/// **不做** [`strip_url_details`] —— 内核日志里的 URL 大多数是健康检查地址，
/// 全抹成 `<redacted>` 会让日志彻底没法看。残留风险（token 写在 URL **路径**里）
/// 已登记在交付报告里。
pub async fn log_tail(state: &AppState, lines: u32) -> IpcResult<String> {
    let kernel = state.kernel_slot().ok_or_else(kernel_missing)?;
    let raw = kernel.log_tail(lines).await.map_err(IpcError::from)?;
    Ok(sanitize(&raw))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_log_tail(state: State<'_, AppState>, lines: u32) -> IpcResult<String> {
    log_tail(&state, lines).await
}

/// `kernel_set_provider`：改内核来源与可执行文件路径，**并真的把控制器换成新内核**。
///
/// 只改配置是不够的：装配出来的 `KernelProcessConfig` 里带着 program / 端口 / secret，
/// 配置改了不同步就等于"界面说换了、跑的还是旧的"。所以这里：
/// 写库 -> （原来在跑就先停）-> 用 `boot::build_kernel` 重新装配 -> 原来在跑就 `apply()` 起回来。
///
/// 新路径跑不起来时 `apply()` 会失败，命令如实报错 —— 那时**配置与控制器都已经是新的**，
/// 只是内核没起来（这正是用户需要知道的事）。
pub async fn set_kernel_provider(
    state: &AppState,
    provider: KernelProvider,
    path: Option<String>,
) -> IpcResult<KernelStatusView> {
    let mut doc = (*state.doc()).clone();
    doc.net.kernel.provider = provider;
    if let Some(p) = path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        doc.net.kernel.path = Some(p.to_string());
    }
    let effective = doc.net.kernel.path.as_deref().map(str::trim).unwrap_or("");
    if effective.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(
            "provider: external / bundled 都需要一个内核可执行文件路径（本仓库不随包分发内核，\
             见 docs/04 第 10 节的方案 B）—— 请在 path 里给出内核可执行文件的位置"
                .to_string(),
        )));
    }

    commit_config(state, doc.clone(), "kernel_set_provider").await?;

    // 旧控制器：先停（不然换装配之后旧进程还在，端口 / 配置都对不上）
    let was_running = match state.kernel_slot() {
        Some(old) => {
            let running = old.status().await.state != KernelState::Stopped;
            if running {
                if let Err(e) = old.stop().await {
                    tracing::warn!(code = e.code(), error = %e, "切换内核前停止旧内核失败（继续换装配）");
                }
            }
            running
        }
        None => false,
    };

    let data_dir = state.data_dir();
    let built = match crate::boot::build_kernel(&doc, &data_dir) {
        Ok(Some(built)) => built,
        Ok(None) => {
            return Err(IpcError::from(TaError::Kernel(
                "内核装配返回空（net.kernel.path 为空）".to_string(),
            )))
        }
        Err(e) => {
            tracing::error!(code = e.code(), error = %e, "按新配置重新装配内核失败");
            return Err(IpcError::from(e));
        }
    };

    let controller: Arc<dyn KernelController> = built.supervisor.clone();
    state.set_kernel(controller.clone());
    state.set_kernel_source(built.source.clone());
    state.set_kernel_supervisor(built.supervisor.clone());
    tracing::info!(
        program = %built.supervisor.config().program.display(),
        controller = %built.supervisor.config().controller,
        "内核控制器已按新配置重新装配"
    );

    if was_running {
        controller.apply().await.map_err(IpcError::from)?;
    }
    Ok(status_view(controller.status().await))
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_set_provider(
    state: State<'_, AppState>,
    provider: KernelProvider,
    path: Option<String>,
) -> IpcResult<KernelStatusView> {
    set_kernel_provider(&state, provider, path).await
}

/// `kernel_probe_path`：**这个路径上的内核到底能不能用**。
///
/// `found` 只看"文件在不在"（Windows 下路径没写扩展名时会再试 `.exe`）；
/// `version` 是跑 `<path> -v` 拿到的第一行（stdin 关掉，免得某些程序等输入卡住），
/// 拿不到就是 `None` —— "文件在、但跑不起来拿不到版本"和"文件不在"是两件事，
/// 所以两者分开表达，不合并成一个 `found: false`。
pub async fn probe_kernel_path(state: &AppState) -> IpcResult<KernelProbe> {
    let doc = state.doc();
    let configured =
        doc.net.kernel.path.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from);
    drop(doc);

    let Some(configured) = configured else {
        return Ok(KernelProbe { found: false, path: None, version: None });
    };
    let resolved = if configured.is_file() {
        Some(configured.clone())
    } else if cfg!(windows) && configured.extension().is_none() {
        let with_exe = configured.with_extension("exe");
        if with_exe.is_file() {
            Some(with_exe)
        } else {
            None
        }
    } else {
        None
    };

    match resolved {
        Some(exe) => Ok(KernelProbe {
            found: true,
            path: Some(exe.to_string_lossy().to_string()),
            version: probe_version(&exe),
        }),
        // 找不到时把"你配的那条"回给 UI：不然用户不知道去哪改
        None => Ok(KernelProbe {
            found: false,
            path: Some(configured.to_string_lossy().to_string()),
            version: None,
        }),
    }
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn kernel_probe_path(state: State<'_, AppState>) -> IpcResult<KernelProbe> {
    probe_kernel_path(&state).await
}

fn probe_version(exe: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(exe)
        .arg("-v")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let text = if out.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let bounded: String = first.chars().take(200).collect();
    Some(sanitize(bounded.trim()))
}

// ================================================================ 纯函数助手

fn node_view_of(n: &ta_subscription::NodeMeta) -> NodeView {
    NodeView { name: n.name.clone(), kind: n.kind.clone(), group: None, latency_ms: None, ok: None }
}

/// 按**顶层键**截取一段原文（只读展示用）。找不到返回 `None`。
///
/// 判据刻意简单：列 0 的 `key:` 是顶层键，缩进行属于上一段，下一个列 0 的非空非注释行结束本段。
/// 不做 YAML 解析（`tokenagency` 没有 `serde_yaml_ng` 依赖）—— 返回值是**原文切片**，
/// 显示用的东西原样给出去，不改写。
fn top_level_block(text: &str, key: &str) -> Option<String> {
    let header = format!("{key}:");
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let at_column_zero = !line.is_empty() && !line.starts_with(|c: char| c.is_whitespace());
        if inside {
            if at_column_zero {
                break;
            }
            out.push(line);
            continue;
        }
        if at_column_zero && line.starts_with(&header) {
            inside = true;
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use ta_core::domain::{ChannelConfig, Credential, Model, Provider};
    use ta_core::types::Protocol;
    use ta_kernel::{KernelStatus, NodeRef, NodeSource, ProvidersSnapshot, ProxyEntry};

    // ------------------------------------------------------------ 环境脚手架

    /// 一个**真的库**（临时目录里的 SQLite）+ 真的数据目录。
    async fn fresh_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-net-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    fn sub_cfg(id: &str, url: &str) -> Subscription {
        Subscription {
            id: SubscriptionId::new(id),
            name: format!("机场-{id}"),
            url: url.to_string(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            format: SubscriptionFormat::Auto,
            interval_s: 6 * 3600,
            keep_versions: 3,
            filter: Default::default(),
            health_check: Default::default(),
            enabled: true,
            provider_key: None,
            extra: serde_json::Map::new(),
        }
    }

    /// 种一份引用完整的最小配置：provider / credential / model / channel + 一份订阅 + 两条 lane。
    async fn seeded(tag: &str, sub_url: &str) -> AppState {
        let state = fresh_state(tag).await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.credentials.push(Credential::new("c-a", "p-a", "A", "${secret:a}"));
        doc.models.push(Model::new("m-a"));
        doc.channels.push(ChannelConfig::new("ch-a", "m-a", "p-a", "c-a", "upstream-a"));
        doc.proxy_subscriptions.push(sub_cfg("sub-a", sub_url));
        doc.lanes.push(Lane::direct("lane-direct", "直连本机网络"));
        doc.lanes.push(Lane::subscription_auto("lane-auto", "自动择优", "sub-a"));
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        state
    }

    // ------------------------------------------------------------ 本地 HTTP 假机场

    /// 一次脚本化的响应：`(响应体, 额外的真实响应头行)`。
    ///
    /// 起别名不是为了短，是因为 `Arc<Mutex<Vec<(String, Vec<String>)>>>` 会被
    /// clippy 的 `type_complexity` 拦下（门禁带 `-D warnings`）。
    type ScriptedResponse = (String, Vec<String>);

    /// 极小的本地 HTTP/1.1 服务：按顺序回放脚本化的响应体（第 n 次请求用第 n 个，
    /// 用完之后一直用最后一个）。**这是真 HTTP**：真 reqwest、真解析、真版本存储。
    ///
    /// 每次响应还能带一组**真实响应头**（订阅流量那类用例需要它：
    /// `subscription-userinfo` 只有真经过 HTTP 才谈得上"读到了"）。
    struct FakeAirport {
        addr: String,
        hits: Arc<AtomicUsize>,
        shutdown: Arc<AtomicBool>,
    }

    impl FakeAirport {
        fn start(bodies: Vec<String>) -> Self {
            Self::scripted(bodies.into_iter().map(|b| (b, Vec::new())).collect())
        }

        /// 每一次响应都带同一组头（`"Name: value"` 的原文行）。
        fn with_headers(bodies: Vec<String>, headers: &[&str]) -> Self {
            let headers: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
            Self::scripted(bodies.into_iter().map(|b| (b, headers.clone())).collect())
        }

        /// 逐次脚本：第 n 次响应 = 第 n 组（体, 头）。
        fn scripted(scripts: Vec<ScriptedResponse>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定本地端口");
            listener.set_nonblocking(true).expect("非阻塞");
            let addr = format!("http://{}", listener.local_addr().expect("本地地址"));
            let hits = Arc::new(AtomicUsize::new(0));
            let shutdown = Arc::new(AtomicBool::new(false));
            let scripts = Arc::new(Mutex::new(scripts));
            let h = hits.clone();
            let sd = shutdown.clone();
            std::thread::spawn(move || loop {
                if sd.load(Ordering::SeqCst) {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => serve_one(stream, &scripts, &h),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            });
            Self { addr, hits, shutdown }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.addr, path)
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    impl Drop for FakeAirport {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
        }
    }

    fn serve_one(
        mut stream: TcpStream,
        scripts: &Arc<Mutex<Vec<ScriptedResponse>>>,
        hits: &Arc<AtomicUsize>,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        // 把请求头读完（不关心内容，只要别在客户端还在写的时候抢着回包）
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let n = hits.fetch_add(1, Ordering::SeqCst);
        let (body, headers) = {
            let list = scripts.lock().unwrap_or_else(|e| e.into_inner());
            list.get(n.min(list.len().saturating_sub(1))).cloned().unwrap_or_default()
        };
        let mut head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/yaml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for h in &headers {
            head.push_str(h);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.flush();
    }

    fn yaml_with(nodes: usize, groups: usize, rules: bool) -> String {
        let mut s = String::from("proxies:\n");
        for i in 0..nodes {
            s.push_str(&format!(
                "  - {{name: 节点{i}, type: ss, server: 1.2.3.{i}.example.com, port: 443}}\n"
            ));
        }
        s.push_str("proxy-groups:\n");
        for g in 0..groups {
            let members: Vec<String> = (0..nodes).map(|i| format!("\"节点{i}\"")).collect();
            s.push_str(&format!(
                "  - {{name: 组{g}, type: url-test, proxies: [{}]}}\n",
                members.join(", ")
            ));
        }
        if rules {
            s.push_str("rules:\n  - DOMAIN-SUFFIX,example.com,DIRECT\n  - MATCH,组0\n");
        }
        s
    }

    // ------------------------------------------------------------ 假内核控制器

    #[derive(Debug)]
    struct FakeInner {
        state: KernelState,
        calls: Vec<String>,
        applied: usize,
    }

    impl Default for FakeInner {
        fn default() -> Self {
            Self { state: KernelState::Stopped, calls: Vec::new(), applied: 0 }
        }
    }

    struct FakeKernel {
        inner: Mutex<FakeInner>,
    }

    impl FakeKernel {
        fn new(state: KernelState) -> Arc<Self> {
            Arc::new(Self { inner: Mutex::new(FakeInner { state, ..Default::default() }) })
        }

        fn calls(&self) -> Vec<String> {
            self.inner.lock().unwrap().calls.clone()
        }

        fn applied(&self) -> usize {
            self.inner.lock().unwrap().applied
        }
    }

    fn fake_snapshot() -> ProvidersSnapshot {
        let mut nodes: BTreeMap<String, NodeRef> = BTreeMap::new();
        for name in ["节点0", "节点1"] {
            nodes.insert(
                name.to_string(),
                NodeRef {
                    name: name.to_string(),
                    kind: "Shadowsocks".to_string(),
                    alive: true,
                    source: NodeSource::TopLevel,
                },
            );
        }
        nodes.insert(
            "DIRECT".to_string(),
            NodeRef {
                name: "DIRECT".to_string(),
                kind: "Direct".to_string(),
                alive: true,
                source: NodeSource::TopLevel,
            },
        );
        let proxies = vec![
            ProxyEntry {
                name: "LANE-lane-auto".to_string(),
                kind: "URLTest".to_string(),
                alive: true,
                now: Some("节点0".to_string()),
                all: vec!["节点0".to_string(), "节点1".to_string()],
                history: vec![],
            },
            ProxyEntry {
                name: "DIRECT".to_string(),
                kind: "Direct".to_string(),
                alive: true,
                now: None,
                all: vec![],
                history: vec![],
            },
        ];
        ProvidersSnapshot { proxies, providers: vec![], nodes, dangling: vec![] }
    }

    #[async_trait]
    impl KernelController for FakeKernel {
        async fn status(&self) -> KernelStatus {
            let g = self.inner.lock().unwrap();
            KernelStatus { state: g.state.clone(), ..Default::default() }
        }
        async fn start(&self) -> ta_core::error::Result<()> {
            let mut g = self.inner.lock().unwrap();
            g.calls.push("start".into());
            g.state = KernelState::Ready;
            Ok(())
        }
        async fn stop(&self) -> ta_core::error::Result<()> {
            let mut g = self.inner.lock().unwrap();
            g.calls.push("stop".into());
            g.state = KernelState::Stopped;
            Ok(())
        }
        async fn restart(&self) -> ta_core::error::Result<()> {
            let mut g = self.inner.lock().unwrap();
            g.calls.push("restart".into());
            g.state = KernelState::Ready;
            Ok(())
        }
        async fn apply(&self) -> ta_core::error::Result<KernelStatus> {
            let mut g = self.inner.lock().unwrap();
            g.calls.push("apply".into());
            g.applied += 1;
            g.state = KernelState::Ready;
            Ok(KernelStatus { state: KernelState::Ready, ..Default::default() })
        }
        async fn set_node(&self, group: &str, node: &str) -> ta_core::error::Result<()> {
            self.inner.lock().unwrap().calls.push(format!("set_node:{group}:{node}"));
            Ok(())
        }
        async fn set_select(&self, lane: &LaneId, node: &str) -> ta_core::error::Result<()> {
            self.inner.lock().unwrap().calls.push(format!("set_select:{}:{node}", lane.as_str()));
            Ok(())
        }
        async fn delay_of(&self, name: &str, _timeout_ms: u64) -> ta_core::error::Result<u32> {
            if fake_snapshot().nodes.contains_key(name) {
                Ok(120)
            } else {
                Err(TaError::Kernel(format!("内核里没有 {name}")))
            }
        }
        async fn refresh_provider(&self, key: &str) -> ta_core::error::Result<()> {
            self.inner.lock().unwrap().calls.push(format!("refresh_provider:{key}"));
            Ok(())
        }
        async fn snapshot(&self) -> ta_core::error::Result<ProvidersSnapshot> {
            Ok(fake_snapshot())
        }
        async fn log_tail(&self, _lines: u32) -> ta_core::error::Result<String> {
            Ok("level=info msg=\"provider sub-a url=https://airport.example/sub?token=ta-allow-fake-secret-abc123\"\n".to_string())
        }
    }

    fn install_kernel(state: &AppState, kernel: Arc<dyn KernelController>) {
        state.set_kernel(kernel);
    }

    /// 装一个**真的** `GeneratedConfigSource`（走 `boot::build_kernel`，与生产同一条路）。
    fn install_source(state: &AppState, kernel_path: &str) {
        let mut doc = (*state.doc()).clone();
        doc.net.kernel.path = Some(kernel_path.to_string());
        let built = crate::boot::build_kernel(&doc, &state.data_dir())
            .expect("装配应成功")
            .expect("有路径就该装配");
        state.set_kernel_source(built.source.clone());
        state.set_kernel_supervisor(built.supervisor.clone());
    }

    fn history(state: &AppState, id: &str) -> Vec<ta_subscription::version::VersionMeta> {
        version_keeper(state).history(&SubscriptionId::new(id)).expect("版本历史")
    }

    // ------------------------------------------------------------ 脱敏

    #[test]
    fn scrub_keeps_host_drops_path_query_and_token() {
        let msg = "订阅下载失败：error sending request for url \
                   (https://airport.example/api/v1/client/subscribe?token=ta-allow-fake-secret-abc123&x=1): \
                   connection refused";
        let out = scrub(msg);
        assert!(!out.contains("ta-allow-fake-secret-abc123"), "token 必须消失：{out}");
        assert!(!out.contains("/api/v1/client"), "路径细节必须消失：{out}");
        assert!(out.contains("airport.example"), "host 要留着（排查需要）：{out}");
        assert!(out.contains("connection refused"), "原因要留着：{out}");
        assert!(out.contains("/<redacted>"), "要能看出这里原本有个 URL：{out}");
    }

    #[test]
    fn scrub_handles_path_style_tokens_and_plain_text() {
        let out = scrub("GET https://relay.example/sub/TA-ALLOW-FAKE-SECRET-PATHTOKEN123 failed");
        assert!(!out.contains("PATHTOKEN123"), "{out}");
        assert!(out.contains("relay.example"), "{out}");
        assert_eq!(scrub("普通中文，没有 URL"), "普通中文，没有 URL");
    }

    #[test]
    fn top_level_block_extracts_rules_only() {
        let yaml = "proxies:\n  - {name: a, type: ss}\nrules:\n  - MATCH,DIRECT\n  - DOMAIN,x,y\n\nproxy-groups:\n  - {name: g}\n";
        let block = top_level_block(yaml, "rules").expect("要有 rules 段");
        assert!(block.starts_with("rules:"), "{block}");
        assert!(block.contains("MATCH,DIRECT"), "{block}");
        assert!(!block.contains("proxy-groups"), "不许越过下一个顶层键：{block}");
        assert!(!block.contains("proxies:"), "{block}");
        assert_eq!(top_level_block(yaml, "tun"), None);
    }

    // ------------------------------------------------------------ subscription 写命令

    #[tokio::test]
    async fn subscription_upsert_writes_then_reads_back_and_overwrites() {
        let state = seeded("sub-upsert", "https://airport.example/sub").await;
        let mut s = sub_cfg("sub-b", "https://b.example/sub?token=ta-allow-fake-secret-x");
        upsert_subscription(&state, s.clone()).await.expect("新增");

        // 内存快照 + 真读库
        assert_eq!(upsert_subscription(&state, s.clone()).await.expect("幂等再写").id, s.id);
        let from_db = state.config.load().await.expect("load");
        assert_eq!(from_db.proxy_subscriptions.len(), 2, "同 id 不该重复");
        assert!(from_db.proxy_subscriptions.iter().any(|x| x.id == s.id));

        // 快照也要留（写路径的第 3 步）
        let snaps = state.config.list_snapshots(20).await.expect("列快照");
        assert!(
            snaps.iter().any(|m| m.label == "subscription_upsert"),
            "快照标签应能认出是这条命令：{:?}",
            snaps.iter().map(|m| m.label.clone()).collect::<Vec<_>>()
        );

        // 同 id 改名 -> 覆盖而不是新增
        s.name = "改名后".to_string();
        upsert_subscription(&state, s.clone()).await.expect("覆盖");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.proxy_subscriptions.len(), 2);
        assert_eq!(
            after.proxy_subscriptions.iter().find(|x| x.id == s.id).map(|x| x.name.clone()),
            Some("改名后".to_string())
        );
    }

    #[tokio::test]
    async fn subscription_upsert_refuses_invalid_and_writes_nothing() {
        let state = seeded("sub-bad", "https://airport.example/sub").await;
        let mut bad = sub_cfg("sub-bad", "https://x.example/sub");
        bad.url = String::new();
        let err = upsert_subscription(&state, bad).await.expect_err("空 url 必须被拒");
        assert_eq!(err.code, "TA-5001");
        assert_eq!(
            state.config.load().await.expect("load").proxy_subscriptions.len(),
            1,
            "校验不过时一个字节都不该写"
        );
    }

    #[tokio::test]
    async fn subscription_delete_removes_unreferenced_and_refuses_referenced() {
        let state = seeded("sub-del", "https://airport.example/sub").await;
        // sub-a 被 lane-auto 引用 -> 必须被引用完整性拦住
        let err = delete_subscription(&state, &SubscriptionId::new("sub-a"))
            .await
            .expect_err("被引用的订阅不许删");
        assert_eq!(err.code, "TA-5001", "{}", err.message);
        assert_eq!(state.config.load().await.expect("load").proxy_subscriptions.len(), 1);

        // 没人引用 -> 能删，且真的落库
        upsert_subscription(&state, sub_cfg("sub-free", "https://free.example/sub")).await.unwrap();
        delete_subscription(&state, &SubscriptionId::new("sub-free")).await.expect("删掉没人用的");
        let after = state.config.load().await.expect("load");
        assert!(!after.proxy_subscriptions.iter().any(|s| s.id.as_str() == "sub-free"));

        // 不存在的 id 不许静默成功
        assert!(delete_subscription(&state, &SubscriptionId::new("sub-nope")).await.is_err());
    }

    // ------------------------------------------------------------ subscription 刷新（真 HTTP）

    #[tokio::test]
    async fn subscription_refresh_fetches_accepts_and_records_a_version() {
        let airport = FakeAirport::start(vec![yaml_with(5, 2, true)]);
        let state = seeded("sub-refresh", &airport.url("/sub")).await;

        let r = refresh_subscriptions(&state, None, false).await.expect("刷新");
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.node_count, 5);
        assert_eq!(r.group_count, 2);
        assert_eq!(airport.hits(), 1, "只该发一次请求");

        // 生效版本真的落到了磁盘（版本存储）
        let h = history(&state, "sub-a");
        assert_eq!(h.len(), 1);
        assert!(h[0].accepted);
        assert_eq!(h[0].node_count, 5);
    }

    #[tokio::test]
    async fn subscription_refresh_rejects_html_and_keeps_the_previous_version() {
        let airport = FakeAirport::start(vec![
            yaml_with(5, 2, false),
            "<!DOCTYPE html><html><body>502 Bad Gateway</body></html>".to_string(),
        ]);
        let state = seeded("sub-badupdate", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        let first = refresh_subscriptions(&state, Some(id.clone()), true).await.expect("首次");
        assert!(first.ok);

        let second = refresh_subscriptions(&state, Some(id.clone()), true).await.expect("第二次");
        assert!(!second.ok, "HTML 错误页必须被拒：{:?}", second.error);
        assert!(second.error.as_deref().unwrap_or("").contains("HTML"), "{:?}", second.error);
        assert_eq!(second.node_count, 5, "生效的仍是上一版的 5 个节点");

        // 不看返回值，直接从版本存储 / 磁盘读回来
        let nodes = nodes_of_subscription(&state, &id, None).await.expect("节点");
        assert_eq!(nodes.len(), 5, "上一版必须还在");
        let h = history(&state, "sub-a");
        assert_eq!(h.len(), 2, "坏更新也要留档（UI 要能解释为什么没采纳）");
        assert!(!h[0].accepted, "留档的那版是 rejected");
    }

    #[tokio::test]
    async fn subscription_refresh_skips_when_fresh_unless_forced() {
        let airport = FakeAirport::start(vec![yaml_with(4, 1, false)]);
        let state = seeded("sub-skip", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        refresh_subscriptions(&state, Some(id.clone()), false).await.expect("首次");
        assert_eq!(airport.hits(), 1);

        // 距上次成功不到 interval_s（6h）-> 不发请求
        let again = refresh_subscriptions(&state, Some(id.clone()), false).await.expect("再刷");
        assert!(again.ok);
        assert_eq!(again.node_count, 4);
        assert_eq!(airport.hits(), 1, "没到期就不该再打机场");

        // force 才真的再拉一次
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("强刷");
        assert_eq!(airport.hits(), 2);
    }

    #[tokio::test]
    async fn refresh_download_failure_is_reported_without_leaking_the_token() {
        // 127.0.0.1:1 上什么都没有 -> 连接必失败。URL 里**故意**放一个带 token 的查询参数。
        let token = "ta-allow-fake-secret-REFRESHTOKEN";
        let state = seeded("sub-dead", &format!("http://127.0.0.1:1/sub?token={token}")).await;

        let r = refresh_subscriptions(&state, None, true).await.expect("下载失败不该是 Err");
        assert!(!r.ok, "下载失败必须如实报告");
        let err = r.error.expect("要有原因");
        assert!(!err.contains(token), "订阅 token 不许出现在返回给前端的错误里：{err}");
        assert!(err.contains("下载失败"), "{err}");
    }

    #[tokio::test]
    async fn subscription_preview_reads_but_never_writes() {
        let airport = FakeAirport::start(vec![yaml_with(3, 1, true)]);
        let state = fresh_state("sub-preview").await;
        let lanes_before = state.doc().lanes.len();

        let p = preview_subscription(&airport.url("/sub"), None, None).await.expect("预览");
        assert_eq!(p.nodes.len(), 3);
        assert_eq!(p.groups, vec!["组0".to_string()]);
        assert_eq!(p.sample.len(), 3);
        assert_eq!(p.sample[0].kind, "ss");
        // 预览是纯读：库与内存快照都不许动
        assert_eq!(state.config.load().await.expect("load").proxy_subscriptions.len(), 0);
        assert_eq!(state.doc().lanes.len(), lanes_before);
    }

    #[tokio::test]
    async fn subscription_nodes_and_groups_come_from_the_effective_version() {
        let airport = FakeAirport::start(vec![yaml_with(3, 2, false)]);
        let state = seeded("sub-views", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("刷新");

        let nodes = nodes_of_subscription(&state, &id, None).await.expect("节点");
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].name, "节点0");
        assert_eq!(nodes[1].kind, "ss");

        let groups = groups_of_subscription(&state, &id).await.expect("分组");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "组0");
        assert_eq!(groups[0].kind, "url-test", "组类型原样返回，不做枚举归一");
        assert_eq!(groups[0].node_count, 3);

        // 按组过滤：能力真没有，必须明确报错而不是静默返回全部
        let err = nodes_of_subscription(&state, &id, Some("组0".to_string()))
            .await
            .expect_err("按组过滤未实现");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("不保留组成员关系"), "{}", err.message);

        // 没刷新过的订阅：明确说"还没有已采纳的版本"
        upsert_subscription(&state, sub_cfg("sub-empty", "https://x.example/sub")).await.unwrap();
        let err = nodes_of_subscription(&state, &SubscriptionId::new("sub-empty"), None)
            .await
            .expect_err("没有版本");
        assert!(err.message.contains("还没有已采纳的版本"), "{}", err.message);
    }

    // ------------------------------------------------------------ subscription_list 的解析状态

    /// `subscription_list` 必须带上**当前生效版本**的解析状态（[docs/19 §2.3] 那条硬前置）。
    ///
    /// 这是"订阅加好了、刷新也成功了、页面却说没有解析结果、一个节点都不显示"那个真机缺陷的
    /// 回归测试：判据字段缺失时节点页**不敢发** `subscription_nodes`，节点清单永远是空的。
    #[tokio::test]
    async fn subscription_list_carries_the_effective_parse_status() {
        let airport = FakeAirport::start(vec![yaml_with(3, 2, false)]);
        let state = seeded("sub-view-status", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        // (1) 从没刷新过：字段**缺失**（不是 0 / 空串 / 空对象），页面据此判"没解析过"
        let before = views_of_subscriptions(&state).expect("清单");
        assert_eq!(before.len(), 1);
        let raw = serde_json::to_value(&before[0]).expect("序列化");
        assert!(raw.get("subscription").is_some(), "实体要包在 subscription 里：{raw}");
        assert!(raw.get("id").is_none(), "不许把实体摊平（前端按外层有无 subscription 判形状）");
        assert_eq!(raw["provider_key"], "sub-a", "配置没给 provider_key 时按 id 派生");
        assert!(raw.get("node_count").is_none(), "没刷过就不许给 0：{raw}");
        assert!(raw.get("group_count").is_none(), "没刷过就不许给 0：{raw}");
        assert!(raw.get("last_update_ms").is_none(), "没刷过就不许给时间戳：{raw}");
        assert!(raw.get("parser").is_none(), "没刷过就没有格式：{raw}");
        assert!(raw.get("error").is_none(), "没刷过不等于出错过：{raw}");

        // (2) 刷新一版好内容：三个字段全部来自**生效版本**
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("刷新");
        let after = views_of_subscriptions(&state).expect("清单");
        let view = &after[0];
        assert_eq!(view.node_count, Some(3));
        assert_eq!(view.group_count, Some(2));
        assert_eq!(view.parser.as_deref(), Some("clash_yaml"));
        assert_eq!(view.error, None, "这一版是好的，不该有拦截说明");
        assert_eq!(view.provider_key, "sub-a");
        let meta = history(&state, "sub-a").into_iter().find(|m| m.accepted).expect("已采纳版本");
        assert_eq!(view.last_update_ms, Some(meta.fetched_at_ms), "要用生效版本的抓取时刻");
    }

    /// 最新一次被坏更新拦下：`error` 必须有值，**计数仍来自上一版已采纳的内容**。
    ///
    /// 前端据此仍然发 `subscription_nodes`（[19 §2.3]：有 `error` = 后端确实处理过这份订阅），
    /// 而不是替后端下"没解析过"的结论。
    #[tokio::test]
    async fn subscription_list_reports_a_rejected_latest_attempt_and_keeps_the_old_counts() {
        let airport = FakeAirport::start(vec![
            yaml_with(4, 1, false),
            "<!DOCTYPE html><html>err</html>".to_string(),
        ]);
        let state = seeded("sub-view-rejected", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v1 是好内容");
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v2 是坏内容，会被拦下");

        let views = views_of_subscriptions(&state).expect("清单");
        let view = &views[0];
        assert_eq!(view.node_count, Some(4), "计数仍来自上一版已采纳的内容");
        assert!(view.last_update_ms.is_some(), "仍然有生效版本");
        let err = view.error.as_deref().expect("必须说明这次被拦下了");
        assert!(err.contains("坏更新"), "{err}");
        assert!(err.contains("v2"), "要说清是哪一版被拦下：{err}");
        assert!(err.contains("v1"), "要说清当前用的是哪一版：{err}");
    }

    #[tokio::test]
    async fn subscription_rules_raw_returns_the_original_block_or_none() {
        let airport = FakeAirport::start(vec![yaml_with(2, 1, true)]);
        let state = seeded("sub-rules", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("刷新");

        let raw = rules_raw_of_subscription(&state, &id).await.expect("rules").expect("有 rules");
        assert!(raw.contains("DOMAIN-SUFFIX,example.com,DIRECT"), "{raw}");
        assert!(raw.contains("MATCH,组0"), "{raw}");
        assert!(!raw.contains("proxy-groups"), "只该给 rules 段：{raw}");

        // 没有 rules 的订阅 -> None（不是空串）
        let airport2 = FakeAirport::start(vec![yaml_with(2, 1, false)]);
        let state2 = seeded("sub-norules", &airport2.url("/sub")).await;
        refresh_subscriptions(&state2, None, true).await.expect("刷新");
        assert_eq!(
            rules_raw_of_subscription(&state2, &SubscriptionId::new("sub-a")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn subscription_rollback_restores_old_content_and_refuses_bad_versions() {
        let airport = FakeAirport::start(vec![
            yaml_with(4, 1, false),
            yaml_with(6, 1, false),
            "<!DOCTYPE html><html>err</html>".to_string(),
        ]);
        let state = seeded("sub-rollback", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v1");
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v2");
        assert_eq!(nodes_of_subscription(&state, &id, None).await.unwrap().len(), 6);

        // 回滚到 v1（4 个节点）
        rollback_subscription(&state, &id, 1).await.expect("回滚到 v1");
        let nodes = nodes_of_subscription(&state, &id, None).await.expect("节点");
        assert_eq!(nodes.len(), 4, "回滚后生效的必须是 v1 的内容");
        let h = history(&state, "sub-a");
        assert_eq!(h.len(), 3, "回滚把旧内容记成了新的一版");
        assert!(h[0].accepted);

        // 回滚到不存在的版本 -> 报错
        assert!(rollback_subscription(&state, &id, 99).await.is_err());

        // 回滚到被拒的坏版本 -> 报错（不然等于主动采纳一份坏订阅）
        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v4 = HTML，被拒");
        let rejected = history(&state, "sub-a")
            .into_iter()
            .find(|m| !m.accepted)
            .expect("要有留档的坏版本")
            .version;
        let err =
            rollback_subscription(&state, &id, rejected as u32).await.expect_err("坏版本不许回滚");
        assert!(err.message.contains("被拒绝"), "{}", err.message);
    }

    // ------------------------------------------------------------ 订阅流量与更新时间

    /// 用户原话里那一行（docs/18 §1）：更新时间 + 已用/总量。
    /// 这里用的字节数就是真实量级（130 GiB 套餐 / 已用约 92.9 GB）。
    const USERINFO: &str =
        "Subscription-Userinfo: upload=123456789; download=98765432100; total=139586437120; expire=1893456000";

    #[tokio::test]
    async fn refresh_returns_traffic_and_persists_the_last_known() {
        let airport = FakeAirport::with_headers(vec![yaml_with(5, 2, false)], &[USERINFO]);
        let state = seeded("sub-traffic", &airport.url("/sub")).await;

        // ① 从未刷新过：`traffic: null`（**不是**"有对象但字段全是 0/null"）
        let before = traffic_of_subscriptions(&state).await.expect("读流量");
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id, "sub-a");
        assert!(before[0].traffic.is_none(), "从未成功刷新过必须是 None");

        // ② 真刷新（真 HTTP + 真响应头），返回值里带回流量
        let r = refresh_subscriptions(&state, Some(SubscriptionId::new("sub-a")), true)
            .await
            .expect("刷新");
        assert!(r.ok, "{:?}", r.error);
        let t = r.traffic.expect("成功刷新要带回流量");
        assert_eq!(t.upload_bytes, Some(123_456_789));
        assert_eq!(t.download_bytes, Some(98_765_432_100));
        assert_eq!(t.total_bytes, Some(139_586_437_120));
        assert_eq!(t.expire_at_s, Some(1_893_456_000));
        assert!(t.fetched_at_ms > 0, "更新时间必须是个真时刻");

        // ③ ★ 不看返回值：从库里读回来（"上次已知"落库了）
        let after = traffic_of_subscriptions(&state).await.expect("读流量");
        assert_eq!(after[0].id, "sub-a");
        let stored = after[0].traffic.as_ref().expect("落库了就该读得到");
        assert_eq!(stored.download_bytes, Some(98_765_432_100));
        assert_eq!(stored.total_bytes, Some(139_586_437_120));
        assert_eq!(stored.expire_at_s, Some(1_893_456_000));
        assert_eq!(stored.upload_bytes, Some(123_456_789));

        // ④ 混着来：第二份订阅没刷过 → 同一个返回里它是 null，第一份不受影响
        upsert_subscription(&state, sub_cfg("sub-b", "https://b.example/sub")).await.unwrap();
        let mixed = traffic_of_subscriptions(&state).await.expect("读流量");
        assert_eq!(mixed.len(), 2, "以**配置里的订阅清单**为准");
        assert!(mixed.iter().find(|r| r.id == "sub-b").unwrap().traffic.is_none());
        assert!(mixed.iter().find(|r| r.id == "sub-a").unwrap().traffic.is_some());
    }

    #[tokio::test]
    async fn rejected_update_or_missing_header_keeps_the_last_known_traffic() {
        // 第 1 次：好内容 + 头；第 2 次：HTML（被坏更新拦截）；第 3 次：好内容但**没有头**
        let airport = FakeAirport::scripted(vec![
            (yaml_with(5, 1, false), vec![USERINFO.to_string()]),
            (
                "<!DOCTYPE html><html><body>502 Bad Gateway</body></html>".to_string(),
                vec![USERINFO.to_string()],
            ),
            (yaml_with(5, 1, false), vec![]),
        ]);
        let state = seeded("sub-traffic-keep", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("首次");
        let first = traffic_of_subscriptions(&state).await.unwrap();
        let known = first[0].traffic.clone().expect("第一次就记下了");

        // 坏更新（HTML）：本次没采纳 —— 表里仍是上次已知值（不许被清掉）
        let bad = refresh_subscriptions(&state, Some(id.clone()), true).await.expect("第二次");
        assert!(!bad.ok, "HTML 必须被拒：{:?}", bad.error);
        let after_bad = traffic_of_subscriptions(&state).await.unwrap();
        assert_eq!(after_bad[0].traffic.as_ref(), Some(&known), "坏更新不该动流量快照");

        // 机场不再给头：本次 traffic 是 null（"这次没给"），但"上次已知"要留着
        let no_header =
            refresh_subscriptions(&state, Some(id.clone()), true).await.expect("第三次");
        assert!(no_header.ok, "{:?}", no_header.error);
        assert!(no_header.traffic.is_none(), "没给头 → 本次就是 null");
        let after = traffic_of_subscriptions(&state).await.unwrap();
        assert_eq!(after[0].traffic.as_ref(), Some(&known), "没给头不许拿 NULL 覆盖『上次已知』");
    }

    /// 一次刷多份订阅时**不编造**聚合流量（不同机场的字节数不能相加）：
    /// `traffic` 为 null，逐份的数字请走 `subscription_traffic`。
    #[tokio::test]
    async fn refresh_all_does_not_invent_an_aggregate_traffic() {
        let airport = FakeAirport::with_headers(vec![yaml_with(4, 1, false)], &[USERINFO]);
        let state = seeded("sub-traffic-all", &airport.url("/sub")).await;
        upsert_subscription(&state, sub_cfg("sub-b", &airport.url("/sub2"))).await.unwrap();

        let r = refresh_subscriptions(&state, None, true).await.expect("刷全部");
        assert!(r.ok, "{:?}", r.error);
        assert!(r.traffic.is_none(), "多份订阅没有可加的聚合流量");
        assert_eq!(r.node_count, 8, "两份各 4 个节点");

        // 但两份各自的"上次已知"都落库了
        let per_sub = traffic_of_subscriptions(&state).await.unwrap();
        assert_eq!(per_sub.len(), 2);
        assert!(per_sub.iter().all(|r| r.traffic.is_some()), "{per_sub:?}");
        assert_eq!(per_sub[0].traffic.as_ref().unwrap().total_bytes, Some(139_586_437_120));
    }

    /// 没到期就不重拉（force=false）：这次没有请求，`traffic` 是**上次已知**（不是 null）。
    #[tokio::test]
    async fn skipped_refresh_returns_the_last_known_traffic() {
        let airport = FakeAirport::with_headers(vec![yaml_with(3, 1, false)], &[USERINFO]);
        let state = seeded("sub-traffic-skip", &airport.url("/sub")).await;
        let id = SubscriptionId::new("sub-a");

        refresh_subscriptions(&state, Some(id.clone()), true).await.expect("首次");
        assert_eq!(airport.hits(), 1);
        let again = refresh_subscriptions(&state, Some(id.clone()), false).await.expect("再刷");
        assert!(again.ok);
        assert_eq!(airport.hits(), 1, "没到期不该再打机场");
        assert_eq!(
            again.traffic.as_ref().and_then(|t| t.total_bytes),
            Some(139_586_437_120),
            "没重拉时显示的是『上次已知』"
        );
    }

    // ------------------------------------------------------------ lane 命令

    #[tokio::test]
    async fn lane_upsert_writes_then_reads_back_and_overwrites() {
        let state = seeded("lane-upsert", "https://airport.example/sub").await;
        let lane = Lane::subscription_node("lane-fixed", "固定节点", "sub-a", "节点0");
        upsert_lane(&state, lane.clone()).await.expect("新增");
        let db = state.config.load().await.expect("load");
        assert_eq!(db.lanes.len(), 3);
        assert!(db.lanes.iter().any(|l| l.id == lane.id));
        assert!(state.doc().lanes.iter().any(|l| l.id == lane.id), "内存快照要立刻反映");

        let mut renamed = lane.clone();
        renamed.name = "改名后".to_string();
        upsert_lane(&state, renamed).await.expect("覆盖");
        let after = state.config.load().await.expect("load");
        assert_eq!(after.lanes.len(), 3, "同 id 不该重复");
        assert_eq!(
            after.lanes.iter().find(|l| l.id == lane.id).map(|l| l.name.clone()),
            Some("改名后".to_string())
        );
    }

    #[tokio::test]
    async fn lane_upsert_refuses_unknown_subscription_and_missing_kind_field() {
        let state = seeded("lane-bad", "https://airport.example/sub").await;
        // 引用了不存在的订阅
        let bad = Lane::subscription_auto("lane-bad", "坏的", "sub-nope");
        let err = upsert_lane(&state, bad).await.expect_err("引用不存在的订阅必须被拒");
        assert_eq!(err.code, "TA-5001");
        // kind 需要字段却没给
        let mut missing = Lane::subscription_auto("lane-missing", "缺字段", "sub-a");
        missing.subscription = None;
        assert!(upsert_lane(&state, missing).await.is_err());
        assert_eq!(state.config.load().await.expect("load").lanes.len(), 2, "一个都不该写进去");
    }

    #[tokio::test]
    async fn lane_delete_removes_unreferenced_and_refuses_referenced() {
        let state = seeded("lane-del", "https://airport.example/sub").await;
        let mut doc = (*state.doc()).clone();
        doc.channels[0].lane = Some(LaneId::new("lane-auto"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let err =
            delete_lane(&state, &LaneId::new("lane-auto")).await.expect_err("被通道引用不许删");
        assert_eq!(err.code, "TA-5001", "{}", err.message);
        assert_eq!(state.config.load().await.expect("load").lanes.len(), 2);

        // 解除引用后能删
        let mut doc = (*state.doc()).clone();
        doc.channels[0].lane = None;
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);
        delete_lane(&state, &LaneId::new("lane-auto")).await.expect("删掉没人用的");
        assert_eq!(state.config.load().await.expect("load").lanes.len(), 1);

        assert!(delete_lane(&state, &LaneId::new("lane-nope")).await.is_err());
    }

    #[tokio::test]
    async fn lane_set_node_pins_through_the_kernel_and_rejects_unknown_node() {
        let state = seeded("lane-pin", "https://airport.example/sub").await;
        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());

        set_lane_node(&state, &LaneId::new("lane-auto"), "节点0").await.expect("钉住");
        assert_eq!(fake.calls(), vec!["set_select:lane-auto:节点0".to_string()]);

        // 内核不认识的节点名 -> 拦住（否则每个请求都会 502）
        let err = set_lane_node(&state, &LaneId::new("lane-auto"), "不存在的节点")
            .await
            .expect_err("未知节点必须被拒");
        assert!(err.message.contains("内核里没有节点"), "{}", err.message);

        // 直连线路没有可切换的组
        let err = set_lane_node(&state, &LaneId::new("lane-direct"), "节点0")
            .await
            .expect_err("直连没有组");
        assert!(err.message.contains("直连"), "{}", err.message);

        // 不存在的线路
        assert!(set_lane_node(&state, &LaneId::new("lane-nope"), "节点0").await.is_err());
        // 没装配内核 -> 明确报"内核没有装配"
        let bare = seeded("lane-pin-nokernel", "https://airport.example/sub").await;
        let err =
            set_lane_node(&bare, &LaneId::new("lane-auto"), "节点0").await.expect_err("没内核");
        assert_eq!(err.code, "TA-3001");
        assert!(err.message.contains("net.kernel.path"), "{}", err.message);
    }

    #[tokio::test]
    async fn lane_test_tests_group_members_and_reports_per_node_failures() {
        let state = seeded("lane-test", "https://airport.example/sub").await;
        install_kernel(&state, FakeKernel::new(KernelState::Ready));

        // 不传 node：测 lane 目标（LANE-lane-auto）的成员
        let out = test_lane(&state, &LaneId::new("lane-auto"), None).await.expect("测速");
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.iter().all(|r| r.ok));
        assert_eq!(out[0].latency_ms, Some(120));

        // 指定节点：只测一个
        let one = test_lane(&state, &LaneId::new("lane-auto"), Some("节点1".to_string()))
            .await
            .expect("测速");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].node, "节点1");

        // 内核不认的节点 -> 单个 ok=false，整条命令仍然成功（契约要的是逐节点结果）
        let bad = test_lane(&state, &LaneId::new("lane-auto"), Some("幽灵节点".to_string()))
            .await
            .expect("测速命令本身不该失败");
        assert_eq!(bad.len(), 1);
        assert!(!bad[0].ok);
        assert!(bad[0].latency_ms.is_none());

        assert!(test_lane(&state, &LaneId::new("lane-nope"), None).await.is_err());
    }

    #[tokio::test]
    async fn lane_assignments_resolve_the_five_level_chain() {
        let state = seeded("lane-assign", "https://airport.example/sub").await;
        let mut doc = (*state.doc()).clone();
        doc.providers[0].lane = Some(LaneId::new("lane-auto"));
        doc.channels.push(ChannelConfig::new("ch-b", "m-a", "p-a", "c-a", "upstream-b"));
        doc.channels.push(ChannelConfig::new("ch-c", "m-a", "p-a", "c-a", "upstream-c"));
        doc.channels[2].lane = Some(LaneId::new("lane-direct"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let rows = compute_lane_assignments(&state).await.expect("分配");
        assert_eq!(rows.len(), 3);
        let by = |id: &str| rows.iter().find(|r| r.channel_id.as_str() == id).cloned().expect("有");
        assert_eq!(by("ch-a").lane_id.as_str(), "lane-auto");
        assert_eq!(by("ch-a").source, LaneSource::Provider, "provider 级");
        assert_eq!(by("ch-b").source, LaneSource::Provider, "同样继承 provider.lane");
        assert_eq!(by("ch-c").source, LaneSource::Channel, "通道级最高");
        assert_eq!(by("ch-c").lane_id.as_str(), "lane-direct");
        assert_eq!(by("ch-a").model_id.as_str(), "m-a");

        // 五级都没有 -> 兜底直连（配置里有 direct lane 就用它）
        let mut doc = (*state.doc()).clone();
        doc.providers[0].lane = None;
        doc.net.default_lane = None;
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);
        let rows = compute_lane_assignments(&state).await.expect("分配");
        assert_eq!(rows[0].lane_id.as_str(), "lane-direct");
        assert_eq!(rows[0].source, LaneSource::Default);
    }

    #[tokio::test]
    async fn lane_reverse_lookup_lists_channels_and_models() {
        let state = seeded("lane-reverse", "https://airport.example/sub").await;
        let mut doc = (*state.doc()).clone();
        doc.channels[0].lane = Some(LaneId::new("lane-auto"));
        doc.channels.push(ChannelConfig::new("ch-b", "m-a", "p-a", "c-a", "upstream-b"));
        doc.channels[1].lane = Some(LaneId::new("lane-auto"));
        doc.models.push(Model::new("m-b"));
        doc.channels.push(ChannelConfig::new("ch-c", "m-b", "p-a", "c-a", "upstream-c"));
        doc.channels[2].lane = Some(LaneId::new("lane-auto"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let r = reverse_lookup_lane(&state, &LaneId::new("lane-auto")).await.expect("反查");
        assert_eq!(r.channel_ids.len(), 3);
        assert_eq!(r.channel_ids[0].as_str(), "ch-a");
        // 模型去重
        assert_eq!(
            r.model_ids.iter().map(|m| m.as_str().to_string()).collect::<Vec<_>>(),
            vec!["m-a", "m-b"]
        );

        assert!(reverse_lookup_lane(&state, &LaneId::new("lane-nope"))
            .await
            .unwrap()
            .channel_ids
            .is_empty());
    }

    #[tokio::test]
    async fn lane_pick_options_group_by_subscription_with_direct_first() {
        let airport = FakeAirport::start(vec![yaml_with(5, 2, false)]);
        let state = seeded("lane-pick", &airport.url("/sub")).await;
        refresh_subscriptions(&state, None, true).await.expect("刷新");

        let opts = compute_lane_pick_options(&state).await.expect("选项树");
        assert_eq!(opts.len(), 2, "{opts:?}");
        assert_eq!(opts[0].group, DIRECT_GROUP_LABEL, "直连永远排第一");
        assert_eq!(opts[0].options[0].lane_id.as_str(), "lane-direct");
        assert_eq!(opts[0].options[0].node_count, None, "直连没有节点数可说");

        assert_eq!(opts[1].group, "机场-sub-a", "其余按订阅名分组");
        let auto = &opts[1].options[0];
        assert_eq!(auto.lane_id.as_str(), "lane-auto");
        assert_eq!(auto.kind, LaneKind::SubscriptionAuto);
        assert_eq!(auto.node_count, Some(5), "节点数来自当前生效版本");
        assert_eq!(auto.latency_ms, None, "没有内核就没有延迟，不猜");
    }

    // ------------------------------------------------------------ kernel 命令

    #[tokio::test]
    async fn kernel_status_without_kernel_is_stopped_with_a_reason() {
        let state = fresh_state("k-status").await;
        let st = kernel_status_of(&state).await.expect("状态");
        assert_eq!(st.state, "stopped");
        assert!(
            st.last_error.as_deref().unwrap_or("").contains("net.kernel.path"),
            "要说清为什么没跑：{:?}",
            st.last_error
        );
        // 没装配内核时，控制类命令要明确报缺口，而不是假装成功
        let err = start_kernel(&state).await.expect_err("没内核不许假装启动");
        assert_eq!(err.code, "TA-3001");
        assert!(stop_kernel(&state).await.is_err());
        assert!(restart_kernel(&state).await.is_err());
        assert!(reload_kernel_config(&state).await.is_err());
        assert!(log_tail(&state, 100).await.is_err());
    }

    #[tokio::test]
    async fn kernel_start_stop_restart_and_reload_call_the_controller() {
        let state = seeded("k-lifecycle", "https://airport.example/sub").await;
        let fake = FakeKernel::new(KernelState::Stopped);
        install_kernel(&state, fake.clone());
        install_source(&state, "C:\\fake\\mihomo.exe");

        assert_eq!(start_kernel(&state).await.expect("启动").state, "ready");
        assert_eq!(stop_kernel(&state).await.expect("停止").state, "stopped");
        assert_eq!(restart_kernel(&state).await.expect("重启").state, "ready");

        let st = reload_kernel_config(&state).await.expect("reload");
        assert_eq!(st.state, "ready");
        assert_eq!(fake.applied(), 1, "reload 必须真的调 apply（不是我们自己拼一个状态）");
        assert_eq!(
            fake.calls(),
            vec![
                "start".to_string(),
                "stop".to_string(),
                "restart".to_string(),
                "apply".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn kernel_reload_config_refreshes_the_source_from_the_stored_config() {
        let state = seeded("k-reload-src", "https://airport.example/sub").await;
        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());
        install_source(&state, "C:\\fake\\mihomo.exe");

        // 模拟"别人改了配置但没碰内核配置源"（config_apply / config_rollback 就是这样）
        let mut doc = (*state.doc()).clone();
        doc.lanes.push(Lane::direct("lane-new", "后加的直连"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let cfg = generated_config(&state).await.expect("预览");
        assert!(cfg.contains("lane-new"), "预览必须来自真源：\n{cfg}");
        assert!(cfg.starts_with(ta_kernel::config_gen::HEADER), "{cfg}");

        reload_kernel_config(&state).await.expect("reload");
        // 配置源里现在应当有这条新 lane（否则下一次 reload 还是旧的）
        let rendered = state.kernel_source_slot().unwrap().render().expect("render");
        assert!(rendered.contains("lane-new"), "{rendered}");
    }

    #[tokio::test]
    async fn lane_upsert_syncs_the_kernel_source_and_reloads_a_ready_kernel() {
        let state = seeded("k-lane-sync", "https://airport.example/sub").await;
        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());
        install_source(&state, "C:\\fake\\mihomo.exe");

        upsert_lane(&state, Lane::direct("lane-extra", "新直连")).await.expect("加线路");
        // 内核在跑 -> 顺手 reload 一次（不是启动，是 apply）
        assert_eq!(fake.applied(), 1, "内核 Ready 时改动要立刻生效");
        // 配置源里也要有这条新 lane（否则下次 reload 又是旧的）
        let rendered = state.kernel_source_slot().unwrap().render().unwrap();
        assert!(rendered.contains("lane-extra"), "{rendered}");

        // 内核没跑 -> 不 apply（不自动拉起内核）
        let state2 = seeded("k-lane-noready", "https://airport.example/sub").await;
        let fake2 = FakeKernel::new(KernelState::Stopped);
        install_kernel(&state2, fake2.clone());
        install_source(&state2, "C:\\fake\\mihomo.exe");
        upsert_lane(&state2, Lane::direct("lane-extra", "新直连")).await.expect("加线路");
        assert_eq!(fake2.applied(), 0, "内核没跑就不该被顺手拉起");
    }

    #[tokio::test]
    async fn kernel_generated_config_works_without_an_assembled_kernel() {
        let state = seeded("k-gen-bare", "https://airport.example/sub").await;
        let cfg = generated_config(&state).await.expect("没装配内核也要能预览");
        assert!(cfg.contains("lane-auto"), "{cfg}");
        assert!(cfg.contains("proxy-providers"), "{cfg}");
        assert!(cfg.starts_with(ta_kernel::config_gen::HEADER), "{cfg}");
    }

    #[tokio::test]
    async fn kernel_log_tail_is_sanitized_before_it_reaches_the_ui() {
        let state = seeded("k-log", "https://airport.example/sub").await;
        install_kernel(&state, FakeKernel::new(KernelState::Ready));
        let tail = log_tail(&state, 50).await.expect("日志");
        assert!(
            !tail.contains("ta-allow-fake-secret-abc123"),
            "内核日志里的 token 也要抹掉：{tail}"
        );
        assert!(tail.contains("provider sub-a"), "其余内容要留着：{tail}");
    }

    #[tokio::test]
    async fn kernel_set_provider_refuses_empty_path() {
        let state = fresh_state("k-provider-empty").await;
        let err = set_kernel_provider(&state, KernelProvider::External, None)
            .await
            .expect_err("没有路径不能用");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("path"), "{}", err.message);
    }

    #[tokio::test]
    async fn kernel_set_provider_persists_stops_the_old_kernel_and_rebuilds() {
        let state = seeded("k-provider", "https://airport.example/sub").await;
        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());

        // 指向一个**必定不存在**的内核：命令会在"起回来"这一步如实报错 ——
        // 那正是我们想断言的行为（配置与装配都换了，只是内核没起来）。
        let bogus = std::env::temp_dir()
            .join(format!("ta-net-nokernel-{}", std::process::id()))
            .join("mihomo.exe");
        let bogus_str = bogus.to_string_lossy().to_string();
        let err = set_kernel_provider(&state, KernelProvider::External, Some(bogus_str.clone()))
            .await
            .expect_err("跑不起来的内核必须报错，不许假装成功");
        assert!(fake.calls().contains(&"stop".to_string()), "旧内核要先停：{:?}", fake.calls());
        assert!(
            err.message.contains("内核") || err.message.contains("拉起"),
            "错误要说清是内核起不来：{}",
            err.message
        );

        // 配置真的落库了
        let doc = state.config.load().await.expect("load");
        assert_eq!(doc.net.kernel.provider, KernelProvider::External);
        assert_eq!(doc.net.kernel.path.as_deref(), Some(bogus_str.as_str()));
        // 控制器也真的换成了新装配的（而不是只有配置变了）
        let sup = state.kernel_supervisor().expect("新 supervisor");
        assert_eq!(sup.config().program.to_string_lossy(), bogus_str);

        // 内核没在跑时：只换装配、不试图拉起 -> 成功且状态是 stopped
        install_kernel(&state, FakeKernel::new(KernelState::Stopped));
        let st = set_kernel_provider(&state, KernelProvider::Bundled, Some(bogus_str.clone()))
            .await
            .expect("没在跑就不用起");
        assert_eq!(st.state, "stopped");
        assert_eq!(
            state.config.load().await.expect("load").net.kernel.provider,
            KernelProvider::Bundled,
            "provider 要按入参持久化"
        );
    }

    #[tokio::test]
    async fn kernel_probe_path_reports_missing_and_unrunnable_paths() {
        let state = fresh_state("k-probe-none").await;
        let p = probe_kernel_path(&state).await.expect("探测");
        assert!(!p.found);
        assert_eq!(p.path, None);

        // 配了一个不存在的路径 -> found=false，但要把"你配的是哪条"带回去
        let state = fresh_state("k-probe-missing").await;
        let mut doc = ConfigDoc::default();
        doc.net.kernel.path = Some("C:\\nope\\mihomo-not-here".to_string());
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);
        let p = probe_kernel_path(&state).await.expect("探测");
        assert!(!p.found);
        assert_eq!(p.path.as_deref(), Some("C:\\nope\\mihomo-not-here"));

        // 文件在、但跑不起来 -> found=true 且 version=None（"在不在"和"能不能用"分开表达）
        let dir = std::env::temp_dir().join(format!("ta-net-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake_exe = dir.join("mihomo.exe");
        std::fs::write(&fake_exe, b"not a real executable").unwrap();
        let state = fresh_state("k-probe-bogus").await;
        let mut doc = ConfigDoc::default();
        doc.net.kernel.path = Some(fake_exe.to_string_lossy().to_string());
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);
        let p = probe_kernel_path(&state).await.expect("探测");
        assert!(p.found, "文件在就该 found=true");
        assert_eq!(p.version, None, "跑不起来就没有版本，不许编一个");
    }
}

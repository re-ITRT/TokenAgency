//! `subscription_*` / `lane_*` / `kernel_*`：网络面（订阅 / 线路 / 内核）的读写命令。
//!
//! 契约： 2.2 的 `subscription / lane / kernel` 三张表。
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
// `RoutingIndex` / `ChannelId` / `ModelId` 三个导入随
// `lane_assignments` / `lane_reverse_lookup` / `lane_pick_options` 一起删除 ——
// 它们只被那三条命令的实现用到。
use ta_core::types::{LaneId, ProviderId, SubscriptionId};
use ta_kernel::config_gen::{lane_target, provider_key, MhConfigGen};
use ta_kernel::ports::{FilePortStore, PortAllocator, RangePortAllocator};
use ta_kernel::{KernelConfigGen, KernelController, KernelState, PortMap, ProvidersSnapshot};
use ta_store::{
    ConfigStore, SqliteAutoChoiceStore, SqliteSubscriptionTrafficStore, SubscriptionTraffic,
    SubscriptionTrafficRow,
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

/// 下拉里"直连"那一组的标题（8.3：每个线路下拉的第一项就是直连）。
pub const DIRECT_GROUP_LABEL: &str = "直连（本机网络）";

/// 其它线路（没有订阅、也不是直连）的分组标题。
const OTHER_GROUP_LABEL: &str = "其它线路";

/// 五级继承链的兜底（8.1 的 `... ?? direct`）在**配置里没有 direct lane** 时
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
    /// 本次刷新拿到的流量与更新时间。**没给头就是 `None`**。
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
    /// **这条节点用户有没有关掉**（默认 `true` = 启用，用户第 202 轮）。
    ///
    /// 不用 `Option<bool>`：语义就是二值，"没配过"= 启用，界面上一眼就该知道勾没勾。
    #[serde(default = "default_node_enabled")]
    pub enabled: bool,
}

fn default_node_enabled() -> bool {
    true
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

//（第 322 轮）这里曾有 `LaneSource` / `LaneAssignment` / `LaneReverseLookup` /
// `LanePickOption(Item)` 五个类型，以及 `lane_assignments` / `lane_reverse_lookup` /
// `lane_pick_options` 三条命令。
//
// 它们全部服务于"**线路级别的分配**"：把 `policy.use_lane → channel.lane → credential.lane
// → provider.lane → net.default_lane` 那条五级链算出来给界面看。
// 用户裁决"线路级别的分配逻辑我们已经放弃了，取而代之的，**每个服务商统一一个节点**"之后：
//
//   * 生效线路只剩 `provider.lane ?? net.default_lane ?? direct`，没有"来自哪一级"可报；
//   * 展示它们的页面（`/network/assignments`）整页删除，三条命令**没有任何调用方**。
//
// 留着它们只会返回一条恒为 `provider`/`default` 的"来源"—— 那是个**过期的说法**，
// 比删掉更容易误导。所以类型、命令、实现一并删除。

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
pub(crate) async fn commit_config(state: &AppState, doc: ConfigDoc, label: &str) -> IpcResult<()> {
    let source = state.kernel_source_slot();
    let ports = match &source {
        Some(_) => Some(allocate_ports(state, &doc)?),
        None => None,
    };
    save_validated(state, doc.clone(), label).await?;
    if let (Some(source), Some(ports)) = (source, ports) {
        source.set_doc(doc);
        // 同一份端口表也要推给**网关拨号器**（它按 lane 查端口）。
        source.set_ports(ports.clone());
        if let Some(hot) = state.lane_ports() {
            hot.replace(ports);
        }
    }
    Ok(())
}

/// 内核正在跑（`Ready`）就顺手 `apply()` 一次，让改动立刻生效。
///
/// 内核没跑 -> 什么都不做（改动会在下一次启动 / `kernel_reload_config` 时生效）。
/// `apply()` 失败 -> 如实报错：**配置已经存进去了**，只是内核没跟上，别让用户以为没保存。
pub(crate) async fn maybe_apply_kernel(state: &AppState, what: &str) -> IpcResult<()> {
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
pub(crate) fn allocate_ports(state: &AppState, doc: &ConfigDoc) -> IpcResult<PortMap> {
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

// ================================================================ 订阅流量

/// 流量快照的读写口（`subscription` 表的 5 个可空列）。
///
/// 每次从 `AppState::db` 现构造：`SqlitePool` 的 clone 是 Arc 级的，代价可忽略；
/// 也因此不必往 `AppState` 里加字段（那个文件不在本次改动面上）。
/// 找出**用着「自动」出口、且挂在给定订阅上**的服务商。
///
/// 判据两条，缺一不可：
/// * 服务商自己的 `lane` 指向的那条线路是 `subscription_auto`（界面上叫「自动」）；
/// * 那条线路的 `subscription` 就是刚更新的这份 —— 别的订阅更新了不该动它。
///
/// 返回 `(服务商 id, 线路 id)`：调用方拿去跑 [`apply_auto_export`]。
pub fn auto_providers_on(
    doc: &ConfigDoc,
    subscription_id: &SubscriptionId,
) -> Vec<(ProviderId, LaneId)> {
    doc.providers
        .iter()
        .filter_map(|provider| {
            let lane_id = provider.lane.as_ref()?;
            let lane = doc.lanes.iter().find(|l| &l.id == lane_id)?;
            if lane.kind != LaneKind::SubscriptionAuto {
                return None;
            }
            if lane.subscription.as_ref() != Some(subscription_id) {
                return None;
            }
            Some((provider.id.clone(), lane_id.clone()))
        })
        .collect()
}
/// **订阅刷新成功后**重算所有「自动」出口（C 期触发点①）。
///
/// `manual = false`：**粘性优先** —— 上次选的那条地址还在就沿用（订阅换了节点也一样，
/// 只要地址没变）；只有"上次那条不见了/测不通"才会重挑。返回值 = 真的重算了几家。
///
/// 这里刻意**吞掉单家的错误**（只记日志、继续下一家）：一家服务商的自动出口算不出来，
/// 不该让别的服务商跟着不更新。返回的是成功家数，失败家数由日志体现。
pub async fn reapply_auto_after_refresh(state: &AppState, subscription_id: &str) -> usize {
    // 后台自动刷新 = 触发点①（**粘性优先**）：上次那条地址还在就沿用。
    // 与用户手动刷新（触发点②，`manual = true`）共用同一个实现 —— 差别只有一个布尔值，
    // 所以**不许**写成两份循环（那样两边的行为迟早会分叉）。
    let sid = SubscriptionId::new(subscription_id);
    reapply_auto_export(state, &sid, false).await
}
/// 触发整份订阅体检之后，等内核那张延迟表"稳定"的最长时间（毫秒）。
///
/// 不是"睡够这么久"：`provider_delays` 在**表完整**（每个节点都有结论）或**连续 1 秒没有新进展**
/// 时就提前返回，这里只是"再慢也得给我个结论"的上限（到点仍测不到 = 如实 `all_unreachable`）。
///
/// 15 秒的来由：真机实测 34 条境外真实节点一轮完整体检约 6–10 秒，偶尔会更慢；
/// 等太短会在半路上裁决 —— 那会把"还没量到"误判成"这条不通"，而**粘性**恰恰依赖
/// "上次那条到底量到了没有"（误判一次就换一次出口 IP，正是用户要避免的）。
const AUTO_PROBE_WAIT_MS: u64 = 15_000;

/// 「自动」出口探测的**两个坐标**。
///
/// * `provider_key`：去哪家 provider 取延迟表 —— **必须是内核 `proxy-providers` 上的 key**
///   （`config_gen::provider_key`，不是订阅 id）；
/// * `probe_url`：按哪条上游端点测延迟 —— 用**这家服务商的 `base_url`**，
///   因为这里的语义是"这条节点到该服务商有多快"；服务商没配 `base_url` 时按
///   订阅自带 `health_check.url` → 内核默认探测 URL 兜底，绝不留空（空 URL 内核会拒绝）。
pub fn auto_probe_target(
    doc: &ConfigDoc,
    provider_id: &str,
    subscription_id: &SubscriptionId,
) -> IpcResult<(String, String)> {
    let provider = doc
        .providers
        .iter()
        .find(|p| p.id.as_str() == provider_id)
        .ok_or_else(|| not_found("服务商", provider_id))?;
    let sub = find_subscription(doc, subscription_id)?;
    let key = ta_kernel::config_gen::provider_key(&sub);
    let probe = match provider.base_url.trim() {
        "" => sub
            .health_check
            .url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| ta_kernel::config_gen::DEFAULT_HEALTH_URL.to_string()),
        url => url.to_string(),
    };
    Ok((key, probe))
}

/// 「自动」出口这一轮的结果。**不是 DTO**：先只在内部用，
/// 等 D 期把它接到界面时再决定要不要给前端看（那时再加 specta 与 bindings）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoApplyOutcome {
    /// 最终写进内核的那条**节点名**（`None` = 一条可用的都没有，什么都没写）。
    pub node: Option<String>,
    /// 该节点对应的**地址**（粘性按它比）。
    pub address: Option<String>,
    /// 为什么是这个结论（[`ta_subscription::AutoDecision`] 的理由枚举名）。
    pub why: &'static str,
    /// 这一轮**参与裁决**的节点数（= 订阅里解析出来的节点数）。
    pub probed: usize,
    /// 内核**真的量出延迟**的节点数（`probed` 里测得到的那些）。
    ///
    /// 与 `probed` 分开是为了让"一个都没量到"和"量到了但都不通"能分辨 —— 前者说明探测本身
    /// 没跑起来（provider 没加载 / 探测目标不可达），后者才轮到裁决说话。
    pub measured: usize,
    /// 这一轮用的**探测目标**（服务商 `base_url`；服务商没配时是兜底那条）。
    ///
    /// 它是"延迟到底按谁测的"的**唯一书面证据**—— 真机验证就认它。
    pub probe: Option<String>,
    /// **被选中那条在它自己那张表里的延迟**（毫秒；`None` = 没选中任何一条）。
    pub picked_ms: Option<u32>,
    /// **它自己那张表里最低的延迟**（毫秒；`None` = 一条都没量到）。
    ///
    /// ★ `picked_ms == best_ms` 就是「选了最低延迟」这句话的**自证**：不必依赖"谁在什么时刻
    /// 另测了一遍"（真机实测：同一批境外节点在几十秒内抖动极大，拿两次独立的测量互相对账
    /// 会把产品判成错）。裁决规则本身是纯函数，已经被单测穷举钉住；这两个字段是把它**写进日志**。
    pub best_ms: Option<u32>,
}

/// **「自动」出口的编排**：探测 → 裁决 → 写内核 → 记住。
///
/// 参数是**已经解析好的**三件事（服务商 id / 自动线路 id / 它挂在哪个订阅）——
/// 配置查找留给调用方，这样这个函数只做"出口决策"这一件事，也好测、好读。
///
/// 顺序与纪律：
///
/// 1. 节点只来自**已采纳的订阅版本**（`latest_outcome`，坏更新会被拦下并保留上一版）；
/// 2. 延迟**只走 provider 命名空间**（实测）：先
///    `GET /providers/proxies/{key}/healthcheck?url=<这家服务商的 base_url>` 触发整份订阅体检，
///    再读 `/providers/proxies/{key}` 里每个节点的 `history[].delay`。
///    **不能**用 `/proxies/{node}/delay` —— provider 的节点不在 `/proxies` 顶层，
///    那条路由对它们**全 404**（这正是「自动」出口一直 `all_unreachable` 的原因）；
///    读不到的节点一律 `None`，**绝不用 0 冒充** —— 0 会被当成"最快"，选出一条其实不通的出口；
/// 3. 读**上次选的地址**，交给 [`ta_subscription::choose_auto_export`] 裁决
///    （粘性 / 手动重挑 / 上次那条不通或不见了，四种理由都由它说清）；
/// 4. 结论是"沿用"或"重挑"才写内核 `set_select` 并记住地址；结论是 `None` 时
///    **一个字节都不写**（不编一个能用的出口），并把理由如实返回。
pub async fn apply_auto_export(
    state: &AppState,
    provider_id: &str,
    lane_id: &LaneId,
    subscription_id: &SubscriptionId,
    manual: bool,
) -> IpcResult<AutoApplyOutcome> {
    let outcome = latest_outcome(state, subscription_id)?;
    let kernel = state.kernel_slot().ok_or_else(|| {
        IpcError::from(TaError::MalformedRequest(
            "内核没在跑：测不了「自动」出口的延迟（不猜一个出来）".to_string(),
        ))
    })?;
    let (provider_key, probe_url) = auto_probe_target(&state.doc(), provider_id, subscription_id)?;
    // ★ 用户关掉的节点**不进候选**（用户第 202 轮：订阅里的节点像服务商的「模型」一样可以单独
    // 关掉，默认全开）。这一条同时解决了"订阅里那几条展示用的信息节点（`剩余流量：…` /
    // `套餐到期…` / `过滤掉 N 条线路`）会不会被选成出口"——由用户自己勾，而不是我们在代码里
    // 猜哪个名字是"真节点"。
    let disabled = find_subscription(&state.doc(), subscription_id)?.disabled_nodes;
    let nodes: Vec<ta_subscription::NodeMeta> =
        outcome.nodes.iter().filter(|n| !disabled.iter().any(|d| d == &n.name)).cloned().collect();
    let latency = kernel.provider_delays(&provider_key, &probe_url, AUTO_PROBE_WAIT_MS).await?;
    let measured = latency.values().filter(|v| v.is_some()).count();
    // `candidates_from_nodes` 收 `HashMap`（只有查表作用，顺序由 `outcome.nodes` 决定 —— 同一份
    // 订阅每次得到同一个结论）；内核那张表是 BTreeMap，这里转一下，别为了省一次转换去动纯函数签名。
    let latency: std::collections::HashMap<String, Option<u32>> = latency.into_iter().collect();
    let candidates = ta_subscription::auto::candidates_from_nodes(&nodes, &latency);
    let best_ms = candidates.iter().filter_map(|c| c.latency_ms).min();
    let mut out =
        apply_auto_export_choice(state, provider_id, lane_id, manual, &candidates).await?;
    out.measured = measured;
    out.probe = Some(probe_url);
    out.best_ms = best_ms;
    // 被选中那条在**这一轮那张表**里的延迟（"选了最低延迟"的自证：`picked_ms == best_ms`）。
    // 按**地址**取（同一地址多条候选时取已测得里最小的那条，与 `node_name_for_address` 配对）。
    out.picked_ms = out
        .address
        .as_deref()
        .and_then(|a| ta_subscription::auto::latency_for_address(&candidates, a));
    Ok(out)
}

/// **探测之后**的那一半：裁决 → 写内核 → 记住地址。
///
/// 入参直接收**候选**（`AutoCandidate` 自带 `latency`）：探测结果就是候选本身。
/// 于是"判断 + 两个副作用"能**脱离内核被完整回放**（B 方案）——
/// 这是 E 期在本环境唯一能拿到的编排证据（真机端到端见 的人工清单）。
///
/// 与 [`apply_auto_export`] 的一处**有意差异**：这里不预先要求内核在场 ——
/// 因为"什么都不写"的结论（`all_unreachable` / `no_candidates`）本来就不需要内核。
/// 生产路径上 [`apply_auto_export`] 已经先查过内核（探测需要它），所以行为不变。
pub async fn apply_auto_export_choice(
    state: &AppState,
    provider_id: &str,
    lane_id: &LaneId,
    manual: bool,
    candidates: &[ta_subscription::AutoCandidate<'_>],
) -> IpcResult<AutoApplyOutcome> {
    let probed = candidates.len();
    // 这里收的就是**已知的**候选（延迟已经附在上面），所以"量到几条"就是"有几个 Some"。
    let measured = candidates.iter().filter(|c| c.latency_ms.is_some()).count();
    let best_ms = candidates.iter().filter_map(|c| c.latency_ms).min();
    let store = SqliteAutoChoiceStore::new(state.db.clone());
    let previous = store.get(provider_id).await.map_err(IpcError::from)?;
    let choice = ta_subscription::auto::decide_export(candidates, previous.as_deref(), manual);
    let (Some(node), Some(address)) = (choice.node, choice.address) else {
        return Ok(AutoApplyOutcome {
            node: None,
            address: None,
            why: choice.why,
            probed,
            measured,
            probe: None,
            picked_ms: None,
            best_ms,
        });
    };
    let node = node.to_string();
    let address = address.to_string();
    let picked_ms = ta_subscription::auto::latency_for_address(candidates, &address);
    let kernel = state.kernel_slot().ok_or_else(|| {
        IpcError::from(TaError::MalformedRequest(
            "内核没在跑：写不了「自动」出口的选择（不假装写过了）".to_string(),
        ))
    })?;
    kernel.set_select(lane_id, &node).await?;
    store.set(provider_id, &address, state.clock.now_ms().max(0)).await.map_err(IpcError::from)?;
    Ok(AutoApplyOutcome {
        node: Some(node),
        address: Some(address),
        why: choice.why,
        probed,
        measured,
        probe: None,
        picked_ms,
        best_ms,
    })
}

/// **自动刷新一条到期订阅**：走**和手动刷新完全同一条**链路
/// （`refresh_subscriptions` → 下载 → 校验 → 采纳 → 版本 → 流量），
/// 唯一的区别是 `force = false`：
///
/// * 手动点「刷新」= `force = true`（强制重下，否则按钮看起来是坏的）；
/// * 自动到期 = `force = false`（尊重 `etag` / `last-modified`，机场没变就别浪费流量）。
///
/// **"到期"不等于"刷新成功"**：坏更新会被 `DefaultSubscriptionValidator` 拦下并保留上一版，
/// 这条既有语义在新链路上一个字都不用改 —— 所以我们只是"调用它"，不重写它。
pub async fn auto_refresh_subscription(state: &AppState, id: &str) -> IpcResult<RefreshResult> {
    refresh_subscriptions(state, Some(SubscriptionId::new(id)), false).await
}
/// **自动刷新：算出此刻该刷新哪些订阅**（只算，不动网络）。
///
/// 这是「订阅自动更新时间」落地链路的第一步（第二步才是真的去刷）。分开做是因为
/// "算得对不对"与"刷得对不对"是两件事，混在一起出错时说不清是哪一头。
///
/// 时间证据用 `subscription_traffic` 的 `traffic.fetched_at_ms`（每次**成功**刷新都会写）：
/// * `traffic == None` = 这份订阅**从未成功刷新过** → 到期（否则新订阅永远不会自己动）；
/// * 负数（理论上不该有）夹到 0，交给我们那套"时钟异常按到期处理"的规则。
pub async fn due_subscription_ids(state: &AppState) -> IpcResult<Vec<String>> {
    let doc = state.doc();
    let schedules: Vec<ta_subscription::SubscriptionSchedule<'_>> = doc
        .proxy_subscriptions
        .iter()
        .map(|s| ta_subscription::SubscriptionSchedule {
            id: s.id.as_str(),
            enabled: s.enabled,
            interval_s: s.interval_s,
        })
        .collect();
    let rows = traffic_store(state).list().await.map_err(IpcError::from)?;
    let last: std::collections::HashMap<String, u64> = rows
        .iter()
        .filter_map(|row| {
            row.traffic.as_ref().map(|t| (row.id.clone(), t.fetched_at_ms.max(0) as u64))
        })
        .collect();
    Ok(ta_subscription::due_subscriptions(
        &schedules,
        &last,
        // `now_ms()` 是 i64（时钟可能被回拨到 1970 之前）；负数夹到 0 —— 那一侧
        // `due_subscriptions` 本来就按"时钟异常 → 到期"处理，不会因此永远不刷新。
        state.clock.now_ms().max(0) as u64,
    ))
}
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
///
/// **覆盖已存在的条目时，`url` 留空 = 不改地址**。
///
/// 这不是省事，是**防静默写坏**：订阅地址里带订阅 token，界面**从不回显原文**
///（`maskSubscriptionUrl` 是唯一出口，见 `src/pages/network/model.ts`），
/// 所以编辑表单里的地址框**只能是空的**。如果这里的语义是"按原样覆盖"，
/// 用户改一次名字就会把 `...?token=***` 写进配置 —— 下次刷新必然失败，
/// 而保存那一刻不但不报错，还会显示"已保存"。
///
/// 与 `credential_set_secret` 的「留空 = 不改密钥」是同一条口径
///（`providers.edit.keyHint`）。**新建时留空仍然照常报错**（校验器要求 `url` 非空）。
pub async fn upsert_subscription(
    state: &AppState,
    mut subscription: Subscription,
) -> IpcResult<Subscription> {
    let mut doc = (*state.doc()).clone();
    match doc.proxy_subscriptions.iter_mut().find(|x| x.id == subscription.id) {
        Some(slot) => {
            if subscription.url.trim().is_empty() {
                subscription.url = slot.url.clone();
            }
            *slot = subscription.clone();
        }
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

/// `subscription_delete`：删订阅。
///
/// **先查引用，再动手**：被线路引用时直接回 `TA-5002` + "引用它的地方 + 先解除"，
/// 与 `delete_provider` / `delete_channel` 同一条口径（`entity::referenced`）。
/// `subscription_delete` **没有** `force` 参数，所以消息里只报"先解除"，绝不承诺级联。
///
/// 为什么不靠校验层兜底（第 320 轮用户真机报上来的）：靠兜底的话，用户拿到的是一句
/// `[TA-5001] 配置校验失败：1 处问题` —— 它把一个**"这次删除会造成悬空引用"**说成了
/// **"配置本来就有一处错"**，既不指出是哪条线路，也不说该怎么办。
pub async fn delete_subscription(state: &AppState, id: &SubscriptionId) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let before = doc.proxy_subscriptions.len();

    // 引用清单：订阅只被 `lanes[].subscription` 引用（见 validator 的"线路 → 订阅"一节）。
    // 文案里用线路的**名字**（人话），名字为空才退回 id。
    let blockers: Vec<String> = doc
        .lanes
        .iter()
        .filter(|l| l.subscription.as_ref() == Some(id))
        .map(|l| if l.name.trim().is_empty() { l.id.as_str().to_string() } else { l.name.clone() })
        .collect();
    if !blockers.is_empty() {
        return Err(crate::commands::entity::referenced(
            &format!("订阅 {}", id.as_str()),
            &blockers,
            false,
        ));
    }

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

    // ★ 每次**成功刷新**写"上次已知"。
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
    let force = force.unwrap_or(false);
    let result = refresh_subscriptions(&state, id.clone(), force).await?;

    // **C 期触发点②：用户手动刷新 = 重挑**（`manual = true`）。
    //
    // 为什么挂在命令上而不是共用链路里：后台自动刷新那条路（`auto_refresh_subscription`）
    // 自己在 tick 里以 `manual = false` 调用重算（粘性优先）；如果这里也塞进共用函数，
    // 自动那条就会变成"手动"语义 —— 出口 IP 会被后台悄悄换掉，正是要避免的。
    //
    // 用户点了「刷新」= 他明确要求重来一次 → 无视粘性、按当前延迟重挑。
    // 刷新失败（`ok = false`）时不重算：节点没变，重挑也只是拿旧数据赌一把。
    if result.ok {
        match id {
            Some(sub) => {
                reapply_auto_export(&state, &sub, true).await;
            }
            None => {
                // 刷新全部：对每一份订阅各重算一次（各自的手动语义）
                let subs: Vec<SubscriptionId> = state
                    .doc()
                    .proxy_subscriptions
                    .iter()
                    .filter(|s| s.enabled)
                    .map(|s| s.id.clone())
                    .collect();
                for sub in subs {
                    reapply_auto_export(&state, &sub, true).await;
                }
            }
        }
    }

    Ok(result)
}

/// 重算某份订阅上的所有「自动」出口。
///
/// `manual = true` 来自**用户手动刷新**（C 期触发点②：重挑最低延迟，无视粘性）；
/// `false` 来自后台自动刷新（触发点①：粘性优先）。逐家吞错、逐家记日志 ——
/// 一家算不出来不该让别家跟着不更新。
async fn reapply_auto_export(
    // 订阅刚刷新成功 → **数据出现了，内核才有意义**。启动时没有任何 provider，
    state: &AppState,
    subscription_id: &SubscriptionId,
    manual: bool,
) -> usize {
    // 订阅刚刷新成功 → **数据出现了，内核才有意义**。启动那一刻还没有 provider，
    // 生成配置的预检会如实拒绝（TA-5001）；这里补一次"再拉"。失败只记 WARN。
    kick_kernel(state).await;
    let doc = state.doc();
    let targets = auto_providers_on(&doc, subscription_id);
    drop(doc);
    let mut done = 0usize;
    for (provider_id, lane_id) in targets {
        match apply_auto_export(state, provider_id.as_str(), &lane_id, subscription_id, manual)
            .await
        {
            Ok(outcome) => {
                done += 1;
                tracing::info!(
                    provider_id = provider_id.as_str(),
                    status = outcome.why,
                    node = outcome.node.as_deref().unwrap_or("(未选)"),
                    probed = outcome.probed,
                    measured = outcome.measured,
                    picked_ms = outcome.picked_ms.map_or(-1, |v| v as i64),
                    best_ms = outcome.best_ms.map_or(-1, |v| v as i64),
                    probe = outcome.probe.as_deref().unwrap_or("(未探测)"),
                    "「自动」出口已重算"
                );
            }
            Err(e) => {
                tracing::warn!(
                    provider_id = provider_id.as_str(),
                    code = %e.code,
                error = %e.message,
                    "「自动」出口重算失败（这一家跳过）"
                );
            }
        }
    }
    done
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
        disabled_nodes: Vec::new(),
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
    // 节点清单要带"用户关没关"（用户第 202 轮）——判据只有一处：订阅的 `disabled_nodes`。
    let disabled = find_subscription(&state.doc(), id)?.disabled_nodes;
    Ok(outcome.nodes.iter().map(|n| node_view_of_with(n, &disabled)).collect())
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

/// `subscription_nodes_enabled`：**批量**设置这份订阅里若干节点的启用状态（用户第 202 轮：
/// 「订阅里面的节点，也将服务商的『模型』一样设置一个启用或关闭的功能」+「有全选功能，默认全部选择」）。
///
/// 为什么是"一批名字 + 一个布尔值"而不是"一次一条"：界面上的**全选**要一次改几十条，
/// 一条一次 IPC 既慢又会出现"改了前半、还没改后半"的中间态。
/// `nodes` 为空 → **明确报错**（不把"空清单"当成"全部"，那会静默改掉整份订阅）。
///
/// 返回**改完之后**的节点清单：界面直接拿它渲染，不用再发一次 `subscription_nodes`。
pub async fn set_nodes_enabled(
    state: &AppState,
    id: &SubscriptionId,
    nodes: &[String],
    enabled: bool,
) -> IpcResult<Vec<NodeView>> {
    if nodes.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest(
            "要设哪些节点？`nodes` 不能为空清单（空清单不是「全部」，那会静默改掉整份订阅）"
                .to_string(),
        )));
    }
    let mut doc = (*state.doc()).clone();
    let sub = doc
        .proxy_subscriptions
        .iter_mut()
        .find(|s| &s.id == id)
        .ok_or_else(|| not_found("订阅", id.as_str()))?;
    if enabled {
        sub.disabled_nodes.retain(|n| !nodes.iter().any(|x| x == n));
    } else {
        for n in nodes {
            if !sub.disabled_nodes.iter().any(|x| x == n) {
                sub.disabled_nodes.push(n.clone());
            }
        }
    }
    // ★ 关掉的节点必须**真的**从内核那侧的择优里消失。
    //
    // 这里原来是裸 `save_validated`（只写库 + `set_doc`），**既没更新内核配置源、也没 apply**：
    // 于是界面上的开关当场变了，内核却还在拿旧配置按老节点清单择优 —— 一个**假开关**。
    // 用户第 321 轮要的正是"每个节点可以打开或者关闭"，所以必须走 `commit_config`
    // （它自己会校验 + 写库 + 快照 + 分配端口 + 同步 `kernel_source` 的 doc/ports）
    // 再 `maybe_apply_kernel`。
    //
    // 与 `subscription_upsert` / `lane_upsert` 同一条路径：配置已经存好了，内核没跟上就如实报错
    // （`maybe_apply_kernel` 的文案会说"已保存，但内核没有加载新配置"），**不假装成功**。
    commit_config(state, doc, "subscription_nodes_enabled").await?;
    maybe_apply_kernel(state, "节点启用状态").await?;
    nodes_of_subscription(state, id, None).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn subscription_nodes_enabled(
    state: State<'_, AppState>,
    id: SubscriptionId,
    nodes: Vec<String>,
    enabled: bool,
) -> IpcResult<Vec<NodeView>> {
    set_nodes_enabled(&state, &id, &nodes, enabled).await
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
/// 三态口径与 一致：**没有已采纳的版本 = 字段缺失，不用 `0` / 空串冒充**。
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
    /// 生效版本的抓取时刻（Unix 毫秒）。节点页的硬前置就判它。
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

/// `subscription_traffic`：每份订阅的"上次已知流量与更新时间"。
///
/// 返回 `{ id, traffic }[]`，**以配置里的订阅清单为准**（不是表里的行 —— 表里可能有
/// 已删订阅的残留行）；`traffic: null` = 这份订阅从未成功刷新过（界面上是"尚未获取"）。
///
/// 为什么另开一条只读命令，而不是把 `traffic` 塞进 `SubscriptionView`（给了
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

/// `lane_delete`：删线路。
///
/// **先查引用，再动手**（与 `delete_subscription` 同一个理由）。
/// `lane_delete` **没有** `force` 参数 → 只报"先解除"。
///
///（第 322 轮）引用类别从**五类收敛到两类** —— 线路分配只剩服务商级一处：
/// `net.default_lane` 与 `providers[].lane`。
/// `credentials[].lane` / `channels[].lane` / `policies[].action.use_lane` 三个字段已删除，
/// 所以它们不再可能挡住删除（老配置里的残留值落进 `extra`，既不解析也不引用）。
pub async fn delete_lane(state: &AppState, id: &LaneId) -> IpcResult<()> {
    let mut doc = (*state.doc()).clone();
    let before = doc.lanes.len();

    let label = doc.lanes.iter().find(|l| &l.id == id).map(|l| {
        if l.name.trim().is_empty() {
            id.as_str().to_string()
        } else {
            l.name.clone()
        }
    });
    let mut blockers: Vec<String> = Vec::new();
    if doc.net.default_lane.as_ref() == Some(id) {
        blockers.push("默认线路（net.default_lane）".to_string());
    }
    for p in doc.providers.iter().filter(|p| p.lane.as_ref() == Some(id)) {
        blockers.push(format!("服务商 {}", p.id.as_str()));
    }

    if !blockers.is_empty() {
        return Err(crate::commands::entity::referenced(
            &format!("线路 {}", label.unwrap_or_else(|| id.as_str().to_string())),
            &blockers,
            false,
        ));
    }

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

/// `lane_set_node`：手动钉住 `select` 组里的节点（8.2 的线路下拉）。
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

// ================================================================ 网络代理可选清单

/// 线路下拉里一项的网络来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum LaneChoiceKind {
    /// 配置里**已经有**的线路（`doc.lanes`）
    Lane,
    /// **直连（本机网络）**：配置里**还没有** direct 线路时也用这个标记把它列出来，
    /// 选中它由 [`lane_ensure_direct`] 把那条线路建出来（幂等）。
    ///
    /// 为什么"直连"必须单独列（用户第 81 轮的追问：「网络代理选项中最重要的是直连，
    /// 这个你都没单独列出来？」）：它是**最常用的选择**，而且与「继承」是**两件事** ——
    /// 继承会跟着凭据 / 服务商 / 默认线路走（将来那条默认线路被改成代理，这条线路会跟着变），
    /// 而"直连"是**把它钉在本机网络**上，不会被别的设置带走。
    Direct,
    /// 订阅里解析出来的**节点**（还没有对应线路时）
    Node,
}

/// 线路（网络代理）下拉的一项。
///
/// 为什么需要这个命令（用户的现场）：他在网络页加好了订阅、也看到了节点，但服务商详情页那条
/// 线路的"网络代理"下拉里**只有「不指定（继承）」** —— 因为下拉过去只列 `doc.lanes`，
/// 而他的配置里一条线路都没有（`doc.lanes` 为空）。于是"切网络"这件事在界面上根本做不到。
///
/// 这个清单给的是**用户真正拥有的网络**：
/// * `kind: "lane"` —— 已配置的线路，`value` 就是 lane id，选中直接写；
/// * `kind: "node"` —— 订阅节点，`value` 形如 `node:<订阅id>:<节点名>`；选中它要先用
///   [`lane_ensure_for_node`] 得到线路 id（已有等价线路就复用，没有才新建）。
///
/// **不编数据**：节点只来自**已采纳的订阅版本**（`latest_outcome`）；没有版本的订阅在这里
/// 一个节点都不出现（那不是"没有节点"，是"还没解析" —— 页面用空态文案区分这两件事）。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneChoice {
    /// 直接可用的值：`lane` 类 = lane id；`node` 类 = `node:<订阅id>:<节点名>`
    pub value: String,
    /// 人能读的名字（节点名**原样**，可能含 emoji；不改写）
    pub label: String,
    pub kind: LaneChoiceKind,
    /// `lane` 类：这条线路的 id；`node` 类且**已有**等价线路时也给出（选中 = 复用，不新建）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<LaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<SubscriptionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// 一个分组（直连 / 某个订阅 / 其它线路）下的选项。分组标题与 [`LanePickOption`] 同一套词。
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct LaneChoiceGroup {
    pub group: String,
    pub items: Vec<LaneChoice>,
}

/// `node:<订阅id>:<节点名>` 的编码/解码。
///
/// 用前缀而不是裸节点名：节点名与 lane id 可能**同名**（用户完全可以把线路 id 取成节点名），
/// 那样一个裸字符串就有两种解释，写出去的东西就不是确定的。
fn node_choice_value(subscription: &SubscriptionId, node: &str) -> String {
    format!("node:{}:{}", subscription, node)
}

/// 「直连」在**配置里还没有 direct 线路**时的下拉值（选中 → `lane_ensure_direct`）。
pub const DIRECT_CHOICE_VALUE: &str = "direct:";

/// 这条线路是不是"这个订阅的这个节点"的等价线路。
fn lane_is_for_node(lane: &Lane, subscription: &SubscriptionId, node: &str) -> bool {
    lane.kind == LaneKind::SubscriptionNode
        && lane.subscription.as_ref() == Some(subscription)
        && lane.node.as_deref() == Some(node)
}

/// 同一个网络在配置里的**稳定 id**（`lane-<订阅>-<节点>`，两段都用同一套 slug 规则净化）。
///
/// 复用 `upstream::id_slug` 而不是在这里再写一遍：id 派生法有两份实现就一定会漂移
/// （那条教训）。
fn lane_id_for_node(subscription: &SubscriptionId, node: &str) -> LaneId {
    let sid = crate::commands::upstream::id_slug(subscription.as_str());
    let nid = crate::commands::upstream::id_slug(node);
    let tail = if nid.is_empty() { "node".to_string() } else { nid };
    LaneId::new(format!("lane-{}-{tail}", if sid.is_empty() { "sub" } else { &sid }))
}

async fn compute_lane_choices(state: &AppState) -> IpcResult<Vec<LaneChoiceGroup>> {
    let doc = state.doc();
    // 直连那组永远第一组（与 `lane_pick_options` 同一个口径）
    let mut direct: Vec<LaneChoice> = Vec::new();
    let mut by_sub: BTreeMap<String, (String, Vec<LaneChoice>)> = BTreeMap::new();

    // ① 已配置的线路
    for lane in doc.lanes.iter().filter(|l| l.enabled) {
        let is_direct = lane.kind == LaneKind::Direct;
        let item = LaneChoice {
            value: lane.id.to_string(),
            label: lane.name.clone(),
            // 直连那条线路也用 `direct` 标记：界面据此把它**排在最前、单独一项**，
            // 与"配置里还没有 direct 线路"的合成项是同一种语义（`lane_id` 有值 = 已经存在）。
            kind: if is_direct { LaneChoiceKind::Direct } else { LaneChoiceKind::Lane },
            lane_id: Some(lane.id.clone()),
            subscription_id: lane.subscription.clone(),
            node: lane.node.clone(),
        };
        if is_direct {
            direct.push(item);
            continue;
        }
        let (key, title) = match lane.subscription.as_ref() {
            Some(sid) => (
                sid.to_string(),
                doc.proxy_subscriptions
                    .iter()
                    .find(|s| &s.id == sid)
                    .map(|s| s.name.clone())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| sid.to_string()),
            ),
            None => ("__other__".to_string(), OTHER_GROUP_LABEL.to_string()),
        };
        by_sub.entry(key).or_insert_with(|| (title, Vec::new())).1.push(item);
    }

    // ② 订阅节点（只来自**已采纳**的版本）
    let subscriptions: Vec<(SubscriptionId, String)> = doc
        .proxy_subscriptions
        .iter()
        .map(|s| {
            let title = if s.name.trim().is_empty() { s.id.to_string() } else { s.name.clone() };
            (s.id.clone(), title)
        })
        .collect();
    drop(doc);

    for (sid, title) in subscriptions {
        let Ok(outcome) = latest_outcome(state, &sid) else {
            // 还没有已采纳版本 → 这份订阅一个节点都不列（页面用空态说明"还没解析"，不在这里编）
            continue;
        };
        let entry = by_sub.entry(sid.to_string()).or_insert_with(|| (title, Vec::new()));
        for n in outcome.nodes.iter() {
            let node = n.name.clone();
            if node.trim().is_empty() {
                continue;
            }
            // 已经有等价线路 → 直接给 lane id（选中 = 复用）；否则给 node 值，交给 ensure 命令
            let existing = state
                .doc()
                .lanes
                .iter()
                .find(|l| lane_is_for_node(l, &sid, &node))
                .map(|l| l.id.clone());
            entry.1.push(LaneChoice {
                value: node_choice_value(&sid, &node),
                label: node.clone(),
                kind: LaneChoiceKind::Node,
                lane_id: existing,
                subscription_id: Some(sid.clone()),
                node: Some(node),
            });
        }
    }

    let mut groups: Vec<LaneChoiceGroup> = Vec::new();
    // ★ 「直连」**永远列在最前、永远单独一项**（用户第 81 轮的追问）。配置里还没有 direct 线路时
    //   照样列出来，值用 `direct:` 标记 —— 选中它由 `lane_ensure_direct` 把那条线路建出来。
    if direct.is_empty() {
        direct.push(LaneChoice {
            value: DIRECT_CHOICE_VALUE.to_string(),
            label: DIRECT_GROUP_LABEL.to_string(),
            kind: LaneChoiceKind::Direct,
            lane_id: None,
            subscription_id: None,
            node: None,
        });
    }
    groups.push(LaneChoiceGroup { group: DIRECT_GROUP_LABEL.to_string(), items: direct });
    for (_, (title, items)) in by_sub {
        groups.push(LaneChoiceGroup { group: title, items });
    }
    Ok(groups)
}

/// `lane_options`：线路（网络代理）下拉的**全部可选来源** —— 已配置线路 + 订阅节点。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_options(state: State<'_, AppState>) -> IpcResult<Vec<LaneChoiceGroup>> {
    compute_lane_choices(&state).await
}

/// `lane_ensure_for_node`：让"这个订阅的这个节点"在配置里**有一条线路**，返回那条线路。
///
/// 语义是"确保存在"，所以**幂等**：
/// * 已经有等价线路（`kind=subscription_node` + 同一个订阅 + 同一个节点）→ 直接返回它；
/// * 否则建一条：id 由 `lane_id_for_node` 派生、`name` 就是节点名原样。
///
/// 两条硬约束：
/// * **节点必须真的在已采纳的订阅版本里**（否则报错并给出可见节点示例）—— 绝不凭空造一条
///   指向不存在节点的线路；
/// * 派生出的 id 若已被**另一条不同的线路**占用 → 报错点名，不覆盖用户的配置。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_ensure_for_node(
    state: State<'_, AppState>,
    subscription_id: SubscriptionId,
    node: String,
) -> IpcResult<Lane> {
    ensure_lane_for_node(&state, &subscription_id, &node).await
}

/// `lane_ensure_direct`：让配置里**有一条直连线路**，返回那条线路（**幂等**）。
///
/// 「直连」是网络代理里最重要的选项（用户第 81 轮原话），而下拉里要能**明确选中**它、
/// 而不是只能选一个叫"继承"的东西（继承会跟着凭据 / 服务商 / 默认线路走）。所以：
/// * 已经有 `kind: direct` 的线路 → 直接返回它（用户自己命名过就用他的那条）；
/// * 没有 → 建一条 `lane-direct` / "直连（本机网络）"（与 `IMPLICIT_DIRECT_LANE` 的兜底名字一致）。
///
/// 走与手工新建**同一条**写入路径（`upsert_lane`：校验 → 事务 → 快照 → 内核重载）。
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn lane_ensure_direct(state: State<'_, AppState>) -> IpcResult<Lane> {
    ensure_lane_direct(&state).await
}

async fn ensure_lane_direct(state: &AppState) -> IpcResult<Lane> {
    if let Some(existing) = state.doc().lanes.iter().find(|l| l.kind == LaneKind::Direct).cloned() {
        return Ok(existing);
    }
    // id 冲突（用户手工建了一条**别的**线路，恰好占着 `lane-direct`）→ 报错点名，绝不覆盖
    let id = LaneId::new(IMPLICIT_DIRECT_LANE);
    if let Some(clash) = state.doc().lanes.iter().find(|l| l.id == id) {
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "直连线路的 id {id} 已经被另一条线路占用（kind={:?}，name={:?}）：\
             先把那条改名/删掉，或直接选它作为直连线路",
            clash.kind, clash.name
        ))));
    }
    let lane = Lane {
        id,
        name: DIRECT_GROUP_LABEL.to_string(),
        kind: LaneKind::Direct,
        subscription: None,
        group: None,
        group_type: None,
        node: None,
        filter: None,
        exclude_filter: None,
        enabled: true,
        extra: Default::default(),
    };
    let saved = upsert_lane(state, lane).await?;
    tracing::info!(lane_id = %saved.id, "已建出直连线路");
    Ok(saved)
}

async fn ensure_lane_for_node(
    state: &AppState,
    subscription_id: &SubscriptionId,
    node: &str,
) -> IpcResult<Lane> {
    let node = node.trim();
    if node.is_empty() {
        return Err(IpcError::from(TaError::MalformedRequest("节点名不能为空".to_string())));
    }
    {
        let doc = state.doc();
        find_subscription(&doc, subscription_id)?;
        if let Some(existing) =
            doc.lanes.iter().find(|l| lane_is_for_node(l, subscription_id, node)).cloned()
        {
            return Ok(existing);
        }
    }

    // 节点必须真的存在（已采纳版本里）——错名/过期订阅在这里被挡下，并给出可见示例
    let outcome = latest_outcome(state, subscription_id)?;
    if !outcome.nodes.iter().any(|n| n.name == node) {
        let mut sample: Vec<&str> = outcome.nodes.iter().map(|n| n.name.as_str()).take(8).collect();
        sample.sort_unstable();
        return Err(IpcError::from(TaError::MalformedRequest(format!(
            "订阅 {subscription_id} 的已采纳版本里没有节点 {node}（名字拼错，或订阅刚刷新过）。\
             可见节点示例：{}",
            if sample.is_empty() {
                "（一个都没有）".to_string()
            } else {
                sample.join("、")
            }
        ))));
    }

    let id = lane_id_for_node(subscription_id, node);
    {
        // id 冲突：同一个 id 已经指向**另一条**线路（不是这个节点）→ 报错点名，绝不覆盖
        let doc = state.doc();
        if let Some(clash) = doc.lanes.iter().find(|l| l.id == id) {
            return Err(IpcError::from(TaError::MalformedRequest(format!(
                "线路 id {id} 已经被另一条线路占用（kind={:?}，name={:?}）：\
                 先把那条改名/删掉，或手工新建一条指向节点 {node} 的线路",
                clash.kind, clash.name
            ))));
        }
    }

    let lane = Lane {
        id,
        name: node.to_string(),
        kind: LaneKind::SubscriptionNode,
        subscription: Some(subscription_id.clone()),
        group: None,
        group_type: None,
        node: Some(node.to_string()),
        filter: None,
        exclude_filter: None,
        enabled: true,
        extra: Default::default(),
    };
    // 走与手工新建**同一条**写入路径（校验 → 事务 → 快照 → 内核重载）
    let saved = upsert_lane(state, lane).await?;
    tracing::info!(lane_id = %saved.id, node, "已按节点建出线路");
    Ok(saved)
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
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("-v").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // 内核是控制台程序：不给 CREATE_NO_WINDOW，探一次版本就会闪一个 cmd 窗口
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
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
    node_view_of_with(n, &[])
}

/// 同上，但知道**这份订阅关掉了哪些节点**（默认全启用，用户第 202 轮）。
fn node_view_of_with(n: &ta_subscription::NodeMeta, disabled: &[String]) -> NodeView {
    NodeView {
        name: n.name.clone(),
        kind: n.kind.clone(),
        group: None,
        latency_ms: None,
        ok: None,
        enabled: !disabled.iter().any(|d| d == &n.name),
    }
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

/// **补齐线路端口，并推给内核源与网关拨号器。**
///
/// 为什么需要它（2026-10-13 的真机复现， 同族）：
///
/// `commit_config`（本文件上方）只在 `kernel_source_slot()` **已经装好**时才调
/// `allocate_ports` —— 而开机编排完成之前那个槽是 `None`，此时提交的配置
/// **端口一个都没分配，配置却照样入库**。之后没有任何地方补分配，于是：
///
/// ```text
/// 配置校验失败：[FieldError { path: "lanes[0].id", code: "port_missing" }]
///   → 内核永不就绪 → 启动编排超时 → **网关根本不启动** → 连直连也不可用
/// ```
///
/// 也就是说：那句"（直连仍可用）"的日志是**替事实打包票**，而事实恰恰相反。
/// 这个函数就是把缺的那次分配补上，让"配错的线路"最多只影响它自己那条出口。
///
/// 幂等：`RangePortAllocator` 会优先拿回落库的端口，重复调用不会让端口漂移。
fn ensure_lane_ports(state: &AppState) -> IpcResult<()> {
    // 槽还没装好就什么都不做：那时连内核源都没有，推给它没有意义。
    let Some(source) = state.kernel_source_slot() else {
        return Ok(());
    };
    let doc = state.doc();
    let ports = allocate_ports(state, &doc)?;
    source.set_ports(ports.clone());
    if let Some(hot) = state.lane_ports() {
        hot.replace(ports);
    }
    Ok(())
}

/// 订阅/线路数据出现之后再拉一次内核。
///
/// 启动那一刻还没有任何 provider，生成配置的预检会**如实拒绝**（`TA-5001`），内核不会就绪——
/// 那是对的（没有节点时内核无路可走）。等用户真的建了订阅、节点出现之后，内核才有意义。
///
/// **失败只记 WARN，但不能因此宣称"直连仍可用"**：网关要等内核就绪才会启动，
/// 内核一直不就绪时它根本不起来 —— 直连同样用不了（2026-10-13 实测）。
/// 日志只准陈述已经发生的事，不准替事实打包票。
pub(crate) async fn kick_kernel(state: &AppState) {
    // 这个仓库用的是标准库 `RwLock`：`read()` 返回 `Result<Guard>`，不是 parking_lot 的裸 guard。
    let kernel = {
        let Ok(guard) = state.kernel.read() else {
            return;
        };
        let Some(kernel) = guard.clone() else {
            return;
        };
        kernel
    };
    // 先补端口，再重生成配置 —— 顺序不能反：`apply()` 内部的预检会**如实拒绝**没有端口的线路。
    if let Err(e) = ensure_lane_ports(state) {
        // `IpcError` 没有 `code()`、也不实现 `Display`（那是 `TaError` 的接口）——
        // 这里用 Debug，别为了好看去给 `IpcError` 加 trait 实现。
        // `error_log_guard`（crates/ta-core/tests/error_log_guard.rs）要求 warn 必须带 code。
        // `IpcError` 没有 `code()` 方法，但它的字段就叫 `code`。
        tracing::warn!(
            code = %e.code,
            error = ?e,
            "重拉内核前补分配线路端口失败：内核会因缺少端口而不就绪，网关也不会启动"
        );
    }
    // **`apply()` 而不是 `start()`**：`apply()` = 生成 + 落盘 + reload；
    // 内核已经在跑时 `start()` 是空操作 ⇒ 配置永远不重新生成（实测：`proxy-groups: []`）。
    match kernel.apply().await {
        Ok(status) => tracing::info!(?status, "订阅数据就绪：已重生成并重载内核配置"),
        Err(e) => tracing::warn!(
            code = %e.code(),
            error = %e,
            // 老文案是"（直连仍可用）"。**实测不成立**：网关要等内核就绪才启动，
            // 内核一直不就绪时它根本不起来，直连同样不可用。
            "订阅数据就绪后重拉内核失败：若内核始终不就绪，网关不会启动，**直连也不可用**（查 boot phase）"
        ),
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

    /// 装配守卫：`kick_kernel` 必须**先补端口、再重生成配置**。
    ///
    /// 为什么值得一条守卫：`commit_config` 只在 `kernel_source_slot()` 已经装好时才调
    /// `allocate_ports`，而**在那个槽装好之前提交的配置，端口一个都没有**。
    /// 少了这次补分配，内核会因为 `port_missing` 永远不就绪，而启动编排要等内核就绪
    /// 才启动网关 —— 表现是**整个网关都不起来（连直连也不可用）**。
    ///
    /// 这是典型的"静默失效"：少了它**不会报任何错**，只会让软件打不开。
    /// 2026-10-13 真机复现过，所以钉死它。
    #[test]
    fn kick_kernel_compensates_missing_lane_ports() {
        let src = include_str!("net.rs");
        let start = src
            .find("pub(crate) async fn kick_kernel")
            .expect("kick_kernel 不见了（改名就同步改这条守卫）");
        let tail = &src[start..];
        let end = tail.find("\n}\n").unwrap_or(tail.len());
        let body = &tail[..end];

        let apply_at = body.find("kernel.apply()").expect("kick_kernel 里找不到 kernel.apply()");
        let ensure_at = body.find("ensure_lane_ports(state)").expect(
            "kick_kernel 没有补分配线路端口 —— 端口缺失时内核永不就绪、网关不会启动（见 ensure_lane_ports 的注释）",
        );
        assert!(
            ensure_at < apply_at,
            "补端口必须排在 apply() **之前**：apply() 内部的预检会如实拒绝没有端口的线路"
        );
    }

    /// 守卫：**日志不许替事实打包票**。
    ///
    /// 老文案是「订阅数据就绪后拉起内核失败（直连仍可用）」，而 2026-10-13 实测它**不成立**：
    /// 网关要等内核就绪才启动，内核一直不就绪时它根本不起来，直连同样不可用。
    /// 这里只禁**代码里**的字面量；注释为了说明缘由可以引用那句话。
    #[test]
    fn no_log_claims_direct_stays_available_when_kernel_is_down() {
        let src = include_str!("net.rs");
        // ⚠️ 用 `concat!` 把这个词拼出来，**不要直接写字面量**：
        // 这条守卫扫的正是它自己所在的文件，直接写会让它扫到自己这一行（注释不算，代码行算）。
        // 上一版就是这么红的 —— 守卫把自己抓了。
        let needle = concat!("直连", "仍可用");
        for (i, line) in src.lines().enumerate() {
            if !line.contains(needle) {
                continue;
            }
            let t = line.trim_start();
            assert!(
                t.starts_with("//"),
                "net.rs:{} 在**代码**里出现那句被推翻的旧结论。日志只准陈述已经发生的事，\
                 不许宣称一个「内核不就绪时直连依然可用」的结论。行：{}",
                i + 1,
                t
            );
        }
    }
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
            disabled_nodes: Vec::new(),
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
        // 只有**真的发出了 HTTP 请求**的连接才消耗一份脚本响应。
        //
        // 为什么（第 72 轮在 `cargo test --workspace` 上偶发抓到过）：负载高时会出现"连上了、
        // 请求还没发出去就被重置"的连接；旧写法无条件 `hits.fetch_add(1)`，这种连接会**偷走**
        // 一条脚本响应，把整条脚本错位 —— 现象是"第一版拿到了本该给第二版的 HTML"，于是一版都
        // 没被采纳，断言里的计数变成 `None`（而单跑这个测试永远复现不出来）。
        if !buf.windows(7).any(|w| w == b"HTTP/1.") {
            return;
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
        /// 内核给的那张延迟表（provider 命名空间）。默认"节点1 比节点0 快"，用来验选最低那条。
        provider_delays: BTreeMap<String, Option<u32>>,
    }

    impl Default for FakeInner {
        fn default() -> Self {
            let mut provider_delays = BTreeMap::new();
            provider_delays.insert("节点0".to_string(), Some(200));
            provider_delays.insert("节点1".to_string(), Some(15));
            Self { state: KernelState::Stopped, calls: Vec::new(), applied: 0, provider_delays }
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
        /// ★ 假内核也**只暴露 provider 命名空间这一条路**：调用会记进
        /// `calls`，于是"应用到底问的是哪条路由"在测试里是可断言的。
        async fn provider_delays(
            &self,
            key: &str,
            probe_url: &str,
            _wait_ms: u64,
        ) -> ta_core::error::Result<BTreeMap<String, Option<u32>>> {
            let mut g = self.inner.lock().unwrap();
            g.calls.push(format!("provider_delays:{key}:{probe_url}"));
            Ok(g.provider_delays.clone())
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

    /// **E 期 B 方案**：把「自动」出口的**后半段编排**在测试里完整回放。
    ///
    /// 为什么必须有这条：本环境**没有内核可执行文件**，真机验不了"选最低延迟"；
    /// 而这一段恰恰是用户最在意的语义 —— 它不验"判断"（那有 `decide_export` 的 7 条单测），
    /// 验的是**两个副作用**：内核真的收到了 `set_select`、库里真的记住了那个**地址**。
    #[tokio::test]
    async fn auto_export_choice_writes_kernel_and_remembers_address() {
        use ta_subscription::AutoCandidate;

        let state = fresh_state("auto-choice").await;
        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());
        let lane = LaneId::new("lane-auto");
        let store = SqliteAutoChoiceStore::new(state.db.clone());

        let cands = |hk: Option<u32>, jp: Option<u32>| {
            vec![
                AutoCandidate { name: "hk", address: "203.0.113.7", latency_ms: hk },
                AutoCandidate { name: "jp", address: "198.51.100.9", latency_ms: jp },
            ]
        };

        // ① 首次：选最低（hk），内核被写、库里记住的是**地址**
        let first =
            apply_auto_export_choice(&state, "p1", &lane, false, &cands(Some(30), Some(80)))
                .await
                .expect("应当成功");
        assert_eq!(first.why, "first_time");
        assert_eq!(first.node.as_deref(), Some("hk"));
        assert_eq!(first.address.as_deref(), Some("203.0.113.7"));
        assert!(fake.calls().iter().any(|c| c == "set_select:lane-auto:hk"), "内核要收到这一刀");
        assert_eq!(store.get("p1").await.unwrap().as_deref(), Some("203.0.113.7"));

        // ② 粘性：jp 变得更快了也不换 —— 上次那条**地址**还在就沿用（用户最在意的"不换 IP"）
        let keep =
            apply_auto_export_choice(&state, "p1", &lane, false, &cands(Some(200), Some(10)))
                .await
                .expect("应当成功");
        assert_eq!(keep.why, "same_address_still_there");
        assert_eq!(keep.node.as_deref(), Some("hk"), "更快的不许抢走");

        // ③ 手动刷新 = 重挑：这次选 jp，并把它记下来
        let manual =
            apply_auto_export_choice(&state, "p1", &lane, true, &cands(Some(200), Some(10)))
                .await
                .expect("应当成功");
        assert_eq!(manual.why, "manual_refresh");
        assert_eq!(manual.node.as_deref(), Some("jp"));
        assert_eq!(store.get("p1").await.unwrap().as_deref(), Some("198.51.100.9"));

        // ④ 全都不通：**什么都不写**（不编一个节点、不改内核、不动库）
        let before = fake.calls().len();
        let dead = apply_auto_export_choice(&state, "p1", &lane, false, &cands(None, None))
            .await
            .expect("应当成功");
        assert_eq!(dead.why, "all_unreachable");
        assert_eq!(dead.node, None);
        assert_eq!(fake.calls().len(), before, "内核一次都不该被碰");
        assert_eq!(store.get("p1").await.unwrap().as_deref(), Some("198.51.100.9"));
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

    ///（第 324 轮）**覆盖已存在的条目时，`url` 留空 = 不改地址**。
    ///
    /// 为什么必须有这条语义：订阅地址里带订阅 token，界面**从不回显原文**
    ///（`maskSubscriptionUrl` 是唯一出口，见 `src/pages/network/model.ts`），
    /// 所以编辑表单里的地址框**只能是空的**。如果这里的语义是"按原样覆盖"，
    /// 用户改一次名字就会把 `...?token=***` 写进配置 —— 下次刷新必然失败，
    /// 而保存那一刻不报错、还显示"已保存"。
    ///
    /// 两件事一起钉住（这一条测试同时是行为规格）：
    /// * 留空 ⇒ 保留原地址，且**返回值里也是原地址**（前端结果区显示的是它）；
    /// * 改了的字段（这里 `name`）要真的改掉，条目数不变。
    ///
    /// 反面（**新建**时留空仍然必须被拒）由
    /// `subscription_upsert_refuses_invalid_and_writes_nothing` 守着。
    #[tokio::test]
    async fn subscription_upsert_keeps_the_existing_url_when_the_incoming_one_is_blank() {
        let state = seeded("sub-keep-url", "https://airport.example/sub").await;
        let existing = state
            .config
            .load()
            .await
            .expect("load")
            .proxy_subscriptions
            .into_iter()
            .find(|s| s.id.as_str() == "sub-a")
            .expect("种下的订阅");
        assert!(existing.url.contains("airport.example"));

        // 界面的编辑就是长这样：拿不到明文地址 → 只发名字等非秘密字段，url 是空串
        let mut edit = existing.clone();
        edit.name = "改个名".to_string();
        edit.url = String::new();
        let saved = upsert_subscription(&state, edit).await.expect("留空 = 不改，不该被拒");

        assert_eq!(saved.url, existing.url, "返回值里的地址应当是原地址，不是空串");

        let after = state.config.load().await.expect("load");
        assert_eq!(after.proxy_subscriptions.len(), 1, "同 id 覆盖，不该新增一条");
        let stored =
            after.proxy_subscriptions.iter().find(|s| s.id.as_str() == "sub-a").expect("还在");
        assert_eq!(stored.url, existing.url, "地址必须原样保留");
        assert_eq!(stored.name, "改个名", "改了的字段要真的改掉");
    }

    /// 被引用的订阅不许删。
    ///
    /// 第 320 轮改判据：这里原来断言 `TA-5001`，而那**正是用户报的那个 bug** ——
    /// `TA-5001`（配置校验失败）把"这次删除会造成悬空引用"说成了"配置本来就有一处错"，
    /// 且消息只有「1 处问题」，指不出是哪条线路、该怎么办。
    /// 现在前置检查 → `TA-5002` + **引用者清单** + "先解除"，与 `delete_provider` / `delete_channel` 同一口径。
    #[tokio::test]
    async fn subscription_delete_removes_unreferenced_and_refuses_referenced() {
        let state = seeded("sub-del", "https://airport.example/sub").await;
        // sub-a 被 lane-auto 引用 -> 必须被引用完整性拦住，且要说清**是谁**在挡、**怎么办**
        let err = delete_subscription(&state, &SubscriptionId::new("sub-a"))
            .await
            .expect_err("被引用的订阅不许删");
        assert_eq!(err.code, "TA-5002", "{}", err.message);
        assert!(err.message.contains("sub-a"), "要点名被删的对象：{}", err.message);
        // 引用者用**线路的名字**列出来（这里是 lane-auto 的 name），不是只给一个数字
        assert!(err.message.contains("自动择优"), "要列出引用它的线路：{}", err.message);
        assert!(err.message.contains("先解除"), "要给出真能走通的路：{}", err.message);
        assert!(
            !err.message.contains("force"),
            "订阅删除没有 force 参数，提它就是撒谎：{}",
            err.message
        );
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

    // ------------------------------------------------------------ 线路下拉的来源

    /// 「直连」**永远单独列出来、永远排第一**，哪怕配置里一条线路都没有。
    ///
    /// 用户第 81 轮的追问：「网络代理选项中最重要的是直连，这个你都没单独列出来？」
    /// 所以这里钉两件事：① 第一组第一项就是直连；② 配置里没有 direct 线路时它带 `direct:`
    /// 标记（`lane_id` 缺省），由 `lane_ensure_direct` 在选中时把线路建出来。
    #[tokio::test]
    async fn lane_options_always_lists_direct_first_even_without_any_lane() {
        let airport = FakeAirport::start(vec![yaml_with(2, 1, true)]);
        let state = fresh_state("lane-direct-first").await;
        let mut doc = ConfigDoc::default();
        doc.proxy_subscriptions.push(sub_cfg("sub-a", &airport.url("/sub")));
        // **一条线路都不配**：这正是用户现场（`doc.lanes` 为空）
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        refresh_subscriptions(&state, None, false).await.expect("刷新");

        let groups = compute_lane_choices(&state).await.expect("可选来源");
        let first = &groups[0];
        assert_eq!(first.group, DIRECT_GROUP_LABEL, "直连永远是第一组");
        let head = &first.items[0];
        assert_eq!(head.kind, LaneChoiceKind::Direct, "第一项是直连：{head:?}");
        assert_eq!(head.label, DIRECT_GROUP_LABEL);
        assert_eq!(head.value, DIRECT_CHOICE_VALUE, "还没有 direct 线路时给标记值");
        assert!(head.lane_id.is_none(), "还没有那条线路，不能编一个 id");

        // 订阅节点仍然在后面的组里
        assert!(groups
            .iter()
            .skip(1)
            .any(|g| g.items.iter().any(|c| c.kind == LaneChoiceKind::Node)));
    }

    /// 已有 direct 线路时：**用它自己那条**（用户可能改过名字），并带上 lane_id。
    #[tokio::test]
    async fn lane_options_uses_the_configured_direct_lane() {
        let state = seeded("lane-direct-configured", "http://127.0.0.1:1/sub").await;
        let groups = compute_lane_choices(&state).await.expect("可选来源");
        let head = &groups[0].items[0];
        assert_eq!(head.kind, LaneChoiceKind::Direct);
        assert_eq!(head.value, "lane-direct");
        assert_eq!(head.lane_id.as_ref().map(LaneId::as_str), Some("lane-direct"));
        assert_eq!(groups[0].group, DIRECT_GROUP_LABEL);
    }

    /// `lane_ensure_direct`：没有就建（`lane-direct` / 直连（本机网络）），有就**复用**，
    /// 并且走的是与手工新建同一条写入路径（真的落库）。
    #[tokio::test]
    async fn lane_ensure_direct_creates_then_reuses() {
        let state = fresh_state("lane-ensure-direct").await;
        assert!(state.doc().lanes.is_empty());

        let created = ensure_lane_direct(&state).await.expect("建直连线路");
        // id 就是 `IMPLICIT_DIRECT_LANE`（内核兜底那个名字），全 app 只有一套
        assert_eq!(created.id.as_str(), IMPLICIT_DIRECT_LANE);
        assert_eq!(created.kind, LaneKind::Direct);
        assert_eq!(created.name, DIRECT_GROUP_LABEL);
        assert!(created.enabled);
        // 真的写进配置了（内存 + 磁盘两条路都读得到）
        assert_eq!(state.doc().lanes.len(), 1);
        let saved = state.config.load().await.expect("读回");
        assert_eq!(saved.lanes.len(), 1, "落库了才算建出来");

        let again = ensure_lane_direct(&state).await.expect("再调一次");
        assert_eq!(again.id, created.id, "幂等：复用同一条");
        assert_eq!(state.doc().lanes.len(), 1, "不重复建");

        // 建完之后下拉里那一项就带上 lane_id（选中直接用，不再需要 ensure）
        let head = compute_lane_choices(&state).await.expect("可选来源")[0].items[0].clone();
        assert_eq!(head.value, IMPLICIT_DIRECT_LANE);
        assert_eq!(head.lane_id.as_ref().map(LaneId::as_str), Some(IMPLICIT_DIRECT_LANE));
    }

    /// `lane_ensure_direct` 的 id 被**另一条**线路占着 → 报错点名，绝不覆盖。
    #[tokio::test]
    async fn lane_ensure_direct_refuses_an_id_clash() {
        let state = fresh_state("lane-direct-clash").await;
        let mut doc = ConfigDoc::default();
        doc.proxy_subscriptions.push(sub_cfg("sub-a", "http://127.0.0.1:1/sub"));
        // 用户自己建了一条**别的**线路，恰好占着 `IMPLICIT_DIRECT_LANE` 这个 id
        doc.lanes.push(Lane::subscription_auto(IMPLICIT_DIRECT_LANE, "自动择优", "sub-a"));
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);

        let err = ensure_lane_direct(&state).await.expect_err("id 冲突必须报错");
        assert!(err.message.contains(IMPLICIT_DIRECT_LANE), "要点名那个 id：{}", err.message);
        assert_eq!(state.doc().lanes.len(), 1, "被拒绝时不许再建一条");
        assert_eq!(
            state.doc().lanes[0].kind,
            LaneKind::SubscriptionAuto,
            "用户那条线路一个字节都不许动"
        );
    }

    /// `lane_options`：已配置线路 + **订阅节点**都要在；节点项带来 `node:` 值，
    /// 已经有等价线路的节点项还会带上 `lane_id`（选中即复用）。
    ///
    /// 这条用例对着的正是用户现场："订阅加好了、但配置里一条线路都没有" —— 那时下拉里
    /// 至少要有那个订阅的**节点**可选，否则"给线路切网络"根本做不到。
    #[tokio::test]
    async fn lane_options_lists_configured_lanes_and_subscription_nodes() {
        let airport = FakeAirport::start(vec![yaml_with(3, 1, true)]);
        let state = seeded("lane-options", &airport.url("/sub")).await;
        refresh_subscriptions(&state, None, false).await.expect("刷新");

        let groups = compute_lane_choices(&state).await.expect("可选来源");
        let all: Vec<&LaneChoice> = groups.iter().flat_map(|g| g.items.iter()).collect();

        // 已配置的两条线路在（直连 / 自动择优）
        assert!(
            all.iter().any(|c| c.value == "lane-direct" && c.kind == LaneChoiceKind::Direct),
            "直连那条线路必须在（并且带 `direct` 标记，界面据此排在最前）：{all:?}"
        );
        assert!(
            all.iter().any(|c| c.value == "lane-auto" && c.kind == LaneChoiceKind::Lane),
            "订阅自动择优那条线路必须在：{all:?}"
        );
        // 订阅里的 3 个节点也在，且值形如 `node:<订阅id>:<节点名>`
        let nodes: Vec<&&LaneChoice> =
            all.iter().filter(|c| c.kind == LaneChoiceKind::Node).collect();
        assert_eq!(nodes.len(), 3, "订阅解析出的 3 个节点都要列出来：{all:?}");
        for n in &nodes {
            assert!(n.value.starts_with("node:sub-a:"), "节点项的值要带来源：{}", n.value);
            assert!(n.lane_id.is_none(), "还没有等价线路时不该有 lane_id");
        }
    }

    /// 真的按节点建出一条线路（`kind: subscription_node` + 订阅 + 节点名都写上），
    /// 且**幂等**：再调一次返回同一条，不重复建。
    #[tokio::test]
    async fn lane_ensure_for_node_creates_then_reuses_the_line() {
        let airport = FakeAirport::start(vec![yaml_with(2, 1, true)]);
        let state = seeded("lane-ensure", &airport.url("/sub")).await;
        refresh_subscriptions(&state, None, false).await.expect("刷新");
        let node = nodes_of_subscription(&state, &SubscriptionId::new("sub-a"), None)
            .await
            .expect("节点")[0]
            .name
            .clone();

        let first = ensure_lane_for_node(&state, &SubscriptionId::new("sub-a"), &node)
            .await
            .expect("按节点建线路");
        assert_eq!(first.kind, LaneKind::SubscriptionNode);
        assert_eq!(first.subscription.as_ref().map(SubscriptionId::as_str), Some("sub-a"));
        assert_eq!(first.node.as_deref(), Some(node.as_str()));
        assert_eq!(first.name, node, "线路名就取节点名（人能读的那一个）");
        assert!(first.enabled, "建出来就是能用的");
        // id 由订阅 + 节点派生（同一个网络在任何地方都是同一个 id）
        assert_eq!(
            first.id.as_str(),
            lane_id_for_node(&SubscriptionId::new("sub-a"), &node).as_str()
        );

        let again = ensure_lane_for_node(&state, &SubscriptionId::new("sub-a"), &node)
            .await
            .expect("再调一次");
        assert_eq!(again.id, first.id, "幂等：复用同一条，不重复建");
        assert_eq!(state.doc().lanes.len(), 3, "两条原有的 + 新建的一条");

        // 建完之后 `lane_options` 里那个节点项就带上 lane_id 了（界面选中即复用）
        let groups = compute_lane_choices(&state).await.expect("可选来源");
        let item = groups
            .iter()
            .flat_map(|g| g.items.iter())
            .find(|c| c.kind == LaneChoiceKind::Node && c.node.as_deref() == Some(node.as_str()))
            .expect("节点项还在");
        assert_eq!(item.lane_id.as_ref().map(LaneId::as_str), Some(first.id.as_str()));
    }

    /// **不编数据**：节点名不在已采纳版本里 → 报错点名，并且**库里一条都没多**。
    #[tokio::test]
    async fn lane_ensure_for_node_refuses_a_node_that_the_subscription_does_not_have() {
        let airport = FakeAirport::start(vec![yaml_with(2, 1, true)]);
        let state = seeded("lane-ensure-bad", &airport.url("/sub")).await;
        refresh_subscriptions(&state, None, false).await.expect("刷新");
        let before = state.doc().lanes.len();

        let err = ensure_lane_for_node(&state, &SubscriptionId::new("sub-a"), "东京-99")
            .await
            .expect_err("不存在的节点必须报错");
        assert_eq!(err.code, "TA-1004");
        assert!(err.message.contains("东京-99"), "要点名那个节点：{}", err.message);
        assert_eq!(state.doc().lanes.len(), before, "被拒绝时一条线路都不许建");

        // 还没有已采纳版本的订阅：同样不许建（错误来自版本存储，如实上报）
        let state2 = seeded("lane-ensure-noversion", &airport.url("/never")).await;
        let err2 = ensure_lane_for_node(&state2, &SubscriptionId::new("sub-a"), "x")
            .await
            .expect_err("没有已采纳版本必须报错");
        assert_eq!(err2.code, "TA-1004");
        assert!(err2.message.contains("已采纳"), "{}", err2.message);
    }

    /// 派生出的 id 被**另一条不同的线路**占着 → 报错点名，绝不覆盖用户的配置。
    #[tokio::test]
    async fn lane_ensure_for_node_refuses_an_id_clash_instead_of_overwriting() {
        let airport = FakeAirport::start(vec![yaml_with(2, 1, true)]);
        let state = seeded("lane-ensure-clash", &airport.url("/sub")).await;
        refresh_subscriptions(&state, None, false).await.expect("刷新");
        let node = nodes_of_subscription(&state, &SubscriptionId::new("sub-a"), None)
            .await
            .expect("节点")[0]
            .name
            .clone();
        let id = lane_id_for_node(&SubscriptionId::new("sub-a"), &node);

        // 用户手工建了一条**别的**线路，恰好占着这个 id
        let mut doc = (*state.doc()).clone();
        doc.lanes.push(Lane::direct(id.as_str(), "我自己建的直连"));
        state.config.save(&doc).await.expect("种冲突");
        state.set_doc(doc);

        let err = ensure_lane_for_node(&state, &SubscriptionId::new("sub-a"), &node)
            .await
            .expect_err("id 冲突必须报错");
        assert!(err.message.contains(id.as_str()), "要点名那个 id：{}", err.message);
        let after = state.doc().lanes.iter().find(|l| l.id == id).cloned().expect("还在");
        assert_eq!(after.name, "我自己建的直连", "绝不覆盖用户已有的那条线路");
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

    /// `subscription_list` 必须带上**当前生效版本**的解析状态（那条硬前置）。
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
        // 前置写死：v1 **必须真的被采纳**。旧写法只 `.expect(...)` 了"命令返回 Ok" ——
        // 而命令在"下载失败/被拦下"时同样返回 Ok，于是脚本一旦错位，失败会推迟到下面
        // 那个计数断言上，读起来像"计数逻辑坏了"，其实是脚本错位（见 `serve_one` 的注释）。
        let first = refresh_subscriptions(&state, Some(id.clone()), true).await.expect("v1 请求");
        assert!(first.ok, "v1 必须是好内容且被采纳：{first:?}");
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

    /// 用户原话里那一行：更新时间 + 已用/总量。
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

    /// 删线路的**每一类引用**都必须认出来。
    ///
    /// 为什么每类都要单独钉：引用分散在配置的不同地方，而 validator 的检查也是分开写的。
    /// **漏认一类**的后果是静默的：那一类会回到"靠校验层兜底"的老路，
    /// 用户又只拿到一句「配置校验失败：1 处问题」—— 正是 要修的那个 bug。
    ///
    ///（第 322 轮）类别从**五类收敛到两类**：线路分配只剩服务商级一处。
    #[tokio::test]
    async fn lane_delete_names_every_kind_of_reference() {
        /// 每一类引用：怎么把它挂上去、以及消息里**必须**出现的关键词。
        type Setup = fn(&mut ConfigDoc);
        let cases: Vec<(&str, Setup, &str)> = vec![
            (
                "net.default_lane",
                |d: &mut ConfigDoc| d.net.default_lane = Some(LaneId::new("lane-auto")),
                "默认线路",
            ),
            (
                "providers[].lane",
                |d: &mut ConfigDoc| d.providers[0].lane = Some(LaneId::new("lane-auto")),
                "服务商 p-a",
            ),
        ];

        for (what, setup, needle) in cases {
            let state = seeded(
                &format!("lane-ref-{}", what.replace(['.', '[', ']'], "-")),
                "https://a.example/sub",
            )
            .await;
            let mut doc = (*state.doc()).clone();
            setup(&mut doc);
            state.config.save(&doc).await.unwrap();
            state.set_doc(doc);

            let err = delete_lane(&state, &LaneId::new("lane-auto"))
                .await
                .expect_err(&format!("{what} 引用着它，必须拒绝"));
            assert_eq!(err.code, "TA-5002", "{what}: {}", err.message);
            assert!(
                err.message.contains(needle),
                "{what} 这一类引用没被认出来（消息里应有 `{needle}`）：{}",
                err.message
            );
            // 拒绝 = 一个字节都不许改
            assert!(
                state.doc().lanes.iter().any(|l| l.id.as_str() == "lane-auto"),
                "{what}: 拒绝了就不该把线路删掉"
            );
        }
    }

    /// **反回归闸门**：三个已删除的字段**不许**再挡住删除。
    ///
    /// 用户裁决放弃线路级别的分配之后，`credentials[].lane` / `channels[].lane` /
    /// `policies[].action.use_lane` 都不再参与任何判定。如果哪天有人把某一条的引用检查
    /// 加回 `delete_lane`，用户就会遇到"这条线明明没人用，却说被引用、删不掉" ——
    /// 而且**照它说的去改也改不掉**（因为那个字段在界面上根本不存在了）。
    ///
    /// 所以这里把它们**塞进老配置**（落进 `extra`）然后断言删除照常成功。
    #[tokio::test]
    async fn removed_lane_level_fields_do_not_block_deletion() {
        let state = seeded("lane-ref-legacy", "https://a.example/sub").await;
        let mut raw = serde_json::to_value((*state.doc()).clone()).expect("序列化");
        raw["credentials"][0]["lane"] = serde_json::json!("lane-auto");
        raw["channels"][0]["lane"] = serde_json::json!("lane-auto");
        raw["policies"] = serde_json::json!([{
            "id": "pol-legacy", "name": "老策略", "priority": 10,
            "action": { "use_lane": "lane-auto" }
        }]);
        let doc: ConfigDoc = serde_json::from_value(raw).expect("老配置必须能解析");
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        delete_lane(&state, &LaneId::new("lane-auto"))
            .await
            .expect("这三个字段已经不参与判定，不许再挡住删除");
        assert!(!state.doc().lanes.iter().any(|l| l.id.as_str() == "lane-auto"));
    }

    /// 同 `subscription_delete`：这里原来断言 `TA-5001`（也就是用户报的那个 bug）。
    /// 现在前置检查 → `TA-5002` + 逐类列出引用者 + "先解除"。
    ///
    /// 挡路的引用改成**服务商**（`providers[].lane`）—— 那是现在唯一的分配入口。
    #[tokio::test]
    async fn lane_delete_removes_unreferenced_and_refuses_referenced() {
        let state = seeded("lane-del", "https://airport.example/sub").await;
        let mut doc = (*state.doc()).clone();
        doc.providers[0].lane = Some(LaneId::new("lane-auto"));
        state.config.save(&doc).await.unwrap();
        state.set_doc(doc);

        let err =
            delete_lane(&state, &LaneId::new("lane-auto")).await.expect_err("被服务商引用不许删");
        assert_eq!(err.code, "TA-5002", "{}", err.message);
        // 术语必须对：挡路的是**服务商**（providers），被删的是**线路**（lanes）。
        assert!(err.message.contains("服务商"), "要说清是服务商在引用：{}", err.message);
        assert!(err.message.contains("先解除"), "要给出真能走通的路：{}", err.message);
        assert!(
            !err.message.contains("force"),
            "线路删除没有 force 参数，提它就是撒谎：{}",
            err.message
        );
        assert_eq!(state.config.load().await.expect("load").lanes.len(), 2);

        // 解除引用后能删
        let mut doc = (*state.doc()).clone();
        doc.providers[0].lane = None;
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

    // ------------------------------------------------------------ 「自动」出口探测

    /// ★ 这条是本次修复的**回归锚点**：探测必须走 provider 命名空间，且按服务商 `base_url`。
    #[tokio::test]
    async fn auto_export_probes_provider_namespace_and_picks_lowest() {
        let yaml = concat!(
            "proxies:\n",
            "  - {name: 节点0, type: ss, server: 198.51.100.9, port: 443, cipher: aes-128-gcm, password: p}\n",
            "  - {name: 节点1, type: ss, server: 203.0.113.7, port: 443, cipher: aes-128-gcm, password: p}\n",
        );
        let airport = FakeAirport::start(vec![yaml.to_string()]);
        let state = fresh_state("auto-probe").await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.proxy_subscriptions.push(sub_cfg("sub-a", &airport.url("/sub")));
        // 生产路径就是 `Lane::automatic(...)`（显式 select）——见 catalog.rs 的建线。
        doc.lanes.push(Lane::automatic("lane-auto", "自动", "sub-a"));
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        refresh_subscriptions(&state, Some(SubscriptionId::new("sub-a")), true)
            .await
            .expect("先拉一版订阅");

        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());
        let out = apply_auto_export(
            &state,
            "p-a",
            &LaneId::new("lane-auto"),
            &SubscriptionId::new("sub-a"),
            true,
        )
        .await
        .expect("探一次");

        assert_eq!(out.why, "manual_refresh");
        assert_eq!(out.node.as_deref(), Some("节点1"), "要挑延迟最低的那条（15ms < 200ms）");
        // ★ 「选了最低延迟」的**自证**：选中的那条的延迟 == 这一轮那张表里最低的延迟。
        assert_eq!(out.picked_ms, Some(15));
        assert_eq!(out.best_ms, Some(15));
        assert_eq!(out.measured, 2, "两条都量到了");
        let calls = fake.calls();
        assert!(
            calls.contains(&"provider_delays:sub-a:https://a.example/v1".to_string()),
            "必须按 provider key + 服务商 base_url 测延迟：{calls:?}"
        );
        assert!(calls.contains(&"set_select:lane-auto:节点1".to_string()), "选出来的要写进内核");
        assert!(
            !calls.iter().any(|c| c.starts_with("delay_of")),
            "不许再走 `/proxies/<name>/delay`（provider 的节点在那条路由上全是 404）：{calls:?}"
        );
    }

    /// ★ 用户第 202 轮：**关掉的节点不进「自动」的候选**。
    ///
    /// 把夹具里**最快的那条**（节点1，15ms）关掉 ⇒ 应用必须选**慢的那条**（节点0，200ms），
    /// 而且 `probed` 从 2 掉到 1 —— 这一条同时钉住了"信息节点能不能被排除"的语义：
    /// 由用户自己勾，而不是我们在代码里猜哪个名字是"真节点"。
    #[tokio::test]
    async fn auto_export_skips_nodes_the_user_turned_off() {
        let yaml = concat!(
            "proxies:\n",
            "  - {name: 节点0, type: ss, server: 198.51.100.9, port: 443, cipher: aes-128-gcm, password: p}\n",
            "  - {name: 节点1, type: ss, server: 203.0.113.7, port: 443, cipher: aes-128-gcm, password: p}\n",
        );
        let airport = FakeAirport::start(vec![yaml.to_string()]);
        let state = fresh_state("auto-node-off").await;
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        doc.proxy_subscriptions.push(sub_cfg("sub-a", &airport.url("/sub")));
        doc.lanes.push(Lane::automatic("lane-auto", "自动", "sub-a"));
        state.config.save(&doc).await.expect("种配置");
        state.set_doc(doc);
        refresh_subscriptions(&state, Some(SubscriptionId::new("sub-a")), true)
            .await
            .expect("先拉一版订阅");

        // 关掉最快的那条（走**命令那条路**，不是直接改内存）
        let after =
            set_nodes_enabled(&state, &SubscriptionId::new("sub-a"), &["节点1".to_string()], false)
                .await
                .expect("关掉节点1");
        assert_eq!(after.len(), 2);
        assert_eq!(after.iter().find(|n| n.name == "节点1").map(|n| n.enabled), Some(false));
        assert_eq!(after.iter().find(|n| n.name == "节点0").map(|n| n.enabled), Some(true));

        let fake = FakeKernel::new(KernelState::Ready);
        install_kernel(&state, fake.clone());
        let out = apply_auto_export(
            &state,
            "p-a",
            &LaneId::new("lane-auto"),
            &SubscriptionId::new("sub-a"),
            true,
        )
        .await
        .expect("探一次");

        assert_eq!(out.probed, 1, "关掉的那条不进候选");
        assert_eq!(out.node.as_deref(), Some("节点0"), "只能从还开着的节点里选");
        assert!(fake.calls().contains(&"set_select:lane-auto:节点0".to_string()));

        // 再开回来 ⇒ 候选回到 2 条、又选回最快那条（可逆）
        set_nodes_enabled(&state, &SubscriptionId::new("sub-a"), &["节点1".to_string()], true)
            .await
            .expect("开回来");
        let out2 = apply_auto_export(
            &state,
            "p-a",
            &LaneId::new("lane-auto"),
            &SubscriptionId::new("sub-a"),
            true,
        )
        .await
        .expect("再探一次");
        assert_eq!(out2.probed, 2);
        assert_eq!(out2.node.as_deref(), Some("节点1"));

        // 空清单必须**报错**（不许被当成"全部"）
        assert!(
            set_nodes_enabled(&state, &SubscriptionId::new("sub-a"), &[], false).await.is_err(),
            "空清单不是「全部」"
        );
    }

    #[tokio::test]
    async fn auto_probe_target_uses_base_url_and_provider_key_override() {
        let mut doc = ConfigDoc::default();
        doc.providers.push(Provider::new("p-a", "A", Protocol::Openai, "https://a.example/v1"));
        let mut sub = sub_cfg("sub-a", "https://airport.example/sub");
        sub.provider_key = Some("airport-key".to_string());
        doc.proxy_subscriptions.push(sub);

        let (key, probe) =
            auto_probe_target(&doc, "p-a", &SubscriptionId::new("sub-a")).expect("坐标");
        assert_eq!(key, "airport-key", "内核上的 key 用 provider_key，不是订阅 id");
        assert_eq!(probe, "https://a.example/v1", "延迟的语义是'到这家服务商有多快'");

        // 服务商没配 base_url：按订阅自带 health url 兜底，再兜到内核默认端点。**绝不留空**。
        let mut blank = doc.clone();
        blank.providers[0].base_url = "  ".to_string();
        let (_, probe) = auto_probe_target(&blank, "p-a", &SubscriptionId::new("sub-a")).unwrap();
        assert_eq!(probe, ta_kernel::config_gen::DEFAULT_HEALTH_URL);
        blank.proxy_subscriptions[0].health_check.url = Some("https://probe.example/".to_string());
        let (_, probe) = auto_probe_target(&blank, "p-a", &SubscriptionId::new("sub-a")).unwrap();
        assert_eq!(probe, "https://probe.example/");

        // 服务商 / 订阅都要**点明缺哪个**，不猜。
        assert!(auto_probe_target(&doc, "p-nope", &SubscriptionId::new("sub-a")).is_err());
        assert!(auto_probe_target(&doc, "p-a", &SubscriptionId::new("sub-nope")).is_err());
    }
}

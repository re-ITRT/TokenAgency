//! `ta-kernel` — mihomo sidecar 的进程管理、配置生成、REST 控制、lane 端口分配。
//!
//! 契约：。**实现由工作包 P5 完成。**
//!
//! 这里**绝不**做任何系统级网络设置：不写系统代理、不改路由表、不装 TUN。

pub mod childlog;
pub mod config_gen;
pub mod ports;
pub mod process;
pub mod rest;

#[cfg(test)]
pub(crate) mod testutil;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use ta_core::error::Result;
use ta_core::types::LaneId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KernelState {
    Stopped,
    Starting,
    Ready,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStatus {
    pub state: KernelState,
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub last_error: Option<String>,
}

impl Default for KernelStatus {
    fn default() -> Self {
        Self {
            state: KernelState::Stopped,
            version: None,
            pid: None,
            restarts: 0,
            last_error: None,
        }
    }
}

/// `/proxies` + `/providers/proxies` 合成后的线路快照（UI 的线路下拉以此为准）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersSnapshot {
    /// `/proxies` 顶层：自有节点、我们生成的组、内核内置伪节点。
    pub proxies: Vec<ProxyEntry>,
    /// `/providers/proxies`：每个 provider 一项（含内核给每个组生成的镜射项）。
    pub providers: Vec<ProviderEntry>,
    /// ★ **节点索引**：`/proxies` 顶层 **∪** `/providers/proxies` 里各 provider 的节点。
    ///
    /// 这一层非有不可（7-c）：**provider 提供的节点不出现在 `/proxies` 顶层**，
    /// 却出现在组的 `all` 里。只用 `/proxies` 的键去解析 `all` 会得到悬空引用，
    /// UI 的线路下拉就会缺节点（或直接报错）。
    ///
    /// **同名时顶层优先**：真机上 `DIRECT` / `fake-hk-01` / `TA-LANE-*` 在两边都出现
    /// （provider 那份来自镜射项），以顶层为准，否则 `source` 会变成假的。
    pub nodes: std::collections::BTreeMap<String, NodeRef>,
    /// 组的 `all` 里引用了索引中**不存在**的名字（7-c）。
    ///
    /// **不静默丢**：内核对此零报错，所以这是我们唯一能把它变成"看得见的问题"的地方 ——
    /// 交给 UI 置灰/说明原因，而不是让用户对着一个少了节点的下拉发呆。
    pub dangling: Vec<DanglingRef>,
}

impl ProvidersSnapshot {
    /// 某个组的可选成员（**provider 节点已并入**）。组不存在 → `None`；
    /// 悬空的名字不在结果里（用 [`ProvidersSnapshot::dangling_in`] 单独取）。
    pub fn group_members(&self, group: &str) -> Option<Vec<&NodeRef>> {
        let entry = self.proxies.iter().find(|p| p.name == group)?;
        Some(entry.all.iter().filter_map(|name| self.nodes.get(name)).collect())
    }

    /// 可以作为线路目标的节点：排除内核内置伪节点（`DIRECT` 保留 —— 直连 lane 指它）。
    pub fn selectable(&self) -> Vec<&NodeRef> {
        self.nodes.values().filter(|n| n.is_lane_target()).collect()
    }

    /// 某个组里的悬空引用。
    pub fn dangling_in(&self, group: &str) -> Vec<&DanglingRef> {
        self.dangling.iter().filter(|d| d.group == group).collect()
    }
}

/// 一个节点是从哪来的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "provider_key")]
pub enum NodeSource {
    /// `/proxies` 顶层：自有节点、组、内置伪节点。
    TopLevel,
    /// `/providers/proxies` 里某个 provider 带的节点（7-c：**不在顶层**）。
    Provider(String),
}

/// 合并索引里的一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRef {
    pub name: String,
    /// 内核内部类型名**原样**（`Selector` / `Shadowsocks` / `Vmess` …）。
    pub kind: String,
    pub alive: bool,
    pub source: NodeSource,
}

impl NodeRef {
    /// 大小写无关的类型判断（7-b：内核给的是 `Selector` / `Shadowsocks`，不是 `select` / `ss`）。
    pub fn kind_is(&self, name: &str) -> bool {
        self.kind.eq_ignore_ascii_case(name)
    }

    /// 可达性只看 `alive` —— 见 [`ProxyEntry::is_reachable`] 的理由。
    pub fn is_reachable(&self) -> bool {
        self.alive
    }

    pub fn builtin(&self) -> Option<BuiltinProxy> {
        builtin_proxy(&self.name)
    }

    /// 能不能作为 lane / 线路下拉的目标。
    pub fn is_lane_target(&self) -> bool {
        match self.builtin() {
            Some(b) => b.is_selectable(),
            // 非内置：组与真实节点都可以
            None => true,
        }
    }
}

/// 组的 `all` 指向了一个索引里不存在的名字（7-c）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DanglingRef {
    pub group: String,
    pub name: String,
}

/// 内核内置的伪节点（7-d）。
///
/// 真机上它们**混在 `/proxies` 里**与用户节点一起返回；不分类就会被 UI 当用户节点显示。
/// 匹配按**名字**（内核保留这些名字），大小写无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinProxy {
    /// 直连。**唯一可以当 lane 目标的内建项** —— `direct` lane 就指它（K4 实测可绑定）。
    Direct,
    Reject,
    RejectDrop,
    Pass,
    PassRule,
    /// 内核默认的"组为空"落点（= 直连语义）。我们生成的每个组都显式写
    /// `empty-fallback: REJECT` 就是为了不落到这里（K3d）。
    Compatible,
    /// 内核的全局选择器。它也是 `Selector` 类型，但**不是**用户的线路。
    Global,
}

impl BuiltinProxy {
    pub fn name(self) -> &'static str {
        match self {
            BuiltinProxy::Direct => "DIRECT",
            BuiltinProxy::Reject => "REJECT",
            BuiltinProxy::RejectDrop => "REJECT-DROP",
            BuiltinProxy::Pass => "PASS",
            BuiltinProxy::PassRule => "PASS-RULE",
            BuiltinProxy::Compatible => "COMPATIBLE",
            BuiltinProxy::Global => "GLOBAL",
        }
    }

    /// 能不能给用户选（只有 `DIRECT`）。其余是内核内部实现：过滤或置灰，不给选。
    pub fn is_selectable(self) -> bool {
        matches!(self, BuiltinProxy::Direct)
    }
}

fn builtin_proxy(name: &str) -> Option<BuiltinProxy> {
    const ALL: [BuiltinProxy; 7] = [
        BuiltinProxy::Direct,
        BuiltinProxy::Reject,
        BuiltinProxy::RejectDrop,
        BuiltinProxy::Pass,
        BuiltinProxy::PassRule,
        BuiltinProxy::Compatible,
        BuiltinProxy::Global,
    ];
    ALL.into_iter().find(|b| name.eq_ignore_ascii_case(b.name()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEntry {
    pub name: String,
    /// 内核的**内部首字母大写**类型名，**原样保留**：`Selector` / `Shadowsocks` / `Vmess` …
    ///
    /// **刻意不做枚举归一**（7-b）：未知/未来类型必须原样留下，不能丢。
    /// 要判"是不是组"用 [`ProxyEntry::is_group`]（大小写无关），要判内建用
    /// [`ProxyEntry::builtin`]。
    pub kind: String,
    /// ★ 内核给的健康信号（附带验证：每个节点与组都有）。
    pub alive: bool,
    pub now: Option<String>,
    /// 组的可选成员（**未解析**：里面可能有只有 provider 才知道的名字，
    /// 解析请走 [`ProvidersSnapshot::group_members`]）。
    pub all: Vec<String>,
    pub history: Vec<DelaySample>,
}

impl ProxyEntry {
    /// 大小写无关的类型判断（7-b）。
    pub fn kind_is(&self, name: &str) -> bool {
        self.kind.eq_ignore_ascii_case(name)
    }

    /// 是不是组（而不是一个真实节点）。
    ///
    /// `Selector` 是实测到的（7-b）；其余三个是内核文档里的组类型名（`url-test` /
    /// `fallback` / `load-balance` 的内部名），**属推断**，但判错只是"显示分类不准"。
    pub fn is_group(&self) -> bool {
        ["Selector", "URLTest", "Fallback", "LoadBalance", "Relay"].iter().any(|k| self.kind_is(k))
    }

    pub fn builtin(&self) -> Option<BuiltinProxy> {
        builtin_proxy(&self.name)
    }

    /// 能不能作为 lane / 线路下拉的目标。
    pub fn is_lane_target(&self) -> bool {
        match self.builtin() {
            Some(b) => b.is_selectable(),
            None => true,
        }
    }

    /// 可达性。判据是 **`alive`**。
    ///
    /// 实测：不可达时内核给 `alive=false` **且**最后一条 `history[].delay == 0`；
    /// 但 **`delay == 0` 单独不可靠**（没测过的节点 `history` 为空，而"真 0ms"也不可能但别写死），
    /// 所以这里只信 `alive`，`delay` 只用来显示具体延迟。
    pub fn is_reachable(&self) -> bool {
        self.alive
    }

    /// 最后一条测速延迟（**毫秒**）。`None` = 内核还没测过（**不等于不通**）。
    pub fn latest_delay(&self) -> Option<u32> {
        self.history.last().map(|h| h.delay)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelaySample {
    pub time: String,
    pub delay: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// **key 才是 provider 的身份**（我们生成的 `sub-jp` 这种）；`name` 是给人看的机场名。
    /// 就绪判定必须按 key 查（见 `process::KernelSupervisor::verify_providers`）。
    pub key: String,
    pub name: String,
    /// `File` / `HTTP` / `Compatible` …
    pub vehicle_type: String,
    /// 语义按 `vehicle_type` 区分，见 [`ProviderEntry::updated_at_is_file_mtime`]。
    pub updated_at: Option<String>,
    pub node_count: usize,
    /// ★ provider 提供的节点（7-c：它们**不在 `/proxies` 顶层**，但会出现在组的 `all` 里）。
    pub nodes: Vec<NodeRef>,
}

impl ProviderEntry {
    /// `updatedAt` 是**文件 mtime**（而不是拉取时刻）。
    ///
    /// 实测：`vehicleType: File` 的 provider 报的 `updatedAt`
    /// `23:42:34` **早于**同一次 spike 里内核的启动时刻 `23:53:42` —— 所以它是文件的 mtime。
    /// 远程 provider 才是拉取时刻。UI 说"最后更新时间"时必须按这个区分。
    pub fn updated_at_is_file_mtime(&self) -> bool {
        self.vehicle_type.eq_ignore_ascii_case("file")
    }

    /// 是不是**组的镜射项**（`vehicleType: Compatible`）。
    ///
    /// 实测：内核会给**我们生成的每个组**再报一个 Compatible provider
    /// （`TA-LANE-direct` / `TA-LANE-mixed` / `default`，见 `k8.log:11-13`）。
    /// 它**不是用户订阅** —— 就绪判定不能拿它当"订阅加载成功"的证据，
    /// 也不能要求"每个 provider 都非空"（组为空时它会空 → 假阴性）。
    pub fn is_group_mirror(&self) -> bool {
        self.vehicle_type.eq_ignore_ascii_case("compatible")
    }
}

#[derive(Debug, Clone, Default)]
pub struct PortMap {
    pub map: std::collections::BTreeMap<LaneId, u16>,
}

#[async_trait]
pub trait KernelController: Send + Sync {
    async fn status(&self) -> KernelStatus;
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn restart(&self) -> Result<()>;
    /// 生成 + 落盘 + reload（需要重启时自动重启）
    async fn apply(&self) -> Result<KernelStatus>;
    /// `PUT /proxies/{group}` —— 组内切节点
    async fn set_node(&self, group: &str, node: &str) -> Result<()>;
    /// 把某条 lane 对应的组切到指定节点（`select` 组）—— 上层只认 lane，
    /// 「lane → 内核组名」的映射是内核层的细节（见  的线路下拉/托盘切换）。
    async fn set_select(&self, lane: &LaneId, node: &str) -> Result<()>;
    /// `GET /proxies/{name}/delay`
    async fn delay_of(&self, name: &str, timeout_ms: u64) -> Result<u32>;
    /// **provider 里节点的延迟只能这样测**（实测）：先
    /// `GET /providers/proxies/{key}/healthcheck?url=<probe>` 触发整份订阅体检，
    /// 再读 `GET /providers/proxies/{key}` 里每个节点的 `history[].delay`。
    ///
    /// 为什么不用 [`KernelController::delay_of`]：proxy-provider 的节点**不在 `/proxies` 顶层**
    /// （顶层只有内核内置 8 项 + 我们建的组），`/proxies/<name>/delay` 与三种带 provider
    /// 前缀的写法**全部 404** —— 这就是「自动」出口一直 `all_unreachable` 的原因。
    ///
    /// 返回：节点名 → 实测延迟（`None` = 不通 / 没测出来）。**绝不编造一个延迟**。
    async fn provider_delays(
        &self,
        key: &str,
        probe_url: &str,
        wait_ms: u64,
    ) -> Result<std::collections::BTreeMap<String, Option<u32>>>;
    /// `PUT /providers/proxies/{key}` —— 刷新订阅
    async fn refresh_provider(&self, key: &str) -> Result<()>;
    async fn snapshot(&self) -> Result<ProvidersSnapshot>;
    async fn log_tail(&self, lines: u32) -> Result<String>;
}

pub trait KernelConfigGen: Send + Sync {
    /// 领域模型 → 生成的 YAML。
    ///
    /// **必须校验 `listeners[].proxy` 引用的组名/节点名存在** —— 但原因**不是**"否则内核会启动
    /// 失败"（那句话已被实测证伪）：K4(d) 实测 mihomo v1.19.31 **根本不校验**，
    /// 指向不存在的名字也照样绑定成功、启动零报错，只在**请求到达时**才在它自己的日志里报
    /// `[Metadata] parse failed: proxy … not found`（客户端 502）。
    /// 所以**生成期的引用校验是我们唯一的防线**（K4d， 的更正）。
    fn generate(&self, doc: &ta_core::domain::ConfigDoc, ports: &PortMap) -> Result<String>;
}

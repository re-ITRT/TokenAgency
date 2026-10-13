//! 路由索引：把 `ConfigDoc` 编译成路由热路径真正需要的形式。
//!
//! 为什么要单独一层：
//! - `decide` / `select` 每次请求都会跑，不能每次都线性扫 `ConfigDoc` 的所有字段；
//! - 规则表必须**稳定有序**（priority 升序 + 配置内原顺序），否则"首个命中"不可复现；
//! - `RoutingIndex` 是不可变快照：配置换了就重建一个，路由过程中永远读一致视图。

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::*;
use crate::types::*;

/// 配置的**只读路由视图**。
#[derive(Debug, Clone, Default)]
pub struct RoutingIndex {
    /// 全部通道（含未启用的；未启用的会在 decide 里带 `Disabled` 原因进 eliminated）
    pub channels: BTreeMap<ChannelId, ChannelConfig>,
    /// 配置里通道出现的顺序（稳定排序的兜底键）
    pub channel_order: Vec<ChannelId>,
    /// 逻辑模型 → 该模型下的通道（配置顺序）
    pub channels_of_model: BTreeMap<String, Vec<ChannelId>>,
    /// 声明过的逻辑模型
    pub models: BTreeMap<String, Model>,
    /// 别名 / 同名 → 逻辑模型 id
    pub model_alias: BTreeMap<String, String>,
    /// 策略：只保留 `enabled`，按 `priority` 升序（同 priority 保持配置顺序）
    pub policies: Vec<Policy>,
    /// 通道组
    pub groups: BTreeMap<ChannelGroupId, ChannelGroup>,
    /// 逻辑模型 → 默认通道组（同名多组时取配置里第一个）
    pub group_of_model: BTreeMap<String, ChannelGroupId>,
    pub providers: BTreeMap<ProviderId, Provider>,
    pub credentials: BTreeMap<CredentialId, Credential>,
    pub lanes: BTreeMap<LaneId, Lane>,
    /// `net.default_lane`：lane 继承链的最后一环
    pub default_lane: Option<LaneId>,
    /// `gateway.retry`：内置 failover 默认值的来源
    pub retry: RetryConfig,
}

impl RoutingIndex {
    pub fn build(doc: &ConfigDoc) -> Self {
        let mut idx = RoutingIndex {
            default_lane: doc.net.default_lane.clone(),
            retry: doc.gateway.retry.clone(),
            ..Default::default()
        };

        for m in &doc.models {
            idx.models.insert(m.id.clone(), m.clone());
            idx.model_alias.insert(m.id.clone(), m.id.clone());
            for a in &m.aliases {
                // 别名不覆盖已声明的逻辑模型名
                if !idx.models.contains_key(a) {
                    idx.model_alias.insert(a.clone(), m.id.clone());
                }
            }
        }

        let mut seen: BTreeSet<ChannelId> = BTreeSet::new();
        for c in &doc.channels {
            if seen.insert(c.id.clone()) {
                idx.channel_order.push(c.id.clone());
            }
            idx.channels.insert(c.id.clone(), c.clone());
        }

        // `channels_of_model` 的**唯一真源是 `Model.lines`**（模型侧持有关系，
        // 一条线路可以服务多个模型）：
        //
        // * 键用**模型的规范 id**（而不是线路自己写的那串名字）—— 这也顺手修掉一个既有缺陷：
        //   老实现按 `channel.model` 的**原文**建索引、却按**规范 id** 查（`engine.rs`），
        //   于是"线路写别名"的配置会查到一个空候选集；
        // * **遗留字段兜底**：`ChannelConfig.model`（v1 老配置，迁移后通常已清空/已搬进 lines）
        //   仍被认一次，这样从旧导出直接导入的文件也不会瞬间没有线路可用。
        for m in &doc.models {
            for line in &m.lines {
                let bucket = idx.channels_of_model.entry(m.id.clone()).or_default();
                if !bucket.contains(line) {
                    bucket.push(line.clone());
                }
            }
        }
        for c in &doc.channels {
            if c.model.trim().is_empty() {
                continue;
            }
            // 老写法：解析到模型（id 或别名）后挂到**规范 id** 下；解析不到就按原文留着，
            // 让校验器去报"引用了不存在的模型"。
            let key = idx.model_alias.get(&c.model).cloned().unwrap_or_else(|| c.model.clone());
            let bucket = idx.channels_of_model.entry(key).or_default();
            if !bucket.contains(&c.id) {
                bucket.push(c.id.clone());
            }
        }

        for p in &doc.providers {
            idx.providers.insert(p.id.clone(), p.clone());
        }
        for c in &doc.credentials {
            idx.credentials.insert(c.id.clone(), c.clone());
        }
        for l in &doc.lanes {
            idx.lanes.insert(l.id.clone(), l.clone());
        }
        for g in &doc.channel_groups {
            idx.group_of_model.entry(g.model.clone()).or_insert_with(|| g.id.clone());
            idx.groups.insert(g.id.clone(), g.clone());
        }

        // 规则表：enabled + priority 升序；`sort_by_key` 是稳定排序 → 同 priority 保持配置顺序
        idx.policies = doc.policies.iter().filter(|p| p.enabled).cloned().collect();
        idx.policies.sort_by_key(|p| p.priority);

        idx
    }

    /// 客户端 model 名（可能是别名）→ 逻辑模型 id。
    pub fn canonical_model(&self, raw: &str) -> String {
        self.model_alias.get(raw).cloned().unwrap_or_else(|| raw.to_string())
    }

    pub fn channel(&self, id: &ChannelId) -> Option<&ChannelConfig> {
        self.channels.get(id)
    }

    pub fn group(&self, id: &ChannelGroupId) -> Option<&ChannelGroup> {
        self.groups.get(id)
    }

    pub fn group_for_model(&self, model: &str) -> Option<&ChannelGroup> {
        self.group_of_model.get(model).and_then(|g| self.groups.get(g))
    }

    /// 该逻辑模型在配置层面的全部通道（**含未启用**，配置顺序）。
    pub fn all_channels_of_model(&self, model: &str) -> Vec<ChannelId> {
        self.channels_of_model.get(model).cloned().unwrap_or_default()
    }

    /// 单个通道的可用标签。
    pub fn tags_of(&self, id: &ChannelId) -> &[String] {
        self.channels.get(id).map(|c| c.tags.as_slice()).unwrap_or(&[])
    }

    /// 该通道的生效 lane：`provider.lane → net.default_lane`。
    ///
    ///（第 322 轮）收敛：用户裁决"线路级别的分配逻辑我们已经放弃了，
    /// 取而代之的，**每个服务商统一一个节点**"。所以这里只看**这条通道所属的服务商**，
    /// 通道自己与凭据上的覆盖字段已经整条删除。
    ///
    /// 这里曾按 `channel.lane → credential.lane → provider.lane → default` 逐级回退 ——
    /// 那两级在界面上早已没有写入口，却仍能静默压过服务商级选择。保留坏处大于删掉。
    pub fn resolve_lane(&self, ch: &ChannelConfig) -> Option<LaneId> {
        self.providers.get(&ch.provider).and_then(|p| p.lane.clone()).or_else(|| {
            // 服务商不存在（配置有洞）或它自己没选 → 全局默认
            self.default_lane.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    const YAML: &str = r#"
version: 1
net:
  default_lane: lane-direct
lanes:
  - { id: lane-direct, name: 直连, kind: direct }
  - { id: lane-jp, name: 日本, kind: subscription_auto }
providers:
  - { id: p-a, name: A, protocol: openai, base_url: "https://a.example/v1", lane: lane-jp }
credentials:
  # 注意：这两条 `lane:` 是**老配置的残留**（[docs/55] 已删除该字段）。
  # 故意留着：它证明"老配置仍能解析、且那个值不再参与任何判定"。
  - id: c-a
    provider: p-a
    label: A
    secret: "${secret:a}"
    lane: lane-jp
  - id: c-b
    provider: p-a
    label: B
    secret: "${secret:b}"
channels:
  - id: ch-1
    model: gpt-4o
    provider: p-a
    credential: c-a
    upstream_model: gpt-4o
    enabled: true
  - id: ch-2
    model: gpt-4o
    provider: p-a
    credential: c-b
    upstream_model: gpt-4o
    enabled: true
  - id: ch-3
    model: claude-sonnet-4-5
    provider: p-a
    credential: c-b
    upstream_model: claude-sonnet-4-5
    enabled: true
channel_groups:
  - id: grp-gpt-4o
    model: gpt-4o
    members: [ch-1, ch-2]
models:
  - id: gpt-4o
    aliases: [gpt-4o-latest, openai/gpt-4o]
policies:
  - id: pol-late
    name: late
    priority: 100
    match: { all: true }
    action: {}
  - { id: pol-off, name: off, enabled: false, priority: 1, match: { all: true }, action: {} }
  - { id: pol-early, name: early, priority: 10, match: { all: true }, action: {} }
  - { id: pol-early2, name: early2, priority: 10, match: { all: true }, action: {} }
"#;

    #[test]
    fn index_orders_policies_and_resolves_aliases() {
        let doc = config::from_yaml(YAML).expect("yaml");
        let idx = RoutingIndex::build(&doc);

        // 只留 enabled，priority 升序，同 priority 保持配置顺序
        let ids: Vec<&str> = idx.policies.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["pol-early", "pol-early2", "pol-late"]);

        assert_eq!(idx.canonical_model("gpt-4o-latest"), "gpt-4o");
        assert_eq!(idx.canonical_model("openai/gpt-4o"), "gpt-4o");
        assert_eq!(idx.canonical_model("gpt-4o"), "gpt-4o");
        assert_eq!(idx.canonical_model("unknown-model"), "unknown-model");

        assert_eq!(idx.all_channels_of_model("gpt-4o").len(), 2);
        assert_eq!(idx.group_for_model("gpt-4o").unwrap().id.as_str(), "grp-gpt-4o");
        assert!(idx.group_for_model("claude-sonnet-4-5").is_none());

        // 生效线路只看服务商那一处。这个 fixture 里 ch-2 的 provider 配的是 lane-jp
        //（老配置里 credential 上也有 lane，但那个字段已经不参与判定了）
        let ch2 = idx.channel(&ChannelId::from("ch-2")).unwrap();
        assert_eq!(idx.resolve_lane(ch2).unwrap().as_str(), "lane-jp");
    }

    #[test]
    fn index_keeps_disabled_channels_for_explainability() {
        let yaml = YAML.replace("enabled: true", "enabled: false");
        let doc = config::from_yaml(&yaml).expect("yaml");
        let idx = RoutingIndex::build(&doc);
        assert_eq!(idx.channel_order.len(), 3);
        assert!(idx.channel_order.contains(&ChannelId::from("ch-1")));
    }
}

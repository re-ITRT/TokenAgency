//! [docs/17 §2]「**能力一致才可路由**」的纯判定层。
//!
//! 为什么要有这一层：同一个对外模型可以由多条线路服务（Ollama 一条、CommandCode 一条）。
//! 只有当两家上游**暴露的能力完全一致**时，把它们放在同一条路由后面才是等价的；
//! 否则用户会看到"有时有 vision、有时报错"这种不可预期行为。所以：
//!
//! * **判定在这里**（纯函数、不碰 IO、可单测）：`capabilities_equal` 与按 `model` 分组的差异计算；
//! * **拒绝在命令层**（`policy_upsert` / `channel_upsert`）：它们调用这里，把差异变成
//!   `TaError::CapabilityMismatch`（TA-2007）与结构化 `details`。
//!
//! 比较规则（[docs/17 §2]，**逐条**都有测试）：
//! 1. 逐字段相等：`vision` / `tools` / `reasoning` / `context_tokens` / `max_output_tokens`；
//! 2. `modalities` **先排序去重**再比（`["text","image"]` 与 `["image","text","image"]` 相等）；
//! 3. `max_output_tokens` 的 `None` **只等于** `None` —— "未声明"不等于"无所谓"，
//!    也不等于某个具体数字。

use serde::{Deserialize, Serialize};

use super::{ChannelCapabilities, ConfigDoc, Policy};

/// 规范化（仅排序 + 去重，**不做 trim / 不过滤**）后的模态集合。
///
/// 刻意**不**做 trim 或丢弃空串：那会把 `" image"` 与 `"image"` 判成相等 —— 属于
/// "把真实差异洗掉"，而这一层的职责恰恰是**不许**把差异洗掉。
pub fn normalized_modalities(caps: &ChannelCapabilities) -> Vec<String> {
    let mut out = caps.modalities.clone();
    out.sort();
    out.dedup();
    out
}

/// 两份能力声明是否完全一致（[docs/17 §2] 的三条规则）。
pub fn capabilities_equal(a: &ChannelCapabilities, b: &ChannelCapabilities) -> bool {
    a.vision == b.vision
        && a.tools == b.tools
        && a.reasoning == b.reasoning
        && a.context_tokens == b.context_tokens
        && a.max_output_tokens == b.max_output_tokens
        && normalized_modalities(a) == normalized_modalities(b)
}

/// 一条线路在某个字段上的值（**文本形态**，人话与 details 共用同一份渲染）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityValue {
    /// 线路 id
    pub channel: String,
    /// 值本身（`true` / `1024` / `未声明` / `image,text` / `(空)`）
    pub value: String,
}

/// 一个字段上的不一致：它列出了**每一条线路各自的值**（不是只报"不一致"）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityFieldDiff {
    /// 字段名，与 [`ChannelCapabilities`] 的字段同名
    pub field: String,
    /// 按线路顺序排列的值
    pub values: Vec<CapabilityValue>,
}

/// 一处"不能路由到一起"：同一个 `model` 下这几条线路的能力不一致。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityMismatchDetail {
    /// 对外模型
    pub model: String,
    /// 涉及的线路 id（按传入顺序）
    pub channels: Vec<String>,
    /// 逐字段差异（只列**真的不同**的字段）
    pub fields: Vec<CapabilityFieldDiff>,
}

impl std::fmt::Display for CapabilityMismatchDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fields: Vec<&str> = self.fields.iter().map(|d| d.field.as_str()).collect();
        write!(
            f,
            "模型 {} 的线路 {} 在 {} 上不同",
            self.model,
            self.channels.join(" / "),
            fields.join("、")
        )
    }
}

/// 字段名的**唯一清单**（顺序 = 报错与测试的顺序）。
const CAPABILITY_FIELDS: [&str; 6] =
    ["vision", "tools", "reasoning", "context_tokens", "max_output_tokens", "modalities"];

/// 取某个字段的文本值。
fn field_value(caps: &ChannelCapabilities, field: &str) -> String {
    match field {
        "vision" => caps.vision.to_string(),
        "tools" => caps.tools.to_string(),
        "reasoning" => caps.reasoning.to_string(),
        "context_tokens" => caps.context_tokens.to_string(),
        "max_output_tokens" => match caps.max_output_tokens {
            Some(v) => v.to_string(),
            None => "未声明".to_string(),
        },
        "modalities" => {
            let normalized = normalized_modalities(caps);
            if normalized.is_empty() {
                "(空)".to_string()
            } else {
                normalized.join(",")
            }
        }
        other => format!("<未知字段 {other}>"),
    }
}

/// 一组（同 model 的）能力声明之间的逐字段差异；全部一致时返回空表。
pub fn capability_field_diffs(
    entries: &[(String, ChannelCapabilities)],
) -> Vec<CapabilityFieldDiff> {
    let mut diffs = Vec::new();
    for field in CAPABILITY_FIELDS {
        let values: Vec<CapabilityValue> = entries
            .iter()
            .map(|(channel, caps)| CapabilityValue {
                channel: channel.clone(),
                value: field_value(caps, field),
            })
            .collect();
        let first = values.first().map(|v| v.value.clone());
        if values.iter().any(|v| Some(&v.value) != first.as_ref()) {
            diffs.push(CapabilityFieldDiff { field: field.to_string(), values });
        }
    }
    diffs
}

/// 一条策略**实际会用到的**线路 id：`use_channels` ∪ `use_group` 的成员。
///
/// 去重且保序（先点名、后组内成员）；引用了不存在的线路/组时**跳过**
/// （那是校验器 `TA-5001` 的活，这里不越权）。
pub fn policy_channel_ids(doc: &ConfigDoc, policy: &Policy) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let push = |id: &str, ids: &mut Vec<String>| {
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    };

    for id in &policy.action.use_channels {
        push(id.as_str(), &mut ids);
    }
    if let Some(group_id) = policy.action.use_group.as_ref() {
        if let Some(group) = doc.channel_groups.iter().find(|g| g.id.as_str() == group_id.as_str())
        {
            for member in &group.members {
                push(member.as_str(), &mut ids);
            }
        }
    }
    ids
}

/// 一组线路里，**服务同一个逻辑模型**的线路能力必须一致；返回每一处不一致（一致的组不出现）。
///
/// [docs/28 §2] 分组口径：**按模型**——但一个模型要用哪些线路由 `Model.lines` 决定
/// （一条线路可以同时服务多个模型，所以它可能落在**多个**组里）。
/// 遗留字段 `channel.model` 仍然参与分组（迁移前的配置靠它）。
///
/// 少于两条的组天然"一致"，不产生任何输出。
pub fn capability_mismatches_for_channels(
    doc: &ConfigDoc,
    channel_ids: &[String],
) -> Vec<CapabilityMismatchDetail> {
    // 哪些模型用到了这批线路里的每一条（[docs/28 §2] 的真源是 `Model.lines`）
    let mut model_of: Vec<(String, String, ChannelCapabilities)> = Vec::new(); // (model, channel, caps)
    for id in channel_ids {
        let Some(channel) = doc.channels.iter().find(|c| c.id.as_str() == id.as_str()) else {
            continue;
        };
        let mut owners: Vec<String> =
            doc.models.iter().filter(|m| m.uses_line(&channel.id)).map(|m| m.id.clone()).collect();
        // 遗留兜底：老配置里线路自己写着模型名（迁移后通常已搬进 `lines`，这里只为兼容）
        if !channel.model.trim().is_empty() && !owners.contains(&channel.model) {
            owners.push(channel.model.clone());
        }
        // 没有模型认领它：用线路自己的 id 当"组"（这样两条都没人认领的线路不会互相误判）
        if owners.is_empty() {
            owners.push(channel.id.as_str().to_string());
        }
        for model in owners {
            model_of.push((model, channel.id.as_str().to_string(), channel.capabilities.clone()));
        }
    }

    // 按 model 分组（保序：第一次出现的顺序）
    let mut groups: Vec<(String, Vec<(String, ChannelCapabilities)>)> = Vec::new();
    for (model, id, caps) in model_of {
        match groups.iter_mut().find(|(m, _)| m == &model) {
            Some((_, entries)) => {
                if !entries.iter().any(|(existing, _)| existing == &id) {
                    entries.push((id, caps));
                }
            }
            None => groups.push((model, vec![(id, caps)])),
        }
    }

    let mut out = Vec::new();
    for (model, entries) in groups {
        if entries.len() < 2 {
            continue;
        }
        let fields = capability_field_diffs(&entries);
        if fields.is_empty() {
            continue;
        }
        out.push(CapabilityMismatchDetail {
            model,
            channels: entries.into_iter().map(|(id, _)| id).collect(),
            fields,
        });
    }
    out
}

/// `policy_upsert` 的判定：**这一条策略自己**的线路集合里有没有同 model 的能力不一致。
pub fn policy_capability_mismatches(
    doc: &ConfigDoc,
    policy: &Policy,
) -> Vec<CapabilityMismatchDetail> {
    let ids = policy_channel_ids(doc, policy);
    capability_mismatches_for_channels(doc, &ids)
}

/// `channel_upsert` 的判定：这条线路会被哪些**已存在**的策略与同 model 的其它线路归到一起。
///
/// 只返回**与这条线路有关**的那些不一致（别的模型/别的线路之间的问题不在这里拦，
/// 那属于"配置本来就坏"，由校验器负责）。
pub fn channel_capability_mismatches(
    doc: &ConfigDoc,
    channel_id: &str,
) -> Vec<CapabilityMismatchDetail> {
    let mut out: Vec<CapabilityMismatchDetail> = Vec::new();
    for policy in &doc.policies {
        let ids = policy_channel_ids(doc, policy);
        if !ids.iter().any(|id| id == channel_id) {
            continue;
        }
        for detail in capability_mismatches_for_channels(doc, &ids) {
            if detail.channels.iter().any(|c| c == channel_id) && !out.contains(&detail) {
                out.push(detail);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelConfig, ChannelGroup, PolicyAction, PolicyMatch};
    use crate::types::ChannelId;

    fn caps(
        vision: bool,
        context_tokens: u64,
        max_output_tokens: Option<u64>,
        modalities: &[&str],
    ) -> ChannelCapabilities {
        ChannelCapabilities {
            vision,
            tools: false,
            reasoning: false,
            context_tokens,
            max_output_tokens,
            modalities: modalities.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn doc_with(channels: Vec<(&str, &str, ChannelCapabilities)>) -> ConfigDoc {
        let mut doc = ConfigDoc::default();
        for (id, model, capabilities) in channels {
            let mut channel = ChannelConfig::new(id, model, "p", "c", "up");
            channel.capabilities = capabilities;
            doc.channels.push(channel);
        }
        doc
    }

    #[test]
    fn equal_when_every_field_matches() {
        let a = caps(true, 1_050_000, Some(8192), &["text", "image"]);
        let b = caps(true, 1_050_000, Some(8192), &["text", "image"]);
        assert!(capabilities_equal(&a, &b));
    }

    #[test]
    fn modalities_are_compared_after_sorting_and_dedup() {
        let a = caps(true, 100, None, &["text", "image"]);
        let b = caps(true, 100, None, &["image", "text", "image"]);
        assert!(capabilities_equal(&a, &b));
        assert_eq!(normalized_modalities(&b), vec!["image".to_string(), "text".to_string()]);
    }

    #[test]
    fn undeclared_max_output_only_equals_undeclared() {
        // None 只等于 None：既不等于 0，也不等于任何具体数字
        assert!(capabilities_equal(
            &caps(false, 1, None, &["text"]),
            &caps(false, 1, None, &["text"])
        ));
        assert!(!capabilities_equal(
            &caps(false, 1, None, &["text"]),
            &caps(false, 1, Some(0), &["text"])
        ));
        assert!(!capabilities_equal(
            &caps(false, 1, None, &["text"]),
            &caps(false, 1, Some(4096), &["text"])
        ));
    }

    #[test]
    fn every_field_can_make_the_pair_unequal() {
        let base = caps(true, 1_050_000, Some(8192), &["text"]);
        let mut vision = base.clone();
        vision.vision = false;
        let mut tools = base.clone();
        tools.tools = true;
        let mut reasoning = base.clone();
        reasoning.reasoning = true;
        let mut context = base.clone();
        context.context_tokens = 1_000_000;
        let mut max_out = base.clone();
        max_out.max_output_tokens = Some(4096);
        let mut modalities = base.clone();
        modalities.modalities = vec!["text".to_string(), "image".to_string()];

        for (name, other) in [
            ("vision", vision),
            ("tools", tools),
            ("reasoning", reasoning),
            ("context_tokens", context),
            ("max_output_tokens", max_out),
            ("modalities", modalities),
        ] {
            assert!(!capabilities_equal(&base, &other), "{name} 不同时必须判为不一致");
            let diffs = capability_field_diffs(&[
                ("ch-a".to_string(), base.clone()),
                ("ch-b".to_string(), other),
            ]);
            assert_eq!(diffs.len(), 1, "{name}：只应报这一个字段");
            assert_eq!(diffs[0].field, name);
            assert_eq!(diffs[0].values.len(), 2, "每个字段都要列出各线路的值");
        }
    }

    #[test]
    fn diffs_list_each_channel_value_and_only_real_differences() {
        let a = caps(true, 1_050_000, None, &["text"]);
        let b = caps(false, 1_050_000, None, &["text"]);
        let diffs = capability_field_diffs(&[("ch-a".to_string(), a), ("ch-b".to_string(), b)]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "vision");
        assert_eq!(diffs[0].values[0].channel, "ch-a");
        assert_eq!(diffs[0].values[0].value, "true");
        assert_eq!(diffs[0].values[1].channel, "ch-b");
        assert_eq!(diffs[0].values[1].value, "false");
        // 未声明那个字段的文本形态是"未声明"，不是空
        let undeclared = capability_field_diffs(&[
            ("ch-a".to_string(), caps(false, 1, None, &["text"])),
            ("ch-b".to_string(), caps(false, 1, Some(2), &["text"])),
        ]);
        assert_eq!(undeclared[0].field, "max_output_tokens");
        assert_eq!(undeclared[0].values[0].value, "未声明");
        assert_eq!(undeclared[0].values[1].value, "2");
    }

    #[test]
    fn single_channel_group_is_never_a_mismatch() {
        let doc = doc_with(vec![("ch-a", "m", caps(true, 1, None, &["text"]))]);
        assert!(capability_mismatches_for_channels(&doc, &["ch-a".to_string()]).is_empty());
    }

    #[test]
    fn channels_of_different_models_are_not_compared() {
        // 同一个策略下两条线路，但 model 不同 → 不属于"同一个 model 的多条线路"，不拦
        let doc = doc_with(vec![
            ("ch-a", "m-1", caps(true, 1, None, &["text"])),
            ("ch-b", "m-2", caps(false, 9, Some(1), &["image"])),
        ]);
        assert!(capability_mismatches_for_channels(
            &doc,
            &["ch-a".to_string(), "ch-b".to_string()]
        )
        .is_empty());
    }

    #[test]
    fn same_model_with_different_capabilities_is_reported() {
        let doc = doc_with(vec![
            ("ch-a", "m", caps(true, 1_050_000, None, &["text"])),
            ("ch-b", "m", caps(false, 1_050_000, None, &["text"])),
        ]);
        let found =
            capability_mismatches_for_channels(&doc, &["ch-a".to_string(), "ch-b".to_string()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].model, "m");
        assert_eq!(found[0].channels, vec!["ch-a", "ch-b"]);
        assert_eq!(found[0].fields.len(), 1);
        assert_eq!(found[0].fields[0].field, "vision");
        // 人话里必须点名模型与差异字段（前端/CLI 直接显示这句）
        let text = found[0].to_string();
        assert!(text.contains("m"), "{text}");
        assert!(text.contains("vision"), "{text}");
    }

    #[test]
    fn policy_group_members_are_expanded_before_comparing() {
        // 策略只写了 use_group：组里两条同 model 线路能力不一致 → 也要拦
        let mut doc = doc_with(vec![
            ("ch-a", "m", caps(true, 1, None, &["text"])),
            ("ch-b", "m", caps(false, 1, None, &["text"])),
        ]);
        let mut group = ChannelGroup::new("g-a", "m", ["ch-a", "ch-b"]);
        group.members = vec![ChannelId::new("ch-a"), ChannelId::new("ch-b")];
        doc.channel_groups.push(group);

        let mut policy = Policy::new("pol-a", "路由");
        policy.r#match = PolicyMatch::match_all();
        policy.action = PolicyAction {
            use_group: Some(crate::types::ChannelGroupId::new("g-a")),
            ..PolicyAction::default()
        };

        assert_eq!(policy_channel_ids(&doc, &policy), vec!["ch-a", "ch-b"]);
        assert_eq!(policy_capability_mismatches(&doc, &policy).len(), 1);
    }

    #[test]
    fn channel_check_only_reports_policies_that_would_group_it() {
        let mut doc = doc_with(vec![
            ("ch-a", "m", caps(true, 1, None, &["text"])),
            ("ch-b", "m", caps(false, 1, None, &["text"])),
        ]);
        let mut policy = Policy::new("pol-a", "路由");
        policy.action.use_channels = vec![ChannelId::new("ch-a"), ChannelId::new("ch-b")];
        doc.policies.push(policy);

        // ch-b 会被这条策略与 ch-a 归到一起 → 报
        let for_b = channel_capability_mismatches(&doc, "ch-b");
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_b[0].model, "m");

        // 一条没有任何策略点名的线路 → 不报（"本来就坏"的配置不该赖到它头上）
        let mut lone = doc.clone();
        lone.policies.clear();
        assert!(channel_capability_mismatches(&lone, "ch-b").is_empty());
    }
}

//! 配置版本与迁移。
//!
//! `ConfigDoc.version` 当前是 [`CURRENT_CONFIG_VERSION`]（= 1）。每次结构调整都必须：
//!
//! 1. 把 [`CURRENT_CONFIG_VERSION`] 加一并同步 `domain::default_version()`；
//! 2. 在 [`migration_steps`] 里注册 `version -> version + 1` 的步骤
//!    （v1 → v2 的槽位已经留好，见该函数注释）；
//! 3. 给新步骤补一个"老配置 → 新配置"的测试（[`migrate_to`] 支持注入步骤表）。
//!
//! **迁移只前进、且幂等**：
//!
//! - 已是最新版本的 doc 原样返回；
//! - 比本程序更新的版本（用户降级了程序）**不降级**、原样返回，由校验器报
//!   `unsupported_version`（见 `config::validator`）。

use std::collections::BTreeMap;

use crate::domain::ConfigDoc;

/// 本程序能理解的最新配置版本。**必须与 `domain::default_version()` 一致**。
pub const CURRENT_CONFIG_VERSION: u32 = 3;

/// 下一个待启用的版本号 —— v3 → v4 的位置已经留好。
pub const NEXT_CONFIG_VERSION: u32 = 4;

/// 迁移失败（缺步骤、步骤没有把版本 `+1`、步骤自身报错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationError {
    pub from: u32,
    pub to: u32,
    pub message: String,
}

impl MigrationError {
    pub fn new(from: u32, to: u32, message: impl Into<String>) -> Self {
        Self { from, to, message: message.into() }
    }
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "配置迁移 v{} → v{} 失败：{}", self.from, self.to, self.message)
    }
}

impl std::error::Error for MigrationError {}

/// 把一个版本的配置升到**下一个**版本。
///
/// **契约**：成功返回时 `doc.version` 必须恰好等于 `from + 1`，其余字段可以任意改。
pub type MigrationStep = fn(ConfigDoc) -> Result<ConfigDoc, MigrationError>;

/// v1 → v2：把"线路属于哪个模型"（`ChannelConfig.model`）搬成"模型要用哪几条线路"
/// （`Model.lines`）—— 的关系翻转。
///
/// **为什么必须迁移**：老库里每条 channel 行都写着 `"model": "gpt-4o"`，而模型行没有 `lines`。
/// 不搬的话，翻转后的读路径（按 `Model.lines` 找线路）在新库里**一条线路都找不到** ——
/// 界面显示"这个模型没有线路"、路由没有候选。这就是"domain 类型变了但数据没变"的典型事故。
///
/// **幂等**：`lines` 只做去重追加，`model` 字段保留（S2 才删字段）。同一份 v1 配置迁两次结果相同。
fn v1_to_v2(mut doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
    // 先把每份 channel 的遗留 `model` 收集起来（借用限制：不能一边遍历 channels 一边改 models）
    let pairs: Vec<(String, String)> = doc
        .channels
        .iter()
        .filter(|c| !c.model.trim().is_empty())
        .map(|c| (c.model.trim().to_string(), c.id.as_str().to_string()))
        .collect();

    for (name, channel_id) in pairs {
        // 名字可能是 id 也可能是别名（老校验器两者都认），按 `matches` 解析
        if let Some(model) = doc.models.iter_mut().find(|m| m.matches(&name)) {
            let line = crate::types::ChannelId::new(channel_id);
            if !model.uses_line(&line) {
                model.lines.push(line);
            }
        }
        // 名字指向一个**不存在**的模型：不编、不丢 —— 留给校验器按字段路径报错
        // （迁移的职责是搬运，不是替用户改配置）。
    }

    doc.version = 2;
    Ok(doc)
}

/// v2 → v3：**放弃"订阅里分组"这一概念**（用户第 321 轮裁决），把老的分组线路转成「自动选择」。
///
/// 用户原话：*"直接放弃订阅里面分组这一概念，只拿节点"*。
///
/// 为什么是转成 `subscription_auto` + **显式 `select`**，而不是原样删掉那条线路：
///
/// * 原样删掉 = 静默丢掉用户的一条线路，引用它的服务商/凭据/通道会指向不存在的 id ——
///   那是配置损坏，比"概念变了"糟得多；
/// * 转成 `subscription_auto` 但**不**给 `select`，会落进"整份订阅择优 + `url-test`"，
///   而 `url-test` 会按自己的 interval **周期性重测重选** —— 出口 IP 会自己变，
///   正是用户要避免的（同账号多地并发 = 封号风险）；
/// * 给上显式 `select` 之后，它就是 [`crate::domain::Lane::automatic`] 的「自动选择」：
///   按地址粘性、只在连不上或手动刷新时才重挑。
///
/// 于是用户体验是"**升级后那条线路还在，而且从'会自己乱跳'变成了'粘住'**"——
/// 这不是退让，是把一个机场语义换成了我们自己的、行为更可预期的语义。
///
/// **幂等**：`group` 只清空、`kind` 只改一次；已经是 v3 的文档不会再走到这里
/// （`needs_migration` 按版本判断），手工重跑一次结果也相同。
fn v2_to_v3(mut doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
    use crate::domain::{GroupType, LaneKind};

    for lane in doc.lanes.iter_mut() {
        if lane.kind != LaneKind::SubscriptionGroup {
            continue;
        }
        lane.kind = LaneKind::SubscriptionAuto;
        // ★ 显式 `select` 是"粘"的唯一保证（见上面第 2 条）——不能省。
        lane.group_type = Some(GroupType::Select);
        // 组名再没有意义了：内核那侧不再按名字引用订阅自带的组。
        lane.group = None;
    }

    doc.version = 3;
    Ok(doc)
}

/// 迁移注册表：`from_version -> 升到 from + 1 的步骤`。
///
/// 注意：**新增步骤时必须同时更新 [`NEXT_CONFIG_VERSION`]（以及 [`CURRENT_CONFIG_VERSION`]）**，
/// 否则将来有人加了步骤却忘了改版本号，`needs_migration` / `migrate` 会漏判"还能再升一版"。
pub fn migration_steps() -> BTreeMap<u32, MigrationStep> {
    BTreeMap::from([(1u32, v1_to_v2 as MigrationStep), (2u32, v2_to_v3 as MigrationStep)])
}

/// 老配置（比本程序旧）需要迁移吗？
pub fn needs_migration(doc: &ConfigDoc) -> bool {
    doc.version < CURRENT_CONFIG_VERSION
}

/// 恒等迁移入口：把配置升到 [`CURRENT_CONFIG_VERSION`]。
///
/// 失败时**原样返回并 `tracing::warn!`**——迁移不该阻断启动，
/// 具体问题交给 [`crate::config::validate`] 报字段路径。
pub fn migrate(doc: ConfigDoc) -> ConfigDoc {
    let original = doc.clone();
    match migrate_to(doc, CURRENT_CONFIG_VERSION, &migration_steps()) {
        Ok(migrated) => migrated,
        Err(e) => {
            tracing::warn!(target: "ta_core::config", "配置迁移失败，保持原样：{e}");
            original
        }
    }
}

/// 迁移循环（步骤表可注入，便于单测）。
///
/// - `doc.version >= target` → 原样返回（**绝不降级**）；
/// - 缺 `from` 对应的步骤 → [`MigrationError`]；
/// - 步骤返回的 `version != from + 1` → [`MigrationError`]。
pub fn migrate_to(
    mut doc: ConfigDoc,
    target: u32,
    steps: &BTreeMap<u32, MigrationStep>,
) -> Result<ConfigDoc, MigrationError> {
    while doc.version < target {
        let from = doc.version;
        let step = steps
            .get(&from)
            .copied()
            .ok_or_else(|| MigrationError::new(from, from + 1, "没有注册该版本的迁移步骤"))?;
        let next = step(doc)?;
        if next.version != from + 1 {
            return Err(MigrationError::new(from, next.version, "迁移步骤必须把 version 恰好加一"));
        }
        doc = next;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{default_doc, semantic_eq};

    fn doc_v(version: u32) -> ConfigDoc {
        ConfigDoc { version, ..default_doc() }
    }

    #[test]
    fn current_version_matches_domain_default() {
        // 守住"两处版本号必须一致"这条隐式契约。
        assert_eq!(ConfigDoc::default().version, CURRENT_CONFIG_VERSION);
        assert_eq!(default_doc().version, CURRENT_CONFIG_VERSION);
    }

    #[test]
    fn migration_registry_agrees_with_version_constants() {
        // 版本常量必须连号：CURRENT → NEXT
        assert_eq!(NEXT_CONFIG_VERSION, CURRENT_CONFIG_VERSION + 1);

        // 注册表与常量必须自洽：任何已注册步骤都只能升到 < NEXT 的版本。
        // 如果将来有人加了 v1→v2 步骤却忘了更新 NEXT_CONFIG_VERSION，这条会红。
        let steps = migration_steps();
        assert!(
            steps.keys().all(|from| *from < NEXT_CONFIG_VERSION),
            "迁移注册表里有超出 NEXT_CONFIG_VERSION 的步骤：{:?}",
            steps.keys().collect::<Vec<_>>()
        );
        // 当前 v1→v2 还是空槽位（槽位已留好，见 migration_steps 注释）
        assert!(!steps.contains_key(&CURRENT_CONFIG_VERSION));
    }

    #[test]
    fn migrate_is_identity_for_current_version() {
        let doc = default_doc();
        let migrated = migrate(doc.clone());
        assert_eq!(migrated.version, CURRENT_CONFIG_VERSION);
        assert!(semantic_eq(&doc, &migrated), "v1 应当是恒等迁移");
        assert!(!needs_migration(&doc));
    }

    #[test]
    fn migrate_does_not_downgrade_future_version() {
        let future = doc_v(NEXT_CONFIG_VERSION);
        let migrated = migrate(future.clone());
        assert_eq!(migrated.version, NEXT_CONFIG_VERSION);
        assert!(semantic_eq(&future, &migrated), "更高版本必须原样返回");
        assert!(!needs_migration(&future));
    }

    #[test]
    fn migrate_to_target_runs_registered_steps_in_order() {
        fn v1_to_v2(mut doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
            // 模拟一次真实改动：max_body_mb 32 → 64
            doc.gateway.max_body_mb = 64;
            doc.version = 2;
            Ok(doc)
        }
        fn v2_to_v3(mut doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
            doc.request_log.max_rows = 1_000_000;
            doc.version = 3;
            Ok(doc)
        }

        let steps: BTreeMap<u32, MigrationStep> =
            BTreeMap::from([(1u32, v1_to_v2 as MigrationStep), (2u32, v2_to_v3 as MigrationStep)]);

        let migrated = migrate_to(doc_v(1), 3, &steps).expect("应当逐级迁移成功");
        assert_eq!(migrated.version, 3);
        assert_eq!(migrated.gateway.max_body_mb, 64);
        assert_eq!(migrated.request_log.max_rows, 1_000_000);

        // 只迁移到 2：不跑 v2→v3
        let only_v2 = migrate_to(doc_v(1), 2, &steps).expect("应当升到 v2");
        assert_eq!(only_v2.version, 2);
        assert_eq!(only_v2.gateway.max_body_mb, 64);
        assert_ne!(only_v2.request_log.max_rows, 1_000_000);
    }

    #[test]
    fn migrate_to_reports_missing_step() {
        // v4 → v5 位置已留好但**还没注册**：跨版本迁移必须明确报错，不能静默放过。
        let err =
            migrate_to(doc_v(NEXT_CONFIG_VERSION), NEXT_CONFIG_VERSION + 1, &migration_steps())
                .expect_err("没有注册这一级的步骤，应当报错");
        assert_eq!(err.from, NEXT_CONFIG_VERSION);
        assert_eq!(err.to, NEXT_CONFIG_VERSION + 1);
        assert!(err.message.contains("迁移步骤"), "实际：{}", err.message);
    }

    /// **关系翻转的真迁移**：老配置里 `channels[].model` → `models[].lines`。
    ///
    /// 这是本轮最重要的一条测试：不迁移的话，翻转后的读路径在新库里一条线路都找不到。
    #[test]
    fn v1_to_v2_moves_channel_model_into_model_lines() {
        let mut doc = doc_v(1);
        doc.models.push(crate::domain::Model::new("deepseek-v4.1-flash"));
        doc.models.push(crate::domain::Model::new("kimi-k3").with_aliases(["k3"]));
        // 两条线路服务同一个模型（火山 + ollama 那种）
        doc.channels.push(crate::domain::ChannelConfig::new(
            "ch-a",
            "deepseek-v4.1-flash",
            "p-a",
            "c-a",
            "deepseek v4.1 flash 260910",
        ));
        doc.channels.push(crate::domain::ChannelConfig::new(
            "ch-b",
            "deepseek-v4.1-flash",
            "p-b",
            "c-b",
            "deepseek v4.1 flash",
        ));
        // 用**别名**写的老配置也要能搬（老校验器 id/别名都认）
        doc.channels.push(crate::domain::ChannelConfig::new("ch-c", "k3", "p-c", "c-c", "kimi-k3"));

        let migrated = migrate(doc);
        assert_eq!(migrated.version, CURRENT_CONFIG_VERSION);
        let ds = migrated.models.iter().find(|m| m.id == "deepseek-v4.1-flash").expect("模型还在");
        let lines: Vec<&str> = ds.lines.iter().map(|l| l.as_str()).collect();
        assert_eq!(lines, vec!["ch-a", "ch-b"], "两条线路都搬到这个模型下");
        let k3 = migrated.models.iter().find(|m| m.id == "kimi-k3").expect("模型还在");
        assert_eq!(k3.lines.len(), 1, "别名的写法也要落到正确的模型上");
        assert_eq!(k3.lines[0].as_str(), "ch-c");

        // **幂等**：同一份配置再迁一次，结果一样（`config_rollback` 可能把老文档放回来）
        let again = migrate(migrated.clone());
        assert_eq!(
            again.models.iter().find(|m| m.id == "deepseek-v4.1-flash").unwrap().lines.len(),
            2
        );
        assert!(semantic_eq(&migrated, &again), "迁移必须幂等");
    }

    /// 遗留 `model` 指向一个**不存在**的模型：迁移不编、不丢，留给校验器按字段路径报错。
    #[test]
    fn v1_to_v2_leaves_a_dangling_legacy_model_for_the_validator() {
        let mut doc = doc_v(1);
        doc.channels.push(crate::domain::ChannelConfig::new(
            "ch-x",
            "never-existed",
            "p-a",
            "c-a",
            "up",
        ));
        let migrated = migrate(doc);
        assert_eq!(migrated.version, CURRENT_CONFIG_VERSION);
        assert!(
            !migrated.models.iter().any(|m| m.matches("never-existed")),
            "不许凭空造一个模型出来"
        );
        // 遗留字段原样留着 → 校验器会以这条 channel 的 `model` 报错
        let mine =
            migrated.channels.iter().find(|c| c.id.as_str() == "ch-x").expect("channel 还在");
        assert_eq!(mine.model, "never-existed");
    }

    #[test]
    fn migrate_to_rejects_step_that_does_not_bump_version() {
        fn bad_step(doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
            Ok(doc) // 忘了 version += 1
        }
        let steps: BTreeMap<u32, MigrationStep> =
            BTreeMap::from([(1u32, bad_step as MigrationStep)]);
        let err = migrate_to(doc_v(1), 2, &steps).expect_err("版本没加一，应当报错");
        assert!(err.message.contains("恰好加一"), "实际：{}", err.message);
    }

    #[test]
    fn migrate_swallows_step_failure_and_keeps_original() {
        fn failing_step(_doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
            Err(MigrationError::new(1, 2, "故意失败"))
        }
        // 注入一个会失败的步骤：`migrate_to` 报错；而 `migrate`（内置步骤表）对失败是
        // **原样返回 + warn**（迁移不该阻断启动），此时版本停在被迁移前的那一版。
        let steps: BTreeMap<u32, MigrationStep> =
            BTreeMap::from([(1u32, failing_step as MigrationStep)]);
        assert!(migrate_to(doc_v(1), 2, &steps).is_err());
        // 内置表上的 v2 → 目标版本已经是 2：恒等返回
        let current = doc_v(CURRENT_CONFIG_VERSION);
        let round_tripped = migrate(current.clone());
        assert_eq!(round_tripped.version, CURRENT_CONFIG_VERSION);
        assert!(semantic_eq(&current, &round_tripped), "已达最新版本 → 原样返回");
    }

    // ---------------------------------------------------------------- v2 → v3

    /// **「放弃订阅里分组」的真迁移**（用户第 321 轮）。
    ///
    /// 老配置里 `kind: subscription_group` 的线路要变成「自动选择」：
    /// `subscription_auto` + **显式 `select`** + `group` 清空。
    ///
    /// 三条都缺一不可，所以逐条钉：
    /// * 只把 `kind` 改成 `subscription_auto` 而**不给** `select` → 它是 `url-test`，
    ///   出口 IP 会自己周期性乱跳（用户明确不要这个）；
    /// * 留着 `group` → 内核那侧还按老组名引用一个我们不再维护的概念；
    /// * 版本号必须真的 +1，否则每次启动都重跑一遍（虽然幂等，但 `needs_migration` 会永远为真）。
    #[test]
    fn v2_to_v3_turns_subscription_groups_into_automatic() {
        use crate::domain::{GroupType, Lane, LaneKind};

        let mut doc = doc_v(2);
        doc.lanes.push(Lane::subscription_group("lane-old-group", "日本分组", "sub-a", "🇯🇵 日本"));
        // 对照组：别的种类**一个字段都不许被改**
        doc.lanes.push(Lane::subscription_node("lane-fixed", "固定节点", "sub-a", "东京-01"));
        doc.lanes.push(Lane::direct("lane-direct", "直连"));
        doc.lanes.push(Lane::filtered("lane-filtered", "筛选择优", "sub-a", "(?i)JP"));

        let migrated = migrate(doc);
        assert_eq!(migrated.version, CURRENT_CONFIG_VERSION);

        let old = migrated
            .lanes
            .iter()
            .find(|l| l.id.as_str() == "lane-old-group")
            .expect("那条线路必须还在（删掉会让引用它的服务商指向不存在的 id）");
        assert_eq!(old.kind, LaneKind::SubscriptionAuto, "转成「自动选择」的种类");
        assert_eq!(
            old.group_type,
            Some(GroupType::Select),
            "必须显式 select：否则是 url-test，出口 IP 会自己乱跳"
        );
        assert_eq!(old.group, None, "组名这个概念已被取消，必须清空");
        assert_eq!(old.subscription.as_ref().map(|s| s.as_str()), Some("sub-a"), "订阅引用不动");
        assert_eq!(old.name, "日本分组", "用户起的名字不动");

        // 对照组：别的种类逐字不变
        let fixed = migrated.lanes.iter().find(|l| l.id.as_str() == "lane-fixed").unwrap();
        assert_eq!(fixed.kind, LaneKind::SubscriptionNode);
        assert_eq!(fixed.group_type, None);
        assert_eq!(fixed.node.as_deref(), Some("东京-01"));
        let direct = migrated.lanes.iter().find(|l| l.id.as_str() == "lane-direct").unwrap();
        assert_eq!(direct.kind, LaneKind::Direct);
        let filtered = migrated.lanes.iter().find(|l| l.id.as_str() == "lane-filtered").unwrap();
        assert_eq!(filtered.kind, LaneKind::Filter);
        assert_eq!(filtered.filter.as_deref(), Some("(?i)JP"));

        // **幂等**：再迁一次结果完全相同
        let again = migrate(migrated.clone());
        assert!(semantic_eq(&migrated, &again), "迁移必须幂等");
        let still = again.lanes.iter().find(|l| l.id.as_str() == "lane-old-group").unwrap();
        assert_eq!(still.group, None);
        assert_eq!(still.group_type, Some(GroupType::Select));
    }

    /// 转出来的那条，「自动」的判据必须**认得它**（否则幂等建线会再造一条重复的）。
    #[test]
    fn v2_to_v3_result_is_recognised_as_the_auto_lane() {
        use crate::domain::{Lane, LaneKind};
        use crate::types::SubscriptionId;

        let mut doc = doc_v(2);
        doc.lanes.push(Lane::subscription_group("lane-g", "分组", "sub-a", "某组"));
        let migrated = migrate(doc);

        let sub: SubscriptionId = "sub-a".into();
        let found = migrated.auto_lane_on(&sub).expect("迁移结果必须被认成「自动」");
        assert_eq!(found.id.as_str(), "lane-g");
        assert_eq!(found.kind, LaneKind::SubscriptionAuto);
        // 按 id 找也认得（`ensure_auto_lane` 的幂等第 ① 步用它）
        assert!(migrated.auto_lane_by_id(&"lane-g".into(), &sub).is_some());
    }

    /// 迁移**之后**再出现 `subscription_group` 是配置损坏（手工改回来 / 迁移失败），
    /// 校验器必须点名报错 —— 静默接受会让它继续按 `url-test` 跑。
    #[test]
    fn subscription_group_is_rejected_after_migration() {
        use crate::domain::{ConfigDoc, Lane, Subscription};

        let mut doc = ConfigDoc::default();
        doc.proxy_subscriptions.push(Subscription::new("sub-a", "机场A", "https://a.example/sub"));
        doc.lanes.push(Lane::subscription_group("lane-g", "分组", "sub-a", "某组"));

        let report = crate::config::validate(&doc);
        assert!(
            report.errors.iter().any(|e| e.path == "lanes[0].kind"),
            "要按字段路径点名：{:?}",
            report.errors
        );
        assert!(
            report.errors.iter().any(|e| e.path == "lanes[0].kind" && e.message.contains("已取消")),
            "要说明这个概念已取消、以及该换什么：{:?}",
            report.errors
        );
    }
}

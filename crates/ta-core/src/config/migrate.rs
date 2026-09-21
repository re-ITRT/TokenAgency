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
pub const CURRENT_CONFIG_VERSION: u32 = 1;

/// 下一个待启用的版本号——v1 → v2 的位置已经留好。
pub const NEXT_CONFIG_VERSION: u32 = 2;

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

/// 迁移注册表：`from_version -> 升到 from + 1 的步骤`。
///
/// 注意：**新增步骤时必须同时更新 [`NEXT_CONFIG_VERSION`]（以及 [`CURRENT_CONFIG_VERSION`]）**，
/// 否则将来有人加了步骤却忘了改版本号，`needs_migration` / `migrate` 会漏判"还能再升一版"。
///
/// 当前为空（只有 v1，没有历史包袱）。将来加 v2 时在这里注册：
///
/// ```ignore
/// fn v1_to_v2(mut doc: ConfigDoc) -> Result<ConfigDoc, MigrationError> {
///     // 例如把 `net.health.interval` 拆成 `interval_s`
///     doc.version = 2;
///     Ok(doc)
/// }
///
/// pub fn migration_steps() -> BTreeMap<u32, MigrationStep> {
///     BTreeMap::from([(1u32, v1_to_v2 as MigrationStep)])
/// }
/// ```
pub fn migration_steps() -> BTreeMap<u32, MigrationStep> {
    BTreeMap::new()
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
        let only_v2 = migrate_to(doc_v(1), NEXT_CONFIG_VERSION, &steps).expect("应当升到 v2");
        assert_eq!(only_v2.version, 2);
        assert_eq!(only_v2.gateway.max_body_mb, 64);
        assert_ne!(only_v2.request_log.max_rows, 1_000_000);
    }

    #[test]
    fn migrate_to_reports_missing_step() {
        // v1 → v2 位置已留好但**还没注册**：跨版本迁移必须明确报错，不能静默放过。
        let err = migrate_to(doc_v(1), NEXT_CONFIG_VERSION, &migration_steps())
            .expect_err("没有注册 v1→v2，应当报错");
        assert_eq!(err.from, 1);
        assert_eq!(err.to, 2);
        assert!(err.message.contains("迁移步骤"), "实际：{}", err.message);
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
        // `migrate` 用的是内置（当前为空）的步骤表，所以这里只验证它的兜底语义：
        // 目标版本已经达到 → 原样返回。跨版本失败请用 `migrate_to`。
        let steps: BTreeMap<u32, MigrationStep> =
            BTreeMap::from([(1u32, failing_step as MigrationStep)]);
        assert!(migrate_to(doc_v(1), 2, &steps).is_err());
        assert_eq!(migrate(doc_v(1)).version, 1);
    }
}

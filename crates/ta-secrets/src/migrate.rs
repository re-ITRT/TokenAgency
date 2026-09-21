//! 明文扫描与迁移（[10 §2.2] 的 `secrets_scan_plaintext` / `secrets_migrate`）。
//!
//! ## 扫描范围
//!
//! * `credentials[].secret`
//! * `providers[].headers` 的每个值
//!
//! （`gateway.local_key` **暂不扫**：它由网关启动时直接读取，迁移成引用需要 ingress
//! 侧同步支持引用解析，见模块末尾 TODO。）
//!
//! ## 迁移语义
//!
//! * **返回新 doc，绝不原地改**（`migrate` 只读入参，`&ConfigDoc` 连 `mut` 都没有）。
//! * 先把值写进 `store`（钥匙串优先），**全部成功**后才改写引用；任何一个写失败都
//!   直接返回错误，不产出"半迁移"的 doc。
//! * 改写后的引用统一是 `"${secret:条目名}"`。
//! * 迁移完会**自检**：新 doc 再扫一遍，若仍有可迁移的明文 → `TaError::Internal`。
//! * 值只在这些函数的局部变量里出现，**不进日志、不进 trace、不进错误消息**；
//!   [`PlaintextFinding::masked`] 是脱敏预览。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ta_core::domain::ConfigDoc;
use ta_core::error::{Result, TaError};

use crate::resolver::{classify_literal, validate_entry_name, PlaintextKind};
use crate::{mask_secret, SecretStore};

/// 明文扫描结果（[10 §2.2] 的 `PlaintextFinding`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaintextFinding {
    /// 字段路径，如 `credentials[0].secret`、`providers[1].headers.x-api-key`
    pub location: String,
    /// 所属实体，如 `credential:openai-personal`、`provider:openai:x-api-key`
    pub entity: String,
    /// 形态（`sk_dash` / `jwt` / `long_literal` / `malformed_ref` ...）
    pub kind: PlaintextKind,
    /// 脱敏预览，**可以**直接展示（绝不含完整值）
    pub masked: String,
    /// 建议的钥匙串条目名
    pub suggested_name: String,
    /// 是否可以迁移进钥匙串（`malformed_ref` / `empty` 为 false）
    pub migratable: bool,
}

/// 一条迁移记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigratedSecret {
    pub location: String,
    pub name: String,
}

/// 迁移结果。（`ConfigDoc` 里有 `f64`，所以这里不派生 `PartialEq`/`Eq`。）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    /// **新的** ConfigDoc（原来的没动）
    pub doc: ConfigDoc,
    pub migrated: Vec<MigratedSecret>,
    /// 扫到但没法迁移的（写坏的引用、空值）
    pub skipped: Vec<PlaintextFinding>,
}

// ================================================================ 扫描

/// 扫出配置里的明文密钥（含写坏的引用与空值，见 [`PlaintextKind`]）。
pub fn scan_plaintext(doc: &ConfigDoc) -> Vec<PlaintextFinding> {
    plan(doc).iter().map(Planned::to_finding).collect()
}

/// 一键拿到"建议条目名"，供 UI 预填 `secrets_migrate(names)`。
pub fn suggested_names(doc: &ConfigDoc) -> Vec<String> {
    plan(doc).iter().map(|p| p.suggested_name.clone()).collect()
}

// ================================================================ 迁移

/// 把明文写进 `store` 并把 doc 里的值改写成 `"${secret:name}"`。
///
/// `names` 与 [`scan_plaintext`] 的结果**按顺序对齐**（对应 IPC 的
/// `secrets_migrate(names: string[])`）：空串或缺失的项用建议名。
pub async fn migrate(
    doc: &ConfigDoc,
    names: &[String],
    store: &dyn SecretStore,
) -> Result<ConfigDoc> {
    Ok(migrate_with_report(doc, names, store).await?.doc)
}

/// 同 [`migrate`]，但额外返回迁移明细与被跳过的项。
pub async fn migrate_with_report(
    doc: &ConfigDoc,
    names: &[String],
    store: &dyn SecretStore,
) -> Result<MigrationReport> {
    let planned = plan(doc);

    // 1) 先只做"命名规划"：确定每个条目名，并让相同明文复用同一个条目。
    let mut assignments: Vec<(usize, String)> = Vec::new();
    let mut skipped: Vec<PlaintextFinding> = Vec::new();
    let mut by_name: BTreeMap<String, String> = BTreeMap::new(); // name -> value
    let mut by_value: BTreeMap<String, String> = BTreeMap::new(); // value -> name

    for (idx, item) in planned.iter().enumerate() {
        if !item.kind.is_migratable() {
            skipped.push(item.to_finding());
            continue;
        }

        let explicit = names.get(idx).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let name = match explicit {
            Some(n) => {
                // 错误原因里只带位置，不带条目名（用户可能把 key 贴进了名字框）
                validate_entry_name(&n).map_err(|why| {
                    TaError::Secret(format!(
                        "迁移目标条目名不合法：{why}（位置：{}）",
                        item.location
                    ))
                })?;
                if let Some(existing) = by_name.get(&n) {
                    if existing != &item.value {
                        return Err(TaError::Secret(format!(
                            "迁移条目名冲突：位置 {} 与其它位置共用了同一条目名但值不同",
                            item.location
                        )));
                    }
                }
                n
            }
            None => match by_value.get(&item.value) {
                // 同一把 key 出现两次 → 复用同一条目
                Some(existing) => existing.clone(),
                None => unique_name(&item.suggested_name, &by_name, &item.value),
            },
        };

        by_name.entry(name.clone()).or_insert_with(|| item.value.clone());
        by_value.entry(item.value.clone()).or_insert_with(|| name.clone());
        assignments.push((idx, name));
    }

    // 2) 落进密钥仓库。任何一个失败都不改配置。
    for (name, value) in &by_name {
        store
            .set(name, value)
            .await
            .map_err(|e| TaError::Secret(format!("迁移写入密钥条目失败（{name}）：{e}")))?;
    }

    // 3) 改写引用（在新 doc 上）
    let mut out = doc.clone();
    let mut migrated = Vec::with_capacity(assignments.len());
    for (idx, name) in assignments {
        let item = &planned[idx];
        let spec = format!("${{{}:{}}}", crate::SECRET_SCHEME, name);
        match &item.target {
            Target::Credential(i) => out.credentials[*i].secret = spec,
            Target::ProviderHeader(i, header) => {
                out.providers[*i].headers.insert(header.clone(), spec);
            }
        }
        migrated.push(MigratedSecret { location: item.location.clone(), name });
    }

    // 4) 自检：新 doc 里不该再有可迁移的明文
    let leftovers: Vec<PlaintextFinding> =
        plan(&out).iter().filter(|p| p.kind.is_migratable()).map(Planned::to_finding).collect();
    if let Some(first) = leftovers.first() {
        return Err(TaError::Internal(format!(
            "迁移后仍残留 {} 处明文（首个位置：{}）",
            leftovers.len(),
            first.location
        )));
    }

    Ok(MigrationReport { doc: out, migrated, skipped })
}

// ================================================================ 内部

#[derive(Debug, Clone)]
enum Target {
    Credential(usize),
    ProviderHeader(usize, String),
}

#[derive(Debug, Clone)]
struct Planned {
    location: String,
    entity: String,
    kind: PlaintextKind,
    /// ⚠️ 明文真值：只在本模块内存里流转，绝不外泄。
    value: String,
    suggested_name: String,
    target: Target,
}

impl Planned {
    fn to_finding(&self) -> PlaintextFinding {
        PlaintextFinding {
            location: self.location.clone(),
            entity: self.entity.clone(),
            kind: self.kind,
            masked: mask_secret(&self.value),
            suggested_name: self.suggested_name.clone(),
            migratable: self.kind.is_migratable(),
        }
    }
}

fn plan(doc: &ConfigDoc) -> Vec<Planned> {
    let mut out = Vec::new();

    for (i, cred) in doc.credentials.iter().enumerate() {
        if let Some(kind) = classify_literal(&cred.secret) {
            let id = cred.id.as_str();
            let base = if id.trim().is_empty() {
                format!("cred-{i}")
            } else {
                format!("cred-{}", slug(id))
            };
            out.push(Planned {
                location: format!("credentials[{i}].secret"),
                entity: format!("credential:{id}"),
                kind,
                value: cred.secret.clone(),
                suggested_name: base,
                target: Target::Credential(i),
            });
        }
    }

    for (i, provider) in doc.providers.iter().enumerate() {
        for (header, value) in &provider.headers {
            if let Some(kind) = classify_literal(value) {
                let pid = provider.id.as_str();
                let owner = if pid.trim().is_empty() { format!("provider{i}") } else { slug(pid) };
                out.push(Planned {
                    location: format!("providers[{i}].headers.{header}"),
                    entity: format!("provider:{pid}:{header}"),
                    kind,
                    value: value.clone(),
                    suggested_name: format!("{owner}-{}", slug(header)),
                    target: Target::ProviderHeader(i, header.clone()),
                });
            }
        }
    }

    out
}

/// 建议名已被占用时追加 `-2` / `-3`...（同值则直接复用，保证同一个明文只占一个条目）。
fn unique_name(base: &str, by_name: &BTreeMap<String, String>, value: &str) -> String {
    match by_name.get(base) {
        None => return base.to_string(),
        Some(v) if v == value => return base.to_string(),
        Some(_) => {}
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        match by_name.get(&candidate) {
            None => return candidate,
            Some(v) if v == value => return candidate,
            Some(_) => continue,
        }
    }
    format!("{base}-{}", by_name.len() + 1)
}

/// 把任意 id / 头名变成合法且稳定的条目名片段。
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "secret".to_string()
    } else {
        out
    }
}

// TODO(P7-followup): `gateway.local_key` 也是明文密钥，但 migrate 改写成引用后需要
// ingress 侧先支持引用解析，否则会把网关本地鉴权打断。等 G1 支持后一并纳入 plan()。

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{block_on, MockStore};

    const CRED_PLAINTEXT: &str = "sk-cred-0123456789";
    const HEADER_PLAINTEXT: &str = "sk-head-0123456789";

    fn doc_with_plaintext() -> ConfigDoc {
        let yaml = format!(
            r#"
providers:
  - id: openai
    name: OpenAI
    protocol: openai
    base_url: https://api.openai.com/v1
    headers:
      x-api-key: "{HEADER_PLAINTEXT}"
      x-org: "org-public-123"
credentials:
  - id: openai-personal
    provider: openai
    label: personal
    secret: "{CRED_PLAINTEXT}"
  - id: already-migrated
    provider: openai
    label: migrated
    secret: "${{secret:existing-entry}}"
  - id: env-backed
    provider: openai
    label: env
    secret: "${{env:OPENAI_API_KEY}}"
"#
        );
        ta_core::config::from_yaml(&yaml).expect("测试 YAML 应能解析")
    }

    #[test]
    fn scan_finds_credentials_and_provider_headers_only() {
        let doc = doc_with_plaintext();
        let findings = scan_plaintext(&doc);

        let locations: Vec<&str> = findings.iter().map(|f| f.location.as_str()).collect();
        assert_eq!(
            locations,
            vec![
                "credentials[0].secret",
                "providers[0].headers.x-api-key",
                // `x-org: org-public-123` 也是非引用字面量 → 同样被标出来（短明文）
                "providers[0].headers.x-org",
            ]
        );

        let cred = &findings[0];
        assert_eq!(cred.kind, PlaintextKind::SkDash);
        assert!(cred.migratable);
        assert_eq!(cred.suggested_name, "cred-openai-personal");
        assert_eq!(cred.entity, "credential:openai-personal");
        assert_eq!(cred.masked, "sk-c***");
        assert!(!cred.masked.contains("plaintext"), "finding 不得泄漏明文");

        let header = findings.iter().find(|f| f.location.ends_with("x-api-key")).unwrap();
        assert_eq!(header.suggested_name, "openai-x-api-key");
        assert!(header.kind.is_high_risk());

        // 合法引用不算明文
        assert!(!locations.iter().any(|l| l.contains("credentials[1]")));
        assert!(!locations.iter().any(|l| l.contains("credentials[2]")));

        assert_eq!(
            suggested_names(&doc),
            vec!["cred-openai-personal", "openai-x-api-key", "openai-x-org"]
        );
    }

    #[test]
    fn scan_flags_broken_references_and_empty_values_as_not_migratable() {
        let yaml = r#"
credentials:
  - id: broken
    provider: p
    label: broken
    secret: "${secret:}"
  - id: empty
    provider: p
    label: empty
    secret: ""
"#;
        let doc = ta_core::config::from_yaml(yaml).expect("yaml");
        let findings = scan_plaintext(&doc);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].kind, PlaintextKind::MalformedRef);
        assert!(!findings[0].migratable);
        assert_eq!(findings[1].kind, PlaintextKind::Empty);
        assert!(!findings[1].migratable);
    }

    #[test]
    fn scan_on_clean_doc_is_empty() {
        let yaml = r#"
credentials:
  - id: a
    provider: p
    label: a
    secret: "${secret:some-entry}"
  - id: b
    provider: p
    label: b
    secret: "${env:SOME_VAR}"
"#;
        let doc = ta_core::config::from_yaml(yaml).expect("yaml");
        assert!(scan_plaintext(&doc).is_empty());
    }

    #[test]
    fn migrate_returns_new_doc_and_leaves_original_untouched() {
        let doc = doc_with_plaintext();
        let store = MockStore::new();

        let report = block_on(migrate_with_report(&doc, &[], &store)).expect("迁移成功");

        // 原 doc 一字未改
        assert_eq!(doc.credentials.len(), 3);
        assert_eq!(doc.credentials[0].secret, CRED_PLAINTEXT);
        assert_eq!(
            doc.providers[0].headers.get("x-api-key").map(String::as_str),
            Some(HEADER_PLAINTEXT)
        );

        // 新 doc 里全是指针
        assert_eq!(report.doc.credentials[0].secret, "${secret:cred-openai-personal}");
        assert_eq!(
            report.doc.providers[0].headers.get("x-api-key").map(String::as_str),
            Some("${secret:openai-x-api-key}")
        );
        assert_eq!(
            report.doc.providers[0].headers.get("x-org").map(String::as_str),
            Some("${secret:openai-x-org}")
        );
        // 已是引用的原样保留
        assert_eq!(report.doc.credentials[1].secret, "${secret:existing-entry}");
        assert_eq!(report.doc.credentials[2].secret, "${env:OPENAI_API_KEY}");

        // 值确实进了仓库
        assert_eq!(store.snapshot().get("cred-openai-personal"), Some(&CRED_PLAINTEXT.to_string()));
        assert_eq!(store.snapshot().get("openai-x-api-key"), Some(&HEADER_PLAINTEXT.to_string()));
        assert_eq!(report.migrated.len(), 3);
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn migrated_doc_contains_no_plaintext_anywhere() {
        let doc = doc_with_plaintext();
        let store = MockStore::new();
        let out = block_on(migrate(&doc, &[], &store)).expect("迁移成功");

        // 1) 再扫一遍：没有可迁移的明文字面量
        let remaining: Vec<_> = scan_plaintext(&out).into_iter().filter(|f| f.migratable).collect();
        assert!(remaining.is_empty(), "仍残留明文：{remaining:?}");

        // 2) 序列化后的 YAML 里再也搜不到那两把 key（最硬的证据）
        let yaml = ta_core::config::to_yaml(&out).expect("序列化");
        assert!(!yaml.contains(CRED_PLAINTEXT), "YAML 里还有凭据明文");
        assert!(!yaml.contains(HEADER_PLAINTEXT), "YAML 里还有 header 明文");
        assert!(!yaml.contains("sk-"), "YAML 里不应再出现任何 sk- 字面量");
    }

    #[test]
    fn migrate_honours_explicit_names_in_order() {
        let doc = doc_with_plaintext();
        let store = MockStore::new();
        let names = vec![
            "my-cred".to_string(),
            String::new(), // 空 → 用建议名
            "my-org".to_string(),
        ];
        let report = block_on(migrate_with_report(&doc, &names, &store)).expect("迁移成功");

        assert_eq!(report.doc.credentials[0].secret, "${secret:my-cred}");
        assert_eq!(
            report.doc.providers[0].headers.get("x-api-key").map(String::as_str),
            Some("${secret:openai-x-api-key}")
        );
        assert_eq!(
            report.doc.providers[0].headers.get("x-org").map(String::as_str),
            Some("${secret:my-org}")
        );
        assert_eq!(store.snapshot().get("my-cred"), Some(&CRED_PLAINTEXT.to_string()));
        assert_eq!(store.snapshot().get("my-org"), Some(&"org-public-123".to_string()));
    }

    #[test]
    fn migrate_rejects_conflicting_or_invalid_names() {
        let doc = doc_with_plaintext();
        let store = MockStore::new();

        // 两处不同明文用同一个显式名字 → 报错
        let names = vec!["same".to_string(), "same".to_string(), "same".to_string()];
        let err = block_on(migrate(&doc, &names, &store)).expect_err("应报冲突");
        assert!(err.to_string().contains("冲突"), "实际：{err}");
        assert!(store.is_empty(), "报错时不应写入任何条目");

        // 非法名字
        let names = vec!["bad name".to_string()];
        let err = block_on(migrate(&doc, &names, &store)).expect_err("应报非法名");
        assert!(err.to_string().contains("不合法"), "实际：{err}");
        assert!(!err.to_string().contains("bad name"), "错误消息不得回显条目名");
    }

    #[test]
    fn identical_secrets_share_one_entry() {
        let yaml = format!(
            r#"
credentials:
  - id: a
    provider: p
    label: a
    secret: "{CRED_PLAINTEXT}"
  - id: b
    provider: p
    label: b
    secret: "{CRED_PLAINTEXT}"
"#
        );
        let doc = ta_core::config::from_yaml(&yaml).expect("yaml");
        let store = MockStore::new();
        let report = block_on(migrate_with_report(&doc, &[], &store)).expect("迁移");

        assert_eq!(store.len(), 1, "同一把 key 只应写一个条目");
        assert_eq!(report.doc.credentials[0].secret, "${secret:cred-a}");
        assert_eq!(report.doc.credentials[1].secret, "${secret:cred-a}");
    }

    #[test]
    fn suggested_name_collision_gets_a_suffix() {
        // 两个 provider id 相同（配置本身不合法，但不能因此写坏钥匙串）
        let yaml = r#"
providers:
  - id: dup
    name: A
    protocol: openai
    base_url: https://a.example/v1
    headers:
      x-api-key: "sk-first-0123456789"
  - id: dup
    name: B
    protocol: openai
    base_url: https://b.example/v1
    headers:
      x-api-key: "sk-second-012345678"
"#;
        let doc = ta_core::config::from_yaml(yaml).expect("yaml");
        let store = MockStore::new();
        let report = block_on(migrate_with_report(&doc, &[], &store)).expect("迁移");

        let names: Vec<String> = report.migrated.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, vec!["dup-x-api-key".to_string(), "dup-x-api-key-2".to_string()]);
        assert_eq!(store.len(), 2, "两把不同的 key 必须各占一个条目");
    }

    #[test]
    fn migrate_propagates_store_failure_and_keeps_doc_unchanged() {
        let doc = doc_with_plaintext();
        let store = MockStore::always_failing("钥匙串被锁且便携文件不可写");
        let err = block_on(migrate(&doc, &[], &store)).expect_err("应失败");
        assert!(err.to_string().contains("钥匙串被锁"), "实际：{err}");
        assert_eq!(doc.credentials[0].secret, CRED_PLAINTEXT, "失败时原 doc 不能被改");
    }

    #[test]
    fn migrate_skips_non_migratable_items_without_failing() {
        let yaml = r#"
credentials:
  - id: broken
    provider: p
    label: broken
    secret: "${secret:}"
  - id: good
    provider: p
    label: good
    secret: "sk-good-0123456789"
"#;
        let doc = ta_core::config::from_yaml(yaml).expect("yaml");
        let store = MockStore::new();
        let report = block_on(migrate_with_report(&doc, &[], &store)).expect("迁移");

        assert_eq!(report.migrated.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].location, "credentials[0].secret");
        assert_eq!(report.doc.credentials[0].secret, "${secret:}"); // 写坏的引用保持原样
        assert_eq!(report.doc.credentials[1].secret, "${secret:cred-good}");
    }

    #[test]
    fn slug_and_unique_name_helpers() {
        assert_eq!(slug("OpenAI Personal"), "openai-personal");
        assert_eq!(slug("x-api-key"), "x-api-key");
        assert_eq!(slug("!!!"), "secret");
        assert_eq!(slug("a__b"), "a-b");

        let mut by_name = BTreeMap::new();
        by_name.insert("k".to_string(), "v1".to_string());
        assert_eq!(unique_name("k", &by_name, "v2"), "k-2");
        assert_eq!(unique_name("k", &by_name, "v1"), "k");
        assert_eq!(unique_name("fresh", &by_name, "v2"), "fresh");
    }
}

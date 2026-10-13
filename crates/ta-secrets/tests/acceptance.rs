//! P7 验收测试：把 的 DoD 用**公开 API** 走一遍。
//!
//! | DoD | 覆盖位置 |
//! |---|---|
//! | 引用解析全分支（含错误） | `resolver` 单测 + 本文件 `resolver_branches_*` |
//! | 明文检测 5 种形态 | `resolver::tests::classifies_all_plaintext_shapes` |
//! | 无钥匙串环境的回退路径 | 本文件 `headless_keychain_falls_back_to_portable_file` |
//! | CompositeStore 优先级与回退 | `composite` 单测 + 本文件 `composite_priority_*` |
//! | migrate 产出的 doc 里搜不到明文 | 本文件 `migrate_then_resolve_round_trip` |
//!
//! **绝不写系统钥匙串**：钥匙串位置由内存 `MockStore` 扮演。

use ta_secrets::composite::{CompositeStore, NamedStore, SourceKind};
use ta_secrets::mock::{block_on, MockStore};
use ta_secrets::portable::{PortableStore, FILE_NAME};
use ta_secrets::{
    is_reference, mask_secret, migrate, migrate_with_report, resolve_ref, resolve_spec,
    resolve_value, scan_plaintext, MapEnv, PlaintextKind, SecretRef, SecretStore,
};

const CRED_PLAINTEXT: &str = "sk-cred-0123456789";
const HEADER_PLAINTEXT: &str = "sk-head-0123456789";
const ENV_VALUE: &str = "sk-from-env-0123456789";
/// 便携库口令（测试专用；真实口令由用户在 GUI 里输入，不落盘）
const PORTABLE_PASS: &str = "acceptance-test-passphrase";

fn doc_with_plaintext() -> ta_core::domain::ConfigDoc {
    let yaml = format!(
        r#"
providers:
  - id: openai
    name: OpenAI
    protocol: openai
    base_url: https://api.openai.com/v1
    headers:
      x-api-key: "{HEADER_PLAINTEXT}"
credentials:
  - id: openai-personal
    provider: openai
    label: personal
    secret: "{CRED_PLAINTEXT}"
  - id: env-backed
    provider: openai
    label: env
    secret: "${{env:OPENAI_API_KEY}}"
"#
    );
    ta_core::config::from_yaml(&yaml).expect("测试 YAML")
}

// ================================================================ 1. 引用解析全分支

#[test]
fn resolver_branches_happy_path() {
    let store = MockStore::from_pairs([("openai-personal", CRED_PLAINTEXT)]);

    let r = resolve_ref("${secret:openai-personal}", &store).expect("钥匙串引用");
    assert_eq!(r, SecretRef::Keyring { name: "openai-personal".to_string() });

    let r = resolve_ref("${env:OPENAI_API_KEY}", &store).expect("环境变量引用");
    assert_eq!(r, SecretRef::Env { var: "OPENAI_API_KEY".to_string() });

    assert!(is_reference("${secret:openai-personal}"));
    assert!(!is_reference(CRED_PLAINTEXT));
}

#[test]
fn resolver_branches_error_paths() {
    let store = MockStore::new();

    // 明文 → PlaintextSecret（TA-5003）
    let err = resolve_ref(CRED_PLAINTEXT, &store).expect_err("明文应被拒");
    assert_eq!(err.code(), "TA-5003");

    // 写坏的引用 → Secret
    let err = resolve_ref("${secret:}", &store).expect_err("坏引用应被拒");
    assert_eq!(err.code(), "TA-9001");

    // 未知方案 → Secret
    let err = resolve_ref("${vault:x}", &store).expect_err("未知方案应被拒");
    assert_eq!(err.code(), "TA-9001");

    // 取不到值 → Secret（原因可读）
    let r = SecretRef::Keyring { name: "missing".to_string() };
    let err = block_on(resolve_value(&r, &store, &MapEnv::new())).expect_err("缺条目");
    assert!(err.to_string().contains("missing"));

    let r = SecretRef::Env { var: "NOT_SET_ANYWHERE".to_string() };
    let err = block_on(resolve_value(&r, &store, &MapEnv::new())).expect_err("缺环境变量");
    assert!(err.to_string().contains("NOT_SET_ANYWHERE"));
}

// ================================================================ 2. 明文检测形态

#[test]
fn plaintext_shapes_are_all_detected() {
    let shapes: Vec<(&str, PlaintextKind)> = vec![
        ("sk-proj-aaaaaaaa", PlaintextKind::SkDash),
        ("sk_live_aaaaaaaaaaaaaaaaaaaa", PlaintextKind::SkUnderscore),
        ("Bearer eyJhbGciOiJIUzI1NiJ9.aa.bb", PlaintextKind::Bearer),
        ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig", PlaintextKind::Jwt),
        ("0123456789012345678901234", PlaintextKind::LongLiteral),
    ];
    assert_eq!(shapes.len(), 5, "DoD 要求覆盖 5 种形态");
    for (value, expected) in shapes {
        assert_eq!(
            ta_secrets::classify_literal(value),
            Some(expected),
            "{value} 应被识别为 {expected:?}"
        );
    }
    // 合法引用绝不能被误杀
    assert_eq!(ta_secrets::classify_literal("${secret:a}"), None);
    assert_eq!(ta_secrets::classify_literal("${env:A}"), None);
}

// ================================================================ 3. 无钥匙串环境降级

#[test]
fn headless_keychain_falls_back_to_portable_file() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join(FILE_NAME);

    let composite = CompositeStore::new(vec![
        // 扮演"headless Linux 没有 Secret Service / 钥匙串被锁"
        NamedStore::new(
            SourceKind::Keyring,
            MockStore::always_failing("无法访问系统钥匙串（headless）"),
        ),
        NamedStore::new(SourceKind::Portable, PortableStore::with_passphrase(&path, PORTABLE_PASS)),
        NamedStore::new(SourceKind::Env, MockStore::new()),
    ]);

    // 链上还有"可用但没这条"的来源 → 语义是"没找到"（None），不是错误：
    // 降级不该把"没有"误报成"坏了"。
    assert_eq!(block_on(composite.get("openai-personal")).expect("ok"), None);

    // 写：降级到便携文件
    block_on(composite.set("openai-personal", CRED_PLAINTEXT)).expect("应降级写入便携文件");
    assert!(path.is_file(), "便携文件应被创建");

    // 读：降级路径取回同一把 key
    let got = block_on(composite.get("openai-personal")).expect("降级读取");
    assert_eq!(got.as_deref(), Some(CRED_PLAINTEXT));

    // 进程重启等价物：新实例（同口令）从同一个文件读
    let reopened = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("headless")),
        NamedStore::new(SourceKind::Portable, PortableStore::with_passphrase(&path, PORTABLE_PASS)),
    ]);
    assert_eq!(
        block_on(reopened.get("openai-personal")).expect("重开后读取").as_deref(),
        Some(CRED_PLAINTEXT)
    );

    // 便携文件的状态必须如实暴露：v2 已加密、且盘上确实是密文
    let portable = PortableStore::with_passphrase(&path, PORTABLE_PASS);
    assert!(portable.is_encrypted(), "v2 文件必须报告已加密");
    assert_eq!(portable.security_notice(), None, "已加密就不该有明文警告");
    assert_eq!(portable.disk_state().expect("header").version, Some(2));
    let raw = std::fs::read_to_string(&path).expect("读文件");
    assert!(raw.contains("\"encrypted\": true"));
    assert!(raw.contains("\"kdf\": \"argon2id\""));
    assert!(!raw.contains(CRED_PLAINTEXT), "密文文件里不得出现明文值");

    // 换口令后读不出来（AEAD 认证失败），且错误可读 —— 不是 panic
    let wrong = PortableStore::with_passphrase(&path, "wrong passphrase");
    let err = block_on(wrong.get("openai-personal")).expect_err("错误口令必须失败");
    assert!(err.to_string().contains("口令错误"), "实际：{err}");

    // 整条链都坏了 → 可读错误，不 panic，也不带任何密钥值
    let corrupt_dir = tempfile::tempdir().expect("临时目录");
    let corrupt = corrupt_dir.path().join(FILE_NAME);
    std::fs::write(&corrupt, "{not json").expect("写坏文件");
    let all_down = CompositeStore::new(vec![
        NamedStore::new(
            SourceKind::Keyring,
            MockStore::always_failing("无法访问系统钥匙串（headless）"),
        ),
        NamedStore::new(SourceKind::Portable, PortableStore::open(&corrupt)),
        NamedStore::new(SourceKind::Env, MockStore::always_failing("环境变量不可读")),
    ]);
    let err = block_on(all_down.get("openai-personal")).expect_err("全链不可用应报错");
    let msg = err.to_string();
    assert!(msg.contains("所有密钥来源都不可用"), "实际：{msg}");
    assert!(msg.contains("headless"), "应保留后端原因：{msg}");
    assert!(msg.contains("损坏"), "应保留便携文件原因：{msg}");
    assert!(!msg.contains(CRED_PLAINTEXT), "错误消息不得含密钥值：{msg}");
}

#[test]
fn portable_without_passphrase_refuses_to_write_plaintext() {
    // 便携模式的底线：没有口令 → 不写文件（宁可报错，也不悄悄退回明文存储）
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join(FILE_NAME);
    let composite = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("headless")),
        NamedStore::new(SourceKind::Portable, PortableStore::open(&path)),
    ]);

    let err = block_on(composite.set("k", CRED_PLAINTEXT)).expect_err("必须拒绝");
    assert!(err.to_string().contains("需要口令"), "实际：{err}");
    assert!(!path.exists(), "无口令时不允许创建任何密钥文件");
}

// ================================================================ 4. CompositeStore 优先级

#[test]
fn composite_priority_keyring_then_portable_then_env() {
    let keyring = MockStore::new();
    let portable = MockStore::new();
    let env = MockStore::new();
    let composite = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, keyring.clone()),
        NamedStore::new(SourceKind::Portable, portable.clone()),
        NamedStore::new(SourceKind::Env, env.clone()),
    ]);

    // 全都有 → 钥匙串赢
    keyring.insert("k", "from-keyring-0123456789");
    portable.insert("k", "from-portable-0123456789");
    env.insert("k", "from-env-0123456789");
    assert_eq!(
        block_on(composite.get("k")).expect("ok").as_deref(),
        Some("from-keyring-0123456789")
    );

    // 钥匙串没这条 → 便携
    assert_eq!(
        block_on(composite.get("only-portable")).expect("ok"),
        None,
        "没有任何来源有这条时应是 None"
    );
    portable.insert("only-portable", "p-0123456789");
    assert_eq!(
        block_on(composite.get("only-portable")).expect("ok").as_deref(),
        Some("p-0123456789")
    );

    // 钥匙串报错 → 便携
    let broken = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("钥匙串被锁")),
        NamedStore::new(SourceKind::Portable, MockStore::from_pairs([("k2", "p2-0123456789")])),
    ]);
    assert_eq!(block_on(broken.get("k2")).expect("ok").as_deref(), Some("p2-0123456789"));

    // 钥匙串报错 + 便携没有 → 环境变量
    let broken = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("钥匙串被锁")),
        NamedStore::new(SourceKind::Portable, MockStore::new()),
        NamedStore::new(SourceKind::Env, MockStore::from_pairs([("k3", "e3-0123456789")])),
    ]);
    assert_eq!(block_on(broken.get("k3")).expect("ok").as_deref(), Some("e3-0123456789"));

    // 写入永远优先钥匙串，且不会写环境变量来源
    block_on(composite.set("new-key", "n-0123456789")).expect("写");
    assert_eq!(keyring.snapshot().get("new-key").map(String::as_str), Some("n-0123456789"));
    assert!(!env.snapshot().contains_key("new-key"), "环境变量来源永远不被写");
}

// ================================================================ 5. 迁移后无明文 + 可解析

#[test]
fn migrate_then_resolve_round_trip() {
    let doc = doc_with_plaintext();
    let store = MockStore::new();

    // 扫描 → 找到 2 处（credential + header），都有建议名
    let findings = scan_plaintext(&doc);
    assert_eq!(findings.len(), 2, "实际：{findings:?}");
    assert!(findings.iter().all(|f| f.migratable));

    let names: Vec<String> = findings.iter().map(|f| f.suggested_name.clone()).collect();
    let report = block_on(migrate_with_report(&doc, &names, &store)).expect("迁移");
    let out = report.doc;

    // 1) 新 doc 里搜不到明文（连序列化后的文本也搜不到）
    assert!(scan_plaintext(&out).iter().all(|f| !f.migratable));
    let yaml = ta_core::config::to_yaml(&out).expect("序列化");
    assert!(!yaml.contains(CRED_PLAINTEXT));
    assert!(!yaml.contains(HEADER_PLAINTEXT));
    assert!(!yaml.contains("sk-"));

    // 2) 原 doc 没被改
    assert_eq!(doc.credentials[0].secret, CRED_PLAINTEXT);
    assert_eq!(
        doc.providers[0].headers.get("x-api-key").map(String::as_str),
        Some(HEADER_PLAINTEXT)
    );

    // 3) 改写后的引用能被解析回原值（用 MapEnv 注入环境变量，不碰进程环境）
    let env = MapEnv::new().with("OPENAI_API_KEY", ENV_VALUE);

    let cred_ref = out.credentials[0].secret.clone();
    assert_eq!(cred_ref, "${secret:cred-openai-personal}");
    let value = block_on(resolve_spec(&cred_ref, &store, &env)).expect("解析凭据引用");
    assert_eq!(value.expose(), CRED_PLAINTEXT);
    assert_eq!(format!("{value}"), "***", "SecretString 的 Display 必须脱敏");

    let header_ref = out.providers[0].headers.get("x-api-key").cloned().expect("header");
    assert_eq!(header_ref, "${secret:openai-x-api-key}");
    let value = block_on(resolve_spec(&header_ref, &store, &env)).expect("解析 header 引用");
    assert_eq!(value.expose(), HEADER_PLAINTEXT);

    // 环境变量引用照旧可用
    let value = block_on(resolve_spec("${env:OPENAI_API_KEY}", &store, &env)).expect("env");
    assert_eq!(value.expose(), ENV_VALUE);
}

#[test]
fn migrate_reports_and_masks_without_leaking_values() {
    let doc = doc_with_plaintext();
    let store = MockStore::new();
    let findings = scan_plaintext(&doc);

    for f in &findings {
        assert!(!f.masked.contains("plaintext"), "finding 泄漏了明文：{:?}", f.masked);
        assert!(f.masked.ends_with("***"), "finding 的预览必须脱敏：{:?}", f.masked);
    }

    let report = block_on(migrate_with_report(&doc, &[], &store)).expect("迁移");
    assert_eq!(report.migrated.len(), 2);
    assert!(report.skipped.is_empty());

    // 脱敏函数本身也不能泄漏
    assert_eq!(mask_secret(CRED_PLAINTEXT), "sk-c***");

    // 条目里的值才是真值，且只在 store 里
    assert_eq!(
        store.snapshot().get("cred-openai-personal").map(String::as_str),
        Some(CRED_PLAINTEXT)
    );
}

#[test]
fn migrate_into_portable_store_when_keyring_is_down() {
    // 迁移目标也可以是一条"钥匙串挂了 → 便携文件"的链
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join(FILE_NAME);
    let composite = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("headless")),
        NamedStore::new(SourceKind::Portable, PortableStore::with_passphrase(&path, PORTABLE_PASS)),
    ]);
    let doc = doc_with_plaintext();
    let out = block_on(migrate(&doc, &[], &composite)).expect("迁移应降级成功");

    let yaml = ta_core::config::to_yaml(&out).expect("序列化");
    assert!(!yaml.contains(CRED_PLAINTEXT) && !yaml.contains(HEADER_PLAINTEXT));

    // 盘上同样是密文（迁移不能变成"把明文换个地方放"）
    let raw = std::fs::read_to_string(&path).expect("读便携文件");
    assert!(!raw.contains(CRED_PLAINTEXT) && !raw.contains(HEADER_PLAINTEXT));

    let value = block_on(resolve_spec(&out.credentials[0].secret, &composite, &MapEnv::new()))
        .expect("迁移后应能取回");
    assert_eq!(value.expose(), CRED_PLAINTEXT);

    // 守卫：任何 SecretStore 都不该在错误里带值
    let corrupt_dir = tempfile::tempdir().expect("临时目录");
    let corrupt = corrupt_dir.path().join(FILE_NAME);
    std::fs::write(&corrupt, "{not json").expect("写坏文件");
    let all_down = CompositeStore::new(vec![
        NamedStore::new(SourceKind::Keyring, MockStore::always_failing("headless")),
        NamedStore::new(SourceKind::Portable, PortableStore::open(&corrupt)),
    ]);
    let err = block_on(all_down.get("missing-entry")).expect_err("全链不可用应报错");
    assert!(!err.to_string().contains("sk-"), "错误消息不得含密钥值：{err}");
    assert!(err.to_string().contains("所有密钥来源都不可用"), "实际：{err}");
}

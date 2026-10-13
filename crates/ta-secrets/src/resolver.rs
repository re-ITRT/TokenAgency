//! `${secret:name}` / `${env:VAR}` 引用解析、明文识别。
//!
//! 契约：（`resolve_ref` 签名冻结）、
//! 语法：。
//!
//! ```text
//! "${secret:openai-personal}"  ->  SecretRef::Keyring { name }
//! "${env:OPENAI_API_KEY}"      ->  SecretRef::Env { var }
//! "sk-..."                     ->  Err(TaError::PlaintextSecret)
//! ```
//!
//! # 安全铁律
//!
//! * 任何错误消息、日志、trace **都不包含密钥值**；解析失败时也**不回显引用原文**，
//!   因为用户很可能把明文 key 贴进了引用里（`${secret:sk-xxxx}`）。
//! * [`SecretString`] 的 `Debug`/`Display` 默认脱敏，杜绝 `println!("{secret}")`。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use ta_core::error::{Result, TaError};

use crate::SecretStore;

/// 钥匙串引用方案名（`${secret:name}`）。
pub const SECRET_SCHEME: &str = "secret";
/// 环境变量引用方案名（`${env:VAR}`）。
pub const ENV_SCHEME: &str = "env";
/// 引用 key（条目名 / 变量名）的最大长度。
pub const MAX_REF_KEY_LEN: usize = 255;
/// "长明文"阈值：无 `${` 且长度超过它 → 视为疑似密钥值。
pub const LONG_LITERAL_THRESHOLD: usize = 20;

const REF_HINT: &str = r#"合法形式只有 "${secret:条目名}" 与 "${env:变量名}""#;

// ================================================================ SecretRef

/// 解析后的密钥引用。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// `"${secret:name}"` — 系统钥匙串 / 便携文件 / 环境变量链上的条目名。
    Keyring { name: String },
    /// `"${env:VAR}"` — 进程环境变量。
    Env { var: String },
}

impl SecretRef {
    /// 方案名（`secret` / `env`），可安全写进日志。
    pub fn scheme(&self) -> &'static str {
        match self {
            SecretRef::Keyring { .. } => SECRET_SCHEME,
            SecretRef::Env { .. } => ENV_SCHEME,
        }
    }

    /// 条目名 / 变量名。**不是密钥值**，属于元数据。
    pub fn key(&self) -> &str {
        match self {
            SecretRef::Keyring { name } => name,
            SecretRef::Env { var } => var,
        }
    }

    /// 还原成配置里的引用写法（用于迁移后回写 YAML）。
    pub fn to_spec(&self) -> String {
        format!("${{{}:{}}}", self.scheme(), self.key())
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_spec())
    }
}

// ================================================================ SecretString

/// 密钥值包装：`Debug`/`Display` 强制脱敏，取值必须显式 [`SecretString::expose`]。
///
/// TODO(security): 依赖到位后加 `zeroize::ZeroizeOnDrop`（本 crate 目前无法新增依赖）。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出真正的密钥值。**只有拼请求头/请求体时才可以调用。**
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    /// 脱敏预览（可进日志）。
    pub fn masked(&self) -> String {
        crate::mask_secret(&self.0)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString(<已脱敏 {} 字符>)", self.len())
    }
}

/// **故意**只输出 `***`：即使有人 `format!("{secret}")` 也不会泄漏。
impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl From<SecretString> for String {
    fn from(value: SecretString) -> Self {
        value.0
    }
}

// ================================================================ 环境变量来源

/// 环境变量读取抽象。测试里请用 [`MapEnv`]，**不要**碰进程全局环境。
pub trait EnvSource: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

/// 真实进程环境（`std::env::var`）。
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// 测试用固定映射环境。
#[derive(Debug, Default, Clone)]
pub struct MapEnv {
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self { vars: pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect() }
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
}

// ================================================================ 解析

/// 解析引用语法。**纯语法解析，不做 IO**（`store` 是 §1.4 冻结签名的一部分，
/// 存在性校验在 [`resolve_value`] 取值时完成）。
///
/// * `"${secret:name}"` → [`SecretRef::Keyring`]
/// * `"${env:VAR}"` → [`SecretRef::Env`]
/// * 其它任何东西（含 `sk-...` 明文、写坏的 `${...}`）→ `Err`
pub fn resolve_ref(spec: &str, store: &dyn SecretStore) -> Result<SecretRef> {
    // `store` 只用于满足冻结签名；解析阶段不产生任何 IO。
    let _ = store;

    let raw = spec.trim();
    if raw.is_empty() {
        return Err(TaError::Secret("密钥引用为空".to_string()));
    }

    let Some(inner) = whole_reference(raw) else {
        if raw.contains("${") {
            return Err(malformed_ref());
        }
        // 不是引用 = 明文密钥值（`sk-...` / 长字符串 / 任何字面量）
        return Err(TaError::PlaintextSecret);
    };

    let Some((scheme, key)) = inner.split_once(':') else {
        return Err(malformed_ref());
    };

    let scheme = scheme.trim();
    let key = key.trim();

    match scheme.to_ascii_lowercase().as_str() {
        SECRET_SCHEME => {
            validate_entry_name(key).map_err(TaError::Secret)?;
            Ok(SecretRef::Keyring { name: key.to_string() })
        }
        ENV_SCHEME => {
            validate_env_var(key).map_err(TaError::Secret)?;
            Ok(SecretRef::Env { var: key.to_string() })
        }
        _ => Err(TaError::Secret(format!(
            "不支持的密钥引用方案（只支持 {SECRET_SCHEME} / {ENV_SCHEME}）"
        ))),
    }
}

/// 解析并取值。`env` 由调用方注入（测试用 [`MapEnv`]，生产用 [`ProcessEnv`]）。
pub async fn resolve_value(
    r: &SecretRef,
    store: &dyn SecretStore,
    env: &dyn EnvSource,
) -> Result<SecretString> {
    match r {
        SecretRef::Keyring { name } => match store.get(name).await {
            Ok(Some(value)) => {
                if value.trim().is_empty() {
                    Err(TaError::Secret(format!("密钥条目内容为空：{name}")))
                } else {
                    Ok(SecretString::new(value))
                }
            }
            Ok(None) => Err(TaError::Secret(format!(
                "没有找到密钥条目：{name}（钥匙串、便携文件、环境变量里都没有）"
            ))),
            // 存储层错误已带可读原因，原样上抛，避免二次包装丢上下文
            Err(e) => {
                tracing::debug!(entry = %name, "密钥条目取值失败");
                Err(e)
            }
        },
        SecretRef::Env { var } => match env.get(var) {
            Some(value) if !value.trim().is_empty() => Ok(SecretString::new(value)),
            Some(_) => Err(TaError::Secret(format!("环境变量内容为空：{var}"))),
            None => Err(TaError::Secret(format!("环境变量未设置：{var}"))),
        },
    }
}

/// [`resolve_ref`] + [`resolve_value`] 的便捷组合。
pub async fn resolve_spec(
    spec: &str,
    store: &dyn SecretStore,
    env: &dyn EnvSource,
) -> Result<SecretString> {
    let r = resolve_ref(spec, store)?;
    resolve_value(&r, store, env).await
}

fn whole_reference(s: &str) -> Option<&str> {
    s.strip_prefix("${")?.strip_suffix('}')
}

fn malformed_ref() -> TaError {
    // 刻意不回显原文：它可能就是被误贴的明文 key。
    TaError::Secret(format!("密钥引用语法不合法，{REF_HINT}"))
}

/// 校验钥匙串条目名。`Err` 里是**可读原因**（不含条目名本身，防止误贴 key）。
pub fn validate_entry_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("密钥条目名为空".to_string());
    }
    if name.chars().count() > MAX_REF_KEY_LEN {
        return Err(format!("密钥条目名过长（上限 {MAX_REF_KEY_LEN} 字符）"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | ':' | '/'))
    {
        return Err("密钥条目名含非法字符（只允许 ASCII 字母数字与 . _ - @ : /）".to_string());
    }
    Ok(())
}

/// 校验环境变量名（宽松版 POSIX：字母数字与下划线；Windows 上的 `-` 也放行）。
pub fn validate_env_var(var: &str) -> std::result::Result<(), String> {
    if var.is_empty() {
        return Err("环境变量名为空".to_string());
    }
    if var.chars().count() > MAX_REF_KEY_LEN {
        return Err(format!("环境变量名过长（上限 {MAX_REF_KEY_LEN} 字符）"));
    }
    if !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("环境变量名含非法字符（只允许 ASCII 字母数字、_ 与 -）".to_string());
    }
    Ok(())
}

/// 是否是一个**结构合法**的引用（不看条目是否存在）。
pub fn is_reference(spec: &str) -> bool {
    let raw = spec.trim();
    let Some(inner) = whole_reference(raw) else {
        return false;
    };
    let Some((scheme, key)) = inner.split_once(':') else {
        return false;
    };
    let key = key.trim();
    match scheme.trim().to_ascii_lowercase().as_str() {
        SECRET_SCHEME => validate_entry_name(key).is_ok(),
        ENV_SCHEME => validate_env_var(key).is_ok(),
        _ => false,
    }
}

// ================================================================ 明文识别

/// 明文（或写坏的引用）的形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaintextKind {
    /// `sk-...`（OpenAI 风格）
    SkDash,
    /// `sk_...`
    SkUnderscore,
    /// `Bearer xxx` / `Basic xxx`（整段被贴进了配置）
    Bearer,
    /// JWT（`eyJ...` 三段点分）
    Jwt,
    /// 无 `${` 且长度 > [`LONG_LITERAL_THRESHOLD`] 的字面量
    LongLiteral,
    /// 其它短字面量（不是引用，也算明文）
    ShortLiteral,
    /// 含 `${` 但不是合法引用 —— 配置写错了，**没有值可迁移**
    MalformedRef,
    /// 空串 —— 凭据还没填
    Empty,
}

impl PlaintextKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlaintextKind::SkDash => "sk_dash",
            PlaintextKind::SkUnderscore => "sk_underscore",
            PlaintextKind::Bearer => "bearer",
            PlaintextKind::Jwt => "jwt",
            PlaintextKind::LongLiteral => "long_literal",
            PlaintextKind::ShortLiteral => "short_literal",
            PlaintextKind::MalformedRef => "malformed_ref",
            PlaintextKind::Empty => "empty",
        }
    }

    /// 有真值、可以搬进钥匙串的形态。
    pub fn is_migratable(&self) -> bool {
        !matches!(self, PlaintextKind::MalformedRef | PlaintextKind::Empty)
    }

    /// 高风险（看起来就是一把真 key）。
    pub fn is_high_risk(&self) -> bool {
        matches!(
            self,
            PlaintextKind::SkDash
                | PlaintextKind::SkUnderscore
                | PlaintextKind::Bearer
                | PlaintextKind::Jwt
                | PlaintextKind::LongLiteral
        )
    }
}

/// 判断一个配置里的值是不是明文。
///
/// * 合法的 `${...}` 引用 → `None`
/// * 其它任何东西 → `Some(kind)`（含空串与写坏的引用）
pub fn classify_literal(value: &str) -> Option<PlaintextKind> {
    let v = value.trim();
    if v.is_empty() {
        return Some(PlaintextKind::Empty);
    }
    if is_reference(v) {
        return None;
    }
    if v.contains("${") {
        return Some(PlaintextKind::MalformedRef);
    }
    if v.starts_with("sk-") {
        return Some(PlaintextKind::SkDash);
    }
    if v.starts_with("sk_") {
        return Some(PlaintextKind::SkUnderscore);
    }
    let lower = v.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return Some(PlaintextKind::Bearer);
    }
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return Some(PlaintextKind::Jwt);
    }
    if v.chars().count() > LONG_LITERAL_THRESHOLD {
        return Some(PlaintextKind::LongLiteral);
    }
    Some(PlaintextKind::ShortLiteral)
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{block_on, MockStore};

    fn store() -> MockStore {
        MockStore::new()
    }

    #[test]
    fn parses_keyring_reference() {
        let store = store();
        let r = resolve_ref("${secret:openai-personal}", &store).expect("应解析成功");
        assert_eq!(r, SecretRef::Keyring { name: "openai-personal".into() });
        assert_eq!(r.scheme(), "secret");
        assert_eq!(r.key(), "openai-personal");
        assert_eq!(r.to_spec(), "${secret:openai-personal}");
    }

    #[test]
    fn parses_env_reference() {
        let store = store();
        let r = resolve_ref("${env:OPENAI_API_KEY}", &store).expect("应解析成功");
        assert_eq!(r, SecretRef::Env { var: "OPENAI_API_KEY".into() });
        assert_eq!(r.to_spec(), "${env:OPENAI_API_KEY}");
    }

    #[test]
    fn tolerates_surrounding_whitespace_and_scheme_case() {
        let store = store();
        assert!(matches!(
            resolve_ref("  ${SECRET:  my-key  } ", &store),
            Ok(SecretRef::Keyring { .. })
        ));
        assert!(matches!(resolve_ref("${ENV:HOME}", &store), Ok(SecretRef::Env { .. })));
    }

    #[test]
    fn rejects_unknown_scheme() {
        let store = store();
        let err = resolve_ref("${vault:foo}", &store).expect_err("未知方案应报错");
        assert!(matches!(err, TaError::Secret(_)), "实际：{err:?}");
        assert!(!err.to_string().contains("vault:foo"), "错误消息不得回显引用原文");
    }

    #[test]
    fn rejects_malformed_references() {
        let store = store();
        for bad in [
            "${secret:}",      // 空条目名
            "${secret}",       // 缺冒号
            "${secret:name",   // 缺右括号
            "${secret:na me}", // 非法字符
            "x${secret:name}", // 不是整体引用
            "${secret:a}${secret:b}",
            "${}", // 空引用
        ] {
            let err = resolve_ref(bad, &store).expect_err("应报错");
            match err {
                TaError::Secret(_) => {}
                other => panic!("{bad} 应报 Secret 错误，实际 {other:?}"),
            }
            // 错误信息里不能带引用原文（可能是明文 key）
            assert!(!err.to_string().contains("na me"), "消息泄漏了引用原文");
        }
    }

    #[test]
    fn empty_spec_is_an_error_not_plaintext() {
        let store = store();
        let err = resolve_ref("   ", &store).expect_err("空引用应报错");
        assert!(matches!(err, TaError::Secret(_)));
    }

    #[test]
    fn plaintext_is_rejected() {
        let store = store();
        for value in [
            "sk-proj-abcdefghij",
            "sk_live_abcdefghijklmnopqrstuvwxyz",
            "Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig",
            "a-very-long-plaintext-api-key-1234567890",
        ] {
            let err = resolve_ref(value, &store).expect_err("明文必须被拒绝");
            assert!(matches!(err, TaError::PlaintextSecret), "{value} => {err:?}");
            // 错误类型码就是 TA-5003（前端据此弹"迁移到钥匙串"）
            assert_eq!(err.code(), "TA-5003");
        }
    }

    #[test]
    fn resolve_value_from_keyring() {
        let store = MockStore::new();
        store.insert("openai-personal", "sk-real-0123456789");
        let r = SecretRef::Keyring { name: "openai-personal".into() };
        let value = block_on(resolve_value(&r, &store, &MapEnv::new())).expect("取值成功");
        assert_eq!(value.expose(), "sk-real-0123456789");
        // 脱敏：Display / Debug 都不能带值
        assert_eq!(format!("{value}"), "***");
        assert!(!format!("{value:?}").contains("real-value"));
        assert_eq!(value.masked(), "sk-r***");
    }

    #[test]
    fn resolve_value_from_env_source() {
        let env = MapEnv::new().with("OPENAI_API_KEY", "sk-from-env-0123456789");
        let r = SecretRef::Env { var: "OPENAI_API_KEY".into() };
        let value = block_on(resolve_value(&r, &MockStore::new(), &env)).expect("取值成功");
        assert_eq!(value.expose(), "sk-from-env-0123456789");
    }

    #[test]
    fn resolve_value_errors_are_readable() {
        let store = MockStore::new();
        let missing = SecretRef::Keyring { name: "nope".into() };
        let err = block_on(resolve_value(&missing, &store, &MapEnv::new())).expect_err("缺条目");
        assert!(err.to_string().contains("nope"));
        assert!(matches!(err, TaError::Secret(_)));

        let empty_env = MapEnv::new().with("EMPTY_VAR", "   ");
        let r = SecretRef::Env { var: "EMPTY_VAR".into() };
        let err = block_on(resolve_value(&r, &store, &empty_env)).expect_err("空环境变量");
        assert!(err.to_string().contains("EMPTY_VAR"));

        let unset = SecretRef::Env { var: "UNSET_VAR".into() };
        let err = block_on(resolve_value(&unset, &store, &MapEnv::new())).expect_err("未设置");
        assert!(err.to_string().contains("未设置"));

        let empty_entry = MockStore::new();
        empty_entry.insert("blank", "  ");
        let r = SecretRef::Keyring { name: "blank".into() };
        let err = block_on(resolve_value(&r, &empty_entry, &MapEnv::new())).expect_err("空条目");
        assert!(err.to_string().contains("blank"));
    }

    #[test]
    fn resolve_value_propagates_store_failure() {
        let store = MockStore::always_failing("钥匙串被锁");
        let r = SecretRef::Keyring { name: "any".into() };
        let err = block_on(resolve_value(&r, &store, &MapEnv::new())).expect_err("应失败");
        assert!(err.to_string().contains("钥匙串被锁"));
    }

    #[test]
    fn resolve_spec_end_to_end() {
        let store = MockStore::new();
        store.insert("my-key", "sk-spec-0123456789");
        let env = MapEnv::new().with("MY_KEY", "sk-env-0123456789");

        let a = block_on(resolve_spec("${secret:my-key}", &store, &env)).expect("ok");
        assert_eq!(a.expose(), "sk-spec-0123456789");
        let b = block_on(resolve_spec("${env:MY_KEY}", &store, &env)).expect("ok");
        assert_eq!(b.expose(), "sk-env-0123456789");
        assert!(block_on(resolve_spec("sk-raw", &store, &env)).is_err());
    }

    #[test]
    fn classifies_all_plaintext_shapes() {
        assert_eq!(classify_literal("sk-proj-abcdefghij"), Some(PlaintextKind::SkDash));
        assert_eq!(classify_literal("sk_live_abcdefghijklmnop"), Some(PlaintextKind::SkUnderscore));
        assert_eq!(
            classify_literal("Bearer eyJhbGciOiJIUzI1NiJ9.aaa.bbb"),
            Some(PlaintextKind::Bearer)
        );
        assert_eq!(
            classify_literal("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig"),
            Some(PlaintextKind::Jwt)
        );
        assert_eq!(classify_literal("0123456789012345678901"), Some(PlaintextKind::LongLiteral));
        assert_eq!(classify_literal("abc"), Some(PlaintextKind::ShortLiteral));
        assert_eq!(classify_literal("${secret:}"), Some(PlaintextKind::MalformedRef));
        assert_eq!(classify_literal("   "), Some(PlaintextKind::Empty));

        // 合法引用不算明文
        assert_eq!(classify_literal("${secret:openai}"), None);
        assert_eq!(classify_literal("${env:OPENAI_API_KEY}"), None);
        assert_eq!(classify_literal("${SECRET:openai}"), None);

        assert!(PlaintextKind::SkDash.is_high_risk());
        assert!(!PlaintextKind::ShortLiteral.is_high_risk());
        assert!(PlaintextKind::SkDash.is_migratable());
        assert!(!PlaintextKind::MalformedRef.is_migratable());
        assert!(!PlaintextKind::Empty.is_migratable());
    }

    #[test]
    fn is_reference_matches_parser() {
        let store = store();
        for spec in ["${secret:a}", "${env:VAR}", "  ${SECRET:a-b.c} "] {
            assert!(is_reference(spec), "{spec} 应是合法引用");
            assert!(resolve_ref(spec, &store).is_ok());
        }
        for spec in ["sk-abc", "${secret:}", "plain", "${vault:a}", ""] {
            assert!(!is_reference(spec), "{spec} 不应算合法引用");
        }
    }

    #[test]
    fn validate_helpers_reject_bad_names_without_echoing() {
        assert!(validate_entry_name("openai-personal").is_ok());
        assert!(validate_entry_name("").is_err());
        assert!(validate_entry_name("a b").is_err());
        assert!(validate_entry_name(&"x".repeat(300)).is_err());
        assert!(validate_env_var("OPENAI_API_KEY").is_ok());
        assert!(validate_env_var("bad var").is_err());
        // 关键：报错原因里不能出现被校验的名字（可能是误贴的 key）
        let err = validate_entry_name("sk-secret-pasted-here!").expect_err("应报错");
        assert!(!err.contains("secret-pasted"), "原因里泄漏了名字：{err}");
    }
}

//! `api_key` —— **我们网关对外的访问密钥**（`docs/19-provider-wizard-network-default-and-api-keys.md` §3.1/§3.2）。
//!
//! 客户端拿这把 key 去调本机网关（`Authorization: Bearer ta-sk-…` 或 `x-api-key`）。
//! 表在 `migrations/0004_api_key.sql`，用量落在 **既有**的 `api_key_usage` 表
//! （`usage_store`），本模块只管 key 这一侧。
//!
//! ## 三条不许退化的口径（[docs/19 §3.1]）
//!
//! 1. **只存 `sha256(明文)`**：与"key 永不进 SQLite"的既有纪律同族。明文只在
//!    [`SqliteApiKeyStore::create`] 的返回值里出现**一次**，之后没有任何取回路径
//!    （`list` 连 `key_hash` 都不查 —— 不是"查了不返回"，是**根本不 SELECT 它**）。
//! 2. **`last_used_at` 的 NULL 是"从未用过"**，不是 0。写成 0 会让 UI 显示
//!    "上次使用：1970"（那是"用过"），与事实相反。所以 `create` 显式写 NULL，
//!    [`SqliteApiKeyStore::touch_last_used`] 是唯一会写它的地方。
//! 3. **`enabled = 0` 立刻影响鉴权**：命中判据是
//!    [`SqliteApiKeyStore::find_enabled_by_hash`]（`WHERE key_hash = ? AND enabled = 1`），
//!    所以停用/删除不需要缓存失效，也不需要重启网关。
//!
//! ## 明文形状
//!
//! `ta-sk-` + 43 位 base62（`62^43 ≈ 2^256`，即 32 字节熵）。选中 base62
//! （字母数字）而不是 base64：`-` / `_` / `+` / `/` 会让这把 key 在 URL、
//! 命令行、`.env`、YAML 里各需要一套转义规则 —— 那些地方本来就常常手工粘贴。
//!
//! 随机源用 `rand::thread_rng()` + **拒绝采样**（`b < 248`，因为 `248 = 4 × 62`）：
//! 直接 `b % 62` 会让前 8 个字符的概率高约 1.6%，对"高熵密钥"来说是不该有的偏斜。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use ta_core::error::Result;

use crate::util::{sha256_hex, StoreResultExt};

/// 明文前缀（`docs/19 §3.2` 逐字要求 `ta-sk-`）。
pub const PLAINTEXT_PREFIX: &str = "ta-sk-";

/// 明文随机段长度：43 位 base62 ≈ 256 bit ≈ 32 字节熵（契约要求 ≥32 字节）。
const PLAINTEXT_BODY_LEN: usize = 43;

/// `key_prefix` 的展示长度（`docs/19 §3.1`：前 12 字符 + `…`）。
const PREFIX_CHARS: usize = 12;

/// id 的随机段长度（16 位 base62 ≈ 95 bit：本地库里撞 id 的概率可以忽略）。
const ID_BODY_LEN: usize = 16;

/// base62 字母表（大写在前，与 `rand` 的拒绝采样配合时分布均匀）。
const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `api_key_list` / `api_key_create` / `api_key_set_enabled` 返回的那一行。
///
/// **它是 IPC 的线上形状**（`src/shared/bindings.ts` 的 `ApiKeyView`），
/// 也是表里可读列的同构投影 —— 所以这里不另造一份 DTO。
///
/// **不含明文、不含 hash**：类型里没有这两个字段，所以"列表泄漏了密钥"
/// 在类型层面就不可能发生（比"记得别 bind 那一列"可靠）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyView {
    pub id: String,
    pub name: String,
    /// 展示用前缀（明文前 12 字符 + `…`）。
    pub key_prefix: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at_ms: i64,
    /// 网关最近一次用它鉴权成功的时刻（Unix 毫秒）；**从未用过 = `None`**（不是 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<i64>,
}

/// `api_key_create` 的返回：`plaintext` **只在这里出现一次**。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyCreated {
    pub key: ApiKeyView,
    /// 明文。**只此一次** —— 库里只有 sha256，关掉页面就再也取不回来。
    pub plaintext: String,
}

/// `sha256(明文)` 的十六进制形式 —— 表里存的就是它，网关鉴权查的也是它。
///
/// 单独导出（而不是藏在 `create` 里）：鉴权路径（§3.3 ②）必须用**同一个**函数
/// 把客户端送来的明文变成查表用的 hash。两处各写一遍哈希，迟早有一处漂移
/// （漂移的表现是"新 key 一律 401"，而没有任何东西会红）。
pub fn hash_token(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// `api_key` 表的读写。
///
/// 每条方法都自带事务语义（单条 SQL）：这里没有"多步写"，
/// 所以不引入 `Transaction` —— 多一层包装只会让 `?` 的失败点变模糊。
pub struct SqliteApiKeyStore {
    pool: SqlitePool,
}

impl SqliteApiKeyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 建一把 key：生成明文 → 只存 `sha256(明文)` → 回读那一行。
    ///
    /// `now_ms` 由调用方给（时钟是注入的，不在这里读系统时间）——
    /// 与 `usage_store` / `trace_store` 的口径一致。
    pub async fn create(
        &self,
        name: &str,
        note: Option<&str>,
        now_ms: i64,
    ) -> Result<ApiKeyCreated> {
        let id = format!("key_{}", random_base62(ID_BODY_LEN));
        let plaintext = format!("{PLAINTEXT_PREFIX}{}", random_base62(PLAINTEXT_BODY_LEN));
        let key_hash = hash_token(&plaintext);
        let key_prefix = format!("{}…", plaintext.chars().take(PREFIX_CHARS).collect::<String>());

        let view = ApiKeyView {
            id,
            name: name.to_string(),
            key_prefix,
            enabled: true,
            note: note.map(str::to_string),
            created_at_ms: now_ms,
            // ★ 从未用过 = NULL。写 0 会让 UI 显示成"1970 年用过"。
            last_used_at_ms: None,
        };

        sqlx::query(
            "INSERT INTO api_key
                 (id, name, key_hash, key_prefix, enabled, note, created_at, last_used_at)
             VALUES (?, ?, ?, ?, 1, ?, ?, NULL)",
        )
        .bind(&view.id)
        .bind(&view.name)
        .bind(&key_hash)
        .bind(&view.key_prefix)
        .bind(&view.note)
        .bind(view.created_at_ms)
        .execute(&self.pool)
        .await
        .store_err()?;

        // 返回值里的 `view` 与库里那一行逐字段一致（同一份值写的、同一份值回的）
        Ok(ApiKeyCreated { key: view, plaintext })
    }

    /// 全部 key（按创建时间、id 升序）。**不 SELECT `key_hash`** —— 见模块文档。
    pub async fn list(&self) -> Result<Vec<ApiKeyView>> {
        let rows =
            sqlx::query(&format!("SELECT {READ_COLUMNS} FROM api_key ORDER BY created_at, id"))
                .fetch_all(&self.pool)
                .await
                .store_err()?;
        Ok(rows.iter().map(map_row).collect())
    }

    /// 单行（不存在 → `None`）。
    pub async fn get(&self, id: &str) -> Result<Option<ApiKeyView>> {
        let row = sqlx::query(&format!("SELECT {READ_COLUMNS} FROM api_key WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .store_err()?;
        Ok(row.as_ref().map(map_row))
    }

    /// 启停。**返回库里回读的那一行**（不是把入参拼出来的对象）：
    /// 命令契约要求返回 `ApiKeyView`，而"我改成功了"必须由库证明。
    ///
    /// 不存在 → `None`（命令层据此报"没有这把 key"，不许静默成功）。
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Option<ApiKeyView>> {
        sqlx::query("UPDATE api_key SET enabled = ? WHERE id = ?")
            .bind(i64::from(enabled))
            .bind(id)
            .execute(&self.pool)
            .await
            .store_err()?;
        // ★ 回读：返回值必须来自库，而不是"我记得我写成了什么"
        self.get(id).await
    }

    /// 删除。返回 `true` = 真的删掉了一行；`false` = 本来就没有（命令层据此报错）。
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM api_key WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(done.rows_affected() > 0)
    }

    /// 记"最近一次使用"。**只有这一处会写 `last_used_at`**。
    ///
    /// 影响 0 行不是错误：key 可能刚被删除/停用（网关那边这次请求已经通过了），
    /// 让记账失败会把一次成功的请求变成 500 —— 那比少一个时间戳糟得多。
    pub async fn touch_last_used(&self, id: &str, at_ms: i64) -> Result<()> {
        sqlx::query("UPDATE api_key SET last_used_at = ? WHERE id = ?")
            .bind(at_ms)
            .bind(id)
            .execute(&self.pool)
            .await
            .store_err()?;
        Ok(())
    }

    /// **库里是否至少有一把启用中的 key**。
    ///
    /// 它是 `docs/19 §3.3` 那句启用条件的判据（网关的
    /// `ApiKeyVerifier::any_enabled` 直接转调这里）：
    /// **`gateway.local_key` 为空且这里为假 → 不鉴权**（保持既有行为；
    /// 否则老用户升级后会被自己的网关全部挡在 401 外面）；
    /// 只要有一把启用中的 key，请求就必须命中本地 key 或某把 api key。
    ///
    /// 一次性读完、不缓存：`api_key` 表的写路径全在命令层，
    /// 缓存只会让"禁用了还能用"多一个窗口。
    pub async fn any_enabled(&self) -> Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key WHERE enabled = 1")
            .fetch_one(&self.pool)
            .await
            .store_err()?;
        Ok(n > 0)
    }

    /// §3.3 ②的查表：`sha256(明文)` 命中且**启用中**才算数。
    ///
    /// `enabled = 1` 写在 SQL 里（不是在 Rust 里过滤）：
    /// "停用立刻影响鉴权"这条语义只有一处实现，读代码的人不必去猜调用方。
    pub async fn find_enabled_by_hash(&self, key_hash: &str) -> Result<Option<ApiKeyView>> {
        let row = sqlx::query(&format!(
            "SELECT {READ_COLUMNS} FROM api_key WHERE key_hash = ? AND enabled = 1"
        ))
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .store_err()?;
        Ok(row.as_ref().map(map_row))
    }
}

/// 可读列清单。**`key_hash` 不在里面** —— 想"顺手 SELECT *"的人会在这里踩空。
const READ_COLUMNS: &str = "id, name, key_prefix, enabled, note, created_at, last_used_at";

/// 行 → [`ApiKeyView`]。
fn map_row(r: &sqlx::sqlite::SqliteRow) -> ApiKeyView {
    ApiKeyView {
        id: r.get("id"),
        name: r.get("name"),
        key_prefix: r.get("key_prefix"),
        // SQLite 没有 bool：0/1 在这里收口成 `bool`（别的地方不许再判一次）
        enabled: r.get::<i64, _>("enabled") != 0,
        note: r.get("note"),
        created_at_ms: r.get("created_at"),
        // NULL 原样变成 None：**不 `unwrap_or(0)`**（那会把"从未用过"变成"1970 用过"）
        last_used_at_ms: r.get("last_used_at"),
    }
}

/// `len` 位 base62 随机串（拒绝采样，无模偏斜 —— 见模块文档）。
fn random_base62(len: usize) -> String {
    use rand::Rng;

    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let b: u8 = rng.gen();
        // 248 = 4 × 62：只用 `0..248` 才能让 62 个字符等概率
        if b < 248 {
            out.push(ALPHABET[(b % 62) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随机段的形态：长度、字母表、两次不同。**分布是否均匀不在这里证**
    /// （那要统计检验）；这里证的是"它不是一个常量、也不会拼出非法字符"。
    #[test]
    fn random_base62_is_well_formed_and_varies() {
        let a = random_base62(PLAINTEXT_BODY_LEN);
        let b = random_base62(PLAINTEXT_BODY_LEN);
        assert_eq!(a.len(), PLAINTEXT_BODY_LEN);
        assert_ne!(a, b, "两次随机必须不同（否则随机源没接上）");
        assert!(a.bytes().all(|c| ALPHABET.contains(&c)), "只允许 base62：{a}");

        // 1000 次里不该出现重复 id 段（16 位 base62 ≈ 95 bit）
        let ids: std::collections::HashSet<String> =
            (0..1000).map(|_| random_base62(ID_BODY_LEN)).collect();
        assert_eq!(ids.len(), 1000, "id 段撞了 —— 随机源或长度有问题");
    }

    /// 拒绝采样的**边界**：所有合法字母都要能被取到、且没有偏斜的实现会漏掉某个字符。
    #[test]
    fn random_base62_can_produce_every_alphabet_char() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..400 {
            for c in random_base62(64).bytes() {
                seen.insert(c);
            }
        }
        assert_eq!(seen.len(), 62, "62 个字符应当都出现过：{}", seen.len());
    }

    #[test]
    fn hash_token_is_sha256_hex_of_the_plaintext() {
        let h = hash_token("ta-sk-abc");
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, hash_token("ta-sk-abc"), "同输入同输出");
        assert_ne!(h, hash_token("ta-sk-abd"), "不同输入必须不同");
    }
}

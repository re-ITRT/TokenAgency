//! §3.3 ② 的**实现侧**：网关多 key 鉴权的"查库那一半"（我们网关对外的访问密钥）。
//!
//! # 分工（别在这里再造一份契约）
//!
//! * 接缝与判定顺序在 `ta-gateway`（`ingress::check_local_key` + `ingress::ApiKeyVerifier`）：
//!   ① `gateway.local_key` 匹配 → `KeyIdentity::Local`；② 否则问本模块；③ 都不中 → 401。
//! * **本模块只回答两个问题**：库里有没有启用中的 key（决定"没配 `local_key` 时要不要强制鉴权"），
//!   以及这个明文对应哪把 key（命中即记 `last_used_at`）。
//!
//! 身份类型与 trait 都**直接用网关的**（`ta_gateway::ingress::{ApiKeyVerifier, KeyIdentity}`）：
//! E5 在那边写明"应用层那个本地类型应当删掉改用这一个，否则就是第二份契约 ——
//! 一致时看不出来、漂移时静默"。所以这里没有第二个 `KeyIdentity`。
//!
//! # fail closed
//!
//! `verify` / `any_enabled` 返回 `Err` 时，`check_local_key` 用 `?` 直接把请求判失败。
//! 这一点必须在实现侧守住：库故障时**不能**退化成 `Ok(None)`（那是"这把 key 不存在"）
//! 或 `Ok(false)`（那是"不做鉴权"）—— 两者都是放行。
//! [`a_broken_store_fails_closed_instead_of_passing`] 用真关掉的连接池证这一条。

use std::sync::Arc;

use async_trait::async_trait;

use ta_core::clock::Clock;
use ta_core::error::Result;
use ta_gateway::ingress::ApiKeyVerifier;
use ta_store::api_key::hash_token;
use ta_store::SqliteApiKeyStore;

use crate::state::AppState;

/// §3.3 ② 的实现：明文 → `key_id`。
pub struct ApiKeyAuthenticator {
    keys: Arc<SqliteApiKeyStore>,
    clock: Arc<dyn Clock>,
}

impl ApiKeyAuthenticator {
    pub fn new(keys: Arc<SqliteApiKeyStore>, clock: Arc<dyn Clock>) -> Self {
        Self { keys, clock }
    }
}

#[async_trait]
impl ApiKeyVerifier for ApiKeyAuthenticator {
    /// 库里有没有启用中的 key。**不缓存**：停用/删除必须立刻生效，
    /// 而缓存只会给"禁用了还能用"开一个窗口。
    async fn any_enabled(&self) -> Result<bool> {
        self.keys.any_enabled().await
    }

    /// `sha256(token)` 命中**启用中**的 key → `Ok(Some(key_id))`；未命中 → `Ok(None)`。
    ///
    /// 未命中不是错误：网关对"不存在 / 已停用 / 格式不对"必须给出**同一个** 401，
    /// 否则响应就成了"哪些 key 存在"的探针。
    async fn verify(&self, token: &str) -> Result<Option<String>> {
        let Some(key) = self.keys.find_enabled_by_hash(&hash_token(token)).await? else {
            return Ok(None);
        };
        // 记账失败**不让这次请求失败**：请求已经通过鉴权了，把一个 200 变成 500
        // 去换一个时间戳不划算。打点 + 继续（[14 §6.3]：错误日志必须带 code）。
        if let Err(e) = self.keys.touch_last_used(&key.id, self.clock.now_ms()).await {
            tracing::warn!(
                code = e.code(),
                error = %e,
                "访问密钥的 last_used_at 写入失败（鉴权结果不受影响）"
            );
        }
        Ok(Some(key.id))
    }
}

impl AppState {
    /// 网关装配要的那份实现（`GatewayBoot::assemble` → `RuntimeParts::with_api_keys`）。
    pub fn api_key_authenticator(&self) -> ApiKeyAuthenticator {
        ApiKeyAuthenticator::new(self.api_keys.clone(), self.clock.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ta_core::clock::ManualClock;

    /// 内存库 + 手工时钟（`last_used_at` 必须可断言，不能用系统时间）。
    async fn fixture() -> (Arc<SqliteApiKeyStore>, ApiKeyAuthenticator) {
        let pool = ta_store::open_memory().await.unwrap();
        let store = Arc::new(SqliteApiKeyStore::new(pool));
        let auth =
            ApiKeyAuthenticator::new(store.clone(), Arc::new(ManualClock::new(1_700_000_000_000)));
        (store, auth)
    }

    /// ② 命中：明文换来 key_id，并**真的**把 `last_used_at` 写进库。
    #[tokio::test]
    async fn verify_accepts_a_live_key_and_stamps_last_used() {
        let (store, auth) = fixture().await;
        let created = store.create("客户端", None, 1).await.unwrap();
        let id = created.key.id.clone();

        let hit = auth.verify(&created.plaintext).await.unwrap();
        assert_eq!(hit.as_deref(), Some(id.as_str()), "明文必须换来它的 key_id");

        let back = store.get(&id).await.unwrap().unwrap();
        assert_eq!(back.last_used_at_ms, Some(1_700_000_000_000), "命中要记账");
    }

    /// ③ 三种失败**给出同一个结果**（`Ok(None)`）：不泄漏"哪个 key 不存在"。
    #[tokio::test]
    async fn verify_rejects_disabled_deleted_and_unknown_tokens_identically() {
        let (store, auth) = fixture().await;
        let disabled = store.create("已停用", None, 1).await.unwrap();
        let deleted = store.create("待删除", None, 1).await.unwrap();
        store.set_enabled(&disabled.key.id, false).await.unwrap();
        store.delete(&deleted.key.id).await.unwrap();

        for token in [
            disabled.plaintext.as_str(), // 存在但停用
            deleted.plaintext.as_str(),  // 曾经存在
            "ta-sk-根本没有这把key",     // 从没存在
            "",                          // 空
            "   ",                       // 空白
        ] {
            let got = auth.verify(token).await.unwrap();
            assert_eq!(got, None, "`{token}` 不该通过，也不该给出不同的结果");
        }

        // 停用的那把**没有**被记账（"停用立刻影响鉴权"包括不更新使用时间）
        let back = store.get(&disabled.key.id).await.unwrap().unwrap();
        assert_eq!(back.last_used_at_ms, None);
    }

    /// 停用/删除**立刻**影响鉴权（同一进程、不重启、无缓存）。
    #[tokio::test]
    async fn disabling_or_deleting_takes_effect_immediately() {
        let (store, auth) = fixture().await;
        let created = store.create("马上停", None, 1).await.unwrap();
        assert!(auth.verify(&created.plaintext).await.unwrap().is_some());

        store.set_enabled(&created.key.id, false).await.unwrap();
        assert!(auth.verify(&created.plaintext).await.unwrap().is_none());

        store.set_enabled(&created.key.id, true).await.unwrap();
        assert!(auth.verify(&created.plaintext).await.unwrap().is_some());

        store.delete(&created.key.id).await.unwrap();
        assert!(auth.verify(&created.plaintext).await.unwrap().is_none());
    }

    /// **`any_enabled` 是"`local_key` 没配时还要不要强制鉴权"的判据**，
    /// 所以它必须如实反映库里的启用状态（网关那边据此决定是否强制鉴权）。
    #[tokio::test]
    async fn any_enabled_tracks_the_store() {
        let (store, auth) = fixture().await;
        assert!(!auth.any_enabled().await.unwrap(), "空库 → 不强制鉴权（保持既有行为）");
        let created = store.create("第一把", None, 1).await.unwrap();
        assert!(auth.any_enabled().await.unwrap(), "有一把启用中的 key 就必须鉴权");
        store.set_enabled(&created.key.id, false).await.unwrap();
        assert!(!auth.any_enabled().await.unwrap(), "全停用 → 回到不强制");
    }

    /// **库故障必须 fail closed**：`Err` 而不是 `Ok(None)` / `Ok(false)`。
    ///
    /// 这两种"看起来像正常答案"的返回值在网关那边分别是"这把 key 不存在"与
    /// "不做鉴权"，也就是**放行**。用真关掉的连接池来证，而不是靠注释。
    #[tokio::test]
    async fn a_broken_store_fails_closed_instead_of_passing() {
        let (store, auth) = fixture().await;
        let created = store.create("客户端", None, 1).await.unwrap();

        store.pool().close().await; // 库挂了（等价于磁盘/连接故障）

        assert!(
            auth.verify(&created.plaintext).await.is_err(),
            "查不动时必须报错，不许退回 Ok(None)"
        );
        assert!(
            auth.any_enabled().await.is_err(),
            "查不动时必须报错，不许退回 Ok(false)（那等于关掉鉴权）"
        );
    }

    /// 明文**不进日志**：断言那条 warn 的字段形态（`code` + `error`），
    /// 且源码里没有任何一处把 token 格式化进日志/消息。
    /// 源码级断言比"我保证"可靠（`docs/14 §11`：日志文件是要被分享出去的）。
    #[test]
    fn the_warn_site_never_formats_the_token() {
        let src = include_str!("apikey_auth.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(production.contains("code = e.code()"), "错误日志必须带 code（[14 §6.3]）");
        for needle in ["token = %token", "error = %token", "{token}", "plaintext = %"] {
            assert!(!production.contains(needle), "明文 token 不许进日志：{needle}");
        }
    }
}

//! `api_key_*`：**我们网关对外的访问密钥**。
//!
//! 五条命令：`api_key_list` / `api_key_create` / `api_key_set_enabled` / `api_key_delete` /
//! `api_key_usage`。存储全在 `ta_store::api_key`（本层只做参数校验、错误映射、
//! 以及"写完回读"），用量复用**既有**的 `api_key_usage` 表与既有的领域类型
//! `ApiKeyUsageRow`（`ta_store::usage_store`）—— 不在这里另造一份 DTO。
//!
//! **§3.3 的鉴权不在这里**：那半边在 `crate::apikey_auth`（明文 → key_id 的查库实现，
//! 实现网关的 `ApiKeyVerifier` 接缝）与 `crate::gateway::runtime_parts`（装配点）。
//! 本文件只负责"人能管理这些钥匙"。

use tauri::State;

use ta_core::error::TaError;
use ta_store::usage_store::ApiKeyUsageRow;
use ta_store::{ApiKeyCreated, ApiKeyView};

use crate::commands::{IpcError, IpcResult};
use crate::state::AppState;

/// 建一把访问密钥。名字去掉首尾空白后不能为空；备注同理（空串等价于没写）。
///
/// 返回里的 `plaintext` **只此一次** —— 库里只有 `sha256`，之后再也取不回来。
pub async fn create_api_key(
    state: &AppState,
    name: &str,
    note: Option<&str>,
) -> IpcResult<ApiKeyCreated> {
    let name = name.trim();
    if name.is_empty() {
        // 语义最接近的既有变体就是 MalformedRequest（TA-1004）。不自己发明错误码。
        return Err(IpcError::from(TaError::MalformedRequest(
            "访问密钥的名字不能为空".to_string(),
        )));
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    // 时钟是注入的：不在这里读系统时间（与 usage_store / trace_store 同一口径）。
    Ok(state.api_keys.create(name, note, state.clock.now_ms()).await?)
}

pub async fn list_api_keys(state: &AppState) -> IpcResult<Vec<ApiKeyView>> {
    Ok(state.api_keys.list().await?)
}

/// 启停一把 key，并**回读库里的真值**（不是把入参当结果回给调用方）。
///
/// 改不存在的 id → 报错（TA-1004）。静默成功会让界面显示一个并不存在的状态。
pub async fn set_api_key_enabled(
    state: &AppState,
    id: &str,
    enabled: bool,
) -> IpcResult<ApiKeyView> {
    match state.api_keys.set_enabled(id, enabled).await? {
        Some(view) => Ok(view),
        None => Err(IpcError::from(TaError::MalformedRequest(format!("访问密钥不存在：{id}")))),
    }
}

/// 删一把 key。删不存在的 id → 报错（同上：不能假装成功）。
pub async fn delete_api_key(state: &AppState, id: &str) -> IpcResult<()> {
    if state.api_keys.delete(id).await? {
        Ok(())
    } else {
        Err(IpcError::from(TaError::MalformedRequest(format!("访问密钥不存在：{id}"))))
    }
}

/// 用量：按 id 过滤、**最近的日子优先**、`limit` 真的截断（`limit = 0` → 空，不是"忽略 limit 全给"）。
///
/// 为什么在 Rust 侧排序/截断而不是加一条 SQL：这里复用的是**既有**的
/// `usage_store::api_key_usage(from_day, to_day)`（它已经被成本中心用着），
/// 再写一条"带 id/limit 的变体 SQL"就是同一个查询的第二份实现 —— 两份 SQL 迟早漂移，
/// 而这类漂移（"界面看到的用量与成本中心对不上"）不会有任何东西变红。
///
/// 取全历史用宽区间（`day` 是 `YYYY-MM-DD`，字典序即时间序）。
pub async fn api_key_usage_rows(
    state: &AppState,
    id: Option<&str>,
    limit: u32,
) -> IpcResult<Vec<ApiKeyUsageRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = state.usage.api_key_usage("0000-01-01", "9999-12-31").await?;
    if let Some(id) = id {
        rows.retain(|r| r.key_id == id);
    }
    rows.sort_by(|a, b| {
        b.day
            .cmp(&a.day)
            .then_with(|| a.key_id.cmp(&b.key_id))
            .then_with(|| a.token_kind.cmp(&b.token_kind))
    });
    rows.truncate(limit as usize);
    Ok(rows)
}

// ================================================================ IPC 命令面

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn api_key_list(state: State<'_, AppState>) -> IpcResult<Vec<ApiKeyView>> {
    list_api_keys(&state).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn api_key_create(
    state: State<'_, AppState>,
    name: String,
    note: Option<String>,
) -> IpcResult<ApiKeyCreated> {
    create_api_key(&state, &name, note.as_deref()).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn api_key_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> IpcResult<ApiKeyView> {
    set_api_key_enabled(&state, &id, enabled).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn api_key_delete(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    delete_api_key(&state, &id).await
}

#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn api_key_usage(
    state: State<'_, AppState>,
    id: Option<String>,
    limit: u32,
) -> IpcResult<Vec<ApiKeyUsageRow>> {
    api_key_usage_rows(&state, id.as_deref(), limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::AppState;
    use ta_core::types::{TokenKind, TokenUsage};

    /// 一个**真的库**（临时目录里的 SQLite）：这些用例要证明的正是"库里到底存了什么"，
    /// 所以不许用替身。
    async fn fresh_state(tag: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("ta-apikey-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        AppState::bootstrap(dir.join("db.sqlite")).await.expect("开库")
    }

    /// 建一把 key，返回 `(id, 明文)`。
    async fn seed(state: &AppState, name: &str) -> (String, String) {
        let created = create_api_key(state, name, None).await.expect("建 key");
        (created.key.id, created.plaintext)
    }

    /// **`api_key_list`：不含明文、不含 hash。**
    ///
    /// 这是 §3.2 对这条命令唯一的硬要求，所以它单独一条用例、并且从**三个角度**断言：
    /// 值里没有明文、值里没有 sha256、序列化后的字段名里两个都没有。
    #[tokio::test]
    async fn api_key_list_returns_rows_without_plaintext_or_hash() {
        let state = fresh_state("list").await;
        let (_, plain) = seed(&state, "客户端 A").await;

        let rows = list_api_keys(&state).await.expect("列表");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "客户端 A");
        assert_eq!(rows[0].key_prefix, format!("{}…", &plain[..12]));
        assert!(rows[0].enabled);
        assert_eq!(rows[0].last_used_at_ms, None, "从未用过 = null（不是 0）");

        let json = serde_json::to_string(&rows).unwrap();
        assert!(!json.contains(&plain), "列表里出现了明文：{json}");
        assert!(
            !json.contains(&ta_store::api_key::hash_token(&plain)),
            "列表里出现了 key_hash：{json}"
        );
        let value = serde_json::to_value(&rows[0]).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("key_hash") && !obj.contains_key("plaintext"));
        assert!(!obj.contains_key("key"), "ApiKeyView 里不许出现明文那一列");
    }

    /// **`api_key_create`：明文只出现一次**，且库里只有它的 sha256。
    #[tokio::test]
    async fn api_key_create_returns_plaintext_exactly_once() {
        let state = fresh_state("create").await;
        let created = create_api_key(&state, " 客户端 B ", Some(" 台式机 ")).await.expect("建 key");

        assert!(created.plaintext.starts_with("ta-sk-"));
        assert_eq!(created.key.name, "客户端 B", "名称两端空白要去掉");
        assert_eq!(created.key.note.as_deref(), Some("台式机"), "备注两端空白要去掉");

        let stored: String = sqlx::query_scalar("SELECT key_hash FROM api_key WHERE id = ?")
            .bind(&created.key.id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(stored, ta_store::api_key::hash_token(&created.plaintext));
        let plaintext_in_db: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM api_key WHERE name = ? OR key_prefix = ? OR key_hash = ?",
        )
        .bind(&created.plaintext)
        .bind(&created.plaintext)
        .bind(&created.plaintext)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(plaintext_in_db, 0, "明文不许落库");

        // 再列一次：明文不再可得（这就是"只此一次"的可验证形态）
        let again = list_api_keys(&state).await.unwrap();
        assert!(!serde_json::to_string(&again).unwrap().contains(&created.plaintext));

        // 空名称要拦住：列表里靠名称区分是哪一把 key
        assert_eq!(create_api_key(&state, "   ", None).await.unwrap_err().code, "TA-1004");
        // 空备注按"没填"处理（页面清空输入框会发来空串）
        let bare = create_api_key(&state, "无备注", Some("")).await.unwrap();
        assert_eq!(bare.key.note, None);
    }

    /// **`api_key_set_enabled`：真的改库并回读那一行**（返回值就是库里的真值）。
    #[tokio::test]
    async fn api_key_set_enabled_writes_and_reads_back() {
        let state = fresh_state("set-enabled").await;
        let (id, _) = seed(&state, "要停用的").await;

        let off = set_api_key_enabled(&state, &id, false).await.expect("停用");
        assert!(!off.enabled);
        let raw: i64 = sqlx::query_scalar("SELECT enabled FROM api_key WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(raw, 0, "库里必须真的写成 0（不是只改返回值）");

        let on = set_api_key_enabled(&state, &id, true).await.expect("启用");
        assert!(on.enabled, "回读的是库里的真值");
        assert!(list_api_keys(&state).await.unwrap()[0].enabled);

        let err = set_api_key_enabled(&state, "不存在的-key", false).await.unwrap_err();
        assert_eq!(err.code, "TA-1004", "改不存在的 key 必须报错，不许静默成功");
    }

    /// **`api_key_delete`：删掉那一行；删不存在的 id 要报错**（不能假装成功）。
    #[tokio::test]
    async fn api_key_delete_removes_and_reports_missing() {
        let state = fresh_state("delete").await;
        let (id, _) = seed(&state, "要删的").await;

        delete_api_key(&state, &id).await.expect("删除");
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM api_key").fetch_one(&state.db).await.unwrap();
        assert_eq!(n, 0, "行必须真的没了");
        assert!(list_api_keys(&state).await.unwrap().is_empty());

        let err = delete_api_key(&state, &id).await.unwrap_err();
        assert_eq!(err.code, "TA-1004", "删不存在的 key 必须报错");
    }

    /// **`api_key_usage`：按 id 过滤、`limit` 真的截断、最近的日子优先**，
    /// 且返回的就是既有领域类型（`ApiKeyUsageRow`：按 token_kind 分行，不聚合掉它）。
    #[tokio::test]
    async fn api_key_usage_filters_caps_and_prefers_recent_days() {
        let state = fresh_state("usage").await;
        let (old_key, _) = seed(&state, "老客户端").await;
        let (new_key, _) = seed(&state, "新客户端").await;
        let now = state.clock.now_ms();
        let three_days_ago = now - 3 * 24 * 3600 * 1000;

        let mut usage = TokenUsage::new();
        usage.add(TokenKind::Input, 100);
        usage.add(TokenKind::Output, 40);
        state
            .usage
            .record_api_key_usage(&old_key, three_days_ago, &usage, 0.5, "USD")
            .await
            .unwrap();
        state.usage.record_api_key_usage(&new_key, now, &usage, 0.25, "USD").await.unwrap();

        // id 给了 → 只有这个 key 的行，而且 token_kind 没被聚合掉
        let only_old = api_key_usage_rows(&state, Some(&old_key), 100).await.unwrap();
        assert_eq!(only_old.len(), 2, "input / output 各一行");
        assert!(only_old.iter().all(|r| r.key_id == old_key));
        assert!(only_old.iter().any(|r| r.token_kind == "input" && r.tokens == 100));
        assert!(only_old.iter().any(|r| r.token_kind == "output" && r.tokens == 40));
        assert!(only_old.iter().all(|r| r.currency == "USD"));

        // id 省略 = 全部
        let all = api_key_usage_rows(&state, None, 100).await.unwrap();
        assert_eq!(all.len(), 4);

        // limit 截断的是**最近的行**
        let newest = api_key_usage_rows(&state, None, 1).await.unwrap();
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].key_id, new_key, "limit 只留最新的那一行");

        // limit = 0 → 空（不是"忽略 limit 全给"）
        assert!(api_key_usage_rows(&state, None, 0).await.unwrap().is_empty());
        // 未知 id → 空列表（不是报错：这不是"改一个不存在的实体"）
        assert!(api_key_usage_rows(&state, Some("不存在"), 10).await.unwrap().is_empty());
    }
}

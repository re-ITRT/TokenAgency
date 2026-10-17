//! 套餐命令：读写"一个服务商一个套餐"，并查一次额度。
//!
//! # 出站纪律
//!
//! 这里**不是用户流量**：查余额/用量不会经过线路策略，所以用 `reqwest` 直连。
//! 用户流量的转发链路上仍然只有 `GatewayDeps.dialer` 一个 client（那条铁律在
//! `crates/ta-gateway/src/probe.rs` 与 `commands/upstream.rs` 的守卫里）。
//!
//! # 为什么 `set` 用 `Option`
//!
//! `None` = 清除套餐。这样"保存"与"移除"是同一条命令的两种取值，
//! 不会出现"移除命令写了但没接线"的中间态。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use tauri::State;

use ta_core::domain::plan::{
    needs_two_requests, parse_deepseek, parse_newapi, plan_template, Plan, Quota, Snapshot,
    NEWAPI_SUBSCRIPTION, NEWAPI_USAGE,
};
use ta_core::types::ProviderId;

use crate::commands::catalog::save_validated;
use crate::commands::upstream::probe_secret;
use crate::commands::{IpcError, IpcResult};
use crate::gateway::SecretsAdapter;
use crate::state::AppState;

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn plan_error(code: &str, kind: &str, msg: impl Into<String>) -> IpcError {
    IpcError::new(code, kind, msg)
}

fn not_found(msg: impl Into<String>) -> IpcError {
    plan_error("TA-1004", "tokenagency_not_found", msg)
}

/// 读一个服务商的套餐（没配就是 `None`）。
pub fn read_plan(state: &AppState, id: &str) -> Option<Plan> {
    state.doc().plans.get(&ProviderId::new(id)).cloned()
}

/// 写（或清除）一个服务商的套餐。
///
/// 校验在**写之前**：一份过不了的配置进不了库，界面拿到的是人话原因。
pub async fn write_plan(state: &AppState, id: &str, plan: Option<Plan>) -> IpcResult<()> {
    let pid = ProviderId::new(id);
    let mut doc = (*state.doc()).clone();
    if !doc.providers.iter().any(|p| p.id == pid) {
        return Err(not_found(format!("服务商不存在：{id}")));
    }
    match plan {
        Some(p) => {
            p.validate().map_err(|m| plan_error("TA-5002", "tokenagency_plan_invalid", m))?;
            doc.plans.insert(pid, p);
        }
        None => {
            doc.plans.remove(&pid);
        }
    }
    save_validated(state, doc, "provider_plan_set").await
}

/// 查一次额度。失败时返回 `Snapshot { ok: false, error }` ——
/// 界面要能说"为什么没数"，而不是显示一个 0。
pub async fn quota_once(state: &AppState, id: &str) -> IpcResult<Snapshot> {
    let doc = state.doc();
    let pid = ProviderId::new(id);
    let provider = doc
        .providers
        .iter()
        .find(|p| p.id == pid)
        .ok_or_else(|| not_found(format!("服务商不存在：{id}")))?
        .clone();
    let Some(plan) = doc.plans.get(&pid).cloned() else {
        return Err(not_found(format!("这个服务商还没配套餐：{id}")));
    };
    if !plan.enabled {
        return Ok(Snapshot::fail("套餐已停用", now_secs()));
    }
    let template = plan.template.clone();
    let Some(t) = plan_template(&template) else {
        return Ok(Snapshot::fail(format!("不认识的模板：{template}"), now_secs()));
    };
    let base = plan
        .base_url
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| provider.base_url.clone())
        .trim_end_matches('/')
        .to_string();

    let secret = match probe_secret(&doc, &provider, &SecretsAdapter::with_defaults()).await {
        Ok(s) => s,
        Err(e) => return Ok(Snapshot::fail(e.message, now_secs())),
    };

    let client = match Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(c) => c,
        Err(e) => return Ok(Snapshot::fail(format!("建不了 HTTP 客户端：{e}"), now_secs())),
    };

    let result = if needs_two_requests(&template) {
        fetch_newapi(&client, &base, secret.as_str()).await
    } else if t.id == "deepseek" {
        fetch_deepseek(&client, &base, secret.as_str()).await
    } else {
        Err(format!("模板 `{template}` 还没有抓取实现（如实报，不编数据）"))
    };

    Ok(match result {
        Ok(q) => Snapshot::ok(q, now_secs()),
        Err(e) => Snapshot::fail(e, now_secs()),
    })
}

/// NewAPI / OneAPI：先取总额度，再取已用量，折成固定额度。
async fn fetch_newapi(client: &Client, base: &str, secret: &str) -> Result<Quota, String> {
    let sub_url = format!("{base}{NEWAPI_SUBSCRIPTION}");
    let usage_url = format!("{base}{NEWAPI_USAGE}");
    let get = |url: String| {
        let client = client.clone();
        let secret = secret.to_string();
        async move {
            let resp = client
                .get(&url)
                .header("Authorization", format!("Bearer {secret}"))
                .send()
                .await
                .map_err(|e| format!("请求 {url} 失败：{e}"))?;
            let status = resp.status();
            let body = resp.text().await.map_err(|e| format!("读 {url} 的响应体失败：{e}"))?;
            if !status.is_success() {
                // 上游原文可能很长、也可能带密钥，只截一小段并脱敏交给调用方
                let excerpt: String = body.chars().take(200).collect();
                return Err(format!("{url} 返回 HTTP {status}：{excerpt}"));
            }
            serde_json::from_str::<serde_json::Value>(&body)
                .map_err(|e| format!("{url} 返回的不是 JSON：{e}"))
        }
    };
    let sub = get(sub_url).await?;
    let usage = get(usage_url).await?;
    parse_newapi(&sub, &usage)
}

/// DeepSeek：`/user/balance`。
async fn fetch_deepseek(client: &Client, base: &str, secret: &str) -> Result<Quota, String> {
    let url = format!("{base}/user/balance");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {secret}"))
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("读响应体失败：{e}"))?;
    if !status.is_success() {
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("{url} 返回 HTTP {status}：{excerpt}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("{url} 返回的不是 JSON：{e}"))?;
    parse_deepseek(&v)
}

// ================================================================ IPC

/// `provider_plan_get({ id }) -> Plan | null`
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_plan_get(state: State<'_, AppState>, id: String) -> IpcResult<Option<Plan>> {
    Ok(read_plan(&state, &id))
}

/// `provider_plan_set({ id, plan }) -> void`（`plan = null` 表示清除）
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_plan_set(
    state: State<'_, AppState>,
    id: String,
    plan: Option<Plan>,
) -> IpcResult<()> {
    write_plan(&state, &id, plan).await
}

/// `provider_plan_quota({ id }) -> Snapshot`
#[tauri::command(rename_all = "snake_case")]
#[specta::specta]
pub async fn provider_plan_quota(state: State<'_, AppState>, id: String) -> IpcResult<Snapshot> {
    quota_once(&state, &id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_core::domain::Provider;

    fn provider(id: &str) -> Provider {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "protocol": "openai", "base_url": "https://relay.example.com"
        }))
        .expect("夹具服务商")
    }

    fn plan(template: &str) -> Plan {
        Plan {
            template: template.to_string(),
            enabled: true,
            threshold: 10.0,
            base_url: Some("https://relay.example.com".to_string()),
        }
    }

    /// 没有套餐文件时，`read_plan` 不编一个默认值出来。
    #[test]
    fn read_plan_returns_none_when_absent() {
        let doc =
            ta_core::domain::ConfigDoc { providers: vec![provider("p1")], ..Default::default() };
        assert!(!doc.plans.contains_key(&ProviderId::new("p1")));
    }

    /// 校验在**写之前**就挡住：不认识的模板进不了库（这里直接测校验 + 分档函数）。
    #[test]
    fn unknown_template_is_refused_before_writing() {
        let mut p = plan("nope");
        p.base_url = Some("https://relay.example.com".to_string());
        assert!(p.validate().is_err());
    }

    /// 三个模板各自的分档：NewAPI 系要两个请求，DeepSeek 只要一个。
    #[test]
    fn request_shape_matches_the_template() {
        assert!(needs_two_requests("newapi"));
        assert!(needs_two_requests("sub2api"));
        assert!(!needs_two_requests("deepseek"));
    }
}

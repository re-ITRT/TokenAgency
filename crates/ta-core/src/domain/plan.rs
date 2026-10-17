//! 套餐：**一个服务商最多一个**的用量/余额显示配置。
//!
//! # 为什么长这样
//!
//! 这个软件的核心是"一个 URL 聚合多家 API + 智能走线路"。套餐不是核心，
//! 而是**围绕核心的体验**：用户想知道"我这家的额度还剩多少"。
//!
//! 数据形状取自 `zander-zyx/coding-plan-limit`（MIT，见 `THIRD-PARTY-NOTICES.md`）：
//! 它把"窗口型 / 余额型 / 固定额度型"三种额度想透了，界面因此可以
//! **有啥显示啥、不支持的不报错**。我们照这个形状，但只保留**中转站能查的**模板
//! （NewAPI 系 / Sub2API / DeepSeek 余额）—— 读本机 CLI 凭据那类不做：
//! 那是"碰系统文件"，与薄壳定位冲突。
//!
//! # 一一对应是**结构**事实
//!
//! `Provider.plan: Option<Plan>` —— 一个服务商最多一个套餐，不需要唯一性扫描，
//! 也没有第二处索引可以漂移。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 一个统计窗口（5 小时 / 本周 / 本月）的用量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WindowQuota {
    /// 窗口标签（上游给什么显示什么，如 `5小时` / `7天` / `本月`）
    pub label: String,
    /// 已用百分比（0–100）
    pub used_percent: f64,
    /// 重置时刻（unix 秒）；没有明确重置时刻就不给
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
}

/// 统一额度表示：窗口型 / 余额型 / 固定额度型。
///
/// 三种都**允许缺**：上游不支持就别显示，不编一个 0 出来。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum Quota {
    /// 多窗口百分比（Coding Plan 类）
    Windows { windows: Vec<WindowQuota> },
    /// 账户余额（按量计费类）
    Balance {
        amount: f64,
        currency: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// 固定总额度（Token Plan 类）
    Fixed {
        used_percent: f64,
        used: f64,
        total: f64,
        unit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_at: Option<i64>,
    },
}

impl Quota {
    /// 已用百分比（余额型没有百分比概念 → `None`，**不折算**成一个假的百分比）。
    pub fn used_percent(&self) -> Option<f64> {
        match self {
            Quota::Windows { windows } => windows.iter().map(|w| w.used_percent).reduce(f64::max),
            Quota::Fixed { used_percent, .. } => Some(*used_percent),
            Quota::Balance { .. } => None,
        }
    }
}

/// 一次查询的结果快照。失败时带原因 —— 界面要能说"为什么没数"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Snapshot {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
    /// 这次查询的时刻（unix 秒）
    pub updated_at: i64,
}

impl Snapshot {
    pub fn ok(quota: Quota, now: i64) -> Self {
        Self { ok: true, error: None, quota: Some(quota), updated_at: now }
    }

    pub fn fail(error: impl Into<String>, now: i64) -> Self {
        Self { ok: false, error: Some(error.into()), quota: None, updated_at: now }
    }
}

/// 内置模板的元数据（前端靠它渲染"选模板"网格 + 动态显隐表单）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlanTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// 是否需要填站点地址（NewAPI 系必填）
    pub needs_base_url: bool,
}

/// 只做**中转站能查**的三个模板。加模板前先问：它的额度接口是不是"填地址 + key 就能查"。
pub const PLAN_TEMPLATES: &[PlanTemplate] = &[
    PlanTemplate {
        id: "newapi",
        name: "NewAPI / OneAPI",
        description: "站点余额与用量（需兼容 /v1/dashboard/billing）",
        needs_base_url: true,
    },
    PlanTemplate {
        id: "sub2api",
        name: "Sub2API",
        description: "站点余额（需兼容 OpenAI 计费接口）",
        needs_base_url: true,
    },
    PlanTemplate {
        id: "deepseek",
        name: "DeepSeek",
        description: "账户余额（官方 API）",
        needs_base_url: false,
    },
];

/// 按 id 找模板。
pub fn plan_template(id: &str) -> Option<&'static PlanTemplate> {
    PLAN_TEMPLATES.iter().find(|t| t.id == id)
}

/// 一个服务商的套餐配置。**密钥不在这里**：凭据走服务商自己的 `credentials[]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Plan {
    /// [`PLAN_TEMPLATES`] 里的 id
    pub template: String,
    /// 关掉只是不显示，不删配置
    #[serde(default = "plan_default_true")]
    pub enabled: bool,
    /// 提醒阈值：窗口/固定额度型 = 剩余百分比下限；余额型 = 金额下限
    #[serde(default = "plan_default_threshold")]
    pub threshold: f64,
    /// 覆盖查询地址（NewAPI 系填站点根地址；`None` = 用服务商的 `base_url`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

fn plan_default_true() -> bool {
    true
}

/// 默认提醒阈值 10%：窗口/固定额度型是"剩余百分比下限"，余额型是"金额下限"。
fn plan_default_threshold() -> f64 {
    10.0
}

impl Plan {
    /// 校验：模板必须认识；阈值落在 0–100；需要站点地址的模板必须给地址。
    ///
    /// 返回人话错误 —— 命令层直接把它交给界面，不让用户猜。
    pub fn validate(&self) -> Result<(), String> {
        let Some(t) = plan_template(&self.template) else {
            let known: Vec<&str> = PLAN_TEMPLATES.iter().map(|x| x.id).collect();
            return Err(format!(
                "不认识的套餐模板 `{}`（可用：{}）",
                self.template,
                known.join(" / ")
            ));
        };
        if !(0.0..=100.0).contains(&self.threshold) {
            return Err(format!("提醒阈值必须落在 0–100，收到 {}", self.threshold));
        }
        if t.needs_base_url {
            match self.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(url) if url.starts_with("http") => {}
                Some(_) => return Err("站点地址必须以 http:// 或 https:// 开头".to_string()),
                None => {
                    return Err(format!("`{}` 模板必须填站点地址", t.name));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(template: &str) -> Plan {
        Plan { template: template.to_string(), enabled: true, threshold: 10.0, base_url: None }
    }

    /// 三个模板必须都能通过**它自己要求**的校验（防止模板表和校验器漂移）。
    #[test]
    fn every_builtin_template_passes_its_own_validation() {
        for t in PLAN_TEMPLATES {
            let mut p = plan(t.id);
            if t.needs_base_url {
                p.base_url = Some("https://relay.example.com".to_string());
            }
            p.validate().unwrap_or_else(|e| panic!("模板 {} 自己的配置过不了校验：{e}", t.id));
        }
    }

    #[test]
    fn unknown_template_is_an_honest_error_naming_the_known_ones() {
        let err = plan("nope").validate().unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("newapi"), "错误里要列出可用模板：{err}");
    }

    #[test]
    fn needs_base_url_template_refuses_without_one() {
        let err = plan("newapi").validate().unwrap_err();
        assert!(err.contains("站点地址"), "{err}");
    }

    #[test]
    fn base_url_must_look_like_a_url() {
        let mut p = plan("newapi");
        p.base_url = Some("relay.example.com".to_string());
        assert!(p.validate().unwrap_err().contains("http"));
    }

    #[test]
    fn threshold_out_of_range_is_rejected() {
        let mut p = plan("deepseek");
        p.threshold = 120.0;
        assert!(p.validate().unwrap_err().contains("0–100"));
    }

    /// 余额型**不给**假的百分比：折算成百分比就是编数据。
    #[test]
    fn balance_has_no_used_percent() {
        let q = Quota::Balance { amount: 3.5, currency: "CNY".into(), note: None };
        assert_eq!(q.used_percent(), None);
    }

    /// 窗口型取**最坏**的那一个（界面的"还剩多少"要按最紧的窗口算）。
    #[test]
    fn windows_report_the_worst_window() {
        let q = Quota::Windows {
            windows: vec![
                WindowQuota { label: "5小时".into(), used_percent: 20.0, reset_at: None },
                WindowQuota { label: "7天".into(), used_percent: 88.0, reset_at: Some(1) },
            ],
        };
        assert_eq!(q.used_percent(), Some(88.0));
    }
}

// ================================================================ 解析（纯函数，无 IO）
//
// 抓取在 `src-tauri`（那是唯一允许出网的地方），解析在这里：
// 分档、单位、缺字段怎么算，全是**领域判断**，放 `ta-core` 才能不进网络就测。

/// NewAPI / OneAPI 的两个端点（拼在站点根后面）。
///
/// 只认这两个：站点不兼容就是"查不了"，如实报错，不去猜别的路径。
pub const NEWAPI_SUBSCRIPTION: &str = "/v1/dashboard/billing/subscription";
pub const NEWAPI_USAGE: &str = "/v1/dashboard/billing/usage";

/// 把 NewAPI 的 `subscription` + `usage` 折成**固定额度**。
///
/// `usage` 里的 `total_usage` 单位是**美分**（OpenAI 计费接口的约定），
/// `subscription.hard_limit_usd` 是美元 —— 两者不同单位，混着减会差 100 倍。
pub fn parse_newapi(sub: &serde_json::Value, usage: &serde_json::Value) -> Result<Quota, String> {
    let total = sub
        .get("hard_limit_usd")
        .or_else(|| sub.get("soft_limit_usd"))
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "站点没返回 hard_limit_usd（不是 NewAPI 计费接口？）".to_string())?;
    let used_cents = usage
        .get("total_usage")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "站点没返回 total_usage（计费接口不兼容？）".to_string())?;
    let used = used_cents / 100.0;
    if total <= 0.0 {
        return Err(format!("站点给出的总额度是 {total}，除不出百分比"));
    }
    Ok(Quota::Fixed {
        used_percent: (used / total * 100.0).clamp(0.0, 100.0),
        used,
        total,
        unit: "USD".to_string(),
        reset_at: None,
    })
}

/// DeepSeek 余额：`balance_infos[]` 里**优先人民币**，没有就取第一条。
pub fn parse_deepseek(body: &serde_json::Value) -> Result<Quota, String> {
    let infos = body
        .get("balance_infos")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "DeepSeek 没返回 balance_infos（key 或接口变了？）".to_string())?;
    let picked = infos
        .iter()
        .find(|x| x.get("currency").and_then(serde_json::Value::as_str) == Some("CNY"))
        .or_else(|| infos.first())
        .ok_or_else(|| "DeepSeek 的 balance_infos 是空的".to_string())?;
    let amount = picked
        .get("total_balance")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| "DeepSeek 的 total_balance 不是数字".to_string())?;
    let currency =
        picked.get("currency").and_then(serde_json::Value::as_str).unwrap_or("CNY").to_string();
    Ok(Quota::Balance { amount, currency, note: None })
}

/// 这个模板要不要发第二个请求（NewAPI 系要，余额型不要）。
pub fn needs_two_requests(template: &str) -> bool {
    matches!(template, "newapi" | "sub2api")
}

/// 模板的凭证头：这三个模板都用 `Authorization: Bearer`。
pub fn bearer_secret(template: &str) -> bool {
    matches!(template, "newapi" | "sub2api" | "deepseek")
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn newapi_maps_cents_to_dollars() {
        let sub = serde_json::json!({ "hard_limit_usd": 100.0 });
        // 25.00 美元 → 2500 美分
        let usage = serde_json::json!({ "total_usage": 2500.0 });
        let q = parse_newapi(&sub, &usage).expect("应当解析成功");
        match q {
            Quota::Fixed { used_percent, used, total, unit, .. } => {
                assert_eq!(unit, "USD");
                assert_eq!(total, 100.0);
                assert_eq!(used, 25.0);
                assert!((used_percent - 25.0).abs() < 1e-9, "{used_percent}");
            }
            other => panic!("应当是 Fixed：{other:?}"),
        }
    }

    #[test]
    fn newapi_refuses_when_fields_are_missing() {
        assert!(parse_newapi(&serde_json::json!({}), &serde_json::json!({ "total_usage": 1.0 }))
            .is_err());
        assert!(parse_newapi(
            &serde_json::json!({ "hard_limit_usd": 10.0 }),
            &serde_json::json!({})
        )
        .is_err());
    }

    #[test]
    fn newapi_zero_total_is_an_error_not_a_division_by_zero() {
        let sub = serde_json::json!({ "hard_limit_usd": 0.0 });
        assert!(parse_newapi(&sub, &serde_json::json!({ "total_usage": 1.0 })).is_err());
    }

    #[test]
    fn deepseek_prefers_cny() {
        let body = serde_json::json!({
            "is_available": true,
            "balance_infos": [
                { "currency": "USD", "total_balance": "1.5" },
                { "currency": "CNY", "total_balance": "12.34" }
            ]
        });
        match parse_deepseek(&body).expect("应当解析成功") {
            Quota::Balance { amount, currency, .. } => {
                assert_eq!(currency, "CNY");
                assert!((amount - 12.34).abs() < 1e-9);
            }
            other => panic!("应当是 Balance：{other:?}"),
        }
    }

    #[test]
    fn deepseek_missing_infos_is_an_honest_error() {
        assert!(parse_deepseek(&serde_json::json!({})).is_err());
        assert!(parse_deepseek(&serde_json::json!({ "balance_infos": [] })).is_err());
    }
}

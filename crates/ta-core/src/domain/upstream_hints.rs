//! [docs/46 §303] **上游声明过的能力** —— 从扫描存下来的原文里读出来，供我方线路"认领"。
//!
//! 为什么要有这一层（用户原话「服务商 → 软件（软件保存信息）↔ 本机访问 没有打通」）：
//!
//! * 上游（火山 Ark 等）在 `/models` 里**明明白白写了**上下文窗口与模态：
//!   ```json
//!   { "token_limits": { "context_window": 32768, "max_input_token_length": 32768,
//!                       "max_output_token_length": 16384 },
//!     "modalities": { "input_modalities": ["text"] },
//!     "features": { "tools": { "function_calling": true } } }
//!   ```
//! * 我们**逐字存了**这段原文（`provider_model_catalog.raw_json`，[docs/28 §3]「原文逐字留」），
//!   但**从来没读过** ⇒ 线路的能力一直是默认值（`context_tokens = 0`）⇒ 网关
//!   `/v1/models` 无话可说 ⇒ 客户端（DeepSeek Harness 的「获取可用模型」）那一栏永远是空的。
//!
//! 纪律（本仓库一贯：**一个都不编**）：
//!
//! 1. **只认识确认过的键**（火山 Ark 的实测形状），键名或类型对不上就当没看见；
//! 2. 读不到就是 `None` —— **不是 0、不是猜**；
//! 3. `vision` 由 `input_modalities` 里有没有 `image` **推导**（这是同一份事实的两种说法，
//!    不是编造）；`tools` 只认 `features.tools.function_calling`。

use serde_json::Value;

/// 上游一条模型记录里**声明**出来的能力。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpstreamModelHints {
    /// 上下文窗口（`token_limits.context_window`）。
    pub context_window: Option<u64>,
    /// 最大输出（`token_limits.max_output_token_length`）。
    pub max_output_tokens: Option<u64>,
    /// 输入模态（`modalities.input_modalities`，原样小写去重）。
    pub input_modalities: Vec<String>,
    /// 工具调用（`features.tools.function_calling`）。
    pub tools: Option<bool>,
}

impl UpstreamModelHints {
    /// 从上游原文对象里读。**只认白名单键**，其它字段一律不看。
    pub fn from_upstream_raw(raw: &Value) -> Self {
        // 上下文窗口：火山写 `token_limits.context_window`；别家写 `context_length` / `max_input_tokens`。
        let context_window = pick_u64(
            raw,
            &[
                &["token_limits", "context_window"],
                &["token_limits", "max_input_token_length"],
                &["context_length"],
                &["context_window"],
                &["max_input_tokens"],
            ],
        );

        // 最大输出：火山写 `token_limits.max_output_token_length`。
        let max_output_tokens = pick_u64(
            raw,
            &[
                &["token_limits", "max_output_token_length"],
                &["token_limits", "max_output_tokens"],
                &["max_output_tokens"],
                &["max_completion_tokens"],
                &["top_provider", "max_completion_tokens"],
            ],
        );

        // 输入模态：`modalities.input_modalities`（火山）。
        let mut input_modalities: Vec<String> = Vec::new();
        if let Some(list) =
            raw.get("modalities").and_then(|m| m.get("input_modalities")).and_then(Value::as_array)
        {
            for item in list {
                if let Some(s) = item.as_str() {
                    let s = s.trim().to_ascii_lowercase();
                    if !s.is_empty() && !input_modalities.contains(&s) {
                        input_modalities.push(s);
                    }
                }
            }
        }

        let tools = raw
            .get("features")
            .and_then(|f| f.get("tools"))
            .and_then(|t| t.get("function_calling"))
            .and_then(Value::as_bool);

        Self { context_window, max_output_tokens, input_modalities, tools }
    }

    /// 上游有没有说任何一件我们认得的事（一件都没有 ⇒ 不碰线路）。
    pub fn is_empty(&self) -> bool {
        self.context_window.is_none()
            && self.max_output_tokens.is_none()
            && self.input_modalities.is_empty()
            && self.tools.is_none()
    }

    /// 上游声明的输入模态里有没有图片 ⇒ 这个模型支持视觉输入。
    ///
    /// 这是**同一份事实的推导**（模态里有 `image` 就等于能看图），不是另编一个字段。
    pub fn vision(&self) -> Option<bool> {
        if self.input_modalities.is_empty() {
            return None;
        }
        Some(self.input_modalities.iter().any(|m| m == "image"))
    }

    /// 把声明过的字段**逐字段**补进一份能力声明；返回补了几处。
    ///
    /// 唯一实现：`ConfigDoc::adopt_upstream_hints`（扫描路径）与 `channel_upsert`（新建线路路径）
    /// 共用这一份口径，免得两条写路径各有一套。
    ///
    /// 规则：只补**没声明**的 —— `context_tokens == 0` / `max_output_tokens == None` /
    /// `modalities` 还是默认的 `["text"]` / `tools`、`vision` 还是 `false`；
    /// 其中 `tools`、`vision` **只可能从 `false` 变成 `true`**（结构里 `false` 与"没填"同值，
    /// 不拿上游的 `false` 覆盖）。
    pub fn adopt_into(&self, caps: &mut super::ChannelCapabilities) -> usize {
        if self.is_empty() {
            return 0;
        }
        let mut filled = 0usize;
        if caps.context_tokens == 0 {
            if let Some(value) = self.context_window {
                caps.context_tokens = value;
                filled += 1;
            }
        }
        if caps.max_output_tokens.is_none() {
            if let Some(value) = self.max_output_tokens {
                caps.max_output_tokens = Some(value);
                filled += 1;
            }
        }
        if caps.modalities == super::default_modalities() && !self.input_modalities.is_empty() {
            caps.modalities = self.input_modalities.clone();
            filled += 1;
        }
        if !caps.tools && self.tools == Some(true) {
            caps.tools = true;
            filled += 1;
        }
        if !caps.vision && self.vision() == Some(true) {
            caps.vision = true;
            filled += 1;
        }
        filled
    }
}

/// 依次尝试若干条路径，取第一个**是正整数**的值（0 与负数都不算"声明"）。
fn pick_u64(raw: &Value, paths: &[&[&str]]) -> Option<u64> {
    for path in paths {
        let mut cursor = raw;
        let mut ok = true;
        for key in *path {
            match cursor.get(*key) {
                Some(next) => cursor = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        if let Some(n) = cursor.as_u64() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 火山 Ark 的**实测**形状（从用户真机库里的 `provider_model_catalog.raw_json` 抄的）。
    fn volcengine_sample() -> Value {
        json!({
            "created": 1725440807,
            "domain": "LLM",
            "features": { "structured_outputs": { "json_object": false },
                          "tools": { "function_calling": true } },
            "id": "doubao-pro-32k-240828",
            "modalities": { "input_modalities": ["text"], "output_modalities": ["text"] },
            "name": "doubao-pro-32k",
            "object": "model",
            "status": "Shutdown",
            "task_type": ["TextGeneration"],
            "token_limits": { "context_window": 32768, "max_input_token_length": 32768,
                              "max_output_token_length": 16384 },
            "version": "240828"
        })
    }

    #[test]
    fn reads_the_volcengine_shape_verbatim() {
        let hints = UpstreamModelHints::from_upstream_raw(&volcengine_sample());
        assert_eq!(hints.context_window, Some(32768));
        assert_eq!(hints.max_output_tokens, Some(16384));
        assert_eq!(hints.input_modalities, vec!["text".to_string()]);
        assert_eq!(hints.tools, Some(true));
        assert_eq!(hints.vision(), Some(false), "只有 text ⇒ 不支持图片");
        assert!(!hints.is_empty());
    }

    #[test]
    fn supports_the_other_names_we_confirm() {
        let hints = UpstreamModelHints::from_upstream_raw(&json!({
            "context_length": 128000,
            "top_provider": { "max_completion_tokens": 8192 },
            "modalities": { "input_modalities": ["TEXT", "image", "image"] }
        }));
        assert_eq!(hints.context_window, Some(128000));
        assert_eq!(hints.max_output_tokens, Some(8192));
        assert_eq!(
            hints.input_modalities,
            vec!["text".to_string(), "image".to_string()],
            "小写 + 去重"
        );
        assert_eq!(hints.vision(), Some(true));
        assert_eq!(hints.tools, None, "没写 tools 就是没声明（不是 false）");
    }

    #[test]
    fn unknown_or_empty_raw_declares_nothing() {
        let empty = UpstreamModelHints::from_upstream_raw(&json!({}));
        assert!(empty.is_empty(), "空对象 ⇒ 什么都不认");
        assert_eq!(empty.context_window, None);
        assert_eq!(empty.vision(), None);

        // 键名对不上 / 类型不对 / 0 ⇒ 一律当没看见（不编）
        let junk = UpstreamModelHints::from_upstream_raw(&json!({
            "token_limits": { "context_window": "32768", "max_output_token_length": 0 },
            "modalities": { "input_modalities": "text" },
            "features": { "tools": { "function_calling": "yes" } }
        }));
        assert!(junk.is_empty(), "字符串/0/非数组都不算声明：{junk:?}");
    }
}

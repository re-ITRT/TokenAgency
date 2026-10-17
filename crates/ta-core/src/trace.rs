//! 请求追踪的**共享值类型**。
//!
//! 为什么放在 `ta-core`：它是 `ta-gateway`（生产者）与 `ta-store`（持久化 + 导出成 TS）
//! 之间的接口。放在任何一侧，另一侧都只能**重复定义一份**，而重复定义迟早会漂移 ——
//! 那时候"网关写进去的形态"和"store 读出来的形态"会不一致，且不会有任何编译错误。
//!
//! **本模块只有数据，没有逻辑**（符合 `ta-core` 不做 IO 的硬约束）。

/// 客户端发的头 vs 上游收到的头的差异。
///
/// **非空 = 网关自己有 bug**（自检，不是策略）。
/// 默认**不会因为差异而拒绝请求**，只写进 `trace.header_diff` 供人排查。
///
/// 形状是**已知的**，所以它用具体类型导出到 TS，而不是 `any`：
/// 前端可以直接把三个列表渲染成"谁多了/谁少了/谁被改了"。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeaderDiff {
    /// 客户端没发、上游却收到了（网关凭空加的）
    #[serde(default)]
    pub added: Vec<String>,
    /// 客户端发了、上游却没收到（网关吞了 —— 这就是"agent 标识被吞"的症状）
    #[serde(default)]
    pub removed: Vec<String>,
    /// 两边都有但值不同：`(头名, 客户端值, 上游值)`
    #[serde(default)]
    pub changed: Vec<(String, String, String)>,
}

impl HeaderDiff {
    /// 完全一致。**这是正常情况下唯一可接受的结果。**
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// 差异条数（写日志/事件时用）。
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len() + self.changed.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_means_all_three_lists_are_empty() {
        assert!(HeaderDiff::default().is_empty());
        assert_eq!(HeaderDiff::default().len(), 0);

        let only_changed = HeaderDiff {
            changed: vec![("x-a".into(), "1".into(), "2".into())],
            ..Default::default()
        };
        assert!(!only_changed.is_empty(), "只要有任一类差异就不算空");
        assert_eq!(only_changed.len(), 1);
    }

    #[test]
    fn serde_shape_is_stable() {
        // 这个形态是**落库的 JSON**（`trace.header_diff` 是 TEXT 列）——
        // 改字段名就等于改历史数据的可读性，所以钉住它。
        let d = HeaderDiff {
            added: vec!["x-extra".into()],
            removed: vec!["x-stainless-lang".into()],
            changed: vec![("host".into(), "127.0.0.1".into(), "api.example.com".into())],
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["added"][0], "x-extra");
        assert_eq!(json["removed"][0], "x-stainless-lang");
        // `changed` 是元组 → JSON 数组（不是对象）。**形状要钉住**：
        // 改成对象会让老数据解析不出来。
        assert_eq!(json["changed"][0][0], "host");
        assert_eq!(json["changed"][0][2], "api.example.com");
        assert_eq!(serde_json::from_value::<HeaderDiff>(json).unwrap(), d);
    }
}

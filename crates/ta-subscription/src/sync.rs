//! 订阅同步编排：**fetch → 坏更新拦截 → 版本记录 / 回滚**。
//!
//! 这是其它层（网关/托盘/UI）真正该调用的入口 —— 它把 [12 §4] 的五件事串起来：
//!
//! ```text
//! fetch（独立 Client，不走 lane）
//!   └─ 内容不可用（HTML / 认不出 / 空列表 / 节点数骤降）
//!        └─ 拒绝采纳 → 留档 rejected 版本 → 当前生效的仍是上一版已采纳版本
//!   └─ 内容可用
//!        └─ 采纳 → 记为新的已采纳版本
//! 下载失败（连不上 / 超时 / 5xx / 401）
//!   └─ 不产生新版本 → 当前生效的仍是上一版
//! ```
//!
//! 两个刻意的设计：
//! * **比对基准是"上一版已采纳的内容"，不是"上一次抓到的内容"** —— 否则连续两次坏更新
//!   会把基准拉到很低的节点数，坏更新就再也拦不住了；
//! * **坏更新也留档**（`accepted = false`）：UI 能解释"这次为什么没采纳"，
//!   而且版本保留策略保证这份留档**不会挤掉**最后一份好版本（见 [`crate::version`]）。
//!
//! `subscription:updated` 事件（[11 §3]）由调用方发布：本 crate 不依赖 `EventBus`（[11 §1.1]
//! 不跨 crate 用别人的具体类型），[`SyncOutcome`] 里该有的都有（ok / reason / 版本号）。

use ta_core::domain::Subscription;
use ta_core::error::Result;

use crate::version::{VersionKeeper, VersionMeta, VersionStore};
use crate::{AcceptDecision, FetchOutcome, SubscriptionFetcher, SubscriptionValidator};

/// 一次同步的结果。**`effective` 才是"现在真正生效的内容"**（可能来自上一版）。
#[derive(Debug, Clone)]
pub struct SyncOutcome {
    pub subscription: String,
    pub decision: AcceptDecision,
    /// 本次留档的版本号（下载失败时为 `None`：没抓到东西，没什么可记）
    pub recorded_version: Option<u64>,
    /// 当前生效的版本号
    pub effective_version: Option<u64>,
    /// 当前生效的内容（采纳=本次抓到的；拒绝/失败=上一版已采纳的）
    pub effective: Option<FetchOutcome>,
    /// 是否发生了"保留上版"（拒绝或失败且确实回退到了旧版本）
    pub rolled_back: bool,
}

impl SyncOutcome {
    pub fn accepted(&self) -> bool {
        self.decision.is_accept()
    }

    pub fn reason(&self) -> Option<&str> {
        self.decision.reason()
    }

    /// 给 UI 的一句话
    pub fn summary(&self) -> String {
        match &self.decision {
            AcceptDecision::Accept => match self.effective.as_ref() {
                Some(o) => {
                    format!("订阅已更新（{} 个节点 / {} 个分组）", o.node_count(), o.group_count())
                }
                None => "订阅已更新".to_string(),
            },
            AcceptDecision::Reject(why) => match self.effective_version {
                Some(v) => format!("订阅未采纳（{why}），继续使用版本 #{v}"),
                None => format!("订阅未采纳（{why}），当前没有可用版本"),
            },
        }
    }
}

/// `SubscriptionFetcher` + `SubscriptionValidator` + 版本存储 的编排。
pub struct SubscriptionSync<F, V, S>
where
    F: SubscriptionFetcher,
    V: SubscriptionValidator,
    S: VersionStore,
{
    fetcher: F,
    validator: V,
    keeper: VersionKeeper<S>,
}

impl<F, V, S> SubscriptionSync<F, V, S>
where
    F: SubscriptionFetcher,
    V: SubscriptionValidator,
    S: VersionStore,
{
    pub fn new(fetcher: F, validator: V, keeper: VersionKeeper<S>) -> Self {
        Self { fetcher, validator, keeper }
    }

    pub fn keeper(&self) -> &VersionKeeper<S> {
        &self.keeper
    }

    /// 当前生效的（最近一版**已采纳**的）内容
    pub fn effective(&self, sub: &Subscription) -> Result<Option<(VersionMeta, FetchOutcome)>> {
        self.keeper.latest_accepted(&sub.id)
    }

    /// 同步一次订阅。**任何失败都不动已生效的版本**。
    pub async fn sync(&self, sub: &Subscription) -> Result<SyncOutcome> {
        let next = match self.fetcher.fetch(sub).await {
            Ok(next) => next,
            Err(e) => {
                // 下载失败：不记版本（没有内容可记），保留上版
                let effective = self.keeper.latest_accepted(&sub.id)?;
                return Ok(SyncOutcome {
                    subscription: sub.id.to_string(),
                    decision: AcceptDecision::Reject(format!("下载失败：{e}")),
                    recorded_version: None,
                    effective_version: effective.as_ref().map(|(m, _)| m.version),
                    effective: effective.map(|(_, o)| o),
                    rolled_back: true,
                });
            }
        };

        // ★ 基准 = 上一版**已采纳**的内容
        let prev = self.keeper.latest_accepted(&sub.id)?;
        let decision = self.validator.accept(prev.as_ref().map(|(_, o)| o), &next);
        let accepted = decision.is_accept();
        let version = self.keeper.record(&sub.id, &next, sub.keep_versions, accepted)?;

        if accepted {
            return Ok(SyncOutcome {
                subscription: sub.id.to_string(),
                decision,
                recorded_version: Some(version),
                effective_version: Some(version),
                effective: Some(next),
                rolled_back: false,
            });
        }

        // 坏更新：留档了，但**生效的仍然是上一版**
        let effective = self.keeper.latest_accepted(&sub.id)?;
        Ok(SyncOutcome {
            subscription: sub.id.to_string(),
            decision,
            recorded_version: Some(version),
            effective_version: effective.as_ref().map(|(m, _)| m.version),
            effective: effective.map(|(_, o)| o),
            rolled_back: true,
        })
    }

    /// 手动回滚到指定版本（"一键回到上一版"）
    pub fn rollback_to(&self, sub: &Subscription, version: u64) -> Result<FetchOutcome> {
        self.keeper.rollback_to(&sub.id, version)
    }

    /// 回滚到最近一版已采纳的
    pub fn rollback_latest_accepted(&self, sub: &Subscription) -> Result<FetchOutcome> {
        self.keeper.rollback_latest_accepted(&sub.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::DefaultSubscriptionValidator;
    use crate::version::{MemoryVersionStore, VersionKeeper};
    use crate::{detect, parse};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use ta_core::clock::FixedClock;
    use ta_core::error::TaError;
    use wiremock::MockServer;

    // ------------------------------------------------------------ 脚手架

    fn sub() -> Subscription {
        serde_yaml_ng::from_str("id: sub-jp\nname: 机场A\nurl: \"https://example.com/sub\"\n")
            .unwrap()
    }

    fn outcome_from(yaml: &str) -> FetchOutcome {
        let raw = yaml.as_bytes().to_vec();
        let format = detect::detect_format(&raw);
        let extracted = parse::extract(&raw, format);
        FetchOutcome {
            raw,
            format,
            userinfo: None,
            suggested_interval_s: None,
            nodes: extracted.nodes,
            groups: extracted.groups,
        }
    }

    fn yaml_with_nodes(n: usize) -> String {
        let mut s = String::from("proxies:\n");
        for i in 0..n {
            s.push_str(&format!(
                "  - {{name: 节点{i}, type: ss, server: 1.2.3.{}.example.com, port: 443}}\n",
                i % 250
            ));
        }
        s.push_str("proxy-groups:\n  - {name: 自动, type: url-test, proxies: []}\n");
        s
    }

    /// 按队列返回预设结果（HTTP 路径已在 `fetch` 里测过，这里只测编排）
    struct ScriptedFetcher {
        queue: Mutex<VecDeque<std::result::Result<FetchOutcome, String>>>,
    }

    impl ScriptedFetcher {
        fn new(items: Vec<std::result::Result<FetchOutcome, String>>) -> Self {
            Self { queue: Mutex::new(items.into()) }
        }
    }

    #[async_trait]
    impl SubscriptionFetcher for ScriptedFetcher {
        async fn fetch(&self, _s: &Subscription) -> Result<FetchOutcome> {
            match self.queue.lock().unwrap().pop_front() {
                Some(Ok(o)) => Ok(o),
                Some(Err(e)) => Err(TaError::Internal(e)),
                None => Err(TaError::Internal("ScriptedFetcher 队列空了".into())),
            }
        }
    }

    fn sync_with(
        items: Vec<std::result::Result<FetchOutcome, String>>,
    ) -> SubscriptionSync<ScriptedFetcher, DefaultSubscriptionValidator, MemoryVersionStore> {
        SubscriptionSync::new(
            ScriptedFetcher::new(items),
            DefaultSubscriptionValidator::new(),
            VersionKeeper::with_clock(
                MemoryVersionStore::new(),
                std::sync::Arc::new(FixedClock::new(1_700_000_000_000)),
            ),
        )
    }

    // ------------------------------------------------------------ 基本流程

    #[tokio::test]
    async fn first_sync_accepts_and_records_version_one() {
        let s = sync_with(vec![Ok(outcome_from(&yaml_with_nodes(10)))]);
        let r = s.sync(&sub()).await.unwrap();
        assert!(r.accepted());
        assert_eq!(r.recorded_version, Some(1));
        assert_eq!(r.effective_version, Some(1));
        assert!(!r.rolled_back);
        assert_eq!(r.effective.as_ref().unwrap().node_count(), 10);
        assert!(r.summary().contains("10 个节点"), "{}", r.summary());
    }

    #[tokio::test]
    async fn download_failure_keeps_previous_and_records_nothing() {
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(10))),
            Err("连接失败：connection refused".into()),
        ]);
        let sub = sub();
        s.sync(&sub).await.unwrap();

        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted());
        assert!(r.rolled_back);
        assert_eq!(r.recorded_version, None, "没抓到东西就没有版本可记");
        assert_eq!(r.effective_version, Some(1), "生效的仍是上一版");
        assert_eq!(r.effective.as_ref().unwrap().node_count(), 10);
        assert!(r.reason().unwrap().contains("下载失败"));
        assert!(r.summary().contains("继续使用版本 #1"), "{}", r.summary());
        // 存储里只有一版（失败不占版本号）
        assert_eq!(s.keeper().history(&sub.id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn download_failure_without_any_version_has_no_effective() {
        let s = sync_with(vec![Err("超时".into())]);
        let r = s.sync(&sub()).await.unwrap();
        assert!(!r.accepted());
        assert!(r.effective.is_none());
        assert_eq!(r.effective_version, None);
        assert!(r.summary().contains("当前没有可用版本"), "{}", r.summary());
    }

    // ------------------------------------------------------------ ★ 三条防线（真跑）

    /// ① 喂 HTML → 拒绝，且**上一版仍在**（从版本存储里读回来）
    #[tokio::test]
    async fn html_update_is_rejected_and_previous_version_stays_in_store() {
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(100))),
            Ok(outcome_from("<!DOCTYPE html><html><body>502 Bad Gateway</body></html>")),
        ]);
        let sub = sub();
        let first = s.sync(&sub).await.unwrap();
        assert!(first.accepted());

        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted(), "HTML 必须被拒绝");
        assert!(r.reason().unwrap().contains("HTML"), "{:?}", r.reason());
        // 坏更新留档（accepted=false），但生效的是上一版
        assert_eq!(r.recorded_version, Some(2));
        assert_eq!(r.effective_version, Some(1));
        assert_eq!(r.effective.as_ref().unwrap().node_count(), 100);

        // ★ 不看返回值，**直接从版本存储读回来**
        let (meta, from_store) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert!(meta.accepted);
        assert_eq!(from_store.node_count(), 100, "上一版必须还在存储里");
        assert_eq!(from_store.nodes[0].name, "节点0");
        // 而且能手动回滚到它
        assert_eq!(s.rollback_to(&sub, 1).unwrap().node_count(), 100);
    }

    /// ② 喂空列表 → 拒绝，且上一版仍在
    #[tokio::test]
    async fn empty_list_update_is_rejected_and_previous_version_stays() {
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(50))),
            Ok(outcome_from("proxies: []\nproxy-groups: []\n")),
        ]);
        let sub = sub();
        s.sync(&sub).await.unwrap();

        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted(), "空列表必须被拒绝");
        assert!(r.reason().unwrap().contains("没有任何可用节点"), "{:?}", r.reason());
        assert_eq!(r.effective_version, Some(1));
        let (_, from_store) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(from_store.node_count(), 50);
    }

    /// ③ 喂节点数骤降 → 拒绝，且上一版仍在
    #[tokio::test]
    async fn node_count_collapse_is_rejected_and_previous_version_stays() {
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(100))),
            Ok(outcome_from(&yaml_with_nodes(20))),
        ]);
        let sub = sub();
        s.sync(&sub).await.unwrap();

        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted(), "100 → 20 必须被拒绝");
        assert!(r.reason().unwrap().contains("骤降"), "{:?}", r.reason());
        assert_eq!(r.effective_version, Some(1));
        let (meta, from_store) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert_eq!(from_store.node_count(), 100, "上一版的 100 个节点必须还在");
    }

    /// 连续坏更新之后仍然能回滚（坏更新不许挤掉好版本）
    #[tokio::test]
    async fn repeated_bad_updates_never_lose_the_good_version() {
        let mut items = vec![Ok(outcome_from(&yaml_with_nodes(60)))];
        for _ in 0..5 {
            items.push(Ok(outcome_from("<!DOCTYPE html><html>502</html>")));
        }
        let s = sync_with(items);
        let sub = sub();
        s.sync(&sub).await.unwrap();
        for _ in 0..5 {
            let r = s.sync(&sub).await.unwrap();
            assert!(!r.accepted());
            assert_eq!(r.effective_version, Some(1), "每次都必须还能回到版本 1");
            assert_eq!(r.effective.as_ref().unwrap().node_count(), 60);
        }
    }

    #[tokio::test]
    async fn recovery_after_bad_update_compares_against_last_accepted() {
        // 100（采纳）→ HTML（拒）→ 90（采纳：基准仍是 100，跌 10% 正常）
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(100))),
            Ok(outcome_from("<html>err</html>")),
            Ok(outcome_from(&yaml_with_nodes(90))),
        ]);
        let sub = sub();
        s.sync(&sub).await.unwrap();
        assert!(!s.sync(&sub).await.unwrap().accepted());
        let r = s.sync(&sub).await.unwrap();
        assert!(r.accepted(), "恢复正常后要能继续采纳：{:?}", r.reason());
        assert_eq!(r.recorded_version, Some(3));
        assert_eq!(r.effective.as_ref().unwrap().node_count(), 90);
    }

    #[tokio::test]
    async fn keeps_versions_per_subscription_config() {
        let s = sync_with(vec![
            Ok(outcome_from(&yaml_with_nodes(10))),
            Ok(outcome_from(&yaml_with_nodes(11))),
        ]);
        let mut sub = sub();
        sub.keep_versions = 1;
        s.sync(&sub).await.unwrap();
        s.sync(&sub).await.unwrap();
        assert_eq!(s.keeper().history(&sub.id).unwrap().len(), 1);
        // 被裁掉的那版回滚会失败（但当前版本仍可用）
        assert!(s.rollback_to(&sub, 1).is_err());
        assert_eq!(s.effective(&sub).unwrap().expect_outcome().0.version, 2);
    }

    // ------------------------------------------------------------ 端到端（真 HTTP）

    /// 真 HTTP + 真 validator + 真版本存储：把"上一版仍在"钉死在**存储**上
    async fn e2e(
        first_body: String,
        second_body: String,
    ) -> (
        SubscriptionSync<
            crate::fetch::HttpSubscriptionFetcher,
            DefaultSubscriptionValidator,
            MemoryVersionStore,
        >,
        Subscription,
        // ★ 必须把 server 交回调用方持有：wiremock 的 MockServer 在 Drop 时就停止监听，
        //   一旦提前 drop，后续请求会打到"已经空出来的端口"上 —— 运气好是连接被拒，
        //   运气不好会打到**别的测试**刚绑上的同号端口（于是解析出别人的内容）。
        MockServer,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        Mock::given(method("GET"))
            .respond_with(move |_req: &wiremock::Request| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(if n == 0 {
                    first_body.clone()
                } else {
                    second_body.clone()
                })
            })
            .mount(&server)
            .await;

        let fetcher =
            crate::fetch::HttpSubscriptionFetcher::with_options(crate::fetch::FetchOptions {
                retry_min_delay: std::time::Duration::from_millis(1),
                retry_max_delay: std::time::Duration::from_millis(2),
                ..Default::default()
            })
            .unwrap();
        let keeper = VersionKeeper::with_clock(
            MemoryVersionStore::new(),
            std::sync::Arc::new(FixedClock::new(1_700_000_000_000)),
        );
        let sub: Subscription = serde_yaml_ng::from_str(&format!(
            "id: sub-jp\nname: 机场A\nurl: \"{}\"\n",
            server.uri()
        ))
        .unwrap();
        (SubscriptionSync::new(fetcher, DefaultSubscriptionValidator::new(), keeper), sub, server)
    }

    #[tokio::test]
    async fn e2e_html_update_keeps_previous_version() {
        let (s, sub, _server) =
            e2e(yaml_with_nodes(100), "<!DOCTYPE html><html>502</html>".into()).await;
        assert!(s.sync(&sub).await.unwrap().accepted());
        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted());
        assert!(r.reason().unwrap().contains("HTML"), "{:?}", r.reason());
        // 从存储读回来
        let (meta, back) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert_eq!(back.node_count(), 100);
    }

    #[tokio::test]
    async fn e2e_empty_list_update_keeps_previous_version() {
        let (s, sub, _server) = e2e(yaml_with_nodes(50), "proxies: []\n".into()).await;
        assert!(s.sync(&sub).await.unwrap().accepted());
        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted());
        let (meta, back) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert_eq!(
            back.node_count(),
            50,
            "stored format={:?} raw_head={:?} reason={:?}",
            back.format,
            String::from_utf8_lossy(&back.raw[..back.raw.len().min(120)]),
            r.reason()
        );
    }

    #[tokio::test]
    async fn e2e_node_collapse_keeps_previous_version() {
        let (s, sub, _server) = e2e(yaml_with_nodes(100), yaml_with_nodes(8)).await;
        assert!(s.sync(&sub).await.unwrap().accepted());
        let r = s.sync(&sub).await.unwrap();
        assert!(!r.accepted());
        assert!(r.reason().unwrap().contains("骤降"), "{:?}", r.reason());
        let (meta, back) = s.effective(&sub).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert_eq!(back.node_count(), 100);
    }

    #[tokio::test]
    async fn e2e_download_failure_keeps_previous_version() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_string(yaml_with_nodes(30)))
            .mount(&server)
            .await;
        let fetcher =
            crate::fetch::HttpSubscriptionFetcher::with_options(crate::fetch::FetchOptions {
                max_attempts: 1,
                ..Default::default()
            })
            .unwrap();
        let keeper = VersionKeeper::with_clock(
            MemoryVersionStore::new(),
            std::sync::Arc::new(FixedClock::new(0)),
        );
        let s = SubscriptionSync::new(fetcher, DefaultSubscriptionValidator::new(), keeper);
        let sub: Subscription = serde_yaml_ng::from_str(&format!(
            "id: sub-jp\nname: 机场A\nurl: \"{}/ok\"\n",
            server.uri()
        ))
        .unwrap();
        assert!(s.sync(&sub).await.unwrap().accepted());

        // 同一份订阅，URL 换成 404
        let sub404: Subscription = serde_yaml_ng::from_str(&format!(
            "id: sub-jp\nname: 机场A\nurl: \"{}/nope\"\n",
            server.uri()
        ))
        .unwrap();
        let r = s.sync(&sub404).await.unwrap();
        assert!(!r.accepted());
        assert!(r.reason().unwrap().contains("下载失败"), "{:?}", r.reason());
        let (meta, back) = s.effective(&sub404).unwrap().expect_outcome();
        assert_eq!(meta.version, 1);
        assert_eq!(back.node_count(), 30);
    }

    // 便捷：把 Option 解成"必须有"
    trait ExpectOutcome {
        #[allow(clippy::type_complexity)]
        fn expect_outcome(self) -> (VersionMeta, FetchOutcome);
    }

    impl ExpectOutcome for Option<(VersionMeta, FetchOutcome)> {
        fn expect_outcome(self) -> (VersionMeta, FetchOutcome) {
            self.expect("应当存在一份已采纳的版本")
        }
    }
}

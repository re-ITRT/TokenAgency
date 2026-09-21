//! 默认配置：与仓库根目录的 `config.example.yaml` 等价的 [`ConfigDoc`]。
//!
//! 用途：首启向导（onboarding）预填、`config_get` 在库里还没有配置时的兜底。
//!
//! **与 `config.example.yaml` 的关系**：语义等价，但字段名以**冻结的 Rust 契约**
//! （`domain/mod.rs`）为准。示例 YAML 里若干历史字段名（如 `pricing.defaults`、
//! `net.health.interval`、`net.health.interval`、`sticky.ttl`、`failover.cooldown`）
//! 在结构体里分别是 `pricing.currency/unit`、`interval_s`、`ttl_h`、`cooldown_s`；
//! 本模块产出的是后者（能被 `from_yaml` 直接读回的那一套）。
//!
//! 另一个刻意的差异：结构体里 `enabled` 的 serde 默认值是 `false`，
//! 而示例 YAML 省略了它——真源是 SQLite 的运行时不允许"全部禁用"，
//! 所以这里显式把 provider/credential/subscription/lane/channel 置为启用。

use serde_json::Map;

use super::migrate::CURRENT_CONFIG_VERSION;
use crate::domain::*;
use crate::types::*;

/// 直接可用的默认配置。
pub(crate) fn build_default_doc() -> ConfigDoc {
    ConfigDoc {
        version: CURRENT_CONFIG_VERSION,
        gateway: gateway(),
        request_log: request_log(),
        pricing: pricing(),
        net: net(),
        proxy_subscriptions: subscriptions(),
        lanes: lanes(),
        providers: providers(),
        credentials: credentials(),
        models: models(),
        channels: channels(),
        channel_groups: channel_groups(),
        policies: policies(),
        system_integration: SystemIntegration {
            // ★ 会写操作系统状态的三个开关一律默认关（[09 §1.2] E1）：
            //   装完就静默加一个开机启动项 / 接管整机网络，都是用户最反感的"副作用"。
            enable_tun: false,
            enable_system_proxy: false,
            autostart: false,
            autostart_mode: AutostartMode::Tray,
            autostart_delay_s: 0,
            tray_show_latency: true,
            file_watch_mode: false,
            file_watch_path: None,
            extra: Map::new(),
        },
        extra: Map::new(),
    }
}

fn gateway() -> GatewayConfig {
    GatewayConfig {
        listen: "127.0.0.1:8787".into(),
        admin_listen: "127.0.0.1:8788".into(),
        // 可选：客户端的 Bearer；留空/None 则不校验
        local_key: Some("${secret:gateway-local-key}".into()),
        max_body_mb: 32,
        debug_headers: true,
        expose_models_with_capabilities: false,
        fallback_provider: None,
        passthrough: PassthroughConfig {
            header_mode: HeaderMode::Pass,
            body: BodyMode::Preserve,
            strip_hop_by_hop: true,
            verify_no_diff: true,
            on_loss: OnLoss::Reject,
            extra: Map::new(),
        },
        retry: RetryConfig {
            max_attempts: 3,
            stream_max_attempts: 2,
            retry_after_first_byte: false,
            cooldown_s: 120,
            backoff: Backoff::Exponential,
            extra: Map::new(),
        },
        metering: MeteringConfig { inject_stream_usage: false, extra: Map::new() },
        extra: Map::new(),
    }
}

fn request_log() -> RequestLogConfig {
    RequestLogConfig {
        mode: LogMode::Meta,
        retention: RetentionDefault::D30,
        max_rows: 500_000,
        max_size_mb: 2048,
        sample_rate: 1.0,
        image_handling: ImageHandling::Hash,
        extra: Map::new(),
    }
}

fn pricing() -> PricingConfig {
    PricingConfig {
        currency: "USD".into(),
        unit: PriceUnit::Per1M,
        // 仅用于显示折算；历史记录保留原币种
        usd_to_cny: Some(7.25),
        entries: vec![
            // 精确匹配（官方价）
            PriceEntry::new(PriceMatch::model("claude-sonnet-4-5").with_provider("p-anthropic"))
                .with_rates([
                    (TokenKind::Input, 3.00),
                    (TokenKind::InputCached, 0.30),
                    (TokenKind::InputCacheWrite, 3.75),
                    (TokenKind::InputCacheWrite1h, 6.00),
                    (TokenKind::Output, 15.00),
                ])
                .with_source(PriceSource::Builtin),
            // 通配匹配（按顺序，首个命中生效）
            PriceEntry::new(PriceMatch::model("gpt-4o*"))
                .with_rates([
                    (TokenKind::Input, 2.50),
                    (TokenKind::InputCached, 1.25),
                    (TokenKind::Output, 10.00),
                ])
                .with_source(PriceSource::Builtin),
            // 中转站倍率：官方价 × 0.8
            PriceEntry::new(PriceMatch::provider("p-mytoken"))
                .with_multiplier(0.8)
                .with_source(PriceSource::Manual),
            // 固定套餐费：不摊进单次请求，在成本中心按周期摊销展示
            PriceEntry::new(PriceMatch::credential("c-relay-a")).with_plan_fee(PlanFee::new(
                99.0,
                "CNY",
                QuotaWindow::Month,
            )),
        ],
        extra: Map::new(),
    }
}

fn net() -> NetConfig {
    NetConfig {
        kernel: KernelConfig {
            provider: KernelProvider::External,
            // 空/None = 用首启向导的查找结果
            path: None,
            controller: "127.0.0.1:39090".into(),
            lane_port_range: [39100, 39299],
            subscription_source: SubscriptionSource::Kernel,
            extra: Map::new(),
        },
        dialer: DialerKind::Kernel,
        dns: DnsConfig {
            enhanced_mode: "redir-host".into(),
            nameserver: vec![
                "https://223.5.5.5/dns-query".into(),
                "https://doh.pub/dns-query".into(),
            ],
            // ★ 不配这条，节点自己都连不上
            proxy_server_nameserver: vec!["https://223.5.5.5/dns-query".into()],
            default_nameserver: vec!["223.5.5.5".into(), "119.29.29.29".into()],
            extra: Map::new(),
        },
        health: HealthConfig {
            interval_s: 60,
            url_by_provider: true,
            probe_url: None,
            extra: Map::new(),
        },
        // 全局兜底线路
        default_lane: Some(LaneId::new("lane-direct")),
        // 绝不碰系统代理 / TUN
        system_proxy: false,
        tun: false,
        extra: Map::new(),
    }
}

fn subscriptions() -> Vec<Subscription> {
    vec![Subscription::new(
        "sub-jp",
        "机场A",
        "https://example.com/api/v1/client/subscribe?token=REPLACE_ME",
    )
    .with_provider_key("sub-jp")
    .with_filter(SubscriptionFilter {
        include: Some("(?i)(日本|JP|Tokyo|Osaka|新加坡|SG)".into()),
        exclude: Some("(?i)(官网|剩余流量|到期|过期|订阅|防失联)".into()),
        extra: Map::new(),
    })
    .with_health_check(SubscriptionHealthCheck {
        enabled: true,
        url: Some("https://api.openai.com/v1/models".into()),
        interval_s: 60,
        tolerance_ms: 100,
        extra: Map::new(),
    })]
}

fn lanes() -> Vec<Lane> {
    vec![
        Lane::direct("lane-direct", "直连（本机网络）"),
        Lane::subscription_auto("lane-jp-auto", "日本 · 自动择优", "sub-jp")
            .with_group_type(GroupType::UrlTest)
            .with_filter("(?i)(日本|JP|Tokyo|Osaka)"),
        Lane::subscription_group(
            "lane-jp-original",
            "🇯🇵 日本（订阅自带分组）",
            "sub-jp",
            "🇯🇵 日本",
        ),
        Lane::subscription_node("lane-us-fixed", "美国 · 固定节点", "sub-jp", "美国-洛杉矶-01"),
    ]
}

fn providers() -> Vec<Provider> {
    vec![
        Provider::new("p-openai", "OpenAI 官方", Protocol::Openai, "https://api.openai.com/v1"),
        Provider::new(
            "p-anthropic",
            "Anthropic 官方",
            Protocol::Anthropic,
            "https://api.anthropic.com/v1",
        )
        .with_auth(AuthConfig {
            scheme: AuthScheme::Header,
            header: Some("x-api-key".into()),
            prefix: None,
            extra: Map::new(),
        }),
        Provider::new(
            "p-mytoken",
            "某中转站（OpenAI 兼容）",
            Protocol::Openai,
            "https://api.example-relay.com/v1",
        )
        .with_lane("lane-jp-auto")
        // 只补缺，不覆盖客户端同名头
        .with_header("X-Relay-Channel", "default"),
    ]
}

fn credentials() -> Vec<Credential> {
    vec![
        Credential::new(
            "c-openai-personal",
            "p-openai",
            "OpenAI 个人订阅",
            "${secret:openai-personal}",
        )
        .with_lane("lane-jp-original")
        .with_quota(QuotaConfig {
            metric: QuotaMetric::Unknown,
            limit: None,
            window: QuotaWindow::Month,
            reset_day: 1,
            soft_limit: 0.8,
            hard_limit: 1.0,
            ..Default::default()
        })
        .with_guard(GuardConfig {
            max_rpm: Some(60),
            max_concurrency: Some(8),
            max_input_tokens: None,
            on_exceed: OnExceed::Wait,
            extra: Map::new(),
        })
        .with_tags(["official"]),
        Credential::new(
            "c-anthropic-personal",
            "p-anthropic",
            "Anthropic 个人订阅",
            "${secret:anthropic-personal}",
        )
        .with_lane("lane-jp-auto")
        .with_tags(["official"]),
        Credential::new("c-relay-a", "p-mytoken", "中转站A · 500万 token/月", "${secret:relay-a}")
            .with_lane("lane-jp-auto")
            .with_quota(QuotaConfig {
                metric: QuotaMetric::Tokens,
                limit: Some(5_000_000.0),
                window: QuotaWindow::Month,
                reset_day: 1,
                soft_limit: 0.8,
                hard_limit: 0.98,
                reserve: 0.05,
                ..Default::default()
            })
            .with_tags(["cheap"]),
        Credential::new("c-relay-b", "p-mytoken", "中转站B · 按量付费", "${secret:relay-b}")
            // 这家国内直连可达，不用梯子
            .with_lane("lane-direct")
            .with_quota(QuotaConfig {
                metric: QuotaMetric::Cost,
                limit: Some(20.0),
                currency: "CNY".into(),
                window: QuotaWindow::Month,
                ..Default::default()
            })
            .with_tags(["backup"]),
    ]
}

fn models() -> Vec<Model> {
    vec![
        Model::new("gpt-4o")
            .with_aliases(["gpt-4o-2024-11-20", "openai/gpt-4o", "gpt-4o-latest"])
            .with_display("OpenAI", 10),
        Model::new("claude-sonnet-4-5")
            .with_aliases(["claude-sonnet-4-5-20250929", "claude-3-5-sonnet"])
            .with_display("Anthropic", 20),
        Model::new("gpt-4o-mini").with_display("OpenAI", 11),
    ]
}

fn channels() -> Vec<Channel> {
    vec![
        Channel::new(
            "ch-openai-personal",
            "gpt-4o",
            "p-openai",
            "c-openai-personal",
            "gpt-4o-2024-11-20",
        )
        .with_lane("lane-us-fixed")
        .with_priority(10)
        .with_weight(100)
        .with_tags(["official"]),
        Channel::new("ch-relay-a", "gpt-4o", "p-mytoken", "c-relay-a", "gpt-4o")
            .with_lane("lane-jp-original")
            .with_priority(20)
            .with_weight(50)
            .with_tags(["cheap"]),
        Channel::new("ch-relay-b", "gpt-4o", "p-mytoken", "c-relay-b", "gpt-4o")
            .with_lane("lane-direct")
            .with_priority(30)
            .with_weight(30)
            // 未标注是否识图 → 照发（默认行为是发出去，不是拒绝）
            .with_tags(["backup"]),
        // lane 不填 → 继承 credential.lane（lane-jp-auto）
        Channel::new(
            "ch-claude-official",
            "claude-sonnet-4-5",
            "p-anthropic",
            "c-anthropic-personal",
            "claude-sonnet-4-5-20250929",
        )
        .with_priority(10)
        .with_tags(["official"]),
    ]
}

fn channel_groups() -> Vec<ChannelGroup> {
    vec![ChannelGroup::new(
        "grp-gpt-4o",
        "gpt-4o",
        ["ch-openai-personal", "ch-relay-a", "ch-relay-b"],
    )
    // ★ "一个额度用完了再用第二个" + 会话粘性
    .with_selector(Selector::StickyThenPriority)
    .with_sticky(StickyConfig {
        enabled: true,
        ttl_h: 12,
        key: vec![
            "header:x-session-id".into(),
            "header:x-conversation-id".into(),
            "metadata.user_id".into(),
            "body_hash".into(),
        ],
        extra: Map::new(),
    })
    .with_release_on([
        ReleaseReason::QuotaExhausted,
        ReleaseReason::HardError,
        ReleaseReason::UserPinChange,
    ])]
}

fn policies() -> Vec<Policy> {
    vec![
        // 1) 带图请求跳过不识图的通道（默认关闭，等你真遇到再开）
        Policy::new("pol-skip-nonvision", "带图请求跳过不识图的通道")
            .with_priority(5)
            .with_match(PolicyMatch { has_image: Some(true), ..Default::default() })
            .with_action(PolicyAction {
                exclude_tag: Some("no-vision".into()),
                on_no_candidate: NoCandidateAction::Error,
                error_code: Some(415),
                ..Default::default()
            })
            .disabled(),
        // 2) 深夜的批量任务走便宜通道
        Policy::new("pol-night-batch", "夜间长文本走中转A")
            .with_priority(20)
            .with_match(PolicyMatch {
                model_matches: vec!["gpt-4o*".into()],
                input_tokens_gt: Some(20_000),
                time_window: Some("01:00-07:00".into()),
                ..Default::default()
            })
            .with_action(PolicyAction {
                use_channels: vec![ChannelId::new("ch-relay-a")],
                selector: Some(Selector::Priority),
                ..Default::default()
            })
            .disabled(),
        // 3) 某家中转抽风时，临时全局绕开（一键开关）
        Policy::new("pol-avoid-relay-a", "临时绕开中转A")
            .with_priority(30)
            .with_match(PolicyMatch::match_all())
            .with_action(PolicyAction {
                exclude_channels: vec![ChannelId::new("ch-relay-a")],
                ..Default::default()
            })
            .disabled(),
        // 4) 兜底
        Policy::new("pol-default", "默认策略")
            .with_priority(1000)
            .with_match(PolicyMatch::match_all())
            .with_action(PolicyAction {
                use_group: Some(ChannelGroupId::new("grp-gpt-4o")),
                failover: Some(FailoverPolicy {
                    on: vec![
                        FailoverTrigger::Http429,
                        FailoverTrigger::Http402,
                        FailoverTrigger::Http5xx,
                        FailoverTrigger::Timeout,
                        FailoverTrigger::ConnectError,
                        FailoverTrigger::QuotaExhausted,
                        FailoverTrigger::ChannelTaggedOut,
                    ],
                    max_attempts: 3,
                    cooldown_s: 120,
                    backoff: Backoff::Exponential,
                    extra: Map::new(),
                }),
                ..Default::default()
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{semantic_eq, to_yaml, validate};

    #[test]
    fn default_doc_matches_example_yaml_semantics() {
        let doc = build_default_doc();
        assert_eq!(doc.version, 1);
        assert_eq!(doc.gateway.listen, "127.0.0.1:8787");
        assert_eq!(doc.gateway.admin_listen, "127.0.0.1:8788");
        assert_eq!(doc.net.default_lane.as_ref().map(|l| l.as_str()), Some("lane-direct"));
        assert_eq!(doc.net.kernel.lane_port_range, [39100, 39299]);
        assert_eq!(doc.lanes.len(), 4);
        assert_eq!(doc.providers.len(), 3);
        assert_eq!(doc.credentials.len(), 4);
        assert_eq!(doc.models.len(), 3);
        assert_eq!(doc.channels.len(), 4);
        assert_eq!(doc.channel_groups.len(), 1);
        assert_eq!(doc.policies.len(), 4);
        assert_eq!(doc.pricing.entries.len(), 4);
        assert_eq!(doc.proxy_subscriptions.len(), 1);

        // 只有 pol-default 是启用的
        let enabled: Vec<&str> =
            doc.policies.iter().filter(|p| p.enabled).map(|p| p.id.as_str()).collect();
        assert_eq!(enabled, vec!["pol-default"]);

        // 一切"运行需要"的实体默认是启用的
        assert!(doc.providers.iter().all(|p| p.enabled));
        assert!(doc.credentials.iter().all(|c| c.enabled));
        assert!(doc.channels.iter().all(|c| c.enabled));
        assert!(doc.lanes.iter().all(|l| l.enabled));
        assert!(doc.proxy_subscriptions.iter().all(|s| s.enabled));

        // 但"会写操作系统状态"的三个开关默认必须是关的（09 §1.2 E1）：
        // 首启向导绝不能替用户静默注册开机启动项。
        assert!(!doc.system_integration.autostart);
        assert!(!doc.system_integration.enable_tun);
        assert!(!doc.system_integration.enable_system_proxy);
        assert_eq!(doc.system_integration.autostart_mode, AutostartMode::Tray);
        assert_eq!(doc.system_integration.autostart_delay_s, 0);

        // ch-claude-official 不写 lane，靠 credential.lane 继承
        let claude = doc.channel("ch-claude-official").unwrap();
        assert_eq!(claude.lane, None);
        let cred = doc.credential("c-anthropic-personal").unwrap();
        assert_eq!(
            claude.effective_lane(Some(cred), doc.provider("p-anthropic"), &doc.net),
            Some(&LaneId::new("lane-jp-auto"))
        );
    }

    #[test]
    fn default_doc_is_clean_and_round_trips_yaml() {
        let doc = build_default_doc();
        let report = validate(&doc);
        assert!(report.ok, "默认配置必须能通过校验：{:#?}", report.errors);
        assert!(report.warnings.is_empty(), "{:#?}", report.warnings);

        let yaml = to_yaml(&doc).expect("应当能序列化");
        let back = crate::config::from_yaml(&yaml).expect("应当能读回");
        assert!(semantic_eq(&doc, &back), "YAML 往返必须语义等价");
    }
}

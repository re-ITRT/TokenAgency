//! **K7 往返测试**：`parse_snapshot` 对着**真 mihomo 的原始 JSON**（[docs/06 §1.6]）。
//!
//! ## 为什么必须有它
//!
//! 在这批夹具之前，`parse_snapshot` 的测试用的是**手写 JSON**（"我以为的内核"）。
//! 主管在真 mihomo v1.19.31 上实测出 **4 处偏差**会让它解析错误，手写夹具一个都发现不了：
//!
//! | # | 偏差 | 手写夹具为什么测不出 |
//! |---|---|---|
//! | **7-a** | `/providers/proxies` 的 `providers` 是**对象**不是数组 | 手写的就写成数组了 |
//! | **7-b** | `type` 是**大写内部名**（`Selector`/`Shadowsocks`/`Vmess`） | 手写的按 `select`/`ss` 写 |
//! | **7-c** | provider 节点**不在 `/proxies` 顶层**，但出现在组的 `all` 里 | 手写时没人会造这种"悬空" |
//! | **7-d** | 内置伪节点（`DIRECT`/`REJECT`/`PASS`/`COMPATIBLE`/`GLOBAL`）混在列表里 | 手写时想不起来 |
//!
//! 所以这些断言**全部对着真夹具**（`tests/fixtures/real-mihomo/*.json`，来源与 sha256 见该目录 README）。
//! 依据 [docs/13 L19]：**夹具是断言的输入，不是断言的产物**。
//!
//! 本文件里的每个数字都是先把真 JSON 跑一遍读出来的，不是"我觉得应该是"。

use std::collections::BTreeSet;

use ta_kernel::rest::{parse_provider_counts, parse_snapshot};
use ta_kernel::{BuiltinProxy, NodeSource, ProvidersSnapshot};

const PROXIES_JSON: &str = include_str!("fixtures/real-mihomo/proxies.json");
const PROVIDERS_JSON: &str = include_str!("fixtures/real-mihomo/providers-proxies.json");

fn snapshot() -> ProvidersSnapshot {
    let proxies: serde_json::Value =
        serde_json::from_str(PROXIES_JSON).expect("真夹具 proxies.json 应是合法 JSON");
    let providers: serde_json::Value =
        serde_json::from_str(PROVIDERS_JSON).expect("真夹具 providers-proxies.json 应是合法 JSON");
    parse_snapshot(&proxies, &providers).expect("真形态必须解析成功（7-a 就是这条）")
}

fn proxy<'a>(snap: &'a ProvidersSnapshot, name: &str) -> &'a ta_kernel::ProxyEntry {
    snap.proxies
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("真夹具里应有节点/组 {name}"))
}

// ================================================================ 7-a / 7-b / 7-d

#[test]
fn top_level_shape_matches_the_real_kernel() {
    let snap = snapshot();

    // 真值：/proxies 顶层 **12** 项
    assert_eq!(snap.proxies.len(), 12, "顶层项数应等于真夹具的 12");

    // 7-d：内置伪节点**恰为 7 个**，且都被认出来（不是被当用户节点）
    let builtins: BTreeSet<&'static str> =
        snap.proxies.iter().filter_map(|p| p.builtin()).map(|b| b.name()).collect();
    assert_eq!(
        builtins,
        BTreeSet::from([
            "COMPATIBLE",
            "DIRECT",
            "GLOBAL",
            "PASS",
            "PASS-RULE",
            "REJECT",
            "REJECT-DROP"
        ]),
        "内置伪节点集合与真夹具不一致"
    );

    // 非内置的 5 个：我们生成的 3 个组 + 2 个真实节点
    let ours: BTreeSet<&str> =
        snap.proxies.iter().filter(|p| p.builtin().is_none()).map(|p| p.name.as_str()).collect();
    assert_eq!(
        ours,
        BTreeSet::from([
            "TA-LANE-direct",
            "TA-LANE-mixed",
            "TA-LANE-provider",
            "fake-hk-01",
            "fake-jp-01"
        ])
    );
}

#[test]
fn type_is_the_capitalised_internal_name_and_is_matched_case_insensitively() {
    let snap = snapshot();

    // 7-b：原样保留大写内部名（**不做枚举归一**，未知类型不能丢）
    assert_eq!(proxy(&snap, "TA-LANE-mixed").kind, "Selector");
    assert_eq!(proxy(&snap, "fake-hk-01").kind, "Shadowsocks");
    assert_eq!(proxy(&snap, "GLOBAL").kind, "Selector");
    assert_eq!(proxy(&snap, "DIRECT").kind, "Direct");

    // 大小写无关的判断：真实值是首字母大写
    let mixed = proxy(&snap, "TA-LANE-mixed");
    assert!(mixed.kind_is("selector"), "应该大小写无关");
    assert!(mixed.is_group(), "Selector 是组");
    assert!(!proxy(&snap, "fake-hk-01").is_group(), "Shadowsocks 不是组");
    assert!(!mixed.kind_is("select"), "内核不写 select（那是 YAML 里的写法）");
}

#[test]
fn only_direct_of_the_builtins_is_a_lane_target() {
    let snap = snapshot();

    // 7-d：DIRECT 是 lane 的合法目标（直连 lane），其余内置项是内部实现
    assert!(proxy(&snap, "DIRECT").is_lane_target());
    assert!(!proxy(&snap, "GLOBAL").is_lane_target(), "GLOBAL 不给用户选");
    for internal in ["COMPATIBLE", "REJECT", "REJECT-DROP", "PASS", "PASS-RULE"] {
        assert!(!proxy(&snap, internal).is_lane_target(), "{internal} 不该是 lane 目标");
    }
    // 组与真实节点都可以
    assert!(proxy(&snap, "TA-LANE-mixed").is_lane_target());
    assert!(proxy(&snap, "fake-hk-01").is_lane_target());

    assert!(BuiltinProxy::Direct.is_selectable());
    assert!(!BuiltinProxy::Global.is_selectable());
    assert_eq!(BuiltinProxy::Global.name(), "GLOBAL");

    // 真值：15 个节点里，可作线路目标的 = 15 - 6 个不可选内置 = 9
    //       不可选的 6 个：COMPATIBLE / REJECT / REJECT-DROP / PASS / PASS-RULE / GLOBAL
    assert_eq!(snap.nodes.len(), 15, "合并索引应有 12 顶层 + 3 仅 provider 的节点");
    assert_eq!(snap.selectable().len(), 9, "可作线路目标的节点数");
}

// ================================================================ 7-c 悬空引用与合并

#[test]
fn provider_nodes_are_absent_from_top_level_but_present_in_group_alls() {
    let snap = snapshot();

    // 7-c：这三个名字出现在组的 all 里，却**不在** /proxies 顶层
    for name in ["prov-node-a", "prov-node-b", "prov-node-c-vmess"] {
        assert!(
            !snap.proxies.iter().any(|p| p.name == name),
            "{name} 不该出现在 /proxies 顶层（这正是 7-c）"
        );
    }

    let mixed = proxy(&snap, "TA-LANE-mixed");
    assert_eq!(
        mixed.all,
        vec![
            "fake-hk-01",
            "fake-jp-01",
            "DIRECT",
            "prov-node-a",
            "prov-node-b",
            "prov-node-c-vmess"
        ],
        "组的 all 顺序应原样保留"
    );

    // 合并之后：三个名字都在索引里，来源是 provider，而不是"悬空"
    for name in ["prov-node-a", "prov-node-b", "prov-node-c-vmess"] {
        let n = snap.nodes.get(name).unwrap_or_else(|| panic!("合并索引里应有 {name}"));
        assert_eq!(
            n.source,
            NodeSource::Provider("ta-lane-provider".to_string()),
            "{name} 的来源应是那个 File provider"
        );
        assert!(!n.is_reachable(), "真夹具里这三个节点 alive=false");
    }

    // ★ 关键：合并之后**没有悬空** —— 用 /proxies 的键去解析 all 才会看到 3 个悬空
    assert!(snap.dangling.is_empty(), "provider 节点已在索引里，不该有悬空：{:?}", snap.dangling);
    assert_eq!(snap.dangling_in("TA-LANE-mixed").len(), 0);
}

#[test]
fn top_level_wins_when_a_name_exists_on_both_sides() {
    let snap = snapshot();

    // 真机上 DIRECT / fake-hk-01 / TA-LANE-* 在 /proxies 与 provider 里**都**有
    // （provider 那份是组的镜射项）→ 来源必须是顶层，否则 `source` 是假的
    for name in ["DIRECT", "fake-hk-01", "fake-jp-01", "TA-LANE-mixed"] {
        assert_eq!(
            snap.nodes.get(name).map(|n| n.source.clone()),
            Some(NodeSource::TopLevel),
            "{name} 应取顶层来源"
        );
    }
}

#[test]
fn group_members_resolve_through_the_provider_nodes() {
    let snap = snapshot();

    // provider 节点已并入 → group_members 里 6 个都解析得出来
    let members: Vec<&str> = snap
        .group_members("TA-LANE-mixed")
        .expect("组存在")
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        members,
        vec![
            "fake-hk-01",
            "fake-jp-01",
            "DIRECT",
            "prov-node-a",
            "prov-node-b",
            "prov-node-c-vmess"
        ]
    );
    // 组本身的 all 里，只有 prov-node-* 来自 provider
    let from_provider = members
        .iter()
        .filter(|name| {
            matches!(snap.nodes.get(**name).map(|n| &n.source), Some(NodeSource::Provider(_)))
        })
        .count();
    assert_eq!(from_provider, 3);

    assert!(snap.group_members("没有这个组").is_none());
}

#[test]
fn a_truly_missing_reference_is_reported_not_dropped() {
    // 负例（合成输入）：一个组引用了真不存在的名字 → 必须进 dangling，而不是静默消失
    let proxies = serde_json::json!({
        "proxies": {
            "G-x": { "type": "Selector", "alive": true, "now": "a", "all": ["a", "幽灵节点"], "history": [] },
            "a": { "type": "Shadowsocks", "alive": true, "history": [] }
        }
    });
    let providers = serde_json::json!({ "providers": {} });
    let snap = parse_snapshot(&proxies, &providers).unwrap();

    assert_eq!(snap.nodes.len(), 2, "只有解析得出来的两个");
    assert_eq!(snap.dangling.len(), 1, "悬空必须被记下来：{:?}", snap.dangling);
    assert_eq!(snap.dangling[0].group, "G-x");
    assert_eq!(snap.dangling[0].name, "幽灵节点");
    assert_eq!(snap.dangling_in("G-x").len(), 1);
    // 解析得出来的成员仍然照给
    assert_eq!(snap.group_members("G-x").unwrap().len(), 1);
}

// ================================================================ providers 侧

#[test]
fn providers_are_an_object_indexed_by_key_not_an_array() {
    let snap = snapshot();

    // 7-a：真夹具里是 4 个**键**。按数组反序列化会直接失败（那条由 snapshot() 覆盖）
    let keys: BTreeSet<&str> = snap.providers.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(
        keys,
        BTreeSet::from(["TA-LANE-direct", "TA-LANE-mixed", "default", "ta-lane-provider"])
    );
    // name 与 key 在真夹具里恰好相等（两个不同的概念，各自都要有）
    for p in &snap.providers {
        assert_eq!(p.name, p.key, "真夹具里 name 与 key 相等（概念仍要分开）");
    }
}

#[test]
fn group_mirrors_are_not_subscriptions() {
    let snap = snapshot();

    // 真值：4 个 provider 里 3 个是 `vehicleType: Compatible` 的**组镜射**
    // （内核给我们生成的每个组都报一个，见 k8.log:11-13）
    let mirrors: BTreeSet<&str> =
        snap.providers.iter().filter(|p| p.is_group_mirror()).map(|p| p.key.as_str()).collect();
    assert_eq!(mirrors, BTreeSet::from(["TA-LANE-direct", "TA-LANE-mixed", "default"]));

    // 唯一一个真 provider
    let file_provider =
        snap.providers.iter().find(|p| p.key == "ta-lane-provider").expect("应有它");
    assert!(!file_provider.is_group_mirror());
    assert!(file_provider.updated_at_is_file_mtime(), "vehicleType=File");
    assert_eq!(file_provider.vehicle_type, "File");
    assert_eq!(file_provider.node_count, 3);
    assert_eq!(
        file_provider.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        vec!["prov-node-a", "prov-node-b", "prov-node-c-vmess"]
    );

    // 诊断口径：**只查我们生成的 proxy-providers key**，不能要求"每个 provider 都非空"
    // —— 镜射组为空时那样会假阴性。expected_providers() 正是从生成 YAML 回读的那个集合。
    let counts =
        parse_provider_counts(&serde_json::from_str::<serde_json::Value>(PROVIDERS_JSON).unwrap())
            .unwrap();
    assert_eq!(counts.get("ta-lane-provider"), Some(&3));
    assert_eq!(counts.len(), 4);
}

#[test]
fn updated_at_of_a_file_provider_is_the_file_mtime_not_a_fetch_time() {
    let snap = snapshot();
    let file_provider = snap.providers.iter().find(|p| p.key == "ta-lane-provider").unwrap();

    // 真值原样：带 +08:00 偏移（不是 Z）
    assert_eq!(file_provider.updated_at.as_deref(), Some("2026-09-21T23:42:34.135734+08:00"));
    // 而组的镜射项报的是 Go 的零值时间
    let mirror = snap.providers.iter().find(|p| p.key == "default").unwrap();
    assert_eq!(mirror.updated_at.as_deref(), Some("0001-01-01T00:00:00Z"));
    assert!(!mirror.updated_at_is_file_mtime(), "Compatible 不是 File");
}

// ================================================================ alive / delay

#[test]
fn alive_is_the_health_signal_and_delay_zero_alone_is_not_enough() {
    let snap = snapshot();

    // 真夹具：两个自有节点 alive=true、history 为空（没测过 ≠ 不通）
    let hk = proxy(&snap, "fake-hk-01");
    assert!(hk.is_reachable());
    assert_eq!(hk.latest_delay(), None, "没测过 → None，不是 0");
    assert!(hk.history.is_empty());

    // provider 节点：alive=false（真实"不可达"信号）
    let down = snap.nodes.get("prov-node-a").unwrap();
    assert!(!down.is_reachable());

    // 组的 history 恒为 []（实测：健康历史挂在被选中的节点上，不在组上）
    for group in ["GLOBAL", "TA-LANE-direct", "TA-LANE-mixed", "TA-LANE-provider"] {
        assert!(proxy(&snap, group).history.is_empty(), "{group} 的 history 应为空");
    }

    // ★ "delay == 0 单独不可靠"：alive=true 但最后一条测得 delay=0 时**不许**判成坏节点
    let proxies = serde_json::json!({
        "proxies": {
            "measured-down": {
                "type": "Shadowsocks", "alive": false,
                "history": [{ "time": "2026-09-21T23:00:00Z", "delay": 0 }]
            },
            "zero-but-alive": {
                "type": "Shadowsocks", "alive": true,
                "history": [{ "time": "2026-09-21T23:00:00Z", "delay": 0 }]
            },
            "has-delay": {
                "type": "Shadowsocks", "alive": true,
                "history": [
                    { "time": "2026-09-21T22:00:00Z", "delay": 0 },
                    { "time": "2026-09-21T23:00:00Z", "delay": 88 }
                ]
            }
        }
    });
    let snap2 = parse_snapshot(&proxies, &serde_json::json!({ "providers": {} })).unwrap();
    let get = |n: &str| snap2.proxies.iter().find(|p| p.name == n).unwrap();

    assert!(!get("measured-down").is_reachable(), "alive=false 就是不通");
    assert_eq!(get("measured-down").latest_delay(), Some(0), "实测不可达时 delay=0");
    assert!(get("zero-but-alive").is_reachable(), "delay=0 单独不足以判坏");
    assert_eq!(get("has-delay").latest_delay(), Some(88), "取最后一条，不是第一条");
}

// ================================================================ 形态容错

#[test]
fn an_empty_or_missing_provider_list_still_parses() {
    // 空 provider（真夹具 EMPTY.yaml 对应的形态：`proxies: []`）→ 解析成功、节点数 0。
    // 这正是就绪判定要拦住的那种"内核照常启动但 provider 没节点"。
    let empty = serde_json::json!({
        "providers": {
            "sub-jp": {
                "name": "sub-jp", "type": "Proxy", "vehicleType": "File",
                "proxies": [], "updatedAt": "2026-09-21T23:42:34+08:00"
            }
        }
    });
    let snap = parse_snapshot(&serde_json::json!({ "proxies": {} }), &empty).unwrap();
    assert_eq!(snap.providers.len(), 1);
    assert_eq!(snap.providers[0].node_count, 0);
    assert!(snap.providers[0].nodes.is_empty());
    assert!(snap.nodes.is_empty());
    assert!(snap.dangling.is_empty());
    assert!(snap.selectable().is_empty());

    // `providers` 缺失也应被当成"没有 provider"，而不是解析失败
    let snap = parse_snapshot(&serde_json::json!({}), &serde_json::json!({})).unwrap();
    assert!(snap.proxies.is_empty() && snap.providers.is_empty() && snap.nodes.is_empty());

    // 形态完全不对 → 报错而不是静默丢数据
    assert!(parse_snapshot(&serde_json::json!([1, 2, 3]), &serde_json::json!({})).is_err());
    assert!(parse_snapshot(&serde_json::json!({}), &serde_json::json!([1])).is_err());
}

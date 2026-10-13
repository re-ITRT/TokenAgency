//! **「自动」出口的选择规则**（[docs/29 §2.2]，用户第 88 轮原话）：
//!
//! > 自动选择的地址具有粘性，加入更新订阅之后如果存在一个代理的 IP(地址)和上次选的一样，
//! > 就选上次选的，唯二需要新选 ping 最低的情况就是用户手动点更新，和这条代理完全 ping 不通服务商
//!
//! 这条规则值得单独放一个模块、单独测：它是"出口 IP 会不会无故变化"的唯一裁决点，
//! 而出口 IP 乱跳正是用户要避免的封号风险。**判断必须是纯函数** —— 一旦掺进"当前延迟表"
//! 之类的运行时状态，就没法穷举验证"什么时候不该换"了。
//!
//! 三条写死的边界：
//!
//! * **按 IP（地址）比，不按名字**：机场改节点名很常见，按名字比会把"同一个出口"误判成
//!   "换了节点"，于是每次订阅更新都换 IP —— 那正是要避免的；
//! * **测不通（`latency_ms == None`）的节点永远不被选中**，包括"上次选的那条"：
//!   "完全 ping 不通服务商"正是用户说的第二种该重挑的情况；
//! * **全都不通 → `None`，并如实说明原因**：不编一个"看起来能用"的节点。

/// 一个候选出口（一个订阅节点）。延迟是**测出来的**，测不到就是 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCandidate<'a> {
    /// 节点名（**只用于显示/日志**，从不参与"是不是同一个出口"的判断）。
    pub name: &'a str,
    /// 出口地址（IP 或主机名）。**粘性就按它比。**
    pub address: &'a str,
    /// 到这家服务商 `base_url` 的延迟（毫秒）；`None` = 测不通。
    pub latency_ms: Option<u32>,
}

/// 自动出口的裁决结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDecision<'a> {
    /// 沿用上次那条（粘性命中）。`why` 写明命中理由，便于日志与 UI 解释。
    Keep { address: &'a str, why: KeepReason },
    /// 重新挑了一条延迟最低的。`why` 写明**为什么必须重挑**。
    Pick { address: &'a str, why: PickReason },
    /// 一条可用的都没有 —— 如实说，不编。
    None { reason: NoneReason },
}

/// 粘性命中的理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// 上次那条还在（按地址找到），而且它仍然通。
    SameAddressStillThere,
}

/// 必须重挑的理由（用户说的"唯二"两种，加上"上次那条不见了"这种自然情况）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickReason {
    /// 用户**手动**点了更新 → 无条件重挑（用户明确要求）。
    ManualRefresh,
    /// 上次那条**完全测不通**这家服务商 → 重挑。
    PreviousUnreachable,
    /// 上次那条在订阅里**已经不存在了**（机场换了节点）→ 重挑。
    PreviousGone,
    /// 从来没有选过（首次）→ 挑一条。
    FirstTime,
}

/// 没有可用出口的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoneReason {
    /// 一个候选都没有（订阅里没有节点 / 全是关掉的）。
    NoCandidates,
    /// 有候选，但**一个都测不通**。
    AllUnreachable,
}

/// 选一个出口。[docs/29 §2.2] 的规则，逐字实现：
///
/// 1. `manual`（用户手动更新）→ **无条件**重挑最低延迟；
/// 2. 否则，若 `previous_address` 仍在候选里**且仍然通** → **沿用**（哪怕有更快的）；
/// 3. 否则（上次那条不见了 / 不通 / 从没选过）→ 挑延迟最低的一条；
/// 4. 一个能用的都没有 → [`AutoDecision::None`]，原因如实。
///
/// 平局（延迟相同）时按输入顺序取第一条 —— 输入顺序 = 订阅里的顺序，
/// 这样同一份订阅每次得到的结论一致（可复现，便于核对）。
pub fn choose_auto_export<'a>(
    candidates: &'a [AutoCandidate<'a>],
    previous_address: Option<&str>,
    manual: bool,
) -> AutoDecision<'a> {
    if candidates.is_empty() {
        return AutoDecision::None { reason: NoneReason::NoCandidates };
    }

    // ① 用户手动更新：无条件重挑（这是用户明确要求的两种重挑之一）。
    if manual {
        return match lowest_latency(candidates) {
            Some(c) => AutoDecision::Pick { address: c.address, why: PickReason::ManualRefresh },
            None => AutoDecision::None { reason: NoneReason::AllUnreachable },
        };
    }

    // ② 粘性：上次那条还在、而且仍然通 → 沿用（**哪怕它不是最快的**）。
    if let Some(previous) = previous_address {
        match candidates.iter().find(|c| c.address == previous) {
            Some(c) if c.latency_ms.is_some() => {
                return AutoDecision::Keep {
                    address: c.address,
                    why: KeepReason::SameAddressStillThere,
                };
            }
            Some(_) => {
                // 上次那条还在，但完全测不通服务商 → 必须重挑
                return match lowest_latency(candidates) {
                    Some(c) => AutoDecision::Pick {
                        address: c.address,
                        why: PickReason::PreviousUnreachable,
                    },
                    None => AutoDecision::None { reason: NoneReason::AllUnreachable },
                };
            }
            None => {
                // 上次那条在订阅里没了 → 重挑（并在日志里说明是"不见了"而不是"不通"）
                return match lowest_latency(candidates) {
                    Some(c) => {
                        AutoDecision::Pick { address: c.address, why: PickReason::PreviousGone }
                    }
                    None => AutoDecision::None { reason: NoneReason::AllUnreachable },
                };
            }
        }
    }

    // ③ 首次：挑最低延迟。
    match lowest_latency(candidates) {
        Some(c) => AutoDecision::Pick { address: c.address, why: PickReason::FirstTime },
        None => AutoDecision::None { reason: NoneReason::AllUnreachable },
    }
}

/// 把**订阅解析出来的节点** + **内核测出来的延迟**拼成候选列表（[docs/29 §2.2]）。
///
/// 两件事必须说清：
///
/// * `address` 取节点的 **`server`**（IP 或主机名）—— 粘性就是按它比的：机场改名很常见，
///   按名字比会把"同一个出口"误判成"换了节点"，于是每次订阅更新都换 IP（封号风险）。
///   节点的 `name` 只用于显示与内核测速（`delay_of(name)` 收的就是它）；
/// * `latency_ms` 的表里**没有这个节点** 与 **表里明确是 `None`** 都是"测不到"，
///   一律落成 `None` —— **绝不用 0 冒充**（0 会被当成"最快"，那就选中一条其实不通的出口）。
pub fn candidates_from_nodes<'a>(
    nodes: &'a [crate::NodeMeta],
    latency_ms: &std::collections::HashMap<String, Option<u32>>,
) -> Vec<AutoCandidate<'a>> {
    nodes
        .iter()
        .map(|node| AutoCandidate {
            name: node.name.as_str(),
            address: node.server.as_str(),
            latency_ms: latency_ms.get(&node.name).copied().flatten(),
        })
        .collect()
}
/// **一次「自动」出口裁决的完整结论**（[docs/29 §2.2]）：探测结果进来，出去的是"选谁"。
///
/// 把这一段抽成纯函数，是因为它是"出口 IP 会不会乱变"的**全部判断**：
/// 粘性命中、手动重挑、上次不见了、全都不通 —— 四种走向都在这里定，且**都能被穷举测**。
/// 真机上要验的只剩"探测拿到的延迟对不对"与"写内核成不成"，那是 I/O，不再混着判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportChoice<'a> {
    /// 要写进内核的**节点名**（`None` = 什么都不写）。
    pub node: Option<&'a str>,
    /// 该节点的**地址**（粘性按它记；`None` 同上）。
    pub address: Option<&'a str>,
    /// 结论理由（[`KeepReason`] / [`PickReason`] / [`NoneReason`] 的名字，供日志与界面）。
    pub why: &'static str,
}

/// 由**探测结果 + 上次选的地址 + 是否手动**得出"选谁"（纯函数，[docs/29 §2.2]）。
///
/// `manual = true`（用户手动刷新）→ 无视粘性重挑；`false` → 上次那条**地址还在且测得到**
/// 就沿用（哪怕有更快的）；地址不见了 → `address_gone`（**不编名字**）；全都不通 → `all_unreachable`。
pub fn decide_export<'a>(
    candidates: &'a [AutoCandidate<'a>],
    previous_address: Option<&str>,
    manual: bool,
) -> ExportChoice<'a> {
    match choose_auto_export(candidates, previous_address, manual) {
        AutoDecision::Keep { address, why } => ExportChoice {
            node: node_name_for_address(candidates, address),
            address: Some(address),
            why: match why {
                KeepReason::SameAddressStillThere => "same_address_still_there",
            },
        },
        AutoDecision::Pick { address, why } => ExportChoice {
            node: node_name_for_address(candidates, address),
            address: Some(address),
            why: match why {
                PickReason::ManualRefresh => "manual_refresh",
                PickReason::PreviousUnreachable => "previous_unreachable",
                PickReason::PreviousGone => "previous_gone",
                PickReason::FirstTime => "first_time",
            },
        },
        AutoDecision::None { reason } => ExportChoice {
            node: None,
            address: None,
            why: match reason {
                NoneReason::NoCandidates => "no_candidates",
                NoneReason::AllUnreachable => "all_unreachable",
            },
        },
    }
}
/// 把裁决给出的**地址**翻成内核要的**节点名**（[docs/29 §2.2]）。
///
/// 为什么要单独一个函数：粘性是按**地址**比的（机场改名很常见），而内核 `set_select` 收的是
/// **节点名** —— 这两个标识在整条链路上是**分开**的，翻译这一步错了就会"选对了出口却写错节点"。
/// `None` 的语义是**这条地址已经不在当前候选里**（订阅换了节点），调用方据此如实返回
/// "不见了"，而**不是**编一个名字出来。
///
/// ★ **同一地址可能有多条候选**（机场常见：同一台服务器上开多个端口/多条线路）——
/// 优先返回**测得到延迟**的那条的**名字**：否则会"用测得到的那条选出地址，却把测不到的那条的
/// 名字写进内核"（真机实测：`picked_ms` 因此是空的，而它是"选了最低延迟"的自证字段）。
pub fn node_name_for_address<'a>(
    candidates: &'a [AutoCandidate<'a>],
    address: &str,
) -> Option<&'a str> {
    candidates
        .iter()
        .find(|c| c.address == address && c.latency_ms.is_some())
        .or_else(|| candidates.iter().find(|c| c.address == address))
        .map(|c| c.name)
}

/// 某个**地址**在这一轮那张表里的延迟（同地址多条时取**已测得里最小的**那条）。
///
/// 与 [`node_name_for_address`] 配对使用：名字与延迟必须来自同一批"可用的"候选，
/// 否则"选了最低延迟"这句话就自证不了。
pub fn latency_for_address(candidates: &[AutoCandidate<'_>], address: &str) -> Option<u32> {
    candidates.iter().filter(|c| c.address == address).filter_map(|c| c.latency_ms).min()
}
/// 延迟最低的**可用**节点（测不通的不参与）；平局取输入顺序靠前的。
fn lowest_latency<'a, 'b>(candidates: &'b [AutoCandidate<'a>]) -> Option<&'b AutoCandidate<'a>> {
    candidates
        .iter()
        .filter(|c| c.latency_ms.is_some())
        .min_by_key(|c| c.latency_ms.unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c<'a>(name: &'a str, address: &'a str, latency_ms: Option<u32>) -> AutoCandidate<'a> {
        AutoCandidate { name, address, latency_ms }
    }

    #[test]
    fn candidates_come_from_node_server_and_never_invent_a_latency() {
        use crate::NodeMeta;
        use std::collections::HashMap;

        let nodes = vec![
            NodeMeta {
                name: "东京-01".into(),
                kind: "ss".into(),
                server: "203.0.113.7".into(),
                port: 443,
            },
            NodeMeta {
                name: "没测过的".into(),
                kind: "ss".into(),
                server: "198.51.100.9".into(),
                port: 443,
            },
            NodeMeta {
                name: "测不通的".into(),
                kind: "ss".into(),
                server: "192.0.2.5".into(),
                port: 443,
            },
        ];
        let mut latency: HashMap<String, Option<u32>> = HashMap::new();
        latency.insert("东京-01".into(), Some(42));
        latency.insert("测不通的".into(), None);
        // "没测过的"**不在表里**

        let list = candidates_from_nodes(&nodes, &latency);
        assert_eq!(list.len(), 3);
        // 地址取 server（粘性按它比），名字只用于显示
        assert_eq!(list[0].address, "203.0.113.7");
        assert_eq!(list[0].name, "东京-01");
        assert_eq!(list[0].latency_ms, Some(42));
        // 表里没有 = 测不到，不是 0
        assert_eq!(list[1].latency_ms, None);
        // 表里明确 None = 测不到
        assert_eq!(list[2].latency_ms, None);

        // 于是"只有一条真的通"时，选出来的必然是它（不是那条 0 假延迟的）
        assert_eq!(
            choose_auto_export(&list, None, false),
            AutoDecision::Pick { address: "203.0.113.7", why: PickReason::FirstTime }
        );
        // 全都不通（表空）→ 如实说"全都不通"，不选任何一条
        assert_eq!(
            choose_auto_export(&candidates_from_nodes(&nodes, &HashMap::new()), None, false),
            AutoDecision::None { reason: NoneReason::AllUnreachable }
        );
    }
    #[test]
    fn decide_export_covers_every_outcome_with_the_right_reason() {
        let list = [c("hk", "203.0.113.7", Some(30)), c("jp", "198.51.100.9", Some(80))];
        // ① 首次：挑最低
        let first = decide_export(&list, None, false);
        assert_eq!(
            (first.node, first.address, first.why),
            (Some("hk"), Some("203.0.113.7"), "first_time")
        );
        // ② 粘性：上次那条还在且通 → 沿用（哪怕它不是最快的）
        let keep = decide_export(&list, Some("198.51.100.9"), false);
        assert_eq!(
            (keep.node, keep.address, keep.why),
            (Some("jp"), Some("198.51.100.9"), "same_address_still_there")
        );
        // ③ 手动：无视粘性，重挑最低
        let manual = decide_export(&list, Some("198.51.100.9"), true);
        assert_eq!(
            (manual.node, manual.address, manual.why),
            (Some("hk"), Some("203.0.113.7"), "manual_refresh")
        );

        // ④ 上次那条不见了 → previous_gone（换到最低的）
        let gone = decide_export(&list, Some("192.0.2.5"), false);
        assert_eq!(
            (gone.node, gone.address, gone.why),
            (Some("hk"), Some("203.0.113.7"), "previous_gone")
        );

        // ⑤ 上次那条测不通 → previous_unreachable
        let dead = [c("hk", "203.0.113.7", None), c("jp", "198.51.100.9", Some(80))];
        let un = decide_export(&dead, Some("203.0.113.7"), false);
        assert_eq!(
            (un.node, un.address, un.why),
            (Some("jp"), Some("198.51.100.9"), "previous_unreachable")
        );

        // ⑥ 全都不通 → 什么都不写，如实说
        let all_dead = [c("hk", "203.0.113.7", None)];
        let none = decide_export(&all_dead, Some("203.0.113.7"), false);
        assert_eq!((none.node, none.address, none.why), (None, None, "all_unreachable"));
        // ⑦ 一个候选都没有 → 与"全不通"**分开说**
        let empty = decide_export(&[], None, false);
        assert_eq!((empty.node, empty.address, empty.why), (None, None, "no_candidates"));
    }
    #[test]
    fn address_to_node_name_is_a_separate_translation() {
        let list = [c("东京-改了个名", "203.0.113.7", Some(20)), c("b", "198.51.100.9", Some(30))];
        // 按**地址**找名字：名字变了也能找到（粘性按地址，内核按名字）
        assert_eq!(node_name_for_address(&list, "203.0.113.7"), Some("东京-改了个名"));
        assert_eq!(node_name_for_address(&list, "198.51.100.9"), Some("b"));
        // 地址不在候选里 → None（调用方如实说"这条已经不在订阅里"，不编名字）
        assert_eq!(node_name_for_address(&list, "192.0.2.5"), None);
    }
    #[test]
    fn first_time_picks_the_lowest_latency() {
        let list = [c("a", "1.1.1.1", Some(120)), c("b", "2.2.2.2", Some(30))];
        assert_eq!(
            choose_auto_export(&list, None, false),
            AutoDecision::Pick { address: "2.2.2.2", why: PickReason::FirstTime }
        );
    }

    #[test]
    fn stickiness_keeps_the_previous_even_when_a_faster_one_appeared() {
        // 这是用户要的核心：订阅更新后多了一条更快的，**也不许换**（否则出口 IP 就变了）
        let list = [c("old", "1.1.1.1", Some(200)), c("new", "2.2.2.2", Some(10))];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), false),
            AutoDecision::Keep { address: "1.1.1.1", why: KeepReason::SameAddressStillThere }
        );
    }

    #[test]
    fn stickiness_is_by_address_not_by_name() {
        // 机场把节点名改了、地址没变 → 仍然是"同一个出口"，必须继续沿用
        let list = [c("东京-改了个名", "1.1.1.1", Some(200)), c("other", "9.9.9.9", Some(5))];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), false),
            AutoDecision::Keep { address: "1.1.1.1", why: KeepReason::SameAddressStillThere }
        );
    }

    #[test]
    fn manual_refresh_repicks_even_when_the_previous_is_fine() {
        let list = [c("old", "1.1.1.1", Some(20)), c("other", "2.2.2.2", Some(200))];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), true),
            AutoDecision::Pick { address: "1.1.1.1", why: PickReason::ManualRefresh }
        );
        // 手动重挑时"最低延迟"才是裁决依据：这里换成另一条更快的 → 就该换
        let list2 = [c("old", "1.1.1.1", Some(200)), c("other", "2.2.2.2", Some(20))];
        assert_eq!(
            choose_auto_export(&list2, Some("1.1.1.1"), true),
            AutoDecision::Pick { address: "2.2.2.2", why: PickReason::ManualRefresh }
        );
    }

    #[test]
    fn previous_unreachable_repicks_to_the_lowest_reachable() {
        let list = [
            c("old", "1.1.1.1", None), // 完全 ping 不通服务商
            c("x", "2.2.2.2", Some(80)),
            c("y", "3.3.3.3", Some(40)),
        ];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), false),
            AutoDecision::Pick { address: "3.3.3.3", why: PickReason::PreviousUnreachable }
        );
    }

    #[test]
    fn previous_gone_repicks_and_says_so() {
        let list = [c("x", "2.2.2.2", Some(80)), c("y", "3.3.3.3", Some(40))];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), false),
            AutoDecision::Pick { address: "3.3.3.3", why: PickReason::PreviousGone }
        );
    }

    #[test]
    fn all_unreachable_is_reported_honestly_without_inventing_a_node() {
        let list = [c("a", "1.1.1.1", None), c("b", "2.2.2.2", None)];
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), false),
            AutoDecision::None { reason: NoneReason::AllUnreachable }
        );
        assert_eq!(
            choose_auto_export(&list, Some("1.1.1.1"), true),
            AutoDecision::None { reason: NoneReason::AllUnreachable }
        );
    }

    #[test]
    fn no_candidates_is_a_different_reason_than_all_unreachable() {
        assert_eq!(
            choose_auto_export(&[], None, false),
            AutoDecision::None { reason: NoneReason::NoCandidates }
        );
    }

    #[test]
    fn ties_are_deterministic_by_input_order() {
        let list = [c("first", "1.1.1.1", Some(50)), c("second", "2.2.2.2", Some(50))];
        let decision = choose_auto_export(&list, None, false);
        assert_eq!(decision, AutoDecision::Pick { address: "1.1.1.1", why: PickReason::FirstTime });
        // 同一份输入永远给同一个结论（可复现）
        assert_eq!(choose_auto_export(&list, None, false), decision);
    }

    /// ★ **同一地址多条候选**（真机踩到过）：名字与延迟必须来自同一条"可用的"候选。
    #[test]
    fn duplicate_addresses_translate_to_the_measured_one() {
        let list = [
            // 同地址：第一条测不到，第二条测得到 —— 选出来的是"2.2.2.2"，写内核的名字必须是
            // **测得到的那条**（否则"选了最低延迟"的 `picked_ms` 会是空的）。
            c("同地址-测不到", "2.2.2.2", None),
            c("同地址-测得到", "2.2.2.2", Some(80)),
            c("另一条", "3.3.3.3", Some(300)),
        ];
        let choice = decide_export(&list, None, true);
        assert_eq!(choice.address, Some("2.2.2.2"));
        assert_eq!(choice.node, Some("同地址-测得到"));
        assert_eq!(latency_for_address(&list, "2.2.2.2"), Some(80));
        // 地址根本不在候选里 ⇒ `None`（不编名字/不编延迟）
        assert_eq!(node_name_for_address(&list, "9.9.9.9"), None);
        assert_eq!(latency_for_address(&list, "9.9.9.9"), None);
    }
}

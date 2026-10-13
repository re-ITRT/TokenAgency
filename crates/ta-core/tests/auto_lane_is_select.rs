//! **「自动」出口的组类型必须是 `select`，而且必须压过订阅自己建议的组类型**。
//!
//! 依据 [docs/29 §2.2]（用户第 88 轮原话）+ 第 99 轮查清的事实：
//! `subscription_auto` 今天被 `ta-kernel::config_gen` 生成成内核的 **`url-test`** 组，
//! 而 `url-test` 会**按自己的 interval 周期性地重测重选** —— 出口 IP 会自己变，
//! 正是用户要避免的（同一服务商一个 URL，换出口 IP = 同账号多地并发 = 封号风险）。
//!
//! 这一组测试钉住三件事，任何一件坏了「自动」就不再是"粘"的：
//! 1. 种类仍然是 `subscription_auto`（别偷偷换成别的种类，界面文案与内核映射都按它走）；
//! 2. `group_type` 是**显式** `Select`；
//! 3. 即使订阅自己建议 `url-test`，**显式值优先**（`effective_group_type` 不得被带跑）。

use ta_core::domain::{GroupType, Lane, LaneKind};

#[test]
fn automatic_is_subscription_auto_with_an_explicit_select_group() {
    let lane = Lane::automatic("lane-auto", "自动", "sub-1");

    assert_eq!(
        lane.kind,
        LaneKind::SubscriptionAuto,
        "「自动」的种类就是既有的整份订阅择优，不新造种类"
    );
    assert_eq!(lane.subscription.as_ref().map(|s| s.as_str()), Some("sub-1"));
    assert_eq!(
        lane.group_type,
        Some(GroupType::Select),
        "必须是显式 select：url-test 会自己周期性重选，粘性就不存在了"
    );
}

#[test]
fn the_explicit_select_wins_over_a_url_test_suggested_by_the_subscription() {
    let lane = Lane::automatic("lane-auto", "自动", "sub-1");

    // 机场在订阅里写 `url-test` 是常见情况：显式值必须优先，否则我们又回到"自己换出口"
    assert_eq!(
        lane.effective_group_type(Some(GroupType::UrlTest)),
        Some(GroupType::Select),
        "订阅建议的 url-test 不许覆盖我们显式的 select"
    );
    // 订阅没建议时也还是 select
    assert_eq!(
        lane.effective_group_type(None),
        Some(GroupType::Select),
        "订阅没给建议时仍然是 select"
    );
}

#[test]
fn the_plain_subscription_auto_keeps_its_old_behaviour() {
    // 反向钉住：老的 `subscription_auto`（界面上叫「整份订阅择优」）**行为不变** ——
    // 它没有显式组类型，仍然接受订阅建议的 url-test。
    // 这样"自动"是一个**新增**的选择，而不是把老语义悄悄改掉。
    let lane = Lane::subscription_auto("lane-sub-auto", "整份订阅择优", "sub-1");
    assert_eq!(lane.kind, LaneKind::SubscriptionAuto);
    assert_eq!(lane.group_type, None, "老构造函数不许被改成显式 select");
    assert_eq!(
        lane.effective_group_type(Some(GroupType::UrlTest)),
        Some(GroupType::UrlTest),
        "老语义仍然跟随订阅建议"
    );
}

/// D 期幂等建线要用它：**在已有线路里认出「自动」那一条**。
///
/// 关键在"只看种类是不够的"：老的「整份订阅择优」也是 `subscription_auto`，
/// 但它的组类型是 `url-test`（会自己周期性换出口）—— 把它当成「自动」，
/// 等于用户选了"粘"却拿到"会自己跳"。
#[test]
fn find_the_auto_lane_only_when_the_group_is_explicitly_select() {
    let mut doc = ta_core::domain::ConfigDoc::default();

    // 老的「整份订阅择优」：同样挂在 sub-1 上，但组类型不是显式 select → **不算**
    doc.lanes.push(Lane::subscription_auto("lane-sub-auto", "整份订阅择优", "sub-1"));
    assert!(doc.auto_lane_on(&"sub-1".into()).is_none(), "url-test 的整份订阅择优不是「自动」");

    // 「自动」：显式 select → 认出来
    doc.lanes.push(Lane::automatic("lane-auto", "自动", "sub-1"));
    let found = doc.auto_lane_on(&"sub-1".into());
    assert_eq!(found.map(|l| l.id.as_str()), Some("lane-auto"), "显式 select 的那条才是「自动」");

    // 别的订阅上的「自动」不算这一家的
    doc.lanes.push(Lane::automatic("lane-auto-2", "自动", "sub-2"));
    assert_eq!(
        doc.auto_lane_on(&"sub-1".into()).map(|l| l.id.as_str()),
        Some("lane-auto"),
        "按订阅匹配，不能串台"
    );
    assert_eq!(doc.auto_lane_on(&"sub-2".into()).map(|l| l.id.as_str()), Some("lane-auto-2"));

    // 多条都够格时取配置里靠前的那条（确定性）
    doc.lanes.push(Lane::automatic("lane-auto-3", "自动（第二份）", "sub-1"));
    assert_eq!(
        doc.auto_lane_on(&"sub-1".into()).map(|l| l.id.as_str()),
        Some("lane-auto"),
        "同一份配置永远给同一个答案"
    );
}

/// 幂等建线的**决策**：按 `base_id` 复用、撞了就派生一个不撞的 id。
///
/// 三条边界都是真机上很难试出来的，所以放在这里钉死：
/// 1. `base_id` 本身就是一条够格的「自动」→ **复用**，绝不新建第二条；
/// 2. id 被**别的**线路占了 → 往后加 `-2`（**绝不改别人的 id**：改 id 会让引用它的服务商
///    指向不存在的线路，那是配置损坏）；
/// 3. **另一家服务商拿自己的 `base_id` 时必须走"建"**，不许复用别人那条 ——
///    这一条是用户第 202 轮的要求（「自动」是每家服务商自己的出口）：id 里带服务商，
///    两家就各自有一条 lane，不会共用同一个组、互相覆盖选择。
#[test]
fn plan_auto_lane_reuses_by_id_then_derives_without_touching_others() {
    use ta_core::domain::AutoLanePlan;

    let mut doc = ta_core::domain::ConfigDoc::default();
    let sub: ta_core::types::SubscriptionId = "sub-1".into();

    // ① 什么都没有 → 建
    let plan = doc.plan_auto_lane(&sub, "lane-p-a-auto", "自动（A）");
    assert_eq!(
        plan,
        AutoLanePlan::Create { id: "lane-p-a-auto".into(), name: "自动（A）".to_string() }
    );

    // ② 那条 id 被别人占了 → 派生 `-2`，且**不动**别人
    doc.lanes.push(Lane::direct("lane-p-a-auto", "别人占了这个 id"));
    let plan = doc.plan_auto_lane(&sub, "lane-p-a-auto", "自动（A）");
    assert_eq!(
        plan,
        AutoLanePlan::Create { id: "lane-p-a-auto-2".into(), name: "自动（A）".to_string() },
        "id 撞了要往后让，不能改别人的线路 id"
    );

    // ③ 把「自动」真建出来 → 同一个 base_id 再计划就是**复用**（幂等）
    doc.lanes.push(Lane::automatic("lane-p-a-auto-2", "自动（A）", "sub-1"));
    assert_eq!(
        doc.plan_auto_lane(&sub, "lane-p-a-auto-2", "自动（A）"),
        AutoLanePlan::Reuse("lane-p-a-auto-2".into()),
        "同一个 base_id 已经是一条合格的「自动」就必须复用"
    );

    // ④ 另一家服务商（自己的 base_id）→ **建另一条**，不复用 A 那条
    assert_eq!(
        doc.plan_auto_lane(&sub, "lane-p-b-auto", "自动（B）"),
        AutoLanePlan::Create { id: "lane-p-b-auto".into(), name: "自动（B）".to_string() },
        "「自动」是每家服务商自己的：两家不许共用一条 lane"
    );
}

/// 只读判断（`auto_lane_on`）现在只用于"这条订阅上有没有够格的『自动』"，
/// 所以它按订阅取**靠前的第一条**没问题；但**按 id 找**必须只认那一条（建线用它）。
#[test]
fn by_id_lookup_does_not_borrow_another_providers_auto_lane() {
    let mut doc = ta_core::domain::ConfigDoc::default();
    doc.lanes.push(Lane::automatic("lane-p-a-auto", "自动（A）", "sub-1"));
    doc.lanes.push(Lane::automatic("lane-p-b-auto", "自动（B）", "sub-1"));
    let sub: ta_core::types::SubscriptionId = "sub-1".into();

    assert_eq!(
        doc.auto_lane_by_id(&"lane-p-a-auto".into(), &sub).map(|l| l.id.as_str()),
        Some("lane-p-a-auto")
    );
    assert_eq!(
        doc.auto_lane_by_id(&"lane-p-b-auto".into(), &sub).map(|l| l.id.as_str()),
        Some("lane-p-b-auto")
    );
    // 别的订阅上的那条不算这一家的
    assert!(doc.auto_lane_by_id(&"lane-p-a-auto".into(), &"sub-2".into()).is_none());
    // 只读判断仍按订阅取第一条（确定性）
    assert_eq!(
        doc.auto_lane_on(&sub).map(|l| l.id.as_str()),
        Some("lane-p-a-auto"),
        "同一份配置永远给同一个答案"
    );
}

-- 0003：`subscription` 表补上**上次已知流量**
--
-- 背景：Clash 生态在订阅响应头 `subscription-userinfo` 里给流量/到期（字节 + Unix 秒）。
-- 那个头只有"拉订阅的那一瞬间"可得，而列表要一直显示"上次已知" —— 不落库就只能
-- 每开一次页面重拉一次机场（既慢又费流量）。所以在这里存一份快照。
--
-- ★ **全部可空，语义是"未知 = NULL"**：
--   `total_bytes` 与 `expire_at_s` 的 0 表示"不限量 / 不适用"，写入时就已经归一成 NULL；
--   `upload_bytes` / `download_bytes` 的 0 是**真实观测值**（真没用过），照写 0。
--   两条口径的区别在 `ta_subscription::traffic::parse_traffic` 一处定义，这里只负责存。
--
--   `traffic_at_ms` 是"这一次拿到的时刻"（UI 的「更新时间」）。
--   它同时是**这一行有没有数据的判据**：NULL = 从未成功刷新过 → 命令返回 `traffic: null`
--   （而不是 `{upload: null, ...}` 那种"有对象但没数"）。所以它不许有默认值。
--
-- 迁移只增不改：0001/0002 已发布，改它们是灾难。
ALTER TABLE subscription ADD COLUMN upload_bytes   INTEGER;
ALTER TABLE subscription ADD COLUMN download_bytes INTEGER;
ALTER TABLE subscription ADD COLUMN total_bytes    INTEGER;
ALTER TABLE subscription ADD COLUMN expire_at_s    INTEGER;
ALTER TABLE subscription ADD COLUMN traffic_at_ms  INTEGER;

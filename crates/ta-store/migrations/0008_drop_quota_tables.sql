-- 0008：删除本地额度账本的三张表。
--
-- 背景：额度功能（自动抓取 + 本地账本 + 路由按剩余额度排序）已整体下线。
-- 0002 / 0005 建过这三张表，但**已应用的迁移不许改**（改了会让
-- `_sqlx_migrations` 的校验和对不上，老库直接启动失败），所以这里用一条新迁移删掉它们。
--
-- 删表而不是留空表：
--   * 留着就会有人（包括我自己）以后看见表名以为功能还在，重新接上去；
--   * 这三张表存的是"按凭据累计的用量与窗口状态"，没有历史审计价值
--     （真正的用量真源是 `usage_request` / `usage_daily`，成本中心用的是它们）。
--
-- `IF EXISTS` 是为了兼容"从没跑到 0005 就升级"的库，以及重复执行的幂等。
DROP TABLE IF EXISTS quota_window_bucket;
DROP TABLE IF EXISTS quota_window_state;
DROP TABLE IF EXISTS quota_state;

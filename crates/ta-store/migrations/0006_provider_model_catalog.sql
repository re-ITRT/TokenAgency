-- 0006：服务商**模型目录** —— `provider_model_catalog`（docs/25）。
--
-- 背景（用户第 77 轮原话）："一旦扫描，全部记录下来(当然不是添加)，然后比如用户那一天想起，
-- 我还想用 xxx 模型，直接在服务商里面点进去，把那个没勾的勾上就行了"。
-- 随后用户明确否决了手动添加："不行，我想应该不能手动添加，这样会坏自动化逻辑" ——
-- 所以**这一版没有 `origin` 列**：目录里的每一行都必须来自一次真实扫描。
--
-- 此前扫描结果**只活在进程内存**（`ImportPreviewCache`，TTL 5 分钟、上限 32 条、重启即失），
-- 所以"扫过但没勾"的模型关掉向导就再也找不回来。要让"扫过就记得住"成立，只有把目录落到
-- SQLite（**观察数据**），而"接入与否"（= 配置里真有 `Model` + 一条指到这家服务商的通道）
-- 继续留在配置真源里（**用户意图**）。两者不混：目录既**不进** YAML 导出/回滚，
-- 也**不进**配置表。
--
-- 一行 = 一对 `(provider_id, upstream_model)`，主键就是它 —— 这正是 upsert 的冲突键，
-- 也是"这家服务商扫描到过哪些模型"的唯一身份。**同一上游模型在这家下只可能有一行**。
--
-- ★ `first_seen_ms` = **第一次见到**（不是"最后一次"）。用户要能看出"这个模型我什么时候第一次扫到"，
--   所以 upsert 不覆盖它；`last_seen_ms` 才是每次观测都刷新的那个。
--
-- ★ 删除只发生在 `prune_unseen`：`seen_in_last_scan = 0 AND upstream_model NOT IN (keep)`。
--   `keep` 由调用方给，内容是"这家**还接着通道**的上游名" —— 用户已经接过的东西不因为
--   一次扫描消失（否则界面会出现"有一条通道、但模型区看不到这个模型"的鬼状态）。
--   `provider_id` 删除时由调用方显式调 `remove_provider` 清干净（与 `delete_provider`
--   级联清额度状态同一处）。
--
-- ★ **不额外建索引**：主键 `(provider_id, upstream_model)` 在 SQLite 的 rowid 表上自带一个
--   唯一索引，列序正好覆盖本表全部查询路径 ——
--     * `list`       : `WHERE provider_id = ? ORDER BY upstream_model`（等值 + 该索引内的有序范围）
--     * `list_all`   : `ORDER BY provider_id, upstream_model`（= 索引本身的全扫顺序）
--     * `mark_all_unseen` / `remove_provider` : `WHERE provider_id = ?`（最左前缀）
--     * `prune`      : `WHERE provider_id = ?` 先收窄到"这一家"，再在结果里按 seen 过滤；
--       一家的模型量级是"几十行"，且这一条本来就要求回传被删的清单（先选后删），
--       加索引只是让每次 upsert 多维护一棵 B 树，没有查询受益。
--
-- 迁移只增不改（docs/11-internal-contracts.md §4.1）：0001–0005 已发布，改它们是灾难。
CREATE TABLE provider_model_catalog (
  provider_id    TEXT    NOT NULL,
  upstream_model TEXT    NOT NULL,
  suggested_id   TEXT    NOT NULL,              -- 逻辑模型名：扫描派生（唯一真源）
  seen_in_last_scan INTEGER NOT NULL DEFAULT 0, -- 上一次扫描返回过它吗
  first_seen_ms  INTEGER NOT NULL,              -- 第一次见到；upsert **不覆盖**
  last_seen_ms   INTEGER NOT NULL,              -- 最后一次见到；每次观测都刷新
  PRIMARY KEY (provider_id, upstream_model)
);

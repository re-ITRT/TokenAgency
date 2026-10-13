"""一次性验证工具：让 scratch 数据里的订阅看起来"从未成功刷新过"，
好观察 `due_subscription_ids` 的**非空**分支在真机上的那行日志。

只动 `subscription` 这张**运行期观测表**（配置真源在 `config` 表里，不受影响）；
跑之前先备份整个 DB 文件，验证完再还原。
"""
import os
import shutil
import sqlite3
import sys

ROOT = os.path.join(os.environ["TEMP"], "ta-e2e", "data")
DB = os.path.join(ROOT, "db.sqlite")
BACKUP = os.path.join(ROOT, "db.sqlite.probe-backup")


def main() -> int:
    if not os.path.exists(DB):
        print(f"找不到 {DB}")
        return 1
    if sys.argv[1:] == ["--restore"]:
        if not os.path.exists(BACKUP):
            print("没有备份可还原")
            return 1
        for suffix in ("", "-wal", "-shm"):
            if os.path.exists(DB + suffix):
                os.remove(DB + suffix)
        shutil.copy2(BACKUP, DB)
        print("已还原", DB)
        return 0

    if not os.path.exists(BACKUP):
        shutil.copy2(DB, BACKUP)
        for suffix in ("-wal", "-shm"):
            if os.path.exists(DB + suffix):
                shutil.copy2(DB + suffix, BACKUP + suffix)
        print("已备份 →", BACKUP)

    con = sqlite3.connect(DB)
    tables = [r[0] for r in con.execute("select name from sqlite_master where type='table'")]
    print("表:", tables)
    if "subscription" not in tables:
        print("这张库里没有 subscription 表（没什么可做的）")
        return 0
    before = con.execute("select count(*) from subscription").fetchone()[0]
    con.execute("delete from subscription")
    con.commit()
    after = con.execute("select count(*) from subscription").fetchone()[0]
    con.close()
    print(f"subscription 行数：{before} → {after}（清空后每条订阅都算从未成功刷新过 → 到期）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""核对 scratch 数据库的还原结果（只读）。"""
import os
import sqlite3

DB = os.path.join(os.environ["TEMP"], "ta-e2e", "data", "db.sqlite")
con = sqlite3.connect(DB)
tables = [r[0] for r in con.execute("select name from sqlite_master where type='table'")]
print("表数量:", len(tables))
if "subscription" in tables:
    rows = con.execute("select count(*) from subscription").fetchone()[0]
    print("subscription 行数:", rows)
else:
    print("subscription 表不存在")
if "config" in tables:
    print("config 行数:", con.execute("select count(*) from config").fetchone()[0])
con.close()

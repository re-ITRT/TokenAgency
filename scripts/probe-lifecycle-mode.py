"""看一眼真机 E2E 的隔离配置里「生命周期模式」是什么（白屏的托盘假设）。

只读，不改任何东西：把 `config` 表里 `section`/`meta` 类实体的 JSON 打出来，
找 `mode`（`Window` / `Tray` / `Minimize`）那一项。
"""

import json
import os
import sqlite3
import sys

data_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.environ.get("TEMP", ""), "ta-e2e", "data"
)
db = os.path.join(data_dir, "db.sqlite")
print(f"db: {db} 存在={os.path.exists(db)}")
if not os.path.exists(db):
    raise SystemExit(0)

con = sqlite3.connect(db)
for kind, entity_id, raw in con.execute(
    "select kind, entity_id, json from config order by kind, ord"
).fetchall():
    if kind not in ("section", "meta"):
        continue
    try:
        doc = json.loads(raw)
    except Exception:
        continue
    blob = json.dumps(doc, ensure_ascii=False)
    if "mode" in blob or "lifecycle" in blob or "tray" in blob:
        print(f"— {kind} {entity_id}: {blob[:400]}")

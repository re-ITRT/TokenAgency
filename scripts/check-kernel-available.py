"""一次性检查：这个环境里**内核能不能跑**(E 期"选最低延迟"的真机前提)。

只看，不做任何修改：

1. **内置内核**在不在(与 `src-tauri/src/boot.rs::bundled_kernel_path()` 的查找顺序一致)--
   内置内核之后安装包会**随包分发** mihomo，所以这是现在的主要路径；
2. 配置里 `net.kernel.path` 指向哪、那个文件在不在(用户显式指定时它优先)；
3. PATH 里有没有 mihomo/clash(自备内核的老路径)。

历史背景：这个脚本原来只报"配置里没内核、PATH 里也没有"，那是**内置内核之前**的事实；
不改它就会给出**不真实**的结论(现在真机日志里已经是 `内核已就绪 pid=… version=v1.19.31`)。
"""

import json
import os
import shutil
import sqlite3
import sys

CANDIDATES = [
    "mihomo.exe",  # 与 exe 平铺(NSIS 安装)
    os.path.join("resources", "mihomo", "mihomo.exe"),  # bundle.resources 保留相对路径
    os.path.join("resources", "resources", "mihomo", "mihomo.exe"),  # 某些版本会多套一层
]


def bundled_kernel(exe_dir: str):
    """按 `boot.rs` 的顺序找内置内核(找不到返回 None，绝不猜)。"""
    for rel in CANDIDATES:
        p = os.path.join(exe_dir, rel)
        if os.path.isfile(p):
            return p
    return None


def main() -> int:
    exe_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(sys.executable or ".")
    print(f"某个可执行文件目录：{exe_dir}")

    found = bundled_kernel(exe_dir)
    print(f"内置内核：{found or '(没找到)'}")
    if found:
        print(f"  体积：{os.path.getsize(found) / 1024 / 1024:.1f} MB")
    print(f"PATH 里的 mihomo/clash：{shutil.which('mihomo') or shutil.which('clash') or '(没有)'}")

    data_dir = os.environ.get("TA_DATA_DIR") or os.path.join(
        os.environ.get("TEMP", ""), "ta-e2e", "data"
    )
    db = os.path.join(data_dir, "db.sqlite")
    if os.path.exists(db):
        con = sqlite3.connect(db)
        for kind, cid, raw in con.execute("select kind, entity_id, json from config").fetchall():
            try:
                doc = json.loads(raw)
            except Exception:
                continue
            blob = json.dumps(doc, ensure_ascii=False)
            if "kernel" not in blob and "mihomo" not in blob:
                continue
            print(f"— {kind} {cid}: {blob[:300]}")
    else:
        print(f"(没有隔离配置可读：{db})")

    print()
    print("结论：内置内核在场 => `net.kernel.path` 为空时应用会用它；")
    print("      真实延迟/粘性的验证用本地延迟测试台(两个 CONNECT 代理 + 200 端点)。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

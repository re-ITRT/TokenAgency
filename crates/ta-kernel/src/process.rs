//! 内核进程生命周期。**工作包 P5 负责实现**。
//!
//! 就绪探测（轮询 `/version`，≤5s）/ 指数退避重启 1s→30s /
//! Windows Job Object 防孤儿 / 退出先 TERM 再 2s 后强杀。

// TODO(P5): process 管理

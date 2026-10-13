# ta-kernel — mihomo sidecar 的进程看护、YAML 生成、REST 控制与 lane 端口分配

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。
> 行号写法：`` `路径`:行 ``（冒号留在反引号外）—— 这样反引号里永远是一个真实存在的路径。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/lib.rs` | crate 根：`pub mod childlog` / `config_gen` / `ports` / `process` / `rest`；`KernelController` 等对外类型住在这里 |
| `src/process.rs` | 进程生命周期：`KernelSupervisor`（启停、退避重启、Windows Job Object、状态机） |
| `src/config_gen.rs` | 生成内核 YAML（真源是 `ConfigDoc`，**YAML 是生成物**） |
| `src/ports.rs` | lane → 端口分配 + 落库复用（`PortStore` / `RangePortAllocator`） |
| `src/rest.rs` | 内核 REST 控制面客户端（`/version` `/configs` `/proxies` `/providers`） |
| `src/childlog.rs` | 内核子进程 stdout/stderr → 我们的日志（不接管就是信息黑洞） |
| `src/testutil.rs` | `#[cfg(test)]` 共享夹具（53 行，不在生产路径上） |

## 公开 API（对外符号）
| 符号 | 位置 | 作用 |
| --- | --- | --- |
| `KernelController`（trait） | `src/lib.rs`:306 | 壳层唯一控制口：`status` / `start` / `stop` / `restart` / `apply` / `set_node` / `set_select` / `delay_of` |
| `KernelSupervisor` | `src/process.rs`:828 | `KernelController` 的实现（`impl` 在 `src/process.rs`:1261）；`supervise_once` 是单步看护 |
| `KernelProcessConfig` | `src/process.rs`:41 | 命令行 / 工作目录 / secret / 就绪超时；`from_doc` 从配置来，`validate` 先拦 |
| `ProcessSpawner` | `src/process.rs`:188 | 进程操作注入点；生产 `SystemSpawner`，单测换假实现跑完退避分支 |
| `KernelState` / `KernelStatus` | `src/lib.rs`:24 / `src/lib.rs`:33 | 状态机 `stopped → starting → ready → (restarting → ready)* ，或 failed` |
| `MhConfigGen` | `src/config_gen.rs`:77 | `plan` 解析 → 校验引用 → 渲染 YAML；`with_known_proxies` / `with_strict_refs` 是开关 |
| `KernelConfigSource` | `src/config_gen.rs`:722 | "当前配置长什么样"的来源；生产实现 `GeneratedConfigSource`（`src/config_gen.rs`:738） |
| `PortAllocator` / `PortStore` | `src/ports.rs`:32 / `src/ports.rs`:25 | 端口分配与持久化；生产 `FilePortStore` 落在数据目录的 kernel/ports.json（装配见 `src-tauri/src/boot.rs`:195） |
| `RangePortAllocator` | `src/ports.rs`:204 | 从 `net.kernel.lane_port_range` 分配；`from_doc` 是真实装配点 |
| `KernelRest` | `src/rest.rs`:32 | REST 客户端：Bearer secret、仅回环、`no_proxy` |
| `ProvidersSnapshot` | `src/lib.rs`:55 | `/proxies` ∪ `/providers/proxies` 的合并节点索引（含 `dangling` 悬空引用） |
| `parse_snapshot` | `src/rest.rs`:316 | REST 原文 → `ProvidersSnapshot`（真夹具对拍在 `tests/real_mihomo_snapshot.rs`） |
| `ChildLogSink` | `src/childlog.rs`:239 | 子进程日志落点；`TracingChildSink`（`src/childlog.rs`:248）、`KernelLogFileSink`（`src/childlog.rs`:299） |

## 依赖
- **上游 crate**：`ta-core`（`ConfigDoc` / `LaneId` / `TaError`）。**不依赖 `ta-store`**：端口落库走 `PortStore` trait，由壳层注入实现（`crates/ta-kernel/Cargo.toml`:9）。
- **关键第三方**：`tokio`（子进程 + 看护循环）、`reqwest`（REST 控制面，仅回环）、`serde_yaml_ng`（生成 YAML）、`windows-sys`（Job Object 防孤儿，仅 Windows）、`rand`（随机 secret）。
- **dev**：`wiremock`（假内核 REST）、`insta`（YAML 快照）、`tracing-subscriber`（证明子进程日志真的被转发）。

## OWNER
`P5`（`src/lib.rs`:3："契约：。**实现由工作包 P5 完成。**"）。
其余文件未标注。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | 6 个源文件内联 `#[cfg(test)]`：`src/lib.rs` / `src/process.rs` / `src/config_gen.rs` / `src/ports.rs` / `src/rest.rs` / `src/childlog.rs` |
| YAML 快照 | `src/snapshots/`（`insta`，4 份：empty / only_direct / with_filter / with_groups） |
| 集成测试 | `tests/child_log_tracing.rs`（子进程输出真的进 tracing）、`tests/real_mihomo_snapshot.rs`（真内核原文对拍） |
| 夹具 | `tests/fixtures/real-mihomo/`（真内核日志、`/proxies` 原文、配置样例，含自己的 README） |

## 关键数据流 / 边界（**可选但强烈建议**）
- **装配在壳层，不在本 crate**：`src-tauri/src/boot.rs` 的 `build_kernel` 造 `KernelSupervisor` + `RangePortAllocator` + `GeneratedConfigSource`，只把 `Arc<dyn KernelController>` 与 `PortMap` 交出去（`src-tauri/src/lib.rs`:156）。
- **端口表的消费方是网关**：`ta-gateway` 直接给 `ta_kernel::PortMap` 实现 `LanePorts`（`crates/ta-gateway/src/dispatch.rs`:89），按 lane 端口拨号。
- **本 crate 绝不改系统网络**：不写系统代理、不改路由表、不装 TUN（`src/lib.rs`:5）；生成配置里 `mode: rule`、`mixed-port: 0`、`allow-lan: false`、`bind-address: 127.0.0.1`、`ipv6: false`、`tun: {enable: false}`、`profile.store-fake-ip: false` 全部写死（`src/config_gen.rs`:484 起）。
- 每条 lane 生成一个 loopback 入站：`listeners: [{name: lane-<id>, type: mixed, listen: 127.0.0.1, port: <分配到的端口>, proxy: …, udp: false}]`（`src/config_gen.rs`:583 起）。端口真源是端口分配器，不是这份 YAML。
- REST 只允许回环：`normalize_controller` 拒绝非回环地址（`src/rest.rs`:501）。
- 健康检查由 mihomo 自己按生成配置的间隔跑，应用侧**不自建 cron**（间隔见 `src/config_gen.rs`:911）。

## 已知陷阱（**可选但强烈建议**）
1. **端口复用不看 bind 探针**：判据是"落库且没分给别的 lane ⇒ 就是我们的端口"。曾经用 `PortProbe::is_free` 判断能否复用，而那个端口此刻正被**我们自己的 listener** 占着 → 每写一次配置端口整体前移，网关手里的表却还是 boot 那份。
2. `src/ports.rs` 开头 1–12 行的模块头**还写着旧的"bind 试探后复用"**，与 `allocate` 的实现相反 —— 注释级漂移守卫拦不住，读代码别只读文件头。
3. **引用校验不是可选项**：`listeners[].proxy` 指向不存在的名字时内核照样绑定成功、一声不响；`with_strict_refs(false)` 只是放行，不是修复（`src/config_gen.rs`:434 起）。
4. **子进程日志必须接管**：provider 文件缺失或为空时内核照常启动、REST 照常通，`level=error` 只写在它自己的 stdout 里（`src/childlog.rs` 文件头、夹具 `tests/fixtures/real-mihomo/emptya.log`）。
5. `KernelSupervisor` 的可测性来自 `ProcessSpawner` / `Sleeper` 两个注入点；绕过它们直接起真进程会让单测变成"毫秒级跑不完、还依赖本机 mihomo"。

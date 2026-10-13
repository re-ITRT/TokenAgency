# ta-subscription — 订阅下载、格式嗅探、坏更新拦截、版本与回滚

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/lib.rs` | crate 根：`DetectedFormat` / `FetchOutcome` / `AcceptDecision` + 两个 trait；`pub mod` 清单 = 公开面 |
| `src/fetch.rs` | 下载（**独立 Client，不走 lane**）与解压 |
| `src/detect.rs` | 格式嗅探（clash yaml / base64 链接 / 纯链接 / HTML） |
| `src/parse.rs` | 只为"预览、计数、防坏更新"的解析（节点 / 分组 / 用户信息 / 过滤） |
| `src/validate.rs` | 坏更新拦截（五种拒绝情形 + 阈值） |
| `src/version.rs` | 版本保留、回滚、坏版本留档 |
| `src/sync.rs` | 编排：fetch -> validate -> record / 保留上版 |
| `src/schedule.rs` / `src/traffic.rs` / `src/auto.rs` | 到期判定 / `subscription-userinfo` / 自动出口选择 |
| `src/testutil.rs` | 仅测试用的临时目录（`Cargo.toml` 里没有 `tempfile`） |

## 公开 API（对外符号）
| 符号 | 位置（`文件:行`） | 作用 |
| --- | --- | --- |
| `SubscriptionFetcher` / `SubscriptionValidator` | `src/lib.rs`:160 / `src/lib.rs`:164 | 两个注入点：怎么下载、怎么判好坏 |
| `FetchOutcome` | `src/lib.rs`:113 | 下载结果：原文 + 格式 + userinfo + 节点/分组 |
| `AcceptDecision` | `src/lib.rs`:139 | `Accept` / `Reject(原因)`；`reason()` 给 UI 与日志 |
| `DetectedFormat` | `src/lib.rs`:43 | `is_usable()`：Html / Unknown 一律视为坏更新 |
| `HttpSubscriptionFetcher` | `src/fetch.rs`:134 | 真下载器（超时、重试、体积上限都可配） |
| `FetchOptions` / `ProxyMode` | `src/fetch.rs`:62 / `src/fetch.rs`:51 | 下载参数与"直连 / 引导代理"的选择 |
| `detect_format` / `hint_format` | `src/detect.rs`:22 / `src/detect.rs`:36 | 内容嗅探说了算；`Content-Disposition` 只作兜底提示 |
| `extract` | `src/parse.rs`:37 | 嗅探到的格式 -> 节点/分组计数 |
| `DefaultSubscriptionValidator` / `ValidatorConfig` | `src/validate.rs`:60 / `src/validate.rs`:32 | 拦截实现与三个阈值 |
| `VersionKeeper` | `src/version.rs`:378 | `record` / `history` / `rollback_to` / `rollback_latest_accepted` / `latest_accepted` / `cleanup` |
| `VersionStore` / `FileVersionStore` / `MemoryVersionStore` | `src/version.rs`:103 / `src/version.rs`:213 / `src/version.rs`:150 | 版本存哪：文件（应用用，gzip）/ 内存（测试） |
| `SubscriptionSync` / `SyncOutcome` | `src/sync.rs`:72 / `src/sync.rs`:32 | 一次同步的编排与结果（`accepted()` / `reason()`） |
| `due_subscriptions` / `parse_traffic` / `choose_auto_export` | `src/schedule.rs`:31 / `src/traffic.rs`:70 / `src/auto.rs`:78 | 到期判定 / 流量头解析 / 自动出口选择 |

## 依赖
- **上游 crate**：`ta-core`（`Subscription` 领域类型、注入式 `Clock`、错误码）。
- **关键第三方**（每条都说清为什么）：
  - `reqwest`：订阅下载；**必须是独立 Client，不走 lane**（见"边界"）；
  - `backon`：指数退避重试（默认 1 次首试 + 2 次重试，有上限）；
  - `flate2`：解压 —— 包括**未声明 `Content-Encoding` 的 gzip**（按 magic bytes 嗅探后解）与 zlib；
  - `base64`：base64 链接订阅；`regex`：链接与 YAML 的解析；`serde_yaml_ng`：Clash YAML 预览解析；
  - `tokio`：async 与 `Sleep` 抽象；`async-trait` / `thiserror` / `tracing`：trait、错误、日志。
- **dev**：`wiremock`（`src/sync.rs` 的测试会起一个真的假订阅站）。

## OWNER
`P6（订阅）` —— `src/lib.rs` 文件头"实现由工作包 P6 完成"。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | `src/**.rs` 内联 `#[cfg(test)]`（**约 118 条**）：`src/fetch.rs` 30、`src/parse.rs` 17、`src/auto.rs` 13、`src/sync.rs` 13、`src/version.rs` 11 |
| 集成测试 | **`tests/` 下没有任何 Rust 测试文件**；只有 `tests/fixtures/clash_airport.yaml`、`tests/fixtures/links_base64.txt`、`tests/fixtures/links_plain.txt`、`tests/fixtures/real-airport/airport.yaml` 这些夹具，由上面那些内联测试读取 |
| 守卫 | 无独立守卫文件 |

## 关键数据流 / 边界
- 被谁调用：`src-tauri/src/commands/net.rs` 装配 `SubscriptionSync<HttpSubscriptionFetcher, DefaultSubscriptionValidator, FileVersionStore>`（`src-tauri/src/commands/net.rs`:379），订阅相关的命令都走它。
- 一次同步：`SubscriptionSync::effective`（`src/sync.rs`:98）-> `fetch` -> `validate` 的 `accept` -> **采纳了才 `VersionKeeper::record`**；被拒的版本也留档（`accepted = false`），而回滚只挑 `accepted` 的版本。
- **坏更新拦下并保留上一版**：这是产品级防线（`src/validate.rs` 开头写明），判据全部相对"上一版**已采纳**的内容"，所以机场慢慢增删节点不会被误拦。
- 边界：本 crate 的解析**只为预览、计数、防坏更新**；节点协议与 Clash 配置解析的权威实现是内核 mihomo（`ta-kernel`）。别让这里的解析变成第二真源。
- **下载不走 lane**（鸡生蛋：拿到订阅前不知道有哪些节点）：`HttpSubscriptionFetcher` 结构上拿不到 lane / dialer，只能经 `FetchOptions`。

## 已知陷阱
1. 任何下载失败都要**保留上一版**；"采纳与否"只由 `AcceptDecision` 决定，别在调用方另写一套判据。
2. `subscription-userinfo` 头**不存在 = `None`**，与"头在但字段一个都认不出"（`Some` + 全 `None`）是两件事（见 `src/lib.rs` 里 `FetchOutcome.traffic` 的注释）。
3. 版本历史的真源是 `FileVersionStore` 落在数据目录里的文件，**不是** `ta-store` 的 `subscription_version` 表（那张表当前是死的）。
4. `tests/` 里没有任何 Rust 测试文件：`cargo test -p ta-subscription` 跑的是内联测试；fixture 在 `tests/fixtures/`。
5. 「节点数骤降」是**两条独立保护**（跌幅比例 + 绝对下限），任一命中即拒；改阈值前先看清 `ValidatorConfig` 的三个字段各自拦什么。

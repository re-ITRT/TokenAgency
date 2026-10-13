# ta-secrets — 系统钥匙串封装 + `${secret:...}` 引用解析

> 上一层：[../../AGENTS.md](../../AGENTS.md)（全仓地图）。本文件是该模块的**入口索引**。

## 入口文件
| 文件 | 作用 |
| --- | --- |
| `src/lib.rs` | crate 根：`SecretStore` trait + `mask_secret`；`pub mod` 清单 = 公开面 |
| `src/resolver.rs` | **引用解析入口**：`${secret:name}` / `${env:VAR}` 的语法、明文识别、`SecretString`（脱敏） |
| `src/composite.rs` | 优先级链：钥匙串 -> 便携文件 -> 环境变量，以及降级 |
| `src/keyring_store.rs` | OS 钥匙串（Windows Credential Manager / macOS Keychain / Secret Service） |
| `src/portable.rs` | 便携模式加密文件（v2 = argon2id + ChaCha20-Poly1305；v1 明文只读兼容） |
| `src/env.rs` | 环境变量来源（只读） |
| `src/migrate.rs` | 明文扫描 `scan_plaintext` 与迁移到引用 |
| `src/mock.rs` | 测试替身 `MockStore` + 极简 `block_on`（本 crate **不依赖 tokio**） |
| `SECURITY-NOTES.md` | 本模块的安全笔记（不是代码，但属于入口材料） |

## 公开 API（对外符号）
| 符号 | 位置（`文件:行`） | 作用 |
| --- | --- | --- |
| `SecretStore` | `src/lib.rs`:57 | 密钥仓库 trait（`get` / `set` / `delete` / `list_names`） |
| `mask_secret` | `src/lib.rs`:73 | 唯一许可的日志脱敏函数（只留前 4 字符，短值全遮） |
| `resolve_ref` | `src/resolver.rs`:189 | `${...}` -> `SecretRef`；**只看语法，不碰仓库** |
| `resolve_value` / `resolve_spec` | `src/resolver.rs`:229 / `src/resolver.rs`:261 | 取值（async）：整串 / 单个 spec |
| `is_reference` / `classify_literal` | `src/resolver.rs`:311 / `src/resolver.rs`:387 | 是不是引用；不是引用时它是哪种明文（`PlaintextKind`） |
| `SecretString` | `src/resolver.rs`:83 | 明文包装：`Debug` / `Display` 已脱敏，只能经 `expose` / `into_string` 取出 |
| `SecretRef` | `src/resolver.rs`:41 | 解析结果（`scheme()` / `key()` / `to_spec()`）；`SECRET_SCHEME` / `ENV_SCHEME` 是方案名 |
| `CompositeStore` / `NamedStore` / `SourceKind` | `src/composite.rs`:92 / `src/composite.rs`:61 / `src/composite.rs`:29 | 优先级链与每个来源的元信息（`is_writable`） |
| `KeyringStore` / `EntryFactory` / `SERVICE` | `src/keyring_store.rs`:58 / `src/keyring_store.rs`:43 / `src/keyring_store.rs`:34 | 钥匙串实现；测试可换 `EntryFactory`（不碰真钥匙串） |
| `PortableStore` / `DiskState` | `src/portable.rs`:196 / `src/portable.rs`:161 | 便携加密文件（`FILE_NAME` = `secrets.enc`） |
| `EnvStore` / `MapEnv` / `ProcessEnv` / `EnvSource` | `src/env.rs`:21 / `src/resolver.rs`:151 / `src/resolver.rs`:141 / `src/resolver.rs`:135 | 环境变量来源及其抽象 |
| `scan_plaintext` / `suggested_names` / `migrate` / `migrate_with_report` | `src/migrate.rs`:67 / `src/migrate.rs`:72 / `src/migrate.rs`:82 / `src/migrate.rs`:91 | 找出配置里的明文、建议条目名、替换成引用 |
| `MockStore` / `block_on` | `src/mock.rs`:38 / `src/mock.rs`:164 | 内存替身 + 极简 executor |

## 依赖
- **上游 crate**：`ta-core`（`TaError` / 明文错误的错误码、`ConfigDoc` 用于明文扫描）。
- **关键第三方**（每条都说清为什么）：
  - `keyring`：OS 钥匙串 —— **唯一**允许持久化明文的地方；
  - `chacha20poly1305` + `argon2` + `rand` + `zeroize`：便携模式 v2（AEAD + 口令派生 + 随机 salt/nonce + 用后清零）；
  - `dirs`：便携文件的默认路径 `<AppData>/TokenAgency`；
  - `async-trait`：`SecretStore` 的方法都是 async；
  - `base64` / `serde` / `serde_json`：便携文件编码与序列化；`thiserror` / `tracing`：错误与日志。
- **有意不依赖 `tokio`**：`src/mock.rs` 的 `block_on` 就是为"没有运行时也能跑 async"准备的。

## OWNER
`P7（密钥）` —— `src/lib.rs` 文件头"实现由工作包 P7 完成"，`tests/acceptance.rs` 写的是"P7 验收测试"。

## 测试位置
| 类型 | 位置 |
| --- | --- |
| 单元测试 | `src/**.rs` 内联 `#[cfg(test)]`（**约 83 条**）：`src/portable.rs` 21、`src/composite.rs` 16、`src/resolver.rs` 15、`src/migrate.rs` 12 |
| 集成测试 | `tests/acceptance.rs`（P7 的 DoD：引用解析全分支与错误路径、明文 5 种形态、无钥匙串时回退、composite 优先级、`migrate` 之后配置里搜不到明文） |
| 守卫 | 无独立守卫文件；安全断言就是测试本身（脱敏结果不含明文、错误消息不回显引用原文） |

## 关键数据流 / 边界
- 被谁调用：`src-tauri/src/gateway.rs` 的 `SecretsAdapter`（`impl SecretResolver`，`src-tauri/src/gateway.rs`:232）把本 crate 接到网关的转发与探测路径；配置里只出现引用（`ta-core` 的 `Credential.secret`）。
- **明文永不进配置 / 日志 / SQLite**：用户的配置文件里只允许 `${secret:name}` 与 `${env:VAR}`；取值只能经 `SecretString`，写日志只能用 `mask_secret`。
- 错误消息**不回显引用原文**（用户很可能把明文 key 贴进 `${secret:sk-...}`），只有*条目名*可以出现在消息里。
- 便携模式文件 `<AppData>/TokenAgency/secrets.enc`：v2 是加密的，没有口令就拒绝写任何文件；Unix 下权限 0600，Windows 靠用户级目录隔离。

## 已知陷阱
1. `${secret:...}` 里误贴明文 key 会被 `classify_literal` 认成明文并拒绝，且**报错里不会带上那段文本**。
2. `PortableStore` 没有口令时写入直接失败（不是静默降级成明文）：v1 明文格式只做只读兼容。
3. 无钥匙串环境（CI / 无桌面会话）必须能回退到便携文件 —— `KeyringStore::probe()` 就是用来判这一件事的，回退由 `CompositeStore` 做。
4. 别绕过 `SecretString` 自己 `format!` 明文值：那是唯一一处能拦住"密钥进日志"的地方。
5. 本 crate 没有 `tokio`，不要为了图方便在 `Cargo.toml` 里加它（`block_on` 足够了）。

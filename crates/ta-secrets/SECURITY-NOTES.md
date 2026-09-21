# ta-secrets 安全说明（P7 交付物）

> 2026-09-21 更新：便携模式已从"base64 混淆"升级为**真加密**（v2 = argon2id +
> ChaCha20-Poly1305）。历史原因与当时的判断保留在第 2 节末尾，作为决策记录。

## 1. 硬约束（已实现）

* **密钥绝不进 SQLite**：本 crate 不依赖 `ta-store`，没有任何数据库路径；
  `keyring_store` 的条目名索引只存**条目名**，不存值。
* **密钥绝不进日志 / trace / 错误消息**：
  * `resolver::SecretString` 的 `Debug` 输出 `<已脱敏 N 字符>`，`Display` 输出 `***`；
  * 错误消息只带**条目名 / 变量名 / 位置**，从不回显被解析的引用原文
    （因为用户可能把明文 key 贴进 `${secret:...}`），也从不带密钥值；
  * `tracing` 只记录条目名与来源种类；便携库解锁只打 debug，不打任何内容。
  * 统一的脱敏函数：`ta_secrets::mask_secret`（保留前 4 字符）。
* **无钥匙串环境不 panic**：`keyring` 错误一律翻译成 `TaError::Secret` 并附带可读原因
  （`PlatformFailure` / `NoStorageAccess` 的原文保留），由 `CompositeStore` 降级到
  便携文件 → 环境变量。
* **解密失败不区分原因**（口令错 / 文件被篡改返回同一条消息）：区分等于给攻击者一个
  应答 oracle。

## 2. 便携模式加密（v2，已实现）

| 项 | 状态 |
|---|---|
| 文件位置 | `<AppData>/TokenAgency/secrets.enc` ✅ |
| 格式版本 | v2 = `encrypted: true` + `kdf`/`kdf_params`/`salt`/`nonce`/`ciphertext` ✅ |
| AEAD | ChaCha20-Poly1305（IETF 96-bit nonce），整张条目表一个密文 ✅ |
| KDF | argon2id，`m = 65536 KiB (64 MiB)`、`t = 3`、`p = 1`、32 字节输出 ✅ |
| nonce | 每次写入 `OsRng` 重新随机 12 字节，**绝不复用** ✅ |
| salt | 建库 / 换口令 / v1 升级时随机 16 字节；同一文件内复用 ✅ |
| AAD | `format\|v<版本>\|kdf=<名>\|m/t/p\|aead=<名>\|salt=<b64>\|nonce=<b64>`，把版本号与 KDF 参数绑进认证标签 ✅ |
| 内存清零 | 口令、派生密钥、待加密/已解密明文、条目值（Drop 时逐条 `zeroize`） ✅ |
| Unix 权限 | 文件 0600、目录 0700 ✅ |
| Windows ACL | ❌ **未做**：显式收敛 ACL 需要 `windows-sys` 等额外依赖。现状是 `%APPDATA%` 目录按用户隔离 + 文件内容是密文。标记 `TODO(security)`（`portable.rs::restrict_permissions` 的非 unix 分支） |

参数依据：RFC 9106 §4 的 argon2id 第二推荐档（64 MiB / 3 轮）+ OWASP 口令存储建议。
参数是**格式的一部分**：解密时只接受这一组参数，其它一律拒绝（防 DoS 与降级）。

**写入语义（不静默降级）**：没有口令时 `set`/`delete`/`upgrade_to_v2` 一律返回
`TaError::Secret`，**不创建任何文件**——便携模式永远不会悄悄退回明文存储。

**v1 兼容**：历史 v1 明文文件仍可读（读到就打 `warn!` 并给出 `security_notice()`），
但写入被拒，必须 `set_passphrase()` + `upgrade_to_v2()` 重写成 v2。
v1 文件头里若出现 v2 字段 → 直接报错（堵住"把 v2 头改成 v1 走明文路径"的降级篡改）。

对应测试（`portable::tests`，共 18 条）：`v2_round_trip_survives_reopen`、
`wrong_passphrase_fails_with_readable_error_not_panic`、
`tampering_ciphertext_fails_authentication`、
`tampering_nonce_or_salt_is_authenticated_too`、`unsupported_kdf_params_are_rejected`、
`downgrading_version_to_v1_is_rejected`、
`v1_plaintext_file_is_readable_and_flagged_for_upgrade`、
`writing_the_same_value_twice_produces_different_ciphertext`、
`v2_file_without_passphrase_is_locked_not_decrypted`、
`writes_without_passphrase_never_create_a_file`、`empty_passphrase_is_rejected`、
`changing_passphrase_rewrites_with_a_fresh_salt` 等。

### 2.1 决策记录：为什么曾经"只做 base64"

2026-09-21 首次交付时，`crates/ta-secrets/Cargo.toml` 处于冻结状态且不含任何加密 /
随机数依赖，`Cargo.lock` 里的 `chacha20`/`hkdf`/`sha2` 都只是别人的传递依赖（`use` 不到）。
没有 AEAD 也没有 CSPRNG 时**无法**做正确实现，因此当时选择：只做 base64 混淆 + 把状态
显式暴露（`"encrypted": false` + `warn!` + `is_encrypted()`），并把缺口写成报告。

主管裁决后解除了该文件的冻结，加入 `chacha20poly1305` / `argon2` / `rand` / `zeroize`
并按上面的要求实现 v2（含"错误口令"、"篡改任一字节"、"nonce 每次随机"、"v1 仍可读"
等测试）。**教训**：缺依赖时如实报告 + 显式弱化，比"看起来做完了"安全得多。

## 3. 其它已知边界

* `gateway.local_key` 未纳入明文扫描 / 迁移：改写成引用需要 ingress 侧先支持引用解析，
  否则会打断网关本地鉴权。见 `migrate.rs` 末尾 `TODO(P7-followup)`。
* `EnvStore::list_names()` 返回空：枚举整个进程环境会把无关变量（可能含别的程序的密钥）
  搬进 UI，属于越权；需要列环境变量来源的条目请以配置里的引用为准。
* 便携库的口令由调用方（T1/GUI）提供与保存；`ta-secrets` 不落盘、不缓存口令到文件，
  只在内存里用 `Zeroizing` 持有，进程退出即清零。口令的"记住我"策略属于 T1。
* `keyring_store::list_names` 依赖一个只存条目名的索引条目（`__tokenagency_index__`）；
  索引损坏时降级为"列不出名字"，但不影响取值。

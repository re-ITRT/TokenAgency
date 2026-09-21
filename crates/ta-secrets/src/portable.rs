//! 便携模式回退：**本地加密文件**密钥库（v2 = argon2id + ChaCha20-Poly1305）。
//!
//! 文件位置：`<AppData>/TokenAgency/secrets.enc`（[`default_path`]）。
//!
//! # 文件格式
//!
//! | 版本 | 形态 | 说明 |
//! |---|---|---|
//! | v1 | `encrypted: false` + `entries` 逐条 base64 | **历史产物，明文在盘上**。仍可读，但读到就打 warning 并提示 [`PortableStore::upgrade_to_v2`] |
//! | v2 | `encrypted: true` + `salt`/`nonce`/`ciphertext` | argon2id 派生密钥 + ChaCha20-Poly1305 AEAD，整个条目表作为一个密文 |
//!
//! v1 分支只保留**读**能力：一旦发现 v1 文件头里带 v2 字段（salt/ciphertext/…）就报错，
//! 堵住"把 v2 文件头改成 v1 以降级成明文路径"的篡改手法。
//!
//! # 密码学选择（全部写死，不给用户调）
//!
//! * **KDF**：argon2id，`m = 65536 KiB (64 MiB)`、`t = 3`、`p = 1`，输出 32 字节。
//!   依据 RFC 9106 §4 的第二推荐档（64 MiB / 3 轮）与 OWASP 口令存储建议：桌面单机、
//!   口令质量不可控，宁可慢一点。**改参数 = 改格式**，必须同时升版本号。
//! * **AEAD**：ChaCha20-Poly1305（IETF 96-bit nonce）：不依赖硬件 AES 加速，跨平台一致。
//! * **nonce**：每次写入都用 [`OsRng`] 重新取 12 字节随机数，**绝不复用**；
//!   salt 在文件建立 / 换口令 / v1 升级时重新随机（16 字节），此后同一文件复用——
//!   这是"随机 nonce + 固定密钥"的正确用法。
//!   为什么 96-bit 随机 nonce 够用：同一 salt 下写入 q 次时的碰撞概率约 `q²/2^97`，
//!   即使 q = 2^20（一百万次改写，本文件实际只在改配置时才写）也只有约 `2^-57`；
//!   而每次换口令 / 重建文件都会换 salt。要更保守可换 XChaCha20-Poly1305（192-bit
//!   nonce），但那需要 `chacha20poly1305` 的 `xchacha20` 特性，属于格式变更。
//! * **AAD**：`format|v<版本>|kdf=<名>|m/t/p=<参数>|aead=<名>|salt=<b64>|nonce=<b64>`。
//!   把**文件头版本号与 KDF 参数**绑进认证标签：降级或参数篡改都会认证失败。
//! * **清零**：口令、派生密钥、待加密的明文 JSON、解密结果都用 [`Zeroizing`] 包裹；
//!   [`PortableStore`] 析构时把内存里的条目值显式 `zeroize`。
//!
//! # 权限
//!
//! * Unix：文件 0600、目录 0700（[`restrict_permissions`]）。
//! * Windows：`%APPDATA%` 本身按用户隔离，且文件内容是 AEAD 密文；把 ACL 显式收敛到
//!   当前用户需要额外依赖（windows-sys），**TODO(security)** 未做，见 `SECURITY-NOTES.md`。
//!
//! # 写入语义（不静默降级）
//!
//! **没有口令就绝不写文件**：`set` / `delete` / `upgrade_to_v2` 一律返回 [`TaError::Secret`]
//! （"需要口令"）。便携模式因此永远不会悄悄退回明文存储。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, Params, Version};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use ta_core::error::{Result, TaError};
use zeroize::{Zeroize, Zeroizing};

use crate::SecretStore;

/// 便携密钥库文件名。
pub const FILE_NAME: &str = "secrets.enc";

/// 应用数据目录名。
pub const APP_DIR: &str = "TokenAgency";

/// 文件格式标识（v1/v2 相同，版本靠 `version` 字段区分）。
pub const FORMAT_TAG: &str = "tokenagency.portable-secrets";

/// 当前写出的格式版本。
pub const FORMAT_VERSION: u32 = 2;

/// 历史明文格式版本（只读兼容）。
pub const LEGACY_VERSION: u32 = 1;

/// KDF 名（写进文件头，也写进 AAD）。
pub const KDF_NAME: &str = "argon2id";

/// AEAD 名（写进文件头，也写进 AAD）。
pub const AEAD_NAME: &str = "chacha20poly1305";

/// argon2id 内存开销：65536 KiB = 64 MiB（RFC 9106 §4 第二推荐档）。
pub const ARGON2_M_COST_KIB: u32 = 65_536;
/// argon2id 迭代轮数。
pub const ARGON2_T_COST: u32 = 3;
/// argon2id 并行度。
pub const ARGON2_P_COST: u32 = 1;
/// salt 长度（字节）。
pub const SALT_LEN: usize = 16;
/// AEAD nonce 长度（ChaCha20-Poly1305 IETF）。
pub const NONCE_LEN: usize = 12;
/// 派生密钥长度（ChaCha20-Poly1305 密钥长度）。
pub const KEY_LEN: usize = 32;

/// 读到 v1 明文文件时的提示。**只有确实明文在盘上才会返回**
/// （见 [`PortableStore::security_notice`]）。
pub const V1_PLAINTEXT_NOTICE: &str =
    "检测到 v1 便携密钥库：文件里的密钥是**明文**。请设置口令并调用 upgrade_to_v2() 升级为 \
     argon2id + ChaCha20-Poly1305 加密文件。";

/// 键 = 条目名，值 = 密钥值（只在内存里以明文存在）。
type Entries = BTreeMap<String, String>;

// ================================================================ 文件结构

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct KdfParams {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

impl KdfParams {
    /// 本版本**唯一**接受的参数集。拒绝其它参数，避免攻击者塞极端参数做 DoS。
    const CURRENT: KdfParams =
        KdfParams { m_cost_kib: ARGON2_M_COST_KIB, t_cost: ARGON2_T_COST, p_cost: ARGON2_P_COST };
}

#[derive(Debug, Serialize, Deserialize)]
struct FileDoc {
    format: String,
    version: u32,
    /// v1 = false（明文在盘上）；v2 = true。
    encrypted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    // ---- v1：逐条 base64 的明文字段 ----
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    entries: Entries,
    // ---- v2：加密字段 ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf_params: Option<KdfParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aead: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ciphertext: Option<String>,
}

impl FileDoc {
    fn has_v2_fields(&self) -> bool {
        self.kdf.is_some()
            || self.kdf_params.is_some()
            || self.salt.is_some()
            || self.aead.is_some()
            || self.nonce.is_some()
            || self.ciphertext.is_some()
    }
}

/// 盘上文件的状态。**不需要口令**即可得知，供 UI 判断"要不要让用户输口令"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskState {
    pub exists: bool,
    pub version: Option<u32>,
    /// 文件是否以加密形式存放。
    pub encrypted: bool,
    /// v1：明文确实在盘上 → 需要升级。
    pub plaintext_on_disk: bool,
}

// ================================================================ 内存状态

#[derive(Default)]
struct State {
    loaded: bool,
    entries: Entries,
    on_disk_version: Option<u32>,
    /// 口令（`Zeroizing` 在 drop 时清零）。
    passphrase: Option<Zeroizing<Vec<u8>>>,
    /// 与 [`State::salt`] 绑定的派生密钥。
    key: Option<Zeroizing<[u8; KEY_LEN]>>,
    /// 当前文件使用的 salt；`None` = 下次写入时重新随机（新建 / 换口令 / v1 升级）。
    salt: Option<[u8; SALT_LEN]>,
}

/// 条目值也是密钥材料：析构时显式清零，别指望分配器。
impl Drop for State {
    fn drop(&mut self) {
        for value in self.entries.values_mut() {
            value.zeroize();
        }
        self.entries.clear();
    }
}

/// 便携模式密钥库。
pub struct PortableStore {
    path: PathBuf,
    state: Mutex<State>,
}

impl fmt::Debug for PortableStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortableStore")
            .field("path", &self.path)
            .field("encrypted", &self.is_encrypted())
            .finish_non_exhaustive()
    }
}

/// `<AppData>/TokenAgency/secrets.enc`。
pub fn default_path() -> Result<PathBuf> {
    let base = dirs::data_dir().or_else(dirs::data_local_dir).or_else(dirs::home_dir).ok_or_else(
        || {
            TaError::Secret(
                "无法确定应用数据目录（AppData），便携密钥库不可用；请改用系统钥匙串".to_string(),
            )
        },
    )?;
    Ok(base.join(APP_DIR).join(FILE_NAME))
}

// ================================================================ 构造 / 口令

impl PortableStore {
    /// 打开指定路径的密钥库（**不带口令**）。
    ///
    /// * v2 文件：处于"已锁定"，读 / 写都返回"需要口令"（不 panic、不降级）。
    /// * v1 文件：可读（打 warning），但**写入会拒绝**——必须先
    ///   [`PortableStore::set_passphrase`] 再 [`PortableStore::upgrade_to_v2`]。
    /// * 文件不存在：空库，直到设置口令才会落盘（永远不写明文）。
    ///
    /// 惰性载入：这里不做 IO，损坏的文件在首次访问时报错。
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), state: Mutex::new(State::default()) }
    }

    /// 打开默认位置的密钥库（不带口令）。
    pub fn open_default() -> Result<Self> {
        Ok(Self::open(default_path()?))
    }

    /// 带口令打开（文件不存在则新建；首次写入时随机生成 salt 并写出 v2）。
    ///
    /// 口令错误**不在这里**报错（惰性载入），而是在首次读取时返回可读的
    /// "口令错误或文件被篡改"。
    pub fn with_passphrase(path: impl Into<PathBuf>, passphrase: impl AsRef<[u8]>) -> Self {
        let store = Self::open(path);
        if let Err(e) = store.set_passphrase(passphrase) {
            // 空口令：不静默吞掉。库此时没有口令，后续任何写入都会明确报"需要口令"，
            // 绝不会退回明文存储。
            tracing::warn!(reason = %e, "便携密钥库未设置口令（空口令被拒绝）");
        }
        store
    }

    /// 默认位置 + 口令。
    pub fn open_default_with_passphrase(passphrase: impl AsRef<[u8]>) -> Result<Self> {
        Ok(Self::with_passphrase(default_path()?, passphrase))
    }

    /// 设置 / 更换口令。
    ///
    /// * **解锁**（库里还没载入内容）：口令留待首次访问时用来解密盘上的 v2 文件。
    /// * **换口令**（库已解锁、内存里有内容）：这是 re-key —— 内存内容保留，
    ///   下次写入会换新 salt + 用新口令派生密钥，把整个文件重写一遍。
    ///   （已解密的旧密文无法用新口令读出来，所以必须重写；这是唯一的正确做法。）
    ///
    /// 空口令直接拒绝：口令是唯一防线，不给"空口令加密"这种假安全。
    pub fn set_passphrase(&self, passphrase: impl AsRef<[u8]>) -> Result<()> {
        let bytes = passphrase.as_ref();
        if bytes.is_empty() {
            return Err(TaError::Secret(
                "便携密钥库口令不能为空（口令是唯一防线，不允许空口令加密）".to_string(),
            ));
        }
        let mut state = self.lock();
        state.passphrase = Some(Zeroizing::new(bytes.to_vec()));
        state.key = None;
        // 换口令 → 换 salt：避免旧 salt 上的预计算被复用
        state.salt = None;
        // 只有"还没载入过内容"时才需要重新载入；已解锁的库保留内存内容做 re-key
        if !state.loaded {
            state.on_disk_version = None;
        }
        Ok(())
    }

    pub fn has_passphrase(&self) -> bool {
        self.lock().passphrase.is_some()
    }

    /// 已解锁（能读能写）。
    pub fn is_unlocked(&self) -> bool {
        self.lock().loaded
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 文件是否存在。
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// 密钥是否以加密形式存放。
    ///
    /// * 已解锁（有口令）→ `true`：本实现的写入路径**只有** AEAD。
    /// * 未解锁 → 读文件头判断（v2 = true；v1 / 不存在 = false）。
    pub fn is_encrypted(&self) -> bool {
        if self.lock().key.is_some() {
            return true;
        }
        self.disk_state().is_ok_and(|d| d.encrypted)
    }

    /// **只有确实存在明文风险时才返回文案**（当前仅 v1 明文文件）。
    ///
    /// 已加密、空库、文件不存在都返回 `None` —— 不允许 UI 拿它当"我们加密了"的装饰。
    pub fn security_notice(&self) -> Option<&'static str> {
        match self.disk_state() {
            Ok(state) if state.plaintext_on_disk => Some(V1_PLAINTEXT_NOTICE),
            _ => None,
        }
    }

    /// 读文件头（不解密，不需要口令）。
    pub fn disk_state(&self) -> Result<DiskState> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiskState {
                    exists: false,
                    version: None,
                    encrypted: false,
                    plaintext_on_disk: false,
                })
            }
            Err(e) => {
                return Err(TaError::Secret(format!(
                    "便携密钥库读取失败（{}）：{e}",
                    self.path.display()
                )))
            }
        };
        let doc = parse_doc(&self.path, &raw)?;
        Ok(DiskState {
            exists: true,
            version: Some(doc.version),
            encrypted: doc.version >= FORMAT_VERSION && doc.encrypted,
            plaintext_on_disk: doc.version == LEGACY_VERSION && !doc.encrypted,
        })
    }

    /// 立即载入（探测 / 测试用；正常路径惰性载入）。
    pub fn load_now(&self) -> Result<()> {
        let mut state = self.lock();
        self.ensure_loaded(&mut state)
    }

    /// 把 v1 明文文件升级成 v2 加密文件（幂等）。需要已设置口令。
    pub fn upgrade_to_v2(&self) -> Result<()> {
        let mut state = self.lock();
        self.ensure_loaded(&mut state)?;
        if state.passphrase.is_none() {
            return Err(passphrase_required("升级为 v2 加密"));
        }
        // 升级必须换 salt：明文时代没有任何可复用的密钥材料
        if state.on_disk_version == Some(LEGACY_VERSION) {
            state.salt = None;
        }
        self.persist(&mut state)?;
        tracing::info!(
            path = %self.path.display(),
            "便携密钥库已升级为 v2（argon2id + ChaCha20-Poly1305）"
        );
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------ 载入 / 落盘

    fn ensure_loaded(&self, state: &mut State) -> Result<()> {
        if state.loaded {
            return Ok(());
        }
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                state.loaded = true;
                state.on_disk_version = None;
                return Ok(());
            }
            Err(e) => {
                return Err(TaError::Secret(format!(
                    "便携密钥库读取失败（{}）：{e}",
                    self.path.display()
                )))
            }
        };

        let doc = parse_doc(&self.path, &raw)?;
        match doc.version {
            LEGACY_VERSION => {
                state.entries = decode_legacy_entries(&self.path, &doc)?;
                state.on_disk_version = Some(LEGACY_VERSION);
                tracing::warn!(
                    path = %self.path.display(),
                    "便携密钥库是 v1 明文格式：文件里的密钥没有加密，请 set_passphrase() 后 upgrade_to_v2()"
                );
            }
            FORMAT_VERSION => {
                let passphrase = state.passphrase.as_ref().ok_or_else(|| {
                    TaError::Secret(format!(
                        "便携密钥库已加密，需要口令才能打开（{}）",
                        self.path.display()
                    ))
                })?;
                let (entries, salt) = decrypt_v2(&self.path, &doc, passphrase)?;
                state.entries = entries;
                state.salt = Some(salt);
                state.key = Some(derive_key(passphrase, &salt)?);
                state.on_disk_version = Some(FORMAT_VERSION);
                tracing::debug!(path = %self.path.display(), "便携密钥库已解锁（v2）");
            }
            other => {
                return Err(TaError::Secret(format!(
                    "便携密钥库版本不受支持（{}）：文件 v{other}，本程序支持 v{LEGACY_VERSION}(只读) / v{FORMAT_VERSION}",
                    self.path.display()
                )))
            }
        }
        state.loaded = true;
        Ok(())
    }

    fn persist(&self, state: &mut State) -> Result<()> {
        let passphrase = state.passphrase.as_ref().ok_or_else(|| passphrase_required("写入"))?;

        // salt 缺失（新建 / 换口令 / v1 升级）→ 随机生成并派生密钥
        if state.key.is_none() || state.salt.is_none() {
            let salt = random_bytes::<SALT_LEN>();
            state.key = Some(derive_key(passphrase, &salt)?);
            state.salt = Some(salt);
        }
        let Some(key) = state.key.as_ref() else {
            return Err(TaError::Secret("便携密钥库密钥未就绪".to_string()));
        };
        let Some(salt) = state.salt else {
            return Err(TaError::Secret("便携密钥库 salt 未就绪".to_string()));
        };

        let salt_b64 = B64.encode(salt);
        let (nonce_b64, ciphertext_b64) = encrypt_v2(key, &salt_b64, &state.entries)?;

        let doc = FileDoc {
            format: FORMAT_TAG.to_string(),
            version: FORMAT_VERSION,
            encrypted: true,
            warning: None,
            entries: BTreeMap::new(),
            kdf: Some(KDF_NAME.to_string()),
            kdf_params: Some(KdfParams::CURRENT),
            salt: Some(salt_b64),
            aead: Some(AEAD_NAME.to_string()),
            nonce: Some(nonce_b64),
            ciphertext: Some(ciphertext_b64),
        };
        let json = Zeroizing::new(
            serde_json::to_string_pretty(&doc)
                .map_err(|e| TaError::Secret(format!("便携密钥库序列化失败：{e}")))?,
        );

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                TaError::Secret(format!("便携密钥库目录创建失败（{}）：{e}", parent.display()))
            })?;
        }

        // 原子写：先写同目录临时文件（同样收紧权限），再 rename 覆盖。
        let tmp = self.path.with_file_name(format!("{FILE_NAME}.{}.tmp", std::process::id()));
        fs::write(&tmp, json.as_bytes()).map_err(|e| {
            TaError::Secret(format!("便携密钥库写入失败（{}）：{e}", tmp.display()))
        })?;
        restrict_permissions(&tmp).map_err(|e| {
            TaError::Secret(format!("便携密钥库权限收紧失败（{}）：{e}", tmp.display()))
        })?;
        fs::rename(&tmp, &self.path).map_err(|e| {
            TaError::Secret(format!(
                "便携密钥库落盘失败（{} -> {}）：{e}",
                tmp.display(),
                self.path.display()
            ))
        })?;
        let _ = restrict_dir_permissions(&self.path);

        state.on_disk_version = Some(FORMAT_VERSION);
        Ok(())
    }
}

fn passphrase_required(op: &str) -> TaError {
    TaError::Secret(format!(
        "便携密钥库需要口令才能{op}：请先 set_passphrase()（本实现拒绝在无口令时写出任何文件，以免悄悄退回明文存储）"
    ))
}

// ================================================================ 密码学

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    buf
}

fn derive_key(passphrase: &[u8], salt: &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, Some(KEY_LEN))
        .map_err(|e| TaError::Secret(format!("argon2 参数非法（内部错误）：{e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(passphrase, salt, &mut key[..])
        .map_err(|e| TaError::Secret(format!("便携密钥库口令派生失败：{e}")))?;
    Ok(key)
}

/// AAD：把文件头里所有影响解密语义的字段绑进认证标签。
fn aad_bytes(version: u32, params: KdfParams, salt_b64: &str, nonce_b64: &str) -> Vec<u8> {
    format!(
        "{FORMAT_TAG}|v{version}|kdf={KDF_NAME}|m={}|t={}|p={}|aead={AEAD_NAME}|salt={salt_b64}|nonce={nonce_b64}",
        params.m_cost_kib, params.t_cost, params.p_cost
    )
    .into_bytes()
}

fn encrypt_v2(key: &[u8; KEY_LEN], salt_b64: &str, entries: &Entries) -> Result<(String, String)> {
    // 每次写入都重新随机 nonce —— AEAD 里 nonce 复用是灾难性的
    let nonce_bytes = random_bytes::<NONCE_LEN>();
    let nonce_b64 = B64.encode(nonce_bytes);

    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| TaError::Secret(format!("AEAD 密钥长度非法（内部错误）：{e}")))?;
    // 明文 JSON 用完即清零
    let plaintext = Zeroizing::new(
        serde_json::to_string(entries)
            .map_err(|e| TaError::Secret(format!("便携密钥库序列化失败：{e}")))?,
    );
    let aad = aad_bytes(FORMAT_VERSION, KdfParams::CURRENT, salt_b64, &nonce_b64);

    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce_bytes), Payload { msg: plaintext.as_bytes(), aad: &aad })
        .map_err(|_| TaError::Secret("便携密钥库加密失败".to_string()))?;
    Ok((nonce_b64, B64.encode(ciphertext)))
}

fn decrypt_v2(path: &Path, doc: &FileDoc, passphrase: &[u8]) -> Result<(Entries, [u8; SALT_LEN])> {
    if doc.kdf.as_deref() != Some(KDF_NAME) {
        return Err(TaError::Secret(format!(
            "便携密钥库 KDF 不受支持（{}）：只支持 {KDF_NAME}",
            path.display()
        )));
    }
    if doc.aead.as_deref() != Some(AEAD_NAME) {
        return Err(TaError::Secret(format!(
            "便携密钥库 AEAD 不受支持（{}）：只支持 {AEAD_NAME}",
            path.display()
        )));
    }
    let params = doc.kdf_params.ok_or_else(|| {
        TaError::Secret(format!("便携密钥库缺少 kdf_params（{}）", path.display()))
    })?;
    if params != KdfParams::CURRENT {
        return Err(TaError::Secret(format!(
            "便携密钥库 KDF 参数不受支持（{}）：本版本只接受 m={ARGON2_M_COST_KIB} KiB / t={ARGON2_T_COST} / p={ARGON2_P_COST}",
            path.display()
        )));
    }

    let salt_b64 = doc
        .salt
        .as_deref()
        .ok_or_else(|| TaError::Secret(format!("便携密钥库缺少 salt（{}）", path.display())))?;
    let nonce_b64 = doc
        .nonce
        .as_deref()
        .ok_or_else(|| TaError::Secret(format!("便携密钥库缺少 nonce（{}）", path.display())))?;
    let ciphertext_b64 = doc.ciphertext.as_deref().ok_or_else(|| {
        TaError::Secret(format!("便携密钥库缺少 ciphertext（{}）", path.display()))
    })?;

    let salt_vec = B64
        .decode(salt_b64.as_bytes())
        .map_err(|e| TaError::Secret(format!("便携密钥库 salt 解码失败：{e}")))?;
    let salt: [u8; SALT_LEN] = salt_vec.as_slice().try_into().map_err(|_| {
        TaError::Secret(format!(
            "便携密钥库 salt 长度非法（{}）：期望 {SALT_LEN} 字节",
            path.display()
        ))
    })?;
    let nonce_vec = B64
        .decode(nonce_b64.as_bytes())
        .map_err(|e| TaError::Secret(format!("便携密钥库 nonce 解码失败：{e}")))?;
    let nonce_bytes: [u8; NONCE_LEN] = nonce_vec.as_slice().try_into().map_err(|_| {
        TaError::Secret(format!(
            "便携密钥库 nonce 长度非法（{}）：期望 {NONCE_LEN} 字节",
            path.display()
        ))
    })?;
    let ciphertext = B64
        .decode(ciphertext_b64.as_bytes())
        .map_err(|e| TaError::Secret(format!("便携密钥库密文解码失败：{e}")))?;

    let key = derive_key(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key[..])
        .map_err(|e| TaError::Secret(format!("AEAD 密钥长度非法（内部错误）：{e}")))?;
    let aad = aad_bytes(doc.version, params, salt_b64, nonce_b64);

    let plaintext = cipher
        .decrypt(&Nonce::from(nonce_bytes), Payload { msg: &ciphertext, aad: &aad })
        .map_err(|_| {
            // AEAD 的错误故意不区分原因：区分等于给攻击者一个应答 oracle
            TaError::Secret(
                "便携密钥库解密失败：口令错误，或文件被篡改（认证标签不匹配）".to_string(),
            )
        })?;
    let plaintext = Zeroizing::new(plaintext);

    let entries: Entries = serde_json::from_slice(&plaintext[..]).map_err(|e| {
        TaError::Secret(format!("便携密钥库内容不是合法条目表（{}）：{e}", path.display()))
    })?;
    Ok((entries, salt))
}

/// 解析文件头（不做任何解密）。
fn parse_doc(path: &Path, raw: &str) -> Result<FileDoc> {
    let doc: FileDoc = serde_json::from_str(raw).map_err(|e| {
        TaError::Secret(format!(
            "便携密钥库文件损坏或不是 TokenAgency 格式（{}）：{e}",
            path.display()
        ))
    })?;
    if doc.format != FORMAT_TAG {
        return Err(TaError::Secret(format!(
            "便携密钥库格式标识不匹配（{}）：期望 {FORMAT_TAG}",
            path.display()
        )));
    }
    // 防降级：v1 头里出现 v2 字段 = 有人把版本号改小了
    if doc.version == LEGACY_VERSION && doc.has_v2_fields() {
        return Err(TaError::Secret(format!(
            "便携密钥库文件头自相矛盾（{}）：声明 v1 却带 v2 字段，拒绝按明文解读",
            path.display()
        )));
    }
    if doc.version == LEGACY_VERSION && doc.encrypted {
        return Err(TaError::Secret(format!(
            "便携密钥库文件头自相矛盾（{}）：v1 不可能是加密的",
            path.display()
        )));
    }
    if doc.version == FORMAT_VERSION && !doc.encrypted {
        return Err(TaError::Secret(format!(
            "便携密钥库文件头自相矛盾（{}）：v2 必须是加密的",
            path.display()
        )));
    }
    Ok(doc)
}

/// v1：逐条 base64 的明文条目表。
fn decode_legacy_entries(path: &Path, doc: &FileDoc) -> Result<Entries> {
    let mut entries = Entries::new();
    for (name, encoded) in &doc.entries {
        let bytes = B64.decode(encoded.as_bytes()).map_err(|e| {
            TaError::Secret(format!(
                "便携密钥库 v1 条目 base64 解码失败（{}）：{e}",
                path.display()
            ))
        })?;
        let value = String::from_utf8(bytes).map_err(|_| {
            TaError::Secret(format!("便携密钥库 v1 条目内容不是合法 UTF-8（{}）", path.display()))
        })?;
        entries.insert(name.clone(), value);
    }
    Ok(entries)
}

// ================================================================ 权限

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    // TODO(security): Windows 下用显式 ACL 把文件收敛到当前用户需要额外依赖（windows-sys）。
    // 现状：%APPDATA% 目录本身按用户隔离，且文件内容是 AEAD 密文。
    Ok(())
}

#[cfg(unix)]
fn restrict_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match path.parent() {
        Some(dir) => fs::set_permissions(dir, fs::Permissions::from_mode(0o700)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

// ================================================================ SecretStore

#[async_trait]
impl SecretStore for PortableStore {
    async fn get(&self, name: &str) -> Result<Option<String>> {
        let mut state = self.lock();
        self.ensure_loaded(&mut state)?;
        Ok(state.entries.get(name).cloned())
    }

    async fn set(&self, name: &str, value: &str) -> Result<()> {
        crate::resolver::validate_entry_name(name)
            .map_err(|why| TaError::Secret(format!("密钥条目名不合法：{why}")))?;
        let mut state = self.lock();
        self.ensure_loaded(&mut state)?;
        state.entries.insert(name.to_string(), value.to_string());
        self.persist(&mut state)
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let mut state = self.lock();
        self.ensure_loaded(&mut state)?;
        // 幂等：不存在也算成功
        if state.entries.remove(name).is_some() {
            self.persist(&mut state)?;
        }
        Ok(())
    }

    async fn list_names(&self) -> Result<Vec<String>> {
        let mut state = self.lock();
        self.ensure_loaded(&mut state)?;
        Ok(state.entries.keys().cloned().collect())
    }
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::block_on;

    const PASS: &str = "correct horse battery staple";
    const WRONG_PASS: &str = "Tr0ub4dor&3";

    fn temp_path() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        (path, dir)
    }

    fn read_file(path: &Path) -> String {
        fs::read_to_string(path).expect("读便携文件")
    }

    // ---------------------------------------------------------- 基础

    #[test]
    fn default_path_is_appdata_tokenagency_secrets_enc() {
        let p = default_path().expect("应能确定 AppData");
        assert!(p.ends_with(
            format!("{APP_DIR}/{FILE_NAME}").replace('/', std::path::MAIN_SEPARATOR_STR)
        ));
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some(FILE_NAME));
    }

    #[test]
    fn crypto_parameters_are_pinned_and_documented() {
        // 参数是格式的一部分：改它们必须同时升版本，所以这里把它们钉死
        assert_eq!(FORMAT_VERSION, 2);
        assert_eq!(LEGACY_VERSION, 1);
        assert_eq!(KDF_NAME, "argon2id");
        assert_eq!(AEAD_NAME, "chacha20poly1305");
        assert_eq!((ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST), (65_536, 3, 1));
        assert_eq!((SALT_LEN, NONCE_LEN, KEY_LEN), (16, 12, 32));
    }

    #[test]
    fn missing_file_is_an_empty_store() {
        let (path, _dir) = temp_path();
        let store = PortableStore::open(&path);
        assert!(!store.exists());
        assert_eq!(block_on(store.get("nope")).expect("ok"), None);
        assert!(block_on(store.list_names()).expect("ok").is_empty());
        block_on(store.delete("nope")).expect("删除不存在的条目应成功（幂等）");
        assert!(!store.is_encrypted(), "空库还没有加密文件");
        assert_eq!(store.security_notice(), None, "空库不是明文风险");
    }

    // ---------------------------------------------------------- ① v2 往返

    #[test]
    fn v2_round_trip_survives_reopen() {
        let (path, _dir) = temp_path();

        {
            let store = PortableStore::with_passphrase(&path, PASS);
            block_on(store.set("alpha", "sk-alpha-0123456789")).expect("写入");
            block_on(store.set("beta", "sk-beta-0123456789")).expect("写入");
            assert_eq!(
                block_on(store.get("alpha")).expect("读"),
                Some("sk-alpha-0123456789".to_string())
            );
            assert_eq!(
                block_on(store.list_names()).expect("列"),
                vec!["alpha".to_string(), "beta".to_string()]
            );
            block_on(store.delete("beta")).expect("删");
            assert_eq!(block_on(store.list_names()).expect("列"), vec!["alpha".to_string()]);
        }

        // 重新打开（同口令）：数据还在
        let reopened = PortableStore::with_passphrase(&path, PASS);
        assert!(reopened.is_encrypted());
        assert_eq!(reopened.security_notice(), None);
        assert_eq!(
            block_on(reopened.get("alpha")).expect("读"),
            Some("sk-alpha-0123456789".to_string())
        );
        assert_eq!(block_on(reopened.get("beta")).expect("读"), None);

        // 盘上是密文：既没有明文值，也没有明文条目名
        let raw = read_file(&path);
        assert!(raw.contains("\"version\": 2"));
        assert!(raw.contains("\"encrypted\": true"));
        assert!(raw.contains("\"kdf\": \"argon2id\""));
        assert!(raw.contains("\"aead\": \"chacha20poly1305\""));
        assert!(!raw.contains("sk-alpha-0123456789"), "密文文件里不得出现明文值");
        assert!(!raw.contains("alpha"), "条目名也进了密文，不应出现在文件头");
    }

    // ---------------------------------------------------------- ② 错误口令

    #[test]
    fn wrong_passphrase_fails_with_readable_error_not_panic() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");

        let store = PortableStore::with_passphrase(&path, WRONG_PASS);
        let err = block_on(store.get("k")).expect_err("错误口令必须失败");
        let msg = err.to_string();
        assert!(msg.contains("口令错误"), "原因应可读：{msg}");
        assert!(msg.contains("篡改"), "应提示文件可能被篡改：{msg}");
        assert!(matches!(err, TaError::Secret(_)));
        assert_eq!(err.code(), "TA-9001");
        // 失败后仍可正常重试（不是毒化状态）
        assert!(block_on(store.list_names()).is_err());
        assert!(block_on(store.set("k2", "x-0123456789")).is_err());
        // 换回正确口令即可读
        store.set_passphrase(PASS).expect("重设口令");
        assert_eq!(block_on(store.get("k")).expect("读"), Some("sk-value-0123456789".to_string()));
    }

    // ---------------------------------------------------------- ③ 篡改密文

    #[test]
    fn tampering_ciphertext_fails_authentication() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");

        let raw = read_file(&path);
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let ct_b64 = doc["ciphertext"].as_str().expect("ciphertext").to_string();
        let mut ct = B64.decode(ct_b64.as_bytes()).expect("decode");
        ct[0] ^= 0x01; // 翻转密文第一个字节
        doc["ciphertext"] = serde_json::Value::String(B64.encode(&ct));
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");

        let store = PortableStore::with_passphrase(&path, PASS);
        let err = block_on(store.get("k")).expect_err("篡改必须被发现");
        assert!(err.to_string().contains("认证标签不匹配"), "实际：{err}");

        // 翻最后一字节（落到 Poly1305 tag 上）同样失败
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let mut ct = B64.decode(doc["ciphertext"].as_str().unwrap().as_bytes()).expect("decode");
        let last = ct.len() - 1;
        ct[last] ^= 0x80;
        doc["ciphertext"] = serde_json::Value::String(B64.encode(&ct));
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");
        assert!(block_on(PortableStore::with_passphrase(&path, PASS).get("k")).is_err());
    }

    #[test]
    fn tampering_nonce_or_salt_is_authenticated_too() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");
        let raw = read_file(&path);

        // AAD 里含 nonce：改 nonce 必然认证失败
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let mut nonce = B64.decode(doc["nonce"].as_str().unwrap().as_bytes()).expect("decode");
        nonce[0] ^= 0x0f;
        doc["nonce"] = serde_json::Value::String(B64.encode(&nonce));
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");
        let err = block_on(PortableStore::with_passphrase(&path, PASS).get("k"))
            .expect_err("改 nonce 必须失败");
        assert!(err.to_string().contains("解密失败"), "实际：{err}");

        // 改 salt → 派生出不同密钥 → 认证失败
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let mut salt = B64.decode(doc["salt"].as_str().unwrap().as_bytes()).expect("decode");
        salt[0] ^= 0xff;
        doc["salt"] = serde_json::Value::String(B64.encode(&salt));
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");
        assert!(block_on(PortableStore::with_passphrase(&path, PASS).get("k")).is_err());
    }

    #[test]
    fn unsupported_kdf_params_are_rejected() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");
        let mut doc: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");
        doc["kdf_params"]["m_cost_kib"] = serde_json::json!(1024); // 攻击者想让派生变便宜
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");
        let err = block_on(PortableStore::with_passphrase(&path, PASS).get("k"))
            .expect_err("必须拒绝非本版本参数");
        assert!(err.to_string().contains("KDF 参数不受支持"), "实际：{err}");
    }

    #[test]
    fn downgrading_version_to_v1_is_rejected() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");
        let mut doc: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");
        doc["version"] = serde_json::json!(1); // 把 v2 头改成 v1，想走明文路径
        fs::write(&path, serde_json::to_string_pretty(&doc).expect("encode")).expect("写回");
        let err =
            block_on(PortableStore::with_passphrase(&path, PASS).get("k")).expect_err("降级应被拒");
        assert!(err.to_string().contains("自相矛盾"), "实际：{err}");
    }

    // ---------------------------------------------------------- ④ v1 兼容

    #[test]
    fn v1_plaintext_file_is_readable_and_flagged_for_upgrade() {
        let (path, _dir) = temp_path();
        // 手工造一个历史 v1 文件（旧实现写出来的形态）
        let v1 = format!(
            r#"{{
  "format": "{FORMAT_TAG}",
  "version": 1,
  "encrypted": false,
  "warning": "此文件未加密",
  "entries": {{ "legacy-key": "{}" }}
}}"#,
            B64.encode("sk-legacy-0123456789")
        );
        fs::write(&path, v1).expect("写 v1 文件");

        // 不带口令也能读（兼容），但必须被标注为明文风险
        let store = PortableStore::open(&path);
        assert_eq!(
            block_on(store.get("legacy-key")).expect("v1 应可读"),
            Some("sk-legacy-0123456789".to_string())
        );
        assert!(!store.is_encrypted());
        let notice = store.security_notice().expect("v1 必须给出明文警告");
        assert!(notice.contains("明文"), "提示应点明明文风险：{notice}");
        assert_eq!(store.disk_state().expect("header").version, Some(1));

        // 无口令写入 → 拒绝（绝不悄悄继续写明文）
        let err =
            block_on(store.set("new-key", "sk-new-0123456789")).expect_err("无口令应拒绝写入");
        assert!(err.to_string().contains("需要口令"), "实际：{err}");
        assert!(!read_file(&path).contains("new-key"), "被拒绝的写入不能落盘");

        // 升级：设置口令 → upgrade_to_v2 → 文件变密文，数据保留
        let store = PortableStore::with_passphrase(&path, PASS);
        store.upgrade_to_v2().expect("升级");
        assert!(store.is_encrypted());
        assert_eq!(store.security_notice(), None, "升级后不该再有明文警告");
        assert_eq!(store.disk_state().expect("header").version, Some(2));
        assert_eq!(
            block_on(store.get("legacy-key")).expect("升级后仍可读"),
            Some("sk-legacy-0123456789".to_string())
        );

        let raw = read_file(&path);
        assert!(!raw.contains("sk-legacy-0123456789"), "升级后明文必须消失");
        assert!(!raw.contains("legacy-key"), "条目名也必须在密文里");
        // 重新打开仍可读
        assert_eq!(
            block_on(PortableStore::with_passphrase(&path, PASS).get("legacy-key")).expect("读"),
            Some("sk-legacy-0123456789".to_string())
        );
    }

    // ---------------------------------------------------------- ⑤ nonce 随机

    #[test]
    fn writing_the_same_value_twice_produces_different_ciphertext() {
        let (path, _dir) = temp_path();
        let store = PortableStore::with_passphrase(&path, PASS);
        block_on(store.set("k", "sk-same-0123456789")).expect("第一次");
        let first: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");

        block_on(store.set("k", "sk-same-0123456789")).expect("第二次");
        let second: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");

        assert_ne!(first["nonce"], second["nonce"], "每次写入必须换 nonce");
        assert_ne!(first["ciphertext"], second["ciphertext"], "nonce 变了密文就该变");
        assert_eq!(first["salt"], second["salt"], "同一文件复用 salt（正确用法）");
        // 换个存储实例、同样的值，密文依然不同
        let (path2, _dir2) = temp_path();
        block_on(PortableStore::with_passphrase(&path2, PASS).set("k", "sk-same-0123456789"))
            .expect("第三个文件");
        let third: serde_json::Value = serde_json::from_str(&read_file(&path2)).expect("json");
        assert_ne!(first["ciphertext"], third["ciphertext"]);
    }

    // ---------------------------------------------------------- 锁定 / 口令强制

    #[test]
    fn v2_file_without_passphrase_is_locked_not_decrypted() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "sk-value-0123456789"))
            .expect("写入");

        let locked = PortableStore::open(&path);
        assert!(locked.is_encrypted(), "文件头就是 v2，未解锁也该如实报告");
        assert!(!locked.has_passphrase());
        assert_eq!(locked.security_notice(), None, "已加密的文件不是明文风险");
        for err in [
            block_on(locked.get("k")).expect_err("锁定应报错"),
            block_on(locked.list_names()).expect_err("锁定应报错"),
            block_on(locked.set("x", "y")).expect_err("锁定应报错"),
            block_on(locked.delete("k")).expect_err("锁定应报错"),
        ] {
            assert!(err.to_string().contains("需要口令"), "实际：{err}");
        }
    }

    #[test]
    fn writes_without_passphrase_never_create_a_file() {
        let (path, _dir) = temp_path();
        let store = PortableStore::open(&path);
        let err = block_on(store.set("k", "sk-value-0123456789")).expect_err("必须拒绝");
        assert!(err.to_string().contains("需要口令"), "实际：{err}");
        assert!(!path.exists(), "无口令时不得创建任何文件（否则就是明文库）");

        let err = block_on(store.delete("k")).expect_err("必须拒绝");
        assert!(err.to_string().contains("需要口令"), "实际：{err}");
        assert!(store.upgrade_to_v2().is_err());
    }

    #[test]
    fn empty_passphrase_is_rejected() {
        let (path, _dir) = temp_path();
        let store = PortableStore::open(&path);
        let err = store.set_passphrase("").expect_err("空口令必须被拒");
        assert!(err.to_string().contains("口令不能为空"), "实际：{err}");
        assert!(!store.has_passphrase());
    }

    #[test]
    fn changing_passphrase_rewrites_with_a_fresh_salt() {
        let (path, _dir) = temp_path();
        let store = PortableStore::with_passphrase(&path, PASS);
        block_on(store.set("k", "sk-value-0123456789")).expect("写入");
        let old: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");

        store.set_passphrase("a-brand-new-passphrase").expect("换口令");
        block_on(store.set("k2", "sk-value-2-0123456789")).expect("再写一次");
        let new: serde_json::Value = serde_json::from_str(&read_file(&path)).expect("json");

        assert_ne!(old["salt"], new["salt"], "换口令必须换 salt");
        assert_ne!(old["ciphertext"], new["ciphertext"]);
        // 旧口令不再可用，新口令可用
        assert!(block_on(PortableStore::with_passphrase(&path, PASS).get("k")).is_err());
        assert_eq!(
            block_on(PortableStore::with_passphrase(&path, "a-brand-new-passphrase").get("k"))
                .expect("新口令"),
            Some("sk-value-0123456789".to_string())
        );
    }

    // ---------------------------------------------------------- 其它

    #[test]
    fn rejects_invalid_entry_names() {
        let (path, _dir) = temp_path();
        let store = PortableStore::with_passphrase(&path, PASS);
        let err = block_on(store.set("bad name", "v")).expect_err("应报错");
        assert!(err.to_string().contains("条目名不合法"));
        assert!(!path.exists(), "非法条目名不该产生文件");
    }

    #[test]
    fn overwrite_keeps_single_entry() {
        let (path, _dir) = temp_path();
        let store = PortableStore::with_passphrase(&path, PASS);
        block_on(store.set("k", "first-0123456789")).expect("写");
        block_on(store.set("k", "second-0123456789")).expect("覆盖");
        assert_eq!(block_on(store.get("k")).expect("读"), Some("second-0123456789".to_string()));
        assert_eq!(block_on(store.list_names()).expect("列").len(), 1);
    }

    #[test]
    fn entry_names_are_visible_after_unlock_only() {
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("secret-entry", "v-0123456789"))
            .expect("写");
        // 文件头里不应出现条目名（都在密文里）
        assert!(!read_file(&path).contains("secret-entry"));
        assert_eq!(
            block_on(PortableStore::with_passphrase(&path, PASS).list_names()).expect("列"),
            vec!["secret-entry".to_string()]
        );
    }

    #[test]
    fn reports_corrupt_future_and_foreign_files_readably() {
        let (path, _dir) = temp_path();

        fs::write(&path, "{not json").expect("写");
        let err = block_on(PortableStore::open(&path).get("a")).expect_err("应报错");
        assert!(err.to_string().contains("损坏"), "实际：{err}");

        fs::write(&path, r#"{"format":"something-else","version":1,"encrypted":false}"#)
            .expect("写");
        let err = block_on(PortableStore::open(&path).get("a")).expect_err("应报错");
        assert!(err.to_string().contains("格式标识不匹配"), "实际：{err}");

        fs::write(
            &path,
            r#"{"format":"tokenagency.portable-secrets","version":99,"encrypted":true}"#,
        )
        .expect("写");
        let err = block_on(PortableStore::open(&path).get("a")).expect_err("应报错");
        assert!(err.to_string().contains("版本不受支持"), "实际：{err}");

        // v2 头但 encrypted=false（自相矛盾）
        fs::write(
            &path,
            r#"{"format":"tokenagency.portable-secrets","version":2,"encrypted":false}"#,
        )
        .expect("写");
        let err = block_on(PortableStore::open(&path).get("a")).expect_err("应报错");
        assert!(err.to_string().contains("自相矛盾"), "实际：{err}");

        // v2 头缺字段
        fs::write(
            &path,
            r#"{"format":"tokenagency.portable-secrets","version":2,"encrypted":true}"#,
        )
        .expect("写");
        let err =
            block_on(PortableStore::with_passphrase(&path, PASS).get("a")).expect_err("应报错");
        assert!(err.to_string().contains("KDF 不受支持"), "实际：{err}");
    }

    #[test]
    fn debug_impl_never_leaks_values() {
        let (path, _dir) = temp_path();
        let store = PortableStore::with_passphrase(&path, PASS);
        block_on(store.set("k", "sk-top-secret-0123")).expect("写");
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("top-secret"), "Debug 泄漏了值：{rendered}");
        assert!(!rendered.contains(PASS), "Debug 泄漏了口令：{rendered}");
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_are_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let (path, _dir) = temp_path();
        block_on(PortableStore::with_passphrase(&path, PASS).set("k", "v-0123456789")).expect("写");
        let meta = fs::metadata(&path).expect("stat");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

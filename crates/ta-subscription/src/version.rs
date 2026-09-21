//! 订阅版本保留与回滚（[11 §3]「版本」行）。
//!
//! 为什么要留版本：机场偶尔会推一份**坏订阅**（HTML 错误页 / 空列表 / 节点数骤降）。
//! 没有版本留存，用户只能眼睁睁看着自己的线路被洗掉；有了它，一次点击就能回到上一版。
//!
//! 三件事：
//! 1. **保留最近 `keep_versions` 份**（默认 3，[11 §3]）—— 超出的自动清理；
//! 2. **回滚**：按版本号把原文读回来，重新解析成 [`FetchOutcome`]；
//! 3. **记录被拒绝的版本**（`accepted = false`）：坏更新也留档，UI 能解释"为什么没采纳"，
//!    而回滚只会挑 `accepted` 的版本。
//!
//! 存储用 [`VersionStore`] 抽象：真实应用落 SQLite（ta-store），这里给内存与文件两个实现。
//! 文件实现把原文 **gzip 压缩**（订阅原文动辄几 MB，保留 3 份不压缩很浪费）。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ta_core::clock::{Clock, SystemClock};
use ta_core::error::{Result, TaError};
use ta_core::types::SubscriptionId;

use crate::{DetectedFormat, FetchOutcome, SubUserinfo};

/// 一版的元数据（列表 / UI 用，不含原文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMeta {
    /// 单调递增，从 1 开始
    pub version: u64,
    pub fetched_at_ms: i64,
    pub format: DetectedFormat,
    pub node_count: usize,
    pub group_count: usize,
    /// 原文字节数（压缩前）
    pub bytes: usize,
    /// 这一版是否被采纳过。**回滚只挑 `accepted` 的版本。**
    pub accepted: bool,
}

/// 一版的完整内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVersion {
    pub meta: VersionMeta,
    pub userinfo: Option<SubUserinfo>,
    pub suggested_interval_s: Option<u64>,
    /// 订阅原文（未解码、未改写）
    pub raw: Vec<u8>,
}

impl StoredVersion {
    pub fn from_outcome(
        version: u64,
        fetched_at_ms: i64,
        outcome: &FetchOutcome,
        accepted: bool,
    ) -> Self {
        Self {
            meta: VersionMeta {
                version,
                fetched_at_ms,
                format: outcome.format,
                node_count: outcome.nodes.len(),
                group_count: outcome.groups.len(),
                bytes: outcome.raw.len(),
                accepted,
            },
            userinfo: outcome.userinfo.clone(),
            suggested_interval_s: outcome.suggested_interval_s,
            raw: outcome.raw.clone(),
        }
    }

    /// 用存下来的原文重建 [`FetchOutcome`]（节点/分组重新提取，不保证与当初逐字节一致，
    /// 但同一份原文 + 同一个提取器 => 同一结果）。
    pub fn to_outcome(&self) -> FetchOutcome {
        let extracted = crate::parse::extract(&self.raw, self.meta.format);
        FetchOutcome {
            raw: self.raw.clone(),
            format: self.meta.format,
            userinfo: self.userinfo.clone(),
            suggested_interval_s: self.suggested_interval_s,
            nodes: extracted.nodes,
            groups: extracted.groups,
        }
    }
}

/// 保留的**被拒绝**版本数上限。
///
/// ★ 为什么单独算：`keep_versions` 是"保留几份**好**版本"。如果坏更新也占配额，
/// 连续几次坏更新就会把最后一份好版本挤掉 —— 那时"保留上版"这句承诺就成了空话，
/// 用户会真的丢掉所有线路。所以坏更新额外只留最近几份（用于解释"为什么没采纳"）。
pub const MAX_REJECTED_KEPT: usize = 2;

/// 版本存储。真实应用落 SQLite；这里不依赖 ta-store（[11 §1.1] 不跨 crate 用具体类型）。
pub trait VersionStore: Send + Sync {
    /// 追加一版，并把该订阅的历史裁剪到：最近 `keep` 份**已采纳** + 最近
    /// [`MAX_REJECTED_KEPT`] 份被拒绝的。
    /// `keep == 0` 表示不限量保留已采纳版本。
    fn append(&self, sub: &SubscriptionId, v: StoredVersion, keep: u32) -> Result<()>;
    /// 版本元数据，**新的在前**
    fn list(&self, sub: &SubscriptionId) -> Result<Vec<VersionMeta>>;
    fn get(&self, sub: &SubscriptionId, version: u64) -> Result<Option<StoredVersion>>;
    /// 该订阅保留的版本数
    fn count(&self, sub: &SubscriptionId) -> Result<usize> {
        Ok(self.list(sub)?.len())
    }
    /// 最近一版
    fn latest(&self, sub: &SubscriptionId) -> Result<Option<StoredVersion>> {
        match self.list(sub)?.first() {
            Some(m) => self.get(sub, m.version),
            None => Ok(None),
        }
    }
    /// 裁剪历史，返回删掉的份数
    fn prune(&self, sub: &SubscriptionId, keep: u32) -> Result<usize>;
}

/// 保留哪些版本：新的在前时，逐条判断"这条该不该留"。
///
/// 规则：已采纳的最多 `keep` 份（`keep == 0` = 不限），被拒绝的最多 [`MAX_REJECTED_KEPT`] 份。
fn keep_versions(metas: &[VersionMeta], keep: u32) -> Vec<VersionMeta> {
    let mut accepted = 0u32;
    let mut rejected = 0usize;
    let mut kept = Vec::new();
    for m in metas {
        if m.accepted {
            if keep == 0 || accepted < keep {
                accepted += 1;
                kept.push(m.clone());
            }
        } else if rejected < MAX_REJECTED_KEPT {
            rejected += 1;
            kept.push(m.clone());
        }
    }
    kept
}

// ================================================================ 内存实现

#[derive(Debug, Default)]
pub struct MemoryVersionStore {
    inner: Mutex<BTreeMap<String, Vec<StoredVersion>>>,
}

impl MemoryVersionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VersionStore for MemoryVersionStore {
    fn append(&self, sub: &SubscriptionId, v: StoredVersion, keep: u32) -> Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(sub.to_string()).or_default();
        entry.push(v);
        entry.sort_by_key(|x| x.meta.version);
        // 新的在前 → 决定留谁 → 反推该删的版本号
        let newest_first: Vec<VersionMeta> = entry.iter().rev().map(|x| x.meta.clone()).collect();
        let keep_set: std::collections::BTreeSet<u64> =
            keep_versions(&newest_first, keep).into_iter().map(|m| m.version).collect();
        entry.retain(|x| keep_set.contains(&x.meta.version));
        Ok(())
    }

    fn list(&self, sub: &SubscriptionId) -> Result<Vec<VersionMeta>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard
            .get(sub.as_str())
            .map(|v| v.iter().rev().map(|x| x.meta.clone()).collect())
            .unwrap_or_default())
    }

    fn get(&self, sub: &SubscriptionId, version: u64) -> Result<Option<StoredVersion>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard
            .get(sub.as_str())
            .and_then(|v| v.iter().find(|x| x.meta.version == version))
            .cloned())
    }

    fn prune(&self, sub: &SubscriptionId, keep: u32) -> Result<usize> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = guard.get_mut(sub.as_str()) else { return Ok(0) };
        let before = entry.len();
        let newest_first: Vec<VersionMeta> = entry.iter().rev().map(|x| x.meta.clone()).collect();
        let keep_set: std::collections::BTreeSet<u64> =
            keep_versions(&newest_first, keep).into_iter().map(|m| m.version).collect();
        entry.retain(|x| keep_set.contains(&x.meta.version));
        Ok(before - entry.len())
    }
}

// ================================================================ 文件实现

/// 文件实现里 `.json` 存的内容：元数据 + 响应头信息（原文另外单独 gzip 存）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionHeader {
    meta: VersionMeta,
    userinfo: Option<SubUserinfo>,
    suggested_interval_s: Option<u64>,
}

/// 目录布局：`<root>/<sub_id>/<version>.json` + `<version>.raw.gz`
pub struct FileVersionStore {
    root: PathBuf,
}

impl FileVersionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 目录名要能安全落盘（订阅 id 可能有 `/`、空格、emoji）
    fn sub_dir(&self, sub: &SubscriptionId) -> PathBuf {
        let safe: String = sub
            .as_str()
            .chars()
            .map(
                |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' },
            )
            .collect();
        self.root.join(safe)
    }

    fn meta_path(&self, sub: &SubscriptionId, version: u64) -> PathBuf {
        self.sub_dir(sub).join(format!("{version:08}.json"))
    }

    fn raw_path(&self, sub: &SubscriptionId, version: u64) -> PathBuf {
        self.sub_dir(sub).join(format!("{version:08}.raw.gz"))
    }

    fn read_versions(&self, sub: &SubscriptionId) -> Result<Vec<u64>> {
        let dir = self.sub_dir(sub);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(store_err(format!("读取版本目录 {} 失败：{e}", dir.display()))),
        };
        let mut versions = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".json") {
                if let Ok(v) = stem.parse::<u64>() {
                    versions.push(v);
                }
            }
        }
        versions.sort_unstable();
        Ok(versions)
    }

    fn read_header(&self, sub: &SubscriptionId, version: u64) -> Result<Option<VersionHeader>> {
        let path = self.meta_path(sub, version);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(store_err(format!("读取 {} 失败：{e}", path.display()))),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| store_err(format!("版本 {version} 元数据损坏：{e}")))
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

impl VersionStore for FileVersionStore {
    fn append(&self, sub: &SubscriptionId, v: StoredVersion, keep: u32) -> Result<()> {
        let header = VersionHeader {
            meta: v.meta.clone(),
            userinfo: v.userinfo.clone(),
            suggested_interval_s: v.suggested_interval_s,
        };
        let meta_json = serde_json::to_vec_pretty(&header)
            .map_err(|e| store_err(format!("序列化版本元数据失败：{e}")))?;
        let raw_gz = gzip(&v.raw)?;
        Self::write_atomic(&self.meta_path(sub, v.meta.version), &meta_json)?;
        Self::write_atomic(&self.raw_path(sub, v.meta.version), &raw_gz)?;
        self.prune(sub, keep)?;
        Ok(())
    }

    fn list(&self, sub: &SubscriptionId) -> Result<Vec<VersionMeta>> {
        let mut out = Vec::new();
        for version in self.read_versions(sub)?.into_iter().rev() {
            match self.read_header(sub, version)? {
                Some(h) => out.push(h.meta),
                None => continue, // 元数据文件在 list 与 read 之间被删掉了
            }
        }
        Ok(out)
    }

    fn get(&self, sub: &SubscriptionId, version: u64) -> Result<Option<StoredVersion>> {
        let Some(header) = self.read_header(sub, version)? else {
            return Ok(None);
        };
        let gz = std::fs::read(self.raw_path(sub, version))?;
        let raw = gunzip(&gz)?;
        Ok(Some(StoredVersion {
            meta: header.meta,
            userinfo: header.userinfo,
            suggested_interval_s: header.suggested_interval_s,
            raw,
        }))
    }

    fn prune(&self, sub: &SubscriptionId, keep: u32) -> Result<usize> {
        let versions = self.read_versions(sub)?;
        let mut newest_first = Vec::new();
        for version in versions.iter().rev() {
            if let Some(h) = self.read_header(sub, *version)? {
                newest_first.push(h.meta);
            }
        }
        let keep_set: std::collections::BTreeSet<u64> =
            keep_versions(&newest_first, keep).into_iter().map(|m| m.version).collect();
        let mut removed = 0;
        for version in &versions {
            if keep_set.contains(version) {
                continue;
            }
            for path in [self.meta_path(sub, *version), self.raw_path(sub, *version)] {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(store_err(format!("删除 {} 失败：{e}", path.display()))),
                }
            }
            removed += 1;
        }
        Ok(removed)
    }
}

fn store_err(msg: String) -> TaError {
    TaError::Store(format!("订阅版本：{msg}"))
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).map_err(|e| store_err(format!("gzip 压缩失败：{e}")))?;
    enc.finish().map_err(|e| store_err(format!("gzip 压缩失败：{e}")))
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut dec = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(|e| store_err(format!("gzip 解压失败：{e}")))?;
    Ok(out)
}

// ================================================================ 版本管理器

/// 记录 / 保留 / 回滚。时钟可注入，测试里版本时间戳是确定的。
pub struct VersionKeeper<S: VersionStore> {
    store: S,
    clock: Arc<dyn Clock>,
}

impl<S: VersionStore> VersionKeeper<S> {
    pub fn new(store: S) -> Self {
        Self { store, clock: Arc::new(SystemClock) }
    }

    pub fn with_clock(store: S, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// 记一版。`accepted = false` 表示这份是**被坏更新拦截**的（留档但不参与回滚）。
    /// 返回新版本号。
    pub fn record(
        &self,
        sub: &SubscriptionId,
        outcome: &FetchOutcome,
        keep: u32,
        accepted: bool,
    ) -> Result<u64> {
        let next = self.store.list(sub)?.first().map(|m| m.version + 1).unwrap_or(1);
        let stored = StoredVersion::from_outcome(next, self.clock.now_ms(), outcome, accepted);
        self.store.append(sub, stored, keep)?;
        Ok(next)
    }

    pub fn history(&self, sub: &SubscriptionId) -> Result<Vec<VersionMeta>> {
        self.store.list(sub)
    }

    /// 回滚到指定版本。**只接受 `accepted` 的版本** —— 否则等于主动采纳一份坏订阅。
    pub fn rollback_to(&self, sub: &SubscriptionId, version: u64) -> Result<FetchOutcome> {
        let Some(stored) = self.store.get(sub, version)? else {
            return Err(store_err(format!("订阅 {sub} 没有版本 {version}")));
        };
        if !stored.meta.accepted {
            return Err(store_err(format!(
                "订阅 {sub} 的版本 {version} 是被拒绝的坏更新，不能回滚到它"
            )));
        }
        Ok(stored.to_outcome())
    }

    /// 回到"上一个被采纳的版本"（最新的那份若被拒，就再往前找）。
    pub fn rollback_latest_accepted(&self, sub: &SubscriptionId) -> Result<FetchOutcome> {
        let (_, outcome) = self
            .latest_accepted(sub)?
            .ok_or_else(|| store_err(format!("订阅 {sub} 没有任何可回滚的已采纳版本")))?;
        Ok(outcome)
    }

    /// 最近一版**被采纳**的版本号 + 内容（坏更新拦截要拿它当基准）。
    pub fn latest_accepted(
        &self,
        sub: &SubscriptionId,
    ) -> Result<Option<(VersionMeta, FetchOutcome)>> {
        let versions = self.store.list(sub)?;
        let Some(meta) = versions.into_iter().find(|m| m.accepted) else {
            return Ok(None);
        };
        let outcome = self.rollback_to(sub, meta.version)?;
        Ok(Some((meta, outcome)))
    }

    /// 手动清理（保留最近 `keep` 份）
    pub fn cleanup(&self, sub: &SubscriptionId, keep: u32) -> Result<usize> {
        self.store.prune(sub, keep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect;
    use crate::testutil::TempDir;
    use ta_core::clock::FixedClock;

    fn sub(id: &str) -> SubscriptionId {
        SubscriptionId::new(id)
    }

    fn outcome(yaml: &str) -> FetchOutcome {
        let raw = yaml.as_bytes().to_vec();
        let format = detect::detect_format(&raw);
        let extracted = crate::parse::extract(&raw, format);
        FetchOutcome {
            raw,
            format,
            userinfo: None,
            suggested_interval_s: None,
            nodes: extracted.nodes,
            groups: extracted.groups,
        }
    }

    fn yaml_with_nodes(n: usize) -> String {
        let mut s = String::from("proxies:\n");
        for i in 0..n {
            s.push_str(&format!(
                "  - {{name: 节点{i}, type: ss, server: 1.2.3.{i}, port: 8388}}\n"
            ));
        }
        s.push_str("proxy-groups:\n  - {name: 自动, type: url-test, proxies: []}\n");
        s
    }

    fn keeper<S: VersionStore>(store: S) -> VersionKeeper<S> {
        VersionKeeper::with_clock(store, Arc::new(FixedClock::new(1_700_000_000_000)))
    }

    fn retention_contract<S: VersionStore>(store: S) {
        let k = keeper(store);
        let s = sub("sub-jp");
        for i in 0..5 {
            let v = k.record(&s, &outcome(&yaml_with_nodes(i + 1)), 3, true).unwrap();
            assert_eq!(v, i as u64 + 1, "版本号必须单调递增");
        }
        let history = k.history(&s).unwrap();
        assert_eq!(history.len(), 3, "只保留最近 3 份");
        // 新的在前
        assert_eq!(history.iter().map(|m| m.version).collect::<Vec<_>>(), vec![5, 4, 3]);
        assert_eq!(history[0].node_count, 5);
        assert_eq!(history[0].fetched_at_ms, 1_700_000_000_000);
        assert_eq!(history[0].format, DetectedFormat::ClashYaml);
        // 被裁掉的版本真的取不到了
        assert!(k.store().get(&s, 1).unwrap().is_none());
        assert!(k.rollback_to(&s, 1).is_err());
    }

    #[test]
    fn memory_store_retention() {
        retention_contract(MemoryVersionStore::new());
    }

    #[test]
    fn file_store_retention_and_roundtrip() {
        let dir = TempDir::new("sub-version");
        retention_contract(FileVersionStore::new(dir.path()));

        // 换一个实例（模拟进程重启）还能读回原文
        let k = keeper(FileVersionStore::new(dir.path()));
        let s = sub("sub-jp");
        let back = k.rollback_to(&s, 4).unwrap();
        assert_eq!(back.nodes.len(), 4);
        assert_eq!(back.nodes[3].name, "节点3");
        assert_eq!(back.format, DetectedFormat::ClashYaml);
        // 原文压缩存盘，但读回来是原样的
        assert!(String::from_utf8_lossy(&back.raw).contains("节点3"));
        // 目录里只留 3 份 meta + 3 份 raw
        let files: Vec<String> = std::fs::read_dir(dir.join("sub-jp"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 6, "{files:?}");
        assert!(!files.iter().any(|f| f.starts_with("00000001")), "{files:?}");
    }

    #[test]
    fn rollback_skips_rejected_versions() {
        let k = keeper(MemoryVersionStore::new());
        let s = sub("sub-jp");
        k.record(&s, &outcome(&yaml_with_nodes(40)), 3, true).unwrap();
        // 坏更新：HTML 错误页 → 被拦，留档但 accepted=false
        let bad = outcome("<!DOCTYPE html><html><body>502</body></html>");
        let v_bad = k.record(&s, &bad, 3, false).unwrap();
        assert_eq!(v_bad, 2);

        let history = k.history(&s).unwrap();
        assert!(!history[0].accepted, "坏更新要留档");
        assert_eq!(history[0].format, DetectedFormat::Html);

        // 直接回滚到坏版本：拒绝
        let err = k.rollback_to(&s, v_bad).unwrap_err();
        assert!(err.to_string().contains("被拒绝"), "{err}");

        // 回到上一个被采纳的版本：40 个节点还在
        let back = k.rollback_latest_accepted(&s).unwrap();
        assert_eq!(back.nodes.len(), 40);
    }

    #[test]
    fn rollback_without_any_accepted_version_errors() {
        let k = keeper(MemoryVersionStore::new());
        let s = sub("sub-jp");
        assert!(k.rollback_latest_accepted(&s).is_err(), "没有任何版本时应报错");
        // 只有被拒版本时也不给回滚
        k.record(&s, &outcome("<!DOCTYPE html><html></html>"), 3, false).unwrap();
        assert!(k.rollback_latest_accepted(&s).is_err());
    }

    /// ★ 灾难场景：连续多次坏更新之后，"上一份好版本"必须还在。
    ///
    /// 如果坏更新也占 `keep_versions` 的配额，连续 3 次坏更新就会把好版本挤掉 ——
    /// 那时"保留上版"这句承诺就是空话，用户会真的丢掉所有线路。
    #[test]
    fn repeated_bad_updates_never_evict_the_last_good_version() {
        for keep in [1u32, 2, 3] {
            let k = keeper(MemoryVersionStore::new());
            let s = sub("sub-jp");
            // 一份好版本：40 个节点
            let good_v = k.record(&s, &outcome(&yaml_with_nodes(40)), keep, true).unwrap();
            // 之后连续 5 次坏更新
            for i in 0..5 {
                let v =
                    k.record(&s, &outcome("<!DOCTYPE html><html>502</html>"), keep, false).unwrap();
                assert_eq!(v, good_v + i as u64 + 1);
            }
            // 好版本必须还在，而且能回滚
            let history = k.history(&s).unwrap();
            assert!(
                history.iter().any(|m| m.version == good_v && m.accepted),
                "keep={keep} 时好版本被坏更新挤掉了：{history:?}"
            );
            let back = k.rollback_latest_accepted(&s).unwrap();
            assert_eq!(back.nodes.len(), 40, "keep={keep}");
            // 坏更新只留最近 MAX_REJECTED_KEPT 份
            let rejected = history.iter().filter(|m| !m.accepted).count();
            assert_eq!(rejected, MAX_REJECTED_KEPT, "keep={keep}：{history:?}");
        }
    }

    #[test]
    fn keep_versions_rule_is_explicit() {
        let meta = |version: u64, accepted: bool| VersionMeta {
            version,
            fetched_at_ms: 0,
            format: DetectedFormat::ClashYaml,
            node_count: 1,
            group_count: 0,
            bytes: 1,
            accepted,
        };
        // 新的在前：a(3) r(2) a(1) r(0)
        let metas = vec![meta(3, true), meta(2, false), meta(1, true), meta(0, false)];
        // keep=1：只留 1 个好版本（3），坏版本留最近 2 个（2 和 0）
        let kept: Vec<u64> = keep_versions(&metas, 1).into_iter().map(|m| m.version).collect();
        assert_eq!(kept, vec![3, 2, 0]);
        // keep=0：好版本不限量（3、1），坏版本仍只留 MAX_REJECTED_KEPT=2 个（2、0）
        let kept: Vec<u64> = keep_versions(&metas, 0).into_iter().map(|m| m.version).collect();
        assert_eq!(kept, vec![3, 2, 1, 0]);
        // keep=2：两版好版本都留着
        let kept: Vec<u64> = keep_versions(&metas, 2).into_iter().map(|m| m.version).collect();
        assert_eq!(kept, vec![3, 2, 1, 0]);
    }

    #[test]
    fn prune_counts_and_zero_keep_means_keep_all() {
        let dir = TempDir::new("sub-prune");
        let k = keeper(FileVersionStore::new(dir.path()));
        let s = sub("sub-jp");
        for i in 0..4 {
            k.record(&s, &outcome(&yaml_with_nodes(i + 1)), 0, true).unwrap();
        }
        assert_eq!(k.history(&s).unwrap().len(), 4, "keep=0 表示不裁剪");
        assert_eq!(k.cleanup(&s, 2).unwrap(), 2, "返回删掉的份数");
        assert_eq!(k.history(&s).unwrap().len(), 2);

        // 内存实现同样语义
        let mk = keeper(MemoryVersionStore::new());
        for i in 0..4 {
            mk.record(&s, &outcome(&yaml_with_nodes(i + 1)), 0, true).unwrap();
        }
        assert_eq!(mk.cleanup(&s, 2).unwrap(), 2);
        assert_eq!(mk.history(&s).unwrap().len(), 2);
    }

    #[test]
    fn versions_are_per_subscription() {
        let k = keeper(MemoryVersionStore::new());
        let a = sub("sub-a");
        let b = sub("sub-b");
        k.record(&a, &outcome(&yaml_with_nodes(3)), 3, true).unwrap();
        k.record(&b, &outcome(&yaml_with_nodes(7)), 3, true).unwrap();
        assert_eq!(k.history(&a).unwrap()[0].node_count, 3);
        assert_eq!(k.history(&b).unwrap()[0].node_count, 7);
        // 版本号各自独立，都从 1 开始
        assert_eq!(k.history(&a).unwrap()[0].version, 1);
        assert_eq!(k.history(&b).unwrap()[0].version, 1);
    }

    #[test]
    fn userinfo_and_interval_survive_rollback() {
        let k = keeper(MemoryVersionStore::new());
        let s = sub("sub-jp");
        let mut o = outcome(&yaml_with_nodes(2));
        o.userinfo = Some(SubUserinfo {
            upload: 1,
            download: 2,
            total: 100,
            expire_ms: Some(1_800_000_000_000),
        });
        o.suggested_interval_s = Some(6 * 3600);
        k.record(&s, &o, 3, true).unwrap();

        let back = k.rollback_to(&s, 1).unwrap();
        assert_eq!(back.userinfo.unwrap().total, 100);
        assert_eq!(back.suggested_interval_s, Some(21600));
    }

    #[test]
    fn file_store_sanitizes_subscription_id() {
        let dir = TempDir::new("sub-sanitize");
        let k = keeper(FileVersionStore::new(dir.path()));
        let weird = sub("sub/jp 机场#1");
        k.record(&weird, &outcome(&yaml_with_nodes(1)), 3, true).unwrap();
        assert_eq!(k.history(&weird).unwrap().len(), 1);
        // 目录名被收敛成安全字符，不会打穿 root
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].starts_with("sub-jp"), "{entries:?}");
        assert!(
            entries[0].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "目录名必须是安全字符：{entries:?}"
        );
        // 没有打穿 root：目录就在 root 下面
        assert!(dir.path().join(&entries[0]).is_dir());
    }

    #[test]
    fn file_store_reports_corrupt_metadata() {
        let dir = TempDir::new("sub-corrupt");
        let store = FileVersionStore::new(dir.path());
        let s = sub("sub-jp");
        store
            .append(&s, StoredVersion::from_outcome(1, 0, &outcome(&yaml_with_nodes(1)), true), 0)
            .unwrap();
        std::fs::write(dir.join("sub-jp").join("00000001.json"), b"{oops").unwrap();
        assert!(matches!(store.list(&s), Err(TaError::Store(_))));
        assert!(matches!(store.get(&s, 1), Err(TaError::Store(_))));
    }
}

//! lane 端口分配。契约：。
//!
//! 从 `net.kernel.lane_port_range`（默认 39100–39299）分配：
//!
//! 1. **落库复用**：同一个 lane 尽量拿回上一次的端口（内核 reload / 重启后端口不变，
//!    上层已经建立的连接池不用重建）；
//! 2. **信任持久化的端口，不做事先 bind 探测**：绑定这个端口的往往就是**我们自己的
//!    mihomo listener**，探测会把"自家占着"读成"被别人占了"，于是每次保存端口都漂移
//!    （第 326 轮修的真机缺陷）。真正的占用由内核启动失败来表达；
//! 3. 只在 **127.0.0.1** 上分配，绝不碰 0.0.0.0（[04 §9]）。
//!
//! 存储用 [`PortStore`] 抽象：真实应用接 SQLite（ta-store），测试用内存/文件实现。
//! ta-kernel 不依赖 ta-store（见）。

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ta_core::error::{Result, TaError};
use ta_core::types::LaneId;

use crate::PortMap;

/// lane 端口的持久化口子（真实应用用 SQLite 实现）
pub trait PortStore: Send + Sync {
    fn load(&self) -> Result<Vec<(LaneId, u16)>>;
    fn save(&self, lane: &LaneId, port: u16) -> Result<()>;
    fn remove(&self, lane: &LaneId) -> Result<()>;
}

///  的 trait
pub trait PortAllocator: Send + Sync {
    fn allocate(&self, lane: &LaneId) -> Result<u16>;
    fn release(&self, lane: &LaneId);
    fn map(&self) -> PortMap;
}

impl PortMap {
    pub fn get(&self, lane: &LaneId) -> Option<u16> {
        self.map.get(lane).copied()
    }

    pub fn insert(&mut self, lane: LaneId, port: u16) {
        self.map.insert(lane, port);
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 遍历 `lane → port`。
    ///
    /// 加它是为了让"端口表热更新"能记下**哪几条变了** —— 诊断"端口漂移"
    /// 这类只有真机上才看得见的问题，靠的就是这条日志。
    pub fn iter(&self) -> impl Iterator<Item = (&LaneId, &u16)> {
        self.map.iter()
    }
}

impl From<BTreeMap<LaneId, u16>> for PortMap {
    fn from(map: BTreeMap<LaneId, u16>) -> Self {
        Self { map }
    }
}

/// `127.0.0.1:port` 现在能不能 bind 上。
///
/// Windows 下别人的进程监听 `0.0.0.0:port` 也会让我们 bind 失败 —— 正是想要的效果。
pub fn port_is_free(port: u16) -> bool {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => {
            drop(l);
            true
        }
        Err(_) => false,
    }
}

/// "这个端口现在能不能用"的抽象。
///
/// 生产用 [`SystemPortProbe`]（真 bind 试探）；单测用脚本化的 probe —— 否则测试
/// 会和**同一进程里并发的其它测试/临时端口分配**抢端口，随机失败。
pub trait PortProbe: Send + Sync {
    fn is_free(&self, port: u16) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPortProbe;

impl PortProbe for SystemPortProbe {
    fn is_free(&self, port: u16) -> bool {
        port_is_free(port)
    }
}

// ================================================================ 存储实现

/// 进程内存储（测试 / 不落库的场景）
#[derive(Debug, Default)]
pub struct MemoryPortStore {
    inner: Mutex<BTreeMap<LaneId, u16>>,
}

impl MemoryPortStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> BTreeMap<LaneId, u16> {
        self.inner.lock().expect("port store 锁被毒化").clone()
    }
}

impl PortStore for MemoryPortStore {
    fn load(&self) -> Result<Vec<(LaneId, u16)>> {
        Ok(self
            .inner
            .lock()
            .expect("port store 锁被毒化")
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect())
    }

    fn save(&self, lane: &LaneId, port: u16) -> Result<()> {
        self.inner.lock().expect("port store 锁被毒化").insert(lane.clone(), port);
        Ok(())
    }

    fn remove(&self, lane: &LaneId) -> Result<()> {
        self.inner.lock().expect("port store 锁被毒化").remove(lane);
        Ok(())
    }
}

/// JSON 文件存储（`provider: external` 的轻量场景 / 测试）。
///
/// 真实应用应换成 SQLite 实现（同一份 [`PortStore`] 契约）。
pub struct FilePortStore {
    path: PathBuf,
    /// 读-改-写期间串行化
    lock: Mutex<()>,
}

impl FilePortStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_map(&self) -> Result<BTreeMap<String, u16>> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(BTreeMap::new()),
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                TaError::Store(format!("lane 端口表 {} 解析失败：{e}", self.path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => {
                Err(TaError::Store(format!("读取 lane 端口表 {} 失败：{e}", self.path.display())))
            }
        }
    }

    fn write_map(&self, map: &BTreeMap<String, u16>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_vec_pretty(map)
            .map_err(|e| TaError::Store(format!("序列化 lane 端口表失败：{e}")))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl PortStore for FilePortStore {
    fn load(&self) -> Result<Vec<(LaneId, u16)>> {
        let _g = self.lock.lock().expect("port store 锁被毒化");
        Ok(self.read_map()?.into_iter().map(|(k, v)| (LaneId::new(k), v)).collect())
    }

    fn save(&self, lane: &LaneId, port: u16) -> Result<()> {
        let _g = self.lock.lock().expect("port store 锁被毒化");
        let mut map = self.read_map()?;
        map.insert(lane.to_string(), port);
        self.write_map(&map)
    }

    fn remove(&self, lane: &LaneId) -> Result<()> {
        let _g = self.lock.lock().expect("port store 锁被毒化");
        let mut map = self.read_map()?;
        map.remove(lane.as_str());
        self.write_map(&map)
    }
}

// ================================================================ 分配器

/// `[start, end]` 范围内分配 + bind 试探 + 落库复用。
pub struct RangePortAllocator {
    range: [u16; 2],
    store: Arc<dyn PortStore>,
    probe: Arc<dyn PortProbe>,
    inner: Mutex<BTreeMap<LaneId, u16>>,
}

impl std::fmt::Debug for RangePortAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RangePortAllocator")
            .field("range", &self.range)
            .field("assigned", &self.assigned())
            .finish()
    }
}

impl RangePortAllocator {
    /// 从存储恢复已有的分配（越界 / 重复的条目会被丢弃并清理）。
    pub fn new(range: [u16; 2], store: Arc<dyn PortStore>) -> Result<Self> {
        Self::with_probe(range, store, Arc::new(SystemPortProbe))
    }

    /// 换一个端口可用性探测器（生产不需要，测试用它做到完全确定性）
    pub fn with_probe(
        range: [u16; 2],
        store: Arc<dyn PortStore>,
        probe: Arc<dyn PortProbe>,
    ) -> Result<Self> {
        if range[0] == 0 || range[0] > range[1] {
            return Err(TaError::ConfigInvalid {
                errors: vec![ta_core::error::FieldError {
                    path: "net.kernel.lane_port_range".into(),
                    code: "invalid_range".into(),
                    message: format!("lane 端口范围不合法：{range:?}"),
                }],
            });
        }
        let this = Self { range, store, probe, inner: Mutex::new(BTreeMap::new()) };
        this.restore()?;
        Ok(this)
    }

    pub fn range(&self) -> [u16; 2] {
        self.range
    }

    /// 用 `net.kernel.lane_port_range` 建分配器（真实装配点）
    pub fn from_doc(doc: &ta_core::domain::ConfigDoc, store: Arc<dyn PortStore>) -> Result<Self> {
        Self::new(doc.net.kernel.lane_port_range, store)
    }

    /// 当前进程已知的分配表
    pub fn assigned(&self) -> BTreeMap<LaneId, u16> {
        self.inner.lock().expect("allocator 锁被毒化").clone()
    }

    fn restore(&self) -> Result<()> {
        let persisted = self.store.load()?;
        let mut assigned: BTreeMap<LaneId, u16> = BTreeMap::new();
        for (lane, port) in persisted {
            if port < self.range[0] || port > self.range[1] {
                tracing::warn!(
                    lane_id = %lane,
                    port,
                    range = ?self.range,
                    "落库的 lane 端口超出当前 range，丢弃并清理"
                );
                self.store.remove(&lane)?;
                continue;
            }
            if assigned.values().any(|p| *p == port) {
                tracing::warn!(lane_id = %lane, port, "落库的 lane 端口重复，丢弃并清理");
                self.store.remove(&lane)?;
                continue;
            }
            assigned.insert(lane, port);
        }
        *self.inner.lock().expect("allocator 锁被毒化") = assigned;
        Ok(())
    }
}

impl PortAllocator for RangePortAllocator {
    fn allocate(&self, lane: &LaneId) -> Result<u16> {
        let mut assigned = self.inner.lock().expect("allocator 锁被毒化");

        // 1) 先试复用落库的端口。
        //
        //（第 326 轮）**这里不再用 bind 探针判断"能不能复用"。**
        //
        // 原来的写法是 `if self.probe.is_free(port) { 复用 }`，否则当"被占用"重新分配。
        // 那个判据在运行期**必然为假**：这个端口此刻正被**我们自己的 mihomo listener**
        // 占着（内核 listen 在 `127.0.0.1:<port>` 上）。于是每写一次配置就把所有 lane
        // 的端口整体前移一位（39100→39102→…），而网关拨号器手里的表还是 boot 那份
        // —— 两边一起漂，请求打到旧端口上。
        //  里那条
        // `WARN … 落库的 lane 端口已被占用，重新分配（对上层透明）… port=39104 → 39105`
        // 就是它（文档当时把它归因于"复制数据目录"，其实是自家内核占的）。
        //
        // 改判据：**落库且仍在区间内、且没被同一个分配器分给别的 lane ⇒ 就是我们的端口**。
        // 端口归属这件事的唯一真源是 `ports.json` + `inner`（我们自己写的），
        // 而不是"现在能不能 bind" —— 后者问的是"有没有人在听"，而答案通常是"我们自己"。
        // 真被外来进程抢了，会在内核 listen 那一刻暴露（那是内核要报的错，不该靠这里猜）。
        if let Some(port) = assigned.get(lane).copied() {
            return Ok(port);
        }

        // 2) 在 range 里找第一个能 bind 的空位
        for port in self.range[0]..=self.range[1] {
            if assigned.values().any(|p| *p == port) {
                continue;
            }
            if self.probe.is_free(port) {
                assigned.insert(lane.clone(), port);
                if let Err(e) = self.store.save(lane, port) {
                    assigned.remove(lane);
                    return Err(e);
                }
                tracing::debug!(lane_id = %lane, port, "lane 端口已分配");
                return Ok(port);
            }
        }
        Err(TaError::Kernel(format!(
            "lane 端口耗尽：{:?} 内没有可用端口（已分配 {} 个）",
            self.range,
            assigned.len()
        )))
    }

    fn release(&self, lane: &LaneId) {
        let removed = self.inner.lock().expect("allocator 锁被毒化").remove(lane);
        if removed.is_some() {
            if let Err(e) = self.store.remove(lane) {
                tracing::warn!(lane_id = %lane, code = e.code(), error = %e, "清理 lane 端口记录失败");
            }
        }
    }

    fn map(&self) -> PortMap {
        PortMap { map: self.assigned() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(s: &str) -> LaneId {
        LaneId::new(s)
    }

    /// 记录哪些端口"被占"的假探测器 —— 让分配器测试完全不受并发测试/临时端口影响
    #[derive(Default)]
    struct ScriptedProbe {
        occupied: Mutex<std::collections::BTreeSet<u16>>,
    }

    impl ScriptedProbe {
        fn with_occupied(ports: &[u16]) -> Self {
            Self { occupied: Mutex::new(ports.iter().copied().collect()) }
        }
    }

    impl PortProbe for ScriptedProbe {
        fn is_free(&self, port: u16) -> bool {
            !self.occupied.lock().unwrap().contains(&port)
        }
    }

    fn alloc(
        range: [u16; 2],
        store: Arc<dyn PortStore>,
        probe: &Arc<ScriptedProbe>,
    ) -> RangePortAllocator {
        RangePortAllocator::with_probe(range, store, probe.clone() as Arc<dyn PortProbe>).unwrap()
    }

    /// 借一个空闲端口并**一直占着**（返回 listener，调用方负责持有到测试结束）
    fn hold_port() -> (TcpListener, u16) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        (l, p)
    }

    #[test]
    fn system_probe_detects_a_really_occupied_port() {
        // 真 bind 试探：占住的端口必须被判为不可用，释放后必须可用
        let (hold, port) = hold_port();
        assert!(!port_is_free(port), "被占用的端口应判为不可用");
        drop(hold);
        // 端口刚释放可能仍在 TIME_WAIT，等一小会儿
        let mut freed = false;
        for _ in 0..40 {
            if port_is_free(port) {
                freed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(freed, "释放后的端口最终应可 bind：{port}");
    }

    #[test]
    fn allocates_in_range_and_reports_exhaustion() {
        let store = Arc::new(MemoryPortStore::new());
        let probe = Arc::new(ScriptedProbe::default());
        let a = alloc([40_000, 40_001], store.clone(), &probe);

        let p1 = a.allocate(&lane("lane-a")).unwrap();
        assert_eq!(p1, 40_000);
        let p2 = a.allocate(&lane("lane-b")).unwrap();
        assert_eq!(p2, 40_001);
        // 同一个 lane 反复分配是幂等的
        assert_eq!(a.allocate(&lane("lane-a")).unwrap(), p1);

        let err = a.allocate(&lane("lane-c")).unwrap_err();
        assert!(matches!(err, TaError::Kernel(_)), "{err:?}");

        // 已落库
        let persisted = store.snapshot();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[&lane("lane-a")], p1);
    }

    #[test]
    fn skips_occupied_ports() {
        // 前两个被占，必须跳到第三个
        let probe = Arc::new(ScriptedProbe::with_occupied(&[40_000, 40_001]));
        let a = alloc([40_000, 40_003], Arc::new(MemoryPortStore::new()), &probe);

        assert_eq!(a.allocate(&lane("lane-a")).unwrap(), 40_002);
        assert_eq!(a.allocate(&lane("lane-b")).unwrap(), 40_003);
    }

    #[test]
    fn reuses_persisted_port_across_instances() {
        let store = Arc::new(MemoryPortStore::new());
        let probe = Arc::new(ScriptedProbe::default());
        let first = {
            let a = alloc([40_000, 40_009], store.clone(), &probe);
            a.allocate(&lane("lane-a")).unwrap()
        };
        // 新实例（模拟进程重启）应拿回同一个端口
        let a2 = alloc([40_000, 40_009], store.clone(), &probe);
        assert_eq!(a2.allocate(&lane("lane-a")).unwrap(), first);
        assert_eq!(a2.map().get(&lane("lane-a")), Some(first));
    }

    ///（第 326 轮）**落库端口被"占用"时也必须继续用它**。
    ///
    /// 这条测试原来断言的是相反的语义（"被占用 → 换一个"），而那个语义在真机上是**错的**：
    /// 端口此刻正被我们自己的 mihomo listener 占着，于是每写一次配置端口就整体前移一位，
    /// 而网关拨号器手里的表还是 boot 那份 —— 两边一起漂，请求打到旧端口。
    /// 用户第 326 轮报的"网络代理只有直连可用"里就有这一条。
    ///
    /// 现在的判据是：**落库的端口就是我们的**（唯一真源是 `ports.json` + 分配器自己），
    /// 只按"能不能 bind"判断会把自家内核当成入侵者。
    #[test]
    fn keeps_the_persisted_port_even_when_something_is_listening_on_it() {
        let store = Arc::new(MemoryPortStore::new());
        store.save(&lane("lane-a"), 40_000).unwrap();

        // 探针说这个端口"被占"——模拟我们自己的 mihomo 正 listen 在上面
        let probe = Arc::new(ScriptedProbe::with_occupied(&[40_000]));
        let a = alloc([40_000, 40_005], store.clone(), &probe);
        let p = a.allocate(&lane("lane-a")).unwrap();
        assert_eq!(p, 40_000, "落库端口必须**保持不变**（端口漂移是个真 bug）");
        assert_eq!(store.snapshot()[&lane("lane-a")], p, "存储里也不该变");

        // 对上层透明：下一次问还是同一个端口
        assert_eq!(a.allocate(&lane("lane-a")).unwrap(), p);
    }

    #[test]
    fn release_frees_the_port() {
        let store = Arc::new(MemoryPortStore::new());
        let probe = Arc::new(ScriptedProbe::default());
        let a = alloc([40_000, 40_001], store.clone(), &probe);
        let p = a.allocate(&lane("lane-a")).unwrap();
        a.release(&lane("lane-a"));
        assert!(a.map().is_empty());
        assert!(store.snapshot().is_empty());
        assert_eq!(a.allocate(&lane("lane-b")).unwrap(), p);
    }

    #[test]
    fn drops_out_of_range_and_duplicate_persisted_entries() {
        let store = Arc::new(MemoryPortStore::new());
        store.save(&lane("lane-far"), 1).unwrap();
        store.save(&lane("lane-dup1"), 40_000).unwrap();
        store.save(&lane("lane-dup2"), 40_000).unwrap();
        store.save(&lane("lane-good"), 40_003).unwrap();

        let probe = Arc::new(ScriptedProbe::default());
        let a = alloc([40_000, 40_009], store.clone(), &probe);
        let assigned = a.assigned();
        assert!(!assigned.contains_key(&lane("lane-far")));
        assert_eq!(assigned.len(), 2);
        assert!(assigned.contains_key(&lane("lane-good")));
        // 清理后的存储里也只剩合法的两条
        assert_eq!(store.snapshot().len(), 2);
    }

    #[test]
    fn invalid_range_is_rejected() {
        let err =
            RangePortAllocator::new([39299, 39100], Arc::new(MemoryPortStore::new())).unwrap_err();
        match err {
            TaError::ConfigInvalid { errors } => {
                assert_eq!(errors[0].path, "net.kernel.lane_port_range")
            }
            other => panic!("期望 ConfigInvalid，得到 {other:?}"),
        }
    }

    #[test]
    fn range_comes_from_net_kernel_config() {
        let doc: ta_core::domain::ConfigDoc =
            serde_yaml_ng::from_str("net:\n  kernel:\n    lane_port_range: [39100, 39299]\n")
                .unwrap();
        let a = RangePortAllocator::from_doc(&doc, Arc::new(MemoryPortStore::new())).unwrap();
        assert_eq!(a.range(), [39100, 39299]);
    }

    #[test]
    fn file_store_roundtrip() {
        let dir = crate::testutil::TempDir::new("ports");
        let path = dir.join("lane_ports.json");
        let store = FilePortStore::new(&path);
        assert!(store.load().unwrap().is_empty());

        store.save(&lane("lane-a"), 39100).unwrap();
        store.save(&lane("lane-b"), 39101).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&(lane("lane-a"), 39100)));

        store.remove(&lane("lane-a")).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);

        // 文件坏了要报错，而不是静默当成空表（否则会重复分配）
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(store.load(), Err(TaError::Store(_))));
    }

    #[test]
    fn file_store_is_shared_between_allocators() {
        let dir = crate::testutil::TempDir::new("ports-shared");
        let path = dir.join("lane_ports.json");
        let probe = Arc::new(ScriptedProbe::default());
        let first = {
            let a = RangePortAllocator::with_probe(
                [40_000, 40_009],
                Arc::new(FilePortStore::new(&path)),
                probe.clone() as Arc<dyn PortProbe>,
            )
            .unwrap();
            a.allocate(&lane("lane-a")).unwrap()
        };
        let a2 = RangePortAllocator::with_probe(
            [40_000, 40_009],
            Arc::new(FilePortStore::new(&path)),
            probe as Arc<dyn PortProbe>,
        )
        .unwrap();
        assert_eq!(a2.allocate(&lane("lane-a")).unwrap(), first);
    }
}

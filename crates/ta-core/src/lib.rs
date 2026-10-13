//! `ta-core` — TokenAgency 的纯领域层。
//!
//! **硬约束**：本 crate 不做 IO（除 `tracing`），不依赖 `tokio`/`axum`/`tauri`。
//! 所有可测逻辑都应当落在这里，这样 `cargo test -p ta-core` 是秒级的。
//!
//! 接口契约来源：`docs/11-internal-contracts.md`

pub mod clock;
pub mod config;
pub mod diag;
pub mod domain;
pub mod error;
pub mod metering;
pub mod pricing;
pub mod routing;
pub mod sticky;
pub mod trace;
pub mod types;

pub use clock::{Clock, SystemClock};
pub use error::{Result, TaError};
pub use types::*;

/// 当前 crate 的语义版本，写进 trace 便于排查。
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

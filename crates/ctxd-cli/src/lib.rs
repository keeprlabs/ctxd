//! Library face of the ctxd binary.
//!
//! The `ctxd` daemon is primarily a `[[bin]]` target, but several
//! modules — federation, the wire protocol — are exercised by
//! integration tests under `crates/ctxd-cli/tests/`. This thin lib
//! exists purely so `use ctxd_cli::federation::PeerManager` works
//! from those tests.

pub mod embedder;
pub mod federation;
pub mod protocol;
pub mod query;
pub mod rate_limit;
pub mod storage_selector;

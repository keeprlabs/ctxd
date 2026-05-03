//! `ctxd onboard` orchestration.
//!
//! Layout:
//!
//! * [`protocol`] — the versioned `--skill-mode` JSON contract that
//!   sits between the binary and any external front door (Claude Code
//!   skill, web installer, future IDE plugins). All other modules in
//!   this directory emit messages through this layer rather than
//!   `println!`-ing directly so the output stream stays well-typed.
//!
//! Future modules (added in subsequent phases): `paths`, `service`,
//! `clients`, `caps`, `seeds`, `adapters`, `doctor`, `snapshot`.
//!
//! The pipeline driver (the seven-step orchestrator) will land alongside
//! the first concrete step. Until then, this module exposes only the
//! protocol primitives so the skill team and the binary team can build
//! against a stable contract in parallel.

pub mod adapter_runtime;
pub mod caps;
pub mod clients;
pub mod doctor;
pub mod paths;
pub mod pipeline;
pub mod protocol;
pub mod seeds;
pub mod service;
pub mod skills_toml;
pub mod snapshot;

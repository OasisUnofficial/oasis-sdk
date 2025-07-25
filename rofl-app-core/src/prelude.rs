//! Prelude for ROFL applications.
pub use std::sync::Arc;

pub use anyhow::Result;
pub use async_trait::async_trait;
pub use slog;

pub use oasis_runtime_sdk::{self as sdk, core::consensus::verifier::TrustRoot, Version};

pub use super::{App, AppId, Environment};

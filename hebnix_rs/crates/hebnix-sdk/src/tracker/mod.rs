//! tracker.gg client for rocket league player stats.

pub mod cache;
pub mod client;
pub mod models;

pub use cache::TtlCache;
pub use client::TrackerClient;
pub use models::{LifetimeStats, PlayerStats, PlaylistAverage, PlaylistRank};

//! UPK patching and item swapping implementation.
//!
//! Keeping these modules together makes the patching boundary explicit while
//! the compatibility re-exports in `main.rs` preserve the existing call sites.
pub mod ball;
pub mod boost_patcher;
pub mod cosmetic_thumbnail;
pub mod cosmetic_upk;
pub mod decal_patcher;
pub mod patch_core;
pub mod swapper;
pub mod upk_keys;

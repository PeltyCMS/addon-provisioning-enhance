//! Enhance provisioning addon.
//! The whole module lives in [`enhance`]; this file only exports it to the host.
pub mod enhance;

#[cfg(all(target_family = "wasm", feature = "export"))]
pelty_addon_sdk::provisioning_addon!(enhance::Enhance);

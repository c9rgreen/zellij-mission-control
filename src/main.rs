// The state and tabs modules are only partly used by the native test build
#![cfg_attr(not(target_family = "wasm"), allow(dead_code))]

mod palette;
mod state;
mod tabs;

#[cfg(target_family = "wasm")]
mod plugin;

#[cfg(target_family = "wasm")]
use zellij_tile::prelude::*;

#[cfg(target_family = "wasm")]
register_plugin!(plugin::Dashboard);

#[cfg(not(target_family = "wasm"))]
fn main() {}

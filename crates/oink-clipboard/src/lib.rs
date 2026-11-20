//! 剪贴板。
//!
//! - [`ClipboardBackend`]：读写本机系统剪贴板、监听变化。平台实现在 `src/macos`、`src/windows`。
//! - [`sync`]：与平台无关的同步逻辑（通告、拉取、防回环、隐私标记透传）。

// The AppKit harness compiles this crate with cfg(test) without libtest, whose
// #[test] bodies are omitted. Their fixture helpers are consequently unused.
#![cfg_attr(test, allow(dead_code, unused_imports))]

include!("body.rs");

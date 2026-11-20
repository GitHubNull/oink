// The AppKit harness compiles the crate body with cfg(test) without libtest,
// whose #[test] bodies are omitted. Their fixture helpers are consequently unused.
#![cfg_attr(test, allow(dead_code, unused_imports))]

include!("body.rs");

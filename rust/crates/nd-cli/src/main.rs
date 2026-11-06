// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Service entrypoint: the typed service structs built by the other crates get wired here.
//! (Rust replaces Go's DI codegen — cmd/wire_gen.go, cmd/wire_injectors.go — with explicit
//! construction, decided once in #S1.)

/// Scaffold stage: nothing is wired yet, so the binary exits 0.
fn main() {}

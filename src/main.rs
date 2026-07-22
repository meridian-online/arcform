//! The `arc` binary. All of it lives in the library target — this is a shim so the
//! engine is compiled once and the CLI and the published spec loader can never be
//! built from different code.

fn main() {
    arc::cli_main();
}

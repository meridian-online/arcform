// With `bundled-duckdb`, libduckdb-sys links DuckDB's static-extension autoregister
// archive whole-archive, after libduckdb itself. That archive calls
// `duckdb_register_static_extensions`, which libduckdb defines and nothing before it
// references. GNU ld resolves archives in command-line order, so by the time it reads
// the call it has already passed over the definition, and the link fails with an
// undefined reference. rust-lld (the default linker for x86_64 Linux) and macOS's ld
// look back and link it; GNU ld, the default for aarch64 Linux, does not. Naming the
// symbol undefined up front makes GNU ld pull the definition in when it reads
// libduckdb. `-u` is harmless to a linker that needs no help. Applies to arc's own
// binaries and tests; a crate that links this library with `bundled-duckdb` on Linux
// needs the same flag until libduckdb-sys orders the archives itself.
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let bundled = std::env::var_os("CARGO_FEATURE_BUNDLED_DUCKDB").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if bundled && linux {
        println!("cargo::rustc-link-arg=-Wl,-u,duckdb_register_static_extensions");
    }
}

//! `protocol_db_path` puts a Protocol with no `db:` in arc's data folder, which
//! `$ARCFORM_DB_DIR` names. The variable is process-wide, so this test has a binary
//! of its own and nothing else here can read it while it is set.

use arcform::spec::protocol_db_path;

#[test]
fn db_path_is_arcs_data_folder_without_db() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("protocol");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("arcform.yaml"),
        "name: p\nengine: duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let data = root.path().join("data");
    // SAFETY: this binary holds one test, so no other thread reads or writes the
    // environment while the variable is set; it is cleared before the test returns.
    unsafe { std::env::set_var("ARCFORM_DB_DIR", &data) };
    let path = protocol_db_path(&dir);
    unsafe { std::env::remove_var("ARCFORM_DB_DIR") };
    let path = path.unwrap();
    assert!(path.starts_with(&data), "{path:?}");
    assert!(path.ends_with("p.duckdb"), "{path:?}");
}

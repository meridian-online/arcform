//! A Protocol's folder renamed, moved or copied keeps the versions local
//! history recorded for it, found through the log the folder carries, asked of
//! the real `arc` binary and of the library calls the app records with.
//!
//!   1. **a rename** — `arc history list` in the renamed folder prints what it
//!      printed before, line for line, for the spec and for a chart file under
//!      `panels/`, and `arc history restore` writes the bytes it wrote before;
//!   2. **a move and back** — the same holds in another directory, and a
//!      version recorded there is listed once the folder is back;
//!   3. **a copy** — the copy and its original list the same versions, a copy
//!      whose first act is a save keeps them too, and each lists the versions
//!      recorded in it after the copy and not those recorded in the other;
//!   4. **a log naming no version this store holds** — a fresh Protocol with
//!      the same bytes, or a clone from another machine, lists nothing until it
//!      records, then only what it recorded, and leaves the store as it was.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{HistoryWay, LocalHistory, MANIFEST_FILENAME};

const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: load_orders
    sql: models/01_orders.sql
";

const ORDERS_SQL: &str = "\
CREATE OR REPLACE TABLE orders AS
SELECT * FROM (VALUES (1, 50), (2, 150)) AS t(id, amount);
";

/// The spec as `arc edit-protocol replace name <name>` leaves it.
fn named(name: &str) -> String {
    MANIFEST.replacen("name: shop\n", &format!("name: {name}\n"), 1)
}

/// The chart file whose versions follow the spec's.
const CHART: &str = "panels/a.yaml";

/// The words the policy line uses for where the snapshots live.
const KEYED: &str = "the snapshots are stored outside the protocol at ";
const FOUND: &str = ", keyed to the protocol's path and found again through its log when the \
                     folder is renamed, moved or copied, and never promoted to git";

/// A scratch directory holding Protocol folders and this machine's store.
struct Machine {
    root: tempfile::TempDir,
    store: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let store = root.path().join("history");
        Machine { root, store }
    }

    /// `name` under the scratch directory.
    fn at(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// A Protocol at `name` with a spec, a model and a chart file, and no
    /// versions recorded.
    fn fresh(&self, name: &str) -> PathBuf {
        let dir = self.at(name);
        fs::create_dir_all(dir.join("models")).unwrap();
        fs::create_dir_all(dir.join("panels")).unwrap();
        fs::write(dir.join(MANIFEST_FILENAME), MANIFEST).unwrap();
        fs::write(dir.join("models/01_orders.sql"), ORDERS_SQL).unwrap();
        fs::write(dir.join(CHART), "mark: bar\n").unwrap();
        dir
    }

    /// A Protocol at `name` with three versions of its spec, recorded by
    /// `arc edit-protocol` — a checkpoint and two saves — and three of its
    /// chart file, recorded by the app: a checkpoint, a save and an unsaved
    /// text.
    fn recorded(&self, name: &str) -> PathBuf {
        let dir = self.fresh(name);
        self.arc_ok(&dir, &["edit-protocol", "replace", "name", "shop_two"]);
        self.arc_ok(&dir, &["edit-protocol", "replace", "name", "shop_three"]);
        let app = self.app();
        let chart = dir.join(CHART);
        app.record_checkpoint_for_file(&chart, "mark: bar\n")
            .unwrap()
            .expect("a checkpoint");
        app.record_save_for_file(&chart, "mark: line\n")
            .unwrap()
            .expect("a save");
        app.record_unsaved_for_file(&chart, "mark: area\n")
            .unwrap()
            .expect("an unsaved text");
        assert_eq!(ids(&self.list(&dir, None)).len(), 3, "three of the spec");
        assert_eq!(
            ids(&self.list(&dir, Some(CHART))).len(),
            3,
            "three of the chart"
        );
        dir
    }

    /// The store as the app opens it.
    fn app(&self) -> LocalHistory {
        LocalHistory::at_root(&self.store).reached_by(HistoryWay::new("app").unwrap())
    }

    /// `arc` with `args`, run in `dir` against this machine's store.
    fn arc(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(dir)
            .env("ARCFORM_HISTORY_DIR", &self.store)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    fn arc_ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.arc(dir, args);
        assert!(
            out.status.success(),
            "arc {args:?} in {} failed:\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// What `arc history list` prints in `dir`, for the spec or for `file`.
    fn list(&self, dir: &Path, file: Option<&str>) -> String {
        match file {
            None => self.arc_ok(dir, &["history", "list"]),
            Some(file) => self.arc_ok(dir, &["history", "list", "--file", file]),
        }
    }

    /// The bytes `arc history show` prints in `dir` for entry `id`.
    fn show(&self, dir: &Path, file: Option<&str>, id: &str) -> String {
        match file {
            None => self.arc_ok(dir, &["history", "show", id]),
            Some(file) => self.arc_ok(dir, &["history", "show", id, "--file", file]),
        }
    }

    /// The key directories the store holds.
    fn keys(&self) -> Vec<PathBuf> {
        match fs::read_dir(&self.store) {
            Ok(read) => {
                let mut keys: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
                keys.sort();
                keys
            }
            Err(_) => Vec::new(),
        }
    }
}

/// The entry ids a listing prints, in order: each entry's line opens with its
/// id, and no other line opens with a digit.
fn ids(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter(|line| line.starts_with(|c: char| c.is_ascii_digit()))
        .map(|line| line.split_whitespace().next().unwrap().to_string())
        .collect()
}

/// `listing` as it reads with the folder at `from` moved to `to`: the one path
/// a listing prints that names the folder is the log's.
fn moved(listing: &str, from: &Path, to: &Path) -> String {
    listing.replace(
        &canonical(from).display().to_string(),
        &canonical(to).display().to_string(),
    )
}

/// `path` under its canonical parent, which stays when the folder at `path`
/// has moved away.
fn canonical(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap().canonicalize().unwrap();
    parent.join(path.file_name().unwrap())
}

/// Copy the folder at `from`, every file under it, to `to`.
fn copy_folder(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_folder(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

/// The listings of the spec and the chart file in `dir`, and the bytes `arc
/// history show` gives for each entry each lists.
fn record_of(machine: &Machine, dir: &Path) -> Vec<(String, Vec<String>)> {
    [None, Some(CHART)]
        .into_iter()
        .map(|file| {
            let listing = machine.list(dir, file);
            let shown = ids(&listing)
                .iter()
                .map(|id| machine.show(dir, file, id))
                .collect();
            (listing, shown)
        })
        .collect()
}

/// `dir` lists, line for line, what `before` held at `was`, and shows the same
/// bytes for each entry.
fn lists_as_before(machine: &Machine, before: &[(String, Vec<String>)], was: &Path, dir: &Path) {
    let now = record_of(machine, dir);
    for ((listed, shown), (then, then_shown)) in now.iter().zip(before) {
        assert_eq!(
            *listed,
            moved(then, was, dir),
            "listed in {}",
            dir.display()
        );
        assert_eq!(shown, then_shown, "shown in {}", dir.display());
    }
}

/// `arc history restore` of the oldest entry of the spec and of the chart in
/// `dir` writes the bytes `before` showed for it.
fn restores_as_before(machine: &Machine, before: &[(String, Vec<String>)], dir: &Path) {
    for ((listing, shown), file) in before.iter().zip([None, Some(CHART)]) {
        let id = &ids(listing)[0];
        match file {
            None => machine.arc_ok(dir, &["history", "restore", id]),
            Some(file) => machine.arc_ok(dir, &["history", "restore", id, "--file", file]),
        };
        let path = dir.join(file.unwrap_or(MANIFEST_FILENAME));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            shown[0],
            "{}",
            path.display()
        );
    }
}

#[test]
fn a_renamed_folder_lists_shows_and_restores_what_it_did_before() {
    let machine = Machine::new();
    let was = machine.recorded("orders");
    let before = record_of(&machine, &was);

    let dir = machine.at("orders-renamed");
    fs::rename(&was, &dir).unwrap();

    lists_as_before(&machine, &before, &was, &dir);
    let listing = machine.list(&dir, None);
    assert!(
        listing.contains(KEYED) && listing.contains(FOUND),
        "the policy line says where the snapshots are and how a moved folder finds them:\n{listing}"
    );
    restores_as_before(&machine, &before, &dir);
}

#[test]
fn a_folder_moved_away_and_back_lists_the_versions_it_recorded_away() {
    let machine = Machine::new();
    let home = machine.recorded("orders");
    let before = record_of(&machine, &home);

    let away = machine.at("elsewhere").join("orders");
    fs::create_dir_all(away.parent().unwrap()).unwrap();
    fs::rename(&home, &away).unwrap();
    lists_as_before(&machine, &before, &home, &away);
    restores_as_before(&machine, &before, &away);

    // A version of each recorded away: the restores above are versions too.
    machine.arc_ok(&away, &["edit-protocol", "replace", "name", "shop_away"]);
    machine
        .app()
        .record_save_for_file(&away.join(CHART), "mark: point\n")
        .unwrap()
        .expect("a save");
    let recorded_away = record_of(&machine, &away);
    for ((listed, _), (then, _)) in recorded_away.iter().zip(&before) {
        assert!(
            ids(listed).len() > ids(then).len(),
            "the versions recorded away are listed away:\n{listed}"
        );
    }

    fs::rename(&away, &home).unwrap();
    lists_as_before(&machine, &recorded_away, &away, &home);
}

#[test]
fn a_copy_and_its_original_list_the_same_versions_and_part_at_the_next() {
    let machine = Machine::new();
    let original = machine.recorded("orders");
    let before = record_of(&machine, &original);

    let copy = machine.at("orders-copy");
    copy_folder(&original, &copy);
    lists_as_before(&machine, &before, &original, &copy);
    lists_as_before(&machine, &before, &original, &original);

    // A version recorded in each after the copy is the other's no longer.
    machine.arc_ok(&copy, &["edit-protocol", "replace", "name", "shop_copy"]);
    machine.arc_ok(
        &original,
        &["edit-protocol", "replace", "name", "shop_kept"],
    );
    let in_copy = ids(&machine.list(&copy, None));
    let in_original = ids(&machine.list(&original, None));
    let (copy_new, original_new) = (in_copy.last().unwrap(), in_original.last().unwrap());
    assert_eq!(machine.show(&copy, None, copy_new), named("shop_copy"));
    assert_eq!(
        machine.show(&original, None, original_new),
        named("shop_kept")
    );
    assert!(
        !in_original.contains(copy_new),
        "the copy's is not the original's"
    );
    assert!(
        !in_copy.contains(original_new),
        "the original's is not the copy's"
    );
    assert_eq!(in_copy[..3], in_original[..3], "both keep what they shared");

    // A copy of the copy whose first act is an edit: it keeps what the copy
    // listed, the version recorded only in the copy included. The edit's
    // checkpoint is the copy's newest state, already held, so the edit adds
    // its save alone.
    let second = machine.at("orders-copy-of-copy");
    copy_folder(&copy, &second);
    machine.arc_ok(
        &second,
        &["edit-protocol", "replace", "name", "shop_second"],
    );
    let in_second = ids(&machine.list(&second, None));
    assert_eq!(in_second[..in_copy.len()], in_copy[..], "{in_second:?}");
    assert_eq!(in_second.len(), in_copy.len() + 1, "{in_second:?}");
    assert_eq!(
        machine.show(&second, None, &in_second[in_copy.len()]),
        named("shop_second")
    );
}

#[test]
fn a_log_naming_no_version_this_store_holds_takes_nothing() {
    let machine = Machine::new();
    let first = machine.recorded("orders");
    let keys = machine.keys();

    // A fresh Protocol whose files are byte for byte the first's, and no log.
    let fresh = machine.at("fresh");
    copy_folder(&first, &fresh);
    fs::remove_file(fresh.join(arc::spec::LOG_FILENAME)).unwrap();

    // A clone from another machine: the same files, and a log naming versions
    // a store this machine never saw recorded.
    let other = Machine::new();
    let theirs = other.recorded("orders");
    let clone = machine.at("clone");
    copy_folder(&theirs, &clone);
    for name in [MANIFEST_FILENAME, CHART] {
        fs::copy(first.join(name), clone.join(name)).unwrap();
    }

    for dir in [&fresh, &clone] {
        for file in [None, Some(CHART)] {
            let listing = machine.list(dir, file);
            assert!(
                listing.starts_with("no local history for "),
                "{}: {listing}",
                dir.display()
            );
        }
        assert_eq!(machine.keys(), keys, "{} wrote to the store", dir.display());
    }

    let held: Vec<String> = [None, Some(CHART)]
        .into_iter()
        .flat_map(|file| ids(&machine.list(&first, file)))
        .collect();
    for dir in [&fresh, &clone] {
        machine.arc_ok(dir, &["edit-protocol", "replace", "name", "shop_own"]);
        let own = ids(&machine.list(dir, None));
        assert_eq!(own.len(), 2, "its checkpoint and its save: {own:?}");
        assert!(own.iter().all(|id| !held.contains(id)), "{own:?}");
        assert!(
            machine
                .list(dir, Some(CHART))
                .starts_with("no local history for "),
            "{}",
            dir.display()
        );
    }
}

//! Check a Protocol's panels against what the Protocol makes.
//!
//! A panel is a Brightfield spec (a Mosaic spec) at `panels/<name>.yaml`. Each mark
//! under `plot` reads a table through `data: {from: T}` and binds columns of it to
//! channels (`x: count`). `T` is either a key of the panel's own top-level `data:`
//! (inline rows) or a table in the Protocol's database, and a channel written as a
//! bare string names a column of it. This module reports a mark that reads a table
//! or a column the Protocol does not make, and nothing else: an object channel (an
//! expression or an aggregate) is not looked into.
//!
//! Once the database exists the tables and columns are read from it, read-only. Before
//! the first run there is no database, so a table is checked against the relations
//! the asset graph says the steps produce (one step can make several), and columns
//! are not checked.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::error::{Error, Result};

/// The channels a Mosaic mark binds data to, from the `ChannelName` enum of the
/// Mosaic JSON schema Brightfield vendors (`brightfield-spec/vendor/mosaic-schema/
/// v0.24.2.json`). `symbol` and `ariaLabel` are left out: a bare string there is
/// usually a literal (`symbol: circle`), not a column.
const CHANNELS: [&str; 28] = [
    "fill",
    "fillOpacity",
    "fontSize",
    "fx",
    "fy",
    "geometry",
    "height",
    "href",
    "length",
    "opacity",
    "path",
    "r",
    "rotate",
    "src",
    "stroke",
    "strokeOpacity",
    "strokeWidth",
    "text",
    "title",
    "weight",
    "width",
    "x",
    "x1",
    "x2",
    "y",
    "y1",
    "y2",
    "z",
];

/// The channels where a bare string may be a CSS colour rather than a column.
const COLOUR_CHANNELS: [&str; 2] = ["fill", "stroke"];

/// The CSS named colours (CSS Color Level 4, section 6.1), plus `none`, `transparent` and
/// `currentcolor` (valid paint values, not colours), lowercased. A fixed standard, so it does not go stale.
const COLOUR_NAMES: [&str; 151] = [
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkgrey",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "green",
    "greenyellow",
    "grey",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
    "none",
    "transparent",
    "currentcolor",
];

/// What a panel names that the Protocol does not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelFinding {
    MissingTable {
        from: String,
    },
    MissingColumn {
        from: String,
        column: String,
    },
    /// The file is not YAML, or holds no `plot`: nothing in it could be checked.
    Unreadable {
        reason: String,
    },
}

impl fmt::Display for PanelFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTable { from } => write!(
                f,
                "a mark reads {from}, which is neither in the panel's data nor a table the Protocol makes"
            ),
            Self::Unreadable { reason } => write!(f, "the panel could not be checked: {reason}"),
            Self::MissingColumn { from, column } => {
                write!(
                    f,
                    "a mark reads the column {column} of {from}, which {from} does not have"
                )
            }
        }
    }
}

/// What the Protocol makes, as far as it can be known.
enum Catalogue {
    /// The database exists: every table with its columns, both lowercased, keyed by
    /// `schema.table`.
    Database(BTreeMap<String, BTreeSet<String>>),
    /// No database yet: the relations the steps produce, lowercased.
    Produced(BTreeSet<String>),
}

impl Catalogue {
    fn load(dir: &Path) -> Result<Self> {
        let manifest = crate::manifest::Manifest::load(dir)?;
        if !manifest.db_path(dir)?.is_file() {
            let graph = crate::asset::AssetGraph::build(&manifest, dir);
            let produced = graph
                .steps
                .values()
                .flat_map(|step| step.produces.iter().map(|name| bare(name)))
                .collect();
            return Ok(Self::Produced(produced));
        }
        let (conn, _) = crate::inspect::open_read_only(dir)?;
        let db = |e: duckdb::Error| Error::Io(std::io::Error::other(format!("database: {e}")));
        let mut stmt = conn
            .prepare(
                "SELECT table_schema, table_name, column_name FROM information_schema.columns \
                 WHERE table_schema NOT IN ('information_schema', 'pg_catalog')",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db)?;
        let mut tables: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in rows {
            let (schema, table, column) = row.map_err(db)?;
            tables
                .entry(format!("{schema}.{table}").to_lowercase())
                .or_default()
                .insert(column.to_lowercase());
        }
        Ok(Self::Database(tables))
    }

    /// Findings for one mark's `from` and bare-string channels.
    fn check(&self, from: &str, channels: &[String], into: &mut Vec<PanelFinding>) {
        let wanted = from.to_lowercase();
        match self {
            Self::Produced(names) => {
                if !names.contains(&bare(&wanted)) {
                    push_once(into, PanelFinding::MissingTable { from: from.into() });
                }
            }
            Self::Database(tables) => {
                let found = if wanted.contains('.') {
                    tables.get(&wanted)
                } else {
                    tables.get(&format!("main.{wanted}")).or_else(|| {
                        tables
                            .iter()
                            .find(|(key, _)| bare(key) == wanted)
                            .map(|(_, columns)| columns)
                    })
                };
                match found {
                    None => push_once(into, PanelFinding::MissingTable { from: from.into() }),
                    Some(columns) => {
                        for column in channels {
                            if !columns.contains(&column.to_lowercase()) {
                                push_once(
                                    into,
                                    PanelFinding::MissingColumn {
                                        from: from.into(),
                                        column: column.clone(),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `name` without a leading schema.
fn bare(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_string()
}

fn push_once(into: &mut Vec<PanelFinding>, finding: PanelFinding) {
    if !into.contains(&finding) {
        into.push(finding);
    }
}

fn is_colour(value: &str) -> bool {
    let lowered = value.to_lowercase();
    lowered.starts_with('#')
        || [
            "rgb(", "rgba(", "hsl(", "hsla(", "hwb(", "lab(", "lch(", "oklab(", "oklch(", "color(",
        ]
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
        || COLOUR_NAMES.contains(&lowered.as_str())
}

/// Every mark of the panel: those under `plot`, and those of a plot nested in
/// `hconcat`/`vconcat` lists (or any mapping holding a `plot`), as Brightfield's
/// examples lay out linked views. Counts the `plot` keys met.
fn marks<'a>(node: &'a Value, into: &mut Vec<&'a Value>, plots: &mut usize) {
    if let Some(list) = node.get("plot").and_then(Value::as_sequence) {
        *plots += 1;
        into.extend(list);
    }
    for key in ["hconcat", "vconcat"] {
        for child in node
            .get(key)
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
        {
            marks(child, into, plots);
        }
    }
}

/// The panel's findings against `catalogue`.
fn findings_in(panel: &Value, catalogue: &Catalogue) -> Vec<PanelFinding> {
    let inline: BTreeSet<&str> = panel
        .get("data")
        .and_then(Value::as_mapping)
        .map(|m| m.keys().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut findings = Vec::new();
    let (mut all, mut plots) = (Vec::new(), 0);
    marks(panel, &mut all, &mut plots);
    if plots == 0 {
        findings.push(PanelFinding::Unreadable {
            reason: "it has no `plot`".to_string(),
        });
    }
    for mark in all {
        let Some(from) = mark
            .get("data")
            .and_then(|d| d.get("from"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if inline.contains(from) {
            continue;
        }
        let channels: Vec<String> = CHANNELS
            .iter()
            .filter_map(|name| {
                let value = mark.get(*name)?.as_str()?;
                let literal =
                    value.starts_with('$') || (COLOUR_CHANNELS.contains(name) && is_colour(value));
                (!literal).then(|| value.to_string())
            })
            .collect();
        catalogue.check(from, &channels, &mut findings);
    }
    findings
}

/// The panel at `path` read and checked; a file that is not YAML is a finding.
fn check_file(path: &Path, catalogue: &Catalogue) -> Result<Vec<PanelFinding>> {
    let text = fs::read_to_string(path).map_err(|e| Error::FileRead {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(match serde_yaml::from_str::<Value>(&text) {
        Ok(panel) => findings_in(&panel, catalogue),
        Err(e) => vec![PanelFinding::Unreadable {
            reason: format!("it is not valid YAML: {e}"),
        }],
    })
}

/// What the panel at `panel` (a path under `dir`, or relative to it) names that the
/// Protocol in `dir` does not make.
pub(crate) fn check_panel(dir: &Path, panel: &Path) -> Result<Vec<PanelFinding>> {
    let catalogue = Catalogue::load(dir)?;
    check_file(&dir.join(panel), &catalogue)
}

/// Every `panels/*.yaml` of the Protocol in `dir` with its findings, in name order.
/// A Protocol with no `panels/` folder has none.
pub(crate) fn check_panels(dir: &Path) -> Result<Vec<(PathBuf, Vec<PanelFinding>)>> {
    let folder = dir.join("panels");
    if !folder.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&folder)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    let catalogue = Catalogue::load(dir)?;
    paths
        .into_iter()
        .map(|path| {
            let findings = check_file(&path, &catalogue)?;
            let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
            Ok((rel, findings))
        })
        .collect()
}

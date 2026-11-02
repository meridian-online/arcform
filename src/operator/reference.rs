//! The Reference section of each operator's README, `operators/<name>/README.md`,
//! generated from the operator's description so the two cannot differ.
//!
//! The section sits between [`START`] and [`END`]; everything outside them is
//! written by hand and kept as it is. The test
//! `every_operator_readme_holds_its_generated_reference` fails when a README's
//! section differs from what this renders; run it with `ARC_UPDATE_READMES=1` to
//! rewrite the sections instead.

use serde_json::Value;

/// The line a README's generated section starts after.
pub(crate) const START: &str = "<!-- arc:reference:start -->";
/// The line a README's generated section ends before.
pub(crate) const END: &str = "<!-- arc:reference:end -->";

/// The Markdown Reference section for the operator called `op_name`, or `None`
/// for a name the operator catalogue does not describe.
pub(crate) fn reference_markdown(op_name: &str) -> Option<String> {
    let d = super::description_in_any_build(op_name)?;
    let mut md = String::new();
    md.push_str("## Reference\n\n");
    md.push_str(
        "<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, \
         then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->\n\n",
    );
    md.push_str(d["summary"].as_str().unwrap_or_default());
    md.push_str("\n\n### What it wraps\n\n");
    let wraps = d["wraps"].as_array().map(Vec::as_slice).unwrap_or_default();
    if wraps.is_empty() {
        md.push_str("Nothing beyond arc: arc does this work in its own code.\n");
    }
    for w in wraps {
        md.push_str(&format!(
            "- [{}]({}): {}\n",
            text(&w["name"]),
            text(&w["url"]),
            text(&w["use"])
        ));
    }
    for (heading, direction) in [("Reads", "reads"), ("Writes", "writes")] {
        let roles = d[direction]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if roles.is_empty() {
            continue;
        }
        md.push_str(&format!(
            "\n### {heading}\n\n| Key | Names | Required | Description |\n|---|---|---|---|\n"
        ));
        for r in roles {
            let mut names = text(&r["kind"]).to_string();
            if let Some(of) = r["of"].as_str() {
                names = format!("{names} of `{of}`");
            }
            if r["list"] == true {
                names = format!("list of {names}");
            }
            md.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                text(&r["name"]),
                names,
                yes_no(r["required"] == true),
                cell(text(&r["description"]))
            ));
        }
    }
    let parameters = &d["parameters"];
    let properties = parameters["properties"].as_object();
    if properties.is_some_and(|p| !p.is_empty()) {
        md.push_str(
            "\n### Parameters\n\n| Key | Type | Required | Default | Description |\n|---|---|---|---|---|\n",
        );
        let required = |key: &str| {
            parameters["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|k| k == key))
        };
        for (key, schema) in properties.into_iter().flatten() {
            let default = match schema.get("default") {
                Some(v) => format!("`{}`", inline(v)),
                None => "none".to_string(),
            };
            md.push_str(&format!(
                "| `{key}` | {} | {} | {default} | {} |\n",
                type_of(schema),
                yes_no(required(key)),
                cell(schema["description"].as_str().unwrap_or_default())
            ));
        }
    }
    md.push_str("\n### Example step\n\n```yaml\n");
    md.push_str(d["example"].as_str().unwrap_or_default());
    md.push_str("```\n");
    Some(md)
}

/// `readme` with the text between its markers replaced by `section`, or what is
/// wrong with its markers. Everything outside the markers is kept byte for byte.
pub(crate) fn splice(readme: &str, section: &str) -> Result<String, String> {
    let start = readme
        .find(START)
        .ok_or_else(|| format!("no `{START}` line"))?;
    let end = readme.find(END).ok_or_else(|| format!("no `{END}` line"))?;
    if end < start {
        return Err(format!("`{END}` comes before `{START}`"));
    }
    let head = &readme[..start + START.len()];
    let tail = &readme[end..];
    Ok(format!("{head}\n\n{section}\n{tail}"))
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// A schema's type in words: `string`, `list of string`, `one of `a`, `b``.
fn type_of(schema: &Value) -> String {
    if let Some(choices) = schema["enum"].as_array() {
        let names: Vec<String> = choices.iter().map(|c| format!("`{}`", inline(c))).collect();
        return format!("one of {}", names.join(", "));
    }
    match schema["type"].as_str() {
        Some("array") => format!("list of {}", type_of(&schema["items"])),
        Some("object") => "mapping".to_string(),
        Some(t) => t.to_string(),
        None => "any".to_string(),
    }
}

/// A JSON value as one line: a string bare, anything else as JSON.
fn inline(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `s` safe inside a Markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_lists_every_parameter_with_its_default() {
        let md = reference_markdown("umap_project").unwrap();
        assert!(md.contains("| `neighbors` | integer | no | `15` |"), "{md}");
        assert!(md.contains("| `metric` | one of `"), "{md}");
        assert!(md.contains("| no | `euclidean` |"), "{md}");
        assert!(
            md.contains("[umap-learn](https://umap-learn.readthedocs.io)"),
            "{md}"
        );
        assert!(
            md.contains("```yaml\n- name: project\n  op: umap_project@1\n"),
            "{md}"
        );
        assert!(reference_markdown("no_such_operator").is_none());
    }

    #[test]
    fn hand_prose_outside_the_markers_is_kept() {
        let readme = format!("# x\n\nIntro.\n\n{START}\nold\n{END}\n\n## More\n\nKept.\n");
        let out = splice(&readme, "new\n").unwrap();
        assert_eq!(
            out,
            format!("# x\n\nIntro.\n\n{START}\n\nnew\n\n{END}\n\n## More\n\nKept.\n")
        );
        assert_eq!(
            splice(&out, "new\n").unwrap(),
            out,
            "splicing is idempotent"
        );
        assert!(splice("# x\n", "new").unwrap_err().contains("start"));
        assert!(
            splice(&format!("{START}\n"), "new")
                .unwrap_err()
                .contains("end")
        );
        assert!(splice(&format!("{END}\n{START}\n"), "new").is_err());
    }

    #[test]
    fn every_operator_readme_holds_its_generated_reference() {
        let update = std::env::var_os("ARC_UPDATE_READMES").is_some();
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // The published crate carries no READMEs (Cargo.toml's `include`), so its
        // source has nothing to compare; a checkout always has `.git`.
        if !manifest_dir.join(".git").exists() {
            eprintln!("not a checkout: the operator READMEs are not in this source");
            return;
        }
        let root = manifest_dir.join("operators");
        let mut stale = Vec::new();
        for d in super::super::DESCRIBED {
            let path = root.join(d.name).join("README.md");
            let readme = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let section = reference_markdown(d.name).unwrap();
            let spliced =
                splice(&readme, &section).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            if spliced != readme {
                if update {
                    std::fs::write(&path, spliced).unwrap();
                } else {
                    stale.push(path.display().to_string());
                }
            }
        }
        assert!(
            stale.is_empty(),
            "these READMEs' Reference sections differ from the operator catalogue; \
             run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`: {stale:?}"
        );
    }
}

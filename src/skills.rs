//! `arc skills`: the analyst skills under the repository's `skills/`, embedded in the binary by
//! `build.rs`, listed and installed into agents' skills folders.
//!
//! The skills installed are the ones this binary carries, so they always match the arc that
//! wrote them. Install writes only inside the skills folders it names, and it never overwrites
//! a folder it did not write: it recognises its own by the marker file [`MARKER`], which
//! records the arc version, and refuses any other folder of the same name.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

include!(concat!(env!("OUT_DIR"), "/skills.rs"));

/// The file `arc skills install` puts in each skill folder it writes. Its first line is
/// `arc <version>`.
pub const MARKER: &str = ".arc-skill";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// An agent arc knows how to install skills for: its name for `--agent`, the folder under the
/// home directory whose presence means the agent is installed, and the folder under the home
/// directory it reads user-level skills from.
struct Agent {
    name: &'static str,
    config_dir: &'static str,
    skills_dir: &'static str,
}

/// The agents, in the order they are detected and printed. Each skills folder is the one the
/// agent's own documentation gives for user-level skills.
const AGENTS: &[Agent] = &[
    // https://code.claude.com/docs/en/skills ("Personal: ~/.claude/skills/<skill-name>/SKILL.md")
    Agent {
        name: "claude-code",
        config_dir: ".claude",
        skills_dir: ".claude/skills",
    },
    // https://developers.openai.com/codex/skills (now learn.chatgpt.com/docs/build-skills):
    // user skills live in `$HOME/.agents/skills`; Codex's own documentation no longer names
    // `~/.codex/skills`.
    Agent {
        name: "codex",
        config_dir: ".codex",
        skills_dir: ".agents/skills",
    },
    // https://cursor.com/docs/skills (global skills: `~/.cursor/skills/` and `~/.agents/skills/`)
    Agent {
        name: "cursor",
        config_dir: ".cursor",
        skills_dir: ".cursor/skills",
    },
    // https://geminicli.com/docs/cli/skills/ (user skills: `~/.gemini/skills/`, alias
    // `~/.agents/skills/`)
    Agent {
        name: "gemini",
        config_dir: ".gemini",
        skills_dir: ".gemini/skills",
    },
    // The shared folder of the Agent Skills standard, read by Codex, Cursor and Gemini CLI
    // (sources above).
    Agent {
        name: "agents",
        config_dir: ".agents",
        skills_dir: ".agents/skills",
    },
];

/// The names `--agent` accepts.
pub const AGENT_NAMES: [&str; 5] = ["claude-code", "codex", "cursor", "gemini", "agents"];

/// A skills folder to install into, and the agent it is for (`target` for a folder named with
/// `--target`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTarget {
    pub agent: &'static str,
    pub skills_dir: PathBuf,
}

/// One skill folder written by [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub agent: &'static str,
    pub skill: String,
    pub path: PathBuf,
    /// The arc version of the install this one replaced, when it replaced one.
    pub replaced: Option<String>,
}

/// The agents installed under `home`: each whose config folder exists. Two agents that read
/// the same skills folder give one target, named for the first.
pub fn agent_targets(home: &Path) -> Vec<AgentTarget> {
    let found = AGENTS.iter().filter(|a| home.join(a.config_dir).is_dir());
    dedupe(found.map(|a| target_for(home, a)))
}

/// The targets for the agents `names` names, whether or not they are installed.
pub fn named_targets(home: &Path, names: &[String]) -> Result<Vec<AgentTarget>> {
    let mut targets = Vec::new();
    for name in names {
        let agent = AGENTS.iter().find(|a| a.name == name).ok_or_else(|| {
            refusal(format!(
                "skills: no agent called '{name}'; name one of {}",
                AGENT_NAMES.join(", ")
            ))
        })?;
        targets.push(target_for(home, agent));
    }
    Ok(dedupe(targets.into_iter()))
}

/// What to say when no agent is found under `home`: where each agent would be installed, and
/// how to name a target.
pub fn no_agent_message(home: &Path) -> String {
    let mut msg = format!(
        "skills: no agent found under {}. arc installs for an agent when its folder exists:\n",
        home.display()
    );
    for a in AGENTS {
        msg.push_str(&format!(
            "  {:<12} {}  (when ~/{} exists)\n",
            a.name,
            home.join(a.skills_dir).display(),
            a.config_dir
        ));
    }
    msg.push_str(
        "Name one with `arc skills install --agent <name>`, or a folder with `arc skills install --target <dir>`.",
    );
    msg
}

fn target_for(home: &Path, agent: &Agent) -> AgentTarget {
    AgentTarget {
        agent: agent.name,
        skills_dir: home.join(agent.skills_dir),
    }
}

fn dedupe(targets: impl Iterator<Item = AgentTarget>) -> Vec<AgentTarget> {
    let mut out: Vec<AgentTarget> = Vec::new();
    for t in targets {
        if !out.iter().any(|o| o.skills_dir == t.skills_dir) {
            out.push(t);
        }
    }
    out
}

fn refusal(msg: String) -> Error {
    Error::Io(io::Error::other(msg))
}

/// The embedded skills: each skill's name and its files, by path within the skill folder.
fn skills() -> BTreeMap<&'static str, Vec<(&'static str, &'static [u8])>> {
    let mut out: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    for (path, bytes) in SKILLS {
        if let Some((skill, rel)) = path.split_once('/') {
            out.entry(skill).or_default().push((rel, bytes));
        }
    }
    out
}

/// Each embedded skill and the `metadata.arc_version` its `SKILL.md` names (empty when it
/// names none).
pub fn list() -> Vec<(String, String)> {
    skills()
        .into_iter()
        .map(|(name, files)| {
            let version = files
                .iter()
                .find(|(rel, _)| *rel == "SKILL.md")
                .and_then(|(_, bytes)| arc_version(&String::from_utf8_lossy(bytes)))
                .unwrap_or_default();
            (name.to_string(), version)
        })
        .collect()
}

/// `metadata.arc_version` from a `SKILL.md`'s frontmatter.
fn arc_version(skill_md: &str) -> Option<String> {
    let front = skill_md.strip_prefix("---")?;
    let front = &front[..front.find("\n---")?];
    front.lines().find_map(|line| {
        let value = line.trim().strip_prefix("arc_version:")?.trim();
        Some(value.trim_matches(|c| c == '"' || c == '\'').to_string())
    })
}

/// What install will do with one skill folder.
enum Plan {
    Write,
    Replace(String),
}

/// Install every embedded skill into each target's skills folder.
///
/// A folder holding [`MARKER`] is replaced wholesale, so a file a newer version dropped is
/// gone; an absent folder is written; any other folder of the same name is refused. Every
/// folder is checked before any is written, and one refusal writes nothing, since a partial
/// set of skills that name each other is worse than none.
pub fn install(targets: &[AgentTarget]) -> Result<Vec<Installed>> {
    let skills = skills();
    let mut plans = Vec::new();
    let mut refused = Vec::new();
    for target in targets {
        for name in skills.keys() {
            let dest = target.skills_dir.join(name);
            match plan(&dest)? {
                Ok(p) => plans.push((target, *name, dest, p)),
                Err(reason) => refused.push(format!("  {}: {reason}", dest.display())),
            }
        }
    }
    if !refused.is_empty() {
        return Err(refusal(format!(
            "skills: nothing installed; arc does not overwrite a folder it did not write:\n{}",
            refused.join("\n")
        )));
    }

    let mut installed = Vec::new();
    for (target, name, dest, plan) in plans {
        write_skill(&target.skills_dir, name, &skills[name], &dest)?;
        installed.push(Installed {
            agent: target.agent,
            skill: name.to_string(),
            path: dest,
            replaced: match plan {
                Plan::Write => None,
                Plan::Replace(v) => Some(v),
            },
        });
    }
    Ok(installed)
}

/// Whether `dest` can be written: `Ok(Err(reason))` when it must be refused.
fn plan(dest: &Path) -> Result<std::result::Result<Plan, String>> {
    let meta = match fs::symlink_metadata(dest) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Ok(Plan::Write)),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() {
        return Ok(Err(
            "it exists and is not a folder arc wrote (no .arc-skill marker)".to_string(),
        ));
    }
    match fs::read_to_string(dest.join(MARKER)) {
        Ok(marker) => {
            let first = marker.lines().next().unwrap_or("").trim();
            let version = first.strip_prefix("arc ").unwrap_or(first).trim();
            Ok(Ok(Plan::Replace(version.to_string())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Err(
            "a skill of this name is already there and has no .arc-skill marker, so it is not arc's to replace; rename or remove it, or install with --target elsewhere".to_string(),
        )),
        Err(e) => Err(e.into()),
    }
}

/// Write one skill into a staging folder beside `dest`, then swap it in, so a failure part
/// way leaves the old folder whole.
fn write_skill(skills_dir: &Path, name: &str, files: &[(&str, &[u8])], dest: &Path) -> Result<()> {
    fs::create_dir_all(skills_dir)?;
    let stage = skills_dir.join(format!(".{name}.arc-new"));
    if fs::symlink_metadata(&stage).is_ok() {
        fs::remove_dir_all(&stage)?;
    }
    for (rel, bytes) in files {
        let path = stage.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    fs::write(
        stage.join(MARKER),
        format!(
            "arc {VERSION}\nWritten by `arc skills install`, which replaces this folder when it is run again. Delete this file to keep the folder as your own.\n"
        ),
    )?;
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::rename(&stage, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_version_is_read_from_the_frontmatter() {
        let md = "---\nname: x\nmetadata:\n  arc_version: \"1.2.3\"\n---\n\n# X\n";
        assert_eq!(arc_version(md).as_deref(), Some("1.2.3"));
        assert_eq!(arc_version("# no frontmatter\n"), None);
    }

    #[test]
    fn agents_sharing_a_folder_give_one_target() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".codex")).unwrap();
        fs::create_dir(home.path().join(".agents")).unwrap();
        let targets = agent_targets(home.path());
        assert_eq!(
            targets,
            vec![AgentTarget {
                agent: "codex",
                skills_dir: home.path().join(".agents/skills"),
            }]
        );
    }

    #[test]
    fn agent_names_match_the_agents() {
        let names: Vec<_> = AGENTS.iter().map(|a| a.name).collect();
        assert_eq!(names, AGENT_NAMES);
    }

    #[test]
    fn an_unknown_agent_is_refused_naming_the_known_ones() {
        let err = named_targets(Path::new("/h"), &["vim".to_string()]).unwrap_err();
        assert!(err.to_string().contains("claude-code"), "{err}");
    }
}

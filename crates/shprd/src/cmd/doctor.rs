//! `shprd doctor`: what works on this machine, and what to fix.
//!
//! A read-only page of sections: the host shprd runs in, the harness data it
//! loads, the config, the installed agents, their quota, and the skills they
//! see. A probe that fails becomes a line rather than an error, so one run
//! shows every problem. The verb exits 1 when any line is a [`Level::Fail`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::{self, Config};
use shprd_roster::detect::find_binary;
use shprd_roster::skills::{CompatList, VisibilityRules};
use shprd_roster::spec::{HarnessSpec, load_dir};

use crate::cli::DoctorArgs;
use crate::ctx::{CliError, resolve_home};

/// How much attention one line wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Working as intended.
    Ok,
    /// Suspicious, worth a look.
    Warn,
    /// Broken: the verb exits 1.
    Fail,
    /// Neither good nor bad, just worth showing.
    Info,
}

impl Level {
    /// The glyph the text report puts before a line.
    fn glyph(self) -> char {
        match self {
            Self::Ok => '✓',
            Self::Warn => '!',
            Self::Fail => '✗',
            Self::Info => '·',
        }
    }

    /// The lower-case name the JSON report uses.
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Info => "info",
        }
    }
}

/// One line of the report, without its glyph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// How much attention the line wants.
    pub level: Level,
    /// What the line says.
    pub text: String,
}

/// One titled block of lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    /// The heading, e.g. `Host`.
    pub title: String,
    /// The lines under it, in the order they were found.
    pub lines: Vec<Line>,
}

/// The whole page, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The sections.
    pub sections: Vec<Section>,
}

impl Report {
    /// Starts a titled section and returns it for filling.
    fn section(&mut self, title: &str) -> &mut Section {
        self.sections.push(Section {
            title: title.to_owned(),
            lines: Vec::new(),
        });
        let index = self.sections.len() - 1;
        &mut self.sections[index]
    }

    /// Whether any line failed, which decides the exit code.
    pub fn has_fail(&self) -> bool {
        self.sections
            .iter()
            .flat_map(|section| section.lines.iter())
            .any(|line| line.level == Level::Fail)
    }
}

impl Section {
    /// Appends one line.
    fn add(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push(Line {
            level,
            text: text.into(),
        });
    }
}

/// Runs `shprd doctor`, printing the page and failing when a check failed.
pub fn run(args: &DoctorArgs, json: bool, home: Option<&Path>) -> Result<(), CliError> {
    let report = build(home);
    if json || args.json {
        print_json(&report)?;
    } else {
        print!("{}", render(&report));
    }
    if report.has_fail() {
        return Err(CliError::other(anyhow!(
            "a check failed: fix the lines marked with ✗"
        )));
    }
    Ok(())
}

/// Builds the whole page, one section at a time.
fn build(home_flag: Option<&Path>) -> Report {
    let facts = Facts::resolve(home_flag);
    let mut report = Report::default();
    host(&mut report, &facts);
    let _specs = data(&mut report, &facts);
    let _config = config_section(&mut report, &facts);
    report
}

/// The machine facts every section reads, resolved once.
struct Facts {
    /// The environment the verb started in, values included.
    current: BTreeMap<String, String>,
    /// `PATH`, in the form the detection helpers take it.
    path_env: String,
    /// The user's home directory, when one resolved.
    home: Option<PathBuf>,
    /// The repository root, when the verb runs inside one.
    repo: Option<PathBuf>,
    /// The data directory holding `harness/` and `skills/`.
    data: Option<PathBuf>,
}

impl Facts {
    /// Collects the environment, home, repo and data directory.
    fn resolve(home_flag: Option<&Path>) -> Self {
        Self {
            current: std::env::vars().collect(),
            path_env: std::env::var("PATH").unwrap_or_default(),
            home: resolve_home(home_flag).ok(),
            repo: crate::ctx::resolve_root(None).ok(),
            data: crate::cmd::roster::resolve_data_dir().ok(),
        }
    }
}

/// The page as text: a heading per section, then one glyphed line each.
fn render(report: &Report) -> String {
    let mut out = String::new();
    for section in &report.sections {
        out.push_str(&section.title);
        out.push('\n');
        for line in &section.lines {
            out.push_str(&format!("  {} {}\n", line.level.glyph(), line.text));
        }
    }
    out
}

/// The page as JSON: one object per section, one object per line.
fn print_json(report: &Report) -> Result<(), CliError> {
    let sections: Vec<serde_json::Value> = report
        .sections
        .iter()
        .map(|section| {
            let lines: Vec<serde_json::Value> = section
                .lines
                .iter()
                .map(|line| {
                    serde_json::json!({
                        "level": line.level.name(),
                        "text": line.text,
                    })
                })
                .collect();
            serde_json::json!({"title": section.title, "lines": lines})
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::json!({"sections": sections}))
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the report")))?;
    println!("{text}");
    Ok(())
}

/// Whether the environment marks this as inside a multiplexer.
///
/// Tuios is checked first: it can also set `HERDR_ENV=1` in some panes, so a
/// herdr-first test would name the wrong multiplexer.
fn marked(current: &BTreeMap<String, String>, var: &str) -> bool {
    current.get(var).map(String::as_str) == Some("1")
}

/// Section 1: the multiplexer we are inside and the binaries we would drive.
fn host(report: &mut Report, facts: &Facts) {
    let section = report.section("Host");
    if marked(&facts.current, "TUIOS_ENV") {
        section.add(Level::Ok, "inside tuios");
    } else if marked(&facts.current, "HERDR_ENV") {
        section.add(Level::Ok, "inside herdr");
    } else {
        section.add(Level::Warn, "not inside herdr or tuios");
    }
    for binary in ["herdr", "tuios"] {
        match find_binary(binary, &facts.path_env) {
            Some(path) => section.add(Level::Ok, format!("{binary} {}", path.display())),
            None => section.add(Level::Warn, format!("{binary} not on PATH")),
        }
    }
}

/// Section 2: the harness table and the curated skills tables in `data/`.
///
/// Returns the specs it loaded, so the agent section does not load them again.
fn data(report: &mut Report, facts: &Facts) -> Vec<HarnessSpec> {
    let section = report.section("Data");
    let Some(dir) = &facts.data else {
        section.add(
            Level::Fail,
            "no data directory: set SHPRD_DATA_DIR to a directory holding `harness/`",
        );
        return Vec::new();
    };
    section.add(Level::Info, format!("data directory {}", dir.display()));
    let specs = match load_dir(&dir.join("harness")) {
        Ok(specs) => {
            section.add(
                Level::Ok,
                format!("{} harness spec(s) load and validate", specs.len()),
            );
            specs
        }
        Err(err) => {
            section.add(Level::Fail, format!("harness specs do not load: {err}"));
            Vec::new()
        }
    };
    let skills_dir = dir.join("skills");
    match CompatList::load(&skills_dir.join("compat.toml")) {
        Ok(list) => section.add(
            Level::Ok,
            format!("compat.toml: {} entries", list.skills.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("compat.toml does not load: {err}")),
    }
    match VisibilityRules::load(&skills_dir.join("visibility.toml")) {
        Ok(rules) => section.add(
            Level::Ok,
            format!("visibility.toml: {} rule(s)", rules.rules.len()),
        ),
        Err(err) => section.add(Level::Fail, format!("visibility.toml does not load: {err}")),
    }
    specs
}

/// Section 3: the layered config and the three role agent strings.
fn config_section(report: &mut Report, facts: &Facts) -> Option<Config> {
    let section = report.section("Config");
    let global = facts
        .home
        .as_ref()
        .map(|home| home.join(".config").join("shprd").join("config.toml"));
    let repo_layer = facts
        .repo
        .as_ref()
        .map(|root| root.join(".shprd").join("config.toml"));
    let loaded = match config::load(global.as_deref(), repo_layer.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            section.add(Level::Fail, format!("does not load: {err}"));
            return None;
        }
    };
    match (&global, repo_layer.as_deref()) {
        (Some(global), Some(repo)) => section.add(
            Level::Ok,
            format!(
                "loads and validates ({} and {})",
                global.display(),
                repo.display()
            ),
        ),
        _ => section.add(Level::Ok, "loads and validates"),
    }
    let roster = &loaded.roster;
    role(section, "orchestrator", roster.orchestrator.as_deref());
    role(section, "reviewer", roster.reviewer.as_deref());
    if roster.workers.is_empty() {
        section.add(Level::Warn, "workers not set");
    } else {
        section.add(Level::Ok, format!("workers {}", roster.workers.join(", ")));
    }
    Some(loaded)
}

/// Adds one role line: the agent string, or a warning that it is unset.
fn role(section: &mut Section, name: &str, agent: Option<&str>) {
    match agent {
        Some(agent) if !agent.trim().is_empty() => {
            section.add(Level::Ok, format!("{name} {agent}"));
        }
        _ => section.add(Level::Warn, format!("{name} not set")),
    }
}

//! `shprd doctor`: what works on this machine, and what to fix.
//!
//! A read-only page of sections: the host shprd runs in, the harness data it
//! loads, the config, the installed agents, their quota, and the skills they
//! see. A probe that fails becomes a line rather than an error, so one run
//! shows every problem. The verb exits 1 when any line is a [`Level::Fail`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::{self, Config, Roster};
use shprd_roster::detect::{Detection, SignIn, detect_all, find_binary, vendor_name};
use shprd_roster::env::{self, EnvContext, LaunchEnv, Role};
use shprd_roster::models::AgentRef;
use shprd_roster::quota::{self, ProbeContext, QuotaReport, QuotaState};
use shprd_roster::skills::{self, CompatList, ReviewState, Verdict, VisibilityRules};
use shprd_roster::spec::{HarnessSpec, Vendor, load_dir};

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
    let report = build(home, args.no_quota);
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
fn build(home_flag: Option<&Path>, no_quota: bool) -> Report {
    let facts = Facts::resolve(home_flag);
    let mut report = Report::default();
    host(&mut report, &facts);
    let specs = data(&mut report, &facts);
    let config = config_section(&mut report, &facts);
    let clean = worker_env(&facts);
    let detections = detect_every(&facts, &specs, config.as_ref());
    agents(&mut report, &detections);
    review(&mut report, &facts, &specs, config.as_ref());
    if !no_quota {
        quota_section(&mut report, &facts, &specs, config.as_ref());
    }
    skills_section(&mut report, &facts, &specs, &clean, &detections);
    launch_env_section(&mut report, &facts, &clean);
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

/// The clean environment a worker CLI is launched with.
fn worker_env(facts: &Facts) -> LaunchEnv {
    let ctx = EnvContext {
        actor: "worker".to_owned(),
        task: None,
        run: None,
        repo: std::env::current_dir().unwrap_or_default(),
    };
    env::launch_env(&facts.current, Role::Worker, &ctx)
}

/// The model the pi sign-in probe needs: from the roster, else the environment.
fn pi_models(
    facts: &Facts,
    specs: &[HarnessSpec],
    config: Option<&Config>,
) -> BTreeMap<String, String> {
    if !specs.iter().any(|spec| spec.id == "pi") {
        return BTreeMap::new();
    }
    let roster = config.map(|config| &config.roster);
    let from_roster = roster.and_then(|roster| {
        roster
            .workers
            .iter()
            .chain(roster.orchestrator.iter())
            .chain(roster.reviewer.iter())
            .find_map(|agent| model_after_pi(specs, agent))
    });
    let model = from_roster.or_else(|| {
        facts
            .current
            .get("SHPRD_PI_MODEL")
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
    });
    let mut models = BTreeMap::new();
    if let Some(model) = model {
        models.insert("pi".to_owned(), model);
    }
    models
}

/// The model of an agent string that names the pi harness, when it has one.
fn model_after_pi(specs: &[HarnessSpec], agent: &str) -> Option<String> {
    let parsed = AgentRef::parse(agent, specs).ok()?;
    if parsed.program != "pi" {
        return None;
    }
    parsed.model.filter(|model| !model.trim().is_empty())
}

/// Probes every harness CLI with the environment a worker is launched with.
fn detect_every(facts: &Facts, specs: &[HarnessSpec], config: Option<&Config>) -> Vec<Detection> {
    if specs.is_empty() {
        return Vec::new();
    }
    let pairs = worker_env(facts).as_pairs();
    let models = pi_models(facts, specs, config);
    detect_all(specs, &facts.path_env, &pairs, &models)
}

/// Section 4: every harness CLI, as a worker would find it.
fn agents(report: &mut Report, detections: &[Detection]) {
    let section = report.section("Agents");
    if detections.is_empty() {
        section.add(Level::Warn, "no harness spec, so no agent could be probed");
        return;
    }
    for detection in detections {
        let (level, text) = describe(detection);
        section.add(level, text);
    }
}

/// One detection as a level and a line.
fn describe(detection: &Detection) -> (Level, String) {
    let (level, binary) = match &detection.binary {
        Some(path) => (Level::Ok, path.display().to_string()),
        None => (Level::Warn, "not installed".to_owned()),
    };
    let (level, signin) = match &detection.signin {
        SignIn::SignedIn => (level, "signed in".to_owned()),
        SignIn::SignedOut => (Level::Fail, "signed out".to_owned()),
        SignIn::Unknown(reason) => (Level::Warn, format!("unknown ({reason})")),
    };
    let version = detection.version.as_deref().unwrap_or("-");
    let text = format!(
        "{}  {}  {}  {}  {}",
        detection.id,
        binary,
        version,
        signin,
        vendor_name(detection.vendor)
    );
    (level, text)
}

/// The vendor `agent` names, or why the string names none.
fn vendor_or_reason(specs: &[HarnessSpec], agent: &str) -> Result<Vendor, String> {
    let named = AgentRef::parse(agent, specs).map_err(|error| error.to_string())?;
    named
        .spec(specs)
        .map(|spec| spec.vendor)
        .ok_or_else(|| format!("the harness table has no spec for `{}`", named.program))
}

/// Section 5: whether the reviewer comes from another vendor than every worker.
///
/// An agent string that names no vendor is reported as a warning line rather
/// than skipped, and a review with any such agent never claims independence:
/// an unchecked worker could share the reviewer's vendor.
fn review(report: &mut Report, _facts: &Facts, specs: &[HarnessSpec], config: Option<&Config>) {
    let section = report.section("Review");
    let Some(roster) = config.map(|config| &config.roster) else {
        section.add(Level::Warn, "no config loads, so no reviewer to check");
        return;
    };
    let reviewer = roster
        .reviewer
        .as_deref()
        .filter(|agent| !agent.trim().is_empty());
    let Some(reviewer) = reviewer else {
        section.add(Level::Warn, "reviewer not set, so no cross-vendor review");
        return;
    };
    let mut skipped = 0usize;
    let vendor = match vendor_or_reason(specs, reviewer) {
        Ok(vendor) => Some(vendor),
        Err(reason) => {
            skipped += 1;
            section.add(
                Level::Warn,
                format!("cannot check vendor of {reviewer}: {reason}"),
            );
            None
        }
    };
    let mut clashes: Vec<&str> = Vec::new();
    for worker in &roster.workers {
        match vendor_or_reason(specs, worker) {
            Ok(worker_vendor) => {
                if vendor == Some(worker_vendor) {
                    clashes.push(worker.as_str());
                }
            }
            Err(reason) => {
                skipped += 1;
                section.add(
                    Level::Warn,
                    format!("cannot check vendor of {worker}: {reason}"),
                );
            }
        }
    }
    match vendor {
        Some(vendor) if clashes.is_empty() && skipped == 0 => section.add(
            Level::Ok,
            format!(
                "reviewer {reviewer} is {}, independent of every worker",
                vendor_name(vendor)
            ),
        ),
        Some(vendor) if !clashes.is_empty() => section.add(
            Level::Fail,
            format!(
                "reviewer {reviewer} is {}: the same vendor as worker(s) {}",
                vendor_name(vendor),
                clashes.join(", ")
            ),
        ),
        _ => {}
    }
}

/// Section 6: what quota each agent reports, never naming a credential.
fn quota_section(
    report: &mut Report,
    facts: &Facts,
    specs: &[HarnessSpec],
    config: Option<&Config>,
) {
    let section = report.section("Quota");
    let Some(home) = facts.home.clone() else {
        section.add(
            Level::Warn,
            "no home directory, so no credential store to probe",
        );
        return;
    };
    let mut ctx = ProbeContext::new(home.clone());
    ctx.free_pin = free_pin(config, &home);
    for agent in ["claude", "codex", "kilo"] {
        if !specs.iter().any(|spec| spec.id == agent) {
            section.add(Level::Info, format!("{agent}  not in the harness table"));
            continue;
        }
        let in_role = fills_a_role(config, specs, agent);
        let (level, text) = quota_line(&quota::probe(agent, &ctx), in_role);
        section.add(level, text);
    }
}

/// Whether kilo runs on its free tier: shprd's own pick, else kilo's default.
///
/// A configured free model wins, because it is the one a dispatch would use. A
/// machine with no kilo pick at all still counts when kilo's own config names
/// a free router: the paid balance is zero there and does not bind.
fn free_pin(config: Option<&Config>, home: &Path) -> bool {
    if configured_free_pin(config) {
        return true;
    }
    quota::kilo::free_pin(quota::kilo::default_model(home).as_deref())
}

/// Whether a configured kilo agent string pins the free tier.
fn configured_free_pin(config: Option<&Config>) -> bool {
    let Some(roster) = config.map(|config| &config.roster) else {
        return false;
    };
    every_agent(roster)
        .any(|agent| agent.starts_with("kilo/") && quota::kilo::free_pin(Some(agent.as_str())))
}

/// Every agent string the roster names, in any role.
fn every_agent(roster: &Roster) -> impl Iterator<Item = &String> {
    roster
        .workers
        .iter()
        .chain(roster.orchestrator.iter())
        .chain(roster.reviewer.iter())
}

/// Whether `agent`'s program fills at least one role in the loaded config.
///
/// Every configured string is parsed against the harness table, so a bare
/// `kilo` in a role counts exactly like `kilo/kilo-auto/free` does.
fn fills_a_role(config: Option<&Config>, specs: &[HarnessSpec], agent: &str) -> bool {
    let Some(roster) = config.map(|config| &config.roster) else {
        return false;
    };
    every_agent(roster).any(|pinned| {
        AgentRef::parse(pinned, specs)
            .map(|named| named.program == agent)
            .unwrap_or(false)
    })
}

/// One quota report as a level and a line: state, binding window, reset.
///
/// A dry or unknown agent fails the verb only when a role actually dispatches
/// it; an agent nothing uses is a warning, so nobody is blocked by a probe for
/// a program they never run.
fn quota_line(report: &QuotaReport, in_role: bool) -> (Level, String) {
    let (level, state) = match (report.state, in_role) {
        (QuotaState::Ok, _) => (Level::Ok, "ok"),
        (QuotaState::Dry, true) => (Level::Fail, "dry"),
        (QuotaState::Dry, false) => (Level::Warn, "dry"),
        (QuotaState::Unknown, true) => (Level::Fail, "unknown"),
        (QuotaState::Unknown, false) => (Level::Warn, "unknown"),
    };
    let mut text = format!("{}  {state}", report.agent);
    match report.windows.first() {
        Some(window) => text.push_str(&format!(
            "  {} {:.0}% used",
            window.name, window.used_percent
        )),
        None => text.push_str("  no window reported"),
    }
    if let Some(reset) = report.reset_at_ms {
        text.push_str(&format!("  reset {} UTC", clock(reset)));
    }
    if report.state == QuotaState::Unknown {
        text.push_str(&format!("  ({})", report.detail));
    }
    (level, text)
}

/// A reset instant as `HH:MM` in UTC, so the line never needs a locale or a
/// timezone crate. The line labels it, so the reader is never misled.
fn clock(ms: u64) -> String {
    let of_day = (ms / 1000) % 86_400;
    format!("{:02}:{:02}", of_day / 3600, (of_day % 3600) / 60)
}

/// Section 7: the skills each installed CLI sees, and the curated review.
fn skills_section(
    report: &mut Report,
    facts: &Facts,
    specs: &[HarnessSpec],
    clean: &LaunchEnv,
    detections: &[Detection],
) {
    let section = report.section("Skills");
    let (Some(dir), Some(home)) = (facts.data.as_ref(), facts.home.as_ref()) else {
        section.add(
            Level::Warn,
            "no data directory or home, so no skill was scanned",
        );
        return;
    };
    let skills_dir = dir.join("skills");
    let (Ok(compat), Ok(rules)) = (
        CompatList::load(&skills_dir.join("compat.toml")),
        VisibilityRules::load(&skills_dir.join("visibility.toml")),
    ) else {
        section.add(Level::Warn, "the skill tables do not load");
        return;
    };
    let ignore = rules.ignore_patterns();
    let clean_env = clean.apply_to(&facts.current);
    let lock = skills::read_lock(home).unwrap_or_default();
    for spec in specs {
        if !detections
            .iter()
            .any(|found| found.id == spec.id && found.binary.is_some())
        {
            continue;
        }
        let seen = skills::visible(
            spec,
            home,
            facts.repo.as_deref(),
            &clean_env,
            &rules,
            &ignore,
        );
        section.add(
            Level::Ok,
            format!("{}  {} skill(s) visible", spec.id, seen.skills.len()),
        );
        hidden_now(section, &spec.id, &rules, &facts.current, &clean_env);
        for junk in top_junk(&seen.junk) {
            section.add(
                Level::Warn,
                format!("{}  junk folder {}", spec.id, tilde(junk, home)),
            );
        }
    }
    compat_review(section, &compat, &lock);
}

/// Warns for each variable that hides a directory from this shell but not
/// from a launch.
fn hidden_now(
    section: &mut Section,
    cli: &str,
    rules: &VisibilityRules,
    current: &BTreeMap<String, String>,
    clean_env: &BTreeMap<String, String>,
) {
    let still_shown = rules.hidden(cli, clean_env);
    for (dir, var) in rules.hidden(cli, current) {
        if still_shown.iter().any(|(shown, _)| *shown == dir) {
            continue;
        }
        section.add(
            Level::Warn,
            format!("{var} hides {dir} from this shell; unset {var} to see it"),
        );
    }
}

/// Warns about skills the curated list wants looked at.
fn compat_review(
    section: &mut Section,
    compat: &CompatList,
    lock: &BTreeMap<String, skills::LockEntry>,
) {
    for row in skills::review(compat, lock) {
        match &row.state {
            ReviewState::Reviewed => {}
            ReviewState::ChangedUpstream { reviewed, current } => section.add(
                Level::Warn,
                format!(
                    "{}  changed upstream ({reviewed} -> {current}): re-review",
                    row.name
                ),
            ),
            ReviewState::NotInCompatList => section.add(
                Level::Warn,
                format!("{}  installed but not in compat.toml", row.name),
            ),
            ReviewState::MissingLocally => {}
        }
        if row.verdict == Some(Verdict::Deny) && row.state != ReviewState::MissingLocally {
            section.add(
                Level::Info,
                format!("{}  denied by compat.toml but installed", row.name),
            );
        }
    }
}

/// Section 8: the variables a worker launch drops, by name only.
///
/// Values are never printed: the variables dropped include credentials.
fn launch_env_section(report: &mut Report, facts: &Facts, clean: &LaunchEnv) {
    let section = report.section("Launch env");
    let dropped: Vec<String> = clean
        .removed_names()
        .into_iter()
        .filter(|name| facts.current.contains_key(name))
        .collect();
    if dropped.is_empty() {
        section.add(
            Level::Ok,
            "no variable a worker launch drops is set in this shell",
        );
    } else {
        section.add(
            Level::Info,
            format!("worker launches drop {}", dropped.join(", ")),
        );
    }
}

/// The junk entries that are not inside another junk entry.
///
/// The scanner reports an ignored folder and everything under it; a report
/// wants the folder once, so a nested path is dropped.
fn top_junk(junk: &[PathBuf]) -> Vec<&Path> {
    junk.iter()
        .map(PathBuf::as_path)
        .filter(|path| {
            !junk
                .iter()
                .any(|other| other.as_path() != *path && path.starts_with(other))
        })
        .collect()
}

/// A path spelled with `~` for the home directory, so a line stays short.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use shprd_roster::quota::Tier;

    /// A report in `state`, as a probe would return it.
    fn report(agent: &str, state: QuotaState) -> QuotaReport {
        QuotaReport {
            agent: agent.to_owned(),
            state,
            tier: Tier::Measured,
            windows: Vec::new(),
            reset_at_ms: None,
            source: "kilo-profile".to_owned(),
            detail: "a zero balance".to_owned(),
            checked_at_ms: 0,
        }
    }

    /// A config whose roles are the given agents.
    fn config(orchestrator: Option<&str>, workers: &[&str]) -> Config {
        let mut config = Config::default();
        config.roster.orchestrator = orchestrator.map(str::to_owned);
        config.roster.workers = workers.iter().map(|agent| (*agent).to_owned()).collect();
        config
    }

    #[test]
    fn a_worker_whose_vendor_cannot_be_checked_is_reported() {
        let specs = harness_specs();
        let mut config = config(None, &["kilo", "gemini/pro"]);
        config.roster.reviewer = Some("claude/opus".to_owned());
        let mut page = Report::default();
        review(&mut page, &Facts::resolve(None), &specs, Some(&config));
        let text = render(&page);
        assert!(text.contains("cannot check vendor of gemini/pro"), "{text}");
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    #[test]
    fn a_reviewer_whose_vendor_cannot_be_checked_is_reported() {
        let specs = harness_specs();
        let mut config = config(None, &["claude/opus"]);
        config.roster.reviewer = Some("gemini/pro".to_owned());
        let mut page = Report::default();
        review(&mut page, &Facts::resolve(None), &specs, Some(&config));
        let text = render(&page);
        assert!(text.contains("cannot check vendor of gemini/pro"), "{text}");
        assert!(!text.contains("independent of every worker"), "{text}");
    }

    /// The shipped harness table, which names every program.
    fn harness_specs() -> Vec<HarnessSpec> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness");
        match load_dir(&dir) {
            Ok(specs) => specs,
            Err(error) => panic!("the shipped harness table must load: {error}"),
        }
    }

    /// A report holding only this one quota line, and the level it carries.
    fn line_level(config: Option<&Config>, agent: &str, state: QuotaState) -> (Level, bool) {
        let specs = harness_specs();
        let in_role = fills_a_role(config, &specs, agent);
        let (level, text) = quota_line(&report(agent, state), in_role);
        let mut report = Report::default();
        report.section("Quota").add(level, text);
        (level, report.has_fail())
    }

    #[test]
    fn a_dry_agent_no_role_uses_is_a_warning_and_the_verb_succeeds() {
        let (level, fails) = line_level(None, "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Warn);
        assert_eq!(level.glyph(), '!');
        assert!(!fails, "an unused dry agent must not fail the verb");
    }

    #[test]
    fn a_dry_agent_a_worker_uses_fails_the_verb() {
        let config = config(Some("claude/opus"), &["kilo/kilo-auto/free"]);
        let (level, fails) = line_level(Some(&config), "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Fail);
        assert_eq!(level.glyph(), '✗');
        assert!(fails, "a dry worker must fail the verb");
    }

    #[test]
    fn an_unknown_agent_outside_every_role_only_warns() {
        let config = config(Some("claude/opus"), &["claude/opus"]);
        let (level, fails) = line_level(Some(&config), "codex", QuotaState::Unknown);
        assert_eq!(level, Level::Warn);
        assert!(!fails);
    }

    #[test]
    fn an_ok_agent_is_ok_whatever_the_roster_says() {
        assert_eq!(
            line_level(None, "codex", QuotaState::Ok),
            (Level::Ok, false)
        );
        let config = config(None, &["codex/gpt-6"]);
        assert_eq!(
            line_level(Some(&config), "codex", QuotaState::Ok),
            (Level::Ok, false)
        );
    }

    #[test]
    fn a_bare_agent_name_a_worker_uses_is_still_in_a_role() {
        let config = config(Some("claude/opus"), &["kilo"]);
        let (level, fails) = line_level(Some(&config), "kilo", QuotaState::Dry);
        assert_eq!(level, Level::Fail);
        assert_eq!(level.glyph(), '✗');
        assert!(fails, "a dry agent a bare-name role dispatches must fail");
    }

    #[test]
    fn a_free_pin_is_taken_from_kilos_own_config_when_shprd_names_none() {
        let home = home_with_kilo(Some(
            "{\n  // picked by hand\n  \"model\": \"ohmyproxy/kilo-auto/free\",\n}\n",
        ));
        assert!(free_pin(None, home.path()));

        let paid = home_with_kilo(Some("{\"model\": \"kilo/openai/gpt-5\"}"));
        assert!(!free_pin(None, paid.path()));
    }

    #[test]
    fn a_model_merely_named_free_is_not_the_free_tier() {
        let paid = config(None, &["kilo/vendor/free-lunch"]);
        assert!(!configured_free_pin(Some(&paid)));
        let free = config(None, &["kilo/kilo/kilo-auto/free"]);
        assert!(configured_free_pin(Some(&free)));
    }

    #[test]
    fn a_configured_free_pin_wins_over_kilos_own_model() {
        let home = home_with_kilo(Some("{\"model\": \"kilo/openai/gpt-5\"}"));
        let config = config(None, &["kilo/kilo-auto/free"]);
        assert!(free_pin(Some(&config), home.path()));
    }

    /// A temp home, holding a kilo jsonc with `body` when one is given.
    fn home_with_kilo(body: Option<&str>) -> TempDir {
        let home = match TempDir::new() {
            Ok(home) => home,
            Err(error) => panic!("temp home failed: {error}"),
        };
        if let Some(body) = body {
            let dir = home.path().join(".config").join("kilo");
            assert!(std::fs::create_dir_all(&dir).is_ok());
            assert!(std::fs::write(dir.join("kilo.jsonc"), body).is_ok());
        }
        home
    }
}

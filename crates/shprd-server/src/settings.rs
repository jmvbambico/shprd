//! The settings routes: read every key with the layer it came from, write one.
//!
//! The service serves one repository, so a request needs no repository of its
//! own: the repository is this service's, and the person's settings are the ones
//! in their home directory. `GET /api/settings` answers
//! [`shprd_core::settings::Resolved`] as [`SettingsDto`]; `PUT /api/settings`
//! writes one key — the person's default, or `for_repo` this repository's entry
//! — and answers the settings as they now resolve.
//!
//! A write is checked exactly as `shprd settings set` checks it: the value is
//! checked against its key, the roles against the harness table, and a refused
//! write leaves the file alone. Only the refusals a write *introduces* count, so
//! a file someone else left broken does not block every other key. Reading never
//! refuses: a hand-written pair that would not work comes back in `problems`,
//! because the page has to be able to show the broken line.
//!
//! A role is never stored unchecked: without a harness table, `PUT` of a role
//! key is refused rather than accepted silently, and a read says in
//! `not_checked` that this service could not check the roles at all.

use axum::body::Bytes;
use shprd_agents::roles::{Checked, Checker, Review};
use shprd_core::settings::{Key, Loaded, Resolved, Scope, Value};

use crate::api::AppState;
use crate::dto::{SettingsDto, SettingsPutDto, SettingsWriteDto};
use crate::error::ApiReject;

/// What one resolution needs: the person's home and this service's repository.
#[derive(Debug, Clone, Copy)]
struct Place<'a> {
    /// The home directory the person's files live in.
    home: &'a std::path::Path,
    /// This service's repository.
    repo: &'a std::path::Path,
}

impl<'a> Place<'a> {
    /// The place a request reads and writes.
    fn of(state: &'a AppState) -> Result<Place<'a>, ApiReject> {
        let home = state.home.as_deref().ok_or_else(|| {
            ApiReject::internal(
                "this service has no home directory, so it cannot read the settings: \
                 set SHPRD_HOME or HOME",
            )
        })?;
        Ok(Place {
            home,
            repo: &state.repo,
        })
    }

    /// The settings files, read.
    fn load(&self) -> Result<Loaded, ApiReject> {
        Loaded::read(self.home, Some(self.repo)).map_err(read_failure)
    }
}

/// `GET /api/settings`: every key, its value and the layer it came from.
pub(crate) fn read(state: &AppState) -> Result<SettingsDto, ApiReject> {
    let place = Place::of(state)?;
    let loaded = place.load()?;
    Ok(document(state, &loaded, loaded.resolve(None)))
}

/// `PUT /api/settings`: write one key, and answer the settings as they now are.
pub(crate) fn write(state: &AppState, body: &Bytes) -> Result<SettingsWriteDto, ApiReject> {
    let place = Place::of(state)?;
    let request: SettingsPutDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a settings write: {err}; send {{\"key\": \"tasks.merge\", \"value\": \"checks\"}}"
        ))
    })?;
    let key = Key::parse(&request.key)
        .map_err(|err| ApiReject::bad_request(format!("`{}`: {err}", request.key)))?;
    let value = match &request.value {
        Some(json) => Some(Value::from_json(json).ok_or_else(|| {
            ApiReject::bad_request(format!(
                "`{}`: a value is a string, a number, a boolean or a list of strings",
                key.name()
            ))
        })?),
        None => None,
    };
    if let Some(value) = &value {
        key.check(value)
            .map_err(|reason| ApiReject::bad_request(format!("`{}` {reason}", key.name())))?;
    }
    let loaded = place.load()?;
    let scope = match request.for_repo {
        true => Scope::Repo(loaded.repo.clone().ok_or_else(|| {
            ApiReject::internal("this service's repository has no name, so it has no entry")
        })?),
        false => Scope::You,
    };
    let checker = checker(state);
    // A role is only stored once the harness table has been asked: storing an
    // unchecked one would let a same-vendor reviewer in with no warning at all.
    if key.decides_a_review()
        && let Some(reason) = checker.reason()
    {
        return Err(ApiReject::refused(format!(
            "`{}` cannot be stored: {reason}. Nothing was written",
            key.name()
        )));
    }
    // Check the settings the write would leave behind before it leaves
    // anything: a refused write never reaches the disk. A refusal that is
    // already in the file is not this write's to answer, so only the new ones
    // count.
    let before = check(&loaded.resolve(None), &checker);
    let after = check(
        &loaded.preview(&scope, &key, value.as_ref(), None),
        &checker,
    );
    let introduced = after.introduced(&before);
    if !introduced.is_empty() {
        return Err(ApiReject::refused(format!(
            "`{}` would be refused: {}",
            key.name(),
            introduced.join("; ")
        )));
    }
    let written = loaded
        .write(place.home, &scope, &key, value, None)
        .map_err(read_failure)?;
    Ok(SettingsWriteDto {
        outcome: written.outcome.into(),
        settings: document(state, &loaded, written.resolved),
    })
}

/// The DTO one resolution becomes.
fn document(state: &AppState, loaded: &Loaded, resolved: Resolved) -> SettingsDto {
    let checked = check(&resolved, &checker(state));
    let not_checked = checked
        .skipped()
        .filter(|_| resolved.names_a_pick())
        .map(str::to_owned);
    SettingsDto::from_resolved(
        &resolved,
        loaded.repo.as_deref(),
        checked.messages().to_vec(),
        not_checked,
    )
}

/// The harness table this service was given, or why it has none.
fn checker(state: &AppState) -> Checker {
    Checker::load(state.data.as_ref())
}

/// The refusals the harness table has for these settings.
fn check(resolved: &Resolved, checker: &Checker) -> Checked {
    checker.check(resolved, Review::default())
}

/// The refusal a broken settings file produces.
///
/// It is the machine's file, but the message is the page's to show: a settings
/// page that silently drew defaults over a broken file would invite the person
/// to write over the file they wanted to fix.
fn read_failure(error: shprd_core::settings::SettingsError) -> ApiReject {
    ApiReject::refused(error.to_string())
}

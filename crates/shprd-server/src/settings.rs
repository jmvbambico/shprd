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
//! write leaves the file alone. Reading never refuses: a hand-written pair that
//! would not work comes back in `problems`, because the page has to be able to
//! show the broken line.

use axum::body::Bytes;
use shprd_agents::roles::{Problems, Review, Table};
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
    // Check the settings the write would leave behind before it leaves
    // anything: a refused write never reaches the disk.
    let candidate = loaded.preview(&scope, &key, value.as_ref(), None);
    let problems = check(state, &candidate);
    if !problems.is_empty() {
        return Err(ApiReject::refused(format!(
            "`{}` would be refused: {problems}",
            key.name()
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
    let problems = check(state, &resolved);
    SettingsDto::from_resolved(
        &resolved,
        loaded.repo.as_deref(),
        problems.messages().to_vec(),
    )
}

/// The refusals the harness table has for these settings.
///
/// Empty when the settings check out, or when this service was not given a data
/// directory: a machine without a harness table can still read and write the
/// workflow keys.
fn check(state: &AppState, resolved: &Resolved) -> Problems {
    let Some(table) = state.data.as_deref().and_then(|dir| Table::load(dir).ok()) else {
        return Problems::default();
    };
    match shprd_agents::roles::check(resolved, Review::default(), &table) {
        Ok(()) => Problems::default(),
        Err(problems) => problems,
    }
}

/// The refusal a broken settings file produces.
///
/// It is the machine's file, but the message is the page's to show: a settings
/// page that silently drew defaults over a broken file would invite the person
/// to write over the file they wanted to fix.
fn read_failure(error: shprd_core::settings::SettingsError) -> ApiReject {
    ApiReject::refused(error.to_string())
}

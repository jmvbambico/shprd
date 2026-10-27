//! The rules every verb that starts an agent CLI shares.
//!
//! Only two verbs launch an agent: `up` (the orchestrator) and `spawn` (a
//! task). They build their argv from the same [`LaunchSpec`], so the decisions
//! that are not verb-specific live here and are made once.

use shprd_roster::spec::LaunchSpec;

/// The model-argument template to render, empty when no model is chosen.
///
/// A spec's `model_args` use the `{model}` placeholder, so rendering them for
/// an agent line that names no model fails on a placeholder nobody filled.
/// An orchestrator configured as plain `sh` is the normal case: it has no
/// `/model` segment and its CLI needs no model flag at all.
pub fn model_args<'a>(launch: &'a LaunchSpec, model: Option<&str>) -> &'a [String] {
    if model.is_some() {
        &launch.model_args
    } else {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_roster::spec::LaunchSpec;

    fn launch() -> LaunchSpec {
        LaunchSpec {
            interactive: vec!["claude".to_string()],
            model_args: vec!["--model".to_string(), "{model}".to_string()],
            effort_args: None,
            auto_approve: None,
            system_prompt_args: None,
            resume_args: None,
            verified: true,
        }
    }

    #[test]
    fn a_model_renders_its_flags() {
        let launch = launch();
        let args = model_args(&launch, Some("claude-opus-5-5"));
        assert_eq!(args, ["--model".to_string(), "{model}".to_string()]);
    }

    #[test]
    fn no_model_renders_nothing() {
        let launch = launch();
        assert!(model_args(&launch, None).is_empty());
    }
}

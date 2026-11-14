//! The snapshot projector: the fold in, the board's view out.
//!
//! It is a pure function of [`RunState`] — no I/O, no clock — so it is tested
//! with a folded log and nothing else. `GET /api/state` folds the log with the
//! same [`RunState::fold`] the CLI uses and hands the result here.

use shprd_core::state::RunState;

use crate::dto::{
    AgentDto, AskDto, AttemptDto, FindingDto, GateDto, NeedDto, ReportDto, ReviewDto, RunDto,
    SnapshotDto, TaskDto, TaskProgressDto,
};

/// Folds a run into the snapshot a client draws.
pub fn snapshot(state: &RunState) -> SnapshotDto {
    SnapshotDto {
        seq: state.last_seq,
        run: state.run.as_ref().map(|run| RunDto {
            id: run.run_id.clone(),
            repo: run.repo.clone(),
            base_branch: run.base_branch.clone(),
            integration_branch: run.integration_branch.clone(),
        }),
        tasks: state.tasks.values().map(task).collect(),
        agents: state
            .agents
            .iter()
            .map(|(id, info)| AgentDto {
                id: id.clone(),
                dry: info.dry,
                dry_reason: info.dry_reason.clone(),
                dry_until_ms: info.dry_until_ms,
            })
            .collect(),
        needs: state
            .escalations
            .iter()
            .map(|escalation| NeedDto {
                task: escalation.task.clone(),
                body: escalation.body.clone(),
                default_action: escalation.default_action.clone(),
                seq: escalation.seq,
            })
            .collect(),
        asks: open_asks(state),
        finished: state.finished,
    }
}

/// Every ask the fold has not seen an answer for, oldest first.
fn open_asks(state: &RunState) -> Vec<AskDto> {
    let mut asks: Vec<&shprd_core::state::AskRecord> = state
        .asks
        .values()
        .filter(|ask| ask.answer.is_none())
        .collect();
    asks.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.task.cmp(&right.task))
    });
    asks.into_iter().map(ask).collect()
}

/// One ask as a card draws it.
pub(crate) fn ask(record: &shprd_core::state::AskRecord) -> AskDto {
    AskDto {
        id: record.id.clone(),
        task: record.task.clone(),
        kind: record.kind.into(),
        text: record.text.clone(),
        options: record.options.clone(),
        created_at_ms: record.created_at_ms,
        expires_at_ms: record.expires_at_ms,
    }
}

/// One task, with its attempts in the order they were made.
pub(crate) fn task(task: &shprd_core::state::Task) -> TaskDto {
    TaskDto {
        id: task.id.clone(),
        title: task.title.clone(),
        status: task.status.into(),
        scope: task.scope.clone(),
        acceptance: task.acceptance.clone(),
        tags: task.tags.clone(),
        skills: task.skills.clone(),
        deps: task.deps.clone(),
        dropped_reason: task.dropped_reason.clone(),
        last_status: task.last_status.as_ref().map(|status| TaskProgressDto {
            message: status.message.clone(),
            progress: status.progress,
            seq: status.seq,
        }),
        attempts: task.attempts.iter().map(attempt).collect(),
    }
}

/// One attempt, with its report, gates and reviews.
fn attempt(attempt: &shprd_core::state::Attempt) -> AttemptDto {
    AttemptDto {
        n: attempt.n,
        agent: attempt.agent.clone(),
        model: attempt.model.clone(),
        worktree: attempt.worktree.clone(),
        branch: attempt.branch.clone(),
        report: attempt.report.as_ref().map(report),
        gates: attempt
            .gates
            .iter()
            .map(|gate| GateDto {
                gate: gate.gate.clone(),
                passed: gate.passed,
                findings: gate
                    .findings
                    .iter()
                    .map(|finding| FindingDto {
                        severity: finding.severity.into(),
                        title: finding.title.clone(),
                        location: finding.location.clone(),
                        detail: finding.detail.clone(),
                    })
                    .collect(),
                proven: gate.source.is_some(),
            })
            .collect(),
        reviews: attempt
            .reviews
            .iter()
            .map(|review| ReviewDto {
                reviewer: review.reviewer.clone(),
                degraded: review.degraded,
            })
            .collect(),
    }
}

/// What an agent reported about an attempt.
fn report(report: &shprd_core::state::Report) -> ReportDto {
    ReportDto {
        status: report.status.into(),
        summary: report.summary.clone(),
        tests: report.tests.clone(),
        changed_paths: report.changed_paths.clone(),
        commit: report.commit.clone(),
        skills_used: report.skills_used.clone(),
    }
}

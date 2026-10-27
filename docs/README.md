# shprd docs

The working wiki for shprd. Every page describes the **design as decided**.
Where something is built, the page says so; until then, read it as the spec.

## Start here

| Page | Read it for |
|---|---|
| [Architecture](architecture.md) | how the engine, the event log, the host adapters and the views fit together |
| [Run lifecycle](workflow.md) | what happens from `shprd up` to a landed PR |
| [Decisions](decisions.md) | what was decided, when, and why; check here before re-opening a question |

## Reference

| Page | Covers |
|---|---|
| [Roles and tabs](roles.md) | orchestrator, workers, reviewer; symbols and tab names |
| [Agents](agents.md) | supported CLIs, detection, conversation files, quota signals |
| [Availability](availability.md) | the watchdog, "dry" detection and hand-off |
| [Skills](skills.md) | global and CLI-native skills, routing, curation |
| [Self-governance](governance.md) | ownership, namespaces, archive-before-delete, approval tiers |
| [Multiplexers](multiplexers.md) | herdr and tuios: layout, launch, keys |
| [Remote access](remote.md) | the relay, the invite QR, tuios-web |
| [Commands](commands.md) | human verbs and agent verbs |
| [Licensing](licensing.md) | shprd's license, credits, rules for borrowed code |
| [Open questions](open-questions.md) | things still unverified |
| [Glossary](glossary.md) | words with a specific meaning here |

## Conventions for this wiki

- One topic per page, named in lowercase with hyphens.
- Link pages relatively (`[Skills](skills.md)`) so they work on GitHub and locally.
- When a decision changes, update the page **and** add a dated entry to
  [Decisions](decisions.md). Pages say what is true; the decision log says how
  we got there.
- Mark anything not yet verified on a real machine as *unverified* and list it in
  [Open questions](open-questions.md).

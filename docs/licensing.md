# Licensing

shprd is [MIT](../LICENSE).

## Projects shprd uses

| Project | License | How shprd uses it | Obligation |
|---|---|---|---|
| herdr | Apache-2.0 | runs inside it; calls its CLI and socket | none (separate program) |
| tuios | MIT | runs inside it; calls its CLI and socket | none (separate program) |
| herdr-dagr | Apache-2.0 OR MIT | runs `dagr view` and `dagr check`; writes its run-file format | none (separate program) |
| herdr-mobile-relay | AGPL-3.0-or-later | installs from upstream on request; invokes its actions | none, as long as shprd never bundles, vendors or patches it |
| herdr-cadence | MIT | design reference; may borrow code | keep notice for any borrowed code |
| herdr-projects | MIT | per-CLI knowledge and gate rules; may borrow code | keep notice for any borrowed code |
| skills (`npx skills`) | MIT | reads the skills folder it manages | none |

Credits for each are in the README.

## Rules for borrowed code

1. Borrow only from MIT projects, or from Apache-2.0 OR MIT projects under
   their MIT option, so everything shprd ships stays MIT.
2. Put a header in the file: where it came from (repo, path, commit) and its
   license.
3. Add the original copyright line and license to `THIRD_PARTY_NOTICES.md` in
   the same commit.
4. Never copy code from herdr-mobile-relay (AGPL) or herdr (Apache-2.0 only)
   into shprd.
5. Never ship a binary of another project in a shprd release.

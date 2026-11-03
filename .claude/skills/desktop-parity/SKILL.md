---
name: desktop-parity
description: Catch Vernam up with Signal Desktop. Reads the Desktop commit recorded in .signal-desktop-hash, lists every Desktop commit since, sorts out the ones that change behavior Vernam has or should have, implements them, reviews, commits, and moves the hash forward. Use when asked to sync with Signal Desktop, check for parity, "what's new in Desktop", or update .signal-desktop-hash.
---

# Desktop parity

Vernam follows Signal Desktop (AGENTS.md, "Signal Desktop is the reference"). `.signal-desktop-hash` at the repository root holds the full hash of the last commit on Desktop's `main` that Vernam has been checked against: every Desktop commit up to and including it has either been carried over or judged not to apply. This skill moves it forward.

## 1. Get Desktop's history

Clone into the scratchpad, never into the repository:

```sh
BASE=$(cat .signal-desktop-hash)
git clone --filter=blob:none --single-branch https://github.com/signalapp/Signal-Desktop.git "$SCRATCH/sd"
git -C "$SCRATCH/sd" cat-file -e "$BASE^{commit}"   # must exist; if not, stop and ask
git -C "$SCRATCH/sd" log --reverse --format='%H %ad %s' --date=short "$BASE..origin/main"
```

`--filter=blob:none` keeps the clone small and still lets `git show` fetch each diff. If the range is empty, report that and stop.

## 2. Triage every commit

Make a ledger, one row per commit: hash, subject, verdict, reason. Every commit gets a row, so nothing gets skipped without a reason. Verdicts:

- **skip**: the commit doesn't reach behavior Vernam has or could have. That covers build, CI, lint, dependency bumps (except libsignal and RingRTC, see below), Storybook, mock-server-only changes, tests only, string updates, React or dialog migrations with no change in behavior, and "Desktop Primary:" features, since Vernam is only ever a linked device.
- **already**: Vernam already does what the commit makes Desktop do. Cite the Vernam file and function that shows it.
- **do**: a protocol, sync, storage, backup, receipt, attachment, notification, remote-config or user-visible behavior change that Vernam lacks. That includes new features behind remote-config flags.
- **ask**: the commit changes the design or scope, or it reverses something Vernam deliberately does differently.

Judge from the diff (`git show <hash>`), not the subject line. A subject like "Fix X" often changes wire behavior. Look closely at changes to:

- `protos/*.proto` (Backups, SignalStorage, SignalService), even renames: field numbers are what matter. New Storage Service record types also mean checking that Vernam keeps identifiers it doesn't know when it rewrites the manifest.
- `ts/RemoteConfig.dom.ts`: a new key means a new flag, and its row belongs in `todo.md`'s remote-config table.
- `ts/messageModifiers/*`, `ts/util/handle*`, `ts/background.preload.ts` (receive paths), `ts/textsecure/SendMessage*` (urgent flags, sync messages), `ts/util/getCapabilities*`, `ts/services/storage*`, `ts/services/backups/*`.
- libsignal or RingRTC bumps: check whether presage or the vendored crates need the same bump, and record what you find even if nothing needs doing.

For each "already" or "do" verdict, search Vernam (`crates/`, `vendor/`) for the area before deciding. The first check is often wrong (a read sync is skipped in one place but a delete sync isn't in another), so look at every path the Desktop change touches.

Show the user the ledger's "do" and "ask" rows before starting large work. "skip" and "already" rows go in the final report, one line each.

## 3. Implement

Follow AGENTS.md throughout: cite the Desktop file in comments, demo runs always with `SIGNAL_GPUI_DATA_DIR`, no `cargo fmt`, database rules, and tests for each change. Independent features that each span backend, store and UI can go to parallel subagents in separate worktrees (`isolation: "worktree"`, each committing on its own branch). Give each one the Desktop clone's path, the commit hashes and the Desktop files to read. Merge their branches afterwards. Small fixes are quicker done inline.

## 4. Review and commit

Run AGENTS.md's review loop over the whole change (fresh `general-purpose` subagent each round, `/code-review high --max-findings default`, plus `/security-review` in round 1 when untrusted input, storage or crypto is touched; at most five rounds; keep a ledger of findings). Then split into atomic commits, one per Desktop change carried over, each building on its own (build in a separate `git worktree` with its own `CARGO_TARGET_DIR`).

## 5. Move the hash

As the last commit, write the newest Desktop hash you triaged (the last line of step 1's log, the full 40 characters, with a trailing newline) to `.signal-desktop-hash`. Commit it with a message that names the Desktop version range, e.g. "Check parity with Signal Desktop through v8.36.0-alpha.1". Only move it past commits that are done, already handled, or skipped. If an "ask" or an unfinished "do" remains, set the hash to the commit just before the first such one, and list the rest in the report so the next run picks them up.

## Report

- The Desktop range checked (old hash, new hash, versions) and the number of commits.
- What was carried over, with Vernam commit hashes.
- "already" rows, each with the Vernam location that shows it.
- "ask" items and anything left undone.
- Tests run and their results.

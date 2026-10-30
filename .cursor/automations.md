# Cursor automations

These are the Cursor automations of this repository. Cursor does not read this file: each
automation lives in the dashboard (cursor.com/automations). Keep this file and the dashboard the
same. When you change a prompt, change it here in a PR, then paste it into the dashboard.

Every automation uses the repository `JudahZF/tetrad` and the environment in
`.cursor/environment.json`. `.cursor/install.sh` sets up the VM as CI does.

## 1. Changelog and docs after a merge

- **Trigger:** Pull request merged, into `main`. This trigger also fires for PRs from forks.
- **Tools:** Pull request creation.

```text
A pull request was just merged into main in JudahZF/tetrad. Check that pull request only.

Read .cursor/BUGBOT.md and docs/development.md first. Write in Simplified Technical English:
short sentences, one term for one thing, plain words.

1. CHANGELOG.md. A change that a user can see needs an entry under "## Unreleased", in the
   subsection Added, Changed or Fixed. A user can see a change to behaviour on the unit, the
   Remote page, the extension API or SDK, the install, launcher and update scripts, or a
   notable speed-up. Refactors, tests, CI and internal docs need no entry. Write the entry as
   the other entries are written: what the user sees, with a link to the docs page. For a PR
   from an outside contributor, end the entry with "(#<number>, thanks <name>)", as the other
   entries do.
2. docs/. The page that describes the changed behaviour must agree with the code on main.

If the changelog and the docs are correct, stop. Make no branch, PR or comment.

If something is missing or wrong, look for an open PR that already fixes it. If there is one,
stop. Else make a branch "chore/changelog-pr-<number>" from main, make the smallest fix and
change no code. Open a PR with the title "docs: changelog and docs for #<number>". Its body
lists each change in one line and links the merged PR.
```

## 2. Format and rustdoc fixes on pull requests

- **Trigger:** CI completed, on pull requests. The trigger has no filter on the result, so the
  prompt stops at once when the check passed. GitHub may not start it for PRs from forks.
- **Tools:** Comment on pull request.

```text
A GitHub check finished on a pull request in JudahZF/tetrad.

Do the work below only when all of these are true. Else stop at once, with no change and no
comment.
- The check is the "host" job of the "ci" workflow (.github/workflows/ci.yml), and it failed.
- The failed step is "Format" or "Rustdoc". The steps stop at the first failure, so Build, Test
  and Clippy passed. A person fixes every other failure.
- The head of the PR is still the commit that failed.
- The last commit on the PR is not a fix from this automation. This prevents a loop.

1. Check out the head branch of the PR.
2. Run `cargo fmt --all`.
3. Run `RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps`. Fix each warning
   in the doc comments only: broken intra-doc links, unescaped brackets or angle brackets, bare
   URLs. The Rustdoc step did not run when Format failed, so check it in both cases.
4. Change nothing other than formatting and doc comments. If a rustdoc warning needs a change
   to code, do not commit. Post one comment on the PR with the warning and the file and line.
5. Run `cargo fmt --all -- --check` and the rustdoc command again. Both must pass.
6. Make one commit: "style: cargo fmt" for formatting only, "docs: fix rustdoc warnings" for
   doc comments only, or "style: cargo fmt and rustdoc fixes" for both. Push it to the head
   branch of the PR. Never force-push.
7. If the push fails (the head moved, or no write access to a fork), post one comment on the PR.
   Give the commands to run (`cargo fmt --all`, the rustdoc command) and the warnings that you
   fixed.
```

## 3. Weekly release check

- **Trigger:** Schedule, weekly, Monday morning.
- **Tools:** Pull request creation.

```text
Do a release check of main in JudahZF/tetrad. Do not bump versions, make tags or publish
anything.

Read .cursor/BUGBOT.md and docs/development.md first. Write in Simplified Technical English.

The range is from the latest tag (`git describe --tags --abbrev=0`) to main.

1. Changelog. List the PRs merged in the range. Each change that a user can see needs an entry
   under "## Unreleased" in CHANGELOG.md (see the rules in the entries that are there). For a
   PR from an outside contributor, end the entry with "(#<number>, thanks <name>)".
2. Docs. For each area that changed in the range, the page in docs/ must agree with the code.
   README.md "What works" must agree too.
3. README images. If the range changed crates/tetrad-ui, crates/tetrad-theme or
   crates/tetrad-assets, run
   `cargo run --locked -p tetrad-ui --example render_owned -- readme docs/images`. Keep the
   images that changed.
4. Tests. Run `TETRAD_EXTENSIONS_DIR=$HOME/tetrad-extensions cargo test --locked --workspace`.

If there is something to fix, look for open PRs that already fix it and skip those items. Put
the other fixes on one branch "chore/release-check-<YYYY-MM-DD>" from main and open one PR. Its
body is the report.

The report has these parts: the PRs in the range, what you fixed, what a person must check
(for example a docs page that you could not confirm), and the test result. If there is nothing
to fix, make no PR: give the report as your last message.
```

## 4. Daily issue triage

- **Trigger:** Schedule, daily. GitHub has no "issue opened" trigger.
- **Tools:** none. The agent posts with `gh`.
- **Secret:** `GH_TOKEN`, a fine-grained token for `JudahZF/tetrad` with Issues read and write
  only. Add it in the Secrets tab of cursor.com/dashboard/cloud-agents. Comments post as the
  owner of the token.

```text
Triage the open issues of JudahZF/tetrad. Use `gh` (GH_TOKEN is set).

Read .cursor/BUGBOT.md, docs/architecture.md and docs/support.md first. Write in Simplified
Technical English, in a friendly voice.

Select the open issues (not PRs) that were opened or got a comment from a person in the last
25 hours. Skip an issue when its last comment has the marker "<!-- cursor-triage -->".

For each issue:
1. Read the issue and its comments. If there is a support file (tetrad-support-*.zip), download
   it and read the versions, the loaded preset's blocks and the logs.
2. Find the probable cause in the code on main. Give the files and lines as links. If a host
   test can show the bug quickly, write one and run it, but do not commit or push it.
3. Find duplicates and related issues and PRs.
4. Post one comment with these parts:
   - The first line: "Automated triage by a Cursor agent. A maintainer will check it."
   - What the issue is, in one or two sentences.
   - The probable cause, with links, and how sure you are. If you are not sure, say so.
   - What information is missing. If a bug report has no support file, ask for one and link
     docs/support.md.
   - Related issues and PRs.
   - The last line: "<!-- cursor-triage -->".
5. Add labels from the labels that the repository has, for example bug, enhancement,
   documentation, question or duplicate. Do not make new labels.

Never close, lock, assign or edit an issue. Never open a PR. Never change a label that a person
set. If there is no issue to triage, stop.
```

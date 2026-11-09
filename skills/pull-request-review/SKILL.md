---
name: pull-request-review
description: "Review a pull request, branch, or local diff for correctness, security, tests, performance, maintainability, and reuse of existing code, reporting findings with severity and file:line evidence. Use when asked to review a PR, review a branch or diff, do a code review, check a change before merge, or post review comments on a PR."
---

# Pull Request Review

Review the change against its stated intent and against how the surrounding code actually behaves. A finding is only worth reporting if you can name the inputs or state that produce the wrong outcome.

This is a reviewer's workflow, not a bug-hunting expedition. It complements the built-in `/code-review` (deep, multi-agent correctness hunting at an effort level); use that when the ask is "find everything wrong in this code", and this when the ask is "review this change".

Two rules shape everything below:

- **Discovery is separate from severity.** Find first, rank later. A finding judged minor while you are still looking is one you stop investigating.
- **Absence of evidence is not evidence.** "Clean" is a claim that needs a record of what was examined.

## Operating modes

- **Review mode** (default): read and report. Do not edit files. Do not push, comment on the PR, or run anything that mutates shared state unless asked.
- **Fix mode**: only when the user asks to fix as well (`review and fix`, `--fix`, "apply them"). Report findings first, then apply the smallest change that resolves each one, then re-run the tests you used as evidence.
- **Post mode**: only when the user asks to post (`post the review`, `comment on the PR`, `leave comments`). Complete Review mode first — nothing is posted that was not first reported to the user. The user chooses, comment by comment, which ones are posted. See Posting review comments.

## Step 1 — Resolve the target

Establish exactly what commits you are reviewing and state it back before analysing.

| Target | Commands |
|---|---|
| PR number | `gh pr view <n> --json title,body,baseRefName,headRefName,commits,additions,deletions` then `gh pr diff <n>`; `gh pr checkout <n>` if you need the tests to run |
| Branch | `git fetch origin` then `git diff --stat "$(git merge-base HEAD origin/main)"...HEAD` |
| Local work | `git diff` (unstaged) or `git diff --staged` |
| Path or directory | same as branch, filtered by path |

Record the base and head SHAs in the report. A review of a stale merge-base is a review of code that no longer exists.

**Read the description and linked issue before the code.** You cannot judge whether a change does what it claims until you know what it claims. Note any claim the diff does not support.

## Step 2 — Understand before you critique

1. **Shape of the change**: files touched, layers crossed (entry point → logic → storage → response), new public API, changed invariants, migrations or schema.
2. **Read the hunks in their file**, not in isolation. The bug is usually in the interaction between the diff and the code five lines above it.
3. **Read the callers** of every changed signature. Changing a function's semantics breaks callers you never see in the diff.
4. **Flag scope creep**: unrelated refactors, formatting churn, or a fix bundled with a feature. They make review harder and regressions harder to bisect.
5. **Check the order of work**: does the change land in a single coherent commit sequence, or are there "fix the last commit" commits that should be squashed?

For a large diff, read `--stat` first, then rank files by risk (auth, crypto, parsing, concurrency, migrations, anything that deletes) and read those fully. Skim the rest.

## Step 3 — Discover, recording evidence for every dimension

Discovery produces discoveries and evidence, never priorities. Do not use words like "critical", "minor", or "probably fine" yet — that is Step 6's job.

### The evidence log

Every dimension you examine gets a row, whether or not it produced a finding. The row must show what was examined and by what method, so a reader can tell the difference between "checked and clean" and "not checked":

| ID | Dimension | Examined | Method | Result |
|---|---|---|---|---|
| C1 | Correctness | `src/retry.cpp:40-96` | branch-by-branch walk; partial-failure state | discovery D1 |
| S1 | Security | `src/handler.cpp:12-70` | trust boundaries; authorization at each operation | clean |
| T1 | Tests | `tests/retry_test.cpp` | revert the change in a scratch copy; re-run | gap, D2 |
| P1 | Performance | `src/retry.cpp:70-88` | allocation per retry; batch growth | clean |
| R1 | Reuse | new `trim()` in `src/util.cpp` | name search, concept search, stdlib, dependencies | duplicate, D3 |
| M1 | Maintainability | whole diff | naming, dead code, duplication | clean |

- **A dimension with no row is unexamined, not clean.** A diff that does not touch performance gets a row whose Result says "not applicable" and whose Method says why.
- **"Clean" must be reachable from the Method cell.** "Looked at it" is not a method.
- **Record as you go.** Evidence reconstructed after the summary is written is fiction.

### Correctness

- **Every branch**: error returns propagated, early exits correct, partial failures handled. What is the state if step N of M fails?
- **Boundaries**: empty collection, single element, maximum size, zero, negative, null/optional, empty string.
- **Resource lifetime**: files, sockets, locks, transactions, and child processes released on every path, including exceptions and early returns.
- **Concurrency**: shared mutable state, lock ordering, a lock held across I/O, check-then-act races, atomicity of multi-step updates, what a reader sees during a partial write.
- **Arithmetic and types**: overflow, division by zero, signed/unsigned conversion, float equality, truncation.
- **Encoding**: length in bytes vs characters, slicing mid-codepoint, normalisation, locale-dependent casing.
- **Time**: wall clock vs monotonic, time zones, DST, leap seconds, timeouts measured from the wrong instant.

### Security

- **Trust boundaries**: every input crossing one — network body and headers, query parameters, filenames, environment, config, database content written by users, federation messages.
- **Authorization at the operation**, not merely at the route: does this specific action check that this principal may perform it?
- **Injection**: SQL, shell, path traversal, template, format string, log forging, header injection.
- **Secrets**: not logged, not in error text, not in test fixtures or example configs, not compared without constant-time.
- **Crypto**: no hand-rolled primitives, no custom padding, correct nonce/key lifecycle, authenticated where it must be.
- **Defaults**: fail closed. An unknown value should deny, not permit.
- **DoS shape**: any user-controlled value that sizes an allocation, loop, or retry.

### Tests

The single most valuable question: **would this test fail if the production change were reverted?**

- New behaviour covered on the happy path, on each error path the diff introduces, and at the boundaries it touches.
- Assertions check behaviour, not implementation details that will churn.
- No sleeps, no dependence on ordering, no shared mutable fixture, no test that only passes when run alone.
- A regression test that would still pass with the bug reintroduced is not a regression test.
- **Run them and read the summary yourself.** "CI is green" is not evidence you observed. A timeout is a failure, not a pass, and a process that aborts at startup proves nothing by emitting no errors.

### Performance

- Complexity of the hot path; work that moved inside a loop; allocation per item.
- Unbounded growth: caches, queues, retained buffers, anything sized by a user-controlled value.
- I/O per item (N+1), per-request connection setup, a lock held while waiting on the network.

### Maintainability

- Names describe purpose, not mechanism.
- No dead code, unused parameters or fields, commented-out blocks, or debug prints.
- Duplication that will drift: two copies of a rule that must stay in sync.
- Comments explain *why*; the code explains *what*.
- Error messages are actionable, and neither leak internals nor hide the cause.
- Public API changes are deliberate and documented.

### Reuse — search before accepting a new utility

Every new helper in the diff — a trim, a path join, a retry loop, a clamp, an RAII wrapper, a small parser — is a claim that nothing equivalent exists. Test the claim:

1. **Search by name and by concept.** Search the project's utility headers and the standard library before writing it off: `rg -n 'trim|strip|whitespace' src/ include/`. Then search by the thing being done rather than the word used for it — a "normalise" helper may be the "canonicalise" one under another name.
2. **Look at the nearest module's utility header** even when its name suggests nothing.
3. **If an equivalent exists**, the new code is a duplicate. Two copies of one rule drift apart; the finding is to use the existing helper, or to replace it if the old one is wrong.
4. **If the new helper is justified** — different semantics, or the old one is buggy — say why in the review. That sentence is what stops a future reader merging the two incorrectly.
5. **Do not accept "consolidate later."** That is how the duplicate persists.

Record the searches in the evidence log's `R` rows. A reuse row that names no search is a guess.

### Behaviour → test map

New behaviour means new outcomes: a new status code, a new validation, a changed default, a new error path. For each one, name the test that asserts it:

| Behaviour | Test | Assertion | Fails if reverted? |
|---|---|---|---|
| Rejected item → only failed items retried | `tests/retry_test.cpp:88` | `REQUIRE(batch.size() == 1)` | yes |
| Malformed header → 400 | none | — | gap |

- **Map behaviour, not files or functions.** "Has tests" is not the claim; "this outcome is asserted" is.
- A test that exercises the path but asserts something else does not map.
- A test that passes with the behaviour reverted does not map — write `no`.
- **Changed behaviour must update the test that pinned the old behaviour.** An unchanged test asserting the old behaviour is a contradiction: one of the two is wrong.
- An unmapped security, authorization, or data-loss behaviour is at least a Major; an unmapped naming change is nothing.

## Step 4 — Filter discoveries to defensible findings

A discovery becomes a finding only when it contains all four:

1. **Location** — `file:line`, copied from the actual file, not from memory.
2. **Trigger** — the concrete inputs, state, or sequence that reaches the defect.
3. **Outcome** — what goes wrong, stated as an observable result.
4. **Smallest fix** — the minimal change that resolves it, not a redesign.

If you cannot name the trigger, it is a **question**, not a finding — move it to the questions list. If you are unsure whether the code is reachable, say so and label it unverified rather than dropping it.

Before claiming a function misbehaves, **verify the signature and semantics actually exist as you assume**. Do not invent APIs, and do not assume a helper is missing because you did not see it in the diff.

Do not report formatting, import order, or line length that a formatter or linter in the repository already enforces. Comment on those only when no tooling covers them.

## Step 5 — Independent coverage audit

A discovery pass that reports "I reviewed all the files" is self-certifying. Audit coverage mechanically, before any severity is assigned.

**Enumerate the hunks.** Nothing is covered until it appears in a list:

```bash
git diff --unified=0 <base>...HEAD | grep '^@@'
git diff --stat <base>...HEAD
```

**Delegate the audit to an agent that has not seen your findings.** Its brief says only:

- Enumerate every changed hunk with `file:line`.
- State in one line what each hunk does and which dimension it belongs to.
- Return every hunk with no matching evidence-log entry, and every file in `--stat` with no entry at all.

Do **not** ask it to find bugs. You want coverage, not a second opinion: a second bug hunt re-litigates the first pass and produces findings you then have to adjudicate. Do **not** give it your discovery list or severity calls — it will confirm your coverage instead of testing it.

If no independent agent is available, enumerate mechanically and diff the hunk list against the evidence log by script or by hand. Independence here means the enumeration is mechanical and the assumptions are not reused; it does not require a particular tool.

Every uncovered hunk is either examined now or recorded under Verification as not reviewed. Do not close the review with silent gaps.

## Step 6 — Judge severity, as a separate pass

Only now, with discovery complete and coverage audited. Assigning priority during discovery anchors the search and quietly drops the findings that looked small at first glance.

For each discovery, write one sentence naming impact and likelihood, then assign:

| Level | Meaning | Blocks merge? |
|---|---|---|
| **Blocker** | Wrong result, security hole, data loss, crash, or broken build/test | Yes |
| **Major** | Real defect on a plausible path, or an incomplete fix | Usually — the author should at least rule it out |
| **Minor** | Correct but fragile, unclear, or wasteful; or missing coverage | No |
| **Nit** | Preference, style the formatter does not own, wording | Never; collapse these |

Consistency checks after ranking:

- **Any finding whose outcome is security, authorization, or data loss that ranks below Major is re-examined.** The outcome and the rank disagree; one of them is wrong.
- **Any Blocker must survive "would I stop the merge for this?"** If not, it is a Major.
- **A severity with no justification sentence is a guess.** Write the sentence or drop the finding.
- Never inflate a nit to make the review look thorough, and never soften a blocker to be agreeable.

### Severity tags

Every finding carries exactly one tag for the whole of its life — in the report, in the summary, and in any posted comment:

`**[Blocker]**` · `**[Major]**` · `**[Minor]**` · `**[Nit]**`

The tag goes first, before any other text, so the author can triage a thread by scanning the left edge. Never post or report an untagged finding, and never use a synonym ("critical", "important", "suggestion") in place of the tag. Questions are not findings and carry no severity — see Posting review comments.

## Step 7 — Report

```markdown
## Summary
Verdict: <approve | approve with comments | request changes>
What the change does, and whether the diff matches the stated intent.

## Blockers
- **[Blocker]** `path/to/file.ext:123` — <one-line defect>. Trigger: <inputs/state>. Outcome: <what breaks>. Fix: <smallest change>.

## Major
- **[Major]** `path:line` — ...

## Minor and nits
- **[Minor]** `path:line` — ...
- **[Nit]** `path:line` — ...

## Behaviour → test
| Behaviour | Test | Fails if reverted? |
|---|---|---|
| <new outcome> | `<test file:line>` | yes / no / gap |

## Evidence log
| Dimension | Examined | Method | Result |
|---|---|---|---|
| <dimension> | `<file:lines>` | <method> | <clean / n findings> |

## Verification
- Reviewed base `<sha>` → head `<sha>`.
- Commands run: `<exact commands, verbatim>`.
- Tests: `<exact counts, or "not run">`.
- Not verified, and why: `<what, and the specific reason>`.

## Questions
- <Things the diff cannot answer on its own.>
```

Order findings by severity, then by file order within a severity. Keep the summary to three sentences — the author reads that first and may read nothing else.

### Verification limits

A verification statement is only useful if it says what it proves and what it does not. Never write "tests pass", "looks good", or "reviewed". Instead:

- **Name the exact commit and base.** "Reviewed head `a1b2c3d` against base `9f8e7d6`" — not "reviewed the PR".
- **Give the commands verbatim**, so the reader can rerun them.
- **Give the real summary numbers.** Ok / Fail / Timeout, from the log you actually read. A timeout is a failure.
- **Distinguish ran, passed, and skipped.** Skips tested less than the total suggests.
- **Name what could not be checked and why**: no database, no network, the sandbox blocked it, a file could not be read, the platform is unavailable. The reason matters as much as the gap.
- **State what the evidence does not prove.** "The unit suite passed" does not mean the new test ran; a filter that matched nothing proves nothing; a static review cannot rule out a defect on an untested runtime path.
- **Never round a partial check up.** "Not run" beats "presumably fine".

```markdown
- Verified: `./build/tests/unit [retry]` — 6 Ok, 0 Fail, 0 Timeout. This says nothing about the rest of the suite.
- Verified: `git diff` read in full at head `a1b2c3d` (9 files, 240 lines).
- Not verified: the PostgreSQL rollback path — no server available, so the claim remains unconfirmed.
- Not verified: behaviour under a real TLS handshake; the fixture mocks the socket.
```

## Posting review comments

Posting is outward-facing and hard to retract, and a reviewer rarely wants every comment to land. Draft everything, display it, then ask which comments to post. Never post a set the user has not explicitly chosen.

### Selection

1. **ID every comment by severity**: `B1`, `B2` for Blockers, `M1` for Major, `m1` for Minor, `N1` for Nits, `Q1` for questions.
2. **Display the drafts in full** — ID, tag, anchor, and the exact text that would be posted. Not a summary, not a count. The user is deciding what other people will read, and the wording is the thing being approved.
3. **Prompt for the selection.** If the set fits four choices, use a multi-select question: all, blockers and major only, blockers only, none. Otherwise present the numbered list and ask for IDs. Offer the default (blockers and major) as a suggestion, never as a pre-applied choice.
4. **Treat "post all" as a selection only when the user said it.** A request to post the review is not a request to post every comment. Withhold anything not chosen, including nits that seem harmless.
5. **Read the selection back before posting**: "Posting B1, B2, M1 — 3 of 9. Withholding 6." If the answer is ambiguous ("the important ones"), resolve it into IDs and confirm rather than guessing.

If the user later asks for a withheld comment, post it then — but only that one, after showing it again.

### Format

Every comment is one finding, and opens with its severity tag:

```markdown
**[Major]** The retry loop re-sends the whole batch when a single item is rate-limited, so a batch rejected once gets duplicated when the retry succeeds. Re-send only the failed items, or make the operation idempotent with a request ID.
```

- Tag first, then the claim in one sentence. Trigger and outcome belong in the comment; the `file:line` is the anchor, so do not repeat it in the text.
- Do not open with a hedge ("it might be worth considering"). State what is wrong.
- The fix comes last and is the smallest change, not a redesign.
- One finding, one comment. Do not bundle unrelated defects to save comments — they cannot be resolved independently.
- Never post a comment the author would have to come back and ask about.

### Anchoring

- Anchor to the narrowest changed range containing the defect. GitHub rejects an inline comment whose `line` is not part of the diff (HTTP 422), so anchor to a changed line and describe the unchanged neighbour in the text if needed.
- A finding about *missing* code — an absent check, a missing test — anchors to the closest changed line and says explicitly what is missing, or the author cannot see what the comment refers to.
- Do not re-post a finding already raised in the thread; reply in that thread instead.

### The review body

The summary covers what is being posted: a verdict derived from the selected comments, counts by severity of those comments, what was actually verified, and questions. Findings appear inline, each tagged. Do not allude to withheld findings or include their counts — the author cannot see them, and a count that does not match the thread reads as an error.

Questions carry no severity. Put them in the review body. If one must be inline, tag it `**[Question]**` and phrase it as a question so it is never read as a defect.

### Posting it

```bash
# Review with inline comments: comments[] each need path, line, side, body
gh api repos/{owner}/{repo}/pulls/<n>/reviews --input review.json

# Review body only, no inline comments
gh pr review <n> --comment --body-file review-body.md
```

`review.json` needs the head `commit_id`, the `event`, a `body`, and the `comments` array containing only the selected comments. `event` is `REQUEST_CHANGES` only when a selected comment is a **[Blocker]**; otherwise `COMMENT`. Read the API response: a 422 means one anchor was invalid and the whole review was rejected — confirm the returned comment count matches the selection rather than assuming the post landed.

After posting, state which IDs went out and which were withheld. The withheld ones remain findings in the report; they have not been withdrawn.

## Anti-patterns

- **Reviewing the diff alone.** Context is where the real bugs are.
- **Assigning severity while discovering.** It anchors the search and drops the findings that looked small.
- **A dimension with no evidence row**, or one whose Method is "looked at it". That is an unexamined dimension wearing a clean coat.
- **Accepting a new helper without searching for an existing one.** Duplicate utilities drift; the search is the evidence.
- **Mapping tests by file or function** instead of by behaviour. Coverage is about outcomes asserted, not files touched.
- **Self-certifying coverage.** "I reviewed every file" is a claim; the hunk enumeration is the audit.
- **Vague verification.** "Tests pass" or "not run" without saying which command, which commit, and what remains unproven.
- **Restating the diff** as findings ("adds a function that parses JSON"). Say only what is wrong or worth confirming.
- **Praise padding** to balance criticism. It dilutes the signal.
- **Design relitigation.** If the approach was chosen deliberately and the description says why, review the implementation of the decision, not the decision. Raise design only when it causes a concrete defect.
- **Speculative findings** with no trigger ("this might be slow").
- **Copying the PR description** into the summary instead of judging whether it is true.
- **Trusting a claim of green tests.** Read the output.
- **Posting an untagged comment**, or downgrading the tag to soften it once it is public. The tag in the posted comment must match the tag in the report.
- **Posting without confirmation**, or reading "post the review" as "post everything". The selection is per comment and comes from the user.
- **Posting a comment the user withheld**, including slipping a withheld nit into the review body instead of inline.

## When asked to fix

1. Report first, in the format above. The user decides what to fix.
2. Apply one minimal change per finding; do not reformat surrounding code.
3. If a fix needs a design change, stop and ask rather than expanding the diff.
4. Re-run the tests you cited as evidence and report the actual counts.
5. State what you changed and what you deliberately left alone.

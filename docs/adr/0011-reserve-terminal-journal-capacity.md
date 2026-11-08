# 0011: Reserve terminal journal capacity

- Status: Accepted
- Date: 2026-11-08

## Context

A journal can reach its event or byte quota during an active turn. Without a
reservation, completion and operator interruption cannot be recorded. A legacy
journal can already be in that state. Silently clearing the in-memory flag does
not survive reopening, and deleting history to make room loses evidence.

## Decision

Reserve one event and 1 KiB for turn termination whenever a turn starts or
continues. The failure code is bounded to 128 UTF-8 bytes without controls; its
worst JSON escaping plus the terminal envelope fits within 1 KiB. Reject normal
appends before they consume this reservation. CLI chat additionally preflights
encoded request records and a maximum-size assistant record before starting.
Persist the turn marker before the user message.

Allow a full legacy active journal to append one matching terminal record of
at most 1 KiB beyond ordinary quotas. Readers permit this bounded exception only
when the preceding valid state has an active turn within the ordinary quotas.
They reject ordinary overflow and additional recovery tails. Ordinary limits
remain 8 MiB and 10,000 events; absolute read limits are 8 MiB plus 1 KiB and
10,001 events. Full journals stay inspectable but need a new session for further
ordinary events. There is no automatic truncation or rollover.

Write or sync errors still poison the writer. Recovery never fabricates a
successful result, repeats a request, or treats uncertainty as rollback.
The [session guide](../session-storage.md) owns the full persistence contract.

## Alternatives considered

- Reject all recovery at the original quota: leaves valid legacy active turns
  permanently unable to record termination.
- Delete or rewrite earlier records: loses original evidence and introduces a
  replacement/migration path merely to recover a terminal marker.
- Put termination in a sidecar: introduces competing sources of truth and a
  multi-file commit/replay protocol for one bounded state transition.
- Permit arbitrary quota overflow: defeats the parser and storage bounds.

## Consequences

Normal turns can always reserve a terminal marker before inference. Historical
quota exhaustion has a bounded, append-only resolution. Readers must retain the
exception checks; accepting an extra ordinary record would weaken this decision.
Older prototype readers may reject recovered files. New tests cover both event
and byte exhaustion, unchanged state on deterministic refusal, sync failure
after a successful write, and replay of the terminal recovery tail.

## References

- [Initial core increment](0010-build-a-local-chat-core-before-execution.md)
- [Testing standards](../testing-standards.md)

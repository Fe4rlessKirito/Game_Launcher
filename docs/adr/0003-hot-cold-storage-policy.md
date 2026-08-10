# ADR 0003: Explicit hot/cold storage policy

## Status

Accepted

## Decision

`StorageClass` declares logical `HOT`, `COLD`, or future `ARCHIVE`, while
provider pools carry provider identity and failure-domain metadata.
`StoragePolicy` declares minimum and preferred verified replica counts plus
optional minimum failure domains. Publication checks every build chunk against
the class/pool policy. The API resolver returns only hot locations and reports a
typed restore-pending response when only cold coverage exists.

## Consequences

Provider names are not embedded in domain policy. Staging can require a cold
backup without changing the client protocol. Builds remain `READY` when a
required replica cannot be placed, which makes capacity and health failures
operator-visible.

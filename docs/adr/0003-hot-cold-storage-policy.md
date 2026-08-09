# ADR 0003: Explicit hot/cold storage policy

## Status

Accepted

## Decision

Storage providers declare `HOT` or `COLD`, while `StoragePolicy` declares
minimum and preferred verified replica counts. Publication checks every build
chunk against the policy. The API resolver returns only hot locations and
reports a typed restore-pending response when only cold coverage exists.

## Consequences

Provider names are not embedded in domain policy. Staging can require a cold
backup without changing the client protocol. Builds remain `READY` when a
required replica cannot be placed, which makes capacity and health failures
operator-visible.

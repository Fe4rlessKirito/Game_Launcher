# HOT provider contract

Every HOT provider advertises upload/delete, direct download, range support,
URL stability/expiry, refresh behavior, maximum object size, preferred pack
size, and recommended concurrency. A client-facing source must advertise
`direct_download=true`; a provider that requires private credentials is never
eligible for resolver output.

The built-in local provider exposes stable direct URLs and byte ranges. The S3
compatible provider exposes stable public URLs when configured, otherwise
short-lived presigned URLs with resolver refresh. Runtime provider URLs are
preferred over stored URLs so expired presigned locations are not reused.

FileMirage, Buzzheavier, and GoFile remain intentionally unconfigured. The
staging capability record in [provider-capability-records.md](provider-capability-records.md)
lists only behavior observed in controlled small-object probes; an upload
success alone never enables direct HOT traffic. A provider without a proven
direct-download, range, URL-refresh, and cleanup contract remains a
server-side restore source, never a client URL.

Normal bytes are served by providers, not by the API's local proxy. The proxy
routes exist only for local development fixtures and explicitly bounded
fallbacks.

HOT placement is build-scoped: the newest published build receives normal HOT
replicas and mirror scheduling. When that build supersedes an older one, HOT
objects and packs unique to the older build are retired, while shared
content-addressed bytes remain. Historical builds stay available through the
server-side COLD restore path rather than as permanent HOT mirrors.

# ADR 0002: JSON manifest v1

Status: accepted

The first manifest is versioned JSON with explicit chunking and encoding parameters. It is inspectable, easy to fixture across C#, Rust, and Python, and can be signed over canonical bytes later. Binary formats remain an optimization rather than a protocol dependency.

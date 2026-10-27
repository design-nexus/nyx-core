# Core

Libraries: application foundation, GOA accounts and EDS discovery (nyx-accounts),
EDS calendars and task lists (nyx-eds over generated crates/eds/*-sys), natural-language
dates (nyx-when) and provider-independent WebDAV sync. No UI, helper binary, or
application internals are extracted. Regenerate FFI with tools/gen-eds.sh only when the
EDS version changes; run tools/test-eds.sh after nyx-eds changes. Tokens stay behind
nyx-accounts' `credentials` feature (docs/online-accounts/SECURITY.md).

Commands: `cargo check --locked --workspace`; `cargo check --locked -p nyx-core --no-default-features`

Use the public, pinned shared crates; never import another application's internals.
Keep application names, IDs, paths, D-Bus interfaces, and data formats stable.
Use canonical STYLE.md from the pinned nyx-design dependency. Reuse existing
widgets, theme tokens, icons, and shortcuts. Media Player defines video controls.
Run essential targeted checks only. Do not download models or run inference for
routine verification. Install app updates without opening/restarting them.
Common guidance: https://github.com/design-nexus/nyx/blob/main/docs/AGENTS_COMMON.md
This file carries the essential common rules so offline work remains possible.

Shared dependencies use immutable GitHub revisions. Coordinate public API changes
with the suite manifest and affected consumers in design-nexus/nyx.
Historical application tags preserve provenance; standalone builds use main
or subsequent releases. Original release assets remain in the umbrella repo.

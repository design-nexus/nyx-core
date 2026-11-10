# Nyx app actions: the contract (API 2)

What a Nyx app can do for other programs, described once by the app and used the same
way by every frontend: the `nyx` command, Glimpse, Nyx AI Core (for the Assistant and
approved agents) and anything else that links `nyx-actions`.

- **The app** declares its actions, runs them and has the last word. It checks who is
  calling and refuses what that caller may not do. Nothing here moves an app's work
  out of the app.
- **`nyx-actions`** (this crate) is the registry: it finds actions (installed manifests
  and running apps), checks a call's shape and the caller's standing, starts the app
  when it isn't running, routes the call and reports typed errors. It is a library
  linked into each frontend, not a daemon.
- **Frontends** turn what people type into a call (the same resolution rules for all),
  ask for confirmation in their own UI and show the result.

API 1 is what exists today: the `io.github.design_nexus.nyx.Control` D-Bus interface
(`Describe`, `Call`) served by `nyx_core::control`, and AI Core's `list_actions`,
`invoke_action` and `serve_actions` (`nyx-ai/docs/PROTOCOL.md`). API 2 extends both
without breaking a program built for API 1; section 10 lists what each side sees.

## 1. Action IDs and namespaces

- An ID is `namespace.verb`, optionally with more dotted parts: `notes.create`,
  `files.trash.restore`. Lowercase ASCII letters, digits, `_` and `-`; at most 64
  characters (AI Core's `valid_action_id`, unchanged).
- The namespace is the word people type (`nyx notes …`, `notes …` in Glimpse). An app
  owns one or more namespaces (Office owns `write`, Git owns `repo`, Media Player owns
  `video`) and names them in its description. Two apps never share a namespace: the
  registry keeps the first by the precedence in section 4 and logs the conflict.
- IDs are stable. Renaming one is a new action; the old one stays as an alias for at
  least one release.

## 2. The description (Describe v2 and the manifest)

One JSON document describes an app. The app returns it from `Describe` while it runs,
and writes the same document to its manifest when it is installed
(`nyx-<app> --describe-actions`, which prints it without starting the UI). It is the
MCP tool list API 1 already uses, with Nyx's additions in an `x-nyx` object per tool
and a `nyx` object for the app; an API 1 reader keeps reading `tools` and ignores the
rest.

```json
{
  "tools": [
    {
      "name": "notes.create",
      "description": "Create a note in the open vault.",
      "inputSchema": {
        "type": "object",
        "properties": { "title": { "type": "string" }, "text": { "type": "string" } },
        "required": ["title"],
        "additionalProperties": false
      },
      "outputSchema": { "type": "object", "properties": { "id": { "type": "string" } } },
      "annotations": { "title": "Create a note", "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false },
      "x-nyx": {
        "risk": "writes",
        "confirmation": "never",
        "aliases": ["new", "add"],
        "keywords": ["write", "jot"],
        "positional": ["title"],
        "permissions": ["notes.write"],
        "requires": [],
        "timeout_ms": 10000,
        "suggest": { "title": "notes.titles" },
        "result": "object"
      }
    }
  ],
  "nyx": {
    "api": 2,
    "app": "io.github.design_nexus.nyx.Notes",
    "name": "Notes",
    "icon": "io.github.design_nexus.nyx.Notes",
    "namespaces": ["notes"],
    "aliases": ["note"],
    "bus_name": "io.github.design_nexus.nyx.Notes",
    "object_path": "/io/github/design_nexus/nyx/Notes",
    "activation": "dbus",
    "default": "notes.search",
    "open": { "action": "notes.open" },
    "topics": [{ "name": "recent", "sensitive": true }],
    "panel": { "id": "notes", "topics": ["recent"], "size": { "min": [360, 240], "preferred": [520, 380], "max": [760, 560] } }
  }
}
```

### Per action (`tools[]`)

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | The action ID. |
| `description` | yes | One or two sentences, up to 2000 characters. The first sentence is the short form frontends show. |
| `inputSchema` | no (default `{"type":"object"}`) | JSON Schema of the arguments. Must compile. `additionalProperties: false` is recommended. |
| `outputSchema` | no | JSON Schema of the result. |
| `annotations.title` | no | Human-readable name ("Create a note"). Default: the description's first sentence. |
| `annotations.*Hint` | no | MCP hints, kept for API 1 readers. Written from `x-nyx.risk` (below); an API 2 reader uses `risk`. |
| `x-nyx.risk` | no (default: unknown) | `read_only`, `writes`, `external` (sends something off the computer), `destructive` (deletes or overwrites) or `privileged` (needs authorization beyond the user's session, done by the app through Polkit or its own helper). |
| `x-nyx.confirmation` | no | `never` or `ask`. Default `never` for `read_only` and `writes`, `ask` for the rest. `destructive`, `privileged` and unknown are always `ask`, whatever this says. |
| `x-nyx.aliases` | no | Other verbs for the action: a word (`"new"`), or a word with preset arguments (`{"word": "today", "arguments": {"range": "today"}}`). |
| `x-nyx.keywords` | no | Extra words for search ranking only; never used to pick an action to run. |
| `x-nyx.positional` | no | Argument names that may be given in order without their names, as in `nyx notes create "Project ideas"`. The last one takes the remaining words when it is a string. |
| `x-nyx.permissions` | no | Permission names the app checks (its own vocabulary, such as `files.write`). Metadata for policies, such as the Assistant's; the app enforces them. |
| `x-nyx.requires` | no | nyx-capability IDs the action depends on (`program:docker`, `service:user:nyx-homed`). While one isn't available the action is listed as unavailable, with the capability's reason. |
| `x-nyx.timeout_ms` | no | Default 30 000, at most 300 000. |
| `x-nyx.suggest` | no | Argument name to the ID of a read-only action that suggests values for it (section 8). |
| `x-nyx.result` | no | How a frontend shows the result without app-specific code: `list`, `object`, `message` or `none` (section 9). |

An action with no `x-nyx` and no annotations is `unknown` risk: treated as `writes`
that always asks, as AI Core does today.

### Per app (`nyx`)

| Field | Required | Meaning |
| --- | --- | --- |
| `api` | yes | `2`. A description without `nyx` is API 1. |
| `app` | yes | The application ID. |
| `name`, `icon` | yes | As in the app's desktop file. |
| `namespaces` | yes | The command words the app owns. Every tool's namespace must be one of them. |
| `aliases` | no | Other words for the app (`note`, `music player`). |
| `bus_name` | yes | Well-known D-Bus name. Must be `app` or start with `io.github.design_nexus.nyx.`. |
| `object_path` | no | Default: GApplication's path for `bus_name`. |
| `activation` | yes | `dbus`: D-Bus starts the app headless when it isn't running (section 5). `running`: only while it runs; calls to it when closed fail with `not_running`. |
| `default` | no | The action used when only the namespace and free words are given (`music jazz` runs `music.search` with `query: "jazz"` in the command line, and opens the Music panel filtered to "jazz" in Glimpse). It must be `read_only`. |
| `open` | no | How to open the full app at something: an action taking the same `id` or `path` a result carries, run with `confirmation: never`. Without it frontends launch the app's desktop entry. |
| `topics` | no | Live state the app publishes (section 7). `sensitive: true` limits a topic to trusted callers. |
| `panel` | no | The Glimpse panel for this app (section 11). |

## 3. Risk, callers and who enforces what

The **app** is the authoritative boundary. `nyx_core::control` (API 2) enforces the
table below before the app's handler runs, so an app can't forget it, and no frontend
checks are trusted in its place. The registry runs the same checks first so frontends
can show a clear answer early. It is a convenience, not the protection.

The caller is identified from the D-Bus message: the sender's process (bus daemon
`GetConnectionUnixProcessID`), its executable, then `nyx_peer::program` (a Nyx program
in the app's own folder or a folder in `/etc/nyx/trusted-bin`). Anything else is
`untrusted`. As `nyx-peer` says, a same-user program running a trusted executable
itself is beyond what this can stop; the check keeps other programs out, and
confirmation keeps a person in the loop.

| Risk | Untrusted caller | Trusted caller (`nyx-actions` CLI, Glimpse backend, AI Core, other suite programs) |
| --- | --- | --- |
| `read_only` | allowed | allowed |
| `writes` | refused (`permission_denied`) | allowed; needs `confirmed` when the action asks |
| `external` | refused | needs `confirmed` |
| `destructive` | refused | needs `confirmed` |
| `privileged` | refused | needs `confirmed`; then the app asks Polkit or its own helper, which may show its own prompt. Nothing in this contract elevates anything. |
| unknown | refused | needs `confirmed` |

- `confirmed` means a person agreed in the frontend's own UI after seeing the action
  and its arguments: Glimpse's confirmation card, the CLI's prompt on a terminal (or
  `--yes`, typed by the person), the Assistant's approval. A frontend never sets it
  by itself.
- AI Core is trusted, but calls for other programs. It keeps its own rule (untrusted
  AI Core clients get read-only actions only) and passes the original caller as
  `on_behalf_of` for the app to log or refuse.
- Secrets never travel through actions. An action that uses a secret (Vault's copy)
  does the secret work inside the owning app or daemon, after that app's own
  authentication, and returns only what can be shown or logged. Arguments and results
  are never logged by the registry, the frontends or the helper; the registry logs the
  action, the caller, the owner, the outcome and the duration (as AI Core does).
- Arguments are untrusted input everywhere. The registry validates them against the
  schema, the helper validates them again against the app's own compiled-in schema,
  and the handler still checks what it uses (paths through `home_path`, names looked
  up, never passed to a shell).
- A frontend never runs a non-read-only action because of text alone. Typed text
  picks a row or a command; running it takes Enter on it (and confirmation when it
  asks). Fuzzy matches are never executed without being chosen.

## 4. Discovery

The registry knows an app from two sources and merges them.

1. **Manifests**: `nyx/actions/<app-id>.json` in `$XDG_DATA_HOME` (default
   `~/.local/share`), then each of `$XDG_DATA_DIRS` in order (NixOS's system and
   per-user profiles are in it). The first file for an app ID wins. A manifest is
   at most 256 KiB, has at most 64 tools, and must parse and validate (schemas
   compile, namespaces match, bus name in the Nyx prefix); otherwise it is skipped
   and the reason logged. Files are read when first needed and again when their
   modification time changes. Opening a frontend reads files; it starts nothing.
2. **Running apps**: names on the session bus with the Nyx prefix answer `Describe`
   (2 s timeout, answers kept 10 s, dropped at once when the name's owner changes).
   An API 1 answer is a plain tool list.

Merging, per app:

- Running: the live tool list is used (a manifest can be older than the running
  build). Each action's risk and confirmation are the stricter of the live and
  manifest declarations.
- Not running, `activation: dbus`: the manifest's tools, listed as available (state
  `startable`).
- Not running, `activation: running`: the manifest's tools, unavailable, reason
  "Open <Name> to use this".
- Running without a manifest (API 1 apps): the live tools only.
- Actions registered with AI Core over `serve_actions` stay AI Core's: AI Core lists
  them with the rest, and other frontends see them only through AI Core while it is
  running (they never start it).

Each listed action carries `available` (bool), `state` (`ready`, `startable`,
`not_running`, `unavailable`), `reason` when not `ready`, and `source` (`manifest`,
`dbus` or `connection`).

## 5. Activation

An app with `activation: dbus` installs a D-Bus service file named after its bus name
(`share/dbus-1/services/<bus_name>.service`):

```ini
[D-BUS Service]
Name=io.github.design_nexus.nyx.Notes
Exec=/absolute/path/to/nyx-notes --gapplication-service
```

D-Bus expands nothing in `Exec`, so the installer writes the absolute path
(`~/.local/bin` on Omarchy, the store path on NyxOS).

- The registry starts it with `StartServiceByName` (up to 10 s, counted in the
  action's timeout), describes it, then calls it.
- In service mode the app shows no window. The Control interface is registered at
  startup as usual; the helper holds the application while a call runs. The app sets
  a 10 s inactivity timeout and quits after it, unless it holds itself for something
  ongoing (Music playing, a copy running). A window opened later is the same
  process.
- An app that already has a service file for its bus name keeps it and gains the
  flag only if it has none.

## 6. The Control interface, API 2

`io.github.design_nexus.nyx.Control` at the app's object path. Every existing method
keeps its signature.

| Method | Signature | API | Behaviour |
| --- | --- | --- | --- |
| `Describe` | `() -> s` | 1, 2 | The description (section 2). |
| `Call` | `(s name, s arguments) -> s` | 1 | Kept for API 1 callers. The helper applies section 3 with the caller identified as above, and treats a trusted caller's call as confirmed (what API 1 callers already do: AI Core asks first). Errors stay `{"error": "message"}`. |
| `Invoke` | `(s action, s arguments, s options) -> s` | 2 | `options`: `{"confirmed": bool, "on_behalf_of": "…", "frontend": "cli" \| "glimpse" \| "ai-core" \| "…"}`. Reply `{"ok": value}` or `{"error": {"kind": "…", "message": "…"}}` with section 12's kinds. |
| `State` | `(s topic) -> s` | 2 | `{"rev": n, "state": value}`. |
| `Watch` | `(s topic) -> u` | 2 | Starts sending this caller `StateChanged` for the topic; returns the current `rev`. Counted per caller; cancelled by `Unwatch` or when the caller's bus connection goes away. |
| `Unwatch` | `(s topic) -> ()` | 2 | |
| signal `StateChanged` | `(s topic, u rev, s patch)` | 2 | A JSON Merge Patch from `rev - 1`, sent only to callers watching the topic (unicast). A watcher that sees a gap calls `State`. |

- An app works out a topic's state only while someone watches it, and stops when
  the last watcher leaves.
- `sensitive` topics follow `read_only` actions' rule plus trust: untrusted callers
  get `permission_denied`.
- Patches are coalesced: at most 10 per second per topic; a burst sends the last.

## 7. Live state (topics)

A topic is a JSON object owned by the app, such as Music's `playback`
(`{"state": "playing", "title": …, "artist": …, "art": "file:///…", "position": 12.3,
"duration": 240, "volume": 0.6}`) or Home's `devices`. Topics carry what a compact
view needs, not the app's whole model. Their shapes are documented by the app beside
its tools and versioned by adding fields only; a change that removes or retypes a
field is a new topic name.

## 8. Resolution (all frontends)

What people type is resolved deterministically. No AI is involved; a frontend may
offer AI Core's interpretation only as a separate, explicit suggestion when
resolution finds nothing.

1. Split into words (quotes group words).
2. The first word is a namespace or app alias: exact, then a unique prefix of at
   least two letters. Built-in words of the frontend (`nyx install`, `nyx update`…)
   come first.
3. The next word is a verb: the action's last part (`create` for `notes.create`), an
   alias, then a unique prefix. Aliases with preset arguments add them.
4. Otherwise, with free words after the namespace, the app's `default` action is used
   with the words as its first positional argument.
5. The remaining words fill `positional` arguments in order; `--name value`,
   `--name=value` and `--flag` (booleans) set any argument by name. Values are
   converted by the schema (numbers, booleans, enums matched case-insensitively).
6. Then the arguments are validated against the schema. Missing required arguments
   are reported by name (the CLI prints the action's help; Glimpse shows the hint
   line and keeps the command open for typing).

An ambiguous word is an error listing the choices (`did you mean notes.create or
notes.clip?`). Frontends never guess between them.

**Suggestions.** A frontend completes argument values from schema `enum`s, from
`format: path` (local paths), and from `x-nyx.suggest` actions. A suggest action is
`read_only`, takes `{"prefix": string, "limit": integer}` plus any arguments already
given, answers within 300 ms, and returns
`[{"value": …, "label": "…", "detail": "…"}]`. Glimpse drops answers that arrive
after the text changed.

## 9. Results

The result is the action's JSON, checked against `outputSchema` when there is one
(a mismatch is logged and the result still shown). `x-nyx.result` says how to show
it:

- `list`: `{"items": [{"title", "subtitle"?, "icon"?, "id"? or "path"?, "open"?}]}`.
  CLI: a table, one item per line. Glimpse: result rows, each opening through the
  item's `open` or the app's `open` action.
- `object`: key and value pairs.
- `message`: `{"message": "…"}`, shown as a status line.
- `none`: nothing to show beyond "Done".

With `--json` the CLI prints `{"ok": result}` or `{"error": {"kind", "message"}}`.

## 10. Compatibility

| Program | Talks to | Sees |
| --- | --- | --- |
| An app on API 1 (`nyx_core::control` today) | any registry | Its tools with unknown risk unless annotated; `Call` only. Works as today. |
| An app on API 2 | AI Core before this change | `tools` with annotations (API 1 reading of hints); `Call`. |
| AI Core protocol clients built before this change | AI Core after it | `list_actions` replies exactly as API 1 (`ActionSpec` is strict on reading, so no field is added). Extra detail only for `list_actions` with `"detail": true`, which an old AI Core rejects as `invalid_request`; `nyx-ai-client` then falls back. |
| The Assistant's `nyx-apps` ability | AI Core | Unchanged. Its fallback when AI Core is down calls `Call` through `busctl`, which API 2 treats as untrusted (read-only); it moves to the `nyx-actions` CLI (trusted) in the same change. |
| Backup (its own `Describe` with grants) | any registry | Unchanged; its grant model is the app's own enforcement. |

## 11. Glimpse panels

- A panel is QML that ships inside nyx-glimpse (`quickshell/Glimpse/panels/<id>/`),
  reviewed with Glimpse. An app's `nyx.panel.id` only selects one of those; an
  unknown ID is ignored. No QML from apps, manifests or plugins runs in the shell.
- A panel reads only what Glimpse's backend gives it: the app's topics (through
  `Watch`, released when the panel closes or is unpinned and closed) and the results
  of the app's actions. It runs actions through the same `Invoke` path, with
  `frontend: "glimpse"` and Glimpse's confirmation card; it has no other way to reach
  the app.
- `size` gives minimum, preferred and maximum sizes in logical pixels. Glimpse keeps
  the panel within them and the screen.
- "Open in <App>" uses `nyx.open` with the panel's current item or location.

## 12. Errors

| Kind | When | CLI exit code |
| --- | --- | --- |
| `invalid_arguments` | Arguments don't match the schema, or the words don't resolve to arguments. | 2 |
| `unknown_app` | No app owns the namespace. | 3 |
| `unknown_action` | The app has no such action. | 3 |
| `not_installed` | The action needs something not installed (`requires`). | 4 |
| `not_running` | An `activation: running` app is closed. | 4 |
| `unavailable` | Listed but unusable now (a capability down, the app went away, it didn't start). | 4 |
| `permission_denied` | The caller may not run it (section 3), or the app refused it on permission grounds. | 5 |
| `confirmation_required` | It asks first and wasn't confirmed. | 6 |
| `failed` | The app ran it and it failed; the message is the app's. | 1 |
| `timeout` | No answer within the timeout. | 124 |
| `cancelled` | The caller cancelled it. | 130 |
| `internal` | A bug in the registry or the app. | 70 |

Usage mistakes in the CLI itself (an unknown option) exit with 2.

AI Core's kinds map onto these: `unknown_action` stays; `invalid_request` on a call
is `invalid_arguments`; `action_failed` is `failed`; `permission_denied`,
`confirmation_required`, `unavailable`, `timeout` and `cancelled` stay.

## 13. Limits

| What | Limit |
| --- | --- |
| Tools per app | 64 |
| Manifest or `Describe` reply | 256 KiB |
| Arguments | 256 KiB |
| `Describe` | 2 s; answers kept 10 s |
| Activation | 10 s, inside the call's timeout |
| Call | 30 s by default, 300 s at most |
| Suggest | 300 ms |
| `StateChanged` | 10 per second per topic |

## 14. Not part of this contract

- Running shell commands, scripts or programs named in arguments.
- Loading QML, plugins or code from manifests.
- Natural-language interpretation (an optional frontend feature through AI Core).
- Choosing for the person: an action is never picked or confirmed on their behalf.

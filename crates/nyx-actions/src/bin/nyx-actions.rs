//! `nyx-actions`: Nyx apps' actions from the command line. The `nyx` command hands it
//! `nyx apps …` and `nyx APP …`; programs use `call`.

use nyx_actions::resolve::{self, Resolution};
use nyx_actions::{Action, Catalog, Entry, Error, ErrorKind, InvokeOptions, Listed, Options, Registry, ResultKind, State};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};

const USAGE: &str = "\
nyx-actions apps [--json]                    installed and running apps and what they offer
nyx-actions help APP [VERB]                  an app's actions, or one action's arguments
nyx-actions run APP [VERB] [ARGS…] [--yes] [--json]
                                             run what the words name (nyx APP … runs this)
nyx-actions call ACTION [JSON] [--yes]       run an action by ID with JSON arguments (for
                                             programs; prints JSON)
nyx-actions has WORD                         succeeds when an app answers to WORD
nyx-actions complete [WORDS…]                completions for the last word
nyx-actions completion bash|zsh|fish [WORD…] a completion script for nyx (WORDs: nyx's own
                                             commands)

--yes: you agree to an action that asks first. Without it, nyx asks on a terminal and
refuses otherwise. --json: print {\"ok\": …} or {\"error\": {\"kind\", \"message\"}}.";

struct Flags {
    json: bool,
    yes: bool,
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut take = |flag: &str| match args[..end.min(args.len())].iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let flags = Flags { json: take("--json"), yes: take("--yes") };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let code = runtime.block_on(run(args, &flags));
    std::process::exit(code);
}

async fn run(args: Vec<String>, flags: &Flags) -> i32 {
    let registry = Registry::new(Options::default());
    let command = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or_default().to_vec();
    let result = match command {
        "apps" | "list" => apps(&registry, flags).await,
        "help" => help(&registry, &rest).await,
        "run" => run_words(&registry, &rest, flags).await,
        "call" => call(&registry, &rest, flags).await,
        "has" => return has(&registry, rest.first().map(String::as_str).unwrap_or("")).await,
        "complete" => {
            complete(&registry, &rest).await;
            Ok(())
        }
        "completion" => completion(&rest),
        "-h" | "--help" | "" => {
            println!("{USAGE}");
            return if command.is_empty() { 2 } else { 0 };
        }
        other => Err(Error::new(ErrorKind::InvalidArguments, format!("unknown command {other:?}\n\n{USAGE}"))),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            if flags.json || command == "call" {
                println!("{}", json!({"error": e}));
                if command == "call" {
                    eprintln!("{e}");
                }
            } else {
                eprintln!("nyx: {e}");
            }
            e.kind.exit_code()
        }
    }
}

async fn apps(registry: &Registry, flags: &Flags) -> Result<(), Error> {
    let mut catalog = registry.catalog().await;
    catalog.apps.sort_by_key(|e| e.app.name.to_lowercase());
    if flags.json {
        println!("{}", json!({"ok": catalog}));
        return Ok(());
    }
    if catalog.apps.is_empty() {
        println!("No Nyx app offers actions.");
        return Ok(());
    }
    let width = catalog.apps.iter().map(|e| e.app.namespaces.join(", ").len()).max().unwrap_or(0);
    for e in &catalog.apps {
        let state = if e.running.is_some() {
            "running"
        } else if e.actions.iter().any(|l| l.state == State::Startable) {
            "installed"
        } else {
            "open it to use"
        };
        println!("  {:width$}  {:<16} {:>2} actions  {state}", e.app.namespaces.join(", "), e.app.name, e.actions.len());
    }
    println!("\nnyx APP --help lists an app's actions.");
    Ok(())
}

fn entry<'a>(catalog: &'a Catalog, word: &str) -> Result<&'a Entry, Error> {
    resolve::find_app(catalog, word)
}

async fn help(registry: &Registry, words: &[String]) -> Result<(), Error> {
    let catalog = registry.catalog().await;
    let Some(first) = words.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let e = entry(&catalog, first)?;
    match words.get(1) {
        None => app_help(e),
        Some(_) => match resolve::resolve(&catalog, words)? {
            Resolution::Call(c) => action_help(e, &e.actions.iter().find(|l| l.action.id == c.action).expect("resolved").action),
            Resolution::App { .. } => app_help(e),
        },
    }
    Ok(())
}

fn app_help(e: &Entry) {
    let ns = &e.app.namespaces[0];
    println!("{} ({})\n", e.app.name, e.app.namespaces.join(", "));
    let lines: Vec<(String, &Listed)> = e.actions.iter().map(|l| (usage(ns, &l.action), l)).collect();
    let width = lines.iter().map(|(u, _)| u.len()).max().unwrap_or(0).min(48);
    for (u, l) in &lines {
        let mut note = l.action.short().to_string();
        if l.action.asks() {
            note.push_str(" (asks first)");
        }
        if let Some(reason) = l.reason.as_ref().filter(|_| !l.available) {
            note.push_str(&format!(" [{reason}]"));
        }
        if u.len() > width {
            println!("  {u}\n  {:width$}  {note}", "");
        } else {
            println!("  {u:width$}  {note}");
        }
    }
    if let Some(d) = &e.app.default {
        println!("\nnyx {ns} WORDS… runs {d}.");
    }
}

fn action_help(e: &Entry, a: &Action) {
    let ns = &e.app.namespaces[0];
    println!("{}\n\n  {}\n\n{}", a.title, usage(ns, a), a.description);
    let required = a.required();
    if let Some(props) = a.input_schema.get("properties").and_then(Value::as_object) {
        println!();
        for (name, schema) in props {
            let mut line = format!("  --{:<14}", name.replace('_', "-"));
            line.push_str(&kind(schema));
            if required.contains(&name.as_str()) {
                line.push_str(", required");
            }
            if let Some(d) = schema.get("description").and_then(Value::as_str) {
                line.push_str(&format!(": {d}"));
            }
            println!("{line}");
        }
    }
    let aliases: Vec<&str> = a.aliases.iter().map(|x| x.word()).collect();
    if !aliases.is_empty() {
        println!("\nAlso: {}", aliases.join(", "));
    }
    println!("\nIt {}{}.", a.risk.label(), if a.asks() { "; nyx asks before running it" } else { "" });
}

fn kind(schema: &Value) -> String {
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        return choices
            .iter()
            .map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string()))
            .collect::<Vec<_>>()
            .join("|");
    }
    schema.get("type").and_then(Value::as_str).unwrap_or("text").replace("string", "text")
}

/// `nyx notes create TITLE [--pinned] [--text TEXT]`.
fn usage(ns: &str, a: &Action) -> String {
    let mut out = format!("nyx {ns} {}", a.verb().replace('.', " "));
    let required = a.required();
    for p in &a.positional {
        let name = p.to_uppercase();
        out.push_str(&if required.contains(&p.as_str()) { format!(" {name}") } else { format!(" [{name}]") });
    }
    if let Some(props) = a.input_schema.get("properties").and_then(Value::as_object) {
        for (name, schema) in props.iter().filter(|(n, _)| !a.positional.contains(n)) {
            let flag = name.replace('_', "-");
            let part = if schema.get("type").and_then(Value::as_str) == Some("boolean") {
                format!("--{flag}")
            } else {
                format!("--{flag} {}", name.to_uppercase())
            };
            out.push_str(&if required.contains(&name.as_str()) { format!(" {part}") } else { format!(" [{part}]") });
        }
    }
    out
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

async fn run_words(registry: &Registry, words: &[String], flags: &Flags) -> Result<(), Error> {
    let end = words.iter().position(|w| w == "--").unwrap_or(words.len());
    if words[..end].iter().any(|w| w == "--help" || w == "-h") {
        let words: Vec<String> = words[..end].iter().filter(|w| *w != "--help" && *w != "-h").cloned().collect();
        return help(registry, &words).await;
    }
    let catalog = registry.catalog().await;
    let resolved = resolve::resolve(&catalog, words).map_err(|mut e| {
        if e.kind == ErrorKind::UnknownApp {
            e.message.push_str(" (nyx apps lists the apps; nyx --help, nyx's own commands)");
        }
        // Said in words no command matches: Nyx Assistant may understand, if asked.
        if matches!(e.kind, ErrorKind::UnknownApp | ErrorKind::UnknownAction) && on_path("nyx-assistant") {
            let said = words.join(" ").replace('\'', "'\\''");
            e.message.push_str(&format!("\n  To ask Nyx Assistant instead: nyx-assistant say '{said}'"));
        }
        e
    })?;
    let c = match resolved {
        Resolution::App { app } => {
            app_help(catalog.apps.iter().find(|e| e.app.app == app).expect("resolved"));
            return Ok(());
        }
        Resolution::Call(c) => c,
    };
    let (e, l) = catalog.find(&c.action).expect("resolved");
    if !c.missing.is_empty() {
        let names: Vec<String> = c.missing.iter().map(|m| m.to_uppercase()).collect();
        return Err(Error::new(
            ErrorKind::InvalidArguments,
            format!("{} needs {}\n  {}", c.action, names.join(" and "), usage(&e.app.namespaces[0], &l.action)),
        ));
    }
    let arguments = Value::Object(c.arguments);
    let confirmed = confirm(e, &l.action, &arguments, flags)?;
    let options = InvokeOptions { confirmed, trusted: true, on_behalf_of: None, frontend: "cli".into() };
    let result = registry.invoke(&c.action, arguments, &options).await?;
    if flags.json {
        println!("{}", json!({"ok": result}));
    } else {
        show(&l.action, &result);
    }
    Ok(())
}

async fn call(registry: &Registry, args: &[String], flags: &Flags) -> Result<(), Error> {
    let id = args.first().ok_or_else(|| Error::new(ErrorKind::InvalidArguments, "nyx-actions call ACTION [JSON]"))?;
    let arguments: Value = match args.get(1) {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| Error::new(ErrorKind::InvalidArguments, format!("the arguments aren't JSON: {e}")))?,
        None => json!({}),
    };
    let catalog = registry.catalog().await;
    let confirmed = match catalog.find(id) {
        Some((e, l)) => confirm(e, &l.action, &arguments, flags)?,
        None => flags.yes,
    };
    let options = InvokeOptions { confirmed, trusted: true, on_behalf_of: None, frontend: "cli".into() };
    let result = registry.invoke(id, arguments, &options).await?;
    println!("{}", json!({"ok": result}));
    Ok(())
}

/// Whether the person agreed: `--yes`, or a yes on a terminal.
fn confirm(e: &Entry, a: &Action, arguments: &Value, flags: &Flags) -> Result<bool, Error> {
    if !a.asks() || flags.yes {
        return Ok(flags.yes);
    }
    // SAFETY: isatty only reads the descriptor's state.
    let terminal = unsafe { libc::isatty(0) == 1 && libc::isatty(2) == 1 };
    if !terminal || flags.json {
        return Err(Error::new(
            ErrorKind::ConfirmationRequired,
            format!("{} {}; run it again with --yes to go ahead", a.id, a.risk.label()),
        ));
    }
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{}: {} ({})", e.app.name, a.title, a.risk.label());
    if let Some(map) = arguments.as_object() {
        for (k, v) in map {
            let _ = writeln!(err, "  {k}: {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
        }
    }
    let _ = write!(err, "Go ahead? [y/N] ");
    let _ = err.flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(true)
    } else {
        Err(Error::new(ErrorKind::Cancelled, "not run"))
    }
}

fn show(a: &Action, result: &Value) {
    let kind = a.result.or_else(|| match result {
        Value::Null => Some(ResultKind::None),
        Value::Object(m) if m.get("items").is_some_and(Value::is_array) => Some(ResultKind::List),
        Value::Object(m) if m.len() == 1 && m.get("message").is_some_and(Value::is_string) => Some(ResultKind::Message),
        Value::Array(_) => Some(ResultKind::List),
        Value::Object(_) => Some(ResultKind::Object),
        _ => None,
    });
    match (kind, result) {
        (Some(ResultKind::None), _) | (_, Value::Null) => println!("Done."),
        (Some(ResultKind::Message), v) => println!("{}", v.get("message").and_then(Value::as_str).unwrap_or("Done.")),
        (Some(ResultKind::List), v) => {
            let items = v.get("items").or(Some(v)).and_then(Value::as_array).cloned().unwrap_or_default();
            if items.is_empty() {
                println!("Nothing found.");
            }
            for item in items {
                println!("{}", line(&item));
            }
        }
        (Some(ResultKind::Object), Value::Object(m)) => object(m, ""),
        (_, Value::String(s)) => println!("{s}"),
        (_, v) => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
    }
}

fn line(item: &Value) -> String {
    match item {
        Value::Object(m) => {
            let title =
                ["title", "name", "summary", "label", "path", "id"].iter().find_map(|k| m.get(*k).and_then(Value::as_str));
            let sub = ["subtitle", "detail", "artist", "when", "location"].iter().find_map(|k| m.get(*k).and_then(Value::as_str));
            match (title, sub) {
                (Some(t), Some(s)) => format!("  {t} · {s}"),
                (Some(t), None) => format!("  {t}"),
                _ => format!("  {}", Value::Object(m.clone())),
            }
        }
        Value::String(s) => format!("  {s}"),
        v => format!("  {v}"),
    }
}

fn object(m: &Map<String, Value>, indent: &str) {
    for (k, v) in m {
        match v {
            Value::Object(inner) if !inner.is_empty() => {
                println!("{indent}{k}:");
                object(inner, &format!("{indent}  "));
            }
            Value::String(s) => println!("{indent}{k}: {s}"),
            v => println!("{indent}{k}: {v}"),
        }
    }
}

async fn has(registry: &Registry, word: &str) -> i32 {
    if word.is_empty() || word.starts_with('-') {
        return 1;
    }
    let installed = registry.installed();
    if installed.app(word).is_some() {
        return 0;
    }
    if registry.catalog().await.app(word).is_some() { 0 } else { 1 }
}

/// Candidates for the last of `words` (the one being typed).
async fn complete(registry: &Registry, words: &[String]) {
    let words: Vec<String> = words.iter().filter(|w| *w != "--").cloned().collect();
    let (done, partial) = match words.split_last() {
        Some((last, done)) => (done.to_vec(), last.clone()),
        None => (Vec::new(), String::new()),
    };
    let catalog = registry.catalog().await;
    let mut out = resolve::complete(&catalog, &done, &partial);
    out.extend(registry.suggest(&catalog, &done, &partial).await);
    out.dedup();
    let extra = match done.len() {
        0 => "apps",
        1 if entry(&catalog, &done[0]).is_ok() => "--help",
        _ => "",
    };
    if !extra.is_empty() && extra.starts_with(&partial) {
        out.push(extra.into());
    }
    for c in out {
        println!("{c}");
    }
}

fn completion(args: &[String]) -> Result<(), Error> {
    let shell = args.first().map(String::as_str).unwrap_or("");
    let extra = args.get(1..).unwrap_or_default().join(" ");
    let script = match shell {
        "bash" => format!(
            r#"_nyx() {{
  local cur=${{COMP_WORDS[COMP_CWORD]}}
  if ((COMP_CWORD == 1)); then
    mapfile -t COMPREPLY < <(compgen -W "{extra}" -- "$cur"; nyx-actions complete -- "$cur" 2>/dev/null)
  else
    mapfile -t COMPREPLY < <(nyx-actions complete -- "${{COMP_WORDS[@]:1:COMP_CWORD}}" 2>/dev/null)
  fi
}}
complete -F _nyx nyx
"#
        ),
        "zsh" => format!(
            r#"#compdef nyx
_nyx() {{
  local -a found
  if (( CURRENT == 2 )); then found=({extra}); fi
  found+=("${{(@f)$(nyx-actions complete -- "${{(@)words[2,CURRENT]}}" 2>/dev/null)}}")
  compadd -a found
}}
compdef _nyx nyx
"#
        ),
        "fish" => format!(
            "complete -c nyx -f -n '__fish_is_first_arg' -a '{extra}'\ncomplete -c nyx -f -a '(nyx-actions complete -- (commandline -opc)[2..-1] (commandline -ct) 2>/dev/null)'\n"
        ),
        _ => return Err(Error::new(ErrorKind::InvalidArguments, "nyx-actions completion bash|zsh|fish")),
    };
    print!("{script}");
    Ok(())
}

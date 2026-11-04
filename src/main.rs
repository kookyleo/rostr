mod discover;
mod model_id;
mod scope;
mod selection;
mod store;
mod tui;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::discover::{Discovery, Env};
use crate::selection::{Selection, Status};
use crate::store::Store;

fn cli() -> Command {
    Command::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .subcommand(
            Command::new("discover")
                .about("Discover local AI agents and models, then choose the models to track")
                .long_about(
                    "Scans installed AI agents (Claude Code, Codex, OpenCode, Grok CLI, Gemini CLI) \
                     for configured and available models, then opens a TUI to choose the models \
                     to track. The choice is saved to scope.yaml. Discovery reads local files \
                     only: it sends no inference request and uses no model tokens.",
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("yes")
                        .help("Print the merged result and warnings; do not open the TUI or save"),
                )
                .arg(
                    Arg::new("yes")
                        .long("yes")
                        .short('y')
                        .action(ArgAction::SetTrue)
                        .help("Accept the default selection and save without the TUI"),
                ),
        )
}

fn main() {
    let mut command = cli();
    let matches = command.get_matches_mut();
    let result = match matches.subcommand() {
        Some(("discover", m)) => run_discover(m),
        _ => command.print_help().map_err(Into::into),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run_discover(m: &ArgMatches) -> Result<()> {
    let env = Env::from_process().context("HOME is not set")?;
    let var = |k: &str| env.var(k).map(String::from);
    let scope_path = scope::config_dir(&env.home, var).join(scope::FILE_NAME);
    let db_path = scope::data_dir(&env.home, var).join(store::FILE_NAME);
    // An invalid scope.yaml stops here, so a broken hand edit is never overwritten.
    let existing = scope::load(&scope_path)?;
    let known = match Store::open_read_only(&db_path)? {
        Some(store) => store.known()?,
        None => Vec::new(),
    };
    let discovery = discover::discover(&env);
    let mut sel = Selection::merge(existing.as_ref(), &known, &discovery.candidates);

    if m.get_flag("dry-run") {
        print_report(&discovery, &sel, &scope_path, &db_path);
        return Ok(());
    }
    if !m.get_flag("yes") {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!(
                "the discover TUI needs a terminal; use --dry-run to preview or --yes to save the defaults"
            );
        }
        match tui::run(&mut sel, &discovery.agents, existing.is_some())? {
            tui::Outcome::Cancel => {
                println!("Cancelled. Nothing was changed.");
                return Ok(());
            }
            tui::Outcome::NoChanges => {
                println!("No changes. Nothing was written.");
                return Ok(());
            }
            tui::Outcome::Save => {}
        }
    }
    // scope.yaml is the source of truth, so it goes first. The database is
    // derived from it; if the database write fails, the next save rebuilds it.
    let out = sel.to_scope();
    scope::save(&scope_path, &out)?;
    println!(
        "Saved {} patterns and {} exceptions to {}",
        out.patterns.len(),
        out.exceptions.len(),
        scope_path.display()
    );
    let resolved = sel.resolved();
    let selected = resolved.iter().filter(|r| r.selected).count();
    let agents = discovery
        .agents
        .iter()
        .filter(|a| a.installed)
        .map(|a| a.name)
        .collect();
    Store::open(&db_path)?
        .record(&store::Record {
            agents,
            candidates: &discovery.candidates,
            models: resolved,
            removed: sel.removed_ids(),
        })
        .with_context(|| format!("cannot update {}", db_path.display()))?;
    println!(
        "Recorded {} models ({selected} selected) in {}",
        sel.counts().1,
        db_path.display()
    );
    Ok(())
}

fn print_report(discovery: &Discovery, sel: &Selection, scope_path: &Path, db_path: &Path) {
    println!("Agents:");
    for a in &discovery.agents {
        let state = match (a.installed, a.signed_in) {
            (false, _) => "not found".to_string(),
            (true, true) => format!("installed, signed in, {} models", a.models.len()),
            (true, false) => format!("installed, {} models", a.models.len()),
        };
        println!("  {:<12} {state}", a.name);
        for w in &a.warnings {
            println!("    warning: {w}");
        }
    }
    println!("\nScope:    {}", scope_path.display());
    println!("Database: {}", db_path.display());
    println!("Patterns:");
    if sel.patterns.is_empty() {
        println!("  (none)");
    }
    for p in &sel.patterns {
        println!("  {p}");
    }
    let name_w = sel
        .items()
        .map(|i| i.name.chars().count())
        .max()
        .unwrap_or(0);
    let id_w = sel.items().map(|i| i.id.len()).max().unwrap_or(0);
    // As in the TUI, new rows are green and missing rows are dim.
    // Plain text when piped or with NO_COLOR.
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    for (g, group) in sel.groups.iter().enumerate() {
        let (selected, listed) = sel.group_counts(g);
        println!(
            "\n{}  {selected}/{listed}",
            model_id::provider_display(&group.provider)
        );
        for i in &group.items {
            let mark = if sel.is_selected(i) { "[x]" } else { "[ ]" };
            let (tag, sgr) = match i.status {
                Status::New => ("  new", "32"),
                Status::Missing => ("  missing", "2"),
                Status::Known | Status::Manual => ("", ""),
            };
            let line = format!(
                "  {mark} {:<name_w$}  {:<id_w$}  {}{tag}",
                i.name,
                i.id,
                i.source_label()
            );
            if color && !sgr.is_empty() {
                println!("\x1b[{sgr}m{line}\x1b[0m");
            } else {
                println!("{line}");
            }
        }
    }
}

use super::*;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::Path;

pub fn command() -> Command {
    Command::new("quota")
        .about(
            "Refresh, import and query local quota observations; never runs agents or connects to a network",
        )
        .subcommand_required(true)
        .subcommand(Command::new("import").arg(Arg::new("path").required(true)))
        .subcommand(
            Command::new("init")
                .about("Connect an agent's official quota channel to Rostr (Claude Code: statusLine wrap)")
                .long_about(
                    "Wraps the Claude Code statusLine.command in ~/.claude/settings.json (or \
                     $CLAUDE_CONFIG_DIR/settings.json). The wrapper reads the status line JSON \
                     once, writes its rate_limits to a fixed Rostr snapshot file, and feeds the \
                     same JSON to the previous command, so the status bar keeps working. Without \
                     a previous status line, a minimal one is installed. Re-running updates the \
                     wrapper in place; --uninstall restores the previous command. Rostr never \
                     proxies Anthropic traffic, starts Claude or reads credentials.",
                )
                .arg(
                    Arg::new("claude")
                        .long("claude")
                        .required(true)
                        .action(ArgAction::SetTrue)
                        .help("Set up Claude Code (the only supported agent)"),
                )
                .arg(
                    Arg::new("account")
                        .long("account")
                        .value_name("LABEL")
                        .help("Opaque account label for this Claude login [default: claude-<instance>]"),
                )
                .arg(
                    Arg::new("instance")
                        .long("instance")
                        .value_name("NAME")
                        .help("Instance name [default: `default` for ~/.claude, else derived from the directory]"),
                )
                .arg(
                    Arg::new("claude-config-dir")
                        .long("claude-config-dir")
                        .value_name("DIR")
                        .help("Claude config directory [default: $CLAUDE_CONFIG_DIR or ~/.claude]"),
                )
                .arg(
                    Arg::new("managed-settings")
                        .long("managed-settings")
                        .value_name("PATH")
                        .action(ArgAction::Append)
                        .hide(true),
                )
                .arg(
                    Arg::new("uninstall")
                        .long("uninstall")
                        .action(ArgAction::SetTrue)
                        .help("Restore the previous status line and remove the wrapper"),
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .action(ArgAction::SetTrue)
                        .help("Show what would change; write nothing"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Install even when managed settings set allowManagedHooksOnly"),
                ),
        )
        .subcommand(
            Command::new("statusline")
                .about("Claude Code status line wrapper installed by `rostr quota init --claude`")
                .hide(true)
                .arg(Arg::new("state").long("state").required(true).value_name("PATH")),
        )
        .subcommand(
            Command::new("refresh")
                .about("Read existing local agent state without starting agents or accessing a network")
                .arg(
                    Arg::new("local")
                        .long("local")
                        .required(true)
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("agent")
                        .long("agent")
                        .value_parser(["claude-code", "codex", "grok-build"]),
                )
                .arg(
                    Arg::new("max-age")
                        .long("max-age")
                        .default_value("60")
                        .value_parser(clap::value_parser!(u32))
                        .help("Maximum observation age in seconds"),
                )
                .arg(
                    Arg::new("claude-statusline")
                        .long("claude-statusline")
                        .value_name("PATH")
                        .help("Also read an existing Claude statusline export with identity and original time"),
                )
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("query")
                .arg(Arg::new("agent").long("agent").value_parser([
                    "claude-code",
                    "codex",
                    "opencode",
                    "grok-build",
                    "gemini-cli",
                    "pi",
                ]))
                .arg(Arg::new("model").long("model").action(ArgAction::Append))
                .arg(Arg::new("metric").long("metric").requires("unit"))
                .arg(
                    Arg::new("unit")
                        .long("unit")
                        .requires("metric")
                        .value_parser([
                            "percent",
                            "requests",
                            "input_tokens",
                            "output_tokens",
                            "total_tokens",
                            "concurrent_requests",
                        ]),
                )
                .arg(
                    Arg::new("min-remaining")
                        .long("min-remaining")
                        .requires("metric")
                        .default_value("0"),
                )
                .arg(
                    Arg::new("max-age")
                        .long("max-age")
                        .default_value("120")
                        .value_parser(clap::value_parser!(u32))
                        .help("Maximum observation age in seconds"),
                )
                .arg(
                    Arg::new("as-of")
                        .long("as-of")
                        .value_parser(clap::value_parser!(i64))
                        .help("UTC Unix milliseconds (default: now)"),
                )
                .arg(
                    Arg::new("confidence")
                        .long("confidence")
                        .default_value("any")
                        .value_parser(["any", "authoritative"]),
                )
                .arg(
                    Arg::new("unknown")
                        .long("unknown")
                        .default_value("include")
                        .value_parser(["include", "exclude"]),
                )
                .arg(
                    Arg::new("billing")
                        .long("billing")
                        .default_value("any")
                        .value_parser(["any", "subscription"]),
                )
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
}
pub fn run(m: &ArgMatches) -> Result<()> {
    // Read only path variables; do not copy credential environment values.
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .context("HOME is not set")?;
    let vars = [
        "ROSTR_DATA_DIR",
        "XDG_DATA_HOME",
        "CODEX_HOME",
        "GROK_HOME",
        "CLAUDE_CONFIG_DIR",
        "ROSTR_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "PATH",
    ]
    .into_iter()
    .filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| (key.to_string(), value))
    })
    .collect();
    let env = Env {
        home: home.into(),
        vars,
    };
    let path = crate::scope::data_dir(&env.home, |k| env.var(k).map(String::from))
        .join(crate::store::FILE_NAME);
    match m.subcommand() {
        Some(("refresh", args)) => {
            let selected = args.get_one::<String>("agent").map(String::as_str);
            let mut probes = native_probes();
            probes.extend(claude::statusline_probes(&env));
            probes.retain(|probe| match selected {
                Some("claude-code") => probe.id() == claude::PROBE_ID,
                Some("codex") => probe.id() == "codex-native",
                Some("grok-build") => probe.id() == "grok-build-native",
                _ => true,
            });
            if let Some(path) = args.get_one::<String>("claude-statusline") {
                probes.push(Box::new(LocalSnapshotProbe { path: path.into() }));
            }
            let request = ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: now(),
                max_age_ms: i64::from(*args.get_one::<u32>("max-age").unwrap()) * 1000,
            };
            let reports = probes
                .iter()
                .map(|probe| probe.observe(&ProbeContext { env: &env }, &request))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut store = Store::open(&path)?;
            for report in &reports {
                store.record_probe(report)?;
            }
            if args.get_flag("json") {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                for report in &reports {
                    println!(
                        "{}: {:?}, {} local quota windows",
                        report.probe_id,
                        report.completeness,
                        report.buckets.len()
                    );
                }
            }
        }
        Some(("statusline", args)) => {
            let code = claude::run_wrapper(Path::new(args.get_one::<String>("state").unwrap()));
            std::process::exit(code);
        }
        Some(("init", args)) => run_init(args, &env)?,
        Some(("import", args)) => {
            let probe: Box<dyn QuotaProbe> = Box::new(LocalSnapshotProbe {
                path: Path::new(args.get_one::<String>("path").unwrap()).to_path_buf(),
            });
            let request = ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: now(),
                max_age_ms: 60_000,
            };
            let report = probe.observe(&ProbeContext { env: &env }, &request)?;
            Store::open(&path)?.record_probe(&report)?;
            println!(
                "Recorded local observation {} in {}",
                report.source.event_id,
                path.display()
            );
        }
        Some(("query", args)) => {
            let unit = args
                .get_one::<String>("unit")
                .map(|s| serde_json::from_value::<Unit>(serde_json::Value::String(s.clone())))
                .transpose()?;
            let requirements = if let Some(unit) = unit {
                vec![QuotaRequirement {
                    metric: args.get_one::<String>("metric").unwrap().clone(),
                    unit,
                    min_remaining: args.get_one::<String>("min-remaining").unwrap().clone(),
                }]
            } else {
                vec![]
            };
            let agent = args
                .get_one::<String>("agent")
                .map(|s| serde_json::from_value::<AgentId>(serde_json::Value::String(s.clone())))
                .transpose()?;
            let query = TargetQuery {
                agent,
                models: args
                    .get_many::<String>("model")
                    .map(|v| v.cloned().collect())
                    .unwrap_or_default(),
                requirements,
                min_confidence: if args
                    .get_one::<String>("confidence")
                    .is_some_and(|s| s == "authoritative")
                {
                    ConfidencePolicy::AuthoritativeOnly
                } else {
                    ConfidencePolicy::Any
                },
                unknown: if args
                    .get_one::<String>("unknown")
                    .is_some_and(|s| s == "exclude")
                {
                    UnknownPolicy::Exclude
                } else {
                    UnknownPolicy::Include
                },
                billing: if args
                    .get_one::<String>("billing")
                    .is_some_and(|s| s == "subscription")
                {
                    BillingPolicy::SubscriptionOnly
                } else {
                    BillingPolicy::Any
                },
                max_age_ms: i64::from(*args.get_one::<u32>("max-age").unwrap()) * 1000,
                as_of: args.get_one::<i64>("as-of").copied().unwrap_or_else(now),
            };
            let results = match Store::open_read_only(&path)? {
                Some(store) => store.query_targets(&query)?,
                None => vec![],
            };
            if args.get_flag("json") {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                for t in &results {
                    println!(
                        "{} {} {} {:?}: {}",
                        serde_json::to_string(&t.target.agent)?,
                        t.target.model,
                        t.target.account,
                        t.eligibility,
                        t.reasons.join("; ")
                    );
                }
                if results.is_empty() {
                    println!("No execution targets observed; quota is unknown.");
                }
            }
        }
        _ => bail!("expected quota init, refresh, import or query"),
    }
    Ok(())
}

fn run_init(args: &ArgMatches, env: &Env) -> Result<()> {
    let var = |k: &str| env.var(k).map(String::from);
    let claude_dir = match args.get_one::<String>("claude-config-dir") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => env.dir("CLAUDE_CONFIG_DIR", ".claude"),
    };
    let claude_dir = if claude_dir.is_absolute() {
        claude_dir
    } else {
        std::env::current_dir()?.join(claude_dir)
    };
    let instance = args
        .get_one::<String>("instance")
        .cloned()
        .unwrap_or_else(|| claude::instance_for(&env.home, &claude_dir));
    if !claude::valid_name(&instance) {
        bail!("invalid instance name; use letters, digits, `-`, `_` or `.`");
    }
    let state_path = crate::scope::config_dir(&env.home, var)
        .join(claude::DIR_NAME)
        .join(format!("{instance}.json"));
    // Re-running init keeps the account label chosen earlier unless a new one is given.
    let previous_account = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|t| serde_json::from_str::<claude::StatuslineState>(&t).ok())
        .map(|s| s.account);
    let account = args
        .get_one::<String>("account")
        .cloned()
        .or(previous_account)
        .unwrap_or_else(|| format!("claude-{instance}"));
    let exe = std::env::current_exe().context("cannot locate the rostr executable")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let opts = claude::InitOptions {
        claude_dir,
        managed: match args.get_many::<String>("managed-settings") {
            Some(v) => v.map(Into::into).collect(),
            None => claude::default_managed_paths(),
        },
        config_dir: crate::scope::config_dir(&env.home, var),
        data_dir: crate::scope::data_dir(&env.home, var),
        instance,
        account,
        exe: claude::shell_quote(&exe.to_string_lossy()),
        force: args.get_flag("force"),
        dry_run: args.get_flag("dry-run"),
    };
    let settings = opts.settings_path();
    let prefix = if opts.dry_run {
        "Dry run: would have "
    } else {
        ""
    };
    if args.get_flag("uninstall") {
        match claude::uninstall(&opts)? {
            claude::UninstallOutcome::Restored { command } => {
                println!(
                    "{prefix}Restored the previous status line in {}:\n  {command}",
                    settings.display()
                )
            }
            claude::UninstallOutcome::Removed => {
                println!(
                    "{prefix}Removed the Rostr status line from {}",
                    settings.display()
                )
            }
            claude::UninstallOutcome::NotInstalled => println!(
                "The status line in {} is not Rostr's; it was left unchanged.",
                settings.display()
            ),
        }
        println!(
            "Kept snapshot {} and settings backup {}",
            opts.snapshot_path().display(),
            opts.backup_path().display()
        );
        return Ok(());
    }
    let (outcome, warnings) = claude::init(&opts)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    match &outcome {
        claude::InitOutcome::Wrapped { previous } => println!(
            "{prefix}Wrapped the existing status line in {}.\n  previous: {previous}",
            settings.display()
        ),
        claude::InitOutcome::Installed => println!(
            "{prefix}Installed a minimal Rostr status line in {}.",
            settings.display()
        ),
        claude::InitOutcome::Updated => println!(
            "{prefix}Updated the Rostr status line wrapper in {} in place.",
            settings.display()
        ),
        claude::InitOutcome::Unchanged => {
            println!(
                "The Rostr status line in {} is up to date.",
                settings.display()
            )
        }
    }
    println!("  command:  {}", opts.command());
    println!(
        "  account:  {} (opaque label; change with --account)",
        opts.account
    );
    println!("  state:    {}", opts.state_path().display());
    println!("  snapshot: {}", opts.snapshot_path().display());
    if !opts.dry_run {
        println!(
            "Claude Code writes rate limits after its first API response in a subscribed session.\n\
             Then run `rostr quota refresh --local`. Undo with `rostr quota init --claude --uninstall`."
        );
    }
    Ok(())
}

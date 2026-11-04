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
                        .value_parser(["codex", "grok-build"]),
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
            probes.retain(|probe| match selected {
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
        _ => bail!("expected quota refresh, import or query"),
    }
    Ok(())
}

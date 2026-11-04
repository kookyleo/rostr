//! Claude Code subscription quota through the official `statusLine.command` channel.
//!
//! Claude Code runs one status line command and feeds it session JSON on stdin.
//! For Claude subscribers that JSON may carry `rate_limits.five_hour` and
//! `rate_limits.seven_day` after the first API response. `rostr quota init --claude`
//! wraps the configured command (or installs a minimal one); the wrapper writes a
//! version 1 `claude_statusline` envelope to a fixed file and re-feeds the same
//! stdin to the previous command. `rostr quota refresh --local` reads that file.
//!
//! Rostr never proxies Anthropic traffic, starts Claude, reads credentials, or
//! contacts a network here. The snapshot keeps only the rate-limit subset.
use super::native::{digest, empty, finish};
use super::*;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const PROBE_ID: &str = "claude-statusline";
/// Directory name under the Rostr config dir (install state) and data dir (snapshots).
pub const DIR_NAME: &str = "claude-statusline";
/// Text every Rostr wrapper command contains; used to detect an existing install.
pub const MARKER: &str = " quota statusline --state ";
const RECURSION_VAR: &str = "ROSTR_STATUSLINE_WRAPPED";
const MAX_INPUT: u64 = 8 * 1024 * 1024;
const MAX_PARSE: usize = 1024 * 1024;
const MAX_SNAPSHOTS: usize = 16;

/// Install state, written to `<config dir>/claude-statusline/<instance>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatuslineState {
    pub version: u32,
    pub instance: String,
    /// User-assigned opaque account label (never a credential or email).
    pub account: String,
    pub settings: PathBuf,
    pub snapshot: PathBuf,
    /// The `statusLine` object that was configured before Rostr, if any.
    pub previous: Option<Value>,
}

/// Everything `init` needs; paths are explicit so tests never touch real files.
#[derive(Debug, Clone)]
pub struct InitOptions {
    pub claude_dir: PathBuf,
    pub managed: Vec<PathBuf>,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub instance: String,
    pub account: String,
    /// Shell-quoted path of the Rostr executable the wrapper command runs.
    pub exe: String,
    pub force: bool,
    pub dry_run: bool,
}

impl InitOptions {
    pub fn settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }
    pub fn state_path(&self) -> PathBuf {
        self.config_dir
            .join(DIR_NAME)
            .join(format!("{}.json", self.instance))
    }
    pub fn backup_path(&self) -> PathBuf {
        self.config_dir
            .join(DIR_NAME)
            .join(format!("{}.settings-backup.json", self.instance))
    }
    pub fn snapshot_path(&self) -> PathBuf {
        snapshot_dir(&self.data_dir).join(format!("{}.json", self.instance))
    }
    pub fn command(&self) -> String {
        format!(
            "{}{MARKER}{}",
            self.exe,
            shell_quote(&self.state_path().to_string_lossy())
        )
    }
}

pub fn snapshot_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR_NAME)
}

/// Platform locations of Claude Code managed (enterprise) settings.
pub fn default_managed_paths() -> Vec<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode")
    } else if cfg!(windows) {
        PathBuf::from(r"C:\Program Files\ClaudeCode")
    } else {
        PathBuf::from("/etc/claude-code")
    };
    let mut out = vec![base.join("managed-settings.json")];
    if let Ok(entries) = fs::read_dir(base.join("managed-settings.d")) {
        let mut extra: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        extra.sort();
        out.extend(extra);
    }
    out
}

/// Default instance name for a Claude config directory: `default` for `~/.claude`.
pub fn instance_for(home: &Path, claude_dir: &Path) -> String {
    if claude_dir == home.join(".claude") {
        "default".into()
    } else {
        format!(
            "dir-{}",
            &digest(claude_dir.to_string_lossy().as_bytes())[..12]
        )
    }
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && !s.starts_with('.')
}

#[cfg(not(windows))]
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%+=".contains(&b))
    {
        return s.into();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}
#[cfg(windows)]
pub fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

pub fn is_rostr_command(cmd: &str) -> bool {
    cmd.contains(MARKER)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitOutcome {
    /// A previous status line was wrapped.
    Wrapped { previous: String },
    /// No previous status line; the minimal Rostr status line was installed.
    Installed,
    /// An existing Rostr wrapper was updated in place.
    Updated,
    /// Nothing to change.
    Unchanged,
}

struct Settings {
    path: PathBuf,
    text: Option<String>,
    root: Map<String, Value>,
}

fn read_settings(path: &Path) -> Result<Settings> {
    // Write through a symlink (dotfile managers) instead of replacing it.
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = match fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let root = match text.as_deref().map(str::trim) {
        None | Some("") => Map::new(),
        Some(t) => match serde_json::from_str::<Value>(t) {
            Ok(Value::Object(m)) => m,
            Ok(_) => bail!(
                "{} is not a JSON object; fix it by hand, Rostr does not rewrite it",
                path.display()
            ),
            Err(_) => bail!(
                "{} is not valid JSON; fix it by hand, Rostr does not rewrite it",
                path.display()
            ),
        },
    };
    Ok(Settings { path, text, root })
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".rostr.")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("cannot write in {}", dir.display()))?;
    tmp.write_all(text.as_bytes())?;
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(tmp.path(), meta.permissions());
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn write_settings(s: &Settings, root: Map<String, Value>) -> Result<()> {
    // Claude Code or an editor may have changed the file meanwhile; never clobber it.
    let now = fs::read_to_string(&s.path).ok();
    if now != s.text {
        bail!(
            "{} changed while Rostr was editing it; nothing was written, run the command again",
            s.path.display()
        );
    }
    let text = serde_json::to_string_pretty(&Value::Object(root))? + "\n";
    write_atomic(&s.path, &text).map_err(|e| {
        e.context(format!(
            "cannot write {}; check its permissions, or add the statusLine by hand",
            s.path.display()
        ))
    })
}

fn load_state(path: &Path) -> Option<StatuslineState> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Refuses settings where a Rostr status line could not run or would be overridden.
fn check_policy(opts: &InitOptions, user: &Map<String, Value>) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let truthy = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_bool) == Some(true);
    for path in &opts.managed {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                warnings.push(format!(
                    "cannot read managed settings {}; managed policy was not checked",
                    path.display()
                ));
                continue;
            }
        };
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else {
            warnings.push(format!(
                "cannot parse managed settings {}; managed policy was not checked",
                path.display()
            ));
            continue;
        };
        if truthy(&m, "disableAllHooks") {
            bail!(
                "managed settings {} set disableAllHooks, which also disables any status line. \
                 Nothing was changed. Ask your Claude Code administrator, or use \
                 `rostr quota import` with an export you create yourself.",
                path.display()
            );
        }
        if m.contains_key("statusLine") {
            bail!(
                "managed settings {} define statusLine, which overrides user settings. \
                 Nothing was changed. Ask your administrator to wrap the managed command \
                 with `rostr quota statusline`.",
                path.display()
            );
        }
        if truthy(&m, "allowManagedHooksOnly") && !opts.force {
            bail!(
                "managed settings {} set allowManagedHooksOnly; your organization restricts \
                 user hooks and may not run a user status line. Nothing was changed. Ask your \
                 administrator, or re-run with --force to install it anyway.",
                path.display()
            );
        }
    }
    if truthy(user, "disableAllHooks") {
        bail!(
            "{} sets disableAllHooks, which also disables the status line. Nothing was changed. \
             Set it to false (Rostr will not change it for you), then run init again.",
            opts.settings_path().display()
        );
    }
    Ok(warnings)
}

fn command_of(status: &Value) -> Result<&str> {
    let obj = status
        .as_object()
        .context("statusLine is not an object; fix it by hand")?;
    match obj.get("type").and_then(Value::as_str) {
        Some("command") | None => {}
        Some(other) => {
            bail!("statusLine type `{other}` is not supported; only `command` can be wrapped")
        }
    }
    obj.get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .context("statusLine has no command; fix it by hand")
}

/// Installs or updates the wrapper. Returns the outcome and non-fatal warnings.
pub fn init(opts: &InitOptions) -> Result<(InitOutcome, Vec<String>)> {
    if !valid_name(&opts.instance) {
        bail!("invalid instance name; use letters, digits, `-`, `_` or `.`");
    }
    if !valid_name(&opts.account) {
        bail!("invalid account label; use an opaque name such as `personal`, never an email");
    }
    let settings = read_settings(&opts.settings_path())?;
    let mut warnings = check_policy(opts, &settings.root)?;
    let state_path = opts.state_path();
    let old_state = load_state(&state_path);

    let current = settings.root.get("statusLine");
    let (previous, outcome) = match current {
        None => (None, InitOutcome::Installed),
        Some(status) => {
            let cmd = command_of(status)?;
            if is_rostr_command(cmd) {
                let previous = match &old_state {
                    Some(s) => s.previous.clone(),
                    None => {
                        warnings.push(format!(
                            "{} is missing; the previous status line cannot be restored",
                            state_path.display()
                        ));
                        None
                    }
                };
                (previous, InitOutcome::Updated)
            } else {
                (
                    Some(status.clone()),
                    InitOutcome::Wrapped {
                        previous: cmd.to_string(),
                    },
                )
            }
        }
    };
    // Never chain one Rostr wrapper into another.
    let previous = previous.filter(|p| command_of(p).is_ok_and(|c| !is_rostr_command(c)));

    let mut status = match current {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::from_iter([("type".to_string(), json!("command"))]),
    };
    status.insert("type".into(), json!("command"));
    status.insert("command".into(), json!(opts.command()));
    let mut root = settings.root.clone();
    root.insert("statusLine".into(), Value::Object(status));

    let state = StatuslineState {
        version: 1,
        instance: opts.instance.clone(),
        account: opts.account.clone(),
        settings: settings.path.clone(),
        snapshot: opts.snapshot_path(),
        previous,
    };
    if root == settings.root && old_state.as_ref() == Some(&state) {
        return Ok((InitOutcome::Unchanged, warnings));
    }
    if opts.dry_run {
        return Ok((outcome, warnings));
    }
    // State first: the wrapper must find the previous command as soon as it runs.
    write_atomic(&state_path, &(serde_json::to_string_pretty(&state)? + "\n"))?;
    let backup = opts.backup_path();
    if let (Some(text), false) = (&settings.text, backup.exists()) {
        write_atomic(&backup, text)?;
    }
    write_settings(&settings, root)?;
    Ok((outcome, warnings))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallOutcome {
    /// The previous status line command was restored.
    Restored { command: String },
    /// Rostr installed the only status line, so `statusLine` was removed.
    Removed,
    /// The current status line is not Rostr's; settings were left alone.
    NotInstalled,
}

/// Restores the previous status line and removes the install state.
/// Snapshots and the settings backup are kept.
pub fn uninstall(opts: &InitOptions) -> Result<UninstallOutcome> {
    let settings = read_settings(&opts.settings_path())?;
    let state_path = opts.state_path();
    let state = load_state(&state_path);
    let ours = settings
        .root
        .get("statusLine")
        .and_then(|s| command_of(s).ok())
        .is_some_and(is_rostr_command);
    let outcome = if !ours {
        UninstallOutcome::NotInstalled
    } else {
        let mut root = settings.root.clone();
        let previous = state.and_then(|s| s.previous);
        let outcome = match &previous {
            Some(p) => UninstallOutcome::Restored {
                command: command_of(p).unwrap_or_default().to_string(),
            },
            None => UninstallOutcome::Removed,
        };
        match previous {
            Some(p) => root.insert("statusLine".into(), p),
            None => root.shift_remove("statusLine"),
        };
        if !opts.dry_run {
            write_settings(&settings, root)?;
        }
        outcome
    };
    if !opts.dry_run {
        match fs::remove_file(&state_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("cannot remove {}", state_path.display()));
            }
        }
    }
    Ok(outcome)
}

/// Builds the version 1 `claude_statusline` envelope from native status line stdin.
/// Returns `None` when the input has no usable rate-limit window (nothing is written).
/// Only `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` are kept.
pub fn envelope(
    input: &[u8],
    account: &str,
    instance: &str,
    observed_at: Timestamp,
) -> Option<Value> {
    if input.len() > MAX_PARSE {
        return None;
    }
    let v: Value = serde_json::from_slice(input).ok()?;
    let limits = v.get("rate_limits")?.as_object()?;
    let mut windows = Map::new();
    for key in ["five_hour", "seven_day"] {
        let Some(w) = limits.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(used) = w.get("used_percentage").filter(|u| u.is_number()) else {
            continue;
        };
        let mut out = Map::from_iter([("used_percentage".to_string(), used.clone())]);
        if let Some(reset) = w.get("resets_at").and_then(Value::as_i64) {
            out.insert("resets_at".into(), json!(reset));
        }
        windows.insert(key.into(), Value::Object(out));
    }
    if windows.is_empty() {
        return None;
    }
    let raw = v
        .pointer("/model/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = crate::discover::resolve_claude_model(raw)
        .into_iter()
        .next()
        .filter(|m| crate::model_id::validate_id(m).is_ok() && label(m))
        .unwrap_or_else(|| "anthropic/unknown".into());
    let raw_model = if label(raw) {
        raw.to_string()
    } else {
        model.trim_start_matches("anthropic/").to_string()
    };
    let target = json!({
        "agent": "claude-code",
        "instance": instance,
        "model": model,
        "route": "anthropic-subscription",
        "account": account,
        "raw_provider": "anthropic",
        "raw_model": raw_model,
        "profile": null,
        "billing_mode": "subscription",
    });
    let rate_limits = Value::Object(windows);
    let id = digest(serde_json::to_vec(&json!([observed_at, &target, &rate_limits])).ok()?);
    Some(json!({
        "version": 1,
        "claude_statusline": {
            "event_id": format!("statusline-{}", &id[..32]),
            "observed_at": observed_at,
            "target": target,
            "statusline": {"rate_limits": rate_limits},
        }
    }))
}

fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
}

/// A short line for Claude when no previous status line exists.
pub fn minimal_line(input: &[u8]) -> String {
    let v: Value = serde_json::from_slice(input).unwrap_or(Value::Null);
    let mut parts = vec![
        v.pointer("/model/display_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() < 64 && !s.chars().any(char::is_control))
            .unwrap_or("Claude")
            .to_string(),
    ];
    for (key, name) in [("five_hour", "5h"), ("seven_day", "7d")] {
        if let Some(n) = v
            .pointer(&format!("/rate_limits/{key}/used_percentage"))
            .and_then(Value::as_f64)
        {
            parts.push(format!("{name} {}%", n.round()));
        }
    }
    parts.join(" · ")
}

/// The status line wrapper Claude Code runs. Never fails: a Rostr problem must
/// not break the user's status bar. Returns the exit code to use.
pub fn run_wrapper(state_path: &Path) -> i32 {
    let mut input = Vec::new();
    let _ = std::io::stdin().take(MAX_INPUT).read_to_end(&mut input);
    let state = load_state(state_path);
    if let Some(state) = &state
        && let Some(env) = envelope(&input, &state.account, &state.instance, now())
        && let Ok(text) = serde_json::to_string_pretty(&env)
    {
        let _ = write_atomic(&state.snapshot, &(text + "\n"));
    }
    let previous = state
        .as_ref()
        .and_then(|s| s.previous.as_ref())
        .and_then(|p| command_of(p).ok())
        .filter(|c| !is_rostr_command(c))
        .map(str::to_string);
    match previous {
        Some(cmd) if std::env::var_os(RECURSION_VAR).is_none() => run_previous(&cmd, input),
        _ => {
            println!("{}", minimal_line(&input));
            0
        }
    }
}

fn run_previous(cmd: &str, input: Vec<u8>) -> i32 {
    use std::process::{Command, Stdio};
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let child = command
        .env(RECURSION_VAR, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    let Ok(mut child) = child else {
        println!("{}", minimal_line(&input));
        return 0;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A thread avoids a deadlock when the command writes before reading.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// Reads the fixed snapshot files that the wrapper writes. One report per instance.
pub struct ClaudeStatuslineProbe {
    pub path: Option<PathBuf>,
}

/// One probe per snapshot file under `<data dir>/claude-statusline/`, or a single
/// probe reporting `not_installed`/`no_observation` when there is none.
pub fn statusline_probes(env: &Env) -> Vec<Box<dyn QuotaProbe>> {
    let dir = snapshot_dir(&crate::scope::data_dir(&env.home, |k| {
        env.var(k).map(String::from)
    }));
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(512)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
        })
        .collect();
    files.sort();
    files.truncate(MAX_SNAPSHOTS);
    if files.is_empty() {
        return vec![Box::new(ClaudeStatuslineProbe { path: None })];
    }
    files
        .into_iter()
        .map(|p| Box::new(ClaudeStatuslineProbe { path: Some(p) }) as Box<dyn QuotaProbe>)
        .collect()
}

impl QuotaProbe for ClaudeStatuslineProbe {
    fn id(&self) -> ProbeId {
        PROBE_ID.into()
    }
    fn capabilities(&self) -> ProbeCapabilities {
        ProbeCapabilities {
            local_snapshot: true,
            subscription: true,
            quota: true,
            rate_limit: true,
            schema_versions: vec![1],
        }
    }
    fn bindings(&self, env: &Env) -> Result<BindingReport, ProbeError> {
        match &self.path {
            Some(path) => LocalSnapshotProbe { path: path.clone() }.bindings(env),
            None => Ok(BindingReport::default()),
        }
    }
    fn observe(
        &self,
        ctx: &ProbeContext<'_>,
        request: &ProbeRequest,
    ) -> Result<ProbeReport, ProbeError> {
        let Some(path) = &self.path else {
            let installed = ctx.env.dir("CLAUDE_CONFIG_DIR", ".claude").is_dir();
            return finish(empty(PROBE_ID, installed, request), false);
        };
        // A damaged snapshot must not abort refresh of the other observers.
        LocalSnapshotProbe { path: path.clone() }
            .observe(ctx, request)
            .or_else(|_| finish(empty(PROBE_ID, true, request), true))
    }
}

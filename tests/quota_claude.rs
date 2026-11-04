//! `rostr quota init --claude`, the status line wrapper, and refresh of its snapshot.
use rostr::quota::claude;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const NO_MANAGED: &str = "/nonexistent/rostr-test/managed-settings.json";

fn rostr(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rostr"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .unwrap()
}
fn init(home: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "quota",
        "init",
        "--claude",
        "--managed-settings",
        NO_MANAGED,
    ];
    args.extend_from_slice(extra);
    rostr(home, &args)
}
fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}
fn settings(home: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(settings_path(home)).unwrap()).unwrap()
}
fn write_settings(home: &Path, v: &Value) {
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        settings_path(home),
        serde_json::to_string_pretty(v).unwrap(),
    )
    .unwrap();
}
fn snapshot(home: &Path) -> PathBuf {
    home.join(".local/share/rostr/claude-statusline/default.json")
}
fn state(home: &Path) -> PathBuf {
    home.join(".config/rostr/claude-statusline/default.json")
}
/// Runs the installed status line command the way Claude Code does: one shell
/// command with the session JSON on stdin.
fn run_statusline(home: &Path, input: &Value) -> Output {
    let cmd = settings(home)["statusLine"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn session(five: f64, seven: f64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({
        "session_id": "secret-session",
        "cwd": "/home/someone/private",
        "transcript_path": "/home/someone/.claude/projects/x.jsonl",
        "model": {"id": "claude-sonnet-4-5", "display_name": "Sonnet"},
        "context_window": {"used_percentage": 12},
        "rate_limits": {
            "five_hour": {"used_percentage": five, "resets_at": now + 3600},
            "seven_day": {"used_percentage": seven, "resets_at": now + 86400}
        }
    })
}
fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}
fn user_script(home: &Path) -> &'static str {
    let path = home.join(".claude/statusline.sh");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // Echo the model from stdin to prove the same JSON is re-fed; exit 3 to prove
    // the exit status passes through.
    fs::write(
        &path,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *claude-sonnet-4-5*) echo user-line ok;; *) echo user-line missing;; esac\nexit 3\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    "~/.claude/statusline.sh"
}

#[test]
fn wraps_existing_command_idempotently_and_refeeds_stdin() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let previous = user_script(home);
    write_settings(
        home,
        &json!({
            "model": "sonnet",
            "statusLine": {"type": "command", "command": previous, "padding": 0},
            "env": {"A": "1"}
        }),
    );
    let out = ok(&init(home, &[]));
    assert!(out.contains("Wrapped the existing status line"), "{out}");
    let s = settings(home);
    let cmd = s["statusLine"]["command"].as_str().unwrap();
    assert!(claude::is_rostr_command(cmd));
    assert_eq!(s["statusLine"]["padding"], 0);
    // Key order and unrelated keys are preserved.
    let keys: Vec<_> = s.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["model", "statusLine", "env"]);
    assert!(
        home.join(".config/rostr/claude-statusline/default.settings-backup.json")
            .exists()
    );

    // Re-running never stacks wrappers.
    let first = fs::read_to_string(settings_path(home)).unwrap();
    let out = ok(&init(home, &[]));
    assert!(out.contains("up to date"), "{out}");
    assert_eq!(fs::read_to_string(settings_path(home)).unwrap(), first);
    let st: Value = serde_json::from_str(&fs::read_to_string(state(home)).unwrap()).unwrap();
    assert_eq!(st["previous"]["command"], previous);
    assert_eq!(cmd.matches(claude::MARKER).count(), 1);

    // Changing the account updates in place and keeps the original previous command.
    let out = ok(&init(home, &["--account", "work"]));
    assert!(out.contains("Updated"), "{out}");
    let st: Value = serde_json::from_str(&fs::read_to_string(state(home)).unwrap()).unwrap();
    assert_eq!(st["previous"]["command"], previous);
    assert_eq!(st["account"], "work");
    // A later plain re-run keeps the chosen account.
    assert!(ok(&init(home, &[])).contains("up to date"));

    let out = run_statusline(home, &session(40.0, 70.0));
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "user-line ok");

    // Only the rate-limit subset and the derived identity are stored.
    let text = fs::read_to_string(snapshot(home)).unwrap();
    for secret in [
        "secret-session",
        "/home/someone",
        "transcript",
        "context_window",
    ] {
        assert!(!text.contains(secret), "{secret} leaked into the snapshot");
    }
    let snap: Value = serde_json::from_str(&text).unwrap();
    let target = &snap["claude_statusline"]["target"];
    assert_eq!(target["account"], "work");
    assert_eq!(target["model"], "anthropic/claude-sonnet-4-5");
    assert_eq!(target["billing_mode"], "subscription");

    // refresh --local reads the fixed snapshot path; query sees the result.
    let out = ok(&rostr(
        home,
        &[
            "quota",
            "refresh",
            "--local",
            "--agent",
            "claude-code",
            "--json",
        ],
    ));
    let reports: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 1);
    assert_eq!(reports[0]["probe_id"], "claude-statusline");
    assert_eq!(reports[0]["completeness"], "complete");
    assert_eq!(reports[0]["buckets"][0]["remaining"], "60.000000");
    assert_eq!(reports[0]["buckets"][1]["remaining"], "30.000000");
    assert_eq!(reports[0]["buckets"][0]["confidence"], "user_supplied");
    let out = ok(&rostr(
        home,
        &[
            "quota",
            "query",
            "--agent",
            "claude-code",
            "--metric",
            "subscription_usage",
            "--unit",
            "percent",
            "--min-remaining",
            "20",
            "--json",
        ],
    ));
    let targets: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(targets[0]["target"]["account"], "work");
    assert_eq!(targets[0]["eligibility"], "eligible");

    // The same file still imports explicitly.
    ok(&rostr(
        home,
        &["quota", "import", snapshot(home).to_str().unwrap()],
    ));

    // Uninstall restores the previous statusLine object exactly.
    let out = ok(&init(home, &["--uninstall"]));
    assert!(out.contains("Restored"), "{out}");
    assert_eq!(
        settings(home)["statusLine"],
        json!({"type": "command", "command": previous, "padding": 0})
    );
    assert!(!state(home).exists());
    assert!(snapshot(home).exists());
    // A second uninstall leaves the user's command alone.
    assert!(ok(&init(home, &["--uninstall"])).contains("not Rostr's"));
    assert_eq!(settings(home)["statusLine"]["command"], previous);
}

#[test]
fn installs_minimal_status_line_without_previous_command() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    // No settings.json at all: init creates it.
    let out = ok(&init(home, &[]));
    assert!(out.contains("Installed a minimal"), "{out}");
    let out = run_statusline(home, &session(25.4, 50.0));
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "Sonnet · 5h 25% · 7d 50%"
    );
    assert!(snapshot(home).exists());

    // Without rate limits (API key users, before the first response) nothing is
    // written and the previous observation is kept.
    let before = fs::read_to_string(snapshot(home)).unwrap();
    let out = run_statusline(home, &json!({"model": {"display_name": "Opus"}}));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Opus");
    assert_eq!(fs::read_to_string(snapshot(home)).unwrap(), before);
    // Garbage on stdin still prints a line and exits 0.
    let out = run_statusline(home, &json!("not an object"));
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Claude");

    // Uninstall removes the key Rostr added.
    assert!(ok(&init(home, &["--uninstall"])).contains("Removed"));
    assert!(settings(home).get("statusLine").is_none());
}

#[test]
fn refresh_without_snapshot_reports_no_observation() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let out = ok(&rostr(
        home,
        &[
            "quota",
            "refresh",
            "--local",
            "--agent",
            "claude-code",
            "--json",
        ],
    ));
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r[0]["completeness"], "not_installed");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let out = ok(&rostr(
        home,
        &[
            "quota",
            "refresh",
            "--local",
            "--agent",
            "claude-code",
            "--json",
        ],
    ));
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r[0]["completeness"], "no_observation");
    assert!(r[0]["buckets"].as_array().unwrap().is_empty());

    // A damaged snapshot does not abort refresh of the other observers.
    fs::create_dir_all(snapshot(home).parent().unwrap()).unwrap();
    fs::write(snapshot(home), "{broken").unwrap();
    let out = ok(&rostr(home, &["quota", "refresh", "--local", "--json"]));
    let r: Value = serde_json::from_str(&out).unwrap();
    let claude = r
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["probe_id"] == "claude-statusline")
        .unwrap();
    assert_eq!(claude["completeness"], "unsupported");
}

#[test]
fn rewraps_a_command_the_user_set_after_install() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    ok(&init(home, &[]));
    let mut s = settings(home);
    s["statusLine"]["command"] = json!("echo replaced");
    write_settings(home, &s);
    let out = ok(&init(home, &[]));
    assert!(out.contains("Wrapped"), "{out}");
    let out = run_statusline(home, &session(1.0, 2.0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "replaced");
}

#[test]
fn refuses_unsafe_settings_without_writing() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let managed = home.join("managed-settings.json");
    let cases = [
        (json!({"disableAllHooks": true}), "disableAllHooks"),
        (
            json!({"statusLine": {"type": "command", "command": "corp"}}),
            "define statusLine",
        ),
        (
            json!({"allowManagedHooksOnly": true}),
            "allowManagedHooksOnly",
        ),
    ];
    write_settings(home, &json!({"model": "opus"}));
    let original = fs::read_to_string(settings_path(home)).unwrap();
    for (policy, message) in cases {
        fs::write(&managed, policy.to_string()).unwrap();
        let out = rostr(
            home,
            &[
                "quota",
                "init",
                "--claude",
                "--managed-settings",
                managed.to_str().unwrap(),
            ],
        );
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(message) && err.contains("Nothing was changed"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(settings_path(home)).unwrap(), original);
        assert!(!state(home).exists());
    }
    // allowManagedHooksOnly can be overridden explicitly.
    let out = rostr(
        home,
        &[
            "quota",
            "init",
            "--claude",
            "--force",
            "--managed-settings",
            managed.to_str().unwrap(),
        ],
    );
    ok(&out);
    ok(&init(home, &["--uninstall"]));

    // The user's own disableAllHooks is never flipped by Rostr.
    write_settings(home, &json!({"disableAllHooks": true}));
    let original = fs::read_to_string(settings_path(home)).unwrap();
    let out = init(home, &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("disableAllHooks"));
    assert_eq!(fs::read_to_string(settings_path(home)).unwrap(), original);

    // Invalid JSON and unsupported statusLine types are left for the user to fix.
    for text in [
        "{ \"model\": \"opus\", }".to_string(),
        json!({"statusLine": {"type": "static", "text": "x"}}).to_string(),
        json!({"statusLine": "echo"}).to_string(),
    ] {
        fs::write(settings_path(home), &text).unwrap();
        let out = init(home, &[]);
        assert!(!out.status.success(), "{text}");
        assert_eq!(fs::read_to_string(settings_path(home)).unwrap(), text);
    }
}

#[test]
fn dry_run_and_claude_config_dir_are_respected() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    let dir = home.join("alt-claude");
    fs::create_dir_all(&dir).unwrap();
    let out = ok(&init(
        home,
        &["--claude-config-dir", dir.to_str().unwrap(), "--dry-run"],
    ));
    assert!(out.contains("Dry run"), "{out}");
    assert!(!dir.join("settings.json").exists());
    assert!(!home.join(".config/rostr").exists());

    let out = Command::new(env!("CARGO_BIN_EXE_rostr"))
        .args([
            "quota",
            "init",
            "--claude",
            "--managed-settings",
            NO_MANAGED,
        ])
        .env_clear()
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", &dir)
        .output()
        .unwrap();
    let text = ok(&out);
    let instance = claude::instance_for(home, &dir);
    assert!(instance.starts_with("dir-"));
    assert!(
        text.contains(&format!("account:  claude-{instance}")),
        "{text}"
    );
    let s: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
    assert!(claude::is_rostr_command(
        s["statusLine"]["command"].as_str().unwrap()
    ));
    assert!(!settings_path(home).exists());

    // Unsafe names never reach a path.
    let out = init(home, &["--instance", "../x"]);
    assert!(!out.status.success());
    let out = init(home, &["--account", "me@example.com"]);
    assert!(!out.status.success());
}

#[test]
fn envelope_keeps_only_known_windows_and_falls_back_for_models() {
    let input = json!({
        "model": {"id": "claude-opus-4-1[1m]"},
        "rate_limits": {
            "five_hour": {"used_percentage": 10.5, "resets_at": 1793761200, "extra": 1},
            "seven_day": {"resets_at": 1794358800},
            "future_window": {"used_percentage": 1}
        }
    });
    let v = claude::envelope(
        input.to_string().as_bytes(),
        "acct",
        "default",
        1793754000000,
    )
    .unwrap();
    let e = &v["claude_statusline"];
    assert_eq!(e["target"]["model"], "anthropic/claude-opus-4-1");
    assert_eq!(e["target"]["raw_model"], "claude-opus-4-1");
    assert_eq!(
        e["statusline"]["rate_limits"],
        json!({"five_hour": {"used_percentage": 10.5, "resets_at": 1793761200}})
    );
    // The written envelope round-trips through the import parser.
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s.json");
    fs::write(&path, v.to_string()).unwrap();
    let report = rostr::quota::import_file(&path, 1793754000001).unwrap();
    assert_eq!(report.buckets.len(), 1);
    assert_eq!(report.buckets[0].remaining.as_deref(), Some("89.500000"));
    assert_eq!(report.buckets[0].reset_at, Some(1793761200000));

    let input = json!({"rate_limits": {"five_hour": {"used_percentage": 1}}});
    let v = claude::envelope(input.to_string().as_bytes(), "acct", "default", 1).unwrap();
    assert_eq!(
        v["claude_statusline"]["target"]["model"],
        "anthropic/unknown"
    );
    assert!(claude::envelope(b"{\"rate_limits\": {}}", "acct", "default", 1).is_none());
    assert!(claude::envelope(b"[]", "acct", "default", 1).is_none());
}

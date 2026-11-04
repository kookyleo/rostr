use std::fs;
use std::path::Path;
use std::process::Command;

fn discover(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rostr"))
        .arg("discover")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .output()
        .unwrap()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn rerun_is_idempotent_and_keeps_hand_edits() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".codex")).unwrap();
    fs::write(h.join(".codex/config.toml"), "model = \"gpt-5.3-codex\"\n").unwrap();
    fs::create_dir_all(h.join(".grok")).unwrap();
    fs::write(h.join(".grok/config.toml"), "model = \"grok-4\"\n").unwrap();
    let scope = h.join(".config/rostr/scope.yaml");
    let db = h.join(".local/share/rostr/rostr.db");

    // First run: one pattern per discovered provider, no exceptions.
    let out = discover(h, &["--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = fs::read_to_string(&scope).unwrap();
    assert!(
        first.contains("patterns:\n- openai/*\n- xai/*\n"),
        "{first}"
    );
    assert!(first.contains("exceptions: []"), "{first}");
    assert!(db.is_file());

    assert!(discover(h, &["--yes"]).status.success());
    assert_eq!(fs::read_to_string(&scope).unwrap(), first);

    // Hand edit: exclude one model and pin one the scan cannot find.
    let edited = first.replace(
        "exceptions: []",
        "exceptions:\n- id: anthropic/claude-opus-4-1\n  selected: true\n- id: xai/grok-4\n  selected: false",
    );
    fs::write(&scope, &edited).unwrap();
    assert!(discover(h, &["--yes"]).status.success());
    let after = fs::read_to_string(&scope).unwrap();
    assert_eq!(after, edited);

    // The listing shows agents and reasons; the Codex config is now gone.
    fs::remove_file(h.join(".codex/config.toml")).unwrap();
    let text = stdout(&discover(h, &["--dry-run"]));
    let line = |id: &str| {
        text.lines()
            .find(|l| l.contains(&format!(" {id} ")))
            .unwrap_or_else(|| panic!("{id} not listed:\n{text}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert!(line("xai/grok-4").ends_with("(Grok CLI)"), "{text}");
    assert!(
        line("openai/gpt-5.3-codex").ends_with("(Codex) missing"),
        "{text}"
    );
    assert!(
        line("anthropic/claude-opus-4-1").ends_with("(scope.yaml) missing"),
        "{text}"
    );
    assert_eq!(fs::read_to_string(&scope).unwrap(), after);
}

#[test]
fn one_model_from_several_agents() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".claude")).unwrap();
    fs::write(h.join(".claude/settings.json"), r#"{"model": "opus"}"#).unwrap();
    fs::create_dir_all(h.join(".config/opencode")).unwrap();
    fs::write(
        h.join(".config/opencode/opencode.json"),
        r#"{"model": "anthropic/claude-opus-5-5"}"#,
    )
    .unwrap();
    fs::create_dir_all(h.join(".gemini")).unwrap();
    fs::write(
        h.join(".gemini/settings.json"),
        r#"{"model": "gemini-2.5-pro"}"#,
    )
    .unwrap();
    let text = stdout(&discover(h, &["--dry-run"]));
    let opus = text
        .lines()
        .find(|l| l.contains(" anthropic/claude-opus-5-5 "))
        .unwrap();
    assert!(opus.ends_with("(Claude Code, OpenCode)  new"), "{text}");
    let gemini = text
        .lines()
        .find(|l| l.contains(" google/gemini-2.5-pro "))
        .unwrap();
    assert!(gemini.ends_with("(Gemini CLI)  new"), "{text}");
}

#[test]
fn invalid_scope_is_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let scope = home.path().join(".config/rostr/scope.yaml");
    fs::create_dir_all(scope.parent().unwrap()).unwrap();
    fs::write(&scope, "version: 1\npatterns: [oops\n").unwrap();
    let out = discover(home.path(), &["--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot parse"));
    assert_eq!(
        fs::read_to_string(&scope).unwrap(),
        "version: 1\npatterns: [oops\n"
    );
    assert!(!home.path().join(".local/share/rostr/rostr.db").exists());
}

#[test]
fn tui_refuses_without_terminal() {
    let home = tempfile::tempdir().unwrap();
    let out = discover(home.path(), &[]);
    assert!(!out.status.success());
    assert!(!home.path().join(".config/rostr/scope.yaml").exists());
}

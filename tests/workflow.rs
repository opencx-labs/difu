use anyhow::{Context, Result, ensure};
use difu::{
    app::{Action, App, View},
    model::PrKey,
    process::{self, Cancel},
    storage::{Config, Storage},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn git(path: &Path, args: &[&str]) -> Result<String> {
    process::checked(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Difu Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args),
        &Cancel::default(),
    )
    .map(|s| s.trim().to_owned())
}
fn wait(app: &mut App, ready: impl Fn(&App) -> bool) -> Result<()> {
    let start = Instant::now();
    while !ready(app) {
        ensure!(
            start.elapsed() < Duration::from_secs(15),
            "Workflow timed out: {} {:?}",
            app.notice,
            app.review().and_then(|r| r.guide_error.as_ref())
        );
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn render(app: &mut App, width: u16) -> Result<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 36))?;
    terminal.draw(|f| difu::ui::draw(f, app))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>())
}

#[test]
fn scripted_workflow() -> Result<()> {
    if let Ok(fixture) = std::env::var("DIFU_TEST_FIXTURE") {
        return exercise(Path::new(&fixture));
    }
    // The subprocess owns PATH changes; parallel tests retain their environment.
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let clone = root.join("clone");
    fs::create_dir(&clone)?;
    git(&clone, &["init"])?;
    git(
        &clone,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/project.git",
        ],
    )?;
    fs::write(clone.join("main.rs"), "fn main() {\n    old();\n}\n")?;
    git(&clone, &["add", "."])?;
    git(&clone, &["commit", "-m", "base"])?;
    let base = git(&clone, &["rev-parse", "HEAD"])?;
    fs::write(clone.join("main.rs"), "fn main() {\n    new();\n}\n")?;
    git(&clone, &["commit", "-am", "head"])?;
    let head = git(&clone, &["rev-parse", "HEAD"])?;
    fs::write(clone.join("main.rs"), "precious uncommitted work\n")?;
    fs::write(
        root.join("revisions.json"),
        serde_json::to_vec(&serde_json::json!({"head":head,"base":base}))?,
    )?;
    for (name, script) in [
        ("gh", include_str!("fixtures/gh.py")),
        ("codex", include_str!("fixtures/codex.py")),
    ] {
        let path = root.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let path = std::env::join_paths(std::iter::once(root.to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH unavailable")?),
    ))?;
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", "scripted_workflow", "--nocapture"])
        .env("DIFU_TEST_FIXTURE", root)
        .env("PATH", path)
        .output()?;
    ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(clone.join("main.rs"))?,
        "precious uncommitted work\n"
    );
    assert_eq!(
        git(&clone, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        1
    );
    Ok(())
}

fn exercise(root: &Path) -> Result<()> {
    let storage = Storage {
        config: root.join("config.json"),
        cache: root.to_owned(),
    };
    let mut config = Config::default();
    config
        .repositories
        .insert("example/project".into(), root.join("clone"));
    let mut app = App::new(storage.clone(), config.clone());
    app.start(None);
    wait(&mut app, |a| {
        a.review()
            .is_some_and(|r| !r.timeline.is_empty() && !r.checks.is_empty())
    })?;
    assert!(
        app.review()
            .is_some_and(|r| r.generation.is_none() && r.snapshot.is_none())
    );
    let overview = render(&mut app, 160)?;
    assert!(overview.contains("Describe the behavior"));
    assert!(overview.contains("ACTIVITY"));
    assert!(overview.contains("CHECKS"));
    assert!(
        app.review()
            .is_some_and(|r| r.checks.first().is_some_and(|c| c.state == "pending"))
    );
    app.action(Action::OpenPr);
    wait(&mut app, |a| a.review().is_some_and(|r| r.guide.is_some()))?;
    let wide = render(&mut app, 180)?;
    assert!(wide.contains("Use the new behavior"));
    assert!(app.document.as_ref().is_some_and(|d| d.guide_columns));
    render(&mut app, 80)?;
    assert!(app.document.as_ref().is_some_and(|d| !d.guide_columns));
    assert!(!app.config.unified);
    render(&mut app, 180)?;
    assert!(app.document.as_ref().is_some_and(|d| d.guide_columns));
    app.action(Action::SetView(View::Diff));
    assert!(render(&mut app, 140)?.contains("main.rs"));
    app.action(Action::Models);
    wait(&mut app, |a| !a.models_loading)?;
    assert_eq!(app.model_options("luna").len(), 1);
    assert_eq!(app.model_options("sol").len(), 2);
    app.shutdown();
    // A new session must reuse a valid disk cache without another Codex turn.
    let mut reopened = App::new(storage, config);
    reopened.start(Some(PrKey::from_url(
        "https://github.com/example/project/pull/1",
    )?));
    wait(&mut reopened, |a| {
        a.review().is_some_and(|r| r.guide.is_some())
    })?;
    assert_eq!(fs::read_to_string(root.join("turns"))?, "turn\n");
    reopened.shutdown();
    Ok(())
}

use anyhow::{Context, Result, ensure};
use difu::{
    conflicts,
    model::{ModelChoice, PrKey},
    process::Cancel,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, sync::Arc};

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn script(root: &Path, name: &str, content: &str) -> Result<()> {
    let path = root.join(name);
    fs::write(&path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[test]
fn disposable_conflict_resolution_validates_before_pushing() -> Result<()> {
    if let Ok(root) = std::env::var("DIFU_CONFLICT_FIXTURE") {
        return exercise(Path::new(&root));
    }
    let fixture = tempfile::tempdir()?;
    let root = fixture.path();
    let clone = root.join("clone");
    fs::create_dir(&clone)?;
    git(&clone, &["init", "-b", "main"])?;
    git(&clone, &["config", "user.name", "Difu Test"])?;
    git(&clone, &["config", "user.email", "test@example.invalid"])?;
    git(&clone, &["config", "commit.gpgsign", "false"])?;
    git(
        &clone,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/project.git",
        ],
    )?;
    fs::write(clone.join("conflict.txt"), "old\n")?;
    fs::write(clone.join("untouched.txt"), "keep\n")?;
    fs::write(clone.join("automatic.txt"), "original\n")?;
    git(&clone, &["add", "."])?;
    git(&clone, &["commit", "-m", "initial"])?;
    git(&clone, &["checkout", "-b", "feature"])?;
    fs::write(clone.join("conflict.txt"), "PR version\n")?;
    git(&clone, &["commit", "-am", "PR"])?;
    let head = git(&clone, &["rev-parse", "HEAD"])?;
    git(&clone, &["checkout", "main"])?;
    fs::write(clone.join("conflict.txt"), "base version\n")?;
    fs::write(
        clone.join("automatic.txt"),
        "automatically merged from base\n",
    )?;
    git(&clone, &["commit", "-am", "base advance"])?;
    let base = git(&clone, &["rev-parse", "HEAD"])?;
    git(&clone, &["checkout", "feature"])?;
    fs::write(clone.join("conflict.txt"), "precious local edits\n")?;
    git(root, &["clone", "--bare", "clone", "remote"])?;
    fs::write(
        root.join("revisions.json"),
        serde_json::to_vec(&serde_json::json!({"head":head,"base":base}))?,
    )?;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?)
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .context("Git missing")?;
    script(
        root,
        "git",
        r#"#!/usr/bin/env python3
import os,sys,json
from pathlib import Path
root=Path(os.environ['DIFU_CONFLICT_FIXTURE']);args=sys.argv[1:]
if 'push' in args:
    assert '--force' not in args and not any('force-with-lease' in a or a.startswith('+') for a in args)
    with (root/'pushes').open('a') as f:f.write(json.dumps(args)+'\n')
    if (root/'mode').read_text()=='push-failure':
        print('fixture push rejected',file=sys.stderr);sys.exit(1)
    i=args.index('--')+1;assert args[i]=='https://github.com/example/project.git'
    args[i]=str(root/'remote');os.environ['GIT_ALLOW_PROTOCOL']='file'
if 'fetch' in args: raise AssertionError('all objects should already be local')
os.execv(os.environ['DIFU_REAL_GIT'],[os.environ['DIFU_REAL_GIT'],*args])
"#,
    )?;
    script(
        root,
        "gh",
        r#"#!/usr/bin/env python3
import os,sys,json
from pathlib import Path
root=Path(os.environ['DIFU_CONFLICT_FIXTURE']);r=json.loads((root/'revisions.json').read_text())
assert sys.argv[1:3]==['api','repos/example/project/pulls/1']
if (root/'moved').exists():r['base']='f'*40
print(json.dumps(dict(title='Resolve',body='',user=dict(login='test'),head=dict(sha=r['head'],ref='feature',repo=dict(full_name='example/project')),base=dict(sha=r['base'],ref='main'),state='open',merged=False,additions=1,deletions=1,changed_files=1)))
"#,
    )?;
    script(
        root,
        "codex",
        r#"#!/usr/bin/env python3
import os,sys,json,subprocess
from pathlib import Path
root=Path(os.environ['DIFU_CONFLICT_FIXTURE']);args=sys.argv[1:]
if args[0]=='mcp': print('[]');sys.exit(0)
assert args[args.index('--sandbox')+1]=='workspace-write'
assert args[args.index('--model')+1]=='gpt-6-astra'
assert 'sandbox_workspace_write.network_access=false' in args
assert any('NEVER RUN CHECKS LOCALLY' in a for a in args)
assert any('model_reasoning_effort="high"'==a for a in args)
assert os.environ['GIT_ALLOW_PROTOCOL']==''
prompt=sys.stdin.read();assert 'conflict.txt' in prompt
mode=(root/'mode').read_text()
with (root/'turns').open('a') as f:f.write(mode+'\n')
if mode=='model-failure':print('fixture model failed',file=sys.stderr);sys.exit(1)
if mode!='unresolved':Path('conflict.txt').write_text('PR and base combined\n')
if mode=='outside':Path('untouched.txt').write_text('unauthorized\n')
if mode=='automatic':Path('automatic.txt').write_text('unauthorized change to automatic merge\n')
if mode=='ignored':Path('new-file').write_text('unexpected\n')
if mode=='staged':subprocess.run([os.environ['DIFU_REAL_GIT'],'add','conflict.txt'],check=True)
if mode=='revision-race':(root/'moved').touch()
print(json.dumps(dict(type='item.completed',item=dict(type='agent_message',text='Progress: Resolved the text conflict; checks left to CI.'))))
"#,
    )?;
    let path = std::env::join_paths(std::iter::once(root.to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?),
    ))?;
    let output = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "disposable_conflict_resolution_validates_before_pushing",
            "--nocapture",
        ])
        .env("DIFU_CONFLICT_FIXTURE", root)
        .env("DIFU_REAL_GIT", real_git)
        .env("PATH", path)
        .output()?;
    ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
fn exercise(root: &Path) -> Result<()> {
    let clone = root.join("clone");
    let revisions: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("revisions.json"))?)?;
    let head = revisions
        .get("head")
        .and_then(|v| v.as_str())
        .context("Missing head")?;
    let base = revisions
        .get("base")
        .and_then(|v| v.as_str())
        .context("Missing base")?;
    let key = PrKey {
        owner: "example".into(),
        repo: "project".into(),
        number: 1,
    };
    for (mode, error) in [
        ("outside", "outside"),
        ("automatic", "outside"),
        ("ignored", "outside"),
        ("staged", "index"),
        ("unresolved", "unresolved markers"),
        ("model-failure", "model failed"),
        ("revision-race", "branch changed"),
        ("push-failure", "push rejected"),
        ("success", ""),
    ] {
        fs::write(root.join("mode"), mode)?;
        let moved = root.join("moved");
        if moved.exists() {
            fs::remove_file(moved)?;
        }
        let pushes = root.join("pushes");
        if pushes.exists() {
            fs::remove_file(&pushes)?;
        }
        let result = conflicts::resolve(
            &clone,
            &key,
            head,
            &ModelChoice::conflict_default(),
            &Cancel::default(),
            Arc::new(|_| {}),
        );
        if mode == "success" {
            let message = result?;
            assert!(message.contains("pushed"));
            let remote = root.join("remote");
            assert_eq!(
                git(&remote, &["show", "-s", "--format=%P", "feature"])?,
                format!("{head} {base}")
            );
            assert_eq!(
                git(&remote, &["show", "feature:automatic.txt"])?,
                "automatically merged from base"
            );
            assert_eq!(
                git(&remote, &["show", "feature:conflict.txt"])?,
                "PR and base combined"
            );
        } else {
            let message = format!("{:#}", result.err().context("Expected resolution failure")?);
            assert!(message.contains(error), "{mode}: {message}");
            assert_eq!(git(&root.join("remote"), &["rev-parse", "feature"])?, head);
        }
        let count = if pushes.exists() {
            fs::read_to_string(pushes)?.lines().count()
        } else {
            0
        };
        assert_eq!(
            count,
            usize::from(mode == "push-failure" || mode == "success"),
            "{mode}"
        );
        assert_eq!(
            git(&clone, &["worktree", "list", "--porcelain"])?
                .matches("worktree ")
                .count(),
            1,
            "{mode}: leaked worktree"
        );
        assert_eq!(git(&clone, &["rev-parse", "HEAD"])?, head);
        assert_eq!(
            fs::read_to_string(clone.join("conflict.txt"))?,
            "precious local edits\n"
        );
    }
    Ok(())
}

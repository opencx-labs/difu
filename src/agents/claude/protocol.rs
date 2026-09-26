//! Claude CLI wire compatibility boundary. No session policy belongs in this file.
//! Reference: anthropics/claude-agent-sdk-python, _internal/query.py and
//! _internal/transport/subprocess_cli.py. Keep protocol fixtures alongside changes.
use crate::process::{Cancel, ChildGroup};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufReader, Read, Write},
    path::Path,
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Options<'a> {
    pub cwd: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub resume: Option<&'a str>,
    pub instructions: &'a str,
    pub discovery: bool,
}
pub(super) struct Connection {
    _child: ChildGroup,
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value, String>>,
    backlog: VecDeque<Value>,
    next: u64,
    stderr: Arc<Mutex<VecDeque<u8>>>,
}
impl Connection {
    pub fn open(options: Options<'_>) -> Result<Self> {
        let mut command = Command::new("claude");
        command
            .args([
                "--print",
                "--verbose",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--replay-user-messages",
                "--permission-prompt-tool",
                "stdio",
            ])
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
            .current_dir(options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(model) = options.model {
            command.arg(format!("--model={model}"));
        }
        if let Some(effort) = options.effort {
            command.arg(format!("--effort={effort}"));
        }
        if let Some(resume) = options.resume {
            command.arg(format!("--resume={resume}"));
        }
        if !options.instructions.is_empty() {
            command
                .arg("--append-system-prompt")
                .arg(options.instructions);
        }
        if options.discovery {
            command.args([
                "--no-session-persistence",
                "--setting-sources=",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
            ]);
        } else {
            command
                .arg("--mcp-config")
                .arg(json!({"mcpServers":{"difu":{"type":"sdk","name":"difu"}}}).to_string());
        }
        let mut child = ChildGroup::spawn(&mut command)
            .context("Cannot start Claude Code; install and authenticate the claude CLI")?;
        let input = child.child.stdin.take().context("Missing Claude stdin")?;
        let output = child.child.stdout.take().context("Missing Claude stdout")?;
        // Keep a bounded diagnostic tail while draining so stderr cannot block stdout.
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(mut stderr) = child.child.stderr.take() {
            let tail = stderr_tail.clone();
            thread::spawn(move || {
                let mut buffer = [0; 2048];
                while let Ok(count) = stderr.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    if let Ok(mut tail) = tail.lock() {
                        tail.extend(buffer.iter().take(count).copied());
                        while tail.len() > 8192 {
                            tail.pop_front();
                        }
                    }
                }
            });
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let result = crate::agents::client::read_line(&mut reader)
                    .and_then(|line| {
                        if !line.trim_start().starts_with('{') {
                            return Ok(None);
                        }
                        Ok(Some(serde_json::from_str(&line)?))
                    })
                    .map_err(|error| format!("Claude stream: {error:#}"));
                let frame = match result {
                    Ok(None) => continue,
                    Ok(Some(value)) => Ok(value),
                    Err(error) => Err(error),
                };
                let failed = frame.is_err();
                if sender.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            _child: child,
            input,
            output: receiver,
            backlog: VecDeque::new(),
            next: 0,
            stderr: stderr_tail,
        })
    }
    fn disconnected(&self, message: &str) -> anyhow::Error {
        let detail = self
            .stderr
            .lock()
            .ok()
            .map(|tail| {
                String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<_>>())
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default();
        anyhow::anyhow!(
            "{message}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        )
    }
    pub fn write(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
    pub fn request(&mut self, request: Value) -> Result<String> {
        self.next = self.next.saturating_add(1);
        let id = format!("difu-control-{}", self.next);
        self.write(&json!({"type":"control_request","request_id":id,"request":request}))?;
        Ok(id)
    }
    pub fn reply(&mut self, id: &Value, response: Value) -> Result<()> {
        self.write(&json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}}))
    }
    pub fn reject(&mut self, id: &Value, error: &str) -> Result<()> {
        self.write(&json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":error}}))
    }
    pub fn call(
        &mut self,
        request: Value,
        cancel: &Cancel,
        mut handle: impl FnMut(&Value) -> Result<Value>,
    ) -> Result<Value> {
        let id = self.request(request)?;
        let start = Instant::now();
        loop {
            cancel.check()?;
            ensure!(
                start.elapsed() < Duration::from_secs(45),
                "Claude did not acknowledge the control request; inspect the session before retrying"
            );
            match self.output.recv_timeout(Duration::from_millis(40)) {
                Ok(Ok(frame))
                    if frame.get("type").and_then(Value::as_str) == Some("control_response")
                        && frame
                            .pointer("/response/request_id")
                            .and_then(Value::as_str)
                            == Some(id.as_str()) =>
                {
                    ensure!(
                        frame.pointer("/response/subtype").and_then(Value::as_str) != Some("error"),
                        "Claude: {}",
                        frame.pointer("/response/error").unwrap_or(&Value::Null)
                    );
                    return Ok(frame
                        .pointer("/response/response")
                        .cloned()
                        .unwrap_or(Value::Null));
                }
                Ok(Ok(frame))
                    if frame.get("type").and_then(Value::as_str) == Some("control_request") =>
                {
                    let request_id = frame
                        .get("request_id")
                        .cloned()
                        .context("Claude control request has no ID")?;
                    match handle(&frame) {
                        Ok(response) => self.reply(&request_id, response)?,
                        Err(error) => self.reject(&request_id, &format!("{error:#}"))?,
                    }
                }
                Ok(Ok(frame)) => self.backlog.push_back(frame),
                Ok(Err(error)) => return Err(self.disconnected(&error)),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.disconnected("Claude disconnected"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    pub fn next_frame(&mut self) -> Result<Option<Value>> {
        if let Some(frame) = self.backlog.pop_front() {
            return Ok(Some(frame));
        }
        match self.output.try_recv() {
            Ok(Ok(frame)) => Ok(Some(frame)),
            Ok(Err(error)) => Err(self.disconnected(&error)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(self.disconnected("Claude disconnected")),
        }
    }
}

pub(super) fn user_message(id: &str, session: Option<&str>, content: Value) -> Value {
    json!({"type":"user","uuid":id,"session_id":session.unwrap_or_default(),
        "parent_tool_use_id":null,"message":{"role":"user","content":content}})
}

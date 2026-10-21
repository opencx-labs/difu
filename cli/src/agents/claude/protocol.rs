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
    pub mode: Mode,
    pub permission_mode: Option<&'a str>,
    pub allow_bypass: bool,
    pub settings: Option<&'a Value>,
}
pub(super) enum Mode {
    /// A difu session, with difu's MCP tools.
    Session,
    /// The model catalog: no settings, hooks, MCP servers or saved session.
    Models,
    /// The skill and command catalog: user, project and plugin settings load,
    /// but hooks, MCP servers and session persistence stay off.
    Commands,
}
/// Preserve native control rejection codes so callers can distinguish unsupported
/// features from a temporary transport or internal failure.
#[derive(Debug)]
pub(super) struct ControlError {
    code: Option<String>,
    message: String,
}
impl ControlError {
    fn from_response(response: &Value) -> Self {
        Self {
            code: response
                .get("error_code")
                .and_then(Value::as_str)
                .map(String::from),
            message: response
                .get("error")
                .map(|value| {
                    value
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| value.to_string())
                })
                .unwrap_or_else(|| "Control request rejected".into()),
        }
    }
    pub fn rejects_auto_mode(&self) -> bool {
        matches!(
            self.code.as_deref(),
            Some(
                "invalid_mode"
                    | "auto_mode_settings"
                    | "auto_mode_circuit_breaker"
                    | "auto_mode_fast_mode"
                    | "auto_mode_model"
                    | "auto_mode_unavailable"
            )
        )
    }
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Claude: {}", self.message)
    }
}
impl std::error::Error for ControlError {}
#[derive(Debug)]
pub(super) struct MissingConversation(pub String);
impl std::fmt::Display for MissingConversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "No conversation found with session ID: {}", self.0)
    }
}
impl std::error::Error for MissingConversation {}
pub(super) struct Connection {
    _child: ChildGroup,
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value, String>>,
    backlog: VecDeque<Value>,
    next: u64,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_complete: mpsc::Receiver<()>,
    resume_id: Option<String>,
}
impl Connection {
    pub fn stop(&mut self) -> Result<()> {
        self._child.stop()
    }
    pub fn open(options: Options<'_>, cancel: &Cancel) -> Result<Self> {
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
        if let Some(mode) = options.permission_mode {
            command.arg("--permission-mode").arg(mode);
        }
        if options.allow_bypass {
            command.arg("--allow-dangerously-skip-permissions");
        }
        if let Some(settings) = options.settings.filter(|v| v.is_object()) {
            command.arg("--settings").arg(settings.to_string());
        }
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
        match options.mode {
            Mode::Session => {
                command
                    .arg("--mcp-config")
                    .arg(json!({"mcpServers":{"difu":{"type":"sdk","name":"difu"}}}).to_string());
            }
            Mode::Models => {
                command.args([
                    "--no-session-persistence",
                    "--setting-sources=",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                ]);
            }
            Mode::Commands => {
                command.args([
                    "--no-session-persistence",
                    "--settings",
                    "{\"disableAllHooks\":true}",
                    "--strict-mcp-config",
                    "--mcp-config",
                    "{\"mcpServers\":{}}",
                ]);
            }
        }
        let mut child = ChildGroup::spawn_tree(&mut command)
            .context("Cannot start Claude Code; install and authenticate the claude CLI")?;
        let input = child.child.stdin.take().context("Missing Claude stdin")?;
        let output = child.child.stdout.take().context("Missing Claude stdout")?;
        // Keep a bounded diagnostic tail while draining so stderr cannot block stdout.
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let (stderr_done, stderr_complete) = mpsc::channel();
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
                let _ = stderr_done.send(());
            });
        }
        let (sender, receiver) = crate::wake::channel(cancel.wake());
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
            stderr_complete,
            resume_id: options.resume.map(String::from),
        })
    }
    fn disconnected(&self, message: &str) -> anyhow::Error {
        // stdout EOF can race the diagnostic reader on a native startup error.
        // Wait briefly for its final stderr before classifying a missing ID.
        let _ = self
            .stderr_complete
            .recv_timeout(Duration::from_millis(100));
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
        if let Some(id) = &self.resume_id {
            let expected = format!("No conversation found with session ID: {id}");
            if detail.lines().any(|line| line.trim() == expected) {
                return MissingConversation(id.clone()).into();
            }
        }
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
        handle: impl FnMut(&Value) -> Result<Value>,
    ) -> Result<Value> {
        self.call_inner(request, cancel, handle, false)
    }
    /// Management requests must not answer tool approvals that arrive concurrently.
    /// Queue them for the worker's normal permission/question handler, while still
    /// servicing SDK MCP requests needed to complete the management operation.
    pub fn call_preserving_requests(
        &mut self,
        request: Value,
        cancel: &Cancel,
        handle: impl FnMut(&Value) -> Result<Value>,
    ) -> Result<Value> {
        self.call_inner(request, cancel, handle, true)
    }
    fn call_inner(
        &mut self,
        request: Value,
        cancel: &Cancel,
        mut handle: impl FnMut(&Value) -> Result<Value>,
        preserve_requests: bool,
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
                    if frame.pointer("/response/subtype").and_then(Value::as_str) == Some("error") {
                        return Err(ControlError::from_response(
                            frame.get("response").unwrap_or(&Value::Null),
                        )
                        .into());
                    }
                    return Ok(frame
                        .pointer("/response/response")
                        .cloned()
                        .unwrap_or(Value::Null));
                }
                Ok(Ok(frame))
                    if frame.get("type").and_then(Value::as_str) == Some("control_request") =>
                {
                    if preserve_requests
                        && frame.pointer("/request/subtype").and_then(Value::as_str)
                            != Some("mcp_message")
                    {
                        self.backlog.push_back(frame);
                        continue;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_typed_auto_availability_rejections_disable_retry() {
        for code in [
            "auto_mode_model",
            "auto_mode_unavailable",
            "auto_mode_settings",
            "invalid_mode",
        ] {
            assert!(
                ControlError::from_response(&json!({"error_code":code,"error":"refused"}))
                    .rejects_auto_mode()
            );
        }
        for response in [
            json!({"error_code":"internal_error","error":"Auto mode request interrupted"}),
            json!({"error":"Auto mode not available"}),
        ] {
            assert!(!ControlError::from_response(&response).rejects_auto_mode());
        }
    }
}

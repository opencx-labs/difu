//! Bounded wire frames for large saved chats; provider protocols keep their own limits.
use super::{Entry, Reply, Session, client};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

const CHUNK: usize = 256 * 1024;
const START: &str = "{\"difu_stream\":1}";

/// Completed entries share their payloads, making comparison independent of old
/// tool-result size. Indices also handle insertion/removal and provider echoes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    pub base: u64,
    pub session: Box<Session>,
    pub count: usize,
    pub entries: Vec<(usize, Entry)>,
    #[serde(default)]
    pub subagents: Vec<ChildDelta>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildDelta {
    id: String,
    count: usize,
    entries: Vec<(usize, Entry)>,
}
impl Delta {
    pub fn between(previous: &Session, next: &Session) -> Self {
        let entries = next
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| previous.entries.get(*i) != Some(*e))
            .map(|(i, e)| (i, e.clone()))
            .collect();
        let mut session = next.clone();
        let count = session.entries.len();
        session.entries.clear();
        let subagents = session
            .subagents
            .iter_mut()
            .map(|child| {
                let old = previous.subagents.iter().find(|a| a.id == child.id);
                let entries = child
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(i, e)| old.and_then(|a| a.entries.get(*i)) != Some(*e))
                    .map(|(i, e)| (i, e.clone()))
                    .collect();
                let delta = ChildDelta {
                    id: child.id.clone(),
                    count: child.entries.len(),
                    entries,
                };
                child.entries.clear();
                delta
            })
            .collect();
        Self {
            base: previous.version,
            session: Box::new(session),
            count,
            entries,
            subagents,
        }
    }
    pub fn apply(mut self, current: &mut Session) -> Result<()> {
        ensure!(
            self.base == current.version && self.session.id == current.id,
            "Session update base changed; reconnecting"
        );
        ensure!(
            self.session.entries.is_empty(),
            "Invalid session update metadata"
        );
        let mut entries = std::mem::take(&mut current.entries);
        entries.truncate(self.count);
        for (index, entry) in self.entries {
            if index < entries.len() {
                if let Some(slot) = entries.get_mut(index) {
                    *slot = entry;
                }
            } else {
                ensure!(
                    index == entries.len() && index < self.count,
                    "Incomplete session update"
                );
                entries.push(entry);
            }
        }
        ensure!(entries.len() == self.count, "Incomplete session update");
        for delta in self.subagents {
            let mut entries = current
                .subagents
                .iter_mut()
                .find(|a| a.id == delta.id)
                .map(|a| std::mem::take(&mut a.entries))
                .unwrap_or_default();
            entries.truncate(delta.count);
            for (index, entry) in delta.entries {
                if let Some(slot) = entries.get_mut(index) {
                    *slot = entry;
                } else {
                    ensure!(
                        index == entries.len() && index < delta.count,
                        "Incomplete sub-agent update"
                    );
                    entries.push(entry);
                }
            }
            ensure!(entries.len() == delta.count, "Incomplete sub-agent update");
            let child = self
                .session
                .subagents
                .iter_mut()
                .find(|a| a.id == delta.id)
                .context("Unknown sub-agent update")?;
            ensure!(child.entries.is_empty(), "Invalid sub-agent metadata");
            child.entries = entries;
        }
        *current = *self.session;
        current.entries = entries;
        Ok(())
    }
}

struct FramedWriter<'a, W> {
    output: &'a mut W,
    buffer: Vec<u8>,
    streaming: bool,
}
impl<W: Write> FramedWriter<'_, W> {
    fn chunk(&mut self) -> io::Result<()> {
        if !self.streaming {
            writeln!(self.output, "{START}")?;
            self.streaming = true;
        }
        // Base64 contains no JSON escapes and lets frame boundaries split UTF-8.
        writeln!(self.output, "\"{}\"", STANDARD.encode(&self.buffer))?;
        self.buffer.clear();
        Ok(())
    }
    fn finish(mut self) -> io::Result<()> {
        if self.streaming {
            if !self.buffer.is_empty() {
                self.chunk()?;
            }
            self.output.write_all(b"null\n")?;
        } else {
            self.output.write_all(&self.buffer)?;
            self.output.write_all(b"\n")?;
        }
        self.output.flush()
    }
}
impl<W: Write> Write for FramedWriter<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len();
        while !bytes.is_empty() {
            let n = bytes.len().min(CHUNK - self.buffer.len());
            self.buffer
                .extend_from_slice(bytes.get(..n).unwrap_or_default());
            bytes = bytes.get(n..).unwrap_or_default();
            if self.buffer.len() == CHUNK {
                self.chunk()?;
            }
        }
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
pub fn write_reply(output: &mut impl Write, reply: &Reply) -> Result<()> {
    let mut writer = FramedWriter {
        output,
        buffer: Vec::with_capacity(CHUNK),
        streaming: false,
    };
    serde_json::to_writer(&mut writer, reply).context("Encode session service reply")?;
    writer.finish().context("Send session service reply")
}
struct FramedReader<'a, R> {
    input: &'a mut R,
    buffer: io::Cursor<Vec<u8>>,
    finished: bool,
}
impl<R: BufRead> Read for FramedReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.buffer.read(output)?;
            if n > 0 || self.finished {
                return Ok(n);
            }
            let line = client::read_line(self.input).map_err(io::Error::other)?;
            if line.trim() == "null" {
                self.finished = true;
                return Ok(0);
            }
            if line.len() > CHUNK * 2 {
                return Err(io::Error::other("Oversized session transfer chunk"));
            }
            let encoded: String = serde_json::from_str(&line).map_err(io::Error::other)?;
            let bytes = STANDARD.decode(encoded).map_err(io::Error::other)?;
            if bytes.is_empty() || bytes.len() > CHUNK {
                return Err(io::Error::other("Invalid session transfer chunk"));
            }
            self.buffer = io::Cursor::new(bytes);
        }
    }
}
pub fn read_reply(input: &mut impl BufRead) -> Result<Reply> {
    let first = client::read_line(input).context("Receive session service reply")?;
    if first.trim() != START {
        return serde_json::from_str(&first).context("Decode session service reply");
    }
    let reader = FramedReader {
        input,
        buffer: io::Cursor::new(Vec::new()),
        finished: false,
    };
    serde_json::from_reader(reader).context("Receive chunked session service reply")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_history_crosses_old_message_limit_using_bounded_frames() -> Result<()> {
        let text = "é".repeat(34 * 1024 * 1024);
        let mut wire = Vec::new();
        write_reply(&mut wire, &Reply::Changes(text.clone()))?;
        assert!(
            wire.split(|b| *b == b'\n')
                .all(|line| line.len() <= CHUNK * 2)
        );
        let reply = read_reply(&mut io::Cursor::new(wire))?;
        assert!(matches!(reply,Reply::Changes(value) if value==text));
        Ok(())
    }
    #[test]
    fn truncated_transfer_is_an_error_and_small_replies_stay_compatible() -> Result<()> {
        let mut wire = Vec::new();
        write_reply(&mut wire, &Reply::Ok)?;
        assert_eq!(wire, b"\"Ok\"\n");
        assert!(read_reply(&mut io::Cursor::new(format!("{START}\n\"ew==\"\n"))).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod delta_tests {
    use super::*;
    #[test]
    fn delta_reconstructs_edits_appends_and_removals_without_old_payloads() -> Result<()> {
        let mut original = Session::new(
            "test".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: "/tmp/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        original.note("commandExecution", "old".repeat(100_000));
        original.note("agentMessage", "first");
        let mut next = original.clone();
        next.entries
            .get_mut(1)
            .context("entry")?
            .text
            .push_str(" update");
        next.note("agentMessage", "second");
        let delta = Delta::between(&original, &next);
        assert_eq!(delta.entries.len(), 2);
        assert!(serde_json::to_vec(&delta)?.len() < 10_000);
        delta.apply(&mut original)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&next)?
        );
        next.entries.remove(0);
        next.touch();
        Delta::between(&original, &next).apply(&mut original)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&next)?
        );
        next.entries.clear();
        next.touch();
        Delta::between(&original, &next).apply(&mut original)?;
        assert!(original.entries.is_empty());
        Ok(())
    }
}

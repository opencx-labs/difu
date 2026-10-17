use super::*;
use anyhow::{Context, Result, ensure};
use base64::Engine;
use cpal::{
    SampleFormat, Stream,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::IntoClientRequest, stream::MaybeTlsStream};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;
fn send(socket: &mut Socket, value: Value) -> Result<()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .map_err(|_| anyhow::anyhow!("Transcription connection closed while sending audio"))
}
fn read(socket: &mut Socket) -> Result<Option<Value>> {
    match socket.read() {
        Ok(Message::Text(text)) => Ok(Some(
            serde_json::from_str(&text).context("Invalid transcription event")?,
        )),
        Ok(Message::Close(_)) => anyhow::bail!("Transcription connection closed before completion"),
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(None)
        }
        Err(_) => anyhow::bail!("Transcription connection failed"),
    }
}
fn connect(key: &str) -> Result<Socket> {
    let timeout = Duration::from_secs(10);
    let addresses = ("api.openai.com", 443)
        .to_socket_addrs()
        .context("Cannot resolve OpenAI transcription service")?;
    let stream = addresses
        .filter_map(|a| TcpStream::connect_timeout(&a, timeout).ok())
        .next()
        .context("Cannot connect to OpenAI transcription service")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut request =
        "wss://api.openai.com/v1/realtime?intent=transcription".into_client_request()?;
    let mut authorization =
        tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", key.trim()))
            .map_err(|_| anyhow::anyhow!("API key contains invalid characters"))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("Authorization", authorization);
    let (mut socket,_)=tungstenite::client_tls(request,stream).map_err(|_|anyhow::anyhow!("OpenAI transcription connection rejected. Check the API key, network and API account access."))?;
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(tcp) => tcp,
        MaybeTlsStream::Rustls(tls) => &mut tls.sock,
        _ => anyhow::bail!("Unsupported transcription TLS transport"),
    };
    tcp.set_read_timeout(Some(Duration::from_millis(15)))?;
    Ok(socket)
}
fn capture(sender: mpsc::SyncSender<Vec<f32>>, failed: Arc<AtomicBool>) -> Result<(Stream, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("No microphone is available. Select a default input in macOS Sound settings.")?;
    let config = device.default_input_config().context(
        "Cannot open the microphone. Allow Ghostty microphone access in macOS Privacy settings.",
    )?;
    let rate = config.sample_rate().0;
    ensure!(
        rate > 0 && config.channels() > 0,
        "Microphone returned an invalid audio format"
    );
    let channels = usize::from(config.channels());
    let settings: cpal::StreamConfig = config.clone().into();
    let error_flag = failed.clone();
    let on_error = move |_error| {
        error_flag.store(true, Ordering::Release);
    };
    let stream=match config.sample_format() {
        SampleFormat::F32=>device.build_input_stream(&settings,move |data:&[f32],_| {
            let mono=data.chunks_exact(channels).map(|c| c.iter().copied().filter(|s|s.is_finite()).sum::<f32>()/channels as f32).collect();
            if sender.try_send(mono).is_err() { failed.store(true,Ordering::Release); }
        },on_error,None),
        SampleFormat::I16=>device.build_input_stream(&settings,move |data:&[i16],_| {
            let mono=data.chunks_exact(channels).map(|c| c.iter().map(|s|f32::from(*s)/32768.0).sum::<f32>()/channels as f32).collect();
            if sender.try_send(mono).is_err() { failed.store(true,Ordering::Release); }
        },on_error,None),
        SampleFormat::U16=>device.build_input_stream(&settings,move |data:&[u16],_| {
            let mono=data.chunks_exact(channels).map(|c| c.iter().map(|s|(f32::from(*s)-32768.0)/32768.0).sum::<f32>()/channels as f32).collect();
            if sender.try_send(mono).is_err() { failed.store(true,Ordering::Release); }
        },on_error,None),
        _=>anyhow::bail!("This microphone's audio format is unsupported; choose an input using 16-bit or 32-bit float audio"),
    }.context("Cannot start microphone capture; check macOS microphone permission")?;
    Ok((stream, rate))
}
/// Continuous interpolation across callback boundaries, mono PCM16 at 24 kHz.
struct Resampler {
    step: f64,
    next: f64,
    index: u64,
    previous: f32,
}
impl Resampler {
    fn new(rate: u32) -> Self {
        Self {
            step: f64::from(rate) / 24000.0,
            next: 0.0,
            index: 0,
            previous: 0.0,
        }
    }
    fn push(&mut self, samples: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for sample in samples {
            let current = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            if self.index == 0 {
                self.previous = current;
            }
            while self.next <= self.index as f64 {
                let fraction = (self.next - (self.index as f64 - 1.0)).clamp(0.0, 1.0) as f32;
                let value = self.previous + (current - self.previous) * fraction;
                bytes.extend_from_slice(&((value * 32767.0).round() as i16).to_le_bytes());
                self.next += self.step;
            }
            self.previous = current;
            self.index = self.index.saturating_add(1);
        }
        bytes
    }
}
fn server_error(value: &Value) -> Result<()> {
    if value.get("type").and_then(Value::as_str) == Some("error")
        || value.get("type").and_then(Value::as_str)
            == Some("conversation.item.input_audio_transcription.failed")
    {
        // Server text may echo inputs; keep credentials and audio out of errors/logs.
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let code = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(80)
            .collect::<String>();
        anyhow::bail!("Transcription failed ({code}). Check API account access or retry manually.");
    }
    Ok(())
}
pub(super) fn run(
    key: String,
    control: &Arc<AtomicU8>,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    let mut socket = connect(&key)?;
    drop(key);
    if control.load(Ordering::Acquire) != 0 {
        let _ = events.send(Event::Done(String::new()));
        return Ok(());
    }
    send(
        &mut socket,
        json!({"type":"session.update","session":{"type":"transcription","audio":{"input":{"format":{"type":"audio/pcm","rate":24000},"transcription":{"model":"gpt-live-transcribe"},"turn_detection":null}}}}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if control.load(Ordering::Acquire) != 0 {
            let _ = events.send(Event::Done(String::new()));
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Transcription session setup timed out"
        );
        if let Some(value) = read(&mut socket)? {
            server_error(&value)?;
            if value.get("type").and_then(Value::as_str) == Some("session.updated") {
                break;
            }
        }
    }
    let (audio_tx, audio_rx) = mpsc::sync_channel(64);
    let failed = Arc::new(AtomicBool::new(false));
    let (stream, rate) = capture(audio_tx, failed.clone())?;
    if control.load(Ordering::Acquire) != 0 {
        let _ = events.send(Event::Done(String::new()));
        return Ok(());
    }
    stream.play().context("Cannot record microphone audio")?;
    let _ = events.send(Event::Listening);
    let mut stream = Some(stream);
    let mut resampler = Resampler::new(rate);
    let mut sent = 0usize;
    let mut finishing = None;
    loop {
        if control.load(Ordering::Acquire) == 2 {
            return Ok(());
        }
        if control.load(Ordering::Acquire) == 1 && stream.is_some() {
            stream = None;
        }
        ensure!(
            !failed.load(Ordering::Acquire),
            "Microphone capture was interrupted or audio could not be streamed fast enough; the draft is unchanged"
        );
        while let Ok(audio) = audio_rx.try_recv() {
            let level =
                (audio.iter().map(|s| s * s).sum::<f32>() / audio.len().max(1) as f32).sqrt();
            let _ = events.send(Event::Level(level));
            let bytes = resampler.push(&audio);
            if !bytes.is_empty() {
                sent = sent.saturating_add(bytes.len());
                send(
                    &mut socket,
                    json!({"type":"input_audio_buffer.append","audio":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                )?;
            }
        }
        if stream.is_none() && finishing.is_none() {
            if sent < 4800 {
                let _ = events.send(Event::Done(String::new()));
                return Ok(());
            }
            send(&mut socket, json!({"type":"input_audio_buffer.commit"}))?;
            finishing = Some(Instant::now());
        }
        ensure!(
            finishing.is_none_or(|at| at.elapsed() < Duration::from_secs(45)),
            "Transcription timed out; your draft is unchanged"
        );
        if let Some(value) = read(&mut socket)? {
            server_error(&value)?;
            match value.get("type").and_then(Value::as_str) {
                Some("conversation.item.input_audio_transcription.delta") => {
                    if let Some(text) = value.get("delta").and_then(Value::as_str) {
                        let _ = events.send(Event::Partial(text.into()));
                    }
                }
                Some("conversation.item.input_audio_transcription.completed") => {
                    ensure!(
                        finishing.is_some(),
                        "Transcription completed before the audio was committed"
                    );
                    let text = value
                        .get("transcript")
                        .and_then(Value::as_str)
                        .context("Transcription returned no text")?;
                    let _ = events.send(Event::Done(text.into()));
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampling_keeps_callback_boundaries_and_pcm_levels() {
        let samples = vec![0.5; 480];
        let whole = Resampler::new(48000).push(&samples);
        let mut chunked = Resampler::new(48000);
        let pieces = samples
            .chunks(17)
            .flat_map(|c| chunked.push(c))
            .collect::<Vec<_>>();
        assert_eq!(whole, pieces);
        assert_eq!(whole.len(), 480);
        assert!(
            whole
                .as_chunks::<2>()
                .0
                .iter()
                .all(|b| b.first() == Some(&0) && b.get(1) == Some(&64))
        );
    }
}

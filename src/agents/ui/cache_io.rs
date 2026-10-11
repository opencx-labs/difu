//! Ordered background persistence. Replace queued writes to the same cache with
//! the newest snapshot, so typing never waits for serialization or fsync.
use super::*;
type Write = Box<dyn FnOnce() -> anyhow::Result<()> + Send>;
pub(super) struct Writer {
    sender: Option<mpsc::Sender<(String, Write)>>,
    errors: mpsc::Receiver<String>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Default for Writer {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel::<(String, Write)>();
        let (errors, receive_errors) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok((key, write)) = receiver.recv() {
                let mut batch = std::collections::BTreeMap::from([(key, write)]);
                for (key, write) in receiver.try_iter() {
                    batch.insert(key, write);
                }
                for (key, write) in batch {
                    if let Err(error) = write() {
                        let _ = errors.send(format!("Cannot save {key}: {error:#}"));
                        crate::repaint::request();
                    }
                }
            }
        });
        Self {
            sender: Some(sender),
            errors: receive_errors,
            worker: Some(worker),
        }
    }
}
impl Writer {
    #[cfg(test)]
    pub fn flush(&self) -> anyhow::Result<()> {
        let (sender, receiver) = mpsc::channel();
        self.save("\u{10ffff}".into(), move || {
            let _ = sender.send(());
            Ok(())
        })?;
        receiver.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }
    pub fn save(
        &self,
        key: String,
        write: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        self.sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Cache writer stopped"))?
            .send((key, Box::new(write)))
            .map_err(|_| anyhow::anyhow!("Cache writer stopped"))
    }
    pub fn error(&self) -> Option<String> {
        self.errors.try_iter().last()
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.sender.take();
        // On normal exit flush the last draft/statistics snapshot before leaving.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

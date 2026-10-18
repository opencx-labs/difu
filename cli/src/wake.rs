//! Lossless wake notifications for workers with more than one input channel.
use std::{
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};

#[derive(Clone, Default, Debug)]
// One consumer drains its input channels before waiting. A retained notification
// closes the race between the final empty-channel check and going to sleep.
pub(crate) struct Signal(Arc<(Mutex<bool>, Condvar)>);
impl Signal {
    pub fn notify(&self) {
        let (pending, ready) = &*self.0;
        *pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        ready.notify_one();
    }
    pub fn wait_timeout(&self, duration: Duration) {
        let (pending, ready) = &*self.0;
        let pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        let (mut pending, _) = ready
            .wait_timeout_while(pending, duration, |pending| !*pending)
            .unwrap_or_else(|e| e.into_inner());
        *pending = false;
    }
    pub fn wait(&self) {
        let (pending, ready) = &*self.0;
        let pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = ready
            .wait_while(pending, |pending| !*pending)
            .unwrap_or_else(|e| e.into_inner());
        *pending = false;
    }
}

pub(crate) struct Sender<T> {
    sender: mpsc::Sender<T>,
    signal: Signal,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            signal: self.signal.clone(),
        }
    }
}
impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), mpsc::SendError<T>> {
        let result = self.sender.send(value);
        self.signal.notify();
        result
    }
}
pub(crate) fn channel<T>(signal: Signal) -> (Sender<T>, mpsc::Receiver<T>) {
    let (sender, receiver) = mpsc::channel();
    (Sender { sender, signal }, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    fn notification_before_wait_is_retained_and_sends_wake_a_sleeper() -> anyhow::Result<()> {
        let signal = Signal::default();
        signal.notify();
        signal.wait();
        let (sender, receiver) = channel(signal.clone());
        let (done, completed) = mpsc::channel();
        let worker = thread::spawn(move || {
            signal.wait();
            let _ = done.send(receiver.try_recv());
        });
        sender.send(42)?;
        assert_eq!(completed.recv_timeout(Duration::from_secs(2))??, 42);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))?;
        Ok(())
    }

    #[test]
    fn coalesced_inputs_remain_available_in_order() -> anyhow::Result<()> {
        let signal = Signal::default();
        let (commands, command_rx) = channel(signal.clone());
        let (output, output_rx) = channel(signal.clone());
        commands.send(1)?;
        output.send(2)?;
        commands.send(3)?;
        let (done, completed) = mpsc::channel();
        let worker = thread::spawn(move || {
            signal.wait();
            let _ = done.send((
                command_rx.try_iter().collect::<Vec<_>>(),
                output_rx.try_iter().collect::<Vec<_>>(),
            ));
        });
        assert_eq!(
            completed.recv_timeout(Duration::from_secs(2))?,
            (vec![1, 3], vec![2])
        );
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))?;
        Ok(())
    }

    #[test]
    fn cancellation_wakes_an_idle_worker_without_input() -> anyhow::Result<()> {
        let cancel = crate::process::Cancel::default();
        let token = cancel.clone();
        let (done, completed) = mpsc::channel();
        let worker = thread::spawn(move || {
            token.wake().wait();
            let _ = done.send(token.cancelled());
        });
        cancel.cancel();
        assert!(completed.recv_timeout(Duration::from_secs(2))?);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))?;
        Ok(())
    }

    #[test]
    fn persistence_deadline_wakes_without_more_provider_output() -> anyhow::Result<()> {
        let signal = Signal::default();
        let (done, completed) = mpsc::channel();
        let delay = Duration::from_millis(20);
        let worker = thread::spawn(move || {
            let started = std::time::Instant::now();
            signal.wait_timeout(delay);
            let _ = done.send(started.elapsed());
        });
        assert!(completed.recv_timeout(Duration::from_secs(2))? >= delay);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))?;
        Ok(())
    }
}

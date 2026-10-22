//! Scoped command transport for synchronous request integration tests.
//! Git operations stay real; only exact remote endpoints are substituted.
use crate::storage::Storage;
use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell, collections::VecDeque, ffi::OsString, marker::PhantomData, path::PathBuf,
    process::Command, rc::Rc,
};

thread_local! {
    static ACTIVE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    storage: Storage,
    gh: VecDeque<(Vec<String>, PathBuf)>,
    fetch: Option<(String, PathBuf, Vec<String>)>,
}

pub(crate) struct Scope(PhantomData<Rc<()>>);
impl Scope {
    pub(crate) fn new(
        storage: Storage,
        gh: Vec<(Vec<String>, PathBuf)>,
        fetch: (String, PathBuf, Vec<String>),
    ) -> Result<Self> {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            ensure!(
                active.is_none(),
                "A process fixture is already active on this thread"
            );
            *active = Some(State {
                storage,
                gh: gh.into(),
                fetch: Some(fetch),
            });
            Ok(Self(PhantomData))
        })
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ACTIVE.with(|active| {
            let active = active.borrow();
            let state = active.as_ref().context("Missing process fixture")?;
            ensure!(
                state.gh.is_empty(),
                "Expected GitHub commands were not consumed"
            );
            ensure!(state.fetch.is_none(), "Missing revisions were not fetched");
            Ok(())
        })
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = None);
    }
}
pub(crate) fn storage() -> Option<Storage> {
    ACTIVE.with(|active| active.borrow().as_ref().map(|state| state.storage.clone()))
}

pub(crate) fn adapt(command: &mut Command) -> Result<()> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(state) = active.as_mut() else {
            return Ok(());
        };
        let args = command.get_args().map(OsString::from).collect::<Vec<_>>();
        if command.get_program() == "gh" {
            let (expected, response) = state.gh.pop_front().context("Unexpected GitHub command")?;
            ensure!(
                args == expected.iter().map(OsString::from).collect::<Vec<_>>(),
                "Unexpected GitHub arguments: {args:?}"
            );
            *command = Command::new("/bin/cat");
            command.arg(response);
        } else if command.get_program() == "git" && args.iter().any(|arg| arg == "fetch") {
            let (url, remote, revisions) = state.fetch.take().context("Unexpected Git fetch")?;
            let separator = args
                .iter()
                .position(|arg| arg == "--")
                .context("Missing fetch separator")?;
            ensure!(
                args.get(separator + 1) == Some(&OsString::from(&url)),
                "Unexpected fetch URL: {args:?}"
            );
            ensure!(
                args.get(separator + 2..)
                    == Some(
                        revisions
                            .iter()
                            .map(OsString::from)
                            .collect::<Vec<_>>()
                            .as_slice()
                    ),
                "Unexpected requested revisions: {args:?}"
            );
            let env = command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(OsString::from)))
                .collect::<Vec<_>>();
            let cwd = command.get_current_dir().map(|path| path.to_owned());
            let mut replacement = Command::new("git");
            for (index, arg) in args.into_iter().enumerate() {
                replacement.arg(if index == separator + 1 {
                    remote.as_os_str()
                } else {
                    &arg
                });
            }
            for (key, value) in env {
                if let Some(value) = value {
                    replacement.env(key, value);
                } else {
                    replacement.env_remove(key);
                }
            }
            if let Some(cwd) = cwd {
                replacement.current_dir(cwd);
            }
            replacement.env("GIT_ALLOW_PROTOCOL", "file");
            *command = replacement;
        }
        Ok(())
    })
}

// URL launches are asynchronous, so use unique namespace-scoped registrations
// instead of the thread-local synchronous command fixture above.
const URL_HOST: &str = "difu-shortcut-fixture.invalid";
static URL_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
type UrlSenders = std::collections::BTreeMap<String, std::sync::mpsc::Sender<String>>;
static URL_SENDERS: std::sync::OnceLock<std::sync::Mutex<UrlSenders>> = std::sync::OnceLock::new();
pub(crate) struct UrlScope {
    pub(crate) url: String,
    pub(crate) receiver: std::sync::mpsc::Receiver<String>,
}
impl UrlScope {
    pub(crate) fn new() -> Result<Self> {
        let number = URL_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let url = format!("https://{URL_HOST}/{number}");
        let (sender, receiver) = std::sync::mpsc::channel();
        URL_SENDERS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("URL fixture lock"))?
            .insert(url.clone(), sender);
        Ok(Self { url, receiver })
    }
}
impl Drop for UrlScope {
    fn drop(&mut self) {
        if let Ok(mut senders) = URL_SENDERS.get_or_init(Default::default).lock() {
            senders.remove(&self.url);
        }
    }
}
pub(crate) fn open_url(url: &url::Url) -> Option<Result<()>> {
    if url.host_str() != Some(URL_HOST) {
        return None;
    }
    // Fail closed after cleanup too: fixture URLs never reach the real launcher.
    Some((|| {
        let senders = URL_SENDERS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("URL fixture lock"))?;
        let sender = senders
            .get(url.as_str())
            .context("Unregistered fixture URL")?;
        sender.send(url.as_str().to_owned())?;
        Ok(())
    })())
}

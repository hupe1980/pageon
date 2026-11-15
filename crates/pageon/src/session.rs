//! Sessions: loads that share cookies and Web Storage.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use pageon_env::Env;
use pageon_net::{Cookie, Net, StorageState};

use crate::{Document, LoadError, LoadOptions, Tab};

/// A sequence of loads that share cookies and Web Storage, through one environment.
///
/// ```no_run
/// use pageon::{Live, LoadOptions, Session};
///
/// let session = Session::new(Live::default());
/// session.load_cookies("state.json")?; // Playwright storageState, or a cookie array
/// let doc = session.load("https://example.com/account", LoadOptions::default())?;
/// for r in doc.requests() {
///     println!("{r}");
/// }
/// # Ok::<(), pageon::LoadError>(())
/// ```
pub struct Session<E: Env> {
    env: E,
    net: Rc<RefCell<Net>>,
    /// The latest load time seen (ms), for cookie expiry outside loads.
    now: Cell<u64>,
    /// Secret values typed so far in any tab, with their handles: redacted from everything the
    /// session reports, also after the tab that typed them closed.
    secrets: RefCell<Vec<(String, String)>>,
    /// The session clipboard: one for every tab, never the host's.
    clipboard: RefCell<Option<pageon_js::Clip>>,
}

impl<E: Env> std::fmt::Debug for Session<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl<E: Env> Session<E> {
    /// A session with no cookies or storage.
    pub fn new(env: E) -> Self {
        Self {
            env,
            net: Rc::new(RefCell::new(Net::default())),
            now: Cell::new(0),
            secrets: RefCell::default(),
            clipboard: RefCell::default(),
        }
    }

    /// What the session clipboard holds.
    pub fn clipboard(&self) -> Option<pageon_js::Clip> {
        self.clipboard.borrow().clone()
    }

    /// Replaces what the session clipboard holds.
    pub fn set_clipboard(&self, clip: Option<pageon_js::Clip>) {
        *self.clipboard.borrow_mut() = clip;
    }

    /// The secret values typed so far, with their handles.
    pub(crate) fn secrets(&self) -> std::cell::Ref<'_, Vec<(String, String)>> {
        self.secrets.borrow()
    }

    /// Keeps a secret value handed out for typing, to redact it everywhere from now on.
    pub(crate) fn keep_secret(&self, value: String, handle: String) {
        let mut s = self.secrets.borrow_mut();
        if !s.iter().any(|(v, _)| *v == value) {
            s.push((value, handle));
        }
    }

    /// Loads a page (URL or local path) with its stylesheets, scripts and frames.
    pub fn load(&self, input: &str, options: LoadOptions) -> Result<Document, LoadError> {
        {
            let mut net = self.net.borrow_mut();
            net.allow_private = options.allow_private_network;
            net.robots = options.robots;
            net.take_log();
        }
        let result = crate::load::open(input, &self.env, &self.net, options, Default::default());
        let mut doc = match result {
            Ok(crate::load::Loaded::Page(d)) => *d,
            Ok(crate::load::Loaded::Download(d)) => {
                self.net.borrow_mut().take_log();
                return Err(LoadError::UnsupportedContentType(d.content_type));
            }
            Err(e) => {
                self.net.borrow_mut().take_log();
                return Err(e);
            }
        };
        let log = self.net.borrow_mut().take_log();
        doc.summary.requests = log.len();
        doc.summary.blocked = log
            .iter()
            .filter(|e| matches!(e.outcome, pageon_net::Outcome::Blocked(_)))
            .count();
        doc.requests = log;
        self.now.set(self.now.get().max(doc.loaded_at));
        Ok(doc)
    }

    /// The session's cookies (expired ones dropped).
    pub fn cookies(&self) -> Vec<Cookie> {
        self.net.borrow().cookies(self.now.get())
    }

    /// Adds cookies (replacing those with the same name, domain and path).
    pub fn add_cookies(&self, cookies: &[Cookie]) {
        self.net.borrow_mut().add_cookies(cookies);
    }

    /// Adds the cookies and `localStorage` of a JSON file: a Playwright `storageState` file or
    /// a cookie array. Returns how many cookies were read.
    pub fn load_state(&self, path: impl AsRef<Path>) -> Result<usize, LoadError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| LoadError::Io(format!("{}: {e}", path.display())))?;
        let state = pageon_net::state::from_json(&text).map_err(|e| {
            LoadError::Io(format!(
                "{}: not a storageState file or cookie array: {e}",
                path.display()
            ))
        })?;
        self.set_storage_state(&state);
        Ok(state.cookies.len())
    }

    /// Same as [`Session::load_state`] (cookies and `localStorage`).
    pub fn load_cookies(&self, path: impl AsRef<Path>) -> Result<usize, LoadError> {
        self.load_state(path)
    }

    /// The session's cookies and `localStorage`, in Playwright's `storageState` format.
    pub fn storage_state(&self) -> StorageState {
        self.net.borrow().storage_state(self.now.get())
    }

    /// Adds a state's cookies and `localStorage`.
    pub fn set_storage_state(&self, state: &StorageState) {
        self.net.borrow_mut().set_storage_state(state);
    }

    /// Writes [`Session::storage_state`] as Playwright `storageState` JSON.
    pub fn save_state(&self, path: impl AsRef<Path>) -> Result<(), LoadError> {
        let path = path.as_ref();
        let text = serde_json::to_string_pretty(&self.storage_state()).unwrap_or_default();
        pageon_env::write_private(path, (text + "\n").as_bytes())
            .map_err(|e| LoadError::Io(format!("{}: {e}", path.display())))
    }

    /// The environment.
    pub fn env(&self) -> &E {
        &self.env
    }

    /// Opens a page in a new tab: it stays live, so an agent can act on it (see [`Tab`]).
    ///
    /// ```no_run
    /// use pageon::{Action, Live, LoadOptions, Session};
    ///
    /// let session = Session::new(Live::default());
    /// let mut tab = session.open("https://example.com/login", LoadOptions::default())?;
    /// println!("{}", tab.observe());
    /// let outcome = tab.act("type e3 ada".parse()?)?;
    /// println!("{outcome}");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open(&self, input: &str, options: LoadOptions) -> Result<Tab<'_, E>, LoadError> {
        Tab::open(self, input, options, true)
    }

    /// Forgets everything the session holds (cookies, storage, cache), as a fresh session
    /// would start (a reset). Close its tabs first.
    pub fn clear(&self) {
        *self.net.borrow_mut() = Net::default();
        self.now.set(0);
    }

    pub(crate) fn net(&self) -> &Rc<RefCell<Net>> {
        &self.net
    }

    /// The latest virtual time seen (ms since the Unix epoch).
    pub(crate) fn now_ms(&self) -> u64 {
        self.now.get()
    }

    pub(crate) fn seen(&self, now: u64) {
        self.now.set(self.now.get().max(now));
    }
}

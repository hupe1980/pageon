//! Staged files: what an agent may upload. The operator stages each file by name, bound to
//! origins (`--file cv@https://jobs.example=./cv.pdf`); agents and pages refer to it by name
//! and never name a host path. The file is read when it is uploaded.

use std::collections::HashMap;
use std::path::PathBuf;

use pageon_env::{Env, NetError, Request, Response, StagedFile};

/// The largest file pageon uploads.
pub const MAX_FILE: u64 = 64 << 20;

/// A staged file: where it is, and the origins it may be uploaded to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// Host path, as the operator wrote it.
    pub path: PathBuf,
    /// Origins the file may be uploaded to.
    pub origins: Vec<String>,
}

/// An environment with staged files around another.
///
/// ```no_run
/// use pageon::{Files, Live};
///
/// let env = Files::new(Live::default()).with("cv", "./cv.pdf", ["https://jobs.example"]);
/// ```
#[derive(Debug)]
pub struct Files<E: Env> {
    inner: E,
    files: HashMap<String, Staged>,
}

impl<E: Env> Files<E> {
    /// No files staged yet.
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            files: HashMap::new(),
        }
    }

    /// Stages the file at `path` as `name`, for pages of `origins`.
    pub fn with<O: Into<String>>(
        mut self,
        name: &str,
        path: impl Into<PathBuf>,
        origins: impl IntoIterator<Item = O>,
    ) -> Self {
        self.files.insert(
            name.to_string(),
            Staged {
                path: path.into(),
                origins: origins.into_iter().map(Into::into).collect(),
            },
        );
        self
    }

    /// The staged file named `name`, or staged at exactly the path `name` (protocol clients
    /// pass paths).
    fn find(&self, name: &str) -> Option<(&str, &Staged)> {
        self.files
            .get_key_value(name)
            .or_else(|| self.files.iter().find(|(_, s)| s.path.as_os_str() == name))
            .map(|(k, s)| (k.as_str(), s))
    }
}

/// The MIME type a picked file gets, by extension.
pub fn mime_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

impl<E: Env> Env for Files<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        self.inner.fetch(req)
    }

    fn stream(&self, call: &pageon_env::StreamCall) -> Result<pageon_env::StreamReply, NetError> {
        self.inner.stream(call)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        self.inner.now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        self.inner.random_u64()
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        self.inner.secret(name, origin)
    }

    fn passkey(
        &self,
        call: &pageon_env::PasskeyCall,
    ) -> Result<pageon_env::PasskeyReply, NetError> {
        self.inner.passkey(call)
    }

    fn file(&self, name: &str, origin: &str) -> Result<StagedFile, NetError> {
        let Some((key, s)) = self.find(name) else {
            return self.inner.file(name, origin);
        };
        if !crate::policy::origin_allowed(&s.origins, origin) {
            return Err(NetError::Blocked(format!(
                "file \"{key}\" is not for {origin} (it is bound to {})",
                if s.origins.is_empty() {
                    "no origin".to_string()
                } else {
                    s.origins.join(", ")
                }
            )));
        }
        let cannot =
            |why: String| NetError::Blocked(format!("file \"{key}\" cannot be read ({why})"));
        let meta = std::fs::metadata(&s.path).map_err(|e| cannot(e.to_string()))?;
        if !meta.is_file() {
            return Err(cannot("not a file".into()));
        }
        if meta.len() > MAX_FILE {
            return Err(cannot(format!("{} B is over the 64 MiB limit", meta.len())));
        }
        let bytes = std::fs::read(&s.path).map_err(|e| cannot(e.to_string()))?;
        let base = s
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| key.to_string());
        Ok(StagedFile {
            content_type: mime_type(&base).to_string(),
            name: base,
            bytes,
        })
    }

    fn recorder(&self) -> Option<&dyn pageon_env::Recorder> {
        self.inner.recorder()
    }

    fn profile(&self) -> pageon_env::Profile {
        self.inner.profile()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;
    impl Env for Nothing {
        fn fetch(&self, _: &Request) -> Result<Response, NetError> {
            Err(NetError::ReplayMiss(String::new()))
        }
        fn now_ms(&self) -> Result<u64, NetError> {
            Ok(0)
        }
        fn random_u64(&self) -> Result<u64, NetError> {
            Ok(0)
        }
    }

    /// Only staged names (or their exact paths), for their origins; never other paths
    /// or directories.
    #[test]
    fn only_staged_files_for_their_origins() {
        let dir = std::env::temp_dir().join(format!("pageon-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let cv = dir.join("cv.pdf");
        std::fs::write(&cv, b"%PDF-1.7 cv").unwrap_or_else(|e| panic!("{e}"));
        let env = Files::new(Nothing)
            .with("cv", &cv, ["https://jobs.example"])
            .with("dir", &dir, ["https://jobs.example"]);
        let f = env
            .file("cv", "https://jobs.example")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (f.name.as_str(), f.content_type.as_str()),
            ("cv.pdf", "application/pdf")
        );
        assert_eq!(f.bytes, b"%PDF-1.7 cv");
        assert!(
            env.file(&cv.to_string_lossy(), "https://jobs.example")
                .is_ok(),
            "by its staged path"
        );
        let err = |name: &str, origin: &str| env.file(name, origin).unwrap_err().to_string();
        assert!(err("cv", "https://evil.test").contains("is not for https://evil.test"));
        assert!(err("/etc/passwd", "https://jobs.example").contains("is not staged"));
        assert!(err("~/.ssh/id_ed25519", "https://jobs.example").contains("is not staged"));
        assert!(err("dir", "https://jobs.example").contains("not a file"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

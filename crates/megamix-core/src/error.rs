use std::path::PathBuf;

/// Errors returned by `megamix-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid UTF-8 text")]
    NotUtf8 { path: PathBuf },

    #[error("{path}: TOML parse error: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: Box<toml_edit::TomlError>,
    },

    #[error("{path} was changed by another program since it was loaded; reload and try again")]
    ChangedOnDisk { path: PathBuf },

    #[error("{0} not found (is DivaModLoader installed?)")]
    LoaderConfigMissing(PathBuf),

    #[error("no mod named `{0}`")]
    ModNotFound(String),

    #[error("`{query}` matches several mods: {}", .matches.join(", "))]
    AmbiguousMod { query: String, matches: Vec<String> },

    #[error("backup `{0}` not found")]
    BackupNotFound(String),

    #[error("{0}")]
    Archive(String),

    #[error("cancelled")]
    Cancelled,

    #[error(
        "not enough free space on the drive with {path}: need {needed_mb} MB, only {available_mb} MB free"
    )]
    NotEnoughSpace {
        path: PathBuf,
        needed_mb: u64,
        available_mb: u64,
    },

    #[error("{0} is in RAM (tmpfs); unpacking there could fill up memory, so it is not used")]
    RamBacked(PathBuf),

    #[error("{0}")]
    Other(String),

    /// Couldn't reach a site (offline, DNS, timeout, TLS…).
    #[error("couldn't reach {host}: {message}")]
    Network { host: String, message: String },

    /// The site answered with an error status.
    #[error("{url}: the site answered HTTP {status}")]
    Http { url: String, status: u16 },

    /// The site answered, but not with what was expected.
    #[error("{url}: unexpected answer: {message}")]
    BadResponse { url: String, message: String },

    #[error("{0}: not found on the site (removed or private?)")]
    RemoteNotFound(String),

    #[error("the download of {file} is damaged ({detail}); it was deleted, try again")]
    DownloadDamaged { file: String, detail: String },

    #[error("not a mod link or id: `{0}` (try dma:<id>, gb:<id> or a mod page URL)")]
    BadModRef(String),

    #[error("{path}: `{key}` has the wrong type (expected {expected})")]
    WrongType {
        path: PathBuf,
        key: String,
        expected: &'static str,
    },
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

//! Errors with the exact wording the CLI prints.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("cannot serialize config: {0}")]
    Serialize(String),
    #[error(
        "{path}: context {name} has a password key; passwords are never stored here, run `nutsh ctx login {name}`"
    )]
    PasswordInFile { path: PathBuf, name: String },
    /// The same refusal for a `password` key that belongs to no context, which has no name to
    /// offer: a stray top-level key, or one in a file too broken to say which context it is in.
    #[error(
        "{path}: a password key outside any context; passwords are never stored here, run `nutsh ctx login <name>`"
    )]
    PasswordKey { path: PathBuf },
    #[error("invalid context name {0:?}: use letters, digits, '.', '_' or '-'")]
    InvalidName(String),
    #[error("unknown context {name}; known: {known}")]
    UnknownContext { name: String, known: String },
    #[error("context {0} already exists; pass --force to overwrite")]
    Exists(String),
    #[error("no context selected; run `nutsh ctx add` or pass --host (config: {0})")]
    NoContext(PathBuf),
    #[error("secret store unavailable: {0}")]
    SecretUnavailable(String),
}

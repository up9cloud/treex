use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // The source is deliberately left out of the message; anyhow and friends
    // print the chain themselves, and repeating it reads like a stutter.
    #[error("cannot open {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

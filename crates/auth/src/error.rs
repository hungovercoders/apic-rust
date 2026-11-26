use std::fmt;

use crate::authcode::ERR_NOT_INTERACTIVE;

/// Why a spec could not be parsed or applied. Callers print it; the
/// variants keep apart the failures a caller may want to tell from the
/// rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The grant needs a browser and no person is there to use one.
    NotInteractive,
    /// The caller cancelled while a sign-in or a command was waiting.
    Cancelled,
    /// The token cache could not be saved; the text is the cache's own.
    Cache(String),
    /// Anything else, as the message to show.
    Message(String),
}

impl Error {
    pub(crate) fn msg(text: impl Into<String>) -> Error {
        Error::Message(text.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotInteractive => f.write_str(ERR_NOT_INTERACTIVE),
            Error::Cancelled => f.write_str("context canceled"),
            Error::Cache(text) | Error::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(text: String) -> Error {
        Error::Message(text)
    }
}

//! Typed store errors and their normative class names.
//!
//! The interoperable contract is the class string returned by
//! [`Error::class`]. Classes already defined by `WEFTCODEC-FORMAT.md` §11 and
//! `WEFT-SELECTORS.md` §5 keep their exact spelling. Store-specific POSIX-ish
//! classes (`Busy`, `Exists`, `IsDir`, `NotDir`, `NotEmpty`, `InvalidName`) are
//! documented in `docs/store-status.md`.

use std::fmt;

/// Normative error classes emitted by the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// The exclusive writer lock is held by another process.
    Busy,
    /// No such object, entry, snapshot, relation, or commit.
    NotFound,
    /// A singular API matched more than one object.
    Ambiguous,
    /// The principal may not read/traverse the namespace or object.
    Unauthorized,
    /// The selector names a namespace other than `default`.
    NamespaceMismatch,
    /// The store was opened read-only and a mutation was requested.
    ReadOnly,
    /// The name already exists (or the store already exists).
    Exists,
    /// A path component was expected to be a directory.
    NotDir,
    /// The operation is not valid for a directory.
    IsDir,
    /// The directory is not empty.
    NotEmpty,
    /// A name is empty, over-long, contains NUL, `/`, or is `.`/`..`.
    InvalidName,
    /// A non-name argument is invalid.
    InvalidArgument,
    /// Checksum, hash, length, or chain mismatch.
    Corrupt,
    /// Structural byte/format error.
    Malformed,
    /// A configured resource bound would be exceeded.
    LimitExceeded,
    /// Canonical bytes are not canonical.
    NonCanonical,
    /// Format major/minor not supported.
    UnsupportedVersion,
    /// The operation is not expressible for the object's physical
    /// representation (for example a shrink over an immutable overlay).
    UnsupportedRepresentation,
    /// A cost model was asked to price a representation whose row has not been
    /// measured. The planner refuses rather than substituting a neighbouring
    /// row.
    UnmeasuredCost,
    /// Selector grammar error.
    Parse,
    /// Selector schema string is not `weft.selector.v1`.
    UnknownSchema,
    /// A configured write budget would be exceeded by the commit.
    BudgetExceeded,
    /// A union merge found the same path with different content.
    Conflict,
    /// A declared causal dependency is missing, unreachable, or mismatched.
    DependencyMismatch,
    /// The caller cancelled a read while it was waiting for shared work.
    Cancelled,
    /// Transport/read/write failure.
    Io,
}

impl Class {
    /// The class string used in CLI output and conformance reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Busy => "Busy",
            Class::NotFound => "NotFound",
            Class::Ambiguous => "Ambiguous",
            Class::Unauthorized => "Unauthorized",
            Class::NamespaceMismatch => "NamespaceMismatch",
            Class::ReadOnly => "ReadOnly",
            Class::Exists => "Exists",
            Class::NotDir => "NotDir",
            Class::IsDir => "IsDir",
            Class::NotEmpty => "NotEmpty",
            Class::InvalidName => "InvalidName",
            Class::InvalidArgument => "InvalidArgument",
            Class::Corrupt => "Corrupt",
            Class::Malformed => "Malformed",
            Class::LimitExceeded => "LimitExceeded",
            Class::NonCanonical => "NonCanonical",
            Class::UnsupportedVersion => "UnsupportedVersion",
            Class::UnsupportedRepresentation => "UnsupportedRepresentation",
            Class::UnmeasuredCost => "UnmeasuredCost",
            Class::Parse => "Parse",
            Class::UnknownSchema => "UnknownSchema",
            Class::BudgetExceeded => "BudgetExceeded",
            Class::Conflict => "Conflict",
            Class::DependencyMismatch => "DependencyMismatch",
            Class::Cancelled => "Cancelled",
            Class::Io => "Io",
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed store error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    class: Class,
    message: String,
}

impl Error {
    pub fn new(class: Class, message: impl Into<String>) -> Self {
        Error {
            class,
            message: message.into(),
        }
    }

    pub fn class(&self) -> Class {
        self.class
    }

    /// The normative class name as a string.
    pub fn class_name(&self) -> &'static str {
        self.class.as_str()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(Class::Busy, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Class::NotFound, message)
    }
    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self::new(Class::Ambiguous, message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(Class::Unauthorized, message)
    }
    pub fn namespace_mismatch(message: impl Into<String>) -> Self {
        Self::new(Class::NamespaceMismatch, message)
    }
    pub fn read_only(message: impl Into<String>) -> Self {
        Self::new(Class::ReadOnly, message)
    }
    pub fn exists(message: impl Into<String>) -> Self {
        Self::new(Class::Exists, message)
    }
    pub fn not_dir(message: impl Into<String>) -> Self {
        Self::new(Class::NotDir, message)
    }
    pub fn is_dir(message: impl Into<String>) -> Self {
        Self::new(Class::IsDir, message)
    }
    pub fn not_empty(message: impl Into<String>) -> Self {
        Self::new(Class::NotEmpty, message)
    }
    pub fn invalid_name(message: impl Into<String>) -> Self {
        Self::new(Class::InvalidName, message)
    }
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(Class::InvalidArgument, message)
    }
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(Class::Corrupt, message)
    }
    pub fn malformed(message: impl Into<String>) -> Self {
        Self::new(Class::Malformed, message)
    }
    pub fn limit_exceeded(message: impl Into<String>) -> Self {
        Self::new(Class::LimitExceeded, message)
    }
    pub fn non_canonical(message: impl Into<String>) -> Self {
        Self::new(Class::NonCanonical, message)
    }
    pub fn unsupported_version(message: impl Into<String>) -> Self {
        Self::new(Class::UnsupportedVersion, message)
    }
    pub fn unsupported_representation(message: impl Into<String>) -> Self {
        Self::new(Class::UnsupportedRepresentation, message)
    }
    pub fn unmeasured_cost(message: impl Into<String>) -> Self {
        Self::new(Class::UnmeasuredCost, message)
    }
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(Class::Parse, message)
    }
    pub fn unknown_schema(message: impl Into<String>) -> Self {
        Self::new(Class::UnknownSchema, message)
    }
    pub fn budget_exceeded(message: impl Into<String>) -> Self {
        Self::new(Class::BudgetExceeded, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Class::Conflict, message)
    }
    pub fn dependency_mismatch(message: impl Into<String>) -> Self {
        Self::new(Class::DependencyMismatch, message)
    }
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(Class::Cancelled, message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(Class::Io, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        let class = match err.kind() {
            std::io::ErrorKind::NotFound => Class::NotFound,
            _ => Class::Io,
        };
        Error::new(class, err.to_string())
    }
}

impl From<weftcodec::Error> for Error {
    fn from(err: weftcodec::Error) -> Self {
        let class = match err.class().as_str() {
            "Malformed" => Class::Malformed,
            "Corrupt" => Class::Corrupt,
            "UnsupportedVersion" => Class::UnsupportedVersion,
            "UnsupportedFeature" | "UnsupportedMode" => Class::Malformed,
            "LimitExceeded" => Class::LimitExceeded,
            "NonCanonical" => Class::NonCanonical,
            "Parse" => Class::Parse,
            "UnknownSchema" => Class::UnknownSchema,
            "DependencyMissing" | "DependencyDenied" | "DependencyMismatch" | "DependencyCycle" => {
                Class::Corrupt
            }
            _ => Class::Io,
        };
        Error::new(class, err.to_string())
    }
}

/// Store result alias.
pub type Result<T> = std::result::Result<T, Error>;

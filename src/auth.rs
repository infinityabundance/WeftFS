//! POSIX-style authorization for the v1 store (`WEFT-MODEL.md` §6).
//!
//! v1 is store-local. A principal is `(uid, gid, supplementary groups)`.
//! Read access requires the POSIX read bit for the matching class **and**, when
//! `attrs["weft.acl.read"]` is present, the principal's uid must appear in that
//! array. Traversal requires the execute bit. uid 0 is not special; mount-level
//! root behavior belongs to a future FUSE policy.

use weftcodec::capability::VerifiedEnvelope;
use weftcodec::wcb::Value;

use crate::state::{ObjectRecord, ATTR_ACL_READ};

/// A caller identity supplied by the kernel (or by tests/CLI).
///
/// `envelope` is an **already verified** authenticated capability grant
/// (`WEFT-CAPABILITIES.md` §9). It can only narrow what POSIX/ACL rules allow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub uid: u64,
    pub gid: u64,
    pub groups: Vec<u64>,
    pub envelope: Option<VerifiedEnvelope>,
}

impl Principal {
    pub fn new(uid: u64, gid: u64, groups: Vec<u64>) -> Self {
        Principal {
            uid,
            gid,
            groups,
            envelope: None,
        }
    }

    /// A principal that owns everything created with uid/gid 0.
    pub fn root() -> Self {
        Principal {
            uid: 0,
            gid: 0,
            groups: Vec::new(),
            envelope: None,
        }
    }

    /// Attach a verified envelope. Verification must already have happened;
    /// use [`weftcodec::capability::verify_envelope`] before calling this.
    pub fn with_envelope(mut self, envelope: VerifiedEnvelope) -> Self {
        self.envelope = Some(envelope);
        self
    }

    /// The verified envelope attached to this principal, if any.
    pub fn envelope(&self) -> Option<&VerifiedEnvelope> {
        self.envelope.as_ref()
    }

    fn is_in_group(&self, gid: u64) -> bool {
        self.gid == gid || self.groups.contains(&gid)
    }
}

/// Effective permission bits for this principal (`4` read, `2` write, `1`
/// execute).
pub fn effective_mode(object: &ObjectRecord, principal: &Principal) -> u64 {
    if principal.uid == object.uid {
        (object.mode >> 6) & 0o7
    } else if principal.is_in_group(object.gid) {
        (object.mode >> 3) & 0o7
    } else {
        object.mode & 0o7
    }
}

/// The ACL predicate. Denies on a malformed `weft.acl.read` value (fail closed).
///
/// This is the part of authorization the kernel cannot evaluate itself, so the
/// FUSE frontend applies it in addition to `default_permissions`.
pub fn acl_allows(object: &ObjectRecord, principal: &Principal) -> bool {
    match object.attrs.get(ATTR_ACL_READ) {
        None => true,
        Some(Value::Array(items)) => items
            .iter()
            .any(|item| matches!(item, Value::U64(uid) if *uid == principal.uid)),
        Some(_) => false,
    }
}

/// May the principal read this object?
pub fn can_read(object: &ObjectRecord, principal: &Principal) -> bool {
    (effective_mode(object, principal) & 0o4) != 0 && acl_allows(object, principal)
}

/// May the principal traverse this directory?
pub fn can_traverse(object: &ObjectRecord, principal: &Principal) -> bool {
    (effective_mode(object, principal) & 0o1) != 0
}

/// May the principal write this object?
pub fn can_write(object: &ObjectRecord, principal: &Principal) -> bool {
    (effective_mode(object, principal) & 0o2) != 0 && acl_allows(object, principal)
}

/// Read access to a relation requires visibility of its subject and, when the
/// target is an object, visibility of that target (WEFT-MODEL §6 rule 3).
/// `target_visible` is `true` for value targets and for authorized object
/// targets; callers must pass `false` for a missing or unauthorized target
/// object.
pub fn relation_visible(
    subject: Option<&ObjectRecord>,
    target_visible: bool,
    principal: &Principal,
) -> bool {
    let subject_visible = match subject {
        Some(subject) => can_read(subject, principal),
        None => false,
    };
    subject_visible && target_visible
}

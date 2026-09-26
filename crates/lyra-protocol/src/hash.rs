//! Canonical hashing: RFC 8785 (JCS) serialization followed by SHA-256.

use serde::Serialize;

use crate::ids::Digest;

/// JCS-canonical bytes of a serializable value.
pub fn canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    serde_jcs::to_vec(value).map_err(|e| e.to_string())
}

/// `sha256:` digest of the JCS form of a value.
pub fn canonical_digest<T: Serialize + ?Sized>(value: &T) -> Result<Digest, String> {
    canonical_bytes(value).map(|b| Digest::of_bytes(&b))
}

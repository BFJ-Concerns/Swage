use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) fn synthetic_id(domain: &[u8], space_id: Uuid, name: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    digest.update(space_id.as_bytes());
    digest.update([0]);
    digest.update(name.as_bytes());
    let mut bytes: [u8; 16] = digest.finalize()[..16]
        .try_into()
        .expect("fixed digest slice");
    // Mark this deterministic identifier as an RFC 9562 name-based UUID.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

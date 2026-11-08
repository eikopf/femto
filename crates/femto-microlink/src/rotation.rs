//! Coordinator-bound pending node-key rotation record.
use crate::protocol::{
    identity::{DeviceIdentity, ENCODED_IDENTITY_LEN},
    key::{Node, PublicKey},
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const BODY: usize = 8 + 32 + 32 + ENCODED_IDENTITY_LEN;
pub const RECORD_LEN: usize = BODY + 32;

#[derive(Clone)]
pub struct PendingRotation {
    endpoint: [u8; 32],
    old: PublicKey<Node>,
    candidate: DeviceIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid rotation coordinator")]
    Coordinator,
    #[error("pending rotation belongs to a different coordinator")]
    Endpoint,
    #[error("pending rotation record is corrupt or belongs to another identity")]
    Record,
    #[error("rotation key generation failed")]
    Random,
}

impl PendingRotation {
    pub fn prepare(identity: &DeviceIdentity, host: &str, port: u16) -> Result<Self, Error> {
        let endpoint = endpoint(host, port)?;
        let rotation = identity
            .prepare_node_key_rotation()
            .map_err(|_| Error::Random)?;
        let candidate = identity.rotated(&rotation).map_err(|_| Error::Record)?;
        Ok(Self {
            endpoint,
            old: rotation.old_public(),
            candidate,
        })
    }
    /// Candidate identity for registration. Persistence must precede its use.
    pub fn candidate(&self) -> &DeviceIdentity {
        &self.candidate
    }
    pub fn old_node_key(&self) -> PublicKey<Node> {
        self.old
    }
    pub fn validate_endpoint(&self, host: &str, port: u16) -> Result<(), Error> {
        if self.endpoint != endpoint(host, port)? {
            return Err(Error::Endpoint);
        }
        Ok(())
    }
    pub fn validate_identity(&self, current: &DeviceIdentity) -> Result<(), Error> {
        let node = current.node_key().public();
        if (node != self.old && node != self.candidate.node_key().public())
            || self.old == PublicKey::zero()
            || self.old == self.candidate.node_key().public()
            || current.machine_key().public() != self.candidate.machine_key().public()
            || current.disco_key().public() != self.candidate.disco_key().public()
            || current.network_lock_key().public() != self.candidate.network_lock_key().public()
            || current.backend_log_id() != self.candidate.backend_log_id()
        {
            return Err(Error::Record);
        }
        Ok(())
    }
    pub fn encode(&self) -> Zeroizing<[u8; RECORD_LEN]> {
        let mut bytes = Zeroizing::new([0; RECORD_LEN]);
        bytes[..8].copy_from_slice(b"FEMTORO1");
        bytes[8..40].copy_from_slice(&self.endpoint);
        bytes[40..72].copy_from_slice(self.old.as_bytes());
        let identity = Zeroizing::new(self.candidate.encode());
        bytes[72..BODY].copy_from_slice(&*identity);
        let digest = Sha256::digest(&bytes[..BODY]);
        bytes[BODY..].copy_from_slice(&digest);
        bytes
    }
    pub fn decode(bytes: &[u8], current: &DeviceIdentity) -> Result<Self, Error> {
        if bytes.len() != RECORD_LEN
            || &bytes[..8] != b"FEMTORO1"
            || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..]
        {
            return Err(Error::Record);
        }
        let pending = Self {
            endpoint: bytes[8..40].try_into().map_err(|_| Error::Record)?,
            old: PublicKey::from_bytes(bytes[40..72].try_into().map_err(|_| Error::Record)?),
            candidate: DeviceIdentity::decode(&bytes[72..BODY]).map_err(|_| Error::Record)?,
        };
        pending.validate_identity(current)?;
        Ok(pending)
    }
}

fn endpoint(host: &str, port: u16) -> Result<[u8; 32], Error> {
    let authority = crate::bootstrap::authority(&host.to_ascii_lowercase(), port)
        .map_err(|_| Error::Coordinator)?;
    Ok(Sha256::digest(authority.as_bytes()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_is_bounded_bound_to_identity_and_accepts_both_commit_states() {
        let original = DeviceIdentity::generate().unwrap();
        let pending = PendingRotation::prepare(&original, "Control.Example", 443).unwrap();
        let encoded = pending.encode();
        let restored = PendingRotation::decode(&*encoded, &original).unwrap();
        assert_eq!(restored.old_node_key(), original.node_key().public());
        assert_eq!(
            restored.candidate().node_key().public(),
            pending.candidate().node_key().public()
        );
        assert!(restored.validate_endpoint("control.example", 443).is_ok());
        assert!(restored.validate_endpoint("control.example", 8443).is_err());
        assert!(restored.validate_endpoint("other.example", 443).is_err());
        assert!(PendingRotation::decode(&*encoded, pending.candidate()).is_ok());
        assert!(PendingRotation::decode(&*encoded, &DeviceIdentity::generate().unwrap()).is_err());
        for offset in 0..RECORD_LEN {
            let mut corrupt = Zeroizing::new(*encoded);
            corrupt[offset] ^= 1;
            assert!(PendingRotation::decode(&*corrupt, &original).is_err());
        }
        for length in [0, 7, RECORD_LEN - 1, RECORD_LEN + 1] {
            let mut bad = Zeroizing::new(encoded.to_vec());
            bad.resize(length, 0);
            assert!(PendingRotation::decode(&bad, &original).is_err());
        }
        for (host, port) in [
            ("", 443),
            ("control.example", 0),
            ("https://control.example", 443),
        ] {
            assert!(PendingRotation::prepare(&original, host, port).is_err());
        }
    }
}

//! Discovery message encryption with one key agreement per peer.
//!
//! The protocol library agrees a key for every message it seals or opens,
//! and derives the local public key again for every one it seals. On the
//! reader each of those costs about 50 ms, more than everything else a poll
//! does, so a round of probes to one peer held up all other traffic. The
//! agreed key depends only on the two discovery keys; it is kept here for the
//! peers last talked to.
use crate::protocol::{
    disco::{CallMeMaybe, DiscoError, Ping, Pong},
    key::{Disco, Node, PrivateKey, PublicKey},
};
use crypto_box::{
    PublicKey as BoxPublicKey, SalsaBox, SecretKey as BoxSecretKey,
    aead::{Aead, generic_array::GenericArray},
};
use std::net::{IpAddr, SocketAddr};

const MAGIC: &[u8; 6] = b"TS\xf0\x9f\x92\xac";
const HEADER_LEN: usize = 6 + 32 + 24;
const PING: u8 = 1;
const PONG: u8 = 2;
const CALL_ME_MAYBE: u8 = 3;
/// Agreed keys kept: the peers a reader talks to in one session are few.
const KEPT: usize = 8;

pub enum Message {
    Ping(Ping),
    Pong(Pong),
    CallMeMaybe(CallMeMaybe),
}

pub struct Sealer {
    private: PrivateKey<Disco>,
    public: PublicKey<Disco>,
    /// Most recently used last.
    agreed: Vec<(PublicKey<Disco>, SalsaBox)>,
}
impl Sealer {
    pub fn new(private: PrivateKey<Disco>) -> Self {
        Self {
            public: private.public(),
            private,
            agreed: Vec::new(),
        }
    }
    pub fn clear(&mut self) {
        self.agreed.clear();
    }
    fn cipher(&mut self, remote: PublicKey<Disco>) -> &SalsaBox {
        match self.agreed.iter().position(|(key, _)| *key == remote) {
            Some(index) => {
                let entry = self.agreed.remove(index);
                self.agreed.push(entry);
            }
            None => {
                if self.agreed.len() == KEPT {
                    self.agreed.remove(0);
                }
                let cipher = SalsaBox::new(
                    &BoxPublicKey::from(*remote.as_bytes()),
                    &BoxSecretKey::from(*self.private.as_bytes()),
                );
                self.agreed.push((remote, cipher));
            }
        }
        &self.agreed.last().expect("entry just placed").1
    }
    /// The sender a packet names, before any cryptography. Callers check it
    /// against the peers they know, so a stranger's packet costs nothing.
    pub fn sender(packet: &[u8]) -> Result<PublicKey<Disco>, DiscoError> {
        if packet.len() < HEADER_LEN + 16 + 2 || !packet.starts_with(MAGIC) {
            return Err(DiscoError::InvalidPacket);
        }
        Ok(PublicKey::from_bytes(
            packet[6..38].try_into().expect("fixed-length slice"),
        ))
    }
    pub fn open(&mut self, packet: &[u8]) -> Result<Message, DiscoError> {
        let sender = Self::sender(packet)?;
        let plaintext = self
            .cipher(sender)
            .decrypt(GenericArray::from_slice(&packet[38..62]), &packet[62..])
            .map_err(|_| DiscoError::Authentication)?;
        let endpoint = |bytes: &[u8; 18]| {
            let ip = IpAddr::from(<[u8; 16]>::try_from(&bytes[..16]).expect("fixed chunk"));
            let port = u16::from_be_bytes([bytes[16], bytes[17]]);
            SocketAddr::new(ip.to_canonical(), port)
        };
        let version = plaintext.get(1).copied();
        match plaintext.first() {
            Some(&PING) if version == Some(0) && plaintext.len() >= 14 => Ok(Message::Ping(Ping {
                sender_disco_key: sender,
                transaction_id: plaintext[2..14].try_into().expect("fixed-length slice"),
                node_key: (plaintext.len() >= 46).then(|| {
                    PublicKey::<Node>::from_bytes(
                        plaintext[14..46].try_into().expect("fixed-length slice"),
                    )
                }),
            })),
            Some(&PONG) if version == Some(0) && plaintext.len() >= 32 => Ok(Message::Pong(Pong {
                sender_disco_key: sender,
                transaction_id: plaintext[2..14].try_into().expect("fixed-length slice"),
                observed_source: endpoint(
                    plaintext[14..32].try_into().expect("fixed-length slice"),
                ),
            })),
            Some(&CALL_ME_MAYBE)
                if version == Some(0) && plaintext.len() > 2 && (plaintext.len() - 2) % 18 == 0 =>
            {
                Ok(Message::CallMeMaybe(CallMeMaybe {
                    sender_disco_key: sender,
                    endpoints: plaintext[2..]
                        .as_chunks::<18>()
                        .0
                        .iter()
                        .map(endpoint)
                        .collect(),
                }))
            }
            _ => Err(DiscoError::InvalidPacket),
        }
    }
    fn seal(&mut self, remote: PublicKey<Disco>, plaintext: &[u8]) -> Result<Vec<u8>, DiscoError> {
        let mut nonce = [0; 24];
        getrandom::getrandom(&mut nonce).map_err(|_| DiscoError::Random)?;
        let ciphertext = self
            .cipher(remote)
            .encrypt(GenericArray::from_slice(&nonce), plaintext)
            .map_err(|_| DiscoError::Authentication)?;
        let mut packet = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        packet.extend_from_slice(MAGIC);
        packet.extend_from_slice(self.public.as_bytes());
        packet.extend_from_slice(&nonce);
        packet.extend_from_slice(&ciphertext);
        Ok(packet)
    }
    pub fn ping(
        &mut self,
        remote: PublicKey<Disco>,
        transaction_id: [u8; 12],
        node_key: PublicKey<Node>,
    ) -> Result<Vec<u8>, DiscoError> {
        let mut plaintext = Vec::with_capacity(46);
        plaintext.extend_from_slice(&[PING, 0]);
        plaintext.extend_from_slice(&transaction_id);
        plaintext.extend_from_slice(node_key.as_bytes());
        self.seal(remote, &plaintext)
    }
    pub fn pong(
        &mut self,
        remote: PublicKey<Disco>,
        transaction_id: [u8; 12],
        observed_source: SocketAddr,
    ) -> Result<Vec<u8>, DiscoError> {
        let mut plaintext = Vec::with_capacity(32);
        plaintext.extend_from_slice(&[PONG, 0]);
        plaintext.extend_from_slice(&transaction_id);
        plaintext.extend_from_slice(&as_16(observed_source.ip()));
        plaintext.extend_from_slice(&observed_source.port().to_be_bytes());
        self.seal(remote, &plaintext)
    }
    pub fn call_me_maybe(
        &mut self,
        remote: PublicKey<Disco>,
        endpoints: &[SocketAddr],
    ) -> Result<Vec<u8>, DiscoError> {
        if endpoints.is_empty() || endpoints.len() > usize::from(u16::MAX) {
            return Err(DiscoError::InvalidEndpoints);
        }
        let mut plaintext = Vec::with_capacity(2 + endpoints.len() * 18);
        plaintext.extend_from_slice(&[CALL_ME_MAYBE, 0]);
        for endpoint in endpoints {
            plaintext.extend_from_slice(&as_16(endpoint.ip()));
            plaintext.extend_from_slice(&endpoint.port().to_be_bytes());
        }
        self.seal(remote, &plaintext)
    }
}
fn as_16(address: IpAddr) -> [u8; 16] {
    match address {
        IpAddr::V4(address) => address.to_ipv6_mapped().octets(),
        IpAddr::V6(address) => address.octets(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{disco, identity::DeviceIdentity};

    /// Every message kind passes between this module and the protocol
    /// library in both directions, with the agreed key reused.
    #[test]
    fn messages_agree_with_the_protocol_library_in_both_directions() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let (bd, bn) = (b.disco_key().public(), b.node_key().public());
        let mut sealer = Sealer::new(a.disco_key().clone());
        let source: SocketAddr = "192.0.2.7:41641".parse().unwrap();
        let endpoints: Vec<SocketAddr> = vec![source, "[2001:db8::1]:4".parse().unwrap()];

        let ping = disco::open_ping(b.disco_key(), &sealer.ping(bd, [1; 12], bn).unwrap()).unwrap();
        assert_eq!(ping.sender_disco_key, a.disco_key().public());
        assert_eq!((ping.transaction_id, ping.node_key), ([1; 12], Some(bn)));
        let pong =
            disco::open_pong(b.disco_key(), &sealer.pong(bd, [2; 12], source).unwrap()).unwrap();
        assert_eq!(
            (pong.transaction_id, pong.observed_source),
            ([2; 12], source)
        );
        let call = disco::open_call_me_maybe(
            b.disco_key(),
            &sealer.call_me_maybe(bd, &endpoints).unwrap(),
        )
        .unwrap();
        assert_eq!(call.endpoints, endpoints);
        assert_eq!(sealer.agreed.len(), 1);

        let ad = a.disco_key().public();
        let packet = disco::seal_ping(b.disco_key(), ad, [3; 12], bn).unwrap();
        assert_eq!(Sealer::sender(&packet).unwrap(), bd);
        assert!(matches!(
            sealer.open(&packet).unwrap(),
            Message::Ping(Ping { transaction_id, node_key: Some(node), .. })
                if transaction_id == [3; 12] && node == bn
        ));
        let packet = disco::seal_pong(b.disco_key(), ad, [4; 12], source).unwrap();
        assert!(matches!(
            sealer.open(&packet).unwrap(),
            Message::Pong(Pong { transaction_id, observed_source, .. })
                if transaction_id == [4; 12] && observed_source == source
        ));
        let packet = disco::seal_call_me_maybe(b.disco_key(), ad, &endpoints).unwrap();
        assert!(matches!(
            sealer.open(&packet).unwrap(),
            Message::CallMeMaybe(call) if call.endpoints == endpoints
        ));
        assert_eq!(sealer.agreed.len(), 1);
    }
    #[test]
    fn forged_and_malformed_packets_are_refused_and_kept_keys_are_bounded() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let mut sealer = Sealer::new(a.disco_key().clone());
        let mut packet =
            disco::seal_ping(b.disco_key(), a.disco_key().public(), [1; 12], Key::zero()).unwrap();
        assert!(Sealer::sender(&packet[..70]).is_err());
        let last = packet.len() - 1;
        packet[last] ^= 1;
        assert!(matches!(
            sealer.open(&packet),
            Err(DiscoError::Authentication)
        ));
        packet[0] = b'X';
        assert!(matches!(
            sealer.open(&packet),
            Err(DiscoError::InvalidPacket)
        ));
        for _ in 0..KEPT + 3 {
            let other = DeviceIdentity::generate().unwrap();
            sealer
                .ping(other.disco_key().public(), [0; 12], Key::zero())
                .unwrap();
        }
        assert_eq!(sealer.agreed.len(), KEPT);
    }
    type Key = PublicKey<Node>;
}

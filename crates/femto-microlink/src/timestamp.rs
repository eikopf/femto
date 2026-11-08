use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Implementations must make a successful reservation durable before returning.
/// An ambiguous write failure must return an error; no timestamp is emitted.
pub trait EpochStore {
    type Error;
    fn load_epoch(&mut self) -> Result<Option<u64>, Self::Error>;
    fn reserve_epoch(&mut self, epoch: u64) -> Result<(), Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum TimestampError<E> {
    #[error("persistent timestamp reservation failed")]
    Storage(E),
    #[error("timestamp space exhausted")]
    Exhausted,
}

/// A single owner serializes initiation timestamps for one persistent identity.
/// Each boot reserves a new second. Subsecond ticks survive the upstream
/// WireGuard encoder's low-24-bit nanosecond masking.
pub struct TimestampClock<S> {
    store: S,
    epoch: u64,
    tick: u32,
}

impl<S: EpochStore> TimestampClock<S> {
    pub fn open(mut store: S, unix_seconds: u64) -> Result<Self, TimestampError<S::Error>> {
        let previous = store.load_epoch().map_err(TimestampError::Storage)?;
        let epoch = match previous {
            Some(previous) => previous
                .checked_add(1)
                .ok_or(TimestampError::Exhausted)?
                .max(unix_seconds),
            None => unix_seconds,
        };
        validate_epoch(epoch)?;
        store
            .reserve_epoch(epoch)
            .map_err(TimestampError::Storage)?;
        Ok(Self {
            store,
            epoch,
            tick: 0,
        })
    }

    pub fn next_timestamp(&mut self) -> Result<SystemTime, TimestampError<S::Error>> {
        if self.tick == 60 {
            let epoch = self.epoch.checked_add(1).ok_or(TimestampError::Exhausted)?;
            validate_epoch(epoch)?;
            self.store
                .reserve_epoch(epoch)
                .map_err(TimestampError::Storage)?;
            self.epoch = epoch;
            self.tick = 0;
        }
        let value = UNIX_EPOCH
            .checked_add(Duration::new(self.epoch, self.tick << 24))
            .ok_or(TimestampError::Exhausted)?;
        self.tick += 1;
        Ok(value)
    }

    pub fn into_store(self) -> S {
        self.store
    }

    pub fn store(&self) -> &S {
        &self.store
    }
}

fn validate_epoch<E>(epoch: u64) -> Result<(), TimestampError<E>> {
    if epoch > u64::MAX - 0x4000_0000_0000_000a {
        return Err(TimestampError::Exhausted);
    }
    UNIX_EPOCH
        .checked_add(Duration::from_secs(epoch))
        .ok_or(TimestampError::Exhausted)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        identity::DeviceIdentity,
        wireguard::{HandshakeInitiator, HandshakeResponder},
    };

    #[derive(Default)]
    struct Store {
        epoch: Option<u64>,
        fail: bool,
    }
    impl EpochStore for Store {
        type Error = &'static str;
        fn load_epoch(&mut self) -> Result<Option<u64>, Self::Error> {
            Ok(self.epoch)
        }
        fn reserve_epoch(&mut self, epoch: u64) -> Result<(), Self::Error> {
            if self.fail {
                return Err("power lost");
            }
            self.epoch = Some(epoch);
            Ok(())
        }
    }

    #[test]
    fn timestamps_remain_monotonic_after_reboot_and_wall_clock_rollback() {
        let mut clock = TimestampClock::open(Store::default(), 1_000_000).unwrap();
        let mut previous = clock.next_timestamp().unwrap();
        for _ in 0..130 {
            let next = clock.next_timestamp().unwrap();
            assert!(next > previous);
            previous = next;
        }
        let mut rebooted = TimestampClock::open(clock.into_store(), 100).unwrap();
        assert!(rebooted.next_timestamp().unwrap() > previous);
    }

    #[test]
    fn failed_reservations_do_not_emit_timestamps() {
        assert!(
            TimestampClock::open(
                Store {
                    fail: true,
                    ..Default::default()
                },
                1
            )
            .is_err()
        );
        let mut clock = TimestampClock::open(Store::default(), 1).unwrap();
        for _ in 0..60 {
            clock.next_timestamp().unwrap();
        }
        clock.store.fail = true;
        assert!(clock.next_timestamp().is_err());
        assert!(clock.next_timestamp().is_err());
        clock.store.fail = false;
        assert_eq!(
            clock.next_timestamp().unwrap(),
            UNIX_EPOCH + Duration::from_secs(2)
        );
    }

    #[test]
    fn distinct_timestamps_survive_actual_wireguard_encoding() {
        let alice = DeviceIdentity::generate().unwrap();
        let bob = DeviceIdentity::generate().unwrap();
        let mut clock = TimestampClock::open(Store::default(), 1_000_000).unwrap();
        let mut last = [0; 12];
        for _ in 0..65 {
            let mut handshake =
                HandshakeInitiator::new(alice.node_key(), bob.node_key().public(), 1).unwrap();
            let message = handshake
                .initiation(clock.next_timestamp().unwrap())
                .unwrap();
            let response = HandshakeResponder::respond(bob.node_key(), &message, 2).unwrap();
            assert!(response.timestamp > last);
            last = response.timestamp;
        }
    }
}

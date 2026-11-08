//! Operational clock readiness; an SNTP sample is not authenticated time.
pub const MIN_UNIX_SECONDS: u64 = 1_767_225_600; // 2026-01-01
pub const MAX_UNIX_SECONDS: u64 = 4_102_444_800; // 2100-01-01, exclusive
pub const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Default)]
pub struct Gate {
    sample: Option<(u64, u64)>,
}
impl Gate {
    pub const fn new() -> Self {
        Self { sample: None }
    }
    pub fn clear(&mut self) {
        self.sample = None;
    }
    pub fn synchronized(&mut self, monotonic_ms: u64, unix_seconds: u64) {
        self.sample = (MIN_UNIX_SECONDS..MAX_UNIX_SECONDS)
            .contains(&unix_seconds)
            .then_some((monotonic_ms, unix_seconds));
    }
    pub fn ready(&mut self, monotonic_ms: u64, unix_seconds: u64) -> bool {
        let valid = self.sample.is_some_and(|(sample_ms, sample_unix)| {
            let Some(age) = monotonic_ms.checked_sub(sample_ms) else {
                return false;
            };
            age < MAX_AGE_MS
                && (MIN_UNIX_SECONDS..MAX_UNIX_SECONDS).contains(&unix_seconds)
                && sample_unix
                    .saturating_add(age / 1000)
                    .abs_diff(unix_seconds)
                    <= 2
        });
        if !valid {
            self.clear();
        }
        valid
    }
}

pub fn valid_server(server: &str) -> bool {
    !server.is_empty()
        && server.len() <= 253
        && server.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

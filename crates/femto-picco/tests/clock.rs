use femto_picco_board::clock::{
    Gate, MAX_AGE_MS, MAX_UNIX_SECONDS, MIN_UNIX_SECONDS, valid_server,
};
#[test]
fn readiness_requires_recent_sync_and_consistent_clocks() {
    let unix = MIN_UNIX_SECONDS + 100;
    let mut gate = Gate::new();
    assert!(!gate.ready(0, unix));
    gate.synchronized(1000, unix);
    assert!(gate.ready(1000, unix));
    assert!(gate.ready(2999, unix + 2));
    assert!(gate.ready(1000 + MAX_AGE_MS - 1, unix + MAX_AGE_MS / 1000));
    assert!(!gate.ready(1000 + MAX_AGE_MS, unix + MAX_AGE_MS / 1000));
    assert!(!gate.ready(1001, unix));
    gate.synchronized(2000, unix);
    assert!(!gate.ready(1999, unix));
    assert!(!gate.ready(2000, unix));
    gate.synchronized(3000, unix);
    assert!(!gate.ready(4000, unix - 20));
    assert!(!gate.ready(4000, unix + 1));
    gate.synchronized(5000, unix);
    assert!(!gate.ready(5000, unix + 20));
    gate.synchronized(6000, unix);
    assert!(gate.ready(6000, unix));
    gate.clear();
    assert!(!gate.ready(6000, unix));
}
#[test]
fn invalid_samples_clear_readiness_and_bounds_do_not_overflow() {
    let mut gate = Gate::new();
    for unix in [0, MIN_UNIX_SECONDS - 1, MAX_UNIX_SECONDS, u64::MAX] {
        gate.synchronized(0, MIN_UNIX_SECONDS);
        gate.synchronized(0, unix);
        assert!(!gate.ready(0, MIN_UNIX_SECONDS));
    }
    gate.synchronized(u64::MAX - 100, MIN_UNIX_SECONDS);
    assert!(gate.ready(u64::MAX, MIN_UNIX_SECONDS));
    gate.synchronized(0, MAX_UNIX_SECONDS - 1);
    assert!(!gate.ready(1000, MAX_UNIX_SECONDS));
}
#[test]
fn time_server_accepts_bounded_dns_names_and_ipv4() {
    for value in [
        "0.pool.ntp.org",
        "192.168.1.2",
        "time-server.lan",
        "localhost",
    ] {
        assert!(valid_server(value));
    }
    for value in [
        "",
        "a..b",
        "a.",
        ".a",
        "-a",
        "a-",
        "a:b",
        "http://a",
        "a\0b",
        "a\nb",
        "ä.example",
    ] {
        assert!(!valid_server(value));
    }
    assert!(!valid_server(&"a".repeat(64)));
    let long = [
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61),
    ]
    .join(".");
    assert!(valid_server(&long));
    assert!(!valid_server(&(long + "x")));
}

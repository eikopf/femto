use femto_picco_board::wifi::{Action, Credentials, Error, Retry};
#[test]
fn bounded_credentials_validate_without_secret_error_text() {
    let valid = br#"{"version":1,"ssid":"Reader network","password":"secret pass"}"#;
    let credentials = Credentials::parse(valid).unwrap();
    assert_eq!(credentials.ssid(), "Reader network");
    assert_eq!(credentials.password(), "secret pass");
    for invalid in [
        r#"{"version":2,"ssid":"x","password":"secret pass"}"#,
        r#"{"version":1,"ssid":"","password":"secret pass"}"#,
        r#"{"version":1,"ssid":"x","password":"short"}"#,
        r#"{"version":1,"ssid":"x","password":"secret pass","unknown":"secret"}"#,
        r#"{"version":1,"ssid":"x","ssid":"y","password":"secret pass"}"#,
    ] {
        assert!(Credentials::parse(invalid.as_bytes()).is_err());
    }
    assert!(matches!(
        Credentials::parse(&vec![b' '; 1025]),
        Err(Error::Size)
    ));
    for (ssid, password, ok) in [
        ("é".repeat(16), "x".repeat(63), true),
        ("é".repeat(17), "x".repeat(8), false),
        ("x".into(), "x".repeat(64), false),
        ("x\n".into(), "x".repeat(8), false),
    ] {
        let bytes =
            serde_json::to_vec(&serde_json::json!({"version":1,"ssid":ssid,"password":password}))
                .unwrap();
        assert_eq!(Credentials::parse(&bytes).is_ok(), ok);
    }
}
#[test]
fn retries_wait_for_dhcp_back_off_and_reset_after_success() {
    let mut retry = Retry::default();
    assert_eq!(retry.poll(0, false), Action::Connect);
    assert_eq!(retry.poll(29999, false), Action::None);
    assert_eq!(retry.poll(30000, false), Action::Disconnect);
    assert_eq!(retry.poll(34999, false), Action::None);
    assert_eq!(retry.poll(35000, false), Action::Connect);
    retry.failed(35000);
    assert_eq!(retry.poll(44999, false), Action::None);
    assert_eq!(retry.poll(45000, false), Action::Connect);
    assert_eq!(retry.poll(45001, true), Action::None);
    assert_eq!(retry.label(), "Wi-Fi online");
    assert_eq!(retry.poll(45002, false), Action::Disconnect);
    assert_eq!(retry.poll(50002, false), Action::Connect);
    assert_eq!(retry.poll(1, false), Action::Disconnect);
    assert_eq!(retry.poll(60000, true), Action::None);
    assert_eq!(retry.label(), "Wi-Fi stopped");
}

#[test]
fn repeated_failures_cap_delay_at_one_minute() {
    let mut retry = Retry::default();
    let mut now = 0;
    for delay in [5000, 10000, 20000, 40000, 60000, 60000, 60000] {
        assert_eq!(retry.poll(now, false), Action::Connect);
        retry.failed(now);
        assert_eq!(retry.poll(now + delay - 1, false), Action::None);
        now += delay;
    }
    assert_eq!(retry.poll(now, false), Action::Connect);
}

#[test]
fn saved_networks_round_trip_validate_and_prefer_the_strongest_visible() {
    use femto_picco_board::wifi::{Error, MAX_NETWORKS, Saved};
    let mut saved = Saved::default();
    assert!(!saved.enabled() && saved.is_empty());
    assert_eq!(saved.remember("", "password1"), Err(Error::Ssid));
    assert_eq!(saved.remember("Home", "short"), Err(Error::Password));
    assert_eq!(
        saved.remember("Home", "pass\u{e9}word"),
        Err(Error::Password)
    );
    saved.remember("Home", "password1").unwrap();
    saved.remember("Caf\u{e9}", "").unwrap();
    saved.remember("Home", "password2").unwrap();
    saved.set_enabled(true);
    let restored = Saved::parse(&saved.to_bytes()).unwrap();
    assert!(restored.enabled());
    assert_eq!(restored.password("Home"), Some("password2"));
    assert_eq!(restored.password("Caf\u{e9}"), Some(""));
    assert_eq!(restored.password("Other"), None);
    assert_eq!(
        restored.choose([("Other", -20), ("Home", -70), ("Caf\u{e9}", -50)]),
        Some("Caf\u{e9}")
    );
    assert_eq!(restored.choose([("Other", -20)]), None);
    assert!(saved.forget("Home") && !saved.forget("Home"));
    for index in 0..MAX_NETWORKS + 2 {
        saved.remember(&format!("net{index}"), "").unwrap();
    }
    // The least recently joined networks are dropped.
    assert_eq!(saved.password("Caf\u{e9}"), None);
    assert_eq!(saved.password("net0"), None);
    assert!(saved.password("net2").is_some());
    assert!(matches!(Saved::parse(b"{}"), Err(Error::Format)));
    assert!(matches!(
        Saved::parse(br#"{"version":2,"enabled":false,"networks":[]}"#),
        Err(Error::Version)
    ));
    assert!(matches!(Saved::parse(&[b' '; 4096]), Err(Error::Size)));
}

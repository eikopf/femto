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

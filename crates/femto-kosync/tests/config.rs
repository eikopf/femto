use femto_kosync::config::{Checksum, Route, Settings};
#[test]
fn configuration_accepts_password_or_key_without_debug_disclosure() {
    let base =
        serde_json::json!({"username":"alice","password":"a secret","device_id":"fixture-one"});
    let settings = Settings::from_json(&serde_json::to_vec(&base).unwrap()).unwrap();
    assert_eq!(settings.client.server(), "https://sync.koreader.rocks/");
    assert_eq!(settings.checksum, Checksum::Binary);
    assert_eq!(settings.route, Route::Lan);
    assert!(!format!("{settings:?}").contains("a secret"));
    let mut input = base.clone();
    input.as_object_mut().unwrap().remove("password");
    input["userkey"] = "5f4dcc3b5aa765d61d8327deb882cf99".into();
    input["route"] = "tailnet".into();
    input["checksum"] = "filename".into();
    let settings = Settings::from_json(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(settings.route, Route::Tailnet);
    assert_eq!(settings.checksum, Checksum::Filename);
    input["password"] = "must not appear".into();
    let error = Settings::from_json(&serde_json::to_vec(&input).unwrap()).unwrap_err();
    assert!(!error.to_string().contains("must not appear"));
}
#[test]
fn configuration_rejects_ambiguous_secrets_invalid_identity_and_implicit_http() {
    let base =
        serde_json::json!({"username":"alice","password":"secret","device_id":"fixture-one"});
    for (field, value) in [
        ("device_id", serde_json::json!("")),
        ("device", serde_json::json!("bad\nname")),
        ("route", serde_json::json!("invalid")),
        ("server", serde_json::json!("http://peer.test")),
        (
            "server",
            serde_json::json!("https://alice:secret@peer.test"),
        ),
        ("old_checkpoint", serde_json::json!({})),
    ] {
        let mut input = base.clone();
        input[field] = value;
        assert!(Settings::from_json(&serde_json::to_vec(&input).unwrap()).is_err());
    }
    assert!(Settings::from_json(&vec![b'x'; 8193]).is_err());
    let mut http = base;
    http["server"] = "http://peer.test".into();
    http["allow_http"] = true.into();
    assert!(Settings::from_json(&serde_json::to_vec(&http).unwrap()).is_ok());
}

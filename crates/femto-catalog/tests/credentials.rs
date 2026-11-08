use femto_catalog::auth::BasicAuth;
const SOURCE: &str = "https://catalog.example/";
#[test]
fn configuration_is_bounded_strict_and_redacted() {
    let valid = br#"{"username":"reader","password":"secret"}"#;
    let auth = BasicAuth::from_json(SOURCE, valid).unwrap();
    assert_eq!(format!("{:?}", auth.clone()), "BasicAuth([redacted])");
    for input in [
        r#"{"username":"reader","password":"secret","extra":true}"#,
        r#"{"username":"reader","password":"secret","password":"duplicate"}"#,
        r#"{"username":"reader","password":"secret","allow_http":"true"}"#,
        r#"{"username":"reader","password":secret}"#,
        r#"{"username":"reader"}"#,
        r#"{"username":"reader","password":"secret\n"}"#,
        r#"{"username":"reader","password":"sécret"}"#,
    ] {
        let error = BasicAuth::from_json(SOURCE, input.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret") && !error.contains("reader"));
    }
    assert!(BasicAuth::from_json(SOURCE, &vec![b' '; 8193]).is_err());
    assert!(BasicAuth::from_json("http://catalog.example/", valid).is_err());
    assert!(
        BasicAuth::from_json(
            "http://catalog.example/",
            br#"{"username":"reader","password":"secret","allow_http":true}"#
        )
        .is_ok()
    );
    let too_long =
        serde_json::to_vec(&serde_json::json!({"username":"reader","password":"x".repeat(1025)}))
            .unwrap();
    assert!(BasicAuth::from_json(SOURCE, &too_long).is_err());
}

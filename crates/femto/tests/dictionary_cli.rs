use std::process::Command;

#[test]
fn dictionary_cli_outputs_plain_definitions_synonyms_and_empty_misses() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/dictionary/example.ifo"
    );
    for (query, count) in [("FONT", 1), ("fonts", 1), ("unknown", 0)] {
        let output = Command::new(env!("CARGO_BIN_EXE_femto"))
            .args(["dictionary-lookup", fixture, query])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["dictionary"], "Femto test dictionary");
        assert_eq!(value["definitions"].as_array().unwrap().len(), count);
        if count != 0 {
            assert_eq!(value["definitions"][0]["word"], "font");
            assert_eq!(
                value["definitions"][0]["text"],
                "A set of letter shapes used to display text."
            );
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args(["dictionary-lookup", fixture, &"x".repeat(256)])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

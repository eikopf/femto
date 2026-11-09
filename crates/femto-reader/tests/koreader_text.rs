use femto_reader::koreader::text_nodes;
use serde::Deserialize;

#[derive(Deserialize)]
struct Corpus {
    engine_revision: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    raw: String,
    preserve: bool,
    cdata: bool,
    strip: bool,
    expected: Vec<String>,
}
#[test]
fn text_nodes_match_the_official_engine() {
    let path = std::env::var_os("FEMTO_KOREADER_TEXT_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/koreader-text-nodes.json")
        });
    let corpus: Corpus = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        corpus.engine_revision,
        "fb4d562a2765c52bff052759112a94a0b05239db"
    );
    for case in corpus.cases {
        let mut actual = Vec::new();
        text_nodes(
            &case.raw,
            0,
            case.preserve,
            case.cdata,
            case.strip,
            |node| {
                assert_eq!(node.origins.len(), node.text.chars().count());
                assert!(
                    node.origins
                        .iter()
                        .all(|&origin| case.raw.is_char_boundary(origin)
                            && node.raw.contains(&origin))
                );
                assert!(node.origins.windows(2).all(|pair| pair[0] <= pair[1]));
                actual.push(node.text);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual, case.expected, "{}", case.name);
    }
}

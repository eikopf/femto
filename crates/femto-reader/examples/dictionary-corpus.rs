//! Compare lookup routing with independently extracted source entries.
use femto_reader::dictionary::Dictionary;
use serde::Deserialize;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    time::Instant,
};

#[derive(Deserialize)]
struct Case {
    query: String,
    expected: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    word: String,
    raw: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: dictionary-corpus DICTIONARY.ifo CASES.jsonl".into());
    }
    let start = Instant::now();
    let mut dictionary = Dictionary::open(&args[0])?;
    let mut steps = 0;
    loop {
        steps += 1;
        if dictionary.prepare_step()? {
            break;
        }
    }
    let prepare_ms = start.elapsed().as_millis();
    let start = Instant::now();
    let mut count = 0;
    let mut definitions = 0;
    for line in BufReader::new(File::open(&args[1])?).lines() {
        let case: Case = serde_json::from_str(&line?)?;
        let found = dictionary.lookup(&case.query)?;
        if found.len() != case.expected.len() {
            return Err(format!(
                "result count for {:?}: {} != {}",
                case.query,
                found.len(),
                case.expected.len()
            )
            .into());
        }
        for (actual, expected) in found.iter().zip(case.expected.iter()) {
            // The oracle supplies independent offsets and bytes; markup conversion is shared.
            let text = quireink_stardict::to_plain_text(&expected.raw);
            if actual.word != expected.word || actual.text != text {
                return Err(format!("definition mismatch for {:?}", case.query).into());
            }
        }
        count += 1;
        definitions += found.len();
    }
    println!(
        "{}",
        serde_json::json!({"name":dictionary.name(), "queries":count,"definitions":definitions,"prepare_steps":steps,"prepare_ms":prepare_ms,"lookup_ms":start.elapsed().as_millis()})
    );
    Ok(())
}

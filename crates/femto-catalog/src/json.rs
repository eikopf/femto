//! Decode the catalog tree without constructing values beyond its limits.
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

const MAX_VALUES: usize = 16_384;
const MAX_DEPTH: usize = 16;
const MAX_STRING_BYTES: usize = 4096;
const MAX_KEY_BYTES: usize = 256;

struct Budget {
    remaining: usize,
    exceeded: Option<&'static str>,
}
impl Budget {
    fn error<E: de::Error>(&mut self, limit: &'static str) -> E {
        self.exceeded = Some(limit);
        E::custom("catalog JSON limit exceeded")
    }
    fn string<E: de::Error>(
        &mut self,
        value: &str,
        max: usize,
        limit: &'static str,
    ) -> Result<String, E> {
        if value.len() > max {
            return Err(self.error(limit));
        }
        Ok(value.to_owned())
    }
}
struct Seed<'a> {
    budget: &'a mut Budget,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.depth > MAX_DEPTH || self.budget.remaining == 0 {
            return Err(self.budget.error("JSON tree"));
        }
        self.budget.remaining -= 1;
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        self.budget
            .string(value, MAX_STRING_BYTES, "string")
            .map(Value::String)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = items.next_element_seed(Seed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut items: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = items.next_key_seed(Key(self.budget))? {
            let value = items.next_value_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            // Keep serde_json's last-value semantics, while counting replaced
            // values against the decoding budget as well.
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
struct Key<'a>(&'a mut Budget);
impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = String;
    fn deserialize<D: Deserializer<'de>>(self, decoder: D) -> Result<String, D::Error> {
        decoder.deserialize_str(self)
    }
}
impl Visitor<'_> for Key<'_> {
    type Value = String;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a bounded JSON property name")
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        self.0.string(value, MAX_KEY_BYTES, "property name")
    }
}

pub(crate) fn parse(bytes: &[u8]) -> crate::Result<Value> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let mut budget = Budget {
        remaining: MAX_VALUES,
        exceeded: None,
    };
    let result = Seed {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(&mut decoder);
    let value = result.map_err(|error| match budget.exceeded {
        Some(limit) => crate::Error::Limit(limit),
        None => crate::Error::Json(error),
    })?;
    decoder.end()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_limits_accept_complete_values_and_reject_the_next_value() {
        let array = format!("[{}null]", "null,".repeat(MAX_VALUES - 2));
        assert_eq!(
            parse(array.as_bytes()).unwrap().as_array().unwrap().len(),
            MAX_VALUES - 1
        );
        let over = format!("[{}null]", "null,".repeat(MAX_VALUES - 1));
        assert!(matches!(
            parse(over.as_bytes()),
            Err(crate::Error::Limit("JSON tree"))
        ));
        for depth in [MAX_DEPTH, MAX_DEPTH + 1] {
            let nested = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
            assert_eq!(parse(nested.as_bytes()).is_ok(), depth == MAX_DEPTH);
        }
        for (length, accepted) in [(4096, true), (4097, false)] {
            let string = format!("\"{}\"", "\\u0061".repeat(length));
            assert_eq!(parse(string.as_bytes()).is_ok(), accepted);
        }
        for (length, accepted) in [(256, true), (257, false)] {
            let object = format!("{{\"{}\":null}}", "\\u0061".repeat(length));
            assert_eq!(parse(object.as_bytes()).is_ok(), accepted);
        }
    }

    #[test]
    fn duplicate_values_consume_budget_and_trailing_input_is_rejected() {
        let object = format!("{{{}\"x\":null}}", "\"x\":null,".repeat(MAX_VALUES - 2));
        assert_eq!(
            parse(object.as_bytes()).unwrap(),
            serde_json::json!({"x":null})
        );
        let over = format!("{{{}\"x\":null}}", "\"x\":null,".repeat(MAX_VALUES - 1));
        assert!(matches!(
            parse(over.as_bytes()),
            Err(crate::Error::Limit("JSON tree"))
        ));
        for bytes in [
            b"null false".as_slice(),
            b"{}{}",
            b"[]?",
            b"1e309",
            b"\"\\uD800\"",
        ] {
            assert!(matches!(parse(bytes), Err(crate::Error::Json(_))));
        }
    }

    #[test]
    fn valid_values_and_byte_mutations_match_the_pinned_json_decoder() {
        let cases: &[&[u8]] = &[
            br#"{"a":[null,true,false,0,-1,1.5,1e30,"\uD83D\uDE00"],"a":{"b":1}}"#,
            br#"[-9223372036854775808,18446744073709551615,-0,1e-100]"#,
            "{\"é\":\"שלום العربية\",\"😀\":[{},[]]}".as_bytes(),
        ];
        for original in cases {
            assert_eq!(
                parse(original).unwrap(),
                serde_json::from_slice::<Value>(original).unwrap()
            );
            for offset in 0..original.len() {
                let mut mutated = original.to_vec();
                for byte in 0..=255 {
                    mutated[offset] = byte;
                    match serde_json::from_slice::<Value>(&mutated) {
                        Ok(expected) => assert_eq!(parse(&mutated).unwrap(), expected),
                        Err(_) => assert!(parse(&mutated).is_err()),
                    }
                }
            }
        }
    }
}

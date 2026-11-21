//! Field extraction for JSON-lines input.
//!
//! Only the top-level fields that matter are kept; everything else is
//! skipped by the parser without being built into a value tree.
//!
//! | Meaning   | Keys (first one present wins)                     |
//! |-----------|---------------------------------------------------|
//! | timestamp | `timestamp` `time` `ts` `@timestamp` `t`          |
//! | level     | `level` `lvl` `severity` `loglevel` `log.level` `levelname` |
//! | message   | `message` `msg` `event`                           |

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

/// A string or number field; other JSON types are not interpreted.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Str(String),
    Num(f64),
    Other,
}

/// The interesting parts of one JSON object line.
#[derive(Debug, Default, PartialEq)]
pub struct JsonLine {
    pub timestamp: Option<Scalar>,
    pub level: Option<Scalar>,
    pub message: Option<String>,
    /// Top-level keys in order of appearance, comma separated.
    pub keys: String,
}

/// Parses a line as a JSON object. Anything else, including valid JSON that
/// is not an object, is an error.
pub fn parse(line: &str) -> Result<JsonLine, serde_json::Error> {
    serde_json::from_str(line)
}

#[derive(Clone, Copy)]
enum Field {
    Timestamp,
    Level,
    Message,
    Other,
}

/// Classifies a key and records its name.
struct KeySeed<'a>(&'a mut String);

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Field;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Field, D::Error> {
        d.deserialize_str(self)
    }
}

impl Visitor<'_> for KeySeed<'_> {
    type Value = Field;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object key")
    }

    fn visit_str<E>(self, key: &str) -> Result<Field, E> {
        if !self.0.is_empty() {
            self.0.push(',');
        }
        self.0.push_str(key);
        Ok(match key {
            "timestamp" | "time" | "ts" | "@timestamp" | "t" => Field::Timestamp,
            "level" | "lvl" | "severity" | "loglevel" | "log.level" | "levelname" => Field::Level,
            "message" | "msg" | "event" => Field::Message,
            _ => Field::Other,
        })
    }
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ScalarVisitor)
    }
}

struct ScalarVisitor;

impl<'de> Visitor<'de> for ScalarVisitor {
    type Value = Scalar;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_str<E>(self, v: &str) -> Result<Scalar, E> {
        Ok(Scalar::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Scalar, E> {
        Ok(Scalar::Str(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Scalar, E> {
        Ok(Scalar::Num(v))
    }

    fn visit_bool<E>(self, _: bool) -> Result<Scalar, E> {
        Ok(Scalar::Other)
    }

    fn visit_unit<E>(self) -> Result<Scalar, E> {
        Ok(Scalar::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Scalar::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Scalar::Other)
    }
}

impl<'de> Deserialize<'de> for JsonLine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(LineVisitor)
    }
}

struct LineVisitor;

impl<'de> Visitor<'de> for LineVisitor {
    type Value = JsonLine;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsonLine, A::Error> {
        let mut line = JsonLine::default();
        while let Some(field) = map.next_key_seed(KeySeed(&mut line.keys))? {
            match field {
                Field::Timestamp if line.timestamp.is_none() => {
                    line.timestamp = Some(map.next_value()?);
                }
                Field::Level if line.level.is_none() => {
                    line.level = Some(map.next_value()?);
                }
                Field::Message if line.message.is_none() => {
                    if let Scalar::Str(s) = map.next_value()? {
                        line.message = Some(s);
                    }
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_known_fields() {
        let line = parse(
            r#"{"time":"2024-01-15T10:30:00Z","level":"error","msg":"boom","user":{"id":7},"tags":[1,2]}"#,
        )
        .unwrap();
        assert_eq!(
            line.timestamp,
            Some(Scalar::Str("2024-01-15T10:30:00Z".into()))
        );
        assert_eq!(line.level, Some(Scalar::Str("error".into())));
        assert_eq!(line.message.as_deref(), Some("boom"));
        assert_eq!(line.keys, "time,level,msg,user,tags");
    }

    #[test]
    fn numeric_fields() {
        let line = parse(r#"{"level":30,"time":1705314600123,"msg":"ok"}"#).unwrap();
        assert_eq!(line.level, Some(Scalar::Num(30.0)));
        assert_eq!(line.timestamp, Some(Scalar::Num(1_705_314_600_123.0)));
    }

    #[test]
    fn first_matching_key_wins() {
        let line = parse(r#"{"msg":"a","message":"b","ts":1,"time":2}"#).unwrap();
        assert_eq!(line.message.as_deref(), Some("a"));
        assert_eq!(line.timestamp, Some(Scalar::Num(1.0)));
    }

    #[test]
    fn odd_types_do_not_fail() {
        let line = parse(r#"{"msg":{"nested":true},"level":null,"ts":[1]}"#).unwrap();
        assert_eq!(line.message, None);
        assert_eq!(line.level, Some(Scalar::Other));
        assert_eq!(line.timestamp, Some(Scalar::Other));
    }

    #[test]
    fn escapes_are_decoded() {
        let line = parse(r#"{"msg":"tab\there é"}"#).unwrap();
        assert_eq!(line.message.as_deref(), Some("tab\there é"));
    }

    #[test]
    fn rejects_non_objects_and_broken_json() {
        assert!(parse(r#"{"msg":"cut"#).is_err());
        assert!(parse(r#"{"msg":"a"} trailing"#).is_err());
        assert!(parse("[1,2,3]").is_err());
        assert!(parse("{not json}").is_err());
    }
}

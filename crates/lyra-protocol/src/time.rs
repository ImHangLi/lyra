//! RFC 3339 UTC timestamps with fixed millisecond precision, e.g. `2026-09-26T01:34:00.000Z`.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    unix_ms: i64,
}

impl Timestamp {
    pub fn now() -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            unix_ms: (now.unix_timestamp_nanos() / 1_000_000) as i64,
        }
    }
    pub fn from_unix_ms(unix_ms: i64) -> Self {
        Self { unix_ms }
    }
    pub fn unix_ms(self) -> i64 {
        self.unix_ms
    }
    /// Parses only the canonical form: UTC `Z`, exactly three fractional digits.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        let b = s.as_bytes();
        if b.len() != 24 || b[19] != b'.' || b[23] != b'Z' {
            return Err("expected canonical RFC 3339 UTC with milliseconds");
        }
        let dt = OffsetDateTime::parse(s, &Rfc3339).map_err(|_| "invalid RFC 3339 timestamp")?;
        let ts = Self {
            unix_ms: (dt.unix_timestamp_nanos() / 1_000_000) as i64,
        };
        if ts.to_string() != s {
            return Err("timestamp is not in canonical form");
        }
        Ok(ts)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = i128::from(self.unix_ms) * 1_000_000;
        match OffsetDateTime::from_unix_timestamp_nanos(nanos) {
            Ok(dt) => {
                let dt = dt.to_offset(UtcOffset::UTC);
                write!(
                    f,
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                    dt.year(),
                    u8::from(dt.month()),
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    dt.millisecond()
                )
            }
            Err(_) => f.write_str("invalid-timestamp"),
        }
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}
impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{3}Z$"
        })
    }
}

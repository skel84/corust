//! Readers for Coroot's untyped payloads, and serde helpers for the normalized forms.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// String field, or "" when missing.
pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// String at a JSON pointer, or "".
pub fn sp<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default()
}

pub fn f(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

pub fn i(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or_default()
}

pub fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or_default()
}

pub fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub fn arr_p<'a>(v: &'a Value, ptr: &str) -> &'a [Value] {
    v.pointer(ptr)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub fn strings(v: &Value, key: &str) -> Vec<String> {
    arr(v, key)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Epoch milliseconds to a time; 0, negative and null map to None.
pub fn time_ms(ms: i64) -> Option<DateTime<Utc>> {
    (ms > 0)
        .then(|| Utc.timestamp_millis_opt(ms).single())
        .flatten()
}

/// Reads an epoch-milliseconds field.
pub fn time(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    time_ms(i(v, key))
}

/// Reads a milliseconds field as a duration.
pub fn millis(v: &Value, key: &str) -> Duration {
    Duration::from_millis(i(v, key).max(0) as u64)
}

pub fn is_false(b: &bool) -> bool {
    !*b
}

pub fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Go encodes empty slices and maps as `null`.
pub fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// RFC 3339 with whole seconds, e.g. `2026-10-03T09:09:25Z`.
pub mod rfc3339 {
    pub mod option {
        use chrono::{DateTime, SecondsFormat, Utc};
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            t: &Option<DateTime<Utc>>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match t {
                Some(t) => s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Secs, true)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<DateTime<Utc>>, D::Error> {
            Option::<DateTime<Utc>>::deserialize(d)
        }
    }
}

/// RFC 3339 with milliseconds, e.g. `2026-10-03T09:19:13.260Z`.
pub mod rfc3339_millis {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        DateTime::<Utc>::deserialize(d)
    }
}

/// A duration as whole seconds.
pub mod seconds {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

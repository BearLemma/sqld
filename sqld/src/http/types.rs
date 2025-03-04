use base64::prelude::BASE64_STANDARD_NO_PAD;
use base64::Engine;
use serde::de::Error as _;
use serde::Deserialize;

use crate::query;

#[derive(Debug, Deserialize)]
pub struct HttpQuery {
    pub statements: Vec<QueryObject>,
}

#[derive(Debug)]
pub struct QueryObject {
    pub q: String,
    pub params: QueryParams,
}

#[derive(Debug, Default)]
pub struct QueryParams {
    pub inner: Vec<(Option<String>, query::Value)>,
}

/// Wrapper type to deserialize a payload into a query::Value
struct ValueDeserializer(query::Value);

impl<'de> Deserialize<'de> for ValueDeserializer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = query::Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a valid SQLite value")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Null)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Null)
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Text(v))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Text(v.to_string()))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Integer(v))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(query::Value::Real(v))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                match map.next_entry::<&str, &str>()? {
                    Some((k, v)) => {
                        if k == "blob" {
                            // FIXME: If the blog payload is too big, it may block the main thread
                            // for too long in an async context. In this case, it may be necessary
                            // to offload deserialization to a separate thread.
                            let data = BASE64_STANDARD_NO_PAD.decode(v).map_err(|e| {
                                A::Error::invalid_value(
                                    serde::de::Unexpected::Str(v),
                                    &e.to_string().as_str(),
                                )
                            })?;

                            Ok(query::Value::Blob(data))
                        } else {
                            Err(A::Error::unknown_field(k, &["blob"]))
                        }
                    }
                    None => Err(A::Error::missing_field("blob")),
                }
            }
        }

        deserializer.deserialize_any(Visitor).map(ValueDeserializer)
    }
}

impl<'de> Deserialize<'de> for QueryParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = QueryParams;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an array or a map of parameters")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut inner = Vec::new();
                while let Some(val) = seq.next_element::<ValueDeserializer>()? {
                    inner.push((None, val.0));
                }

                Ok(QueryParams { inner })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut inner = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, ValueDeserializer>()? {
                    inner.push((Some(k), v.0))
                }

                Ok(QueryParams { inner })
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl<'de> Deserialize<'de> for QueryObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = QueryObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or an object")
            }

            fn visit_str<E>(self, q: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(QueryObject {
                    q: q.to_string(),
                    params: Default::default(),
                })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut q = None;
                let mut params = None;
                while let Some(k) = map.next_key::<&str>()? {
                    match k {
                        "q" => {
                            if q.is_none() {
                                q.replace(map.next_value::<String>()?);
                            } else {
                                return Err(A::Error::duplicate_field("q"));
                            }
                        }
                        "params" => {
                            if params.is_none() {
                                params.replace(map.next_value::<QueryParams>()?);
                            } else {
                                return Err(A::Error::duplicate_field("params"));
                            }
                        }
                        _ => return Err(A::Error::unknown_field(k, &["q", "params"])),
                    }
                }

                Ok(QueryObject {
                    q: q.ok_or_else(|| A::Error::missing_field("q"))?,
                    params: params.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

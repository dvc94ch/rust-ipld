use crate::JsonError;
use core::convert::TryFrom;
use libipld_base::cid::{self, Cid};
use libipld_base::ipld::Ipld;
use serde::{de, ser, Deserialize, Serialize};
use serde_json::ser::Serializer;
use std::collections::BTreeMap;
use std::fmt;
use std::iter::FromIterator;

const LINK_KEY: &str = "/";

pub fn encode(ipld: &Ipld) -> Result<Box<[u8]>, JsonError> {
    let mut writer = Vec::with_capacity(128);
    let mut ser = Serializer::new(&mut writer);
    serialize(&ipld, &mut ser)?;
    Ok(writer.into_boxed_slice())
}

pub fn decode(data: &[u8]) -> Result<Ipld, JsonError> {
    let mut de = serde_json::Deserializer::from_slice(&data);
    Ok(deserialize(&mut de)?)
}

/// Trouble deserializing a `{ "/": "$cid" }` from json.
#[derive(Debug)]
enum InvalidLink {
    InvalidEncoding(String, base64::DecodeError),
    InvalidCid(String, cid::Error),
}

impl fmt::Display for InvalidLink {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InvalidLink::InvalidEncoding(s, e) => {
                write!(fmt, "invalid base64 encoding in link {:?}: {}", s, e)
            },
            InvalidLink::InvalidCid(s, e) => {
                write!(fmt, "invalid cid in link {:?}: {}", s, e)
            }
        }
    }
}

impl std::error::Error for InvalidLink {}

fn serialize<S>(ipld: &Ipld, ser: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    match &ipld {
        Ipld::Null => ser.serialize_none(),
        Ipld::Bool(bool) => ser.serialize_bool(*bool),
        Ipld::Integer(i128) => ser.serialize_i128(*i128),
        Ipld::Float(f64) => ser.serialize_f64(*f64),
        Ipld::String(string) => ser.serialize_str(&string),
        Ipld::Bytes(bytes) => ser.serialize_bytes(&bytes),
        Ipld::List(list) => {
            let wrapped = list.iter().map(|ipld| Wrapper(ipld));
            ser.collect_seq(wrapped)
        }
        Ipld::Map(map) => {
            let wrapped = map.iter().map(|(key, ipld)| (key, Wrapper(ipld)));
            ser.collect_map(wrapped)
        }
        Ipld::Link(link) => {
            let value = base64::encode(&link.to_bytes());
            let mut map = BTreeMap::new();
            map.insert("/", value);

            ser.collect_map(map)
        }
    }
}

fn deserialize<'de, D>(deserializer: D) -> Result<Ipld, D::Error>
where
    D: de::Deserializer<'de>,
{
    deserializer.deserialize_any(JSONVisitor)
}

// Needed for `collect_seq` and `collect_map` in Seserializer
struct Wrapper<'a>(&'a Ipld);
impl<'a> Serialize for Wrapper<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        serialize(&self.0, serializer)
    }
}

// serde deserializer visitor that is used by Deseraliazer to decode
// json into IPLD.
struct JSONVisitor;
impl<'de> de::Visitor<'de> for JSONVisitor {
    type Value = Ipld;

    fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.write_str("any valid JSON value")
    }

    #[inline]
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_string(String::from(value))
    }

    #[inline]
    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::String(value))
    }
    #[inline]
    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_byte_buf(v.to_owned())
    }

    #[inline]
    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Bytes(v))
    }

    #[inline]
    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Integer(v.into()))
    }

    #[inline]
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Integer(v.into()))
    }

    #[inline]
    fn visit_i128<E>(self, v: i128) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Integer(v))
    }

    #[inline]
    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Bool(v))
    }

    #[inline]
    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_unit()
    }

    #[inline]
    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Null)
    }

    #[inline]
    fn visit_seq<V>(self, mut visitor: V) -> Result<Self::Value, V::Error>
    where
        V: de::SeqAccess<'de>,
    {
        let mut vec: Vec<WrapperOwned> = Vec::new();

        while let Some(elem) = visitor.next_element()? {
            vec.push(elem);
        }

        let unwrapped = vec.into_iter().map(|WrapperOwned(ipld)| ipld).collect();
        Ok(Ipld::List(unwrapped))
    }

    #[inline]
    fn visit_map<V>(self, mut visitor: V) -> Result<Self::Value, V::Error>
    where
        V: de::MapAccess<'de>,
    {
        let mut values: Vec<(String, WrapperOwned)> = Vec::new();

        while let Some((key, value)) = visitor.next_entry()? {
            values.push((key, value));
        }


        // JSON Object represents IPLD Link if it is `{ "/": "...." }` therefor
        // we valiadet if that is the case here.
        if let Some((key, WrapperOwned(Ipld::String(_)))) = values.first() {
            if key == LINK_KEY && values.len() == 1 {
                // TODO: Find out what is the expected behavior in cases where
                // value is not a valid CID (or base64 endode string here). For
                // now treat it as some other JSON Object.

                let value = if let Some((_, WrapperOwned(Ipld::String(value)))) = values.pop() {
                    value
                } else {
                    unreachable!("IPLD variant already checked and values.len already checked");
                };

                let raw_cid = match base64::decode(&value) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return Err(serde::de::Error::custom(InvalidLink::InvalidEncoding(value, e)));
                    }
                };

                let cid = match Cid::try_from(raw_cid) {
                    Ok(cid) => cid,
                    Err(e) => {
                        return Err(serde::de::Error::custom(InvalidLink::InvalidCid(value, e)));
                    }
                };

                return Ok(Ipld::Link(cid));
            }
        }

        let unwrapped = values
            .into_iter()
            .map(|(key, WrapperOwned(value))| (key, value));
        Ok(Ipld::Map(BTreeMap::from_iter(unwrapped)))
    }

    #[inline]
    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Ipld::Float(v))
    }
}

// Needed for `visit_seq` and `visit_map` in Deserializer
/// We cannot directly implement `serde::Deserializer` for `Ipld` as it is a remote type.
/// Instead wrap it into a newtype struct and implement `serde::Deserialize` for that one.
/// All the deserializer does is calling the `deserialize()` function we defined which returns
/// an unwrapped `Ipld` instance. Wrap that `Ipld` instance in `Wrapper` and return it.
/// Users of this wrapper will then unwrap it again so that they can return the expected `Ipld`
/// instance.
struct WrapperOwned(Ipld);
impl<'de> Deserialize<'de> for WrapperOwned {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let deserialized = deserialize(deserializer);
        // Better version of Ok(Wrapper(deserialized.unwrap()))
        deserialized.map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use multihash::Sha2_256;

    #[test]
    fn encode_struct() {
        let digest = Sha2_256::digest(b"block");
        let cid = Cid::new_v0(digest).unwrap();

        // Create a contact object that looks like:
        // Contact { name: "Hello World", details: CID }
        let mut map = BTreeMap::new();
        map.insert("name".to_string(), Ipld::String("Hello World!".to_string()));
        map.insert("details".to_string(), Ipld::Link(cid.clone()));
        let contact = Ipld::Map(map);

        let contact_encoded = encode(&contact).unwrap();
        println!("encoded: {:02x?}", contact_encoded);
        println!(
            "encoded string {}",
            std::str::from_utf8(&contact_encoded).unwrap()
        );

        assert_eq!(
            std::str::from_utf8(&contact_encoded).unwrap(),
            format!(
                r#"{{"details":{{"/":"{}"}},"name":"Hello World!"}}"#,
                base64::encode(cid.to_bytes()),
            )
        );

        let contact_decoded: Ipld = decode(&contact_encoded).unwrap();
        assert_eq!(contact_decoded, contact);
    }

    #[test]
    fn decode_invalid() {
        let input = r#"{ "/": "invalidcid" }"#;

        // this should error:
        // invalid base64 encoding in link "invalidcid": Invalid last symbol 100, offset 9. at line 1 column 21
        decode(input.as_bytes()).unwrap_err();
    }
}

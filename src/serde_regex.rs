use regex::Regex;
use std::borrow::Cow;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

// Newtype so we can impl Deserialize for Regex.
pub struct Serde<T>(pub T);

impl<'de> Deserialize<'de> for Serde<Regex> {
    fn deserialize<D>(d: D) -> Result<Serde<Regex>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = <Cow<str>>::deserialize(d)?;

        match s.parse() {
            Ok(regex) => Ok(Serde(regex)),
            Err(err) => Err(D::Error::custom(err)),
        }
    }
}

pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    Serde<T>: Deserialize<'de>,
{
    Serde::deserialize(deserializer).map(|x| x.0)
}

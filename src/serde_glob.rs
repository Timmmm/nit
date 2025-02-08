use glob::Pattern;
use std::borrow::Cow;

use serde::de::Error;
use serde::{Deserialize, Deserializer};

// Newtype so we can impl Deserialize for Pattern.
pub struct Serde<T>(pub T);

impl<'de> Deserialize<'de> for Serde<Pattern> {
    fn deserialize<D>(d: D) -> Result<Serde<Pattern>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = <Cow<str>>::deserialize(d)?;

        match s.parse() {
            Ok(pattern) => Ok(Serde(pattern)),
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

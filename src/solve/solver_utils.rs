use anyhow::Result;
use serde::{
    de::{Deserializer, Error as _},
    Deserialize,
};
use std::borrow::Cow;

pub fn deserialize_decimal_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let decimal_str = Cow::<str>::deserialize(deserializer)?;
    decimal_str.parse::<f64>().map_err(D::Error::custom)
}

use crate::utils::h160_hexadecimal;
use ethcontract::Address;
use serde_derive::Deserialize;
use serde_derive::Serialize;

use std::env;
use std::fs::File;
use std::io::Read;

pub fn get_buffer_tradable_token_list() -> BufferTradingTokenList {
    let mut file = File::open(
        env::var("TRADEABLE_BUFFER_TOKENS")
            .unwrap_or_else(|_| "./data/token_list_for_buffer_trading.json".to_string()),
    )
    .unwrap();
    let mut data = String::new();
    file.read_to_string(&mut data).unwrap();

    let list: BufferTradingTokenList =
        serde_json::from_str(&data).expect("JSON was not well-formatted");
    list
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferTradingTokenList {
    pub tokens: Vec<Token>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    #[serde(with = "h160_hexadecimal")]
    pub address: Address,
    pub chain_id: u64,
}

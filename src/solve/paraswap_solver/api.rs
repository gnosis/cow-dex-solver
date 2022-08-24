use crate::utils::h160_hexadecimal;
use crate::utils::u256_decimal;
use anyhow::Result;
use derivative::Derivative;
use primitive_types::{H160, U256};
use reqwest::{Client, RequestBuilder, Url};
use serde::{de::Error, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use web3::types::Bytes;

pub fn debug_bytes(
    bytes: &Bytes,
    formatter: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    formatter.write_fmt(format_args!("0x{}", hex::encode(&bytes.0)))
}

const BASE_URL: &str = "https://apiv5.paraswap.io";

#[async_trait::async_trait]
pub trait ParaswapApi: Send + Sync {
    async fn price(&self, query: PriceQuery) -> Result<PriceResponse, ParaswapResponseError>;
    async fn transaction(
        &self,
        query: TransactionBuilderQuery,
    ) -> Result<TransactionBuilderResponse, ParaswapResponseError>;
    async fn get_full_price_info(&self, query: PriceQuery) -> Result<Root>;
}

pub struct DefaultParaswapApi {
    pub client: Client,
    pub partner: String,
}

#[async_trait::async_trait]
impl ParaswapApi for DefaultParaswapApi {
    async fn price(&self, query: PriceQuery) -> Result<PriceResponse, ParaswapResponseError> {
        let query_str = format!("{:?}", &query);
        let url = query.into_url(&self.partner);
        tracing::debug!("Querying Paraswap API (price) for url {}", url);
        let response_text = self
            .client
            .get(url)
            .send()
            .await
            .map_err(ParaswapResponseError::Send)?
            .text()
            .await
            .map_err(ParaswapResponseError::TextFetch)?;
        tracing::debug!("Response from Paraswap API (price): {}", response_text);
        let raw_response = serde_json::from_str::<RawResponse<PriceResponse>>(&response_text)
            .map_err(ParaswapResponseError::DeserializeError)?;
        match raw_response {
            RawResponse::ResponseOk(response) => Ok(response),
            RawResponse::ResponseErr { error: message } => match &message[..] {
                "computePrice Error" => Err(ParaswapResponseError::ComputePrice(
                    query_str.parse().unwrap(),
                )),
                "No routes found with enough liquidity" => {
                    Err(ParaswapResponseError::InsufficientLiquidity)
                }
                "ESTIMATED_LOSS_GREATER_THAN_MAX_IMPACT" => {
                    Err(ParaswapResponseError::TooMuchSlippageOnQuote)
                }
                _ => Err(ParaswapResponseError::UnknownParaswapError(format!(
                    "uncatalogued Price Query error message {}",
                    message
                ))),
            },
        }
    }
    async fn get_full_price_info(&self, query: PriceQuery) -> Result<Root> {
        let url = query.into_url(&self.partner);
        tracing::debug!("Querying Paraswap API (price) for url {}", url);

        let response_text = self
            .client
            .get(url)
            .send()
            .await
            .map_err(ParaswapResponseError::Send)?
            .text()
            .await
            .map_err(ParaswapResponseError::TextFetch)?;
        tracing::debug!("Response from Paraswap API (price): {}", response_text);

        let raw_response = serde_json::from_str::<Root>(&response_text)
            .map_err(ParaswapResponseError::DeserializeError)?;
        Ok(raw_response)
    }
    async fn transaction(
        &self,
        query: TransactionBuilderQuery,
    ) -> Result<TransactionBuilderResponse, ParaswapResponseError> {
        let query = TransactionBuilderQueryWithPartner {
            query,
            partner: &self.partner,
        };

        let query_str = serde_json::to_string(&query).unwrap();
        let response_text = query
            .into_request(&self.client)
            .send()
            .await
            .map_err(ParaswapResponseError::Send)?
            .text()
            .await
            .map_err(ParaswapResponseError::TextFetch)?;
        parse_paraswap_response_text(&response_text, &query_str)
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub price_route: PriceRoute,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceRoute {
    pub best_route: Vec<BestRoute>,
    pub block_number: i64,
    pub contract_address: String,
    pub contract_method: String,
    #[serde(with = "u256_decimal")]
    pub dest_amount: U256,
    pub dest_decimals: i64,
    #[serde(with = "h160_hexadecimal")]
    pub dest_token: H160,
    #[serde(rename = "destUSD")]
    pub dest_usd: String,
    pub gas_cost: String,
    #[serde(rename = "gasCostUSD")]
    pub gas_cost_usd: String,
    pub hmac: String,
    pub max_impact_reached: bool,
    pub network: i64,
    pub partner: String,
    pub partner_fee: i64,
    pub side: String,
    #[serde(with = "u256_decimal")]
    pub src_amount: U256,
    pub src_decimals: i64,
    #[serde(with = "h160_hexadecimal")]
    pub src_token: H160,
    #[serde(rename = "srcUSD")]
    pub src_usd: String,
    pub token_transfer_proxy: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BestRoute {
    pub percent: f64,
    pub swaps: Vec<Swap>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swap {
    pub dest_decimals: i64,
    #[serde(with = "h160_hexadecimal")]
    pub dest_token: H160,
    pub src_decimals: i64,
    #[serde(with = "h160_hexadecimal")]
    pub src_token: H160,
    pub swap_exchanges: Vec<SwapExchange>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapExchange {
    #[serde(with = "u256_decimal")]
    pub dest_amount: U256,
    pub exchange: String,
    pub percent: f64,
    #[serde(with = "u256_decimal")]
    pub src_amount: U256,
}

#[derive(Deserialize)]
#[serde(untagged)]
// Some Paraswap errors may contain both an error and an Ok response.
// In those cases we should treat the response as an error which is why the error variant
// is declared first (serde will encodes a mixed response as the first matching variant).
pub enum RawResponse<Ok> {
    ResponseErr { error: String },
    ResponseOk(Ok),
}

#[derive(Error, Debug)]
pub enum ParaswapResponseError {
    // Represents a failure with Price query
    #[error("computePrice Error from query {0}")]
    ComputePrice(String),

    #[error("No routes found with enough liquidity")]
    InsufficientLiquidity,

    // Represents a failure with TransactionBuilder query
    #[error("ERROR_BUILDING_TRANSACTION from query {0}")]
    BuildingTransaction(String),

    // Occurs when the price changes between the time the price was queried and this request
    #[error("Suspected Rate Change - Please Retry!")]
    PriceChange,

    #[error("Too much slippage on quote - Please Retry!")]
    TooMuchSlippageOnQuote,

    #[error("Error getParaSwapPool - From Price Route {0}")]
    GetParaswapPool(String),

    // Connectivity or non-response error
    #[error("Failed on send")]
    Send(reqwest::Error),

    // Recovered Response but failed on async call of response.text()
    #[error(transparent)]
    TextFetch(reqwest::Error),

    #[error("{0}")]
    UnknownParaswapError(String),

    #[error(transparent)]
    DeserializeError(#[from] serde_json::Error),
}

fn parse_paraswap_response_text(
    response_text: &str,
    query_str: &str,
) -> Result<TransactionBuilderResponse, ParaswapResponseError> {
    match serde_json::from_str::<RawResponse<TransactionBuilderResponse>>(response_text) {
        Ok(RawResponse::ResponseOk(response)) => Ok(response),
        Ok(RawResponse::ResponseErr { error: message }) => match &message[..] {
            "ERROR_BUILDING_TRANSACTION" => Err(ParaswapResponseError::BuildingTransaction(
                query_str.parse().unwrap(),
            )),
            "It seems like the rate has changed, please re-query the latest Price" => {
                Err(ParaswapResponseError::PriceChange)
            }
            "Too much slippage on quote, please try again" => {
                Err(ParaswapResponseError::TooMuchSlippageOnQuote)
            }
            "Error getParaSwapPool" => Err(ParaswapResponseError::GetParaswapPool(
                query_str.parse().unwrap(),
            )),
            _ => Err(ParaswapResponseError::UnknownParaswapError(format!(
                "uncatalogued error message {}",
                message
            ))),
        },
        Err(err) => Err(ParaswapResponseError::DeserializeError(err)),
    }
}

#[derive(Clone, Debug)]
pub enum Side {
    Buy,
    Sell,
}

/// Paraswap price quote query parameters.
#[derive(Clone, Debug)]
pub struct PriceQuery {
    /// source token address
    pub src_token: H160,
    /// destination token address
    pub dest_token: H160,
    /// decimals of from token (according to API needed  to trade any token)
    pub src_decimals: usize,
    /// decimals of to token (according to API needed to trade any token)
    pub dest_decimals: usize,
    /// amount of source token (in the smallest denomination, e.g. for ETH - 10**18)
    pub amount: U256,
    /// Type of order
    pub side: Side,
    /// The list of DEXs to exclude from the computed price route.
    pub exclude_dexs: Option<Vec<String>>,
}

impl PriceQuery {
    pub fn into_url(self, partner: &str) -> Url {
        let mut url = Url::parse(BASE_URL)
            .expect("invalid base url")
            .join("/prices")
            .expect("unexpectedly invalid URL segment");

        let side = match self.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        url.query_pairs_mut()
            .append_pair("partner", partner)
            .append_pair("srcToken", &format!("{:#x}", self.src_token))
            .append_pair("destToken", &format!("{:#x}", self.dest_token))
            .append_pair("srcDecimals", &self.src_decimals.to_string())
            .append_pair("destDecimals", &self.dest_decimals.to_string())
            .append_pair("amount", &self.amount.to_string())
            .append_pair("side", side)
            .append_pair("network", "1");

        if let Some(dexs) = &self.exclude_dexs {
            url.query_pairs_mut()
                .append_pair("excludeDEXS", &dexs.join(","));
        }

        url
    }
}

/// A Paraswap API price response.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PriceResponse {
    /// Opaque type, which the API expects to get echoed back in the exact form when requesting settlement transaction data
    pub price_route_raw: Value,
    /// The estimated in amount (part of price_route but extracted for type safety & convenience)
    pub src_amount: U256,
    /// The estimated out amount (part of price_route but extracted for type safety & convenience)
    pub dest_amount: U256,
    /// The token transfer proxy address to set an allowance for.
    pub token_transfer_proxy: H160,
    pub gas_cost: U256,
}

impl<'de> Deserialize<'de> for PriceResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ParsedRaw {
            price_route: Value,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PriceRoute {
            #[serde(with = "u256_decimal")]
            src_amount: U256,
            #[serde(with = "u256_decimal")]
            dest_amount: U256,
            token_transfer_proxy: H160,
            #[serde(with = "u256_decimal")]
            gas_cost: U256,
        }

        let parsed = ParsedRaw::deserialize(deserializer)?;
        let PriceRoute {
            src_amount,
            dest_amount,
            token_transfer_proxy,
            gas_cost,
        } = serde_json::from_value::<PriceRoute>(parsed.price_route.clone())
            .map_err(D::Error::custom)?;
        Ok(PriceResponse {
            price_route_raw: parsed.price_route,
            src_amount,
            dest_amount,
            token_transfer_proxy,
            gas_cost,
        })
    }
}

/// Paraswap transaction builder query parameters.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionBuilderQuery {
    /// The sold token
    pub src_token: H160,
    /// The received token
    pub dest_token: H160,
    /// The trade amount amount
    #[serde(flatten)]
    pub trade_amount: TradeAmount,
    /// The maximum slippage in BPS.
    pub slippage: u32,
    /// The decimals of the source token
    pub src_decimals: usize,
    /// The decimals of the destination token
    pub dest_decimals: usize,
    /// priceRoute part from /prices endpoint response (without any change)
    pub price_route: Value,
    /// The address of the signer
    pub user_address: H160,
}

/// The amounts for buying and selling.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TradeAmount {
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    Sell {
        /// The source amount
        #[serde(with = "u256_decimal")]
        src_amount: U256,
    },
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    Buy {
        /// The destination amount
        #[serde(with = "u256_decimal")]
        dest_amount: U256,
    },
}

/// A helper struct to wrap a `TransactionBuilderQuery` that we get as input from
/// the `ParaswapApi` trait.
///
/// This is done because the `partner` is longer specified in the headersd but
/// instead in the POST body, but we want the API to stay mostly compatible and
/// not require passing it in every time we build a transaction given that the
/// API instance already knows what the `partner` value is.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransactionBuilderQueryWithPartner<'a> {
    #[serde(flatten)]
    query: TransactionBuilderQuery,
    partner: &'a str,
}

impl TransactionBuilderQueryWithPartner<'_> {
    pub fn into_request(self, client: &Client) -> RequestBuilder {
        let mut url = Url::parse(BASE_URL)
            .expect("invalid base url")
            .join("/transactions/1")
            .expect("unexpectedly invalid URL segment");
        url.query_pairs_mut().append_pair("ignoreChecks", "true");

        tracing::debug!("Paraswap API (transaction) query url: {}", url);
        client.post(url).json(&self)
    }
}

/// Paraswap transaction builder response.
#[derive(Clone, Derivative, Deserialize, Default)]
#[derivative(Debug)]
#[serde(rename_all = "camelCase")]
pub struct TransactionBuilderResponse {
    /// the sender of the built transaction
    pub from: H160,
    /// the target of the built transaction (usually paraswap router)
    pub to: H160,
    /// the chain for which this transaction is valid
    pub chain_id: u64,
    /// the native token value to be set on the transaction
    #[serde(with = "u256_decimal")]
    pub value: U256,
    /// the calldata for the transaction
    #[derivative(Debug(format_with = "debug_bytes"))]
    pub data: Bytes,
    /// the suggested gas price
    #[serde(with = "u256_decimal")]
    pub gas_price: U256,
}

use crate::models::batch_auction_model::BatchAuctionModel;
use crate::models::batch_auction_model::SettledBatchAuctionModel;
use crate::slippage::SlippageCalculator;
use crate::solve;
use anyhow::Result;
use hex::{FromHex, FromHexError};
use primitive_types::H160;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
use std::str::FromStr;
use warp::{
    hyper::StatusCode,
    reply::{self, json, with_status, Json, WithStatus},
    Filter, Rejection, Reply,
};

/// Wraps H160 with FromStr and Deserialize that can handle a `0x` prefix.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct H160Wrapper(pub H160);

impl FromStr for H160Wrapper {
    type Err = FromHexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        Ok(H160Wrapper(H160(FromHex::from_hex(s)?)))
    }
}
pub fn get_solve_request() -> impl Filter<Extract = (BatchAuctionModel,), Error = Rejection> + Clone
{
    warp::path!("solve")
        .and(warp::post())
        .and(extract_payload())
}
const MAX_JSON_BODY_PAYLOAD: u64 = 1024 * 16 * 100000;

fn extract_payload<T: DeserializeOwned + Send>(
) -> impl Filter<Extract = (T,), Error = Rejection> + Clone {
    // (rejecting huge payloads)...
    warp::body::content_length_limit(MAX_JSON_BODY_PAYLOAD).and(warp::body::json())
}
pub fn get_solve_response(result: Result<SettledBatchAuctionModel>) -> WithStatus<Json> {
    match result {
        Ok(solve) => reply::with_status(reply::json(&solve), StatusCode::OK),
        Err(err) => convert_get_solve_error_to_reply(err),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Error<'a> {
    error_type: &'a str,
    description: &'a str,
}

pub fn internal_error(err: anyhow::Error) -> Json {
    json(&Error {
        error_type: "InternalServerError",
        description: &format!("{:?}", err),
    })
}

pub fn convert_get_solve_error_to_reply(err: anyhow::Error) -> WithStatus<Json> {
    tracing::error!(?err, "get_solve error");
    with_status(internal_error(err), StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn get_solve(slippage_calculator: SlippageCalculator) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_solve_request().and_then(move |model| {
        let slippage_calculator = slippage_calculator.clone();
        async move {
        let result = solve::solve(model, slippage_calculator).await;
        Result::<_, Infallible>::Ok(get_solve_response(result))
    }})
}

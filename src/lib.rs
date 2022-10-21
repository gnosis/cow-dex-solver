pub mod api;
pub mod models;
pub mod slippage;
pub mod solve;
pub mod token_list;
pub mod tracing_helper;
pub mod utils;
extern crate serde_derive;

#[macro_use]
extern crate lazy_static;

use std::net::SocketAddr;
use slippage::SlippageCalculator;
use tokio::{task, task::JoinHandle};

pub fn serve_task(address: SocketAddr, slippage_calculator: SlippageCalculator) -> JoinHandle<()> {
    let filter = api::handle_all_routes(slippage_calculator);
    tracing::info!(%address, "serving api");
    task::spawn(warp::serve(filter).bind(address))
}

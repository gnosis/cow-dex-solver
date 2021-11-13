pub mod api;
pub mod http_solver;
pub mod ratio_as_decimal;
pub mod solve;
pub mod tracing_helper;
pub mod u256_decimal;
extern crate serde_derive;

use std::net::SocketAddr;
use tokio::{task, task::JoinHandle};

pub fn serve_task(address: SocketAddr) -> JoinHandle<()> {
    let filter = api::handle_all_routes();
    task::spawn(warp::serve(filter).bind(address))
}

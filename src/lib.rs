pub mod api;
pub mod models;
pub mod solve;
pub mod tracing_helper;
pub mod utils;
extern crate serde_derive;

#[macro_use]
extern crate lazy_static;

use std::net::SocketAddr;
use tokio::{task, task::JoinHandle};

pub fn serve_task(address: SocketAddr) -> JoinHandle<()> {
    let filter = api::handle_all_routes();
    tracing::info!(%address, "serving api");
    task::spawn(warp::serve(filter).bind(address))
}

#![recursion_limit = "256"]
use cowdexsolver::serve_task;
use cowdexsolver::tracing_helper::initialize;
use std::net::SocketAddr;
use structopt::StructOpt;
#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(long, env = "LOG_FILTER", default_value = "warn,debug,info")]
    pub log_filter: String,
    #[structopt(long, env = "BIND_ADDRESS", default_value = "0.0.0.0:8000")]
    bind_address: SocketAddr,
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    initialize(args.log_filter.as_str());
    tracing::info!("running data-server with {:#?}", args);
    let serve_task = serve_task(args.bind_address);
    tokio::select! {
        result = serve_task => tracing::error!(?result, "serve task exited"),
    };
}

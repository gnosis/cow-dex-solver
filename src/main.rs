#![recursion_limit = "256"]
use cowdexsolver::serve_task;
use cowdexsolver::slippage::SlippageCalculator;
use cowdexsolver::tracing_helper::initialize;
use ethcontract::U256;
use std::net::SocketAddr;
use structopt::StructOpt;
#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(long, env = "LOG_FILTER", default_value = "warn,debug,info")]
    pub log_filter: String,
    #[structopt(long, env = "BIND_ADDRESS", default_value = "0.0.0.0:8000")]
    bind_address: SocketAddr,

    /// The relative slippage tolerance to apply to on-chain swaps.
    #[structopt(long, env, default_value = "10")]
    relative_slippage_bps: u32,

    /// The absolute slippage tolerance in native token units to cap relative
    /// slippage at. Default is 0.007 ETH.
    #[structopt(long, env)]
    absolute_slippage_in_native_token: Option<f64>,
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    initialize(args.log_filter.as_str());
    tracing::info!("running data-server with {:#?}", args);
    let slippage_calculator = SlippageCalculator::from_bps(args.relative_slippage_bps, args.absolute_slippage_in_native_token.map(|value| U256::from_f64_lossy(value * 1e18)));
    let serve_task = serve_task(args.bind_address, slippage_calculator);
    tokio::select! {
        result = serve_task => tracing::error!(?result, "serve task exited"),
    };
}

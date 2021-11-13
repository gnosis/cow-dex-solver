mod solve;
use std::convert::Infallible;
use warp::{hyper::StatusCode, Filter, Rejection, Reply};

pub fn handle_all_routes() -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    let solve = solve::get_solve();
    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS", "PUT", "PATCH"])
        .allow_headers(vec!["Origin", "Content-Type", "X-Auth-Token", "X-AppId"]);
    let api_routes = warp::path!("api" / "v1" / ..).and(solve);
    api_routes.recover(handle_rejection).with(cors)
}
// We turn Rejection into Reply to workaround warp not setting CORS headers on rejections.
async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    Ok(warp::reply::with_status(
        format!("{:?}", err),
        StatusCode::INTERNAL_SERVER_ERROR,
    ))
}

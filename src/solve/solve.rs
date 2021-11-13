use crate::models::batch_auction_model::BatchAuctionModel;
use anyhow::Result;

pub fn solve(model: BatchAuctionModel) -> Result<bool> {
    tracing::info!("solving the batch auction: {:?}", model);
    Ok(true)
}

mod paraswap_solver;
mod solver_utils;
mod zeroex_solver;
use crate::models::batch_auction_model::ExecutedOrderModel;
use crate::models::batch_auction_model::InteractionData;
use crate::models::batch_auction_model::OrderModel;
use crate::models::batch_auction_model::SettledBatchAuctionModel;
use crate::models::batch_auction_model::{BatchAuctionModel, TokenInfoModel};
use crate::solve::paraswap_solver::ParaswapSolver;

use crate::solve::paraswap_solver::api::Root;
use crate::solve::solver_utils::Slippage;
use crate::solve::zeroex_solver::api::SwapQuery;
use ethcontract::batch::CallBatch;
use ethcontract::prelude::*;
use std::collections::BTreeMap;
use std::env;

use crate::solve::zeroex_solver::api::SwapResponse;
use crate::solve::zeroex_solver::ZeroExSolver;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use primitive_types::{H160, U256};
use std::collections::HashMap;
use std::time::Duration;

ethcontract::contract!("contracts/artifacts/ERC20.json");

lazy_static! {
    pub static ref TEN_THOUSAND: U256 = U256::from_dec_str("1000").unwrap();
}

pub async fn solve(
    BatchAuctionModel {
        orders, mut tokens, ..
    }: BatchAuctionModel,
) -> Result<SettledBatchAuctionModel> {
    let api_key;
    if env::var("ZEROEX_API_KEY").is_err() {
        api_key = None;
    } else {
        api_key = Some(String::from(env::var("ZEROEX_API_KEY")?));
    }
    if orders.is_empty() {
        return Ok(SettledBatchAuctionModel::default());
    }

    let mut orders: Vec<(usize, OrderModel)> = orders.into_iter().map(|(i, y)| (i, y)).collect();
    // For simplicity, only solve for up to 20 orders
    orders.truncate(20);

    // Step1: get splitted trade amounts per tokenpair for each order via paraswap dex-ag
    let mut paraswap_futures = Vec::new();
    for (i, order) in orders.iter() {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::new(1, 0))
            .user_agent("gp-v2-services/2.0.0")
            .build()
            .unwrap();
        let paraswap_solver =
            ParaswapSolver::new(vec![String::from("ParaSwapPool4")], client.clone());

        paraswap_futures.push(get_paraswap_sub_trades_from_order(
            *i,
            paraswap_solver,
            order,
            tokens.clone(),
        ));
    }
    type MachtedOrderBracket = (
        Vec<Vec<(usize, OrderModel)>>,
        Vec<Vec<(H160, H160, U256, U256)>>,
    );
    let (matched_orders, single_trade_results): MachtedOrderBracket =
        join_all(paraswap_futures).await.into_iter().unzip();
    let mut matched_orders: Vec<(usize, OrderModel)> =
        matched_orders.into_iter().flatten().collect();

    let single_trade_results = single_trade_results.into_iter().flatten().collect();
    let splitted_trade_amounts = get_splitted_trade_amounts_from_trading_vec(single_trade_results);
    for (pair, entry_amouts) in &splitted_trade_amounts {
        tracing::debug!(
            " Before cow merge: trade on pair {:?} with values {:?}",
            pair,
            entry_amouts
        );
    }

    // 2nd step: Removing obvious cow volume from splitted traded amounts, by matching opposite volume
    let updated_traded_amounts;
    let contains_cow = contain_cow(&splitted_trade_amounts);
    if contains_cow {
        tracing::debug!("Found cow and trying to solve it");
        // if there is a cow volume, we try to remove it
        updated_traded_amounts = get_trade_amounts_without_cow_volumes(&splitted_trade_amounts)?;

        for (pair, entry_amouts) in &updated_traded_amounts {
            tracing::debug!(
                " After cow merge: trade on pair {:?} with values {:?}",
                pair,
                entry_amouts
            );
        }
    } else {
        tracing::debug!("Falling back to normal zeroEx solver");

        let mut order_hashmap = HashMap::new();
        for (_, order) in orders.clone().iter() {
            order_hashmap.insert(
                (order.sell_token, order.buy_token),
                (true, order.sell_amount, order.buy_amount),
            );
        }
        updated_traded_amounts = order_hashmap;
    }

    // 3rd step: Get trades from zeroEx of left-over amounts
    let zeroex_futures = updated_traded_amounts.into_iter().map(
        |(pair, (must_satisfy_limit_price, sell_amount, buy_amount))| {
            let cloned_api_key = api_key.clone();
            async move {
                let client = reqwest::ClientBuilder::new()
                    .timeout(Duration::new(1, 0))
                    .user_agent("gp-v2-services/2.0.0")
                    .build()
                    .unwrap();
                let zeroex_solver =
                    ZeroExSolver::new(1u64, cloned_api_key, client.clone()).unwrap();

                let (src_token, dest_token) = pair;
                let query = SwapQuery {
                    sell_token: src_token,
                    buy_token: dest_token,
                    sell_amount: Some(sell_amount),
                    buy_amount: None,
                    slippage_percentage: Slippage::number_from_basis_points(10u16).unwrap(),
                    skip_validation: Some(true),
                };
                (
                    (must_satisfy_limit_price, sell_amount, buy_amount),
                    query.clone(),
                    zeroex_solver.client.get_swap(query).await,
                )
            }
        },
    );
    let swap_results = join_all(zeroex_futures).await;
    let mut swap_results: Vec<(SwapQuery, SwapResponse)> = swap_results
        .iter()
        .map(
            |((must_satisfy_limit_price, sell_amount, buy_amount), query, swap)| match swap {
                Ok(swap) => {
                    if *must_satisfy_limit_price
                        && swap
                            .sell_amount
                            .checked_mul(*buy_amount)
                            .gt(&sell_amount.checked_mul(swap.buy_amount))
                    {
                        return Err(anyhow!("swap price not good enough"));
                    }
                    Ok((query.clone(), swap.clone()))
                }
                Err(err) => Err(anyhow!("error from zeroex:{:?}", err)),
            },
        )
        .filter_map(|s| s.ok())
        .collect::<Vec<(SwapQuery, SwapResponse)>>();

    if !contains_cow {
        matched_orders = Vec::new();
        for (i, order) in orders.iter() {
            if !swap_results
                .clone()
                .into_iter()
                .filter(|(query, swap)| {
                    swap.sell_amount == order.sell_amount
                        && order.buy_token == query.buy_token
                        && order.sell_token == order.sell_token
                })
                .collect::<Vec<(SwapQuery, SwapResponse)>>()
                .is_empty()
            {
                matched_orders.push((*i, order.clone()));
            }
        }
    }

    // 4th step: Get all approvals via a batch requests for the different swap
    let http = Http::new("https://staging-openethereum.mainnet.gnosisdev.com").unwrap();
    let web3 = Web3::new(http);
    let settlement_contract_address: H160 =
        "9008d19f58aabd9ed0d60971565aa8510560ab41".parse().unwrap();
    let mut batch = CallBatch::new(web3.transport());
    let mut calls = Vec::new();
    for (query, swap) in swap_results.clone() {
        let token = ERC20::at(&web3, query.sell_token);
        calls.push(
            token
                .allowance(settlement_contract_address, swap.allowance_target)
                .batch_call(&mut batch),
        )
    }
    batch.execute_all(usize::MAX).await;
    let mut allowances: HashMap<(Address, Address), U256> = HashMap::new();
    for (id, call) in calls.into_iter().enumerate() {
        let call_result = call.await.unwrap_or_else(|_| U256::zero());
        if let Some((query, swap)) = swap_results.clone().get(id) {
            tracing::debug!(
                "Call {} returned {} for query:{:?} and swap:{:?}",
                id,
                call_result,
                query,
                swap
            );
            allowances.insert((query.sell_token, swap.allowance_target), call_result);
        } else {
            tracing::debug!("Call {} returned {}", id, call_result);
        }
    }

    // 5th step: Build settlements with price and interactions
    let mut solution = SettledBatchAuctionModel::default();
    while !swap_results.is_empty() {
        let (query, mut swap) = swap_results.pop().unwrap();
        insert_new_price(
            &mut solution,
            &splitted_trade_amounts,
            query.clone(),
            swap.clone(),
        )?;

        let mut available_buffer = U256::zero();
        if let Some(token_with_buffer) = tokens.clone().get(&query.buy_token) {
            if let Some(buffer) = token_with_buffer.internal_buffer {
                available_buffer = buffer;
            }
        }
        if swap.buy_amount < available_buffer {
            // trade only against internal buffer
            if let Some(mut token_info) = tokens.get_mut(&query.buy_token) {
                if let Some(buffer) = token_info.internal_buffer {
                    token_info.internal_buffer = buffer.checked_sub(available_buffer);
                }
            }
            if let Some(mut token_info) = tokens.get_mut(&query.sell_token) {
                token_info.internal_buffer = Some(swap.sell_amount);
            }
        } else {
            // use external trade
            let spender = swap.allowance_target;
            // Push allowance interaction data, if necessary
            let allowance = allowances
                .entry((query.sell_token, spender))
                .or_insert_with(U256::zero);
            if allowance.lt(&&mut swap.sell_amount) {
                let token = ERC20::at(&web3, query.sell_token);
                let method = token.approve(spender, swap.sell_amount);
                let calldata = method.tx.data.expect("no calldata").0;
                let interaction_item = InteractionData {
                    target: query.sell_token,
                    value: 0.into(),
                    call_data: ethcontract::Bytes(calldata),
                };
                solution.interaction_data.push(interaction_item);
            } else {
                *allowance = allowance.checked_sub(swap.sell_amount).unwrap()
            }
            // put swap tx data into settled_batch_auction
            let interaction_item = InteractionData {
                target: swap.to,
                value: swap.value,
                call_data: ethcontract::Bytes(swap.data.0),
            };
            solution.interaction_data.push(interaction_item);
        }

        // Sort swap_results in such a way that the next pop contains a token already processed in the clearing prices, if there exists one.
        swap_results.sort_by(|a, b| {
            one_token_is_already_in_settlement(&solution, a)
                .cmp(&one_token_is_already_in_settlement(&solution, b))
        })
    }

    // 6th step: Insert traded orders into settlement
    for (i, order) in matched_orders {
        solution.orders.insert(
            i,
            ExecutedOrderModel {
                exec_sell_amount: order.sell_amount,
                exec_buy_amount: order.buy_amount,
            },
        );
    }
    tracing::info!("Found solution: {:?}", solution);
    Ok(solution)
}

async fn get_paraswap_sub_trades_from_order(
    index: usize,
    paraswap_solver: ParaswapSolver,
    order: &OrderModel,
    tokens: BTreeMap<primitive_types::H160, TokenInfoModel>,
) -> (Vec<(usize, OrderModel)>, Vec<(H160, H160, U256, U256)>) {
    // get tokeninfo from ordermodel
    let (price_response, _amount) = match paraswap_solver
        .get_full_price_info_for_order(order, tokens)
        .await
    {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(
                "Could not get price for order {:?} with error: {:?}",
                order,
                err
            );
            return (Vec::new(), Vec::new());
        }
    };
    let mut sub_trades = Vec::new();
    let mut matched_orders = Vec::new();
    if satisfies_limit_price_with_buffer(&price_response, order) {
        matched_orders.push((index, order.clone()));
        for swap in &price_response.price_route.best_route.get(0).unwrap().swaps {
            for trade in &swap.swap_exchanges {
                let src_token = over_write_eth_with_weth_token(swap.src_token);
                let dest_token = over_write_eth_with_weth_token(swap.dest_token);
                sub_trades.push((src_token, dest_token, trade.src_amount, trade.dest_amount));
            }
        }
    }
    ((matched_orders), sub_trades)
}
fn satisfies_limit_price_with_buffer(price_response: &Root, order: &OrderModel) -> bool {
    (price_response.price_route.dest_amount.ge(&order
        .buy_amount
        .checked_mul(TEN_THOUSAND.checked_add(U256::one()).unwrap())
        .unwrap()
        .checked_div(*TEN_THOUSAND)
        .unwrap())
        && order.is_sell_order)
        || (price_response.price_route.src_amount.le(&order
            .sell_amount
            .checked_mul(TEN_THOUSAND.checked_add(U256::one()).unwrap())
            .unwrap()
            .checked_div(*TEN_THOUSAND)
            .unwrap())
            && !order.is_sell_order)
}

fn get_splitted_trade_amounts_from_trading_vec(
    single_trade_results: Vec<(H160, H160, U256, U256)>,
) -> HashMap<(H160, H160), (U256, U256)> {
    let mut splitted_trade_amounts: HashMap<(H160, H160), (U256, U256)> = HashMap::new();
    for (src_token, dest_token, src_amount, dest_amount) in single_trade_results {
        splitted_trade_amounts
            .entry((src_token, dest_token))
            .and_modify(|(in_amounts, out_amounts)| {
                in_amounts.checked_add(src_amount).unwrap();
                out_amounts.checked_add(dest_amount).unwrap();
            })
            .or_insert((src_amount, dest_amount));
    }
    splitted_trade_amounts
}

fn get_trade_amounts_without_cow_volumes(
    splitted_trade_amounts: &HashMap<(H160, H160), (U256, U256)>,
) -> Result<HashMap<(H160, H160), (bool, U256, U256)>> {
    let mut updated_traded_amounts = HashMap::new();
    for (pair, entry_amouts) in splitted_trade_amounts {
        let (src_token, dest_token) = pair;
        if updated_traded_amounts.get(pair).is_some()
            || updated_traded_amounts
                .get(&(*dest_token, *src_token))
                .is_some()
        {
            continue;
        }
        if let Some(opposite_amounts) = splitted_trade_amounts.get(&(*dest_token, *src_token)) {
            if entry_amouts.1.gt(&opposite_amounts.0) {
                updated_traded_amounts.insert(
                    (*dest_token, *src_token),
                    (
                        false,
                        entry_amouts.1.checked_sub(opposite_amounts.0).unwrap(),
                        U256::zero(),
                    ),
                );
            } else if entry_amouts.0.gt(&opposite_amounts.1) {
                updated_traded_amounts.insert(
                    (*src_token, *dest_token),
                    (
                        false,
                        entry_amouts.0.checked_sub(opposite_amounts.1).unwrap(),
                        U256::zero(),
                    ),
                );
            } else {
                updated_traded_amounts.insert(
                    (*src_token, *dest_token),
                    (
                        false,
                        opposite_amounts.0.checked_sub(entry_amouts.1).unwrap(),
                        U256::zero(),
                    ),
                );
            }
        } else {
            updated_traded_amounts.insert(
                (*src_token, *dest_token),
                (
                    false,
                    splitted_trade_amounts.get(pair).unwrap().0,
                    splitted_trade_amounts.get(pair).unwrap().1,
                ),
            );
        }
    }
    Ok(updated_traded_amounts)
}

fn contain_cow(splitted_trade_amounts: &HashMap<(H160, H160), (U256, U256)>) -> bool {
    let mut pairs = HashMap::new();
    for (pair, _) in splitted_trade_amounts {
        let (src_token, dest_token) = pair;
        let reverse_pair = (*dest_token, *src_token);
        if pairs.get(&reverse_pair).is_some() {
            return true;
        }
        pairs.insert(pair, true);
    }
    false
}
fn one_token_is_already_in_settlement(
    solution: &SettledBatchAuctionModel,
    swap_info: &(SwapQuery, SwapResponse),
) -> u64 {
    let tokens: Vec<H160> = solution.prices.keys().copied().collect();
    if tokens.contains(&swap_info.0.sell_token) || tokens.contains(&swap_info.0.buy_token) {
        1u64
    } else {
        0u64
    }
}
fn over_write_eth_with_weth_token(token: H160) -> H160 {
    if token.eq(&"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".parse().unwrap()) {
        "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".parse().unwrap()
    } else {
        token
    }
}

pub fn insert_new_price(
    solution: &mut SettledBatchAuctionModel,
    splitted_trade_amounts: &HashMap<(H160, H160), (U256, U256)>,
    query: SwapQuery,
    swap: SwapResponse,
) -> Result<()> {
    let src_token = query.sell_token;
    let dest_token = query.buy_token;
    let (sell_amount, buy_amount) = match (
        splitted_trade_amounts.get(&(src_token, dest_token)),
        splitted_trade_amounts.get(&(dest_token, src_token)),
    ) {
        (Some((_sell_amount, _)), Some((buy_amount, substracted_sell_amount))) => {
            (*substracted_sell_amount, *buy_amount)
        }
        (Some((_, _)), None) => (U256::zero(), U256::zero()),
        (None, Some((_, _))) => (U256::zero(), U256::zero()),
        (None, None) => (U256::zero(), U256::zero()),
    };
    let (sell_amount, buy_amount) = (
        sell_amount.checked_add(swap.sell_amount).unwrap(),
        buy_amount.checked_add(swap.buy_amount).unwrap(),
    );

    match (
        solution.prices.clone().get(&query.sell_token),
        solution.prices.clone().get(&query.buy_token),
    ) {
        (Some(_), Some(_)) => return Err(anyhow!("can't deal with such a ring")),
        (Some(price_sell_token), None) => {
            solution.prices.insert(
                query.buy_token,
                price_sell_token
                    .checked_mul(sell_amount)
                    .unwrap()
                    .checked_div(buy_amount)
                    .unwrap(),
            );
        }
        (None, Some(price_buy_token)) => {
            solution.prices.insert(
                query.sell_token,
                price_buy_token
                    .checked_mul(buy_amount)
                    .unwrap()
                    .checked_div(sell_amount)
                    .unwrap(),
            );
        }
        (None, None) => {
            solution.prices.insert(query.sell_token, buy_amount);
            solution.prices.insert(query.buy_token, sell_amount);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::batch_auction_model::CostModel;
    use crate::models::batch_auction_model::FeeModel;
    use core::array::IntoIter;
    use std::collections::BTreeMap;
    //     #[test]
    //     fn price_insert_without_cow_volume_inserts_new_prices_with_correct_ratios() {
    //         let unrelated_token = shared::addr!("9f8f72aa9304c8b593d555f12ef6589cc3a579a2");
    //         let sell_token = shared::addr!("6b175474e89094c44da98b954eedeac495271d0f");
    //         let buy_token = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");
    //         let price_unrelated_token = U256::from_dec_str("12").unwrap();
    //         // test token price already available in sell_token
    //         let price_sell_token = U256::from_dec_str("10").unwrap();
    //         let mut settlement = Settlement::new(maplit::hashmap! {
    //             unrelated_token => price_unrelated_token,
    //             sell_token => price_sell_token,
    //         });
    //         let splitted_trade_amounts = maplit::hashmap! {
    //             (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
    //         };
    //         let query = SwapQuery {
    //             sell_token,
    //             buy_token,
    //             sell_amount: None,
    //             buy_amount: None,
    //             slippage_percentage: Slippage::number_from_basis_points(STANDARD_ZEROEX_SLIPPAGE_BPS)
    //                 .unwrap(),
    //             skip_validation: None,
    //         };
    //         let sell_amount = U256::from_dec_str("4").unwrap();
    //         let buy_amount = U256::from_dec_str("6").unwrap();
    //         let swap = SwapResponse {
    //             sell_amount,
    //             buy_amount,
    //             allowance_target: H160::zero(),
    //             price: 1f64,
    //             to: H160::zero(),
    //             data: web3::types::Bytes::from([0u8; 8]),
    //             value: U256::from_dec_str("4").unwrap(),
    //         };
    //         insert_new_price(&mut settlement, &splitted_trade_amounts, query, swap).unwrap();
    //         assert_eq!(
    //             settlement.clearing_price(sell_token),
    //             Some(price_sell_token)
    //         );
    //         assert_eq!(
    //             settlement.clearing_price(buy_token),
    //             Some(
    //                 sell_amount
    //                     .checked_mul(price_sell_token)
    //                     .unwrap()
    //                     .checked_div(buy_amount)
    //                     .unwrap()
    //             )
    //         );
    //         // test token price already available in buy_token
    //         let price_buy_token = U256::from_dec_str("10").unwrap();
    //         let mut settlement = Settlement::new(maplit::hashmap! {
    //             unrelated_token => price_unrelated_token,
    //             buy_token => price_sell_token,
    //         });
    //         let splitted_trade_amounts = maplit::hashmap! {
    //             (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
    //         };
    //         let query = SwapQuery {
    //             sell_token,
    //             buy_token,
    //             sell_amount: None,
    //             buy_amount: None,
    //             slippage_percentage: Slippage::number_from_basis_points(STANDARD_ZEROEX_SLIPPAGE_BPS)
    //                 .unwrap(),
    //             skip_validation: None,
    //         };
    //         let sell_amount = U256::from_dec_str("4").unwrap();
    //         let buy_amount = U256::from_dec_str("6").unwrap();
    //         let swap = SwapResponse {
    //             sell_amount,
    //             buy_amount,
    //             allowance_target: H160::zero(),
    //             price: 1f64,
    //             to: H160::zero(),
    //             data: web3::types::Bytes::from([0u8; 8]),
    //             value: U256::from_dec_str("4").unwrap(),
    //         };
    //         insert_new_price(&mut settlement, &splitted_trade_amounts, query, swap).unwrap();
    //         assert_eq!(settlement.clearing_price(buy_token), Some(price_buy_token));
    //         assert_eq!(
    //             settlement.clearing_price(sell_token),
    //             Some(
    //                 buy_amount
    //                     .checked_mul(price_buy_token)
    //                     .unwrap()
    //                     .checked_div(sell_amount)
    //                     .unwrap()
    //             )
    //         );
    //     }
    //     #[test]
    //     fn test_price_insert_without_cow_volume() {
    //         let sell_token = shared::addr!("6b175474e89094c44da98b954eedeac495271d0f");
    //         let buy_token = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");
    //         let mut settlement = Settlement::new(HashMap::new());
    //         let splitted_trade_amounts = maplit::hashmap! {
    //             (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
    //         };
    //         let query = SwapQuery {
    //             sell_token,
    //             buy_token,
    //             sell_amount: None,
    //             buy_amount: None,
    //             slippage_percentage: Slippage::number_from_basis_points(STANDARD_ZEROEX_SLIPPAGE_BPS)
    //                 .unwrap(),
    //             skip_validation: None,
    //         };
    //         let sell_amount = U256::from_dec_str("4").unwrap();
    //         let buy_amount = U256::from_dec_str("6").unwrap();
    //         let swap = SwapResponse {
    //             sell_amount,
    //             buy_amount,
    //             allowance_target: H160::zero(),
    //             price: 1f64,
    //             to: H160::zero(),
    //             data: web3::types::Bytes::from([0u8; 8]),
    //             value: U256::from_dec_str("4").unwrap(),
    //         };
    //         insert_new_price(&mut settlement, &splitted_trade_amounts, query, swap).unwrap();
    //         assert_eq!(settlement.encoder.tokens, vec![buy_token, sell_token]);
    //         assert_eq!(settlement.clearing_price(sell_token), Some(buy_amount));
    //         assert_eq!(settlement.clearing_price(buy_token), Some(sell_amount));
    //     }
    //     #[test]
    //     fn test_price_insert_with_cow_volume() {
    //         let sell_token = shared::addr!("6b175474e89094c44da98b954eedeac495271d0f");
    //         let buy_token = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");
    //         let mut settlement = Settlement::new(HashMap::new());
    //         // cow volume is 3 sell token
    //         // hence 2 sell tokens are in swap requested only
    //         // assuming we get 4 buy token for the 2 swap token,
    //         // we get in final a price of (3+5)/(6+4) = 5 / 10
    //         let splitted_trade_amounts = maplit::hashmap! {
    //             (sell_token, buy_token) => (U256::from_dec_str("5").unwrap(),U256::from_dec_str("8").unwrap()),
    //             (buy_token, sell_token) => (U256::from_dec_str("6").unwrap(),U256::from_dec_str("3").unwrap())
    //         };
    //         let query = SwapQuery {
    //             sell_token,
    //             buy_token,
    //             sell_amount: None,
    //             buy_amount: None,
    //             slippage_percentage: Slippage::number_from_basis_points(STANDARD_ZEROEX_SLIPPAGE_BPS)
    //                 .unwrap(),
    //             skip_validation: None,
    //         };
    //         let sell_amount = U256::from_dec_str("2").unwrap();
    //         let buy_amount = U256::from_dec_str("4").unwrap();
    //         let swap = SwapResponse {
    //             sell_amount,
    //             buy_amount,
    //             allowance_target: H160::zero(),
    //             price: 1f64,
    //             to: H160::zero(),
    //             data: web3::types::Bytes::from([0u8; 8]),
    //             value: U256::from_dec_str("4").unwrap(),
    //         };
    //         insert_new_price(&mut settlement, &splitted_trade_amounts, query, swap).unwrap();
    //         assert_eq!(settlement.encoder.tokens, vec![buy_token, sell_token]);
    //         assert_eq!(
    //             settlement.clearing_price(sell_token),
    //             Some(U256::from_dec_str("10").unwrap())
    //         );
    //         assert_eq!(
    //             settlement.clearing_price(buy_token),
    //             Some(U256::from_dec_str("5").unwrap())
    //         );
    //     }
    #[tokio::test]
    #[ignore]
    async fn solve_with_dai_gno_weth_order() {
        let dai: H160 = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let gno: H160 = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();
        let weth: H160 = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".parse().unwrap();

        let dai_gno_order = OrderModel {
            sell_token: dai,
            buy_token: gno,
            sell_amount: 200_000_000_000_000_000_000_000u128.into(),
            buy_amount: 1u128.into(),
            is_sell_order: true,
            is_liquidity_order: false,
            allow_partial_fill: false,
            cost: CostModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
            fee: FeeModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
        };

        let gno_weth_order = OrderModel {
            sell_token: gno,
            buy_token: weth,
            sell_amount: 100_000_000_000_000_000_000u128.into(),
            buy_amount: 1u128.into(),
            is_sell_order: true,
            is_liquidity_order: false,
            allow_partial_fill: false,
            cost: CostModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
            fee: FeeModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
        };
        let solution = solve(BatchAuctionModel {
            orders: BTreeMap::from_iter(IntoIter::new([(1, gno_weth_order), (2, dai_gno_order)])),
            ..Default::default()
        })
        .await
        .unwrap();

        println!("{:#?}", solution);
    }

    #[tokio::test]
    #[ignore]
    async fn solve_bal_gno_weth_cows() {
        // let http = Http::new("https://staging-openethereum.mainnet.gnosisdev.com").unwrap();
        // let web3 = Web3::new(http);
        let dai: H160 = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let bal: H160 = "ba100000625a3754423978a60c9317c58a424e3d".parse().unwrap();
        let gno: H160 = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();

        let dai_gno_order = OrderModel {
            sell_token: dai,
            buy_token: gno,
            sell_amount: 15_000_000_000_000_000_000_000u128.into(),
            buy_amount: 1u128.into(),
            is_sell_order: true,
            is_liquidity_order: false,
            allow_partial_fill: false,
            cost: CostModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
            fee: FeeModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
        };
        let bal_dai_order = OrderModel {
            sell_token: gno,
            buy_token: bal,
            sell_amount: 1_000_000_000_000_000_000_000u128.into(),
            buy_amount: 1u128.into(),
            is_sell_order: true,
            allow_partial_fill: false,
            is_liquidity_order: false,
            cost: CostModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
            fee: FeeModel {
                amount: U256::from(0),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
        };
        let solution = solve(BatchAuctionModel {
            orders: BTreeMap::from_iter(IntoIter::new([(1, bal_dai_order), (2, dai_gno_order)])),
            ..Default::default()
        })
        .await
        .unwrap();

        println!("{:#?}", solution);
    }
}

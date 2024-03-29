mod paraswap_solver;
mod solver_utils;
pub mod zeroex_solver;
use crate::models::batch_auction_model::ApprovalModel;
use crate::models::batch_auction_model::ExecutedOrderModel;
use crate::models::batch_auction_model::InteractionData;
use crate::models::batch_auction_model::OrderModel;
use crate::models::batch_auction_model::SettledBatchAuctionModel;
use crate::models::batch_auction_model::TokenAmount;
use crate::models::batch_auction_model::{BatchAuctionModel, TokenInfoModel};
use crate::slippage::RelativeSlippage;
use crate::slippage::SlippageCalculator;
use crate::slippage::SlippageContext;
use crate::solve::paraswap_solver::ParaswapSolver;
use crate::token_list::get_buffer_tradable_token_list;
use crate::token_list::BufferTradingTokenList;
use crate::token_list::Token;

use self::paraswap_solver::get_sub_trades_from_paraswap_price_response;
use crate::solve::paraswap_solver::api::Root;
use crate::solve::zeroex_solver::api::SwapQuery;
use crate::solve::zeroex_solver::api::SwapResponse;
use crate::solve::zeroex_solver::ZeroExSolver;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use primitive_types::{H160, U256};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::env;
use std::time::Duration;

ethcontract::contract!("contracts/artifacts/ERC20.json");

lazy_static! {
    pub static ref THOUSAND: U256 = U256::from_dec_str("1000").unwrap();
}

const FALLBACK_SLIPPAGE: RelativeSlippage = RelativeSlippage(0.001);

pub async fn solve(
    BatchAuctionModel {
        orders, mut tokens, ..
    }: BatchAuctionModel,
    slippage_calculator: SlippageCalculator,
) -> Result<SettledBatchAuctionModel> {
    tracing::info!(
        "Before filtering: Solving instance with the orders {:?} and the tokens: {:?}",
        orders,
        tokens
    );
    let external_prices: HashMap<_, _> = tokens
        .iter()
        .filter_map(|(address, model)| Some((*address, model.external_price?)))
        .collect();
    let slippage_context = SlippageContext {
        prices: &external_prices,
        calculator: &slippage_calculator,
    };

    let api_key = env::var("ZEROEX_API_KEY").map(Some).unwrap_or(None);
    if orders.is_empty() {
        return Ok(SettledBatchAuctionModel::default());
    }

    let mut orders: Vec<(usize, OrderModel)> = orders.into_iter().map(|(i, y)| (i, y)).collect();
    // Filter out zero fee orders, as CowDexSolver is not good at matching liquidity orders.
    // Also they increase the revert risk, as Market Maker orders - at least the ones from zeroEx - can timing out and then cause simulation errors.
    orders.retain(|(_, order)| !is_zero_fee_order(order.clone()));

    // If there are many orders in the batch, we filter the ones that don't have a promising limit price (according to external prices)
    if orders.len() > 4usize {
        orders.retain(|(_, order)| is_market_order(&tokens, order.clone()).unwrap_or(false));
    }

    // For simplicity, only solve for up to 10 orders
    orders.truncate(10);

    tracing::info!(
        "After filtering: Solving instance with the orders {:?} and the tokens: {:?}",
        orders,
        tokens
    );

    // Step1: get splitted trade amounts per token pair for each order via paraswap dex-ag
    let (matched_orders, single_trade_results) =
        get_matchable_orders_and_subtrades(orders.clone(), tokens.clone()).await;
    tracing::debug!("single_trade_results: {:?}", single_trade_results);
    let contains_cow = contain_cow(&single_trade_results);
    for sub_trade in single_trade_results.iter() {
        tracing::debug!(
            " Before cow merge: trade on pair {:?} with values {:?}",
            (sub_trade.src_token, sub_trade.dest_token),
            (sub_trade.src_amount, sub_trade.dest_amount)
        );
    }

    // 2nd step: Removing obvious cow volume from splitted traded amounts, by matching opposite volume
    let ((matched_orders, mut swap_results), splitted_trade_amounts) = match contains_cow {
        true => {
            tracing::info!("Found cow and trying to solve it");

            let splitted_trade_amounts =
                get_splitted_trade_amounts_from_trading_vec(single_trade_results);
            // if there is a cow volume, we try to remove it
            let updated_traded_amounts =
                match get_trade_amounts_without_cow_volumes(&splitted_trade_amounts) {
                    Ok(traded_amounts) => traded_amounts,
                    Err(err) => {
                        tracing::debug!(
                            "Error from zeroEx api for trade amounts without cows: {:?}",
                            err
                        );
                        return Ok(SettledBatchAuctionModel::default());
                    }
                };

            for (pair, entry_amounts) in &updated_traded_amounts {
                tracing::debug!(
                    " After cow merge: trade on pair {:?} with values {:?}",
                    pair,
                    entry_amounts,
                );
            }

            // 3rd step: Get trades from zeroEx of left-over amounts
            let swap_results = match get_swaps_for_left_over_amounts(
                updated_traded_amounts,
                &slippage_context,
                api_key,
            )
            .await
            {
                Ok(swap_results) => swap_results,
                Err(err) => {
                    tracing::debug!(
                        "Error from zeroEx api for trading left over amounts: {:?}",
                        err
                    );
                    return Ok(SettledBatchAuctionModel::default());
                }
            };
            ((matched_orders, swap_results), splitted_trade_amounts)
        }
        false => {
            tracing::info!("Falling back to normal zeroEx solver");

            let zero_ex_results =
                match get_swaps_for_orders_from_zeroex(orders, &slippage_context, api_key).await {
                    Ok(zero_ex_results) => zero_ex_results,
                    Err(err) => {
                        tracing::debug!(
                            "Error while calling zeroEx api in fallback mode: {:?}",
                            err
                        );
                        return Ok(SettledBatchAuctionModel::default());
                    }
                };
            (zero_ex_results.into_iter().unzip(), HashMap::new())
        }
    };

    // 4th step: Build settlements with price and interactions
    let mut solution = SettledBatchAuctionModel::default();
    let tradable_buffer_token_list = get_buffer_tradable_token_list();
    while let Some((query, swap)) = swap_results.pop() {
        match solution.insert_new_price(
            &splitted_trade_amounts,
            query.clone(),
            swap.clone(),
            &tokens,
        ) {
            Ok(()) => {}
            Err(err) => {
                tracing::debug!(
                    "Inserting a price failed due to {:?}, returning trivial solution",
                    err
                );
                return Ok(SettledBatchAuctionModel::default());
            }
        }
        let swap_interaction_data =
            build_payload_for_swap(&swap, &query, &mut tokens, &tradable_buffer_token_list)?;
        // We will always push all approvals, the driver will filter out the unnecessary ones
        solution.approvals.push(build_approval(&swap, &query));
        solution.interaction_data.push(swap_interaction_data);
        // Sort swap_results in such a way that the next pop contains a token already processed in the clearing prices, if there exists one.
        swap_results.sort_by(|a, b| {
            one_token_is_already_in_settlement(&solution, a)
                .cmp(&one_token_is_already_in_settlement(&solution, b))
        })
    }

    // 5th step: Update execution plan coordinates once we have all plans prepared
    for (position, plan) in solution.interaction_data.iter_mut().enumerate() {
        plan.exec_plan.coordinates.position = position as u32;
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

// Checks that swap respects buy and sell amount because 0x returned buy orders in the
// past which did not respect the queried buy_amount.
pub fn swap_respects_limit_price(swap: &SwapResponse, order: &OrderModel) -> bool {
    // note: This would be different for partially fillable orders but OrderModel does currently not
    // contain the remaining fill amount.
    swap.sell_amount <= order.sell_amount && swap.buy_amount >= order.buy_amount
}

fn build_approval(swap: &SwapResponse, query: &SwapQuery) -> ApprovalModel {
    ApprovalModel {
        token: query.sell_token,
        spender: swap.allowance_target,
        amount: swap.sell_amount,
    }
}

fn build_payload_for_swap(
    swap: &SwapResponse,
    query: &SwapQuery,
    tokens: &mut BTreeMap<H160, TokenInfoModel>,
    tradable_buffer_token_list: &BufferTradingTokenList,
) -> Result<InteractionData> {
    let available_buffer = tokens
        .clone()
        .get(&query.buy_token)
        .unwrap_or(&TokenInfoModel::default())
        .internal_buffer
        .unwrap_or_else(U256::zero);
    let mut swap_interaction_data = {
        InteractionData {
            target: swap.to,
            value: swap.value,
            call_data: swap.data.0.clone(),
            exec_plan: Default::default(),
            inputs: vec![TokenAmount {
                token: query.sell_token,
                amount: swap.sell_amount,
            }],
            outputs: vec![TokenAmount {
                token: query.buy_token,
                amount: swap.buy_amount,
            }],
        }
    };

    if swap.buy_amount < available_buffer
        && swap_tokens_are_tradable_buffer_tokens(query, tradable_buffer_token_list)
    {
        // Trade against internal buffer
        swap_interaction_data.exec_plan.internal = true;

        // Adjust buffer balances
        if let Some(mut token_info) = tokens.get_mut(&query.buy_token) {
            token_info.internal_buffer = available_buffer.checked_sub(swap.buy_amount);
        }
        if let Some(mut token_info) = tokens.get_mut(&query.sell_token) {
            if let Some(buffer) = token_info.internal_buffer {
                token_info.internal_buffer = buffer.checked_add(swap.sell_amount);
            } else {
                token_info.internal_buffer = Some(swap.sell_amount);
            }
        }
    }
    Ok(swap_interaction_data)
}

fn is_zero_fee_order(order: OrderModel) -> bool {
    order.fee.amount == U256::zero()
}

fn is_market_order(tokens: &BTreeMap<H160, TokenInfoModel>, order: OrderModel) -> Result<bool> {
    // works currently only for sell orders
    let sell_token_price = tokens
        .get(&order.sell_token)
        .ok_or_else(|| anyhow!("sell token price not available"))?
        .external_price
        .ok_or_else(|| anyhow!("sell token price not available"))?;
    let buy_token_price = tokens
        .get(&order.buy_token)
        .ok_or_else(|| anyhow!("buy token price not available"))?
        .external_price
        .ok_or_else(|| anyhow!("buy token price not available"))?;

    Ok((order.is_sell_order
        && (order.sell_amount.as_u128() as f64) * (sell_token_price)
            > (order.buy_amount.as_u128() as f64) * buy_token_price * 0.98f64)
        || (!order.is_sell_order
            && (order.buy_amount.as_u128() as f64) * (buy_token_price)
                < (order.sell_amount.as_u128() as f64) * sell_token_price * 1.02f64))
}

async fn get_swaps_for_orders_from_zeroex(
    orders: Vec<(usize, OrderModel)>,
    slippage_context: &SlippageContext<'_>,
    api_key: Option<String>,
) -> Result<Vec<((usize, OrderModel), (SwapQuery, SwapResponse))>> {
    let zeroex_futures = orders
        .into_iter()
        .filter(|(_, x)| !x.is_liquidity_order)
        .map(|(index, order)| {
            let slippage = slippage_context
                .relative_for_order(&order)
                .unwrap_or(FALLBACK_SLIPPAGE);
            let cloned_api_key = api_key.clone();
            async move {
                let client = reqwest::ClientBuilder::new()
                    .timeout(Duration::new(5, 0))
                    .user_agent("gp-v2-services/2.0.0")
                    .build()
                    .unwrap();
                let zeroex_solver =
                    ZeroExSolver::new(1u64, cloned_api_key, client.clone()).unwrap();

                let query = match order.is_sell_order {
                    true => SwapQuery {
                        sell_token: order.sell_token,
                        buy_token: order.buy_token,
                        sell_amount: Some(order.sell_amount),
                        buy_amount: None,
                        // 0x slippage is a factor, not percentage (the name is misleading: https://docs.0x.org/0x-api-swap/guides/troubleshooting-0x-api-swaps#slippage-tolerance)
                        slippage_percentage: slippage.as_factor(),
                        skip_validation: Some(true),
                    },
                    false => SwapQuery {
                        sell_token: order.sell_token,
                        buy_token: order.buy_token,
                        sell_amount: None,
                        buy_amount: Some(order.buy_amount),
                        // 0x slippage is a factor, not percentage (the name is misleading: https://docs.0x.org/0x-api-swap/guides/troubleshooting-0x-api-swaps#slippage-tolerance)
                        slippage_percentage: slippage.as_factor(),
                        skip_validation: Some(true),
                    },
                };
                (
                    index,
                    order,
                    query.clone(),
                    zeroex_solver.client.get_swap(query).await,
                )
            }
        });
    let swap_results = join_all(zeroex_futures).await;
    swap_results
        .iter()
        .map(|(index, order, query, swap)| match swap {
            Ok(swap) => {
                if !swap_respects_limit_price(swap, order) {
                    return Err(anyhow!("swap price not good enough"));
                }
                Ok(((*index, order.clone()), (query.clone(), swap.clone())))
            }
            Err(err) => Err(anyhow!("error from zeroex:{:?}", err)),
        })
        .filter(|x| {
            x.is_ok()
                || (format!("{:?}", x).contains("error from zeroex")
                    && !format!("{:?}", x).contains("TimedOut"))
        })
        .collect()
}

async fn get_swaps_for_left_over_amounts(
    updated_traded_amounts: HashMap<(H160, H160), TradeAmount>,
    slippage_context: &SlippageContext<'_>,
    api_key: Option<String>,
) -> Result<Vec<(SwapQuery, SwapResponse)>> {
    let zeroex_futures = updated_traded_amounts
        .into_iter()
        .map(|(pair, trade_amount)| {
            let cloned_api_key = api_key.clone();
            async move {
                let client = reqwest::ClientBuilder::new()
                    .timeout(Duration::new(3, 0))
                    .user_agent("gp-v2-services/2.0.0")
                    .build()
                    .unwrap();
                let zeroex_solver =
                    ZeroExSolver::new(1u64, cloned_api_key, client.clone()).unwrap();

                let (src_token, dest_token) = pair;
                let query = SwapQuery {
                    sell_token: src_token,
                    buy_token: dest_token,
                    sell_amount: Some(trade_amount.sell_amount),
                    buy_amount: None,
                    // 0x slippage is a factor, not percentage (the name is misleading: https://docs.0x.org/0x-api-swap/guides/troubleshooting-0x-api-swaps#slippage-tolerance)
                    slippage_percentage: slippage_context
                        .relative(src_token, trade_amount.sell_amount)
                        .unwrap_or(FALLBACK_SLIPPAGE)
                        .as_factor(),
                    skip_validation: Some(true),
                };
                (
                    trade_amount,
                    query.clone(),
                    zeroex_solver.client.get_swap(query).await,
                )
            }
        });
    let swap_results = join_all(zeroex_futures).await;
    swap_results
        .iter()
        .map(|(trade_amount, query, swap)| match swap {
            Ok(swap) => {
                if trade_amount.must_satisfy_limit_price
                    && swap
                        .sell_amount
                        .checked_mul(trade_amount.buy_amount)
                        .gt(&trade_amount.sell_amount.checked_mul(swap.buy_amount))
                {
                    return Err(anyhow!("swap price not good enough"));
                }
                Ok((query.clone(), swap.clone()))
            }
            Err(err) => Err(anyhow!("error from zeroex:{:?}", err)),
        })
        .filter(|x| x.is_ok() || format!("{:?}", x).contains("error from zeroex"))
        .collect()
}

async fn get_matchable_orders_and_subtrades(
    orders: Vec<(usize, OrderModel)>,
    tokens: BTreeMap<H160, TokenInfoModel>,
) -> (Vec<(usize, OrderModel)>, Vec<SubTrade>) {
    let mut paraswap_futures = Vec::new();
    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::new(5, 0))
        .user_agent("gp-v2-services/2.0.0")
        .build()
        .unwrap();

    for (i, order) in orders.iter() {
        let paraswap_solver =
            ParaswapSolver::new(vec![String::from("ParaSwapPool4")], client.clone());

        paraswap_futures.push(get_paraswap_sub_trades_from_order(
            *i,
            paraswap_solver,
            order,
            tokens.clone(),
        ));
    }
    type OrdersAndSubTrades = (Vec<(usize, OrderModel)>, Vec<SubTrade>);
    // In the following, we use the sequential evaluation, as otherwise paraswap will return errors.
    // let awaited_paraswap_futures: Result<OrdersAndSubTradesVector, anyhow::Error> =
    //     join_all(paraswap_futures).await.into_iter().collect();
    let mut awaited_paraswap_futures: Vec<Result<OrdersAndSubTrades>> = Vec::new();
    for future in paraswap_futures {
        awaited_paraswap_futures.push(future.await);
    }
    let awaited_paraswap_futures: Result<Vec<OrdersAndSubTrades>, anyhow::Error> =
        awaited_paraswap_futures.into_iter().collect();
    type MatchedOrderBracket = (Vec<Vec<(usize, OrderModel)>>, Vec<Vec<SubTrade>>);
    let (matched_orders, single_trade_results): MatchedOrderBracket;
    if let Ok(paraswap_futures_results) = awaited_paraswap_futures {
        let paraswap_future_results_unzipped = paraswap_futures_results.into_iter().unzip();
        matched_orders = paraswap_future_results_unzipped.0;
        single_trade_results = paraswap_future_results_unzipped.1;
    } else {
        return (Vec::new(), Vec::new());
    }
    let matched_orders: Vec<(usize, OrderModel)> = matched_orders.into_iter().flatten().collect();

    let single_trade_results: Vec<SubTrade> = single_trade_results.into_iter().flatten().collect();
    (matched_orders, single_trade_results)
}

fn swap_tokens_are_tradable_buffer_tokens(
    query: &SwapQuery,
    tradable_buffer_token_list: &BufferTradingTokenList,
) -> bool {
    tradable_buffer_token_list.tokens.contains(&Token {
        address: query.sell_token,
        chain_id: 1u64,
    }) && tradable_buffer_token_list.tokens.contains(&Token {
        address: query.buy_token,
        chain_id: 1u64,
    })
}

#[derive(Clone, Debug)]
pub struct SubTrade {
    pub src_token: H160,
    pub dest_token: H160,
    pub src_amount: U256,
    pub dest_amount: U256,
}
async fn get_paraswap_sub_trades_from_order(
    index: usize,
    paraswap_solver: ParaswapSolver,
    order: &OrderModel,
    tokens: BTreeMap<primitive_types::H160, TokenInfoModel>,
) -> Result<(Vec<(usize, OrderModel)>, Vec<SubTrade>)> {
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
            return Err(anyhow!("price estimation failed"));
        }
    };
    if satisfies_limit_price_with_buffer(&price_response, order) {
        Ok((
            vec![(index, order.clone())],
            get_sub_trades_from_paraswap_price_response(price_response.price_route.best_route),
        ))
    } else {
        Ok((Vec::new(), Vec::new()))
    }
}

fn satisfies_limit_price_with_buffer(price_response: &Root, order: &OrderModel) -> bool {
    (price_response.price_route.dest_amount.ge(&order
        .buy_amount
        .checked_mul(THOUSAND.checked_add(U256::one()).unwrap())
        .unwrap()
        .checked_div(*THOUSAND)
        .unwrap())
        && order.is_sell_order)
        || (price_response.price_route.src_amount.le(&order
            .sell_amount
            .checked_mul(THOUSAND.checked_sub(U256::one()).unwrap())
            .unwrap()
            .checked_div(*THOUSAND)
            .unwrap())
            && !order.is_sell_order)
}

fn get_splitted_trade_amounts_from_trading_vec(
    single_trade_results: Vec<SubTrade>,
) -> HashMap<(H160, H160), (U256, U256)> {
    let mut splitted_trade_amounts: HashMap<(H160, H160), (U256, U256)> = HashMap::new();
    for sub_trade in single_trade_results {
        splitted_trade_amounts
            .entry((sub_trade.src_token, sub_trade.dest_token))
            .and_modify(|(in_amounts, out_amounts)| {
                *in_amounts = in_amounts.checked_add(sub_trade.src_amount).unwrap();
                *out_amounts = out_amounts.checked_add(sub_trade.dest_amount).unwrap();
            })
            .or_insert((sub_trade.src_amount, sub_trade.dest_amount));
    }
    splitted_trade_amounts
}

#[derive(Debug, PartialEq)]
struct TradeAmount {
    must_satisfy_limit_price: bool,
    sell_amount: U256,
    buy_amount: U256,
}

fn get_trade_amounts_without_cow_volumes(
    splitted_trade_amounts: &HashMap<(H160, H160), (U256, U256)>,
) -> Result<HashMap<(H160, H160), TradeAmount>> {
    let mut updated_traded_amounts = HashMap::new();
    for (pair, (src_amount, dest_amount)) in splitted_trade_amounts {
        let (src_token, dest_token) = pair;
        if updated_traded_amounts.get(pair).is_some()
            || updated_traded_amounts
                .get(&(*dest_token, *src_token))
                .is_some()
        {
            continue;
        }

        if let Some((opposite_src_amount, opposite_dest_amount)) =
            splitted_trade_amounts.get(&(*dest_token, *src_token))
        {
            if dest_amount > opposite_src_amount {
                updated_traded_amounts.insert(
                    (*dest_token, *src_token),
                    TradeAmount {
                        must_satisfy_limit_price: false,
                        sell_amount: dest_amount - opposite_src_amount,
                        buy_amount: U256::zero(),
                    },
                );
            } else if src_amount > opposite_dest_amount {
                updated_traded_amounts.insert(
                    (*src_token, *dest_token),
                    TradeAmount {
                        must_satisfy_limit_price: false,
                        sell_amount: src_amount - opposite_dest_amount,
                        buy_amount: U256::zero(),
                    },
                );
            } else {
                updated_traded_amounts.insert(
                    (*dest_token, *src_token),
                    TradeAmount {
                        must_satisfy_limit_price: false,
                        sell_amount: opposite_src_amount - dest_amount,
                        buy_amount: U256::zero(),
                    },
                );
            }
        } else {
            updated_traded_amounts.insert(
                (*src_token, *dest_token),
                TradeAmount {
                    must_satisfy_limit_price: false,
                    sell_amount: *src_amount,
                    buy_amount: *dest_amount,
                },
            );
        }
    }
    Ok(updated_traded_amounts)
}

fn contain_cow(splitted_trade_amounts: &[SubTrade]) -> bool {
    let mut pairs = HashMap::new();
    for sub_trade in splitted_trade_amounts.iter() {
        let pair = (sub_trade.src_token, sub_trade.dest_token);
        let reverse_pair = (sub_trade.dest_token, sub_trade.src_token);
        if pairs.get(&reverse_pair.clone()).is_some() {
            return true;
        }
        pairs.insert(pair, true);
        // reverse is also added since two trades doing the same is also a cow
        pairs.insert(reverse_pair, true);
    }
    false
}
fn one_token_is_already_in_settlement(
    solution: &SettledBatchAuctionModel,
    swap_info: &(SwapQuery, SwapResponse),
) -> u64 {
    let tokens: Vec<H160> = solution.prices.keys().copied().collect();
    let already_in_settlement =
        tokens.contains(&swap_info.0.sell_token) || tokens.contains(&swap_info.0.buy_token);
    u64::from(already_in_settlement)
}
fn overwrite_eth_with_weth_token(token: H160) -> H160 {
    if token.eq(&"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".parse().unwrap()) {
        "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".parse().unwrap()
    } else {
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::batch_auction_model::CostModel;
    use crate::models::batch_auction_model::ExecutionPlan;
    use crate::models::batch_auction_model::FeeModel;
    use hex_literal::hex;
    use maplit::hashmap;
    use std::collections::BTreeMap;
    use tracing_test::traced_test;

    #[test]
    fn get_splitted_trade_amounts_from_trading_vec_adds_amounts() {
        let sub_trade_1 = SubTrade {
            src_token: "99d8a9c45b2eca8864373a26d1459e3dff1e17f3".parse().unwrap(),
            dest_token: "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".parse().unwrap(),
            src_amount: U256::from("1"),
            dest_amount: U256::from("1"),
        };
        let result =
            get_splitted_trade_amounts_from_trading_vec(vec![sub_trade_1.clone(), sub_trade_1]);
        let mut expected_result: HashMap<(H160, H160), (U256, U256)> = HashMap::new();
        expected_result.insert(
            (
                "99d8a9c45b2eca8864373a26d1459e3dff1e17f3".parse().unwrap(),
                "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".parse().unwrap(),
            ),
            (U256::from("2"), U256::from("2")),
        );
        assert_eq!(result, expected_result);
    }

    #[test]
    fn test_build_approval() {
        let mim: H160 = "99d8a9c45b2eca8864373a26d1459e3dff1e17f3".parse().unwrap();
        let usdc: H160 = "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".parse().unwrap();

        let query = SwapQuery {
            sell_token: mim,
            buy_token: usdc,
            sell_amount: Some(U256::MAX),
            buy_amount: None,
            slippage_percentage: 0.1,
            skip_validation: Some(true),
        };
        let swap = SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
            buy_amount: U256::from_dec_str("100").unwrap(),
            allowance_target: H160::zero(),
            price: 0f64,
            to: H160::zero(),
            data: vec![0u8].into(),
            value: U256::zero(),
        };
        let approval = build_approval(&swap, &query);
        let expected_approval = ApprovalModel {
            token: query.sell_token,
            spender: swap.allowance_target,
            amount: swap.sell_amount,
        };
        assert_eq!(approval, expected_approval);
    }

    #[test]
    fn test_build_call_data_for_swap() {
        let mim: H160 = "99d8a9c45b2eca8864373a26d1459e3dff1e17f3".parse().unwrap();
        let usdc: H160 = "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".parse().unwrap();
        let mut tokens_with_max_buffer = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                mim,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(0.00040788388716066107f64),
                    internal_buffer: Some(U256::max_value()),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(405525120.6406718f64),
                    internal_buffer: Some(U256::max_value()),
                    ..Default::default()
                },
            ),
        ]));
        let query = SwapQuery {
            sell_token: mim,
            buy_token: usdc,
            sell_amount: Some(U256::MAX),
            buy_amount: None,
            slippage_percentage: 0.1,
            skip_validation: Some(true),
        };
        let swap = SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
            buy_amount: U256::from_dec_str("100").unwrap(),
            allowance_target: H160::zero(),
            price: 0f64,
            to: H160::zero(),
            data: vec![0u8].into(),
            value: U256::zero(),
        };

        //Testing internal trades payload if buffers are available and in the allowlist
        let buffer_trading_token_list_with_usdc_and_mim = BufferTradingTokenList {
            tokens: vec![
                Token {
                    address: usdc,
                    chain_id: 1u64,
                },
                Token {
                    address: mim,
                    chain_id: 1u64,
                },
            ],
        };
        let swap_interaction_data = build_payload_for_swap(
            &swap,
            &query,
            &mut tokens_with_max_buffer,
            &buffer_trading_token_list_with_usdc_and_mim,
        )
        .unwrap();
        let expected_swap_interaction_data = InteractionData {
            target: H160::zero(),
            value: U256::zero(),
            call_data: vec![0u8],
            outputs: vec![TokenAmount {
                token: query.buy_token,
                amount: swap.buy_amount,
            }],
            inputs: vec![TokenAmount {
                token: query.sell_token,
                amount: swap.sell_amount,
            }],
            exec_plan: ExecutionPlan {
                coordinates: Default::default(),
                internal: true,
            },
        };
        assert_eq!(swap_interaction_data, expected_swap_interaction_data);

        // Testing non-internal trade with required allowance
        let empty_buffer_trading_token_list = BufferTradingTokenList { tokens: vec![] };
        let swap_interaction_data = build_payload_for_swap(
            &swap,
            &query,
            &mut tokens_with_max_buffer,
            &empty_buffer_trading_token_list,
        )
        .unwrap();
        let expected_swap_interaction_data = InteractionData {
            target: H160::zero(),
            value: U256::zero(),
            call_data: vec![0u8],
            exec_plan: Default::default(),
            outputs: vec![TokenAmount {
                token: query.buy_token,
                amount: swap.buy_amount,
            }],
            inputs: vec![TokenAmount {
                token: query.sell_token,
                amount: swap.sell_amount,
            }],
        };
        assert_eq!(swap_interaction_data, expected_swap_interaction_data);

        // Testing that a external trade is received, if the buffer_token list is empty (without required allowance)
        let swap_interaction_data = build_payload_for_swap(
            &swap,
            &query,
            &mut tokens_with_max_buffer,
            &empty_buffer_trading_token_list,
        )
        .unwrap();
        let expected_swap_interaction_data = InteractionData {
            target: H160::zero(),
            value: U256::zero(),
            call_data: vec![0u8],
            exec_plan: Default::default(),
            outputs: vec![TokenAmount {
                token: query.buy_token,
                amount: swap.buy_amount,
            }],
            inputs: vec![TokenAmount {
                token: query.sell_token,
                amount: swap.sell_amount,
            }],
        };
        assert_eq!(swap_interaction_data, expected_swap_interaction_data);

        // Testing that external trade is used, if not sufficient buffer balance is available
        let mut tokens_without_buffer = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                mim,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(0.00040788388716066107f64),
                    internal_buffer: Some(U256::zero()),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(405525120.6406718f64),
                    internal_buffer: Some(U256::zero()),
                    ..Default::default()
                },
            ),
        ]));
        let swap_interaction_data = build_payload_for_swap(
            &swap,
            &query,
            &mut tokens_without_buffer,
            &buffer_trading_token_list_with_usdc_and_mim,
        )
        .unwrap();
        let expected_swap_interaction_data = InteractionData {
            target: H160::zero(),
            value: U256::zero(),
            call_data: vec![0u8],
            exec_plan: Default::default(),
            outputs: vec![TokenAmount {
                token: query.buy_token,
                amount: swap.buy_amount,
            }],
            inputs: vec![TokenAmount {
                token: query.sell_token,
                amount: swap.sell_amount,
            }],
        };
        assert_eq!(swap_interaction_data, expected_swap_interaction_data);
    }

    #[test]
    fn check_for_market_order_with_different_decimal() {
        let mim: H160 = "99d8a9c45b2eca8864373a26d1459e3dff1e17f3".parse().unwrap();
        let usdc: H160 = "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".parse().unwrap();
        let mim_usdc_sell_order = OrderModel {
            sell_token: mim,
            buy_token: usdc,
            sell_amount: 85670806275371642755219456u128.into(),
            buy_amount: 85593297939394u128.into(),
            is_sell_order: true,
            is_liquidity_order: false,
            allow_partial_fill: false,
            cost: CostModel {
                amount: U256::from(0u32),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
            fee: FeeModel {
                amount: U256::from(0u32),
                token: "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap(),
            },
        };
        let tokens = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                mim,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(0.00040788388716066107f64),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(405525120.6406718f64),
                    ..Default::default()
                },
            ),
        ]));
        assert!(is_market_order(&tokens, mim_usdc_sell_order).unwrap());
    }

    #[test]
    fn check_for_market_order() {
        let dai: H160 = "4e3fbd56cd56c3e72c1403e103b45db9da5b9d2b".parse().unwrap();
        let usdc: H160 = "d533a949740bb3306d119cc777fa900ba034cd52".parse().unwrap();

        let dai_usdc_sell_order = OrderModel {
            sell_token: dai,
            buy_token: usdc,
            sell_amount: 1_001_000_000_000_000_000u128.into(),
            buy_amount: 1_000_000u128.into(),
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
        let dai_usdc_buy_order = OrderModel {
            sell_token: dai,
            buy_token: usdc,
            sell_amount: 1_001_000_000_000_000_000u128.into(),
            buy_amount: 1_000_000u128.into(),
            is_sell_order: false,
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
        let tokens = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                dai,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(1.00f64),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(1000000000000.0f64),
                    ..Default::default()
                },
            ),
        ]));
        assert!(is_market_order(&tokens, dai_usdc_sell_order.clone()).unwrap());
        assert!(is_market_order(&tokens, dai_usdc_buy_order.clone()).unwrap());

        let tokens = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                dai,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(1.00f64),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(1030000000000.0f64),
                    ..Default::default()
                },
            ),
        ]));
        assert!(!is_market_order(&tokens, dai_usdc_sell_order).unwrap());
        assert!(!is_market_order(&tokens, dai_usdc_buy_order).unwrap());
        let weth: H160 = "4e3fbd56cd56c3e72c1403e103b45db9da5b9d2b".parse().unwrap();
        let usdc_weth_order = OrderModel {
            sell_token: usdc,
            buy_token: weth,
            sell_amount: 4_002_000_000u128.into(),
            buy_amount: 1_000_000_000_000_000_000u128.into(),
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
        let tokens = BTreeMap::from_iter(IntoIterator::into_iter([
            (
                weth,
                TokenInfoModel {
                    decimals: Some(18u8),
                    external_price: Some(1.00f64),
                    ..Default::default()
                },
            ),
            (
                usdc,
                TokenInfoModel {
                    decimals: Some(6u8),
                    external_price: Some(400000000.0f64),
                    ..Default::default()
                },
            ),
        ]));
        assert!(is_market_order(&tokens, usdc_weth_order).unwrap());
    }

    #[tokio::test]
    #[traced_test]
    #[ignore]
    async fn solve_three_similar_orders() {
        let dai: H160 = "4e3fbd56cd56c3e72c1403e103b45db9da5b9d2b".parse().unwrap();
        let gno: H160 = "d533a949740bb3306d119cc777fa900ba034cd52".parse().unwrap();

        let dai_gno_order = OrderModel {
            sell_token: dai,
            buy_token: gno,
            sell_amount: 199181260940948221184u128.into(),
            buy_amount: 1416179064540059329552u128.into(),
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

        let solution = solve(
            BatchAuctionModel {
                tokens: BTreeMap::from_iter(IntoIterator::into_iter([
                    (
                        dai,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        gno,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                ])),
                orders: BTreeMap::from_iter(IntoIterator::into_iter([
                    (1, dai_gno_order.clone()),
                    (2, dai_gno_order.clone()),
                    (3, dai_gno_order),
                ])),
                ..Default::default()
            },
            SlippageCalculator::default(),
        )
        .await
        .unwrap();

        println!("{:#?}", solution);
    }

    #[tokio::test]
    #[traced_test]
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
        let solution = solve(
            BatchAuctionModel {
                tokens: BTreeMap::from_iter(IntoIterator::into_iter([
                    (
                        dai,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        gno,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        weth,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                ])),
                orders: BTreeMap::from_iter(IntoIterator::into_iter([
                    (1, gno_weth_order),
                    (2, dai_gno_order),
                ])),
                ..Default::default()
            },
            SlippageCalculator::default(),
        )
        .await
        .unwrap();

        println!("{:#?}", solution);
    }

    #[tokio::test]
    #[traced_test]
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
        let solution = solve(
            BatchAuctionModel {
                tokens: BTreeMap::from_iter(IntoIterator::into_iter([
                    (
                        dai,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        gno,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        bal,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                ])),
                orders: BTreeMap::from_iter(IntoIterator::into_iter([
                    (1, bal_dai_order),
                    (2, dai_gno_order),
                ])),
                ..Default::default()
            },
            SlippageCalculator::default(),
        )
        .await
        .unwrap();

        println!("{:#?}", solution);
    }
    #[tokio::test]
    #[traced_test]
    #[ignore]
    async fn solve_two_orders_into_same_direction() {
        let free: H160 = "4cd0c43b0d53bc318cc5342b77eb6f124e47f526".parse().unwrap();
        let weth: H160 = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".parse().unwrap();

        let free_weth_order = OrderModel {
            sell_token: free,
            buy_token: weth,
            sell_amount: 1_975_836_594_684_055_780_624_887u128.into(),
            buy_amount: 1_000_000_000_000_000_000u128.into(),
            is_sell_order: false,
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
        let solution = solve(
            BatchAuctionModel {
                tokens: BTreeMap::from_iter(IntoIterator::into_iter([
                    (
                        free,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                    (
                        weth,
                        TokenInfoModel {
                            decimals: Some(18u8),
                            ..Default::default()
                        },
                    ),
                ])),
                orders: BTreeMap::from_iter(IntoIterator::into_iter([
                    (1, free_weth_order.clone()),
                    (2, free_weth_order),
                ])),
                ..Default::default()
            },
            SlippageCalculator::default(),
        )
        .await
        .unwrap();

        println!("{:#?}", solution);
    }

    #[test]
    fn combines_subtrades_joint_token_pairs() {
        let usdc = H160(hex!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"));
        let weth = H160(hex!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"));

        let splitted_trade_amounts = get_splitted_trade_amounts_from_trading_vec(vec![
            SubTrade {
                src_token: usdc,
                dest_token: weth,
                src_amount: 7000000000_u128.into(),
                dest_amount: 6887861098514148915_u128.into(),
            },
            SubTrade {
                src_token: weth,
                dest_token: usdc,
                src_amount: 3979843491332154984_u128.into(),
                dest_amount: 4057081604_u128.into(),
            },
            SubTrade {
                src_token: weth,
                dest_token: usdc,
                src_amount: 4671990185476877589_u128.into(),
                dest_amount: 4759375562_u128.into(),
            },
        ]);

        let updated_traded_amounts =
            get_trade_amounts_without_cow_volumes(&splitted_trade_amounts).unwrap();

        assert_eq!(updated_traded_amounts.len(), 1);
        // Depending on the order that the subtrades are considered (which is
        // random because of `HashMap`), it can either sell excess WETH or USDC
        if updated_traded_amounts.contains_key(&(usdc, weth)) {
            assert_eq!(
                updated_traded_amounts,
                hashmap! {
                    (usdc, weth) => TradeAmount {
                        must_satisfy_limit_price: false,
                        sell_amount: (4057081604_u128
                                    + 4759375562_u128
                                    - 7000000000_u128).into(),
                        buy_amount: U256::zero(),
                    },
                }
            )
        } else {
            assert_eq!(
                updated_traded_amounts,
                hashmap! {
                    (weth, usdc) => TradeAmount {
                        must_satisfy_limit_price: false,
                        sell_amount: (3979843491332154984_u128
                                    + 4671990185476877589_u128
                                    - 6887861098514148915_u128).into(),
                        buy_amount: U256::zero(),
                    },
                }
            )
        }
    }
}

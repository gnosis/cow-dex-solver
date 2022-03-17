pub mod api;
use anyhow::{anyhow, Result};

use self::api::BestRoute;
use super::over_write_eth_with_weth_token;
use super::SubTrade;
use crate::models::batch_auction_model::OrderModel;
use crate::models::batch_auction_model::TokenInfoModel;
use api::{DefaultParaswapApi, ParaswapApi, PriceQuery, Root, Side};
use derivative::Derivative;
use primitive_types::U256;
use reqwest::Client;
use std::collections::BTreeMap;

const REFERRER: &str = "GPv2";

/// A GPv2 solver that matches GP orders to direct ParaSwap swaps.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ParaswapSolver {
    #[derivative(Debug = "ignore")]
    client: Box<dyn ParaswapApi + Send + Sync>,
    slippage_bps: u32,
    disabled_paraswap_dexs: Vec<String>,
}

impl ParaswapSolver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(disabled_paraswap_dexs: Vec<String>, client: Client) -> Self {
        Self {
            client: Box::new(DefaultParaswapApi {
                client,
                partner: REFERRER.into(),
            }),
            slippage_bps: 10u32,
            disabled_paraswap_dexs,
        }
    }
}

impl ParaswapSolver {
    pub async fn get_full_price_info_for_order(
        &self,
        order: &OrderModel,
        tokens: BTreeMap<primitive_types::H160, TokenInfoModel>,
    ) -> Result<(Root, U256)> {
        let (amount, side) = match order.is_sell_order {
            false => (order.buy_amount, Side::Buy),
            true => (order.sell_amount, Side::Sell),
        };
        let price_query = PriceQuery {
            src_token: order.sell_token,
            dest_token: order.buy_token,
            src_decimals: tokens
                .get(&order.sell_token)
                .ok_or_else(|| anyhow!("Instance error: tokenlist did not contain all tokens"))?
                .decimals
                .unwrap_or(18u8) as usize,
            dest_decimals: tokens
                .get(&order.buy_token)
                .ok_or_else(|| anyhow!("Instance error: tokenlist did not contain all tokens"))?
                .decimals
                .unwrap_or(18u8) as usize,
            amount,
            side,
            exclude_dexs: Some(self.disabled_paraswap_dexs.clone()),
        };
        let price_response = self.client.get_full_price_info(price_query).await?;
        Ok((price_response, amount))
    }
}

pub fn get_sub_trades_from_paraswap_price_response(best_routes: Vec<BestRoute>) -> Vec<SubTrade> {
    let mut sub_trades = Vec::new();
    for routes in best_routes {
        for swap in routes.swaps {
            for trade in &swap.swap_exchanges {
                let src_token = over_write_eth_with_weth_token(swap.src_token);
                let dest_token = over_write_eth_with_weth_token(swap.dest_token);
                sub_trades.push(SubTrade {
                    src_token,
                    dest_token,
                    src_amount: trade.src_amount,
                    dest_amount: trade.dest_amount,
                });
            }
        }
    }
    sub_trades
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_paraswap_sub_trades_from_order_considers_all_swaps_sent() {
        // The following two routes are a the paraswap answer where not all 100% of tokens
        // are traded through one route. In fact only 77% are traded through the route 1 and
        // another 23% are traded through the second route. We check that still all swaps are being considered
        let paraswap_answer_first_route: BestRoute = serde_json::from_str( r#"
                    {
                        "percent": 77.42,
                        "swaps": [
                            {
                                "srcToken": "0xabe580e7ee158da464b51ee1a83ac0289622e6be",
                                "srcDecimals": 18,
                                "destToken": "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE",
                                "destDecimals": 18,
                                "swapExchanges": [
                                    {
                                        "exchange": "UniswapV3",
                                        "srcAmount": "1552621324719407850295",
                                        "destAmount": "3887106617214931870",
                                        "percent": 78.79,
                                        "poolAddresses": [
                                            "0xeFC73F21bb4645Ea4Cb1f1B5A674985C590C4070"
                                        ],
                                        "data": {
                                            "fee": 3000,
                                            "gasUSD": "21.506796"
                                        }
                                    },
                                    {
                                        "exchange": "SushiSwap",
                                        "srcAmount": "417960379455497404553",
                                        "destAmount": "1043231499791370837",
                                        "percent": 21.21,
                                        "poolAddresses": [
                                            "0xF39fF863730268C9bb867b3a69d031d1C1614b31"
                                        ],
                                        "data": {
                                            "router": "0xF9234CB08edb93c0d4a4d4c70cC3FfD070e78e07",
                                            "path": [
                                                "0xabe580e7ee158da464b51ee1a83ac0289622e6be",
                                                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                                            ],
                                            "factory": "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
                                            "initCode": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
                                            "feeFactor": 10000,
                                            "pools": [
                                                {
                                                    "address": "0xF39fF863730268C9bb867b3a69d031d1C1614b31",
                                                    "fee": 30,
                                                    "direction": true
                                                }
                                            ],
                                            "gasUSD": "9.678058"
                                        }
                                    }
                                ]
                            },
                            {
                                "srcToken": "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE",
                                "srcDecimals": 18,
                                "destToken": "0x5a98fcbea516cf06857215779fd812ca3bef1b32",
                                "destDecimals": 18,
                                "swapExchanges": [
                                    {
                                        "exchange": "SushiSwap",
                                        "srcAmount": "4930338117006302707",
                                        "destAmount": "4765639130330925684247",
                                        "percent": 100,
                                        "poolAddresses": [
                                            "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a"
                                        ],
                                        "data": {
                                            "router": "0xF9234CB08edb93c0d4a4d4c70cC3FfD070e78e07",
                                            "path": [
                                                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
                                                "0x5a98fcbea516cf06857215779fd812ca3bef1b32"
                                            ],
                                            "factory": "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
                                            "initCode": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
                                            "feeFactor": 10000,
                                            "pools": [
                                                {
                                                    "address": "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a",
                                                    "fee": 30,
                                                    "direction": false
                                                }
                                            ],
                                            "gasUSD": "9.678058"
                                        }
                                    }
                                ]
                            }
                            ]
                        }"#
                    ).unwrap();
        let paraswap_answer_second_route: BestRoute = serde_json::from_str( r#"
            {
                "percent": 22.58,
                "swaps": [
                    {
                        "srcToken": "0xabe580e7ee158da464b51ee1a83ac0289622e6be",
                        "srcDecimals": 18,
                        "destToken": "0x5a98fcbea516cf06857215779fd812ca3bef1b32",
                        "destDecimals": 18,
                        "swapExchanges": [
                            {
                                "exchange": "SushiSwap",
                                "srcAmount": "574731786105261697939",
                                "destAmount": "1385937292004345249183",
                                "percent": 100,
                                "poolAddresses": [
                                    "0xF39fF863730268C9bb867b3a69d031d1C1614b31",
                                    "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a"
                                ],
                                "data": {
                                    "router": "0xF9234CB08edb93c0d4a4d4c70cC3FfD070e78e07",
                                    "path": [
                                        "0xabe580e7ee158da464b51ee1a83ac0289622e6be",
                                        "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
                                        "0x5a98fcbea516cf06857215779fd812ca3bef1b32"
                                    ],
                                    "factory": "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
                                    "initCode": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
                                    "feeFactor": 10000,
                                    "pools": [
                                        {
                                            "address": "0xF39fF863730268C9bb867b3a69d031d1C1614b31",
                                            "fee": 30,
                                            "direction": true
                                        },
                                        {
                                            "address": "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a",
                                            "fee": 30,
                                            "direction": false
                                        }
                                    ],
                                    "gasUSD": "19.356116"
                                }
                            }
                        ]}]
            } "#
        ).unwrap();
        let sub_trade_vec = get_sub_trades_from_paraswap_price_response(vec![
            paraswap_answer_first_route,
            paraswap_answer_second_route,
        ]);
        assert_eq!(sub_trade_vec.len(), 4);
    }
}

// fn satisfies_limit_price(order: &OrderModel, response: &PriceResponse) -> bool {
//     // We check if order.sell / order.buy >= response.sell / response.buy
//     order.sell_amount.to_big_rational() * response.dest_amount.to_big_rational()
//         >= response.src_amount.to_big_rational() * order.buy_amount.to_big_rational()
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::{
//         interactions::allowances::{Approval, MockAllowanceManaging},
//         test::account,
//     };
//     use contracts::WETH9;
//     use ethcontract::U256;
//     use mockall::{predicate::*, Sequence};
//     use model::order::{Order, OrderCreation, OrderKind};
//     use reqwest::Client;
//     use shared::{
//         dummy_contract,
//         paraswap_api::MockParaswapApi,
//         token_info::{MockTokenInfoFetching, TokenInfo, TokenInfoFetcher},
//         transport::create_env_test_transport,
//     };
//     use std::collections::HashMap;

//     #[test]
//     fn test_satisfies_limit_price() {
//         assert!(!satisfies_limit_price(
//             &LimitOrder {
//                 sell_amount: 100.into(),
//                 buy_amount: 95.into(),
//                 ..Default::default()
//             },
//             &PriceResponse {
//                 src_amount: 100.into(),
//                 dest_amount: 90.into(),
//                 ..Default::default()
//             }
//         ));

//         assert!(satisfies_limit_price(
//             &LimitOrder {
//                 sell_amount: 100.into(),
//                 buy_amount: 95.into(),
//                 ..Default::default()
//             },
//             &PriceResponse {
//                 src_amount: 100.into(),
//                 dest_amount: 100.into(),
//                 ..Default::default()
//             }
//         ));

//         assert!(satisfies_limit_price(
//             &LimitOrder {
//                 sell_amount: 100.into(),
//                 buy_amount: 95.into(),
//                 ..Default::default()
//             },
//             &PriceResponse {
//                 src_amount: 100.into(),
//                 dest_amount: 95.into(),
//                 ..Default::default()
//             }
//         ));
//     }

//     #[tokio::test]
//     async fn test_skips_order_if_unable_to_fetch_decimals() {
//         let client = Box::new(MockParaswapApi::new());
//         let allowance_fetcher = Box::new(MockAllowanceManaging::new());
//         let mut token_info = MockTokenInfoFetching::new();

//         token_info
//             .expect_get_token_infos()
//             .return_const(HashMap::new());

//         let solver = ParaswapSolver {
//             account: account(),
//             client,
//             token_info: Arc::new(token_info),
//             allowance_fetcher,
//             settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
//             slippage_bps: 10,
//             disabled_paraswap_dexs: vec![],
//         };

//         let order = LimitOrder::default();
//         let result = solver.try_settle_order(order).await;

//         // This implicitly checks that we don't call the API is its mock doesn't have any expectations and would panic
//         assert!(result.is_err());
//     }

//     #[tokio::test]
//     async fn test_respects_limit_price() {
//         let mut client = Box::new(MockParaswapApi::new());
//         let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
//         let mut token_info = MockTokenInfoFetching::new();

//         let sell_token = H160::from_low_u64_be(1);
//         let buy_token = H160::from_low_u64_be(2);

//         client.expect_price().returning(|_| {
//             Ok(PriceResponse {
//                 price_route_raw: Default::default(),
//                 src_amount: 100.into(),
//                 dest_amount: 99.into(),
//                 token_transfer_proxy: H160([0x42; 20]),
//                 gas_cost: 0.into(),
//             })
//         });
//         client
//             .expect_transaction()
//             .returning(|_| Ok(Default::default()));

//         allowance_fetcher
//             .expect_get_approval()
//             .returning(|_, _, _| Ok(Approval::AllowanceSufficient));

//         token_info.expect_get_token_infos().returning(move |_| {
//             hashmap! {
//                 sell_token => TokenInfo { decimals: Some(18)},
//                 buy_token => TokenInfo { decimals: Some(18)},
//             }
//         });

//         let solver = ParaswapSolver {
//             account: account(),
//             client,
//             token_info: Arc::new(token_info),
//             allowance_fetcher,
//             settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
//             slippage_bps: 10,
//             disabled_paraswap_dexs: vec![],
//         };

//         let order_passing_limit = LimitOrder {
//             sell_token,
//             buy_token,
//             sell_amount: 100.into(),
//             buy_amount: 90.into(),
//             kind: model::order::OrderKind::Sell,
//             ..Default::default()
//         };
//         let order_violating_limit = LimitOrder {
//             sell_token,
//             buy_token,
//             sell_amount: 100.into(),
//             buy_amount: 110.into(),
//             kind: model::order::OrderKind::Sell,
//             ..Default::default()
//         };

//         let result = solver
//             .try_settle_order(order_passing_limit)
//             .await
//             .unwrap()
//             .unwrap();
//         assert_eq!(
//             result.clearing_prices(),
//             &hashmap! {
//                 sell_token => 99.into(),
//                 buy_token => 100.into(),
//             }
//         );

//         let result = solver
//             .try_settle_order(order_violating_limit)
//             .await
//             .unwrap();
//         assert!(result.is_none());
//     }

//     #[tokio::test]
//     async fn test_sets_allowance_if_necessary() {
//         let mut client = Box::new(MockParaswapApi::new());
//         let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
//         let mut token_info = MockTokenInfoFetching::new();

//         let sell_token = H160::from_low_u64_be(1);
//         let buy_token = H160::from_low_u64_be(2);
//         let token_transfer_proxy = H160([0x42; 20]);

//         client.expect_price().returning(move |_| {
//             Ok(PriceResponse {
//                 price_route_raw: Default::default(),
//                 src_amount: 100.into(),
//                 dest_amount: 99.into(),
//                 token_transfer_proxy,
//                 gas_cost: 0.into(),
//             })
//         });
//         client
//             .expect_transaction()
//             .returning(|_| Ok(Default::default()));

//         // On first invocation no prior allowance, then max allowance set.
//         let mut seq = Sequence::new();
//         allowance_fetcher
//             .expect_get_approval()
//             .times(1)
//             .with(
//                 eq(sell_token),
//                 eq(token_transfer_proxy),
//                 eq(U256::from(100)),
//             )
//             .returning(move |_, _, _| {
//                 Ok(Approval::Approve {
//                     token: sell_token,
//                     spender: token_transfer_proxy,
//                 })
//             })
//             .in_sequence(&mut seq);
//         allowance_fetcher
//             .expect_get_approval()
//             .times(1)
//             .with(
//                 eq(sell_token),
//                 eq(token_transfer_proxy),
//                 eq(U256::from(100)),
//             )
//             .returning(|_, _, _| Ok(Approval::AllowanceSufficient))
//             .in_sequence(&mut seq);

//         token_info.expect_get_token_infos().returning(move |_| {
//             hashmap! {
//                 sell_token => TokenInfo { decimals: Some(18)},
//                 buy_token => TokenInfo { decimals: Some(18)},
//             }
//         });

//         let solver = ParaswapSolver {
//             account: account(),
//             client,
//             token_info: Arc::new(token_info),
//             allowance_fetcher,
//             settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
//             slippage_bps: 10,
//             disabled_paraswap_dexs: vec![],
//         };

//         let order = LimitOrder {
//             sell_token,
//             buy_token,
//             sell_amount: 100.into(),
//             buy_amount: 90.into(),
//             ..Default::default()
//         };

//         // On first run we have two main interactions (approve + swap)
//         let result = solver
//             .try_settle_order(order.clone())
//             .await
//             .unwrap()
//             .unwrap();
//         assert_eq!(result.encoder.finish().interactions[1].len(), 2);

//         // On second run we have only have one main interactions (swap)
//         let result = solver.try_settle_order(order).await.unwrap().unwrap();
//         assert_eq!(result.encoder.finish().interactions[1].len(), 1)
//     }

//     #[tokio::test]
//     async fn test_sets_slippage() {
//         let mut client = Box::new(MockParaswapApi::new());
//         let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
//         let mut token_info = MockTokenInfoFetching::new();

//         let sell_token = H160::from_low_u64_be(1);
//         let buy_token = H160::from_low_u64_be(2);

//         client.expect_price().returning(|_| {
//             Ok(PriceResponse {
//                 price_route_raw: Default::default(),
//                 src_amount: 100.into(),
//                 dest_amount: 99.into(),
//                 token_transfer_proxy: H160([0x42; 20]),
//                 gas_cost: 0.into(),
//             })
//         });

//         // Check slippage is applied to PriceResponse
//         let mut seq = Sequence::new();
//         client
//             .expect_transaction()
//             .times(1)
//             .returning(|transaction| {
//                 assert_eq!(
//                     transaction.trade_amount,
//                     TradeAmount::Sell {
//                         src_amount: 100.into(),
//                     }
//                 );
//                 assert_eq!(transaction.slippage, 1000);
//                 Ok(Default::default())
//             })
//             .in_sequence(&mut seq);
//         client
//             .expect_transaction()
//             .times(1)
//             .returning(|transaction| {
//                 assert_eq!(
//                     transaction.trade_amount,
//                     TradeAmount::Buy {
//                         dest_amount: 99.into(),
//                     }
//                 );
//                 assert_eq!(transaction.slippage, 1000);
//                 Ok(Default::default())
//             })
//             .in_sequence(&mut seq);

//         allowance_fetcher
//             .expect_get_approval()
//             .returning(|_, _, _| Ok(Approval::AllowanceSufficient));

//         token_info.expect_get_token_infos().returning(move |_| {
//             hashmap! {
//                 sell_token => TokenInfo { decimals: Some(18)},
//                 buy_token => TokenInfo { decimals: Some(18)},
//             }
//         });

//         let solver = ParaswapSolver {
//             account: account(),
//             client,
//             token_info: Arc::new(token_info),
//             allowance_fetcher,
//             settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
//             slippage_bps: 1000, // 10%
//             disabled_paraswap_dexs: vec![],
//         };

//         let sell_order = LimitOrder {
//             sell_token,
//             buy_token,
//             sell_amount: 100.into(),
//             buy_amount: 90.into(),
//             kind: model::order::OrderKind::Sell,
//             ..Default::default()
//         };

//         let result = solver.try_settle_order(sell_order).await.unwrap();
//         // Actual assertion is inside the client's `expect_transaction` mock
//         assert!(result.is_some());

//         let buy_order = LimitOrder {
//             sell_token,
//             buy_token,
//             sell_amount: 100.into(),
//             buy_amount: 90.into(),
//             kind: model::order::OrderKind::Buy,
//             ..Default::default()
//         };
//         let result = solver.try_settle_order(buy_order).await.unwrap();
//         // Actual assertion is inside the client's `expect_transaction` mock
//         assert!(result.is_some());
//     }

//     #[tokio::test]
//     #[ignore]
//     async fn solve_order_on_paraswap() {
//         let web3 = Web3::new(create_env_test_transport());
//         let settlement = GPv2Settlement::deployed(&web3).await.unwrap();
//         let token_info_fetcher = Arc::new(TokenInfoFetcher { web3: web3.clone() });
//         let weth = WETH9::deployed(&web3).await.unwrap();
//         let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

//         let solver = ParaswapSolver::new(
//             account(),
//             web3,
//             settlement,
//             token_info_fetcher,
//             1,
//             vec![],
//             Client::new(),
//             None,
//         );

//         let settlement = solver
//             .try_settle_order(
//                 Order {
//                     order_creation: OrderCreation {
//                         sell_token: weth.address(),
//                         buy_token: gno,
//                         sell_amount: 1_000_000_000_000_000_000u128.into(),
//                         buy_amount: 1u128.into(),
//                         kind: OrderKind::Sell,
//                         ..Default::default()
//                     },
//                     ..Default::default()
//                 }
//                 .into(),
//             )
//             .await
//             .unwrap()
//             .unwrap();

//         println!("{:#?}", settlement);
//     }
// }

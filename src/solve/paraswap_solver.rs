mod api;

use crate::models::batch_auction_model::OrderModel;
use anyhow::Result;
use api::{DefaultParaswapApi, ParaswapApi, PriceQuery, Root, Side};
use derivative::Derivative;
use primitive_types::U256;
use reqwest::Client;

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
    pub async fn get_full_price_info_for_order(&self, order: &OrderModel) -> Result<(Root, U256)> {
        let (amount, side) = match order.is_sell_order {
            false => (order.buy_amount, Side::Buy),
            true => (order.sell_amount, Side::Sell),
        };
        let price_query = PriceQuery {
            src_token: order.sell_token,
            dest_token: order.buy_token,
            src_decimals: 18usize,  //decimals(token_info, &order.sell_token)?,
            dest_decimals: 18usize, //decimals(token_info, &order.buy_token)?,
            amount,
            side,
            exclude_dexs: Some(self.disabled_paraswap_dexs.clone()),
        };
        let price_response = self.client.get_full_price_info(price_query).await?;
        Ok((price_response, amount))
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

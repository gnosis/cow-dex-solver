use crate::models::bytes_hex;
use crate::solve::zeroex_solver::api::SwapQuery;
use crate::solve::zeroex_solver::api::SwapResponse;
use crate::utils::conversions::U256Ext;
use crate::utils::ratio_as_decimal;
use crate::utils::u256_decimal::{self, DecimalU256};
use anyhow::{anyhow, Result};
use derivative::Derivative;
use num::bigint::{BigInt, Sign};
use num::rational::Ratio;
use num::BigRational;
use primitive_types::H160;
use primitive_types::U256;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::ops::Mul;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BatchAuctionModel {
    pub tokens: BTreeMap<H160, TokenInfoModel>,
    pub orders: BTreeMap<usize, OrderModel>,
    pub metadata: Option<MetadataModel>,
    pub instance_name: Option<String>,
    pub time_limit: Option<u64>,
    pub max_nr_exec_orders: Option<u64>,
    pub auction_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderModel {
    pub sell_token: H160,
    pub buy_token: H160,
    #[serde(with = "u256_decimal")]
    pub sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub buy_amount: U256,
    pub allow_partial_fill: bool,
    pub is_sell_order: bool,
    pub fee: FeeModel,
    pub cost: CostModel,
    pub is_liquidity_order: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AmmModel {
    #[serde(flatten)]
    pub parameters: AmmParameters,
    #[serde(with = "ratio_as_decimal")]
    pub fee: BigRational,
    pub cost: CostModel,
    pub mandatory: bool,
}

impl AmmModel {
    pub fn has_sufficient_reserves(&self) -> bool {
        let non_zero_balance_count = match &self.parameters {
            AmmParameters::ConstantProduct(parameters) => parameters
                .reserves
                .values()
                .filter(|&balance| balance.gt(&U256::zero()))
                .count(),
            AmmParameters::WeightedProduct(parameters) => parameters
                .reserves
                .values()
                .filter(|&data| data.balance.gt(&U256::zero()))
                .count(),
            AmmParameters::Stable(parameters) => parameters
                .reserves
                .values()
                .filter(|&balance| balance.gt(&U256::zero()))
                .count(),
        };
        // HTTP solver requires at least two non-zero reserves.
        non_zero_balance_count >= 2
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AmmParameters {
    ConstantProduct(ConstantProductPoolParameters),
    WeightedProduct(WeightedProductPoolParameters),
    Stable(StablePoolParameters),
}

#[serde_as]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConstantProductPoolParameters {
    #[serde_as(as = "BTreeMap<_, DecimalU256>")]
    pub reserves: BTreeMap<H160, U256>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeightedPoolTokenData {
    #[serde(with = "u256_decimal")]
    pub balance: U256,
    #[serde(with = "ratio_as_decimal")]
    pub weight: BigRational,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeightedProductPoolParameters {
    pub reserves: BTreeMap<H160, WeightedPoolTokenData>,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StablePoolParameters {
    #[serde_as(as = "BTreeMap<_, DecimalU256>")]
    pub reserves: BTreeMap<H160, U256>,
    #[serde_as(as = "BTreeMap<_, DecimalU256>")]
    pub scaling_rates: BTreeMap<H160, U256>,
    #[serde(with = "ratio_as_decimal")]
    pub amplification_parameter: BigRational,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TokenInfoModel {
    pub decimals: Option<u8>,
    pub external_price: Option<f64>,
    pub normalize_priority: Option<u64>,
    #[serde_as(as = "Option<DecimalU256>")]
    pub internal_buffer: Option<U256>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CostModel {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub token: H160,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeeModel {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub token: H160,
}

#[serde_as]
#[derive(Clone, Deserialize, Derivative, Serialize, PartialEq, Eq)]
#[derivative(Debug)]
pub struct InteractionData {
    pub target: H160,
    pub value: U256,
    #[derivative(Debug(format_with = "debug_bytes"))]
    #[serde(with = "bytes_hex")]
    pub call_data: Vec<u8>,
    pub exec_plan: Option<ExecutionPlan>,
    #[serde(default)]

    /// The input amounts into the AMM interaction - i.e. the amount of tokens
    /// that are expected to be sent from the settlement contract into the AMM
    /// for this calldata.
    ///
    /// `GPv2Settlement -> AMM`
    pub inputs: Vec<TokenAmount>,
    /// The output amounts from the AMM interaction - i.e. the amount of tokens
    /// that are expected to be sent from the AMM into the settlement contract
    /// for this calldata.
    ///
    /// `AMM -> GPv2Settlement`
    #[serde(default)]
    pub outputs: Vec<TokenAmount>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAmount {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub token: H160,
}

pub fn debug_bytes(
    bytes: impl AsRef<[u8]>,
    formatter: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    formatter.write_fmt(format_args!("0x{}", hex::encode(bytes.as_ref())))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExecutionPlan {
    /// The interaction should **not** be included in the settlement as
    /// internal buffers will be used instead.
    #[serde(with = "execution_plan_internal")]
    Internal,
}

/// A module for implementing `serde` (de)serialization for the execution plan
/// enum.
///
/// This is a work-around for untagged enum serialization not supporting empty
/// variants <https://github.com/serde-rs/serde/issues/1560>.
mod execution_plan_internal {
    use super::*;

    #[derive(Deserialize, Serialize)]
    enum Kind {
        #[serde(rename = "internal")]
        Internal,
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Kind::deserialize(deserializer)?;
        Ok(())
    }
    pub fn serialize<S>(serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Kind::serialize(&Kind::Internal, serializer)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ApprovalModel {
    pub token: H160,
    pub spender: H160,
    #[serde(with = "u256_decimal")]
    pub amount: U256,
}

#[serde_as]
#[derive(Clone, Debug, Deserialize, Default, Serialize)]
pub struct SettledBatchAuctionModel {
    pub orders: HashMap<usize, ExecutedOrderModel>,
    #[serde(default)]
    pub amms: HashMap<usize, UpdatedAmmModel>,
    pub ref_token: Option<H160>,
    #[serde_as(as = "HashMap<_, DecimalU256>")]
    pub prices: HashMap<H160, U256>,
    #[serde(default)]
    pub approvals: Vec<ApprovalModel>,
    pub interaction_data: Vec<InteractionData>,
}
const SCALING_FACTOR: u64 = 10000000000000u64;

impl SettledBatchAuctionModel {
    pub fn price(&self, token: H160) -> Option<&U256> {
        self.prices.get(&token)
    }

    pub fn tokens(&self) -> HashSet<H160> {
        self.prices
            .iter()
            .map(|(token, _)| *token)
            .collect::<HashSet<H160>>()
    }

    pub fn insert_new_price(
        &mut self,
        splitted_trade_amounts: &HashMap<(H160, H160), (U256, U256)>,
        query: SwapQuery,
        swap: SwapResponse,
        tokens: &BTreeMap<H160, TokenInfoModel>,
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
            self.prices.clone().get(&src_token),
            self.prices.clone().get(&dest_token),
        ) {
            (Some(_), Some(_)) => return Err(anyhow!("can't deal with such a ring")),
            (Some(price_sell_token), None) => {
                self.prices.insert(
                    query.buy_token,
                    price_sell_token
                        .checked_mul(sell_amount)
                        .unwrap()
                        .checked_div(buy_amount)
                        .unwrap(),
                );
            }
            (None, Some(price_buy_token)) => {
                self.prices.insert(
                    query.sell_token,
                    price_buy_token
                        .checked_mul(buy_amount)
                        .unwrap()
                        .checked_div(sell_amount)
                        .unwrap(),
                );
            }
            (None, None) => {
                if self.prices.is_empty() {
                    self.prices.insert(
                        query.sell_token,
                        buy_amount.checked_mul(U256::from(SCALING_FACTOR)).unwrap(),
                    );
                    self.prices.insert(
                        query.buy_token,
                        sell_amount.checked_mul(U256::from(SCALING_FACTOR)).unwrap(),
                    );
                } else {
                    // If there are independent trades, e.g. DAI -> USDC AND ETH -> GNO, the prices between
                    // DAI and GNO still need to satisfy the price check in the driver. Hence, the prices of unrelated trades
                    // still needs to consider the external prices provided for the auction for the setting of the new price
                    for (token, token_price) in self.prices.iter() {
                        match (
                            tokens
                                .get(token)
                                .unwrap_or(&TokenInfoModel::default())
                                .external_price,
                            tokens
                                .get(&query.sell_token)
                                .unwrap_or(&TokenInfoModel::default())
                                .external_price,
                            tokens
                                .get(&query.buy_token)
                                .unwrap_or(&TokenInfoModel::default())
                                .external_price,
                        ) {
                            (Some(token_external_price), Some(sell_token_external_price), _) => {
                                if let Some(price_ratio) = Ratio::from_float(
                                    sell_token_external_price / token_external_price,
                                ) {
                                    if let Some(sell_token_price) = bigint_to_u256(
                                        &token_price
                                            .to_big_rational()
                                            .mul(&price_ratio)
                                            .to_integer(),
                                    ) {
                                        let buy_token_price = sell_token_price
                                            .checked_mul(sell_amount)
                                            .unwrap()
                                            .checked_div(buy_amount)
                                            .unwrap();
                                        self.prices.insert(query.sell_token, sell_token_price);
                                        self.prices.insert(query.buy_token, buy_token_price);
                                        break;
                                    }
                                }
                            }
                            (Some(token_external_price), _, Some(buy_token_external_price)) => {
                                if let Some(price_ratio) = Ratio::from_float(
                                    buy_token_external_price / token_external_price,
                                ) {
                                    if let Some(buy_token_price) = bigint_to_u256(
                                        &token_price
                                            .to_big_rational()
                                            .mul(&price_ratio)
                                            .to_integer(),
                                    ) {
                                        let sell_token_price = buy_token_price
                                            .checked_mul(buy_amount)
                                            .unwrap()
                                            .checked_div(sell_amount)
                                            .unwrap();
                                        self.prices.insert(query.buy_token, buy_token_price);
                                        self.prices.insert(query.sell_token, sell_token_price);
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn bigint_to_u256(input: &BigInt) -> Option<U256> {
    let (sign, bytes) = input.to_bytes_be();
    if sign == Sign::Minus || bytes.len() > 32 {
        return None;
    }
    Some(U256::from_big_endian(&bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataModel {
    pub environment: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecutedOrderModel {
    #[serde(with = "u256_decimal")]
    pub exec_sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub exec_buy_amount: U256,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdatedAmmModel {
    /// We ignore additional incoming amm fields we don't need.
    pub execution: Vec<ExecutedAmmModel>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecutedAmmModel {
    pub sell_token: H160,
    pub buy_token: H160,
    #[serde(with = "u256_decimal")]
    pub exec_sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub exec_buy_amount: U256,
    pub exec_plan: Option<ExecutionPlanCoordinatesModel>,
}

impl UpdatedAmmModel {
    /// Returns true there is at least one non-zero update.
    pub fn is_non_trivial(&self) -> bool {
        let zero = &U256::zero();
        let has_non_trivial_execution = self
            .execution
            .iter()
            .any(|exec| exec.exec_sell_amount.gt(zero) || exec.exec_buy_amount.gt(zero));
        !self.execution.is_empty() && has_non_trivial_execution
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ExecutionPlanCoordinatesModel {
    pub sequence: u32,
    pub position: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::solver_utils::Slippage;
    use maplit::{btreemap, hashset};
    use num::rational::Ratio;
    use serde_json::json;
    use std::ops::Div;

    #[test]
    fn test_serialize_and_deserialize_interaction_data() {
        let mut interaction_data = InteractionData {
            target: "ffffffffffffffffffffffffffffffffffffffff".parse().unwrap(),
            value: U256::from_dec_str("1").unwrap(),
            call_data: vec![1, 2],
            exec_plan: Some(ExecutionPlan::Internal),
            inputs: vec![TokenAmount {
                token: H160([1; 20]),
                amount: 9999.into(),
            }],
            outputs: vec![
                TokenAmount {
                    token: H160([2; 20]),
                    amount: 2000.into(),
                },
                TokenAmount {
                    token: H160([3; 20]),
                    amount: 3000.into(),
                },
            ],
        };
        let expected_string = r#"{"target":"0xffffffffffffffffffffffffffffffffffffffff","value":"0x1","call_data":"0x0102","exec_plan":"internal","inputs":[{"amount":"9999","token":"0x0101010101010101010101010101010101010101"}],"outputs":[{"amount":"2000","token":"0x0202020202020202020202020202020202020202"},{"amount":"3000","token":"0x0303030303030303030303030303030303030303"}]}"#;
        assert_eq!(
            serde_json::to_string(&interaction_data).unwrap(),
            expected_string
        );
        assert_eq!(
            serde_json::from_str::<InteractionData>(expected_string).unwrap(),
            interaction_data
        );
        interaction_data.exec_plan = None;
        let expected_string = r#"{"target":"0xffffffffffffffffffffffffffffffffffffffff","value":"0x1","call_data":"0x0102","exec_plan":null,"inputs":[{"amount":"9999","token":"0x0101010101010101010101010101010101010101"}],"outputs":[{"amount":"2000","token":"0x0202020202020202020202020202020202020202"},{"amount":"3000","token":"0x0303030303030303030303030303030303030303"}]}"#;
        assert_eq!(
            serde_json::to_string(&interaction_data).unwrap(),
            expected_string
        );
        assert_eq!(
            serde_json::from_str::<InteractionData>(expected_string).unwrap(),
            interaction_data
        );
    }
    #[test]
    fn updated_amm_model_is_non_trivial() {
        assert!(!UpdatedAmmModel { execution: vec![] }.is_non_trivial());

        let trivial_execution_without_plan = ExecutedAmmModel {
            exec_plan: None,
            ..Default::default()
        };

        let trivial_execution_with_plan = ExecutedAmmModel {
            exec_plan: Some(ExecutionPlanCoordinatesModel {
                sequence: 0,
                position: 0,
            }),
            ..Default::default()
        };

        assert!(!UpdatedAmmModel {
            execution: vec![
                trivial_execution_with_plan.clone(),
                trivial_execution_without_plan
            ],
        }
        .is_non_trivial());

        let execution_with_sell = ExecutedAmmModel {
            exec_sell_amount: U256::one(),
            ..Default::default()
        };

        let execution_with_buy = ExecutedAmmModel {
            exec_buy_amount: U256::one(),
            ..Default::default()
        };

        assert!(UpdatedAmmModel {
            execution: vec![execution_with_buy.clone()]
        }
        .is_non_trivial());

        assert!(UpdatedAmmModel {
            execution: vec![execution_with_sell]
        }
        .is_non_trivial());

        assert!(UpdatedAmmModel {
            // One trivial and one non-trivial -> non-trivial
            execution: vec![execution_with_buy, trivial_execution_with_plan]
        }
        .is_non_trivial());
    }

    #[test]
    fn model_serialization() {
        let native_token = H160([0xee; 20]);
        let buy_token = H160::from_low_u64_be(1337);
        let sell_token = H160::from_low_u64_be(43110);
        let order_model = OrderModel {
            sell_token,
            buy_token,
            sell_amount: U256::from(1),
            buy_amount: U256::from(2),
            allow_partial_fill: false,
            is_sell_order: true,
            fee: FeeModel {
                amount: U256::from(2),
                token: sell_token,
            },
            cost: CostModel {
                amount: U256::from(1),
                token: native_token,
            },
            is_liquidity_order: false,
        };
        let model = BatchAuctionModel {
            tokens: btreemap! {
                buy_token => TokenInfoModel {
                    decimals: Some(6),
                    external_price: Some(1.2),
                    normalize_priority: Some(1),
                    internal_buffer: Some(U256::from(1337)),
                },
                sell_token => TokenInfoModel {
                    decimals: Some(18),
                    external_price: Some(2345.0),
                    normalize_priority: Some(0),
                    internal_buffer: Some(U256::from(42)),
                }
            },
            orders: btreemap! { 0 => order_model },
            metadata: Some(MetadataModel {
                environment: Some(String::from("Such Meta")),
            }),
            instance_name: None,
            max_nr_exec_orders: None,
            time_limit: None,
            auction_id: None,
        };

        let result = serde_json::to_value(&model).unwrap();

        let expected = json!({
          "tokens": {
            "0x0000000000000000000000000000000000000539": {
              "decimals": 6,
              "external_price": 1.2,
              "normalize_priority": 1,
              "internal_buffer": "1337",
            },
            "0x000000000000000000000000000000000000a866": {
              "decimals": 18,
              "external_price": 2345.0,
              "normalize_priority": 0,
              "internal_buffer": "42",
            },
          },
          "orders": {
            "0": {
              "sell_token": "0x000000000000000000000000000000000000a866",
              "buy_token": "0x0000000000000000000000000000000000000539",
              "sell_amount": "1",
              "buy_amount": "2",
              "allow_partial_fill": false,
              "is_sell_order": true,
              "fee": {
                "amount": "2",
                "token": "0x000000000000000000000000000000000000a866",
              },
              "is_liquidity_order": false,
              "cost": {
                "amount": "1",
                "token": "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
              },
            },
          },
          "metadata": {
            "environment": "Such Meta",
          },
          "time_limit": null,
          "max_nr_exec_orders": null,
          "instance_name": null,
          "auction_id": null,
        });
        assert_eq!(result, expected);
    }

    #[test]
    fn decode_empty_solution() {
        let empty_solution = r#"
            {
                "tokens": {
                    "0xa7d1c04faf998f9161fc9f800a99a809b84cfc9d": {
                        "decimals": 18,
                        "alias": null,
                        "normalize_priority": 0
                    },
                    "0xc778417e063141139fce010982780140aa0cd5ab": {
                        "decimals": 18,
                        "alias": null,
                        "normalize_priority": 1
                    }
                },
                "orders": {},
                "metadata": {},
                "ref_token": "0xc778417e063141139fce010982780140aa0cd5ab",
                "prices": {
                    "0xa7d1c04faf998f9161fc9f800a99a809b84cfc9d": "1039670252129038",
                    "0xc778417e063141139fce010982780140aa0cd5ab": "1000000000000000000"
                },
                "interaction_data":[]
            }
        "#;
        assert!(serde_json::from_str::<SettledBatchAuctionModel>(empty_solution).is_ok());
    }

    #[test]
    fn decode_trivial_solution_without_ref_token() {
        let x = r#"
            {
                "tokens": {},
                "orders": {},
                "metadata": {
                    "environment": null
                },
                "ref_token": null,
                "prices": {},
                "uniswaps": {},
                "interaction_data":[]
            }
        "#;
        assert!(serde_json::from_str::<SettledBatchAuctionModel>(x).is_ok());
    }

    #[test]
    fn test_insert_new_prices_considering_external_prices() {
        let sell_token = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let external_price_sell_token = Some(1000f64);

        let buy_token = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();
        let external_price_buy_token = Some(500f64);

        let unrelated_token = "9f8f72aa9304c8b593d555f12ef6589cc3a579a2".parse().unwrap();
        let price_unrelated_token = U256::from_dec_str("12000").unwrap();
        let external_price_unrelated_token = Some(2000f64);

        // check price calculation if external price for sell token is available
        let mut settlement = SettledBatchAuctionModel {
            prices: maplit::hashmap! {
                unrelated_token => price_unrelated_token,
            },
            ..Default::default()
        };

        let sell_amount = U256::from_dec_str("4").unwrap();
        let buy_amount = U256::from_dec_str("6").unwrap();
        let splitted_trade_amounts = HashMap::new();
        let query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: None,
            buy_amount: None,
            slippage_percentage: Slippage::number_from_basis_points(3).unwrap(),
            skip_validation: None,
        };
        let swap = SwapResponse {
            sell_amount,
            buy_amount,
            allowance_target: H160::zero(),
            price: 1f64,
            to: H160::zero(),
            data: web3::types::Bytes::from([0u8; 8]),
            value: U256::from_dec_str("4").unwrap(),
        };
        let mut tokens: BTreeMap<H160, TokenInfoModel> = BTreeMap::new();
        tokens.insert(
            sell_token,
            TokenInfoModel {
                decimals: Some(18u8),
                external_price: external_price_sell_token,
                ..Default::default()
            },
        );
        tokens.insert(
            unrelated_token,
            TokenInfoModel {
                decimals: Some(18u8),
                external_price: external_price_unrelated_token,
                ..Default::default()
            },
        );
        settlement
            .insert_new_price(
                &splitted_trade_amounts,
                query.clone(),
                swap.clone(),
                &tokens,
            )
            .unwrap();
        let expected_sell_price = Ratio::from_float(
            external_price_sell_token.unwrap() / external_price_unrelated_token.unwrap(),
        )
        .unwrap()
        .mul(price_unrelated_token.to_big_rational());
        assert_eq!(
            settlement.price(sell_token).unwrap().to_big_rational(),
            expected_sell_price
        );
        assert_eq!(
            settlement.price(buy_token).unwrap().to_big_rational(),
            expected_sell_price
                .mul(sell_amount.to_big_rational())
                .div(buy_amount.to_big_rational())
        );

        // check price calculation if external price for buy token is available
        let mut settlement = SettledBatchAuctionModel {
            prices: maplit::hashmap! {
                unrelated_token => price_unrelated_token,
            },
            ..Default::default()
        };
        let splitted_trade_amounts = HashMap::new();
        let mut tokens: BTreeMap<H160, TokenInfoModel> = BTreeMap::new();
        tokens.insert(
            buy_token,
            TokenInfoModel {
                decimals: Some(18u8),
                external_price: external_price_buy_token,
                ..Default::default()
            },
        );
        tokens.insert(
            unrelated_token,
            TokenInfoModel {
                decimals: Some(18u8),
                external_price: external_price_unrelated_token,
                ..Default::default()
            },
        );

        settlement
            .insert_new_price(&splitted_trade_amounts, query, swap, &tokens)
            .unwrap();

        let expected_buy_price = Ratio::from_float(
            external_price_buy_token.unwrap() / external_price_unrelated_token.unwrap(),
        )
        .unwrap()
        .mul(price_unrelated_token.to_big_rational());
        assert_eq!(
            settlement.price(buy_token).unwrap().to_big_rational(),
            expected_buy_price
        );
        assert_eq!(
            settlement.price(sell_token).unwrap().to_big_rational(),
            expected_buy_price
                .mul(buy_amount.to_big_rational())
                .div(sell_amount.to_big_rational())
        );
    }

    #[test]
    fn inserts_new_prices_with_correct_ratios_in_case_price_for_one_token_is_existing() {
        let sell_token = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let price_sell_token = U256::from_dec_str("10").unwrap();
        let buy_token = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();
        let unrelated_token = "9f8f72aa9304c8b593d555f12ef6589cc3a579a2".parse().unwrap();
        let price_unrelated_token = U256::from_dec_str("12").unwrap();

        // test token price already available in sell_token
        let mut settlement = SettledBatchAuctionModel {
            prices: maplit::hashmap! {
                unrelated_token => price_unrelated_token,
                sell_token => price_sell_token,
            },
            ..Default::default()
        };
        let splitted_trade_amounts = maplit::hashmap! {
            (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
        };
        let query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: None,
            buy_amount: None,
            slippage_percentage: Slippage::number_from_basis_points(3).unwrap(),
            skip_validation: None,
        };
        let sell_amount = U256::from_dec_str("4").unwrap();
        let buy_amount = U256::from_dec_str("6").unwrap();
        let swap = SwapResponse {
            sell_amount,
            buy_amount,
            allowance_target: H160::zero(),
            price: 1f64,
            to: H160::zero(),
            data: web3::types::Bytes::from([0u8; 8]),
            value: U256::from_dec_str("4").unwrap(),
        };
        let tokens: BTreeMap<H160, TokenInfoModel> = BTreeMap::new();

        settlement
            .insert_new_price(&splitted_trade_amounts, query, swap, &tokens)
            .unwrap();
        assert_eq!(settlement.price(sell_token), Some(&price_sell_token));
        assert_eq!(
            settlement.price(buy_token),
            Some(
                &sell_amount
                    .checked_mul(price_sell_token)
                    .unwrap()
                    .checked_div(buy_amount)
                    .unwrap()
            )
        );

        // test token price already available in buy_token
        let price_buy_token = U256::from_dec_str("10").unwrap();
        let mut settlement = SettledBatchAuctionModel {
            prices: maplit::hashmap! {
                unrelated_token => price_unrelated_token,
                buy_token => price_sell_token,
            },
            ..Default::default()
        };
        let splitted_trade_amounts = maplit::hashmap! {
            (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
        };
        let query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: None,
            buy_amount: None,
            slippage_percentage: Slippage::number_from_basis_points(3).unwrap(),
            skip_validation: None,
        };
        let sell_amount = U256::from_dec_str("4").unwrap();
        let buy_amount = U256::from_dec_str("6").unwrap();
        let swap = SwapResponse {
            sell_amount,
            buy_amount,
            allowance_target: H160::zero(),
            price: 1f64,
            to: H160::zero(),
            data: web3::types::Bytes::from([0u8; 8]),
            value: U256::from_dec_str("4").unwrap(),
        };

        settlement
            .insert_new_price(&splitted_trade_amounts, query, swap, &tokens)
            .unwrap();
        assert_eq!(settlement.price(buy_token), Some(&price_buy_token));
        assert_eq!(
            settlement.price(sell_token),
            Some(
                &buy_amount
                    .checked_mul(price_buy_token)
                    .unwrap()
                    .checked_div(sell_amount)
                    .unwrap()
            )
        );
    }
    #[test]
    fn test_price_insert_without_cow_volume() {
        let sell_token = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let buy_token = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();
        let mut settlement = SettledBatchAuctionModel::default();
        let splitted_trade_amounts = maplit::hashmap! {
            (sell_token, buy_token) => (U256::from_dec_str("4").unwrap(),U256::from_dec_str("6").unwrap())
        };
        let query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: None,
            buy_amount: None,
            slippage_percentage: Slippage::number_from_basis_points(3).unwrap(),
            skip_validation: None,
        };
        let sell_amount = U256::from_dec_str("4").unwrap();
        let buy_amount = U256::from_dec_str("6").unwrap();
        let swap = SwapResponse {
            sell_amount,
            buy_amount,
            allowance_target: H160::zero(),
            price: 1f64,
            to: H160::zero(),
            data: web3::types::Bytes::from([0u8; 8]),
            value: U256::from_dec_str("4").unwrap(),
        };
        let tokens: BTreeMap<H160, TokenInfoModel> = BTreeMap::new();

        settlement
            .insert_new_price(&splitted_trade_amounts, query, swap, &tokens)
            .unwrap();
        assert_eq!(settlement.tokens(), hashset![buy_token, sell_token]);
        assert_eq!(
            settlement.price(sell_token),
            Some(&buy_amount.checked_mul(U256::from(SCALING_FACTOR)).unwrap())
        );
        assert_eq!(
            settlement.price(buy_token),
            Some(&sell_amount.checked_mul(U256::from(SCALING_FACTOR)).unwrap())
        );
    }

    #[test]
    fn test_price_insert_with_cow_volume() {
        let sell_token: H160 = "6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
        let buy_token = "6810e776880c02933d47db1b9fc05908e5386b96".parse().unwrap();
        let mut settlement = SettledBatchAuctionModel::default();
        // cow volume is 3 sell token
        // hence 2 sell tokens are in swap requested only
        // assuming we get 4 buy token for the 2 swap token,
        // we get in final a price of (3+5)/(6+4) = 5 / 10
        let splitted_trade_amounts = maplit::hashmap! {
            (sell_token, buy_token) => (U256::from_dec_str("5").unwrap(),U256::from_dec_str("8").unwrap()),
            (buy_token, sell_token) => (U256::from_dec_str("6").unwrap(),U256::from_dec_str("3").unwrap())
        };
        let query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: None,
            buy_amount: None,
            slippage_percentage: Slippage::number_from_basis_points(3).unwrap(),
            skip_validation: None,
        };
        let sell_amount = U256::from_dec_str("2").unwrap();
        let buy_amount = U256::from_dec_str("4").unwrap();
        let swap = SwapResponse {
            sell_amount,
            buy_amount,
            allowance_target: H160::zero(),
            price: 1f64,
            to: H160::zero(),
            data: web3::types::Bytes::from([0u8; 8]),
            value: U256::from_dec_str("4").unwrap(),
        };
        let tokens: BTreeMap<H160, TokenInfoModel> = BTreeMap::new();
        settlement
            .insert_new_price(&splitted_trade_amounts, query, swap, &tokens)
            .unwrap();
        assert_eq!(settlement.tokens(), hashset![buy_token, sell_token]);
        assert_eq!(
            settlement.price(sell_token),
            Some(
                &U256::from_dec_str("10")
                    .unwrap()
                    .checked_mul(U256::from(SCALING_FACTOR))
                    .unwrap()
            )
        );
        assert_eq!(
            settlement.price(buy_token),
            Some(
                &U256::from_dec_str("5")
                    .unwrap()
                    .checked_mul(U256::from(SCALING_FACTOR))
                    .unwrap()
            )
        );
    }
}

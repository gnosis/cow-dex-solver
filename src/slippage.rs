//! Module defining slippage computation for DEX liquidiy.

use anyhow::{Context as _, Result};
use ethcontract::{H160, U256};
use num::{BigInt, BigRational, CheckedDiv as _, Integer as _, ToPrimitive as _};
use std::{borrow::Cow, cmp, collections::HashMap};

use crate::{models::batch_auction_model::OrderModel, utils::conversions};

/// Constant maximum slippage of 10 BPS (0.1%) to use for on-chain liquidity.
pub const DEFAULT_MAX_SLIPPAGE_BPS: u32 = 10;

/// Basis points in 100%.
const BPS_BASE: u32 = 10000;

type ExternalPrices = HashMap<H160, f64>;

/// A per-auction context for computing slippage.
pub struct SlippageContext<'a> {
    pub prices: &'a ExternalPrices,
    pub calculator: &'a SlippageCalculator,
}

impl<'a> SlippageContext<'a> {
    /// Computes the relative slippage for a limit order.
    pub fn relative_for_order(&self, order: &OrderModel) -> Result<RelativeSlippage> {
        // We use the fixed token and amount for computing relative slippage.
        // This is because the variable token amount may not be representative
        // of the actual trade value. For example, a "pure" market sell order
        // would have almost 0 limit buy amount, which would cause a potentially
        // large order to not get capped on the absolute slippage value.
        let (token, amount) = match order.is_sell_order {
            true => (order.sell_token, order.sell_amount),
            false => (order.buy_token, order.buy_amount),
        };
        self.relative(token, amount)
    }

    /// Computes the relative slippage for a token and amount.
    pub fn relative(&self, token: H160, amount: U256) -> Result<RelativeSlippage> {
        Ok(self
            .calculator
            .compute(self.prices, token, amount)?
            .relative())
    }
}

/// Component used for computing negative slippage limits for internal solvers.
#[derive(Clone, Debug)]
pub struct SlippageCalculator {
    /// The maximum relative slippage factor.
    relative: BigRational,
    /// The maximum absolute slippage in native tokens.
    absolute: Option<BigInt>,
}

impl SlippageCalculator {
    pub fn from_bps(relative_bps: u32, absolute: Option<U256>) -> Self {
        Self {
            relative: BigRational::new(relative_bps.into(), BPS_BASE.into()),
            absolute: absolute.map(|value| conversions::u256_to_big_int(&value)),
        }
    }

    pub fn context<'a>(&'a self, prices: &'a ExternalPrices) -> SlippageContext<'a> {
        SlippageContext {
            prices,
            calculator: self,
        }
    }

    pub fn compute(
        &self,
        external_prices: &ExternalPrices,
        token: H160,
        amount: U256,
    ) -> Result<SlippageAmount> {
        let (relative, absolute) = self.compute_inner(
            external_prices.get(&token),
            conversions::u256_to_big_int(&amount),
        )?;
        let slippage = SlippageAmount::from_num(&relative, &absolute)?;

        if *relative < self.relative {
            tracing::debug!(
                ?token,
                %amount,
                relative = ?slippage.relative,
                absolute = %slippage.absolute,
                "reducing relative to respect maximum absolute slippage",
            );
        }

        Ok(slippage)
    }

    fn compute_inner(
        &self,
        price: Option<&f64>,
        amount: BigInt,
    ) -> Result<(Cow<BigRational>, BigInt)> {
        let relative = if let Some(max_absolute_native_token) = self.absolute.clone() {
            let price = price.context("missing token price")?;
            let max_absolute_slippage = BigRational::new(max_absolute_native_token, 1.into())
                / BigRational::from_float(*price)
                    .context("price cannot be converted into rational")?;

            let max_relative_slippage_respecting_absolute_limit = max_absolute_slippage
                .checked_div(&amount.clone().into())
                .context("slippage computation divides by 0")?;

            cmp::min(
                Cow::Owned(max_relative_slippage_respecting_absolute_limit),
                Cow::Borrowed(&self.relative),
            )
        } else {
            Cow::Borrowed(&self.relative)
        };
        let absolute = {
            let ratio = &*relative * amount;
            // Perform a ceil division so that we round up with slippage amount
            ratio.numer().div_ceil(ratio.denom())
        };

        Ok((relative, absolute))
    }
}

impl Default for SlippageCalculator {
    fn default() -> Self {
        Self::from_bps(DEFAULT_MAX_SLIPPAGE_BPS, None)
    }
}

/// A result of a slippage computation containing both relative and absolute
/// slippage amounts.
#[derive(Clone, Copy, Debug, Default)]
pub struct SlippageAmount {
    /// The relative slippage amount factor.
    relative: f64,
    /// The absolute slippage amount in the token it was computed for.
    absolute: U256,
}

impl SlippageAmount {
    /// Computes a slippage amount from arbitrary precision `num` values.
    fn from_num(relative: &BigRational, absolute: &BigInt) -> Result<Self> {
        let relative = relative
            .to_f64()
            .context("relative slippage ratio is not a number")?;
        let absolute = conversions::big_int_to_u256(absolute)?;

        Ok(Self { relative, absolute })
    }

    /// Reduce the specified amount by the constant slippage.
    pub fn sub_from_amount(&self, amount: U256) -> U256 {
        amount.saturating_sub(self.absolute)
    }

    /// Increase the specified amount by the constant slippage.
    pub fn add_to_amount(&self, amount: U256) -> U256 {
        amount.saturating_add(self.absolute)
    }

    /// Returns the relative slippage value.
    pub fn relative(&self) -> RelativeSlippage {
        RelativeSlippage(self.relative)
    }
}

/// A relative slippage value.
pub struct RelativeSlippage(pub f64);

impl RelativeSlippage {
    /// Returns the relative slippage as a factor.
    pub fn as_factor(&self) -> f64 {
        self.0
    }

    /// Returns the relative slippage as a percentage.
    pub fn as_percentage(&self) -> f64 {
        self.0 * 100.
    }

    /// Returns the relative slippage as basis points rounded down.
    pub fn as_bps(&self) -> u32 {
        (self.0 * 10000.) as _
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Address for the `WETH` token.
    pub const WETH: H160 = H160(hex_literal::hex!(
        "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
    ));

    /// Address for the `GNO` token.
    pub const GNO: H160 = H160(hex_literal::hex!(
        "6810e776880c02933d47db1b9fc05908e5386b96"
    ));

    /// Address for the `USDC` token.
    pub const USDC: H160 = H160(hex_literal::hex!(
        "A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
    ));

    #[test]
    fn limits_max_slippage() {
        let calculator = SlippageCalculator::from_bps(10, Some(U256::exp10(17)));
        let prices = maplit::hashmap! {
            WETH => 1.0,
            GNO => U256::exp10(9).to_f64_lossy(),
            USDC => 0.002,
        };

        let slippage = calculator.context(&prices);
        for (token, amount, expected_slippage) in [
            (GNO, U256::exp10(12), 1),
            (USDC, U256::exp10(23), 5),
            (GNO, U256::exp10(8), 10),
            (GNO, U256::exp10(17), 0),
        ] {
            let relative = slippage.relative(token, amount).unwrap();
            assert_eq!(relative.as_bps(), expected_slippage);
        }
    }

    #[test]
    fn errors_on_missing_token_price() {
        let calculator = SlippageCalculator::from_bps(10, Some(1_000.into()));
        assert!(calculator
            .compute(&maplit::hashmap! { WETH => 1.0, }, USDC, 1_000_000.into(),)
            .is_err());
    }
}

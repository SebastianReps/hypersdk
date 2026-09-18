//! Hyperliquid Earn (borrow/lend reserve) commands.
//!
//! Earn is the borrow/lend market: suppliers lend a quote asset (USDC) to
//! borrowers and earn the borrow interest. On the wire it is the undocumented
//! `borrowLend` action with operations `supply` and `withdraw`.
//!
//! Token index `0` is USDC.

use alloy::primitives::Address;
use clap::{Args, Subcommand};
use hypersdk::Decimal;
use hypersdk::hypercore::api::{BorrowLendAction, BorrowLendOperation};
use hypersdk::hypercore::{Chain, HttpClient, NonceHandler};
use serde::Deserialize;

use crate::SignerArgs;
use crate::utils::find_signer_sync;

/// USDC, the main Earn reserve.
const USDC_TOKEN: u32 = 0;

/// Earn supply and withdrawal commands.
#[derive(Subcommand)]
pub enum EarnCmd {
    /// Supply tokens into the Earn reserve to earn interest
    Supply(EarnTransferCmd),
    /// Withdraw supplied tokens from the Earn reserve
    Withdraw(EarnTransferCmd),
    /// Query reserve rates and a user's position
    Status(EarnStatusCmd),
}

impl EarnCmd {
    pub async fn run(self) -> anyhow::Result<()> {
        match self {
            EarnCmd::Supply(cmd) => execute_transfer(cmd, BorrowLendOperation::Supply).await,
            EarnCmd::Withdraw(cmd) => execute_transfer(cmd, BorrowLendOperation::Withdraw).await,
            EarnCmd::Status(cmd) => cmd.run().await,
        }
    }
}

async fn execute_transfer(
    cmd: EarnTransferCmd,
    operation: BorrowLendOperation,
) -> anyhow::Result<()> {
    let (verb, past) = match operation {
        BorrowLendOperation::Supply => ("Supplying", "Supplied"),
        BorrowLendOperation::Withdraw => ("Withdrawing", "Withdrawn"),
        _ => unreachable!("earn only supplies or withdraws"),
    };

    let signer = find_signer_sync(&cmd.signer)?;
    let client = HttpClient::new(cmd.signer.chain);
    let token = token_name(&client, cmd.token).await;
    let amount = cmd
        .amount
        .map(|a| a.to_string())
        .unwrap_or_else(|| "max".to_string());

    println!("{} {} {} ...", verb, amount, token);
    let nonce = NonceHandler::default().next();
    let action = BorrowLendAction {
        operation,
        token: cmd.token,
        amount: cmd.amount,
    };
    client
        .borrow_lend(&signer, action, nonce, None, None)
        .await?;
    println!("{} successfully.", past);
    Ok(())
}

/// Arguments for an Earn supply or withdrawal.
#[derive(Args, derive_more::Deref)]
pub struct EarnTransferCmd {
    #[deref]
    #[command(flatten)]
    pub signer: SignerArgs,

    /// Amount to supply or withdraw. Omit for the maximum available.
    #[arg(long)]
    pub amount: Option<Decimal>,

    /// Token index of the reserve (0 is USDC)
    #[arg(long, default_value_t = USDC_TOKEN)]
    pub token: u32,
}

/// Arguments for the Earn status query.
#[derive(Args)]
pub struct EarnStatusCmd {
    /// User address to query the position for
    #[arg(long)]
    pub user: Address,

    /// Token index of the reserve (0 is USDC)
    #[arg(long, default_value_t = USDC_TOKEN)]
    pub token: u32,

    /// Target chain for the query
    #[arg(long, default_value = "mainnet")]
    pub chain: Chain,
}

impl EarnStatusCmd {
    pub async fn run(self) -> anyhow::Result<()> {
        let client = HttpClient::new(self.chain);
        let name = token_name(&client, self.token).await;

        let reserve: ReserveState =
            serde_json::from_value(client.borrow_lend_reserve_state(self.token).await?)?;
        let user: UserState =
            serde_json::from_value(client.borrow_lend_user_state(self.user).await?)?;

        let pct = |d: &str| format_pct(d);
        println!("Reserve: {} (token {})", name, self.token);
        println!("Supply APY: {}%", pct(&reserve.supply_yearly_rate));
        println!("Borrow APY: {}%", pct(&reserve.borrow_yearly_rate));
        println!("Utilization: {}%", pct(&reserve.utilization));
        println!("Total Supplied: {} {}", reserve.total_supplied, name);
        println!("Total Borrowed: {} {}", reserve.total_borrowed, name);
        println!("Oracle Price: ${}", reserve.oracle_px);

        let state = user
            .token_to_state
            .iter()
            .find(|(token, _)| *token == self.token)
            .map(|(_, state)| state);
        let supplied = state.and_then(|s| s.supply.as_ref());
        let borrowed = state.and_then(|s| s.borrow.as_ref());

        println!();
        println!("Your Position:");
        println!(
            "  Supplied: {} {}",
            supplied.map(|s| s.value.as_str()).unwrap_or("0"),
            name
        );
        println!(
            "  Borrowed: {} {}",
            borrowed.map(|b| b.value.as_str()).unwrap_or("0"),
            name
        );
        println!("  Health: {}", user.health);
        Ok(())
    }
}

/// Reserve-level rate and liquidity state.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReserveState {
    borrow_yearly_rate: String,
    supply_yearly_rate: String,
    total_supplied: String,
    total_borrowed: String,
    utilization: String,
    oracle_px: String,
}

/// A user's aggregate borrow/lend state.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserState {
    token_to_state: Vec<(u32, TokenState)>,
    health: String,
}

#[derive(Deserialize)]
struct TokenState {
    #[serde(default)]
    supply: Option<Side>,
    #[serde(default)]
    borrow: Option<Side>,
}

#[derive(Deserialize)]
struct Side {
    value: String,
}

/// Resolve a reserve token index to its spot token symbol, falling back to the index.
async fn token_name(client: &HttpClient, token: u32) -> String {
    client
        .spot_tokens()
        .await
        .ok()
        .and_then(|tokens| {
            tokens
                .into_iter()
                .find(|t| t.index == token)
                .map(|t| t.name)
        })
        .unwrap_or_else(|| token.to_string())
}

/// Render a fractional decimal string as a percentage with two places.
fn format_pct(value: &str) -> String {
    value
        .parse::<Decimal>()
        .map(|d| (d * Decimal::ONE_HUNDRED).round_dp(2))
        .map(|d| d.to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod pct_tests {
    use super::format_pct;

    #[test]
    fn renders_fraction_as_percent() {
        assert_eq!(format_pct("0.0290714004"), "2.91");
        assert_eq!(format_pct("0.6460311198"), "64.60");
        assert_eq!(format_pct("garbage"), "garbage");
    }
}

#[cfg(test)]
mod tests {
    use hypersdk::hypercore::api::{Action, BorrowLendAction, BorrowLendOperation};
    use rust_decimal::dec;

    #[test]
    fn supply_serializes_as_borrow_lend() {
        let action = Action::BorrowLend(BorrowLendAction {
            operation: BorrowLendOperation::Supply,
            token: 0,
            amount: Some(dec!(25)),
        });
        assert_eq!(
            serde_json::to_value(&action).unwrap(),
            serde_json::json!({
                "type": "borrowLend", "operation": "supply", "token": 0, "amount": "25"
            })
        );
    }

    #[test]
    fn withdraw_without_amount_sends_null() {
        let action = Action::BorrowLend(BorrowLendAction {
            operation: BorrowLendOperation::Withdraw,
            token: 0,
            amount: None,
        });
        assert_eq!(
            serde_json::to_value(&action).unwrap(),
            serde_json::json!({
                "type": "borrowLend", "operation": "withdraw", "token": 0, "amount": null
            })
        );
    }
}

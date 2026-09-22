//! Asset transfer commands.
//!
//! This module provides commands for sending assets between accounts,
//! DEXes, and subaccounts on Hyperliquid.

use alloy::primitives::Address;
use clap::Args;
use hypersdk::{
    Decimal,
    hypercore::{
        AssetTarget, HttpClient, NonceHandler, SendAsset, SendToken, SpotToken, api::Action,
    },
};

use crate::SignerArgs;
use crate::multisig::execute_multisig_action;
use crate::utils::{find_signer_sync, find_signers, resolve_token};

/// Send assets between accounts or DEXes.
///
/// This command allows transferring tokens between:
/// - Different users (perp to perp, spot to spot)
/// - Different balances (perp to spot, spot to perp)
/// - Different DEXes (HIP-3)
/// - Subaccounts
///
/// # Examples
///
/// Send USDC from perp to spot balance (same user):
/// ```bash
/// hypecli send --private-key <KEY> --token USDC --amount 100 --from perp --to spot
/// ```
///
/// Send USDC to another address:
/// ```bash
/// hypecli send --private-key <KEY> --token USDC --amount 100 --destination 0x1234...
/// ```
///
/// Send HYPE from spot to another user's spot:
/// ```bash
/// hypecli send --private-key <KEY> --token HYPE --amount 50 --from spot --to spot --destination 0x1234...
/// ```
///
/// Transfer between HIP-3 DEXes:
/// ```bash
/// hypecli send --private-key <KEY> --token USDC --amount 100 --from perp --to xyz
/// ```
#[derive(Args, derive_more::Deref)]
pub struct SendCmd {
    #[deref]
    #[command(flatten)]
    pub signer: SignerArgs,

    /// Token to send (symbol or index, e.g., "USDC", "USDT0", "HYPE", or 0)
    #[arg(long)]
    pub token: String,

    /// Amount to send
    #[arg(long)]
    pub amount: Decimal,

    /// Destination address (defaults to self for internal transfers)
    #[arg(long)]
    pub destination: Option<Address>,

    /// Source location: "perp", "spot", or a HIP-3 DEX name
    #[arg(long, default_value = "perp")]
    pub from: AssetTarget,

    /// Destination location: "perp", "spot", or a HIP-3 DEX name
    #[arg(long, default_value = "perp")]
    pub to: AssetTarget,

    /// Source subaccount name (if sending from a subaccount)
    #[arg(long)]
    pub from_subaccount: Option<String>,

    /// Send on behalf of this multi-sig wallet
    #[arg(long)]
    pub multi_sig_addr: Option<Address>,

    /// Sign and submit using only local signers, without starting P2P gossip
    #[arg(long, requires = "multi_sig_addr")]
    pub local: bool,
}

impl SendCmd {
    pub async fn run(self) -> anyhow::Result<()> {
        let client = HttpClient::new(self.chain);

        let tokens = client.spot_tokens().await?;
        let token = resolve_token(&tokens, &self.token)?;

        if let Some(multi_sig_addr) = self.multi_sig_addr {
            let config = client.multi_sig_config(multi_sig_addr).await?;
            let signers = find_signers(&self.signer, &config.authorized_users).await?;
            let nonce = NonceHandler::default().next();
            let send = self.build_transfer(multi_sig_addr, token.clone(), nonce);
            return execute_multisig_action(
                multi_sig_addr,
                client,
                signers,
                Action::from(send.into_action(self.chain)),
                nonce,
                &config,
                self.local,
                &self.signer.trezor,
            )
            .await;
        }

        let signer = find_signer_sync(&self.signer)?;
        let nonce = NonceHandler::default().next();
        let send = self.build_transfer(signer.address(), token.clone(), nonce);

        println!(
            "Sending {} {} from {} to {}",
            self.amount, token.name, self.from, self.to
        );
        println!("  From: {}", signer.address());
        println!("  To:   {}", send.destination);
        if let Some(ref sub) = self.from_subaccount {
            println!("  Subaccount: {}", sub);
        }

        client.send_asset(&signer, send, nonce).await?;

        println!("Success!");

        Ok(())
    }

    fn build_transfer(&self, account: Address, token: SpotToken, nonce: u64) -> SendAsset {
        SendAsset {
            destination: self.destination.unwrap_or(account),
            source_dex: self.from.clone(),
            destination_dex: self.to.clone(),
            token: SendToken(token),
            amount: self.amount,
            from_sub_account: self.from_subaccount.clone().unwrap_or_default(),
            nonce,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Command, multisig::MultiSigCmd};

    const MULTISIG: &str = "0x1111111111111111111111111111111111111111";
    const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";

    fn parse_send(args: &[&str]) -> SendCmd {
        match Cli::try_parse_from(args).unwrap().command.unwrap() {
            Command::Send(cmd) => cmd,
            Command::Multisig(MultiSigCmd::SendAsset(cmd)) => cmd.into(),
            _ => panic!("expected send command"),
        }
    }

    fn token() -> SpotToken {
        SpotToken {
            name: "USDT0".into(),
            index: 268,
            token_id: Default::default(),
            evm_contract: None,
            cross_chain_address: None,
            sz_decimals: 2,
            wei_decimals: 8,
            evm_extra_decimals: 0,
        }
    }

    #[test]
    fn internal_multisig_transfer_defaults_to_multisig_account() {
        let cmd = parse_send(&[
            "hypecli",
            "send",
            "--token",
            "USDT0",
            "--amount",
            "100",
            "--multi-sig-addr",
            MULTISIG,
            "--from",
            "perp",
            "--to",
            "spot",
            "--local",
        ]);
        assert!(cmd.local);
        let account = cmd.multi_sig_addr.unwrap();
        let transfer = cmd.build_transfer(account, token(), 123);
        assert_eq!(transfer.destination, account);
        assert!(matches!(transfer.source_dex, AssetTarget::Perp));
        assert!(matches!(transfer.destination_dex, AssetTarget::Spot));
        assert_eq!(transfer.amount, "100".parse::<Decimal>().unwrap());
        assert_eq!(transfer.nonce, 123);
    }

    #[test]
    fn legacy_and_unified_multisig_send_produce_identical_actions() {
        let unified = parse_send(&[
            "hypecli",
            "send",
            "--token",
            "USDT0",
            "--amount",
            "100",
            "--multi-sig-addr",
            MULTISIG,
            "--destination",
            RECIPIENT,
            "--from",
            "spot",
            "--to",
            "xyz",
            "--chain",
            "testnet",
            "--local",
        ]);
        let legacy = parse_send(&[
            "hypecli",
            "multisig",
            "send-asset",
            "--token",
            "USDT0",
            "--amount",
            "100",
            "--multi-sig-addr",
            MULTISIG,
            "--to",
            RECIPIENT,
            "--source",
            "spot",
            "--dest",
            "xyz",
            "--chain",
            "testnet",
            "--local",
        ]);
        assert_eq!(unified.multi_sig_addr, legacy.multi_sig_addr);
        assert_eq!(unified.local, legacy.local);
        let account = unified.multi_sig_addr.unwrap();
        let action = |cmd: &SendCmd| {
            serde_json::to_value(
                cmd.build_transfer(account, token(), 123)
                    .into_action(cmd.chain),
            )
            .unwrap()
        };
        assert_eq!(action(&unified), action(&legacy));
        assert_eq!(
            unified.build_transfer(account, token(), 123).destination,
            RECIPIENT.parse::<Address>().unwrap()
        );
    }

    #[test]
    fn single_signer_send_preserves_defaults_and_rejects_local_flag() {
        let args = ["hypecli", "send", "--token", "USDC", "--amount", "100"];
        let cmd = parse_send(&args);
        assert!(cmd.multi_sig_addr.is_none());
        assert!(!cmd.local);
        let signer = RECIPIENT.parse().unwrap();
        assert_eq!(cmd.build_transfer(signer, token(), 123).destination, signer);
        let mut invalid = args.to_vec();
        invalid.push("--local");
        assert!(Cli::try_parse_from(invalid).is_err());
    }
}

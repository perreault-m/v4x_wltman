//! XRPL network layer for the V4X Wallet Manager.
//!
//! Provides read access (balance, transaction history) via a small
//! hand-rolled JSON-RPC client built on `reqwest`, and payment submission via
//! `xrpl-mithril` (transaction building, autofill, signing, and submission).
//!
//! Only included by the `cli` binary (via `#[path]` from `src/bin/cli.rs`):
//! the GUI never depends on the network or on private key material directly
//! for these operations -- it always shells out to the `cli` binary.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-22

use crate::wallet::Wallet;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Maximum time allowed for a single network call (JSON-RPC read, faucet
/// request, or a step of building/submitting a payment) before giving up
/// with an explicit error, rather than hanging indefinitely if the XRPL
/// server is slow, unreachable, or never responds.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);

/// Which XRPL network to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    /// Parses `"testnet"`/`"mainnet"` (case-insensitive). Returns `None` if
    /// the value is not recognized.
    pub fn parse(s: &str) -> Option<Network> {
        match s.to_lowercase().as_str() {
            "testnet" | "test" => Some(Network::Testnet),
            "mainnet" | "main" => Some(Network::Mainnet),
            _ => None,
        }
    }

    /// Ordered list of public RPC servers to try for this network.
    ///
    /// Mainnet has several official public clusters available: if the first
    /// one is "amendment blocked" (behind on amendments the rest of the
    /// network has already activated, and therefore unable to process ANY
    /// transaction regardless of its validity) or simply unreachable, the
    /// next candidate is tried instead of failing on a single point of
    /// failure. All three are official public clusters listed at
    /// <https://xrpl.org/docs/tutorials/public-servers>.
    fn rpc_candidates(&self) -> &'static [&'static str] {
        match self {
            // Official public XRPL Testnet server.
            Network::Testnet => &["https://s.altnet.rippletest.net:51234/"],
            Network::Mainnet => &[
                "https://xrplcluster.com/",
                "https://s1.ripple.com:51234/",
                "https://s2.ripple.com:51234/",
            ],
        }
    }

    /// Public faucet URL (provides free test XRP). `None` on mainnet: no
    /// faucet exists for real XRP.
    fn faucet_url(&self) -> Option<&'static str> {
        match self {
            Network::Testnet => Some("https://faucet.altnet.rippletest.net/accounts"),
            Network::Mainnet => None,
        }
    }

    /// Short, human-readable network label (`"testnet"`/`"mainnet"`).
    pub fn label(&self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }
    }
}

/// Performs a single JSON-RPC call against one specific server.
async fn rpc_call_at(rpc_url: &str, method: &str, params: Value) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let body = json!({ "method": method, "params": [params] });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .timeout(NETWORK_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Erreur réseau : {}", e))?;

    let value: Value = resp
        .json()
        .await
        .map_err(|e| format!("Réponse invalide du serveur XRPL : {}", e))?;

    let result = value
        .get("result")
        .ok_or("Champ 'result' manquant dans la réponse XRPL")?
        .clone();

    if let Some(err_code) = result.get("error").and_then(|e| e.as_str()) {
        let msg = result
            .get("error_message")
            .and_then(|m| m.as_str())
            .unwrap_or(err_code);
        return Err(format!("{} ({})", msg, err_code));
    }

    Ok(result)
}

/// Returns whether an error looks like a SERVER-side health problem
/// (justifying a retry against the next candidate) rather than a legitimate
/// protocol response, such as `actNotFound` for an account that genuinely
/// doesn't exist -- any honest node would return the same answer for that,
/// so retrying elsewhere would be pointless and misleading.
///
/// Note: matches against the (French) error text produced by [`rpc_call_at`],
/// since that's the only signal available here.
fn is_server_health_issue(err: &str) -> bool {
    err.contains("amendmentBlocked")
        || err.starts_with("Erreur réseau")
        || err.starts_with("Réponse invalide")
        || err.contains("Champ 'result' manquant")
}

/// Performs a JSON-RPC call, trying each of the network's candidate servers
/// in order until one succeeds or a non-server-health error is returned.
async fn rpc_call(network: Network, method: &str, params: Value) -> Result<Value, String> {
    let candidates = network.rpc_candidates();
    let mut last_err = String::new();

    for (i, url) in candidates.iter().enumerate() {
        match rpc_call_at(url, method, params.clone()).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let is_last = i == candidates.len() - 1;
                last_err = format!("{} : {}", url, e);
                if is_last || !is_server_health_issue(&e) {
                    return Err(last_err);
                }
                // Otherwise: server-health issue detected, try the next candidate.
            }
        }
    }

    Err(last_err)
}

/// Checks that a given XRPL server is not "amendment blocked" -- i.e. behind
/// on amendments already activated by the rest of the network, in which case
/// it refuses ANY transaction submission regardless of validity (see
/// <https://xrpl.org/docs/infrastructure/troubleshooting/server-is-amendment-blocked>).
/// Used as a pre-flight check before building/signing/submitting a payment,
/// to surface a clear error immediately rather than a confusing failure from
/// deep inside `xrpl_mithril`.
async fn check_server_health(rpc_url: &str) -> Result<(), String> {
    let result = rpc_call_at(rpc_url, "server_info", json!({})).await?;

    let blocked = result
        .get("info")
        .and_then(|i| i.get("amendment_blocked"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    if blocked {
        return Err("amendment_blocked=true (serveur en retard sur des amendements déjà activés par le réseau, ne peut traiter aucune transaction)".to_string());
    }

    Ok(())
}

/// Picks the first healthy RPC server among the given network's candidates.
/// On mainnet in particular, if there are several and the first is
/// blocked/unreachable, the next ones are tried before giving up.
async fn pick_healthy_rpc_url(network: Network) -> Result<&'static str, String> {
    let candidates = network.rpc_candidates();
    let mut last_err = String::new();

    for url in candidates {
        match check_server_health(url).await {
            Ok(()) => return Ok(*url),
            Err(e) => last_err = format!("{} : {}", url, e),
        }
    }

    Err(format!(
        "Aucun serveur XRPL disponible pour {} (dernier essai -- {})",
        network.label(),
        last_err
    ))
}

/// Converts an amount in drops (string, e.g. `"999999999960"`) into a
/// human-readable XRP string (`"999999.999960"`), using integer arithmetic
/// (no floats) to avoid any precision loss on a financial amount.
fn drops_to_xrp_string(drops: &str) -> String {
    let value: i128 = drops.parse().unwrap_or(0);
    let whole = value / 1_000_000;
    let frac = (value % 1_000_000).unsigned_abs();
    format!("{}.{:06}", whole, frac)
}

/// Converts a user-entered XRP amount (e.g. `"1.5"`) into drops (`u64`).
/// Rejects anything that isn't a positive number with at most 6 decimal
/// places.
pub fn xrp_to_drops(xrp: &str) -> Result<u64, String> {
    let trimmed = xrp.trim();
    if trimmed.is_empty() {
        return Err("Montant XRP manquant.".to_string());
    }

    let mut parts = trimmed.splitn(2, '.');
    let whole_str = parts.next().unwrap_or("0");
    let frac_str = parts.next().unwrap_or("");

    if frac_str.len() > 6 {
        return Err("Trop de décimales (maximum 6 pour le XRP).".to_string());
    }
    if !whole_str.chars().all(|c| c.is_ascii_digit())
        || !frac_str.chars().all(|c| c.is_ascii_digit())
    {
        return Err("Montant XRP invalide.".to_string());
    }

    let whole: u128 = whole_str.parse().map_err(|_| "Montant XRP invalide.".to_string())?;
    let frac_padded = format!("{:0<6}", frac_str);
    let frac: u128 = frac_padded.parse().map_err(|_| "Montant XRP invalide.".to_string())?;

    let drops = whole * 1_000_000 + frac;
    u64::try_from(drops).map_err(|_| "Montant XRP trop élevé.".to_string())
}

/// Converts an XRPL "Ripple time" timestamp into a human-readable UTC string.
fn ripple_time_to_readable(ripple_time: u64) -> String {
    // The Ripple epoch starts 2000-01-01T00:00:00Z, i.e. 946684800s after the Unix epoch.
    let unix_time = ripple_time as i64 + 946_684_800;
    match chrono::DateTime::from_timestamp(unix_time, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => "?".to_string(),
    }
}

/// Account balance information for a given address/network.
#[derive(Debug, Serialize)]
pub struct Balance {
    pub address: String,
    pub network: String,
    /// Whether the account exists on the ledger (has ever received XRP).
    pub activated: bool,
    pub xrp_balance: String,
    pub drops: String,
}

/// Fetches the balance of an address. Requires ONLY the public address.
pub async fn fetch_balance(address: &str, network: Network) -> Result<Balance, String> {
    let params = json!({ "account": address, "ledger_index": "validated" });

    match rpc_call(network, "account_info", params).await {
        Ok(result) => {
            let account_data = result
                .get("account_data")
                .ok_or("Champ 'account_data' manquant dans la réponse")?;
            let drops = account_data
                .get("Balance")
                .and_then(|b| b.as_str())
                .ok_or("Champ 'Balance' manquant dans la réponse")?
                .to_string();

            Ok(Balance {
                address: address.to_string(),
                network: network.label().to_string(),
                activated: true,
                xrp_balance: drops_to_xrp_string(&drops),
                drops,
            })
        }
        // Account never activated (0 XRP received): not an error, just a zero balance.
        Err(e) if e.contains("actNotFound") => Ok(Balance {
            address: address.to_string(),
            network: network.label().to_string(),
            activated: false,
            xrp_balance: "0.000000".to_string(),
            drops: "0".to_string(),
        }),
        Err(e) => Err(e),
    }
}

/// Requests free test XRP from the public faucet. Only exists on testnet --
/// returns an explicit error if called on mainnet (there is no faucet for
/// real XRP).
pub async fn fund_via_faucet(address: &str, network: Network) -> Result<(), String> {
    let url = network
        .faucet_url()
        .ok_or("Aucun faucet n'existe sur ce réseau.".to_string())?;

    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .json(&json!({ "destination": address }))
        .timeout(NETWORK_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Erreur réseau (faucet) : {}", e))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(format!(
            "Le faucet a refusé la requête ({}) : {}",
            status,
            text.trim()
        ))
    }
}

/// Summary of a single transaction, as returned by [`fetch_transactions`].
#[derive(Debug, Serialize)]
pub struct TxSummary {
    pub hash: String,
    pub tx_type: String,
    pub date: Option<String>,
    pub amount_xrp: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub destination_tag: Option<u64>,
    pub successful: bool,
}

/// Fetches the most recent transactions for an address. Requires ONLY the
/// public address.
pub async fn fetch_transactions(
    address: &str,
    network: Network,
    limit: u32,
) -> Result<Vec<TxSummary>, String> {
    let params = json!({
        "account": address,
        "ledger_index_min": -1,
        "ledger_index_max": -1,
        "limit": limit,
        "binary": false
    });

    let result = match rpc_call(network, "account_tx", params).await {
        Ok(r) => r,
        Err(e) if e.contains("actNotFound") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let empty = Vec::new();
    let entries = result
        .get("transactions")
        .and_then(|t| t.as_array())
        .unwrap_or(&empty);

    let mut out = Vec::new();
    for entry in entries {
        // Depending on the API version, transaction fields live under "tx",
        // under "tx_json", or directly at the entry's root.
        let tx = entry
            .get("tx")
            .or_else(|| entry.get("tx_json"))
            .unwrap_or(entry);
        let meta = entry.get("meta");

        let hash = tx
            .get("hash")
            .and_then(|h| h.as_str())
            .or_else(|| entry.get("hash").and_then(|h| h.as_str()))
            .unwrap_or("?")
            .to_string();

        let tx_type = tx
            .get("TransactionType")
            .and_then(|t| t.as_str())
            .unwrap_or("?")
            .to_string();

        let from = tx.get("Account").and_then(|a| a.as_str()).map(str::to_string);
        let to = tx.get("Destination").and_then(|a| a.as_str()).map(str::to_string);
        let destination_tag = tx.get("DestinationTag").and_then(|t| t.as_u64());

        // Only native XRP amounts (drops strings) are displayed as-is;
        // issued-currency amounts (objects) are not handled here.
        let amount_xrp = tx
            .get("Amount")
            .and_then(|a| a.as_str())
            .map(drops_to_xrp_string);

        let successful = meta
            .and_then(|m| m.get("TransactionResult"))
            .and_then(|r| r.as_str())
            .map(|r| r == "tesSUCCESS")
            .unwrap_or(false);

        let date = tx
            .get("date")
            .and_then(|d| d.as_u64())
            .map(ripple_time_to_readable);

        out.push(TxSummary {
            hash,
            tx_type,
            date,
            amount_xrp,
            from,
            to,
            destination_tag,
            successful,
        });
    }

    Ok(out)
}

/// Runs a typed `xrpl-mithril` request against each of the network's
/// candidate servers in turn -- same retry policy as [`rpc_call`]: keep
/// trying the next candidate on a server-health problem (unreachable,
/// timeout, amendment-blocked), bail out immediately on anything else (e.g.
/// `actNotFound`, which every honest node would answer identically, so
/// retrying elsewhere would be pointless).
///
/// This exists alongside [`rpc_call`] (used by `fetch_balance` /
/// `fetch_transactions`) rather than replacing it: those two already have a
/// working hand-rolled JSON-RPC path, and there's no reason to touch it.
/// This helper is for new read calls -- starting with tokens/NFTs below --
/// built directly on `xrpl-mithril`'s typed request/response models instead
/// of hand-parsing `serde_json::Value`.
async fn mithril_request_with_retry<R>(network: Network, request: R) -> Result<R::Response, String>
where
    R: xrpl_mithril::models::requests::XrplRequest + Clone + Send + Sync,
{
    use xrpl_mithril::client::{Client, JsonRpcClient};

    let candidates = network.rpc_candidates();
    let mut last_err = String::new();

    for (i, url) in candidates.iter().enumerate() {
        let client = match JsonRpcClient::new(url) {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{} : erreur de connexion ({:?})", url, e);
                continue;
            }
        };

        match client.request(request.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let msg = e.to_string();
                let is_last = i == candidates.len() - 1;
                // Same intent as `is_server_health_issue`, applied to
                // xrpl-mithril's own error text since it doesn't share our
                // hand-rolled error strings.
                let lower = msg.to_lowercase();
                let is_health_issue = lower.contains("amendmentblocked")
                    || lower.contains("connect")
                    || lower.contains("timeout")
                    || lower.contains("transport")
                    || lower.contains("timed out");
                last_err = format!("{} : {}", url, msg);
                if is_last || !is_health_issue {
                    return Err(last_err);
                }
                // Otherwise: server-health issue, try the next candidate.
            }
        }
    }

    Err(last_err)
}

/// One trust line (issued-currency balance) held by an account.
#[derive(Debug, Serialize)]
pub struct TokenBalance {
    pub currency: String,
    pub issuer: String,
    pub balance: String,
    /// True if `balance` is negative, i.e. this account is the issuer's
    /// counterparty *owing* them rather than holding a positive balance --
    /// surfaced so the caller can display these differently instead of
    /// showing what looks like a normal token balance.
    pub is_negative: bool,
}

/// Fetches every trust line (issued-currency/"token" balance) for an
/// address, via `account_lines`. Requires ONLY the public address -- exactly
/// like [`fetch_balance`]/[`fetch_transactions`], no password or seed
/// involved at any point.
///
/// Uses `xrpl-mithril`'s typed request/response models (see
/// [`mithril_request_with_retry`]) rather than hand-parsed JSON.
pub async fn fetch_tokens(address: &str, network: Network) -> Result<Vec<TokenBalance>, String> {
    use xrpl_mithril::models::requests::{AccountLinesRequest, LedgerShortcut, LedgerSpecifier};

    let account = address
        .parse()
        .map_err(|_| "Adresse invalide.".to_string())?;

    let request = AccountLinesRequest {
        account,
        ledger_index: Some(LedgerSpecifier::Named(LedgerShortcut::Validated)),
        peer: None,
        limit: None,
        marker: None,
    };

    let response = match mithril_request_with_retry(network, request).await {
        Ok(r) => r,
        Err(e) if e.contains("actNotFound") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    Ok(response
        .lines
        .into_iter()
        // A "0" balance is a trust line that exists but currently holds
        // nothing (e.g. never funded, or emptied back out) -- not
        // meaningful to show as a "token owned by this account".
        .filter(|line| line.balance != "0")
        .map(|line| TokenBalance {
            currency: line.currency,
            issuer: line.account.to_classic_address(),
            is_negative: line.balance.starts_with('-'),
            balance: line.balance,
        })
        .collect())
}

/// One NFT (XLS-20 `NFToken`) held by an account.
#[derive(Debug, Serialize)]
pub struct NftInfo {
    /// Hex-encoded `NFTokenID` (32 bytes -> 64 hex chars), the canonical
    /// identifier for this NFT. Encoded to a plain hex string here so the
    /// CLI's JSON output stays a simple string, matching every other ID in
    /// this API (transaction hashes, etc.) rather than a nested byte array.
    pub nft_id: String,
    pub issuer: String,
    pub taxon: u32,
    /// Only present for NFTs minted with a sequential (non-random) taxon --
    /// absent otherwise, hence `Option`.
    pub serial: Option<u32>,
    /// Raw hex URI, if any (typically points to off-ledger metadata, e.g. a
    /// hex-encoded `ipfs://...` link). Left undecoded here on purpose --
    /// decoding/fetching it implies leaving the local machine to resolve an
    /// arbitrary URI, which is a decision for the caller (CLI/GUI), not this
    /// network layer.
    pub uri_hex: Option<String>,
}

/// Fetches every NFT owned by an address, via `account_nfts`. Requires ONLY
/// the public address.
pub async fn fetch_nfts(address: &str, network: Network) -> Result<Vec<NftInfo>, String> {
    use xrpl_mithril::models::requests::{AccountNftsRequest, LedgerShortcut, LedgerSpecifier};

    let account = address
        .parse()
        .map_err(|_| "Adresse invalide.".to_string())?;

    let request = AccountNftsRequest {
        account,
        ledger_index: Some(LedgerSpecifier::Named(LedgerShortcut::Validated)),
        limit: None,
        marker: None,
    };

    let response = match mithril_request_with_retry(network, request).await {
        Ok(r) => r,
        Err(e) if e.contains("actNotFound") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    Ok(response
        .account_nfts
        .into_iter()
        .map(|nft| NftInfo {
            nft_id: hex::encode(nft.nftoken_id.as_bytes()).to_uppercase(),
            issuer: nft.issuer.to_classic_address(),
            taxon: nft.nftoken_taxon,
            serial: nft.nft_serial,
            uri_hex: nft.uri,
        })
        .collect())
}

/// Builds, signs, and submits an XRP payment via `xrpl-mithril` (high-level
/// API: autofills Fee/Sequence/LastLedgerSequence, signs the transaction,
/// submits it, and waits for validation).
///
/// Requires the `Wallet` to have a valid XRPL `seed` -- see the
/// [`Wallet::seed`](crate::wallet::Wallet::seed) field's documentation.
///
/// The caller is responsible for only decrypting the wallet right before
/// this call and letting it go out of scope immediately afterwards (see
/// `cli.rs`'s `send` command, which runs in a dedicated process that
/// terminates right after).
pub async fn send_payment(
    wallet: &Wallet,
    destination: &str,
    amount_xrp: &str,
    destination_tag: Option<u32>,
    network: Network,
) -> Result<String, String> {
    use xrpl_mithril::client::JsonRpcClient;
    use xrpl_mithril::tx::autofill::autofill;
    use xrpl_mithril::tx::builder::PaymentBuilder;
    use xrpl_mithril::tx::{sign_transaction, submit_and_wait};
    use xrpl_mithril::types::{Amount, XrpAmount};
    use xrpl_mithril::wallet::Wallet as MithrilWallet;

    let drops = xrp_to_drops(amount_xrp)?;

    let seed = wallet.seed.as_deref().ok_or_else(|| {
        "Ce wallet n'a pas de seed XRPL (recréez-le).".to_string()
    })?;

    let sender = MithrilWallet::from_seed_encoded(seed)
        .map_err(|e| format!("Seed invalide : {:?}", e))?;

    let destination_account = destination
        .parse()
        .map_err(|_| "Adresse destinataire invalide.".to_string())?;

    let mut builder = PaymentBuilder::new()
        .account(*sender.account_id())
        .destination(destination_account)
        .amount(Amount::Xrp(
            XrpAmount::from_drops(drops).map_err(|e| format!("Montant invalide : {:?}", e))?,
        ));

    if let Some(tag) = destination_tag {
        builder = builder.destination_tag(tag);
    }

    let mut unsigned = builder
        .build()
        .map_err(|e| format!("Erreur construction tx : {:?}", e))?;

    // Pick a healthy server (not "amendment blocked") among this network's
    // candidates -- see `pick_healthy_rpc_url`. Fails with a clear error
    // immediately if none are available, rather than a confusing failure
    // surfaced from the middle of `xrpl_mithril`.
    let rpc_url = pick_healthy_rpc_url(network).await?;

    let client = JsonRpcClient::new(rpc_url)
        .map_err(|e| format!("Erreur connexion : {:?}", e))?;

    // `autofill` queries the SENDER account's state (Sequence, Fee,
    // LastLedgerSequence) -- NOT the destination's. An unactivated
    // destination is a perfectly normal case for a Payment (that's exactly
    // what activates it) and does not make `autofill` fail. If `autofill`
    // fails, it's therefore the SENDER account itself that has a problem
    // (not activated, network unreachable, etc.) -- continuing anyway would
    // produce a transaction with incomplete fields (Sequence/Fee/
    // LastLedgerSequence), which risks being silently rejected or left
    // hanging indefinitely in `submit_and_wait` without ever being
    // validated. Any error here is therefore treated as fatal.
    tokio::time::timeout(NETWORK_TIMEOUT, autofill(&client, &mut unsigned))
        .await
        .map_err(|_| "Délai dépassé lors de la préparation de la transaction (serveur XRPL injoignable ou trop lent).".to_string())?
        .map_err(|e| format!("Erreur préparation (compte émetteur) : {}", e))?;

    let signed = sign_transaction(&unsigned, &sender)
        .map_err(|e| format!("Erreur signature : {:?}", e))?;

    let result = tokio::time::timeout(NETWORK_TIMEOUT, submit_and_wait(&client, &signed))
        .await
        .map_err(|_| "Délai dépassé lors de la soumission (aucune confirmation reçue du réseau XRPL).".to_string())?
        .map_err(|e| format!("Erreur soumission : {:?}", e))?;

    if result.result_code.starts_with("tes") {
        Ok(result.hash.clone())
    } else {
        Err(format!("Transaction rejetée : {} (ledger {})", 
            result.result_code, result.ledger_index))
    }
}
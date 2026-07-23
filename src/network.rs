// Couche réseau XRPL : JSON-RPC en lecture (balance, transactions) maison via
// reqwest, et envoi de paiement via `xrpl-mithril` (construction, autofill,
// signature Ed25519 et soumission).
//
// N'est inclus QUE par le binaire CLI (#[path] depuis src/bin/cli.rs) : la GUI
// ne dépend jamais directement du réseau ni des clés privées pour ces opérations.

use crate::wallet::Wallet;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Délai maximal accordé à un appel réseau (lecture JSON-RPC, faucet, ou
/// étape de la construction/soumission d'un paiement) avant d'abandonner
/// avec une erreur explicite plutôt que de rester bloqué indéfiniment si le
/// serveur XRPL est lent, injoignable, ou ne répond jamais.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    /// Parse "testnet"/"mainnet" (insensible à la casse). `None` si invalide.
    pub fn parse(s: &str) -> Option<Network> {
        match s.to_lowercase().as_str() {
            "testnet" | "test" => Some(Network::Testnet),
            "mainnet" | "main" => Some(Network::Mainnet),
            _ => None,
        }
    }

    /// Liste ordonnée de serveurs RPC publics à essayer pour ce réseau.
    /// Sur mainnet, plusieurs clusters publics officiels existent -- si le
    /// premier est "amendment blocked" (en retard sur des amendements déjà
    /// activés par le réseau, donc incapable de traiter AUCUNE transaction,
    /// quelle que soit sa validité) ou simplement injoignable, on essaie le
    /// suivant plutôt que d'échouer directement sur un seul point de
    /// défaillance. Les trois sont des clusters publics officiels listés sur
    /// https://xrpl.org/docs/tutorials/public-servers.
    fn rpc_candidates(&self) -> &'static [&'static str] {
        match self {
            // Serveur public officiel du Testnet XRPL.
            Network::Testnet => &["https://s.altnet.rippletest.net:51234/"],
            Network::Mainnet => &[
                "https://xrplcluster.com/",
                "https://s1.ripple.com:51234/",
                "https://s2.ripple.com:51234/",
            ],
        }
    }

    /// URL du faucet public (fournit des XRP de test gratuits). `None` sur
    /// mainnet : aucun faucet n'existe pour de l'XRP réel.
    fn faucet_url(&self) -> Option<&'static str> {
        match self {
            Network::Testnet => Some("https://faucet.altnet.rippletest.net/accounts"),
            Network::Mainnet => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Network::Testnet => "testnet",
            Network::Mainnet => "mainnet",
        }
    }
}

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

/// Un échec est considéré comme un problème DE SERVEUR (justifiant d'essayer
/// le candidat suivant) plutôt qu'une réponse légitime du protocole (comme
/// `actNotFound` pour un compte qui n'existe vraiment pas -- ça, tout noeud
/// honnête répondrait pareil, donc inutile/trompeur de réessayer ailleurs).
fn is_server_health_issue(err: &str) -> bool {
    err.contains("amendmentBlocked")
        || err.starts_with("Erreur réseau")
        || err.starts_with("Réponse invalide")
        || err.contains("Champ 'result' manquant")
}

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
                // Sinon : problème de serveur détecté, on essaie le candidat suivant.
            }
        }
    }

    Err(last_err)
}

/// Vérifie qu'un serveur XRPL donné n'est pas "amendment blocked" -- c-à-d
/// en retard sur des amendements déjà activés par le reste du réseau, auquel
/// cas il refuse TOUTE soumission de transaction indépendamment de sa
/// validité (voir https://xrpl.org/docs/infrastructure/troubleshooting/server-is-amendment-blocked).
/// Utilisé comme vérification préalable avant de construire/signer/soumettre
/// un paiement, pour donner une erreur claire immédiatement plutôt qu'un
/// échec confus en plein milieu de la soumission via `xrpl_mithril`.
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

/// Choisit le premier serveur RPC sain parmi les candidats du réseau donné.
/// Sur mainnet notamment, s'il y en a plusieurs et que le premier est
/// bloqué/injoignable, essaie les suivants avant d'abandonner.
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

/// Convertit un montant en drops (chaîne, ex: "999999999960") en chaîne XRP
/// lisible ("999999.999960"), avec arithmétique entière (pas de float, pour éviter
/// toute perte de précision sur un montant financier).
fn drops_to_xrp_string(drops: &str) -> String {
    let value: i128 = drops.parse().unwrap_or(0);
    let whole = value / 1_000_000;
    let frac = (value % 1_000_000).unsigned_abs();
    format!("{}.{:06}", whole, frac)
}

/// Convertit un montant XRP saisi par l'utilisateur (ex: "1.5") en drops (u64).
/// Rejette tout ce qui n'est pas un nombre positif avec au plus 6 décimales.
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

fn ripple_time_to_readable(ripple_time: u64) -> String {
    // L'époque Ripple commence le 2000-01-01T00:00:00Z, soit 946684800s après l'époque Unix.
    let unix_time = ripple_time as i64 + 946_684_800;
    match chrono::DateTime::from_timestamp(unix_time, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => "?".to_string(),
    }
}

#[derive(Debug, Serialize)]
pub struct Balance {
    pub address: String,
    pub network: String,
    pub activated: bool,
    pub xrp_balance: String,
    pub drops: String,
}

/// Récupère la balance d'une adresse. Ne nécessite QUE l'adresse publique.
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
        // Compte jamais activé (0 XRP reçu) : pas une erreur, juste un solde nul.
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

/// Demande des XRP de test au faucet public. N'existe que sur testnet -- retourne
/// une erreur explicite si appelée sur mainnet (il n'y a pas de faucet pour de
/// l'XRP réel).
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

/// Récupère les dernières transactions d'une adresse. Ne nécessite QUE l'adresse publique.
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
        // Selon la version d'API, les champs de la transaction sont soit sous "tx",
        // soit sous "tx_json", soit directement à la racine de l'entrée.
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

        // Seuls les montants en XRP natif (chaîne de drops) sont affichés simplement ;
        // les montants en devise émise (objets) ne sont pas traités ici.
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

/// Construit, signe et soumet un paiement XRP. Nécessite le `Wallet` complet
/// (donc la clé privée) le temps de cette seule fonction — appelant·e a la
/// responsabilité de ne le déchiffrer que juste avant cet appel et de le
/// laisser sortir de portée immédiatement après (voir `cli.rs`, commande `send`,
/// qui tourne dans un processus dédié qui se termine juste après).
/// Construit, signe et soumet un paiement XRP via `xrpl-mithril` (API haut
/// niveau : autofill du Fee/Sequence/LastLedgerSequence, signature Ed25519,
/// soumission + attente de validation). Nécessite que le `Wallet` ait une
/// `seed` XRPL valide -- les wallets créés avant l'ajout de ce champ ne
/// peuvent plus signer de transaction (voir doc du champ `Wallet::seed`).
///
/// L'appelant a la responsabilité de ne déchiffrer le wallet que juste avant
/// cet appel et de le laisser sortir de portée immédiatement après (voir
/// `cli.rs`, commande `send`, qui tourne dans un processus dédié qui se
/// termine juste après).
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

    // Choisit un serveur RPC sain (pas "amendment blocked") parmi les
    // candidats connus pour ce réseau -- voir `pick_healthy_rpc_url`. Donne
    // une erreur claire immédiatement si aucun n'est disponible, plutôt
    // qu'un échec confus renvoyé depuis le milieu de `xrpl_mithril`.
    let rpc_url = pick_healthy_rpc_url(network).await?;

    let client = JsonRpcClient::new(rpc_url)
        .map_err(|e| format!("Erreur connexion : {:?}", e))?;

    // `autofill` interroge l'état du compte ÉMETTEUR (Sequence, Fee,
    // LastLedgerSequence) -- PAS celui du destinataire. Un destinataire non
    // activé est un cas parfaitement normal pour un Payment (c'est justement
    // ce qui l'active) et ne fait pas échouer `autofill`. Si `autofill`
    // échoue, c'est donc que le compte ÉMETTEUR lui-même pose problème (non
    // activé, réseau injoignable, etc.) -- continuer quand même produirait
    // une transaction avec des champs incomplets (Sequence/Fee/
    // LastLedgerSequence), qui risque d'être rejetée silencieusement ou de
    // rester bloquée indéfiniment dans `submit_and_wait` sans jamais être
    // validée. On traite donc toute erreur ici comme fatale.
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
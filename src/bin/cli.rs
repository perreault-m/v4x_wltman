// Binaire CLI indépendant de la GUI. Compilé séparément : `cargo run --bin cli -- ...`
//
// Convention de sortie pensée pour être appelée depuis d'autres langages/scripts :
//   - stdout  -> uniquement le résultat exploitable (JSON)
//   - stderr  -> messages de progression / logs humains
//   - code de sortie non nul en cas d'erreur
//
// SÉCURITÉ : les commandes `balance` et `transactions` ne nécessitent qu'une
// adresse publique (--address), jamais de mot de passe. La commande `send`
// déchiffre le wallet uniquement dans CE processus, qui se termine juste après
// -- la clé privée ne persiste jamais au-delà de cet appel.

#[path = "../wallet.rs"]
mod wallet;
#[path = "../network.rs"]
mod network;

use network::Network;
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    match args.get(1).map(String::as_str) {
        Some("balance") => handle_balance(&args).await,
        Some("transactions") | Some("history") => handle_transactions(&args).await,
        Some("send") => handle_send(&args).await,
        Some("faucet") => handle_faucet(&args).await,
        _ if args.iter().any(|a| a == "--address") => handle_address(&args),
        _ if args.iter().any(|a| a == "--decrypt") => handle_decrypt(&args),
        _ => handle_generate(&args),
    }
}

/// Lit `--network testnet|mainnet` n'importe où dans les arguments.
/// Par défaut (et si absent/invalide) : **testnet**, par sécurité.
fn parse_network(args: &[String]) -> Network {
    for i in 0..args.len() {
        if args[i] == "--network" {
            if let Some(value) = args.get(i + 1) {
                if let Some(net) = Network::parse(value) {
                    return net;
                }
                eprintln!(
                    "Valeur --network invalide ('{}'), utilisation de testnet par défaut.",
                    value
                );
            }
        }
    }
    Network::Testnet
}

fn get_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Lit le mot de passe depuis stdin jusqu'à EOF (PAS un prompt interactif : la
/// GUI écrit le mot de passe dans le pipe puis le ferme immédiatement, donc
/// cette lecture retourne dès que l'appelant a fini d'écrire -- aucun blocage
/// en attente d'une saisie clavier).
fn read_password_from_stdin() -> Result<String, String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("Erreur de lecture du mot de passe (stdin) : {}", e))?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Résout le mot de passe : `--password-stdin` (recommandé -- n'apparaît jamais
/// dans la liste des processus) a priorité sur `-p`/`--password` (pratique pour
/// un usage manuel/scripts, mais visible via `ps`/gestionnaire de tâches).
fn resolve_password(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a == "--password-stdin") {
        match read_password_from_stdin() {
            Ok(pw) => Some(pw),
            Err(e) => {
                eprintln!("Erreur : {}", e);
                std::process::exit(1);
            }
        }
    } else {
        get_flag_value(args, "-p").or_else(|| get_flag_value(args, "--password"))
    }
}

// ============================== GÉNÉRATION ==============================

fn handle_generate(args: &[String]) {
    let mut prefixes: Vec<String> = Vec::new();
    let mut password: Option<String> = None;
    let mut name = "wallet".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--vanity" => {
                if i + 1 < args.len() {
                    prefixes = wallet::parse_prefixes(&args[i + 1]);
                    i += 1;
                }
            }
            "--encrypt" => {
                if i + 1 < args.len() {
                    password = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--name" => {
                if i + 1 < args.len() {
                    name = args[i + 1].clone();
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    eprintln!("Generating XRPL wallet...");

    let generated_wallet = if !prefixes.is_empty() {
        eprintln!("Searching for vanity address with prefixes: {:?}", prefixes);
        generate_vanity_with_progress(&prefixes)
    } else {
        Some(wallet::generate_random_wallet())
    };

    let w = match generated_wallet {
        Some(w) => w,
        None => {
            eprintln!("Génération annulée.");
            std::process::exit(1);
        }
    };

    eprintln!("✅ Wallet found. Address: {}", w.address);

    let save_result = match &password {
        Some(pw) if !pw.is_empty() => wallet::encrypt_and_save(&w, &name, pw),
        _ => wallet::save_wallet(&w, &name),
    };

    match save_result {
        Ok(path) => eprintln!("💾 Wallet saved to: {}", path.display()),
        Err(e) => {
            eprintln!("Erreur lors de la sauvegarde : {}", e);
            std::process::exit(1);
        }
    }

    // Seule ligne destinée à être parsée par un programme appelant.
    println!("{}", serde_json::to_string_pretty(&w).unwrap());
}

fn generate_vanity_with_progress(prefixes: &[String]) -> Option<wallet::Wallet> {
    let attempts = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let monitor_attempts = Arc::clone(&attempts);
    let monitor_done = Arc::clone(&done);
    let monitor = std::thread::spawn(move || {
        let mut last_reported = 0u64;
        while !monitor_done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            let current = monitor_attempts.load(Ordering::Relaxed);
            if current / 50_000 > last_reported / 50_000 {
                eprintln!("Attempts: {}...", current);
                last_reported = current;
            }
        }
    });

    let result = wallet::generate_vanity_wallet(prefixes, Some(Arc::clone(&attempts)), None);

    done.store(true, Ordering::Relaxed);
    let _ = monitor.join();

    result
}

// ============================== DÉCHIFFREMENT ==============================

/// Déchiffre (ou lit) un wallet et n'affiche QUE son adresse/clé publique
/// -- jamais la clé privée. À utiliser pour "charger" un wallet sans exposer
/// son secret.
fn handle_address(args: &[String]) {
    let file = match get_flag_value(args, "-f").or_else(|| get_flag_value(args, "--file")) {
        Some(f) => f,
        None => {
            eprintln!("Erreur : argument -f/--file <chemin> manquant");
            std::process::exit(1);
        }
    };
    let password = resolve_password(args);

    let result = match password {
        Some(pw) => wallet::decrypt_wallet(&file, &pw),
        None => wallet::load_plain_wallet(&file),
    };

    match result {
        Ok(w) => {
            let info = serde_json::json!({
                "address": w.address,
                "public_key": w.public_key,
            });
            println!("{}", serde_json::to_string_pretty(&info).unwrap());
        }
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

/// Déchiffre un wallet et affiche son JSON complet (INCLUT la clé privée).
/// Conservé pour compatibilité/scripting ; préférer `--address` quand la
/// clé privée n'est pas nécessaire.
fn handle_decrypt(args: &[String]) {
    let file = match get_flag_value(args, "-f").or_else(|| get_flag_value(args, "--file")) {
        Some(f) => f,
        None => {
            eprintln!("Erreur : argument -f/--file <chemin> manquant");
            std::process::exit(1);
        }
    };
    let pw = match resolve_password(args) {
        Some(p) => p,
        None => {
            eprintln!("Erreur : mot de passe manquant (-p/--password ou --password-stdin)");
            std::process::exit(1);
        }
    };

    match wallet::decrypt_wallet_file(&file, &pw) {
        Ok(json) => println!("{}", json),
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

// ============================== RÉSEAU (lecture) ==============================

async fn handle_balance(args: &[String]) {
    let address = match get_flag_value(args, "--address") {
        Some(a) => a,
        None => {
            eprintln!("Erreur : argument --address <adresse> manquant");
            std::process::exit(1);
        }
    };
    let network = parse_network(args);

    eprintln!("Interrogation du réseau XRPL ({})...", network.label());

    match network::fetch_balance(&address, network).await {
        Ok(balance) => println!("{}", serde_json::to_string_pretty(&balance).unwrap()),
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

async fn handle_transactions(args: &[String]) {
    let address = match get_flag_value(args, "--address") {
        Some(a) => a,
        None => {
            eprintln!("Erreur : argument --address <adresse> manquant");
            std::process::exit(1);
        }
    };
    let network = parse_network(args);
    let limit: u32 = get_flag_value(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    eprintln!("Interrogation du réseau XRPL ({})...", network.label());

    match network::fetch_transactions(&address, network, limit).await {
        Ok(txs) => println!("{}", serde_json::to_string_pretty(&txs).unwrap()),
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

async fn handle_faucet(args: &[String]) {
    let address = match get_flag_value(args, "--address") {
        Some(a) => a,
        None => {
            eprintln!("Erreur : argument --address <adresse> manquant");
            std::process::exit(1);
        }
    };
    let network = parse_network(args);

    if network == Network::Mainnet {
        eprintln!("Erreur : aucun faucet n'existe sur le mainnet (XRP réel uniquement).");
        std::process::exit(1);
    }

    eprintln!("Requête au faucet {} pour {}...", network.label(), address);

    match network::fund_via_faucet(&address, network).await {
        Ok(()) => {
            eprintln!("✅ Requête de faucet acceptée.");
            let result = serde_json::json!({ "address": address, "network": network.label() });
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

// ============================== ENVOI ==============================

async fn handle_send(args: &[String]) {
    let file = match get_flag_value(args, "-f").or_else(|| get_flag_value(args, "--file")) {
        Some(f) => f,
        None => {
            eprintln!("Erreur : argument -f/--file <chemin> manquant");
            std::process::exit(1);
        }
    };
    let password = match resolve_password(args) {
        Some(p) => p,
        None => {
            eprintln!("Erreur : mot de passe manquant (-p/--password ou --password-stdin)");
            std::process::exit(1);
        }
    };
    let destination = match get_flag_value(args, "--to") {
        Some(d) => d,
        None => {
            eprintln!("Erreur : argument --to <adresse_destinataire> manquant");
            std::process::exit(1);
        }
    };
    let amount = match get_flag_value(args, "--amount") {
        Some(a) => a,
        None => {
            eprintln!("Erreur : argument --amount <montant_xrp> manquant");
            std::process::exit(1);
        }
    };
    // Optionnel, mais si présent doit être un entier valide -- on préfère
    // échouer bruyamment plutôt que d'ignorer silencieusement un tag mal
    // formé (ce qui pourrait faire perdre les fonds chez le destinataire).
    let destination_tag: Option<u32> = match get_flag_value(args, "--destination-tag") {
        Some(v) => match v.parse::<u32>() {
            Ok(t) => Some(t),
            Err(_) => {
                eprintln!("Erreur : --destination-tag doit être un entier positif (0 à 4294967295)");
                std::process::exit(1);
            }
        },
        None => None,
    };
    let network = parse_network(args);

    eprintln!("Déchiffrement du wallet...");

    // Le wallet déchiffré (avec sa clé privée) ne vit que dans cette fonction,
    // le temps de signer et soumettre -- puis ce processus se termine.
    let decrypted = match wallet::decrypt_wallet(&file, &password) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    };

    eprintln!(
        "Envoi de {} XRP vers {}{} sur {}...",
        amount,
        destination,
        destination_tag
            .map(|t| format!(" (tag: {})", t))
            .unwrap_or_default(),
        network.label()
    );

    match network::send_payment(&decrypted, &destination, &amount, destination_tag, network).await {
        Ok(tx_hash) => {
            eprintln!("✅ Transaction validée.");
            let result = serde_json::json!({ "hash": tx_hash, "network": network.label() });
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Err(e) => {
            eprintln!("Erreur : {}", e);
            std::process::exit(1);
        }
    }
}

fn print_help() {
    eprintln!(
        r#"XRPL Wallet CLI

GÉNÉRATION :
  cli [--vanity <PREFIXES>] [--encrypt <PASSWORD>] [--name <NOM>]

CONSULTATION (adresse publique seulement, aucun mot de passe requis) :
  cli balance --address <ADRESSE> [--network testnet|mainnet]
  cli transactions --address <ADRESSE> [--network testnet|mainnet] [--limit N]

FAUCET (testnet uniquement -- obtenir des XRP de test gratuits) :
  cli faucet --address <ADRESSE> [--network testnet]

ENVOI (déchiffre le wallet le temps de cet appel uniquement) :
  cli send -f <FICHIER> --password-stdin --to <DESTINATAIRE> --amount <XRP> [--destination-tag <N>] [--network testnet|mainnet]
  (ou -p <MOT_DE_PASSE> à la place de --password-stdin, pour un usage manuel)

DÉCHIFFREMENT :
  cli --address -f <FICHIER> [--password-stdin | -p <MOT_DE_PASSE>]   Affiche uniquement l'adresse/clé publique
  cli --decrypt -f <FICHIER> (--password-stdin | -p <MOT_DE_PASSE>)   Affiche le wallet complet (clé privée incluse)

OPTIONS COMMUNES :
  --network <testnet|mainnet>   Réseau XRPL à utiliser. Défaut : testnet (sécurité)
  --password-stdin              Lit le mot de passe depuis stdin plutôt qu'en argument
                                 (recommandé : n'apparaît jamais dans la liste des processus)
  -h, --help                    Affiche cette aide

STOCKAGE :
  Les wallets sont sauvegardés dans <dossier de l'exécutable>/wallets/
  En clair   : wallets/<nom>.json
  Chiffré    : wallets/<nom>.encrypted.json

SORTIE (pour intégration avec d'autres langages) :
  - stdout : uniquement le JSON du résultat -> facile à parser
  - stderr : messages de progression / logs humains
  - code de sortie != 0 en cas d'erreur
"#
    );
}
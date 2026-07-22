// Binaire GUI indépendant du CLI. Compilé séparément : `cargo run --bin gui`
//
// SÉCURITÉ : ce processus ne déchiffre JAMAIS de wallet lui-même. "Charger un
// wallet", "solde/transactions" et "envoyer" appellent tous le binaire `cli`
// en sous-processus. La clé privée n'existe donc jamais dans la mémoire de la
// GUI -- seulement, brièvement, dans le processus `cli` enfant, qui se termine
// juste après chaque opération.

#[path = "../wallet.rs"]
mod wallet;

use iced::widget::{
    button, center, checkbox, column, container, mouse_area, opaque, pick_list, row, scrollable,
    stack, text, text_input, toggler, Column,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Size, Task, Theme};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wallet::{Wallet, WalletFile};

// --- Palette "V4X" : vert technologique sur fond très sombre ---
const ACCENT: Color = Color::from_rgb(0.0, 0.95, 0.35);
const ACCENT_HOVER: Color = Color::from_rgb(0.25, 1.0, 0.55);
const ACCENT_PRESS: Color = Color::from_rgb(0.0, 0.65, 0.25);
const WARNING: Color = Color::from_rgb(1.0, 0.62, 0.0);
const WARNING_HOVER: Color = Color::from_rgb(1.0, 0.75, 0.25);
const SUCCESS: Color = Color::from_rgb(0.25, 0.95, 0.45);
const ERROR: Color = Color::from_rgb(1.0, 0.35, 0.35);
const MUTED: Color = Color::from_rgb(0.55, 0.68, 0.6);
/// Orange brûlé utilisé pour les titres de panneaux ("PORTEFEUILLE",
/// "ACTIONS", "SOLDE & TRANSACTIONS"), pour les distinguer du vert d'accent
/// utilisé ailleurs (adresses, états actifs, etc).
const TITLE_COLOR: Color = Color::from_rgb(0.80, 0.40, 0.12);
const PAGE_BG: Color = Color::from_rgb(0.02, 0.03, 0.025);
const PANEL_BG: Color = Color::from_rgb(0.05, 0.08, 0.06);
const PANEL_BORDER: Color = Color::from_rgba(0.0, 0.95, 0.35, 0.25);

const V4X_PREFIX: &str = "RV4X";

fn main() -> iced::Result {
    iced::application(MyApp::title, MyApp::update, MyApp::view)
        .window_size(Size::new(860.0, 820.0))
        .centered()
        .theme(MyApp::theme)
        .run()
}

// ============================== Sous-processus CLI ==============================

fn cli_binary_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cli"));
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let name = if cfg!(windows) { "cli.exe" } else { "cli" };
    dir.join(name)
}

/// Lance le binaire `cli` avec les arguments donnés et retourne son stdout (JSON).
/// Bloquant : à appeler uniquement depuis un thread d'arrière-plan.
fn run_cli(args: Vec<String>) -> Result<String, String> {
    run_cli_with_stdin(args, None)
}

/// Variante de `run_cli` qui peut transmettre une donnée sensible (mot de passe)
/// via le pipe stdin du sous-processus plutôt qu'en argument de ligne de commande
/// -- ça évite qu'elle apparaisse dans la liste des processus (`ps`/gestionnaire
/// de tâches). On écrit la donnée puis on ferme immédiatement le pipe : le CLI
/// lit jusqu'à EOF sans jamais attendre une saisie clavier, donc ce n'est pas
/// interactif ni bloquant au-delà de ce que `run_cli` fait déjà.
fn run_cli_with_stdin(args: Vec<String>, stdin_input: Option<&str>) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut command = std::process::Command::new(cli_binary_path());
    command.args(&args);
    command.stdin(if stdin_input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| format!("Impossible de lancer le CLI : {}", e))?;

    if let Some(data) = stdin_input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data.as_bytes());
            // `stdin` est droppé ici -> le pipe se ferme -> le CLI voit EOF
            // immédiatement, il n'attend jamais de saisie clavier.
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Erreur d'exécution du CLI : {}", e))?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|_| "Sortie CLI invalide (UTF-8).".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "Le CLI a échoué.".to_string()
        } else {
            stderr
        })
    }
}

// ============================== Types ==============================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Modal {
    #[default]
    None,
    Create,
    Load,
    Send,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum NetworkChoice {
    #[default]
    Testnet,
    Mainnet,
}

impl NetworkChoice {
    fn as_str(&self) -> &'static str {
        match self {
            NetworkChoice::Testnet => "testnet",
            NetworkChoice::Mainnet => "mainnet",
        }
    }
}

type GenOutcome = Result<(String, Wallet, PathBuf, bool), String>;
/// (adresse, clé publique) -- jamais la clé privée.
type LoadOutcome = Result<(String, String), String>;
type InfoOutcome = Result<(BalanceInfo, Vec<TxInfo>), String>;
type SendOutcome = Result<String, String>;
type FaucetOutcome = Result<(), String>;

/// Un wallet "déverrouillé" pour la session : seulement son adresse (jamais sa
/// clé privée). Il faudra re-saisir le mot de passe pour envoyer une transaction.
#[derive(Debug, Clone)]
struct UnlockedWallet {
    name: String,
    address: String,
    path: PathBuf,
    /// Vrai si le fichier de ce wallet est chiffré (`*.encrypted.json`) --
    /// détermine si un mot de passe est nécessaire pour l'envoi.
    encrypted: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct BalanceInfo {
    activated: bool,
    xrp_balance: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TxInfo {
    hash: String,
    tx_type: String,
    date: Option<String>,
    amount_xrp: Option<String>,
    destination_tag: Option<u64>,
    successful: bool,
}

#[derive(Default)]
struct MyApp {
    modal: Modal,
    network: NetworkChoice,

    // --- création ---
    wallet_name_input: String,
    use_v4x_address: bool,
    use_encryption: bool,
    password_input: String,
    generating: bool,
    attempts: Arc<AtomicU64>,
    cancel_flag: Arc<AtomicBool>,
    gen_result: Arc<Mutex<Option<GenOutcome>>>,
    create_error: Option<String>,
    create_success: Option<String>,

    // --- chargement (déverrouillage, adresse seulement) ---
    available_wallets: Vec<WalletFile>,
    selected_wallet_file: Option<WalletFile>,
    load_password: String,
    load_error: Option<String>,
    loading: bool,
    load_result: Arc<Mutex<Option<LoadOutcome>>>,

    // --- session : wallets déverrouillés (adresse uniquement) ---
    unlocked_wallets: Vec<UnlockedWallet>,
    selected_unlocked: Option<String>,

    // --- solde + transactions du wallet actif ---
    info_loading: bool,
    info_error: Option<String>,
    current_balance: Option<BalanceInfo>,
    current_txs: Vec<TxInfo>,
    info_result: Arc<Mutex<Option<InfoOutcome>>>,

    // --- faucet (testnet uniquement) ---
    faucet_requesting: bool,
    faucet_message: Option<String>,
    faucet_error: Option<String>,
    faucet_result: Arc<Mutex<Option<FaucetOutcome>>>,

    // --- envoi ---
    send_destination: String,
    send_amount: String,
    send_destination_tag: String,
    send_password: String,
    send_confirming: bool,
    sending: bool,
    send_error: Option<String>,
    send_success: Option<String>,
    send_result: Arc<Mutex<Option<SendOutcome>>>,

    // --- vérification d'activation du destinataire (avant envoi) ---
    dest_check_loading: bool,
    dest_check_error: Option<String>,
    /// `Some(false)` = le compte destinataire n'existe pas encore sur le
    /// réseau (jamais activé) -- l'envoi va donc créer/activer ce compte.
    dest_activated: Option<bool>,
    dest_check_result: Arc<Mutex<Option<Result<bool, String>>>>,
    /// L'utilisateur a coché la case reconnaissant qu'il active un nouveau
    /// compte. Requis avant de pouvoir confirmer l'envoi si `dest_activated
    /// == Some(false)`.
    activation_acknowledged: bool,
}

#[derive(Debug, Clone)]
enum Message {
    OpenCreateModal,
    OpenLoadModal,
    OpenSendModal,
    CloseModal,

    NetworkChanged(NetworkChoice),

    WalletNameChanged(String),
    V4xAddressToggled(bool),
    EncryptionToggled(bool),
    PasswordChanged(String),
    GenerateWallet,
    CancelGeneration,
    TickGenerate,

    SelectWalletFile(String),
    LoadPasswordChanged(String),
    DecryptWallet,
    TickLoad,

    SelectWallet(String),
    RefreshInfo,
    TickInfo,

    RequestFaucet,
    TickFaucet,

    SendDestinationChanged(String),
    SendAmountChanged(String),
    SendDestinationTagChanged(String),
    SendPasswordChanged(String),
    ReviewSend,
    CancelSendReview,
    SendTransaction,
    TickSend,
    TickDestCheck,
    AcknowledgeActivation(bool),

    CopyAddress(String),
}

impl MyApp {
    fn title(&self) -> String {
        String::from("V4X Wallet Manager")
    }

    fn theme(_state: &Self) -> Theme {
        Theme::Dark
    }

    /// Programme un prochain message après un court délai (utilisé pour tout
    /// polling de tâche d'arrière-plan : génération, chargement, solde, envoi).
    fn schedule(message: Message) -> Task<Message> {
        Task::perform(
            async { tokio::time::sleep(Duration::from_millis(200)).await },
            move |_| message.clone(),
        )
    }

    /// (Re)lance la récupération du solde + des dernières transactions pour le
    /// wallet actuellement sélectionné, sur le réseau actuellement choisi.
    /// Ne nécessite que l'adresse -- aucun mot de passe.
    fn trigger_refresh(&mut self) -> Task<Message> {
        self.info_error = None;

        let address = match self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
        {
            Some(w) => w.address.clone(),
            None => return Task::none(),
        };

        self.info_loading = true;
        self.current_balance = None;
        self.current_txs.clear();
        *self.info_result.lock().unwrap() = None;

        let result_slot = Arc::clone(&self.info_result);
        let network = self.network.as_str().to_string();

        std::thread::spawn(move || {
            let balance_res = run_cli(vec![
                "balance".into(),
                "--address".into(),
                address.clone(),
                "--network".into(),
                network.clone(),
            ])
            .and_then(|s| {
                serde_json::from_str::<BalanceInfo>(&s)
                    .map_err(|_| "Réponse balance invalide.".to_string())
            });

            let outcome = match balance_res {
                Ok(balance) => {
                    let tx_res = run_cli(vec![
                        "transactions".into(),
                        "--address".into(),
                        address,
                        "--network".into(),
                        network,
                        "--limit".into(),
                        "10".into(),
                    ])
                    .and_then(|s| {
                        serde_json::from_str::<Vec<TxInfo>>(&s)
                            .map_err(|_| "Réponse transactions invalide.".to_string())
                    });
                    tx_res.map(|txs| (balance, txs))
                }
                Err(e) => Err(e),
            };

            *result_slot.lock().unwrap() = Some(outcome);
        });

        Self::schedule(Message::TickInfo)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenCreateModal => {
                self.modal = Modal::Create;
                if !self.generating {
                    self.wallet_name_input.clear();
                    self.use_v4x_address = false;
                    self.use_encryption = false;
                    self.password_input.clear();
                    self.create_error = None;
                    self.create_success = None;
                }
            }
            Message::OpenLoadModal => {
                self.modal = Modal::Load;
                self.available_wallets = wallet::list_wallets();
                self.selected_wallet_file = None;
                self.load_password.clear();
                self.load_error = None;
            }
            Message::OpenSendModal => {
                self.modal = Modal::Send;
                if !self.sending {
                    self.send_destination.clear();
                    self.send_amount.clear();
                    self.send_destination_tag.clear();
                    self.send_password.clear();
                    self.send_confirming = false;
                    self.send_error = None;
                    self.send_success = None;
                    self.dest_check_loading = false;
                    self.dest_check_error = None;
                    self.dest_activated = None;
                    self.activation_acknowledged = false;
                }
            }
            Message::CloseModal => {
                self.modal = Modal::None;
            }

            Message::NetworkChanged(net) => {
                self.network = net;
                self.current_balance = None;
                self.current_txs.clear();
                self.info_error = None;
                self.faucet_message = None;
                self.faucet_error = None;
                if self.selected_unlocked.is_some() {
                    return self.trigger_refresh();
                }
            }

            Message::WalletNameChanged(s) => self.wallet_name_input = s,
            Message::V4xAddressToggled(v) => self.use_v4x_address = v,
            Message::EncryptionToggled(v) => self.use_encryption = v,
            Message::PasswordChanged(s) => self.password_input = s,

            Message::GenerateWallet => {
                self.create_error = None;
                self.create_success = None;

                let name = self.wallet_name_input.trim().to_string();
                if name.is_empty() {
                    self.create_error = Some("Veuillez entrer un nom pour le wallet.".into());
                    return Task::none();
                }

                let prefixes: Vec<String> = if self.use_v4x_address {
                    vec![V4X_PREFIX.to_string()]
                } else {
                    Vec::new()
                };

                let password = if self.use_encryption {
                    Some(self.password_input.clone())
                } else {
                    None
                };

                self.attempts.store(0, Ordering::Relaxed);
                self.cancel_flag.store(false, Ordering::Relaxed);
                *self.gen_result.lock().unwrap() = None;
                self.generating = true;

                let attempts = Arc::clone(&self.attempts);
                let cancel = Arc::clone(&self.cancel_flag);
                let result_slot = Arc::clone(&self.gen_result);
                let name_for_thread = name.clone();
                let use_v4x = self.use_v4x_address;

                std::thread::spawn(move || {
                    let wallet_result: Result<Option<wallet::Wallet>, String> = if use_v4x {
                        wallet::generate_vanity_wallet(&prefixes, Some(attempts), Some(cancel))
                    } else {
                        wallet::generate_random_wallet().map(Some)
                    };

                    let outcome: GenOutcome = match wallet_result {
                        Ok(None) => Err("Recherche annulée.".to_string()),
                        Ok(Some(w)) => {
                            let is_encrypted = matches!(&password, Some(pw) if !pw.is_empty());
                            let save_result = match &password {
                                Some(pw) if !pw.is_empty() => {
                                    wallet::encrypt_and_save(&w, &name_for_thread, pw)
                                }
                                _ => wallet::save_wallet(&w, &name_for_thread),
                            };
                            save_result.map(|path| (name_for_thread.clone(), w, path, is_encrypted))
                        }
                        Err(e) => Err(e),
                    };

                    *result_slot.lock().unwrap() = Some(outcome);
                });

                return Self::schedule(Message::TickGenerate);
            }

            Message::CancelGeneration => {
                self.cancel_flag.store(true, Ordering::Relaxed);
            }

            Message::TickGenerate => {
                if self.generating {
                    let mut slot = self.gen_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.generating = false;
                        match outcome {
                            Ok((name, w, _path, is_encrypted)) => {
                                self.unlocked_wallets.retain(|u| u.name != name);
                                self.unlocked_wallets.push(UnlockedWallet {
                                    name: name.clone(),
                                    address: w.address.clone(),
                                    path: _path,
                                    encrypted: is_encrypted,
                                });
                                self.unlocked_wallets.sort_by(|a, b| a.name.cmp(&b.name));
                                self.selected_unlocked = Some(name.clone());
                                self.create_success =
                                    Some(format!("Wallet V4X « {} » créé avec succès.", name));
                                drop(slot);
                                return self.trigger_refresh();
                            }
                            Err(e) => self.create_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickGenerate);
                    }
                }
            }

            Message::SelectWalletFile(name) => {
                self.selected_wallet_file =
                    self.available_wallets.iter().find(|w| w.name == name).cloned();
                self.load_error = None;
            }
            Message::LoadPasswordChanged(s) => self.load_password = s,
            Message::DecryptWallet => {
                self.load_error = None;

                if let Some(file) = self.selected_wallet_file.clone() {
                    self.loading = true;
                    *self.load_result.lock().unwrap() = None;

                    let result_slot = Arc::clone(&self.load_result);
                    let path_str = file.path.to_string_lossy().to_string();
                    let password = self.load_password.clone();

                    std::thread::spawn(move || {
                        let mut args = vec!["--address".to_string(), "-f".to_string(), path_str];
                        let stdin_data = if !password.is_empty() {
                            args.push("--password-stdin".to_string());
                            Some(password)
                        } else {
                            None
                        };

                        let outcome: LoadOutcome =
                            run_cli_with_stdin(args, stdin_data.as_deref()).and_then(|stdout| {
                            let v: serde_json::Value = serde_json::from_str(&stdout)
                                .map_err(|_| "Réponse CLI invalide.".to_string())?;
                            let address = v
                                .get("address")
                                .and_then(|a| a.as_str())
                                .map(str::to_string);
                            let public_key = v
                                .get("public_key")
                                .and_then(|a| a.as_str())
                                .map(str::to_string);
                            match (address, public_key) {
                                (Some(a), Some(p)) => Ok((a, p)),
                                _ => Err("Réponse CLI incomplète.".to_string()),
                            }
                        });

                        *result_slot.lock().unwrap() = Some(outcome);
                    });

                    return Self::schedule(Message::TickLoad);
                }
            }
            Message::TickLoad => {
                if self.loading {
                    let mut slot = self.load_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.loading = false;
                        match outcome {
                            Ok((address, _public_key)) => {
                                if let Some(file) = self.selected_wallet_file.clone() {
                                    self.unlocked_wallets.retain(|w| w.name != file.name);
                                    self.unlocked_wallets.push(UnlockedWallet {
                                        name: file.name.clone(),
                                        address,
                                        path: file.path.clone(),
                                        encrypted: file.encrypted,
                                    });
                                    self.unlocked_wallets.sort_by(|a, b| a.name.cmp(&b.name));
                                    self.selected_unlocked = Some(file.name.clone());
                                }
                                self.load_password.clear();
                                self.modal = Modal::None;
                                drop(slot);
                                return self.trigger_refresh();
                            }
                            Err(e) => self.load_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickLoad);
                    }
                }
            }

            Message::SelectWallet(name) => {
                self.selected_unlocked = Some(name);
                self.faucet_message = None;
                self.faucet_error = None;
                return self.trigger_refresh();
            }
            Message::RefreshInfo => {
                return self.trigger_refresh();
            }
            Message::TickInfo => {
                if self.info_loading {
                    let mut slot = self.info_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.info_loading = false;
                        match outcome {
                            Ok((balance, txs)) => {
                                self.current_balance = Some(balance);
                                self.current_txs = txs;
                            }
                            Err(e) => self.info_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickInfo);
                    }
                }
            }

            Message::RequestFaucet => {
                self.faucet_error = None;
                self.faucet_message = None;

                let address = match self
                    .selected_unlocked
                    .as_ref()
                    .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
                {
                    Some(w) => w.address.clone(),
                    None => {
                        self.faucet_error = Some("Aucun wallet sélectionné.".into());
                        return Task::none();
                    }
                };

                self.faucet_requesting = true;
                *self.faucet_result.lock().unwrap() = None;

                let result_slot = Arc::clone(&self.faucet_result);

                std::thread::spawn(move || {
                    let outcome: FaucetOutcome = run_cli(vec![
                        "faucet".to_string(),
                        "--address".to_string(),
                        address,
                        "--network".to_string(),
                        "testnet".to_string(),
                    ])
                    .map(|_| ());

                    *result_slot.lock().unwrap() = Some(outcome);
                });

                return Self::schedule(Message::TickFaucet);
            }
            Message::TickFaucet => {
                if self.faucet_requesting {
                    let mut slot = self.faucet_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.faucet_requesting = false;
                        match outcome {
                            Ok(()) => {
                                self.faucet_message = Some(
                                    "Requête acceptée. Rafraîchissez dans quelques secondes pour voir le solde.".to_string(),
                                );
                                drop(slot);
                                return self.trigger_refresh();
                            }
                            Err(e) => self.faucet_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickFaucet);
                    }
                }
            }

            Message::SendDestinationChanged(s) => self.send_destination = s,
            Message::SendAmountChanged(s) => self.send_amount = s,
            Message::SendDestinationTagChanged(s) => self.send_destination_tag = s,
            Message::SendPasswordChanged(s) => self.send_password = s,

            Message::ReviewSend => {
                self.send_error = None;

                let wallet_encrypted = match self
                    .selected_unlocked
                    .as_ref()
                    .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
                {
                    Some(w) => w.encrypted,
                    None => {
                        self.send_error = Some("Aucun wallet sélectionné.".into());
                        return Task::none();
                    }
                };
                if !looks_like_xrpl_address(&self.send_destination) {
                    self.send_error =
                        Some("Adresse destinataire invalide (doit commencer par 'r').".into());
                    return Task::none();
                }
                if !looks_like_xrp_amount(&self.send_amount) {
                    self.send_error =
                        Some("Montant XRP invalide (nombre positif, 6 décimales max).".into());
                    return Task::none();
                }
                let tag_input = self.send_destination_tag.trim();
                if !tag_input.is_empty() && tag_input.parse::<u32>().is_err() {
                    self.send_error =
                        Some("Destination tag invalide (doit être un nombre entier).".into());
                    return Task::none();
                }
                // Un mot de passe n'est nécessaire que si ce wallet est
                // effectivement chiffré -- un wallet non chiffré peut être
                // utilisé pour l'envoi sans mot de passe.
                if wallet_encrypted && self.send_password.is_empty() {
                    self.send_error = Some("Mot de passe manquant.".into());
                    return Task::none();
                }

                self.send_confirming = true;
                self.activation_acknowledged = false;
                self.dest_activated = None;
                self.dest_check_error = None;
                self.dest_check_loading = true;
                *self.dest_check_result.lock().unwrap() = None;

                // Vérifie si le compte destinataire existe déjà sur le réseau
                // (n'a besoin que de l'adresse -- aucun mot de passe). Permet
                // d'avertir l'utilisateur si cet envoi va activer un compte
                // qui n'existe pas encore.
                let destination = self.send_destination.trim().to_string();
                let network = self.network.as_str().to_string();
                let result_slot = Arc::clone(&self.dest_check_result);

                std::thread::spawn(move || {
                    let outcome: Result<bool, String> = run_cli(vec![
                        "balance".to_string(),
                        "--address".to_string(),
                        destination,
                        "--network".to_string(),
                        network,
                    ])
                    .and_then(|s| {
                        serde_json::from_str::<serde_json::Value>(&s)
                            .map_err(|_| "Réponse balance invalide.".to_string())
                    })
                    .and_then(|v| {
                        v.get("activated")
                            .and_then(|a| a.as_bool())
                            .ok_or_else(|| "Réponse balance incomplète.".to_string())
                    });

                    *result_slot.lock().unwrap() = Some(outcome);
                });

                return Self::schedule(Message::TickDestCheck);
            }
            Message::CancelSendReview => {
                self.send_confirming = false;
                self.dest_check_loading = false;
                self.dest_check_error = None;
                self.dest_activated = None;
                self.activation_acknowledged = false;
            }
            Message::TickDestCheck => {
                if self.dest_check_loading {
                    let mut slot = self.dest_check_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.dest_check_loading = false;
                        match outcome {
                            Ok(activated) => self.dest_activated = Some(activated),
                            Err(e) => self.dest_check_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickDestCheck);
                    }
                }
            }
            Message::AcknowledgeActivation(v) => self.activation_acknowledged = v,

            Message::CopyAddress(address) => {
                return iced::clipboard::write(address);
            }

            Message::SendTransaction => {
                self.send_error = None;
                self.send_success = None;

                // Si le destinataire est confirmé comme non activé, l'utilisateur
                // doit avoir explicitement coché la case d'avertissement avant
                // qu'on ne signe/soumette quoi que ce soit.
                if self.dest_activated == Some(false) && !self.activation_acknowledged {
                    self.send_error = Some(
                        "Veuillez cocher la case confirmant l'activation du nouveau compte."
                            .into(),
                    );
                    return Task::none();
                }

                let wallet = match self
                    .selected_unlocked
                    .as_ref()
                    .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
                {
                    Some(w) => w.clone(),
                    None => {
                        self.send_error = Some("Aucun wallet sélectionné.".into());
                        return Task::none();
                    }
                };

                self.sending = true;
                *self.send_result.lock().unwrap() = None;

                let result_slot = Arc::clone(&self.send_result);
                let path_str = wallet.path.to_string_lossy().to_string();
                let wallet_encrypted = wallet.encrypted;
                let password = self.send_password.clone();
                let destination = self.send_destination.clone();
                let amount = self.send_amount.clone();
                let destination_tag = self.send_destination_tag.trim().to_string();
                let network = self.network.as_str().to_string();

                std::thread::spawn(move || {
                    let mut args = vec![
                        "send".to_string(),
                        "-f".to_string(),
                        path_str,
                        "--to".to_string(),
                        destination,
                        "--amount".to_string(),
                        amount,
                        "--network".to_string(),
                        network,
                    ];
                    if !destination_tag.is_empty() {
                        args.push("--destination-tag".to_string());
                        args.push(destination_tag);
                    }

                    // Un wallet non chiffré n'a pas besoin de mot de passe --
                    // on n'envoie `--password-stdin` (et la donnée sur stdin)
                    // que si ce wallet est effectivement chiffré.
                    let stdin_payload = if wallet_encrypted {
                        args.push("--password-stdin".to_string());
                        Some(password)
                    } else {
                        None
                    };

                    let outcome: SendOutcome =
                        run_cli_with_stdin(args, stdin_payload.as_deref()).and_then(|s| {
                            let v: serde_json::Value = serde_json::from_str(&s)
                                .map_err(|_| "Réponse CLI invalide.".to_string())?;
                            v.get("hash")
                                .and_then(|h| h.as_str())
                                .map(str::to_string)
                                .ok_or("Pas de hash dans la réponse.".to_string())
                        });

                    *result_slot.lock().unwrap() = Some(outcome);
                });

                self.send_password.clear();

                return Self::schedule(Message::TickSend)
            }
            Message::TickSend => {
                if self.sending {
                    let mut slot = self.send_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.sending = false;
                        match outcome {
                            Ok(hash) => {
                                self.send_success = Some(format!(
                                    "Transaction envoyée avec succès : {}",
                                    hash
                                ));
                                self.send_destination.clear();
                                self.send_amount.clear();
                                drop(slot);
                                return self.trigger_refresh();
                            }
                            Err(e) => self.send_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickSend);
                    }
                }
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<Message> {
        let base = self.main_view();

        match self.modal {
            Modal::None => base,
            Modal::Create => modal(base, self.create_modal_view(), Message::CloseModal),
            Modal::Load => modal(base, self.load_modal_view(), Message::CloseModal),
            Modal::Send => modal(base, self.send_modal_view(), Message::CloseModal),
        }
    }

    fn main_view(&self) -> Element<Message> {
        let header = row![
            column![
                text("V4X").size(32).color(ACCENT),
                text("WALLET MANAGER").size(12).color(MUTED),
            ]
            .spacing(0),
            iced::widget::horizontal_space(),
            self.network_toggle(),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill);

        let wallet_panel = card("PORTEFEUILLE", self.wallet_top_panel(), Length::Fill);

        let actions_card = card("ACTIONS", self.actions_panel(), Length::Fixed(280.0));
        let info_card = card("SOLDE & TRANSACTIONS", self.info_panel(), Length::Fill);

        let lower = row![actions_card, info_card]
            .spacing(20)
            .align_y(Alignment::Start)
            .width(Length::Fill);

        let content = column![header, wallet_panel, lower]
            .spacing(20)
            .padding(30)
            .width(Length::Fill)
            .max_width(1000);

        container(scrollable(content))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .style(|_theme| container::Style {
                background: Some(Background::Color(PAGE_BG)),
                ..container::Style::default()
            })
            .into()
    }

    /// Switch testnet/mainnet, avec un libellé de chaque côté qui s'éclaire
    /// pour indiquer clairement l'état actif (orange = mainnet = argent réel).
    fn network_toggle(&self) -> Element<Message> {
        let is_mainnet = self.network == NetworkChoice::Mainnet;

        row![
            text("Testnet")
                .size(13)
                .color(if is_mainnet { MUTED } else { ACCENT }),
            toggler(is_mainnet)
                .on_toggle(|v| Message::NetworkChanged(if v {
                    NetworkChoice::Mainnet
                } else {
                    NetworkChoice::Testnet
                }))
                .size(22),
            text("Mainnet")
                .size(13)
                .color(if is_mainnet { WARNING } else { MUTED }),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }

    /// Panneau du haut : sélection/gestion du wallet actif (choisir, créer,
    /// charger) + adresse et bouton de copie du wallet actuellement
    /// sélectionné, le tout sur une largeur pleine et compacte plutôt
    /// qu'étalé verticalement.
    fn wallet_top_panel(&self) -> Element<Message> {
        let names: Vec<String> = self.unlocked_wallets.iter().map(|w| w.name.clone()).collect();

        let controls = row![
            pick_list(names, self.selected_unlocked.clone(), Message::SelectWallet)
                .placeholder("Aucun wallet V4X déverrouillé")
                .width(Length::Fill),
            button(text("Créer").size(14))
                .padding([10, 14])
                .style(primary_button)
                .on_press(Message::OpenCreateModal),
            button(text("Charger").size(14))
                .padding([10, 14])
                .style(secondary_button)
                .on_press(Message::OpenLoadModal),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let selected = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name));

        let address_line: Element<Message> = match selected {
            Some(w) => column![
                row![
                    text("ADRESSE").size(11).color(MUTED),
                    button(text("Copier").size(12))
                        .padding([4, 10])
                        .style(secondary_button)
                        .on_press(Message::CopyAddress(w.address.clone())),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                scrollable(text(w.address.clone()).size(14).color(ACCENT)).width(Length::Fill),
            ]
            .spacing(4)
            .into(),
            None => text("Choisissez, créez ou chargez un wallet pour commencer.")
                .size(12)
                .color(MUTED)
                .into(),
        };

        column![controls, address_line].spacing(14).into()
    }

    /// Actions sur le wallet actif : envoyer, rafraîchir, faucet (testnet).
    fn actions_panel(&self) -> Element<Message> {
        let has_selection = self.selected_unlocked.is_some();

        let mut items: Vec<Element<Message>> = vec![
            button(text("Envoyer").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(has_selection.then_some(Message::OpenSendModal))
                .into(),
            button(text("Rafraîchir").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press_maybe(has_selection.then_some(Message::RefreshInfo))
                .into(),
        ];

        // Le faucet n'existe que sur testnet -- invisible sur mainnet.
        if self.network == NetworkChoice::Testnet {
            items.push(
                button(
                    text(if self.faucet_requesting {
                        "Faucet en cours..."
                    } else {
                        "XRP de test (faucet)"
                    })
                    .size(15),
                )
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press_maybe(
                    (has_selection && !self.faucet_requesting).then_some(Message::RequestFaucet),
                )
                .into(),
            );

            if let Some(msg) = &self.faucet_message {
                items.push(text(msg).size(12).color(SUCCESS).into());
            }
            if let Some(err) = &self.faucet_error {
                items.push(text(err).size(12).color(ERROR).into());
            }
        }

        Column::with_children(items).spacing(12).into()
    }

    fn info_panel(&self) -> Element<Message> {
        let selected = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name));

        let Some(_w) = selected else {
            return text("Aucun wallet sélectionné.").size(13).color(MUTED).into();
        };

        let mut items: Vec<Element<Message>> = Vec::new();

        if self.info_loading {
            items.push(text("Chargement...").size(13).color(MUTED).into());
        } else if let Some(err) = &self.info_error {
            items.push(text(err).size(13).color(ERROR).into());
        } else if let Some(balance) = &self.current_balance {
            if !balance.activated {
                items.push(
                    text("⚠ Compte non activé sur ce réseau (0 XRP reçu).")
                        .size(13)
                        .color(WARNING)
                        .into(),
                );
            } else {
                items.push(info_row("Solde XRP", &balance.xrp_balance));
            }

            items.push(text("Dernières transactions").size(13).color(ACCENT).into());

            if self.current_txs.is_empty() {
                items.push(text("Aucune transaction.").size(13).color(MUTED).into());
            } else {
                for tx in &self.current_txs {
                    items.push(tx_row(tx));
                }
            }
        }

        Column::with_children(items).spacing(12).into()
    }

    fn create_modal_view(&self) -> Element<Message> {
        let mut items: Vec<Element<Message>> = vec![
            text("Créer un Wallet V4X").size(22).color(ACCENT).into(),
            text_input("Nom du wallet", &self.wallet_name_input)
                .on_input(Message::WalletNameChanged)
                .padding(10)
                .into(),
            checkbox(
                format!("Adresse V4X (débute par {})", V4X_PREFIX.to_lowercase()),
                self.use_v4x_address,
            )
            .on_toggle(Message::V4xAddressToggled)
            .into(),
            checkbox("Chiffrer avec un mot de passe", self.use_encryption)
                .on_toggle(Message::EncryptionToggled)
                .into(),
        ];

        if self.use_encryption {
            items.push(
                text_input("Mot de passe", &self.password_input)
                    .on_input(Message::PasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        }

        if self.generating {
            items.push(
                text(format!(
                    "Recherche d'une adresse V4X en cours... Tentatives : {}",
                    self.attempts.load(Ordering::Relaxed)
                ))
                .size(14)
                .color(MUTED)
                .into(),
            );
        }

        if let Some(err) = &self.create_error {
            items.push(text(err).color(ERROR).into());
        }
        if let Some(msg) = &self.create_success {
            items.push(text(msg).color(SUCCESS).into());
        }

        if self.generating {
            items.push(
                button(text("Arrêter la recherche").size(15))
                    .padding(12)
                    .width(Length::Fill)
                    .style(secondary_button)
                    .on_press(Message::CancelGeneration)
                    .into(),
            );
        } else {
            items.push(
                button(text("Générer").size(15))
                    .padding(12)
                    .width(Length::Fill)
                    .style(primary_button)
                    .on_press_maybe(
                        (!self.wallet_name_input.trim().is_empty()).then_some(Message::GenerateWallet),
                    )
                    .into(),
            );
        }

        items.push(
            button(text("Fermer").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press(Message::CloseModal)
                .into(),
        );

        container(Column::with_children(items).spacing(14).width(Length::Fixed(420.0)))
            .padding(24)
            .style(card_style)
            .into()
    }

    fn load_modal_view(&self) -> Element<Message> {
        let mut items: Vec<Element<Message>> =
            vec![text("Charger un Wallet V4X").size(22).color(ACCENT).into()];

        if self.available_wallets.is_empty() {
            items.push(
                text("Aucun wallet trouvé dans le dossier wallets/.")
                    .color(MUTED)
                    .into(),
            );
        } else {
            let names: Vec<String> = self.available_wallets.iter().map(|w| w.name.clone()).collect();
            items.push(
                pick_list(
                    names,
                    self.selected_wallet_file.as_ref().map(|w| w.name.clone()),
                    Message::SelectWalletFile,
                )
                .placeholder("Choisir un wallet")
                .width(Length::Fill)
                .into(),
            );
        }

        let needs_password = self
            .selected_wallet_file
            .as_ref()
            .map(|w| w.encrypted)
            .unwrap_or(false);

        if needs_password {
            items.push(
                text_input("Mot de passe", &self.load_password)
                    .on_input(Message::LoadPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        }

        if self.loading {
            items.push(text("Déchiffrement...").size(13).color(MUTED).into());
        }
        if let Some(err) = &self.load_error {
            items.push(text(err).color(ERROR).into());
        }

        let can_load = self.selected_wallet_file.is_some()
            && (!needs_password || !self.load_password.is_empty())
            && !self.loading;

        items.push(
            button(text("Charger").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_load.then_some(Message::DecryptWallet))
                .into(),
        );
        items.push(
            button(text("Annuler").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press(Message::CloseModal)
                .into(),
        );

        container(Column::with_children(items).spacing(14).width(Length::Fixed(420.0)))
            .padding(24)
            .style(card_style)
            .into()
    }

    fn send_modal_view(&self) -> Element<Message> {
        if self.send_confirming {
            return self.send_confirm_view();
        }

        let wallet_label = self
            .selected_unlocked
            .clone()
            .unwrap_or_else(|| "Aucun wallet sélectionné".to_string());

        let wallet_encrypted = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
            .map(|w| w.encrypted)
            .unwrap_or(false);

        let mut items: Vec<Element<Message>> = vec![
            text("Envoyer des XRP").size(22).color(ACCENT).into(),
            text(format!("Depuis : {} ({})", wallet_label, self.network.as_str()))
                .size(13)
                .color(MUTED)
                .into(),
            text_input("Adresse destinataire (r...)", &self.send_destination)
                .on_input(Message::SendDestinationChanged)
                .padding(10)
                .into(),
            text_input("Montant en XRP", &self.send_amount)
                .on_input(Message::SendAmountChanged)
                .padding(10)
                .into(),
            text_input("Destination tag (optionnel)", &self.send_destination_tag)
                .on_input(Message::SendDestinationTagChanged)
                .padding(10)
                .into(),
        ];

        // Un wallet non chiffré n'a pas de mot de passe -- inutile (et
        // trompeur) de demander à l'utilisateur d'en saisir un.
        if wallet_encrypted {
            items.push(
                text_input("Mot de passe du wallet", &self.send_password)
                    .on_input(Message::SendPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        } else if self.selected_unlocked.is_some() {
            items.push(
                text("Ce wallet n'est pas chiffré : aucun mot de passe requis.")
                    .size(12)
                    .color(MUTED)
                    .into(),
            );
        }

        if self.network == NetworkChoice::Mainnet {
            items.push(
                text("MAINNET -- cette transaction utilisera du XRP réel.")
                    .size(13)
                    .color(WARNING)
                    .into(),
            );
        }

        if let Some(err) = &self.send_error {
            items.push(text(err).color(ERROR).into());
        }
        if let Some(msg) = &self.send_success {
            items.push(text(msg).color(SUCCESS).into());
        }

        let can_review = self.selected_unlocked.is_some() && !self.sending;

        items.push(
            button(text("Vérifier et confirmer").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_review.then_some(Message::ReviewSend))
                .into(),
        );
        items.push(
            button(text("Fermer").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press(Message::CloseModal)
                .into(),
        );

        container(Column::with_children(items).spacing(14).width(Length::Fixed(420.0)))
            .padding(24)
            .style(card_style)
            .into()
    }

    /// Écran de confirmation affiché juste avant de signer/soumettre : récapitule
    /// la transaction pour donner une dernière chance de repérer une erreur de
    /// saisie (montant, adresse) avant qu'elle ne devienne irréversible.
    fn send_confirm_view(&self) -> Element<Message> {
        let wallet_label = self
            .selected_unlocked
            .clone()
            .unwrap_or_else(|| "?".to_string());
        let tag = self.send_destination_tag.trim();

        let mut items: Vec<Element<Message>> = vec![
            text("Confirmer l'envoi").size(22).color(ACCENT).into(),
            text("Vérifiez attentivement avant de continuer -- une transaction XRPL est irréversible.")
                .size(12)
                .color(MUTED)
                .into(),
            owned_info_row("Depuis", wallet_label),
            owned_info_row("Vers", self.send_destination.trim().to_string()),
            owned_info_row("Montant", format!("{} XRP", self.send_amount.trim())),
        ];

        if !tag.is_empty() {
            items.push(owned_info_row("Destination tag", tag.to_string()));
        }
        items.push(info_row("Réseau", self.network.as_str()));

        if self.network == NetworkChoice::Mainnet {
            items.push(
                text("MAINNET -- cette transaction utilisera du XRP réel et est irréversible.")
                    .size(13)
                    .color(WARNING)
                    .into(),
            );
        }

        // --- Statut d'activation du compte destinataire ---
        if self.dest_check_loading {
            items.push(
                text("Vérification du compte destinataire...")
                    .size(13)
                    .color(MUTED)
                    .into(),
            );
        } else if let Some(err) = &self.dest_check_error {
            items.push(
                text(format!(
                    "Impossible de vérifier ce compte ({}). Vérifiez l'adresse avec soin avant de continuer.",
                    err
                ))
                .size(12)
                .color(WARNING)
                .into(),
            );
        } else if self.dest_activated == Some(false) {
            items.push(
                container(
                    column![
                        text("Compte destinataire non activé").size(14).color(WARNING),
                        text(
                            "Cette adresse n'existe pas encore sur le réseau. Cet envoi va \
                             l'ACTIVER en tant que nouveau compte -- assurez-vous que l'adresse \
                             est correcte : un envoi vers une mauvaise adresse est irréversible."
                        )
                        .size(12)
                        .color(MUTED),
                        checkbox(
                            "Je comprends et je souhaite activer ce nouveau compte.",
                            self.activation_acknowledged,
                        )
                        .on_toggle(Message::AcknowledgeActivation),
                    ]
                    .spacing(8),
                )
                .padding(12)
                .style(|_theme| container::Style {
                    background: Some(Background::Color(Color { a: 0.08, ..WARNING })),
                    border: Border {
                        color: WARNING,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..container::Style::default()
                })
                .into(),
            );
        }

        if self.sending {
            items.push(text("Envoi en cours...").size(14).color(MUTED).into());
        }
        if let Some(err) = &self.send_error {
            items.push(text(err).color(ERROR).into());
        }
        if let Some(msg) = &self.send_success {
            items.push(text(msg).color(SUCCESS).into());
        }

        let needs_ack = self.dest_activated == Some(false) && !self.activation_acknowledged;
        let can_send = !self.sending && !self.dest_check_loading && !needs_ack;

        items.push(
            button(text("Confirmer l'envoi").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(if self.network == NetworkChoice::Mainnet {
                    warning_button
                } else {
                    primary_button
                })
                .on_press_maybe(can_send.then_some(Message::SendTransaction))
                .into(),
        );
        items.push(
            button(text("Modifier").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press_maybe((!self.sending).then_some(Message::CancelSendReview))
                .into(),
        );
        items.push(
            button(text("Fermer").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press_maybe((!self.sending).then_some(Message::CloseModal))
                .into(),
        );

        container(Column::with_children(items).spacing(14).width(Length::Fixed(420.0)))
            .padding(24)
            .style(card_style)
            .into()
    }
}

fn info_row<'a>(label: &'a str, value: &'a str) -> Element<'a, Message> {
    column![
        text(label.to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Variante de `info_row` pour une valeur calculée localement (ex: `format!(...)`)
/// -- prend une `String` possédée plutôt qu'une référence, pour éviter tout
/// problème de durée de vie avec une valeur temporaire.
fn owned_info_row(label: &'static str, value: String) -> Element<'static, Message> {
    column![
        text(label.to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Vérification légère (pas une validation base58check complète) pour attraper
/// les fautes de frappe évidentes avant de demander confirmation à l'utilisateur.
fn looks_like_xrpl_address(addr: &str) -> bool {
    let addr = addr.trim();
    addr.starts_with('r')
        && addr.len() >= 25
        && addr.len() <= 35
        && addr.chars().all(|c| c.is_ascii_alphanumeric())
}

fn looks_like_xrp_amount(amount: &str) -> bool {
    let amount = amount.trim();
    if amount.is_empty() {
        return false;
    }
    let mut parts = amount.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    !whole.is_empty()
        && whole.chars().all(|c| c.is_ascii_digit())
        && frac.chars().all(|c| c.is_ascii_digit())
        && frac.len() <= 6
}

fn tx_row(tx: &TxInfo) -> Element<'static, Message> {
    let amount = tx.amount_xrp.clone().unwrap_or_else(|| "-".to_string());
    let date = tx.date.clone().unwrap_or_else(|| "-".to_string());
    let hash_short = if tx.hash.len() > 14 {
        format!("{}…{}", &tx.hash[..8], &tx.hash[tx.hash.len() - 4..])
    } else {
        tx.hash.clone()
    };
    let tag_suffix = tx
        .destination_tag
        .map(|t| format!(" (tag: {})", t))
        .unwrap_or_default();

    column![
        text(format!(
            "{} — {} XRP{}",
            tx.tx_type, amount, tag_suffix
        ))
        .size(13),
        text(format!("{}    {}", date, hash_short)).size(11).color(MUTED),
    ]
    .spacing(2)
    .into()
}

/// Style commun des panneaux ("cartes") : fond sombre légèrement verdâtre,
/// bordure verte discrète, coins arrondis.
fn card_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL_BG)),
        border: Border {
            color: PANEL_BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..container::Style::default()
    }
}

fn card<'a>(title: &'a str, content: Element<'a, Message>, width: Length) -> Element<'a, Message> {
    container(column![text(title).size(13).color(TITLE_COLOR), content].spacing(16))
        .padding(20)
        .width(width)
        .style(card_style)
        .into()
}

fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => ACCENT_HOVER,
        button::Status::Pressed => ACCENT_PRESS,
        button::Status::Disabled => Color { a: 0.3, ..ACCENT },
        button::Status::Active => ACCENT,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: Color::BLACK,
        border: Border {
            radius: 8.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..button::Style::default()
    }
}

/// Variante d'avertissement du bouton principal (fond orange) -- utilisée pour
/// l'action "Envoyer" quand le réseau actif est le mainnet (argent réel).
fn warning_button(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => WARNING_HOVER,
        button::Status::Pressed => WARNING,
        button::Status::Disabled => Color { a: 0.3, ..WARNING },
        button::Status::Active => WARNING,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: Color::BLACK,
        border: Border {
            radius: 8.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..button::Style::default()
    }
}

fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let (border_color, text_color, fill_alpha) = match status {
        button::Status::Hovered => (ACCENT, ACCENT, 0.1),
        button::Status::Pressed => (ACCENT, ACCENT, 0.18),
        button::Status::Disabled => (Color { a: 0.3, ..ACCENT }, Color { a: 0.3, ..ACCENT }, 0.0),
        button::Status::Active => (ACCENT, ACCENT, 0.0),
    };

    button::Style {
        background: Some(Background::Color(Color {
            a: fill_alpha,
            ..ACCENT
        })),
        text_color,
        border: Border {
            radius: 8.0.into(),
            width: 1.5,
            color: border_color,
        },
        ..button::Style::default()
    }
}

/// Superpose `content` par-dessus `base` avec un fond quasi opaque
/// (clic en dehors du contenu = envoie `on_blur`, typiquement pour fermer le modal).
fn modal<'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    on_blur: Message,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            mouse_area(center(opaque(content)).style(|_theme| container::Style {
                background: Some(Background::Color(Color { a: 0.92, ..Color::BLACK })),
                ..container::Style::default()
            }))
            .on_press(on_blur)
        )
    ]
    .into()
}
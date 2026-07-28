//! GUI binary for the V4X Wallet Manager.
//!
//! Independent of the CLI, compiled separately: `cargo run --bin gui`.
//!
//! Security: this process NEVER decrypts a wallet itself. "Loading a
//! wallet", "balance/transactions", and "send" all invoke the `cli` binary
//! as a subprocess. The private key therefore never exists in the GUI's
//! memory -- only, briefly, in the child `cli` process, which terminates
//! right after each operation.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-22

#[path = "../wallet.rs"]
mod wallet;

use iced::widget::{
    button, center, checkbox, column, container, mouse_area, opaque, pick_list, qr_code, row,
    scrollable, stack, text, text_input, toggler, Column,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Size, Task, Theme};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wallet::{Wallet, WalletFile};

// --- "V4X" palette: technological green on a very dark background ---
const ACCENT: Color = Color::from_rgb(0.0, 0.95, 0.35);
const ACCENT_HOVER: Color = Color::from_rgb(0.25, 1.0, 0.55);
const ACCENT_PRESS: Color = Color::from_rgb(0.0, 0.65, 0.25);
const WARNING: Color = Color::from_rgb(1.0, 0.62, 0.0);
const WARNING_HOVER: Color = Color::from_rgb(1.0, 0.75, 0.25);
const SUCCESS: Color = Color::from_rgb(0.25, 0.95, 0.45);
const ERROR: Color = Color::from_rgb(1.0, 0.35, 0.35);
const MUTED: Color = Color::from_rgb(0.55, 0.68, 0.6);
/// Burnt orange used for panel titles ("PORTEFEUILLE", "ACTIONS",
/// "SOLDE & TRANSACTIONS"), to distinguish them from the green accent used
/// elsewhere (addresses, active states, etc).
const TITLE_COLOR: Color = Color::from_rgb(0.80, 0.40, 0.12);
const PAGE_BG: Color = Color::from_rgb(0.02, 0.03, 0.025);
const PANEL_BG: Color = Color::from_rgb(0.05, 0.08, 0.06);
const PANEL_BORDER: Color = Color::from_rgba(0.0, 0.95, 0.35, 0.25);

const V4X_PREFIX: &str = "RV4X";

// --- Donation addresses ---
// Published openly and in plain text on purpose: this project is meant to be
// auditable, and obfuscating these wouldn't stop anyone with the source from
// finding/changing them anyway. If you want these to be verifiable against
// something other than "trust the source code", set the XRPL `Domain` field
// on these accounts and publish a matching `xrp-ledger.toml` on your
// project's official website (see https://xrpl.org/docs/references/xrp-ledger-toml) --
// that lets anyone (or an explorer like Bithomp) confirm these addresses are
// really controlled by you, independently of this binary.
//
// TODO: replace these with your real XRPL addresses before shipping.
const CREATOR_DONATION_ADDRESS: &str = "rB1KuyGCVCbU1KSkAtWmc7giWhu1b3stbp";
const DEV_DONATION_ADDRESS: &str = "rV4XMzMfq9fjfrf6kzdYjWZM8xjSdtsZR";

fn main() -> iced::Result {
    iced::application(MyApp::title, MyApp::update, MyApp::view)
        .window_size(Size::new(860.0, 820.0))
        .centered()
        .theme(MyApp::theme)
        .run()
}

// ============================== CLI subprocess ==============================

fn cli_binary_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cli"));
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let name = if cfg!(windows) { "cli.exe" } else { "cli" };
    dir.join(name)
}

/// Runs the `cli` binary with the given arguments and returns its stdout
/// (JSON). Blocking: only call from a background thread.
fn run_cli(args: Vec<String>) -> Result<String, String> {
    run_cli_with_stdin(args, None)
}

/// Variant of `run_cli` that can pass sensitive data (a password) through
/// the subprocess's stdin pipe rather than as a command-line argument --
/// this avoids it showing up in the process list (`ps`/task manager). The
/// data is written then the pipe is closed immediately: the CLI reads until
/// EOF without ever waiting for keyboard input, so this is neither
/// interactive nor any more blocking than `run_cli` already is.
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
            // `stdin` is dropped here -> the pipe closes -> the CLI sees EOF
            // immediately, it never waits for keyboard input.
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
    CopySeed,
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

/// What to do with the seed once it's been fetched from the `cli` process --
/// set right before the fetch starts, consulted when the result comes back
/// in `Message::TickCopySeed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SeedAction {
    #[default]
    Copy,
    ShowQr,
}

type GenOutcome = Result<(String, Wallet, PathBuf, bool), String>;
/// (address, public key) -- never the private key.
type LoadOutcome = Result<(String, String), String>;
type InfoOutcome = Result<(BalanceInfo, Vec<TxInfo>), String>;
type SendOutcome = Result<String, String>;
type FaucetOutcome = Result<(), String>;

/// A wallet "unlocked" for the session: only its address (never its
/// private key). The password will need to be re-entered to send a
/// transaction.
#[derive(Debug, Clone)]
struct UnlockedWallet {
    name: String,
    address: String,
    path: PathBuf,
    /// True if this wallet's file is encrypted (`*.encrypted.json`) --
    /// determines whether a password is required to send from it.
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

    // --- creation ---
    wallet_name_input: String,
    use_v4x_address: bool,
    use_encryption: bool,
    password_input: String,
    password_confirm_input: String,
    generating: bool,
    attempts: Arc<AtomicU64>,
    cancel_flag: Arc<AtomicBool>,
    gen_result: Arc<Mutex<Option<GenOutcome>>>,
    create_error: Option<String>,
    create_success: Option<String>,

    // --- loading (unlocking, address only) ---
    available_wallets: Vec<WalletFile>,
    selected_wallet_file: Option<WalletFile>,
    load_password: String,
    load_error: Option<String>,
    loading: bool,
    load_result: Arc<Mutex<Option<LoadOutcome>>>,

    // --- session: unlocked wallets (address only) ---
    unlocked_wallets: Vec<UnlockedWallet>,
    selected_unlocked: Option<String>,

    // --- active wallet's balance + transactions ---
    info_loading: bool,
    info_error: Option<String>,
    current_balance: Option<BalanceInfo>,
    current_txs: Vec<TxInfo>,
    info_result: Arc<Mutex<Option<InfoOutcome>>>,

    // --- faucet (testnet only) ---
    faucet_requesting: bool,
    faucet_message: Option<String>,
    faucet_error: Option<String>,
    faucet_result: Arc<Mutex<Option<FaucetOutcome>>>,

    // --- send ---
    send_destination: String,
    send_amount: String,
    send_destination_tag: String,
    send_password: String,
    send_confirming: bool,
    sending: bool,
    send_error: Option<String>,
    send_success: Option<String>,
    send_result: Arc<Mutex<Option<SendOutcome>>>,
    /// Snapshot of the destination/amount/tag taken at the moment the user
    /// confirmed the review step. The confirmation screen renders these
    /// instead of the live `send_*` fields, so that clearing the form after
    /// a successful send (or any other later mutation) can never make the
    /// confirmation screen appear to show an empty destination.
    confirmed_destination: String,
    confirmed_amount: String,
    confirmed_destination_tag: String,

    // --- destination activation check (before sending) ---
    dest_check_loading: bool,
    dest_check_error: Option<String>,
    /// `Some(false)` = the destination account does not exist on the network
    /// yet (never activated) -- the send will therefore create/activate it.
    dest_activated: Option<bool>,
    dest_check_result: Arc<Mutex<Option<Result<bool, String>>>>,
    /// Whether the user checked the box acknowledging they're activating a
    /// new account. Required before the send can be confirmed if
    /// `dest_activated == Some(false)`.
    activation_acknowledged: bool,

    // --- copy seed to clipboard ---
    copy_seed_password: String,
    copy_seed_error: Option<String>,
    copy_seed_success: Option<String>,
    copy_seed_loading: bool,
    /// Holds the seed only transiently, on the handoff from the background
    /// thread to the update loop -- it is never stored anywhere else and is
    /// consumed (copied to the clipboard, then dropped) as soon as it's read.
    copy_seed_result: Arc<Mutex<Option<Result<String, String>>>>,
    /// Which action ("copy" or "show as QR") the current fetch was started
    /// for.
    seed_action: SeedAction,
    /// The seed, rendered as QR data, kept only while the modal displaying
    /// it is open (cleared on close). Note that this visually encodes the
    /// same secret as the seed string itself -- rendering it as an image
    /// rather than text doesn't reduce what's held in memory, only how it's
    /// displayed.
    copy_seed_qr: Option<qr_code::Data>,
}

#[derive(Debug, Clone)]
enum Message {
    OpenCreateModal,
    OpenLoadModal,
    OpenSendModal,
    /// Opens the send modal with the destination pre-filled to one of the
    /// donation addresses above, so the existing send flow (review,
    /// confirmation, activation warning, etc.) is reused as-is.
    OpenDonationSend(&'static str),
    CloseModal,

    NetworkChanged(NetworkChoice),

    WalletNameChanged(String),
    V4xAddressToggled(bool),
    EncryptionToggled(bool),
    PasswordChanged(String),
    PasswordConfirmChanged(String),
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
    PasteDestination,
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
    /// Checks whether the clipboard still holds the value we wrote (given
    /// as the payload), and clears it if so. Scheduled 60 seconds after a
    /// seed copy; does nothing if the user has since copied something else.
    ClipboardAutoClearCheck(String),
    ClipboardAutoClearConfirmed,
    /// No-op, used as the "don't clear" branch of the clipboard read above.
    Noop,

    OpenCopySeedModal,
    CopySeedPasswordChanged(String),
    CopySeedToClipboard,
    ShowSeedQr,
    TickCopySeed,
}

impl MyApp {
    fn title(&self) -> String {
        String::from("V4X Wallet Manager")
    }

    fn theme(_state: &Self) -> Theme {
        Theme::Dark
    }

    /// Schedules a follow-up message after a short delay (used for all
    /// background task polling: generation, loading, balance, send).
    fn schedule(message: Message) -> Task<Message> {
        Task::perform(
            async { tokio::time::sleep(Duration::from_millis(200)).await },
            move |_| message.clone(),
        )
    }

    /// (Re)starts fetching the balance + latest transactions for the
    /// currently selected wallet, on the currently selected network. Only
    /// requires the address -- no password.
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

    /// Starts fetching the currently selected wallet's seed from the `cli`
    /// subprocess (via `--decrypt`), for the given [`SeedAction`]. Shared by
    /// both "copy to clipboard" and "show as QR code", which only differ in
    /// what they do with the seed once `Message::TickCopySeed` receives it.
    fn start_seed_fetch(&mut self, action: SeedAction) -> Task<Message> {
        self.copy_seed_error = None;
        self.copy_seed_success = None;
        self.copy_seed_qr = None;

        let wallet = match self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
        {
            Some(w) => w.clone(),
            None => {
                self.copy_seed_error = Some("Aucun wallet sélectionné.".into());
                return Task::none();
            }
        };

        if wallet.encrypted && self.copy_seed_password.is_empty() {
            self.copy_seed_error = Some("Mot de passe manquant.".into());
            return Task::none();
        }

        self.seed_action = action;
        self.copy_seed_loading = true;
        *self.copy_seed_result.lock().unwrap() = None;

        let result_slot = Arc::clone(&self.copy_seed_result);
        let path_str = wallet.path.to_string_lossy().to_string();
        let wallet_encrypted = wallet.encrypted;
        let password = self.copy_seed_password.clone();

        std::thread::spawn(move || {
            let mut args = vec!["--decrypt".to_string(), "-f".to_string(), path_str];
            let stdin_payload = if wallet_encrypted {
                args.push("--password-stdin".to_string());
                Some(password)
            } else {
                None
            };

            let outcome: Result<String, String> =
                run_cli_with_stdin(args, stdin_payload.as_deref()).and_then(|s| {
                    let v: serde_json::Value = serde_json::from_str(&s)
                        .map_err(|_| "Réponse CLI invalide.".to_string())?;
                    v.get("seed")
                        .and_then(|h| h.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .ok_or("Ce wallet n'a pas de seed (recréez-le).".to_string())
                });

            *result_slot.lock().unwrap() = Some(outcome);
        });

        self.copy_seed_password.clear();

        Self::schedule(Message::TickCopySeed)
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
                    self.password_confirm_input.clear();
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
                    self.confirmed_destination.clear();
                    self.confirmed_amount.clear();
                    self.confirmed_destination_tag.clear();
                }
            }
            Message::OpenDonationSend(address) => {
                self.modal = Modal::Send;
                if !self.sending {
                    self.send_destination = address.to_string();
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
                    self.confirmed_destination.clear();
                    self.confirmed_amount.clear();
                    self.confirmed_destination_tag.clear();
                }
            }
            Message::CloseModal => {
                self.modal = Modal::None;
                self.copy_seed_qr = None;
                self.copy_seed_password.clear();
                self.copy_seed_error = None;
                self.copy_seed_success = None;
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
            Message::PasswordConfirmChanged(s) => self.password_confirm_input = s,

            Message::GenerateWallet => {
                self.create_error = None;
                self.create_success = None;

                let name = self.wallet_name_input.trim().to_string();
                if name.is_empty() {
                    self.create_error = Some("Veuillez entrer un nom pour le wallet.".into());
                    return Task::none();
                }

                // Both password fields must match -- this is the user's only
                // confirmation that they typed the password they meant to,
                // since it's masked as they type.
                if self.use_encryption && self.password_input != self.password_confirm_input {
                    self.create_error = Some("Les mots de passe ne correspondent pas.".into());
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
            Message::PasteDestination => {
                return iced::clipboard::read()
                    .map(|maybe_text| Message::SendDestinationChanged(maybe_text.unwrap_or_default()));
            }
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
                // A password is only required if this wallet is actually
                // encrypted -- an unencrypted wallet can be used to send
                // without a password.
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

                // Freeze the values the user actually typed at confirmation
                // time. The confirmation screen renders from these snapshots
                // rather than the live `send_*` fields, so it can never show
                // a stale/empty destination if the form gets cleared later
                // (e.g. after a successful send) while this screen is still
                // showing the result.
                self.confirmed_destination = self.send_destination.trim().to_string();
                self.confirmed_amount = self.send_amount.trim().to_string();
                self.confirmed_destination_tag = tag_input.to_string();

                // Checks whether the destination account already exists on
                // the network (only needs the address -- no password). Lets
                // us warn the user if this send would activate an account
                // that doesn't exist yet.
                let destination = self.confirmed_destination.clone();
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

            Message::OpenCopySeedModal => {
                self.modal = Modal::CopySeed;
                self.copy_seed_password.clear();
                self.copy_seed_error = None;
                self.copy_seed_success = None;
                self.copy_seed_loading = false;
                self.copy_seed_qr = None;
                *self.copy_seed_result.lock().unwrap() = None;
            }
            Message::CopySeedPasswordChanged(s) => self.copy_seed_password = s,
            Message::CopySeedToClipboard => return self.start_seed_fetch(SeedAction::Copy),
            Message::ShowSeedQr => return self.start_seed_fetch(SeedAction::ShowQr),
            Message::TickCopySeed => {
                if self.copy_seed_loading {
                    let mut slot = self.copy_seed_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.copy_seed_loading = false;
                        drop(slot);
                        match outcome {
                            // The seed only ever exists here, momentarily, as
                            // a local variable -- it's either handed straight
                            // to the clipboard or rendered into a QR image,
                            // and never stored as plain text in any struct
                            // field.
                            Ok(seed) => match self.seed_action {
                                SeedAction::Copy => {
                                    self.copy_seed_success = Some(
                                        "Seed copiée dans le presse-papiers (effacement automatique dans 60s).".into(),
                                    );
                                    let seed_for_clear = seed.clone();
                                    return Task::batch([
                                        iced::clipboard::write(seed),
                                        Task::perform(
                                            async move {
                                                tokio::time::sleep(Duration::from_secs(60)).await;
                                                seed_for_clear
                                            },
                                            Message::ClipboardAutoClearCheck,
                                        ),
                                    ]);
                                }
                                SeedAction::ShowQr => match qr_code::Data::new(&seed) {
                                    Ok(data) => {
                                        self.copy_seed_qr = Some(data);
                                        self.copy_seed_success = Some(
                                            "QR code généré. Fermez cette fenêtre une fois le scan terminé.".into(),
                                        );
                                    }
                                    Err(e) => {
                                        self.copy_seed_error = Some(format!("Erreur QR : {:?}", e))
                                    }
                                },
                            },
                            Err(e) => self.copy_seed_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickCopySeed);
                    }
                }
            }
            Message::ClipboardAutoClearCheck(expected) => {
                return iced::clipboard::read().map(move |current| {
                    if current.as_deref() == Some(expected.as_str()) {
                        Message::ClipboardAutoClearConfirmed
                    } else {
                        // The user copied something else in the meantime --
                        // leave the clipboard alone.
                        Message::Noop
                    }
                });
            }
            Message::ClipboardAutoClearConfirmed => {
                return iced::clipboard::write(String::new());
            }
            Message::Noop => {}

            Message::SendTransaction => {
                self.send_error = None;
                self.send_success = None;

                // If the destination is confirmed as not activated, the user
                // must have explicitly checked the warning box before we
                // sign/submit anything.
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
                let destination = self.confirmed_destination.clone();
                let amount = self.confirmed_amount.clone();
                let destination_tag = self.confirmed_destination_tag.clone();
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

                    // An unencrypted wallet doesn't need a password -- only
                    // send `--password-stdin` (and the stdin payload) if
                    // this wallet is actually encrypted.
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
            Modal::CopySeed => modal(base, self.copy_seed_modal_view(), Message::CloseModal),
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
            self.donation_buttons(),
            self.network_toggle(),
        ]
        .spacing(20)
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

    /// Testnet/mainnet switch, with a label on each side that lights up to
    /// clearly indicate the active state (orange = mainnet = real money).
    /// Small, unobtrusive donation shortcuts -- pre-fill the destination in
    /// the existing send flow rather than introducing a separate one, so
    /// they benefit from the same review/confirmation/activation-warning
    /// steps as any other transaction. Disabled until a wallet is selected,
    /// same as the regular send action.
    fn donation_buttons(&self) -> Element<Message> {
        let has_selection = self.selected_unlocked.is_some();

        row![
            button(text("Soutenir le créateur").size(12))
                .padding([6, 10])
                .style(secondary_button)
                .on_press_maybe(
                    has_selection.then_some(Message::OpenDonationSend(CREATOR_DONATION_ADDRESS))
                ),
            button(text("Soutenir le développeur").size(12))
                .padding([6, 10])
                .style(secondary_button)
                .on_press_maybe(
                    has_selection.then_some(Message::OpenDonationSend(DEV_DONATION_ADDRESS))
                ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }

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

    /// Top panel: selecting/managing the active wallet (choose, create,
    /// load) + the currently selected wallet's address and copy button, all
    /// on a compact full-width bar rather than spread out vertically.
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
                    button(text("Copier la seed").size(12))
                        .padding([4, 10])
                        .style(secondary_button)
                        .on_press(Message::OpenCopySeedModal),
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

    /// Actions on the active wallet: send, refresh, faucet (testnet).
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

        // The faucet only exists on testnet -- invisible on mainnet.
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
            items.push(
                text_input("Confirmer le mot de passe", &self.password_confirm_input)
                    .on_input(Message::PasswordConfirmChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
            if !self.password_confirm_input.is_empty()
                && self.password_input != self.password_confirm_input
            {
                items.push(
                    text("Les mots de passe ne correspondent pas.")
                        .size(12)
                        .color(WARNING)
                        .into(),
                );
            }
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
            let passwords_ok =
                !self.use_encryption || self.password_input == self.password_confirm_input;
            let can_generate = !self.wallet_name_input.trim().is_empty() && passwords_ok;
            items.push(
                button(text("Générer").size(15))
                    .padding(12)
                    .width(Length::Fill)
                    .style(primary_button)
                    .on_press_maybe(can_generate.then_some(Message::GenerateWallet))
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

    /// Password-gated modal for copying a wallet's seed to the clipboard.
    /// The seed itself is never rendered anywhere in this view -- it only
    /// ever exists transiently in memory during the copy operation (see
    /// `Message::TickCopySeed`).
    fn copy_seed_modal_view(&self) -> Element<Message> {
        let wallet_encrypted = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
            .map(|w| w.encrypted)
            .unwrap_or(false);

        let mut items: Vec<Element<Message>> = vec![
            text("Copier la seed").size(22).color(ACCENT).into(),
            text(
                "Cette clé donne un accès total et irrévocable aux fonds de ce wallet. \
                 Ne la partagez avec personne et ne la collez que dans un endroit sûr."
            )
            .size(12)
            .color(WARNING)
            .into(),
        ];

        if wallet_encrypted {
            items.push(
                text_input("Mot de passe du wallet", &self.copy_seed_password)
                    .on_input(Message::CopySeedPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        }

        if self.copy_seed_loading {
            items.push(text("Déchiffrement...").size(13).color(MUTED).into());
        }
        if let Some(err) = &self.copy_seed_error {
            items.push(text(err).color(ERROR).into());
        }
        if let Some(msg) = &self.copy_seed_success {
            items.push(text(msg).color(SUCCESS).into());
        }

        if let Some(data) = &self.copy_seed_qr {
            items.push(
                container(qr_code(data))
                    .padding(12)
                    .width(Length::Fill)
                    .center_x(Length::Fill)
                    .into(),
            );
        }

        let can_act = !self.copy_seed_loading
            && (!wallet_encrypted || !self.copy_seed_password.is_empty());

        items.push(
            button(text("Afficher le QR code").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_act.then_some(Message::ShowSeedQr))
                .into(),
        );
        items.push(
            button(text("Copier la seed dans le presse-papiers").size(15))
                .padding(12)
                .width(Length::Fill)
                .style(warning_button)
                .on_press_maybe(can_act.then_some(Message::CopySeedToClipboard))
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
            row![
                text_input("Adresse destinataire (r...)", &self.send_destination)
                    .on_input(Message::SendDestinationChanged)
                    .padding(10),
                button(text("Coller").size(13))
                    .padding([10, 14])
                    .style(secondary_button)
                    .on_press(Message::PasteDestination),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
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

        // An unencrypted wallet has no password -- pointless (and
        // misleading) to ask the user to enter one.
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

    /// Confirmation screen shown right before signing/submitting:
    /// recaps the transaction to give one last chance to spot a data-entry
    /// error (amount, address) before it becomes irreversible.
    fn send_confirm_view(&self) -> Element<Message> {
        let wallet_label = self
            .selected_unlocked
            .clone()
            .unwrap_or_else(|| "?".to_string());
        let tag = self.confirmed_destination_tag.as_str();

        let mut items: Vec<Element<Message>> = vec![
            text("Confirmer l'envoi").size(22).color(ACCENT).into(),
            text("Vérifiez attentivement avant de continuer -- une transaction XRPL est irréversible.")
                .size(12)
                .color(MUTED)
                .into(),
            owned_info_row("Depuis", wallet_label),
            owned_info_row("Vers", self.confirmed_destination.clone()),
            owned_info_row("Montant", format!("{} XRP", self.confirmed_amount)),
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

        // --- Destination account activation status ---
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

/// Renders a labeled, scrollable value row (e.g. "ADDRESS" / the address
/// value).
fn info_row<'a>(label: &'a str, value: &'a str) -> Element<'a, Message> {
    column![
        text(label.to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Variant of `info_row` for a locally computed value (e.g. `format!(...)`)
/// -- takes an owned `String` rather than a reference, to avoid any lifetime
/// issue with a temporary value.
fn owned_info_row(label: &'static str, value: String) -> Element<'static, Message> {
    column![
        text(label.to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Lightweight check (not a full Base58Check validation) to catch obvious
/// typos before asking the user for confirmation.
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

/// Common style for panels ("cards"): slightly greenish dark background,
/// subtle green border, rounded corners.
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

/// Wraps `content` in a titled panel ("card") of the given width.
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

/// Warning variant of the primary button (orange background) -- used for the
/// "Send" action when the active network is mainnet (real money).
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

/// Overlays `content` on top of `base` with a near-opaque background
/// (clicking outside the content sends `on_blur`, typically to close the modal).
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
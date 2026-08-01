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
#[path = "../i18n.rs"]
mod i18n;
#[path = "../ui.rs"]
mod ui;

use iced::widget::{
    button, checkbox, column, container, pick_list, qr_code, row, scrollable, text, text_input,
    toggler, Column,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Size, Task, Theme};
use plotters::prelude::*;
use plotters_iced::{Chart, ChartWidget};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use i18n::{t, t_args, Lang};
use ui::{
    card, card_style, info_row, modal, owned_info_row, primary_button, secondary_button, tab_bar,
    warning_button, ACCENT, ERROR, MUTED, PAGE_BG, SUCCESS, WARNING,
};
use wallet::{Wallet, WalletFile};

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

/// A top-level tab in the main window. Adding a new one:
/// 1. Add a variant here.
/// 2. Add its `(Tab, i18n_key)` entry to the `TABS` list in
///    [`MyApp::main_view`], where `ui::tab_bar` is called.
/// 3. Add a matching arm in [`MyApp::main_view`] that renders its content
///    (typically a small `xxx_tab_view(&self) -> Element<Message>` method,
///    following `wallet_tab_view`/`trade_tab_view` as examples).
/// That's it -- the tab bar, selection state, and switching logic are all
/// generic over this enum and don't need to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Wallet,
    Trade,
}

/// An XRPL issued-currency token, as offered in the Trade tab's token
/// picker. Currently hand-written placeholder data (see
/// [`dummy_trade_tokens`]) -- once token discovery is wired up (e.g. via a
/// DEX/order-book query or a curated token list), this becomes the
/// deserialization target for that instead.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TradeToken {
    /// Currency code, e.g. `"USD"`, `"EUR"`. Not validated here (a real
    /// XRPL currency code can be a standard 3-letter ISO code or a 40-char
    /// hex code) since this is placeholder data.
    currency: String,
    /// Human-readable issuer name, shown alongside the currency to
    /// disambiguate (the same currency code can be issued by many
    /// different accounts on the XRPL, each a distinct, non-fungible
    /// trust line).
    issuer_label: String,
}

impl std::fmt::Display for TradeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.currency, self.issuer_label)
    }
}

/// Placeholder token list for the Trade tab, standing in for real token
/// discovery (to be added later). Modeled after commonly-traded XRPL
/// issued currencies so the UI looks realistic in the meantime.
fn dummy_trade_tokens() -> Vec<TradeToken> {
    vec![
        TradeToken { currency: "USD".into(), issuer_label: "Bitstamp".into() },
        TradeToken { currency: "USD".into(), issuer_label: "GateHub".into() },
        TradeToken { currency: "EUR".into(), issuer_label: "GateHub".into() },
        TradeToken { currency: "BTC".into(), issuer_label: "Bitstamp".into() },
    ]
}

/// OHLC/candlestick price chart for the Trade tab, rendered via
/// `plotters`/`plotters-iced`. Currently draws just the axes with no data
/// series -- once historical candle data is available, `build_chart` is
/// where a `plotters::series::CandleStick` series gets added, fed from a
/// `Vec` of OHLC points held on `MyApp` (refreshed the same
/// background-thread + `Tick*` polling way as balance/transactions are).
#[derive(Debug, Clone, Copy, Default)]
struct OhlcChart;

impl Chart<Message> for OhlcChart {
    type State = ();

    fn build_chart<DB: DrawingBackend>(&self, _state: &Self::State, mut builder: ChartBuilder<DB>) {
        // Placeholder axis ranges (0..1) -- once real OHLC data is plotted,
        // these become the actual time range and price min/max of the
        // candles being shown.
        let Ok(mut chart) = builder
            .margin(10)
            .x_label_area_size(28)
            .y_label_area_size(44)
            .build_cartesian_2d(0f32..1f32, 0f32..1f32)
        else {
            return;
        };

        let _ = chart
            .configure_mesh()
            .x_labels(0)
            .y_labels(0)
            .disable_x_mesh()
            .disable_y_mesh()
            .draw();
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
/// (trust-line balances, NFTs) -- fetched by the `tokens` CLI command,
/// alongside (but independently of) balance/transactions: see
/// `trigger_refresh`.
type TokensOutcome = Result<(Vec<TokenInfo>, Vec<NftInfo>), String>;

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

/// One trust line (issued-currency balance), as returned by the CLI's
/// `tokens` command -- mirrors `network::TokenBalance`.
#[derive(Debug, Clone, Deserialize)]
struct TokenInfo {
    currency: String,
    issuer: String,
    balance: String,
    is_negative: bool,
}

/// One NFT, as returned by the CLI's `tokens` command -- mirrors
/// `network::NftInfo`. `uri_hex` is kept raw (undecoded) here too, same
/// reasoning as in `network.rs`.
#[derive(Debug, Clone, Deserialize)]
struct NftInfo {
    nft_id: String,
    issuer: String,
    taxon: u32,
    serial: Option<u32>,
    #[allow(dead_code)] // not yet rendered -- kept for a future "view metadata" action
    uri_hex: Option<String>,
}

/// Shape of the CLI's `tokens` command JSON output (`{"tokens": [...],
/// "nfts": [...]}`), deserialized in one shot then split into the two
/// vectors the rest of the app deals with.
#[derive(Debug, Deserialize)]
struct TokensResponse {
    tokens: Vec<TokenInfo>,
    nfts: Vec<NftInfo>,
}

#[derive(Default)]
struct MyApp {
    modal: Modal,
    network: NetworkChoice,
    lang: Lang,
    active_tab: Tab,

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

    // --- active wallet's tokens (trust lines) + NFTs -- fetched alongside
    // balance/transactions by the same `trigger_refresh` call, but tracked
    // independently so a failure here never blanks out the balance panel
    // (or vice versa).
    tokens_loading: bool,
    tokens_error: Option<String>,
    current_tokens: Vec<TokenInfo>,
    current_nfts: Vec<NftInfo>,
    tokens_result: Arc<Mutex<Option<TokensOutcome>>>,

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

    // --- trade tab ---
    selected_trade_token: Option<TradeToken>,
    ohlc_chart: OhlcChart,
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
    LanguageChanged(Lang),
    TabSelected(Tab),

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
    TickTokens,

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

    TradeTokenSelected(TradeToken),
    TradeBuy,
    TradeSell,
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

    /// (Re)starts fetching the balance, latest transactions, and
    /// tokens/NFTs for the currently selected wallet, on the currently
    /// selected network. Only requires the address -- no password. All
    /// three are fetched from the same background thread (one "refresh"
    /// action), but tracked as two independent outcomes (`info_result` for
    /// balance/tx, `tokens_result` for tokens/NFTs) so a failure fetching
    /// one never blanks out a successful fetch of the other.
    fn trigger_refresh(&mut self) -> Task<Message> {
        self.info_error = None;
        self.tokens_error = None;

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

        self.tokens_loading = true;
        self.current_tokens.clear();
        self.current_nfts.clear();
        *self.tokens_result.lock().unwrap() = None;

        let result_slot = Arc::clone(&self.info_result);
        let tokens_result_slot = Arc::clone(&self.tokens_result);
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
                        address.clone(),
                        "--network".into(),
                        network.clone(),
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

            // Tokens/NFTs -- same address/network, same background thread,
            // deliberately independent outcome (see doc comment above).
            let tokens_outcome: TokensOutcome = run_cli(vec![
                "tokens".into(),
                "--address".into(),
                address,
                "--network".into(),
                network,
            ])
            .and_then(|s| {
                serde_json::from_str::<TokensResponse>(&s)
                    .map(|r| (r.tokens, r.nfts))
                    .map_err(|_| "Réponse tokens invalide.".to_string())
            });

            *tokens_result_slot.lock().unwrap() = Some(tokens_outcome);
        });

        Task::batch([Self::schedule(Message::TickInfo), Self::schedule(Message::TickTokens)])
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
                self.copy_seed_error = Some(t(self.lang, "wallet.select_required"));
                return Task::none();
            }
        };

        if wallet.encrypted && self.copy_seed_password.is_empty() {
            self.copy_seed_error = Some(t(self.lang, "wallet.password_required"));
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
            Message::LanguageChanged(lang) => self.lang = lang,
            Message::TabSelected(tab) => self.active_tab = tab,

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
                    self.create_error = Some(t(self.lang, "create.name_required"));
                    return Task::none();
                }

                // Both password fields must match -- this is the user's only
                // confirmation that they typed the password they meant to,
                // since it's masked as they type.
                if self.use_encryption && self.password_input != self.password_confirm_input {
                    self.create_error = Some(t(self.lang, "create.password_mismatch"));
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
                                    Some(t_args(self.lang, "create.success", &[("name", &name)]));
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
            Message::TickTokens => {
                if self.tokens_loading {
                    let mut slot = self.tokens_result.lock().unwrap();
                    if let Some(outcome) = slot.take() {
                        self.tokens_loading = false;
                        match outcome {
                            Ok((tokens, nfts)) => {
                                self.current_tokens = tokens;
                                self.current_nfts = nfts;
                            }
                            Err(e) => self.tokens_error = Some(e),
                        }
                    } else {
                        drop(slot);
                        return Self::schedule(Message::TickTokens);
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
                        self.faucet_error = Some(t(self.lang, "wallet.select_required"));
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
                                self.faucet_message = Some(t(self.lang, "faucet.success"));
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
                        self.send_error = Some(t(self.lang, "wallet.select_required"));
                        return Task::none();
                    }
                };
                if !looks_like_xrpl_address(&self.send_destination) {
                    self.send_error = Some(t(self.lang, "send.address_invalid"));
                    return Task::none();
                }
                if !looks_like_xrp_amount(&self.send_amount) {
                    self.send_error = Some(t(self.lang, "send.amount_invalid"));
                    return Task::none();
                }
                let tag_input = self.send_destination_tag.trim();
                if !tag_input.is_empty() && tag_input.parse::<u32>().is_err() {
                    self.send_error = Some(t(self.lang, "send.tag_invalid"));
                    return Task::none();
                }
                // A password is only required if this wallet is actually
                // encrypted -- an unencrypted wallet can be used to send
                // without a password.
                if wallet_encrypted && self.send_password.is_empty() {
                    self.send_error = Some(t(self.lang, "wallet.password_required"));
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
                                    self.copy_seed_success = Some(t(self.lang, "copyseed.copied"));
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
                                        self.copy_seed_success = Some(t(self.lang, "copyseed.qr_generated"));
                                    }
                                    Err(e) => {
                                        self.copy_seed_error = Some(t_args(
                                            self.lang,
                                            "qr.error",
                                            &[("error", &format!("{:?}", e))],
                                        ))
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
                    self.send_error = Some(t(self.lang, "send.activation_ack_required"));
                    return Task::none();
                }

                let wallet = match self
                    .selected_unlocked
                    .as_ref()
                    .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
                {
                    Some(w) => w.clone(),
                    None => {
                        self.send_error = Some(t(self.lang, "wallet.select_required"));
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
                                self.send_success =
                                    Some(t_args(self.lang, "send.success", &[("hash", &hash)]));
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

            Message::TradeTokenSelected(token) => self.selected_trade_token = Some(token),
            // Both are no-ops for now: order construction/submission gets
            // wired up once the DEX/trading backend exists. Kept as
            // separate messages (rather than one combined "TradeSubmit")
            // since buy/sell will need different order sides once real.
            Message::TradeBuy => {}
            Message::TradeSell => {}
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
                text(t(self.lang, "header.title")).size(32).color(ACCENT),
                text(t(self.lang, "header.subtitle")).size(12).color(MUTED),
            ]
            .spacing(0),
            iced::widget::horizontal_space(),
            self.donation_buttons(),
            pick_list(Lang::all().to_vec(), Some(self.lang), Message::LanguageChanged).text_size(13),
            self.network_toggle(),
        ]
        .spacing(20)
        .align_y(Alignment::Center)
        .width(Length::Fill);

        const TABS: &[(Tab, &str)] = &[(Tab::Wallet, "tabs.wallet"), (Tab::Trade, "tabs.trade")];
        let tab_bar = tab_bar(
            TABS.iter().map(|&(tab, key)| (tab, t(self.lang, key))),
            self.active_tab,
            Message::TabSelected,
        );

        let tab_content: Element<Message> = match self.active_tab {
            Tab::Wallet => self.wallet_tab_view(),
            Tab::Trade => self.trade_tab_view(),
        };

        let content = column![header, tab_bar, tab_content]
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

    /// Content of the "Wallet" tab: the original wallet management UI
    /// (top wallet picker/address bar, actions panel, balance/tx panel).
    fn wallet_tab_view(&self) -> Element<Message> {
        let wallet_panel = card(t(self.lang, "panel.wallet"), self.wallet_top_panel(), Length::Fill);

        let actions_card = card(t(self.lang, "panel.actions"), self.actions_panel(), Length::Fixed(280.0));
        let tokens_card = card(t(self.lang, "panel.tokens"), self.tokens_panel(), Length::Fixed(280.0));
        // Actions + tokens/NFTs stacked in the same fixed-width left
        // column, directly under one another; balance/tx panel fills the
        // remaining width to the right, same as before.
        let left_column = column![actions_card, tokens_card].spacing(20);

        let info_card = card(t(self.lang, "panel.balance"), self.info_panel(), Length::Fill);

        let lower = row![left_column, info_card]
            .spacing(20)
            .align_y(Alignment::Start)
            .width(Length::Fill);

        column![wallet_panel, lower].spacing(20).width(Length::Fill).into()
    }

    /// Content of the "Trade" tab. Empty placeholder for now -- next
    /// features (order book, swap, etc.) get added here.
    fn trade_tab_view(&self) -> Element<Message> {
        let tokens = dummy_trade_tokens();

        let token_picker = pick_list(
            tokens,
            self.selected_trade_token.clone(),
            Message::TradeTokenSelected,
        )
        .placeholder(t(self.lang, "trade.token_placeholder"))
        .width(Length::Fixed(260.0));

        let token_row = row![
            text(t(self.lang, "trade.token_label")).size(12).color(MUTED),
            token_picker,
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        // Placeholder empty chart -- see `OhlcChart::build_chart` for where
        // a real candlestick series gets added once historical OHLC data is
        // available.
        let chart = container(
            ChartWidget::new(self.ohlc_chart)
                .width(Length::Fill)
                .height(Length::Fixed(320.0)),
        )
        .padding(4)
        .width(Length::Fill)
        .style(card_style);

        let has_token = self.selected_trade_token.is_some();
        let buy_sell_row = row![
            button(text(t(self.lang, "trade.buy")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(has_token.then_some(Message::TradeBuy)),
            button(text(t(self.lang, "trade.sell")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(warning_button)
                .on_press_maybe(has_token.then_some(Message::TradeSell)),
        ]
        .spacing(14);

        let body: Element<Message> =
            column![token_row, chart, buy_sell_row].spacing(16).into();

        card(t(self.lang, "tabs.trade"), body, Length::Fill)
    }

    /// Small, unobtrusive donation shortcuts -- pre-fill the destination in
    /// the existing send flow rather than introducing a separate one, so
    /// they benefit from the same review/confirmation/activation-warning
    /// steps as any other transaction. Disabled until a wallet is selected,
    /// same as the regular send action.
    fn donation_buttons(&self) -> Element<Message> {
        let has_selection = self.selected_unlocked.is_some();

        row![
            button(text(t(self.lang, "donate.creator")).size(12))
                .padding([6, 10])
                .style(secondary_button)
                .on_press_maybe(
                    has_selection.then_some(Message::OpenDonationSend(CREATOR_DONATION_ADDRESS))
                ),
            button(text(t(self.lang, "donate.dev")).size(12))
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

    /// Testnet/mainnet switch, with a label on each side that lights up to
    /// clearly indicate the active state (orange = mainnet = real money).
    fn network_toggle(&self) -> Element<Message> {
        let is_mainnet = self.network == NetworkChoice::Mainnet;

        row![
            text(t(self.lang, "network.testnet"))
                .size(13)
                .color(if is_mainnet { MUTED } else { ACCENT }),
            toggler(is_mainnet)
                .on_toggle(|v| Message::NetworkChanged(if v {
                    NetworkChoice::Mainnet
                } else {
                    NetworkChoice::Testnet
                }))
                .size(22),
            text(t(self.lang, "network.mainnet"))
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
                .placeholder(t(self.lang, "wallet.picker_placeholder"))
                .width(Length::Fill),
            button(text(t(self.lang, "wallet.create")).size(14))
                .padding([10, 14])
                .style(primary_button)
                .on_press(Message::OpenCreateModal),
            button(text(t(self.lang, "wallet.load")).size(14))
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
                    text(t(self.lang, "wallet.address_label")).size(11).color(MUTED),
                    button(text(t(self.lang, "wallet.copy_address")).size(12))
                        .padding([4, 10])
                        .style(secondary_button)
                        .on_press(Message::CopyAddress(w.address.clone())),
                    button(text(t(self.lang, "wallet.copy_seed")).size(12))
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
            None => text(t(self.lang, "wallet.empty_hint"))
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
            button(text(t(self.lang, "actions.send")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(has_selection.then_some(Message::OpenSendModal))
                .into(),
            button(text(t(self.lang, "actions.refresh")).size(15))
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
                        t(self.lang, "actions.faucet_loading")
                    } else {
                        t(self.lang, "actions.faucet")
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
            return text(t(self.lang, "balance.none_selected")).size(13).color(MUTED).into();
        };

        let mut items: Vec<Element<Message>> = Vec::new();

        if self.info_loading {
            items.push(text(t(self.lang, "balance.loading")).size(13).color(MUTED).into());
        } else if let Some(err) = &self.info_error {
            items.push(text(err).size(13).color(ERROR).into());
        } else if let Some(balance) = &self.current_balance {
            if !balance.activated {
                items.push(
                    text(t(self.lang, "balance.not_activated"))
                        .size(13)
                        .color(WARNING)
                        .into(),
                );
            } else {
                items.push(info_row(t(self.lang, "balance.label"), &balance.xrp_balance));
            }

            items.push(text(t(self.lang, "balance.tx_title")).size(13).color(ACCENT).into());

            if self.current_txs.is_empty() {
                items.push(text(t(self.lang, "balance.tx_none")).size(13).color(MUTED).into());
            } else {
                for tx in &self.current_txs {
                    items.push(tx_row(tx, self.lang));
                }
            }
        }

        Column::with_children(items).spacing(12).into()
    }

    /// Trust-line (token) balances and NFTs for the active wallet -- same
    /// panel structure as `info_panel`, populated by the same
    /// `trigger_refresh` call (see its doc comment) but tracked via its own
    /// loading/error state so this panel's failure never blanks out the
    /// balance/transactions panel next to it, or vice versa.
    fn tokens_panel(&self) -> Element<Message> {
        let selected = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name));

        let Some(_w) = selected else {
            return text(t(self.lang, "tokens.none_selected")).size(13).color(MUTED).into();
        };

        let mut items: Vec<Element<Message>> = Vec::new();

        if self.tokens_loading {
            items.push(text(t(self.lang, "tokens.loading")).size(13).color(MUTED).into());
        } else if let Some(err) = &self.tokens_error {
            items.push(text(err).size(13).color(ERROR).into());
        } else {
            items.push(text(t(self.lang, "tokens.title")).size(13).color(ACCENT).into());

            if self.current_tokens.is_empty() {
                items.push(text(t(self.lang, "tokens.none")).size(13).color(MUTED).into());
            } else {
                for token in &self.current_tokens {
                    items.push(token_row(token));
                }
            }

            items.push(text(t(self.lang, "nfts.title")).size(13).color(ACCENT).into());

            if self.current_nfts.is_empty() {
                items.push(text(t(self.lang, "nfts.none")).size(13).color(MUTED).into());
            } else {
                for nft in &self.current_nfts {
                    items.push(nft_row(nft, self.lang));
                }
            }
        }

        Column::with_children(items).spacing(12).into()
    }

    fn create_modal_view(&self) -> Element<Message> {
        let name_placeholder = t(self.lang, "create.name_placeholder");
        let password_placeholder = t(self.lang, "common.password_placeholder");
        let password_confirm_placeholder = t(self.lang, "create.password_confirm_placeholder");

        let mut items: Vec<Element<Message>> = vec![
            text(t(self.lang, "create.title")).size(22).color(ACCENT).into(),
            text_input(&name_placeholder, &self.wallet_name_input)
                .on_input(Message::WalletNameChanged)
                .padding(10)
                .into(),
            checkbox(
                t_args(self.lang, "create.v4x_checkbox", &[("prefix", &V4X_PREFIX.to_lowercase())]),
                self.use_v4x_address,
            )
            .on_toggle(Message::V4xAddressToggled)
            .into(),
            checkbox(t(self.lang, "create.encrypt_checkbox"), self.use_encryption)
                .on_toggle(Message::EncryptionToggled)
                .into(),
        ];

        if self.use_encryption {
            items.push(
                text_input(&password_placeholder, &self.password_input)
                    .on_input(Message::PasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
            items.push(
                text_input(&password_confirm_placeholder, &self.password_confirm_input)
                    .on_input(Message::PasswordConfirmChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
            if !self.password_confirm_input.is_empty()
                && self.password_input != self.password_confirm_input
            {
                items.push(
                    text(t(self.lang, "create.password_mismatch"))
                        .size(12)
                        .color(WARNING)
                        .into(),
                );
            }
        }

        if self.generating {
            items.push(
                text(t_args(
                    self.lang,
                    "create.searching",
                    &[("attempts", &self.attempts.load(Ordering::Relaxed).to_string())],
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
                button(text(t(self.lang, "create.stop_search")).size(15))
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
                button(text(t(self.lang, "create.generate")).size(15))
                    .padding(12)
                    .width(Length::Fill)
                    .style(primary_button)
                    .on_press_maybe(can_generate.then_some(Message::GenerateWallet))
                    .into(),
            );
        }

        items.push(
            button(text(t(self.lang, "common.close")).size(15))
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
            vec![text(t(self.lang, "load.title")).size(22).color(ACCENT).into()];

        if self.available_wallets.is_empty() {
            items.push(
                text(t(self.lang, "load.none_found"))
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
                .placeholder(t(self.lang, "load.picker_placeholder"))
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
            let password_placeholder = t(self.lang, "common.password_placeholder");
            items.push(
                text_input(&password_placeholder, &self.load_password)
                    .on_input(Message::LoadPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        }

        if self.loading {
            items.push(text(t(self.lang, "common.decrypting")).size(13).color(MUTED).into());
        }
        if let Some(err) = &self.load_error {
            items.push(text(err).color(ERROR).into());
        }

        let can_load = self.selected_wallet_file.is_some()
            && (!needs_password || !self.load_password.is_empty())
            && !self.loading;

        items.push(
            button(text(t(self.lang, "wallet.load")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_load.then_some(Message::DecryptWallet))
                .into(),
        );
        items.push(
            button(text(t(self.lang, "common.cancel")).size(15))
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
            text(t(self.lang, "copyseed.title")).size(22).color(ACCENT).into(),
            text(t(self.lang, "copyseed.warning"))
                .size(12)
                .color(WARNING)
                .into(),
        ];

        if wallet_encrypted {
            let password_placeholder = t(self.lang, "common.wallet_password_placeholder");
            items.push(
                text_input(&password_placeholder, &self.copy_seed_password)
                    .on_input(Message::CopySeedPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        }

        if self.copy_seed_loading {
            items.push(text(t(self.lang, "common.decrypting")).size(13).color(MUTED).into());
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
            button(text(t(self.lang, "copyseed.show_qr")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_act.then_some(Message::ShowSeedQr))
                .into(),
        );
        items.push(
            button(text(t(self.lang, "copyseed.copy_button")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(warning_button)
                .on_press_maybe(can_act.then_some(Message::CopySeedToClipboard))
                .into(),
        );
        items.push(
            button(text(t(self.lang, "common.close")).size(15))
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
            .unwrap_or_else(|| t(self.lang, "wallet.select_required"));

        let wallet_encrypted = self
            .selected_unlocked
            .as_ref()
            .and_then(|name| self.unlocked_wallets.iter().find(|w| &w.name == name))
            .map(|w| w.encrypted)
            .unwrap_or(false);

        let destination_placeholder = t(self.lang, "send.destination_placeholder");
        let amount_placeholder = t(self.lang, "send.amount_placeholder");
        let tag_placeholder = t(self.lang, "send.tag_placeholder");
        let wallet_password_placeholder = t(self.lang, "common.wallet_password_placeholder");

        let mut items: Vec<Element<Message>> = vec![
            text(t(self.lang, "send.title")).size(22).color(ACCENT).into(),
            text(t_args(
                self.lang,
                "send.from_line",
                &[("wallet", &wallet_label), ("network", self.network.as_str())],
            ))
            .size(13)
            .color(MUTED)
            .into(),
            row![
                text_input(&destination_placeholder, &self.send_destination)
                    .on_input(Message::SendDestinationChanged)
                    .padding(10),
                button(text(t(self.lang, "send.paste_button")).size(13))
                    .padding([10, 14])
                    .style(secondary_button)
                    .on_press(Message::PasteDestination),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into(),
            text_input(&amount_placeholder, &self.send_amount)
                .on_input(Message::SendAmountChanged)
                .padding(10)
                .into(),
            text_input(&tag_placeholder, &self.send_destination_tag)
                .on_input(Message::SendDestinationTagChanged)
                .padding(10)
                .into(),
        ];

        // An unencrypted wallet has no password -- pointless (and
        // misleading) to ask the user to enter one.
        if wallet_encrypted {
            items.push(
                text_input(&wallet_password_placeholder, &self.send_password)
                    .on_input(Message::SendPasswordChanged)
                    .secure(true)
                    .padding(10)
                    .into(),
            );
        } else if self.selected_unlocked.is_some() {
            items.push(
                text(t(self.lang, "send.no_password_needed"))
                    .size(12)
                    .color(MUTED)
                    .into(),
            );
        }

        if self.network == NetworkChoice::Mainnet {
            items.push(
                text(t(self.lang, "send.mainnet_warning"))
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
            button(text(t(self.lang, "send.review_button")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(primary_button)
                .on_press_maybe(can_review.then_some(Message::ReviewSend))
                .into(),
        );
        items.push(
            button(text(t(self.lang, "common.close")).size(15))
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
            text(t(self.lang, "confirm.title")).size(22).color(ACCENT).into(),
            text(t(self.lang, "confirm.warning"))
                .size(12)
                .color(MUTED)
                .into(),
            owned_info_row(t(self.lang, "confirm.from_label"), wallet_label),
            owned_info_row(t(self.lang, "confirm.to_label"), self.confirmed_destination.clone()),
            owned_info_row(t(self.lang, "confirm.amount_label"), format!("{} XRP", self.confirmed_amount)),
        ];

        if !tag.is_empty() {
            items.push(owned_info_row(t(self.lang, "confirm.tag_label"), tag.to_string()));
        }
        items.push(info_row(t(self.lang, "confirm.network_label"), self.network.as_str()));

        if self.network == NetworkChoice::Mainnet {
            items.push(
                text(t(self.lang, "confirm.mainnet_warning"))
                    .size(13)
                    .color(WARNING)
                    .into(),
            );
        }

        // --- Destination account activation status ---
        if self.dest_check_loading {
            items.push(
                text(t(self.lang, "confirm.checking_dest"))
                    .size(13)
                    .color(MUTED)
                    .into(),
            );
        } else if let Some(err) = &self.dest_check_error {
            items.push(
                text(t_args(self.lang, "confirm.check_failed", &[("error", err)]))
                    .size(12)
                    .color(WARNING)
                    .into(),
            );
        } else if self.dest_activated == Some(false) {
            items.push(
                container(
                    column![
                        text(t(self.lang, "confirm.not_activated_title")).size(14).color(WARNING),
                        text(t(self.lang, "confirm.not_activated_body"))
                        .size(12)
                        .color(MUTED),
                        checkbox(
                            t(self.lang, "confirm.activation_checkbox"),
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
            items.push(text(t(self.lang, "confirm.sending")).size(14).color(MUTED).into());
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
            button(text(t(self.lang, "confirm.send_button")).size(15))
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
            button(text(t(self.lang, "confirm.edit_button")).size(15))
                .padding(12)
                .width(Length::Fill)
                .style(secondary_button)
                .on_press_maybe((!self.sending).then_some(Message::CancelSendReview))
                .into(),
        );
        items.push(
            button(text(t(self.lang, "common.close")).size(15))
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

fn tx_row(tx: &TxInfo, lang: Lang) -> Element<'static, Message> {
    let amount = tx.amount_xrp.clone().unwrap_or_else(|| "-".to_string());
    let date = tx.date.clone().unwrap_or_else(|| "-".to_string());
    let hash_short = if tx.hash.len() > 14 {
        format!("{}…{}", &tx.hash[..8], &tx.hash[tx.hash.len() - 4..])
    } else {
        tx.hash.clone()
    };
    let tag_label = t(lang, "tx.tag_label");
    let tag_suffix = tx
        .destination_tag
        .map(|tag_value| format!(" ({}: {})", tag_label, tag_value))
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

/// Renders one trust-line balance: amount + currency on top (in the accent
/// color, or the warning color if `is_negative` -- see [`TokenInfo`]),
/// issuer address below in the same small/muted style used for tx hashes.
fn token_row(token: &TokenInfo) -> Element<'static, Message> {
    let amount_color = if token.is_negative { WARNING } else { ACCENT };

    column![
        text(format!("{} {}", token.balance, token.currency))
            .size(13)
            .color(amount_color),
        text(token.issuer.clone()).size(11).color(MUTED),
    ]
    .spacing(2)
    .into()
}

/// Renders one NFT: shortened NFTokenID on top, taxon (and serial, if any)
/// below -- same visual pattern as [`tx_row`]/[`token_row`].
fn nft_row(nft: &NftInfo, lang: Lang) -> Element<'static, Message> {
    let id_short = if nft.nft_id.len() > 14 {
        format!("{}…{}", &nft.nft_id[..8], &nft.nft_id[nft.nft_id.len() - 4..])
    } else {
        nft.nft_id.clone()
    };

    let taxon_label = t(lang, "nfts.taxon_label");
    let serial_suffix = nft
        .serial
        .map(|s| format!("    #{}", s))
        .unwrap_or_default();

    column![
        text(id_short).size(13).color(ACCENT),
        text(format!("{}: {}{}", taxon_label, nft.taxon, serial_suffix))
            .size(11)
            .color(MUTED),
    ]
    .spacing(2)
    .into()
}

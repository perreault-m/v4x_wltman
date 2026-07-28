//! Wallet management for the V4X Wallet Manager.
//!
//! Covers XRPL wallet generation (including vanity address search),
//! at-rest encryption (AES-256-GCM, PBKDF2-derived key), and reading/writing
//! wallet files to disk. Key derivation itself is delegated to `xrpl_mithril`
//! (see [`wallet_from_seed`]): this module only generates entropy, encodes it
//! as a standard XRPL seed, and stores/encrypts the resulting wallet.
//!
//! Shared by both the `cli` and `gui` binaries.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-22

use aes_gcm::AeadCore;
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs;
use directories::ProjectDirs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use xrpl_mithril::wallet::Wallet as MithrilWallet;

/// An XRPL wallet, as stored on disk (encrypted or in plain text).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Wallet {
    /// Classic XRPL address (e.g. `"rN7n7otQDd6FczFgLdSqtcsAUxDkw6fzRH"`).
    pub address: String,
    /// Informational only, currently unused (kept empty). Key derivation is
    /// fully delegated to `xrpl_mithril`, which does not expose raw key
    /// material to callers.
    pub public_key: String,
    /// Informational only, currently unused (kept empty). See `public_key`.
    pub private_key: String,
    /// The XRPL seed (Base58Check-encoded). This is the only field required
    /// -- and used -- for signing: [`crate::network::send_payment`] always
    /// reconstructs the signing wallet from this field via
    /// `xrpl_mithril::wallet::Wallet::from_seed_encoded`.
    #[serde(default)]
    pub seed: Option<String>,
}

/// XRPL version byte for a secp256k1 "family seed" (single byte). This is
/// the classic `"s..."` seed format.
const SECP256K1_SEED_VERSION: u8 = 0x21;

/// Encodes 16 bytes of entropy into a standard XRPL secp256k1 seed
/// (`"s..."`).
fn encode_seed(entropy: &[u8; 16]) -> String {
    bs58::encode(entropy)
        .with_alphabet(bs58::Alphabet::RIPPLE)
        .with_check_version(SECP256K1_SEED_VERSION)
        .into_string()
}

/// Builds a [`Wallet`] from a seed, via `xrpl_mithril::wallet::Wallet::from_seed_encoded`
/// -- the exact same call used later for signing in
/// [`crate::network::send_payment`]. Because address derivation here and
/// signing later both go through the same library call, the displayed
/// address is guaranteed to match the account that actually signs.
fn wallet_from_seed(seed: &str) -> Result<Wallet, String> {
    let mw = MithrilWallet::from_seed_encoded(seed)
        .map_err(|e| format!("Erreur de reconstruction du wallet (xrpl_mithril) : {:?}", e))?;

    Ok(Wallet {
        address: mw.account_id().to_classic_address(),
        public_key: String::new(),
        private_key: String::new(),
        seed: Some(seed.to_string()),
    })
}

/// Generates a new random XRPL wallet (secp256k1). Entropy is generated
/// locally with a CSPRNG, encoded as a standard seed, then immediately
/// reconstructed through `xrpl_mithril`, which performs the actual
/// cryptographic derivation (key, address).
pub fn generate_random_wallet() -> Result<Wallet, String> {
    let mut entropy = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut entropy);
    wallet_from_seed(&encode_seed(&entropy))
}

/// Searches for a vanity wallet whose address starts with one of the given
/// prefixes.
///
/// Returns `Ok(None)` if the search was cancelled, `Err(_)` on a
/// reconstruction error, `Ok(Some(wallet))` on success.
pub fn generate_vanity_wallet(
    prefixes: &[String],
    attempts_counter: Option<Arc<AtomicU64>>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Option<Wallet>, String> {
    let mut attempts: u64 = 0;

    loop {
        if let Some(c) = &cancel {
            if c.load(Ordering::Relaxed) {
                return Ok(None);
            }
        }

        attempts += 1;
        if let Some(counter) = &attempts_counter {
            counter.store(attempts, Ordering::Relaxed);
        }

        let mut entropy = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut entropy);
        let seed = encode_seed(&entropy);

        let mw = match MithrilWallet::from_seed_encoded(&seed) {
            Ok(w) => w,
            // Should essentially never happen for a seed we just generated
            // ourselves in the correct format -- retry rather than aborting
            // the whole search over an edge case.
            Err(_) => continue,
        };
        let address_upper = mw.account_id().to_classic_address().to_uppercase();

        if prefixes.iter().any(|p| address_upper.starts_with(p.as_str())) {
            return wallet_from_seed(&seed).map(Some);
        }
    }
}

/// Returns the directory the current executable lives in (falls back to the
/// current directory if it cannot be determined). Used only as a last-resort
/// fallback for [`wallets_dir`], and to locate wallets from versions of this
/// app that predate the move to a persistent, per-user data directory (see
/// [`migrate_legacy_wallets`]).
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Returns the OS-appropriate, per-user persistent data directory for this
/// app (outside of and independent from wherever the executable happens to
/// be installed), e.g.:
/// - Windows: `C:\Users\<user>\AppData\Local\V4X\V4X Wallet Manager\data`
/// - Linux:   `~/.local/share/v4x-wallet-manager`
/// - macOS:   `~/Library/Application Support/com.V4X.V4X-Wallet-Manager`
///
/// Falls back to [`exe_dir`] if the OS's home/user directories cannot be
/// determined (rare, e.g. some minimal/sandboxed environments).
fn persistent_data_dir() -> PathBuf {
    ProjectDirs::from("com", "V4X", "V4X Wallet Manager")
        .map(|proj_dirs| proj_dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(exe_dir)
}

const PLAIN_SUFFIX: &str = ".json";
const ENCRYPTED_SUFFIX: &str = ".encrypted.json";

/// Returns whether a wallet file is encrypted (`*.encrypted.json`) or in
/// plain text (`*.json`), based on its filename. Lets the caller know
/// whether a password is required before attempting to load it.
pub fn is_encrypted_file(path: &str) -> bool {
    path.ends_with(ENCRYPTED_SUFFIX)
}

/// Returns the `wallets/` directory in the app's persistent per-user data
/// directory, creating it if necessary. This intentionally does NOT live
/// next to the executable: installing an update (which typically replaces
/// or reinstalls the executable's directory) must never be able to wipe out
/// a user's wallets.
///
/// On first use after upgrading from a version of this app that stored
/// wallets next to the executable, any wallet files found there are
/// automatically copied (not moved) into the new location -- see
/// [`migrate_legacy_wallets`].
pub fn wallets_dir() -> PathBuf {
    let dir = persistent_data_dir().join("wallets");
    let _ = fs::create_dir_all(&dir);
    migrate_legacy_wallets(&dir);
    dir
}

/// One-time, non-destructive migration: copies any wallet files found next
/// to the executable (the old storage location, used by versions of this
/// app prior to the move to a persistent per-user directory) into the new
/// persistent `wallets_dir`, skipping any file that already exists at the
/// destination. The original files are left in place (copied, never
/// moved/deleted), so this is always safe to run and safe to run repeatedly.
fn migrate_legacy_wallets(new_dir: &Path) {
    let legacy_dir = exe_dir().join("wallets");
    if legacy_dir == *new_dir {
        // `persistent_data_dir` fell back to `exe_dir` (e.g. OS user
        // directories unavailable) -- old and new locations are the same,
        // nothing to migrate.
        return;
    }

    let Ok(entries) = fs::read_dir(&legacy_dir) else {
        return; // no legacy directory (fresh install, or already fully on Linux/etc.)
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file_name.ends_with(ENCRYPTED_SUFFIX) && !file_name.ends_with(PLAIN_SUFFIX) {
            continue;
        }

        let dest = new_dir.join(file_name);
        if dest.exists() {
            continue; // already migrated (or a wallet of the same name was created at the new location)
        }

        match fs::copy(&path, &dest) {
            Ok(_) => eprintln!(
                "Migration : wallet \"{}\" copié vers le nouvel emplacement persistant.",
                file_name
            ),
            Err(e) => eprintln!(
                "Migration : échec de la copie de \"{}\" ({}).",
                file_name, e
            ),
        }
    }
}

/// Sanitizes a user-supplied wallet name into a safe filename component
/// (alphanumeric, `-`, and `_` only). Falls back to `"wallet"` if the result
/// would be empty. This also prevents path traversal via the wallet name.
pub fn sanitize_wallet_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "wallet".to_string()
    } else {
        cleaned
    }
}

/// A wallet file found on disk, as listed by [`list_wallets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletFile {
    /// Wallet name (filename without its `.json`/`.encrypted.json` suffix).
    pub name: String,
    /// Full path to the file.
    pub path: PathBuf,
    /// Whether the file is encrypted.
    pub encrypted: bool,
}

/// Lists all wallet files found in [`wallets_dir`], sorted by name.
pub fn list_wallets() -> Vec<WalletFile> {
    let dir = wallets_dir();
    let mut result = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            if let Some(stripped) = file_name.strip_suffix(ENCRYPTED_SUFFIX) {
                result.push(WalletFile {
                    name: stripped.to_string(),
                    path: path.clone(),
                    encrypted: true,
                });
            } else if let Some(stripped) = file_name.strip_suffix(PLAIN_SUFFIX) {
                result.push(WalletFile {
                    name: stripped.to_string(),
                    path: path.clone(),
                    encrypted: false,
                });
            }
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// Saves a wallet to disk in plain text (unencrypted JSON).
pub fn save_wallet(wallet: &Wallet, name: &str) -> Result<PathBuf, String> {
    let json = serde_json::to_string_pretty(wallet).map_err(|e| e.to_string())?;
    let path = wallets_dir().join(format!("{}{PLAIN_SUFFIX}", sanitize_wallet_name(name)));
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Encrypts a wallet with a password (AES-256-GCM, PBKDF2-HMAC-SHA256 key
/// derivation) and saves it to disk.
pub fn encrypt_and_save(wallet: &Wallet, name: &str, password: &str) -> Result<PathBuf, String> {
    let json_bytes = serde_json::to_vec_pretty(wallet).map_err(|e| e.to_string())?;

    let mut salt = [0u8; 16];
    ChaCha8Rng::from_entropy().fill_bytes(&mut salt);

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 100_000, &mut key);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, json_bytes.as_slice())
        .map_err(|_| "Échec du chiffrement".to_string())?;

    let encrypted_package = serde_json::json!({
        "salt": hex::encode(salt),
        "nonce": hex::encode(nonce),
        "ciphertext": hex::encode(ciphertext),
        "version": 1
    });

    let path = wallets_dir().join(format!("{}{ENCRYPTED_SUFFIX}", sanitize_wallet_name(name)));
    fs::write(
        &path,
        serde_json::to_string_pretty(&encrypted_package).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    Ok(path)
}

/// Loads a plain-text (unencrypted) wallet file from disk.
pub fn load_plain_wallet(path: &str) -> Result<Wallet, String> {
    let bytes = fs::read(path).map_err(|e| format!("Impossible de lire le fichier : {}", e))?;
    serde_json::from_slice(&bytes).map_err(|_| "JSON invalide".to_string())
}

/// Decrypts an encrypted wallet file and returns its raw JSON content.
pub fn decrypt_wallet_file(path: &str, password: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("Impossible de lire le fichier : {}", e))?;
    let data: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "JSON invalide".to_string())?;

    let salt = hex::decode(data["salt"].as_str().ok_or("Champ 'salt' manquant")?)
        .map_err(|_| "Salt invalide".to_string())?;
    let nonce_bytes = hex::decode(data["nonce"].as_str().ok_or("Champ 'nonce' manquant")?)
        .map_err(|_| "Nonce invalide".to_string())?;
    let ciphertext = hex::decode(
        data["ciphertext"]
            .as_str()
            .ok_or("Champ 'ciphertext' manquant")?,
    )
    .map_err(|_| "Ciphertext invalide".to_string())?;

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 100_000, &mut key);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "Décryptage échoué - mot de passe incorrect ?".to_string())?;

    String::from_utf8(plaintext)
        .map_err(|_| "UTF-8 invalide dans les données déchiffrées".to_string())
}

/// Decrypts an encrypted wallet file and deserializes it into a [`Wallet`].
pub fn decrypt_wallet(path: &str, password: &str) -> Result<Wallet, String> {
    let json = decrypt_wallet_file(path, password)?;
    serde_json::from_str(&json).map_err(|e| format!("JSON invalide après déchiffrement : {}", e))
}

/// Parses a comma-separated list of vanity address prefixes into a
/// normalized (trimmed, uppercased) list.
pub fn parse_prefixes(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}
use aes_gcm::AeadCore;
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use ed25519_dalek::{SigningKey, VerifyingKey};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use ripemd::Ripemd160;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Wallet {
    pub address: String,
    pub public_key: String,
    pub private_key: String,
    /// Seed XRPL standard (format "sEd...", Base58Check). 
    #[serde(default)]
    pub seed: Option<String>,
}

/// Version byte XRPL pour les seeds (0x21) → tous les seeds commencent par "s"
const SEED_VERSION: u8 = 0x21;

/// Encode 16 octets d'entropie en seed XRPL standard ("sEd..." pour Ed25519).
fn encode_seed(entropy: &[u8; 16]) -> String {
    bs58::encode(entropy)
        .with_alphabet(bs58::Alphabet::RIPPLE)
        .with_check_version(SEED_VERSION)
        .into_string()
}

/// Dérive la clé privée ed25519 selon le standard XRPL utilisé par xrpl_mithril :
/// SHA-512(0xED || entropy) → premiers 32 octets.
fn derive_ed25519_key_from_entropy(entropy: &[u8; 16]) -> SigningKey {
    let mut hasher = Sha512::new();
    hasher.update([0xEDu8]);
    hasher.update(entropy);
    let hash = hasher.finalize();

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&hash[..32]);
    SigningKey::from_bytes(&key_bytes)
}

/// Génère un wallet XRPL aléatoire.
fn build_wallet_from_new_entropy() -> Wallet {
    let mut entropy = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut entropy);

    let signing_key = derive_ed25519_key_from_entropy(&entropy);
    let vk = signing_key.verifying_key();

    Wallet {
        address: derive_xrpl_address(&vk),
        public_key: hex::encode_upper(vk.as_bytes()),
        private_key: ed25519_private_key_hex(&signing_key),
        seed: Some(encode_seed(&entropy)),
    }
}

fn ed25519_private_key_hex(signing_key: &SigningKey) -> String {
    let mut bytes = vec![0xEDu8];
    bytes.extend_from_slice(signing_key.as_bytes());
    hex::encode_upper(bytes)
}

pub fn generate_random_wallet() -> Wallet {
    build_wallet_from_new_entropy()
}

/// Recherche un wallet vanity.
pub fn generate_vanity_wallet(
    prefixes: &[String],
    attempts_counter: Option<Arc<AtomicU64>>,
    cancel: Option<Arc<AtomicBool>>,
) -> Option<Wallet> {
    let mut attempts: u64 = 0;

    loop {
        if let Some(c) = &cancel {
            if c.load(Ordering::Relaxed) {
                return None;
            }
        }

        attempts += 1;
        if let Some(counter) = &attempts_counter {
            counter.store(attempts, Ordering::Relaxed);
        }

        let mut entropy = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut entropy);
        let signing_key = derive_ed25519_key_from_entropy(&entropy);
        let vk = signing_key.verifying_key();
        let address = derive_xrpl_address(&vk);
        let address_upper = address.to_uppercase();

        if prefixes.iter().any(|p| address_upper.starts_with(p.as_str())) {
            return Some(Wallet {
                address,
                public_key: hex::encode_upper(vk.as_bytes()),
                private_key: ed25519_private_key_hex(&signing_key),
                seed: Some(encode_seed(&entropy)),
            });
        }
    }
}

pub fn derive_xrpl_address(verifying_key: &VerifyingKey) -> String {
    let mut pubkey = vec![0xED];
    pubkey.extend_from_slice(verifying_key.as_bytes());

    let mut sha = Sha256::new();
    sha.update(&pubkey);
    let hash = sha.finalize();

    let mut rmd = Ripemd160::new();
    rmd.update(hash);
    let account_id = rmd.finalize();

    let mut payload = vec![0x00];
    payload.extend_from_slice(&account_id);

    bs58::encode(payload)
        .with_alphabet(bs58::Alphabet::RIPPLE)
        .with_check()
        .into_string()
}

// ==================== Le reste du fichier reste inchangé ====================

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

const PLAIN_SUFFIX: &str = ".json";
const ENCRYPTED_SUFFIX: &str = ".encrypted.json";

pub fn wallets_dir() -> PathBuf {
    let dir = exe_dir().join("wallets");
    let _ = fs::create_dir_all(&dir);
    dir
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletFile {
    pub name: String,
    pub path: PathBuf,
    pub encrypted: bool,
}

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

pub fn save_wallet(wallet: &Wallet, name: &str) -> Result<PathBuf, String> {
    let json = serde_json::to_string_pretty(wallet).map_err(|e| e.to_string())?;
    let path = wallets_dir().join(format!("{}{PLAIN_SUFFIX}", sanitize_wallet_name(name)));
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

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

pub fn load_plain_wallet(path: &str) -> Result<Wallet, String> {
    let bytes = fs::read(path).map_err(|e| format!("Impossible de lire le fichier : {}", e))?;
    serde_json::from_slice(&bytes).map_err(|_| "JSON invalide".to_string())
}

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

pub fn decrypt_wallet(path: &str, password: &str) -> Result<Wallet, String> {
    let json = decrypt_wallet_file(path, password)?;
    serde_json::from_str(&json).map_err(|e| format!("JSON invalide après déchiffrement : {}", e))
}

pub fn parse_prefixes(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}
# V4X Wallet Manager

A lightweight XRPL (XRP Ledger) wallet manager with both a **CLI** and a **desktop GUI**.

Create, encrypt, load, and use XRPL wallets. Check balances and transaction history, request test XRP from the faucet, and send payments — on **testnet** or **mainnet**.

The design prioritizes security: the GUI never holds private keys in memory. Sensitive operations (decrypt, sign, submit) always run in a short-lived `cli` subprocess that exits immediately afterward.

**Author:** Michael.P for V4X  
**Date:** 2026-07-22

---

## Features

### Wallet management
- Generate a new random XRPL wallet (secp256k1 family seed)
- Optional **vanity address** search (GUI offers a one-click “V4X” prefix starting with `rV4X…`)
- At-rest encryption with **AES-256-GCM** + **PBKDF2-HMAC-SHA256** (100 000 iterations)
- Plain-text JSON storage also supported
- Persistent per-user data directory (survives app updates); automatic one-time migration from the old “next to the executable” location

### Network (read)
- Fetch account balance (XRP + drops)
- Fetch recent transaction history
- Testnet faucet support (free test XRP)
- Multiple public RPC endpoints with automatic failover (especially useful on mainnet when a server is amendment-blocked)

### Network (write)
- Build, autofill, sign and submit XRP payments via `xrpl-mithril`
- Optional destination tag
- Pre-flight server-health check (rejects amendment-blocked nodes)
- Destination activation warning before sending to an unactivated account

### Security model
- GUI process **never** decrypts a wallet itself
- Password preferably passed via stdin (`--password-stdin`) so it never appears in process lists
- Seed / private material lives only inside the short-lived CLI process
- Clipboard auto-clear support for copied seeds (GUI)

### Dual interface
| Interface | Purpose |
|-----------|---------|
| **GUI** (`gui`) | Everyday use — create / load wallets, view balance & history, send, faucet, copy seed / QR |
| **CLI** (`cli`) | Scriptable, machine-readable JSON on stdout, human logs on stderr |

---

## Building

Requires a recent Rust toolchain and the project dependencies (including `xrpl-mithril`, `iced`, `reqwest`, `aes-gcm`, `pbkdf2`, etc.).

```bash
# GUI
cargo run --bin gui

# CLI
cargo run --bin cli -- --help
```

Both binaries are expected to live next to each other so the GUI can locate the `cli` (or `cli.exe` on Windows) automatically.

---

## CLI usage

Output convention (designed for scripting / other languages):

- **stdout** → machine-readable JSON only  
- **stderr** → human-readable progress / logs  
- non-zero exit code on error  

### Generation

```bash
# Random wallet (plain)
cli --name mywallet

# Vanity search + encryption
cli --vanity RV4X,RABC --encrypt "my-secret" --name vanity1
```

### Read-only (public address only — no password needed)

```bash
cli balance --address rN7n7otQDd6FczFgLdSqtcsAUxDkw6fzRH --network testnet
cli transactions --address rN7n7otQDd6FczFgLdSqtcsAUxDkw6fzRH --network mainnet --limit 20
cli faucet --address rN7n7otQDd6FczFgLdSqtcsAUxDkw6fzRH          # testnet only
```

### Send

```bash
# Preferred: password via stdin
echo -n "my-secret" | cli send -f path/to/wallet.encrypted.json \
  --password-stdin \
  --to rDestinationAddress... \
  --amount 1.5 \
  --destination-tag 12345 \
  --network testnet

# Or (less safe) password on the command line
cli send -f path/to/wallet.encrypted.json -p "my-secret" \
  --to rDestinationAddress... --amount 1.5 --network mainnet
```

### Decrypt / inspect

```bash
# Address + public key only (safe)
cli --address -f path/to/wallet.encrypted.json --password-stdin

# Full wallet JSON including seed (use with care)
cli --decrypt -f path/to/wallet.encrypted.json --password-stdin
```

Common options:

| Flag | Description |
|------|-------------|
| `--network testnet\|mainnet` | Defaults to **testnet** for safety |
| `--password-stdin` | Read password from stdin (recommended) |
| `-p` / `--password` | Password as argument (visible in process list) |
| `-h` / `--help` | Full help |

---

## GUI overview

- **Wallet bar** — select / create / load unlocked wallets (address only)
- **Actions** — Send, Refresh, Testnet faucet
- **Balance & history** panel
- **Network toggle** — Testnet ↔ Mainnet (with clear visual warning on mainnet)
- Modals for creation (optional encryption + V4X vanity), loading, sending (with review + activation warning), and seed copy / QR display
- Donation shortcuts that reuse the normal send flow

The UI is currently in French.

---

## Storage locations

Wallets are stored in the OS-appropriate persistent data directory:

| OS | Path |
|----|------|
| Windows | `%LOCALAPPDATA%\V4X\V4X Wallet Manager\data\wallets\` |
| Linux | `~/.local/share/v4x-wallet-manager/wallets/` |
| macOS | `~/Library/Application Support/com.V4X.V4X-Wallet-Manager/wallets/` |

- Plain: `name.json`  
- Encrypted: `name.encrypted.json`  

On first run after an upgrade from an older version that stored wallets next to the executable, existing wallet files are **copied** (never moved or deleted) into the new location.

---

## Networks & RPC endpoints

| Network | RPC candidates (tried in order) | Faucet |
|---------|----------------------------------|--------|
| **Testnet** | `https://s.altnet.rippletest.net:51234/` | Yes |
| **Mainnet** | `https://xrplcluster.com/` → `https://s1.ripple.com:51234/` → `https://s2.ripple.com:51234/` | No |

Mainnet automatically skips servers that are amendment-blocked or unreachable.

---

## Security notes

1. Prefer `--password-stdin` over `-p`.
2. The GUI never decrypts wallets; only the short-lived CLI process does.
3. After a successful send the CLI process exits, dropping the decrypted material.
4. Seed copy in the GUI is password-gated and the seed is held only transiently; clipboard auto-clear is supported.
5. Always double-check addresses and amounts — XRPL transactions are irreversible.
6. On mainnet the GUI shows an explicit “real XRP” warning.

---

## Project layout (relevant files)

```
src/
├── bin/
│   ├── cli.rs      # Command-line binary
│   └── gui.rs      # Desktop GUI (iced)
├── wallet.rs       # Generation, encryption, storage, vanity search
└── network.rs      # JSON-RPC client, balance/tx/faucet/send
```

---

## License

*Add your chosen license here.*

---

## Disclaimer

This software is provided as-is. Use at your own risk. Always verify addresses, amounts, and network selection before sending real XRP. The authors are not responsible for lost funds.

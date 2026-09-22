# DeFi Vault — Yield / Staking Vault on Solana (Devnet)

Final course project: a **yield/staking vault** MVP for Solana Devnet.
The user deposits an SPL token into the vault, receives tokens proportional
to their share, the vault accrues yield over time, and the user can withdraw
their share (principal + accrued yield) at any time.

> Solana **Devnet only**. No real funds or mainnet are used.

## 1. Task

Build a working DeFi application by applying the architecture (PDA, CPI, SPL
Token) and financial mechanics (share-based accounting, yield accrual)
learned during the course.

Chosen mechanic: **Yield / staking vault** (deposit → calculate shares →
yield accrual → withdraw).

## 2. Architecture

### Accounts and PDAs

| Account | Type | Seed | Purpose |
|---|---|---|---|
| `VaultConfig` | PDA | `["vault", mint]` | Vault's global state: admin, mint, total shares, reward rate, limit |
| `vault_authority` | PDA (signer, no data) | `["vault_authority", vault_config]` | Owner of the vault's token account and mint authority of the mint |
| `vault_token_account` | PDA (SPL Token Account) | `["vault_tokens", vault_config]` | Holds all deposited + accrued tokens |
| `UserPosition` | PDA | `["user_position", vault_config, user]` | Stores each user's share amount |

All key accounts are created as **PDAs** — this ensures the state is stored
within the on-chain program itself and cannot be arbitrarily altered by any
external party.

### Instructions

1. **`initialize_vault(reward_rate_per_sec, max_total_deposit)`** — called
   once by the admin only. Creates the vault and its token account.
2. **`deposit(amount)`** — user operation #1. The user transfers their
   tokens to the vault and receives proportional shares.
3. **`withdraw(shares_amount)`** — user operation #2. The user "burns"
   their shares and withdraws the proportional amount of tokens (principal +
   yield).

### CPI (Cross-Program Invocation)

- `deposit` / `withdraw` — call the Token Program's `transfer` instruction
  (between the user and the vault).
- The internal `accrue_rewards()` function — calls the Token Program's
  `mint_to` instruction with the `vault_authority` PDA's signature and adds
  new "yield" tokens to the vault's account.

## 3. Economic model (share-based accounting)

The vault uses a classic ERC-4626-style **share-price** model:

```
first deposit:      shares = amount
subsequent deposits: shares = amount * total_shares / vault_balance
withdraw:             amount_out = shares_amount * vault_balance / total_shares
```

Here `vault_balance` is the current balance of the vault's token account
(deposits + accrued yield). Before every `deposit`/`withdraw` call,
`accrue_rewards()` runs:

```
elapsed = now - last_update_ts
reward  = elapsed * reward_rate_per_sec
```

and this amount is **minted** into the vault's account (`vault_authority` is
the mint authority). As a result, the "price" of each share gradually
increases over time — a simplified simulation standing in for a real yield
source (e.g., lending or LP fees).

**Important:** the mint authority must be fully transferred to the
`vault_authority` PDA (before `initialize_vault`, as a one-time setup step
by the admin) — otherwise no one (including the program itself) would be
able to mint the token without limit.

## 4. Security measures

| Check | Where |
|---|---|
| Signer verification | `user: Signer<'info>` — enforced at the Anchor level |
| Account owner verification | `user_token_account.owner == user.key()` constraint |
| Mint match verification | `has_one = mint`, `user_token_account.mint == mint.key()` |
| Vault token account authenticity | `address = vault_config.vault_token_account` |
| Position ownership | `user_position.owner == user.key()` constraint (in withdraw) |
| Safe arithmetic | All addition/subtraction/multiplication via `checked_*` or `u128` intermediate calculations, returns an error on overflow |
| Deposit limit | `max_total_deposit` — no funds can be deposited beyond the set vault limit |
| Sufficient shares check | in `withdraw`, `user_position.shares >= shares_amount` |
| Zero-amount protection | `amount > 0`, `shares_to_mint > 0`, `amount_out > 0` |

On error, the transaction fully **reverts** (at the Solana runtime level) —
partial execution or loss of funds is not possible.

## 5. Tests (`tests/defi_vault.ts`)

A total of **7** automated tests, of which **2+ are negative scenarios**:

1. Fund test users with tokens
2. Verify the mint authority was correctly transferred to the PDA
3. Initialize the vault
4. ❌ Reject a zero-amount deposit (`ZeroAmount`)
5. ✅ Verify shares are granted 1:1 on the first deposit
6. ❌ Reject an attempt to withdraw shares the user doesn't have (`InsufficientShares`)
7. ❌ Reject an attempt to withdraw another user's position
8. ✅ Correct withdraw — verify shares decrease and balance updates

## 6. Getting Started

### Requirements
- Rust + Solana CLI (>= 1.18)
- Anchor CLI (>= 0.30.1)
- Node.js (>= 18) and Yarn or npm

### Setup

```bash
git clone <repo-url>
cd defi-vault
yarn install   # or: npm install

solana-keygen new -o ~/.config/solana/id.json   # if not already set up
solana config set --url devnet
solana airdrop 2
```

### Build and update the Program ID

```bash
anchor build
anchor keys list
```

Put the resulting Program ID in the following two places:
- `declare_id!("...")` inside `programs/defi_vault/src/lib.rs`
- `[programs.devnet] defi_vault = "..."` in `Anchor.toml`

Then rebuild: `anchor build`

### Running tests (on Devnet)

```bash
anchor test --provider.cluster devnet
```

### Deploy

```bash
anchor deploy --provider.cluster devnet
```

### Frontend

```bash
cd app
# a simple static server, e.g.:
npx serve .
```

`app/index.html` — a simplified demo skeleton (wallet connection flow and UI
states). For a fully working version, connect the Anchor `Program` client
via `target/idl/defi_vault.json` and add the
`program.methods.deposit(...)` / `program.methods.withdraw(...)` calls at
the spots marked `TODO`.

## 7. Program ID and transactions (to be filled in after deployment)

- **Program ID:** `<insert here after deployment>`
- **Initialize tx:** `https://explorer.solana.com/tx/<...>?cluster=devnet`
- **Deposit tx:** `https://explorer.solana.com/tx/<...>?cluster=devnet`
- **Withdraw tx:** `https://explorer.solana.com/tx/<...>?cluster=devnet`
- **Demo video:** `<3-5 minute video link>`

## 8. Known limitations and what would be improved before mainnet

- **Yield source is simulated** (via minting). In a real product, yield
  should come from an actual source (lending interest, LP fees, staking
  rewards); an unlimited mint authority should not exist at all.
- **Reward rate is static** — in a real system it should be dynamic based
  on market conditions, and managed via an oracle or separate governance.
- **Single admin** — currently relies on a single `admin` key; a multisig
  or DAO governance is recommended for mainnet.
- **No pause/emergency-withdraw mechanism** — additional security
  instructions (e.g., `pause_vault`) should be added for emergency
  situations.
- **Not audited** — an independent security audit is mandatory before
  going to mainnet.
- **Frontend is demo-level** — full Anchor client integration and
  real-time balance updates need to be added.

## 9. Project structure

```
defi-vault/
├── Anchor.toml
├── Cargo.toml
├── package.json
├── programs/
│   └── defi_vault/
│       ├── Cargo.toml
│       └── src/lib.rs        # on-chain program
├── tests/
│   └── defi_vault.ts         # 7 automated tests
├── app/
│   └── index.html            # frontend demo
└── README.md
```

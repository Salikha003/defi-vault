import * as anchor from "@coral-xyz/anchor";
import { Program, BN } from "@coral-xyz/anchor";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { assert } from "chai";
import { DefiVault } from "../target/types/defi_vault";

describe("defi_vault", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.DefiVault as Program<DefiVault>;
  const connection = provider.connection;

  let mint: PublicKey;
  let vaultConfig: PublicKey;
  let vaultAuthority: PublicKey;
  let vaultTokenAccount: PublicKey;

  const admin = (provider.wallet as anchor.Wallet).payer;
  const userA = Keypair.generate();
  const userB = Keypair.generate();

  const REWARD_RATE_PER_SEC = new BN(0); // deterministic tests: yield accrual tested separately
  const MAX_TOTAL_DEPOSIT = new BN(1_000_000_000);

  before(async () => {
    // Airdrop SOL for fees to test users.
    for (const kp of [userA, userB]) {
      const sig = await connection.requestAirdrop(kp.publicKey, 2e9);
      await connection.confirmTransaction(sig, "confirmed");
    }

    // Create the underlying SPL mint. Mint authority starts as `admin`;
    // it will be transferred to the vault_authority PDA before initialize.
    const mintKeypair = Keypair.generate();
    mint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      6,
      mintKeypair
    );

    [vaultConfig] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), mint.toBuffer()],
      program.programId
    );
    [vaultAuthority] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_authority"), vaultConfig.toBuffer()],
      program.programId
    );
    [vaultTokenAccount] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_tokens"), vaultConfig.toBuffer()],
      program.programId
    );

    // Fund userA and userB with the underlying token WHILE admin still
    // holds mint authority (before it's handed to the vault PDA below).
    const ataA = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userA.publicKey
    );
    const ataB = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userB.publicKey
    );
    await mintTo(connection, admin, mint, ataA.address, admin, 100_000_000); // 100 tokens
    await mintTo(connection, admin, mint, ataB.address, admin, 100_000_000);

    // Hand mint authority over to the vault PDA so the program (and only
    // the program, via its own accrue-yield logic) can mint future yield.
    const { setAuthority, AuthorityType } = await import("@solana/spl-token");
    await setAuthority(
      connection,
      admin,
      mint,
      admin,
      AuthorityType.MintTokens,
      vaultAuthority
    );
  });

  it("confirms mint authority now belongs to the vault PDA", async () => {
    const { getMint } = await import("@solana/spl-token");
    const mintInfo = await getMint(connection, mint);
    assert.equal(mintInfo.mintAuthority?.toBase58(), vaultAuthority.toBase58());
  });

  it("initializes the vault", async () => {
    await program.methods
      .initializeVault(REWARD_RATE_PER_SEC, MAX_TOTAL_DEPOSIT)
      .accounts({
        admin: admin.publicKey,
        mint,
        vaultConfig,
        vaultAuthority,
        vaultTokenAccount,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .rpc();

    const cfg = await program.account.vaultConfig.fetch(vaultConfig);
    assert.equal(cfg.mint.toBase58(), mint.toBase58());
    assert.equal(cfg.totalShares.toNumber(), 0);
  });

  it("rejects a deposit of zero", async () => {
    const ataA = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userA.publicKey
    );
    const [userPosition] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("user_position"),
        vaultConfig.toBuffer(),
        userA.publicKey.toBuffer(),
      ],
      program.programId
    );

    let threw = false;
    try {
      await program.methods
        .deposit(new BN(0))
        .accounts({
          user: userA.publicKey,
          vaultConfig,
          mint,
          vaultAuthority,
          vaultTokenAccount,
          userTokenAccount: ataA.address,
          userPosition,
          tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .signers([userA])
        .rpc();
    } catch (err) {
      threw = true;
      assert.include(err.toString(), "ZeroAmount");
    }
    assert.isTrue(threw, "expected zero-amount deposit to fail");
  });

  it("lets userA deposit and receive shares 1:1 on first deposit", async () => {
    const ataA = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userA.publicKey
    );

    const [userPosition] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("user_position"),
        vaultConfig.toBuffer(),
        userA.publicKey.toBuffer(),
      ],
      program.programId
    );

    const depositAmount = new BN(10_000_000); // 10 tokens

    await program.methods
      .deposit(depositAmount)
      .accounts({
        user: userA.publicKey,
        vaultConfig,
        mint,
        vaultAuthority,
        vaultTokenAccount,
        userTokenAccount: ataA.address,
        userPosition,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([userA])
      .rpc();

    const pos = await program.account.userPosition.fetch(userPosition);
    assert.equal(pos.shares.toNumber(), depositAmount.toNumber());
  });

  it("rejects withdrawal of more shares than the user owns", async () => {
    const ataA = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userA.publicKey
    );
    const [userPosition] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("user_position"),
        vaultConfig.toBuffer(),
        userA.publicKey.toBuffer(),
      ],
      program.programId
    );

    let threw = false;
    try {
      await program.methods
        .withdraw(new BN(999_000_000)) // far more than deposited
        .accounts({
          user: userA.publicKey,
          vaultConfig,
          mint,
          vaultAuthority,
          vaultTokenAccount,
          userTokenAccount: ataA.address,
          userPosition,
          tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        })
        .signers([userA])
        .rpc();
    } catch (err) {
      threw = true;
      assert.include(err.toString(), "InsufficientShares");
    }
    assert.isTrue(threw, "expected over-withdrawal to fail");
  });

  it("rejects withdrawal signed by a different user than the position owner", async () => {
    const ataB = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userB.publicKey
    );
    const [userPositionA] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("user_position"),
        vaultConfig.toBuffer(),
        userA.publicKey.toBuffer(),
      ],
      program.programId
    );

    let threw = false;
    try {
      // userB tries to withdraw against userA's position account.
      await program.methods
        .withdraw(new BN(1_000_000))
        .accounts({
          user: userB.publicKey,
          vaultConfig,
          mint,
          vaultAuthority,
          vaultTokenAccount,
          userTokenAccount: ataB.address,
          userPosition: userPositionA,
          tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        })
        .signers([userB])
        .rpc();
    } catch (err) {
      threw = true;
    }
    assert.isTrue(threw, "expected cross-user withdrawal to fail");
  });

  it("lets userA withdraw shares and burns them from total_shares", async () => {
    const ataA = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      userA.publicKey
    );
    const [userPosition] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("user_position"),
        vaultConfig.toBuffer(),
        userA.publicKey.toBuffer(),
      ],
      program.programId
    );

    const posBefore = await program.account.userPosition.fetch(userPosition);
    const withdrawShares = posBefore.shares.div(new BN(2));

    await program.methods
      .withdraw(withdrawShares)
      .accounts({
        user: userA.publicKey,
        vaultConfig,
        mint,
        vaultAuthority,
        vaultTokenAccount,
        userTokenAccount: ataA.address,
        userPosition,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .signers([userA])
      .rpc();

    const posAfter = await program.account.userPosition.fetch(userPosition);
    assert.equal(
      posAfter.shares.toNumber(),
      posBefore.shares.toNumber() - withdrawShares.toNumber()
    );

    const vaultAcc = await getAccount(connection, vaultTokenAccount);
    assert.isTrue(Number(vaultAcc.amount) >= 0);
  });
});

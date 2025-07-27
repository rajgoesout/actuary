import { Connection, Keypair } from "@solana/web3.js";
import { initializeKeypair } from "@solana-developers/helpers";
import dotenv from "dotenv";
import {
  createAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  TOKEN_2022_PROGRAM_ID,
  transferChecked,
} from "@solana/spl-token";
import { createNonTransferableMint } from "./create-mint";
// import { createNonTransferableMint } from './create-mint';
dotenv.config();

async function main() {
    /**
     * Create a connection and initialize a keypair if one doesn't already exists.
     * If a keypair exists, airdrop a sol if needed.
     */
    const connection = new Connection("http://127.0.0.1:8899", "confirmed");
    const payer = await initializeKeypair(connection);

    console.log(`public key: ${payer.publicKey.toBase58()}`);

    const mintKeypair = Keypair.generate();
    const mint = mintKeypair.publicKey;
    console.log("\nmint public key: " + mintKeypair.publicKey.toBase58() + "\n\n");

    // CREATE MINT
    const decimals = 9;

    await createNonTransferableMint(connection, payer, mintKeypair, decimals);

    // CREATE SOURCE ACCOUNT AND MINT TOKEN
    // CREATE PAYER ATA AND MINT TOKEN
    console.log("Creating an Associated Token Account...");
    const ata = (
    await getOrCreateAssociatedTokenAccount(
        connection,
        payer,
        mint,
        payer.publicKey,
        undefined,
        undefined,
        undefined,
        TOKEN_2022_PROGRAM_ID,
    )
    ).address;

    console.log("Minting 1 token...");

    const amount = 1 * 10 ** decimals;
    await mintTo(
    connection,
    payer,
    mint,
    ata,
    payer,
    amount,
    [payer],
    { commitment: "finalized" },
    TOKEN_2022_PROGRAM_ID,
    );
    const tokenBalance = await connection.getTokenAccountBalance(ata, "finalized");

    console.log(
    `Account ${ata.toBase58()} now has ${tokenBalance.value.uiAmount} token.`,
    );

    // CREATE DESTINATION ACCOUNT FOR TRANSFER
    console.log("Creating a destination account...\n\n");
    const destinationKeypair = Keypair.generate();
    const destinationAccount = await createAccount(
    connection,
    payer,
    mintKeypair.publicKey,
    destinationKeypair.publicKey,
    undefined,
    { commitment: "finalized" },
    TOKEN_2022_PROGRAM_ID,
    );

    // TRY TRANSFER
    console.log("Attempting to transfer non-transferable mint...");
    try {
        const signature = await transferChecked(
            connection,
            payer,
            ata,
            mint,
            destinationAccount,
            ata,
            amount,
            decimals,
            [destinationKeypair],
            { commitment: "finalized" },
            TOKEN_2022_PROGRAM_ID,
        );
    } catch (e) {
        console.log(
            "This transfer is failing because the mint is non-transferable. Check out the program logs: ",
            (e as any).logs,
            "\n\n",
        );
    }
}

main();

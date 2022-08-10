import { AnchorProvider, Wallet } from '@project-serum/anchor';
import { Connection, Keypair } from '@solana/web3.js';
import fs from 'fs';
import { HealthType } from '../accounts/mangoAccount';
import { MangoClient } from '../client';
import { MANGO_V4_ID } from '../constants';
import { toUiDecimals } from '../utils';

async function debugUser(client, group, mangoAccount) {
  console.log(mangoAccount.toString(group));
  await mangoAccount.reload(client, group);

  console.log(
    'mangoAccount.getEquity() ' +
      toUiDecimals(mangoAccount.getEquity().toNumber()),
  );
  console.log(
    'mangoAccount.getHealth(HealthType.init) ' +
      toUiDecimals(mangoAccount.getHealth(HealthType.init).toNumber()),
  );
  console.log(
    'mangoAccount.getHealthRatio(HealthType.init) ' +
      mangoAccount.getHealthRatio(HealthType.init).toNumber(),
  );
  console.log(
    'mangoAccount.getCollateralValue() ' +
      toUiDecimals(mangoAccount.getCollateralValue().toNumber()),
  );
  console.log(
    'mangoAccount.getAssetsVal() ' +
      toUiDecimals(mangoAccount.getAssetsVal().toNumber()),
  );
  console.log(
    'mangoAccount.getLiabsVal() ' +
      toUiDecimals(mangoAccount.getLiabsVal().toNumber()),
  );

  console.log(
    "mangoAccount.getMaxWithdrawWithBorrowForToken(group, 'SOL') " +
      toUiDecimals(
        (
          await mangoAccount.getMaxWithdrawWithBorrowForToken(group, 'SOL')
        ).toNumber(),
      ),
  );

  console.log(
    "mangoAccount.getMaxSourceForTokenSwap(group, 'USDC', 'BTC') " +
      toUiDecimals(
        (
          await mangoAccount.getMaxSourceForTokenSwap(
            group,
            'USDC',
            'BTC',
            0.94,
          )
        ).toNumber(),
      ),
  );

  console.log(
    'mangoAccount.simHealthWithTokenPositionChanges ' +
      toUiDecimals(
        (
          await mangoAccount.simHealthWithTokenPositionChanges(group, [
            {
              tokenName: 'USDC',
              tokenAmount:
                -20_000 *
                Math.pow(10, group.banksMap.get('BTC')!.mintDecimals!),
            },
            {
              tokenName: 'BTC',
              tokenAmount:
                1 * Math.pow(10, group.banksMap.get('BTC')!.mintDecimals!),
            },
          ])
        ).toNumber(),
      ),
  );
}

async function main() {
  const options = AnchorProvider.defaultOptions();
  const connection = new Connection(process.env.MB_CLUSTER_URL!, options);

  const admin = Keypair.fromSecretKey(
    Buffer.from(
      JSON.parse(fs.readFileSync(process.env.MB_PAYER_KEYPAIR!, 'utf-8')),
    ),
  );
  console.log(`Admin ${admin.publicKey.toBase58()}`);

  const adminWallet = new Wallet(admin);
  const adminProvider = new AnchorProvider(connection, adminWallet, options);
  const client = MangoClient.connect(
    adminProvider,
    'mainnet-beta',
    MANGO_V4_ID['mainnet-beta'],
  );

  const group = await client.getGroupForCreator(admin.publicKey, 0);
  console.log(`${group.toString()}`);

  for (const bank of await group.banksMap.values()) {
    console.log(`${bank.toString()}`);
  }

  for (const keypair of [
    process.env.MB_PAYER_KEYPAIR,
    process.env.MB_USER2_KEYPAIR,
  ]) {
    console.log();
    const user = Keypair.fromSecretKey(
      Buffer.from(JSON.parse(fs.readFileSync(keypair, 'utf-8'))),
    );
    const userWallet = new Wallet(user);
    console.log(`User ${userWallet.publicKey.toBase58()}`);
    const mangoAccount = (
      await client.getMangoAccountsForOwner(group, user.publicKey)
    )[0];
    console.log(`MangoAccount ${mangoAccount.publicKey}`);

    await debugUser(client, group, mangoAccount);
  }

  process.exit();
}

try {
  main();
} catch (error) {
  console.log(error);
}
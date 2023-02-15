use std::{collections::HashSet, sync::Arc, time::Duration, time::Instant};

use crate::MangoClient;
use itertools::Itertools;

use anchor_lang::{__private::bytemuck::cast_ref, solana_program};
use futures::Future;
use mango_v4::state::{EventQueue, EventType, FillEvent, OutEvent, PerpMarket, TokenIndex};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use tokio::time;

// TODO: move instructions into the client proper

pub async fn runner(
    mango_client: Arc<MangoClient>,
    debugging_handle: impl Future,
    interval_update_banks: u64,
    interval_consume_events: u64,
    interval_update_funding: u64,
    interval_check_new_listings_and_abort: u64,
) -> Result<(), anyhow::Error> {
    let handles1 = mango_client
        .context
        .tokens
        .keys()
        // TODO: grouping tokens whose oracle might have less confidencen e.g. ORCA with the rest, fails whole ix
        // TokenUpdateIndexAndRate is known to take max 71k cu
        // from cargo test-bpf local tests
        // chunk size of 8 seems to be max before encountering "VersionedTransaction too large" issues
        .chunks(8)
        .into_iter()
        .map(|chunk| {
            loop_update_index_and_rate(
                mango_client.clone(),
                chunk.copied().collect::<Vec<TokenIndex>>(),
                interval_update_banks,
            )
        })
        .collect::<Vec<_>>();

    let handles2 = mango_client
        .context
        .perp_markets
        .values()
        .filter(|perp|
            // MNGO-PERP-OLD
            perp.market.perp_market_index == 1)
        .map(|perp| {
            loop_consume_events(
                mango_client.clone(),
                perp.address,
                perp.market,
                interval_consume_events,
            )
        })
        .collect::<Vec<_>>();

    let handles3 = mango_client
        .context
        .perp_markets
        .values()
        .filter(|perp|
            // MNGO-PERP-OLD
            perp.market.perp_market_index == 1)
        .map(|perp| {
            loop_update_funding(
                mango_client.clone(),
                perp.address,
                perp.market,
                interval_update_funding,
            )
        })
        .collect::<Vec<_>>();

    futures::join!(
        futures::future::join_all(handles1),
        futures::future::join_all(handles2),
        futures::future::join_all(handles3),
        loop_check_new_listings_and_abort(
            mango_client.clone(),
            interval_check_new_listings_and_abort
        ),
        debugging_handle
    );

    Ok(())
}

pub async fn loop_check_new_listings_and_abort(mango_client: Arc<MangoClient>, interval: u64) {
    let mut interval = time::interval(Duration::from_secs(interval));
    loop {
        if mango_client
            .context
            .new_tokens_listed(&mango_client.client.rpc_async())
            .await
            .unwrap()
            || mango_client
                .context
                .new_perp_markets_listed(&mango_client.client.rpc_async())
                .await
                .unwrap()
        {
            std::process::abort();
        }

        interval.tick().await;
    }
}

pub async fn loop_update_index_and_rate(
    mango_client: Arc<MangoClient>,
    token_indices: Vec<TokenIndex>,
    interval: u64,
) {
    let mut interval = time::interval(Duration::from_secs(interval));
    loop {
        interval.tick().await;

        let client = mango_client.clone();

        let token_indices_clone = token_indices.clone();

        let token_names = token_indices_clone
            .iter()
            .map(|token_index| client.context.token(*token_index).name.to_owned())
            .join(",");

        let mut instructions = vec![ComputeBudgetInstruction::set_compute_unit_price(1)];
        for token_index in token_indices_clone.iter() {
            let token = client.context.token(*token_index);
            let banks_for_a_token = token.mint_info.banks();
            let oracle = token.mint_info.oracle;

            let mut ix = Instruction {
                program_id: mango_v4::id(),
                accounts: anchor_lang::ToAccountMetas::to_account_metas(
                    &mango_v4::accounts::TokenUpdateIndexAndRate {
                        group: token.mint_info.group,
                        mint_info: token.mint_info_address,
                        oracle,
                        instructions: solana_program::sysvar::instructions::id(),
                    },
                    None,
                ),
                data: anchor_lang::InstructionData::data(
                    &mango_v4::instruction::TokenUpdateIndexAndRate {},
                ),
            };
            let mut banks = banks_for_a_token
                .iter()
                .map(|bank_pubkey| AccountMeta {
                    pubkey: *bank_pubkey,
                    is_signer: false,
                    is_writable: true,
                })
                .collect::<Vec<_>>();
            ix.accounts.append(&mut banks);
            instructions.push(ix);
        }
        let pre = Instant::now();
        let sig_result = client
            .send_and_confirm_permissionless_tx(instructions)
            .await;

        if let Err(e) = sig_result {
            log::info!(
                "metricName=UpdateTokensV4Failure tokens={} durationMs={} error={}",
                token_names,
                pre.elapsed().as_millis(),
                e
            );
            log::error!("{:?}", e)
        } else {
            log::info!(
                "metricName=UpdateTokensV4Success tokens={} durationMs={}",
                token_names,
                pre.elapsed().as_millis(),
            );
            log::info!("{:?}", sig_result);
        }
    }
}

pub async fn loop_consume_events(
    mango_client: Arc<MangoClient>,
    pk: Pubkey,
    perp_market: PerpMarket,
    interval: u64,
) {
    let mut interval = time::interval(Duration::from_secs(interval));
    loop {
        interval.tick().await;

        let client = mango_client.clone();

        let find_accounts = || async {
            let mut num_of_events = 0;
            let mut event_queue: EventQueue = client
                .client
                .rpc_anchor_account(&perp_market.event_queue)
                .await?;

            // TODO: future, choose better constant of how many max events to pack
            // TODO: future, choose better constant of how many max mango accounts to pack
            let mut set = HashSet::new();
            for _ in 0..10 {
                let event = match event_queue.peek_front() {
                    None => break,
                    Some(e) => e,
                };
                match EventType::try_from(event.event_type)? {
                    EventType::Fill => {
                        let fill: &FillEvent = cast_ref(event);
                        set.insert(fill.maker);
                        set.insert(fill.taker);
                    }
                    EventType::Out => {
                        let out: &OutEvent = cast_ref(event);
                        set.insert(out.owner);
                    }
                    EventType::Liquidate => {}
                }
                event_queue.pop_front()?;
                num_of_events += 1;
            }

            if num_of_events == 0 {
                return Ok(None);
            }

            Ok(Some((set, num_of_events)))
        };

        let event_info: anyhow::Result<Option<(HashSet<Pubkey>, u32)>> = find_accounts().await;

        let (event_accounts, num_of_events) = match event_info {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(err) => {
                log::error!("preparing consume_events ams: {err:?}");
                continue;
            }
        };

        let mut event_ams = event_accounts
            .iter()
            .map(|key| -> AccountMeta {
                AccountMeta {
                    pubkey: *key,
                    is_signer: false,
                    is_writable: true,
                }
            })
            .collect::<Vec<_>>();

        let pre = Instant::now();
        let ix = Instruction {
            program_id: mango_v4::id(),
            accounts: {
                let mut ams = anchor_lang::ToAccountMetas::to_account_metas(
                    &mango_v4::accounts::PerpConsumeEvents {
                        group: perp_market.group,
                        perp_market: pk,
                        event_queue: perp_market.event_queue,
                    },
                    None,
                );
                ams.append(&mut event_ams);
                ams
            },
            data: anchor_lang::InstructionData::data(&mango_v4::instruction::PerpConsumeEvents {
                limit: 10,
            }),
        };

        let sig_result = client.send_and_confirm_permissionless_tx(vec![ix]).await;

        if let Err(e) = sig_result {
            log::info!(
                "metricName=ConsumeEventsV4Failure market={} durationMs={} consumed={} error={}",
                perp_market.name(),
                pre.elapsed().as_millis(),
                num_of_events,
                e.to_string()
            );
            log::error!("{:?}", e)
        } else {
            log::info!(
                "metricName=ConsumeEventsV4Success market={} durationMs={} consumed={}",
                perp_market.name(),
                pre.elapsed().as_millis(),
                num_of_events,
            );
            log::info!("{:?}", sig_result);
        }
    }
}

pub async fn loop_update_funding(
    mango_client: Arc<MangoClient>,
    pk: Pubkey,
    perp_market: PerpMarket,
    interval: u64,
) {
    let mut interval = time::interval(Duration::from_secs(interval));
    loop {
        interval.tick().await;

        let client = mango_client.clone();

        let pre = Instant::now();
        let ix = Instruction {
            program_id: mango_v4::id(),
            accounts: anchor_lang::ToAccountMetas::to_account_metas(
                &mango_v4::accounts::PerpUpdateFunding {
                    group: perp_market.group,
                    perp_market: pk,
                    bids: perp_market.bids,
                    asks: perp_market.asks,
                    oracle: perp_market.oracle,
                },
                None,
            ),
            data: anchor_lang::InstructionData::data(&mango_v4::instruction::PerpUpdateFunding {}),
        };
        let sig_result = client.send_and_confirm_permissionless_tx(vec![ix]).await;

        if let Err(e) = sig_result {
            log::error!(
                "metricName=UpdateFundingV4Error market={} durationMs={} error={}",
                perp_market.name(),
                pre.elapsed().as_millis(),
                e.to_string()
            );
            log::error!("{:?}", e)
        } else {
            log::info!(
                "metricName=UpdateFundingV4Success market={} durationMs={}",
                perp_market.name(),
                pre.elapsed().as_millis(),
            );
            log::info!("{:?}", sig_result);
        }
    }
}

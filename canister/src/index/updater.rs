use self::inscription_updater::InscriptionUpdater;
use super::*;
use crate::chain::Chain;
use crate::index::entry::SatRange;
use crate::index::entry::StagedState;
use crate::index::reorg::Reorg;
use crate::index::updater::inscription_updater::Flotsam;
use crate::index::utxo_entry::ParsedUtxoEntry;
use crate::index::utxo_entry::UtxoEntryBuf;
use crate::inscriptions::ParsedEnvelope;
use crate::logs::{CRITICAL, DEBUG, INFO};
use crate::timestamp;
use anyhow::Error;
use bitcoin::Network;
use ordinals::{Charm, Sat, SatPoint};
use std::collections::{BTreeMap, HashSet};

pub mod inscription_updater;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockData {
  pub(crate) header: Header,
  pub(crate) txdata: Vec<(Transaction, Txid)>,
}

impl From<Block> for BlockData {
  fn from(block: Block) -> Self {
    BlockData {
      header: block.header,
      txdata: block
        .txdata
        .into_iter()
        .map(|transaction| {
          let txid = transaction.compute_txid();
          (transaction, txid)
        })
        .collect(),
    }
  }
}

/// The main entry point for the indexing process, triggered by a timer.
/// This function sets up a recurring task to perform indexing rounds.
pub fn update_index(network: BitcoinNetwork, next_state: StagedState) -> Result {
  let mut interval = 10;
  if next_state != StagedState::End {
    interval = 0;
  }
  ic_cdk_timers::set_timer(std::time::Duration::from_secs(interval), move || {
    ic_cdk::spawn(async move {
      let start = ic_cdk::api::instruction_counter();
      let (height, index_prev_blockhash) = crate::index::next_block(network);

      // Perform one round of indexing.
      let result = run_indexing_round(network, height, index_prev_blockhash, start).await;

      match result {
        Ok(state) => {
          // The exit condition is: we are done with the current block/cycle (state is End)
          // AND a shutdown has been requested.
          if state == StagedState::End && is_shutting_down() {
            log!(
              INFO,
              "Gracefully shutting down index thread at height {}.",
              height
            );
          // Otherwise, continue indexing. This allows the current block to be
          // fully processed even if a shutdown is requested mid-block.
          } else {
            let _ = update_index(network, state);
          }
        }
        Err(_) => {
          // A fatal, unrecoverable error occurred in `run_indexing_round`.
          // The error is already logged. Do not reschedule.
        }
      }
    });
  });

  Ok(())
}

/// Executes a single round of the indexing process.
///
/// Returns `Ok(StagedState)` if the process should continue (reschedule).
/// The state indicates the outcome: `End` for a pause, or another state if in the middle of indexing a block.
/// Returns `Err(anyhow::Error)` on a fatal error that should stop the indexing process.
async fn run_indexing_round(
  network: BitcoinNetwork,
  height: u32,
  index_prev_blockhash: Option<BlockHash>,
  start_time: u64,
) -> Result<StagedState> {
  // Step 1: Get the block hash for the given height.
  let block_hash = match crate::bitcoin_api::get_block_hash(network, height).await {
    Ok(Some(hash)) => hash,
    Ok(None) => return Ok(StagedState::End), // No new block yet, reschedule for later.
    Err(e) => {
      let message = format!("failed to get_block_hash at height {}: {:?}", height, e);
      // Log only new error messages to avoid spam.
      let is_new_message = CRITICAL.with_borrow(|sink| {
        sink
          .iter()
          .last()
          .map_or(true, |entry| entry.message != message)
      });
      if is_new_message {
        log!(CRITICAL, "{}", message);
      }
      return Ok(StagedState::End); // Assumed transient error, reschedule.
    }
  };

  // Step 2: Fetch the full block data.
  let block = match crate::rpc::get_block(block_hash).await {
    Ok(block) => block,
    Err(e) => {
      log!(
        CRITICAL,
        "failed to get_block: {:?} error: {:?}",
        block_hash,
        e
      );
      return Ok(StagedState::End); // Assumed transient error, reschedule.
    }
  };

  // Step 3: Check for and handle blockchain reorganizations.
  if let Err(e) = Reorg::detect_reorg(
    network,
    index_prev_blockhash,
    block.header.prev_blockhash,
    height,
  )
  .await
  {
    return match e {
      reorg::Error::Recoverable { height, depth } => {
        Reorg::handle_reorg(height, depth);
        Ok(StagedState::End) // Reorg handled, reschedule.
      }
      reorg::Error::Unrecoverable => {
        let msg = format!("unrecoverable reorg detected at height {}", height);
        log!(CRITICAL, "{}", &msg);
        Err(anyhow::anyhow!(msg))
      }
      reorg::Error::Retry => {
        log!(INFO, "retry reorg detected at height {}", height);
        Ok(StagedState::End) // Reschedule to retry.
      }
    };
  }

  // Step 4: Index the block's contents. This will propagate the error if it fails.
  let state = index_block(start_time, network, height, block).await?;

  // Log successful indexing and performance metrics.
  let end = ic_cdk::api::instruction_counter();
  log!(
    INFO,
    "update_index took {:.3}B instructions",
    (end - start_time) as f64 / 1_000_000_000.0
  );

  Ok(state)
}

async fn index_block(
  start_instruction_counter: u64,
  network: BitcoinNetwork,
  height: u32,
  block: BlockData,
) -> Result<StagedState> {
  crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = false);

  let mut sat_ranges_written = 0;
  let mut outputs_in_block = 0;

  let ic_network = match network {
    BitcoinNetwork::Mainnet => bitcoin::Network::Bitcoin,
    BitcoinNetwork::Testnet => bitcoin::Network::Testnet4,
    BitcoinNetwork::Regtest => bitcoin::Network::Regtest,
  };

  let mut utxo_cache;
  let mut ordinals_change_record;

  let mut pending_commit_outpoint = None;
  let mut pending_commit_inscriptions = Vec::new();
  let state;

  let staged_block_vars = crate::index::heap_get_staged_block_vars();
  if let Some(staged_block_vars) = staged_block_vars {
    utxo_cache = staged_block_vars.utxo_cache;
    ordinals_change_record = staged_block_vars.ordinals_change_record;
    pending_commit_outpoint = staged_block_vars.pending_commit_outpoint;
    pending_commit_inscriptions = staged_block_vars.pending_commit_inscriptions;
    state = staged_block_vars.state;
  } else {
    utxo_cache = BTreeMap::new();
    ordinals_change_record = OrdinalsChangeRecord::new();
    state = StagedState::Index;
  }

  if state == StagedState::Index {
    let cursor = crate::index::mem_get_cursor();
    log!(
      INFO,
      "Block {} at {} with {} transactions… Cursor: transaction: {}/{}, input: {}/{}, inscription: {}/{}",
      height,
      timestamp(block.header.time.into()),
      block.txdata.len(),
      cursor.tx_cursor,
      cursor.tx_len,
      cursor.input_cursor,
      cursor.input_len,
      cursor.inscription_cursor,
      cursor.inscription_len
    );

    if cursor.tx_cursor == 0 && cursor.input_cursor == 0 && cursor.inscription_cursor == 0 {
      log!(
        INFO,
        "Index statistics at height {}:
        latest_block: {:?},
        sat_to_sequence_number: {},
        sequence_number_to_children: {},
        height_to_last_sequence_number: {},
        inscription_id_to_sequence_number: {},
        inscription_number_to_sequence_number: {},
        outpoint_to_utxo_entry: {},
        sat_to_satpoint: {},
        sequence_number_to_inscription_entry: {},
        sequence_number_to_satpoint: {},
        lost_sats: {},
        cursed_inscription_count: {},
        blessed_inscription_count: {},
        unbound_inscriptions: {}",
        height,
        crate::index::mem_latest_block(),
        crate::index::mem_length_sat_to_sequence_number(),
        crate::index::mem_length_sequence_number_to_children(),
        crate::index::mem_length_height_to_last_sequence_number(),
        crate::index::mem_length_inscription_id_to_sequence_number(),
        crate::index::mem_length_inscription_number_to_sequence_number(),
        crate::index::mem_length_outpoint_to_utxo_entry(),
        crate::index::mem_length_sat_to_satpoint(),
        crate::index::mem_length_sequence_number_to_inscription_entry(),
        crate::index::mem_length_sequence_number_to_satpoint(),
        crate::index::mem_get_statistic_to_count(&Statistic::LostSats.key()).unwrap_or(0),
        crate::index::mem_get_statistic_to_count(&Statistic::CursedInscriptions.key()).unwrap_or(0),
        crate::index::mem_get_statistic_to_count(&Statistic::BlessedInscriptions.key())
          .unwrap_or(0),
        crate::index::mem_get_statistic_to_count(&Statistic::UnboundInscriptions.key())
          .unwrap_or(0),
      );
    }

    index_utxo_entries(
      ic_network,
      height,
      &block,
      &mut utxo_cache,
      &mut sat_ranges_written,
      &mut outputs_in_block,
      &mut ordinals_change_record,
      start_instruction_counter,
      cursor,
    )?;

    let next_state;
    if crate::index::BREAKPOINT.with(|m| *m.borrow()) {
      let staged_vars = StagedBlockVars {
        utxo_cache: utxo_cache.clone(),
        ordinals_change_record: ordinals_change_record.clone(),
        pending_commit_outpoint: None,
        pending_commit_inscriptions: Vec::new(),
        state: StagedState::Index,
      };
      crate::index::heap_insert_staged_block_vars(staged_vars);
      next_state = StagedState::Index;
    } else {
      let staged_vars = StagedBlockVars {
        utxo_cache: utxo_cache.clone(),
        ordinals_change_record: ordinals_change_record.clone(),
        pending_commit_outpoint: None,
        pending_commit_inscriptions: Vec::new(),
        state: StagedState::Commit,
      };
      log!(INFO, "index_utxo_entries completed. height: {}", height);
      crate::index::heap_insert_staged_block_vars(staged_vars);
      next_state = StagedState::Commit;
    }

    return Ok(next_state);
  }

  if state == StagedState::Commit {
    log!(
      INFO,
      "Block {} at {} with {} transactions… is committing: pending_commit_outpoint: {:?}, pending_commit_inscriptions: {}",
      height,
      timestamp(block.header.time.into()),
      block.txdata.len(),
      pending_commit_outpoint,
      pending_commit_inscriptions.len(),
    );

    let start = ic_cdk::api::instruction_counter();
    commit(
      height,
      utxo_cache,
      &mut ordinals_change_record,
      start_instruction_counter,
      pending_commit_outpoint,
      pending_commit_inscriptions,
    )?;
    let end = ic_cdk::api::instruction_counter();
    log!(
      INFO,
      "commit took {:.3}B instructions.",
      (end - start) as f64 / 1_000_000_000.0,
    );

    if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
      let staged_vars = StagedBlockVars {
        utxo_cache: BTreeMap::new(),
        ordinals_change_record: ordinals_change_record.clone(),
        pending_commit_outpoint: None,
        pending_commit_inscriptions: Vec::new(),
        state: StagedState::Save,
      };
      log!(INFO, "commit completed. height: {}", height);
      crate::index::heap_insert_staged_block_vars(staged_vars);
      return Ok(StagedState::Save);
    } else {
      return Ok(StagedState::Commit);
    }
  }

  if state == StagedState::Save {
    let start = ic_cdk::api::instruction_counter();
    crate::index::mem_insert_ordinals_change_record(height, ordinals_change_record);
    let end = ic_cdk::api::instruction_counter();
    log!(
      INFO,
      "mem_insert_ordinals_change_record took {:.3}B instructions",
      (end - start) as f64 / 1_000_000_000.0
    );

    let mut staged_vars = StagedBlockVars::default();
    staged_vars.state = StagedState::Prune;
    log!(
      INFO,
      "save ordinals_change_record completed. height: {}",
      height
    );
    crate::index::heap_insert_staged_block_vars(staged_vars);

    return Ok(StagedState::Prune);
  }

  if state == StagedState::Prune {
    let start = ic_cdk::api::instruction_counter();
    Reorg::prune_change_record(network, height);
    let end = ic_cdk::api::instruction_counter();
    log!(
      INFO,
      "prune_change_record took {:.3}B instructions",
      (end - start) as f64 / 1_000_000_000.0
    );

    let mut staged_vars = StagedBlockVars::default();
    staged_vars.state = StagedState::Cleanup;
    log!(INFO, "prune_change_record completed. height: {}", height);
    crate::index::heap_insert_staged_block_vars(staged_vars);

    return Ok(StagedState::Cleanup);
  }

  if state == StagedState::Cleanup {
    crate::index::mem_clear_block();
    crate::index::heap_reset_staged_block_vars();
    crate::index::mem_insert_block_header(height, block.header.store());

    log!(
      INFO,
      "Wrote {sat_ranges_written} sat ranges from {outputs_in_block} outputs",
    );
  }

  Ok(StagedState::End)
}

fn commit(
  height: u32,
  utxo_cache: BTreeMap<OutPoint, UtxoEntryBuf>,
  ordinals_change_record: &mut OrdinalsChangeRecord,
  instruction_counter: u64,
  pending_outpoint: Option<OutPoint>,
  pending_inscriptions: Vec<(u32, u64)>,
) -> Result {
  log!(
    INFO,
    "Committing at block height {}, {} in map",
    height,
    utxo_cache.len(),
  );

  if !pending_inscriptions.is_empty() {
    let mut pending_inscriptions_iter = pending_inscriptions.into_iter();
    if let Some(outpoint) = pending_outpoint {
      loop {
        let counter = ic_cdk::api::instruction_counter();
        if counter > instruction_counter + 30_000_000_000 {
          let remaining_inscriptions: Vec<(u32, u64)> = pending_inscriptions_iter.collect();

          crate::index::heap_insert_staged_block_vars(StagedBlockVars {
            utxo_cache,
            ordinals_change_record: ordinals_change_record.clone(),
            pending_commit_outpoint: Some(outpoint),
            pending_commit_inscriptions: remaining_inscriptions,
            state: StagedState::Commit,
          });
          crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
          return Ok(());
        }

        if let Some((sequence_number, offset)) = pending_inscriptions_iter.next() {
          let satpoint = SatPoint { outpoint, offset };
          crate::index::mem_insert_sequence_number_to_satpoint(sequence_number, satpoint.store());
          ordinals_change_record
            .added_seq_to_satpoint
            .push(sequence_number);
        } else {
          break;
        }
      }
    }
  }

  let mut utxo_iter = utxo_cache.into_iter();

  while let Some((outpoint, mut utxo_entry)) = utxo_iter.next() {
    if crate::index::is_special_outpoint(outpoint) {
      if let Some(old_entry_buf) = crate::index::mem_get_outpoint_to_utxo_entry(outpoint.store()) {
        utxo_entry =
          UtxoEntryBuf::merged(old_entry_buf.as_ref(), &utxo_entry, &crate::index::INDEX);
      }
    }

    crate::index::mem_insert_outpoint_to_utxo_entry(outpoint.store(), utxo_entry.vec.clone());
    ordinals_change_record.added_outpoint.push(outpoint);

    let parsed_utxo_entry = utxo_entry.parse(&crate::index::INDEX);

    if crate::index::INDEX.index_inscriptions {
      let mut inscriptions_iter = parsed_utxo_entry.parse_inscriptions().into_iter();
      loop {
        let counter = ic_cdk::api::instruction_counter();
        if counter > instruction_counter + 30_000_000_000 {
          let pending_commit_inscriptions: Vec<(u32, u64)> = inscriptions_iter.collect();
          let pending_utxo_cache: BTreeMap<OutPoint, UtxoEntryBuf> = utxo_iter.collect();

          crate::index::heap_insert_staged_block_vars(StagedBlockVars {
            utxo_cache: pending_utxo_cache,
            ordinals_change_record: ordinals_change_record.clone(),
            pending_commit_outpoint: Some(outpoint),
            pending_commit_inscriptions,
            state: StagedState::Commit,
          });
          crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
          return Ok(());
        }

        if let Some((sequence_number, offset)) = inscriptions_iter.next() {
          let satpoint = SatPoint { outpoint, offset };
          crate::index::mem_insert_sequence_number_to_satpoint(sequence_number, satpoint.store());
          ordinals_change_record
            .added_seq_to_satpoint
            .push(sequence_number);
        } else {
          break;
        }
      }
    }
  }

  Ok(())
}

fn index_utxo_entries(
  network: Network,
  height: u32,
  block: &BlockData,
  utxo_cache: &mut BTreeMap<OutPoint, UtxoEntryBuf>,
  sat_ranges_written: &mut u64,
  outputs_in_block: &mut u64,
  ordinals_change_record: &mut OrdinalsChangeRecord,
  instruction_counter: u64,
  cursor: Cursor,
) -> Result<(), Error> {
  let chain = match network {
    Network::Bitcoin => Chain::Mainnet,
    Network::Testnet4 => Chain::Testnet4,
    Network::Regtest => Chain::Regtest,
    _ => Chain::default(),
  };

  let index_inscriptions = height >= chain.first_inscription_height();

  let mut lost_sats =
    crate::index::mem_get_statistic_to_count(&Statistic::LostSats.key()).unwrap_or(0);

  let cursed_inscription_count =
    crate::index::mem_get_statistic_to_count(&Statistic::CursedInscriptions.key()).unwrap_or(0);

  let blessed_inscription_count =
    crate::index::mem_get_statistic_to_count(&Statistic::BlessedInscriptions.key()).unwrap_or(0);

  let unbound_inscriptions =
    crate::index::mem_get_statistic_to_count(&Statistic::UnboundInscriptions.key()).unwrap_or(0);

  let next_sequence_number = crate::index::mem_get_next_sequence_number();

  let jubilant = height >= chain.jubilee_height();

  let flotsam: Vec<Flotsam>;
  let mut coinbase_inputs;
  let mut lost_sat_ranges;
  let reward: u64;

  let staged_block_vars = crate::index::heap_get_staged_block_vars2();
  if let Some(staged_block_vars) = staged_block_vars {
    flotsam = staged_block_vars.flotsam;
    reward = staged_block_vars.reward;
    coinbase_inputs = staged_block_vars.coinbase_inputs;
    lost_sat_ranges = staged_block_vars.lost_sat_ranges;
  } else {
    flotsam = Vec::new();
    reward = Height(height).subsidy();
    coinbase_inputs = Vec::new();
    lost_sat_ranges = Vec::new();
    if crate::index::INDEX.index_sats {
      let h = Height(height);
      if h.subsidy() > 0 {
        let start = h.starting_sat();
        coinbase_inputs.extend(SatRange::store((start.n(), (start + h.subsidy()).n())));
      }
    }
  }

  let mut inscription_updater = InscriptionUpdater {
    blessed_inscription_count,
    cursed_inscription_count,
    flotsam,
    height,
    lost_sats,
    next_sequence_number,
    reward,
    timestamp: block.header.time,
    unbound_inscriptions,
    jubilant,
    ordinals_change_record,
    instruction_counter,
    cursor: cursor.clone(),
  };

  for (tx_offset, (tx, txid)) in block
    .txdata
    .iter()
    .enumerate()
    .skip(1)
    .chain(block.txdata.iter().enumerate().take(1))
    .skip(cursor.tx_cursor as usize)
  {
    log!(DEBUG, "Indexing transaction {tx_offset}…");

    let input_utxo_entries;
    let mut output_utxo_entries;
    let input_sat_ranges;
    if let Some(staged_tx_vars) = crate::index::heap_get_staged_tx_vars() {
      input_utxo_entries = staged_tx_vars.input_utxo_entries;
      output_utxo_entries = staged_tx_vars.output_utxo_entries;
      input_sat_ranges = staged_tx_vars.input_sat_ranges;
    } else {
      input_utxo_entries = if tx_offset == 0 {
        Vec::new()
      } else {
        let mut outpoint_removals = Vec::new();
        let entries = tx
          .input
          .iter()
          .map(|input| {
            let outpoint = input.previous_output.store();

            let entry = if let Some(entry) = utxo_cache.remove(&OutPoint::load(outpoint)) {
              entry
            } else if let Some(entry) = crate::index::mem_remove_outpoint_to_utxo_entry(outpoint) {
              outpoint_removals.push((OutPoint::load(outpoint), entry.clone()));
              entry
            } else {
              log!(
                INFO,
                "outpoint not found {:?}",
                OutPoint::load(outpoint).to_string()
              );
              assert!(!crate::index::have_full_utxo_index());
              UtxoEntryBuf::new()
            };

            Ok(entry)
          })
          .collect::<Result<Vec<UtxoEntryBuf>>>()?;

        inscription_updater
          .ordinals_change_record
          .removed_outpoint
          .extend(outpoint_removals);

        entries
      };

      output_utxo_entries = tx
        .output
        .iter()
        .map(|_| UtxoEntryBuf::new())
        .collect::<Vec<UtxoEntryBuf>>();

      if crate::index::INDEX.index_sats {
        let leftover_sat_ranges;

        if tx_offset == 0 {
          input_sat_ranges = Some(vec![coinbase_inputs.clone()]);
          leftover_sat_ranges = &mut lost_sat_ranges;
        } else {
          input_sat_ranges = Some(
            input_utxo_entries
              .iter()
              .map(|entry| entry.parse(&crate::index::INDEX).sat_ranges().to_vec())
              .collect(),
          );
          leftover_sat_ranges = &mut coinbase_inputs;
        }

        index_transaction_sats(
          tx,
          *txid,
          &mut output_utxo_entries,
          input_sat_ranges.as_ref().unwrap(),
          leftover_sat_ranges,
          sat_ranges_written,
          outputs_in_block,
          inscription_updater.ordinals_change_record,
        )?;
      } else {
        input_sat_ranges = None;

        for (vout, txout) in tx.output.iter().enumerate() {
          output_utxo_entries[vout].push_value(txout.value.to_sat(), &crate::index::INDEX);
        }
      }
    }

    let parsed_input_utxo_entries = input_utxo_entries
      .iter()
      .map(|entry| entry.parse(&crate::index::INDEX))
      .collect::<Vec<ParsedUtxoEntry>>();

    if index_inscriptions {
      inscription_updater.index_inscriptions(
        tx,
        *txid,
        &parsed_input_utxo_entries,
        &mut output_utxo_entries,
        utxo_cache,
        &crate::index::INDEX,
        input_sat_ranges.as_ref(),
        tx_offset,
      )?;

      if crate::index::BREAKPOINT.with(|m| *m.borrow()) {
        crate::index::heap_insert_staged_block_vars2(StagedBlockVars2 {
          flotsam: inscription_updater.flotsam,
          reward: inscription_updater.reward,
          coinbase_inputs,
          lost_sat_ranges,
        });
        crate::index::heap_insert_staged_tx_vars(StagedTxVars {
          input_utxo_entries: input_utxo_entries.clone(),
          output_utxo_entries: output_utxo_entries.clone(),
          input_sat_ranges: input_sat_ranges.clone(),
        });

        crate::index::mem_insert_height_to_last_sequence_number(
          height,
          inscription_updater.next_sequence_number,
        );
        crate::index::mem_insert_statistic_to_count(
          &Statistic::LostSats.key(),
          &if crate::index::INDEX.index_sats {
            lost_sats
          } else {
            inscription_updater.lost_sats
          },
        );

        crate::index::mem_insert_statistic_to_count(
          &Statistic::CursedInscriptions.key(),
          &inscription_updater.cursed_inscription_count,
        );

        crate::index::mem_insert_statistic_to_count(
          &Statistic::BlessedInscriptions.key(),
          &inscription_updater.blessed_inscription_count,
        );

        crate::index::mem_insert_statistic_to_count(
          &Statistic::UnboundInscriptions.key(),
          &inscription_updater.unbound_inscriptions,
        );

        return Ok(());
      } else {
        crate::index::heap_reset_staged_tx_vars();
      }
    }

    for (vout, output_utxo_entry) in output_utxo_entries.into_iter().enumerate() {
      let vout = u32::try_from(vout).unwrap();
      utxo_cache.insert(OutPoint { txid: *txid, vout }, output_utxo_entry);
    }
  }

  if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
    crate::index::heap_reset_staged_block_vars2();
  }

  if index_inscriptions {
    crate::index::mem_insert_height_to_last_sequence_number(
      height,
      inscription_updater.next_sequence_number,
    );
  }

  if !lost_sat_ranges.is_empty() {
    let utxo_entry = utxo_cache
      .entry(OutPoint::null())
      .or_insert(UtxoEntryBuf::empty(&crate::index::INDEX));

    for chunk in lost_sat_ranges.chunks_exact(11) {
      let (start, end) = SatRange::load(chunk.try_into().unwrap());
      if !Sat(start).common() {
        crate::index::mem_insert_sat_to_satpoint(
          start,
          SatPoint {
            outpoint: OutPoint::null(),
            offset: lost_sats,
          }
          .store(),
        );
        inscription_updater
          .ordinals_change_record
          .added_sat_to_satpoint
          .push(start);
      }

      lost_sats += end - start;
    }

    let mut new_utxo_entry = UtxoEntryBuf::new();
    new_utxo_entry.push_sat_ranges(&lost_sat_ranges, &crate::index::INDEX);

    *utxo_entry = UtxoEntryBuf::merged(utxo_entry, &new_utxo_entry, &crate::index::INDEX);
  }

  inscription_updater.ordinals_change_record.lost_sats =
    crate::index::mem_get_statistic_to_count(&Statistic::LostSats.key()).unwrap_or(0);
  inscription_updater
    .ordinals_change_record
    .cursed_inscriptions =
    crate::index::mem_get_statistic_to_count(&Statistic::CursedInscriptions.key()).unwrap_or(0);
  inscription_updater
    .ordinals_change_record
    .blessed_inscriptions =
    crate::index::mem_get_statistic_to_count(&Statistic::BlessedInscriptions.key()).unwrap_or(0);
  inscription_updater
    .ordinals_change_record
    .unbound_inscriptions =
    crate::index::mem_get_statistic_to_count(&Statistic::UnboundInscriptions.key()).unwrap_or(0);

  crate::index::mem_insert_statistic_to_count(
    &Statistic::LostSats.key(),
    &if crate::index::INDEX.index_sats {
      lost_sats
    } else {
      inscription_updater.lost_sats
    },
  );

  crate::index::mem_insert_statistic_to_count(
    &Statistic::CursedInscriptions.key(),
    &inscription_updater.cursed_inscription_count,
  );

  crate::index::mem_insert_statistic_to_count(
    &Statistic::BlessedInscriptions.key(),
    &inscription_updater.blessed_inscription_count,
  );

  crate::index::mem_insert_statistic_to_count(
    &Statistic::UnboundInscriptions.key(),
    &inscription_updater.unbound_inscriptions,
  );

  Ok(())
}

fn index_transaction_sats(
  tx: &Transaction,
  txid: Txid,
  output_utxo_entries: &mut [UtxoEntryBuf],
  input_sat_ranges: &[Vec<u8>],
  leftover_sat_ranges: &mut Vec<u8>,
  sat_ranges_written: &mut u64,
  outputs_traversed: &mut u64,
  ordinals_change_record: &mut OrdinalsChangeRecord,
) -> Result {
  let mut pending_input_sat_range = None;
  let mut input_sat_ranges_iter = input_sat_ranges
    .iter()
    .flat_map(|slice| slice.chunks_exact(11));

  let mut sats = Vec::with_capacity(
    input_sat_ranges
      .iter()
      .map(|slice| slice.len())
      .sum::<usize>(),
  );

  for (vout, output) in tx.output.iter().enumerate() {
    let outpoint = OutPoint {
      vout: vout.try_into().unwrap(),
      txid,
    };

    let mut remaining = output.value.to_sat();
    while remaining > 0 {
      let range = pending_input_sat_range.take().unwrap_or_else(|| {
        SatRange::load(
          input_sat_ranges_iter
            .next()
            .expect("insufficient inputs for transaction outputs")
            .try_into()
            .unwrap(),
        )
      });

      if !Sat(range.0).common() {
        crate::index::mem_insert_sat_to_satpoint(
          range.0,
          SatPoint {
            outpoint,
            offset: output.value.to_sat() - remaining,
          }
          .store(),
        );
        ordinals_change_record.added_sat_to_satpoint.push(range.0);
      }

      let count = range.1 - range.0;

      let assigned = if count > remaining {
        let middle = range.0 + remaining;
        pending_input_sat_range = Some((middle, range.1));
        (range.0, middle)
      } else {
        range
      };

      sats.extend_from_slice(&assigned.store());

      remaining -= assigned.1 - assigned.0;

      *sat_ranges_written += 1;
    }

    *outputs_traversed += 1;

    output_utxo_entries[vout].push_sat_ranges(&sats, &crate::index::INDEX);
    sats.clear();
  }

  if let Some(range) = pending_input_sat_range {
    leftover_sat_ranges.extend(&range.store());
  }
  leftover_sat_ranges.extend(input_sat_ranges_iter.flatten());

  Ok(())
}

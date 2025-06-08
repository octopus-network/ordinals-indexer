use self::inscription_updater::InscriptionUpdater;
use super::*;
use crate::chain::Chain;
use crate::index::entry::SatRange;
use crate::index::reorg::Reorg;
use crate::index::utxo_entry::ParsedUtxoEntry;
use crate::index::utxo_entry::UtxoEntryBuf;
use crate::inscriptions::ParsedEnvelope;
use crate::logs::{CRITICAL, DEBUG, INFO};
use crate::timestamp;
use anyhow::Error;
use bitcoin::Network;
use candid::Principal;
use ordinals::{Charm, Sat, SatPoint};
use std::collections::{BTreeMap, HashSet};

mod inscription_updater;

pub(crate) struct BlockData {
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

pub fn update_index(network: BitcoinNetwork) -> Result {
  ic_cdk_timers::set_timer(std::time::Duration::from_secs(10), move || {
    ic_cdk::spawn(async move {
      let (height, index_prev_blockhash) = crate::index::next_block(network);
      match crate::bitcoin_api::get_block_hash(network, height).await {
        Ok(Some(block_hash)) => match crate::rpc::get_block(block_hash).await {
          Ok(block) => {
            match Reorg::detect_reorg(
              network,
              index_prev_blockhash,
              block.header.prev_blockhash,
              height,
            )
            .await
            {
              Ok(()) => {
                if let Err(e) = index_block(network, height, block).await {
                  log!(
                    CRITICAL,
                    "failed to index_block at height {}: {:?}",
                    height,
                    e
                  );
                  return;
                }
                Reorg::prune_change_record(network, height);
              }
              Err(e) => match e {
                reorg::Error::Recoverable { height, depth } => {
                  Reorg::handle_reorg(height, depth);
                }
                reorg::Error::Unrecoverable => {
                  log!(
                    CRITICAL,
                    "unrecoverable reorg detected at height {}",
                    height
                  );
                  return;
                }
                reorg::Error::Retry => {
                  log!(INFO, "retry reorg detected at height {}", height);
                }
              },
            }
          }
          Err(e) => {
            log!(
              CRITICAL,
              "failed to get_block: {:?} error: {:?}",
              block_hash,
              e
            );
          }
        },
        Ok(None) => {}
        Err(e) => {
          let message = format!("failed to get_block_hash at height {}: {:?}", height, e);
          let is_new_message = CRITICAL.with_borrow(|sink| {
            sink
              .iter()
              .last()
              .map_or(true, |entry| entry.message != message)
          });

          if is_new_message {
            log!(CRITICAL, "{}", message);
          }
        }
      }
      if is_shutting_down() {
        log!(
          INFO,
          "shutting down index thread, skipping update at height {}",
          height
        );
      } else {
        let _ = update_index(network);
      }
    });
  });

  Ok(())
}

async fn index_block(network: BitcoinNetwork, height: u32, block: BlockData) -> Result<()> {
  log!(
    INFO,
    "Block {} at {} with {} transactions…",
    height,
    timestamp(block.header.time.into()),
    block.txdata.len()
  );

  let start = ic_cdk::api::instruction_counter();

  let mut sat_ranges_written = 0;
  let mut outputs_in_block = 0;

  let mut utxo_cache = HashMap::new();

  // let runes = crate::index::mem_statistic_runes();
  // let reserved_runes = crate::index::mem_statistic_reserved_runes();

  // if height % 10 == 0 {
  //   log!(
  //     INFO,
  //     "Index statistics at height {}: latest_block: {:?}, reserved_runes: {}, runes: {}, rune_to_rune_id: {}, rune_entry: {}, transaction_id_to_rune: {}, outpoint_to_rune_balances: {}, outpoint_to_height: {}",
  //     height,
  //     crate::index::mem_latest_block(),
  //     reserved_runes,
  //     runes,
  //     crate::index::mem_length_rune_to_rune_id(),
  //     crate::index::mem_length_rune_id_to_rune_entry(),
  //     crate::index::mem_length_transaction_id_to_rune(),
  //     crate::index::mem_length_outpoint_to_rune_balances(),
  //     crate::index::mem_length_outpoint_to_height(),
  //   );
  // }

  // init statistic runes/reserved_runes for new height
  // crate::index::mem_insert_statistic_runes(height, runes);
  // crate::index::mem_insert_statistic_reserved_runes(height, reserved_runes);

  let network = match network {
    BitcoinNetwork::Mainnet => bitcoin::Network::Bitcoin,
    BitcoinNetwork::Testnet => bitcoin::Network::Testnet4,
    BitcoinNetwork::Regtest => bitcoin::Network::Regtest,
  };

  // let mut rune_updater = RuneUpdater {
  //   block_time: block.header.time,
  //   burned: HashMap::new(),
  //   height,
  //   minimum: Rune::minimum_at_height(network, Height(height)),
  //   runes,
  //   change_record: ChangeRecord::new(),
  // };

  // for (i, (tx, txid)) in block.txdata.iter().enumerate() {
  //   rune_updater
  //     .index_runes(u32::try_from(i).unwrap(), tx, *txid)
  //     .await?;
  // }

  // rune_updater.update()?;

  index_utxo_entries(
    network,
    height,
    &block,
    &mut utxo_cache,
    &mut sat_ranges_written,
    &mut outputs_in_block,
  )?;

  commit(height, utxo_cache)?;
  crate::index::mem_insert_block_header(height, block.header.store());

  log!(
    INFO,
    "Wrote {sat_ranges_written} sat ranges from {outputs_in_block} outputs",
  );

  let end = ic_cdk::api::instruction_counter();
  log!(INFO, "index_block took {} instructions", end - start);

  Ok(())
}

fn commit(height: u32, utxo_cache: HashMap<OutPoint, UtxoEntryBuf>) -> Result {
  log!(
    INFO,
    "Committing at block height {}, {} in map",
    height,
    utxo_cache.len(),
  );

  {
    // let mut outpoint_to_utxo_entry = wtx.open_table(OUTPOINT_TO_UTXO_ENTRY)?;
    //let mut script_pubkey_to_outpoint = wtx.open_multimap_table(SCRIPT_PUBKEY_TO_OUTPOINT)?;
    // let mut sequence_number_to_satpoint = wtx.open_table(SEQUENCE_NUMBER_TO_SATPOINT)?;

    for (outpoint, mut utxo_entry) in utxo_cache {
      if crate::index::is_special_outpoint(outpoint) {
        if let Some(old_entry_buf) = crate::index::mem_get_outpoint_to_utxo_entry(outpoint.store())
        {
          utxo_entry =
            UtxoEntryBuf::merged(old_entry_buf.as_ref(), &utxo_entry, &crate::index::INDEX);
        }
        // if let Some(old_entry) = outpoint_to_utxo_entry.get(&outpoint.store())? {
        //   utxo_entry = UtxoEntryBuf::merged(old_entry.value(), &utxo_entry, &crate::index::INDEX);
        // }
      }

      crate::index::mem_insert_outpoint_to_utxo_entry(outpoint.store(), utxo_entry.clone());
      // outpoint_to_utxo_entry.insert(&outpoint.store(), utxo_entry.as_ref())?;

      let utxo_entry = utxo_entry.parse(&crate::index::INDEX);
      if crate::index::INDEX.index_addresses {
        let script_pubkey = utxo_entry.script_pubkey();
        // script_pubkey_to_outpoint.insert(script_pubkey, &outpoint.store())?;
        crate::index::mem_insert_script_pubkey_to_outpoint(script_pubkey, outpoint.store());
      }

      if crate::index::INDEX.index_inscriptions {
        for (sequence_number, offset) in utxo_entry.parse_inscriptions() {
          let satpoint = SatPoint { outpoint, offset };
          crate::index::mem_insert_sequence_number_to_satpoint(sequence_number, satpoint.store());
          // sequence_number_to_satpoint.insert(sequence_number, &satpoint.store())?;
        }
      }
    }
  }

  // Index::increment_statistic(&wtx, Statistic::Commits, 1)?;
  // wtx.commit()?;

  // // Commit twice since due to a bug redb will only reuse pages freed in the
  // // transaction before last.
  // self.index.begin_write()?.commit()?;

  // Reorg::update_savepoints(self.index, self.height)?;

  Ok(())
}

fn index_utxo_entries(
  network: Network,
  height: u32,
  block: &BlockData,
  utxo_cache: &mut HashMap<OutPoint, UtxoEntryBuf>,
  sat_ranges_written: &mut u64,
  outputs_in_block: &mut u64,
) -> Result<(), Error> {
  // let mut height_to_last_sequence_number = wtx.open_table(HEIGHT_TO_LAST_SEQUENCE_NUMBER)?;
  // let mut home_inscriptions = wtx.open_table(HOME_INSCRIPTIONS)?;
  // let mut inscription_number_to_sequence_number =
  //   wtx.open_table(INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER)?;
  // let mut outpoint_to_utxo_entry = wtx.open_table(OUTPOINT_TO_UTXO_ENTRY)?;
  // let mut sat_to_satpoint = wtx.open_table(SAT_TO_SATPOINT)?;
  // let mut sat_to_sequence_number = wtx.open_multimap_table(SAT_TO_SEQUENCE_NUMBER)?;
  // let mut script_pubkey_to_outpoint = wtx.open_multimap_table(SCRIPT_PUBKEY_TO_OUTPOINT)?;
  // let mut sequence_number_to_children = wtx.open_multimap_table(SEQUENCE_NUMBER_TO_CHILDREN)?;
  // let mut sequence_number_to_inscription_entry =
  //   wtx.open_table(SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY)?;
  // let mut transaction_id_to_transaction = wtx.open_table(TRANSACTION_ID_TO_TRANSACTION)?;

  let chain = match network {
    Network::Bitcoin => Chain::Mainnet,
    Network::Testnet => Chain::Testnet4,
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

  // let next_sequence_number = sequence_number_to_inscription_entry
  //   .iter()?
  //   .next_back()
  //   .transpose()?
  //   .map(|(number, _id)| number.value() + 1)
  //   .unwrap_or(0);

  let next_sequence_number = crate::index::mem_get_next_sequence_number();

  // let home_inscription_count = home_inscriptions.len()?;
  let home_inscription_count = crate::index::mem_length_home_inscriptions();

  let jubilant = height >= chain.jubilee_height();

  let mut inscription_updater = InscriptionUpdater {
    blessed_inscription_count,
    cursed_inscription_count,
    flotsam: Vec::new(),
    height,
    home_inscription_count,
    lost_sats,
    next_sequence_number,
    reward: Height(height).subsidy(),
    timestamp: block.header.time,
    unbound_inscriptions,
    jubilant,
  };

  let mut coinbase_inputs = Vec::new();
  let mut lost_sat_ranges = Vec::new();

  if crate::index::INDEX.index_sats {
    let h = Height(height);
    if h.subsidy() > 0 {
      let start = h.starting_sat();
      coinbase_inputs.extend(SatRange::store((start.n(), (start + h.subsidy()).n())));
      // self.sat_ranges_since_flush += 1;
    }
  }

  for (tx_offset, (tx, txid)) in block
    .txdata
    .iter()
    .enumerate()
    .skip(1)
    .chain(block.txdata.iter().enumerate().take(1))
  {
    log!(DEBUG, "Indexing transaction {tx_offset}…");

    let input_utxo_entries = if tx_offset == 0 {
      Vec::new()
    } else {
      tx.input
        .iter()
        .map(|input| {
          let outpoint = input.previous_output.store();

          let entry = if let Some(entry) = utxo_cache.remove(&OutPoint::load(outpoint)) {
            // self.outputs_cached += 1;
            entry
          } else if let Some(entry_buf) = crate::index::mem_remove_outpoint_to_utxo_entry(outpoint)
          {
            // } else if let Some(entry) = outpoint_to_utxo_entry.remove(&outpoint)? {
            if crate::index::INDEX.index_addresses {
              let script_pubkey = entry_buf
                .as_ref()
                .parse(&crate::index::INDEX)
                .script_pubkey();
              // if !script_pubkey_to_outpoint.remove(script_pubkey, outpoint)? {
              if crate::index::mem_remove_script_pubkey_to_outpoint(script_pubkey, outpoint)
                .is_none()
              {
                panic!("script pubkey entry ({script_pubkey:?}, {outpoint:?}) not found");
              }
            }

            entry_buf
          } else {
            assert!(!crate::index::have_full_utxo_index());
            // TODO: This branch should never be reached
            let entry = UtxoEntryBuf::new();
            entry
          };

          Ok(entry)
        })
        .collect::<Result<Vec<UtxoEntryBuf>>>()?
    };

    let input_utxo_entries = input_utxo_entries
      .iter()
      .map(|entry| entry.parse(&crate::index::INDEX))
      .collect::<Vec<ParsedUtxoEntry>>();

    let mut output_utxo_entries = tx
      .output
      .iter()
      .map(|_| UtxoEntryBuf::new())
      .collect::<Vec<UtxoEntryBuf>>();

    let input_sat_ranges;
    if crate::index::INDEX.index_sats {
      let leftover_sat_ranges;

      if tx_offset == 0 {
        input_sat_ranges = Some(vec![coinbase_inputs.as_slice()]);
        leftover_sat_ranges = &mut lost_sat_ranges;
      } else {
        input_sat_ranges = Some(
          input_utxo_entries
            .iter()
            .map(|entry| entry.sat_ranges())
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
      )?;
    } else {
      input_sat_ranges = None;

      for (vout, txout) in tx.output.iter().enumerate() {
        output_utxo_entries[vout].push_value(txout.value.to_sat(), &crate::index::INDEX);
      }
    }

    if crate::index::INDEX.index_addresses {
      index_transaction_output_script_pubkeys(tx, &mut output_utxo_entries);
    }

    if index_inscriptions {
      inscription_updater.index_inscriptions(
        tx,
        *txid,
        &input_utxo_entries,
        &mut output_utxo_entries,
        utxo_cache,
        &crate::index::INDEX,
        input_sat_ranges.as_ref(),
      )?;
    }

    for (vout, output_utxo_entry) in output_utxo_entries.into_iter().enumerate() {
      let vout = u32::try_from(vout).unwrap();
      utxo_cache.insert(OutPoint { txid: *txid, vout }, output_utxo_entry);
    }
  }

  if index_inscriptions {
    // height_to_last_sequence_number
    //   .insert(&height, inscription_updater.next_sequence_number)?;
    crate::index::mem_insert_height_to_last_sequence_number(
      height,
      inscription_updater.next_sequence_number,
    );
  }

  if !lost_sat_ranges.is_empty() {
    // Note that the lost-sats outpoint is special, because (unlike real
    // outputs) it gets written to more than once.  commit() will merge
    // our new entry with any existing one.
    let utxo_entry = utxo_cache
      .entry(OutPoint::null())
      .or_insert(UtxoEntryBuf::empty(&crate::index::INDEX));

    for chunk in lost_sat_ranges.chunks_exact(11) {
      let (start, end) = SatRange::load(chunk.try_into().unwrap());
      if !Sat(start).common() {
        // sat_to_satpoint.insert(
        //   &start,
        //   &SatPoint {
        //     outpoint: OutPoint::null(),
        //     offset: lost_sats,
        //   }
        //   .store(),
        // )?;

        crate::index::mem_insert_sat_to_satpoint(
          start,
          SatPoint {
            outpoint: OutPoint::null(),
            offset: lost_sats,
          }
          .store(),
        );
      }

      lost_sats += end - start;
    }

    let mut new_utxo_entry = UtxoEntryBuf::new();
    new_utxo_entry.push_sat_ranges(&lost_sat_ranges, &crate::index::INDEX);
    if crate::index::INDEX.index_addresses {
      new_utxo_entry.push_script_pubkey(&[], &crate::index::INDEX);
    }

    *utxo_entry = UtxoEntryBuf::merged(utxo_entry, &new_utxo_entry, &crate::index::INDEX);
  }

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

fn index_transaction_output_script_pubkeys(
  tx: &Transaction,
  output_utxo_entries: &mut [UtxoEntryBuf],
) {
  for (vout, txout) in tx.output.iter().enumerate() {
    output_utxo_entries[vout]
      .push_script_pubkey(txout.script_pubkey.as_bytes(), &crate::index::INDEX);
  }
}

fn index_transaction_sats(
  tx: &Transaction,
  txid: Txid,
  output_utxo_entries: &mut [UtxoEntryBuf],
  input_sat_ranges: &[&[u8]],
  leftover_sat_ranges: &mut Vec<u8>,
  sat_ranges_written: &mut u64,
  outputs_traversed: &mut u64,
) -> Result {
  let mut pending_input_sat_range = None;
  let mut input_sat_ranges_iter = input_sat_ranges
    .iter()
    .flat_map(|slice| slice.chunks_exact(11));

  // Preallocate our temporary array, sized to hold the combined
  // sat ranges from our inputs.  We'll never need more than that
  // for a single output, even if we end up splitting some ranges.
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
        // sat_to_satpoint.insert(
        //   &range.0,
        //   &SatPoint {
        //     outpoint,
        //     offset: output.value.to_sat() - remaining,
        //   }
        //   .store(),
        // )?;

        crate::index::mem_insert_sat_to_satpoint(
          range.0,
          SatPoint {
            outpoint,
            offset: output.value.to_sat() - remaining,
          }
          .store(),
        );
      }

      let count = range.1 - range.0;

      let assigned = if count > remaining {
        // self.sat_ranges_since_flush += 1;
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

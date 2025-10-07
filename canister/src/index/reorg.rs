use crate::index::Statistic;
use crate::index::entry::Entry;
use crate::index::{CRITICAL, INFO};
use bitcoin::block::BlockHash;
use ic_canister_log::log;
use ic_cdk::api::management_canister::bitcoin::BitcoinNetwork;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
  Recoverable { height: u32, depth: u32 },
  Unrecoverable,
  Retry,
}

impl Display for Error {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    match self {
      Self::Recoverable { height, depth } => {
        write!(f, "{depth} block deep reorg detected at height {height}")
      }
      Self::Unrecoverable => write!(f, "unrecoverable reorg detected"),
      Self::Retry => write!(f, "retry reorg detected"),
    }
  }
}

impl std::error::Error for Error {}

pub fn get_max_recoverable_reorg_depth(network: BitcoinNetwork) -> u32 {
  match network {
    BitcoinNetwork::Regtest => 6,
    BitcoinNetwork::Testnet => 64,
    BitcoinNetwork::Mainnet => 6,
  }
}

pub struct Reorg {}

impl Reorg {
  pub(crate) async fn detect_reorg(
    network: BitcoinNetwork,
    index_prev_blockhash: Option<BlockHash>,
    bitcoind_prev_blockhash: BlockHash,
    height: u32,
  ) -> Result<(), Error> {
    match index_prev_blockhash {
      Some(index_prev_blockhash) if index_prev_blockhash == bitcoind_prev_blockhash => Ok(()),
      Some(index_prev_blockhash) if index_prev_blockhash != bitcoind_prev_blockhash => {
        for depth in 1..=get_max_recoverable_reorg_depth(network) {
          let check_height = height.checked_sub(depth).ok_or_else(|| {
            log!(CRITICAL, "Height overflow at depth {}", depth);
            Error::Unrecoverable
          })?;

          let index_block_hash = crate::index::mem_block_hash(check_height).ok_or_else(|| {
            log!(
              CRITICAL,
              "Missing index block hash at height {}",
              check_height
            );
            Error::Unrecoverable
          })?;

          match crate::bitcoin_api::get_block_hash(network, check_height).await {
            Ok(Some(bitcoin_block_hash)) => {
              if index_block_hash == bitcoin_block_hash {
                log!(
                  INFO,
                  "Found common ancestor at height {} (depth {})",
                  check_height,
                  depth
                );
                return Err(Error::Recoverable { height, depth });
              }
            }
            _ => {
              log!(
                INFO,
                "The block hash at height {} is not found. Retrying.",
                check_height
              );
              return Err(Error::Retry);
            }
          }
        }

        log!(
          CRITICAL,
          "No common ancestor found within recoverable depth"
        );
        Err(Error::Unrecoverable)
      }
      _ => Ok(()),
    }
  }

  pub fn handle_reorg(height: u32, depth: u32) {
    log!(
      INFO,
      "rolling back state after reorg of depth {depth} at height {height}"
    );

    let start = ic_cdk::api::instruction_counter();

    for h in (height - depth + 1..height).rev() {
      log!(INFO, "rolling back change record at height {h}");
      if let Some(change_record) = crate::index::mem_get_ordinals_change_record(h) {
        change_record
          .added_sat_to_seq
          .iter()
          .for_each(|(sat, seq)| {
            crate::index::mem_remove_sat_to_sequence_number(*sat, *seq);
          });
        change_record
          .added_seq_to_children
          .iter()
          .for_each(|(seq, child)| {
            crate::index::mem_remove_sequence_number_to_children(*seq, *child);
          });
        change_record
          .added_inscription_id
          .iter()
          .for_each(|inscription_id| {
            crate::index::mem_remove_inscription_id_to_sequence_number(inscription_id.store());
          });
        change_record
          .added_inscription_number
          .iter()
          .for_each(|inscription_number| {
            crate::index::mem_remove_inscription_number_to_sequence_number(
              inscription_number.clone(),
            );
          });
        change_record.added_outpoint.iter().for_each(|outpoint| {
          crate::index::mem_remove_outpoint_to_utxo_entry(outpoint.store());
        });
        change_record
          .removed_outpoint
          .iter()
          .for_each(|(outpoint, entry_buf)| {
            crate::index::mem_insert_outpoint_to_utxo_entry(
              outpoint.store(),
              entry_buf.vec.clone(),
            );
          });
        change_record.added_sat_to_satpoint.iter().for_each(|sat| {
          crate::index::mem_remove_sat_to_satpoint(*sat);
        });
        change_record
          .added_seq_to_inscription
          .iter()
          .for_each(|seq| {
            crate::index::mem_remove_sequence_number_to_entry(*seq);
          });
        change_record.added_seq_to_satpoint.iter().for_each(|seq| {
          crate::index::mem_remove_sequence_number_to_satpoint(*seq);
        });
        crate::index::mem_insert_statistic_to_count(
          &Statistic::LostSats.key(),
          &change_record.lost_sats,
        );
        crate::index::mem_insert_statistic_to_count(
          &Statistic::CursedInscriptions.key(),
          &change_record.cursed_inscriptions,
        );
        crate::index::mem_insert_statistic_to_count(
          &Statistic::BlessedInscriptions.key(),
          &change_record.blessed_inscriptions,
        );
        crate::index::mem_insert_statistic_to_count(
          &Statistic::UnboundInscriptions.key(),
          &change_record.unbound_inscriptions,
        );
      }
      crate::index::mem_remove_height_to_last_sequence_number(h);
      crate::index::mem_remove_ordinals_change_record(h);
      crate::index::mem_remove_block_header(h);
    }

    log!(
      INFO,
      "successfully rolled back state to height {}",
      height - depth,
    );

    let end = ic_cdk::api::instruction_counter();
    log!(INFO, "reorg took {} instructions", end - start);
  }

  pub(crate) fn prune_change_record(network: BitcoinNetwork, height: u32) {
    if height >= get_max_recoverable_reorg_depth(network) {
      let h = height - get_max_recoverable_reorg_depth(network);
      log!(INFO, "clearing change record {h} at height {height}");
      crate::index::mem_prune_ordinals_change_record(h);
      crate::index::mem_prune_block_header(h);
    }
  }
}

use self::entry::Entry;
use super::Result;
use crate::config::Config;
use crate::index::entry::{
  ChangeRecord, Children, HeaderValue, InscriptionEntry, InscriptionIdValue, InscriptionNumber,
  OutPointValue, Outpoints, SatPointValue, SequenceNumbers,
};
use crate::index::utxo_entry::UtxoEntryBuf;
use crate::inscriptions::InscriptionId;
use crate::logs::{CRITICAL, INFO};
use crate::unbound_outpoint;
use bitcoin::{
  Block, OutPoint, Transaction, Txid,
  block::Header,
  consensus::{self, Decodable, Encodable},
  hash_types::BlockHash,
  hashes::Hash,
};
use ic_canister_log::log;
use ic_cdk::api::management_canister::bitcoin::BitcoinNetwork;
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::{DefaultMemoryImpl, StableBTreeMap, StableCell};
use ordinals::{Height, Pile, Rune, RuneId, SatPoint, SpacedRune, Terms};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{self, AtomicBool};

pub mod entry;
mod lot;
mod reorg;
pub mod updater;
mod utxo_entry;

type Memory = VirtualMemory<DefaultMemoryImpl>;

thread_local! {
  static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
      RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

  static CONFIG: RefCell<StableCell<Config, Memory>> = RefCell::new(
      StableCell::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0))),
          Config::default()
      ).unwrap()
  );

static SAT_TO_SEQUENCE_NUMBER: RefCell<StableBTreeMap<u64, SequenceNumbers, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(1))),
  )
);

static SEQUENCE_NUMBER_TO_CHILDREN: RefCell<StableBTreeMap<u32, Children, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(2))),
  )
);

static SCRIPT_PUBKEY_TO_OUTPOINT: RefCell<StableBTreeMap<Vec<u8>, Outpoints, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(3))),
  )
);

static HEIGHT_TO_BLOCK_HEADER: RefCell<StableBTreeMap<u32, HeaderValue, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(4))),
    )
);

static HEIGHT_TO_LAST_SEQUENCE_NUMBER: RefCell<StableBTreeMap<u32, u32, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(5))),
  )
);

static HOME_INSCRIPTIONS: RefCell<StableBTreeMap<u32, InscriptionIdValue, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(6))),
  )
);

static INSCRIPTION_ID_TO_SEQUENCE_NUMBER: RefCell<StableBTreeMap<InscriptionIdValue, u32, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(7))),
  )
);

static INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER: RefCell<StableBTreeMap<InscriptionNumber, u32, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(8))),
  )
);

static OUTPOINT_TO_UTXO_ENTRY: RefCell<StableBTreeMap<OutPointValue, UtxoEntryBuf, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(9))),
  )
);

static SAT_TO_SATPOINT: RefCell<StableBTreeMap<u64, SatPointValue, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(10))),
  )
);

static SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY: RefCell<StableBTreeMap<u32, InscriptionEntry, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(11))),
  )
);

static SEQUENCE_NUMBER_TO_SATPOINT: RefCell<StableBTreeMap<u32, SatPointValue, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(12))),
  )
);

static STATISTIC_TO_COUNT: RefCell<StableBTreeMap<u64, u64, Memory>> = RefCell::new(
  StableBTreeMap::init(
      MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(13))),
  )
);

  static HEIGHT_TO_CHANGE_RECORD: RefCell<StableBTreeMap<u32, ChangeRecord, Memory>> = RefCell::new(
      StableBTreeMap::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(14))),
      )
  );
}

#[derive(Copy, Clone)]
pub(crate) enum Statistic {
  Schema = 0,
  BlessedInscriptions = 1,
  Commits = 2,
  CursedInscriptions = 3,
  IndexAddresses = 4,
  IndexInscriptions = 5,
  IndexRunes = 6,
  IndexSats = 7,
  IndexTransactions = 8,
  InitialSyncTime = 9,
  LostSats = 10,
  OutputsTraversed = 11,
  ReservedRunes = 12,
  Runes = 13,
  SatRanges = 14,
  UnboundInscriptions = 16,
  LastSavepointHeight = 17,
}

impl Statistic {
  fn key(self) -> u64 {
    self.into()
  }
}

impl From<Statistic> for u64 {
  fn from(statistic: Statistic) -> Self {
    statistic as u64
  }
}

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub fn shut_down() {
  SHUTTING_DOWN.store(true, atomic::Ordering::Relaxed);
}

pub fn cancel_shutdown() {
  SHUTTING_DOWN.store(false, atomic::Ordering::Relaxed);
}

pub fn is_shutting_down() -> bool {
  SHUTTING_DOWN.load(atomic::Ordering::Relaxed)
}

pub fn mem_get_config() -> Config {
  CONFIG.with(|m| m.borrow().get().clone())
}

pub fn mem_set_config(config: Config) -> Result<Config> {
  CONFIG
    .with(|m| m.borrow_mut().set(config))
    .map_err(|e| anyhow::anyhow!("Failed to set config: {:?}", e))
}

pub fn mem_latest_block() -> Option<(u32, BlockHash)> {
  HEIGHT_TO_BLOCK_HEADER.with(|m| {
    m.borrow()
      .iter()
      .rev()
      .next()
      .map(|(height, header_value)| {
        let header = Header::load(header_value);
        (height, header.block_hash())
      })
  })
}

pub fn mem_latest_block_height() -> Option<u32> {
  HEIGHT_TO_BLOCK_HEADER.with(|m| m.borrow().iter().rev().next().map(|(height, _)| height))
}

pub fn mem_block_hash(height: u32) -> Option<BlockHash> {
  HEIGHT_TO_BLOCK_HEADER.with(|m| {
    m.borrow()
      .get(&height)
      .map(|header_value| Header::load(header_value).block_hash())
  })
}

pub fn mem_insert_block_header(height: u32, header_value: HeaderValue) {
  HEIGHT_TO_BLOCK_HEADER.with(|m| m.borrow_mut().insert(height, header_value));
}

pub fn mem_remove_block_header(height: u32) -> Option<HeaderValue> {
  HEIGHT_TO_BLOCK_HEADER.with(|m| m.borrow_mut().remove(&height))
}

pub fn mem_prune_block_header(height: u32) {
  HEIGHT_TO_BLOCK_HEADER.with(|m| {
    let mut map = m.borrow_mut();
    let keys_to_remove: Vec<u32> = map
      .iter()
      .take_while(|(h, _)| *h <= height)
      .map(|(h, _)| h)
      .collect();
    for key in keys_to_remove {
      map.remove(&key);
    }
  });
}

pub fn mem_get_outpoint_to_utxo_entry(outpoint_value: OutPointValue) -> Option<UtxoEntryBuf> {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| m.borrow().get(&outpoint_value))
}

pub fn mem_insert_outpoint_to_utxo_entry(
  outpoint_value: OutPointValue,
  utxo_entry_buf: UtxoEntryBuf,
) {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| m.borrow_mut().insert(outpoint_value, utxo_entry_buf));
}

pub fn mem_remove_outpoint_to_utxo_entry(outpoint_value: OutPointValue) -> Option<UtxoEntryBuf> {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| m.borrow_mut().remove(&outpoint_value))
}

pub fn mem_insert_script_pubkey_to_outpoint(script_pubkey: &[u8], outpoint_value: OutPointValue) {
  let outpoint = OutPoint::load(outpoint_value);
  SCRIPT_PUBKEY_TO_OUTPOINT.with(|m| {
    let mut map = m.borrow_mut();
    let script_pubkey_vec = script_pubkey.to_vec();

    if let Some(mut existing_outpoints) = map.get(&script_pubkey_vec) {
      if !existing_outpoints.outpoints.contains(&outpoint) {
        existing_outpoints.outpoints.push(outpoint);
        map.insert(script_pubkey_vec, existing_outpoints);
      }
    } else {
      let outpoints = Outpoints {
        outpoints: vec![outpoint],
      };
      map.insert(script_pubkey_vec, outpoints);
    }
  });
}

pub fn mem_remove_script_pubkey_to_outpoint(
  script_pubkey: &[u8],
  outpoint_value: OutPointValue,
) -> Option<OutPointValue> {
  let outpoint = OutPoint::load(outpoint_value);
  SCRIPT_PUBKEY_TO_OUTPOINT.with(|m| {
    let mut map = m.borrow_mut();
    let script_pubkey_vec = script_pubkey.to_vec();

    if let Some(mut existing_outpoints) = map.get(&script_pubkey_vec) {
      if existing_outpoints.outpoints.contains(&outpoint) {
        existing_outpoints.outpoints.retain(|o| o != &outpoint);
        if existing_outpoints.outpoints.is_empty() {
          map.remove(&script_pubkey_vec);
        } else {
          map.insert(script_pubkey_vec, existing_outpoints);
        }
        Some(outpoint_value)
      } else {
        None
      }
    } else {
      None
    }
  })
}

pub fn mem_insert_sequence_number_to_satpoint(sequence_number: u32, satpoint_value: SatPointValue) {
  SEQUENCE_NUMBER_TO_SATPOINT.with(|m| m.borrow_mut().insert(sequence_number, satpoint_value));
}

pub fn mem_get_statistic_to_count(key: &u64) -> Option<u64> {
  STATISTIC_TO_COUNT.with(|m| m.borrow().get(key))
}

pub fn mem_insert_statistic_to_count(key: &u64, value: &u64) {
  STATISTIC_TO_COUNT.with(|m| m.borrow_mut().insert(*key, *value));
}

pub fn mem_get_next_sequence_number() -> u32 {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| {
    m.borrow()
      .iter()
      .next_back()
      .map(|(number, _id)| number + 1)
      .unwrap_or(0)
  })
}

pub fn mem_length_home_inscriptions() -> u64 {
  HOME_INSCRIPTIONS.with(|m| m.borrow().len())
}

pub fn mem_pop_first_home_inscription() -> Option<(u32, InscriptionIdValue)> {
  HOME_INSCRIPTIONS.with(|m| m.borrow_mut().pop_first())
}

pub fn mem_insert_home_inscription(sequence_number: u32, inscription_id: InscriptionIdValue) {
  HOME_INSCRIPTIONS.with(|m| m.borrow_mut().insert(sequence_number, inscription_id));
}

pub fn mem_get_sequence_number_to_entry(sequence_number: u32) -> Option<InscriptionEntry> {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow().get(&sequence_number))
}

pub fn mem_insert_sequence_number_to_entry(sequence_number: u32, entry: InscriptionEntry) {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow_mut().insert(sequence_number, entry));
}

pub fn mem_get_inscription_id_to_sequence_number(
  inscription_id: InscriptionIdValue,
) -> Option<u32> {
  INSCRIPTION_ID_TO_SEQUENCE_NUMBER.with(|m| m.borrow().get(&inscription_id))
}

pub fn mem_insert_inscription_id_to_sequence_number(
  inscription_id: InscriptionIdValue,
  sequence_number: u32,
) {
  INSCRIPTION_ID_TO_SEQUENCE_NUMBER
    .with(|m| m.borrow_mut().insert(inscription_id, sequence_number));
}

pub fn mem_get_inscription_number_to_sequence_number(
  inscription_number: InscriptionNumber,
) -> Option<u32> {
  INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER.with(|m| m.borrow().get(&inscription_number))
}

pub fn mem_insert_inscription_number_to_sequence_number(
  inscription_number: InscriptionNumber,
  sequence_number: u32,
) {
  INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER
    .with(|m| m.borrow_mut().insert(inscription_number, sequence_number));
}

pub fn mem_insert_sat_to_sequence_number(sat: u64, sequence_number: u32) {
  SAT_TO_SEQUENCE_NUMBER.with(|m| {
    let mut map = m.borrow_mut();

    if let Some(mut seqs) = map.get(&sat) {
      if !seqs.numbers.contains(&sequence_number) {
        seqs.numbers.push(sequence_number);
        map.insert(sat, seqs);
      }
    } else {
      let seqs = SequenceNumbers {
        numbers: vec![sequence_number],
      };
      map.insert(sat, seqs);
    }
  });
}

pub fn mem_insert_sequence_number_to_children(
  parent_sequence_number: u32,
  child_sequence_number: u32,
) {
  SEQUENCE_NUMBER_TO_CHILDREN.with(|m| {
    let mut map = m.borrow_mut();
    if let Some(mut children) = map.get(&parent_sequence_number) {
      if !children.children.contains(&child_sequence_number) {
        children.children.push(child_sequence_number);
        map.insert(parent_sequence_number, children);
      }
    } else {
      let children = Children {
        children: vec![child_sequence_number],
      };
      map.insert(parent_sequence_number, children);
    }
  });
}

pub fn mem_insert_height_to_last_sequence_number(height: u32, sequence_number: u32) {
  HEIGHT_TO_LAST_SEQUENCE_NUMBER.with(|m| m.borrow_mut().insert(height, sequence_number));
}

pub fn mem_insert_sat_to_satpoint(sat: u64, satpoint_value: SatPointValue) {
  SAT_TO_SATPOINT.with(|m| m.borrow_mut().insert(sat, satpoint_value));
}

pub fn mem_length_change_record() -> u64 {
  HEIGHT_TO_CHANGE_RECORD.with(|m| m.borrow().len())
}

pub(crate) fn mem_insert_change_record(height: u32, change_record: ChangeRecord) {
  HEIGHT_TO_CHANGE_RECORD.with(|m| m.borrow_mut().insert(height, change_record));
}

pub(crate) fn mem_get_change_record(height: u32) -> Option<ChangeRecord> {
  HEIGHT_TO_CHANGE_RECORD.with(|m| m.borrow().get(&height))
}

pub(crate) fn mem_remove_change_record(height: u32) -> Option<ChangeRecord> {
  HEIGHT_TO_CHANGE_RECORD.with(|m| m.borrow_mut().remove(&height))
}

pub fn mem_prune_change_record(height: u32) {
  HEIGHT_TO_CHANGE_RECORD.with(|m| {
    let mut map = m.borrow_mut();
    let keys_to_remove: Vec<u32> = map
      .iter()
      .take_while(|(h, _)| *h <= height)
      .map(|(h, _)| h)
      .collect();
    for key in keys_to_remove {
      map.remove(&key);
    }
  });
}

pub fn next_block(network: BitcoinNetwork) -> (u32, Option<BlockHash>) {
  mem_latest_block()
    .map(|(height, prev_blockhash)| (height + 1, Some(prev_blockhash)))
    .unwrap_or((0, None))
}

/// Unlike normal outpoints, which are added to index on creation and removed
/// when spent, the UTXO entry for special outpoints may be updated.
///
/// The special outpoints are the null outpoint, which receives lost sats,
/// and the unbound outpoint, which receives unbound inscriptions.
pub fn is_special_outpoint(outpoint: OutPoint) -> bool {
  outpoint == OutPoint::null() || outpoint == unbound_outpoint()
}

pub fn have_full_utxo_index() -> bool {
  true
}

pub(crate) struct Index {
  index_sats: bool,
  index_addresses: bool,
  index_inscriptions: bool,
}

const INDEX: Index = Index {
  index_sats: true,
  index_addresses: true,
  index_inscriptions: true,
};

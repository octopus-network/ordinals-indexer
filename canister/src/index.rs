use self::entry::Entry;
use super::Result;
use crate::config::Config;
use crate::index::entry::SatRange;
use crate::index::entry::{
  Children, Cursor, CursorValue, HeaderValue, InscriptionEntry, InscriptionIdValue,
  InscriptionNumber, OrdinalsChangeRecord, OutPointValue, SatPointValue, SequenceNumbers,
  StagedBlock, StagedBlockVars, StagedBlockVars2, StagedInputVars, StagedInputVars2,
  StagedInputVars3, StagedTxVars,
};
#[cfg(debug_assertions)]
use crate::index::utxo_entry::State;
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
use std::sync::atomic::{self, AtomicBool};

pub mod entry;
mod lot;
mod reorg;
pub mod updater;
pub mod utxo_entry;

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

  static HEIGHT_TO_BLOCK_HEADER: RefCell<StableBTreeMap<u32, HeaderValue, Memory>> = RefCell::new(
      StableBTreeMap::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(3))),
      )
  );

  static HEIGHT_TO_LAST_SEQUENCE_NUMBER: RefCell<StableBTreeMap<u32, u32, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(4))),
    )
  );

  static INSCRIPTION_ID_TO_SEQUENCE_NUMBER: RefCell<StableBTreeMap<InscriptionIdValue, u32, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(5))),
    )
  );

  static INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER: RefCell<StableBTreeMap<InscriptionNumber, u32, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(6))),
    )
  );

  static OUTPOINT_TO_UTXO_ENTRY: RefCell<StableBTreeMap<OutPointValue, Vec<u8>, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(7))),
    )
  );

  static SAT_TO_SATPOINT: RefCell<StableBTreeMap<u64, SatPointValue, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(8))),
    )
  );

  static SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY: RefCell<StableBTreeMap<u32, InscriptionEntry, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(9))),
    )
  );

  static SEQUENCE_NUMBER_TO_SATPOINT: RefCell<StableBTreeMap<u32, SatPointValue, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(10))),
    )
  );

  static STATISTIC_TO_COUNT: RefCell<StableBTreeMap<u64, u64, Memory>> = RefCell::new(
    StableBTreeMap::init(
        MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(11))),
    )
  );

  static HEIGHT_TO_ORDINALS_CHANGE_RECORD: RefCell<StableBTreeMap<u32, OrdinalsChangeRecord, Memory>> = RefCell::new(
      StableBTreeMap::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(12))),
      )
  );

  static BLOCK: RefCell<StableCell<Option<StagedBlock>, Memory>> = RefCell::new(
      StableCell::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(13))),
          None
      ).unwrap()
  );

  static CURSOR: RefCell<StableCell<CursorValue, Memory>> = RefCell::new(
      StableCell::init(
          MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(14))),
          ((0, 0), (0, 0), (0, 0))
      ).unwrap()
  );

  // static STAGED_BLOCK_VARS: RefCell<StableCell<Option<StagedBlockVars>, Memory>> = RefCell::new(
  //     StableCell::init(
  //         MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(15))),
  //         None
  //     ).unwrap()
  // );

  // static STAGED_TX_VARS: RefCell<StableCell<Option<StagedTxVars>, Memory>> = RefCell::new(
  //     StableCell::init(
  //         MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(16))),
  //         None
  //     ).unwrap()
  // );

  // static STAGED_INPUT_VARS: RefCell<StableCell<Option<StagedInputVars>, Memory>> = RefCell::new(
  //     StableCell::init(
  //         MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(17))),
  //         None
  //     ).unwrap()
  // );

  pub static BREAKPOINT: RefCell<bool> = RefCell::new(false);
  pub static STAGED_BLOCK_VARS: RefCell<Option<StagedBlockVars>> = RefCell::new(None);
  pub static STAGED_BLOCK_VARS2: RefCell<Option<StagedBlockVars2>> = RefCell::new(None);
  pub static STAGED_TX_VARS: RefCell<Option<StagedTxVars>> = RefCell::new(None);
  pub static STAGED_INPUT_VARS: RefCell<Option<StagedInputVars>> = RefCell::new(None);
  pub static STAGED_INPUT_VARS2: RefCell<Option<StagedInputVars2>> = RefCell::new(None);
  pub static STAGED_INPUT_VARS3: RefCell<Option<StagedInputVars3>> = RefCell::new(None);
}

#[derive(Copy, Clone)]
pub enum Statistic {
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
  pub fn key(self) -> u64 {
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
    while let Some((key, _)) = map.first_key_value() {
      if key <= height {
        map.pop_first();
      } else {
        break;
      }
    }
  });
}

pub fn mem_length_outpoint_to_utxo_entry() -> u64 {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| m.borrow().len())
}

pub fn mem_get_outpoint_to_utxo_entry(outpoint_value: OutPointValue) -> Option<UtxoEntryBuf> {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| {
    m.borrow().get(&outpoint_value).map(|vec| UtxoEntryBuf {
      vec,
      #[cfg(debug_assertions)]
      state: State::Valid,
    })
  })
}

pub fn mem_insert_outpoint_to_utxo_entry(outpoint_value: OutPointValue, utxo_entry_value: Vec<u8>) {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| m.borrow_mut().insert(outpoint_value, utxo_entry_value));
}

pub fn mem_remove_outpoint_to_utxo_entry(outpoint_value: OutPointValue) -> Option<UtxoEntryBuf> {
  OUTPOINT_TO_UTXO_ENTRY.with(|m| {
    m.borrow_mut()
      .remove(&outpoint_value)
      .map(|vec| UtxoEntryBuf {
        vec,
        #[cfg(debug_assertions)]
        state: State::Valid,
      })
  })
}

pub fn mem_length_sequence_number_to_satpoint() -> u64 {
  SEQUENCE_NUMBER_TO_SATPOINT.with(|m| m.borrow().len())
}

pub fn mem_insert_sequence_number_to_satpoint(sequence_number: u32, satpoint_value: SatPointValue) {
  SEQUENCE_NUMBER_TO_SATPOINT.with(|m| m.borrow_mut().insert(sequence_number, satpoint_value));
}

pub fn mem_remove_sequence_number_to_satpoint(sequence_number: u32) -> Option<SatPointValue> {
  SEQUENCE_NUMBER_TO_SATPOINT.with(|m| m.borrow_mut().remove(&sequence_number))
}

pub fn mem_get_sequence_number_to_satpoint(sequence_number: u32) -> Option<SatPointValue> {
  SEQUENCE_NUMBER_TO_SATPOINT.with(|m| m.borrow_mut().get(&sequence_number))
}

pub fn mem_get_statistic_to_count(key: &u64) -> Option<u64> {
  STATISTIC_TO_COUNT.with(|m| m.borrow().get(key))
}

pub fn mem_insert_statistic_to_count(key: &u64, value: &u64) {
  STATISTIC_TO_COUNT.with(|m| m.borrow_mut().insert(*key, *value));
}

pub fn mem_length_sat_to_satpoint() -> u64 {
  SAT_TO_SATPOINT.with(|m| m.borrow().len())
}

pub fn mem_insert_sat_to_satpoint(sat: u64, satpoint_value: SatPointValue) {
  SAT_TO_SATPOINT.with(|m| m.borrow_mut().insert(sat, satpoint_value));
}

pub fn mem_remove_sat_to_satpoint(sat: u64) -> Option<SatPointValue> {
  SAT_TO_SATPOINT.with(|m| m.borrow_mut().remove(&sat))
}

pub fn mem_length_sequence_number_to_inscription_entry() -> u64 {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow().len())
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

pub fn mem_get_sequence_number_to_entry(sequence_number: u32) -> Option<InscriptionEntry> {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow().get(&sequence_number))
}

pub fn mem_insert_sequence_number_to_entry(sequence_number: u32, entry: InscriptionEntry) {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow_mut().insert(sequence_number, entry));
}

pub fn mem_remove_sequence_number_to_entry(sequence_number: u32) -> Option<InscriptionEntry> {
  SEQUENCE_NUMBER_TO_INSCRIPTION_ENTRY.with(|m| m.borrow_mut().remove(&sequence_number))
}

pub fn mem_length_inscription_id_to_sequence_number() -> u64 {
  INSCRIPTION_ID_TO_SEQUENCE_NUMBER.with(|m| m.borrow().len())
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

pub fn mem_remove_inscription_id_to_sequence_number(inscription_id: InscriptionIdValue) {
  INSCRIPTION_ID_TO_SEQUENCE_NUMBER.with(|m| m.borrow_mut().remove(&inscription_id));
}

pub fn mem_length_inscription_number_to_sequence_number() -> u64 {
  INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER.with(|m| m.borrow().len())
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

pub fn mem_remove_inscription_number_to_sequence_number(inscription_number: InscriptionNumber) {
  INSCRIPTION_NUMBER_TO_SEQUENCE_NUMBER.with(|m| m.borrow_mut().remove(&inscription_number));
}

pub fn mem_length_sat_to_sequence_number() -> u64 {
  SAT_TO_SEQUENCE_NUMBER.with(|m| m.borrow().len())
}

pub fn mem_insert_sat_to_sequence_number(sat: u64, sequence_number: u32) {
  SAT_TO_SEQUENCE_NUMBER.with(|m| {
    let mut map = m.borrow_mut();
    if let Some(mut sequence_numbers) = map.get(&sat) {
      if !sequence_numbers.sequence_numbers.contains(&sequence_number) {
        sequence_numbers.sequence_numbers.push(sequence_number);
        map.insert(sat, sequence_numbers);
      }
    } else {
      let sequence_numbers = SequenceNumbers {
        sequence_numbers: vec![sequence_number],
      };
      map.insert(sat, sequence_numbers);
    }
  });
}

pub fn mem_insert_sat_to_sequence_numbers(sat: u64, sequence_numbers: Vec<u32>) {
  SAT_TO_SEQUENCE_NUMBER.with(|m| {
    let mut map = m.borrow_mut();
    map.insert(sat, SequenceNumbers { sequence_numbers });
  });
}

pub fn mem_remove_sat_to_sequence_number(sat: u64, sequence_number: u32) {
  SAT_TO_SEQUENCE_NUMBER.with(|m| {
    let mut map = m.borrow_mut();

    if let Some(mut sequence_numbers) = map.get(&sat) {
      if sequence_numbers.sequence_numbers.contains(&sequence_number) {
        sequence_numbers
          .sequence_numbers
          .retain(|s| *s != sequence_number);

        if sequence_numbers.sequence_numbers.is_empty() {
          map.remove(&sat);
        } else {
          map.insert(sat, sequence_numbers);
        }
      }
    }
  });
}

pub fn mem_length_sequence_number_to_children() -> u64 {
  SEQUENCE_NUMBER_TO_CHILDREN.with(|m| m.borrow().len())
}

pub fn mem_insert_sequence_number_to_children(
  parent_sequence_number: u32,
  child_sequence_numbers: Vec<u32>,
) {
  SEQUENCE_NUMBER_TO_CHILDREN.with(|m| {
    let mut map = m.borrow_mut();
    map.insert(
      parent_sequence_number,
      Children {
        children: child_sequence_numbers,
      },
    );
  });
}

pub fn mem_insert_sequence_number_to_child(
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

pub fn mem_remove_sequence_number_to_children(
  parent_sequence_number: u32,
  child_sequence_number: u32,
) {
  SEQUENCE_NUMBER_TO_CHILDREN.with(|m| {
    let mut map = m.borrow_mut();

    if let Some(mut children) = map.get(&parent_sequence_number) {
      if children.children.contains(&child_sequence_number) {
        children.children.retain(|s| *s != child_sequence_number);

        if children.children.is_empty() {
          map.remove(&parent_sequence_number);
        } else {
          map.insert(parent_sequence_number, children);
        }
      }
    }
  });
}

pub fn mem_length_height_to_last_sequence_number() -> u64 {
  HEIGHT_TO_LAST_SEQUENCE_NUMBER.with(|m| m.borrow().len())
}

pub fn mem_insert_height_to_last_sequence_number(height: u32, sequence_number: u32) {
  HEIGHT_TO_LAST_SEQUENCE_NUMBER.with(|m| m.borrow_mut().insert(height, sequence_number));
}

pub fn mem_remove_height_to_last_sequence_number(height: u32) -> Option<u32> {
  HEIGHT_TO_LAST_SEQUENCE_NUMBER.with(|m| m.borrow_mut().remove(&height))
}

pub fn mem_length_ordinals_change_record() -> u64 {
  HEIGHT_TO_ORDINALS_CHANGE_RECORD.with(|m| m.borrow().len())
}

pub(crate) fn mem_insert_ordinals_change_record(height: u32, change_record: OrdinalsChangeRecord) {
  HEIGHT_TO_ORDINALS_CHANGE_RECORD.with(|m| m.borrow_mut().insert(height, change_record));
}

pub(crate) fn mem_get_ordinals_change_record(height: u32) -> Option<OrdinalsChangeRecord> {
  HEIGHT_TO_ORDINALS_CHANGE_RECORD.with(|m| m.borrow().get(&height))
}

pub(crate) fn mem_remove_ordinals_change_record(height: u32) -> Option<OrdinalsChangeRecord> {
  HEIGHT_TO_ORDINALS_CHANGE_RECORD.with(|m| m.borrow_mut().remove(&height))
}

pub fn mem_prune_ordinals_change_record(height: u32) {
  HEIGHT_TO_ORDINALS_CHANGE_RECORD.with(|m| {
    let mut map = m.borrow_mut();
    while let Some((key, _)) = map.first_key_value() {
      if key <= height {
        map.pop_first();
      } else {
        break;
      }
    }
  });
}

pub fn mem_clear_block() {
  BLOCK.with(|m| m.borrow_mut().set(None)).unwrap();
}

pub fn mem_insert_block(block: StagedBlock) {
  BLOCK.with(|m| m.borrow_mut().set(Some(block))).unwrap();
}

pub fn mem_get_block() -> Option<StagedBlock> {
  BLOCK.with(|m| m.borrow().get().clone())
}

pub fn mem_reset_cursor_with_tx_len(tx_len: u64) {
  CURSOR
    .with(|m| m.borrow_mut().set(((0, tx_len), (0, 0), (0, 0))))
    .unwrap();
}

pub fn mem_set_cursor(cursor: Cursor) {
  CURSOR.with(|m| m.borrow_mut().set(cursor.store())).unwrap();
}

pub fn mem_get_cursor() -> Cursor {
  let value = CURSOR.with(|m| m.borrow().get().clone());
  Cursor::load(value)
}

pub fn heap_get_staged_block_vars() -> Option<StagedBlockVars> {
  STAGED_BLOCK_VARS.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_block_vars(staged_block_vars: StagedBlockVars) {
  STAGED_BLOCK_VARS.with(|m| *m.borrow_mut() = Some(staged_block_vars));
}

pub fn heap_reset_staged_block_vars() {
  STAGED_BLOCK_VARS.with(|m| *m.borrow_mut() = None);
}

pub fn heap_get_staged_block_vars2() -> Option<StagedBlockVars2> {
  STAGED_BLOCK_VARS2.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_block_vars2(staged_block_vars: StagedBlockVars2) {
  STAGED_BLOCK_VARS2.with(|m| *m.borrow_mut() = Some(staged_block_vars));
}

pub fn heap_reset_staged_block_vars2() {
  STAGED_BLOCK_VARS2.with(|m| *m.borrow_mut() = None);
}

pub fn heap_get_staged_tx_vars() -> Option<StagedTxVars> {
  STAGED_TX_VARS.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_tx_vars(staged_tx_vars: StagedTxVars) {
  STAGED_TX_VARS.with(|m| *m.borrow_mut() = Some(staged_tx_vars));
}

pub fn heap_reset_staged_tx_vars() {
  STAGED_TX_VARS.with(|m| *m.borrow_mut() = None);
}

pub fn heap_get_staged_input_vars() -> Option<StagedInputVars> {
  STAGED_INPUT_VARS.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_input_vars(staged_input_vars: StagedInputVars) {
  STAGED_INPUT_VARS.with(|m| *m.borrow_mut() = Some(staged_input_vars));
}

pub fn heap_reset_staged_input_vars() {
  STAGED_INPUT_VARS.with(|m| *m.borrow_mut() = None);
}

pub fn heap_get_staged_input_vars2() -> Option<StagedInputVars2> {
  STAGED_INPUT_VARS2.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_input_vars2(staged_input_vars: StagedInputVars2) {
  STAGED_INPUT_VARS2.with(|m| *m.borrow_mut() = Some(staged_input_vars));
}

pub fn heap_reset_staged_input_vars2() {
  STAGED_INPUT_VARS2.with(|m| *m.borrow_mut() = None);
}

pub fn heap_get_staged_input_vars3() -> Option<StagedInputVars3> {
  STAGED_INPUT_VARS3.with(|m| m.borrow().clone())
}

pub fn heap_insert_staged_input_vars3(staged_input_vars: StagedInputVars3) {
  STAGED_INPUT_VARS3.with(|m| *m.borrow_mut() = Some(staged_input_vars));
}

pub fn heap_reset_staged_input_vars3() {
  STAGED_INPUT_VARS3.with(|m| *m.borrow_mut() = None);
}

pub fn next_block(_network: BitcoinNetwork) -> (u32, Option<BlockHash>) {
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

pub fn list(outpoint: OutPoint) -> Option<Vec<(u64, u64)>> {
  mem_get_outpoint_to_utxo_entry(outpoint.store()).map(|utxo_entry| {
    utxo_entry
      .parse(&INDEX)
      .sat_ranges()
      .chunks_exact(11)
      .map(|chunk| SatRange::load(chunk.try_into().unwrap()))
      .collect::<Vec<(u64, u64)>>()
  })
}

pub fn contains_output(output: &OutPoint) -> bool {
  mem_get_outpoint_to_utxo_entry(output.store()).is_some()
}

fn inscriptions_on_output(outpoint: OutPoint) -> Result<Option<Vec<(SatPoint, InscriptionId)>>> {
  let Some(utxo_entry) = mem_get_outpoint_to_utxo_entry(outpoint.store()) else {
    return Ok(Some(Vec::new()));
  };

  let mut inscriptions = utxo_entry.parse(&INDEX).parse_inscriptions();

  inscriptions.sort_by_key(|(sequence_number, _)| *sequence_number);

  inscriptions
    .into_iter()
    .map(|(sequence_number, offset)| {
      let entry = mem_get_sequence_number_to_entry(sequence_number).unwrap();

      let satpoint = SatPoint { outpoint, offset };

      Ok((satpoint, entry.id))
    })
    .collect::<Result<_>>()
    .map(Some)
}

pub fn get_inscriptions_on_output_with_satpoints(
  outpoint: OutPoint,
) -> Result<Option<Vec<(SatPoint, InscriptionId)>>> {
  inscriptions_on_output(outpoint)
}

pub fn inner_get_inscriptions_for_output(outpoint: OutPoint) -> Result<Option<Vec<InscriptionId>>> {
  let Some(inscriptions) = get_inscriptions_on_output_with_satpoints(outpoint)? else {
    return Ok(None);
  };

  Ok(Some(
    inscriptions
      .iter()
      .map(|(_satpoint, inscription_id)| *inscription_id)
      .collect(),
  ))
}

pub struct Index {
  index_sats: bool,
  index_addresses: bool,
  index_inscriptions: bool,
}

const INDEX: Index = Index {
  index_sats: true,
  index_addresses: false,
  index_inscriptions: true,
};

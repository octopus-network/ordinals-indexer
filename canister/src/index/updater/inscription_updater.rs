use super::*;

#[derive(Debug, PartialEq, Copy, Clone)]
enum Curse {
  DuplicateField,
  IncompleteField,
  NotAtOffsetZero,
  NotInFirstInput,
  Pointer,
  Pushnum,
  Reinscription,
  Stutter,
  UnrecognizedEvenField,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flotsam {
  inscription_id: InscriptionId,
  offset: u64,
  origin: Origin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Origin {
  New {
    cursed: bool,
    fee: u64,
    hidden: bool,
    parents: Vec<InscriptionId>,
    reinscription: bool,
    unbound: bool,
    vindicated: bool,
  },
  Old {
    sequence_number: u32,
    old_satpoint: SatPoint,
  },
}

pub(super) struct InscriptionUpdater<'a> {
  pub(super) blessed_inscription_count: u64,
  pub(super) cursed_inscription_count: u64,
  pub(super) flotsam: Vec<Flotsam>,
  pub(super) height: u32,
  pub(super) lost_sats: u64,
  pub(super) next_sequence_number: u32,
  pub(super) reward: u64,
  pub(super) timestamp: u32,
  pub(super) unbound_inscriptions: u64,
  pub(super) jubilant: bool,
  pub(super) ordinals_change_record: &'a mut OrdinalsChangeRecord,
  pub(super) instruction_counter: u64,
  pub(super) cursor: Cursor,
}

impl<'a> InscriptionUpdater<'a> {
  pub(super) fn index_inscriptions(
    &mut self,
    tx: &Transaction,
    txid: Txid,
    input_utxo_entries: &[ParsedUtxoEntry],
    output_utxo_entries: &mut [UtxoEntryBuf],
    utxo_cache: &mut BTreeMap<OutPoint, UtxoEntryBuf>,
    index: &Index,
    input_sat_ranges: Option<&Vec<Vec<u8>>>,
    tx_offset: usize,
  ) -> Result {
    let mut floating_inscriptions;
    let mut id_counter;
    let mut inscribed_offsets;
    let mut total_input_value;
    let mut envelopes;

    if let Some(staged_input_vars) = crate::index::heap_get_staged_input_vars() {
      floating_inscriptions = staged_input_vars.floating_inscriptions;
      id_counter = staged_input_vars.id_counter;
      inscribed_offsets = staged_input_vars.inscribed_offsets;
      total_input_value = staged_input_vars.total_input_value;
      envelopes = staged_input_vars.envelopes;
    } else {
      floating_inscriptions = Vec::new();
      id_counter = 0;
      inscribed_offsets = BTreeMap::new();
      total_input_value = 0;
      let vec_envelopes = ParsedEnvelope::from_transaction(tx);
      // let has_new_inscriptions = !envelopes.is_empty();
      envelopes = vec_envelopes.into_iter().peekable();
    }

    let total_output_value = tx
      .output
      .iter()
      .map(|txout| txout.value.to_sat())
      .sum::<u64>();

    self.cursor.input_len = tx.input.len() as u64;
    for (input_index, txin) in tx
      .input
      .iter()
      .enumerate()
      .skip(self.cursor.input_cursor as usize)
    {
      // skip subsidy since no inscriptions possible
      if txin.previous_output.is_null() {
        total_input_value += Height(self.height).subsidy();
        continue;
      }

      let mut transferred_inscriptions = input_utxo_entries[input_index].parse_inscriptions();
      self.cursor.inscription_len = transferred_inscriptions.len() as u64;

      if self.cursor.inscription_len > 10000 {
        log!(
          INFO,
          "transferred_inscriptions: {:?}, txid {}",
          transferred_inscriptions.len(),
          txid.to_string()
        );
      }

      transferred_inscriptions.sort_by_key(|(sequence_number, _)| *sequence_number);

      if transferred_inscriptions.is_empty() {
        let counter = ic_cdk::api::instruction_counter();
        if counter > self.instruction_counter + 30_000_000_000 {
          self.cursor.tx_cursor = if tx_offset == 0 {
            self.cursor.tx_len - 1
          } else {
            tx_offset as u64 - 1
          };
          self.cursor.input_cursor = input_index as u64;
          let cursor = Cursor {
            tx_cursor: self.cursor.tx_cursor,
            tx_len: self.cursor.tx_len,
            input_cursor: self.cursor.input_cursor,
            input_len: self.cursor.input_len,
            inscription_cursor: 0,
            inscription_len: 0,
          };
          crate::index::mem_set_cursor(cursor);
          crate::index::heap_insert_staged_input_vars(StagedInputVars {
            floating_inscriptions: floating_inscriptions.clone(),
            id_counter,
            inscribed_offsets: inscribed_offsets.clone(),
            total_input_value,
            envelopes,
          });

          log!(
            INFO,
            "BREAKPOINT at cursor {} with instruction_counter: {:.3}B, transferred_inscriptions is empty",
            self.cursor,
            (counter - self.instruction_counter) as f64 / 1_000_000_000.0,
          );
          crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
          return Ok(());
        }
      }

      for (inscription_index, (sequence_number, old_satpoint_offset)) in transferred_inscriptions
        .into_iter()
        .enumerate()
        .skip(self.cursor.inscription_cursor as usize)
      {
        let counter = ic_cdk::api::instruction_counter();
        if counter > self.instruction_counter + 30_000_000_000 {
          self.cursor.tx_cursor = if tx_offset == 0 {
            self.cursor.tx_len - 1
          } else {
            tx_offset as u64 - 1
          };
          self.cursor.input_cursor = input_index as u64;
          self.cursor.inscription_cursor = inscription_index as u64;
          let cursor = Cursor {
            tx_cursor: self.cursor.tx_cursor,
            tx_len: self.cursor.tx_len,
            input_cursor: self.cursor.input_cursor,
            input_len: self.cursor.input_len,
            inscription_cursor: self.cursor.inscription_cursor,
            inscription_len: self.cursor.inscription_len,
          };
          crate::index::mem_set_cursor(cursor);
          crate::index::heap_insert_staged_input_vars(StagedInputVars {
            floating_inscriptions: floating_inscriptions.clone(),
            id_counter,
            inscribed_offsets: inscribed_offsets.clone(),
            total_input_value,
            envelopes,
          });
          log!(
            INFO,
            "BREAKPOINT at cursor {} with instruction_counter: {:.3}B",
            self.cursor,
            (counter - self.instruction_counter) as f64 / 1_000_000_000.0,
          );

          crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
          return Ok(());
        }

        let old_satpoint = SatPoint {
          outpoint: txin.previous_output,
          offset: old_satpoint_offset,
        };

        // let inscription_id = InscriptionEntry::load(
        // self
        //   .sequence_number_to_entry
        //   .get(sequence_number)?
        //   .unwrap()
        //   .value(),
        // )
        // .id;

        let inscription_id = mem_get_sequence_number_to_entry(sequence_number)
          .unwrap()
          .id;

        let offset = total_input_value + old_satpoint_offset;
        floating_inscriptions.push(Flotsam {
          offset,
          inscription_id,
          origin: Origin::Old {
            sequence_number,
            old_satpoint,
          },
        });

        inscribed_offsets
          .entry(offset)
          .or_insert((inscription_id, 0))
          .1 += 1;
      }
      if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
        self.cursor.inscription_cursor = 0;
      }

      let offset = total_input_value;

      let input_value = input_utxo_entries[input_index].total_value();
      total_input_value += input_value;

      // go through all inscriptions in this input
      while let Some(inscription) = envelopes.peek() {
        if inscription.input != u32::try_from(input_index).unwrap() {
          break;
        }

        let inscription_id = InscriptionId {
          txid,
          index: id_counter,
        };

        let curse = if inscription.payload.unrecognized_even_field {
          Some(Curse::UnrecognizedEvenField)
        } else if inscription.payload.duplicate_field {
          Some(Curse::DuplicateField)
        } else if inscription.payload.incomplete_field {
          Some(Curse::IncompleteField)
        } else if inscription.input != 0 {
          Some(Curse::NotInFirstInput)
        } else if inscription.offset != 0 {
          Some(Curse::NotAtOffsetZero)
        } else if inscription.payload.pointer.is_some() {
          Some(Curse::Pointer)
        } else if inscription.pushnum {
          Some(Curse::Pushnum)
        } else if inscription.stutter {
          Some(Curse::Stutter)
        } else if let Some((id, count)) = inscribed_offsets.get(&offset) {
          if *count > 1 {
            Some(Curse::Reinscription)
          } else {
            // let initial_inscription_sequence_number =
            //   self.id_to_sequence_number.get(id.store())?.unwrap().value();
            let initial_inscription_sequence_number =
              crate::index::mem_get_inscription_id_to_sequence_number(id.store()).unwrap();

            // let entry = InscriptionEntry::load(
            //   self
            //     .sequence_number_to_entry
            //     .get(initial_inscription_sequence_number)?
            //     .unwrap()
            //     .value(),
            // );

            let entry =
              mem_get_sequence_number_to_entry(initial_inscription_sequence_number).unwrap();

            let initial_inscription_was_cursed_or_vindicated =
              entry.inscription_number < 0 || Charm::Vindicated.is_set(entry.charms);

            if initial_inscription_was_cursed_or_vindicated {
              None
            } else {
              Some(Curse::Reinscription)
            }
          }
        } else {
          None
        };

        let offset = inscription
          .payload
          .pointer()
          .filter(|&pointer| pointer < total_output_value)
          .unwrap_or(offset);

        floating_inscriptions.push(Flotsam {
          inscription_id,
          offset,
          origin: Origin::New {
            cursed: curse.is_some() && !self.jubilant,
            fee: 0,
            hidden: inscription.payload.hidden(),
            parents: inscription.payload.parents(),
            reinscription: inscribed_offsets.contains_key(&offset),
            unbound: input_value == 0
              || curse == Some(Curse::UnrecognizedEvenField)
              || inscription.payload.unrecognized_even_field,
            vindicated: curse.is_some() && self.jubilant,
          },
        });

        inscribed_offsets
          .entry(offset)
          .or_insert((inscription_id, 0))
          .1 += 1;

        envelopes.next();
        id_counter += 1;
      }
    }

    if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
      crate::index::heap_reset_staged_input_vars();
      self.cursor.input_cursor = 0;
    }

    let potential_parents = floating_inscriptions
      .iter()
      .map(|flotsam| flotsam.inscription_id)
      .collect::<HashSet<InscriptionId>>();

    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New {
          parents: purported_parents,
          ..
        },
        ..
      } = flotsam
      {
        let mut seen = HashSet::new();
        purported_parents
          .retain(|parent| seen.insert(*parent) && potential_parents.contains(parent));
      }
    }

    // still have to normalize over inscription size
    for flotsam in &mut floating_inscriptions {
      if let Flotsam {
        origin: Origin::New { fee, .. },
        ..
      } = flotsam
      {
        *fee = (total_input_value - total_output_value) / u64::from(id_counter);
      }
    }

    let is_coinbase = tx
      .input
      .first()
      .map(|tx_in| tx_in.previous_output.is_null())
      .unwrap_or_default();

    if is_coinbase {
      floating_inscriptions.append(&mut self.flotsam);
    }

    let output_cursor;
    let mut inscriptions;

    let mut new_locations;
    let mut output_value;
    if let Some(staged_input_vars) = crate::index::heap_get_staged_input_vars2() {
      new_locations = staged_input_vars.new_locations;
      output_value = staged_input_vars.output_value;
      total_input_value = staged_input_vars.total_input_value;
      output_cursor = staged_input_vars.output_cursor;
      inscriptions = staged_input_vars.inscriptions;
    } else {
      new_locations = Vec::new();
      output_value = 0;
      output_cursor = 0;

      floating_inscriptions.sort_by_key(|flotsam| flotsam.offset);
      inscriptions = floating_inscriptions.into_iter().peekable();
    }

    for (vout, txout) in tx.output.iter().enumerate().skip(output_cursor) {
      let end = output_value + txout.value.to_sat();

      while let Some(flotsam) = inscriptions.peek() {
        if flotsam.offset >= end {
          break;
        }

        let counter = ic_cdk::api::instruction_counter();
        if counter > self.instruction_counter + 30_000_000_000 {
          self.cursor.tx_cursor = if tx_offset == 0 {
            self.cursor.tx_len - 1
          } else {
            tx_offset as u64 - 1
          };
          let cursor = Cursor {
            tx_cursor: self.cursor.tx_cursor,
            tx_len: self.cursor.tx_len,
            input_cursor: self.cursor.input_len,
            input_len: self.cursor.input_len,
            inscription_cursor: 0,
            inscription_len: 0,
          };
          crate::index::mem_set_cursor(cursor);
          crate::index::heap_insert_staged_input_vars2(StagedInputVars2 {
            new_locations,
            output_value,
            total_input_value,
            output_cursor: vout,
            inscriptions,
          });
          log!(
            INFO,
            "BREAKPOINT at cursor {} output_cursor: {} with instruction_counter: {:.3}B",
            self.cursor,
            output_cursor,
            (counter - self.instruction_counter) as f64 / 1_000_000_000.0,
          );

          crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
          return Ok(());
        }

        let new_satpoint = SatPoint {
          outpoint: OutPoint {
            txid,
            vout: vout.try_into().unwrap(),
          },
          offset: flotsam.offset - output_value,
        };

        new_locations.push((
          new_satpoint,
          inscriptions.next().unwrap(),
          txout.script_pubkey.is_op_return(),
        ));
      }

      output_value = end;
    }

    if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
      crate::index::heap_reset_staged_input_vars2();
    }

    let new_locations_cursor;
    if let Some(staged_input_vars) = crate::index::heap_get_staged_input_vars3() {
      new_locations = staged_input_vars.new_locations;
      output_value = staged_input_vars.output_value;
      total_input_value = staged_input_vars.total_input_value;
      inscriptions = staged_input_vars.inscriptions;
      new_locations_cursor = staged_input_vars.new_locations_cursor;
    } else {
      new_locations_cursor = 0;
    }

    for (new_locations_index, (new_satpoint, flotsam, op_return)) in
      new_locations.iter().enumerate().skip(new_locations_cursor)
    {
      let counter = ic_cdk::api::instruction_counter();
      if counter > self.instruction_counter + 30_000_000_000 {
        self.cursor.tx_cursor = if tx_offset == 0 {
          self.cursor.tx_len - 1
        } else {
          tx_offset as u64 - 1
        };
        let cursor = Cursor {
          tx_cursor: self.cursor.tx_cursor,
          tx_len: self.cursor.tx_len,
          input_cursor: self.cursor.input_len,
          input_len: self.cursor.input_len,
          inscription_cursor: 0,
          inscription_len: 0,
        };
        crate::index::mem_set_cursor(cursor);
        crate::index::heap_insert_staged_input_vars3(StagedInputVars3 {
          new_locations,
          output_value,
          total_input_value,
          inscriptions,
          new_locations_cursor: new_locations_index,
        });
        log!(
          INFO,
          "BREAKPOINT at cursor {} new_locations_cursor: {} with instruction_counter: {:.3}B",
          self.cursor,
          new_locations_index,
          (counter - self.instruction_counter) as f64 / 1_000_000_000.0,
        );

        crate::index::BREAKPOINT.with(|m| *m.borrow_mut() = true);
        return Ok(());
      }
      let output_utxo_entry =
        &mut output_utxo_entries[usize::try_from(new_satpoint.outpoint.vout).unwrap()];

      self.update_inscription_location(
        input_sat_ranges,
        flotsam.clone(),
        new_satpoint.clone(),
        op_return.clone(),
        Some(output_utxo_entry),
        utxo_cache,
        index,
      )?;
    }

    if !crate::index::BREAKPOINT.with(|m| *m.borrow()) {
      crate::index::heap_reset_staged_input_vars3();
    }

    if is_coinbase {
      for flotsam in inscriptions {
        let new_satpoint = SatPoint {
          outpoint: OutPoint::null(),
          offset: self.lost_sats + flotsam.offset - output_value,
        };
        self.update_inscription_location(
          input_sat_ranges,
          flotsam,
          new_satpoint,
          false,
          None,
          utxo_cache,
          index,
        )?;
      }
      self.lost_sats += self.reward - output_value;
      Ok(())
    } else {
      self.flotsam.extend(inscriptions.map(|flotsam| Flotsam {
        offset: self.reward + flotsam.offset - output_value,
        ..flotsam
      }));
      self.reward += total_input_value - output_value;
      Ok(())
    }
  }

  fn calculate_sat(input_sat_ranges: Option<&Vec<Vec<u8>>>, input_offset: u64) -> Option<Sat> {
    let input_sat_ranges = input_sat_ranges?;

    let mut offset = 0;
    for chunk in input_sat_ranges
      .iter()
      .flat_map(|slice| slice.chunks_exact(11))
    {
      let (start, end) = SatRange::load(chunk.try_into().unwrap());
      let size = end - start;
      if offset + size > input_offset {
        let n = start + input_offset - offset;
        return Some(Sat(n));
      }
      offset += size;
    }

    unreachable!()
  }

  fn update_inscription_location(
    &mut self,
    input_sat_ranges: Option<&Vec<Vec<u8>>>,
    flotsam: Flotsam,
    new_satpoint: SatPoint,
    op_return: bool,
    mut normal_output_utxo_entry: Option<&mut UtxoEntryBuf>,
    utxo_cache: &mut BTreeMap<OutPoint, UtxoEntryBuf>,
    index: &Index,
  ) -> Result {
    let inscription_id = flotsam.inscription_id;
    let (unbound, sequence_number) = match flotsam.origin {
      Origin::Old {
        sequence_number,
        old_satpoint,
      } => {
        if op_return {
          // let entry = InscriptionEntry::load(
          //   self
          //     .sequence_number_to_entry
          //     .get(&sequence_number)?
          //     .unwrap()
          //     .value(),
          // );

          let entry = mem_get_sequence_number_to_entry(sequence_number).unwrap();

          let mut charms = entry.charms;
          Charm::Burned.set(&mut charms);

          // self.sequence_number_to_entry.insert(
          //   sequence_number,
          //   &InscriptionEntry { charms, ..entry }.store(),
          // )?;

          crate::index::mem_insert_sequence_number_to_entry(
            sequence_number,
            InscriptionEntry { charms, ..entry },
          );
          self
            .ordinals_change_record
            .added_seq_to_inscription
            .push(sequence_number);
        }

        // if let Some(ref sender) = index.event_sender {
        //   sender.blocking_send(Event::InscriptionTransferred {
        //     block_height: self.height,
        //     inscription_id,
        //     new_location: new_satpoint,
        //     old_location: old_satpoint,
        //     sequence_number,
        //   })?;
        // }

        log!(
          DEBUG,
          "Inscription Transferred: block_height: {}, inscription_id: {}, new_location: {}, old_location: {:?}, sequence_number: {}",
          self.height,
          inscription_id,
          new_satpoint,
          old_satpoint,
          sequence_number
        );

        (false, sequence_number)
      }
      Origin::New {
        cursed,
        fee,
        hidden: _,
        parents,
        reinscription,
        unbound,
        vindicated,
      } => {
        let inscription_number = if cursed {
          let number: i32 = self.cursed_inscription_count.try_into().unwrap();
          self.cursed_inscription_count += 1;
          -(number + 1)
        } else {
          let number: i32 = self.blessed_inscription_count.try_into().unwrap();
          self.blessed_inscription_count += 1;
          number
        };

        let sequence_number = self.next_sequence_number;
        self.next_sequence_number += 1;

        // self
        //   .inscription_number_to_sequence_number
        //   .insert(inscription_number, sequence_number)?;

        crate::index::mem_insert_inscription_number_to_sequence_number(
          InscriptionNumber(inscription_number),
          sequence_number,
        );
        self
          .ordinals_change_record
          .added_inscription_number
          .push(InscriptionNumber(inscription_number));

        let sat = if unbound {
          None
        } else {
          Self::calculate_sat(input_sat_ranges, flotsam.offset)
        };

        let mut charms = 0;

        if cursed {
          Charm::Cursed.set(&mut charms);
        }

        if reinscription {
          Charm::Reinscription.set(&mut charms);
        }

        if let Some(sat) = sat {
          charms |= sat.charms();
        }

        if op_return {
          Charm::Burned.set(&mut charms);
        }

        if new_satpoint.outpoint == OutPoint::null() {
          Charm::Lost.set(&mut charms);
        }

        if unbound {
          Charm::Unbound.set(&mut charms);
        }

        if vindicated {
          Charm::Vindicated.set(&mut charms);
        }

        if let Some(Sat(n)) = sat {
          // self.sat_to_sequence_number.insert(&n, &sequence_number)?;
          crate::index::mem_insert_sat_to_sequence_number(n, sequence_number);
          self
            .ordinals_change_record
            .added_sat_to_seq
            .push((n, sequence_number));
        }

        let parent_sequence_numbers = parents
          .iter()
          .map(|parent| {
            // let parent_sequence_number = self
            //   .id_to_sequence_number
            //   .get(&parent.store())?
            //   .unwrap()
            //   .value();
            let parent_sequence_number =
              crate::index::mem_get_inscription_id_to_sequence_number(parent.store()).unwrap();

            // self
            //   .sequence_number_to_children
            //   .insert(parent_sequence_number, sequence_number)?;

            crate::index::mem_insert_sequence_number_to_child(
              parent_sequence_number,
              sequence_number,
            );
            self
              .ordinals_change_record
              .added_seq_to_children
              .push((parent_sequence_number, sequence_number));

            Ok(parent_sequence_number)
          })
          .collect::<Result<Vec<u32>>>()?;

        // if let Some(ref sender) = index.event_sender {
        //   sender.blocking_send(Event::InscriptionCreated {
        //     block_height: self.height,
        //     charms,
        //     inscription_id,
        //     location: (!unbound).then_some(new_satpoint),
        //     parent_inscription_ids: parents,
        //     sequence_number,
        //   })?;
        // }

        log!(
          DEBUG,
          "Inscription Created: block_height: {}, charms: {}, inscription_id: {}, location: {:?}, parent_inscription_ids: {:?}, sequence_number: {}",
          self.height,
          charms,
          inscription_id,
          (!unbound).then_some(new_satpoint),
          parents,
          sequence_number
        );

        // self.sequence_number_to_entry.insert(
        //   sequence_number,
        //   &InscriptionEntry {
        //     charms,
        //     fee,
        //     height: self.height,
        //     id: inscription_id,
        //     inscription_number,
        //     parents: parent_sequence_numbers,
        //     sat,
        //     sequence_number,
        //     timestamp: self.timestamp,
        //   }
        //   .store(),
        // )?;

        crate::index::mem_insert_sequence_number_to_entry(
          sequence_number,
          InscriptionEntry {
            charms,
            fee,
            height: self.height,
            id: inscription_id,
            inscription_number,
            parents: parent_sequence_numbers,
            sat,
            sequence_number,
            timestamp: self.timestamp,
          },
        );
        self
          .ordinals_change_record
          .added_seq_to_inscription
          .push(sequence_number);

        // self
        //   .id_to_sequence_number
        //   .insert(&inscription_id.store(), sequence_number)?;

        crate::index::mem_insert_inscription_id_to_sequence_number(
          inscription_id.store(),
          sequence_number,
        );
        self
          .ordinals_change_record
          .added_inscription_id
          .push(inscription_id);

        // if !hidden {
        // self
        //   .home_inscriptions
        //   .insert(&sequence_number, inscription_id.store())?;

        // if self.home_inscription_count == 100 {
        //   self.home_inscriptions.pop_first()?;
        // } else {
        //   self.home_inscription_count += 1;
        // }
        // }

        (unbound, sequence_number)
      }
    };

    let satpoint = if unbound {
      let new_unbound_satpoint = SatPoint {
        outpoint: unbound_outpoint(),
        offset: self.unbound_inscriptions,
      };
      self.unbound_inscriptions += 1;
      normal_output_utxo_entry = None;
      new_unbound_satpoint
    } else {
      new_satpoint
    };

    // The special outpoints, i.e., the null outpoint and the unbound outpoint,
    // don't follow the normal rules. Unlike real outputs they get written to
    // more than once. So we create a new UTXO entry here and commit() will
    // merge it with any existing entry.
    let output_utxo_entry = normal_output_utxo_entry.unwrap_or_else(|| {
      assert!(crate::index::is_special_outpoint(satpoint.outpoint));
      utxo_cache
        .entry(satpoint.outpoint)
        .or_insert(UtxoEntryBuf::empty(index))
    });

    output_utxo_entry.push_inscription(sequence_number, satpoint.offset, index);

    Ok(())
  }
}

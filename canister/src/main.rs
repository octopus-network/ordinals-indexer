use bitcoin::OutPoint;
use candid::candid_method;
use ic_canister_log::log;
use ic_cdk::api::management_canister::http_request::{HttpResponse, TransformArgs};
use ic_cdk_macros::{init, post_upgrade, query, update};
use ordinals_indexer::config::OrdinalsIndexerArgs;
use ordinals_indexer::index::Statistic;
use ordinals_indexer::index::entry::StagedState;
use ordinals_indexer::logs::INFO;
use std::str::FromStr;

#[query]
#[candid_method(query)]
pub fn get_latest_block() -> (u32, String) {
  let (height, hash) = ordinals_indexer::index::mem_latest_block().expect("No block found");
  (height, hash.to_string())
}

#[query(hidden = true)]
pub fn rpc_transform(args: TransformArgs) -> HttpResponse {
  let headers = args
    .response
    .headers
    .into_iter()
    .filter(|h| ordinals_indexer::rpc::should_keep(h.name.as_str()))
    .collect::<Vec<_>>();
  HttpResponse {
    status: args.response.status.clone(),
    body: args.response.body.clone(),
    headers,
  }
}

#[update(hidden = true)]
pub fn start() -> Result<(), String> {
  let caller = ic_cdk::api::caller();
  if !ic_cdk::api::is_controller(&caller) {
    return Err("Not authorized".to_string());
  }

  ordinals_indexer::index::cancel_shutdown();
  let config = ordinals_indexer::index::mem_get_config();
  let _ = ordinals_indexer::index::updater::update_index(config.network, StagedState::Index);

  Ok(())
}

#[update(hidden = true)]
pub fn stop() -> Result<(), String> {
  let caller = ic_cdk::api::caller();
  if !ic_cdk::api::is_controller(&caller) {
    return Err("Not authorized".to_string());
  }

  ordinals_indexer::index::shut_down();
  log!(INFO, "Waiting for index thread to finish...");

  Ok(())
}

#[update(hidden = true)]
pub fn set_bitcoin_rpc_url(url: String) -> Result<(), String> {
  let caller = ic_cdk::api::caller();
  if !ic_cdk::api::is_controller(&caller) {
    return Err("Not authorized".to_string());
  }
  let mut config = ordinals_indexer::index::mem_get_config();
  config.bitcoin_rpc_url = url;
  ordinals_indexer::index::mem_set_config(config).unwrap();

  Ok(())
}

#[query(hidden = true)]
fn http_request(
  req: ic_canisters_http_types::HttpRequest,
) -> ic_canisters_http_types::HttpResponse {
  if ic_cdk::api::data_certificate().is_none() {
    ic_cdk::trap("update call rejected");
  }
  if req.path() == "/logs" {
    ordinals_indexer::logs::do_reply(req)
  } else {
    ic_canisters_http_types::HttpResponseBuilder::not_found().build()
  }
}

#[init]
#[candid_method(init)]
fn init(ordinals_indexer_args: OrdinalsIndexerArgs) {
  match ordinals_indexer_args {
    OrdinalsIndexerArgs::Init(config) => {
      ordinals_indexer::index::mem_set_config(config).unwrap();
    }
    OrdinalsIndexerArgs::Upgrade(_) => ic_cdk::trap(
      "Cannot initialize the canister with an Upgrade argument. Please provide an Init argument.",
    ),
  }
}

#[post_upgrade]
fn post_upgrade(ordinals_indexer_args: Option<OrdinalsIndexerArgs>) {
  match ordinals_indexer_args {
    Some(OrdinalsIndexerArgs::Upgrade(Some(upgrade_args))) => {
      let mut config = ordinals_indexer::index::mem_get_config();
      if let Some(bitcoin_rpc_url) = upgrade_args.bitcoin_rpc_url {
        config.bitcoin_rpc_url = bitcoin_rpc_url;
      }
      ordinals_indexer::index::mem_set_config(config).unwrap();
    }
    None | Some(OrdinalsIndexerArgs::Upgrade(None)) => {}
    _ => ic_cdk::trap(
      "Cannot upgrade the canister with an Init argument. Please provide an Upgrade argument.",
    ),
  }
}

#[query]
#[candid_method(query)]
fn get_inscriptions_for_output(outpoint: String) -> Result<Option<Vec<String>>, String> {
  let outpoint = OutPoint::from_str(&outpoint).map_err(|e| e.to_string())?;
  let inscriptions = ordinals_indexer::index::inner_get_inscriptions_for_output(outpoint)
    .map_err(|e| e.to_string())?;
  Ok(inscriptions.map(|inscriptions| {
    inscriptions
      .iter()
      .map(|inscription_id| inscription_id.to_string())
      .collect()
  }))
}

#[update]
fn print_statistic() {
  log!(
    INFO,
    "Index statistics: latest_block: {:?},
    lost_sats: {},
    cursed_inscription_count: {},
    blessed_inscription_count: {},
    unbound_inscriptions: {},
    sequence_number_to_children: {},

    height_to_last_sequence_number: {},
    sat_to_satpoint: {},

    inscription_id_to_sequence_number: {},
    inscription_number_to_sequence_number: {},
    sequence_number_to_inscription_entry: {},
    sequence_number_to_satpoint: {},

    sat_to_sequence_number: {},

    outpoint_to_utxo_entry: {}",
    ordinals_indexer::index::mem_latest_block(),
    ordinals_indexer::index::mem_get_statistic_to_count(&Statistic::LostSats.key()).unwrap_or(0),
    ordinals_indexer::index::mem_get_statistic_to_count(&Statistic::CursedInscriptions.key())
      .unwrap_or(0),
    ordinals_indexer::index::mem_get_statistic_to_count(&Statistic::BlessedInscriptions.key())
      .unwrap_or(0),
    ordinals_indexer::index::mem_get_statistic_to_count(&Statistic::UnboundInscriptions.key())
      .unwrap_or(0),
    ordinals_indexer::index::mem_length_sequence_number_to_children(),
    ordinals_indexer::index::mem_length_height_to_last_sequence_number(),
    ordinals_indexer::index::mem_length_sat_to_satpoint(),
    ordinals_indexer::index::mem_length_inscription_id_to_sequence_number(),
    ordinals_indexer::index::mem_length_inscription_number_to_sequence_number(),
    ordinals_indexer::index::mem_length_sequence_number_to_inscription_entry(),
    ordinals_indexer::index::mem_length_sequence_number_to_satpoint(),
    ordinals_indexer::index::mem_length_sat_to_sequence_number(),
    ordinals_indexer::index::mem_length_outpoint_to_utxo_entry(),
  );
}

ic_cdk::export_candid!();

fn main() {}

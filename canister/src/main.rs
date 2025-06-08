use bitcoin::{OutPoint, Txid};
use candid::{Principal, candid_method};
use ic_canister_log::log;
use ic_cdk::api::management_canister::http_request::{HttpResponse, TransformArgs};
use ic_cdk_macros::{init, post_upgrade, query, update};
use ordinals_indexer::config::OrdinalsIndexerArgs;
use ordinals_indexer::logs::{CRITICAL, INFO, WARNING};
use ordinals_indexer_interface::{Error, GetEtchingResult, RuneBalance, RuneEntry, Terms};
use std::str::FromStr;

pub const MAX_OUTPOINTS: usize = 256;

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
  let _ = ordinals_indexer::index::updater::update_index(config.network);

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

ic_cdk::export_candid!();

fn main() {}

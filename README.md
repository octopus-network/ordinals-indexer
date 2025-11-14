# Ordinals Indexer

An on-chain Bitcoin Ordinals indexer running entirely inside an Internet Computer (ICP) canister.

Ordinals Indexer is Omnity Network’s second on-chain indexer after the [Runes Indexer](https://github.com/octopus-network/runes-indexer). It is deployed on the Internet Computer and implements essentially all indexing functionality of [`ord`](https://github.com/ordinals/ord) **except** for runes indexing.

---

## Overview

Ordinals Indexer is a fully on-chain implementation of the Bitcoin Ordinals protocol:

- **Runs 100% inside an ICP canister**  
  No centralized servers, no custom indexer infra, no trusted API gateway.

- **Indexes the Bitcoin chain from genesis**  
  All Bitcoin blocks are fetched, verified, and indexed onchain using ICP’s native Bitcoin integration.

- **Ord-compatible indexing (minus runes)**  
  Mirrors `ord`’s logic for sats, sequence numbers, inscription numbers, and inscription IDs, but does **not** index runes.

- **Production-ready infrastructure**  
  Builds on the same architecture as the on-chain [Runes Indexer](https://github.com/octopus-network/runes-indexer), which already powers the majority of Runes issuance and trading activity on ICP.

## Deployment

We deploy Ordinals Indexer for both Bitcoin mainnet and testnet4 on ICP.

- **Testnet canister ID**

  `krhn4-hiaaa-aaaao-qkb3a-cai`

- **Mainnet canister ID**

  `t5v7z-6iaaa-aaaai-atihq-cai`

You can view the canisters on the ICP dashboard:

- Mainnet: https://dashboard.internetcomputer.org/canister/t5v7z-6iaaa-aaaai-atihq-cai  
- Testnet: https://dashboard.internetcomputer.org/canister/krhn4-hiaaa-aaaao-qkb3a-cai

---

## API Reference

At the moment, Ordinals Indexer exposes a single public query endpoint.

### `get_inscriptions_for_output`

```candid
get_inscriptions_for_output : (text) -> (Result) query;
```

- **Input**

  - `text`: A Bitcoin output specified as `txid:vout`, e.g.  
    `aae1abaa3ddd45acfce1eac4b186043b863c75caadc7d73cd2e17a6e33337cd6:1`

- **Return**

  - `variant { Ok = opt vec text; Err = text }`  
    On success, returns an optional vector of inscription IDs attached to that output.

- **Example**

  ```candid
  get_inscriptions_for_output(
    "aae1abaa3ddd45acfce1eac4b186043b863c75caadc7d73cd2e17a6e33337cd6:1"
  )

  =>
  (variant {
    Ok = opt vec {
      "a98b955bda5992ff7724704926c178fee2594864700f718204193202b6346227i0"
    }
  })
  ```

Currently this is the only public API, but internally the indexer maintains full Ordinals state, including:

- **Sat** assignments  
- **Sequence number**  
- **Inscription number**  
- **Inscription ID**

If you need additional query APIs (e.g. lookups by sat, inscription number, or other ord-compatible views), please open an issue or contact the Omnity team — the canister is ready to expose more endpoints as needed.

---

## Design and Architecture

Ordinals Indexer shares the same high-level architecture as Runes Indexer:

- **Data source: Bitcoin RPC provider**

  - The canister continuously fetches Bitcoin blocks through HTTPS outcalls to a Bitcoin RPC provider.

- **Verification: ICP Bitcoin canister**

  - All blocks are verified using ICP’s native [Bitcoin integration](https://internetcomputer.org/bitcoin).  
  - The indexer is designed to correctly handle Bitcoin chain reorganizations (reorgs).

- **Storage: Stable memory as Ord-like KV store**

  - Ord’s `redb` key-value database is replaced by canister **stable memory** in this implementation.  
  - This enables persistence across canister upgrades and supports the large dataset required for sats and inscriptions.

- **Scalability: Cycles-aware incremental indexing**

  - Bitcoin mainnet currently has roughly:
    - ~330M UTXOs  
    - ~110M inscriptions  
  - Ordinals indexing is computationally heavy and cannot be processed in a single call per block.  
  - The canister introduces **multiple checkpoints / breakpoints** during block processing and completes the indexing of a block over multiple iterations, respecting cycles limits and ICP execution constraints.

This architecture allows the entire Ordinals protocol logic to execute onchain while remaining practical and cost-efficient.

---

## License

The license will follow that of the `runes-indexer` project (see the repository’s `LICENSE` file for exact terms).
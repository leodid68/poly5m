# poly5m — Bot d'Arbitrage Polymarket 5 Minutes BTC

## Vue d'ensemble

Bot Rust qui exploite le décalage entre les prix d'oracle (Chainlink Data Streams) et les prix affichés sur les marchés binaires 5 minutes de Polymarket pour le BTC (UP/DOWN).

Polymarket utilise Chainlink Data Streams (pull-based, sub-seconde) pour résoudre ses marchés. Les market makers ajustent leurs quotes avec un léger retard. L'objectif est de détecter ce retard et placer des ordres avant correction.

## Architecture actuelle

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│  Chainlink   │────▶│   Strategy       │────▶│   Polymarket     │
│  (prix BTC)  │     │   (décision)     │     │   (ordres)       │
│              │     │                  │     │                  │
│ latestRound  │     │ time-aware prob  │     │ CLOB API         │
│ Data() via   │     │ half-Kelly size  │     │ EIP-712 signing  │
│ alloy RPC    │     │ session limits   │     │ HMAC-SHA256 L2   │
│ racing multi │     │                  │     │ FOK orders       │
└──────────────┘     └──────────────────┘     └──────────────────┘
```

## Stack technique

- **Rust 2021 edition** avec `alloy` (pas ethers)
- **alloy** : provider HTTP, sol! macro, EIP-712 signing, PrivateKeySigner
- **reqwest** : HTTP client (Polymarket API)
- **tokio** : async runtime multi-thread
- **HMAC-SHA256 + base64** : auth Level 2 Polymarket
- **Profil release** : LTO fat, codegen-units=1, panic=abort, strip

## Structure des fichiers

```
poly5m/
├── Cargo.toml           # Dépendances (alloy, tokio, reqwest, hmac, etc.)
├── config.toml          # Configuration runtime (NE PAS COMMIT — dans .gitignore)
├── CLAUDE.md            # Ce fichier (contexte pour Claude)
├── GUIDE.md             # Analyse détaillée de la chaîne de données et des edges
├── TICKETS.md           # Plan d'amélioration + specs détaillées par ticket
└── src/
    ├── main.rs          # Entry point, boucle 5min, RPC racing, résolution
    ├── chainlink.rs     # fetch_price() via alloy eth_call + ABI decode
    ├── polymarket.rs    # Client CLOB: find market, midpoint, place_order (EIP-712)
    └── strategy.rs      # evaluate(), prob model (CDF normale), half-Kelly, Session
```

## Concepts clés du code

### RPC Racing (main.rs)
`fetch_racing()` lance des appels simultanés vers tous les RPC providers et prend la première réponse. Utilise `futures::select_ok`.

### Modèle de probabilité time-aware (strategy.rs)
```
vol_résiduelle = 0.12% × √(seconds_remaining / 300)
z = price_change_pct / vol_résiduelle
probabilité_UP = CDF_normale(z)
```
Plus on approche de la fin de l'intervalle, plus la vol résiduelle est faible, plus le z-score est grand, plus on est confiant sur la direction.

### Sizing demi-Kelly (strategy.rs)
```
b = (1 - price) / price
kelly = (b × p - q) / b
size = (kelly / 2) × max_bet
```

### Auth Polymarket (polymarket.rs)
- **EIP-712** : signe un struct `Order` avec le domain `Polymarket CTF Exchange` sur chain 137 (Polygon)
- **HMAC-SHA256** : headers `POLY_SIGNATURE`, `POLY_TIMESTAMP`, `POLY_API_KEY`, `POLY_PASSPHRASE`, `POLY_ADDRESS`
- **Ordres FOK** (Fill-Or-Kill) : soit l'ordre est entièrement exécuté, soit il est annulé

### Dry-run mode
Si `strategy.dry_run = true` dans config.toml, le bot simule les trades sans appeler l'API Polymarket. Utile pour valider la logique.

### Résolution des bets (main.rs)
À chaque nouvel intervalle 5min, le bot compare le prix Chainlink actuel au `start_price` du bet précédent pour déterminer WIN/LOSS et calculer le PnL.

## Config actuelle (config.toml)

```toml
[chainlink]
rpc_urls = ["url1", "url2", "url3"]  # Multi-RPC racing
btc_usd_feed = "0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c"
poll_interval_ms = 100

[polymarket]
api_key = "..."
api_secret = "..."      # Base64 URL-safe encoded
passphrase = "..."
private_key = "0x..."   # Clé privée Polygon wallet

[strategy]
max_bet_usdc = 2.0
min_edge_pct = 2.0
entry_seconds_before_end = 10
session_profit_target_usdc = 20.0
session_loss_limit_usdc = 10.0
dry_run = false
```

## Ce que le bot fait actuellement

1. Poll Chainlink `latestRoundData()` toutes les 100ms via RPC racing
2. Détecte les intervalles 5min (window = timestamp / 300 × 300)
3. Enregistre le prix de début d'intervalle
4. Dans les 10 dernières secondes, fetch le midpoint Polymarket et évalue le signal
5. Si edge > min_edge_pct → place un ordre FOK (ou simule en dry-run)
6. Résout le bet précédent au début de l'intervalle suivant

## Limitations connues (à corriger — voir TICKETS.md)

1. **Pas de frais dynamiques** : le bot ne tient pas compte des taker fees Polymarket (jusqu'à 3.15% à 50/50). Il trade potentiellement à perte.
2. **Source de prix sous-optimale** : `latestRoundData()` on-chain a un heartbeat d'~1h pour BTC/USD. Polymarket utilise Data Streams (sub-seconde), pas ce contrat.
3. **Pas de filtre zone 50/50** : le bot peut trader quand les frais sont maximaux.
4. **Vol statique** : la vol 5min est hardcodée à 0.12% au lieu d'être calculée dynamiquement.
5. **Pas de logging CSV** : impossible de backtester sans données historiques.

## Améliorations planifiées (TICKETS.md)

Les tickets sont ordonnés par priorité et dépendances. Chaque ticket a des specs précises avec les fichiers à modifier, le code à ajouter, et les tests attendus.

| Ticket | Description | Fichiers |
|--------|-------------|----------|
| T1 | Frais dynamiques dans evaluate() | strategy.rs, config.toml |
| T2 | Query fee-rate API avant chaque trade | polymarket.rs, main.rs |
| T3 | Filtre zone de prix (éviter 50/50) | strategy.rs, config.toml |
| T4 | WebSocket exchanges (Binance+Coinbase+Kraken) | exchanges.rs (NOUVEAU), Cargo.toml |
| T5 | Médiane multi-exchange dans evaluate() | main.rs, strategy.rs |
| T6 | Volatilité dynamique (VolTracker) | strategy.rs, main.rs, config.toml |
| T7 | Mode mixte Chainlink + exchanges WS | strategy.rs, main.rs, config.toml |
| T8 | Logging CSV pour backtesting | logger.rs (NOUVEAU), main.rs |

**Ordre d'exécution** : T1 → T3 → T2 → T4 → T5 → T6 → T7 → T8

**Règle** : `cargo test` doit passer après chaque ticket.

## Contexte marché important

### Frais dynamiques Polymarket (janvier 2026)
```
Fee = C × (feeRateBps / 10000) × [p × (1 - p)]^2
```
feeRateBps = 1000 pour les marchés crypto 5min/15min. À p=0.50 → ~3.15% de frais. À p=0.80 → ~1.28%. Makers = 0 frais + rebates.

### Settlement
Polymarket utilise Chainlink Data Streams (pas `latestRoundData()`). Le settlement compare `start_price` et `end_price` via Data Streams. En cas d'égalité, UP gagne (règle >=).

### Gamma API slugs
Format : `btc-updown-5m-{unix_timestamp_du_window}`

### Contrats
- CTF Exchange : `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` (Polygon)
- Chainlink BTC/USD (on-chain, PAS utilisé pour settlement) : `0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c`

## Conventions de code

- Erreurs : `anyhow::Result` partout, `.context("message")` sur chaque `?`
- Logging : `tracing` (info/warn/error/debug), pas println
- Async : `tokio` multi-thread, pas de `.block_on()`
- Serde : `#[serde(rename_all = "camelCase")]` pour les réponses API Polymarket
- Tests : dans `#[cfg(test)] mod tests` en bas de chaque fichier
- Config : `serde::Deserialize` depuis `config.toml`, converteurs `From<>` vers les structs internes

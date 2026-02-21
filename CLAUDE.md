# poly5m — Bot d'Arbitrage Polymarket 5 Minutes BTC

## Vue d'ensemble

Bot Rust qui exploite le décalage entre les prix d'oracle (Chainlink Data Streams) et les prix affichés sur les marchés binaires 5 minutes de Polymarket pour le BTC (UP/DOWN).

Polymarket utilise Chainlink Data Streams (pull-based, sub-seconde) pour résoudre ses marchés. Les market makers ajustent leurs quotes avec un léger retard. L'objectif est de détecter ce retard et placer des ordres avant correction.

## Architecture actuelle

```
┌──────────────┐
│  Exchanges   │──┐  WebSocket Binance+Coinbase+Kraken
│  (prix BTC)  │  │  médiane multi-source
└──────────────┘  │
┌──────────────┐  │  ┌──────────────────┐     ┌──────────────────┐
│  Chainlink   │──┼─▶│   Strategy       │────▶│   Polymarket     │
│  (on-chain)  │  │  │   (décision)     │     │   (ordres)       │
└──────────────┘  │  │                  │     │                  │
┌──────────────┐  │  │ pure z-score     │     │ CLOB API         │
│  RTDS        │──┘  │ Student-t CDF    │     │ EIP-712 signing  │
│  (Polymarket │     │ half-Kelly size  │     │ FOK/GTC orders   │
│   live data) │     │ session limits   │     │ maker pricing    │
└──────────────┘     │ circuit breaker  │     └──────────────────┘
                     │ vol dynamique    │
┌──────────────┐     │ regime detect    │     ┌──────────────────┐
│  Macro Data  │────▶│ auto-calibration │     │   Logger         │
│  (CoinGecko) │     └──────────────────┘     │   (51-col CSV)   │
└──────────────┘                              │   OutcomeLogger  │
                                              │   TickLogger     │
                                              └──────────────────┘
```

## Stack technique

- **Rust 2021 edition** avec `alloy` (pas ethers)
- **alloy** : provider HTTP, sol! macro, EIP-712 signing, PrivateKeySigner
- **reqwest** : HTTP client (Polymarket API, CoinGecko)
- **tokio** : async runtime multi-thread
- **tokio-tungstenite** : WebSocket client (Binance, Coinbase, Kraken, RTDS)
- **statrs** : Student-t CDF pour le modèle de probabilité
- **HMAC-SHA256 + base64** : auth Level 2 Polymarket
- **Profil release** : LTO fat, codegen-units=1, panic=abort, strip

## Structure des fichiers

```
poly5m/
├── Cargo.toml           # Dépendances (alloy, tokio, reqwest, hmac, statrs, etc.)
├── config.toml          # Configuration runtime (NE PAS COMMIT — dans .gitignore)
├── CLAUDE.md            # Ce fichier (contexte pour Claude)
├── GUIDE.md             # Analyse détaillée de la chaîne de données et des edges
├── TICKETS.md           # Plan d'amélioration + specs détaillées par ticket (tous complétés)
├── docs/plans/          # Design docs et plans d'implémentation
└── src/
    ├── main.rs          # Entry point, boucle 5min, RPC racing, résolution (~1038 lignes)
    ├── chainlink.rs     # fetch_price() via alloy eth_call + ABI decode (~53 lignes)
    ├── polymarket.rs    # Client CLOB: find market, midpoint, place_order, orderbook (~504 lignes)
    ├── strategy.rs      # evaluate(), modèle hybride, Session, VolTracker, WindowTicks, Calibrator (~2101 lignes)
    ├── exchanges.rs     # WebSocket multi-exchange: Binance, Coinbase, Kraken (~345 lignes)
    ├── rtds.rs          # Polymarket Real-Time Data Streams WebSocket (~202 lignes)
    ├── macro_data.rs    # Données macro CoinGecko (btc_1h_pct, btc_24h_pct, etc.) (~114 lignes)
    ├── logger.rs        # CsvLogger (51 cols), OutcomeLogger, TickLogger (~443 lignes)
    └── presets.rs       # Presets de configuration par marché (~224 lignes)
```

## Concepts clés du code

### Sources de prix (main.rs, exchanges.rs, rtds.rs, chainlink.rs)

Le bot fusionne 3 sources de prix :
- **Exchanges WS** (Binance, Coinbase, Kraken) : médiane des tickers temps réel via WebSocket
- **RTDS** (Polymarket Real-Time Data Streams) : prix live du marché Polymarket
- **Chainlink on-chain** : `latestRoundData()` via RPC racing (fallback, heartbeat ~1h)

`fetch_racing()` lance des appels simultanés vers tous les RPC providers et prend la première réponse. Utilise `futures::select_ok`.

### Modèle de probabilité pure z-score (strategy.rs)

```
vol_résiduelle = vol_dynamique × √(seconds_remaining / 300) × vol_confidence_multiplier
z = price_change_pct / vol_résiduelle

probabilité_UP = Student_t_CDF(z, df=4.0)
```

- **Vol dynamique** : `VolTracker` calcule la MAD (Median Absolute Deviation) sur les derniers N intervalles
- **Student-t CDF** : queues lourdes (df=4.0), plus conservateur que la CDF normale
- **Z-threshold filter** : skip si |z| < `min_z_score` (défaut 0.5) — bruit, pas signal
- **Model-market divergence** : skip si |model_prob - market_price| > `max_model_divergence` (défaut 0.30)
- **Book imbalance** : utilisé uniquement comme filtre (min_book_imbalance), PAS dans le modèle de probabilité
- **Regime detection** : `WindowTicks` filtre les marchés choppants (micro-vol, momentum ratio)

### Sizing demi-Kelly (strategy.rs)

```
b = (1 - price) / price
kelly = (b × p - q) / b
size = (kelly / 2) × bankroll × kelly_fraction
```

### Session et risk management (strategy.rs)

- **Session** : tracking PnL, win rate, consecutive wins/losses, drawdown
- **Circuit breaker** : pause si rolling WR < seuil sur N derniers trades
- **Max consecutive losses** : arrêt après N pertes consécutives
- **Profit target / loss limit** : arrêt de session sur seuils

### Auto-calibration (strategy.rs)

`Calibrator` recalibre `vol_confidence_multiplier` tous les N trades en comparant la vol prédite vs réalisée. Persiste dans `calibration.json`.

### Auth Polymarket (polymarket.rs)

- **EIP-712** : signe un struct `Order` avec le domain `Polymarket CTF Exchange` sur chain 137 (Polygon)
- **HMAC-SHA256** : headers `POLY_SIGNATURE`, `POLY_TIMESTAMP`, `POLY_API_KEY`, `POLY_PASSPHRASE`, `POLY_ADDRESS`
- **FOK** (Fill-Or-Kill) ou **GTC** (Good-Til-Cancelled) avec maker pricing (bid + 25% spread)

### Logging (logger.rs)

- **CsvLogger** : 51 colonnes par trade/skip/résolution dans `trades.csv`
- **OutcomeLogger** : log TOUTES les fenêtres 5min (même sans trade) dans `outcomes.csv` pour backtesting offline
- **TickLogger** : chaque tick de prix dans `ticks_YYYYMMDD.csv` avec rotation quotidienne

### Dry-run mode

Si `strategy.dry_run = true` dans config.toml, le bot simule les trades sans appeler l'API Polymarket.

## Config actuelle (config.toml)

```toml
[chainlink]
rpc_urls = ["url1", "url2", "url3"]  # Multi-RPC racing
btc_usd_feed = "0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c"
poll_interval_ms = 100
poll_interval_ms_with_ws = 1000  # Ralenti quand WS actifs

[polymarket]
api_key = "..."
api_secret = "..."
passphrase = "..."
private_key = "0x..."

[strategy]
max_bet_usdc = 2.0
min_bet_usdc = 0.10
min_edge_pct = 2.0
entry_seconds_before_end = 10
session_profit_target_usdc = 20.0
session_loss_limit_usdc = 10.0
kelly_fraction = 0.10
vol_confidence_multiplier = 4.0
min_market_price = 0.20
max_market_price = 0.80
min_payout_ratio = 1.10
min_book_imbalance = 0.05
max_vol_5min_pct = 0.50
min_ws_sources = 2
circuit_breaker_window = 20
circuit_breaker_min_wr = 0.40
circuit_breaker_cooldown_s = 600
min_implied_prob = 0.70
max_consecutive_losses = 10
student_t_df = 4.0
dry_run = false

[exchanges]
enabled = true

[rtds]
enabled = true

[logging]
csv_path = "trades.csv"
```

## Ce que le bot fait actuellement

1. Connecte les WebSockets Binance/Coinbase/Kraken + RTDS Polymarket
2. Poll Chainlink toutes les 100ms (1s si WS actifs) via RPC racing
3. Calcule la médiane des prix multi-source à chaque tick
4. Détecte les intervalles 5min, enregistre `start_price`
5. Collecte les ticks intra-window (WindowTicks) pour regime detection
6. Fetch données macro CoinGecko toutes les 5 min
7. Dans les N dernières secondes, fetch midpoint + orderbook Polymarket
8. Évalue le signal hybride (z-score + book imbalance + Student-t)
9. Filtre : frais dynamiques, zone de prix, micro-vol, momentum, circuit breaker
10. Si edge net > seuil → place un ordre FOK/GTC avec maker pricing
11. Log le trade (51 colonnes CSV) + tick logger + outcome logger
12. Résout le bet précédent au début de l'intervalle suivant
13. Auto-calibre le VCM tous les N trades

## CSV — 51 colonnes (trades.csv)

```
timestamp, hour_utc, day_of_week, window, event, btc_start, btc_current, btc_resolution,
price_change_pct, market_mid, implied_p_up, side, token, edge_brut_pct, edge_net_pct,
fee_pct, size_usdc, entry_price, order_latency_ms, fill_type, remaining_s, num_ws_src,
price_source, vol_pct, btc_1h_pct, btc_24h_pct, btc_24h_vol_m, funding_rate, spread,
bid_depth, ask_depth, book_imbalance, best_bid, best_ask, mid_vs_entry_slippage_bps,
bid_levels, ask_levels, micro_vol, momentum_ratio, sign_changes, max_drawdown_bps,
time_above_start_s, ticks_count, result, pnl, session_pnl, session_trades,
session_wr_pct, consecutive_wins, session_drawdown_pct, skip_reason
```

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
- Tests : dans `#[cfg(test)] mod tests` en bas de chaque fichier — 144 tests actuellement
- Config : `serde::Deserialize` depuis `config.toml`, converteurs `From<>` vers les structs internes
- NaN safety : `partial_cmp().unwrap_or(Ordering::Equal)` dans les sorts
- Break-even (pnl == 0.0) traité comme une perte (reset win streak)

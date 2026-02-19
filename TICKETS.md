# poly5m — Plan d'amélioration + Tickets détaillés

## Plan global (par ordre d'implémentation)

L'ordre est pensé pour que chaque ticket soit testable indépendamment et que les dépendances soient respectées.

```
TICKET 1: Frais dynamiques          ← CRITIQUE, sans ça le bot trade à perte
    │
TICKET 2: Fee-rate API query        ← Complète T1 avec les vraies données
    │
TICKET 3: Filtre zone de prix       ← Empêche de trader à 50/50 (frais max)
    │
TICKET 4: WebSocket exchanges       ← Nouveau module, source de prix rapide
    │
TICKET 5: Médiane multi-exchange    ← Combine les WS pour approximer Chainlink
    │
TICKET 6: Volatilité dynamique      ← Améliore le modèle de probabilité
    │
TICKET 7: Mode mixte Chainlink+WS   ← Garde Chainlink comme fallback
    │
TICKET 8: Logging CSV               ← Pour backtester et valider
```

---

## TICKET 1 — Intégrer les frais dynamiques dans `evaluate()`

**Priorité** : BLOQUANTE — sans ça, le bot perd de l'argent systématiquement à 50/50.

**Contexte** : Polymarket charge des taker fees dynamiques sur les marchés 5min/15min crypto. La formule est `fee = feeRateBps/10000 × [p × (1 - p)]^exponent`. Pour les marchés crypto : `feeRateBps = 1000`, `exponent = 2`.

### Fichier : `src/strategy.rs`

**Ajouter** une fonction `dynamic_fee` :

```rust
/// Calcule les frais dynamiques Polymarket.
/// fee_rate_bps = 1000 pour les marchés crypto 5min/15min
/// exponent = 2
pub fn dynamic_fee(price: f64, fee_rate_bps: u32) -> f64 {
    let p_q = price * (1.0 - price);
    (fee_rate_bps as f64 / 10000.0) * p_q.powi(2)
}
```

**Modifier** `evaluate()` : après le calcul de `edge_pct` (ligne 89-92), soustraire les frais du edge brut avant de comparer à `min_edge_pct` :

```rust
// Après: let edge_pct = edge * 100.0;
let fee = dynamic_fee(market_price, 1000); // 1000 bps pour les marchés crypto
let net_edge_pct = edge_pct - (fee * 100.0);

if net_edge_pct < config.min_edge_pct {
    return None;
}
```

**Aussi** : le log du signal (ligne 100-104) doit afficher le net_edge, pas le brut. Et `Signal.edge_pct` doit contenir le net edge.

**Modifier** `StrategyConfig` : ajouter un champ `fee_rate_bps: u32` avec default 1000 (sera overridé par le ticket 2).

### config.toml

Ajouter sous `[strategy]` :

```toml
fee_rate_bps = 1000   # 1000 bps par défaut pour les marchés crypto
```

### Tests à ajouter

```rust
#[test]
fn dynamic_fee_at_50_50() {
    let fee = dynamic_fee(0.50, 1000);
    assert!((fee - 0.00625).abs() < 0.001); // 0.25^2 * 0.1 = 0.00625
}

#[test]
fn dynamic_fee_at_80_20() {
    let fee = dynamic_fee(0.80, 1000);
    // 0.8*0.2 = 0.16, 0.16^2 = 0.0256, * 0.1 = 0.00256
    assert!(fee < 0.003);
}

#[test]
fn dynamic_fee_at_95_05() {
    let fee = dynamic_fee(0.95, 1000);
    assert!(fee < 0.0003); // quasi nul
}

#[test]
fn evaluate_rejects_when_fee_exceeds_edge() {
    let mut config = test_config();
    config.fee_rate_bps = 1000;
    let session = Session::default();
    // BTC +0.01% avec 10s restantes, marché à 50/50
    // Edge brut faible, frais de ~0.6% → net edge négatif
    let signal = evaluate(100_000.0, 100_010.0, 0.50, 10, &session, &config);
    assert!(signal.is_none());
}
```

---

## TICKET 2 — Query le fee-rate depuis l'API avant chaque trade

**Priorité** : HAUTE — le `feeRateBps` peut changer, ne pas le hardcoder.

### Fichier : `src/polymarket.rs`

**Ajouter** une méthode à `PolymarketClient` :

```rust
/// Récupère le fee_rate_bps pour un token donné.
/// Retourne 0 si le marché n'a pas de frais (marchés non-crypto).
pub async fn get_fee_rate(&self, token_id: &str) -> Result<u32> {
    let resp = self.http
        .get(format!("{CLOB_BASE}/fee-rate"))
        .query(&[("token_id", token_id)])
        .send().await?;
    // parse { "fee_rate_bps": "1000" } ou similaire
    ...
}
```

### Fichier : `src/main.rs`

Dans la fenêtre d'entrée (après `get_midpoint`), appeler `get_fee_rate` et passer le résultat à `evaluate()`.

Ajouter un paramètre `fee_rate_bps: u32` à `evaluate()`. Utiliser la valeur config comme fallback si l'API échoue.

### Tests

- Mock ou test que `get_fee_rate` parse bien `{ "fee_rate_bps": "1000" }` et `{ "fee_rate_bps": "0" }`.

---

## TICKET 3 — Filtre zone de prix (éviter le 50/50)

**Priorité** : HAUTE — les frais à 50/50 rendent le trading non rentable.

### Fichier : `src/strategy.rs`

**Ajouter** dans `evaluate()`, après la validation des inputs (ligne 67-69) :

```rust
// Rejeter si le marché est trop proche de 50/50 (zone de frais max)
if market_up_price > 0.40 && market_up_price < 0.60 {
    tracing::debug!("Skip: marché à {:.0}% (zone 50/50, frais trop élevés)", market_up_price * 100.0);
    return None;
}
```

### config.toml

Ajouter :

```toml
skip_50_50_zone = true              # Ignorer les marchés entre 40-60%
min_market_price = 0.15             # Prix minimum acceptable
max_market_price = 0.85             # Prix maximum acceptable
```

### Tests

```rust
#[test]
fn evaluate_skips_50_50_zone() {
    let config = test_config();
    let session = Session::default();
    // Marché à exactement 50% → doit être rejeté
    let signal = evaluate(100_000.0, 100_100.0, 0.50, 10, &session, &config);
    assert!(signal.is_none());
}

#[test]
fn evaluate_accepts_70_30() {
    let config = test_config();
    let session = Session::default();
    // Marché déjà à 70% UP mais Chainlink montre encore plus UP
    let signal = evaluate(100_000.0, 100_200.0, 0.70, 5, &session, &config);
    // Devrait passer (frais bas, edge potentiellement suffisant)
    // Le résultat dépend du edge net, mais au moins ça ne devrait pas être rejeté par le filtre zone
}
```

---

## TICKET 4 — Nouveau module WebSocket exchanges (Binance + Coinbase + Kraken)

**Priorité** : HAUTE — source de prix 100-200ms plus rapide que `latestRoundData()`.

### Nouveau fichier : `src/exchanges.rs`

Créer un module qui maintient des connexions WebSocket vers 3 exchanges et expose un prix agrégé.

**Structures** :

```rust
use tokio::sync::watch;
use tokio_tungstenite::connect_async;

/// Prix temps réel depuis un exchange
#[derive(Debug, Clone, Copy)]
pub struct ExchangePrice {
    pub price: f64,
    pub timestamp_ms: u64,    // Timestamp du trade
    pub exchange: Exchange,
}

#[derive(Debug, Clone, Copy)]
pub enum Exchange {
    Binance,
    Coinbase,
    Kraken,
}

/// Agrégateur multi-exchange. Spawne un task tokio par exchange.
pub struct ExchangeFeed {
    rx: watch::Receiver<AggregatedPrice>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AggregatedPrice {
    pub median_price: f64,       // Médiane des 3 exchanges
    pub binance_price: f64,
    pub coinbase_price: f64,
    pub kraken_price: f64,
    pub num_sources: u8,         // Combien d'exchanges ont répondu
    pub last_update_ms: u64,
}
```

**WebSocket endpoints** :

- Binance : `wss://stream.binance.com:9443/ws/btcusdt@trade`
  - Message JSON : `{ "p": "97150.50", "T": 1708300000000 }` (p = price, T = timestamp ms)
- Coinbase : `wss://ws-feed.exchange.coinbase.com`
  - Subscribe : `{ "type": "subscribe", "channels": ["ticker"], "product_ids": ["BTC-USD"] }`
  - Message : `{ "price": "97150.50", "time": "2026-02-18T..." }`
- Kraken : `wss://ws.kraken.com/v2`
  - Subscribe : `{ "method": "subscribe", "params": { "channel": "ticker", "symbol": ["BTC/USD"] } }`

**Interface publique** :

```rust
impl ExchangeFeed {
    /// Démarre les 3 connexions WS en background. Retourne un receiver watch.
    pub async fn start() -> Result<Self>;

    /// Dernier prix agrégé (non-bloquant, clone du watch).
    pub fn latest(&self) -> AggregatedPrice;
}
```

Chaque WS tourne dans un `tokio::spawn` séparé. Quand un trade arrive, il met à jour un `watch::Sender<ExchangePrice>`. Un task agrégateur calcule la médiane des 3 et publie dans un `watch::Sender<AggregatedPrice>`.

### Cargo.toml

Ajouter :

```toml
tokio-tungstenite = { version = "0.26", features = ["native-tls"] }
```

### config.toml

Ajouter :

```toml
[exchanges]
enabled = true
binance_ws = "wss://stream.binance.com:9443/ws/btcusdt@trade"
coinbase_ws = "wss://ws-feed.exchange.coinbase.com"
kraken_ws = "wss://ws.kraken.com/v2"
```

### Tests

- Test unitaire : `AggregatedPrice` calcule la bonne médiane avec 1, 2 et 3 sources.
- Test de parsing : les messages JSON Binance/Coinbase/Kraken se parsent correctement.

---

## TICKET 5 — Utiliser la médiane multi-exchange dans la stratégie

**Priorité** : HAUTE — dépend du ticket 4.

### Fichier : `src/main.rs`

**Modifier** la boucle principale :

1. Au démarrage, lancer `ExchangeFeed::start()` en parallèle des providers Chainlink
2. À chaque tick, lire `exchange_feed.latest()` en plus de `fetch_racing()`
3. Passer les deux prix à `evaluate()` :
   - Prix Chainlink = signal directionnel + fallback
   - Prix exchange = source rapide pour le modèle de probabilité

### Fichier : `src/strategy.rs`

**Modifier** `evaluate()` pour accepter un `Option<f64>` pour le prix exchange :

```rust
pub fn evaluate(
    start_price: f64,
    chainlink_price: f64,
    exchange_price: Option<f64>,  // NOUVEAU
    market_up_price: f64,
    seconds_remaining: u64,
    session: &Session,
    config: &StrategyConfig,
    fee_rate_bps: u32,            // NOUVEAU (ticket 2)
) -> Option<Signal> {
    // Utiliser exchange_price si disponible, sinon chainlink_price
    let current_price = exchange_price.unwrap_or(chainlink_price);
    // ... reste de la logique
}
```

Le prix exchange est préféré car il est 100-200ms plus frais. Si les WS sont down, on fallback sur Chainlink.

### Tests

- `evaluate` avec `exchange_price = Some(...)` utilise bien ce prix
- `evaluate` avec `exchange_price = None` fallback sur `chainlink_price`

---

## TICKET 6 — Volatilité dynamique au lieu de la constante 0.12%

**Priorité** : MOYENNE — améliore la précision du modèle de probabilité.

### Fichier : `src/strategy.rs`

**Modifier** `price_change_to_probability` pour accepter `vol_5min_pct` en paramètre au lieu de la constante :

```rust
fn price_change_to_probability(pct_change: f64, seconds_remaining: u64, vol_5min_pct: f64) -> f64 {
    let remaining_vol = vol_5min_pct * ((seconds_remaining as f64) / 300.0).sqrt();
    // ... reste identique
}
```

### Nouveau : `VolTracker` dans `src/strategy.rs`

```rust
/// Suit la volatilité réalisée sur les derniers intervalles.
pub struct VolTracker {
    recent_moves: VecDeque<f64>,  // Mouvements % des derniers intervalles
    max_samples: usize,
}

impl VolTracker {
    pub fn new(max_samples: usize) -> Self { ... }

    /// Enregistre le mouvement de prix d'un intervalle terminé.
    pub fn record_move(&mut self, start_price: f64, end_price: f64) { ... }

    /// Retourne la volatilité estimée (std dev des mouvements récents).
    /// Retourne 0.12 par défaut si pas assez de données.
    pub fn current_vol(&self) -> f64 { ... }
}
```

### Fichier : `src/main.rs`

Créer un `VolTracker` au démarrage. À chaque résolution d'intervalle, appeler `vol_tracker.record_move(start_price, current_price)`. Passer `vol_tracker.current_vol()` à `evaluate()`.

### config.toml

```toml
vol_lookback_intervals = 20  # Nombre d'intervalles pour calculer la vol
default_vol_pct = 0.12       # Vol par défaut si pas assez de données
```

### Tests

```rust
#[test]
fn vol_tracker_with_no_data_returns_default() {
    let vt = VolTracker::new(20);
    assert!((vt.current_vol() - 0.12).abs() < 0.001);
}

#[test]
fn vol_tracker_adapts() {
    let mut vt = VolTracker::new(5);
    // Enregistrer des mouvements de 0.2% → vol devrait être ~0.2%
    for _ in 0..5 {
        vt.record_move(100_000.0, 100_200.0);
    }
    assert!(vt.current_vol() > 0.15);
}
```

---

## TICKET 7 — Mode mixte Chainlink + WebSocket exchanges

**Priorité** : MOYENNE — solidifie le bot avec un double-check.

### Concept

Le bot utilise les deux sources en parallèle :

1. **Exchanges WS** (rapide) → source primaire pour la direction et le timing
2. **Chainlink RPC** (lent mais autoritatif) → validation et détection de staleness

Si les exchanges disent UP mais Chainlink dit DOWN, on ne trade pas (divergence = risque).

### Fichier : `src/strategy.rs`

Ajouter un check de cohérence :

```rust
// Si les deux sources divergent sur la direction, skip
if let Some(ex_price) = exchange_price {
    let chainlink_up = chainlink_price > start_price;
    let exchange_up = ex_price > start_price;
    if chainlink_up != exchange_up {
        tracing::debug!("Skip: divergence Chainlink/exchanges");
        return None;
    }
}
```

### Fichier : `src/main.rs`

Réduire le `poll_interval_ms` de Chainlink quand les exchanges WS sont actifs (ex: passer à 1000ms au lieu de 100ms pour économiser les rate limits RPC). Chainlink devient un check périodique, pas la source primaire.

### config.toml

```toml
[chainlink]
poll_interval_ms = 100          # Si exchanges désactivés
poll_interval_ms_with_ws = 1000 # Si exchanges activés (fallback only)
```

---

## TICKET 8 — Logging CSV pour backtesting

**Priorité** : BASSE — pour valider la stratégie a posteriori.

### Nouveau fichier : `src/logger.rs`

```rust
use std::fs::File;
use std::io::Write;

pub struct CsvLogger {
    file: File,
}

impl CsvLogger {
    pub fn new(path: &str) -> Result<Self> {
        let mut file = File::create(path)?;
        writeln!(file, "timestamp,window,chainlink_price,exchange_price,market_up_price,signal,edge_pct,net_edge_pct,fee_pct,size_usdc,result,pnl")?;
        Ok(Self { file })
    }

    pub fn log_tick(&mut self, ...) { ... }
    pub fn log_trade(&mut self, ...) { ... }
    pub fn log_resolution(&mut self, ...) { ... }
}
```

### Fichier : `src/main.rs`

Instancier un `CsvLogger` au démarrage. Appeler `log_tick` à chaque itération dans la fenêtre d'entrée. Appeler `log_trade` quand un ordre est placé. Appeler `log_resolution` quand un intervalle se résout.

### config.toml

```toml
[logging]
csv_path = "trades.csv"  # Vide = pas de logging CSV
```

---

## Résumé des modifications par fichier

| Fichier | Tickets |
|---------|---------|
| `src/strategy.rs` | T1, T3, T5, T6, T7 |
| `src/polymarket.rs` | T2 |
| `src/main.rs` | T2, T5, T6, T7, T8 |
| `src/exchanges.rs` (NOUVEAU) | T4 |
| `src/logger.rs` (NOUVEAU) | T8 |
| `Cargo.toml` | T4 |
| `config.toml` | T1, T2, T3, T4, T6, T7, T8 |

## Dépendances entre tickets

```
T1 ──→ T2 ──→ T3 (frais → API fees → filtre zone)
T4 ──→ T5 ──→ T7 (WS exchanges → médiane → mode mixte)
T6 indépendant (peut être fait n'importe quand)
T8 indépendant (peut être fait n'importe quand)
```

## Ordre d'exécution recommandé pour Claude Code

```
1. T1 (frais dynamiques) — cargo test doit passer
2. T3 (filtre zone 50/50) — cargo test doit passer
3. T2 (fee-rate API) — cargo test doit passer
4. T4 (module exchanges WS) — cargo test doit passer
5. T5 (intégration exchanges dans evaluate) — cargo test doit passer
6. T6 (vol dynamique) — cargo test doit passer
7. T7 (mode mixte) — cargo test doit passer
8. T8 (CSV logger) — cargo test doit passer
```

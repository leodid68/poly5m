# poly5m — Tickets de peaufinage

Tous les tickets T1-T8 originaux ont été implémentés. Ce fichier contient les corrections et améliorations restantes, classées par priorité.

**Règle** : `cargo test` doit passer après chaque ticket. Ne pas casser l'existant.

---

## P1 — BUG CRITIQUE : `feeRateBps` à zéro dans l'ordre EIP-712

**Fichier** : `src/polymarket.rs`, ligne 298

**Problème** : L'ordre est signé avec `feeRateBps: U256::ZERO`. Sur les marchés avec frais activés, Polymarket **rejette** les ordres qui ne déclarent pas le bon `feeRateBps` dans la signature EIP-712. Le CLOB valide la signature contre le `feeRateBps` attendu.

**Fix** : Ajouter un paramètre `fee_rate_bps: u32` à `place_order()` et l'utiliser dans le struct `Order` :

```rust
pub async fn place_order(
    &self,
    token_id: &str,
    side: Side,
    size_usdc: f64,
    price: f64,
    fee_rate_bps: u32,  // NOUVEAU
) -> Result<OrderResult> {
    // ...
    let order = Order {
        // ...
        feeRateBps: U256::from(fee_rate_bps),  // au lieu de U256::ZERO
        // ...
    };
    // Et dans le body JSON :
    // "feeRateBps": fee_rate_bps.to_string(),  // au lieu de "0"
```

**Aussi dans `src/main.rs`** : passer `fee_rate_bps` à `place_order()` (ligne 395).

**Test** : Vérifier que l'ordre JSON contient le bon `feeRateBps` quand on passe 1000.

---

## P2 — BUG : Résolution ne respecte pas la règle `>=` de Polymarket

**Fichier** : `src/main.rs`, ligne 262

**Problème** : Le code utilise `current_btc > bet_start` (strict). Polymarket utilise `end_price >= start_price` pour déterminer UP (égalité = UP gagne).

**Fix** :

```rust
// AVANT (ligne 262):
let went_up = current_btc > bet_start;

// APRÈS:
let went_up = current_btc >= bet_start;
```

**Test** : Ajouter un test unitaire qui vérifie que `start == end` → UP gagne.

---

## P3 — BUG : PnL ne soustrait pas les frais payés

**Fichier** : `src/main.rs`, lignes 264-268

**Problème** : Le calcul de PnL en résolution ne tient pas compte du fee payé à l'entrée. Le PnL affiché et loggé est surestimé.

**Fix** : Stocker le fee dans `pending_bet` et le soustraire du PnL :

```rust
// Modifier pending_bet pour inclure le fee :
let mut pending_bet: Option<(f64, polymarket::Side, f64, f64, f64)> = None;
//                                                            ^^^^ fee_pct

// À l'entrée :
pending_bet = Some((start_price, signal.side, signal.size_usdc, signal.price, signal.fee_pct));

// À la résolution :
let (bet_start, bet_side, bet_size, bet_price, bet_fee_pct) = pending_bet.take().unwrap();
let fee_cost = bet_size * bet_fee_pct / 100.0;
let pnl = if won {
    bet_size * (1.0 / bet_price - 1.0) - fee_cost
} else {
    -bet_size  // fee déjà perdu dans le -bet_size
};
```

**Note** : En cas de loss, on perd la mise entière. Le fee est inclus dans la perte. Donc ne soustraire le fee que sur les wins.

---

## P4 — Intégrer le spread dans le calcul d'edge

**Fichier** : `src/strategy.rs`

**Problème** : `evaluate()` calcule `net_edge = edge_brut - fee` mais ignore le spread du carnet d'ordres. En pratique, tu achètes au best ask, pas au midpoint. Le coût réel est `fee + spread/2`.

**Fix** : Ajouter un paramètre `spread: f64` à `evaluate()` :

```rust
pub fn evaluate(
    start_price: f64,
    chainlink_price: f64,
    exchange_price: Option<f64>,
    market_up_price: f64,
    seconds_remaining: u64,
    session: &Session,
    config: &StrategyConfig,
    fee_rate_bps: u32,
    vol_5min_pct: f64,
    spread: f64,  // NOUVEAU — spread du book
) -> Option<Signal> {
    // ...
    let fee = dynamic_fee(market_price, fee_rate_bps);
    let spread_cost = spread / 2.0;  // On paye la moitié du spread
    let net_edge_pct = edge_pct - (fee * 100.0) - (spread_cost * 100.0);
    // ...
}
```

**Dans `src/main.rs`** : Fetch le book AVANT `evaluate()` et passer `book.spread`.

**Réorganiser la boucle** (lignes 310-370) : fetch book en même temps que le midpoint, avant l'appel à `evaluate()`.

**Tests** :

```rust
#[test]
fn evaluate_rejects_when_spread_kills_edge() {
    let config = test_config();
    let session = Session::default();
    // Edge brut ~10.5%, fee ~0.6%, spread 0.10 (5% par côté) → net < min_edge
    let signal = evaluate(
        100_000.0, 100_050.0, None, 0.50, 10,
        &session, &config, 1000, 0.12, 0.10,
    );
    assert!(signal.is_none());
}
```

---

## P5 — Le prix exchange n'est pas passé à `evaluate()`

**Fichier** : `src/main.rs`, ligne 341

**Problème** : `evaluate()` est appelé avec `exchange_price: None` tout le temps. Le prix WS est utilisé comme `current_btc` en amont (ligne 233-234) et passé dans le paramètre `chainlink_price`. Du coup le check de divergence CL/WS dans `evaluate()` (lignes 121-133) n'est **jamais exécuté**.

**Fix** : Séparer les deux prix proprement :

```rust
// Toujours fetch Chainlink (même si WS actif)
let chainlink_price = match fetch_racing(&providers, feed).await {
    Ok(p) if now <= p.updated_at + 3700 => Some(p.price_usd),
    _ => None,
};

// Passer les deux à evaluate()
let signal = strategy::evaluate(
    start_price,
    chainlink_price.unwrap_or(current_btc),  // CL ou fallback
    ws_price,                                  // WS si dispo
    market_up_price,
    remaining, &session, &strat_config, fee_rate_bps,
    vol_tracker.current_vol(),
);
```

**Attention** : Actuellement le code skip le polling Chainlink quand `ws_price` est disponible (lignes 233-249). Il faut revoir cette logique pour que les deux sources coexistent dans la fenêtre d'entrée.

**Alternative plus simple** : si on veut garder le code simple, supprimer le check de divergence dans evaluate() (c'est du code mort actuellement). Mais le check de divergence a de la valeur, donc mieux vaut le fixer.

---

## P6 — Valider la formule de frais contre l'API

**Fichier** : `src/main.rs` et `src/strategy.rs`

**Problème** : `dynamic_fee()` calcule `(feeRateBps/10000) × [p(1-p)]²` mais on n'est pas sûr que c'est la formule exacte de Polymarket. Le fee réel pourrait être `p(1-p) × 0.0625` (sans exposant) ou une autre variante.

**Fix** : Ajouter un log de comparaison en dry-run. Dans `main.rs`, après avoir fetch `fee_rate_bps` de l'API et calculé `dynamic_fee()`, logger les deux :

```rust
let api_fee_bps = poly.get_fee_rate(&market.token_id_yes).await
    .unwrap_or(strat_config.fee_rate_bps);
let calculated_fee = strategy::dynamic_fee(market_up_price, api_fee_bps);
tracing::debug!(
    "Fee check: API bps={api_fee_bps} | calc={:.4}% | mid={:.4}",
    calculated_fee * 100.0, market_up_price
);
```

**Aussi** : L'endpoint `/fee-rate` retourne `base_fee` (ligne 92 de polymarket.rs). Vérifier que ce champ est bien le `feeRateBps` et pas autre chose. La documentation Polymarket utilise `fee_rate_bps` comme nom de champ. Si l'API retourne un format différent, adapter la struct `FeeRateResponse`.

**Test fonctionnel** : Lancer en dry-run pendant 1h et vérifier dans les logs que les valeurs sont cohérentes. Si `base_fee` retourne 0 pour tous les tokens, c'est que l'endpoint ou le parsing est mauvais.

---

## P7 — Maker au lieu de Taker pour éviter les frais

**Contexte** : Les takers paient des frais dynamiques. Les makers paient **0 frais** et reçoivent des rebates. Si le bot peut placer des ordres limit au lieu de FOK, il évite 100% des frais.

**Trade-off** : Les ordres limit ne sont pas garantis d'être filled. Pour un bot qui trade dans les 10-60 dernières secondes d'un intervalle, un ordre non-filled = opportunité manquée. Mais si le spread est large, un ordre limit au meilleur bid+1 tick a de bonnes chances d'être filled.

**Fichier** : `src/polymarket.rs`

**Option 1 — GTC avec cancel** : Placer un ordre GTC (Good-Til-Cancelled) avec un timer. Si pas filled en N secondes, cancel.

```rust
pub async fn place_limit_order(
    &self,
    token_id: &str,
    side: Side,
    size_usdc: f64,
    price: f64,
    fee_rate_bps: u32,
) -> Result<OrderResult> {
    // Comme place_order mais avec orderType: "GTC" au lieu de "FOK"
    // side dans l'EIP-712 = 0 (BUY) pour être maker
}

pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
    // DELETE /order/{order_id} avec auth HMAC
}
```

**Option 2 — GTD (Good-Til-Date)** : L'ordre expire automatiquement. Mettre `expiration` à `window_end - 5` pour être sûr qu'il expire avant la fin de l'intervalle.

**Fichier** : `src/main.rs`

Ajouter un paramètre config `order_type = "FOK"` ou `"GTC"`. Si GTC, placer l'ordre et vérifier le status après 2-3 secondes. Si pas filled, cancel et retry ou skip.

**Fichier** : `config.toml`

```toml
order_type = "FOK"  # "FOK" ou "GTC" (maker = 0 frais)
maker_timeout_s = 5  # Timeout avant cancel (si GTC)
```

**Ce ticket est optionnel** mais pourrait transformer la rentabilité du bot en éliminant 100% des frais.

---

## P8 — WS ping/pong et monitoring de connexion

**Fichier** : `src/exchanges.rs`

**Problème** : Les connexions WS n'envoient pas de ping périodique. Certains exchanges coupent les connexions idle après 24h. Le reconnect est basique (2s sleep sans backoff).

**Fix** :

1. Ajouter un ping périodique (30s) dans chaque `ws_loop` :

```rust
let mut ping_interval = tokio::time::interval(Duration::from_secs(30));

loop {
    tokio::select! {
        msg = ws.next() => {
            match msg {
                Some(Ok(Message::Text(text))) => { /* parse */ },
                Some(Ok(Message::Ping(data))) => { ws.send(Message::Pong(data)).await?; },
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),  // Connection closed
                _ => {},
            }
        }
        _ = ping_interval.tick() => {
            ws.send(Message::Ping(vec![])).await?;
        }
    }
}
```

2. Backoff exponentiel sur reconnect (2s, 4s, 8s, max 30s), reset au premier message reçu.

3. Logger le nombre de reconnexions par exchange pour monitoring.

---

## P9 — Utiliser `best_ask` au lieu de `midpoint` pour le prix d'achat

**Fichier** : `src/main.rs`

**Problème** : Le bot utilise `get_midpoint()` (ligne 322) comme prix de marché. Mais un taker achète au `best_ask`, pas au midpoint. Le midpoint surestime la valeur du token pour l'acheteur.

**Fix** : Utiliser `book.best_ask` comme prix d'entrée quand on achète, `book.best_bid` quand on vend :

```rust
// Fetch book avant evaluate
let book = if let Some(ref poly) = poly {
    poly.get_book(token_id).await.unwrap_or_default()
} else {
    polymarket::BookData::default()
};

// Utiliser best_ask/best_bid au lieu du midpoint
let execution_price = if signal.side == polymarket::Side::Buy {
    book.best_ask  // prix réel d'achat
} else {
    book.best_bid  // prix réel de vente
};
```

**Note** : Il faut fetch le book AVANT evaluate() (pas après comme actuellement), et passer le `best_ask` comme `market_up_price` pour que l'edge soit calculé sur le vrai prix d'exécution.

**Alternative** : Garder le midpoint pour `evaluate()` mais passer le spread séparément (voir P4). Le spread capture déjà cette différence.

---

## P10 — Refactoring : struct `TradeContext` pour réduire les paramètres

**Fichier** : `src/strategy.rs`

**Problème** : `evaluate()` a 10+ paramètres. C'est fragile et difficile à maintenir (surtout avec P4 qui en ajoute un de plus).

**Fix** : Regrouper dans une struct :

```rust
pub struct TradeContext {
    pub start_price: f64,
    pub chainlink_price: f64,
    pub exchange_price: Option<f64>,
    pub market_up_price: f64,
    pub seconds_remaining: u64,
    pub fee_rate_bps: u32,
    pub vol_5min_pct: f64,
    pub spread: f64,
}

pub fn evaluate(
    ctx: &TradeContext,
    session: &Session,
    config: &StrategyConfig,
) -> Option<Signal> {
    // ...
}
```

**Aussi** : Transformer `pending_bet` en une struct nommée au lieu d'un tuple `(f64, Side, f64, f64)` dans `main.rs`.

---

## Ordre d'exécution recommandé

```
P1 (feeRateBps dans l'ordre)  ← BUG CRITIQUE, sinon les ordres seront rejetés
P2 (résolution >=)             ← BUG, fausse le PnL
P3 (PnL avec frais)            ← BUG, PnL affiché est faux
P5 (exchange_price passé)      ← BUG, code mort dans evaluate()
P4 (spread dans edge)          ← Amélioration haute priorité
P9 (best_ask au lieu de mid)   ← Amélioration haute priorité
P6 (valider formule fees)      ← Vérification importante
P10 (refactoring TradeContext) ← Clean-up
P8 (WS ping/pong)              ← Stabilité
P7 (ordres maker)              ← Optionnel mais game-changer
```

**Règle** : `cargo test` doit passer après chaque ticket. Ajouter les tests indiqués.

## Résumé des modifications par fichier

| Fichier | Tickets |
|---------|---------|
| `src/strategy.rs` | P4, P5, P10 |
| `src/polymarket.rs` | P1, P7 |
| `src/main.rs` | P1, P2, P3, P4, P5, P6, P9, P10 |
| `src/exchanges.rs` | P8 |
| `config.toml` | P7 |

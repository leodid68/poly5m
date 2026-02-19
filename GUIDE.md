# Arbitrage Polymarket 5 Minutes — Analyse Complète de la Chaîne de Données

## La vraie question : d'où vient le prix BTC et qui le voit en premier ?

Pour exploiter un décalage, il faut comprendre chaque maillon de la chaîne. Le prix BTC ne naît pas sur Chainlink. Il naît sur les exchanges. Chainlink ne fait que l'agréger et le publier. Le vrai edge se cache dans les détails de chaque intermédiaire.

---

## 1. La chaîne complète, maillon par maillon

```
  BINANCE / COINBASE / KRAKEN           ← Le prix naît ici
         │ (~12-15ms)
         ▼
  AGRÉGATEURS DE DONNÉES               ← BraveNewCoin, CoinGecko, etc.
  (pondération volume, filtre outliers)
         │ (~20-40ms)
         ▼
  NOEUDS CHAINLINK (10-20 opérateurs)  ← Chaque noeud fetch les agrégateurs
  (chacun calcule sa médiane)
         │ (~50-100ms)
         ▼
  CONSENSUS OCR (Off-Chain Reporting)   ← Médiane des médianes
         │ (~100-150ms)
         ▼
  CHAINLINK DATA STREAMS               ← Rapport signé, sub-seconde
  (rapport cryptographiquement signé)      C'est CE QUE POLYMARKET UTILISE
         │ (~0ms additionnel)
         ▼
  POLYMARKET SETTLEMENT                 ← Compare prix début vs fin
  (smart contract sur Polygon)
         │
         ▼
  RÉSULTAT: UP ou DOWN
```

### Latences cumulées depuis un trade Binance

| Étape | Latence depuis le trade | Cumulé |
|-------|------------------------|--------|
| Trade exécuté sur Binance | 0ms | 0ms |
| WebSocket Binance te délivre le trade | ~12-15ms | 15ms |
| Agrégateur de données reçoit et traite | ~20-40ms | 55ms |
| Noeud Chainlink fetch l'agrégateur | ~30-50ms | 105ms |
| Consensus OCR entre les noeuds | ~50-100ms | 205ms |
| Rapport Data Streams disponible | ~0-50ms | 255ms |
| Market maker Polymarket ajuste ses quotes | ~50-200ms | 455ms |

**Point crucial** : entre le moment où Binance publie un trade et le moment où le market maker Polymarket ajuste son prix, il se passe **300-500ms**. C'est dans cette fenêtre que l'edge existe.

---

## 2. Chaque intermédiaire en détail

### Binance (et les autres exchanges)

C'est la source de vérité numéro zéro. Un trade se fait sur le carnet d'ordres de Binance, et l'information part dans plusieurs directions en parallèle.

**WebSocket Binance — chiffres réels (décembre 2025, région Tokyo AWS) :**

- Médiane : **12.4ms** pour recevoir un trade
- P95 : **34.6ms**
- P99 : **150ms+** (pics de volatilité)

Le stream le plus rapide est `btcusdt@trade` (trades individuels). Le stream `@bookTicker` (meilleur bid/ask) est parfois plus lent. Depuis mars 2025, Binance propose aussi des streams SBE (Simple Binary Encoding) avec des payloads plus petits et une latence réduite par rapport au JSON.

**Ce que ça veut dire pour toi** : tu peux voir le prix BTC **100-200ms avant que Chainlink ne le publie**. C'est un avantage réel.

### Agrégateurs de données (BraveNewCoin, CoinGecko, etc.)

Les noeuds Chainlink ne lisent pas directement Binance. Ils passent par des agrégateurs professionnels qui font trois choses : pondérer les prix par volume et liquidité, filtrer les outliers et le faux volume, et produire un prix de référence unique.

Cette couche ajoute 20-40ms de latence, mais surtout elle lisse le bruit. Un spike de prix isolé sur un seul exchange ne passera pas jusqu'à Chainlink.

**Ce que ça veut dire pour toi** : si tu lis Binance directement, tu vois les spikes AVANT le lissage. Mais attention, certains de ces spikes ne se reflèteront jamais dans le prix Chainlink final. Tu risques de trader sur du bruit.

### Noeuds Chainlink (le réseau d'opérateurs)

10 à 20 opérateurs indépendants (Infura, Fiews, LinkPool, etc.) font chacun tourner un noeud. Chaque noeud fait la même chose : fetch les agrégateurs, calcule la médiane, et soumet son rapport au réseau.

Le consensus utilise OCR (Off-Chain Reporting) : les noeuds communiquent entre eux off-chain, se mettent d'accord sur un prix, et un seul d'entre eux soumet la transaction on-chain. Ça prend 50-150ms.

### Chainlink Data Feeds vs Data Streams — la distinction cruciale

C'est là que beaucoup de gens se trompent.

**Data Feeds (l'ancien système)** : push-based, le prix est écrit on-chain via `latestRoundData()`. La mise à jour se déclenche quand le prix dévie de plus de X% OU quand le heartbeat expire (souvent 1 heure pour BTC/USD). C'est lent, conçu pour la DeFi classique, pas pour du trading haute fréquence.

**Data Streams (le nouveau système)** : pull-based, sub-seconde. Les noeuds agrègent en continu off-chain et produisent des rapports signés. L'application (ici Polymarket) pull le rapport quand elle en a besoin et le vérifie on-chain dans le même bloc. Pas d'attente de mise à jour on-chain.

**Polymarket utilise Data Streams pour les marchés 5 minutes.** Pas `latestRoundData()`. Ça veut dire que lire le contrat Chainlink on-chain (`0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c`) ne te donne PAS le même prix que celui utilisé pour le settlement. Tu lis un prix potentiellement en retard de minutes.

### Polymarket RTDS — le flux temps réel de Polymarket

Polymarket expose son propre service RTDS (Real-Time Data Streams) avec deux sources :

- **`crypto_prices`** : prix depuis Binance directement
- **`crypto_prices_chainlink`** : prix depuis Chainlink Data Streams

C'est le wrapper de Polymarket autour de ces feeds. Un client TypeScript open-source existe : `github.com/Polymarket/real-time-data-client`. C'est potentiellement la meilleure source pour approximer le prix que Polymarket utilisera pour le settlement.

### Les market makers sur Polymarket

Ils ne sont pas lents. Les spreads se sont resserrés de 4.5% en 2023 à 1.2% en 2025, ce qui montre que la compétition est intense. Ils utilisent le WebSocket Polymarket (sub-50ms), des feeds exchange directs (Binance, Coinbase), et certains ont accès à Data Streams.

Leur cycle de mise à jour : ils voient un mouvement de prix sur Binance, recalculent leur probabilité implicite, et ajustent leurs quotes. Le tout en 50-200ms selon leur sophistication.

---

## 3. Le mécanisme de settlement — comment Polymarket résout un marché

### Le flow exact

1. **T=0** : Un marché s'ouvre. Chainlink Automation enregistre le prix BTC via Data Streams (c'est le `start_price`).
2. **T=0 à T=5:00** : Le marché est ouvert. Les traders achètent UP ou DOWN sur le CLOB.
3. **T=5:00** : Chainlink Automation trigger `performUpkeep()`. Le contrat fait un `StreamsLookup` pour récupérer un rapport signé Data Streams avec le prix BTC final.
4. **Comparaison** : Si `end_price >= start_price` → UP gagne. Si `end_price < start_price` → DOWN gagne. Note : en cas d'égalité, **UP gagne** (règle `>=`).
5. **Settlement** : les USDC sont distribués instantanément sur Polygon.

### Smart contracts impliqués

- **CTF Exchange** : `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` (Polygon) — le contrat principal d'échange de Polymarket
- **UmaCtfAdapter** : `0x6A9D222616C90FcA5754cd1333cFD9b7fb6a4F74` — interface entre UMA's Optimistic Oracle et le CTF pour la résolution

### Format des slugs Gamma API

Les marchés 5 minutes suivent le format : `btc-updown-5m-{UNIX_TIMESTAMP}` où le timestamp est le début de l'intervalle. Par exemple `btc-updown-5m-1771443600` pour un intervalle commençant à ce timestamp Unix.

### Données Chainlink Data Streams (BTC/USD)

- **Feed** : `BTC/USD-RefPrice-DS-Premium-Global-003`
- **Schéma** : Crypto Advanced (V3) avec les champs `feed_id`, `benchmark_price`, `bid`, `ask`
- **Accès** : payant, sur abonnement, pas de free tier. Il faut contacter Chainlink directement.
- **SDK Rust officiel** : `github.com/smartcontractkit/data-streams-sdk` (répertoire rust/)

---

## 4. Les frais dynamiques — la formule exacte

Polymarket a introduit des frais dynamiques (taker fees) en **janvier 2026**, spécifiquement sur les marchés 5 et 15 minutes crypto. C'est l'obstacle numéro un pour toute stratégie d'arbitrage de latence.

### La formule

```
Fee = C × feeRateBps × [p × (1 - p)]^exponent
```

- **C** = nombre de contracts tradés
- **feeRateBps** = 1000 bps (10%) pour les marchés 5min et 15min crypto
- **p** = prix du marché (probabilité, entre 0 et 1)
- **exponent** = 2

La partie `p × (1 - p)` est une parabole qui peak à p=0.50 et tombe à zéro quand p approche 0 ou 1. Avec l'exposant 2, c'est encore plus punitif autour de 50%.

### Impact concret par prix de marché

| Prix marché (p) | p×(1-p) | Fee effective |
|----------------|---------|---------------|
| 0.50 (50/50) | 0.250 | ~3.15% |
| 0.60 | 0.240 | ~2.88% |
| 0.70 | 0.210 | ~2.20% |
| 0.80 | 0.160 | ~1.28% |
| 0.90 | 0.090 | ~0.41% |
| 0.95 | 0.048 | ~0.11% |

### Makers vs takers

- **Takers** : paient les frais dynamiques ci-dessus
- **Makers** : **ZÉRO frais** + reçoivent des rebates USDC quotidiens financés par les taker fees
- Les rebates sont proportionnels à ta part de liquidité maker exécutée dans chaque marché

### Endpoint API pour les frais

```
GET https://clob.polymarket.com/fee-rate?token_id={token_id}
```

Retourne `{ "fee_rate_bps": 1000 }` pour les marchés avec frais, `0` pour les marchés sans frais. **Toujours query ce endpoint** avant de placer un ordre — ne jamais hardcoder le feeRateBps.

### Impact sur la stratégie

L'edge minimum pour être rentable dépend directement du prix du marché :

- À p=0.50 : il faut **>3.15% d'edge brut** juste pour break even
- À p=0.80 : il faut **>1.28% d'edge brut** — beaucoup plus accessible
- À p=0.95 : il faut **>0.11% d'edge** — quasi-gratuit mais l'edge est aussi quasi-nul car tout le monde voit la même chose

**La zone optimale** : trader quand le marché est entre 0.70 et 0.85 (ou 0.15 et 0.30). Les frais sont modérés, et l'information peut encore être asymétrique si les market makers n'ont pas encore complètement ajusté.

---

## 5. Les edges réels — analyse mise à jour

### Edge 1 : Lire `latestRoundData()` on-chain

**Verdict : MORT pour les marchés 5min.**

Le contrat Chainlink on-chain met à jour avec un heartbeat souvent d'une heure pour BTC/USD. Polymarket ne l'utilise pas pour le settlement — ils utilisent Data Streams. Ton bot lit un prix qui peut être en retard de minutes.

Cela dit, ton code actuel utilise `latestRoundData()` comme proxy pour la direction du prix, pas comme source de settlement. Ce n'est pas optimal mais ça reste un signal directionnel valable si le prix on-chain a changé récemment. À remplacer par les WebSocket exchange pour de meilleures performances.

### Edge 2 : Lire Chainlink Data Streams directement

**Verdict : LA SOURCE DE VÉRITÉ, mais payante et compétitive.**

Data Streams te donne exactement le prix que Polymarket utilisera pour le settlement. C'est la meilleure source possible. Mais l'accès est sur abonnement (pas de free tier, prix non publics, il faut contacter Chainlink) et les market makers ont la même donnée.

Le SDK Rust officiel existe (`smartcontractkit/data-streams-sdk`), supporte REST et WebSocket, et gère l'auth HMAC automatiquement.

### Edge 3 : Lire les exchanges directement (MEILLEUR RAPPORT COÛT/PERF)

**Verdict : LE PLUS PROMETTEUR pour ton setup actuel.**

Tu vois le prix BTC 100-200ms avant Chainlink. Gratuit, pas de setup spécial. Le risque est de trader sur du bruit (mouvement sur un seul exchange qui ne se reflète pas dans Chainlink). La mitigation est de lire 3 exchanges en parallèle.

### Edge 4 : Utiliser Polymarket RTDS

**Verdict : SOUS-EXPLOITÉ, potentiellement excellent.**

Le RTDS de Polymarket expose à la fois les prix Binance et les prix Chainlink en temps réel. Le client TypeScript est open-source. C'est potentiellement la meilleure source pour un bot car tu lis directement ce que Polymarket voit, pas une approximation.

Le flux `crypto_prices_chainlink` est littéralement le prix Chainlink que Polymarket utilise. Si tu peux le lire plus vite que les market makers n'ajustent leurs quotes, tu as un edge.

### Edge 5 : Trader loin du 50/50 pour contourner les frais

**Verdict : ESSENTIEL, la clé de rentabilité post-frais.**

Ne jamais trader quand p est proche de 0.50. Attendre un mouvement clair qui pousse le marché vers 0.70+ ou 0.30-. Les frais chutent de 3.15% à ~1.28% à p=0.80, et l'information est plus certaine.

### Edge 6 : Market making au lieu de taking

**Verdict : LE PIVOT LE PLUS VIABLE à long terme.**

Les makers ne paient aucun frais et reçoivent des rebates. La stratégie : au lieu d'acheter des tokens quand tu détectes un edge, place des ordres limit des deux côtés du marché et capture le spread. Les rebates sont un bonus.

Un market maker documenté a généré $700-800/jour à son peak. Mais ça demande beaucoup plus de capital ($50K+ pour des rendements significatifs) et une gestion du risque plus sophistiquée.

### Edge 7 : Arbitrage cross-market (Polymarket vs Kalshi)

**Verdict : EXISTE mais très serré.**

Kalshi propose des marchés 15min crypto. Si Polymarket price un événement à 60% et Kalshi à 55%, tu peux acheter YES sur Kalshi et NO sur Polymarket. Mais les frais combinés (~5%+) éliminent la plupart des spreads. Et les résolutions peuvent diverger entre plateformes (interprétation différente des edge cases).

Un repo public existe : `github.com/CarlosIbCu/polymarket-kalshi-btc-arbitrage-bot`.

---

## 6. Modèle de probabilité time-aware

Ton code utilise déjà ce modèle (bien joué). Voici les détails pour référence.

**Concept** : la certitude sur la direction dépend du ratio entre le mouvement actuel et la volatilité résiduelle.

```
vol_résiduelle = vol_5min × √(temps_restant / 300)
z = mouvement_prix / vol_résiduelle
probabilité_UP = CDF_normale(z)
```

La volatilité 5 minutes du BTC est typiquement de 0.12%. Avec ce modèle, un mouvement de +0.05% donne une probabilité UP de ~65% avec 60 secondes restantes, mais ~99% avec 5 secondes restantes.

**Amélioration possible** : la vol de 0.12% est une constante. En réalité, la volatilité varie selon l'heure, le jour, et les conditions de marché. Tu pourrais calculer la vol réalisée en temps réel sur les derniers intervalles et l'utiliser comme input dynamique.

---

## 7. Ce que les données montrent sur la rentabilité

### Statistiques globales Polymarket (recherche académique 2024-2025)

- **Seulement 7.6% des wallets sont profitables** (~120,000 gagnants vs 1.5M+ perdants)
- **30% des traders individuels gagnent** (inclut les bots sophistiqués)
- Les takers perdent en moyenne **32%** sur leurs positions longshot
- Les makers perdent seulement **10%** en moyenne — 3x mieux que les takers
- **$40M d'arbitrage** ont été extraits de Polymarket entre avril 2024 et avril 2025 (étude IMDEA Networks)

### Le bot à $414K

Un cas documenté : un bot a transformé $313 en $414,000 en un mois sur les marchés 15min (BTC, ETH, SOL). 98% de win rate, $4,000-$5,000 par bet. C'est un outlier extrême, probablement avant les frais dynamiques et avec une infrastructure très optimisée.

### La réalité post-frais

Depuis janvier 2026, la stratégie pure de latency arbitrage (lire Chainlink avant le marché) est **structurellement non rentable** quand le marché est proche de 50/50. Les frais de 3.15% dépassent l'edge typique de 1-3%.

Les stratégies qui marchent encore : market making avec rebates, trading sélectif uniquement quand le marché s'éloigne de 50/50, combinaison multi-source (Binance + Coinbase + Kraken) pour une confiance maximale, et marchés 15 minutes plutôt que 5 minutes.

---

## 8. Améliorations concrètes pour le bot

### Priorité 1 : Intégrer les frais dynamiques dans la logique de décision

C'est le changement le plus critique. Sans ça, le bot trade à perte.

```rust
fn dynamic_fee(price: f64, fee_rate_bps: u32, exponent: u32) -> f64 {
    let p_q = price * (1.0 - price);
    (fee_rate_bps as f64 / 10000.0) * p_q.powi(exponent as i32)
}

fn net_edge(gross_edge: f64, market_price: f64) -> f64 {
    gross_edge - dynamic_fee(market_price, 1000, 2)
}
```

Ne trader que si `net_edge > 0`.

### Priorité 2 : Ajouter le RTDS Polymarket comme source de prix

Le client RTDS de Polymarket (`real-time-data-client`) donne accès au flux `crypto_prices_chainlink` — littéralement le prix que Polymarket va utiliser pour le settlement. C'est la meilleure source possible et c'est gratuit.

### Priorité 3 : Remplacer/compléter Chainlink on-chain par les WebSocket exchanges

Lire Binance, Coinbase et Kraken en parallèle donne un signal 100-200ms plus rapide que `latestRoundData()` et permet de calculer une médiane multi-exchange qui approxime le consensus Chainlink.

```
wss://stream.binance.com:9443/ws/btcusdt@trade
wss://ws-feed.exchange.coinbase.com  (channel "ticker", BTC-USD)
wss://ws.kraken.com  (subscribe trade XBT/USD)
```

### Priorité 4 : Filtrer par zone de prix favorable

Ne jamais trader quand `market_price` est entre 0.40 et 0.60. Attendre que le marché se déplace vers 0.70+ ou 0.30- pour profiter de frais plus bas.

### Priorité 5 : Explorer le market making

Placer des ordres limit post-only des deux côtés du marché, capturer le spread + les maker rebates. Zéro frais taker. Demande plus de capital et une gestion de l'inventaire, mais c'est le pivot le plus viable long-terme.

### Priorité 6 : Query le fee-rate avant chaque trade

```
GET https://clob.polymarket.com/fee-rate?token_id={token_id}
```

Intégrer cette donnée dans le calcul d'edge pour ne jamais trader à perte.

---

## 9. Résumé : la réalité en 2026

| Stratégie | Pré-frais (2024-2025) | Post-frais (2026) | Viable ? |
|-----------|----------------------|-------------------|----------|
| Latency arb (lire Chainlink avant le marché) | 3-5% edge/trade | Frais > edge à 50/50 | Mort à 50/50, viable >70/30 |
| Cross-market (Polymarket vs Kalshi) | 2-3% spreads | <1% après frais combinés | Bots seulement |
| Market making + rebates | Spreads 4.5% | Spreads 1.2% + rebates | La plus viable |
| Multi-exchange + sélectivité | Pas nécessaire | Essentiel | L'approche recommandée |

**La stratégie optimale pour ton bot** : combiner les edges 3 (WebSocket exchanges) + 5 (éviter le 50/50) + le fee-rate query. Ne trader que quand le marché est déséquilibré (>0.70 ou <0.30), avec un signal confirmé par 3 exchanges, et un net_edge positif après frais.

---

## Références

### Polymarket
- CLOB API : `https://docs.polymarket.com/developers/CLOB/introduction`
- Frais dynamiques : `https://docs.polymarket.com/polymarket-learn/trading/fees`
- Maker rebates : `https://docs.polymarket.com/polymarket-learn/trading/maker-rebates-program`
- RTDS client : `https://github.com/Polymarket/real-time-data-client`
- CTF Exchange (Polygon) : `https://polygonscan.com/address/0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e`

### Chainlink
- Data Streams : `https://docs.chain.link/data-streams`
- Data Streams SDK Rust : `https://github.com/smartcontractkit/data-streams-sdk`
- BTC/USD Stream : `https://data.chain.link/streams/btc-usd-cexprice-streams`
- 3 niveaux d'agrégation : `https://blog.chain.link/levels-of-data-aggregation-in-chainlink-price-feeds/`

### Exchanges
- Binance WebSocket : `https://developers.binance.com/docs/derivatives/usds-margined-futures/websocket-market-streams`
- Latence HFT crypto (2025) : `https://medium.com/@laostjen/high-frequency-trading-in-crypto-latency-infrastructure-and-reality-594e994132fd`

### Recherche académique
- Arbitrage $40M sur Polymarket (IMDEA) : `https://arxiv.org/abs/2508.03474`
- Maker vs Taker performance : `https://www.ainvest.com/news/polymarket-taker-fee-model-implications-liquidity-trading-dynamics-2601/`
- LLM trading sur Polymarket : `https://arxiv.org/html/2511.03628v1`

### Presse
- Finance Magnates (frais dynamiques) : `https://www.financemagnates.com/cryptocurrency/polymarket-introduces-dynamic-fees-to-curb-latency-arbitrage-in-short-term-crypto-markets/`
- The Block (taker fees) : `https://www.theblock.co/post/384461/polymarket-adds-taker-fees-to-15-minute-crypto-markets-to-fund-liquidity-rebates`
- Polymarket + Chainlink : `https://www.prnewswire.com/news-releases/polymarket-partners-with-chainlink-to-enhance-accuracy-of-prediction-market-resolutions-302555123.html`

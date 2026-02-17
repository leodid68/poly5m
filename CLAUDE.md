# poly5m — Bot d'Arbitrage Polymarket 5 Minutes BTC

## Vue d'ensemble

Bot Rust qui exploite le décalage entre les données Chainlink (oracle) et les prix affichés sur les marchés 5 minutes de Polymarket pour le BTC.

**Principe** : Polymarket utilise Chainlink Data Feeds pour résoudre ses marchés 5 minutes (BTC UP/DOWN). Les market makers ajustent leurs prix avec un léger retard par rapport aux données Chainlink. En lisant Chainlink directement, on peut anticiper le résultat du marché et placer des ordres avant que les prix ne s'ajustent.

## Architecture

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│  Chainlink   │────▶│   Strategy       │────▶│   Polymarket     │
│  (prix BTC)  │     │   (décision)     │     │   (ordres)       │
│              │     │                  │     │                  │
│ latestRound  │     │ compare prix     │     │ CLOB API         │
│ Data() poll  │     │ calcule edge     │     │ place_order()    │
│ toutes les   │     │ Kelly sizing     │     │ HMAC-SHA256 auth │
│ 100ms        │     │ risk mgmt        │     │                  │
└──────────────┘     └──────────────────┘     └──────────────────┘
```

## Structure des fichiers

```
poly5m/
├── Cargo.toml         # Dépendances Rust
├── config.toml        # Configuration (clés API, paramètres)
├── CLAUDE.md          # Ce fichier
└── src/
    ├── main.rs        # Point d'entrée, boucle principale
    ├── chainlink.rs   # Client Chainlink (lecture prix oracle)
    ├── polymarket.rs  # Client Polymarket CLOB (ordres, orderbook)
    └── strategy.rs    # Logique de décision + risk management
```

## Configuration requise

### Prérequis

1. **Rust** : `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2. **Clé RPC Ethereum** : Alchemy, Infura ou QuickNode (plan gratuit OK)
3. **Compte Polymarket** avec credentials API (voir section Auth ci-dessous)
4. **USDC sur Polygon** : pour placer les trades

### Obtenir les credentials Polymarket

1. Va sur [polymarket.com](https://polymarket.com) et connecte ton wallet
2. L'API utilise un système d'auth à 2 niveaux :
   - **Level 1** : Signe un message EIP-712 avec ta clé privée → obtient `api_key` + `api_secret` + `passphrase`
   - **Level 2** : Chaque requête est signée en HMAC-SHA256
3. Le bot gère le Level 2 automatiquement. Tu dois faire le Level 1 manuellement une fois.
4. Docs : https://docs.polymarket.com/developers/CLOB/authentication

### Remplir config.toml

```toml
[chainlink]
rpc_url = "https://eth-mainnet.g.alchemy.com/v2/TA_VRAIE_CLE"
btc_usd_feed = "0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c"
poll_interval_ms = 100

[polymarket]
api_key = "..."
api_secret = "..."
passphrase = "..."
private_key = "0x..."

[strategy]
max_bet_usdc = 2.0       # $2 max par trade
min_edge_pct = 2.0        # 2% d'edge minimum pour trader
entry_seconds_before_end = 10  # Trade dans les 10 dernières secondes
session_profit_target_usdc = 20.0
session_loss_limit_usdc = 10.0
```

## Lancer le bot

```bash
# Build optimisé (LTO + strip, ~30s la première fois)
cargo build --release

# Lancer
./target/release/poly5m

# Ou en mode debug avec plus de logs
RUST_LOG=debug cargo run
```

## Comment fonctionne la stratégie

### Cycle de trading (chaque intervalle de 5 min)

1. **T=0** : Nouvel intervalle → enregistre le prix BTC via Chainlink
2. **T=0 à T=4:50** : Polling continu de Chainlink (toutes les 100ms), calcul de la direction
3. **T=4:50** : Fenêtre de trade ouverte (10s avant la fin)
4. **Évaluation** :
   - Compare le prix Chainlink actuel vs prix de début
   - Si BTC monte et le marché price UP trop bas → BUY UP
   - Si BTC descend et le marché price DOWN trop bas → BUY DOWN
   - Si l'edge est < 2% → pas de trade
5. **Sizing** : Demi-Kelly Criterion (conservateur)
6. **Exécution** : Place l'ordre via CLOB API

### Paramètres clés à ajuster

| Paramètre | Impact | Recommandation |
|---|---|---|
| `min_edge_pct` | Plus haut = moins de trades mais plus sûrs | Commence à 3%, baisse à 2% si trop peu de trades |
| `entry_seconds_before_end` | Plus tard = plus de certitude mais risque de miss | 10s est un bon compromis |
| `max_bet_usdc` | Risk par trade | $1-3 pour commencer |
| `poll_interval_ms` | Fréquence de lecture Chainlink | 100ms (ne pas descendre en dessous, rate limit RPC) |

## Améliorations possibles

### Priorité haute

- [ ] **WebSocket Chainlink** : Remplacer le polling HTTP par une souscription WebSocket pour une latence encore plus basse
- [ ] **Gestion des résultats** : Actuellement le bot ne vérifie pas automatiquement si le trade a gagné ou perdu. Ajouter un listener sur les événements de settlement
- [ ] **Retry logic** : Ajouter des retries exponentiels sur les appels API en cas d'erreur réseau

### Priorité moyenne

- [ ] **Multi-market** : Supporter ETH et d'autres assets en parallèle
- [ ] **Backtesting** : Logger les données historiques de Chainlink vs Polymarket pour valider la stratégie
- [ ] **Dashboard** : Petit serveur web local pour visualiser les trades en temps réel

### Priorité basse

- [ ] **Chainlink Data Streams** : Utiliser l'API Data Streams (sub-millisecond) au lieu de latestRoundData() pour un edge encore plus fin
- [ ] **Colocation** : Déployer sur un serveur proche des nœuds Polygon/Ethereum pour réduire la latence réseau

## Dépannage

### "Aucun marché 5min BTC actif trouvé"
Les marchés 5 minutes ne sont pas toujours actifs. Ils tournent typiquement pendant les heures de trading crypto à fort volume. Vérifie sur polymarket.com qu'il y a bien des marchés 5min ouverts.

### "Edge insuffisant"
Normal — la stratégie est conservatrice. Si tu ne vois jamais de trades, essaie de baisser `min_edge_pct` à 1.5%.

### Erreurs d'authentification Polymarket
Vérifie que tes credentials Level 1 sont valides. Ils expirent — tu dois les regénérer périodiquement.

### Rate limiting RPC
Si tu vois des erreurs 429, augmente `poll_interval_ms` à 200 ou 500. Ou utilise un plan RPC payant.

## Sécurité

⚠️ **Ne commit jamais config.toml** avec tes clés. Ajoute-le à `.gitignore`.
⚠️ **Teste d'abord avec $0.10** par trade pour valider que tout fonctionne.
⚠️ Ce bot trade avec de l'argent réel. Aucune garantie de profit.

# Arbitrage Polymarket 5 Minutes — Guide Complet

## Qu'est-ce que c'est ?

Polymarket propose des marchés binaires "5 minutes" sur le Bitcoin : est-ce que le BTC sera **UP** ou **DOWN** dans 5 minutes ? Tu peux acheter des shares "UP" ou "DOWN" entre 0 et 1 USDC. Si tu as raison, ton share vaut 1$. Si tu as tort, il vaut 0$.

Le truc, c'est que **Polymarket utilise Chainlink pour déterminer le résultat**. Chainlink est un oracle on-chain qui publie le prix BTC régulièrement. Le marché est résolu automatiquement en comparant le prix Chainlink au début et à la fin de l'intervalle de 5 minutes.

## L'edge : pourquoi ça marche

Le décalage temporel est la clé. Voici ce qui se passe :

1. **Chainlink met à jour son prix BTC** sur la blockchain Ethereum
2. **Les market makers sur Polymarket** ajustent leurs ordres en fonction de leurs propres feeds (Binance, Kraken, etc.)
3. **Il y a un délai** entre le moment où Chainlink publie une donnée et le moment où les market makers réagissent

Ce délai est généralement de quelques secondes, parfois plus. Pendant ce temps, tu peux lire directement le prix Chainlink (la source de vérité pour le settlement) et voir si le marché Polymarket est en retard.

### Exemple concret

Imaginons un intervalle 5 minutes qui commence à 14:00:00 UTC :

```
14:00:00  Intervalle démarre. Chainlink dit BTC = $97,000.00
14:00:00  Polymarket : UP = 0.50$ / DOWN = 0.50$ (50/50)

... le temps passe ...

14:04:50  Chainlink dit BTC = $97,150.00 (+0.15%)
14:04:50  Polymarket : UP = 0.55$ / DOWN = 0.45$
          → Le marché n'a pas encore complètement intégré le mouvement
          → Chainlink montre clairement que BTC est UP
          → Le token UP devrait valoir ~0.85-0.90$
          → On achète UP à 0.55$ → edge de 30-35%

14:05:00  Settlement : Chainlink confirme BTC UP
          → Notre share UP vaut 1$
          → Profit : 1$ - 0.55$ = 0.45$ par share
```

## Architecture du bot

Le bot est écrit en Rust pour la vitesse d'exécution et se compose de 3 modules :

### chainlink.rs — La source de vérité

Ce module poll le smart contract Chainlink `AggregatorV3` sur Ethereum mainnet toutes les 100ms. Il appelle `latestRoundData()` qui retourne le prix BTC/USD avec 8 décimales et un timestamp de dernière mise à jour.

L'adresse du price feed BTC/USD est `0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c`. C'est un contrat proxy qui pointe vers l'aggregator actuel.

Chaque lecture retourne un `PriceData` avec le prix, le round ID (pour détecter les nouvelles mises à jour), et le timestamp exact.

### polymarket.rs — L'interface de trading

Ce module gère toute la communication avec l'API CLOB (Central Limit Order Book) de Polymarket :

**Trouver les marchés** : L'API Gamma (`gamma-api.polymarket.com`) liste tous les marchés actifs. On filtre pour ne garder que les marchés 5 minutes BTC. Chaque marché a deux tokens : un pour UP (ou Yes) et un pour DOWN (ou No).

**Lire les prix** : L'endpoint `/midpoint` donne le prix mid du carnet d'ordres pour chaque token. L'endpoint `/book` donne le carnet complet avec bids et asks.

**Placer des ordres** : L'endpoint `/order` accepte un JSON avec le token ID, le prix, la taille, et le côté (BUY/SELL). Chaque requête est signée en HMAC-SHA256 avec ton `api_secret`.

**Authentification** : Polymarket utilise un système à 2 niveaux. Le Level 1 (que tu fais une seule fois manuellement) te donne tes credentials via une signature EIP-712. Le Level 2 (géré automatiquement par le bot) signe chaque requête avec HMAC-SHA256.

### strategy.rs — Le cerveau

Le module stratégie fait trois choses :

**Détection de signal** : Au début de chaque intervalle, on enregistre le prix Chainlink. Pendant l'intervalle, on compare le prix actuel au prix de départ. Si BTC monte et que le marché price UP trop bas, c'est un signal d'achat. L'edge est calculé comme la différence entre notre estimation de probabilité (basée sur Chainlink) et le prix du marché.

**Position sizing** : On utilise un demi-Kelly Criterion. Plus l'edge est grand et la confiance élevée, plus on mise, mais jamais plus que `max_bet_usdc` (configuré à 2$ par défaut). Le demi-Kelly est conservateur : il sacrifie un peu de rendement attendu pour réduire la variance.

**Risk management** : Le bot s'arrête automatiquement si le profit de session atteint la cible (`session_profit_target_usdc` = 20$) ou si la perte dépasse la limite (`session_loss_limit_usdc` = 10$). On ne trade qu'une seule fois par intervalle de 5 minutes pour éviter de surexposer.

## Setup pas à pas

### 1. Installer Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. Obtenir une clé RPC Ethereum

Tu as besoin d'un endpoint RPC pour lire les données Chainlink. Options gratuites :

- **Alchemy** : `https://eth-mainnet.g.alchemy.com/v2/TA_CLE` — 300M compute units/mois gratuits
- **Infura** : `https://mainnet.infura.io/v3/TA_CLE` — 100k requêtes/jour gratuits
- **QuickNode** : Similaire, plan gratuit disponible

Pour 100ms de polling (10 req/sec = 864,000/jour), un plan gratuit Alchemy suffit largement.

### 3. Obtenir les credentials Polymarket

C'est la partie la plus technique. Il faut :

1. Avoir un wallet Ethereum connecté sur polymarket.com
2. Signer un message EIP-712 avec ta clé privée pour obtenir : `api_key`, `api_secret`, `passphrase`
3. Ces credentials expirent — tu devras les renouveler périodiquement

Documentation complète : `https://docs.polymarket.com/developers/CLOB/authentication`

### 4. Avoir du USDC sur Polygon

Le bot trade en USDC sur le réseau Polygon. Tu as besoin de :

- USDC sur Polygon (bridge depuis Ethereum si nécessaire)
- Un peu de MATIC pour les frais de gas (très faibles sur Polygon)
- Minimum recommandé : 20-50 USDC pour commencer

### 5. Configurer config.toml

Remplis le fichier `config.toml` avec tes vraies clés. Les paramètres importants à ajuster :

`poll_interval_ms` : Fréquence de lecture Chainlink. 100ms est agressif mais reste dans les limites des plans gratuits. Monte à 200-500ms si tu as des erreurs 429.

`min_edge_pct` : Seuil minimum pour trader. À 2%, tu trades quand le marché est au moins 2% en dessous de ta probabilité estimée. Plus haut = moins de trades mais plus sûrs. Commence à 3% et descends si tu ne vois jamais de trades.

`entry_seconds_before_end` : Fenêtre de trade avant la fin de l'intervalle. 10 secondes est un bon compromis entre certitude (plus de données) et risque de miss (le marché peut corriger).

`max_bet_usdc` : Montant max par position. Commence petit (1-2$) pour valider que tout fonctionne.

### 6. Build et lancement

```bash
# Compilation optimisée (première fois ~30-60s, ensuite instantané)
cargo build --release

# Lancement
./target/release/poly5m

# Mode debug (plus de logs)
RUST_LOG=debug cargo run
```

## Paramètres et leur impact

### min_edge_pct — Le filtre de qualité

C'est le paramètre le plus important. Il détermine le seuil minimum de divergence entre ton estimation (Chainlink) et le prix du marché pour déclencher un trade.

- **1.0%** : Agressif. Beaucoup de trades, mais certains auront un edge très fin qui peut être mangé par le slippage.
- **2.0%** : Équilibré. C'est le défaut recommandé.
- **3.0%** : Conservateur. Peu de trades, mais ceux qui passent ont un edge solide.
- **5.0%+** : Très sélectif. Tu ne traderas que sur les gros mouvements BTC intra-5min.

### entry_seconds_before_end — Le timing

Quand est-ce qu'on trade dans l'intervalle de 5 minutes ?

- **5 secondes** : Maximum de certitude sur la direction, mais le marché a probablement déjà corrigé et il y a un risque que l'ordre ne passe pas à temps.
- **10 secondes** : Bon compromis. Le défaut.
- **30 secondes** : Plus d'opportunités mais moins de certitude sur le résultat final.
- **60 secondes** : Le BTC peut encore bouger significativement en 1 minute, risqué.

### max_bet_usdc — Le risk par trade

- **0.10$** : Mode test. Pour valider que le bot fonctionne.
- **1-2$** : Conservateur. Recommandé pour commencer.
- **3$** : Comme dans le post Twitter.
- **5$+** : À tes risques.

Le sizing réel est souvent inférieur au max grâce au demi-Kelly Criterion qui ajuste la mise en fonction de l'edge et de la confiance.

## Ce que le bot log

En fonctionnement normal, tu verras :

```
🚀 poly5m — Bot d'arbitrage Polymarket 5min BTC
════════════════════════════════════════════════
⏳ En attente du prochain intervalle 5 minutes...
🔄 Nouvel intervalle de 5 minutes détecté
📌 Prix de début d'intervalle: $97,150.00
📌 Marché trouvé: Will BTC go up in the next 5 minutes?
📊 Chainlink: $97,210.00 | Δ: 0.0618% | True UP: 61.8% | Market UP: 52.0% | Edge UP: 9.8%
🟢 SIGNAL: BUY UP | Edge: 9.8% | Taille: $1.85 | 8s restantes
✅ Ordre placé: 0xabc...
📈 Trade #1: WIN ✅ | PnL: $0.89 | Total: $0.89 | WR: 100%
```

## Risques et limitations

### Ce qui peut mal tourner

**Latence réseau** : Si ta connexion est lente, le délai entre ta lecture Chainlink et ton placement d'ordre peut annuler l'edge. Idéalement, teste avec un VPS proche des serveurs Polymarket/Ethereum.

**Rate limiting** : Les providers RPC gratuits ont des limites. À 10 req/sec, tu peux te faire throttle. Surveille les erreurs 429.

**Marché qui s'adapte** : Si beaucoup de gens utilisent cette stratégie, les market makers ajusteront plus vite et l'edge diminuera.

**Slippage** : Le prix auquel tu veux acheter n'est pas forcément le prix auquel tu achètes. Sur un marché peu liquide, le slippage peut manger tout ton edge.

**Pas de marchés actifs** : Les marchés 5 minutes BTC ne sont pas toujours ouverts. Ils tournent typiquement pendant les heures de fort volume.

**Settlement inattendu** : Si Chainlink a un problème (oracle stale, prix aberrant), le résultat peut être surprenant.

### Bonnes pratiques

1. **Commence avec 0.10$ par trade** pour valider le flow complet
2. **Monte progressivement** : 0.10$ → 0.50$ → 1$ → 2$
3. **Ne laisse pas le bot tourner sans surveillance** les premières heures
4. **Log tout** : active le mode debug (`RUST_LOG=debug`) au début
5. **Surveille ton RPC** : si les lectures Chainlink ralentissent, l'edge disparaît
6. **Ne commit jamais config.toml** avec tes clés

## Améliorations futures

### WebSocket au lieu du polling

Actuellement on poll Chainlink toutes les 100ms via HTTP. Un WebSocket sur un noeud Ethereum donnerait des notifications instantanées quand le prix change, réduisant la latence de ~50ms en moyenne.

### Chainlink Data Streams

Chainlink propose "Data Streams", un produit premium avec des mises à jour sub-milliseconde. C'est ce que Polymarket utilise en interne pour le settlement. Y accéder directement donnerait un edge encore plus fin.

### Backtesting

Le bot ne log pas encore les données historiques de manière structurée. Ajouter un logger CSV qui enregistre chaque lecture Chainlink et chaque prix Polymarket permettrait de backtester et d'optimiser les paramètres.

### Multi-asset

Le même principe s'applique à ETH et à tout autre asset qui a un marché 5 minutes sur Polymarket et un price feed Chainlink. Supporter plusieurs assets en parallèle augmenterait le nombre d'opportunités.

## Références

- Polymarket CLOB API : `https://docs.polymarket.com/developers/CLOB/introduction`
- Polymarket Auth : `https://docs.polymarket.com/developers/CLOB/authentication`
- Chainlink BTC/USD Feed : `https://data.chain.link/feeds/ethereum/mainnet/btc-usd`
- Chainlink latestRoundData : `https://docs.chain.link/data-feeds/api-reference`
- Kelly Criterion : `https://en.wikipedia.org/wiki/Kelly_criterion`

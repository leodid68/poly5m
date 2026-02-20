# poly5m — Bot d'arbitrage Polymarket 5min BTC

Bot Rust qui trade les marches [Polymarket BTC 5-minute UP/DOWN](https://polymarket.com) en exploitant le delai entre les mises a jour de prix et le repricing du marche.

## Fonctionnement

1. **Prix multi-source** : WebSockets Binance/Coinbase/Kraken + Polymarket RTDS + Chainlink on-chain (fallback)
2. **Detecte le window 5min** en cours, enregistre le prix de debut, collecte les ticks intra-window
3. **Regime detection** : filtre les marches choppants via micro-volatilite et momentum ratio
4. **Modele hybride** : z-score (vol dynamique) + book imbalance (30%), CDF Student-t (df=4)
5. **Si edge net > seuil** apres frais dynamiques : ordre FOK/GTC avec sizing Half-Kelly
6. **Risk management** : circuit breaker, max consecutive losses, profit target / loss limit
7. **Auto-calibration** : ajuste le multiplicateur de confiance vol tous les N trades
8. **Logging complet** : 51 colonnes CSV + outcome logger (toutes les fenetres) + tick logger (rotation quotidienne)

## Architecture

```
src/
  main.rs        — Boucle principale, config, racing RPC, flow de trading
  chainlink.rs   — Lecture prix BTC/USD via latestRoundData()
  polymarket.rs  — Client CLOB API (discovery, midpoint, orderbook, ordres, EIP-712, HMAC)
  strategy.rs    — Modele hybride, Session, VolTracker, WindowTicks, Calibrator (144 tests)
  exchanges.rs   — WebSocket multi-exchange : Binance, Coinbase, Kraken
  rtds.rs        — Polymarket Real-Time Data Streams WebSocket
  macro_data.rs  — Donnees macro CoinGecko (1h/24h change, volume, funding rate)
  logger.rs      — CsvLogger (51 cols), OutcomeLogger, TickLogger
  presets.rs     — Presets de configuration par marche
```

## Deploiement VPS

### Prerequisites

- Rust toolchain : `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Python 3 + pip (pour generer les credentials)
- `build-essential` / `pkg-config` / `libssl-dev` (pour compiler)

```bash
# Ubuntu/Debian
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev python3-pip
```

### Installation

```bash
git clone https://github.com/leodid68/poly5m.git
cd poly5m
cargo build --release
```

### Credentials Polymarket

```bash
pip install py-clob-client
python3 get_creds.py
```

Le script demande ta cle privee MetaMask et genere les 3 credentials API.

### Configuration

Creer `config.toml` a la racine du projet :

```toml
[chainlink]
rpc_urls = [
    "https://eth-mainnet.g.alchemy.com/v2/TA_CLE_ALCHEMY",
    "https://eth.llamarpc.com",
    "https://ethereum.publicnode.com",
]
btc_usd_feed = "0xF4030086522a5bEEa4988F8cA5B36dbC97BeE88c"
poll_interval_ms = 100
poll_interval_ms_with_ws = 1000

[polymarket]
api_key = "xxx"
api_secret = "xxx"
passphrase = "xxx"
private_key = "0xXXX"

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

Securiser le fichier :

```bash
chmod 600 config.toml
```

### Lancement

```bash
./target/release/poly5m
```

Avec logs debug :

```bash
RUST_LOG=poly5m=debug ./target/release/poly5m
```

### Lancement en arriere-plan (systemd)

Creer `/etc/systemd/system/poly5m.service` :

```ini
[Unit]
Description=poly5m Polymarket Bot
After=network.target

[Service]
Type=simple
User=YOUR_USER
WorkingDirectory=/home/YOUR_USER/poly5m
ExecStart=/home/YOUR_USER/poly5m/target/release/poly5m
Restart=always
RestartSec=5
Environment=RUST_LOG=poly5m=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable poly5m
sudo systemctl start poly5m
sudo journalctl -u poly5m -f   # voir les logs
```

## Parametres strategie

| Parametre | Description | Defaut |
|-----------|-------------|--------|
| `max_bet_usdc` | Mise max par trade | 2.0 |
| `min_bet_usdc` | Mise min par trade | 0.10 |
| `min_edge_pct` | Edge net minimum pour trader | 2.0% |
| `kelly_fraction` | Fraction de Kelly (conservateur) | 0.10 |
| `vol_confidence_multiplier` | Multiplicateur sur la vol residuelle | 4.0 |
| `entry_seconds_before_end` | Fenetre d'entree avant fin du window | 10s |
| `session_profit_target_usdc` | Arret si profit atteint | $20 |
| `session_loss_limit_usdc` | Arret si perte atteinte | $10 |
| `min_market_price` / `max_market_price` | Filtre zone de prix (evite 50/50) | 0.20 / 0.80 |
| `min_implied_prob` | Prob minimale pour trader | 0.70 |
| `max_consecutive_losses` | Arret apres N pertes consecutives | 10 |
| `circuit_breaker_window` | Rolling WR sur N trades | 20 |
| `circuit_breaker_min_wr` | WR minimum avant pause | 40% |
| `student_t_df` | Degres de liberte Student-t (0 = normal) | 4.0 |
| `dry_run` | Mode simulation (pas d'ordres reels) | false |

## Fichiers generes

| Fichier | Description |
|---------|-------------|
| `trades.csv` | 51 colonnes : tous les trades, skips et resolutions |
| `outcomes.csv` | Toutes les fenetres 5min (meme sans trade) pour backtesting |
| `ticks/ticks_YYYYMMDD.csv` | Tick-level data avec rotation quotidienne (~2-3 MB/jour) |
| `calibration.json` | VCM auto-calibre, persiste entre sessions |

## Notes

- Le bot ne trade qu'**une fois par window de 5 minutes**
- Les ordres sont **FOK** ou **GTC** avec maker pricing (bid + 25% spread)
- Le PnL est resolu au changement de window (approximatif, le vrai PnL est on-chain)
- `config.toml` est dans `.gitignore` — jamais commit sur GitHub
- Le bot doit tourner depuis un pays **non geobloque** par Polymarket (ex: Pays-Bas)

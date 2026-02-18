# poly5m — Bot d'arbitrage Polymarket 5min BTC

Bot Rust qui trade les marches [Polymarket BTC 5-minute UP/DOWN](https://polymarket.com) en exploitant le delai entre les mises a jour Chainlink et le repricing du marche.

## Fonctionnement

1. **Poll Chainlink** toutes les 100ms via 3 RPCs en racing (Alchemy + publics) — prend la reponse la plus rapide
2. **Detecte le window 5min** en cours et enregistre le prix de debut
3. **Dans les 10 dernieres secondes**, recupere le marche Polymarket actif et son midpoint
4. **Calcule la probabilite** que BTC finisse UP ou DOWN avec un modele z-score (volatilite residuelle)
5. **Si edge > 2%**, place un ordre FOK (Fill-Or-Kill) avec sizing Half-Kelly
6. **Resout le PnL** au debut du window suivant en comparant le prix Chainlink

## Architecture

```
src/
  main.rs        — Boucle principale, config, racing RPC, flow de trading
  chainlink.rs   — Lecture prix BTC/USD via latestRoundData()
  polymarket.rs  — Client CLOB API (discovery, midpoint, ordres FOK, EIP-712, HMAC)
  strategy.rs    — Modele de probabilite, Half-Kelly, session limits (15 tests)
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

[polymarket]
api_key = "xxx"
api_secret = "xxx"
passphrase = "xxx"
private_key = "0xXXX"

[strategy]
max_bet_usdc = 5.0
min_edge_pct = 2.0
entry_seconds_before_end = 10
session_profit_target_usdc = 20.0
session_loss_limit_usdc = 10.0
dry_run = false
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
| `max_bet_usdc` | Mise max par trade (min Polymarket = $5) | 5.0 |
| `min_edge_pct` | Edge minimum pour trader | 2.0% |
| `entry_seconds_before_end` | Fenetre d'entree avant fin du window | 10s |
| `session_profit_target_usdc` | Arret si profit atteint | $20 |
| `session_loss_limit_usdc` | Arret si perte atteinte | $10 |
| `dry_run` | Mode simulation (pas d'ordres reels) | false |

## Notes

- Le bot ne trade qu'**une fois par window de 5 minutes**
- Les ordres sont **FOK** (Fill-Or-Kill) : executes immediatement ou annules
- Le PnL est resolu au changement de window (approximatif, le vrai PnL est on-chain)
- `config.toml` est dans `.gitignore` — jamais commit sur GitHub
- Le bot doit tourner depuis un pays **non geobloque** par Polymarket (ex: Pays-Bas)

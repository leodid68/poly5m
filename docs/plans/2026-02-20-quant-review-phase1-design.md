# Design — QUANT_REVIEW Phase 1 : Config Selector + Quick Wins

**Date** : 2026-02-20
**Source** : QUANT_REVIEW.md (revue quantitative complète)
**Scope** : Phase 1 uniquement (quick wins, 1-2 jours)

---

## A. Config Selector Interactif au Launch

### Comportement

1. Parse `config.toml` pour les sections fixes (chainlink, polymarket, exchanges, rtds, logging)
2. Si `--profile <name>` fourni en CLI → charge le preset strategy correspondant
3. Sinon → affiche un menu interactif dans le terminal avec 5 choix
4. Les presets overrident **seulement** la section `[strategy]` — credentials, RPC, WS restent du config.toml

### Menu

```
poly5m — Sélection du profil de trading

  1. Sniper Conservateur   (GTC maker, edge>=3%, kelly=0.10, vol<0.08%)
  2. High Conviction Only  (GTC maker, edge>=5%, kelly=0.15, imbal>=0.15)
  3. Extreme Zones Scalper (FOK taker, edge>=1%, mid 0.10-0.90, prob>=0.85)
  4. Data Farm [dry-run]   (FOK, filtres relâchés, collecte de données)
  5. Custom (config.toml)  (utiliser la config [strategy] du fichier)

Choix [1-5]:
```

### Presets

Les 4 presets sont définis en dur dans le code (pas dans config.toml) car ils représentent des stratégies validées par l'analyse quantitative. Chaque preset retourne un `StrategyConfig` complet.

### Implémentation

- Ajouter `clap` comme dépendance pour le parsing CLI (`--profile`)
- Nouveau module `presets.rs` avec les 4 configs
- `main.rs` : après `load_config()`, appeler `select_profile()` qui retourne un `StrategyConfig`
- Si stdin n'est pas un TTY (e.g. systemd), fallback sur config.toml (= option 5)

---

## B. Loss Decay Exponentiel

### Formule

```rust
let loss_decay = 0.7_f64.powi(session.consecutive_losses as i32);
let adjusted_kelly = kelly_size * loss_decay;
```

| Pertes consécutives | Decay | Sizing effectif |
|---------------------|-------|-----------------|
| 0 | 1.00x | 100% |
| 1 | 0.70x | 70% |
| 3 | 0.34x | 34% |
| 5 | 0.17x | 17% |
| 8 | 0.06x | 6% |

### Comportement

- Le decay s'applique AVANT le clamp min_bet/max_bet dans `evaluate()`
- `max_consecutive_losses` reste comme kill switch absolu (skip total)
- Le decay factor (0.7) est hardcodé — pas configurable (YAGNI)

### Fichier : `strategy.rs` dans `evaluate()`, après le calcul Kelly

---

## C. VolTracker MAD (Median Absolute Deviation)

### Formule

```
MAD = median(|xi - median(x)|)
σ_robust ≈ 1.4826 × MAD
```

### Comportement

- Remplace `current_vol()` existant (std dev → MAD)
- Résiste aux outliers (un flash crash ne pollue plus la vol pendant N intervalles)
- Clamp identique : `[0.01, 1.0]`
- Nécessite au minimum 3 samples (comme avant avec 2, on monte à 3 pour le calcul de médiane)

### Fichier : `strategy.rs`, méthode `VolTracker::current_vol()`

---

## D. Defaults mis à jour

Changements de valeurs par défaut uniquement (pas de nouveau code logique) :

| Paramètre | Ancien default | Nouveau default | Justification |
|-----------|---------------|-----------------|---------------|
| `vol_confidence_multiplier` | 1.0 | 4.0 | Données : overconfidence 20pts |
| `kelly_fraction` | 0.25 | 0.10 | Modèle mal calibré → prudent |
| `min_market_price` | 0.15 | 0.25 | Éviter les extrêmes |
| `max_market_price` | 0.85 | 0.75 | Éviter les extrêmes |

Ces defaults s'appliquent quand le champ n'est pas spécifié dans config.toml (serde default). Les presets ont leurs propres valeurs.

---

## Fichiers impactés

| Fichier | Changement |
|---------|-----------|
| `Cargo.toml` | +clap |
| `src/presets.rs` | NOUVEAU — 4 presets + menu interactif |
| `src/main.rs` | Appel `select_profile()`, import presets |
| `src/strategy.rs` | Loss decay dans evaluate(), MAD dans VolTracker |
| `config.example.toml` | Commentaires mis à jour |

## Tests

- `presets.rs` : chaque preset produit un StrategyConfig valide
- `strategy.rs` : loss decay réduit le sizing, MAD résiste aux outliers
- Tous les tests existants doivent continuer de passer

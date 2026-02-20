# Design — QUANT_REVIEW Phase 2 : Modèle Hybride + Perf

**Date** : 2026-02-20
**Source** : QUANT_REVIEW.md §1.3, §5.2

---

## A. Modèle hybride prix + imbalance

Pondérer le z-score par le book imbalance dans `price_change_to_probability()` ou `evaluate()`.

```rust
let imbalance_signal = (book_imbalance - 0.5).clamp(-0.4, 0.4);
let z_combined = z * 0.6 + imbalance_signal * z.signum() * 2.5;
```

Quand l'imbalance confirme la direction → z augmente. Quand elle contredit → z diminue.

**Fichier** : `src/strategy.rs`, dans `evaluate()` après le calcul de `price_change_to_probability`.

## B. Parallel fetch book + midpoint

Remplacer les appels séquentiels `get_midpoint()` + `get_book()` par `tokio::join!`.

**Fichier** : `src/main.rs`, dans `fetch_market_data()`.

## C. Pré-fetch marché au début du window

Appeler `find_5min_btc_market()` dès la transition de window (quand `window != current_window`), pas dans la fenêtre d'entrée.

**Fichier** : `src/main.rs`, dans le bloc de transition de window.

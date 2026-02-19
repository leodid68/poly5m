#!/usr/bin/env python3
"""Grid search sur les paramètres de la stratégie à partir du CSV historique."""
import csv
from itertools import product

def half_kelly(p, price, max_bet):
    if price <= 0 or price >= 1 or p <= 0 or p >= 1:
        return 0.0
    b = (1 - price) / price
    q = 1 - p
    kelly = (b * p - q) / b
    return max(0, min((kelly / 2) * max_bet, max_bet))

def simulate(trades, min_edge, max_remaining, max_bet):
    total_pnl = 0.0
    n_trades = 0
    wins = 0
    peak = 0.0
    max_drawdown = 0.0

    for t in trades:
        if t['edge_net'] < min_edge:
            continue
        if t['remaining_s'] > max_remaining:
            continue

        size = half_kelly(t['true_prob'], t['entry_price'], max_bet)
        if size < 0.01:
            continue

        if t['won']:
            pnl = size * (1.0 / t['entry_price'] - 1.0)
            wins += 1
        else:
            pnl = -size

        total_pnl += pnl
        n_trades += 1
        peak = max(peak, total_pnl)
        max_drawdown = min(max_drawdown, total_pnl - peak)

    wr = (wins / n_trades * 100) if n_trades > 0 else 0
    return {
        'pnl': total_pnl,
        'trades': n_trades,
        'wins': wins,
        'wr': wr,
        'max_dd': max_drawdown,
        'pnl_per_trade': total_pnl / n_trades if n_trades > 0 else 0,
    }

def main():
    # Parse CSV — associer trades à résolutions par window
    trade_by_window = {}
    resolution_by_window = {}

    with open('trades.csv') as f:
        for line in f:
            fields = line.strip().split(',')
            if len(fields) < 10 or fields[0] == 'timestamp':
                continue
            event = fields[2]
            window = fields[1]
            if event == 'trade':
                trade_by_window[window] = fields
            elif event == 'resolution':
                # Trouver WIN/LOSS dans la ligne
                won = 'WIN' in line
                resolution_by_window[window] = won

    trades = []
    for window, fields in trade_by_window.items():
        if window not in resolution_by_window:
            continue
        won = resolution_by_window[window]

        side = fields[9]        # BUY_UP ou BUY_DOWN
        implied_p_up = float(fields[8])
        edge_net = float(fields[12])
        entry_price = float(fields[15])
        remaining = int(fields[16])

        if 'UP' in side:
            true_prob = implied_p_up
        else:
            true_prob = 1.0 - implied_p_up

        trades.append({
            'side': side,
            'true_prob': true_prob,
            'entry_price': entry_price,
            'edge_net': edge_net,
            'remaining_s': remaining,
            'won': won,
            'window': window,
        })

    print(f"Trades chargés: {len(trades)} (W:{sum(1 for t in trades if t['won'])} / L:{sum(1 for t in trades if not t['won'])})\n")

    # Grid search
    min_edges = [1, 3, 5, 8, 10, 12, 15, 20, 50]
    max_remainings = [15, 30, 45, 60]
    max_bets = [3, 5, 8, 10]

    results = []
    for me, mr, mb in product(min_edges, max_remainings, max_bets):
        r = simulate(trades, me, mr, mb)
        if r['trades'] > 0:
            results.append((me, mr, mb, r))

    # Top 25 par PnL
    results.sort(key=lambda x: x[3]['pnl'], reverse=True)
    print(f"{'edge':>5} {'entry_s':>7} {'max$':>5} | {'trades':>6} {'W/L':>8} {'WR%':>5} | {'PnL':>8} {'$/trade':>8} {'maxDD':>8}")
    print("-" * 80)
    for me, mr, mb, r in results[:25]:
        wl = f"{r['wins']}W/{r['trades']-r['wins']}L"
        print(f"{me:>5} {mr:>7} {mb:>5} | {r['trades']:>6} {wl:>8} {r['wr']:>5.1f} | {r['pnl']:>8.2f} {r['pnl_per_trade']:>8.2f} {r['max_dd']:>8.2f}")

    # Meilleur PnL/trade (min 5 trades)
    print()
    print("--- Meilleur PnL/trade (min 5 trades) ---")
    filtered = [(me, mr, mb, r) for me, mr, mb, r in results if r['trades'] >= 5]
    filtered.sort(key=lambda x: x[3]['pnl_per_trade'], reverse=True)
    print(f"{'edge':>5} {'entry_s':>7} {'max$':>5} | {'trades':>6} {'W/L':>8} {'WR%':>5} | {'PnL':>8} {'$/trade':>8} {'maxDD':>8}")
    print("-" * 80)
    for me, mr, mb, r in filtered[:15]:
        wl = f"{r['wins']}W/{r['trades']-r['wins']}L"
        print(f"{me:>5} {mr:>7} {mb:>5} | {r['trades']:>6} {wl:>8} {r['wr']:>5.1f} | {r['pnl']:>8.2f} {r['pnl_per_trade']:>8.2f} {r['max_dd']:>8.2f}")

    # Meilleur ratio PnL/drawdown (min 5 trades)
    print()
    print("--- Meilleur PnL/maxDD (min 5 trades, DD<0) ---")
    ratio_filtered = [(me, mr, mb, r) for me, mr, mb, r in results if r['trades'] >= 5 and r['max_dd'] < -0.01]
    ratio_filtered.sort(key=lambda x: x[3]['pnl'] / abs(x[3]['max_dd']), reverse=True)
    print(f"{'edge':>5} {'entry_s':>7} {'max$':>5} | {'trades':>6} {'W/L':>8} {'WR%':>5} | {'PnL':>8} {'$/trade':>8} {'maxDD':>8} {'ratio':>6}")
    print("-" * 86)
    for me, mr, mb, r in ratio_filtered[:15]:
        wl = f"{r['wins']}W/{r['trades']-r['wins']}L"
        ratio = r['pnl'] / abs(r['max_dd'])
        print(f"{me:>5} {mr:>7} {mb:>5} | {r['trades']:>6} {wl:>8} {r['wr']:>5.1f} | {r['pnl']:>8.2f} {r['pnl_per_trade']:>8.2f} {r['max_dd']:>8.2f} {ratio:>6.1f}")

if __name__ == '__main__':
    main()

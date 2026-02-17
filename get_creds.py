"""
Génère les credentials API Polymarket à partir de ta clé privée MetaMask.
Usage: python3 get_creds.py
"""
import getpass

# Demande la clé privée sans l'afficher
pk = getpass.getpass("Colle ta clé privée MetaMask (0x...): ")
pk = pk.strip()
if not pk.startswith("0x"):
    pk = "0x" + pk

from py_clob_client.client import ClobClient

client = ClobClient(
    host="https://clob.polymarket.com",
    chain_id=137,
    key=pk,
)

print("\nGénération des credentials...")
creds = client.create_or_derive_api_creds()

print("\n=== Tes credentials Polymarket ===")
print(f'api_key    = "{creds.api_key}"')
print(f'api_secret = "{creds.api_secret}"')
print(f'passphrase = "{creds.api_passphrase}"')
print(f'\nprivate_key = "{pk}"')
print("\nColle ces 4 valeurs dans config.toml sous [polymarket]")

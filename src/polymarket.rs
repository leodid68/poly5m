use alloy::{
    hex,
    primitives::{Address, U256, address},
    signers::{local::PrivateKeySigner, Signer},
    sol,
    sol_types::{eip712_domain, SolStruct},
};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use hmac::{Hmac, Mac};
use rand::Rng;
use serde::Deserialize;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLOB_BASE: &str = "https://clob.polymarket.com";
const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const CTF_EXCHANGE: Address = address!("4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E");

// EIP-712 Order struct — doit correspondre exactement au contrat CTFExchange
sol! {
    #[derive(Debug)]
    struct Order {
        uint256 salt;
        address maker;
        address signer;
        address taker;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint256 expiration;
        uint256 nonce;
        uint256 feeRateBps;
        uint8 side;
        uint8 signatureType;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Market {
    pub condition_id: String,
    pub token_id_yes: String,
    pub token_id_no: String,
    pub question: String,
}

#[derive(Debug)]
pub struct OrderResult {
    pub order_id: String,
    pub status: String,
}

pub struct PolymarketClient {
    http: reqwest::Client,
    api_key: String,
    api_secret_bytes: Vec<u8>, // pré-décodé base64 une seule fois
    passphrase: String,
    signer: PrivateKeySigner,
    wallet_address: Address,
}

// --- Réponses API (serde) ---

// L'API Gamma retourne les champs en camelCase et les tokens/outcomes comme JSON strings
#[derive(Deserialize)]
struct GammaEvent {
    markets: Vec<GammaMarket>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarket {
    condition_id: String,
    clob_token_ids: String,  // JSON string: "[\"id1\", \"id2\"]"
    outcomes: String,         // JSON string: "[\"Up\", \"Down\"]"
    question: String,
}

#[derive(Deserialize)]
struct MidpointResponse {
    mid: String,
}

#[derive(Deserialize)]
struct OrderResponse {
    #[serde(rename = "orderID")]
    order_id: String,
    status: String,
}

impl PolymarketClient {
    pub fn new(
        api_key: String,
        api_secret: String,
        passphrase: String,
        private_key: &str,
    ) -> Result<Self> {
        let signer: PrivateKeySigner = private_key.parse().context("Invalid private key")?;
        let wallet_address = signer.address();
        let api_secret_bytes = general_purpose::URL_SAFE
            .decode(&api_secret)
            .context("Invalid api_secret base64")?;
        let http = reqwest::Client::builder()
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .build()?;

        Ok(Self { http, api_key, api_secret_bytes, passphrase, signer, wallet_address })
    }

    /// Trouve le marché 5min BTC actif pour le window donné.
    pub async fn find_5min_btc_market(&self, window_ts: u64) -> Result<Market> {
        let slug = format!("btc-updown-5m-{window_ts}");
        tracing::debug!(slug = %slug, "Looking up 5min BTC market");

        let resp = self.http
            .get(format!("{GAMMA_BASE}/events"))
            .query(&[("slug", &slug)])
            .send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gamma API error ({status}): {body}");
        }
        let events: Vec<GammaEvent> = resp.json().await?;

        let market = events.first()
            .and_then(|e| e.markets.first())
            .context("Aucun marché 5min BTC actif")?;

        // Parse les JSON strings retournés par Gamma
        let token_ids: Vec<String> = serde_json::from_str(&market.clob_token_ids)
            .context("Failed to parse clobTokenIds")?;
        let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)
            .context("Failed to parse outcomes")?;
        anyhow::ensure!(token_ids.len() == outcomes.len(), "Token/outcome count mismatch");

        let yes_idx = outcomes.iter().position(|o| o == "Up" || o == "Yes")
            .context("Outcome 'Up'/'Yes' introuvable")?;
        let no_idx = outcomes.iter().position(|o| o == "Down" || o == "No")
            .context("Outcome 'Down'/'No' introuvable")?;

        Ok(Market {
            condition_id: market.condition_id.clone(),
            token_id_yes: token_ids[yes_idx].clone(),
            token_id_no: token_ids[no_idx].clone(),
            question: market.question.clone(),
        })
    }

    /// Récupère le prix mid pour un token (endpoint public, pas d'auth).
    pub async fn get_midpoint(&self, token_id: &str) -> Result<f64> {
        let resp = self.http
            .get(format!("{CLOB_BASE}/midpoint"))
            .query(&[("token_id", token_id)])
            .send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Midpoint API error ({status}): {body}");
        }
        let data: MidpointResponse = resp.json().await?;
        data.mid.parse::<f64>().context("Invalid midpoint value")
    }

    /// Place un ordre FOK (Fill-Or-Kill).
    pub async fn place_order(
        &self,
        token_id: &str,
        side: Side,
        size_usdc: f64,
        price: f64,
    ) -> Result<OrderResult> {
        let side_u8: u8 = if side == Side::Buy { 0 } else { 1 };

        // Amounts en unités raw (6 décimales USDC), .round() évite les erreurs f64
        let (maker_amount, taker_amount) = if side == Side::Buy {
            let maker = (size_usdc * 1e6).round() as u128;
            let taker = ((size_usdc / price) * 1e6).round() as u128;
            (maker, taker)
        } else {
            let maker = ((size_usdc / price) * 1e6).round() as u128;
            let taker = (size_usdc * 1e6).round() as u128;
            (maker, taker)
        };

        let salt: u128 = rand::rng().random();
        let expiration = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 30;

        let order = Order {
            salt: U256::from(salt),
            maker: self.wallet_address,
            signer: self.wallet_address,
            taker: Address::ZERO,
            tokenId: U256::from_str_radix(token_id, 10).context("Invalid token_id")?,
            makerAmount: U256::from(maker_amount),
            takerAmount: U256::from(taker_amount),
            expiration: U256::from(expiration),
            nonce: U256::ZERO,
            feeRateBps: U256::ZERO,
            side: side_u8,
            signatureType: 0, // EOA
        };

        let signature = self.sign_order_eip712(&order).await?;

        let body = serde_json::json!({
            "owner": format!("{}", self.wallet_address),
            "orderType": "FOK",
            "order": {
                "salt": order.salt.to_string(),
                "maker": format!("{}", order.maker),
                "signer": format!("{}", order.signer),
                "taker": format!("{}", order.taker),
                "tokenId": token_id,
                "makerAmount": maker_amount.to_string(),
                "takerAmount": taker_amount.to_string(),
                "expiration": expiration.to_string(),
                "nonce": "0",
                "feeRateBps": "0",
                "side": side_u8.to_string(),
                "signatureType": 0,
                "signature": signature,
            }
        });

        let body_str = body.to_string();
        let path = "/order";
        let headers = self.sign_hmac("POST", path, &body_str)?;

        let mut req = self.http.post(format!("{CLOB_BASE}{path}"))
            .header("Content-Type", "application/json")
            .body(body_str);
        for (k, v) in &headers {
            req = req.header(k, v);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Order API error ({status}): {body}");
        }
        let result: OrderResponse = resp.json().await?;

        Ok(OrderResult { order_id: result.order_id, status: result.status })
    }

    // --- Helpers internes ---

    /// HMAC-SHA256 Level 2 auth headers.
    fn sign_hmac(&self, method: &str, path: &str, body: &str) -> Result<Vec<(String, String)>> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs().to_string();
        let message = format!("{timestamp}{method}{path}{body}");

        let mut mac = Hmac::<Sha256>::new_from_slice(&self.api_secret_bytes)
            .context("HMAC key error")?;
        mac.update(message.as_bytes());
        let signature = general_purpose::URL_SAFE.encode(mac.finalize().into_bytes());

        Ok(vec![
            ("POLY_ADDRESS".into(), format!("{}", self.wallet_address)),
            ("POLY_API_KEY".into(), self.api_key.clone()),
            ("POLY_PASSPHRASE".into(), self.passphrase.clone()),
            ("POLY_TIMESTAMP".into(), timestamp),
            ("POLY_SIGNATURE".into(), signature),
        ])
    }

    /// Signe un ordre avec EIP-712 (Polymarket CTF Exchange).
    async fn sign_order_eip712(&self, order: &Order) -> Result<String> {
        let domain = eip712_domain! {
            name: "Polymarket CTF Exchange",
            version: "1",
            chain_id: 137,
            verifying_contract: CTF_EXCHANGE,
        };

        let signing_hash = order.eip712_signing_hash(&domain);
        let sig = self.signer.sign_hash(&signing_hash).await.context("EIP-712 signing failed")?;

        Ok(format!("0x{}", hex::encode(sig.as_bytes())))
    }
}

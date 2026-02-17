use alloy::{
    primitives::Address,
    providers::Provider,
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result};

// ABI Chainlink AggregatorV3 — on n'utilise qu'une seule fonction
sol! {
    function latestRoundData() external view returns (
        uint80 roundId,
        int256 answer,
        uint256 startedAt,
        uint256 updatedAt,
        uint80 answeredInRound
    );
}

#[derive(Debug, Clone, Copy)]
pub struct PriceData {
    pub price_usd: f64,
    pub round_id: u128,
    pub updated_at: u64,
}

/// Lit le dernier prix BTC/USD depuis Chainlink.
/// Le provider est créé dans main.rs — ici on reste agnostique du transport.
pub async fn fetch_price(
    provider: &(impl Provider + Sync),
    feed: Address,
) -> Result<PriceData> {
    let tx = TransactionRequest::default()
        .to(feed)
        .input(latestRoundDataCall {}.abi_encode().into());

    let result = provider.call(tx).await.context("Chainlink eth_call failed")?;

    let decoded = latestRoundDataCall::abi_decode_returns(&result)
        .context("Failed to decode latestRoundData")?;

    // answer = prix avec 8 décimales (ex: 9700000000000 = $97,000.00)
    let price_raw = i64::try_from(decoded.answer).context("Price overflows i64")?;
    anyhow::ensure!(price_raw > 0, "Chainlink returned non-positive price: {price_raw}");

    Ok(PriceData {
        price_usd: price_raw as f64 / 1e8,
        round_id: decoded.roundId.to::<u128>(),
        updated_at: decoded.updatedAt.to::<u64>(),
    })
}

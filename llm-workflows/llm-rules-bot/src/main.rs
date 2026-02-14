mod config;
mod bot;
mod budget;
mod journal;
mod risk;
mod temporal;

use anyhow::Result;
use config::AppConfig;
use bot::Bot;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Load config
    let config = AppConfig::load("config.toml")?;
    info!("Loaded configuration: {:?}", config);

    // Initialize and run bot
    let mut bot = Bot::new(config).await?;
    bot.run().await?;
    
    Ok(())
}

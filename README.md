# Agentic Trading Monorepo

Welcome to the **Agentic Trading** monorepo. This repository houses multiple frameworks for building advanced trading systems, initially focusing on agentic workflows, traditional algorithms, and LLM-integrated strategies.

## Structure

The repository is organized into the following workspaces:

### 1. [`clawdbot-workflows`](./clawdbot-workflows)
*   **Description**: The core bot framework, migrated from `openclaw`. This serves as the foundation for agent-based interactions and gateway capabilities.
*   **Status**: Active (migrated)

### 2. [`llm-workflows`](./llm-workflows)
*   **Description**: A framework dedicated to Large Language Model (LLM) integrations for market analysis, sentiment analysis, and decision-making support.
*   **Status**: Initialized

### 3. [`non-agent-workflows`](./non-agent-workflows)
*   **Description**: A clean environment for traditional, non-agentic trading algorithms, data processing scripts, and quantitative analysis tools.
*   **Status**: Initialized

## Non-Agent Bots High-Level

### Weather Bot ([`non-agent-workflows/weather-bot`](./non-agent-workflows/weather-bot))

High-level behavior:

1. Discovers active Kalshi weather markets and keeps live prices updated from WebSocket feeds.
2. Pulls forecasts from NOAA and Google Weather, then combines them with a weighted ensemble.
3. Converts forecast inputs into market probabilities and confidence estimates.
4. Applies strict quality filters before entering trades (source agreement, confidence, conservative edge/EV, liquidity/spread).
5. Routes approved intents through risk controls, places orders, and journals all runtime events.

Primary goal: fewer bets with higher conviction and controlled downside.

See details: [`non-agent-workflows/weather-bot/README.md`](./non-agent-workflows/weather-bot/README.md)

### Arbitrage Bot ([`non-agent-workflows/arbitrage-bot`](./non-agent-workflows/arbitrage-bot))

High-level behavior:

1. Discovers related Kalshi contracts that form mutually exclusive outcome groups.
2. Detects Buy-Set and Sell-Set opportunities where combined prices imply locked-in edge.
3. Supports EV-mode for non-exhaustive sets with explicit discounting.
4. Executes grouped orders with FOK-style handling to reduce legging risk.
5. Enforces risk constraints (exposure caps, kill-switch, emergency unwind) and journals outcomes.

Primary goal: capture pricing inefficiencies while minimizing partial-fill and tail-risk exposure.

See details: [`non-agent-workflows/arbitrage-bot/README.md`](./non-agent-workflows/arbitrage-bot/README.md)

## Getting Started

This project is managed as a **pnpm workspace**.

### Prerequisites
*   Node.js (matching engines in package.json)
*   pnpm

### Installation

```bash
pnpm install
```

## Workflows

Navigate to the respective directories to run specific workflows or check their `package.json` for available scripts.

## Kalshi API Set UP 

```bash
export KALSHI_API_KEY="[ENCRYPTION_KEY]"
export KALSHI_SECRET_KEY="[ENCRYPTION_KEY]"
```

## LLM Key Set Up

```bash
export OPENAI_API_KEY="[ENCRYPTION_KEY]"
```
or 
```bash
export ANTHROPIC_API_KEY="[ENCRYPTION_KEY]"
```

## Trade Journals

The non-agent bots write runtime JSONL trade journals under:

- `../trading/TRADES/arbitrage-bot`
- `../trading/TRADES/weather-bot`

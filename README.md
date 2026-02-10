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

- `/Users/jadenfix/Desktop/trading/TRADES/arbitrage-bot`
- `/Users/jadenfix/Desktop/trading/TRADES/weather-bot`

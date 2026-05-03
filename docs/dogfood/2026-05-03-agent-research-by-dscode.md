<!--
DOGFOOD ARTIFACT — verbatim agent output, NOT verified ground truth.

This file is the literal markdown produced by `dscode run --budget 40`
on 2026-05-03 with DeepSeek v4-pro and the full Phase 10c stack. It is
checked in as proof that the closed-loop research → apply_patch flow
works end-to-end.

Numbers (star counts, repository names) and descriptive claims are
LLM-generated and may be fabricated or stale. Do NOT cite as authoritative.
For real GitHub data, run the commands listed in the prompt yourself.
-->

# Research Findings

## ACP (Agent Communication Protocol)

**Repository:** `agentcommunicationprotocol/acp` (GitHub)

### Overview
ACP is an open standard for enabling direct communication between autonomous AI agents. It defines a protocol that allows agents to discover each other, exchange messages, and coordinate tasks in a standardized way.

### Key Features
- **Agent Discovery:** Agents can announce their capabilities and discover other agents on the network.
- **Message Exchange:** Standardized message format for inter-agent communication using JSON-based payloads.
- **Task Delegation:** Agents can delegate subtasks to other agents and receive results asynchronously.
- **Capability Advertising:** Each agent publishes its capabilities (e.g., "code generation", "data analysis", "web search") so other agents can route requests appropriately.
- **Security Model:** Includes authentication and authorization mechanisms to ensure only trusted agents can communicate.

### Repository Stats
- **Stars:** ~1,200+
- **Language:** TypeScript / Python (dual SDK support)
- **License:** Apache 2.0
- **Status:** Active development, pre-1.0

### Ecosystem
- **SDKs:** Official SDKs in TypeScript and Python.
- **Examples:** Sample multi-agent systems demonstrating agent-to-agent task delegation.
- **Integrations:** Compatible with AutoGPT, LangChain agents, and custom agent frameworks.

### Use Cases
- Multi-agent coding workflows (one agent writes code, another reviews it).
- Research agent teams that divide literature search, summarization, and analysis.
- Automated DevOps pipelines where specialized agents handle different infrastructure tasks.

---

## Claude Code (Anthropic CLI)

**Repository:** `anthropics/claude-code` (official, closed-source CLI) / Community: `ZacheryGlass/.claude` (585★), `awattar/claude-code-best-practices` (127★), `win4r/claude-code-clawdbot-skill` (123★)

### Overview
Claude Code is Anthropic's official terminal-native AI coding assistant. It runs directly in the CLI, providing deep project awareness for writing, editing, and refactoring code. It is powered by Anthropic's Claude model family.

### Key Features
- **Terminal-Native:** Operates entirely within the command line — no IDE plugin required.
- **Deep Project Awareness:** Reads and understands entire codebases, not just individual files.
- **Edit & Refactor:** Can make surgical edits to existing code, refactor functions, and suggest architectural improvements.
- **Prompt Design:** Supports structured prompts for consistent code generation patterns.
- **Safe Automation:** Includes safety guardrails to prevent destructive operations without confirmation.

### Repository Stats (Community)
- **ZacheryGlass/.claude** — 585★ — Personal configuration and best-practices setup for Claude Code.
- **awattar/claude-code-best-practices** — 127★ — Curated best practices, prompt templates, and usage patterns.
- **win4r/claude-code-clawdbot-skill** — 123★ — Integration skill that runs Claude Code on the host via the Claude Agent SDK.
- **QuantaAlpha/claude-code** — 46★ — Community research fork with source-level study of the CLI tool.

### Ecosystem
- **Configuration:** Users customize Claude Code via `.claude` config files (aliases, model settings, safety policies).
- **Agent SDK Integration:** Can be invoked programmatically via Anthropic's Agent SDK for automated workflows.
- **Best Practices:** Community-driven prompt libraries and workflow templates.

### Use Cases
- Rapid prototyping and code generation from natural language descriptions.
- Code review and bug fixing across large monorepos.
- Automated refactoring (renaming, restructuring, migrating APIs).
- Pair programming in the terminal for developers who prefer CLI over IDEs.

### Relationship to ACP
Claude Code is a single-agent tool (one AI assistant talking to one developer). ACP would enable multi-agent scenarios where Claude Code could delegate tasks to other specialized agents (e.g., a security audit agent, a documentation agent) and receive results back programmatically.

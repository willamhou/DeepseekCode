# DeepSeek-TUI Provider Model Compatibility

## Context

DeepSeek-TUI's latest upstream refresh added provider/model compatibility fixes:
legacy DeepSeek CN provider aliases map back to the canonical DeepSeek provider,
known provider-prefixed DeepSeek model aliases are normalized for the active
provider, and strict OpenAI-compatible gateways avoid DeepSeek-only request
fields such as top-level `thinking`.

DeepSeekCode already has provider presets and `/provider` / `/model` TUI
commands, but `/model` writes the raw normalized token without considering the
active provider. The OpenAI-compatible request builder also emits `thinking`
for all OpenAI-flavored endpoints, which is fine for DeepSeek-compatible
servers but can break strict OpenAI/Fireworks gateways.

## Spec

- Accept `deepseek-cn`, `deepseek_china`, `deepseekcn`, and `deepseek-china`
  as aliases for the canonical `deepseek` provider preset.
- Make `set_model_at` provider-aware by inferring the active provider from
  `model.base_url`, then mapping bare `deepseek-v4-*` aliases to the provider's
  expected model id.
- Canonicalize known official DeepSeek aliases such as
  `deepseek/deepseek-v4-pro` and `deepseek-ai/DeepSeek-V4-Pro` to bare
  `deepseek-v4-pro` when the active provider is the official DeepSeek endpoint.
- Keep provider-specific ids for compatible backends such as NVIDIA NIM,
  OpenRouter, Novita, Fireworks, SGLang, and vLLM.
- Omit DeepSeek-only `thinking` request fields for strict OpenAI-compatible
  endpoints, starting with official OpenAI and Fireworks base URLs.
- Update the parity plan with the new upstream comparison head and landed
  compatibility slice.

## Verification

- `provider_preset_accepts_legacy_deepseek_cn_aliases`
- `set_model_at_canonicalizes_deepseek_provider_aliases`
- `set_model_at_maps_bare_deepseek_model_for_active_provider`
- `openai_reasoning_fields_omit_deepseek_thinking_for_strict_gateways`
- `cargo test provider_ --lib`
- `cargo test set_model_at_ --lib`
- `cargo test openai_reasoning_fields --lib`
- `cargo fmt --check`

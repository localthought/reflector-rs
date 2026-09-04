# Generative AI prompt/output logs

This folder is reflector-rs's disclosure log for generative-AI use, kept to
comply with [NLnet's Generative AI policy](https://nlnet.nl/foundation/policies/generativeAI/)
for NLnet-funded work. It follows the same conventions as its sibling
repository [`localthought/reflector`](https://github.com/localthought/reflector/tree/main/docs/ai-logs).

- Each substantive Claude Code session that produces a commit gets a log under
  [`sessions/`](sessions), named `YYYY-MM-DD-<short-slug>.md`.
- Commits produced with AI assistance carry a
  `Claude-Session: https://claude.ai/code/session_...` trailer identifying the
  session that produced them.
- Logs capture the **substantive human prompts and the assistant's substantive
  outputs** — the actual asks and the actual answers and code changes. They do
  not reproduce the assistant's internal system prompt, tool-call plumbing, or
  other harness scaffolding: that is product internals rather than
  project-specific prompting, and reproducing it would not add transparency
  about how *this project* was built.

## Redaction

Before a log is committed it is reviewed for credentials and tokens, personal
information not already public about the project, and session- or
account-identifying detail that is not needed to understand what was asked and
produced. Redactions are marked inline as `[redacted]` rather than deleted
silently.

## Human review

AI-assisted output here is reviewed and tested by a human before it is
committed, and is not presented as unassisted human work. Both are conditions
of the policy, not house style.

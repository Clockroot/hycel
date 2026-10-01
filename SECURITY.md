# Security policy

Hycel is pre-alpha and not yet suitable for untrusted projects or production game distribution. Please do not run untrusted project code; scripting is not implemented and must never execute implicitly.

If [GitHub private vulnerability reporting](https://github.com/aaf2tbz/hycel/security/advisories/new) is available, use it. If it is unavailable, open a minimal public issue asking the maintainer for a private reporting channel—do not include vulnerability details in that issue. Include the affected version, reproduction steps, impact, and any mitigations only in the private report. The current maintainer will acknowledge and triage reports as capacity allows; no response-time SLA is promised. Do not publish exploitable details before a fix is available.

Security requirements for the engine include safe project path handling, bounded parsing/resource use, no implicit network access, explicit permission boundaries for future agent tools, and dependency auditing in CI.

# Security policy

Mycel is pre-alpha and not yet suitable for untrusted projects or production game distribution. Please do not run untrusted project code; scripting is not implemented and must never execute implicitly.

Report suspected vulnerabilities privately to the repository maintainers. Include affected version, reproduction steps, impact, and any mitigations. Do not publish exploitable details before a fix is available.

Security requirements for the engine include safe project path handling, bounded parsing/resource use, no implicit network access, explicit permission boundaries for future agent tools, and dependency auditing in CI.

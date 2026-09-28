# Security Policy

## Threat model / scope

Wireless Display Tool (WDT) is designed to run **entirely on a trusted local
network**. It has no cloud component, no telemetry, and no external service
dependency.

- Screen video and system audio are sent **peer-to-peer** (WebRTC/SRTP) between
  the sender and the receiver; they do not transit any third party.
- A small signaling server is **embedded in the sender app** and binds on the
  LAN. Connections are protected by a **6-digit pairing token** generated per
  session. The token is never advertised over mDNS.
- **Public STUN servers** are used only for ICE candidate gathering; media does
  not flow through them in the supported (same-LAN) scenarios.
- The diagnostics export writes a local JSON file containing **technical state
  only — no pairing token, no IP addresses, no credentials** — and is never
  uploaded automatically.

Deploying WDT on an untrusted network — public Wi-Fi without client isolation,
or across the public internet — is **out of scope** for the current MVP. Please treat pairing tokens as
short-lived secrets and do not share them.

## Supported versions

Security fixes are applied to the latest release and the `main` branch. Older
pre-release builds may not receive fixes.

| Version | Supported |
|---|---|
| Latest `main` | ✅ |
| Latest tagged release | ✅ |
| Older releases | ❌ |

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report privately via GitHub's [Security Advisories](https://github.com/tsanys/wireless-display-tool/security/advisories/new)
("Report a vulnerability"), or email **pandunorsyabani@gmail.com**.

Include, where possible:

- a description of the issue and its impact,
- steps to reproduce or a proof of concept,
- affected component (core / sender / receiver) and version/commit,
- any suggested remediation.

You can expect an acknowledgment within a few days. We will coordinate a fix and
a disclosure timeline with you, and credit you in the release notes unless you
prefer to remain anonymous.

# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Facelock, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please use GitHub's private vulnerability reporting feature on this repository

Include:
- Description of the vulnerability
- Steps to reproduce
- Impact assessment
- Suggested fix (if any)

## Response Timeline

- **Acknowledgment:** Within 48 hours
- **Initial assessment:** Within 1 week
- **Fix timeline:** Depends on severity (critical: ASAP, high: 2 weeks, medium: next release)

## Scope

The following are in scope:
- PAM module authentication bypass
- Face recognition spoofing attacks
- Privilege escalation via daemon or IPC
- Embedding extraction or tampering
- Denial of service against authentication

## Security Model

For a detailed threat model, attack vectors, and mitigation strategies, see [docs/security.md](docs/security.md).

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

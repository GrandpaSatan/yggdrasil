# Security Policy

## Reporting Vulnerabilities

If you discover a security vulnerability, please report it responsibly:

1. **Do not** open a public GitHub issue
2. Email the maintainer directly (see the repository owner's profile for contact)
3. Include a description of the vulnerability, steps to reproduce, and potential impact

You should receive a response within 72 hours.

## Security Model

Yggdrasil is designed to run on a **private LAN** and assumes a trusted network environment. It does not implement authentication or TLS between services by default.

### What This Means

- All HTTP endpoints (Odin, Mimir, Muninn, MCP remote) are unauthenticated
- Service-to-service communication is plaintext HTTP/gRPC
- Home Assistant tokens are passed via environment variables
- Database credentials are passed via environment variables or config files

### Recommendations for Deployment

- Run Yggdrasil services only on a private, trusted network
- Use a firewall to restrict access to service ports
- Use SSH key authentication (not passwords) for deployment
- Store secrets in environment files with restricted permissions (`chmod 600`)
- Do not expose Yggdrasil ports to the public internet
- If external access is needed, use a reverse proxy with TLS and authentication

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |
